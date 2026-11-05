// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two overlapping requests and expiry of one closed operation. The pin is
/// transient request ownership, never permanent unacknowledged history.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn noexec_flight_model() -> Model {
    aterm_spec::ty_model! {
        NoExecFlight {
            const Buggy = 0;
            var a = 0; var b = 0; var pins = 0;
            var expired = 0; var gone = 0; var idle_refusal = 0;
            action BeginA when (a == 0 && gone == 0) {
                a = 1; pins = if Buggy == 1 { pins } else { pins + 1 };
            }
            action BeginB when (b == 0 && gone == 0) {
                b = 1; pins = pins + 1;
            }
            action EndA when (a == 1) {
                a = 2; pins = if Buggy == 2 { pins } else { if Buggy == 3 { 0 } else { pins - 1 } };
            }
            action EndB when (b == 1) { b = 2; pins = pins - 1; }
            action Expire when (expired == 0) { expired = 1; }
            action Reclaim when (expired == 1 && gone == 0) {
                gone = if (pins == 0 || Buggy == 4) && (Buggy <= 4 || Buggy > 5) { 1 } else { 0 };
                idle_refusal = if pins == 0 && Buggy == 5 { 1 } else { 0 };
            }
            invariant Counted: pins == (if a == 1 {1} else {0}) + (if b == 1 {1} else {0});
            invariant RetainedWhileActive: gone == 0 || ((a == 0 || a == 2) && (b == 0 || b == 2));
            invariant IdleCanReclaim: idle_refusal == 0;
            invariant Finite: 0 <= a && a <= 2 && 0 <= b && b <= 2 && 0 <= pins && pins <= 2
                && 0 <= expired && expired <= 1 && 0 <= gone && gone <= 1
                && 0 <= idle_refusal && idle_refusal <= 1;
        }
    }
}

/// Receipt publication and authenticated observation, assuming the host has
/// already sealed an unissued operation. Kernel closure is a separate model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn noexec_receipt_model() -> Model {
    aterm_spec::ty_model! {
        NoExecReceipt {
            const Buggy = 0;
            var stored = 0; var healthy = 1; var key = 1; var same = 1;
            var records = 0; var changed = 0; var read = 0; var returned = 0;
            var absent_read = 0; var damaged_read = 0; var wrong_key_read = 0; var wrong_binding_read = 0;
            action Record when (stored == 0 && healthy == 1 && key == 1 && same == 1) {
                stored = 1; records = 1;
            }
            action Retry when (stored == 1 && healthy == 1 && key == 1 && same == 1) {
                records = if Buggy == 5 { 2 } else { records };
            }
            action ChangeBinding when (same == 1) { same = 0; }
            action Damage when (healthy == 1) { healthy = 0; }
            action LoseKey when (key == 1) { key = 0; }
            action Conflict when (stored == 1 && healthy == 1 && key == 1 && same == 0) {
                changed = if Buggy == 6 { 1 } else { changed };
            }
            action Read when (read == 0) {
                read = 1;
                returned = if (stored == 1 && healthy == 1 && key == 1 && same == 1)
                    || (stored == 0 && Buggy == 1)
                    || (stored == 1 && healthy == 0 && Buggy == 2)
                    || (stored == 1 && healthy == 1 && key == 0 && Buggy == 3)
                    || (stored == 1 && healthy == 1 && key == 1 && same == 0 && Buggy == 4) { 1 } else { 0 };
                absent_read = if stored == 0 && Buggy == 1 { 1 } else { 0 };
                damaged_read = if stored == 1 && healthy == 0 && Buggy == 2 { 1 } else { 0 };
                wrong_key_read = if stored == 1 && healthy == 1 && key == 0 && Buggy == 3 { 1 } else { 0 };
                wrong_binding_read = if stored == 1 && healthy == 1 && key == 1 && same == 0 && Buggy == 4 { 1 } else { 0 };
            }
            invariant NeedsReceipt: absent_read == 0;
            invariant NeedsIntactHistory: damaged_read == 0;
            invariant NeedsIssuer: wrong_key_read == 0;
            invariant NeedsExactBinding: wrong_binding_read == 0;
            invariant Idempotent: records <= 1;
            invariant Immutable: changed == 0;
            bound Finite: 0 <= stored && stored <= 1 && 0 <= healthy && healthy <= 1
                && 0 <= key && key <= 1 && 0 <= same && same <= 1
                && 0 <= records && records <= 2 && 0 <= changed && changed <= 1
                && 0 <= read && read <= 1 && 0 <= returned && returned <= 1 && 0 <= absent_read && absent_read <= 1
                && 0 <= damaged_read && damaged_read <= 1
                && 0 <= wrong_key_read && wrong_key_read <= 1
                && 0 <= wrong_binding_read && wrong_binding_read <= 1;
        }
    }
}
