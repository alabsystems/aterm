// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A managed launch must distinguish refusal from leaving an unmanaged starter alone.
/// The fixed scenarios model local admission, not provider identity or a lease over files.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_launch_model() -> Model {
    aterm_spec::ty_model! {
        AccountPoolLaunch {
            const Buggy = 0;
            var phase = 0; var case = 0; var outcome = 0;
            action Unmanaged when (phase == 0) { phase = 1; case = 1; }
            action Ready when (phase == 0) { phase = 1; case = 2; }
            action StateUnread when (phase == 0) { phase = 1; case = 3; }
            action BindingsUnread when (phase == 0) { phase = 1; case = 4; }
            action Exhausted when (phase == 0) { phase = 1; case = 5; }
            action Drifted when (phase == 0) { phase = 1; case = 6; }
            action RosterUnread when (phase == 0) { phase = 1; case = 7; }
            action NoLocalLogin when (phase == 0) { phase = 1; case = 8; }
            action InvalidPin when (phase == 0) { phase = 1; case = 9; }
            action UsageWriteFails when (phase == 0) { phase = 1; case = 10; }
            action Choose when (phase == 1) {
                phase = 2;
                outcome = if case == 1 { if Buggy == 4 {2} else {0} }
                    else if case == 2 { if Buggy == 5 {2} else {1} }
                    else if (case == 3 && Buggy == 1)
                        || (case == 5 && Buggy == 2)
                        || (case == 6 && Buggy == 3)
                        || ((case == 4 || case == 7) && Buggy == 6)
                        || (case == 8 && Buggy == 7)
                        || (case == 9 && Buggy == 8) {0}
                    else if case == 10 && Buggy == 9 {1}
                    else {2};
            }
            invariant UnmanagedPreserved: phase <= 1 || case > 1 || outcome == 0;
            invariant ReadySelected: phase <= 1 || case <= 1 || case > 2 || outcome == 1;
            invariant UnreadHeld: phase <= 1 || (case <= 2 || case > 4) && case <= 6 || outcome == 2;
            invariant ExhaustedHeld: phase <= 1 || (case <= 4 || case > 5) && (case <= 7 || case > 8) || outcome == 2;
            invariant DriftHeld: phase <= 1 || case <= 5 || case > 6 || outcome == 2;
            invariant InvalidPinHeld: phase <= 1 || case <= 8 || case > 9 || outcome == 2;
            invariant FreshUsageRecorded: phase <= 1 || case <= 9 || outcome == 2;
        }
    }
}
