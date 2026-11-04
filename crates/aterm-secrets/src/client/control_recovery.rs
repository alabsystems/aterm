// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit restoration of an existing published Commons policy. Planning is
//! read-only; execution is bound to that exact vault and membership snapshot.

use super::*;
use crate::commons::{
    bootstrap::Descriptor,
    journal::{Journal, Trust},
};
use crate::membership_log::{
    Body, Budget, EpochAudience, Log, MemberId, Record, RecordBudgets, RecordCounts, RecordId,
    Store,
};

/// An existing recipient retained or restored by the published control policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommonsRecoveryRecipient {
    pub recipient: String,
    pub label: Option<String>,
    pub restored: bool,
}

/// Exact record-count cost, before signing. A fitting count does not guarantee
/// the separate byte budget; the atomic commit still checks every limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommonsRecoveryCapacity {
    pub current: RecordCounts,
    pub additional: RecordCounts,
    pub budgets: RecordBudgets,
    pub exceeded: Option<Budget>,
}

/// Value-free proposal. Its digest binds execution to the exact authenticated
/// vault, root, current log heads and recipients; it grants no access itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommonsRecoveryPlan {
    pub digest: [u8; 32],
    pub trust: Trust,
    pub vault_version: u64,
    pub current_epoch: RecordId,
    pub recipients: Vec<CommonsRecoveryRecipient>,
    pub needed: bool,
    /// Whether the descriptor must come from the authenticated published vault.
    pub published_descriptor: bool,
    pub record_capacity: CommonsRecoveryCapacity,
}

/// Result of an explicitly requested, snapshot-bound restoration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommonsRecovery {
    pub trust: Trust,
    pub epoch: RecordId,
    pub recipients: usize,
    pub changed: bool,
}

struct Context {
    plan: CommonsRecoveryPlan,
    store: Store,
    log: Log,
    descriptor: Descriptor,
    recipients: BTreeSet<Recipient>,
    actor: MemberId,
    device: crate::commons::bootstrap::Device,
    incarnation: [u8; 16],
    identity: Identity,
    custody: Vec<(PathBuf, [u8; 32])>,
    journal_ids: BTreeSet<RecordId>,
}

fn custody(paths: &Paths) -> crate::Result<Vec<(PathBuf, [u8; 32])>> {
    [
        paths.source_file(),
        paths.state_file(),
        paths.cache_file(),
        paths.identity_file(),
        paths.dir.join("commons/device.age"),
        paths.dir.join("commons/device.claim"),
        paths.dir.join("commons/default.pin"),
        paths.dir.join("commons.claim"),
        paths.dir.join("commons/control/control.anchor"),
    ]
    .into_iter()
    .map(|path| {
        let bytes = fsx::read_private(&path, envelope::MAX_ENVELOPE)?
            .ok_or_else(|| Error::new("Commons recovery custody is incomplete"))?;
        Ok((path, aterm_digest::Sha256::digest(bytes.as_slice())))
    })
    .collect()
}

fn context(paths: &Paths, lock: &StateLock) -> crate::Result<Context> {
    let before = custody(paths)?;
    let admin = super::enrollment::admin_cached_unlocked(paths)?;
    if !admin.commons_bound {
        return Err(Error::new(
            "Commons recovery requires an already bound authority",
        ));
    }
    let mut descriptor =
        admin.published_control.descriptor.clone().ok_or_else(|| {
            Error::new("Commons recovery requires the published typed descriptor")
        })?;
    let trust = descriptor.trust();
    if trust.authority != admin.commons_authority()? {
        return Err(Error::new(
            "Commons recovery has another published authority",
        ));
    }
    let (store, _) = log_entries::trusted_seeded_store(
        paths,
        &admin.config,
        &envelope::signer_public(&admin.seed)?,
        &admin.handoff,
    )?
    .ok_or_else(|| Error::new("Commons recovery requires the existing authenticated log"))?;
    let log = store
        .log()
        .cloned()
        .ok_or_else(|| Error::new("Commons recovery log has no genesis"))?;
    if log.waiting() != 0 {
        return Err(Error::new(
            "Commons recovery requires complete causal retention",
        ));
    }
    if !matches!(log.genesis().body(), Body::Genesis { root, .. }
        if root.network == trust.network && root.authority == trust.authority)
    {
        return Err(Error::new(
            "Commons recovery cannot replace the existing root",
        ));
    }
    let journal = crate::commons::bootstrap::validate_established_root(paths, trust)?;
    let journal_ids: BTreeSet<_> = journal.inventory().copied().collect();
    // Restoring access is a positive grant: all locally retained control
    // dependencies must be present, with one unambiguous current head.
    let constraints = journal.constraints()?;
    let (actor, device, incarnation) =
        log_entries::open_local_member_signer_held(paths, &log, lock)?
            .ok_or_else(|| Error::new("Commons recovery requires an active local member"))?;
    let identity = read_identity(paths)?
        .ok_or_else(|| Error::new("Commons recovery requires the existing identity"))?;
    if !admin
        .published_control
        .recipients
        .contains_key(&identity.recipient())
    {
        return Err(Error::new(
            "the published control policy does not include this admin",
        ));
    }
    let opened = log
        .open_audience_entries(&identity)?
        .ok_or_else(|| Error::new("Commons recovery cannot read the membership policy"))?;
    if !opened
        .fully_unwrapped_audiences
        .contains(&EpochAudience::Everyone)
    {
        return Err(Error::new(
            "Commons recovery cannot read all current shared policy",
        ));
    }
    let published_descriptor = opened.commons_control.is_none();
    if let Some(control) = &opened.commons_control {
        let current = Descriptor::decode(control.as_slice())?;
        if current.trust() != trust {
            return Err(Error::new("current Commons control has another root"));
        }
        descriptor = current;
    }
    let current = opened
        .maximal_epochs
        .get(&EpochAudience::Commons)
        .ok_or_else(|| Error::new("Commons recovery requires an existing control epoch"))?;
    let [current_epoch] = current.as_slice() else {
        return Err(Error::new(
            "concurrent Commons epochs need reconciliation before recovery",
        ));
    };
    let existing = opened
        .wrapped_recipients
        .get(current_epoch)
        .cloned()
        .unwrap_or_default();
    let mut retired = BTreeSet::new();
    for recipient in existing
        .iter()
        .chain(admin.published_control.recipients.keys())
    {
        if constraints.recipient_retired(recipient)? {
            retired.insert(*recipient);
        }
    }
    let roster = log_entries::local_swarm_roster_for_log_held(paths, &log, lock)?;
    let permits = |recipient: &Recipient| {
        !log.recipient_is_removed(recipient)
            && !retired.contains(recipient)
            && !log.audience_left_out_recipient(&EpochAudience::Everyone, recipient)
            && !log_entries::recipient_denied_by_roster_or_log(roster.as_ref(), &log, recipient)
    };
    // Retain current recipients, including a key whose old label is no longer
    // available. Recovery must not implicitly revoke another current reader.
    let mut recipients: BTreeSet<_> = existing.iter().copied().filter(permits).collect();
    let membership = log.membership();
    for (member_actor, member) in membership.members() {
        let recipient = member.binding.recipient();
        if admin.published_control.recipients.contains_key(&recipient)
            && permits(&recipient)
            && !membership.member_node_removed(member)
            && !log.audience_left_out(&EpochAudience::Everyone, member_actor)
        {
            recipients.insert(recipient);
        }
    }
    if !recipients.contains(&identity.recipient()) {
        return Err(Error::new("Commons recovery cannot restore a denied admin"));
    }
    let targets: Vec<_> = recipients
        .iter()
        .map(|recipient| CommonsRecoveryRecipient {
            recipient: recipient.to_bech32(),
            label: admin
                .published_control
                .recipients
                .get(recipient)
                .cloned()
                .or_else(|| {
                    membership
                        .members()
                        .values()
                        .find(|member| member.binding.recipient() == *recipient)
                        .map(|member| member.label.clone())
                }),
            restored: !existing.contains(recipient),
        })
        .collect();
    let state = load_state(paths)?;
    let needed = opened.commons_control.is_none() || targets.iter().any(|target| target.restored);
    let additional = RecordCounts {
        weighed: 0,
        // One epoch, one wrap per recipient, and the descriptor Put.
        keyed: if needed { recipients.len() + 2 } else { 0 },
    };
    let budgets = log.budgets();
    let record_capacity = CommonsRecoveryCapacity {
        current: log.counts(),
        additional,
        budgets,
        exceeded: budgets.passed_by(log.counts(), additional),
    };
    let vault_digest = state
        .digest
        .ok_or_else(|| Error::new("Commons recovery needs an authenticated vault digest"))?;
    let mut bound = b"aterm/commons/published-control-recovery/1\0".to_vec();
    bound.extend_from_slice(&vault_digest);
    bound.extend_from_slice(&admin.seq().to_le_bytes());
    bound.extend_from_slice(&trust.network);
    bound.extend_from_slice(&trust.authority);
    bound.extend_from_slice(&trust.genesis);
    bound.extend_from_slice(&actor);
    for limit in [budgets.held, budgets.weighed, budgets.keyed] {
        bound.extend_from_slice(&(limit as u64).to_le_bytes());
    }
    bound.extend_from_slice(&aterm_digest::Sha256::digest(descriptor.bytes()));
    let heads = log.heads();
    bound.extend_from_slice(&(heads.len() as u64).to_le_bytes());
    for head in heads {
        bound.extend_from_slice(&head);
    }
    bound.extend_from_slice(&(journal_ids.len() as u64).to_le_bytes());
    for node in &journal_ids {
        bound.extend_from_slice(node);
    }
    bound.extend_from_slice(&(recipients.len() as u64).to_le_bytes());
    for recipient in &recipients {
        bound.extend_from_slice(&recipient.0);
    }
    let plan = CommonsRecoveryPlan {
        digest: aterm_digest::Sha256::digest(&bound),
        trust,
        vault_version: admin.seq(),
        current_epoch: *current_epoch,
        needed,
        recipients: targets,
        published_descriptor,
        record_capacity,
    };
    if custody(paths)? != before {
        return Err(Error::new(
            "Commons recovery custody changed during observation",
        ));
    }
    Ok(Context {
        plan,
        store,
        log,
        descriptor,
        recipients,
        actor,
        device,
        incarnation,
        identity,
        custody: before,
        journal_ids,
    })
}

fn observe_context(paths: &Paths) -> crate::Result<Context> {
    let lock = StateLock::observe(paths)?
        .ok_or_else(|| Error::new("Commons recovery observation is busy"))?;
    context(paths, &lock)
}

/// Plan restoration from the already-applied signed vault without creating an
/// identity, writing a record, contacting a peer or granting access.
pub fn plan_commons_control_recovery(paths: &Paths) -> crate::Result<CommonsRecoveryPlan> {
    observe_context(paths).map(|context| context.plan)
}

/// Restore control access to current readers and eligible existing members
/// named by the published policy, preserving a visible current descriptor or
/// otherwise using the authenticated published one. The caller supplies the exact
/// digest from a reviewed plan. A changed vault or log refuses before signing.
/// Explicit removals, denied nodes and members outside shared policy stay out.
/// The complete new epoch, wraps and control Put are committed as one batch;
/// no prior record, table, root or identity is replaced.
pub fn restore_commons_control(
    paths: &Paths,
    expected: &[u8; 32],
) -> crate::Result<CommonsRecovery> {
    let mut context = observe_context(paths)?;
    if !aterm_digest::ct_eq(&context.plan.digest, expected) {
        return Err(Error::new(
            "Commons recovery plan changed; inspect a new plan",
        ));
    }
    if let Some(budget) = context.plan.record_capacity.exceeded {
        return Err(Error::of_kind(
            crate::ErrorKind::MembershipCapacity,
            format!(
                "Commons recovery exceeds its {} budget: {} retained records plus {} recovery records; inspect the plan's record_capacity",
                budget.counts(),
                context.plan.record_capacity.current.held(),
                context.plan.record_capacity.additional.held(),
            ),
        ));
    }
    #[cfg(test)]
    tests::before_lock();
    let result = {
        let _author = log_entries::RecordsAuthorLock::take(paths)?;
        let lock = StateLock::take(paths)?;
        if custody(paths)? != context.custody
            || load_records(paths)?.inventory() != context.store.inventory()
        {
            return Err(Error::new(
                "Commons recovery plan changed; inspect a new plan",
            ));
        }
        // Match retirement's state-before-journal ordering, then keep the
        // independent control writer through the membership commit. A read-only
        // inventory check alone would allow a retirement to land after it.
        let journal =
            Journal::open_existing(&paths.dir.join("commons/control"), context.plan.trust)?;
        if journal.inventory().copied().collect::<BTreeSet<_>>() != context.journal_ids {
            return Err(Error::new(
                "Commons control history changed; inspect a new plan",
            ));
        }
        let (actor, _, incarnation) =
            log_entries::open_local_member_signer_held(paths, &context.log, &lock)?
                .ok_or_else(|| Error::new("Commons recovery member changed"))?;
        if actor != context.actor || incarnation != context.incarnation {
            return Err(Error::new("Commons recovery member changed"));
        }
        let roster = log_entries::local_swarm_roster_for_log_held(paths, &context.log, &lock)?;
        if context.recipients.iter().any(|recipient| {
            context.log.recipient_is_removed(recipient)
                || context
                    .log
                    .audience_left_out_recipient(&EpochAudience::Everyone, recipient)
                || log_entries::recipient_denied_by_roster_or_log(
                    roster.as_ref(),
                    &context.log,
                    recipient,
                )
        }) {
            return Err(Error::new("Commons recovery recipient is now denied"));
        }
        if !context.plan.needed {
            return Ok(CommonsRecovery {
                trust: context.plan.trust,
                epoch: context.plan.current_epoch,
                recipients: context.recipients.len(),
                changed: false,
            });
        }
        #[cfg(test)]
        tests::before_commit();
        let opened = context
            .log
            .open_audience_entries(&context.identity)?
            .ok_or_else(|| Error::new("Commons recovery policy became unavailable"))?;
        let generation = opened.max_generation[&EpochAudience::Commons]
            .checked_add(1)
            .ok_or_else(|| Error::new("Commons epoch generation is exhausted"))?;
        let now = aterm_time::unix_millis()
            .ok_or_else(|| Error::new("Commons recovery needs the current time"))?;
        let records = context
            .device
            .with_seed(|seed| -> crate::Result<Vec<Record>> {
                let (epoch, key) = context.log.draft_epoch(
                    &context.actor,
                    now,
                    EpochAudience::Commons,
                    generation,
                )?;
                let epoch = epoch.sign(seed, context.incarnation)?;
                let id = *epoch.id();
                context.log.insert(epoch.clone())?;
                let mut records = vec![epoch];
                for recipient in &context.recipients {
                    let wrap = context
                        .log
                        .draft_wrap(&context.actor, now, id, &key, *recipient)?
                        .sign(seed, context.incarnation)?;
                    context.log.insert(wrap.clone())?;
                    records.push(wrap);
                }
                let put = context
                    .log
                    .draft_commons_put(&context.actor, now, id, &key, &context.descriptor.bytes())?
                    .sign(seed, context.incarnation)?;
                context.log.insert(put.clone())?;
                records.push(put);
                Ok(records)
            })?;
        let epoch = *records[0].id();
        log_entries::commit_authored_records(
            paths,
            &mut context.store,
            &crate::membership_log::encode_records(&records),
            &records,
        )?;
        CommonsRecovery {
            trust: context.plan.trust,
            epoch,
            recipients: context.recipients.len(),
            changed: true,
        }
    };
    let _ = post_records_to_servers(paths);
    Ok(result)
}

#[cfg(test)]
mod tests {
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
        let mut journal =
            Journal::open_existing(&owner.dir.join("commons/control"), trust).unwrap();
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
        let mut journal =
            Journal::open_existing(&macs.m22.dir.join("commons/control"), trust).unwrap();
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
}
