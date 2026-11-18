// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One duty of the window's accounts' upkeep, made on the keeper's thread through its verdict
/// (`aterm_agent::harness::accounts_upkeep_memo`), and the keeper's look at whether the upkeep is
/// due (`aterm_gui::accounts_keeper`'s `Part::due`).
///
/// `held` is what the duty's last run left: 0 none yet, 1 a verdict kept, 2 a run not kept.
/// `dirty` and `unsettled` say whether that run did something and whether it read a file that
/// had not settled. `moved` says something it read or compared moved since; `past` that the
/// deadline it owes came (a kept run's monotonic deadline, or a run not kept's rerun); `floor`
/// that the least wait after a run not kept passed. `soil` and `stir` say whether the next run
/// will do something and read a file that has not settled. `hit` says the last make of the duty
/// answered its kept verdict without running; `due` is the last look's answer, and `cause`
/// whether a cause stood at it: no run yet, or something the last run read or compared moved, or
/// the deadline it owes came. `spent` says a run was made over a kept verdict with no cause (its
/// input unmoved, its deadline not come): a run with no verdict (none yet, or the last not
/// kept, which the memo runs at every pass) has its cause in that.
///
/// The environment moves the inputs and the clocks (`Soil`, `Stir`, `Move`, `Elapse`,
/// `Floor`); the memo answers a verdict (`Hit`) or runs the duty (`Run`); the keeper looks
/// (`Look`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_upkeep_verdict_model() -> Model {
    aterm_spec::ty_model! {
        AccountUpkeepVerdict {
            const Buggy = 0;
            var held = 0; var dirty = 0; var unsettled = 0;
            var moved = 0; var past = 0; var floor = 0;
            var soil = 0; var stir = 0;
            var hit = 0; var due = 0; var cause = 0; var spent = 0;
            action Soil when (soil == 0) { soil = 1; }
            action Stir when (stir == 0) { stir = 1; }
            action Move when (held > 0 && moved == 0) { moved = 1; hit = 0; }
            action Elapse when (held > 0 && past == 0) { past = 1; floor = 1; hit = 0; }
            action Floor when (held == 2 && floor == 0) { floor = 1; }
            action Hit when (held == 1 && (moved == 0 || Buggy == 1) && (past == 0 || Buggy == 2)) {
                hit = 1;
            }
            action Run when (held == 0 || held == 2 || moved == 1 || past == 1 || Buggy == 5) {
                spent = if held == 1 && moved == 0 && past == 0 {1} else {spent};
                held = if (soil == 0 || Buggy == 3) && (stir == 0 || Buggy == 4) {1} else {2};
                dirty = soil; unsettled = stir;
                moved = 0; past = 0; floor = 0; soil = 0; stir = 0; hit = 0;
            }
            action Look when (held <= 2) {
                due = if Buggy == 6 || held == 0 || (held == 1 && (moved == 1 || past == 1))
                    || (held == 2 && (past == 1 || (moved == 1 && floor == 1))) {1} else {0};
                cause = if held == 0 || moved == 1 || past == 1 {1} else {0};
            }
            invariant KeptIsClean: dirty == 0 || held == 2;
            invariant UnsettledNotKept: unsettled == 0 || held == 2;
            invariant HitImpliesReplayEqual: hit == 0 || moved == 0;
            invariant HitWithinHorizon: hit == 0 || past == 0;
            invariant NoRunWithoutCause: spent == 0;
            invariant DueOnlyForCause: due == 0 || cause == 1;
            bound VerdictDomain: held <= 2 && dirty <= 1 && unsettled <= 1 && moved <= 1
                && past <= 1 && floor <= 1 && soil <= 1 && stir <= 1 && hit <= 1 && due <= 1
                && cause <= 1 && spent <= 1;
        }
    }
}
