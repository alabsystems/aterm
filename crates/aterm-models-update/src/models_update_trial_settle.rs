// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A boot trial whose recovery proof keeps being refused ends: settled at its bound, never
//! reverted for it.

use aterm_spec::derive::{Liveness, Model, eq, int, var};

/// One armed trial as the processes that run its build see it. `refusals` is the running
/// process's streak before its next ask (it saturates one below `SettleAfter`: the ask that
/// settles is the refusal that reaches it), `spanned` whether that streak's refusal will have
/// lasted the bound's awake span at the next ask. A span is measured only from a refusal
/// (`ASpanStartsAtARefusal`), so a retry that drops the count while keeping the time is
/// caught as a safety fault as well as a trial that never ends. A relaunch is a new process:
/// it starts no inherited streak and is counted toward the revert, which alone takes the
/// build back. `settled` is 1 for a proven settle and 2 for an unproven one.
///
/// Not modelled, and unbounded in the code: a window that never draws reaches no window
/// checkpoint, so its trial stays armed until a window of it draws or a relaunch counts; a
/// sentinel the filesystem will not remove stays armed; a refusal that says the installed
/// bundle is not the running build, with a predecessor that verifies, is never counted (here
/// it is no `Refuse`: only `Relaunch` and `Revert` move that trial); and a process with no
/// awake clock never takes `Elapse`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_trial_settle_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateTrialSettle {
            const Buggy = 0;
            const SettleAfter = 3;
            const MaxLaunch = 3;
            var armed = 1;
            var launches = 1;
            var refusals = 0;
            var spanned = 0;
            var rollback = 1;
            var reverted = 0;
            var settled = 0;
            var settled_spanned = 0;

            action Refuse when (armed == 1 && (refusals + 2 <= SettleAfter || spanned == 0)) {
                refusals = if refusals + 2 <= SettleAfter { refusals + 1 } else { refusals };
            }
            action Elapse when (armed == 1 && refusals > 0 && spanned == 0) {
                spanned = 1;
            }
            action Settle when (
                armed == 1 && SettleAfter <= refusals + 1 && (spanned == 1 || Buggy == 2)
            ) {
                armed = 0;
                settled = 2;
                settled_spanned = spanned;
                reverted = if Buggy == 3 { 1 } else { reverted };
                rollback = if Buggy == 3 { 0 } else { rollback };
            }
            action Prove when (armed == 1) {
                armed = 0;
                settled = 1;
            }
            action Relaunch when (armed == 1 && launches + 1 <= MaxLaunch) {
                launches = launches + 1;
                refusals = 0;
                spanned = 0;
            }
            action Revert when (armed == 1 && MaxLaunch <= launches) {
                armed = 0;
                reverted = 1;
                rollback = 0;
            }

            action RetryForgetsTheStreak when (Buggy == 1 && armed == 1 && refusals > 0) {
                refusals = 0;
            }

            invariant SettledOnlyPastSpan: settled <= 1 || settled_spanned == 1;
            invariant UnprovenSettleKeepsTheBuild:
                settled <= 1 || (reverted == 0 && rollback == 1);
            invariant RevertOnlyByLaunches: reverted == 0 || MaxLaunch <= launches;
            invariant ASpanStartsAtARefusal: spanned == 0 || refusals > 0;
            bound StreakIsFinite: refusals <= SettleAfter;
            bound LaunchesCounted: launches <= MaxLaunch;
        }
    }
}

/// [`native_update_trial_settle_model`]'s progress claim: the trial ends. The mutant is the
/// defect this model was written for, a retry that keeps no count.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(native_update_trial_settle_model)]
pub fn native_update_trial_settle_liveness() -> Liveness {
    Liveness {
        name: "TheTrialEnds",
        goal: eq(var("armed"), int(0)),
        weak: vec!["Refuse", "Elapse", "Settle"],
        strong: vec![],
        mutants: vec!["RetryForgetsTheStreak"],
    }
}
