// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Close the legacy reservation from an authenticated, terminal complete history.
/// This releases reserved closing capacity but never releases the operation ID.
/// Possible execution remains possible execution in the legacy journal. A retry
/// resumes a committed MayExecute prefix without replaying any delivery.
///
/// This is storage bookkeeping, not proof that a recipient retired and not an
/// execution or succession grant. Both journal locks remain held throughout.
/// # Errors
/// Refuses missing custody, a mismatched original reservation or binding,
/// nonterminal complete history, incompatible legacy history, or a failed barrier.
pub fn close_recipient_reservation(prepare: &Prepare, binding: &Binding) -> io::Result<Observed> {
    close_at(&Paths::locate().map_err(|_| invalid())?, prepare, binding)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ReservationClosure",
        action = "Close",
        project = "aterm_agent::harness::native_receipts::recipient_custody::closure::tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ReservationClosure",
        action = "NoExec",
        reason = "The separately checked recipient writer commits terminal NoExec history"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ReservationClosure",
        action = "EmptySeal",
        reason = "The separately checked recipient writer seals history without a recipient"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ReservationClosure",
        action = "ExecutedSeal",
        reason = "The separately checked recipient writer seals possible execution"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ReservationClosure",
        action = "Prefix",
        reason = "A crash loses the closing call after its MayExecute record committed"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ReservationClosure",
        action = "LoseCustody",
        reason = "The environment removes enrollment outside the journal locks"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ReservationClosure",
        action = "WrongBinding",
        reason = "The caller changes the immutable recipe supplied on retry"
    )
)]
pub(super) fn close_at(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
) -> io::Result<Observed> {
    if prepare.receipt_mode != Some(ReceiptMode::Recipients5) || !valid_pair(prepare, binding) {
        return Err(invalid());
    }
    let key = existing_key(paths)?;
    let expected = receipt(&key, prepare, binding)?;
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    // Keep the global lock order, including retries after the first closing record.
    let (mut log, mut view) = open_existing(paths, &key, LIMITS)?;
    check_rejection_domain(paths, &key, prepare)?;
    let opened = super::open_at(paths, false)?;
    if prepare.custody.as_deref() != Some(opened.custody.stamp()) {
        return Err(invalid());
    }
    let origin = opened.view.legacy_origin(
        &opened.key,
        prepare,
        binding,
        &view.custody(&key).ok_or_else(invalid)?,
    )?;
    // Replay authenticated the entire chain. Read the binding's original position,
    // rather than mistaking the latest closing record for the original reservation.
    let original = log.read_range(astream_wire::Offset(origin.sequence), 1)?;
    let original = original.first().ok_or_else(invalid)?;
    let body = &original.body;
    if original.seq.0 != origin.sequence
        || original.subject != ISSUER_SUBJECT
        || body.len() != ISSUER_BYTES
        || &body[..4] != b"ANI2"
        || body[12..20] != origin.recorded_ms.to_le_bytes()
        || body[52..84] != expected.operation
        || body[84..116] != expected.prepare
        || body[116..148] != expected.capability
        || body[148] != Event::Prepare as u8
        || body[149..181] != epoch
    {
        return Err(invalid());
    }
    let history = opened
        .view
        .observe(&opened.key, prepare)?
        .ok_or_else(invalid)?;
    let target = match history.state {
        Recorded::NoExec => IssuerState::NoExec,
        Recorded::Sealed { may_execute } => IssuerState::Sealed { may_execute },
        _ => return Err(invalid()),
    };
    loop {
        if existing_key(paths)?.anchor != key.anchor || log.is_poisoned() {
            return Err(invalid());
        }
        opened.intact(paths)?;
        let entry = view.entries.get(&expected.operation).ok_or_else(invalid)?;
        if entry.receipt != expected
            || entry.epoch != Some(epoch)
            || entry.predecessor.is_some()
            || entry.successor.is_some()
            || entry.closed_phase.is_some()
            || entry.account.is_some()
            || entry.recipients.is_some()
        {
            return Err(invalid());
        }
        if entry.state == target {
            return Ok(entry.observed);
        }
        let event = match (entry.state, target) {
            (IssuerState::Prepared, IssuerState::NoExec) => Event::NoExec,
            (IssuerState::Prepared, IssuerState::Sealed { may_execute: false })
            | (IssuerState::MayExecute, IssuerState::Sealed { may_execute: true }) => Event::Seal,
            (IssuerState::Prepared, IssuerState::Sealed { may_execute: true }) => Event::MayExecute,
            _ => return Err(invalid()),
        };
        let body = view.encode_event(
            &key,
            &expected,
            epoch,
            event,
            aterm_time::unix_millis().ok_or_else(invalid)?,
        );
        view.ingest_bounded(&key, &body, LIMITS)?;
        log.append(ISSUER_SUBJECT, body)?;
        // The next iteration rechecks both custodians before returning success or
        // writing Seal. An interrupted MayExecute is a valid retry prefix.
    }
}

#[cfg(test)]
#[path = "native_recipient_closure_tests.rs"]
mod tests;
