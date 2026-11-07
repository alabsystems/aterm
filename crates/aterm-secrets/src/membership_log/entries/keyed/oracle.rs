// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keyed-epoch reading [`Log::keyed_epochs`] replaced, kept only as the tests' oracle: for
//! every counting epoch, a scan of its audience's epochs for those in its past and a test of
//! each pair among them, and a scan of the earlier full reseals for each keyed epoch.
//!
//! Every reading the tests take checks itself against it ([`check`]).

use super::super::Keyed;
use crate::age::Recipient;
use crate::membership_log::{Body, EpochAudience, Log, MemberId, RecordId, eval};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

/// [`Log::keyed_epochs`] as it was, reading `effective`.
pub(in crate::membership_log) fn keyed_epochs_in(
    log: &Log,
    effective: &eval::Effective,
) -> Vec<Keyed> {
    let keyed: Vec<usize> = (1..log.order.len())
        .filter(|&i| effective.keyed(i))
        .collect();
    if keyed.is_empty() {
        return Vec::new();
    }
    let membership = log.membership_ref();
    let member_of: BTreeMap<Recipient, MemberId> = membership
        .members()
        .iter()
        .map(|(actor, m)| (m.binding.recipient(), *actor))
        .collect();
    // Every recipient a seat binds to each actor: an attestation is a `Wrap` its signer
    // addressed to itself.
    let mut recipients: BTreeMap<MemberId, BTreeSet<Recipient>> = BTreeMap::new();
    let mut epochs: BTreeMap<&EpochAudience, Vec<usize>> = BTreeMap::new();
    let mut wraps: BTreeMap<RecordId, Vec<(usize, Recipient)>> = BTreeMap::new();
    for (i, record) in log.order.iter().enumerate() {
        match record.body() {
            Body::Genesis { founders, .. } => {
                for founder in founders {
                    if let Some(actor) = founder.member() {
                        recipients
                            .entry(actor)
                            .or_default()
                            .insert(founder.recipient);
                    }
                }
            }
            Body::Admit { entrant, .. } => {
                recipients
                    .entry(entrant.member())
                    .or_default()
                    .insert(entrant.request.binding().recipient());
            }
            Body::Epoch { audience, .. } if effective.counts(i) => {
                epochs.entry(audience).or_default().push(i);
            }
            Body::Wrap {
                epoch, recipient, ..
            } => wraps.entry(*epoch).or_default().push((i, *recipient)),
            _ => {}
        }
    }
    let audience_of = |e: usize| match log.order[e].body() {
        Body::Epoch { audience, .. } => Some(audience),
        _ => None,
    };
    let wraps_of = |e: usize| wraps.get(log.order[e].id()).map_or(&[][..], Vec::as_slice);
    let own = |actor: &MemberId, recipient: &Recipient| {
        recipients
            .get(actor)
            .is_some_and(|set| set.contains(recipient))
    };
    // At record `at`, `actor` has the key of the counting epoch `h` when it minted `h` or a
    // counting `Wrap` of `h` to it lies in `at`'s past.
    let has_counting_key = |actor: &MemberId, h: usize, at: usize| {
        log.order[h].actor() == actor
            || wraps_of(h).iter().any(|&(w, recipient)| {
                effective.counts(w) && own(actor, &recipient) && effective.before(w, at)
            })
    };
    // ...and of the keyed epoch `k` only when its attestation of `k` (a counting `Wrap` of
    // `k` it signed to itself) lies in `at`'s past: only a reseal signs one.
    let attests = |actor: &MemberId, k: usize, at: usize| {
        wraps_of(k).iter().any(|&(w, recipient)| {
            effective.counts(w)
                && log.order[w].actor() == actor
                && own(actor, &recipient)
                && effective.before(w, at)
        })
    };
    // Each keyed epoch's dependents and holders: the dependents' signers, and the members
    // rule 2's usable Wraps of it are addressed to.
    let held: Vec<(usize, &EpochAudience, &[usize], BTreeSet<MemberId>)> = keyed
        .iter()
        .filter_map(|&e| {
            let audience = audience_of(e)?;
            let dependents = effective.dependents(e);
            let mut holders: BTreeSet<MemberId> =
                dependents.iter().map(|&d| *log.order[d].actor()).collect();
            for &(w, recipient) in wraps_of(e) {
                if effective.opens_wrap(w)
                    && let Some(member) = member_of.get(&recipient)
                {
                    holders.insert(*member);
                }
            }
            Some((e, audience, dependents, holders))
        })
        .collect();
    // A full reseal: a counting epoch of the audience whose signer, there, has the key of
    // every newest counting epoch of the audience in its past and of every keyed epoch of the
    // audience that a current member holds, with a dependent in its past, that no earlier
    // full reseal in its past covers already. A covered epoch's key goes to no newcomer and
    // nobody attests it again, so a later keyed epoch of the audience is covered by whoever
    // reseals it; one no current member holds nobody can attest, and rule 7 ignores it too.
    // Decided in record order, where an epoch's past comes before it, and kept per epoch.
    let mut full: BTreeMap<usize, bool> = BTreeMap::new();
    for (audience, list) in &epochs {
        for &e2 in list {
            let signer = log.order[e2].actor();
            let past: Vec<usize> = list
                .iter()
                .copied()
                .filter(|&h| effective.before(h, e2))
                .collect();
            let newest_held = past
                .iter()
                .filter(|&&h| !past.iter().any(|&h2| effective.before(h, h2)))
                .all(|&h| has_counting_key(signer, h, e2));
            let is_full = newest_held
                && held
                    .iter()
                    .filter(|(_, aud, _, holders)| {
                        aud == audience && holders.iter().any(|h| membership.contains(h))
                    })
                    .filter(|(_, _, dependents, _)| {
                        dependents.iter().any(|&d| effective.before(d, e2))
                    })
                    .filter(|(_, _, dependents, _)| {
                        !past.iter().any(|&e1| {
                            full.get(&e1).copied().unwrap_or(false)
                                && dependents.iter().all(|&d| effective.before(d, e1))
                        })
                    })
                    .all(|&(k, _, _, _)| attests(signer, k, e2));
            full.insert(e2, is_full);
        }
    }
    held.into_iter()
        .map(|(e, audience, dependents, holders)| {
            let covered = epochs.get(audience).into_iter().flatten().any(|&e2| {
                full.get(&e2).copied().unwrap_or(false)
                    && dependents.iter().all(|&d| effective.before(d, e2))
            });
            Keyed {
                epoch: *log.order[e].id(),
                audience: audience.clone(),
                dependents: dependents.iter().map(|&d| *log.order[d].id()).collect(),
                holders,
                covered,
            }
        })
        .collect()
}

thread_local! {
    /// Readings checked against the oracle on this thread, and how many disagreed.
    pub(in crate::membership_log) static CHECKED: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    /// Readings checked on this thread that found a keyed epoch, and that found one covered.
    pub(in crate::membership_log) static FOUND: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    /// Count a disagreement rather than fail on it: a negative control's mutant disagrees.
    pub(in crate::membership_log) static COUNT_ONLY: Cell<bool> = const { Cell::new(false) };
}

/// Check `got`, what [`Log::keyed_epochs`] read of `log`, against the oracle: every reading a
/// test takes checks itself, so every test that reads keyed epochs is a case of the
/// differential. A disagreement fails the test, or is counted under [`COUNT_ONLY`].
pub(in crate::membership_log) fn check(log: &Log, effective: &eval::Effective, got: &[Keyed]) {
    let want = keyed_epochs_in(log, effective);
    let (checked, wrong) = CHECKED.get();
    let disagrees = got != want.as_slice();
    CHECKED.set((checked + 1, wrong + usize::from(disagrees)));
    let (found, covered) = FOUND.get();
    FOUND.set((
        found + usize::from(!want.is_empty()),
        covered + usize::from(want.iter().any(|keyed| keyed.covered)),
    ));
    assert!(
        !disagrees || COUNT_ONLY.get(),
        "the keyed epochs disagree with the replaced reading: got {got:?}, want {want:?}"
    );
}
