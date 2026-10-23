// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model for raising each exception cause once across restarts and updates.

use aterm_spec::derive::Model;

/// Returns the formal model for raising each exception cause once.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exceptions_once_model() -> Model {
    aterm_spec::ty_model! {
        ExceptionsOnce {
            const Buggy = 0;
            const Reopen = 0;
            const Early = 0;
            const Volatile = 0;
            const StaleSuccessor = 0;
            const NoFlush = 0;
            var st = 0;
            var cause = 1;
            var ecause = 0;
            var fcause = 0;
            var disk = 0;
            var shown = 0;
            var dcause = 0;
            var reshown = 0;
            var succ = 0;
            var sst = 0;
            var sfc = 0;

            action Assert when (st == 0 || (st == 2 && (cause > fcause || fcause > cause)) || (st == 2 && (Buggy == 1 || Reopen == 1))) {
                st = 1;
                ecause = cause;
                shown = 0;
                disk = 0;
            }
            action Restate when (st == 1 && (ecause > cause || cause > ecause)) {
                ecause = cause;
            }
            action Log when ((st == 1 || st == 2) && disk == 0) {
                disk = 1;
            }
            action Show when (st == 1 && (disk > 0 || Buggy == 1 || Early == 1)) {
                shown = 1;
                reshown = if ecause == dcause { 1 } else { reshown };
            }
            action Dismiss when (st == 1) {
                st = 2;
                fcause = ecause;
                dcause = ecause;
                shown = 0;
            }
            action LogFiled when (st == 2 && disk == 1 && Buggy <= 0 && Volatile <= 0) {
                disk = 2;
            }
            action Resolve when (st == 1) {
                st = 0;
                shown = 0;
                disk = 0;
            }
            action Drift when (cause == 1) {
                cause = 2;
            }
            action Restart when (succ == 0) {
                shown = 0;
                st = if st == 0 { 0 } else { if st == 1 { 1 } else { if Buggy <= 0 && Volatile <= 0 { 2 } else { 1 } } };
                fcause = if st == 2 { if Buggy <= 0 && Volatile <= 0 { fcause } else { 0 } } else { fcause };
                disk = if st == 0 { disk } else { if st == 1 { 1 } else { if Buggy <= 0 && Volatile <= 0 { 2 } else { 1 } } };
            }

            action Handoff when (succ == 0) {
                succ = 1;
                sst = if disk == 2 { 2 } else { if disk == 1 { 1 } else { 0 } };
                sfc = if disk == 2 { fcause } else { 0 };
            }
            action Commit when (succ == 1) {
                disk = if NoFlush == 1 || Buggy == 1 { disk } else { if st == 0 { disk } else { if st == 1 { 1 } else { if Volatile <= 0 { 2 } else { 1 } } } };
                st = if StaleSuccessor == 1 || Buggy == 1 { sst } else { if NoFlush == 1 { if disk == 2 { 2 } else { if disk == 1 { 1 } else { 0 } } } else { if st == 0 { if disk == 2 { 2 } else { if disk == 1 { 1 } else { 0 } } } else { if st == 1 { 1 } else { if Volatile <= 0 { 2 } else { 1 } } } } };
                fcause = if StaleSuccessor == 1 || Buggy == 1 { sfc } else { if NoFlush == 1 { if disk == 2 { fcause } else { 0 } } else { fcause } };
                shown = 0;
                succ = 0;
                sst = 0;
                sfc = 0;
            }

            invariant DismissedStaysQuiet: reshown == 0;
            invariant ShownImpliesQueued: shown == 0 || disk > 0;
        }
    }
}
