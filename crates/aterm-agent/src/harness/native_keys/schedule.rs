// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::trace::Since;

/// Local scheduling hints. A selected account still needs current
/// root authority and local policy before any credential or provider access.
#[derive(Default, Hash)]
pub struct PreparationQueue {
    seen: BTreeMap<(PathBuf, [u8; 32]), Seen>,
}

#[derive(Clone, Copy, Hash)]
struct Seen {
    revision: [u8; 32],
    at: Instant,
}

const COOLDOWN: Duration = Duration::from_secs(60);

impl PreparationQueue {
    /// A queue that has selected nothing, as [`Default`] makes one, for a static.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            seen: BTreeMap::new(),
        }
    }

    /// Choose the least recently selected mature candidate, with unseen accounts
    /// first and account IDs breaking ties. Retired accounts release this home's
    /// slots after cooling; temporary absence cannot bypass a cooldown. Another
    /// home's timestamps remain intact. Selection grants no access.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "Initial",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "Sparse",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "Late",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "Prune",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "OtherHome",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "Grow",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "Cooldown",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "Boundary",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "Future",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "Empty",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "Disappear",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationQueue",
            action = "Reappear",
            project = "aterm_agent::harness::native_keys::schedule::tests::transition"
        )
    )]
    pub fn pick(&mut self, home: &Path, candidates: &[[u8; 32]], now: Instant) -> Option<[u8; 32]> {
        self.pick_if(home, candidates, now, |_| true)
    }

    /// Offer eligible accounts in scheduling order until one is admitted.
    /// Only that account consumes its cooldown; rejected accounts remain eligible.
    /// Admission must be synchronous and must not reenter this queue.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationAdmission",
            action = "Reject",
            project = "aterm_agent::harness::native_keys::schedule::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationAdmission",
            action = "Retry",
            project = "aterm_agent::harness::native_keys::schedule::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationAdmission",
            action = "Skip",
            project = "aterm_agent::harness::native_keys::schedule::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationAdmission",
            action = "Cooling",
            project = "aterm_agent::harness::native_keys::schedule::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreparationAdmission",
            action = "Grow",
            project = "aterm_agent::harness::native_keys::schedule::tests::admission_transition"
        )
    )]
    pub fn pick_if(
        &mut self,
        home: &Path,
        candidates: &[[u8; 32]],
        now: Instant,
        mut admit: impl FnMut([u8; 32]) -> bool,
    ) -> Option<[u8; 32]> {
        let revisions = candidates.iter().map(|id| (*id, [0; 32])).collect();
        self.pick_revisions_if(home, &revisions, now, |account, _| admit(account))
            .map(|(account, _)| account)
    }

    /// A changed checked revision is eligible without consuming another slot.
    /// Refusal preserves the previous revision and timestamp. Revision identifiers
    /// are scheduling context only; selection grants no authority or credential access.
    ///
    /// Every comparison of `now` with a selection's time goes through
    /// `aterm_uds::trace::Since`: a selection still cooling, or one stamped after `now`, records
    /// when that answer ends in the trace scope armed on this thread, so the accounts keeper's
    /// verdict of a pass that found nothing eligible stands no longer than the first cooldown it
    /// waited on.
    pub fn pick_revisions_if(
        &mut self,
        home: &Path,
        candidates: &BTreeMap<[u8; 32], [u8; 32]>,
        now: Instant,
        mut admit: impl FnMut([u8; 32], [u8; 32]) -> bool,
    ) -> Option<([u8; 32], [u8; 32])> {
        self.seen.retain(|(path, account), seen| {
            path != home || candidates.contains_key(account) || !cooled(seen, now)
        });
        let eligible: BTreeSet<_> = candidates
            .iter()
            .filter_map(|(account, revision)| {
                let seen = self.seen.get(&(home.to_owned(), *account));
                match seen {
                    Some(seen) if !Since::new(seen.at).elapsed_at_least(Duration::ZERO, now) => {
                        None
                    }
                    Some(seen) if seen.revision == *revision && !cooled(seen, now) => None,
                    _ => Some((
                        seen.filter(|seen| seen.revision == *revision)
                            .map(|seen| seen.at),
                        *account,
                        *revision,
                    )),
                }
            })
            .collect();
        let (account, revision) = eligible
            .into_iter()
            .map(|(_, account, revision)| (account, revision))
            .find(|(account, revision)| admit(*account, *revision))?;
        self.seen
            .insert((home.to_owned(), account), Seen { revision, at: now });
        Some((account, revision))
    }
}

/// Whether `seen`'s cooldown has gone by as of `now` ([`COOLDOWN`] since it was selected), judged
/// through `aterm_uds::trace::Since`; never for a selection stamped after `now`.
fn cooled(seen: &Seen, now: Instant) -> bool {
    Since::new(seen.at).elapsed_at_least(COOLDOWN, now)
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
