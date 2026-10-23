// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of desktop alert notification debt across enable/disable transitions.

use aterm_spec::derive::Model;

/// One session's herald, with two alternating escalation keys/labels.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn desktop_alert_debt_model() -> Model {
    aterm_spec::ty_model! {
        DesktopAlertDebt {
            const Buggy = 0;
            const Fault = 0;
            var enabled = 0;
            var label = 0;
            var seen = 0;
            var pending = 0;
            var notice = 0;

            action Enable when (enabled == 0) {
                enabled = 1;
                notice = 0;
            }

            action Disable when (enabled == 1) {
                enabled = 0;
                pending = if Buggy == 1 && (Fault == 0 || Fault == 1) {
                    pending
                } else { 0 };
                seen = if Buggy == 1 && (Fault == 0 || Fault == 3) {
                    0
                } else { seen };
                notice = 0;
            }

            action ObserveNewLimited {
                label = if label == 1 { 2 } else { 1 };
                seen = if label == 1 { 2 } else { 1 };
                pending = if enabled == 1 || (Buggy == 1 && (Fault == 0 || Fault == 2)) {
                    1
                } else { 0 };
                notice = 0;
            }

            action ObserveNewFree {
                label = if label == 1 { 2 } else { 1 };
                seen = if label == 1 { 2 } else { 1 };
                pending = 0;
                notice = if enabled == 1 || (Buggy == 1 && (Fault == 0 || Fault == 2)) {
                    1
                } else { 0 };
            }

            action Reobserve {
                seen = label;
                pending = 0;
                notice = if label > 0 && (enabled == 1 || (Buggy == 1 && (Fault == 0 || Fault == 2))) {
                    if pending > 0 { 1 } else { if seen == label { 0 } else { 1 } }
                } else { 0 };
            }

            action Tick {
                pending = 0;
                notice = if enabled == 1 && pending > 0 { 1 } else { 0 };
            }

            invariant DisabledOwnsNoDebt: enabled == 1 || pending == 0;
            invariant SeenTracksLabel: seen == label;
            invariant OnlyEnabledNotices: enabled == 1 || notice == 0;
        }
    }
}
