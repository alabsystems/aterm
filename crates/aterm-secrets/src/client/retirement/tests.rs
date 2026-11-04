// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::{binding::Pending, node::Edit, roster::Role};

/// A revoke must fit its whole security operation before retiring the recipient.
/// The retirement itself fits here; the changed envelope's cascade needs more
/// records afterwards. Discarding that later draft error would publish an
/// unstamped envelope after already making the retirement durable.
#[test]
fn a_revoke_that_cannot_fit_its_cascade_refuses_before_retirement() {
    use crate::membership_log::{Body, MAX_HELD_RECORDS};

    let lab = Lab::new(true, Custody::Founded);
    let paths = &lab.paths[0];
    let owner = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let binding = peer.request().unwrap();
    let target = binding.binding().actor();
    let recipient = binding.binding().recipient();
    let admin = admin_cached(paths).unwrap();
    let mut doc = admin.published_doc.clone();
    let code = encode_fleet_join_code(&[7; FLEET_SECRET_LEN], Some("127.0.0.1:7199"));
    doc.set_entry(
        &Audience::Everyone,
        crate::client::fleet::FLEET_CODE_ENTRY,
        code.as_bytes(),
    )
    .unwrap();
    admin_publish(paths, &admin, &doc).unwrap();
    seed_vault_genesis(paths, &lab.root.path().join("vault.genesis")).unwrap();
    assert!(
        crate::client::log_entries::trusted_commons_log(paths, owner.trust())
            .unwrap()
            .is_some()
    );

    let mut store = load_records(paths).unwrap();
    assert!(store.log().unwrap().has_commons_epochs());
    assert!(store.log().unwrap().membership().contains(&target));
    let excludes = std::collections::BTreeSet::from([recipient]);
    let retirement =
        crate::client::log_entries::draft_retire_in_log(paths, &store, &[target], &excludes)
            .unwrap();
    assert!(!retirement.is_empty());
    let room = MAX_HELD_RECORDS.checked_sub(retirement.len()).unwrap();
    let request = owner.request().unwrap();
    owner.device().with_seed(|seed| {
        while store.log().unwrap().held_count() < room {
            let record = store
                .log()
                .unwrap()
                .draft(
                    &request.binding().actor(),
                    2_000,
                    Body::Name { name: "m1".into() },
                )
                .sign(seed, *request.binding().incarnation())
                .unwrap();
            assert!(store.insert(record).unwrap());
        }
    });
    assert_eq!(store.log().unwrap().held_count(), room);
    fsx::write_atomic(&paths.records_file(), &store.encode(), 0o600).unwrap();
    let before_records = std::fs::read(paths.records_file()).unwrap();
    let before_source = std::fs::read(&lab.source).unwrap();
    let admin = admin_cached(paths).unwrap();

    // Prove both sides of the capacity boundary on a private copy: retirement
    // fits, but the final document needs an additional record after it.
    let preview = Paths::for_home(&lab.root.path().join("preview"));
    crate::client::log_entries_tests::copy_private_tree(&paths.home, &preview.home);
    let mut wanted = admin.published_doc.clone();
    wanted.remove_machine("m2", false).unwrap();
    apply_swarm_revoke_cascade(&preview, &admin, &mut wanted, "m2", &recipient).unwrap();
    assert!(
        retire_cached(&preview, &admin, &wanted, &[recipient])
            .unwrap()
            .is_some_and(|retired| retired.changed)
    );
    assert_eq!(
        load_records(&preview).unwrap().log().unwrap().held_count(),
        MAX_HELD_RECORDS
    );
    assert!(
        crate::client::log_entries::draft_document_diff(
            &preview,
            &admin.config,
            &envelope::signer_public(&admin.seed).unwrap(),
            &admin.handoff,
            &read_identity(&preview).unwrap().unwrap(),
            &wanted,
            crate::client::log_entries::Base {
                doc: Some(&admin.published_doc),
                resolve: Some(&admin.resolve),
                readmit: None,
            },
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&lab.source).unwrap(), before_source);

    let outcome = admin_revoke(paths, &admin, "m2", false);

    assert!(outcome.is_err(), "a full revoke cannot fit");
    assert_eq!(std::fs::read(paths.records_file()).unwrap(), before_records);
    assert_eq!(std::fs::read(&lab.source).unwrap(), before_source);
    assert!(
        load_records(paths)
            .unwrap()
            .log()
            .unwrap()
            .membership()
            .contains(&target)
    );
}

fn planned_revoke() -> (Lab, Admin, Document, Recipient) {
    let lab = Lab::new(true, Custody::Founded);
    seed_vault_genesis(&lab.paths[0], &lab.root.path().join("planned.genesis")).unwrap();
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let recipient = read_identity(&lab.paths[1]).unwrap().unwrap().recipient();
    let mut doc = admin.published_doc.clone();
    doc.remove_machine("m2", false).unwrap();
    apply_swarm_revoke_cascade(&lab.paths[0], &admin, &mut doc, "m2", &recipient).unwrap();
    (lab, admin, doc, recipient)
}

#[test]
fn removal_preflight_reports_the_exact_batch_without_writing_it() {
    let (lab, admin, doc, recipient) = planned_revoke();
    let paths = &lab.paths[0];
    let identity = read_identity(paths).unwrap().unwrap();
    let before = std::fs::read(paths.records_file()).unwrap();
    let source = std::fs::read(&lab.source).unwrap();
    let plan =
        crate::client::removal_plan::prepare(paths, &admin, &doc, &identity, &[recipient], None)
            .unwrap();
    let cost = plan.cost;
    assert!(cost.after.complete.held() > cost.before.complete.held());
    assert!(cost.after.encoded_bytes > cost.before.encoded_bytes);
    assert_eq!(std::fs::read(paths.records_file()).unwrap(), before);
    assert_eq!(std::fs::read(&lab.source).unwrap(), source);
    assert!(matches!(
        plan.commit(paths, &admin, &doc, None).unwrap(),
        crate::client::removal_plan::Committed::Done(Some(_))
    ));
    let committed = load_records(paths).unwrap();
    assert_eq!(
        committed.log().unwrap().held_count(),
        cost.after.complete.held()
    );
    assert_eq!(committed.len(), cost.after.stored_records);
    assert_eq!(committed.encode().len(), cost.after.encoded_bytes);
    assert!(committed.log().unwrap().recipient_is_removed(&recipient));
    assert_eq!(std::fs::read(&lab.source).unwrap(), source);
}

/// A removal plan that would pass one of the log's budgets is refused whole, before anything is
/// written, with that budget named: each budget alone refuses, set one record short of what the
/// plan needs of it.
///
/// NEGATIVE CONTROL (in the test): under budgets with exactly the plan's need of room in each,
/// the same plan is prepared, so each refusal is its own budget's boundary.
#[test]
fn a_removal_plan_that_would_pass_a_budget_is_refused_whole() {
    use crate::membership_log::{Budget, RecordBudgets};

    let (lab, admin, doc, recipient) = planned_revoke();
    let paths = &lab.paths[0];
    let identity = read_identity(paths).unwrap().unwrap();
    let records = std::fs::read(paths.records_file()).unwrap();
    let source = std::fs::read(&lab.source).unwrap();
    let prepare =
        || crate::client::removal_plan::prepare(paths, &admin, &doc, &identity, &[recipient], None);
    // Each plan is dropped before the next: it holds the records author lock.
    let cost = prepare().unwrap().cost;
    let (was, will) = (cost.before.complete, cost.after.complete);
    assert!(
        will.weighed > was.weighed && will.keyed > was.keyed,
        "{was:?} to {will:?}"
    );
    let exact = RecordBudgets {
        weighed: will.weighed,
        keyed: will.keyed,
        held: will.held(),
    };
    assert!(
        exact.within(prepare).is_ok(),
        "the plan fits budgets with exactly its room"
    );
    for (short, budget) in [
        (
            RecordBudgets {
                weighed: will.weighed - 1,
                ..exact
            },
            Budget::Weighed,
        ),
        (
            RecordBudgets {
                keyed: will.keyed - 1,
                ..exact
            },
            Budget::Keyed,
        ),
        (
            RecordBudgets {
                held: will.held() - 1,
                ..exact
            },
            Budget::Held,
        ),
    ] {
        let Err(error) = short.within(prepare) else {
            panic!("a plan past its {budget:?} budget was prepared");
        };
        assert_eq!(
            error.kind(),
            crate::ErrorKind::MembershipCapacity,
            "{budget:?}: {error}"
        );
        assert!(
            error
                .to_string()
                .contains(&format!("limit of {}", budget.counts())),
            "{budget:?}: {error}"
        );
        assert_eq!(std::fs::read(paths.records_file()).unwrap(), records);
        assert_eq!(std::fs::read(&lab.source).unwrap(), source);
    }
}

#[test]
fn removal_preflight_refuses_a_different_config_before_retirement() {
    let (lab, admin, doc, recipient) = planned_revoke();
    let paths = &lab.paths[0];
    let identity = read_identity(paths).unwrap().unwrap();
    let before = std::fs::read(paths.records_file()).unwrap();
    let source = std::fs::read(&lab.source).unwrap();
    let plan =
        crate::client::removal_plan::prepare(paths, &admin, &doc, &identity, &[recipient], None)
            .unwrap();
    let mut moved = admin.config.clone();
    moved.source = Source::Path(lab.root.path().join("different-vault"));
    {
        let _state = StateLock::take(paths).unwrap();
        save_config(paths, &moved).unwrap();
    }
    assert!(plan.commit(paths, &admin, &doc, None).is_err());
    assert_eq!(std::fs::read(paths.records_file()).unwrap(), before);
    assert_eq!(std::fs::read(&lab.source).unwrap(), source);
}

#[test]
fn removal_preflight_refuses_an_already_replaced_same_sequence_envelope() {
    let (lab, admin, doc, recipient) = planned_revoke();
    let paths = &lab.paths[0];
    let identity = read_identity(paths).unwrap().unwrap();
    let records = std::fs::read(paths.records_file()).unwrap();
    let mut other = admin.published_doc.clone();
    other
        .set_entry(&Audience::Everyone, "another-write", b"same-sequence")
        .unwrap();
    let replacement = seal_with_admissions_and_parents(
        &admin.config.name,
        admin.seq,
        &admin.seed,
        &admin.commons_seed,
        admin.commons_bound,
        &other,
        &admin.handoff,
        &admin.opened_heads,
        &admin.admissions,
    )
    .unwrap();
    let digest = envelope::parse(&replacement).unwrap().digest();
    assert_ne!(admin.opened_heads.first(), Some(&digest));
    {
        let _state = StateLock::take(paths).unwrap();
        fsx::write_atomic(&paths.cache_file(), &replacement, 0o600).unwrap();
        let mut state = load_state(paths).unwrap();
        state.seq = admin.seq;
        state.digest = Some(digest);
        save_state(paths, &state).unwrap();
    }
    let result =
        crate::client::removal_plan::prepare(paths, &admin, &doc, &identity, &[recipient], None);
    assert!(matches!(result, Err(error) if error.kind() == crate::ErrorKind::VaultChanged));
    assert_eq!(std::fs::read(paths.records_file()).unwrap(), records);
    assert_eq!(std::fs::read(paths.cache_file()).unwrap(), replacement);
}

#[test]
fn removal_preflight_accepts_the_existing_log_only_source_binding() {
    let (lab, admin, doc, recipient) = planned_revoke();
    let paths = &lab.paths[0];
    let identity = read_identity(paths).unwrap().unwrap();
    {
        let _state = StateLock::take(paths).unwrap();
        let mut state = load_state(paths).unwrap();
        assert!(state.records_digest.is_some());
        state.digest = None;
        std::fs::remove_file(paths.cache_file()).unwrap();
        save_state(paths, &state).unwrap();
    }
    let plan =
        crate::client::removal_plan::prepare(paths, &admin, &doc, &identity, &[recipient], None)
            .unwrap();
    assert!(matches!(
        plan.commit(paths, &admin, &doc, None).unwrap(),
        crate::client::removal_plan::Committed::Done(Some(_))
    ));
    assert!(
        load_records(paths)
            .unwrap()
            .log()
            .unwrap()
            .recipient_is_removed(&recipient)
    );
}

#[test]
fn removal_preflight_refuses_changed_commons_custody_with_the_same_age_identity() {
    let (lab, admin, doc, recipient) = planned_revoke();
    let paths = &lab.paths[0];
    let identity = read_identity(paths).unwrap().unwrap();
    let before = std::fs::read(paths.records_file()).unwrap();
    let source = std::fs::read(&lab.source).unwrap();
    let plan =
        crate::client::removal_plan::prepare(paths, &admin, &doc, &identity, &[recipient], None)
            .unwrap();

    let replacement = Paths::for_home(&lab.root.path().join("different-device"));
    crate::client::log_entries_tests::clone_identity_to(paths, &replacement);
    let other = crate::commons::bootstrap::Device::open(&replacement)
        .unwrap()
        .unwrap();
    let original = lab.bootstrap(0).request().unwrap();
    let changed = other
        .request(
            *original.binding().network(),
            *original.binding().authority(),
        )
        .unwrap();
    assert_ne!(original.binding(), changed.binding());
    assert_eq!(
        original.binding().recipient(),
        changed.binding().recipient()
    );
    {
        let _state = StateLock::take(paths).unwrap();
        for name in ["device.age", "device.claim"] {
            let bytes = std::fs::read(replacement.dir.join("commons").join(name)).unwrap();
            fsx::write_atomic(&paths.dir.join("commons").join(name), &bytes, 0o600).unwrap();
        }
    }
    assert!(plan.commit(paths, &admin, &doc, None).is_err());
    assert_eq!(std::fs::read(paths.records_file()).unwrap(), before);
    assert_eq!(std::fs::read(&lab.source).unwrap(), source);
}

#[test]
fn removal_preflight_discards_a_plan_when_peer_ingress_changes_its_inventory() {
    let (lab, admin, doc, recipient) = planned_revoke();
    let paths = &lab.paths[0];
    let identity = read_identity(paths).unwrap().unwrap();
    let plan =
        crate::client::removal_plan::prepare(paths, &admin, &doc, &identity, &[recipient], None)
            .unwrap();
    let peer = lab.bootstrap(1);
    let request = peer.request().unwrap();
    let store = load_records(paths).unwrap();
    let incoming = peer.device().with_seed(|seed| {
        store
            .log()
            .unwrap()
            .draft(
                &request.binding().actor(),
                4_000,
                crate::membership_log::Body::Name { name: "m2".into() },
            )
            .sign(seed, *request.binding().incarnation())
            .unwrap()
    });
    save_records(paths, incoming.bytes()).unwrap();
    let with_ingress = std::fs::read(paths.records_file()).unwrap();
    assert!(matches!(
        plan.commit(paths, &admin, &doc, None).unwrap(),
        crate::client::removal_plan::Committed::Changed
    ));
    assert_eq!(std::fs::read(paths.records_file()).unwrap(), with_ingress);
    assert!(
        !load_records(paths)
            .unwrap()
            .log()
            .unwrap()
            .recipient_is_removed(&recipient)
    );
    let fresh =
        crate::client::removal_plan::prepare(paths, &admin, &doc, &identity, &[recipient], None)
            .unwrap();
    assert!(matches!(
        fresh.commit(paths, &admin, &doc, None).unwrap(),
        crate::client::removal_plan::Committed::Done(Some(_))
    ));
    assert!(load_records(paths).unwrap().holds(incoming.id()));
    assert!(
        load_records(paths)
            .unwrap()
            .log()
            .unwrap()
            .records()
            .any(|record| {
                matches!(record.body(), crate::membership_log::Body::Remove { member, cutoff }
            if *member == request.binding().actor() && cutoff == incoming.id())
            })
    );
}

struct Lab {
    root: aterm_tempfile::TempDir,
    paths: [Paths; 2],
    source: PathBuf,
}

/// How a lab's vault holds its admin custody.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Custody {
    /// Created with a Commons root (the default since Commons custody).
    Founded,
    /// Created without one, its Commons key never bound.
    Unbound,
    /// Last written by an admin before Commons custody: a version-1 locker, read as bound,
    /// with no Commons control.
    V1,
}

impl Lab {
    fn new(enrolled: bool, custody: Custody) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = [
            Paths::for_home(&root.path().join("m1")),
            Paths::for_home(&root.path().join("m2")),
        ];
        let source = root.path().join("vault");
        let created = if custody == Custody::Founded {
            create(&paths[0], source.to_str().unwrap(), None, Some("m1"))
        } else {
            test_create_unbound(&paths[0], source.to_str().unwrap(), None, Some("m1"))
        }
        .unwrap();
        let recipient = machine_identity(&paths[1]).unwrap().recipient().to_bech32();
        let admin = admin_cached(&paths[0]).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &recipient).unwrap();
        admin_publish_approved(&paths[0], &admin, &doc, &[recipient]).unwrap();
        join(
            &paths[1],
            &JoinOptions {
                source: format!("{}#{}", source.display(), created.signer),
                host: Some("m2".into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: crate::client::JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        if custody == Custody::V1 {
            admin_keys::legacy::publish_legacy(&paths[0], &admin_cached(&paths[0]).unwrap());
            let admin = admin_cached(&paths[0]).unwrap();
            assert!(admin.commons_bound && admin.custody != admin_keys::MAGIC);
        }
        let lab = Self {
            root,
            paths,
            source,
        };
        if enrolled {
            let owner = lab.bootstrap(0);
            let peer = lab.bootstrap(1);
            let request = peer.request().unwrap();
            let (mut pending, challenge) =
                Pending::issue(&request, peer.trust().network, peer.trust().authority).unwrap();
            let answer = peer.device().answer(&request, &challenge).unwrap();
            let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
            admin_cached(&lab.paths[0])
                .unwrap()
                .enroll_commons_member(&owner, &mut journal, pending.verify(&answer).unwrap())
                .unwrap();
        }
        lab
    }
    fn bootstrap(&self, n: usize) -> Bootstrap {
        Bootstrap::required(&self.paths[n]).unwrap()
    }
    fn third(&self) -> String {
        let paths = Paths::for_home(&self.root.path().join("m3"));
        let recipient = machine_identity(&paths).unwrap().recipient().to_bech32();
        let admin = admin_cached(&self.paths[0]).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m3", &recipient).unwrap();
        admin_publish_approved(
            &self.paths[0],
            &admin,
            &doc,
            std::slice::from_ref(&recipient),
        )
        .unwrap();
        join(
            &paths,
            &JoinOptions {
                source: format!(
                    "{}#{}",
                    self.source.display(),
                    crate::b64::encode_padded(&admin.config.signer.unwrap())
                ),
                host: Some("m3".into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: crate::client::JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        let owner = self.bootstrap(0);
        let peer = Bootstrap::required(&paths).unwrap();
        let request = peer.request().unwrap();
        let (mut pending, challenge) =
            Pending::issue(&request, owner.trust().network, owner.trust().authority).unwrap();
        let response = peer.device().answer(&request, &challenge).unwrap();
        let admin = admin_cached(&self.paths[0]).unwrap();
        let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        admin
            .enroll_commons_member(&owner, &mut journal, pending.verify(&response).unwrap())
            .unwrap();
        recipient
    }
    fn obstruct(&self) {
        std::fs::rename(&self.source, self.root.path().join("held-vault")).unwrap();
        std::fs::create_dir(&self.source).unwrap();
    }
    fn legacy_remove(&self, names: &[&str]) {
        // Simulate a signed publication from the earlier vault-only workflow.
        let admin = admin_cached(&self.paths[0]).unwrap();
        let mut doc = admin.doc.clone();
        for name in names {
            doc.remove_machine(name, false).unwrap();
        }
        let sequence = envelope::next_seq(admin.seq).unwrap();
        let admissions = admin.admissions.retained(&doc).unwrap();
        let bytes = seal_with_admissions_and_parents(
            &admin.config.name,
            sequence,
            &admin.seed,
            &admin.commons_seed,
            true,
            &doc,
            &admin.handoff,
            &admin.opened_heads,
            &admissions,
        )
        .unwrap();
        transport::publish(&admin.config.source, &bytes).unwrap();
        sync(&self.paths[0], None).unwrap();
    }
    fn restore(&self) {
        std::fs::remove_dir(&self.source).unwrap();
        std::fs::rename(self.root.path().join("held-vault"), &self.source).unwrap();
    }
    fn publish_failure(&self) {
        let admin = admin_cached(&self.paths[0]).unwrap();
        self.obstruct();
        let error = admin_revoke(&self.paths[0], &admin, "m2", false)
            .err()
            .unwrap();
        assert!(error.to_string().contains("permanently retired"), "{error}");
        assert!(
            admin_cached(&self.paths[0])
                .unwrap()
                .doc
                .machines()
                .iter()
                .any(|machine| machine.name == "m2")
        );
    }
}

pub(crate) fn retirement_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let custody = match action {
        "Legacy" => Custody::Unbound,
        "Unfounded" | "RetainedRoot" | "UnfoundedEdit" => Custody::V1,
        _ => Custody::Founded,
    };
    let lab = Lab::new(
        custody == Custody::Founded && action != "NoBinding",
        custody,
    );
    if custody != Custody::Founded {
        return without_control(state, &lab, action);
    }
    // What the setup made: every founded vault names its control entry.
    let controlled = admin_keys::has_control(&admin_cached(&lab.paths[0]).unwrap().published_doc);
    let owner = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let actor = peer.request().unwrap().binding().actor();
    let mut targets = vec![peer.request().unwrap().binding().recipient().to_bech32()];
    let scenario = match action {
        "Revoke" => 1,
        "PublishFailure" => 2,
        "Retry" => {
            lab.publish_failure();
            lab.restore();
            3
        }
        "NoBinding" => 4,
        "SelfRemoval" => 6,
        "Admin" => {
            let admin = admin_cached(&lab.paths[0]).unwrap();
            let mut doc = admin.doc.clone();
            doc.add_admin("m2").unwrap();
            admin_publish(&lab.paths[0], &admin, &doc).unwrap();
            7
        }
        "Fork" => {
            let admin = admin_cached(&lab.paths[0]).unwrap();
            let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
            let head = *owner.access_in(&journal).unwrap().unwrap().head();
            for payload in [b"left", b"rght"] {
                let node = admin
                    .seal_commons(
                        owner.trust().network,
                        Edit::Update {
                            parent: head,
                            hosts: &["m1".into()],
                            payload,
                        },
                    )
                    .unwrap();
                journal.ingest(node.bytes()).unwrap();
            }
            8
        }
        "MissingIdentity" => 9,
        "RepairRemoved" => {
            lab.legacy_remove(&["m2"]);
            10
        }
        "BatchRemoved" => {
            targets.push(lab.third());
            lab.legacy_remove(&["m2", "m3"]);
            11
        }
        "UnknownBatch" => {
            targets.push(
                Identity::from_secret_bytes([77; 32])
                    .recipient()
                    .to_bech32(),
            );
            12
        }
        "GenericRemoval" => 13,
        _ => panic!("unknown retirement action"),
    };
    let before = Journal::open_existing(owner.journal_dir(), owner.trust())
        .unwrap()
        .inventory()
        .count();
    let admin = admin_cached(&lab.paths[0]).unwrap();
    if action == "PublishFailure" {
        lab.obstruct();
    }
    if action == "MissingIdentity" {
        std::fs::rename(
            lab.paths[0].identity_file(),
            lab.root.path().join("held-identity"),
        )
        .unwrap();
    }
    let result = if action == "GenericRemoval" {
        let mut doc = admin.doc.clone();
        doc.remove_machine("m2", false).unwrap();
        admin_publish(&lab.paths[0], &admin, &doc).map(|_| ())
    } else if matches!(action, "RepairRemoved" | "BatchRemoved" | "UnknownBatch") {
        admin_retire(&lab.paths[0], &admin, &targets).map(|_| ())
    } else {
        admin_revoke(
            &lab.paths[0],
            &admin,
            if action == "SelfRemoval" { "m1" } else { "m2" },
            false,
        )
        .map(|_| ())
    };
    if action == "MissingIdentity" {
        std::fs::rename(
            lab.root.path().join("held-identity"),
            lab.paths[0].identity_file(),
        )
        .unwrap();
    }
    if (6..=9).contains(&scenario)
        || matches!(action, "PublishFailure" | "UnknownBatch" | "GenericRemoval")
    {
        assert!(result.is_err());
    } else {
        assert!(result.is_ok());
    }
    let journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let after = journal.inventory().count();
    let retired = journal
        .inventory()
        .filter_map(|id| journal.node(id))
        .any(|node| {
            node.facts().contains(&Fact::Retire { actor })
                || node.facts().contains(&Fact::RetireRecipient {
                    recipient: peer.request().unwrap().binding().recipient(),
                })
        });
    if action == "BatchRemoved" {
        assert_eq!(
            owner.access_in(&journal).unwrap().unwrap().members().len(),
            1
        );
        assert_eq!(
            journal
                .constraints()
                .unwrap()
                .facts()
                .filter(|fact| matches!(fact, Fact::RetireRecipient { .. }))
                .count(),
            2
        );
    }
    let published = !admin_cached(&lab.paths[0])
        .unwrap()
        .doc
        .machines()
        .iter()
        .any(|machine| machine.name == "m2");
    let mut next = state.clone();
    for (name, value) in [
        ("scenario", scenario),
        ("controlled", i64::from(controlled)),
        ("retired", i64::from(retired)),
        ("published", i64::from(published)),
        ("same", i64::from(before == after)),
    ] {
        next.insert(name, value);
    }
    next
}

/// [`retirement_transition`] on a vault with no control journal: an unbound one (`Legacy`) or a
/// version-1 one, which removes vault-only unless this Mac keeps Commons control state
/// (`RetainedRoot`, a control directory planted beside the vault's files).
fn without_control(
    state: &aterm_spec::interp::State,
    lab: &Lab,
    action: &str,
) -> aterm_spec::interp::State {
    if action == "RetainedRoot" {
        std::fs::create_dir_all(lab.paths[0].dir.join("commons/control")).unwrap();
    }
    let admin = admin_cached(&lab.paths[0]).unwrap();
    // What the setup made: only `RetainedRoot` leaves Commons state on the admin's Mac.
    let controlled = action == "RetainedRoot";
    let authority = admin.commons_authority().unwrap();
    let result = if action == "UnfoundedEdit" {
        let mut doc = admin.doc.clone();
        doc.remove_machine("m2", false).unwrap();
        admin_publish(&lab.paths[0], &admin, &doc).map(|_| None)
    } else {
        admin_revoke(&lab.paths[0], &admin, "m2", false).map(|done| done.retirement)
    };
    let reopened = admin_cached(&lab.paths[0]).unwrap();
    let scenario = match action {
        "Legacy" => 5,
        "Unfounded" => 14,
        "RetainedRoot" => 15,
        "UnfoundedEdit" => 16,
        _ => panic!("unknown retirement action"),
    };
    match result {
        Err(refused) => {
            assert_eq!(scenario, 15, "{action}: {refused}");
            assert!(
                refused.to_string().contains("missing its descriptor"),
                "{refused}"
            );
            assert_eq!(reopened.seq, admin.seq, "nothing was published");
        }
        Ok(retirement) => {
            assert_ne!(scenario, 15, "retained control refuses");
            assert_eq!(retirement, None, "{action}: nothing to retire");
            assert_eq!(reopened.custody, admin_keys::MAGIC);
            assert_eq!(reopened.commons_bound, custody_bound(action));
            if action != "Legacy" {
                assert_eq!(
                    reopened.commons_authority().unwrap(),
                    authority,
                    "the root key stays"
                );
            }
        }
    }
    let published = !reopened
        .doc
        .machines()
        .iter()
        .any(|machine| machine.name == "m2");
    let mut next = state.clone();
    for (name, value) in [
        ("scenario", scenario),
        ("controlled", i64::from(controlled)),
        ("retired", 0),
        ("published", i64::from(published)),
        ("same", 1),
    ] {
        next.insert(name, value);
    }
    next
}

/// Whether `action`'s lab is bound once it published: every version-1 vault is.
fn custody_bound(action: &str) -> bool {
    action != "Legacy"
}

#[test]
fn an_admin_named_later_removes_a_machine_once_it_enrolls() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = ["m1", "m2", "m3"].map(|host| Paths::for_home(&root.path().join(host)));
    let source = root.path().join("vault");
    let created = create(&paths[0], source.to_str().unwrap(), None, Some("m1")).unwrap();
    let pinned = format!("{}#{}", source.display(), created.signer);
    // m2 and m3 come in by a plain publish, which records no decision and leaves them
    // outside the control entry's audience.
    for (machine, host) in paths[1..].iter().zip(["m2", "m3"]) {
        let recipient = machine_identity(machine).unwrap().recipient().to_bech32();
        let admin = admin_cached(&paths[0]).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine(host, &recipient).unwrap();
        admin_publish(&paths[0], &admin, &doc).unwrap();
        join(
            machine,
            &JoinOptions {
                source: pinned.clone(),
                host: Some(host.into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: crate::client::JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
    }
    let in_audience = |host: &str| {
        matches!(
            admin_cached(&paths[0]).unwrap().published_doc.commons_audience(),
            Some(Audience::Hosts(hosts)) if hosts.iter().any(|h| h == host)
        )
    };
    let remove_m3 = || admin_revoke(&paths[1], &admin_open(&paths[1]).unwrap(), "m3", false);
    // An earlier aterm named m2 an admin by a plain publish.
    let admin = admin_cached(&paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_admin("m2").unwrap();
    admin_publish(&paths[0], &admin, &doc).unwrap();
    sync(&paths[1], None).unwrap();
    assert!(
        !in_audience("m2"),
        "CONTROL: a plain publish leaves it outside"
    );
    let refused = remove_m3().err().unwrap().to_string();
    assert!(
        refused.contains("outside the vault's Commons control")
            && refused.contains("remove it from m1,")
            && refused.contains(&format!(
                "aterm secrets admins add {} there",
                join_code(&machine_identity(&paths[1]).unwrap().recipient())
            )),
        "{refused}"
    );

    // Naming it again from m1 brings it into the audience, so it holds the control entry...
    let admin = admin_cached(&paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_admin("m2").unwrap();
    admin_publish_admin(&paths[0], &admin, &doc, "m2").unwrap();
    assert!(in_audience("m2"));
    assert!(!in_audience("m3"), "only the admin named");
    // Until m2 applies that version, it is told to.
    let refused = remove_m3().err().unwrap().to_string();
    assert!(
        refused.contains("not yet applied the vault version"),
        "{refused}"
    );
    sync(&paths[1], None).unwrap();
    let peer = Bootstrap::required(&paths[1]).unwrap();
    // ...though no roster names it yet.
    let refused = remove_m3().err().unwrap().to_string();
    assert!(
        refused.contains("not yet an enrolled member") && refused.ends_with("remove it from m1"),
        "{refused}"
    );

    // Enrolled by m1, with m1's journal, it removes m3.
    let owner = Bootstrap::required(&paths[0]).unwrap();
    let request = peer.request().unwrap();
    let (mut pending, challenge) =
        Pending::issue(&request, owner.trust().network, owner.trust().authority).unwrap();
    let answer = peer.device().answer(&request, &challenge).unwrap();
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    admin_cached(&paths[0])
        .unwrap()
        .enroll_commons_member(&owner, &mut journal, pending.verify(&answer).unwrap())
        .unwrap();
    let mut held = Journal::open_existing(peer.journal_dir(), peer.trust()).unwrap();
    for id in journal.inventory() {
        held.ingest(journal.node(id).unwrap().bytes()).unwrap();
    }
    drop(held);
    let revoked = remove_m3().unwrap();
    assert!(revoked.retirement.is_some_and(|retired| retired.changed));
    assert!(
        !admin_cached(&paths[1])
            .unwrap()
            .doc
            .machines()
            .iter()
            .any(|machine| machine.name == "m3")
    );
}

#[test]
fn retirement_publication_matches_real_failures_retries_and_preflight_refusals() {
    let model = aterm_models_db::retirement_publication_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let next = retirement_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
    }
    let mut mutant = retirement_transition(&initial, "Retry");
    mutant.insert("same", 0);
    assert!(!model.check_invariant("RetryKeepsEpoch", &mutant));
}

#[test]
fn retirement_preserves_other_roles_and_reapproval_cannot_resurrect_the_recipient() {
    let lab = Lab::new(true, Custody::Founded);
    let owner = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let access = owner.access_in(&journal).unwrap().unwrap();
    let grants = access
        .members()
        .iter()
        .map(|member| {
            Grant::recorded(
                if member.binding().recipient() == access.local().binding().recipient() {
                    "m1"
                } else {
                    "m2"
                },
                member.binding().clone(),
                Role::Reader,
            )
        })
        .collect();
    let node = admin.prepare_commons_rotation(&journal, grants).unwrap();
    journal.ingest(node.bytes()).unwrap();
    drop(journal);
    let result = admin_revoke(&lab.paths[0], &admin, "m2", false).unwrap();
    assert!(result.retirement.unwrap().changed);
    assert_eq!(
        owner.access().unwrap().unwrap().local().role(),
        Role::Reader
    );
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let recipient = peer.request().unwrap().binding().recipient().to_bech32();
    doc.add_machine("m2", &recipient).unwrap();
    admin_publish_approved(&lab.paths[0], &admin, &doc, &[recipient]).unwrap();
    let request = peer.request().unwrap();
    let (mut pending, challenge) =
        Pending::issue(&request, peer.trust().network, peer.trust().authority).unwrap();
    let response = peer.device().answer(&request, &challenge).unwrap();
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    assert!(
        admin
            .enroll_commons_member(&owner, &mut journal, pending.verify(&response).unwrap())
            .is_err()
    );
}

pub(crate) fn recipient_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let lab = Lab::new(false, Custody::Founded);
    let owner = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let request = peer.request().unwrap();
    let recipient = request.binding().recipient();
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let possession = || {
        let (mut pending, challenge) =
            Pending::issue(&request, owner.trust().network, owner.trust().authority).unwrap();
        pending
            .verify(&peer.device().answer(&request, &challenge).unwrap())
            .unwrap()
    };
    let concurrent = if action == "ConcurrentRecovery" {
        let journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        let certificate = admin
            .auto_approve_commons_member(&journal, "m2", possession())
            .unwrap();
        Some(vec![
            admin
                .prepare_commons_rotation(
                    &journal,
                    vec![
                        Grant::recorded(
                            "m1",
                            owner.request().unwrap().binding().clone(),
                            Role::Writer,
                        ),
                        Grant::certified("m2", certificate, Role::Writer),
                    ],
                )
                .unwrap(),
        ])
    } else if action == "UnboundEpoch" {
        let journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        let retained: Vec<_> = journal
            .inventory()
            .map(|id| journal.node(id).unwrap().clone())
            .collect();
        drop(journal);
        let mut other =
            Journal::open(&lab.root.path().join("offline-admin"), owner.trust()).unwrap();
        for node in retained {
            other.ingest(node.bytes()).unwrap();
        }
        let grants = || {
            vec![Grant::recorded(
                "m1",
                owner.request().unwrap().binding().clone(),
                Role::Writer,
            )]
        };
        let first = admin.prepare_commons_rotation(&other, grants()).unwrap();
        other.ingest(first.bytes()).unwrap();
        let second = admin.prepare_commons_rotation(&other, grants()).unwrap();
        Some(vec![first, second])
    } else {
        None
    };
    let retirement = admin_revoke(&lab.paths[0], &admin, "m2", false)
        .unwrap()
        .retirement
        .unwrap();
    assert_eq!(
        retirement.bindings, 0,
        "no actor was known when the recipient was retired"
    );
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let mut old_ready = false;
    if let Some(concurrent) = concurrent {
        for node in &concurrent {
            journal.ingest(node.bytes()).unwrap();
        }
        let head = concurrent.last().unwrap().id();
        let recovery = admin
            .seal_commons(
                owner.trust().network,
                Edit::Recover {
                    heads: &[retirement.head, head],
                    selected: head,
                },
            )
            .unwrap();
        journal.ingest(recovery.bytes()).unwrap();
        old_ready = owner
            .access_in(&journal)
            .is_ok_and(|access| access.is_some());
        let fresh = admin
            .prepare_commons_rotation(
                &journal,
                vec![Grant::recorded(
                    "m1",
                    owner.request().unwrap().binding().clone(),
                    Role::Writer,
                )],
            )
            .unwrap();
        journal.ingest(fresh.bytes()).unwrap();
    }
    let retired = journal
        .constraints()
        .unwrap()
        .recipient_retired(&recipient)
        .unwrap();
    let allowed = admin
        .enroll_commons_member(&owner, &mut journal, possession())
        .is_ok();
    let ready = owner
        .access_in(&journal)
        .is_ok_and(|access| access.is_some());
    let mut next = state.clone();
    for (name, value) in [
        (
            "scenario",
            match action {
                "LateBinding" => 1,
                "ConcurrentRecovery" => 2,
                "UnboundEpoch" => 3,
                _ => panic!("unknown recipient action"),
            },
        ),
        ("retired", i64::from(retired)),
        ("allowed", i64::from(allowed)),
        ("old_ready", i64::from(old_ready)),
        ("ready", i64::from(ready)),
    ] {
        next.insert(name, value);
    }
    next
}

#[test]
fn recipient_retirement_survives_delayed_binding_and_concurrent_recovery() {
    let model = aterm_models_db::recipient_retirement_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let next = recipient_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
    }
    let mut mutant = recipient_transition(&initial, "ConcurrentRecovery");
    mutant.insert("old_ready", 1);
    assert!(!model.check_invariant("OldKeyWithheld", &mutant));
}

/// A vault with no Commons control (version-1 custody, as an admin before Commons custody
/// wrote it) revokes vault-only and keeps its root key. Each control after the first changes
/// one input: Commons state on the admin's Mac, or a control entry the vault or the edit
/// names, refuses with nothing published; a device key alone does not.
#[test]
fn a_vault_without_commons_control_revokes_vault_only_and_keeps_its_root() {
    let revoke = |lab: &Lab| {
        let admin = admin_cached(&lab.paths[0]).unwrap();
        let result = admin_revoke(&lab.paths[0], &admin, "m2", false).map(|done| done.retirement);
        (admin.seq, result)
    };
    let refused = |lab: &Lab, case: &str| {
        let (seq, result) = revoke(lab);
        let Err(refused) = result else {
            panic!("{case}: revoked");
        };
        let after = admin_cached(&lab.paths[0]).unwrap();
        assert_eq!(after.seq, seq, "{case}: nothing was published");
        assert!(
            after
                .doc
                .machines()
                .iter()
                .any(|machine| machine.name == "m2"),
            "{case}"
        );
        refused.to_string()
    };
    let gone = |lab: &Lab| {
        !admin_cached(&lab.paths[0])
            .unwrap()
            .doc
            .machines()
            .iter()
            .any(|machine| machine.name == "m2")
    };

    // Revokes vault-only, and the marker stays: an admin added later cannot be demoted.
    let lab = Lab::new(false, Custody::V1);
    let authority = admin_cached(&lab.paths[0])
        .unwrap()
        .commons_authority()
        .unwrap();
    assert_eq!(revoke(&lab).1.unwrap(), None);
    assert!(gone(&lab));
    let admin = admin_cached(&lab.paths[0]).unwrap();
    assert!(admin.commons_bound);
    assert_eq!(admin.custody, admin_keys::MAGIC);
    assert_eq!(admin.commons_authority().unwrap(), authority);
    let third = Identity::from_secret_bytes([41; 32])
        .recipient()
        .to_bech32();
    let mut doc = admin.doc.clone();
    doc.add_machine("m3", &third).unwrap();
    doc.add_admin("m3").unwrap();
    admin_publish_approved(&lab.paths[0], &admin, &doc, &[third]).unwrap();
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.remove_admin("m3").unwrap();
    let demoted = admin_publish(&lab.paths[0], &admin, &doc).err().unwrap();
    assert!(demoted.to_string().contains("cannot demote"), "{demoted}");
    // There is no Commons membership to retire.
    let error = admin_retire(&lab.paths[0], &admin, &["m3".into()])
        .err()
        .unwrap();
    assert!(error.to_string().contains("no Commons control"), "{error}");

    // A version-1 vault a later build already rewrote (bound, no entry) revokes too.
    let lab = Lab::new(false, Custody::V1);
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&crate::text::Audience::Everyone, "note", b"kept")
        .unwrap();
    admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    let admin = admin_cached(&lab.paths[0]).unwrap();
    assert!(admin.commons_bound && admin.custody == admin_keys::MAGIC);
    assert_eq!(revoke(&lab).1.unwrap(), None);
    assert!(gone(&lab));

    // A device key alone is no control state.
    let lab = Lab::new(false, Custody::V1);
    std::fs::create_dir_all(lab.paths[0].dir.join("commons")).unwrap();
    std::fs::write(lab.paths[0].dir.join("commons/device.age"), b"").unwrap();
    std::fs::write(lab.paths[0].dir.join("commons/device.claim"), b"").unwrap();
    assert_eq!(revoke(&lab).1.unwrap(), None);
    assert!(gone(&lab));

    // A Commons claim on the admin's Mac refuses, the revoke and a generic removal alike.
    let lab = Lab::new(false, Custody::V1);
    std::fs::write(lab.paths[0].dir.join("commons.claim"), b"").unwrap();
    let why = refused(&lab, "a retained claim");
    assert!(why.contains("missing its descriptor"), "{why}");
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.remove_machine("m2", false).unwrap();
    let why = admin_publish(&lab.paths[0], &admin, &doc).err().unwrap();
    assert!(why.to_string().contains("custody is unavailable"), "{why}");

    // A control entry the vault names, with no descriptor yet, refuses.
    let lab = Lab::new(false, Custody::V1);
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &crate::text::Audience::Hosts(vec!["m1".into()]),
        crate::commons::ENTRY,
        b"pending",
    )
    .unwrap();
    admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    let why = refused(&lab, "a published control entry");
    assert!(why.contains("missing its descriptor"), "{why}");
    // Nor does an edit that drops that entry and the member together.
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.unset_entry(
        &crate::text::Audience::Hosts(vec!["m1".into()]),
        crate::commons::ENTRY,
    )
    .unwrap();
    doc.remove_machine("m2", false).unwrap();
    let why = admin_publish(&lab.paths[0], &admin, &doc).err().unwrap();
    assert!(why.to_string().contains("custody is unavailable"), "{why}");
    assert_eq!(admin_cached(&lab.paths[0]).unwrap().seq, admin.seq);
    assert!(!gone(&lab));

    // An edit that removes a member and adds a control entry in one publish refuses.
    let lab = Lab::new(false, Custody::V1);
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.remove_machine("m2", false).unwrap();
    doc.set_entry(
        &crate::text::Audience::Hosts(vec!["m1".into()]),
        crate::commons::ENTRY,
        b"pending",
    )
    .unwrap();
    let why = admin_publish(&lab.paths[0], &admin, &doc).err().unwrap();
    assert!(why.to_string().contains("custody is unavailable"), "{why}");
    assert_eq!(admin_cached(&lab.paths[0]).unwrap().seq, admin.seq);
    assert!(!gone(&lab));

    // An unbound vault removes vault-only whatever this Mac keeps.
    let lab = Lab::new(false, Custody::Unbound);
    std::fs::write(lab.paths[0].dir.join("commons.claim"), b"").unwrap();
    assert_eq!(revoke(&lab).1.unwrap(), None);
    assert!(gone(&lab));
}

#[test]
fn the_roster_gate_knows_its_local_version_without_hearing_itself() {
    use crate::commons::hearing::{Hold, LIFTED, Ledger, Said, Version};
    let lab = Lab::new(true, Custody::Founded);
    let owner = lab.bootstrap(0);
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let access = owner.access_in(&journal).unwrap().unwrap();
    let local = access.local().binding().actor();
    let peer = access
        .members()
        .iter()
        .find(|member| member.binding().actor() != local)
        .unwrap()
        .binding()
        .actor();
    let now = aterm_time::unix_millis().unwrap();
    let mut ledger = Ledger::new(owner.trust());
    assert!(
        admin
            .refresh_commons_roster(&owner, &mut journal, &mut ledger, now)
            .unwrap()
            .is_none()
    );
    let held = |ledger: &Ledger| {
        ledger.holding(access.members().iter().map(|member| member.binding()), now)
    };
    assert_eq!(held(&ledger), vec![(peer, Hold::Older(None))]);
    // Knowing this running build is not an authenticated peer contact or a version report.
    assert!(!ledger.watch(now));
    assert_eq!(ledger.entry(&local).unwrap().version, None);
    assert!(!ledger.lifted_within(&local, now, crate::commons::hearing::RECENT));
    // An older or unreported remote reader still holds the first larger roster back.
    ledger.record(peer, now, Said::Version(Version(0, 145, 0)));
    assert_eq!(
        held(&ledger),
        vec![(peer, Hold::Older(Some(Version(0, 145, 0))))]
    );
    ledger.record(peer, now, Said::Nothing);
    assert_eq!(held(&ledger), vec![(peer, Hold::Older(None))]);
    ledger.record(peer, now, Said::Version(LIFTED));
    assert!(held(&ledger).is_empty());
    // Enrollment and retirement reopen this ledger rather than refreshing it first.
    ledger.write(&owner).unwrap();
    let mut reopened = Ledger::read(&owner);
    assert!(held(&reopened).is_empty());
    assert!(
        !reopened.watch(now + 1),
        "a reopen needs a real peer contact"
    );
}

#[test]
fn a_member_silent_for_30_days_leaves_the_roster_and_returns_when_heard() {
    use crate::commons::hearing::{Ledger, SILENT, Said, Version};
    let lab = Lab::new(true, Custody::Founded);
    let owner = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let silent = peer.request().unwrap().binding().clone();
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let members = |journal: &Journal| {
        owner
            .access_in(journal)
            .unwrap()
            .unwrap()
            .members()
            .iter()
            .map(|member| member.binding().clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(members(&journal).len(), 2);
    let now = aterm_time::unix_millis().unwrap();
    // Negative control: a watch a day short of 30 days names nobody silent.
    let mut short = Ledger::watched(owner.trust(), now - SILENT + 86_400_000, now);
    assert!(
        admin
            .refresh_commons_roster(&owner, &mut journal, &mut short, now)
            .unwrap()
            .is_none()
    );
    // A member never heard is noted on the roster first, which starts its 30 days: without
    // the note it would read as silent through this watch.
    let mut unheard = Ledger::watched(owner.trust(), now - SILENT - 1000, now);
    assert!(unheard.silent(&silent.actor(), now));
    assert!(
        admin
            .refresh_commons_roster(&owner, &mut journal, &mut unheard, now)
            .unwrap()
            .is_none()
    );
    assert!(!unheard.silent(&silent.actor(), now));
    assert_eq!(members(&journal).len(), 2);
    // m2 unheard for 30 days through a 30-day watch leaves the roster; this Mac, never heard
    // by itself, stays.
    let mut ledger = Ledger::watched(owner.trust(), now - SILENT - 1000, now);
    ledger.record(
        silent.actor(),
        now - SILENT - 1,
        Said::Version(Version::own()),
    );
    admin
        .refresh_commons_roster(&owner, &mut journal, &mut ledger, now)
        .unwrap()
        .unwrap();
    assert_eq!(
        members(&journal),
        vec![owner.request().unwrap().binding().clone()]
    );
    assert!(!matches!(peer.access_in(&journal), Ok(Some(_))));
    // Retired from the roster, not from Commons or the vault: its binding is still good.
    let constraints = journal.constraints().unwrap();
    assert!(constraints.check_recorded_device(&silent).is_ok());
    assert!(!constraints.recipient_retired(&silent.recipient()).unwrap());
    assert!(
        admin
            .refresh_commons_roster(&owner, &mut journal, &mut ledger, now)
            .unwrap()
            .is_none()
    );
    // Heard again, it returns.
    ledger.record(silent.actor(), now, Said::Unknown);
    admin
        .refresh_commons_roster(&owner, &mut journal, &mut ledger, now)
        .unwrap()
        .unwrap();
    assert!(members(&journal).contains(&silent));
    assert!(matches!(peer.access_in(&journal), Ok(Some(_))));
}

#[test]
fn repair_half_done_revokes_expires_steward_lease_held_by_denied_member() {
    let r1 = Identity::generate().unwrap().recipient().to_bech32();
    let r2 = Identity::generate().unwrap().recipient().to_bech32();
    let mut doc = Document::new_vault("m1", &r1);
    // m2 was removed from [machines] on one branch, leaving m2 in the roster and holding the lease.
    let roster = crate::swarm_roster::SwarmRoster {
        members: vec![
            crate::swarm_roster::SwarmMember {
                label: "m1".to_owned(),
                vault_name: "m1".to_owned(),
                recipient: r1,
                node_id: None,
                node_key: None,
                hint: None,
                hw_hint: None,
                admitted_ms: 1000,
                aliases: Vec::new(),
                claimed_label: None,
                claimed_at_ms: None,
            },
            crate::swarm_roster::SwarmMember {
                label: "m2".to_owned(),
                vault_name: "m2".to_owned(),
                recipient: r2,
                node_id: None,
                node_key: None,
                hint: None,
                hw_hint: None,
                admitted_ms: 1000,
                aliases: Vec::new(),
                claimed_label: None,
                claimed_at_ms: None,
            },
        ],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::swarm_roster::ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    let lease = crate::swarm_roster::SwarmLease {
        holder: "m2".to_owned(),
        epoch: 4,
        until: 1_900_000_000,
    };
    doc.set_entry(
        &Audience::Hosts(vec!["m1".to_owned()]),
        crate::swarm_roster::LEASE_ENTRY,
        lease.to_toml().as_bytes(),
    )
    .unwrap();

    let summary = repair_half_done_revokes(&mut doc)
        .unwrap()
        .expect("should repair stale m2 member and expire lease");
    assert!(summary.contains("retired m2"), "summary: {summary}");
    assert!(
        summary.contains("expired steward lease"),
        "summary: {summary}"
    );
    let updated_lease = crate::swarm_roster::SwarmLease::from_document(&doc)
        .unwrap()
        .expect("lease entry preserved");
    assert_eq!(updated_lease.epoch, 4, "epoch must be preserved");
    assert_eq!(updated_lease.until, 0, "until must be expired to 0");
    assert!(
        repair_half_done_revokes(&mut doc).unwrap().is_none(),
        "idempotent after repair"
    );
}

#[test]
fn repair_half_done_revokes_retires_node_denied_member_and_strips_pool_and_lease() {
    let r1 = Identity::generate().unwrap().recipient().to_bech32();
    let r2 = Identity::generate().unwrap().recipient().to_bech32();
    let mut doc = Document::new_vault("m1", &r1);
    doc.add_machine("m2", &r2).unwrap();
    let nk2 = "dd".repeat(32);
    let roster = crate::swarm_roster::SwarmRoster {
        members: vec![
            crate::swarm_roster::SwarmMember {
                label: "m1".to_owned(),
                vault_name: "m1".to_owned(),
                recipient: r1,
                node_id: None,
                node_key: None,
                hint: None,
                hw_hint: None,
                admitted_ms: 1000,
                aliases: Vec::new(),
                claimed_label: None,
                claimed_at_ms: None,
            },
            crate::swarm_roster::SwarmMember {
                label: "m2".to_owned(),
                vault_name: "m2".to_owned(),
                recipient: r2,
                node_id: None,
                node_key: Some(nk2.clone()),
                hint: None,
                hw_hint: None,
                admitted_ms: 1000,
                aliases: vec!["m20".to_owned()],
                claimed_label: None,
                claimed_at_ms: None,
            },
        ],
        deny_labels: Vec::new(),
        deny_nodes: vec![nk2],
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::swarm_roster::ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    let pool_toml = r#"version = 1

[[account]]
provider = "claude"
email = "a@example.test"
warm = ["m2", "m1"]
"#;
    doc.set_entry(&Audience::Everyone, AI_POOL_ENTRY, pool_toml.as_bytes())
        .unwrap();
    let lease = crate::swarm_roster::SwarmLease {
        holder: "m2".to_owned(),
        epoch: 7,
        until: 1_900_000_000,
    };
    doc.set_entry(
        &Audience::Hosts(vec!["m1".to_owned()]),
        crate::swarm_roster::LEASE_ENTRY,
        lease.to_toml().as_bytes(),
    )
    .unwrap();

    let summary = repair_half_done_revokes(&mut doc)
        .unwrap()
        .expect("node-denied member must be retired and cascaded");
    assert!(summary.contains("retired m2"), "summary: {summary}");
    assert!(
        summary.contains("released denied warm slots"),
        "summary: {summary}"
    );
    assert!(
        summary.contains("expired steward lease"),
        "summary: {summary}"
    );
    let updated_roster = crate::swarm_roster::SwarmRoster::from_document(&doc)
        .unwrap()
        .unwrap();
    assert!(updated_roster.member_by_label_or_alias("m2").is_none());
    assert!(updated_roster.is_label_denied("m2"));
    assert!(updated_roster.is_label_denied("m20"));
    let updated_lease = crate::swarm_roster::SwarmLease::from_document(&doc)
        .unwrap()
        .unwrap();
    assert_eq!(updated_lease.until, 0);
    assert!(repair_half_done_revokes(&mut doc).unwrap().is_none());
}

#[test]
fn repair_and_cascade_protect_surviving_canonical_label_from_suffixed_stale_claim() {
    let r1 = Identity::generate().unwrap().recipient().to_bech32();
    let r2 = Identity::generate().unwrap();
    let r2_str = r2.recipient().to_bech32();
    let mut doc = Document::new_vault("m1", &r1);
    // m2 is not in [machines] (stale), and has a clash-suffixed `claimed_label = Some("m1-3fa2")`
    // whose canonical `leading_m_label` is `"m1"` (held by active `m1`).
    let roster = crate::swarm_roster::SwarmRoster {
        members: vec![
            crate::swarm_roster::SwarmMember {
                label: "m1".to_owned(),
                vault_name: "m1".to_owned(),
                recipient: r1.clone(),
                node_id: None,
                node_key: None,
                hint: None,
                hw_hint: None,
                admitted_ms: 1000,
                aliases: Vec::new(),
                claimed_label: None,
                claimed_at_ms: None,
            },
            crate::swarm_roster::SwarmMember {
                label: "m2".to_owned(),
                vault_name: "m2".to_owned(),
                recipient: r2_str.clone(),
                node_id: None,
                node_key: None,
                hint: None,
                hw_hint: None,
                admitted_ms: 1000,
                aliases: Vec::new(),
                claimed_label: Some("m1-3fa2".to_owned()),
                claimed_at_ms: Some(1000),
            },
        ],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::swarm_roster::ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();

    // 1. `cascade_member_removal` on `--stolen` for `r2` must deny `m2` without folding
    //    `m1-3fa2` into `deny_labels` as `"m1"` and denying surviving `m1`.
    let mut entries = std::collections::BTreeMap::new();
    entries.insert(
        crate::swarm_roster::ROSTER_ENTRY.to_owned(),
        Entry {
            name: crate::swarm_roster::ROSTER_ENTRY.to_owned(),
            value: SecretBytes::from(roster.to_toml()),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        },
    );
    super::cascade_member_removal(
        &mut entries,
        &r2.recipient(),
        &std::collections::BTreeSet::from(["m2".to_owned()]),
        &std::collections::BTreeSet::new(),
        true,
    )
    .unwrap();
    let cascaded = crate::swarm_roster::SwarmRoster::from_toml(
        std::str::from_utf8(entries[crate::swarm_roster::ROSTER_ENTRY].value.as_slice()).unwrap(),
    )
    .unwrap();
    assert!(cascaded.is_label_denied("m2"));
    assert!(
        !cascaded.is_label_denied("m1"),
        "surviving m1 must not be collateral-denied by m2's suffixed claim: {:?}",
        cascaded.deny_labels
    );

    // 2. `repair_half_done_revokes` must also retire stale `m2` without denying surviving `m1`.
    repair_half_done_revokes(&mut doc)
        .unwrap()
        .expect("should retire stale m2");
    let repaired = crate::swarm_roster::SwarmRoster::from_document(&doc)
        .unwrap()
        .unwrap();
    assert!(repaired.is_label_denied("m2"));
    assert!(
        !repaired.is_label_denied("m1"),
        "surviving m1 must not be collateral-denied by repair_half_done_revokes: {:?}",
        repaired.deny_labels
    );

    // 3. When a stale row (`m2`) shares `node_id` / `node_key` with surviving `m1` (e.g. after `m1`
    //    rotated its age recipient), `repair_half_done_revokes` must retire `m2` without pushing
    //    `m1`'s active `node_id` / `node_key` into `deny_nodes`.
    let shared_node = "00000000000000000000000000000001";
    let shared_nk = "11".repeat(32);
    let shared_roster = crate::swarm_roster::SwarmRoster {
        members: vec![
            crate::swarm_roster::SwarmMember {
                label: "m1".to_owned(),
                vault_name: "m1".to_owned(),
                recipient: r1,
                node_id: Some(shared_node.to_owned()),
                node_key: Some(shared_nk.clone()),
                hint: None,
                hw_hint: None,
                admitted_ms: 2000,
                aliases: Vec::new(),
                claimed_label: None,
                claimed_at_ms: None,
            },
            crate::swarm_roster::SwarmMember {
                label: "m2".to_owned(),
                vault_name: "m2".to_owned(),
                recipient: r2_str,
                node_id: Some(shared_node.to_owned()),
                node_key: Some(shared_nk.clone()),
                hint: None,
                hw_hint: None,
                admitted_ms: 1000,
                aliases: Vec::new(),
                claimed_label: None,
                claimed_at_ms: None,
            },
        ],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::swarm_roster::ROSTER_ENTRY,
        shared_roster.to_toml().as_bytes(),
    )
    .unwrap();
    repair_half_done_revokes(&mut doc)
        .unwrap()
        .expect("should retire stale m2");
    let repaired_shared = crate::swarm_roster::SwarmRoster::from_document(&doc)
        .unwrap()
        .unwrap();
    assert!(!repaired_shared.is_node_denied(shared_node));
    assert!(!repaired_shared.is_node_denied(&shared_nk));
    assert!(
        !repaired_shared.is_member_denied(repaired_shared.member_by_label_or_alias("m1").unwrap())
    );

    // 4. Even when surviving `m1` is in `doc.machines()` only (not yet seeded into `roster.members`),
    //    `repair_half_done_revokes` retiring stale `m2` (which carries `claimed_label = Some("m1")` or
    //    `Some("m1-3fa2")`) must NOT collateral-deny `m1`.
    let unseeded_m1_roster = crate::swarm_roster::SwarmRoster {
        members: vec![crate::swarm_roster::SwarmMember {
            label: "m2".to_owned(),
            vault_name: "m2".to_owned(),
            recipient: Identity::generate().unwrap().recipient().to_bech32(),
            node_id: None,
            node_key: None,
            hint: None,
            hw_hint: None,
            admitted_ms: 1000,
            aliases: Vec::new(),
            claimed_label: Some("m1".to_owned()),
            claimed_at_ms: Some(1000),
        }],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::swarm_roster::ROSTER_ENTRY,
        unseeded_m1_roster.to_toml().as_bytes(),
    )
    .unwrap();
    repair_half_done_revokes(&mut doc)
        .unwrap()
        .expect("should retire stale m2");
    let repaired_unseeded = crate::swarm_roster::SwarmRoster::from_document(&doc)
        .unwrap()
        .unwrap();
    assert!(repaired_unseeded.is_label_denied("m2"));
    assert!(
        !repaired_unseeded.is_label_denied("m1"),
        "surviving m1 in doc.machines() must not be collateral-denied by stale m2's claimed_label: {:?}",
        repaired_unseeded.deny_labels
    );
}

/// A plain removal (no `--stolen`) rotates the fleet code too, so the removed Mac's copy of it
/// lets it into neither the fleet nor the vault by the fleet's key (`client::readmit`), while it
/// still denies nothing on the roster (an "Allow again" restores it).
///
/// NEGATIVE CONTROL: the rotation gated on `stolen` again (`if stolen && let Some(entry) = …`):
/// the code is unchanged and `assert!(cascade.fleet_code_rotated)` fails.
#[test]
fn a_plain_removal_rotates_the_fleet_code_and_denies_nothing() {
    let removed = crate::age::Identity::generate().unwrap().recipient();
    let code = encode_fleet_join_code(&[7; FLEET_SECRET_LEN], Some("10.0.0.2:7878"));
    let roster = crate::swarm_roster::SwarmRoster {
        members: vec![crate::swarm_roster::SwarmMember {
            label: "m9".to_owned(),
            vault_name: "m9".to_owned(),
            recipient: removed.to_bech32(),
            node_id: Some("00000000000000000000000000000009".to_owned()),
            node_key: Some("09".repeat(32)),
            hint: None,
            hw_hint: None,
            admitted_ms: 1000,
            aliases: Vec::new(),
            claimed_label: None,
            claimed_at_ms: None,
        }],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };
    let entry = |name: &str, value: String| {
        (
            name.to_owned(),
            Entry {
                name: name.to_owned(),
                value: SecretBytes::from(value),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            },
        )
    };
    let mut entries = std::collections::BTreeMap::from([
        entry(fleet::FLEET_CODE_ENTRY, code.clone()),
        entry(crate::swarm_roster::ROSTER_ENTRY, roster.to_toml()),
    ]);
    let cascade = super::cascade_member_removal(
        &mut entries,
        &removed,
        &std::collections::BTreeSet::from(["m9".to_owned()]),
        &std::collections::BTreeSet::from(["09".repeat(32)]),
        false,
    )
    .unwrap();
    let rotated = std::str::from_utf8(entries[fleet::FLEET_CODE_ENTRY].value.as_slice())
        .unwrap()
        .to_owned();
    assert!(cascade.fleet_code_rotated);
    assert_ne!(rotated, code, "the removed Mac's copy of the code is stale");
    assert!(
        rotated.ends_with("@10.0.0.2:7878"),
        "the rotated code keeps its place: {}",
        rotated.len()
    );
    let after = crate::swarm_roster::SwarmRoster::from_toml(
        std::str::from_utf8(entries[crate::swarm_roster::ROSTER_ENTRY].value.as_slice()).unwrap(),
    )
    .unwrap();
    assert!(after.deny_labels.is_empty() && after.deny_nodes.is_empty());
    assert!(after.members.is_empty(), "its roster row goes");
}
