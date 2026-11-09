// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact, local account claim/fence facts. Nothing here restores live authority.

use super::*;

/// The immutable relation shared by an account claim and its no-claim fence.
/// These references may contain secrets; observations never return their values.
#[derive(Clone, Copy)]
pub struct AccountRelation<'a> {
    /// Original, immutable predecessor recipe.
    pub predecessor: &'a Prepare,
    /// Exact capability issued for that predecessor.
    pub predecessor_binding: &'a Binding,
    /// Explicit phase in the predecessor's committed NoExec record.
    pub phase: ImagePhase,
    /// Immutable candidate recipe; retries cannot change any field.
    pub candidate: &'a Prepare,
}

impl std::fmt::Debug for AccountRelation<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountRelation(redacted)")
    }
}

/// A claimed child's recorded lifecycle, not permission to execute it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimedState {
    Prepared,
    MayExecute,
    /// A claimed child's NoExec is explicitly phased and cannot lose rollback.
    NoExec {
        phase: ImagePhase,
    },
    Sealed {
        may_execute: bool,
    },
}

/// A permanent outcome for one exact candidate relation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountOutcome {
    Claimed(ClaimedState),
    Fenced,
}

/// The original claim/fence's position and time, with its current classification.
/// No field is a native capability or a grant reconstructed from disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountObservation {
    pub sequence: u64,
    pub recorded_ms: u64,
    pub outcome: AccountOutcome,
}

#[derive(Clone, Copy)]
pub(super) struct Link {
    pub(super) phase: ImagePhase,
    sequence: u64,
    recorded_ms: u64,
}

pub(super) struct Bound {
    predecessor: Receipt,
    candidate: Digest,
    prepare: Digest,
    epoch: Digest,
    phase: ImagePhase,
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountJournal",
        action = "LegacyParent",
        reason = "The projection selects the immutable parent format, request identity or physical capacity"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountJournal",
        action = "WrongPhase",
        reason = "The projection selects the immutable parent format, request identity or physical capacity"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountJournal",
        action = "RightPhase",
        reason = "The projection selects the immutable parent format, request identity or physical capacity"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountJournal",
        action = "WrongBinding",
        reason = "The projection selects the immutable parent format, request identity or physical capacity"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountJournal",
        action = "RightBinding",
        reason = "The projection selects the immutable parent format, request identity or physical capacity"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountJournal",
        action = "TightRecords",
        reason = "The projection selects the immutable parent format, request identity or physical capacity"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountJournal",
        action = "TightBytes",
        reason = "The projection selects the immutable parent format, request identity or physical capacity"
    )
)]
fn valid_relation(relation: AccountRelation<'_>) -> bool {
    valid_pair(relation.predecessor, relation.predecessor_binding)
        && relation
            .predecessor
            .account_successor_recipe(relation.candidate, relation.phase)
}

impl Bound {
    pub(super) fn new(key: &Key, relation: AccountRelation<'_>) -> io::Result<Self> {
        if !valid_relation(relation) {
            return Err(invalid());
        }
        Ok(Self {
            predecessor: receipt(key, relation.predecessor, relation.predecessor_binding)?,
            candidate: key.operation(&relation.candidate.id),
            prepare: key.prepare(relation.candidate)?,
            epoch: key.digest(b"epoch\0", relation.predecessor.host_epoch.as_bytes()),
            phase: relation.phase,
        })
    }

    fn parent<'a>(&self, view: &'a View) -> io::Result<&'a Entry> {
        view.entries
            .get(&self.predecessor.operation)
            .filter(|parent| {
                parent.receipt == self.predecessor
                    && parent.state == IssuerState::NoExec
                    && parent.epoch == Some(self.epoch)
                    && parent.closed_phase == Some(self.phase)
            })
            .ok_or_else(invalid)
    }

    fn observation(&self, view: &View) -> Option<AccountObservation> {
        let parent = self.parent(view).ok()?;
        let entry = view.entries.get(&self.candidate)?;
        let link = entry.account?;
        if entry.receipt.prepare != self.prepare
            || entry.epoch != Some(self.epoch)
            || entry.predecessor != Some(self.predecessor.operation)
            || link.phase != self.phase
        {
            return None;
        }
        let outcome = match entry.state {
            IssuerState::Fenced if parent.successor != Some(self.candidate) => {
                AccountOutcome::Fenced
            }
            IssuerState::Fenced | IssuerState::Rejected => return None,
            state if parent.successor == Some(self.candidate) => {
                AccountOutcome::Claimed(match state {
                    IssuerState::Prepared => ClaimedState::Prepared,
                    IssuerState::MayExecute => ClaimedState::MayExecute,
                    IssuerState::NoExec => ClaimedState::NoExec {
                        phase: entry.closed_phase?,
                    },
                    IssuerState::Sealed { may_execute } => ClaimedState::Sealed { may_execute },
                    IssuerState::Fenced | IssuerState::Rejected => unreachable!(),
                })
            }
            _ => return None,
        };
        Some(AccountObservation {
            sequence: link.sequence,
            recorded_ms: link.recorded_ms,
            outcome,
        })
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClosurePhase",
        action = "InitialRollback",
        reason = "The authenticated claim fixes the child initial phase before closure"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClosurePhase",
        action = "ProposeForward",
        reason = "The caller proposes the closure phase"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClosurePhase",
        action = "ProposeRollback",
        reason = "The caller proposes the closure phase"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountClosurePhase",
        action = "Check",
        project = "aterm_agent::harness::native_receipts::tests::accounts::closure_phase_transition"
    )
)]
pub(super) fn permits_noexec(entry: &Entry, phase: Option<ImagePhase>) -> bool {
    entry.account.is_none_or(|link| {
        phase
            .is_some_and(|phase| link.phase == ImagePhase::Forward || phase == ImagePhase::Rollback)
    })
}

pub(super) fn encode(
    view: &View,
    key: &Key,
    bound: &Bound,
    receipt: &Receipt,
    now_ms: u64,
    fence: bool,
) -> Vec<u8> {
    let mut body = view.encode_successor(
        key,
        receipt,
        bound.epoch,
        bound.predecessor.operation,
        now_ms,
    );
    body.truncate(SUCCESSOR_BYTES - 32);
    body[..4].copy_from_slice(if fence { b"ANF4" } else { b"ANS4" });
    body[148] = if fence { 7 } else { Event::Prepare as u8 };
    body.push(match bound.phase {
        ImagePhase::Forward => 1,
        ImagePhase::Rollback => 2,
    });
    body.extend_from_slice(hmac::sign(&key.mac, &body).as_ref());
    body
}

pub(super) fn ingest(view: &mut View, key: &Key, body: &[u8]) -> io::Result<()> {
    if body.len() != ACCOUNT_BYTES {
        return Err(invalid());
    }
    let fence = &body[..4] == b"ANF4";
    if (!fence && &body[..4] != b"ANS4")
        || body[148] != if fence { 7 } else { Event::Prepare as u8 }
    {
        return Err(invalid());
    }
    hmac::verify(
        &key.mac,
        &body[..ACCOUNT_BYTES - 32],
        &body[ACCOUNT_BYTES - 32..],
    )
    .map_err(|_| invalid())?;
    let number = |at| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
    let digest = |at| -> Digest { body[at..at + 32].try_into().unwrap() };
    if number(4) != view.sequence || digest(20) != view.head {
        return Err(invalid());
    }
    let phase = match body[213] {
        1 => ImagePhase::Forward,
        2 => ImagePhase::Rollback,
        _ => return Err(invalid()),
    };
    let receipt = Receipt {
        operation: digest(52),
        prepare: digest(84),
        capability: digest(116),
    };
    let epoch = digest(149);
    let predecessor = digest(181);
    let parent = view.entries.get(&predecessor).ok_or_else(invalid)?;
    if view.entries.contains_key(&receipt.operation)
        || parent.state != IssuerState::NoExec
        || parent.closed_phase != Some(phase)
        || parent.epoch != Some(epoch)
        || (!fence && parent.successor.is_some())
        || (fence && (receipt.capability != [0; 32] || parent.successor == Some(receipt.operation)))
    {
        return Err(invalid());
    }
    let sequence = view.sequence.checked_add(1).ok_or_else(invalid)?;
    let committed_bytes = view
        .committed_bytes
        .checked_add(if fence { FENCE_FRAME } else { ACCOUNT_FRAME })
        .ok_or_else(invalid)?;
    if !fence {
        view.entries.get_mut(&predecessor).unwrap().successor = Some(receipt.operation);
    }
    let link = Link {
        phase,
        sequence: view.sequence,
        recorded_ms: number(12),
    };
    view.entries.insert(
        receipt.operation,
        Entry {
            receipt,
            observed: Observed {
                sequence: link.sequence,
                recorded_ms: link.recorded_ms,
            },
            epoch: Some(epoch),
            state: if fence {
                IssuerState::Fenced
            } else {
                IssuerState::Prepared
            },
            predecessor: Some(predecessor),
            successor: None,
            closed_phase: None,
            account: Some(link),
        },
    );
    view.sequence = sequence;
    view.committed_bytes = committed_bytes;
    view.head = key.digest(b"receipt\0", body);
    Ok(())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "ClaimA",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "ClaimB",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "RefuseA",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "RefuseB",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "ReplayA",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "ReplayB",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
pub(super) fn claim_at(
    paths: &Paths,
    relation: AccountRelation<'_>,
    binding: &Binding,
    limits: Limits,
) -> io::Result<AccountObservation> {
    if !valid_relation(relation) || !valid_pair(relation.candidate, binding) {
        return Err(invalid());
    }
    let key = existing_key(paths)?;
    let bound = Bound::new(&key, relation)?;
    let candidate = receipt(&key, relation.candidate, binding)?;
    let (mut log, view) = open_existing(paths, &key, limits)?;
    check_rejection_domain(paths, &key, relation.predecessor)?;
    check_rejection_domain(paths, &key, relation.candidate)?;
    let parent = bound.parent(&view)?;
    if let Some(old) = view.entries.get(&bound.candidate) {
        return bound
            .observation(&view)
            .filter(|observation| {
                old.receipt == candidate
                    && matches!(observation.outcome, AccountOutcome::Claimed(_))
            })
            .ok_or_else(invalid);
    }
    if parent.successor.is_some() {
        return Err(invalid());
    }
    if !view.within_capacity_extra(limits, 3, ACCOUNT_FRAME + 2 * ISSUER_FRAME) {
        return Err(io::Error::other(
            "native account claim closing capacity is unavailable",
        ));
    }
    let recorded_ms = aterm_time::unix_millis().ok_or_else(invalid)?;
    log.append(
        ACCOUNT_SUBJECT,
        encode(&view, &key, &bound, &candidate, recorded_ms, false),
    )?;
    Ok(AccountObservation {
        sequence: view.sequence,
        recorded_ms,
        outcome: AccountOutcome::Claimed(ClaimedState::Prepared),
    })
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "FenceA",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "FenceB",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "RefuseFenceA",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "RefuseFenceB",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "ReplayFenceA",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "ReplayFenceB",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
pub(super) fn fence_at(
    paths: &Paths,
    relation: AccountRelation<'_>,
    limits: Limits,
) -> io::Result<AccountObservation> {
    if !valid_relation(relation) {
        return Err(invalid());
    }
    let key = existing_key(paths)?;
    let bound = Bound::new(&key, relation)?;
    let (mut log, view) = open_existing(paths, &key, limits)?;
    check_rejection_domain(paths, &key, relation.predecessor)?;
    check_rejection_domain(paths, &key, relation.candidate)?;
    bound.parent(&view)?;
    if view.entries.contains_key(&bound.candidate) {
        return bound
            .observation(&view)
            .filter(|observation| observation.outcome == AccountOutcome::Fenced)
            .ok_or_else(invalid);
    }
    if !view.within_capacity_extra(limits, 1, FENCE_FRAME) {
        return Err(io::Error::other(
            "native account fence capacity is unavailable",
        ));
    }
    let recorded_ms = aterm_time::unix_millis().ok_or_else(invalid)?;
    let candidate = Receipt {
        operation: bound.candidate,
        prepare: bound.prepare,
        capability: [0; 32],
    };
    log.append(
        FENCE_SUBJECT,
        encode(&view, &key, &bound, &candidate, recorded_ms, true),
    )?;
    Ok(AccountObservation {
        sequence: view.sequence,
        recorded_ms,
        outcome: AccountOutcome::Fenced,
    })
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "ObserveA",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountJournal",
        action = "ObserveB",
        project = "aterm_agent::harness::native_receipts::tests::accounts::model::transition"
    )
)]
pub(super) fn observe_at(
    paths: &Paths,
    relation: AccountRelation<'_>,
) -> io::Result<Option<AccountObservation>> {
    if !valid_relation(relation) {
        return Err(invalid());
    }
    let Some((key, view)) = read(paths)? else {
        return Ok(None);
    };
    Ok(Bound::new(&key, relation)?.observation(&view))
}

pub(super) fn recover_at(
    paths: &Paths,
    relation: AccountRelation<'_>,
    binding: &Binding,
) -> io::Result<Option<AccountObservation>> {
    if !valid_relation(relation) || !valid_pair(relation.candidate, binding) {
        return Err(invalid());
    }
    let key = existing_key(paths)?;
    let bound = Bound::new(&key, relation)?;
    // Reopen completes the log/checkpoint durability barriers. A plain read
    // cannot acknowledge a prior writer's uncertain directory flush.
    let (_log, view) = open_existing(paths, &key, LIMITS)?;
    bound.parent(&view)?;
    let Some(entry) = view.entries.get(&bound.candidate) else {
        return Ok(None);
    };
    if entry.receipt != receipt(&key, relation.candidate, binding)? {
        return Err(invalid());
    }
    bound
        .observation(&view)
        .filter(|observation| matches!(observation.outcome, AccountOutcome::Claimed(_)))
        .map(Some)
        .ok_or_else(invalid)
}

/// Recover an already recorded exact claim without creating a claim or key.
/// Reopening completes any outstanding durability barriers before acknowledging
/// the supplied Binding. Absence never releases a pending reservation.
/// # Errors
/// Refuses invalid relations, mismatched capabilities, incompatible outcomes,
/// absent enrollment, damaged custody or a failed durability barrier.
pub fn recover_account_successor(
    relation: AccountRelation<'_>,
    binding: &Binding,
) -> io::Result<Option<AccountObservation>> {
    recover_at(&Paths::locate().map_err(|_| invalid())?, relation, binding)
}

/// Record one exact claim, or replay its current state with the original receipt.
/// The caller must independently establish live native and account authority.
/// # Errors
/// Refuses a wrong relation/binding, legacy history, conflicting outcomes,
/// insufficient closing capacity, or unavailable/damaged custody.
pub fn claim_account_successor(
    relation: AccountRelation<'_>,
    binding: &Binding,
) -> io::Result<AccountObservation> {
    claim_at(
        &Paths::locate().map_err(|_| invalid())?,
        relation,
        binding,
        LIMITS,
    )
}

/// Permanently exclude this candidate from claims without consuming its parent.
/// # Errors
/// Refuses a wrong relation, an existing incompatible entry, insufficient
/// capacity, or unavailable/damaged custody. An error proves no absence.
pub fn fence_account_successor(relation: AccountRelation<'_>) -> io::Result<AccountObservation> {
    fence_at(&Paths::locate().map_err(|_| invalid())?, relation, LIMITS)
}

/// Read an exact outcome without writing or recreating any authority.
/// This does not validate a candidate Binding supplied by another source.
/// `None` means no matching outcome was observed, never permission to abandon
/// a retained candidate or evidence that a later claim cannot exist.
/// # Errors
/// Refuses a malformed relation and unavailable/damaged established custody.
pub fn observe_account_successor(
    relation: AccountRelation<'_>,
) -> io::Result<Option<AccountObservation>> {
    observe_at(&Paths::locate().map_err(|_| invalid())?, relation)
}
