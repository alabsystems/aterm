// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Disabling desktop alerts spends notification debt without forgetting the
//! escalation that the menu still shows. Re-enabling cannot replay it.

use super::Model;

/// One session's herald, with two alternating escalation keys/labels.
/// `label` is the current menu label's identity, `seen` the remembered
/// escalation key, `pending` projects `owed > 0`, and `notice` is the last
/// decision's actual notification. `enabled` is the App's configuration,
/// not a second switch stored by the herald.
///
/// `ObserveNewLimited` holds a new transition behind the rate limiter;
/// `ObserveNewFree`, `Reobserve` and `Tick` have an available rate-limit
/// slot. `Tick` pays existing debt, while `Reobserve` rereads the same fact.
/// The model checks cancellation and remembered transitions, not timing or
/// eventual delivery. Tier-1 drives the genuine GUI herald and config edge.
///
/// `Buggy=1` restores three independent defects. `Fault=1` retains debt on
/// disable, `Fault=2` allows disabled observations to create work, and
/// `Fault=3` forgets the seen key on disable (replaying it after enable).
/// The default `Fault=0` includes every defect for per-invariant non-vacuity;
/// Tier-0 also proves and catches each selected defect independently.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn desktop_alert_debt_model() -> Model {
    crate::ty_model! {
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
