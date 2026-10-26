// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded GUI read decisions over absent, empty and populated rosters and real
/// pending-record fixtures. This does not model concurrent external file editors.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_gui_read_model() -> Model {
    aterm_spec::ty_model! {
        AccountGuiRead {
            const Buggy = 0;
            var phase = 0; var reader = 1; var shape = 0; var fault = 1;
            var available = 0; var coherent = 1; var stable = 1; var preserved = 1;
            action AnotherReader when (phase == 0 && 4 > reader) { reader = reader + 1; }
            action AnotherShape when (phase == 0 && 2 > shape) { shape = shape + 1; }
            action AnotherFault when (phase == 0 && 7 > fault) { fault = fault + 1; }
            action Read when (phase == 0) {
                phase = 1;
                available = if 3 > fault || fault == 7 && shape == 2 {
                    if Buggy == 1 {0} else {1}
                } else { if Buggy == 2 {1} else {0} };
                coherent = if Buggy == 3 {0} else {1};
                stable = if Buggy == 4 {0} else {1};
                preserved = if Buggy == 5 {0} else {1};
            }
            invariant ReplayableReadAvailable:
                phase == 0 || (fault > 2 && (7 > fault || 2 > shape)) || available == 1;
            invariant UnresolvedReadRefused:
                phase == 0 || 3 > fault || (fault == 7 && shape == 2) || available == 0;
            invariant ExpectedReadAndRecords: coherent == 1;
            invariant StableRepeat: stable == 1;
            invariant CredentialsAndRosterPreserved: preserved == 1;
        }
    }
}
