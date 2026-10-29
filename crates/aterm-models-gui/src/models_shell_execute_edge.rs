// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shell execution rising-edge pulse latch (`RainLatches::note_shell_executing`).

use aterm_spec::derive::Model;

/// Rising-edge detector over `(session, executing)` observations: only an observed `false -> true`
/// transition within the same session emits an `Execute` rain pulse. `Buggy = 1` treats a held
/// `executing == true` level or an initial `true` observation as a pulse.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shell_execute_edge_model() -> Model {
    aterm_spec::ty_model! {
        ShellExecuteEdge {
            const Buggy = 0;
            var observed = 0;
            var session = 0;
            var executing = 0;
            var pulse = 0;
            var valid_edge = 0;
            action ObserveIdle0 {
                valid_edge = 0;
                pulse = 0;
                observed = 1;
                session = 0;
                executing = 0;
            }
            action ObserveExec0 {
                valid_edge = if observed == 1 && session == 0 && executing == 0 { 1 } else { 0 };
                pulse = if Buggy == 1 { 1 } else { if observed == 1 && session == 0 && executing == 0 { 1 } else { 0 } };
                observed = 1;
                session = 0;
                executing = 1;
            }
            action ObserveIdle1 {
                valid_edge = 0;
                pulse = 0;
                observed = 1;
                session = 1;
                executing = 0;
            }
            action ObserveExec1 {
                valid_edge = if observed == 1 && session == 1 && executing == 0 { 1 } else { 0 };
                pulse = if Buggy == 1 { 1 } else { if observed == 1 && session == 1 && executing == 0 { 1 } else { 0 } };
                observed = 1;
                session = 1;
                executing = 1;
            }
            action Reset {
                observed = 0;
                session = 0;
                executing = 0;
                pulse = 0;
                valid_edge = 0;
            }
            bound BooleanBounds: observed <= 1 && session <= 1 && executing <= 1 && pulse <= 1 && valid_edge <= 1;
            invariant PulseExactlyMatchesEdge: pulse == valid_edge;
        }
    }
}
