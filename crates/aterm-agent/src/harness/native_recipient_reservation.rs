// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared operation ownership only: no complete-history write or execution grant.

use super::*;
use crate::harness::native_act::ReceiptMode;

/// A durable namespace reservation, distinct from admission or NoExec evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecipientReservation {
    /// Original reservation position in the legacy-readable journal.
    sequence: u64,
    /// Attribution time of that original reservation, never an authority clock.
    recorded_ms: u64,
}

impl RecipientReservation {
    /// Original reservation position in the journal.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Original attribution time, never an authority clock.
    #[must_use]
    pub const fn recorded_ms(self) -> u64 {
        self.recorded_ms
    }

    fn from_observed(observed: Observed) -> Self {
        Self {
            sequence: observed.sequence,
            recorded_ms: observed.recorded_ms,
        }
    }
}

pub(super) struct HeldReservation {
    pub acknowledgement: RecipientReservation,
    pub custody: String,
    identity: Vec<u8>,
    _log: Log,
}

impl HeldReservation {
    pub(super) fn intact(&self, paths: &Paths) -> io::Result<()> {
        if existing_key(paths)?.anchor != self.identity || self._log.is_poisoned() {
            return Err(invalid());
        }
        Ok(())
    }
}

fn matches_open(entry: &Entry, expected: &Receipt, epoch: Digest) -> bool {
    entry.receipt == *expected
        && entry.epoch == Some(epoch)
        && entry.state == IssuerState::Prepared
        && entry.predecessor.is_none()
        && entry.successor.is_none()
        && entry.closed_phase.is_none()
        && entry.account.is_none()
        && entry.recipients.is_none()
}

pub(super) fn hold_at(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
) -> io::Result<HeldReservation> {
    if prepare.receipt_mode != Some(ReceiptMode::Recipients5) || !valid_pair(prepare, binding) {
        return Err(invalid());
    }
    let key = existing_key(paths)?;
    let expected = receipt(&key, prepare, binding)?;
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    let (log, view) = open_existing(paths, &key, LIMITS)?;
    check_rejection_domain(paths, &key, prepare)?;
    let entry = view
        .entries
        .get(&expected.operation)
        .filter(|entry| matches_open(entry, &expected, epoch))
        .ok_or_else(invalid)?;
    Ok(HeldReservation {
        acknowledgement: RecipientReservation::from_observed(entry.observed),
        custody: view.custody(&key).ok_or_else(invalid)?,
        identity: key.anchor.clone(),
        _log: log,
    })
}

/// Reserve a complete-receipt recipe's operation ID in the legacy-readable journal.
/// The caller supplies its original binding; a retry returns the same acknowledgement.
/// This reserves legacy closing capacity only. It does not validate live caller authority,
/// establish complete-journal custody, publish admission, or grant execution.
/// The admission protocol must independently validate the recipe's custody stamp.
///
/// # Errors
/// Refuses an unmarked or malformed recipe, a wrong journal domain, unavailable
/// established custody, a conflicting or closed operation, or insufficient capacity.
pub fn reserve_recipient_operation(
    prepare: &Prepare,
    binding: &Binding,
) -> io::Result<RecipientReservation> {
    reserve_at(
        &Paths::locate().map_err(|_| invalid())?,
        prepare,
        binding,
        LIMITS,
    )
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientReservation",
        action = "Reserve",
        project = "aterm_agent::harness::native_receipts::reservation::tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientReservation",
        action = "Legacy",
        reason = "The existing issuer writer contends for this ID under the same journal lock"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientReservation",
        action = "Close",
        reason = "The existing issuer writer closes its own legacy recipe without releasing its ID"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientReservation",
        action = "Crash",
        reason = "The caller loses acknowledgements and reopens the committed journal"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientReservation",
        action = "Lose",
        reason = "The environment removes an established journal while enrollment persists"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientReservation",
        action = "WrongDomain",
        reason = "The caller supplies a different journal domain"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientReservation",
        action = "Tight",
        reason = "The environment supplies insufficient admission capacity"
    )
)]
pub(super) fn reserve_at(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
    limits: Limits,
) -> io::Result<RecipientReservation> {
    if prepare.receipt_mode != Some(ReceiptMode::Recipients5) || !valid_pair(prepare, binding) {
        return Err(invalid());
    }
    // Domain discovery already enrolled this namespace. Never repair missing custody.
    let key = existing_key(paths)?;
    let expected = receipt(&key, prepare, binding)?;
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    let (mut log, view) = open_existing(paths, &key, limits)?;
    check_rejection_domain(paths, &key, prepare)?;
    if let Some(old) = view.entries.get(&expected.operation) {
        return if matches_open(old, &expected, epoch) {
            Ok(RecipientReservation::from_observed(old.observed))
        } else {
            Err(invalid())
        };
    }
    if !view.can_append(Some(Event::Prepare), limits) {
        return Err(io::Error::other(
            "native recipient reservation closing capacity is unavailable",
        ));
    }
    let observed = Observed {
        sequence: view.sequence,
        recorded_ms: aterm_time::unix_millis().ok_or_else(invalid)?,
    };
    log.append(
        ISSUER_SUBJECT,
        view.encode_event(&key, &expected, epoch, Event::Prepare, observed.recorded_ms),
    )?;
    Ok(RecipientReservation::from_observed(observed))
}

#[cfg(test)]
#[path = "native_recipient_reservation_tests.rs"]
mod tests;
