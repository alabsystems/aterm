// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The observation kernel's CHANGE TEST must see a re-entered alternate
//! screen, even one that lands on the seq the old grid showed.

use super::Model;

/// `aterm-core`'s `WatcherSet` decides whether a batch moved the surface
/// before it scans rows or latches `await seq` (`WatcherSet::is_advance`).
/// `content_seq` is per grid: `Reenter` (a `?1049l ?1049h`) installs a fresh
/// grid whose counter starts again low, and `Write`s can bring it back to the
/// very value the old grid had — box A and box B both `seq=15`, measured. The
/// alt-screen flag reads the same before and after, so it cannot tell either.
/// `epoch` is the terminal's `invalidation_epoch`, which every screen switch
/// advances and nothing lowers. `Observe` is one processed batch that changed
/// the surface (`dirty`): the kernel must read it as an advance, or every
/// watcher armed before it starves (`missed`).
///
/// `Buggy=0` is the shipped test, `(epoch, seq)`: a higher seq, or a later
/// epoch. `Buggy=1` is the old `(seq, alt)` test with `alt` constant across the
/// re-entry — `Write, Observe, Reenter, Write, Observe` reaches the old seq and
/// is missed. Tier-1 (`aterm-core/tests/conformance_observe.rs`) projects the
/// real engine's `content_seq` and `invalidation_epoch` around real re-entry
/// batches onto this state and checks the kernel's latch against `Observe`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn observation_screen_generation_model() -> Model {
    crate::ty_model! {
        ObservationScreenGeneration {
            const Buggy = 0;
            const SeqCap = 3;
            const EpochCap = 2;
            var seq = 1;
            var epoch = 0;
            var seen_seq = 1;
            var seen_epoch = 0;
            var dirty = 0;
            var missed = 0;

            action Write when (seq <= SeqCap - 1) {
                seq = seq + 1;
                dirty = 1;
            }
            action Reenter when (epoch <= EpochCap - 1) {
                epoch = epoch + 1;
                seq = 1;
                dirty = 1;
            }
            action Observe when (dirty == 1) {
                missed = if (seq > seen_seq || (Buggy == 0 && epoch > seen_epoch)) {
                    missed
                } else {
                    1
                };
                seen_seq = if (seq > seen_seq || (Buggy == 0 && epoch > seen_epoch)) {
                    seq
                } else {
                    seen_seq
                };
                seen_epoch = if (Buggy == 0 && epoch > seen_epoch) { epoch } else { seen_epoch };
                dirty = 0;
            }

            invariant EveryChangeIsSeen: missed == 0;
        }
    }
}
