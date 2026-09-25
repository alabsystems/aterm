// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine-wide package pass record: the atpkg `update` pass as the WRITER of
//! `status.toml`'s stamps, and the schedulers that decide on them as its READERS.

use super::*;

/// `AtpkgPassStamps` — one contract for the writer and its readers, in the shape of
/// `NativeUpdateFailedMarkSuppression` (AGENTS.md).
///
/// THE WRITER (`atpkg::cli::finish_update_pass`, then the recorded pass's
/// `status::stamp_pass_end_with_index`): a full `update` pass that REACHED the signed index
/// — verified one the channel served this pass, not the §14 cache's — stamps
/// `last_success_at` and records `last_pass = "ok"`; one that did not (served from the cache
/// after a refusal, offline, or a resolve that failed) records `failed` or `offline` at
/// `last_pass_at` and leaves the success where it was. The model's `fail` is that recorded
/// failure's end while no success follows it (0 once one does). Every other write — a
/// vendor door, a typed verb, a row — moves `updated_at` alone (`OtherWrite`).
///
/// THE READERS, all over those stamps (`aterm_update_core::pkg_check`): the record's
/// last pass FAILED (`Stamps::last_failed` — the launch's "failed ⇒ due now", the walk's
/// interval after a failure, `LaunchDue`'s re-check); the last pass ATTEMPTED
/// (`Stamps::last_attempt`, the five-minute spacing); a window lane's own failure HEALED by
/// a later success (`pkg_check::healed`, the retry's re-check); and a queued pass STANDING
/// DOWN behind one that succeeded while it waited (`atpkg::cli::pass_ended_since_queued`,
/// which orders by the store's full-pass count `pass_seq`, not a clock).
///
/// `Buggy = 1` is the record as it was until 2026-09-23 (audit PK-3, PK-4): a pass served
/// from the cache stamped a SUCCESS, and the readers took `updated_at` for the last pass
/// attempted — so "attempted after the last success" read as failed. Each invariant has its
/// own counterexample: a cache-served stamp healing a sibling's ladder
/// (`HealedOnlyByAReachedPass`) or standing a waiter down (`StandDownOnlyBehindAReachedPass`)
/// with nothing reached, a vendor door's write read as a failed pass
/// (`FailedMeansTheLastPassFailed`) and as a pass attempted (`SpacingCountsOnlyFullPasses`).
/// Tier-1 (`atpkg::cli` `the_pass_record_refines_the_derived_model`) drives the real pass
/// over a real `status.toml` and reads it back through the real readers.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_pass_stamps_model() -> Model {
    crate::ty_model! {
        AtpkgPassStamps {
            const Buggy = 0;
            // Every write advances the clock, so stamps are strictly ordered. Seven writes
            // cover every ordering of the four readers' cases (and the Tier-1 trace).
            const MaxT = 7;
            var t = 0;
            // `status.toml`: `last_success_at`, the recorded failure (`last_pass_at` of a
            // pass that did not end `ok`, 0 = absent or healed), `updated_at`.
            var succ = 0;
            var fail = 0;
            var upd = 0;
            // Ghosts: when the last FULL pass ended, and whether it failed to reach the index.
            var pass_end = 0;
            var pass_failed = 0;
            // A window lane's own failed pass, and whether a pass that REACHED the index has
            // ended since it.
            var own_fail = 0;
            var reached_since_own = 0;
            // When a queued pass queued (the store's `pass_seq` then), and whether a pass
            // that REACHED the index has ended since it.
            var wait = 0;
            var reached_since_wait = 0;

            action PassReached when (t <= MaxT - 1) {
                t = t + 1;
                succ = t + 1;
                fail = 0;
                upd = t + 1;
                pass_end = t + 1;
                pass_failed = 0;
                reached_since_own = 1;
                reached_since_wait = 1;
            }
            // Another process's pass that did not reach the index: served from the §14
            // cache after a refusal, offline, a resolve that failed.
            action PassUnreached when (t <= MaxT - 1) {
                t = t + 1;
                succ = if Buggy == 1 { t + 1 } else { succ };
                fail = if Buggy == 1 { fail } else { t + 1 };
                upd = t + 1;
                pass_end = t + 1;
                pass_failed = 1;
            }
            // This window lane's own pass that did not reach the index: the lane notes it.
            action OwnPassUnreached when (t <= MaxT - 1) {
                t = t + 1;
                fail = t + 1;
                upd = t + 1;
                pass_end = t + 1;
                pass_failed = 1;
                own_fail = t + 1;
                reached_since_own = 0;
            }
            // A vendor door, a typed verb, a row: `updated_at` alone.
            action OtherWrite when (t <= MaxT - 1) {
                t = t + 1;
                upd = t + 1;
            }
            // A pass queues behind the store lock.
            action Wait when (wait == 0 && t <= MaxT - 1) {
                t = t + 1;
                wait = t + 1;
                reached_since_wait = 0;
            }

            invariant FailedMeansTheLastPassFailed:
                (if Buggy == 1 {
                    if upd > succ { 1 } else { 0 }
                } else {
                    if fail > succ { 1 } else { 0 }
                }) == pass_failed;
            invariant SpacingCountsOnlyFullPasses:
                (if Buggy == 1 {
                    upd
                } else {
                    if succ > fail { succ } else { fail }
                }) == pass_end;
            invariant HealedOnlyByAReachedPass:
                own_fail == 0 || succ <= own_fail || reached_since_own == 1;
            invariant StandDownOnlyBehindAReachedPass:
                wait == 0 || succ <= wait || reached_since_wait == 1;
        }
    }
}
