// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Two-session ordered-input admission. GUI paste and key submissions have a
//! single event-loop producer; each sink's worker completes its own jobs.

use super::*;

/// A read must answer from the selected sink's outstanding jobs, regardless
/// of how many the other sink has. `Buggy=1` is the tempting process-wide
/// ACTIVE shortcut: a paste in either session orders a key in both sessions.
///
/// `Begin` is the enqueue claim, before channel send; `Complete` is the worker
/// retirement after its PTY write (or the send-failure rollback). Counts 0..2
/// cover a paste plus a following key and a partial drain in either session.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn paste_order_sink_isolation_model() -> Model {
    crate::ty_model! {
        PasteOrderSinkIsolation {
            const Buggy = 0;
            var phase = 0;
            var pending_a = 0;
            var pending_b = 0;
            var selected = 0;
            var decision = 0;

            action BeginA when (phase == 0 && pending_a <= 1) {
                pending_a = pending_a + 1;
            }
            action BeginB when (phase == 0 && pending_b <= 1) {
                pending_b = pending_b + 1;
            }
            action CompleteA when (phase == 0 && pending_a > 0) {
                pending_a = pending_a - 1;
            }
            action CompleteB when (phase == 0 && pending_b > 0) {
                pending_b = pending_b - 1;
            }
            action ReadA when (phase == 0) {
                phase = 1;
                selected = 1;
                decision = if Buggy == 1 && pending_a + pending_b > 0 {
                    1
                } else {
                    if pending_a > 0 { 1 } else { 0 }
                };
            }
            action ReadB when (phase == 0) {
                phase = 1;
                selected = 2;
                decision = if Buggy == 1 && pending_a + pending_b > 0 {
                    1
                } else {
                    if pending_b > 0 { 1 } else { 0 }
                };
            }

            invariant PendingIsBounded: pending_a <= 2 && pending_b <= 2;
            invariant DecisionReadsOnlySelectedSink: phase == 0
                || (selected == 1 && decision == if pending_a > 0 { 1 } else { 0 })
                || (selected == 2 && decision == if pending_b > 0 { 1 } else { 0 });
        }
    }
}

/// A queued press renews fast echo presentation only from a full, non-empty
/// direct-kernel receipt. A spill receipt does not wake, even after its backlog
/// drains: the worker must retire its FIFO job immediately rather than wait for
/// a whole-sink condition unrelated writers can extend. `Buggy=1` replays the
/// mistake of treating spill admission as direct delivery. Tier-1 drives real
/// sink receipts through the GUI worker's wake decision.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn queued_key_kernel_delivery_model() -> Model {
    crate::ty_model! {
        QueuedKeyKernelDelivery {
            const Buggy = 0;
            // 0 unwritten, 1 accepted, 2 wake posted, 3 failed/no wake.
            var phase = 0;
            var accepted_full = 0;
            var direct_receipt = 0;
            var kernel_drained = 0;
            var wake = 0;

            action AcceptDirect when (phase == 0) {
                phase = 1;
                accepted_full = 1;
                direct_receipt = 1;
                kernel_drained = 1;
            }
            action AcceptSpill when (phase == 0) {
                phase = 1;
                accepted_full = 1;
            }
            action Reject when (phase == 0) {
                phase = 3;
            }
            action DrainSuccess when (phase == 1 && kernel_drained == 0) {
                kernel_drained = 1;
            }
            action DrainFailure when (phase == 1 && kernel_drained == 0) {
                phase = 3;
            }
            action PostWake when (
                phase == 1 && accepted_full == 1 &&
                (direct_receipt == 1 || Buggy == 1)
            ) {
                phase = 2;
                wake = 1;
            }

            invariant WakeRequiresDirectReceipt:
                wake == 0 || (accepted_full == 1 && direct_receipt == 1);
        }
    }
}
