// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Same-schema boundary publication.

use aterm_spec::derive::Model;

/// One hosted task's publication boundary.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn aos_boundary_publication_model() -> Model {
    aterm_spec::ty_model! {
        AosBoundaryPublication {
            const Buggy = 0;
            const MaxIn = 3;
            const Cap = 2;
            const MaxRev = 2;
            scenario Cap = 0;
            var paused = 0;
            var rev = 0;
            var cur = 0;
            var code_v = 0;
            var state_v = 0;
            var arrived = 0;
            var queued = 0;
            var delivered = 0;
            var pa = 0;
            var ra = 0;
            var ia = 0;
            var pb = 0;
            var rb = 0;
            var ib = 0;
            var stale = 0;

            action Input when (paused == 0 && arrived + 1 <= MaxIn) {
                arrived = arrived + 1;
                delivered = delivered + 1;
                cur = cur + 1;
            }
            action Pause when (paused == 0 && rev + 1 <= MaxRev) {
                paused = 1;
            }
            action PrepareA when (paused == 1 && pa == 0) {
                pa = 1;
                ra = rev;
                ia = cur;
            }
            action PrepareB when (paused == 1 && pb == 0) {
                pb = 1;
                rb = rev;
                ib = cur;
            }
            action Hold when (paused == 1 && arrived + 1 <= MaxIn && queued + 1 <= Cap) {
                arrived = arrived + 1;
                queued = queued + 1;
            }
            action Overflow when (paused == 1 && arrived + 1 <= MaxIn && queued == Cap) {
                paused = 0;
                arrived = arrived + 1;
                delivered = delivered + queued + 1;
                cur = cur + queued + 1;
                queued = 0;
            }
            action CommitA when (paused == 1 && pa == 1 && ia == cur && (ra == rev || Buggy == 1)) {
                paused = 0;
                pa = 0;
                ra = 0;
                ia = 0;
                rev = rev + 1;
                code_v = code_v + 1;
                state_v = if Buggy == 2 { state_v } else { state_v + 1 };
                stale = if ra == rev && ia == cur { stale } else { 1 };
                cur = if Buggy == 4 { ia } else { ia + queued };
                delivered = delivered + queued;
                queued = 0;
            }
            action CommitB when (paused == 1 && pb == 1 && ib == cur && (rb == rev || Buggy == 1)) {
                paused = 0;
                pb = 0;
                rb = 0;
                ib = 0;
                rev = rev + 1;
                code_v = code_v + 1;
                state_v = if Buggy == 2 { state_v } else { state_v + 1 };
                stale = if rb == rev && ib == cur { stale } else { 1 };
                cur = if Buggy == 4 { ib } else { ib + queued };
                delivered = delivered + queued;
                queued = 0;
            }
            action Reject when (paused == 1) {
                paused = 0;
                delivered = if Buggy == 3 { delivered } else { delivered + queued };
                cur = if Buggy == 3 { cur } else { cur + queued };
                queued = 0;
            }
            action DiscardA when (pa == 1) {
                pa = 0;
                ra = 0;
                ia = 0;
            }
            action DiscardB when (pb == 1) {
                pb = 0;
                rb = 0;
                ib = 0;
            }

            invariant NeverMixed: code_v == state_v;
            invariant EachInputOnce: delivered + queued == arrived;
            invariant StaleNeverCommitted: stale == 0;
            invariant DeliveredToTheLivePair: cur == delivered;
            bound ArrivalsBounded: arrived <= MaxIn;
            bound QueueBounded: queued <= Cap;
            bound RevisionsBounded: rev <= MaxRev;
        }
    }
}
