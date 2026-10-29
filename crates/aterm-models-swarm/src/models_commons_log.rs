// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of log-backed Commons control across partitioned islands
//! (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.6, §3.3.7, §3.5.3, and §4.1 F18).

use aterm_spec::derive::*;

/// `CommonsLog`: two partitioned islands of one log-backed Commons installation (`{m1, m2, m3,
/// m4}`, founded at `Commons` epoch generation 1) admit, remove, merge their `.records` streams,
/// and reconcile missing `Wrap`s and concurrent `Commons` epochs.
///
/// Island A (`{m1, m2}`) may admit newcomer `D` (`admitted_d`, wrapping its current `Commons`
/// epoch without rotating generation or touching `Journal`) and may remove `B` (`removed_b`,
/// rotating Island A's `Commons` epoch). Island B (`{m3, m4}`) may remove `C` (`removed_c`,
/// rotating Island B's `Commons` epoch without wrapping `C` or unknown `D`). Meanwhile, legacy v1
/// authority-roster records may fork `Journal` (`journal_fork == 1`).
///
/// `MergeIslands` takes the set union of `.records` before reconciliation: concurrent removals on
/// both islands produce `epoch_heads == 2`, and if Island B rotated while Island A admitted `D`,
/// `D` lacks a `Wrap` of Island B's maximal epoch (`d_wrapped_current == 0`). `Reconcile` runs
/// `reconcile_commons_log`, closing `D`'s wrap gap (`d_wrapped_current == 1`), coalescing
/// concurrent maximal epochs into one higher `Commons` epoch (`epoch_heads == 1`), keeping removed
/// `C` out (`c_has_current == 0`), and preserving log-backed Commons readiness even when legacy
/// `Journal` is conflicted (`ready == 1`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn commons_log_model() -> Model {
    aterm_spec::ty_model! {
        CommonsLog {
            const Buggy = 0;

            var admitted_d = 0;
            var removed_b = 0;
            var removed_c = 0;
            var journal_fork = 0;
            var merged = 0;
            var reconciled = 0;
            var epoch_heads = 1;
            var d_wrapped_current = 0;
            var c_has_current = 1;
            var ready = 1;

            action AdmitDOnIslandA when (merged == 0 && admitted_d == 0) {
                admitted_d = 1;
                d_wrapped_current = 1;
            }
            action RemoveBOnIslandA when (merged == 0 && removed_b == 0) {
                removed_b = 1;
            }
            action RemoveCOnIslandB when (merged == 0 && removed_c == 0) {
                removed_c = 1;
                c_has_current = 0;
            }
            action ForkLegacyJournal when (journal_fork == 0 && reconciled == 0) {
                journal_fork = 1;
            }
            action MergeIslands when (merged == 0) {
                merged = 1;
                epoch_heads = if removed_b == 1 && removed_c == 1 { 2 } else { 1 };
                d_wrapped_current = if admitted_d == 1 && removed_c == 0 { 1 } else { 0 };
                c_has_current = if removed_c == 1 { 0 } else { 1 };
            }
            action Reconcile when (merged == 1 && reconciled == 0) {
                reconciled = 1;
                epoch_heads = if Buggy == 3 && removed_b == 1 && removed_c == 1 {
                    2
                } else {
                    1
                };
                d_wrapped_current = if admitted_d == 1 {
                    if Buggy == 2 && removed_c == 1 { 0 } else { 1 }
                } else {
                    0
                };
                c_has_current = if removed_c == 1 {
                    if Buggy == 1 { 1 } else { 0 }
                } else {
                    1
                };
                ready = if Buggy == 4 && journal_fork == 1 { 0 } else { 1 };
            }

            invariant RemovedLacksCurrentAccess: removed_c == 0 || c_has_current == 0;
            invariant WrapGapClosedAfterReconcile:
                reconciled == 0 || admitted_d == 0 || d_wrapped_current == 1;
            invariant ConcurrentEpochsCoalesced: reconciled == 0 || epoch_heads == 1;
            invariant JournalForkDoesNotBlockLogAccess: reconciled == 0 || ready == 1;
            bound CommonsLogBox:
                admitted_d <= 1 && removed_b <= 1 && removed_c <= 1 && journal_fork <= 1 &&
                merged <= 1 && reconciled <= 1 && epoch_heads <= 2 && d_wrapped_current <= 1 &&
                c_has_current <= 1 && ready <= 1;
        }
    }
}
