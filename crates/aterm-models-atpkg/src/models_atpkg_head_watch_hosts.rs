// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who watches the vendor heads: the window's head watch or, with no window open, one terminal
//! session, never both kinds at once. The rendezvous is two store-scoped `flock`s
//! (`atpkg::vendor_direct::watch::host`).

use aterm_spec::derive::*;

/// One window and two terminal sessions over one store. `w`: 0 not running, 1 running without
/// the rendezvous, 2 holding it shared (watching, for its life). `s1`, `s2`: 0 not running, 1
/// standing by without the seat, 2 in the seat between rounds, 3 in the seat mid-round holding
/// the rendezvous exclusive (watching). `orphan` is a seat whose holder is gone but which still
/// names it.
///
/// A host's `Admits` is its real claim in one step (`HostClaim::admit`): a window's shared lock
/// is granted unless a session's round holds the exclusive one; a session takes the seat when no
/// other session holds it, then the rendezvous when neither a window nor the other session
/// holds it. `EndsRound` lets the rendezvous go; `Exits` ends a process, and the kernel releases
/// whatever it held.
///
/// A window and a session never watch at once (`NeverBothKinds`); at most one session holds
/// the seat or a round (`OneSessionWatcher`); a seat never outlives its holder
/// (`NoOrphanedSeat`), so the next session takes it within a minute. `Buggy = 1` adds one dead
/// action per claim: a session that probes the rendezvous free, lets it go, then rounds on that
/// stale answer though a window took it since; a session that rounds without the seat; a seat
/// kept in a pid file, which survives its process. Tier 1 (atpkg's `host` tests) drives the real
/// claims over every reachable step, with the probe design's counterexample, refused by the real
/// session, as the negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_head_watch_hosts_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgHeadWatchHosts {
            const Buggy = 0;
            var w = 0;
            var s1 = 0;
            var s2 = 0;
            var orphan = 0;

            action WindowOpens when (w == 0) {
                w = 1;
            }
            action WindowAdmits when (w == 1) {
                w = if s1 == 3 || s2 == 3 { 1 } else { 2 };
            }
            action WindowCloses when (1 <= w) {
                w = 0;
            }

            action S1Starts when (s1 == 0) {
                s1 = 1;
            }
            action S1Admits when (s1 == 1 || s1 == 2) {
                s1 = if s1 == 2 || (s2 <= 1 && orphan == 0) {
                    if w == 2 || s2 == 3 { 2 } else { 3 }
                } else {
                    1
                };
            }
            action S1EndsRound when (s1 == 3) {
                s1 = 2;
            }
            action S1Exits when (1 <= s1) {
                s1 = 0;
            }

            action S2Starts when (s2 == 0) {
                s2 = 1;
            }
            action S2Admits when (s2 == 1 || s2 == 2) {
                s2 = if s2 == 2 || (s1 <= 1 && orphan == 0) {
                    if w == 2 || s1 == 3 { 2 } else { 3 }
                } else {
                    1
                };
            }
            action S2EndsRound when (s2 == 3) {
                s2 = 2;
            }
            action S2Exits when (1 <= s2) {
                s2 = 0;
            }

            // The replaced designs.
            action S1RoundsOnStaleProbe when (Buggy == 1 && s1 == 2 && s2 <= 2) {
                s1 = 3;
            }
            action S2RoundsWithoutSeat when (Buggy == 1 && s2 == 1 && w <= 1 && s1 <= 2) {
                s2 = 3;
            }
            action S1ExitsKeepingSeat when (Buggy == 1 && 2 <= s1) {
                s1 = 0;
                orphan = 1;
            }

            invariant NeverBothKinds:
                w <= 1 || (if s1 == 3 { 1 } else { 0 }) + (if s2 == 3 { 1 } else { 0 }) == 0;
            invariant OneSessionWatcher:
                (if 2 <= s1 { 1 } else { 0 }) + (if 2 <= s2 { 1 } else { 0 }) <= 1;
            invariant NoOrphanedSeat: orphan == 0;
        }
    }
}
