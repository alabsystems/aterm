// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Outgoing process wait model for a launched successor's rendezvous dial.

use aterm_spec::derive::{Liveness, Model, eq, gt, int, le, or_, var};

/// Model of the parent's wait for a launched successor's dial.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_launched_dial_wait_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateLaunchedDialWait {
            const Buggy = 0;
            var successor = 0;
            var answer = 0;
            var slot = 0;
            var wait = 0;
            var expired = 0;
            var polls_answer = 1;
            var watches_named = 1;
            var taken = 0;

            action Dial when (successor == 0) {
                successor = 1;
            }
            action Die when (successor == 0) {
                successor = 2;
            }
            action AnswerExited when (answer == 0 && successor == 2) {
                answer = 2;
                slot = 1;
            }
            action AnswerOther when (answer == 0) {
                answer = 1;
                slot = 1;
            }
            action AnswerNamed when (answer == 0) {
                answer = 3;
                slot = 1;
            }
            action Expire when (wait == 0 && expired == 0) {
                expired = 1;
            }

            action Accept when (wait == 0 && successor == 1) {
                wait = 1;
            }
            action TimeOut when (wait == 0 && expired == 1) {
                wait = 2;
            }
            action EndOnExit when (wait == 0 && polls_answer == 1 && slot == 1 && answer == 2) {
                wait = 3;
            }
            action EndOnNamedExit when (
                wait == 0 && watches_named == 1 && slot == 1 && answer == 3 && successor == 2
            ) {
                wait = 3;
            }
            action Corroborate when (wait == 1 && slot == 1 && taken == 0) {
                slot = 0;
                taken = 1;
            }

            action EndOnAnyAnswer when (Buggy == 1 && wait == 0 && slot == 1 && answer == 1) {
                wait = 3;
            }
            action ConsumingPoll when (Buggy == 1 && wait == 0 && slot == 1) {
                slot = 0;
            }
            action PollCancelOnly when (Buggy == 1 && wait == 0 && polls_answer == 1) {
                polls_answer = 0;
            }
            action EndOnNamedPid when (Buggy == 1 && wait == 0 && slot == 1 && answer == 3) {
                wait = 3;
            }
            action IgnoreNamedPid when (Buggy == 1 && wait == 0 && watches_named == 1) {
                watches_named = 0;
            }

            invariant NeverRefusesALiveSuccessor: wait <= 2 || successor == 2;
            invariant AnswerKeptForTheCorroboration: answer == 0 || slot == 1 || taken == 1;
            invariant AReportedDeathIsSeen:
                wait > 0 || answer <= 1 || answer > 2 || polls_answer == 1;
            invariant ANamedDeathIsSeen: wait > 0 || answer <= 2 || watches_named == 1;
        }
    }
}

/// [`native_update_launched_dial_wait_model`]'s progress claim.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(native_update_launched_dial_wait_model)]
pub fn native_update_launched_dial_wait_liveness() -> Liveness {
    Liveness {
        name: "TheDeadSuccessorEndsTheWait",
        goal: or_(
            gt(var("wait"), int(0)),
            or_(le(var("successor"), int(1)), eq(var("answer"), int(1))),
        ),
        weak: vec!["AnswerExited", "EndOnExit", "EndOnNamedExit"],
        strong: vec![],
        mutants: vec!["PollCancelOnly", "IgnoreNamedPid"],
    }
}
