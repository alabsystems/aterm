// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Same-operation installation under real native exclusion. Pending writes do
//! not become reader authority until their exact encrypted payload is durable.
use super::*;
use crate::harness::{codex_session_slot as slot, codex_source_registry::TargetAdmission};

pub(in crate::harness) struct Pending {
    book: Book,
    bytes: SecretBytes,
}
impl Pending {
    pub(in crate::harness::codex_assignment) fn book(&self) -> &Book {
        &self.book
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    Before,
    Effect,
}

fn encoded(book: &Book, identity: &age::Identity) -> Result<SecretBytes, String> {
    if !book.valid(&book.caller) {
        return Err("assignment book is invalid".into());
    }
    #[derive(Serialize)]
    struct Signed<'a> {
        book: &'a Book,
        mac: String,
    }
    Ok(SecretBytes::new(
        aterm_json::to_vec(&Signed {
            book,
            mac: signature(identity, book)?,
        })
        .map_err(|_| "assignment book encoding unavailable")?,
    ))
}
pub(in crate::harness::codex_assignment) fn recover(
    claimed: &mut Claimed,
    identity: &age::Identity,
) -> Result<(), String> {
    if let Some(pending) = &claimed.pending_install {
        if claimed
            .store
            .recover_exact_write(identity, pending.bytes.as_slice())
            .map_err(str::to_owned)?
        {
            claimed.book = claimed
                .pending_install
                .take()
                .ok_or("assignment write disappeared")?
                .book;
        } else {
            claimed.store.unchanged().map_err(str::to_owned)?;
        }
    } else {
        let bytes = encoded(&claimed.book, identity)?;
        if !claimed
            .store
            .recover_exact_write(identity, bytes.as_slice())
            .map_err(str::to_owned)?
        {
            return Err("assignment canonical book changed".into());
        }
    }
    Ok(())
}
pub(in crate::harness::codex_assignment) fn write(
    claimed: &mut Claimed,
    identity: &age::Identity,
    book: Book,
) -> Result<(), String> {
    let bytes = encoded(&book, identity)?;
    if let Some(pending) = &claimed.pending_install {
        if !aterm_digest::ct_eq(pending.bytes.as_slice(), bytes.as_slice()) {
            return Err("assignment pending write changed".into());
        }
    } else {
        claimed.pending_install = Some(Pending { book, bytes });
    }
    recover(claimed, identity)?;
    let Some(pending) = &claimed.pending_install else {
        return Ok(());
    };
    let saved = claimed
        .store
        .save(pending.bytes.as_slice())
        .map_err(str::to_owned);
    if saved.is_err() {
        recover(claimed, identity)?;
        if claimed.pending_install.is_some() {
            return saved;
        }
    } else {
        claimed.book = claimed
            .pending_install
            .take()
            .ok_or("assignment write disappeared")?
            .book;
    }
    claimed.store.unchanged().map_err(str::to_owned)
}
fn position(book: &Book, intent: &TargetIntent, boundary: &str) -> Result<Position, String> {
    if book.consumer != Consumer::observe(&intent.caller)? {
        return Err("assignment consumer changed".into());
    }
    position_metadata(book, intent, boundary)
}
fn position_metadata(
    book: &Book,
    intent: &TargetIntent,
    boundary: &str,
) -> Result<Position, String> {
    if !book.valid(&intent.caller)
        || !admitted(&book.prepared, &intent.record.body)?
            .is_some_and(|record| same_prepared(record, &intent.record))
    {
        return Err("prepared assignment changed".into());
    }
    let recorded =
        admitted(&book.prepared, &intent.record.body)?.ok_or("prepared assignment unavailable")?;
    if recorded
        .commit_boundary
        .as_deref()
        .is_some_and(|value| value != boundary)
    {
        return Err("assignment commit body changed".into());
    }
    let current = &book.current;
    let body = &intent.record.body;
    if current.binding == intent.record.source
        && current.authority == intent.record.source_authority
        && current.generation == body.expected_assignment
        && matches!(current.phase, Phase::Startup | Phase::Applied)
    {
        return Ok(Position::Before);
    }
    let target = intent
        .record
        .target
        .as_ref()
        .ok_or("prepared target unavailable")?;
    if current.binding != target.binding
        || Some(current.generation) != body.expected_assignment.checked_add(1)
        || current.previous_generation != body.expected_assignment
        || current.authority
            != (SourceAuthority::Assignment {
                operation: body.operation.clone(),
            })
        || current.boundary.as_deref() != Some(boundary)
        || !matches!(
            current.phase,
            Phase::Installing | Phase::AwaitingAck | Phase::Applied
        )
    {
        return Err("assignment operation changed".into());
    }
    Ok(Position::Effect)
}
fn verify_target(intent: &TargetIntent, grant: &owner::AdmittedGrant<'_>) -> Result<(), String> {
    let target = intent
        .record
        .target
        .as_ref()
        .ok_or("prepared target unavailable")?;
    if grant.binding() != &target.binding
        || grant.identity() != &target.identity
        || grant.revision().generation < target.credential_floor
        || OAuth::from_document(grant.document())? != target.oauth
    {
        return Err("prepared target authority changed".into());
    }
    Ok(())
}

/// The exclusion/resource role is live and non-deserializable; signed Book data
/// alone cannot call this path or authorize any destination factory.
pub(in crate::harness) fn install(
    env: &Env,
    admission: &TargetAdmission<'_>,
    boundary: &str,
) -> Result<
    (
        slot::SourceHandle,
        slot::MaterializationReceipt,
        AssignmentCommit,
    ),
    String,
> {
    if !hex(boundary, 32) {
        return Err("assignment boundary is invalid".into());
    }
    admission.current()?;
    let intent = admission.intent();
    let home = env.home.as_deref().ok_or("assignment home unavailable")?;
    if !admission.matches_home(home) {
        return Err("assignment source namespace changed".into());
    }
    let identity = platform::machine_identity(home)
        .map_err(|_| "assignment identity unavailable")?
        .ok_or("assignment identity unavailable")?;
    let lease = open(env, &intent.caller)?;
    let before = {
        let mut claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
        if claimed.store.physical_identity().map_err(str::to_owned)? != *admission.book_namespace()
        {
            return Err("assignment Book namespace changed".into());
        }
        recover(&mut claimed, &identity)?;
        position(&claimed.book, intent, boundary)?
    };
    if before == Position::Before {
        admission.permits_new_effect()?;
    }
    let use_grant = |grant: owner::AdmittedGrant<'_>| {
        admission.current()?;
        verify_target(intent, &grant)?;
        let assignment = {
            let mut claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
            if claimed.store.physical_identity().map_err(str::to_owned)?
                != *admission.book_namespace()
            {
                return Err("assignment Book namespace changed".into());
            }
            recover(&mut claimed, &identity)?;
            let now = position(&claimed.book, intent, boundary)?;
            if now != before {
                return Err("assignment installation raced".into());
            }
            if now == Position::Before {
                admission.permits_new_effect()?;
                // Bind the accepted commit body durably before assignment or
                // credential effects. Retargeted same-op retries cannot revive.
                if claimed
                    .book
                    .prepared
                    .iter()
                    .find(|record| record.body.operation == intent.record.body.operation)
                    .is_some_and(|record| record.commit_boundary.is_none())
                {
                    let mut accepted = claimed.book.clone();
                    accepted
                        .prepared
                        .iter_mut()
                        .find(|record| record.body.operation == intent.record.body.operation)
                        .ok_or("prepared assignment disappeared")?
                        .commit_boundary = Some(boundary.into());
                    accepted.schema = if accepted.acknowledged.is_empty() {
                        4
                    } else {
                        5
                    };
                    write(&mut claimed, &identity, accepted)?;
                    admission.permits_new_effect()?;
                }
                let mut next = claimed.book.clone();
                let generation = next
                    .clock
                    .checked_add(1)
                    .ok_or("assignment generation exhausted")?;
                next.current = Current {
                    binding: grant.binding().clone(),
                    generation,
                    previous_generation: next.clock,
                    authority: SourceAuthority::Assignment {
                        operation: intent.record.body.operation.clone(),
                    },
                    boundary: Some(boundary.into()),
                    phase: Phase::Installing,
                };
                next.clock = generation;
                write(&mut claimed, &identity, next)?;
            }
            let current = &claimed.book.current;
            AssignmentCommit {
                caller: intent.caller.clone(),
                binding: current.binding.clone(),
                generation: current.generation,
                previous_generation: current.previous_generation,
                operation: intent.record.body.operation.clone(),
                boundary: boundary.into(),
                _lease: lease.clone(),
            }
        };
        // Account/owner -> Book -> private Slot ordering. No Book mutex remains
        // held while the Slot's typed commit reopens its exact current claim.
        let (source, receipt) = slot::install(env, &grant, &assignment)?;
        if receipt.caller() != &intent.caller
            || receipt.slot() != source.slot()
            || receipt.generation() != source.credential_generation()
            || receipt.digest() != source.digest()
        {
            return Err("assignment materialization receipt changed".into());
        }
        admission.current()?;
        {
            let mut claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
            recover(&mut claimed, &identity)?;
            if position(&claimed.book, intent, boundary)? != Position::Effect {
                return Err("assignment publication changed".into());
            }
            if claimed.book.current.phase == Phase::Installing {
                let mut next = claimed.book.clone();
                next.current.phase = Phase::AwaitingAck;
                write(&mut claimed, &identity, next)?;
            }
        }
        admission.current()?;
        Ok((source, receipt, assignment))
    };
    if before == Position::Before {
        owner::with_selected_target(
            env,
            &intent.caller,
            &intent.record.body.pool_revision,
            &intent.record.body.selection(),
            &intent.record.source,
            intent.target_binding(),
            use_grant,
        )
    } else {
        owner::with_current_published_grant(
            env,
            intent
                .target_binding()
                .ok_or("prepared target unavailable")?,
            &intent.caller,
            use_grant,
        )
    }
}

#[cfg(all(test, unix))]
#[path = "codex_assignment_target_install_tests.rs"]
mod tests;
