// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Relaunch prompt model: whether the resume line may be typed at the shell prompt.

use aterm_spec::derive::{Liveness, Model, and_, eq, int, or_, var};

/// One relaunch's reads of one returned prompt ([`relaunch_prompt_read_liveness`]).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn relaunch_prompt_read_model() -> Model {
    aterm_spec::ty_model! {
        RelaunchPromptRead {
            const Buggy = 0;
            var phase = 0;
            var mark = 0;
            var stamped = 0;
            var blank = 0;
            var person = 0;
            var typed = 0;
            var said = 0;
            var misread = 0;

            action MarkHere when (phase == 0) { phase = 1; mark = 1; }
            action MarkElsewhere when (phase == 0) { phase = 1; mark = 2; }
            action Unmarked when (phase == 0) { phase = 1; mark = 0; }
            action Stamped when (phase == 1) { phase = 2; stamped = 1; }
            action Unstamped when (phase == 1) { phase = 2; stamped = 0; }
            action EndsInBlank when (phase == 2) { phase = 3; blank = 1; }
            action EndsInText when (phase == 2) { phase = 3; blank = 0; }
            action Key when (phase == 3 && typed == 0 && person == 0) { person = 1; }
            action Read when (phase == 3 && typed == 0 && misread == 0
                && ((mark == 1 && person == 0)
                    || (mark == 2 && stamped == 1 && person == 0)
                    || (mark == 0 && blank == 1))) {
                typed = 1;
            }
            action WaitTyping when (phase == 3 && typed == 0
                && ((mark == 1 && person == 1)
                    || (mark == 0 && blank == 0 && (person == 1 || stamped == 0)))) {
                said = 1;
            }
            action WaitElsewhere when (phase == 3 && typed == 0 && mark == 2
                && (person == 1 || stamped == 0)) {
                said = 2;
            }
            action WaitUnmarked when (phase == 3 && typed == 0 && mark == 0 && blank == 0
                && stamped == 1 && person == 0) {
                said = 3;
            }
            action ElsewhereReadAsTyping when (Buggy == 1 && phase == 3 && mark == 2
                && typed == 0 && misread == 0) {
                misread = 1;
                said = 1;
            }
            action ElsewhereTypedBlind when (Buggy == 1 && phase == 3 && mark == 2
                && typed == 0) {
                typed = 1;
            }
            action ElsewhereByShape when (Buggy == 1 && phase == 3 && mark == 2
                && blank == 1 && typed == 0) {
                typed = 1;
            }
            action UnmarkedTypedByStamp when (Buggy == 1 && phase == 3 && mark == 0
                && stamped == 1 && person == 0 && typed == 0) {
                typed = 1;
            }

            invariant NeverOverAPersonAtAMarkedPrompt: typed == 0 || person == 0 || mark == 0;
            invariant OnlyAtAnAttestedPrompt: typed == 0 || mark > 0 || blank == 1;
            invariant TypingIsAPersonsWhereStamped:
                said == 0 || said > 1 || person == 1 || stamped == 0;
        }
    }
}

/// `APromptNobodyKeyedIsTypedAt` over [`relaunch_prompt_read_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(relaunch_prompt_read_model)]
pub fn relaunch_prompt_read_liveness() -> Liveness {
    Liveness {
        name: "APromptNobodyKeyedIsTypedAt",
        goal: or_(
            eq(var("typed"), int(1)),
            and_(
                eq(var("phase"), int(3)),
                or_(
                    eq(var("person"), int(1)),
                    or_(
                        and_(eq(var("mark"), int(0)), eq(var("blank"), int(0))),
                        and_(eq(var("mark"), int(2)), eq(var("stamped"), int(0))),
                    ),
                ),
            ),
        ),
        weak: vec!["Unmarked", "Unstamped", "EndsInText", "Read"],
        strong: vec![],
        mutants: vec!["ElsewhereReadAsTyping"],
    }
}
