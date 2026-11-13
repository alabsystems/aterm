// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fallback_convergence_action;

#[test]
fn fallback_convergence_truth_table() {
    for (pending, was, want, why) in [
        (true, false, (true, false), "first pending frame"),
        (true, true, (true, false), "steady pending frame"),
        (false, true, (true, true), "landing edge"),
        (false, false, (false, false), "not converging"),
    ] {
        assert_eq!(fallback_convergence_action(pending, was), want, "{why}");
    }
}
