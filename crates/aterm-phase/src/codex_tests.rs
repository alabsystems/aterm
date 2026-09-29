// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One test per measured Codex screen (codex 0.156.1, and the 0.157 ones
//! measured since), each with its negative control.

use super::fixtures::*;
use super::*;
use crate::prompt::fixtures::{provenance, rows, screen};
use crate::reader::{ClaudeReader, CodexReader, Program, ScreenReader, identify, read};

fn codex(text: &str) -> crate::reader::Reading {
    read(Some("codex"), &screen(text), None)
}

fn roles(p: &PromptV2) -> Vec<Role> {
    p.options.iter().map(|o| o.role).collect()
}

fn labels(p: &PromptV2) -> Vec<&str> {
    p.options.iter().map(|o| o.label.as_str()).collect()
}

const ALL: &[&str] = &[
    TRUST,
    IDLE,
    BUSY,
    STREAMING,
    END_OF_TURN,
    BOX_EXEC,
    BOX_PATCH,
    QUESTION,
    PLAN,
    INTERRUPTED,
    STATUS,
    QUESTION_END,
    DRAFT,
    EXIT,
    HIT_LIMIT,
    TRUST_TALL_PANE,
    END_OF_TURN_TIP,
    RATE_NUDGE,
    IDLE_158,
    MODEL_PICK,
    EFFORT_PICK,
    EFFORT_PICK_MORE,
    EFFORT_PICK_OTHER,
    ADVANCED_PICK,
    ADVANCED_PICK_ULTRA,
    GOAL_PAUSE_NONE,
    GOAL_BUSY,
    GOAL_BUSY_0_158,
    GOAL_NEXT_TURN_0_158,
    GOAL_RESUME,
    HIT_LIMIT_NO_RESET,
    QUESTION_0157,
    PLAN_0157,
    RESUMED_DAEMON,
    RESUMED_DAEMON_0_159,
    RESUMED_INLINE_0_159,
];

/// Every Codex fixture names the program, its version and how its rows were
/// got on line 1 — and only the one hand-built from the binary says so.
#[test]
fn every_fixture_names_codex_its_version_and_its_provenance() {
    for text in ALL {
        let line = provenance(text).expect("a provenance line");
        assert!(
            ["0.156.1", "0.157.0", "0.157.1", "0.158.0", "0.159.0"]
                .iter()
                .any(|v| line.starts_with(&format!("codex {v} · "))),
            "{line}"
        );
        let measured = line.contains(" · MEASURED 2026-09-2");
        assert_eq!(measured, !line.contains("HAND-BUILT"), "{line}");
        assert!(!screen(text)[0].starts_with("# "), "{line}");
    }
    assert!(provenance(HIT_LIMIT).is_some_and(|l| l.contains("HAND-BUILT")));
    assert!(provenance(RATE_NUDGE).is_some_and(|l| l.contains("HAND-BUILT")));
    assert!(provenance(GOAL_RESUME).is_some_and(|l| l.contains("HAND-BUILT")));
    for measured in [MODEL_PICK, EFFORT_PICK, ADVANCED_PICK, IDLE_158] {
        assert!(provenance(measured).is_some_and(|l| l.contains("MEASURED 2026-09-28")));
    }
}

/// A fresh session at its composer: idle, and evidence (the session card,
/// no message yet). The composer's text is the placeholder; its cursor
/// (row 15, col 2, measured) is what says so. NEGATIVE CONTROL: the busy
/// screen's composer reads the same placeholder and the phase is busy.
#[test]
fn a_fresh_session_is_idle_at_its_composer() {
    let r = codex(IDLE);
    assert_eq!(r.program, Program::Codex);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(r.prompt, None);
    assert_eq!(r.wall, None);
    assert_eq!(r.said_tail, None);
    assert!(!r.interrupted);
    assert_eq!(
        r.composer,
        Some((
            15,
            vec![anchor_text("codex.composer.placeholder").to_string()]
        ))
    );
    assert_eq!(r.suggestion, None, "the placeholder is no suggestion");
    let busy = codex(BUSY);
    assert_eq!(busy.phase, Phase::Busy, "the control");
    assert_eq!(
        busy.composer.map(|(_, l)| l),
        Some(vec![anchor_text("codex.composer.placeholder").to_string()])
    );
}

/// The status row over the composer is busy. NEGATIVE CONTROL: the same
/// screen with the status row gone and an end row under the last block is
/// an ended turn.
#[test]
fn the_status_row_is_busy() {
    let r = screen(BUSY);
    assert_eq!(phase(&r), (Phase::Busy, true));
    let at = r
        .iter()
        .position(|row| row.starts_with("• Working ("))
        .expect("the status row");
    let mut ended = r.clone();
    ended[at] = "  1:37 PM".to_string();
    assert_eq!(phase(&ended), (Phase::Idle, true));
}

/// While the final answer streams Codex draws NO status row and no end row
/// (measured): the user's message with no end under it is a turn in flight.
/// NEGATIVE CONTROL: the same turn a second later, its end row drawn.
#[test]
fn a_streaming_answer_is_busy_and_its_end_row_ends_it() {
    let r = screen(STREAMING);
    assert!(!r.iter().any(|row| row.contains("esc to interrupt")));
    assert_eq!(phase(&r), (Phase::Busy, true));
    assert_eq!(said_tail(&r), None, "no turn has ended");
    let done = codex(END_OF_TURN);
    assert_eq!((done.phase, done.phase_authoritative), (Phase::Idle, true));
    let said = done.said_tail.expect("the answer");
    assert!(said.starts_with("Tests matter because"), "{said}");
    assert!(said.ends_with("as it evolves continuously."), "{said}");
}

/// An end row is the clock under a blank row: a bare clock the streaming
/// answer wrote under its own words is not one, and the turn still runs.
/// NEGATIVE CONTROL: with a blank row above it, it ends the turn.
#[test]
fn a_clock_in_the_answer_is_not_an_end_row() {
    let r = screen(STREAMING);
    let last = r
        .iter()
        .rposition(|row| row.starts_with("  design, since code"))
        .expect("the streaming row");
    let mut quoted = r.clone();
    quoted.insert(last + 1, "  12:30".to_string());
    assert_eq!(phase(&quoted).0, Phase::Busy);
    let mut ended = r.clone();
    ended.insert(last + 1, String::new());
    ended.insert(last + 2, "  12:30".to_string());
    assert_eq!(phase(&ended).0, Phase::Idle);
}

/// An answer's own paragraph that is a bare clock (`  9:30` over `  -
/// standup`) is no end row: the answer goes on under it at column 2, and a
/// real end row is followed only by blank rows and column-0 rows (a
/// message, the composer, a `•` or `■` block, a slash command's echo).
/// NEGATIVE CONTROL: the same clock with nothing under it but the composer
/// ends the turn (the one frame no reader can tell from a real end).
#[test]
fn a_clock_paragraph_inside_the_answer_is_not_an_end_row() {
    let r = screen(STREAMING);
    let last = r
        .iter()
        .rposition(|row| row.starts_with("  design, since code"))
        .expect("the streaming row");
    let mut plan = r.clone();
    for (k, row) in ["", "  9:30", "  - standup"].into_iter().enumerate() {
        plan.insert(last + 1 + k, row.to_string());
    }
    assert_eq!(phase(&plan), (Phase::Busy, true));
    let mut ended = r.clone();
    ended.insert(last + 1, String::new());
    ended.insert(last + 2, "  9:30".to_string());
    assert_eq!(phase(&ended), (Phase::Idle, true));
}

/// A user message that opens on a numbered list (`› 1. Write …`) is a
/// message, not a box option: while its answer streams (no status row, no
/// end row) the turn runs. Read as an option, the message vanished and the
/// turn BEFORE it read as the last one — `idle`, authoritatively, mid-turn,
/// where full power types a continuation and park-at-idle restarts Codex.
/// A list the message goes on with `Press enter …` is no box either: a box
/// is the live zone, its footer the screen's last row.
/// NEGATIVE CONTROL: the real exec box still reads a prompt.
#[test]
fn a_numbered_user_message_is_a_message_not_a_box() {
    let r = screen(STREAMING);
    let user = r
        .iter()
        .rposition(|row| row.starts_with("› Write a 120-word"))
        .expect("the user's message");
    let mut numbered = r.clone();
    numbered[user] = "› 1. Write a 120-word paragraph about why tests matter.".to_string();
    numbered.insert(user + 1, "  2. Do not use tools.".to_string());
    assert_eq!(phase(&numbered), (Phase::Busy, true));
    let reading = codex(&numbered.join("\n"));
    assert_eq!(reading.prompt, None);
    assert_eq!(reading.said_tail, None, "no turn has ended");

    let mut hinted = numbered.clone();
    hinted.insert(
        user + 2,
        "  Press enter twice and esc once when you are done.".to_string(),
    );
    assert_eq!(
        prompt(&hinted),
        None,
        "a box's footer is the screen's last row"
    );
    assert_eq!(phase(&hinted), (Phase::Busy, true));

    let exec = codex(BOX_EXEC);
    assert_eq!(exec.phase, Phase::Prompt, "the control");
    assert_eq!(exec.prompt.map(|p| p.kind), Some(PromptKind::Bash));
}

/// A frame caught mid-redraw — the user's message drawn, the rows under it
/// still blank, no composer or footer yet — says nothing: read with the
/// message as the composer, the turn BEFORE it is the last one and the
/// session reads `idle` mid-answer (the likeliest cause of the live probe's
/// one 220 ms `idle` flicker).
/// NEGATIVE CONTROL: the same frame with its composer and footer drawn is
/// the turn in flight.
#[test]
fn a_frame_mid_redraw_is_no_evidence() {
    let r = screen(STREAMING);
    let user = r
        .iter()
        .rposition(|row| row.starts_with("› Write a 120-word"))
        .expect("the user's message");
    let mut torn: Vec<String> = r[..=user].to_vec();
    torn.extend(std::iter::repeat_n(String::new(), r.len() - user - 1));
    assert_eq!(phase(&torn), (Phase::Idle, false));
    let mut drawn = r[..=user].to_vec();
    drawn.extend(rows(&[
        "",
        "",
        "› Ask Codex to do anything",
        "",
        "  gpt · /x",
    ]));
    assert_eq!(phase(&drawn), (Phase::Busy, true));
}

/// A turn that ended on a question is `question`. NEGATIVE CONTROL: the
/// turn that ended on a statement is idle.
#[test]
fn a_turn_ending_on_a_question_is_a_question() {
    let r = codex(QUESTION_END);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Question, true));
    assert_eq!(
        r.said_tail.as_deref(),
        Some("Should I also add a multiply function?")
    );
    assert_eq!(codex(END_OF_TURN).phase, Phase::Idle, "the control");
}

/// The exec approval box: a Bash prompt for its `$` command with the
/// model's reason, the one-shot allow `Yes, proceed` (its `(y)` shortcut
/// stripped), the durable prefix rule and the refusal, chosen by digit,
/// Esc refusing. NEGATIVE CONTROLS: a second cursor, or a gap in the
/// numbering, and no option carries a role.
#[test]
fn the_exec_box_is_a_bash_prompt_with_its_roles() {
    let r = codex(BOX_EXEC);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    let p = r.prompt.expect("the box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.title, anchor_text("codex.box.exec"));
    assert_eq!(p.command, "touch made-by-codex.txt");
    assert_eq!(p.readings(), vec!["touch made-by-codex.txt".to_string()]);
    assert_eq!(
        p.description,
        "Allow creating the requested file in the workspace?"
    );
    assert_eq!(labels(&p)[0], "Yes, proceed");
    assert!(
        labels(&p)[1].ends_with("`touch made-by-codex.txt`"),
        "{:?}",
        labels(&p)
    );
    assert_eq!(roles(&p), vec![Role::Once, Role::Persist, Role::Deny]);
    assert_eq!(p.focused().map(|o| o.n), Some(Some(1)));
    assert_eq!(p.select, Select::Digits);
    let cancel = p.cancel.clone().expect("esc");
    assert_eq!(
        (cancel.verb.as_str(), cancel.effect),
        ("cancel", CancelEffect::Reject)
    );
    assert_eq!(r.wall, None, "no wall under a box");
    assert_eq!(r.composer, None, "the box replaces the composer");

    let rows = screen(BOX_EXEC);
    let two = rows
        .iter()
        .position(|row| row.starts_with("  2. "))
        .expect("option 2");
    let mut cursors = rows.clone();
    cursors[two] = cursors[two].replacen("  2.", "› 2.", 1);
    let p = prompt(&cursors).expect("still a box");
    assert!(
        roles(&p).iter().all(|&r| r == Role::Other),
        "{:?}",
        roles(&p)
    );
    let mut gap = rows.clone();
    gap[two] = gap[two].replacen("2.", "4.", 1);
    let p = prompt(&gap).expect("still a box");
    assert!(
        roles(&p).iter().all(|&r| r == Role::Other),
        "{:?}",
        roles(&p)
    );
}

/// The patch approval box: an Edit prompt for its destination (a path
/// that wraps is joined), `Yes, proceed` once, `these files` for the
/// session, the refusal.
#[test]
fn the_patch_box_is_an_edit_prompt_with_its_roles() {
    let p = codex(BOX_PATCH).prompt.expect("the box");
    assert_eq!(p.kind, PromptKind::Edit);
    assert_eq!(p.description, "Apply proposed file edits");
    assert!(
        p.path
            .as_deref()
            .is_some_and(|f| f.ends_with("/cx.xxxx/work/calc.py")),
        "{:?}",
        p.path
    );
    assert_eq!(p.command, p.path.clone().unwrap_or_default());
    assert!(p.command_rows.is_empty());
    assert_eq!(roles(&p), vec![Role::Once, Role::Session, Role::Deny]);
    // The diff above the box is transcript, not the box.
    assert!(
        p.span.0
            > screen(BOX_PATCH)
                .iter()
                .rposition(|r| r.contains("return a + b"))
                .unwrap_or(0)
    );
}

/// Plan mode's question: a Question prompt at column 2, its text, the
/// answers' labels without their descriptions, `(Recommended)` kept on the
/// label, the answers [`Role::Answer`] (answers, not permissions) and `None
/// of the above` [`Role::Other`], Esc interrupting.
/// Its footer's `esc to interrupt` is not the status row. NEGATIVE
/// CONTROL: Claude Code's reader sees no box on it.
#[test]
fn the_question_dialog_is_a_question_prompt() {
    // 0.157.1's, measured as the supervisor answered it live (lane G of
    // tools/test-codex-live-upgrade.sh), reads the same.
    for text in [QUESTION_0157, QUESTION] {
        let r = codex(text);
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
        let p = r.prompt.expect("the dialog");
        assert_eq!(
            (p.kind, p.title.as_str()),
            (PromptKind::Question, "Question 1/1 (1 unanswered)")
        );
        assert_eq!(
            labels(&p),
            vec!["subtract (Recommended)", "sub", "None of the above"]
        );
        assert_eq!(roles(&p), vec![Role::Answer, Role::Answer, Role::Other]);
        assert_eq!(
            p.recommended().map(|o| (o.n, o.label.as_str())),
            Some((Some(1), "subtract (Recommended)"))
        );
        assert_ne!(
            ClaudeReader.phase(&screen(text)),
            Phase::Prompt,
            "the control"
        );
    }
    let r = codex(QUESTION);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    let p = r.prompt.expect("the dialog");
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(p.title, "Question 1/1 (1 unanswered)");
    assert_eq!(
        p.description,
        "What should the new subtraction function be called?"
    );
    assert_eq!(
        labels(&p),
        vec!["subtract (Recommended)", "sub", "None of the above"]
    );
    assert!(labels(&p)[0].contains(anchor_text("codex.question.recommended")));
    // The model's answers are answers; `None of the above` asks for the
    // person's own words.
    assert_eq!(roles(&p), vec![Role::Answer, Role::Answer, Role::Other]);
    assert_eq!(p.focused().map(|o| o.n), Some(Some(1)));
    assert_eq!(
        p.recommended().map(|o| (o.n, o.label.as_str())),
        Some((Some(1), "subtract (Recommended)"))
    );
    // Esc here stops the whole turn (the footer says so): no refusal of
    // the box.
    let cancel = p.cancel.expect("esc");
    assert_eq!(
        (cancel.verb.as_str(), cancel.effect),
        ("interrupt", CancelEffect::Interrupt)
    );
    assert_eq!(
        codex(BOX_EXEC)
            .prompt
            .and_then(|p| p.recommended().cloned()),
        None,
        "the control: an approval box recommends nothing"
    );
    assert_ne!(
        ClaudeReader.phase(&screen(QUESTION)),
        Phase::Prompt,
        "the control"
    );
}

/// Plan mode's end: `Implement this plan?` is a Plan prompt, the plain
/// first yes its one-shot, `No, stay in Plan mode` the refusal, the fresh
/// thread neither; Esc goes back. The plan's `Worked for … · 1:43 PM` row
/// above it is transcript.
#[test]
fn the_plan_box_is_a_plan_prompt() {
    // 0.157.1's (its descriptions without the full stop, the fresh thread's
    // without the context figure), measured as the supervisor pressed it
    // live, reads the same. NEGATIVE CONTROL: its answered question above
    // is transcript, never a second box.
    for text in [PLAN, PLAN_0157] {
        let p = codex(text).prompt.expect("the box");
        assert_eq!(p.kind, PromptKind::PlanExit);
        assert_eq!(p.title, anchor_text("codex.plan.title"));
        assert_eq!(
            labels(&p),
            vec![
                "Yes, implement this plan",
                "Yes, clear context and implement",
                "No, stay in Plan mode"
            ]
        );
        assert_eq!(roles(&p), vec![Role::Once, Role::Other, Role::Deny]);
        assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Back));
    }
}

/// Esc stopped the turn: idle, evidence, `interrupted`, no wall.
/// NEGATIVE CONTROL: a turn that ended by itself is not interrupted.
#[test]
fn an_interrupted_turn_is_the_persons() {
    let r = codex(INTERRUPTED);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert!(r.interrupted);
    assert_eq!(r.wall, None);
    assert_eq!(r.said_tail, None, "the turn ended on the vendor's row");
    assert!(!codex(END_OF_TURN).interrupted, "the control");
}

/// In a pane narrower than the interrupt's sentence (about 68 columns) its
/// head row wraps mid-sentence, and it is still the person's Esc — read as
/// an ordinary ended turn, the continuation policy would type over it once
/// the person's grace ran out. NEGATIVE CONTROL: a wrapped error that is
/// not the interrupt is not one.
#[test]
fn a_wrapped_interrupt_is_still_the_persons() {
    let r = screen(INTERRUPTED);
    let head = r
        .iter()
        .position(|row| row.starts_with("■ Conversation interrupted"))
        .expect("the interrupt row");
    let mut narrow = r.clone();
    narrow[head] = "■ Conversation interrupted - tell the model what to do".to_string();
    narrow[head + 1] =
        "differently. Something went wrong? Hit `/feedback` to report the issue.".to_string();
    assert_eq!(phase(&narrow), (Phase::Idle, true));
    assert!(interrupted(&narrow));
    assert_eq!(wall(&narrow), None);
    let mut other = narrow.clone();
    other[head] = "■ Conversation failed - tell the model what to do".to_string();
    assert!(!interrupted(&other), "the control");
}

/// `/status` after an ended turn: its card sits between the end row and
/// the composer, and the turn stays ended (the card is no message). The
/// last words are the turn's.
#[test]
fn a_slash_command_after_a_turn_is_idle() {
    let r = codex(STATUS);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert!(
        r.said_tail
            .as_deref()
            .is_some_and(|t| t.ends_with("as it evolves continuously.")),
        "{:?}",
        r.said_tail
    );
    assert_eq!(
        r.context_left, None,
        "the card's `99% left` is no footer item"
    );
}

/// A draft the paste guard left unsubmitted: the composer's lines are the
/// text and the empty row its Enter became; the turn before it has ended.
/// NEGATIVE CONTROL: an idle composer is its one row.
#[test]
fn an_unsubmitted_draft_is_the_composers() {
    let r = codex(DRAFT);
    assert_eq!(r.phase, Phase::Idle);
    let (caret, lines) = r.composer.expect("the composer");
    assert_eq!(caret, 30);
    assert_eq!(
        lines,
        vec![
            "Fix the bug in calc.py so add returns a + b. Edit the file with a patch; do not run anything."
                .to_string(),
            String::new()
        ]
    );
    assert_eq!(codex(IDLE).composer.map(|(_, l)| l.len()), Some(1));
}

/// After `/quit` Codex prints its resume command and the shell comes back:
/// no composer, so nothing Codex's reader says is evidence.
/// NEGATIVE CONTROL: a live session's turn end is.
#[test]
fn the_exit_screen_is_no_evidence() {
    assert_eq!(phase(&screen(EXIT)), (Phase::Idle, false));
    assert!(phase(&screen(END_OF_TURN)).1, "the control");
}

/// HAND-BUILT from the binary: a turn that ended on the usage limit is
/// `limited`, its wall the usage window with the reset its row names, the
/// year dropped, the row wrapped to column 0 joined back. NEGATIVE
/// CONTROL: a turn that ended by itself has no wall.
#[test]
fn the_usage_limit_row_is_a_wall() {
    let r = codex(HIT_LIMIT);
    let w = r.wall.clone().expect("the wall");
    assert_eq!(w.kind, WallKind::UsageSession);
    assert_eq!(w.reset.as_deref(), Some("Sep 25th 3:05 PM"));
    assert!(
        w.message.ends_with("try again at Sep 25th, 2026 3:05 PM."),
        "{}",
        w.message
    );
    assert_eq!(
        r.phase,
        Phase::Limited {
            message: w.message.clone(),
            reset: w.reset.clone()
        }
    );
    assert!(r.phase_authoritative);
    assert_eq!(codex(END_OF_TURN).wall, None, "the control");
}

/// MEASURED on 0.157.1 (2026-09-27, a private instance whose loopback model
/// answered `usage_limit_reached`): the wall the hand-built fixture above
/// stood in for, reached live — the usage window, the row naming NO reset
/// (`try again later`), so a supervisor waits out its longest back-off.
/// NEGATIVE CONTROL: the hand-built row's reset is read where it names one.
#[test]
fn the_measured_usage_limit_row_is_a_wall_with_no_reset() {
    let r = codex(HIT_LIMIT_NO_RESET);
    let w = r.wall.clone().expect("the wall");
    assert_eq!(w.kind, WallKind::UsageSession);
    assert_eq!(w.reset, None, "{}", w.message);
    assert!(w.message.contains("try again later"), "{}", w.message);
    assert!(r.phase_authoritative);
    assert!(
        codex(HIT_LIMIT).wall.and_then(|w| w.reset).is_some(),
        "the control"
    );
}

/// The rest of [`WALLS`], each as the turn's last `■` row; an interrupt
/// and an error no row names are no wall, and a `■` row a newer message
/// answered is no wall either.
#[test]
fn every_wall_row_is_read_by_kind() {
    let ended = |row: &str| -> Vec<String> {
        rows(&[
            "› go on",
            "",
            row,
            "",
            "› Ask Codex to do anything",
            "",
            "  gpt · /x",
        ])
    };
    for (row, kind) in [
        (
            "■ You've hit your usage limit for GPT-6-Astra. Switch to another model now, or try again at 4:10 PM.",
            WallKind::ModelBucket { consent: false },
        ),
        (
            "■ You hit your spend cap set in your workspace. Increase your spend cap to continue.",
            WallKind::Spend,
        ),
        (
            "■ Codex ran out of room in the model's context window. Start a new thread or clear earlier history before retrying.",
            WallKind::Context,
        ),
        (
            "■ Your access token could not be refreshed. Please log out and sign in again.",
            WallKind::Auth,
        ),
        (
            "■ Selected model is at capacity. Please try a different model.",
            WallKind::Overloaded,
        ),
        (
            "■ exceeded retry limit, last status: 429 Too Many Requests",
            WallKind::ApiError {
                code: Some(429),
                retryable: true,
                cause: ApiCause::Server,
            },
        ),
        (
            "■ exceeded retry limit, last status: 400 Bad Request",
            WallKind::ApiError {
                code: Some(400),
                retryable: false,
                cause: ApiCause::Server,
            },
        ),
        (
            "■ stream disconnected before completion: connection reset",
            WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::Server,
            },
        ),
    ] {
        let w = wall(&ended(row)).unwrap_or_else(|| panic!("{row}"));
        assert_eq!(w.kind, kind, "{row}");
        assert!(phase(&ended(row)).1, "{row}");
    }
    assert_eq!(
        wall(&ended("■ You've hit your usage limit for GPT-6-Astra. Switch to another model now, or try again at 4:10 PM."))
            .and_then(|w| w.reset),
        Some("4:10 PM".to_string())
    );
    for row in [
        "■ Conversation interrupted - tell the model what to do differently. Something went wrong?",
        "■ sandbox error: something the table does not name",
    ] {
        assert_eq!(wall(&ended(row)), None, "{row}");
        assert_eq!(phase(&ended(row)), (Phase::Idle, true), "{row}");
    }
    // A wall a newer message answered: the turn after it runs.
    let mut answered = ended("■ You've hit your usage limit. Try again at 3:05 PM.");
    answered.insert(4, "› try again".to_string());
    answered.insert(5, String::new());
    assert_eq!(wall(&answered), None);
    assert_eq!(phase(&answered).0, Phase::Busy);
}

/// Codex's `<n>% context left` footer item (from the binary; the probe's
/// footer showed none). NEGATIVE CONTROL: the measured footer.
#[test]
fn the_context_left_footer_item_is_read() {
    let r = rows(&[
        "  1:39 PM",
        "",
        "› Ask Codex to do anything",
        "",
        "  gpt-5 · 37% context left",
    ]);
    assert_eq!(context_left(&r), Some(37));
    assert_eq!(context_left(&screen(IDLE)), None, "the control");
}

/// Claude Code's reader on Codex's screens: never a box, never a question
/// — the measured screens are why a session's PROGRAM picks its reader.
#[test]
fn claudes_reader_on_codex_screens_sees_no_box_and_no_question() {
    for text in ALL {
        let r = screen(text);
        let phase = ClaudeReader.phase(&r);
        assert!(
            !matches!(phase, Phase::Prompt | Phase::Question),
            "{}: {phase:?}",
            provenance(text).unwrap_or_default()
        );
        assert_eq!(ClaudeReader.prompt(&r), None);
    }
}

/// A shell, a pager or a viewer showing Codex's screen is not Codex: the
/// program's NAME wins. NEGATIVE CONTROL: the same screen under `codex`,
/// and with no name at all its box identifies it.
#[test]
fn a_shell_quoting_codex_is_not_codex() {
    let r = screen(BOX_EXEC);
    for shell in ["zsh", "-bash", "less", "cat"] {
        let reading = read(Some(shell), &r, None);
        assert_eq!(reading.program, Program::Generic, "{shell}");
        assert_eq!(reading.prompt, None, "{shell}");
    }
    assert_eq!(identify(Some("codex"), &r).program(), Program::Codex);
    assert_eq!(identify(None, &r).program(), Program::Codex);
    // With no name, or a runtime's (an npm install runs as `node`), the
    // idle and the busy screen identify it by its composer.
    for text in [IDLE, BUSY, STREAMING, END_OF_TURN] {
        for name in [None, Some("node")] {
            assert_eq!(
                identify(name, &screen(text)).program(),
                Program::Codex,
                "{name:?}: {}",
                provenance(text).unwrap_or_default()
            );
        }
        assert_eq!(
            identify(Some("zsh"), &screen(text)).program(),
            Program::Generic,
            "the name wins"
        );
    }
    // A screen with Codex's words and no box names nothing.
    let quoted = rows(&[
        "% grep -h Working log.txt",
        "• Working (6s • esc to interrupt)",
        "› Ask Codex to do anything",
        "%",
    ]);
    assert_eq!(identify(None, &quoted).program(), Program::Generic);
}

/// The supervisor hosts Claude Code and Codex, by name or by the reader
/// that identified them; never a shell, a runtime by name, or nothing.
#[test]
fn claude_and_codex_are_supervisable() {
    use crate::reader::program_of;
    let supervisable = |program: Option<&str>| {
        program
            .and_then(program_of)
            .is_some_and(Program::supervisable)
    };
    for yes in [
        "claude",
        "/opt/bin/codex --full-auto",
        "codex",
        "claude-code",
    ] {
        assert!(supervisable(Some(yes)), "{yes}");
    }
    for no in [Some("zsh"), Some("node"), Some("less"), None] {
        assert!(!supervisable(no), "{no:?}");
    }
    assert!(Program::Codex.supervisable() && Program::Claude.supervisable());
    assert!(!Program::Generic.supervisable());
    assert!(
        CodexReader
            .anchors()
            .iter()
            .all(|a| a.id.starts_with("codex."))
    );
}

/// CODEX 0.157.0 (the E2E probe of 2026-09-25): a turn that ended with the
/// right-aligned `Tip:` row between its end row and the composer is ENDED —
/// idle, as evidence, its last words read — and the composer is its caret
/// row alone: the two-row footer's status row (`GPT-5.6-Sol low · <cwd> ·
/// <thread title>`) is no draft. Before the fix the tip read as a row under
/// the clock, so the turn ran for ever (`agent=busy`, no idle point, nothing
/// continued); and the status row read as typed text, a person "typing" at
/// every idle point. NEGATIVE CONTROLS: the same `Tip:` words as an
/// answer's own paragraph at column 2 still keep the clock above them from
/// being an end; a draft typed into the composer is still read, over the
/// two-row footer.
#[test]
fn a_0_157_tip_and_two_row_footer_end_the_turn_and_type_nothing() {
    let r = codex(END_OF_TURN_TIP);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(
        r.said_tail.as_deref(),
        Some("Created y in the current folder.")
    );
    let rows = screen(END_OF_TURN_TIP);
    let caret = rows
        .iter()
        .position(|row| row.starts_with("› Ask Codex"))
        .expect("the composer");
    assert_eq!(
        r.composer,
        Some((
            caret,
            vec![anchor_text("codex.composer.placeholder").to_string()]
        ))
    );
    assert!(is_codex_screen(&rows));

    let tip = rows
        .iter()
        .position(|row| row.trim_start().starts_with("Tip: "))
        .expect("the tip");
    let mut paragraph = rows.clone();
    paragraph[tip] = format!("  {}", rows[tip].trim_start());
    assert_eq!(phase(&paragraph), (Phase::Busy, true));

    let mut drafted = rows.clone();
    drafted[caret] = "› run the tests".to_string();
    drafted[caret + 1] = "  and then the linter".to_string();
    drafted.insert(caret + 2, String::new());
    assert_eq!(
        composer_draft(&drafted).map(|(_, l)| l),
        Some(vec![
            "run the tests".to_string(),
            "and then the linter".to_string()
        ])
    );
}

/// CODEX 0.157.0 (the E2E probe of 2026-09-25): the folder-trust gate drawn
/// at the top of a 45-row pane, read whole, is the trust gate; read from its
/// last 40 rows (the loop's `tail=` read) its title is cut, and the box is
/// read with its head OFF the rows — never titled by its body row `model
/// request. …` as a dialog of no kind, which escalated the gate. A cut
/// exec box keeps its one-shot allow by its label. NEGATIVE CONTROL: the
/// 0.156.1 gate, whole, has its head on the screen.
#[test]
fn a_box_cut_at_the_top_of_the_rows_read_has_its_head_off_screen() {
    let whole = screen(TRUST_TALL_PANE);
    assert_eq!(whole.len(), 45);
    let p = prompt(&whole).expect("the gate");
    assert_eq!((p.kind, p.head_off_screen), (PromptKind::Trust, false));
    assert_eq!(roles(&p)[0], Role::Trust);

    let tail = whole[5..].to_vec();
    let cut = prompt(&tail).expect("the gate, cut");
    assert!(cut.head_off_screen, "{cut:?}");
    assert_eq!(cut.kind, PromptKind::Other);
    assert!(cut.title.starts_with("model request."), "{}", cut.title);

    let exec = screen(BOX_EXEC);
    let first = exec
        .iter()
        .position(|row| row.trim_start().starts_with("$ "))
        .expect("the command row");
    let cut = prompt(&exec[first..]).expect("the exec box, cut");
    assert!(cut.head_off_screen, "{cut:?}");
    assert_eq!(
        cut.options
            .iter()
            .find(|o| o.role == Role::Once)
            .map(|o| o.label.as_str()),
        Some(anchor_text("codex.box.once"))
    );

    let gate = prompt(&screen(TRUST)).expect("the 0.156.1 gate");
    assert!(!gate.head_off_screen);
}

/// A session nobody has asked anything is FRESH (no turn has ended): the
/// session card and no message. NEGATIVE CONTROL: after a turn, not fresh.
#[test]
fn a_session_with_no_turn_is_fresh() {
    assert!(codex(IDLE).fresh);
    assert!(!codex(END_OF_TURN).fresh);
    assert!(!codex(END_OF_TURN_TIP).fresh);
    assert!(!codex(BUSY).fresh);
}

/// A BACKGROUND TERMINAL a finished turn left keeps the Codex screen busy —
/// its status line sits under the turn's end row, where only blank rows and
/// a tip may, so [`phase`] reads the turn running — and the break is named
/// ([`background_wait`]): the turn is over and the agent's own work runs on.
/// NEGATIVE CONTROLS: the same line under a turn still running, a person's
/// message saying the words, and a screen with no such line.
#[test]
fn a_background_terminal_after_an_ended_turn_is_the_agents_own_work() {
    let line = "  1 background terminal running · /ps to view · /stop to close";
    let ended = |status: &str| {
        rows(&[
            "› start bg 7771 please",
            "",
            "• The server runs in the background.",
            "",
            "  1:41 AM",
            "",
            status,
            "",
            "› Ask Codex to do anything",
            "",
            "  fake-model default · /w",
        ])
    };
    let bg = ended(line);
    assert_eq!(
        phase(&bg),
        (Phase::Busy, true),
        "the line holds the turn open"
    );
    assert_eq!(background_wait(&bg), Some("a background terminal running"));
    assert_eq!(
        CodexReader.background_wait(&bg),
        Some("a background terminal running")
    );
    assert!(is_background_terminal_row(line));
    assert!(is_background_terminal_row(
        "  2 background terminals running · /ps to view"
    ));
    // A turn still running with the same line: a live turn, no break.
    let running = rows(&[
        "› start bg 7771 please",
        "",
        "• Working (6s • esc to interrupt)",
        line,
        "",
        "› Ask Codex to do anything",
        "",
        "  fake-model default · /w",
    ]);
    assert_eq!(phase(&running).0, Phase::Busy);
    assert_eq!(background_wait(&running), None);
    // A person's message saying the words is no status line.
    assert!(!is_background_terminal_row(
        "› 1 background terminal running is fine"
    ));
    // Nothing running: idle, and no break of the background kind.
    let idle = ended("");
    assert_eq!(phase(&idle), (Phase::Idle, true));
    assert_eq!(background_wait(&idle), None);
}

/// A TURN ENDED UNDER A BACKGROUND TERMINAL, HOWEVER IT ENDED
/// ([`ended_under_background`], the round-3 re-review of 2026-09-28): with
/// the line under it the whole screen reads busy, and the rest reads the
/// ended turn — idle, a QUESTION (the save's "Should I merge them and push
/// again?") — each named; a usage WALL's or an API error's `■` block under
/// the line is the turn's end already (not busy), and named the same.
/// NEGATIVE CONTROLS: the same screens without the line
/// name nothing (they are points already), a turn still running beside the
/// line names nothing, and only the idle end is the host's
/// [`background_wait`].
#[test]
fn a_turn_ended_under_a_background_terminal_is_named_however_it_ended() {
    let line = "  1 background terminal running · /ps to view · /stop to close";
    let ended = |said: &[&str], status: &str| {
        let mut r = vec!["› start bg 7771 please", ""];
        r.extend_from_slice(said);
        r.extend_from_slice(&[
            "",
            "  1:41 AM",
            "",
            status,
            "",
            "› Ask Codex to do anything",
            "",
        ]);
        r.push("  fake-model default · /w");
        rows(&r)
    };
    let idle = ["• The server runs in the background."];
    let asks = [
        "• The push was rejected: the remote has two new commits.",
        "",
        "  Should I merge them and push again?",
    ];
    for (said, want) in [(&idle[..], Phase::Idle), (&asks[..], Phase::Question)] {
        let bg = ended(said, line);
        assert_eq!(phase(&bg), (Phase::Busy, true), "{said:?}");
        assert_eq!(ended_under_background(&bg), Some(want.clone()), "{said:?}");
        assert_eq!(
            background_wait(&bg).is_some(),
            want == Phase::Idle,
            "{said:?}"
        );
        assert_eq!(ended_under_background(&ended(said, "")), None, "{said:?}");
    }
    // A wall's `■` block under the line is the turn's end whatever runs
    // under it — a point already, not busy (measured on these screens) —
    // and named the same.
    let walls = [
        rows(&[
            "› save it",
            "",
            "■ You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to \
             purchase more credits or try again at Sep 25th, 2026 3:05 PM.",
            "",
            line,
            "",
            "› Ask Codex to do anything",
            "",
            "  fake-model default · /w",
        ]),
        rows(&[
            "› save it",
            "",
            "■ stream disconnected before completion: exceeded retry limit, last status: 500",
            "",
            line,
            "",
            "› Ask Codex to do anything",
            "",
            "  fake-model default · /w",
        ]),
    ];
    for w in &walls {
        let rest: Vec<String> = w
            .iter()
            .filter(|r| !is_background_terminal_row(r))
            .cloned()
            .collect();
        let (p, auth) = phase(&rest);
        assert!(auth, "{rest:#?}");
        assert_eq!(phase(w), (p.clone(), true), "{w:#?}");
        assert_ne!(p, Phase::Busy, "{w:#?}");
        assert_eq!(ended_under_background(w), Some(p.clone()), "{w:#?}");
        assert!(wall(&rest).is_some(), "{rest:#?}");
    }
    // A turn still running beside the line: no end.
    let running = rows(&[
        "› start bg 7771 please",
        "",
        "• Working (6s • esc to interrupt)",
        line,
        "",
        "› Ask Codex to do anything",
        "",
        "  fake-model default · /w",
    ]);
    assert_eq!(ended_under_background(&running), None);
}

// --- the rate-limit nudge, the `/model` picker, the footer (2026-09-28) -----

/// The rate-limit model nudge (HAND-BUILT from the 0.157.1 strings on the
/// measured 0.158.0 list geometry) is its own kind: its plain keep the
/// refusal that settles nothing, its never-show-again keep a standing
/// setting (it writes Codex's config), its switch no role at all. NEGATIVE
/// CONTROLS: retitled it is a box of no kind whose options carry no roles;
/// its head cut off the rows read, it is named by nothing either.
#[test]
fn the_rate_limit_nudge_is_its_own_kind_and_keeps_by_role() {
    let r = codex(RATE_NUDGE);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Prompt, true)
    );
    let p = r.prompt.expect("the nudge");
    assert_eq!(p.kind, PromptKind::RateNudge);
    assert_eq!(p.kind.name(), "rate-nudge");
    assert_eq!(p.title, anchor_text("codex.nudge.title"));
    assert_eq!(
        labels(&p),
        [
            "Switch to gpt-6-luna",
            "Keep current model",
            "Keep current model (never show again)"
        ]
    );
    assert_eq!(roles(&p), [Role::Other, Role::Deny, Role::Persist]);
    assert_eq!(p.select, Select::Digits);
    assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Back));
    assert!(!p.head_off_screen);

    let retitled: Vec<String> = screen(RATE_NUDGE)
        .into_iter()
        .map(|r| r.replace("Approaching rate limits", "Approaching usage limits"))
        .collect();
    let q = prompt(&retitled).expect("still a box");
    assert_eq!(q.kind, PromptKind::Other);
    assert!(roles(&q).iter().all(|r| *r == Role::Other), "{q:?}");

    let all = screen(RATE_NUDGE);
    let first = all
        .iter()
        .position(|r| r.starts_with("› 1."))
        .expect("the options");
    // Cut under the title: the subtitle is the first row read.
    let cut = all[first - 3..].to_vec();
    assert!(cut[0].contains("for lower credit usage?"), "{cut:#?}");
    let q = prompt(&cut).expect("the options still read");
    assert!(q.head_off_screen);
    assert_eq!(q.kind, PromptKind::Other);
    assert!(!roles(&q).contains(&Role::Deny), "{q:?}");
    // The plan box stays a plan box.
    assert_eq!(
        prompt(&screen(PLAN)).map(|p| p.kind),
        Some(PromptKind::PlanExit)
    );
}

/// `/model`'s boxes (MEASURED on 0.158.0): the model box, the effort box —
/// of the current model and of another, its cursor on `More reasoning…` —
/// and the advanced box are all the model picker, every option of no role
/// (a choice the harness makes only by label, only for its own restore), each
/// chosen by digit on its list, each footer's Esc a going back. NEGATIVE
/// CONTROL: the exec box stays an exec box.
#[test]
fn the_model_picker_boxes_are_the_model_picker() {
    let cases: [(&str, &str, &[&str], &str); 6] = [
        (
            MODEL_PICK,
            "Select Model and Effort",
            &[
                "GPT-6-Astra (current)",
                "GPT-6-Sol",
                "GPT-6-Luna",
                "GPT-5.6-Sol",
                "GPT-5.6-Terra",
                "GPT-5.6-Luna",
                "GPT-5.5",
            ],
            "GPT-6-Astra (current)",
        ),
        (
            EFFORT_PICK,
            "Select Reasoning Level for GPT-6-Astra",
            &[
                "Low",
                "Medium (default) (current)",
                "High",
                "Extra high",
                "More reasoning…",
            ],
            "Medium (default) (current)",
        ),
        (
            EFFORT_PICK_MORE,
            "Select Reasoning Level for GPT-6-Astra",
            &[
                "Low",
                "Medium (default) (current)",
                "High",
                "Extra high",
                "More reasoning…",
            ],
            "More reasoning…",
        ),
        (
            EFFORT_PICK_OTHER,
            "Select Reasoning Level for GPT-6-Luna",
            &[
                "Low",
                "Medium (default)",
                "High",
                "Extra high",
                "More reasoning…",
            ],
            "Medium (default)",
        ),
        (
            ADVANCED_PICK,
            "Advanced Reasoning",
            &["Max", "Ultra"],
            "Max",
        ),
        (
            ADVANCED_PICK_ULTRA,
            "Advanced Reasoning",
            &["Max", "Ultra"],
            "Ultra",
        ),
    ];
    for (text, title, want, focused) in cases {
        let r = codex(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Prompt, true),
            "{title}"
        );
        let p = r.prompt.expect("the picker");
        assert_eq!(p.kind, PromptKind::ModelPick, "{title}");
        assert_eq!(p.kind.name(), "model-pick");
        assert_eq!(p.title, title);
        assert_eq!(labels(&p), want, "{title}");
        assert!(roles(&p).iter().all(|r| *r == Role::Other), "{title}");
        assert_eq!(
            p.focused().map(|o| o.label.as_str()),
            Some(focused),
            "{title}"
        );
        assert_eq!(p.select, Select::Digits);
        assert_eq!(
            p.cancel.map(|c| c.effect),
            Some(CancelEffect::Back),
            "{title}"
        );
        // The footer is gone under the box: no model is read off it.
        assert_eq!(footer_model(&screen(text)), None, "{title}");
    }
    // The effort boxes offer the session-only `s` beside Enter's default.
    for text in [
        EFFORT_PICK,
        EFFORT_PICK_OTHER,
        ADVANCED_PICK,
        ADVANCED_PICK_ULTRA,
    ] {
        assert!(
            screen(text)
                .last()
                .is_some_and(|f| f.contains(anchor_text("codex.pick.session"))),
            "{}",
            provenance(text).unwrap_or_default()
        );
    }
    assert!(
        !screen(MODEL_PICK)
            .last()
            .is_some_and(|f| f.contains(anchor_text("codex.pick.session")))
    );
    assert_eq!(
        prompt(&screen(BOX_EXEC)).map(|p| p.kind),
        Some(PromptKind::Bash)
    );
}

/// The footer's first field is the THREAD's model and effort (0.158.0 at
/// idle, measured; a live goal tab's footer; 0.157.0's and 0.156.1's): the
/// display name the picker lists, the effort lowercased. NEGATIVE CONTROLS:
/// under a box nothing is read, and a footer field with no effort word is
/// the model alone.
#[test]
fn the_footer_names_the_threads_model_and_effort() {
    let m = |text: &str| footer_model(&screen(text));
    let want = |model: &str, effort: &str| Some((model.to_string(), Some(effort.to_string())));
    assert_eq!(m(IDLE_158), want("GPT-6-Astra", "medium"));
    assert_eq!(m(GOAL_BUSY), want("GPT-6-Astra", "ultra"));
    assert_eq!(
        footer_status(&screen(IDLE_158)),
        Some("GPT-6-Astra medium · ~")
    );
    let old = m(END_OF_TURN_TIP).expect("0.157.0's footer");
    assert_eq!(old.1.as_deref(), Some("low"), "{old:?}");
    assert_eq!(m(RATE_NUDGE), None, "a box");
    let bare = rows(&[
        "  1:39 PM",
        "",
        "› Ask Codex to do anything",
        "",
        "  gpt-6-astra · ~/pj",
    ]);
    assert_eq!(footer_model(&bare), Some(("gpt-6-astra".to_string(), None)));
    let extra = rows(&[
        "› Ask Codex to do anything",
        "",
        "  GPT-6-Sol extra high · ~/pj",
    ]);
    assert_eq!(
        footer_model(&extra),
        Some(("GPT-6-Sol".to_string(), Some("extra high".to_string())))
    );
}

/// The footer's right side names Codex's goal: pursued (measured on a live
/// tab), paused, stopped at a usage limit, stalled, achieved, unmet — each by
/// its own words.
/// NEGATIVE CONTROLS: a footer with no goal, and the words in a transcript
/// row rather than the footer, name none.
#[test]
fn the_footer_names_codexs_goal() {
    let with = |right: &str| -> Vec<String> {
        screen(GOAL_BUSY)
            .into_iter()
            .map(|r| r.replace("Pursuing goal (10d 3h 2m)", right))
            .collect()
    };
    assert_eq!(goal_state(&screen(GOAL_BUSY)), Some(CodexGoal::Pursuing));
    assert!(CodexReader.goal_active(&screen(GOAL_BUSY)));
    assert_eq!(
        goal_state(&with("Goal paused (/goal resume)")),
        Some(CodexGoal::Paused)
    );
    assert_eq!(
        goal_state(&with("Goal hit usage limits (/goal resume)")),
        Some(CodexGoal::UsageLimited)
    );
    assert_eq!(
        goal_state(&with("Goal stalled (/goal resume)")),
        Some(CodexGoal::Stalled)
    );
    // A goal that ENDED, as the vendor's footer draws it (rust-v0.158.0
    // `goal_status_indicator_line`): complete, or out of its token budget.
    for (right, ended) in [
        ("Goal achieved (10h 12m)", CodexGoal::Achieved),
        ("Goal achieved", CodexGoal::Achieved),
        ("Goal unmet (63.9K / 50K tokens)", CodexGoal::Unmet),
        ("Goal abandoned", CodexGoal::Unmet),
    ] {
        assert_eq!(goal_state(&with(right)), Some(ended), "{right}");
    }
    assert!(!CodexReader.goal_active(&with("Goal paused (/goal resume)")));
    assert_eq!(goal_state(&screen(IDLE_158)), None);
    let quoted = rows(&[
        "• The footer said Pursuing goal (3m) before.",
        "",
        "  1:39 PM",
        "",
        "› Ask Codex to do anything",
        "",
        "  GPT-6-Astra ultra · ~/pj",
    ]);
    assert_eq!(goal_state(&quoted), None);
}

/// IN A NARROW PANE the goal still reads (the goal-pause review of
/// 2026-09-28; each footer CAPTURED LIVE from Codex 0.158.0 in a scratch
/// headless aterm, a scratch `$CODEX_HOME` and a dummy key): Codex cuts the
/// status row's left side to keep the goal's state whole, and its ` · `
/// goes — at 45 columns the paused goal's row, at 40 both — while the
/// key-hint row under it keeps its own. Read by the ` · ` alone, the
/// key-hint row was taken for the status row: the goal read as none, and
/// the upgrade released a hold that still owed its resume. NEGATIVE
/// CONTROLS: the same pane with no goal on its cut row names none and reads
/// no model off the key-hint row; a key-hint row alone is no status row.
#[test]
fn a_narrow_panes_footer_still_names_the_goal() {
    let pane = |status: &str, hints: &str| {
        rows(&[
            "    dummy**robe. You can find your API key a\u{2026}",
            "",
            "\u{203a} Ask Codex to do anything",
            "",
            status,
            hints,
            "",
        ])
    };
    let hints45 = "  \u{2190} for agents \u{b7} ? for shortcuts    \u{26a0} 1 \u{b7} f2";
    let hints40 = "  \u{2190} for agents \u{b7} ? for shortc  \u{26a0} 1 \u{b7} f2";
    let pursuing45 = pane(
        "  gpt-5 default \u{b7} /pri\u{2026} Pursuing goal (21s)",
        hints45,
    );
    let paused45 = pane(
        "  gpt-5 default\u{2026} Goal paused (/goal resume)",
        hints45,
    );
    let paused40 = pane("  gpt-5 de\u{2026} Goal paused (/goal resume)", hints40);
    let wide = pane(
        "  gpt-5 default \u{b7} /private/tmp/claude-502/scratchpad/\u{2026} Goal paused (/goal \
         resume)",
        "  \u{2190} for agents \u{b7} ? for shortcuts        \u{26a0} 1 warning \u{b7} f2 to view",
    );
    assert_eq!(goal_state(&pursuing45), Some(CodexGoal::Pursuing));
    assert_eq!(goal_state(&paused45), Some(CodexGoal::Paused));
    assert_eq!(goal_state(&paused40), Some(CodexGoal::Paused));
    assert_eq!(goal_state(&wide), Some(CodexGoal::Paused));
    assert_eq!(
        footer_status(&paused40),
        Some("gpt-5 de\u{2026} Goal paused (/goal resume)")
    );
    let bare = pane("  gpt-5 de\u{2026}", hints40);
    assert_eq!(footer_status(&bare), None, "never the key-hint row");
    assert_eq!(goal_state(&bare), None);
    assert_eq!(footer_model(&bare), None);
    let hints_only = rows(&["\u{203a} Ask Codex to do anything", "", hints45]);
    assert_eq!(footer_status(&hints_only), None);
}

/// 0.158.0's status row while a background terminal runs carries the
/// status line's items after its Esc hint on the same row (measured on a
/// live tab): still a turn running — under the `»` input line a goal turn
/// draws, which is the composer ([`CARETS`], the one reader), as under `›`.
/// NEGATIVE CONTROL: an agent's own row that mentions the hint mid-sentence
/// is no status row.
#[test]
fn a_status_row_with_the_background_terminal_tail_is_busy() {
    let busy = screen(GOAL_BUSY);
    assert_eq!(
        phase(&busy),
        (Phase::Busy, true),
        "the `»` input line is the composer"
    );
    let at_caret: Vec<String> = busy
        .iter()
        .map(|r| r.replacen("» Ask Codex", "› Ask Codex", 1))
        .collect();
    assert_eq!(phase(&at_caret), (Phase::Busy, true));
    let mut prose = busy.clone();
    let at = prose
        .iter()
        .position(|r| r.starts_with("• Working ("))
        .expect("the status row");
    prose[at] = "• I pressed (esc to interrupt) and then went on with the fix".to_string();
    prose[at + 1] = String::new();
    prose.insert(at + 1, String::new());
    prose.insert(at + 2, "  1:39 PM".to_string());
    assert_ne!(phase(&prose), (Phase::Busy, true), "{prose:#?}");
}

/// A usage wall with Codex's `Tip:` row right under it (0.157.1, the owner's
/// 2026-09-27 wall): the reset is the wall's own words, never the tip joined
/// on. NEGATIVE CONTROL: the same wall with no tip reads the same reset.
#[test]
fn a_tip_under_a_usage_wall_is_not_its_reset() {
    let plain = screen(HIT_LIMIT);
    let want = wall(&plain).expect("the wall").reset;
    assert!(want.is_some());
    let head = plain
        .iter()
        .rposition(|r| r.starts_with('■'))
        .expect("the ■ row");
    let end = (head + 1..plain.len())
        .find(|&i| plain[i].trim().is_empty())
        .expect("the blank under the block");
    for tip in [
        "  Tip: Use /statusline to configure which items appear in the status line.",
        "  └ Tip: Use /statusline to configure which items appear in the status line.",
    ] {
        let mut with_tip = plain.clone();
        with_tip.insert(end, tip.to_string());
        let w = wall(&with_tip).expect("still the wall");
        assert_eq!(w.reset, want, "{tip}");
        assert!(!w.message.contains("Tip:"), "{}", w.message);
    }
}

/// `/goal pause` with no goal set (MEASURED 0.158.0): the `■ Failed to
/// update thread goal …` row ends nothing a wall names — no wall, and no
/// person's interrupt. NEGATIVE CONTROL: the measured interrupt is one.
#[test]
fn a_failed_goal_command_is_no_wall_and_no_interrupt() {
    let r = codex(GOAL_PAUSE_NONE);
    assert_eq!(r.wall, None);
    assert!(!r.interrupted);
    assert!(codex(INTERRUPTED).interrupted, "the control");
}

/// CODEX 0.158.0 DRAWS ITS INPUT LINE `»` (measured 2026-09-28 on a live
/// goal-mode session, read only: `cell 60 0` is `»` bold, `cell 60 2` the
/// placeholder's `A` dim). Read with `›` alone that screen had no composer —
/// `idle` NOT authoritatively — and the owner's tab stood a Codex upgrade
/// behind with nothing that acts on an idle point ever running there. The
/// `»` row is the composer, its placeholder the draft, its mark the one a
/// typed guard anchors to, and a turn running under it is busy as evidence.
/// The same screen with its turn ended is idle as evidence.
///
/// NEGATIVE CONTROLS: a `»` or `›` row of the TRANSCRIPT — the blocks under
/// it — is never the input line; and the same screen with a mark no build
/// draws in its place reads no composer and no evidence, as the old reader
/// read the real one.
#[test]
fn a_0_158_input_line_drawn_with_its_new_mark_is_the_composer() {
    let rows = screen(GOAL_BUSY_0_158);
    let caret = rows
        .iter()
        .position(|row| row.starts_with("» Ask Codex"))
        .expect("the input line");
    assert_eq!(
        crate::prompt::fixtures::cursor(GOAL_BUSY_0_158),
        Some((caret, 2)),
        "measured: the cursor on the placeholder"
    );
    let r = codex(GOAL_BUSY_0_158);
    assert_eq!(r.program, Program::Codex);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Busy, true)
    );
    assert_eq!(
        r.composer,
        Some((
            caret,
            vec![anchor_text("codex.composer.placeholder").to_string()]
        ))
    );
    assert_eq!(caret_on(&rows), Some('»'));
    assert_eq!(CodexReader.caret_on(&rows), Some('»'));
    assert!(is_codex_screen(&rows));
    assert_eq!(identify(None, &rows).program(), Program::Codex);

    // Its turn ended (the status row and its tip gone, an end row under the
    // last answer): idle, as evidence, at the same input line.
    let status = rows
        .iter()
        .position(|row| row.starts_with("• Working ("))
        .expect("the status row");
    let mut ended = rows.clone();
    ended[status] = "  8:51 AM".to_string();
    ended[status + 1] = String::new();
    assert_eq!(phase(&ended), (Phase::Idle, true));
    assert_eq!(composer(&ended), Some(caret));

    // A typed draft on the new mark is the person's.
    let mut drafted = ended.clone();
    drafted[caret] = "» fix the footer".to_string();
    assert_eq!(
        composer_draft(&drafted).map(|(_, l)| l),
        Some(vec!["fix the footer".to_string()])
    );

    // NEGATIVE CONTROL: a transcript row opening with either mark, blocks
    // under it, is no input line — the one under them all is.
    for mark in CARETS {
        let mut quoted = ended.clone();
        quoted[status - 1] = format!("{mark} a message that opens with the mark");
        assert_eq!(composer(&quoted), Some(caret), "{mark}");
        let mut last = ended[..status].to_vec();
        last.push(format!("{mark} a message with the transcript under it"));
        last.push("• and the agent's answer".to_string());
        last.push("  1:39 PM".to_string());
        assert_eq!(composer(&last), None, "{mark}");
    }

    // NEGATIVE CONTROL: a mark no build draws reads as the old reader read
    // the real screen — no composer, and no evidence either way.
    let mut unknown = ended.clone();
    unknown[caret] = unknown[caret].replacen('»', "▸", 1);
    assert_eq!(composer(&unknown), None);
    assert_eq!(phase(&unknown), (Phase::Idle, false));
    assert_eq!(caret_on(&unknown), None);
    assert_eq!(CodexReader.caret_on(&unknown), Some('›'), "the stock mark");
}

/// A GOAL'S NEXT TURN IS INVISIBLE TO THE SCREEN FOR ITS FIRST SECONDS
/// (measured 2026-09-28 on the owner's goal-mode Codex 0.158.0, read only):
/// about a second after a turn's end row, the next turn — a goal
/// continuation, which writes no user row — streams its first message under
/// that row with no status row, and the screen reads an ENDED turn, idle as
/// evidence, its last words the previous turn's. Frames a second apart grew
/// the message row by row; the goal's
/// rollout began that turn within 14 ms of the last one's end. This reading
/// is pinned as the fact it is: the screen cannot see such a turn, and what
/// may not end a running turn asks the thread's own record instead (the live
/// upgrade's floor on a client's own thread, `aterm_agent::harness::upgrade::
/// Facts::daemon_turn`). NEGATIVE CONTROL: the same frame with its status row
/// up reads busy.
#[test]
fn a_goals_next_turn_streaming_under_the_last_end_row_reads_ended() {
    let rows = screen(GOAL_NEXT_TURN_0_158);
    assert_eq!(
        crate::prompt::fixtures::cursor(GOAL_NEXT_TURN_0_158),
        Some((60, 2))
    );
    assert_eq!(phase(&rows), (Phase::Idle, true));
    // The last words read are the ENDED turn's; the next turn's streaming
    // message under its end row is not even among them.
    let said = said_tail(&rows).expect("the ended turn's last words");
    assert!(said.starts_with("All three are clean on main"), "{said}");
    assert!(!said.contains("The last turn made"), "{said}");
    assert_eq!(caret_on(&rows), Some('»'));
    let mut running = rows.clone();
    running[58] = "• Working (2s • esc to interrupt)".to_string();
    assert_eq!(phase(&running), (Phase::Busy, true));
}

// --- the paused goal's box, as `codex resume` draws it (2026-09-28) --------

/// `Resume paused goal?` (HAND-BUILT from the vendor's own render of it) is
/// its own kind: `Leave paused` the refusal that settles nothing, `Resume
/// goal` no role at all — the one decider that presses it matches it by its
/// label ([`resume_box`]: the live upgrade's resume of a goal it paused),
/// focused first. Its footer is a box's, so no footer (and no goal) is read
/// under it. NEGATIVE CONTROLS: retitled it is a box of no kind, which
/// [`resume_box`] does not read; the focus moved to `Leave paused`, the box
/// reads with no resume focused; the composer screen reads no such box.
#[test]
fn the_paused_goals_box_is_its_own_kind_and_reads_its_focus() {
    let r = codex(GOAL_RESUME);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Prompt, true)
    );
    let p = r.prompt.expect("the box");
    assert_eq!(p.kind, PromptKind::GoalResume);
    assert_eq!(p.kind.name(), "goal-resume");
    assert_eq!(p.title, anchor_text("codex.goal.resume.title"));
    assert_eq!(
        labels(&p),
        [
            anchor_text("codex.goal.resume.yes"),
            anchor_text("codex.goal.resume.leave")
        ]
    );
    assert_eq!(roles(&p), [Role::Other, Role::Deny]);
    assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Back));
    let rows = screen(GOAL_RESUME);
    let (row, resume) = resume_box(&rows).expect("read");
    assert!(resume);
    assert!(rows[row].starts_with("› 1. Resume goal"), "{}", rows[row]);
    assert_eq!(
        footer_status(&rows),
        None,
        "a box stands where the footer was"
    );
    assert_eq!(goal_state(&rows), None);

    let retitled: Vec<String> = rows
        .iter()
        .map(|r| r.replace("Resume paused goal?", "Resume paused task?"))
        .collect();
    assert_eq!(
        prompt(&retitled).expect("still a box").kind,
        PromptKind::Other
    );
    assert_eq!(resume_box(&retitled), None);

    let moved: Vec<String> = rows
        .iter()
        .map(|r| {
            r.replacen("› 1. Resume goal", "  1. Resume goal", 1)
                .replacen("  2. Leave paused", "› 2. Leave paused", 1)
        })
        .collect();
    let (row, resume) = resume_box(&moved).expect("read");
    assert!(!resume);
    assert!(moved[row].starts_with("› 2. Leave paused"));

    assert_eq!(resume_box(&screen(GOAL_NEXT_TURN_0_158)), None);
    assert_eq!(resume_box(&screen(RATE_NUDGE)), None);
}

/// A DAEMON-MODE CLIENT THE UPGRADE RELAUNCHED (lane A, 2026-09-27): its
/// conversation is redrawn under the session card over a BARE composer —
/// `›` and nothing, no placeholder — and, the program named, it is an
/// ended turn at an idle composer, authoritatively (its last block a tool
/// row: no last words). NEGATIVE CONTROL, and why a supervisor must name the
/// program before it trusts the point: by its layout alone the screen is
/// nobody's (the placeholder is what names an empty Codex composer), and a
/// generic read of it is no evidence.
#[test]
fn a_resumed_daemon_client_is_idle_once_its_program_is_named() {
    let rows = screen(RESUMED_DAEMON);
    let r = codex(RESUMED_DAEMON);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert_eq!(r.said_tail, None, "a tool row is no last words");
    assert!(composer(&rows).is_some());
    assert!(!is_codex_screen(&rows), "no placeholder, no status row");
    let unnamed = read(None, &rows, None);
    assert_eq!(identify(None, &rows).program(), Program::Generic);
    assert!(!unnamed.phase_authoritative, "{unnamed:?}");
}

/// CODEX 0.159.0 ENDS A TURN ON `Worked for <1s • 2:58 AM` — U+2022 where
/// every build before it drew ` · ` (measured 2026-09-29, lanes A and D of
/// `tools/test-codex-live-upgrade.sh`, the managed store on 0.159.0). Read
/// with ` · ` alone, no end was found under the last message: a relaunched
/// daemon client read BUSY for good and an inline one, its message scrolled
/// off, idle with no evidence — so the loop reached no idle point after the
/// live upgrade's `adopted` and nothing typed its carry-on (every tab stood
/// at `adopted` for the lane's 600 s). Both screens are ended turns at an
/// idle composer, authoritatively. NEGATIVE CONTROLS: the last end row taken
/// away, the daemon client's message has no end under it (busy) and the
/// inline screen says nothing (idle, no evidence); a `Worked for` row whose
/// clock follows neither mark is no end row; and 0.158.0's ` · ` still ends
/// a turn.
#[test]
fn a_relaunched_0_159_codex_ends_its_turns_on_the_bullet_end_row() {
    let r = codex(RESUMED_DAEMON_0_159);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(r.said_tail.as_deref(), Some("ok"));
    assert_eq!(r.composer.map(|(row, _)| row), Some(36));
    let r = codex(RESUMED_INLINE_0_159);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert!(
        r.said_tail
            .as_deref()
            .is_some_and(|t| t.ends_with("line 45 of a long answer")),
        "{:?}",
        r.said_tail
    );
    // NEGATIVE CONTROLS: the last end row gone.
    let without_last_end = |text: &str| {
        let mut rows = screen(text);
        let end = rows
            .iter()
            .rposition(|row| row.trim_start().starts_with("Worked for "))
            .expect("an end row");
        rows[end].clear();
        rows
    };
    let daemon = read(Some("codex"), &without_last_end(RESUMED_DAEMON_0_159), None);
    assert_eq!(
        (daemon.phase, daemon.phase_authoritative),
        (Phase::Busy, true)
    );
    let inline = read(Some("codex"), &without_last_end(RESUMED_INLINE_0_159), None);
    assert_eq!(
        (inline.phase, inline.phase_authoritative),
        (Phase::Idle, false)
    );
    let end = |row: &str| {
        let rows = rows(&["• ok", "", row, "", "› Ask Codex to do anything"]);
        is_end_row(&rows, 2)
    };
    assert!(end("  Worked for <1s • 2:58 AM"), "0.159.0");
    assert!(end("  Worked for 22m 18s · 9:17 AM"), "0.158.0");
    assert!(end("  Worked for 1m 2s • 13:39"), "a 24-hour clock");
    assert!(!end("  Worked for <1s - 2:58 AM"), "no mark");
    assert!(!end("  Worked for <1s • soon"), "no clock after the mark");
}
