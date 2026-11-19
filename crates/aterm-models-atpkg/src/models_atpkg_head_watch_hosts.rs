// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who watches the vendor heads.

use aterm_spec::derive::*;

/// One window and two terminal sessions over one store.
///
/// `w` is 0 with no window, 1 for one open but refused, 2 for one holding the rendezvous shared.
/// `s1` and `s2` are 0 for no session, 1 for one without the seat, 2 for one holding the seat,
/// 3 for one holding the seat and the rendezvous exclusive. A session's claim has a ring, as every
/// runner's has (`Ringed`): refused, it arms a waiter, and `q1`/`q2` is 1 while that waiter blocks
/// on the seat, 2 while it blocks on the rendezvous, and 3 once it took the lock in the step its
/// holder let go and handed it over (the lock counts in `s1`/`s2`), until the session's next
/// admission takes it. The `Ringed = 0` scenario is a claim with no waiter (no ring, a waiter
/// that failed, no thread), which asks again on its own clock.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_head_watch_hosts_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgHeadWatchHosts {
            const Buggy = 0;
            const Ringed = 1;
            scenario Ringed = 0;
            var w = 0;
            var s1 = 0;
            var s2 = 0;
            var q1 = 0;
            var q2 = 0;
            var orphan = 0;

            action WindowOpens when (w == 0) {
                w = 1;
            }
            action WindowAdmits when (w == 1) {
                w = if s1 == 3 || s2 == 3 { 1 } else { 2 };
            }
            action WindowCloses when (1 <= w) {
                w = 0;
                s1 = if q1 == 2 { if Buggy == 2 { s1 } else { 3 } } else { s1 };
                q1 = if q1 == 2 { if Buggy == 2 { q1 } else { 3 } } else { q1 };
                s2 = if q2 == 2 { 3 } else { s2 };
                q2 = if q2 == 2 { 3 } else { q2 };
            }

            action S1Starts when (s1 == 0) {
                s1 = 1;
            }
            action S1Admits when (((s1 == 1 || s1 == 2) && q1 == 0) || q1 == 3) {
                s1 = if s1 == 3 {
                    3
                } else {
                    if s1 == 2 || (s2 <= 1 && orphan == 0) {
                        if w == 2 || s2 == 3 { 2 } else { 3 }
                    } else {
                        1
                    }
                };
                q1 = if Ringed == 0 || s1 == 3 {
                    0
                } else {
                    if s1 == 2 || (s2 <= 1 && orphan == 0) {
                        if w == 2 || s2 == 3 { 2 } else { 0 }
                    } else {
                        1
                    }
                };
            }
            action S1EndsRound when (s1 == 3 && q1 == 0) {
                s1 = 2;
            }
            action S1Exits when (1 <= s1) {
                s1 = 0;
                q1 = 0;
                s2 = if q2 == 1 && 2 <= s1 { 2 } else { s2 };
                q2 = if q2 == 1 && 2 <= s1 { 3 } else { q2 };
            }

            action S2Starts when (s2 == 0) {
                s2 = 1;
            }
            action S2Admits when (((s2 == 1 || s2 == 2) && q2 == 0) || q2 == 3) {
                s2 = if s2 == 3 {
                    3
                } else {
                    if s2 == 2 || (s1 <= 1 && orphan == 0) {
                        if w == 2 || s1 == 3 { 2 } else { 3 }
                    } else {
                        1
                    }
                };
                q2 = if Ringed == 0 || s2 == 3 {
                    0
                } else {
                    if s2 == 2 || (s1 <= 1 && orphan == 0) {
                        if w == 2 || s1 == 3 { 2 } else { 0 }
                    } else {
                        1
                    }
                };
            }
            action S2EndsRound when (s2 == 3 && q2 == 0) {
                s2 = 2;
            }
            action S2Exits when (1 <= s2) {
                s2 = 0;
                q2 = 0;
                s1 = if q1 == 1 && 2 <= s2 { 2 } else { s1 };
                q1 = if q1 == 1 && 2 <= s2 { 3 } else { q1 };
            }

            action S1RoundsOnStaleProbe when (Buggy == 1 && s1 == 2 && s2 <= 2) {
                s1 = 3;
            }
            action S2RoundsWithoutSeat when (Buggy == 1 && s2 == 1 && w <= 1 && s1 <= 2) {
                s2 = 3;
            }
            action S1ExitsKeepingSeat when (Buggy == 1 && 2 <= s1) {
                s1 = 0;
                q1 = 0;
                orphan = 1;
            }

            invariant NeverBothKinds:
                w <= 1 || (if s1 == 3 { 1 } else { 0 }) + (if s2 == 3 { 1 } else { 0 }) == 0;
            invariant OneSessionWatcher:
                (if 2 <= s1 { 1 } else { 0 }) + (if 2 <= s2 { 1 } else { 0 }) <= 1;
            invariant NoOrphanedSeat: orphan == 0;
            invariant WaitsOnlyOnAHolder:
                (if q1 == 1 && s2 <= 1 && orphan == 0 { 1 } else { 0 })
                    + (if q1 == 2 && w <= 1 && s2 <= 2 { 1 } else { 0 })
                    + (if q2 == 1 && s1 <= 1 && orphan == 0 { 1 } else { 0 })
                    + (if q2 == 2 && w <= 1 && s1 <= 2 { 1 } else { 0 })
                    == 0;
        }
    }
}
