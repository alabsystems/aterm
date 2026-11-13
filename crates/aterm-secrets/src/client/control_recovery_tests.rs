// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::log_entries_tests::{TwoMacs, member_key, two_macs};
use crate::commons::bootstrap::Bootstrap;
use crate::commons::roster::{Grant, Role};
use crate::membership_log::encode_records;

thread_local! {
    static BEFORE_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn before_lock() {
    let action = BEFORE_LOCK.with(|slot| slot.borrow_mut().take());
    if let Some(action) = action {
        action();
    }
}

pub(super) fn before_commit() {
    let action = BEFORE_COMMIT.with(|slot| slot.borrow_mut().take());
    if let Some(action) = action {
        action();
    }
}

fn retire_in_control(owner: &Paths, peer: &Paths) {
    let admin = admin_cached(owner).unwrap();
    let trust = admin.inspect_commons_control().unwrap().descriptor.unwrap();
    let log = load_records(owner).unwrap().log().unwrap().clone();
    let own = read_identity(owner).unwrap().unwrap().recipient();
    let other = read_identity(peer).unwrap().unwrap().recipient();
    let membership = log.membership();
    let binding = membership
        .members()
        .values()
        .find(|member| member.binding.recipient() == own)
        .unwrap()
        .binding
        .clone();
    let retired = membership
        .members()
        .values()
        .find(|member| member.binding.recipient() == other)
        .unwrap()
        .binding
        .actor();
    let mut journal = Journal::open_existing(&owner.dir.join("commons/control"), trust).unwrap();
    let node = admin
        .prepare_commons_retirement(
            &journal,
            vec![Grant::recorded("m22", binding, Role::Writer)],
            &[retired],
        )
        .unwrap();
    journal.ingest(node.bytes()).unwrap();
    assert!(
        journal
            .retained_constraints()
            .unwrap()
            .recipient_retired(&other)
            .unwrap()
    );
}

fn hidden() -> (TwoMacs, Trust) {
    hidden_with(&[])
}

fn hidden_with(extra: &[Recipient]) -> (TwoMacs, Trust) {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"retained-value")]);
    let (bootstrap, actor, incarnation) = member_key(&macs.m22);
    let trust = bootstrap.trust();
    let mut log = load_records(&macs.m22).unwrap().log().unwrap().clone();
    let identity = read_identity(&macs.m22).unwrap().unwrap();
    let holder = read_identity(&macs.m7).unwrap().unwrap().recipient();
    let generation = log
        .open_audience_entries(&identity)
        .unwrap()
        .unwrap()
        .max_generation[&EpochAudience::Commons]
        + 1;
    let records = bootstrap.device().with_seed(|seed| {
        let (epoch, key) = log
            .draft_epoch(&actor, 1000, EpochAudience::Commons, generation)
            .unwrap();
        let epoch = epoch.sign(seed, incarnation).unwrap();
        log.insert(epoch.clone()).unwrap();
        let id = *epoch.id();
        let mut records = vec![epoch];
        for recipient in std::iter::once(&holder).chain(extra) {
            let wrap = log
                .draft_wrap(&actor, 1000, id, &key, *recipient)
                .unwrap()
                .sign(seed, incarnation)
                .unwrap();
            log.insert(wrap.clone()).unwrap();
            records.push(wrap);
        }
        records
    });
    save_records(&macs.m22, &encode_records(&records)).unwrap();
    (macs, trust)
}

#[test]
fn a_plan_is_read_only_and_restores_the_same_root_without_losing_history() {
    let (macs, trust) = hidden();
    let files = || {
        (
            std::fs::read(macs.m22.cache_file()).unwrap(),
            std::fs::read(macs.m22.records_file()).unwrap(),
        )
    };
    let before = files();
    let history = load_records(&macs.m22).unwrap().inventory();
    let plan = plan_commons_control_recovery(&macs.m22).unwrap();
    assert!(plan.needed);
    assert!(plan.published_descriptor);
    assert_eq!(plan.trust, trust);
    assert_eq!(files(), before);
    assert_eq!(
        plan.recipients
            .iter()
            .filter(|recipient| recipient.restored)
            .map(|recipient| recipient.label.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("m22")]
    );
    let recovered = restore_commons_control(&macs.m22, &plan.digest).unwrap();
    assert!(recovered.changed);
    assert_eq!(recovered.trust, trust);
    assert_eq!(recovered.recipients, 2);
    assert_eq!(std::fs::read(macs.m22.cache_file()).unwrap(), before.0);
    let current = load_records(&macs.m22).unwrap();
    assert!(history.is_subset(&current.inventory()));
    let identity = read_identity(&macs.m22).unwrap().unwrap();
    let opened = current
        .log()
        .unwrap()
        .open_audience_entries(&identity)
        .unwrap()
        .unwrap();
    assert!(opened.commons_control.is_some());
    assert_eq!(
        opened.by_audience[&Audience::Everyone]["shared"]
            .value
            .as_slice(),
        b"retained-value"
    );
    assert_eq!(Bootstrap::required(&macs.m22).unwrap().trust(), trust);
    let next = plan_commons_control_recovery(&macs.m22).unwrap();
    assert!(!next.needed);
    assert!(!next.published_descriptor);
    let retained = files();
    assert!(
        !restore_commons_control(&macs.m22, &next.digest)
            .unwrap()
            .changed
    );
    assert_eq!(files(), retained);
    assert!(restore_commons_control(&macs.m22, &plan.digest).is_err());
    assert_eq!(files(), retained);
    macs.server.stop();
}

#[test]
fn a_changed_vault_refuses_the_old_plan_before_any_recovery_record() {
    let (macs, _) = hidden();
    let plan = plan_commons_control_recovery(&macs.m22).unwrap();
    let admin = admin_cached(&macs.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"newer-value")
        .unwrap();
    admin_publish(&macs.m22, &admin, &doc).unwrap();
    let before = std::fs::read(macs.m22.records_file()).unwrap();
    assert!(restore_commons_control(&macs.m22, &plan.digest).is_err());
    assert_eq!(std::fs::read(macs.m22.records_file()).unwrap(), before);
    macs.server.stop();
}

#[test]
fn absent_custody_and_non_admins_cannot_plan_or_restore() {
    let root = aterm_tempfile::tempdir().unwrap();
    let absent = Paths::for_home(&root.path().join("absent"));
    assert!(plan_commons_control_recovery(&absent).is_err());
    assert!(restore_commons_control(&absent, &[0; 32]).is_err());
    assert!(!absent.home.exists());
    let (macs, _) = hidden();
    let before = std::fs::read(macs.m7.records_file()).unwrap();
    assert!(plan_commons_control_recovery(&macs.m7).is_err());
    assert!(restore_commons_control(&macs.m7, &[0; 32]).is_err());
    assert_eq!(std::fs::read(macs.m7.records_file()).unwrap(), before);
    macs.server.stop();
}

#[test]
fn a_removed_recipient_stays_out_and_invalidates_an_earlier_plan() {
    let (macs, _) = hidden();
    let earlier = plan_commons_control_recovery(&macs.m22).unwrap();
    let log = load_records(&macs.m22).unwrap().log().unwrap().clone();
    let (actor, device, incarnation) = log_entries::open_local_member_signer(&macs.m22, &log)
        .unwrap()
        .unwrap();
    let peer = log
        .membership()
        .members()
        .values()
        .find(|member| member.label == "m7")
        .unwrap()
        .binding
        .actor();
    let cutoff = log.membership().get(&peer).unwrap().entry;
    let remove = device.with_seed(|seed| {
        log.draft(
            &actor,
            2000,
            Body::Remove {
                member: peer,
                cutoff,
            },
        )
        .sign(seed, incarnation)
        .unwrap()
    });
    save_records(&macs.m22, &encode_records(&[remove])).unwrap();
    let before = std::fs::read(macs.m22.records_file()).unwrap();
    assert!(restore_commons_control(&macs.m22, &earlier.digest).is_err());
    assert_eq!(std::fs::read(macs.m22.records_file()).unwrap(), before);
    let plan = plan_commons_control_recovery(&macs.m22).unwrap();
    assert_eq!(plan.recipients.len(), 1);
    assert_eq!(plan.recipients[0].label.as_deref(), Some("m22"));
    restore_commons_control(&macs.m22, &plan.digest).unwrap();
    let identity = read_identity(&macs.m7).unwrap().unwrap();
    assert!(
        load_records(&macs.m22)
            .unwrap()
            .log()
            .unwrap()
            .open_commons_control(&identity)
            .unwrap()
            .is_none()
    );
    macs.server.stop();
}

#[test]
fn roster_denials_override_the_published_audience() {
    for node in [false, true] {
        let (macs, _) = hidden();
        let admin = admin_cached(&macs.m22).unwrap();
        let mut doc = admin.doc.clone();
        let mut roster = crate::swarm_roster::SwarmRoster::from_document(&doc)
            .unwrap()
            .unwrap_or_default();
        if node {
            let (_, actor, _) = member_key(&macs.m7);
            roster
                .deny_nodes
                .push(aterm_codec::hex::encode_lower(&actor[..32]));
        } else {
            roster.deny_labels.push("m7".into());
        }
        doc.set_entry(
            &Audience::Everyone,
            crate::swarm_roster::ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .unwrap();
        admin_publish(&macs.m22, &admin, &doc).unwrap();
        let plan = plan_commons_control_recovery(&macs.m22).unwrap();
        assert_eq!(plan.recipients.len(), 1);
        assert_eq!(plan.recipients[0].label.as_deref(), Some("m22"));
        restore_commons_control(&macs.m22, &plan.digest).unwrap();
        let identity = read_identity(&macs.m7).unwrap().unwrap();
        assert!(
            load_records(&macs.m22)
                .unwrap()
                .log()
                .unwrap()
                .open_commons_control(&identity)
                .unwrap()
                .is_none()
        );
        macs.server.stop();
    }
}

#[test]
fn current_unlabelled_recipients_are_preserved() {
    let identity = Identity::generate().unwrap();
    let recipient = identity.recipient();
    let (macs, _) = hidden_with(&[recipient]);
    let plan = plan_commons_control_recovery(&macs.m22).unwrap();
    let retained = plan
        .recipients
        .iter()
        .find(|target| target.recipient == recipient.to_bech32())
        .unwrap();
    assert!(!retained.restored);
    assert!(retained.label.is_none());
    assert_eq!(plan.recipients.len(), 3);
    restore_commons_control(&macs.m22, &plan.digest).unwrap();
    assert!(
        load_records(&macs.m22)
            .unwrap()
            .log()
            .unwrap()
            .open_commons_control(&identity)
            .unwrap()
            .is_some()
    );
    macs.server.stop();
}

#[test]
fn a_changed_immutable_pin_refuses_recovery_without_records() {
    let (macs, _) = hidden();
    let plan = plan_commons_control_recovery(&macs.m22).unwrap();
    let path = macs.m22.dir.join("commons/default.pin");
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(path, bytes).unwrap();
    let before = std::fs::read(macs.m22.records_file()).unwrap();
    assert!(restore_commons_control(&macs.m22, &plan.digest).is_err());
    assert_eq!(std::fs::read(macs.m22.records_file()).unwrap(), before);
    macs.server.stop();
}

#[test]
fn a_publication_between_preflight_and_commit_refuses_recovery() {
    let (macs, _) = hidden();
    let plan = plan_commons_control_recovery(&macs.m22).unwrap();
    let paths = macs.m22.clone();
    BEFORE_LOCK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let admin = admin_cached(&paths).unwrap();
            let mut doc = admin.doc.clone();
            doc.set_entry(&Audience::Everyone, "shared", b"raced-value")
                .unwrap();
            admin_publish(&paths, &admin, &doc).unwrap();
        }))
    });
    assert!(restore_commons_control(&macs.m22, &plan.digest).is_err());
    let identity = read_identity(&macs.m22).unwrap().unwrap();
    let opened = load_records(&macs.m22)
        .unwrap()
        .log()
        .unwrap()
        .open_audience_entries(&identity)
        .unwrap()
        .unwrap();
    assert_eq!(
        opened.maximal_epochs[&EpochAudience::Commons],
        vec![plan.current_epoch]
    );
    assert!(opened.commons_control.is_none());
    assert_eq!(
        opened.by_audience[&Audience::Everyone]["shared"]
            .value
            .as_slice(),
        b"raced-value"
    );
    macs.server.stop();
}

#[test]
fn root_retirements_are_preserved_even_when_the_membership_log_still_admits_the_peer() {
    let (macs, trust) = hidden();
    retire_in_control(&macs.m22, &macs.m7);
    let before = Journal::read_existing(&macs.m22.dir.join("commons/control"), trust)
        .unwrap()
        .inventory()
        .copied()
        .collect::<BTreeSet<_>>();
    let plan = plan_commons_control_recovery(&macs.m22).unwrap();
    assert_eq!(plan.recipients.len(), 1);
    assert_eq!(plan.recipients[0].label.as_deref(), Some("m22"));
    restore_commons_control(&macs.m22, &plan.digest).unwrap();
    let after = Journal::read_existing(&macs.m22.dir.join("commons/control"), trust).unwrap();
    assert_eq!(after.inventory().copied().collect::<BTreeSet<_>>(), before);
    let other = read_identity(&macs.m7).unwrap().unwrap().recipient();
    assert!(
        after
            .retained_constraints()
            .unwrap()
            .recipient_retired(&other)
            .unwrap()
    );
    macs.server.stop();
}

#[test]
fn a_root_retirement_between_preflight_and_commit_refuses_recovery() {
    let (macs, _) = hidden();
    let plan = plan_commons_control_recovery(&macs.m22).unwrap();
    let owner = macs.m22.clone();
    let peer = macs.m7.clone();
    let before = std::fs::read(macs.m22.records_file()).unwrap();
    BEFORE_LOCK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            retire_in_control(&owner, &peer);
        }))
    });
    assert!(restore_commons_control(&macs.m22, &plan.digest).is_err());
    assert_eq!(std::fs::read(macs.m22.records_file()).unwrap(), before);
    macs.server.stop();
}

#[test]
fn recovery_holds_the_control_writer_until_its_membership_commit() {
    let (macs, trust) = hidden();
    let plan = plan_commons_control_recovery(&macs.m22).unwrap();
    let dir = macs.m22.dir.join("commons/control");
    let held_dir = dir.clone();
    BEFORE_COMMIT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let error = Journal::open_existing_until(&held_dir, trust, Instant::now())
                .err()
                .expect("a second control writer must not enter the recovery commit");
            assert_eq!(error.kind(), crate::ErrorKind::JournalBusy);
        }))
    });
    assert!(
        restore_commons_control(&macs.m22, &plan.digest)
            .unwrap()
            .changed
    );
    assert!(Journal::open_existing_until(&dir, trust, Instant::now()).is_ok());
    macs.server.stop();
}

#[test]
fn an_incomplete_control_history_cannot_authorize_recovery() {
    let (macs, trust) = hidden();
    let admin = admin_cached(&macs.m22).unwrap();
    let identity = read_identity(&macs.m22).unwrap().unwrap();
    let orphan = crate::commons::node::Node::test_seal(
        &admin.seed,
        trust.network,
        crate::commons::node::Edit::Update {
            parent: [91; 32],
            hosts: &[],
            payload: b"unresolved control dependency",
        },
        &[identity.recipient()],
        1001,
        &[],
    )
    .unwrap();
    let mut journal = Journal::open_existing(&macs.m22.dir.join("commons/control"), trust).unwrap();
    journal.ingest(orphan.bytes()).unwrap();
    assert_eq!(journal.state(), crate::commons::journal::State::Pending);
    drop(journal);
    let before = std::fs::read(macs.m22.records_file()).unwrap();
    assert!(plan_commons_control_recovery(&macs.m22).is_err());
    assert_eq!(std::fs::read(macs.m22.records_file()).unwrap(), before);
    macs.server.stop();
}

#[test]
fn a_recovery_that_exceeds_a_record_budget_is_reported_before_any_write() {
    let (macs, _) = hidden();
    let counts = load_records(&macs.m22).unwrap().log().unwrap().counts();
    let before = std::fs::read(macs.m22.records_file()).unwrap();
    for (budgets, exceeded) in [
        (
            RecordBudgets {
                held: counts.held() + 2,
                ..RecordBudgets::PRODUCTION
            },
            Budget::Held,
        ),
        (
            RecordBudgets {
                keyed: counts.keyed + 2,
                ..RecordBudgets::PRODUCTION
            },
            Budget::Keyed,
        ),
    ] {
        budgets.within(|| {
            let plan = plan_commons_control_recovery(&macs.m22).unwrap();
            assert_eq!(plan.record_capacity.current, counts);
            assert_eq!(
                plan.record_capacity.additional.held(),
                plan.recipients.len() + 2
            );
            assert_eq!(plan.record_capacity.exceeded, Some(exceeded));
            assert_eq!(
                restore_commons_control(&macs.m22, &plan.digest)
                    .unwrap_err()
                    .kind(),
                crate::ErrorKind::MembershipCapacity
            );
        });
        assert_eq!(std::fs::read(macs.m22.records_file()).unwrap(), before);
    }
    macs.server.stop();
}
