// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The epoch verdicts [`super::Epochs`] replaced, kept only as the tests' oracle: each asking
//! scans every record and keeps an audience's epochs that no other follows by testing each pair.
//! The pair test counts its tests and gives up past a budget, so a test can show it breaks a
//! bound without running out the quadratic count.
//!
//! Every verdict the tests take checks itself against it ([`check`]).

use super::super::{Body, EpochAudience, Evidence, Log, MemberId, RecordId};
use crate::age::Recipient;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

/// The budget ran out.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::membership_log) struct Exhausted;

/// The records of `epochs` no other of them follows, by testing each against every other with
/// `before`, giving up once more than `budget` tests have run.
pub(in crate::membership_log) fn pairwise_newest(
    epochs: &[usize],
    before: &dyn Fn(usize, usize) -> bool,
    budget: usize,
) -> Result<Vec<usize>, Exhausted> {
    let mut tests = 0usize;
    let mut newest = Vec::new();
    for &a in epochs {
        let mut follows = false;
        for &b in epochs {
            if a != b {
                tests += 1;
                if before(a, b) {
                    follows = true;
                    break;
                }
            }
            if tests > budget {
                return Err(Exhausted);
            }
        }
        if !follows {
            newest.push(a);
        }
    }
    Ok(newest)
}

/// `log`'s counting epochs of `audience` (index and id), and its counting wraps to `recipient`
/// (each epoch's first).
fn scan(
    log: &Log,
    audience: &EpochAudience,
    recipient: &Recipient,
) -> (Vec<(usize, RecordId)>, BTreeMap<RecordId, usize>) {
    let effective = log.effective();
    let mut epochs: Vec<(usize, RecordId)> = Vec::new();
    let mut wrapped_epochs: BTreeMap<RecordId, usize> = BTreeMap::new();
    for (i, record) in log.order.iter().enumerate().skip(1) {
        if !effective.counts(i) {
            continue;
        }
        match record.body() {
            Body::Epoch { audience: aud, .. } if aud == audience => {
                epochs.push((i, *record.id()));
            }
            Body::Wrap {
                epoch,
                recipient: to,
                ..
            } if to == recipient => {
                wrapped_epochs.entry(*epoch).or_insert(i);
            }
            _ => {}
        }
    }
    (epochs, wrapped_epochs)
}

/// The newest of `epochs`, as the pair test gives them.
fn newest(log: &Log, epochs: &[(usize, RecordId)]) -> Vec<(usize, RecordId)> {
    let effective = log.effective();
    let at: Vec<usize> = epochs.iter().map(|&(at, _)| at).collect();
    let newest = pairwise_newest(&at, &|a, b| effective.before(a, b), usize::MAX)
        .expect("an unbounded budget never runs out");
    epochs
        .iter()
        .copied()
        .filter(|(at, _)| newest.contains(at))
        .collect()
}

/// [`Log::audience_left_out_recipient`] as it was.
pub(in crate::membership_log) fn audience_left_out_recipient(
    log: &Log,
    audience: &EpochAudience,
    recipient: &Recipient,
) -> bool {
    let membership = log.membership_ref();
    let entry_at = membership
        .members()
        .values()
        .filter(|m| m.binding.recipient() == *recipient)
        .filter_map(|seat| {
            let &at = log.held.get(&seat.entry)?;
            let is_listed = matches!(
                log.order[at].body(),
                Body::Admit {
                    evidence: Evidence::Listed,
                    ..
                }
            );
            Some((at, is_listed))
        })
        .max_by_key(|&(at, is_listed)| (!is_listed, at));
    let effective = log.effective();
    let (epochs, wrapped_epochs) = scan(log, audience, recipient);
    let epoch_ids: BTreeSet<RecordId> = epochs.iter().map(|&(_, id)| id).collect();
    let is_commons_or_everyone =
        matches!(audience, EpochAudience::Everyone | EpochAudience::Commons);
    let prior_wrap_indices: Vec<usize> = wrapped_epochs
        .iter()
        .filter_map(|(ep, &w_at)| epoch_ids.contains(ep).then_some(w_at))
        .collect();
    newest(log, &epochs).iter().any(|&(at, ref epoch)| {
        if wrapped_epochs.contains_key(epoch) {
            return false;
        }
        let knew_entry = is_commons_or_everyone
            && entry_at.is_some_and(|(e_at, _)| e_at == at || effective.before(e_at, at));
        let knew_wrap = prior_wrap_indices.iter().any(|&w_at| {
            let superseded_by_entry = is_commons_or_everyone
                && entry_at.is_some_and(|(e_at, is_listed)| {
                    effective.before(at, e_at) || (!is_listed && effective.before(w_at, e_at))
                });
            !superseded_by_entry && (w_at == at || effective.before(w_at, at))
        });
        knew_entry || knew_wrap
    })
}

/// [`Log::audience_left_out`] as it was.
pub(in crate::membership_log) fn audience_left_out(
    log: &Log,
    audience: &EpochAudience,
    member: &MemberId,
) -> bool {
    let membership = log.membership_ref();
    let Some(seat) = membership.get(member) else {
        return true;
    };
    let Some(&entry_at) = log.held.get(&seat.entry) else {
        return true;
    };
    let recipient = seat.binding.recipient();
    if audience_left_out_recipient(log, audience, &recipient) {
        return true;
    }
    let effective = log.effective();
    let (epochs, wrapped_epochs) = scan(log, audience, &recipient);
    newest(log, &epochs).iter().any(|&(at, ref epoch)| {
        !wrapped_epochs.contains_key(epoch) && (entry_at == at || effective.before(entry_at, at))
    })
}

/// [`Log::wraps_maximal_audience_to`] as it was.
pub(in crate::membership_log) fn wraps_maximal_audience_to(
    log: &Log,
    audience: &EpochAudience,
    recipient: &Recipient,
) -> bool {
    let (epochs, wrapped_epochs) = scan(log, audience, recipient);
    let maximal = newest(log, &epochs);
    !maximal.is_empty()
        && maximal
            .iter()
            .all(|(_, id)| wrapped_epochs.contains_key(id))
}

thread_local! {
    /// Verdicts checked against the oracle on this thread, and how many disagreed.
    pub(in crate::membership_log) static CHECKED: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    /// Count a disagreement rather than fail on it: a negative control's mutant disagrees.
    pub(in crate::membership_log) static COUNT_ONLY: Cell<bool> = const { Cell::new(false) };
}

/// Check `got`, what the cache said of `what`, against `want`, what the replaced code says:
/// every verdict a test takes checks itself, so every test that asks one is a case of the
/// differential. A disagreement fails the test, or is counted under [`COUNT_ONLY`].
pub(in crate::membership_log) fn check(what: &str, got: bool, want: impl FnOnce() -> bool) {
    let want = want();
    let (checked, wrong) = CHECKED.get();
    let disagrees = got != want;
    CHECKED.set((checked + 1, wrong + usize::from(disagrees)));
    assert!(
        !disagrees || COUNT_ONLY.get(),
        "{what}: the cached verdict {got} disagrees with the replaced scan's {want}"
    );
}
