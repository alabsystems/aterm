// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Local source admission only: explicit input, automatic legacy or native source;
/// unchanged, equivalent, changed, pending or rebound observation; and config changes.
/// Publication atomicity, recovery and provider authority are outside this model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_source_capture_model() -> Model {
    aterm_spec::ty_model! {
        AccountSourceCapture {
            const Buggy = 0;
            var origin = 0; var change = 0; var config = 0;
            var admitted = 0; var native = 0; var phase = 0;
            action Origin when (phase == 0 && 2 > origin) { origin = origin + 1; }
            action Change when (phase == 0 && 4 > change) { change = change + 1; }
            action Config when (phase == 0 && 2 > config) { config = config + 1; }
            action Resolve when (phase == 0) {
                admitted = if origin == 0 {if Buggy == 5 && config > 0 {0} else {1}}
                    else if Buggy == 1 {1}
                    else if config > 0 {0}
                    else if Buggy == 2 {1}
                    else if Buggy == 3 && change == 1 {0}
                    else if 2 > change {1} else {0};
                // Updates are simultaneous: compute this choice from the same inputs,
                // not the previous state's admission value.
                native = if Buggy == 4 || 2 > origin {0}
                    else if Buggy == 1 {1}
                    else if config > 0 {0}
                    else if Buggy == 2 {1}
                    else if Buggy == 3 && change == 1 {0}
                    else if 2 > change {1} else {0};
                phase = 1;
            }
            invariant ConfigAdmission: phase == 0 || origin == 0 || config == 0 || admitted == 0;
            invariant SourceAdmission: phase == 0 || origin == 0 || 2 > change || admitted == 0;
            invariant EquivalentAvailable: phase == 0 || origin == 0 || config > 0
                || change > 1 || admitted == 1;
            invariant ExplicitChoice: phase == 0 || origin > 0 || admitted == 1;
            invariant NativeChoice: phase == 0
                || (origin == 2 && admitted == 1 && native == 1)
                || ((2 > origin || admitted == 0) && native == 0);
            bound CaptureBox: origin <= 2 && change <= 4 && config <= 2
                && admitted <= 1 && native <= 1 && phase <= 1;
        }
    }
}
