// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The relaunch line's read of a returned prompt: whether the line that resumes an agent the
//! harness ended may be typed at the shell's prompt.

use aterm_spec::derive::{Liveness, Model, and_, eq, int, or_, var};

/// One relaunch's reads of one returned prompt.
///
/// The world, chosen first (`phase` 0 → 3) and then left alone but for a person's keys:
/// * `mark` — the shell's input mark (`blocks 1 --json`'s `cmd`/`cmdcol`, read through `line`):
///   0 none (no integration, or a newest block that is not `entering`: a running command holds
///   the terminal), 1 it names the prompt's row (it reads, left of its column, as the cursor's
///   row does), 2 it names another row: a mark the engine misplaced, or a person's typing that
///   wrapped past the prompt's row, which look the same.
/// * `stamped` — the host's stamp dates a person's last key against the agent's end (the age is
///   `text --json`'s `human_ms`, the end the one the restart asked, `relaunch::end_asked_at`);
///   0 where it cannot: an older host, a session never keyed since the host began serving it (a
///   seamless update's successor reads every adopted session so), or a relaunch on exit, whose
///   record is dated after the agent left.
/// * `blank` — the text left of the cursor ends in a blank: the prompt's own shape while nothing
///   is typed after it, else the shape of what is.
/// * `person` — a person's keys reached the shell's line since the agent's end was asked, and
///   what they typed stands there (`Key`, unfair: the world's).
///
/// One try of the relaunch line (`relaunch::first_mark` and the fenced paste after it) is
/// exactly one of:
/// * `Read`, weakly fair (the host carries a restart in flight every `CARRY_EVERY` until it
///   lands): the line goes in where a mark naming the prompt's row finds the cursor at its
///   column, where a mark names another row and the stamp proves no person keyed since the end,
///   or, with no mark, on the cursor following a blank. The shape rule's blind spot, typeahead
///   that ends in a blank, is the price of a shell with no integration, and why the first
///   invariant is about marked prompts.
/// * `WaitTyping` — the wait says `typing`: a person's text after a mark on the prompt's row, or
///   no mark, no blank and nothing to prove the line clean (a person's keys, or no stamp).
/// * `WaitElsewhere` — the wait says `mark-elsewhere`: the mark names another row and the stamp
///   cannot clear the line (a person's keys, or no stamp).
/// * `WaitUnmarked` — the wait says `unmarked`: no mark, a cursor not after a blank, and a stamp
///   that shows nobody keyed. Nothing says the shell is at a prompt (`claude; read`'s builtin
///   holds the terminal as a prompt does), so the stamp never stands in for the prompt's shape.
///
/// `said` keeps the last wait's word (1 `typing`, 2 `mark-elsewhere`, 3 `unmarked`).
///
/// `NeverOverAPersonAtAMarkedPrompt`: the line never goes in after a person's keys at a prompt
/// the shell marked. `OnlyAtAnAttestedPrompt`: the line goes in only where the shell's mark or
/// the prompt's shape says a prompt is there, never on the stamp alone.
/// `TypingIsAPersonsWhereStamped`: a wait says `typing` only with a person's keys behind it,
/// wherever the host stamps them. [`relaunch_prompt_read_liveness`] is the progress half.
///
/// `Buggy = 1` mutants, each its own action:
/// * `ElsewhereReadAsTyping` — a mark naming another row read as a person's typing, every try
///   after it waiting `typing`. Breaks the liveness and the word.
/// * `ElsewhereTypedBlind` — trust the screen over a mark it cannot match, and type. Breaks the
///   first invariant (typing that wrapped past the prompt's row).
/// * `ElsewhereByShape` — fall back to the shape rule when the mark names another row. Breaks
///   the first invariant the same way, for wrapped typing that ends in a blank.
/// * `UnmarkedTypedByStamp` — the stamp stands in for the shape where no mark says the shell is
///   at a prompt, typing into whatever holds the terminal. Breaks `OnlyAtAnAttestedPrompt`.
///
/// Tier-1 (`aterm-agent`'s `upgrade_drive_tests::
/// the_real_prompt_read_conforms_to_the_model`) drives the real
/// `relaunch::type_relaunch_line` against a stand-in tab in every world of
/// this model, with and without a person's keys, and checks that it typed
/// exactly where `Read` is enabled and said exactly the word of the wait the
/// model takes everywhere else; tab #1's world is among them, and a
/// never-keyed stamp projects as unstamped.
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

/// `APromptNobodyKeyedIsTypedAt` over [`relaunch_prompt_read_model`]: the line goes in, unless a
/// person's keys stand at the prompt or nothing the host reads can prove it clean (no mark and
/// no blank, or a mark elsewhere and no stamp). Weak fairness on one choice of each phase of the
/// world and on `Read` (the host's carry); `Key` stays unfair. Broken by `ElsewhereReadAsTyping`
/// alone.
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
        // One choice per phase says the world gets chosen (each phase's choices are enabled
        // together, and a fair one is taken unless another is).
        weak: vec!["Unmarked", "Unstamped", "EndsInText", "Read"],
        strong: vec![],
        mutants: vec!["ElsewhereReadAsTyping"],
    }
}
