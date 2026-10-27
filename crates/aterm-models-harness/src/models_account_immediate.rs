// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Rotation attempts are recorded before restart; completion follows its durable write.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_immediate_model() -> Model {
    aterm_spec::ty_model! {
        AccountImmediate {
            const Buggy = 0;
            var phase = 0; var case = 1;
            var prepared = 0; var restarts = 0; var rotations = 0; var strikes = 0;
            var moved = 0; var pending = 0; var bound = 0; var present = 1;
            action AnotherCase when (phase == 0 && case <= 7) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Observe when (phase == 1) {
                phase = 2;
                prepared = if Buggy == 1 && case == 1 {0} else {1};
                restarts = if Buggy == 4 && case == 3 {2} else {if Buggy == 8 && case == 7 {1}
                    else {if case == 7 {0} else {if case == 4 || case == 5 || case == 6 {2} else {1}}}};
                rotations = if (Buggy == 5 && case == 4) || (Buggy == 7 && case == 6) {1}
                    else {if case == 7 {0} else {if case == 4 || case == 5 || case == 6 {2} else {1}}};
                strikes = if Buggy == 6 && case == 4 {1}
                    else {if case == 4 {2} else {if case == 5 {1} else {0}}};
                moved = if Buggy == 2 && case == 2 {1} else {if Buggy == 10 && case == 1 {0}
                    else {if case == 1 || case == 3 || case == 5 || case == 6 {1} else {0}}};
                pending = if Buggy == 3 && case == 2 {0} else {if Buggy == 12 && case == 3 {1}
                    else {if case == 2 {1} else {0}}};
                bound = if Buggy == 11 && case == 5 {0}
                    else {if case == 1 || case == 3 || case == 5 || case == 6 {1} else {0}};
                present = if Buggy == 9 && case == 8 {1} else {if case == 8 {0} else {1}};
            }
            invariant PreparedBeforeRestart: phase <= 1 || restarts == 0 || prepared == 1;
            invariant CommittedBeforeMoved: phase <= 1 || moved == 0 || bound == 1;
            invariant RetainUnwritten: phase <= 1 || case <= 1 || case > 2 || (pending == 1 && moved == 0);
            invariant RetryWithoutRestart: phase <= 1 || case <= 2 || case > 3 || restarts == 1;
            invariant AttemptsRetained: phase <= 1 || case <= 3 || case > 6 || rotations == 2;
            invariant FailedAttemptsRetained: phase <= 1 || case <= 3 || case > 4 || strikes == 2;
            invariant RefusedBeforeRestart: phase <= 1 || case <= 6 || case > 7 || (restarts == 0 && rotations == 0);
            invariant RetiredIsQuiet: phase <= 1 || case <= 7 || (moved == 0 && pending == 0 && present == 0);
            invariant SuccessCompletes: phase <= 1 || case == 2 || case == 4 || case > 6 || (moved == 1 && pending == 0 && bound == 1);
        }
    }
}
