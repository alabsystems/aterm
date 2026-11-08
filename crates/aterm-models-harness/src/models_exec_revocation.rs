// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// One automatic revocation across reachable local transaction states. A
/// prospective receipt write is allowed before Go, and cancellation retains
/// grant history. This models neither process death nor account reclamation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exec_revocation_model() -> Model {
    aterm_spec::ty_model! {
        ExecRevocation {
            const Buggy = 0;
            // Armed, Reserved, Reserved with a prospective marker, Executing,
            // Loaded, Consumed, Cancelled before Go, Cancelled after Go.
            var case = 0; var done = 0;
            var history = 0; var cancelled = 0; var terminate = 0; var late = 0;
            action NextCase when (done == 0 && case <= 6) { case = case + 1; }
            action Revoke when (done == 0) {
                done = 1;
                history = if Buggy == 2 && (case <= 4 || case > 5) {0}
                    else if (case > 2 && case <= 5) || case == 7 {1} else {0};
                cancelled = if Buggy == 3 && case == 2 {0}
                    else if Buggy == 4 || case <= 4 || case > 5 {1} else {0};
                terminate = if Buggy == 1 && case == 2 {1}
                    else if Buggy == 4 && case == 5 {1}
                    else if Buggy == 2 || case == 5 {0}
                    else if case == 3 || case == 4 || case == 7 {1} else {0};
                late = if Buggy == 3 && case == 2 {1} else {0};
            }
            invariant GrantHistory: done == 0 || history ==
                if (case > 2 && case <= 5) || case == 7 {1} else {0};
            invariant NoPrematureTermination: terminate == 0 || case == 3 || case == 4 || case == 7;
            invariant GrantedRevocation: done == 0 || case <= 2 || case == 5 || case == 6 || terminate == 1;
            invariant AdmissionClosed: done == 0 || case == 5 || (cancelled == 1 && late == 0);
            invariant ConsumedUnchanged: case <= 4 || case > 5 || cancelled == 0;
            bound Finite: 0 <= case && case <= 7 && 0 <= done && done <= 1
                && 0 <= history && history <= 1 && 0 <= cancelled && cancelled <= 1
                && 0 <= terminate && terminate <= 1 && 0 <= late && late <= 1;
        }
    }
}
