// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// The first NoExec closure of an account-claimed child retains its known
/// initial phase. Existing NoExec records separately enforce exact retries.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_closure_phase_model() -> Model {
    aterm_spec::ty_model! {
        AccountClosurePhase {
            const Buggy = 0;
            var initial = 0;
            // Missing phase, forward, rollback.
            var proposed = 0;
            var checked = 0;
            var allowed = 0;
            action InitialRollback when (initial == 0 && checked == 0) { initial = 1; }
            action ProposeForward when (proposed == 0 && checked == 0) { proposed = 1; }
            action ProposeRollback when (proposed <= 1 && checked == 0) { proposed = 2; }
            action Check when (checked == 0) {
                checked = 1;
                allowed = if Buggy == 3 {0} else {
                    if proposed == 2 || (proposed == 1 && (initial == 0 || Buggy == 2))
                        || (proposed == 0 && Buggy == 1) {1} else {0}
                };
            }
            invariant NoMissingPhase: allowed == 0 || proposed > 0;
            invariant NoRollbackLoss: allowed == 0 || initial == 0 || proposed == 2;
            invariant ValidPhaseAvailable: checked == 0 || allowed == 1 || proposed == 0
                || (initial == 1 && proposed == 1);
            bound Finite: 0 <= initial && initial <= 1 && 0 <= proposed && proposed <= 2
                && 0 <= checked && checked <= 1 && 0 <= allowed && allowed <= 1;
        }
    }
}
