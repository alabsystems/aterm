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
    let admin = super::enrollment::admin_cached_held(paths, lock)?;
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
#[path = "control_recovery_tests.rs"]
mod tests;
