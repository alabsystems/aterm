// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Two-session ordered-input admission and fast-echo wake models.

use aterm_spec::derive::*;

/// Two-session ordered-input byte and job admission model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ordered_input_admission_model() -> Model {
    aterm_spec::ty_model! {
        OrderedInputAdmission {
            const Buggy = 0;
            var bytes = 0;
            var jobs = 0;
            var pastes = 0;
            var keys = 0;
            var tiny = 0;
            var paste_bytes = 0;
            var paste_jobs = 0;

            action AdmitPaste when (
                (jobs <= 1 && bytes <= 2) || (Buggy == 1 && pastes <= 2)
            ) {
                bytes = bytes + 2;
                jobs = jobs + 1;
                pastes = pastes + 1;
                paste_bytes = bytes + 2;
                paste_jobs = jobs + 1;
            }
            action AdmitInput when (
                (jobs <= 2 && bytes <= 4) || (Buggy == 1 && keys <= 3)
            ) {
                bytes = bytes + 1;
                jobs = jobs + 1;
                keys = keys + 1;
            }
            action AdmitTiny when (jobs <= 2 || (Buggy == 1 && tiny <= 3)) {
                jobs = jobs + 1;
                tiny = tiny + 1;
            }
            action ReleasePaste when (pastes > 0) {
                bytes = bytes - 2;
                jobs = jobs - 1;
                pastes = pastes - 1;
            }
            action ReleaseInput when (keys > 0) {
                bytes = bytes - 1;
                jobs = jobs - 1;
                keys = keys - 1;
            }
            action ReleaseTiny when (tiny > 0) {
                jobs = jobs - 1;
                tiny = tiny - 1;
            }

            invariant RetainedWithinBothBounds: bytes <= 5 && jobs <= 3;
            invariant PasteLeavesInputRoom: paste_bytes <= 4 && paste_jobs <= 2;
        }
    }
}

/// Per-session ordered-input active-state isolation model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn paste_order_sink_isolation_model() -> Model {
    aterm_spec::ty_model! {
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

            bound PendingIsBounded: pending_a <= 2 && pending_b <= 2;
            invariant DecisionReadsOnlySelectedSink: phase == 0
                || (selected == 1 && decision == if pending_a > 0 { 1 } else { 0 })
                || (selected == 2 && decision == if pending_b > 0 { 1 } else { 0 });
        }
    }
}

/// Fast-echo wake model for direct-kernel vs spill write receipts.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn queued_key_kernel_delivery_model() -> Model {
    aterm_spec::ty_model! {
        QueuedKeyKernelDelivery {
            const Buggy = 0;
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
