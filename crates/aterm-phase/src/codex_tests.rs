// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One test per measured Codex screen (codex 0.156.1, and the ones measured
//! on later builds through 0.163.0), each with its negative control.

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
    WALL_AFTER_NOTICE_0_159,
    WALL_RESUMED_0_159,
    WALL_MID_TURN_0_159,
    WALL_MID_TURN_100X30_0_159,
    WALL_MID_TURN_160X50_0_159,
    IDLE_0_159,
    IDLE_0_163,
    BUSY_0_163,
    END_OF_TURN_0_163,
    BOX_EXEC_0_163,
    BOX_PATCH_0_163,
    INTERRUPTED_0_163,
    WALL_5H_FREE_0_163,
    WALL_5H_FREE_AFTER_ESC_0_163,
    WALL_5H_NOPLAN_0_163,
    WALL_5H_NOPLAN_AFTER_ESC_0_163,
    WALL_5H_PLUS_0_163,
    WALL_5H_PLUS_AFTER_ESC_0_163,
    WALL_5H_PLUS_CREDITS_0_163,
    WALL_5H_PLUS_CREDITS_AFTER_ESC_0_163,
    WALL_5H_PRO_0_163,
    WALL_5H_PRO_AFTER_ESC_0_163,
    WALL_5H_TEAM_0_163,
    WALL_5H_TEAM_AFTER_ESC_0_163,
    WALL_WEEKLY_PLUS_0_163,
    WALL_MEMBER_CREDITS_0_163,
    WALL_MEMBER_CREDITS_AFTER_ESC_0_163,
    WALL_MEMBER_SPEND_CAP_0_163,
    WALL_MEMBER_SPEND_CAP_AFTER_ESC_0_163,
    WALL_OWNER_CREDITS_0_163,
    WALL_OWNER_CREDITS_AFTER_ESC_0_163,
    WALL_OWNER_USAGE_0_163,
    WALL_OWNER_USAGE_AFTER_ESC_0_163,
    NEAR_LIMIT_5H_0_163,
    NEAR_LIMIT_WEEKLY_0_163,
    CREDITS_IN_USE_0_163,
    STATUS_CHATGPT_0_163,
    STATUS_CHATGPT_CREDITS_IN_USE_0_163,
    STATUS_LAB_0_163,
    STATUS_LINE_LIMITS_CHATGPT_0_163,
    STATUS_LINE_LIMITS_LAB_0_163,
    AUTH_401_API_KEY_0_163,
    AUTH_401_REFRESH_EXPIRED_0_163,
    AUTH_401_REFRESH_INVALIDATED_0_163,
    AUTH_401_REFRESH_REUSED_0_163,
    AUTH_401_ACCOUNT_CHANGED_0_163,
    SIGN_IN_0_163,
    TRUST_0_163,
    SLASH_LOGIN_0_163,
    SLASH_LOG_0_163,
    LOGOUT_0_163,
    EXIT_0_163,
    BOX_RULE_0_163,
    BOX_RULE_JUSTIFIED_0_163,
    BOX_RULE_OVER_ESCALATION_0_163,
    BOX_ESCALATED_0_163,
    BOX_ESCALATED_NO_REASON_0_163,
    BOX_ESCALATED_UNNAMED_0_163,
    NO_RULE_NO_BOX_0_163,
    RULE_UNNAMED_NO_BOX_0_163,
    QUESTION_0_163,
    QUESTION_NONE_FOCUSED_0_163,
    QUESTION_NOTES_OPEN_0_163,
    QUESTION_NOTES_TYPED_0_163,
    QUESTION_NOTES_SUBMITTED_0_163,
    QUESTION_DIGIT_SUBMITS_0_163,
    QUESTION_NOTES_ESC_0_163,
    QUESTION_NOTES_REOPENED_0_163,
    QUESTION_NOTES_ESC_EMPTY_0_163,
    QUESTION_NOTES_TAB_CLEARS_0_163,
    QUESTION_INTERRUPTED_0_163,
    STREAM_CUT_RETRY_1_0_163,
    STREAM_CUT_RETRY_5_0_163,
    STREAM_CUT_FINAL_0_163,
    STREAM_CUT_NOTEXT_RETRY_1_0_163,
    STREAM_CUT_NOTEXT_RETRY_5_0_163,
    STREAM_CUT_NOTEXT_FINAL_0_163,
    STREAM_CUT_RECOVER_RETRY_0_163,
    STREAM_CUT_RECOVERED_0_163,
    HIGH_DEMAND_RETRY_1_0_163,
    HIGH_DEMAND_RETRY_2_0_163,
    HIGH_DEMAND_FINAL_0_163,
    STATUS_502_RETRY_0_163,
    STATUS_502_FINAL_0_163,
    CTX_DEFAULT_0_163,
    CTX_BUSY_0_163,
    COMPACTING_0_163,
    COMPACTED_0_163,
    COMPACTION_WARNING_0_163,
    CTX_STATUS_CARD_0_163,
    CTX_LEFT_0_163,
    CTX_LEFT_FULL_0_163,
    CTX_LEFT_BUSY_0_163,
    CTX_STATUS_LINE_ITEMS_0_163,
    MODEL_PICK_0_163,
    EFFORT_PICK_0_163,
    MODEL_CHANGED_0_163,
    PERMISSIONS_0_163,
    FULL_ACCESS_CONFIRM_0_163,
    FULL_ACCESS_APPLIED_0_163,
    RESUMED_0_163,
    RESUMED_AFTER_TURN_0_163,
    RESUMED_PERMISSIONS_0_163,
    RESUMED_NOFLAGS_PERMISSIONS_0_163,
    QUESTION_LAB_0_163,
    QUESTION_ANSWERED_LAB_0_163,
    QUESTION_ESC_LAB_0_163,
    TALL_REPLY_LAB_0_163,
    PINNED_PROMPT_LAB_0_163,
    PINNED_QUESTION_LAB_0_163,
];

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

/// An end row is the clock under a blank row, followed only by blank rows
/// and column-0 rows (a message, the composer, a `•` or `■` block, a slash
/// command's echo): a bare clock the streaming answer wrote under its own
/// words, or an answer's own paragraph that is a bare clock (`  9:30` over
/// `  - standup`, the answer going on under it at column 2), is no end row,
/// and the turn still runs. NEGATIVE CONTROL: the same clock under a blank
/// row with nothing under it but the composer ends the turn (the one frame
/// no reader can tell from a real end).
#[test]
fn a_clock_in_the_answer_is_not_an_end_row() {
    let r = screen(STREAMING);
    let last = r
        .iter()
        .rposition(|row| row.starts_with("  design, since code"))
        .expect("the streaming row");
    for (under, want) in [
        (&["  12:30"][..], Phase::Busy),
        (&["", "  9:30", "  - standup"], Phase::Busy),
        (&["", "  12:30"], Phase::Idle),
        (&["", "  9:30"], Phase::Idle),
    ] {
        let mut x = r.clone();
        for (k, row) in under.iter().enumerate() {
            x.insert(last + 1 + k, row.to_string());
        }
        assert_eq!(phase(&x), (want, true), "{under:?}");
    }
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
    // `targo --unverified lab codex-upgrade`), reads the same.
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
        // The model's answers are answers; `None of the above` asks for the
        // person's own words.
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
    let p = codex(QUESTION).prompt.expect("the dialog");
    assert_eq!(
        p.description,
        "What should the new subtraction function be called?"
    );
    assert!(labels(&p)[0].contains(anchor_text("codex.question.recommended")));
    assert_eq!(p.focused().map(|o| o.n), Some(Some(1)));
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

/// CODEX 0.159.0 REWORDED THE ESC ROW (the strings of the 0.159.3 binary):
/// `■ Conversation interrupted - use /feedback if something went wrong`,
/// where 0.156.1 went on `tell the model what to do differently.`. Anchored
/// on that whole old sentence, the person's Esc on a current Codex read as
/// an ordinary ended turn, and the continuation policy typed over it once
/// the person's grace ran out. NEGATIVE CONTROL: a failure row is not the
/// interrupt, whatever it goes on to say.
#[test]
fn the_0_159_interrupt_is_still_the_persons() {
    let mut r = screen(INTERRUPTED);
    let head = r
        .iter()
        .position(|row| row.starts_with("■ Conversation interrupted"))
        .expect("the interrupt row");
    r[head] = "■ Conversation interrupted - use /feedback if something went wrong".to_string();
    r[head + 1] = String::new();
    assert_eq!(phase(&r), (Phase::Idle, true));
    assert!(interrupted(&r));
    assert_eq!(wall(&r), None);
    let mut narrow = r.clone();
    narrow[head] = "■ Conversation interrupted - use /feedback if".to_string();
    narrow[head + 1] = "something went wrong".to_string();
    assert!(interrupted(&narrow), "wrapped mid-sentence");
    let mut other = r.clone();
    other[head] = "■ Conversation failed - use /feedback if something went wrong".to_string();
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

/// MEASURED on 0.163.0 (the weekly wall of a Plus plan, Esc pressed once on
/// the nudge under it; it replaced 0.156.1's hand-built row): a turn that
/// ended on the usage limit is `limited`, its wall the usage window with the
/// reset its row names, the year dropped, the row wrapped to column 0 joined
/// back, the usage notice right-aligned over the composer no part of it.
/// NEGATIVE CONTROL: a turn that ended by itself has no wall.
#[test]
fn the_usage_limit_row_is_a_wall() {
    let r = codex(HIT_LIMIT);
    let w = r.wall.clone().expect("the wall");
    assert_eq!(w.kind, WallKind::UsageSession);
    assert_eq!(w.reset.as_deref(), Some("Oct 17th 1:14 PM"));
    assert!(
        w.message.ends_with("try again at Oct 17th, 2026 1:14 PM."),
        "{}",
        w.message
    );
    assert!(!w.message.contains('⚠'), "{}", w.message);
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

/// MEASURED on 0.159.2 (2026-09-29, a private instance whose loopback model
/// answered `usage_limit_reached`): the usage wall as the last block, on
/// every screen it was reached on — after a turn that ran a command, after
/// a notice typed as a turn, redrawn by `codex resume` — and however wide
/// the pane wraps its row (after `purchase more`, after `or try again`, not
/// at all). Each is the usage window with no reset, the row read whole, the
/// phase `Limited` and authoritative. NEGATIVE CONTROLS: the fresh idle
/// screen has no wall, and neither has a wall screen whose `■` block is
/// gone — the notice's words above it are no wall.
#[test]
fn the_measured_0_159_usage_wall_is_read_on_every_screen_and_width_it_wraps_at() {
    let whole = "You’ve hit your usage limit. Visit https://chatgpt.com/codex/settings/usage \
                 to purchase more credits or try again later.";
    for (text, what) in [
        (WALL_AFTER_NOTICE_0_159, "after the notice's turn"),
        (WALL_RESUMED_0_159, "redrawn by codex resume"),
        (WALL_MID_TURN_0_159, "mid-turn, 120 columns"),
        (WALL_MID_TURN_100X30_0_159, "mid-turn, 100 columns"),
        (WALL_MID_TURN_160X50_0_159, "mid-turn, 160 columns"),
    ] {
        let r = codex(text);
        let w = r.wall.clone().unwrap_or_else(|| panic!("{what}: a wall"));
        assert_eq!(w.kind, WallKind::UsageSession, "{what}");
        assert!(w.kind.reads_limited(), "{what}");
        assert_eq!(w.reset, None, "{what}: {}", w.message);
        assert_eq!(w.message, whole, "{what}");
        assert!(matches!(r.phase, Phase::Limited { .. }), "{what}");
        assert!(r.phase_authoritative, "{what}");
        // The same screen without the wall's block: nothing limits it.
        let rows = screen(text);
        let at = rows.iter().position(|r| r.starts_with('■')).expect(what);
        let end = (at..rows.len())
            .find(|&i| rows[i].trim().is_empty())
            .expect(what);
        let mut gone = rows.clone();
        gone.drain(at..end);
        assert_eq!(wall(&gone), None, "{what}: the control");
    }
    assert_eq!(codex(IDLE_0_159).wall, None, "the control");
    assert_eq!(codex(IDLE_0_159).phase, Phase::Idle, "the control");
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
        // A stream cut off before it was whole: trying again answers it.
        (
            "■ stream disconnected before completion: connection reset",
            WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::CutOff,
            },
        ),
        // NEGATIVE CONTROL: one that names the status it was cut on is the
        // API's answer.
        (
            "■ stream disconnected before completion: last status: 503 Service Unavailable",
            WallKind::ApiError {
                code: Some(503),
                retryable: true,
                cause: ApiCause::Server,
            },
        ),
        // The API's status as Codex prints it (0.163.0, measured): a 401
        // is the login refused, any other an API error by its status.
        (
            "■ unexpected status 401 Unauthorized: Incorrect API key provided: lab-not-a-key., url: http://127.0.0.1:1/v1/responses",
            WallKind::Auth,
        ),
        (
            "■ unexpected status 502 Bad Gateway: event: response.created",
            WallKind::ApiError {
                code: Some(502),
                retryable: true,
                cause: ApiCause::Server,
            },
        ),
        (
            "■ unexpected status 400 Bad Request: {\"error\":\"bad\"}",
            WallKind::ApiError {
                code: Some(400),
                retryable: false,
                cause: ApiCause::Server,
            },
        ),
        // Codex's own words for a 500 (0.163.0, measured): no status on
        // the row, and the status it stands for.
        (
            "■ We’re currently experiencing high demand, which may cause temporary errors.",
            WallKind::ApiError {
                code: Some(500),
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

/// Every fixture FILE under `src/fixtures/` that is not Codex's, as `(its
/// name, its rows)`: Claude Code's screens — the `claude-*` captures and the
/// hand-built and saved ones beside them.
fn claude_fixture_files() -> Vec<(String, Vec<String>)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures");
    let mut out: Vec<(String, Vec<String>)> = std::fs::read_dir(&dir)
        .expect("the fixtures")
        .map(|e| e.expect("a fixture").path())
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_string();
            let text = std::fs::read_to_string(&p).expect("a fixture");
            (!name.starts_with("codex-")).then(|| (name, screen(&text)))
        })
        .collect();
    out.sort();
    out
}

/// Codex's reader on Claude Code's screens, the other way round from
/// [`claudes_reader_on_codex_screens_sees_no_box_and_no_question`]: no box,
/// no question, no composer, no wall, and no screen that names Codex — on
/// every one it reads `idle` and NOT as evidence, so nothing acts on it.
/// `identify` sniffs Claude Code first, but a session NAMED `codex` is read by
/// this reader whatever its screen shows. NEGATIVE CONTROLS: the same reads
/// on Codex's own screens do find its box, its question and its composer.
#[test]
fn codex_reader_on_claude_screens_sees_no_box_and_no_question() {
    let files = claude_fixture_files();
    let claude = files
        .iter()
        .filter(|(n, _)| n.starts_with("claude-"))
        .count();
    assert!(claude >= 300, "only {claude} claude-* fixtures read");
    for (name, r) in &files {
        let phase = CodexReader.phase(r);
        assert!(
            !matches!(phase, Phase::Prompt | Phase::Question),
            "{name}: {phase:?}"
        );
        assert_eq!(super::phase(r), (Phase::Idle, false), "{name}");
        assert!(
            !CodexReader.phase_authoritative(r, &phase),
            "{name}: evidence"
        );
        assert_eq!(CodexReader.prompt(r), None, "{name}");
        assert_eq!(codex_box(r), None, "{name}");
        assert_eq!(CodexReader.composer(r), None, "{name}");
        assert_eq!(CodexReader.wall(r), None, "{name}");
        assert!(!is_codex_screen(r), "{name}");
        assert_ne!(identify(None, r).program(), Program::Codex, "{name}");
    }
    // The controls: Codex's own box, question and composer are read.
    assert!(CodexReader.prompt(&screen(BOX_EXEC)).is_some());
    assert_eq!(CodexReader.phase(&screen(QUESTION_END)), Phase::Question);
    assert!(is_codex_screen(&screen(IDLE)));
    assert!(CodexReader.composer(&screen(IDLE)).is_some());
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
    assert!(!Program::Generic.supervisable(), "the generic reader");
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

/// Codex 0.163.0 through one turn, measured 2026-10-16 on a real session:
/// a fresh session is idle WITH evidence, as 0.156.1's is — though the
/// session card has drawn no box since 0.158.0, its title row says it
/// ([`is_card_title`]), and so 0.158.0's and 0.159.2's fresh screens are
/// fresh too — a turn running is busy, its end idle with what it said, both
/// boxes are boxes, and a declined box is the turn interrupted; and every
/// one of them names Codex with no program name, or a runtime's. NEGATIVE
/// CONTROLS: the fresh screen without its card's title row is idle with no
/// evidence (nothing tells it from a transcript scrolled past its first
/// message), and not fresh; nor is it with the title where a tool's output
/// draws it (column 4, under `└`), or at column 2 with no folder row under
/// it (an answer quoting the words).
#[test]
fn codex_0_163_reads_a_fresh_session_and_a_turn() {
    for text in [IDLE_0_163, IDLE_158, IDLE_0_159] {
        let r = codex(text);
        let at = provenance(text).unwrap_or_default();
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true),
            "{at}"
        );
        assert!(r.fresh, "{at}");
        assert!(r.composer.is_some(), "{at}");
    }
    let busy = codex(BUSY_0_163);
    assert_eq!((busy.phase, busy.phase_authoritative), (Phase::Busy, true));
    let end = codex(END_OF_TURN_0_163);
    assert_eq!(
        (end.phase.clone(), end.phase_authoritative),
        (Phase::Idle, true)
    );
    assert!(!end.fresh);
    assert_eq!(end.said_tail.as_deref(), Some("hi"));
    for text in [BOX_EXEC_0_163, BOX_PATCH_0_163] {
        let r = codex(text);
        let at = provenance(text).unwrap_or_default();
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Prompt, true),
            "{at}"
        );
        assert!(r.prompt.is_some(), "{at}");
    }
    let stop = codex(INTERRUPTED_0_163);
    assert_eq!(
        (stop.phase.clone(), stop.phase_authoritative),
        (Phase::Idle, true)
    );
    assert!(stop.interrupted && !stop.fresh);
    for text in [
        IDLE_0_163,
        BUSY_0_163,
        END_OF_TURN_0_163,
        BOX_EXEC_0_163,
        BOX_PATCH_0_163,
    ] {
        for name in [None, Some("node")] {
            assert_eq!(
                identify(name, &screen(text)).program(),
                Program::Codex,
                "{name:?}: {}",
                provenance(text).unwrap_or_default()
            );
        }
    }

    let untitled: Vec<String> = screen(IDLE_0_163)
        .into_iter()
        .filter(|r| !r.contains(anchor_text("codex.card.title")))
        .collect();
    let r = read(Some("codex"), &untitled, None);
    assert_eq!(
        (r.phase, r.phase_authoritative),
        (Phase::Idle, false),
        "the control"
    );
    assert!(!r.fresh, "the control");
    let title = screen(IDLE_0_163)
        .iter()
        .position(|r| r.contains(anchor_text("codex.card.title")))
        .expect("the card's title row");
    let mut tool_output = screen(IDLE_0_163);
    tool_output[title] = format!("  {}", tool_output[title]);
    let mut quoted = screen(IDLE_0_163);
    quoted[title + 1] = "  is what the card says.".to_string();
    for (what, rows) in [("tool output", tool_output), ("quoted", quoted)] {
        let r = read(Some("codex"), &rows, None);
        assert_eq!(
            (r.phase, r.phase_authoritative),
            (Phase::Idle, false),
            "{what}"
        );
        assert!(!r.fresh, "{what}");
    }
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
/// ([`ended_under_background`]): with
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

/// A USAGE WALL UNDER A BACKGROUND TERMINAL IS LIMITED, on every wall screen
/// measured (and the hand-built one that names a reset), the line spliced in
/// over the composer or right under the wall's block: a `■` block is the
/// turn's end whatever is drawn under it, so [`phase`] — through
/// [`CodexReader`], the supervisor's turn reading — is the wall the screen
/// without the line reads, its message and reset the block's own, and the
/// one [`ended_under_background`] names. NEGATIVE CONTROLS: the measured idle
/// and question ends under the same line read busy (the host's background
/// step waits on the idle one), each named by [`ended_under_background`].
#[test]
fn a_usage_wall_under_a_background_terminal_is_limited() {
    let line = "  1 background terminal running · /ps to view · /stop to close";
    for text in [
        HIT_LIMIT,
        HIT_LIMIT_NO_RESET,
        WALL_AFTER_NOTICE_0_159,
        WALL_RESUMED_0_159,
        WALL_MID_TURN_0_159,
        WALL_MID_TURN_100X30_0_159,
        WALL_MID_TURN_160X50_0_159,
    ] {
        let what = provenance(text).unwrap_or_default();
        let r = screen(text);
        let (alone, auth) = phase(&r);
        assert!(auth && matches!(alone, Phase::Limited { .. }), "{what}");
        let c = composer(&r).expect(what);
        let head = r.iter().position(|row| row.starts_with('■')).expect(what);
        let under = (head..r.len())
            .find(|&i| crate::text::trim(&r[i]).is_empty())
            .expect(what);
        for at in [c - 1, under] {
            let mut bg = r.clone();
            bg.insert(at, line.to_string());
            assert_eq!(phase(&bg), (alone.clone(), true), "{what}: row {at}");
            assert_eq!(CodexReader.phase(&bg), alone, "{what}: row {at}");
            assert_eq!(
                ended_under_background(&bg),
                Some(alone.clone()),
                "{what}: row {at}"
            );
            assert_eq!(background_wait(&bg), None, "{what}: row {at}");
        }
    }
    for (text, ended) in [(END_OF_TURN, Phase::Idle), (QUESTION_END, Phase::Question)] {
        let mut bg = screen(text);
        let c = composer(&bg).expect("a composer");
        bg.insert(c - 1, line.to_string());
        assert_eq!(phase(&bg), (Phase::Busy, true), "{ended:?}");
        assert_eq!(CodexReader.phase(&bg), Phase::Busy, "{ended:?}");
        assert_eq!(ended_under_background(&bg), Some(ended.clone()));
        assert_eq!(background_wait(&bg).is_some(), ended == Phase::Idle);
    }
}

// --- the rate-limit nudge, the `/model` picker, the footer (2026-09-28) -----

/// The rate-limit model nudge (MEASURED on 0.163.0, after a turn whose answer
/// said 92% of the 5h window was used; it replaced the box hand-built from the
/// 0.157.1 strings) is its own kind: its plain keep the
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

/// IN A NARROW PANE the goal still reads (each footer CAPTURED LIVE,
/// 2026-09-28, from Codex 0.158.0 in a scratch headless aterm, a scratch
/// `$CODEX_HOME` and a dummy key): Codex cuts the
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
/// `targo --unverified lab codex-upgrade`, the managed store on 0.159.0). Read
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

// --- codex 0.163.0, the lab's captures (2026-10-16) --------------------------

/// The rows of `text` with its last `■` block — the head and the rows it
/// wraps onto, to the blank row under it — taken out.
fn without_wall(text: &str) -> Vec<String> {
    let rows = screen(text);
    let at = rows
        .iter()
        .rposition(|r| r.starts_with('■'))
        .expect("a ■ block");
    let end = (at..rows.len())
        .find(|&i| rows[i].trim().is_empty())
        .expect("a blank row under it");
    let mut gone = rows.clone();
    gone.drain(at..end);
    gone
}

/// THE USAGE WALL OF EVERY PLAN, AS 0.163.0 DRAWS IT: the `■` row, and under
/// it — before any key — the rate-limit nudge, which replaces the composer
/// and is read as its own box (`RateNudge`: switch no role, keep the
/// refusal, never-again a standing setting). After one Esc the nudge is gone
/// and the turn reads `Limited` by the usage window, the reset its row names
/// (none on the owner's `Increase your limits` row), the usage notice
/// right-aligned over the composer no part of it. An account holding credits
/// is drawn no nudge at all. NEGATIVE CONTROL: each after-Esc screen with its
/// `■` block taken out has no wall — its message has no end under it, and
/// its turn reads running.
#[test]
fn the_0_163_usage_walls_of_every_plan_are_read_and_the_nudge_under_them_is_a_box() {
    for (before, after, reset, what) in [
        (
            WALL_5H_FREE_0_163,
            WALL_5H_FREE_AFTER_ESC_0_163,
            Some("11:23 AM"),
            "free",
        ),
        (
            WALL_5H_NOPLAN_0_163,
            WALL_5H_NOPLAN_AFTER_ESC_0_163,
            Some("11:23 AM"),
            "no plan",
        ),
        (
            WALL_5H_PLUS_0_163,
            WALL_5H_PLUS_AFTER_ESC_0_163,
            Some("11:23 AM"),
            "plus",
        ),
        (
            WALL_5H_PRO_0_163,
            WALL_5H_PRO_AFTER_ESC_0_163,
            Some("11:23 AM"),
            "pro",
        ),
        (
            WALL_5H_TEAM_0_163,
            WALL_5H_TEAM_AFTER_ESC_0_163,
            Some("11:24 AM"),
            "team",
        ),
        (
            WALL_WEEKLY_PLUS_0_163,
            HIT_LIMIT,
            Some("Oct 17th 1:14 PM"),
            "weekly plus",
        ),
        (
            WALL_OWNER_CREDITS_0_163,
            WALL_OWNER_CREDITS_AFTER_ESC_0_163,
            None,
            "owner credits",
        ),
        (
            WALL_OWNER_USAGE_0_163,
            WALL_OWNER_USAGE_AFTER_ESC_0_163,
            None,
            "owner usage",
        ),
    ] {
        let r = codex(before);
        assert_eq!(
            (r.phase, r.phase_authoritative),
            (Phase::Prompt, true),
            "{what}"
        );
        assert_eq!(r.composer, None, "{what}: the nudge replaces the composer");
        let p = r.prompt.expect(what);
        assert_eq!(p.kind, PromptKind::RateNudge, "{what}");
        assert_eq!(
            roles(&p),
            [Role::Other, Role::Deny, Role::Persist],
            "{what}"
        );
        let r = codex(after);
        let w = r.wall.clone().unwrap_or_else(|| panic!("{what}: the wall"));
        assert_eq!(w.kind, WallKind::UsageSession, "{what}");
        assert_eq!(w.reset.as_deref(), reset, "{what}: {}", w.message);
        assert!(!w.message.contains('⚠'), "{what}: {}", w.message);
        assert!(matches!(r.phase, Phase::Limited { .. }), "{what}");
        assert!(r.phase_authoritative, "{what}");
        let gone = without_wall(after);
        assert_eq!(wall(&gone), None, "{what}: the control");
        assert_eq!(phase(&gone), (Phase::Busy, true), "{what}: the control");
    }
    for text in [
        WALL_5H_PLUS_CREDITS_0_163,
        WALL_5H_PLUS_CREDITS_AFTER_ESC_0_163,
    ] {
        let r = codex(text);
        assert_eq!(r.prompt, None, "credits: no nudge");
        assert_eq!(
            r.wall.map(|w| (w.kind, w.reset)),
            Some((WallKind::UsageSession, Some("11:24 AM".to_string())))
        );
    }
    // A member's walls are the spend kind, read once the banner and the
    // nudge are gone.
    for text in [
        WALL_MEMBER_CREDITS_AFTER_ESC_0_163,
        WALL_MEMBER_SPEND_CAP_AFTER_ESC_0_163,
    ] {
        let rows = screen(text);
        assert_eq!(prompt(&rows).map(|p| p.kind), Some(PromptKind::RateNudge));
    }
}

/// THE LIMIT-INCREASE BANNER IS A BOX WHOSE YES NO RULE PRESSES (gap #3): a
/// workspace MEMBER out of credits, or at the spend cap the owner set, is
/// drawn `Usage limit reached` / `Request a limit increase from your owner to
/// continue using codex. Request increase?` over `1. Yes (y)` and `› 2. No
/// (default) (n)` under the wall, in place of the composer (measured on
/// 0.163.0; the banner's `Yes` notifies the owner). It is a prompt, its `No`
/// the refusal, its `Yes` no role at all — and with the composer gone,
/// nothing reads a point to type at. NEGATIVE CONTROLS: the same box without
/// the banner's question is a box of no kind whose options carry no roles —
/// its `No` too; and the banner's words in a box of a known kind (an exec
/// box whose Reason row the model wrote as `Request increase?`) leave its
/// roles as its kind gives them.
#[test]
fn the_member_request_banner_is_a_box_whose_yes_has_no_role() {
    for text in [WALL_MEMBER_CREDITS_0_163, WALL_MEMBER_SPEND_CAP_0_163] {
        let r = codex(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Prompt, true)
        );
        assert_eq!(r.composer, None);
        assert_eq!(
            r.wall, None,
            "the wall is under the box, not the turn's end"
        );
        let p = r.prompt.expect("the banner");
        assert_eq!(p.kind, PromptKind::Other);
        assert_eq!(p.title, "Usage limit reached");
        assert_eq!(labels(&p), ["Yes", "No (default)"]);
        assert_eq!(roles(&p), [Role::Other, Role::Deny]);
        assert_eq!(p.focused().map(|o| o.label.as_str()), Some("No (default)"));
        assert!(
            screen(text)
                .iter()
                .any(|r| r.contains(anchor_text("codex.banner.request")))
        );
        let plain: Vec<String> = screen(text)
            .into_iter()
            .map(|r| r.replace(anchor_text("codex.banner.request"), "Continue?"))
            .collect();
        let q = prompt(&plain).expect("still a box");
        assert!(roles(&q).iter().all(|r| *r == Role::Other), "{q:?}");
    }
    let asked: Vec<String> = screen(BOX_ESCALATED_0_163)
        .into_iter()
        .map(|r| {
            if r.trim_start().starts_with("Reason: ") {
                format!("  Reason: {}", anchor_text("codex.banner.request"))
            } else {
                r
            }
        })
        .collect();
    let p = prompt(&asked).expect("the exec box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(
        roles(&p),
        [Role::Once, Role::Persist, Role::Deny],
        "the control"
    );
}

/// A USAGE NOTICE UNDER THE END ROW ENDS THE TURN AS A TIP DOES: 0.163.0
/// draws `⚠ 5h limit: 24% left · resets at 12:14 PM · /status` right-aligned
/// between a turn's end row and the composer once the window passes a
/// threshold, and its warning cell (`⚠ Heads up, you have less than 25% of
/// your 5h limit left. …`) in column 0 between the answer and the end row.
/// The turn reads ENDED — idle, as evidence — and the agent's last words are
/// its answer, not the warning. NEGATIVE CONTROLS: the same notice drawn at
/// column 2 (as an answer's own paragraph would be) keeps the clock above it
/// from being an end; an answer that ended on a question still reads as one
/// with the warning cell under it, while the same words in a `•` block are
/// the agent's own last words.
#[test]
fn a_usage_notice_under_the_end_row_ends_the_turn() {
    for text in [
        NEAR_LIMIT_5H_0_163,
        NEAR_LIMIT_WEEKLY_0_163,
        STATUS_LINE_LIMITS_CHATGPT_0_163,
        STATUS_LINE_LIMITS_LAB_0_163,
    ] {
        let what = provenance(text).unwrap_or_default();
        let r = codex(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true),
            "{what}"
        );
        assert_eq!(r.wall, None, "{what}");
        let said = r.said_tail.expect(what);
        assert!(
            said.starts_with("Hi! I am the local lab stub"),
            "{what}: {said}"
        );
        assert!(!said.contains("Heads up"), "{what}: {said}");
        let rows = screen(text);
        let notice = rows
            .iter()
            .position(|r| r.trim_start().starts_with("⚠ ") && r.starts_with("    "))
            .expect("the usage notice");
        let mut left = rows.clone();
        left[notice] = format!("  {}", rows[notice].trim_start());
        assert_eq!(phase(&left), (Phase::Busy, true), "{what}: the control");
    }
    let rows = screen(NEAR_LIMIT_5H_0_163);
    let answer = rows
        .iter()
        .rposition(|r| r.starts_with("• Hi! I am the local lab stub"))
        .expect("the answer");
    let mut asks = rows.clone();
    asks[answer] = "• Should I run the migration now?".to_string();
    assert_eq!(phase(&asks), (Phase::Question, true));
    assert_eq!(
        said_tail(&asks).as_deref(),
        Some("Should I run the migration now?")
    );
    let warning = rows
        .iter()
        .position(|r| r.starts_with("⚠ Heads up"))
        .expect("the warning cell");
    let mut said = asks.clone();
    said[warning] = rows[warning].replacen('⚠', "•", 1);
    assert_eq!(phase(&said), (Phase::Idle, true), "the control");
}

/// THE 401s OF 0.163.0 ARE THE LOGIN'S WALL: an API key the provider refused
/// (`■ unexpected status 401 Unauthorized: Incorrect API key provided: …`)
/// and a ChatGPT login whose refresh token expired, was revoked, was used
/// already, or belongs to an account `auth.json` no longer holds — each
/// `Auth`, read at an idle composer as evidence. NEGATIVE CONTROL: another
/// status on the same row is an API error by its status.
#[test]
fn every_0_163_401_is_the_logins_wall() {
    for text in [
        AUTH_401_API_KEY_0_163,
        AUTH_401_REFRESH_EXPIRED_0_163,
        AUTH_401_REFRESH_INVALIDATED_0_163,
        AUTH_401_REFRESH_REUSED_0_163,
        AUTH_401_ACCOUNT_CHANGED_0_163,
    ] {
        let what = provenance(text).unwrap_or_default();
        let r = codex(text);
        assert_eq!(
            r.wall.as_ref().map(|w| w.kind),
            Some(WallKind::Auth),
            "{what}"
        );
        assert_eq!(
            (r.phase, r.phase_authoritative),
            (Phase::Idle, true),
            "{what}"
        );
    }
    let rows: Vec<String> = screen(AUTH_401_API_KEY_0_163)
        .into_iter()
        .map(|r| r.replace("401 Unauthorized", "403 Forbidden"))
        .collect();
    assert_eq!(
        wall(&rows).map(|w| w.kind),
        Some(WallKind::ApiError {
            code: Some(403),
            retryable: false,
            cause: ApiCause::Server
        }),
        "the control"
    );
}

/// A STREAM CUT, AND THE API'S OTHER FAILURES, AS 0.163.0 DRAWS THEM: while
/// Codex retries (`• Reconnecting... N/5 (… • esc to interrupt)` over `└
/// <the error>`) the turn is busy and no wall is read; once its retries ran
/// out, the turn ends on a `■` row — a cut stream `ApiError` of cause
/// `CutOff` (its partial answer the last words when one was drawn), a 500
/// Codex's `high demand` words with status 500, a 502 its status, the SSE
/// body the 502 row quotes joined into its message. A cut the next answer
/// recovered from ends on that answer, the tip under it no part of it.
/// NEGATIVE CONTROL: each final screen with its `■` block taken out has no
/// wall.
#[test]
fn a_stream_cut_and_the_apis_failures_are_read_by_cause() {
    for text in [
        STREAM_CUT_RETRY_1_0_163,
        STREAM_CUT_RETRY_5_0_163,
        STREAM_CUT_NOTEXT_RETRY_1_0_163,
        STREAM_CUT_NOTEXT_RETRY_5_0_163,
        STREAM_CUT_RECOVER_RETRY_0_163,
        HIGH_DEMAND_RETRY_1_0_163,
        HIGH_DEMAND_RETRY_2_0_163,
        STATUS_502_RETRY_0_163,
    ] {
        let what = provenance(text).unwrap_or_default();
        let r = codex(text);
        assert_eq!(
            (r.phase, r.phase_authoritative),
            (Phase::Busy, true),
            "{what}"
        );
        assert_eq!(r.wall, None, "{what}");
    }
    let cut = WallKind::ApiError {
        code: None,
        retryable: true,
        cause: ApiCause::CutOff,
    };
    let server = |code| WallKind::ApiError {
        code: Some(code),
        retryable: true,
        cause: ApiCause::Server,
    };
    for (text, kind, said) in [
        (
            STREAM_CUT_FINAL_0_163,
            cut,
            Some("Partial answer before the cut"),
        ),
        (STREAM_CUT_NOTEXT_FINAL_0_163, cut, None),
        (HIGH_DEMAND_FINAL_0_163, server(500), None),
        (STATUS_502_FINAL_0_163, server(502), None),
    ] {
        let what = provenance(text).unwrap_or_default();
        let r = codex(text);
        assert_eq!(r.wall.as_ref().map(|w| w.kind), Some(kind), "{what}");
        assert_eq!(
            (r.phase, r.phase_authoritative),
            (Phase::Idle, true),
            "{what}"
        );
        assert_eq!(
            r.said_tail.as_deref().map(|s| &s[..s.len().min(29)]),
            said,
            "{what}"
        );
        assert_eq!(wall(&without_wall(text)), None, "{what}: the control");
    }
    let quoted = codex(STATUS_502_FINAL_0_163).wall.expect("the 502").message;
    assert!(
        quoted.ends_with("http://127.0.0.1:49683/v1/responses"),
        "{quoted}"
    );
    let r = codex(STREAM_CUT_RECOVERED_0_163);
    assert_eq!((r.phase, r.wall), (Phase::Idle, None));
    assert_eq!(
        r.said_tail.as_deref(),
        Some("Hi! I am the local lab stub, not a model. (reply 100)")
    );
}

/// THE QUESTION'S NOTES FIELD (0.163.0, measured; gap #11): Down Down puts
/// the focus on `› 3. None of the above` (no role an answer takes); Tab
/// opens the notes field under the options, `  › Add notes` while empty,
/// and the footer becomes `tab or esc to clear notes | enter to submit
/// answer`. The dialog with its field open is STILL the question box —
/// before, the user's message above it read as the composer, the session
/// idle mid-question — its field read empty, then holding the text typed.
/// Esc or Tab in the field clears and closes it, `None of the above` still
/// focused; Tab again reopens it empty. Enter submits the note with `None of
/// the above` (the turn then ends on the stub's reply); the digit 3 alone
/// answers at once, with no note. NEGATIVE CONTROL: a closed field reads
/// none, and no screen of the dialog has a composer.
#[test]
fn the_questions_notes_field_is_read_open_and_closed() {
    let none = anchor_text("codex.question.none");
    for (text, focus, notes) in [
        (QUESTION_0_163, "Short-term (Recommended)", None),
        (QUESTION_NONE_FOCUSED_0_163, none, None),
        (QUESTION_NOTES_OPEN_0_163, none, Some("")),
        (
            QUESTION_NOTES_TYPED_0_163,
            none,
            Some("use the long-term option"),
        ),
        (QUESTION_NOTES_REOPENED_0_163, none, Some("")),
        (QUESTION_NOTES_ESC_0_163, none, None),
        (QUESTION_NOTES_ESC_EMPTY_0_163, none, None),
        (QUESTION_NOTES_TAB_CLEARS_0_163, none, None),
    ] {
        let what = provenance(text).unwrap_or_default();
        let r = codex(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Prompt, true),
            "{what}"
        );
        assert_eq!(r.composer, None, "{what}");
        let p = r.prompt.expect(what);
        assert_eq!(p.kind, PromptKind::Question, "{what}");
        assert_eq!(
            p.description, "Which rollout should the migration use?",
            "{what}"
        );
        assert_eq!(
            roles(&p),
            [Role::Answer, Role::Answer, Role::Other],
            "{what}"
        );
        assert_eq!(p.focused().map(|o| o.label.as_str()), Some(focus), "{what}");
        let read = question_notes(&screen(text));
        assert_eq!(read.as_ref().map(|n| n.text.as_str()), notes, "{what}");
        if let Some(n) = read {
            assert!(screen(text)[n.row].starts_with("  › "), "{what}");
        }
        let footer = p.footer.unwrap_or_default();
        let open = anchor_text("codex.question.notes.open");
        let tab = anchor_text("codex.question.notes.tab");
        assert_eq!(
            footer.starts_with(open),
            notes.is_some(),
            "{what}: {footer}"
        );
        assert_eq!(footer.starts_with(tab), notes.is_none(), "{what}: {footer}");
    }
    for text in [QUESTION_NOTES_SUBMITTED_0_163, QUESTION_DIGIT_SUBMITS_0_163] {
        let r = codex(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true)
        );
        assert_eq!(question_notes(&screen(text)), None);
        assert!(
            r.said_tail
                .is_some_and(|s| s.starts_with("Hi! I am the local lab stub"))
        );
    }
    assert!(codex(QUESTION_INTERRUPTED_0_163).interrupted);
}

/// AN OWNER'S PROMPT RULE IS THE OWNER'S OWN REVIEW (gap #8, measured on
/// 0.163.0 with `prefix_rule(pattern=["touch"], decision="prompt")` in
/// `$CODEX_HOME/rules/`): its exec box's Reason row reads `` `/bin/zsh -lc
/// 'touch probe.txt'` requires approval by policy`` — or, with the rule's
/// justification, `` … requires approval: owner rule: touch needs a person``
/// — even over a call the model escalated with words of its own, and the box
/// offers two options, no `(p)`. Read as [`PromptV2::owner_review`], a
/// Codex prompt rule. NEGATIVE CONTROLS: a box the model escalated (its own
/// justification as the Reason row, or none) with no rule naming the command
/// is no owner's review and offers `(p)`; nor is 0.163.0's read-only
/// sandbox's ask; and a Reason row wrapped onto a second row is read whole.
#[test]
fn a_rule_prompted_exec_box_is_the_owners_review() {
    use crate::prompt::ReviewKind;
    for (text, why) in [
        (BOX_RULE_0_163, "requires approval by policy"),
        (
            BOX_RULE_JUSTIFIED_0_163,
            "requires approval: owner rule: touch needs a person",
        ),
        (
            BOX_RULE_OVER_ESCALATION_0_163,
            "requires approval by policy",
        ),
    ] {
        let what = provenance(text).unwrap_or_default();
        let p = codex(text).prompt.expect(what);
        assert_eq!(p.kind, PromptKind::Bash, "{what}");
        assert_eq!(p.command, "touch probe.txt", "{what}");
        assert_eq!(roles(&p), [Role::Once, Role::Deny], "{what}");
        let review = p.owner_review().expect(what);
        assert_eq!(review.kind, ReviewKind::CodexRule, "{what}");
        assert_eq!(
            review.text,
            format!("Reason: `/bin/zsh -lc 'touch probe.txt'` {why}"),
            "{what}"
        );
    }
    for (text, reason) in [
        (
            BOX_ESCALATED_0_163,
            "Do you want to create probe.txt outside the sandbox?",
        ),
        (BOX_ESCALATED_NO_REASON_0_163, ""),
        (
            BOX_ESCALATED_UNNAMED_0_163,
            "Do you want to create probe-dir outside the sandbox?",
        ),
        (BOX_EXEC_0_163, ""),
    ] {
        let what = provenance(text).unwrap_or_default();
        let p = codex(text).prompt.expect(what);
        assert_eq!(p.owner_review(), None, "{what}: the control");
        assert!(roles(&p).contains(&Role::Persist), "{what}");
        if !reason.is_empty() {
            assert_eq!(p.description, reason, "{what}");
        }
    }
    for text in [NO_RULE_NO_BOX_0_163, RULE_UNNAMED_NO_BOX_0_163] {
        assert_eq!(codex(text).prompt, None, "no box is drawn");
    }
    let rows = screen(BOX_RULE_0_163);
    let at = rows
        .iter()
        .position(|r| r.trim_start().starts_with("Reason: "))
        .expect("the reason row");
    assert!(rows[at + 1].trim().is_empty(), "a blank row under it");
    let mut wrapped = rows.clone();
    wrapped[at] = "  Reason: `/bin/zsh -lc 'touch probe.txt'` requires approval by".to_string();
    wrapped[at + 1] = "  policy".to_string();
    wrapped.insert(at + 2, String::new());
    wrapped.remove(0);
    let p = prompt(&wrapped).expect("the box");
    assert_eq!(
        p.owner_review().map(|r| r.kind),
        Some(ReviewKind::CodexRule),
        "{p:?}"
    );
}

/// THE 0.163.0 FOLDER GATE, SIGN-IN AND SLASH COMMANDS: the first run's
/// gate in an untrusted repository's subdirectory is the trust kind, chosen
/// by the cursor and Enter, the folder its first path row; the signed-out
/// first screen (`> 1. Sign in with ChatGPT`, a `>` cursor and `Press enter
/// to continue`) is no box and no evidence, so nothing acts on it; `/log`
/// typed shows the slash popup's match (`› /logout  log out of Codex`) over
/// the composer, and the fresh session stays idle with `/log` its draft —
/// read as a user message, the match made it busy. `/login` is no command
/// of 0.163.0 (`• Unrecognized command '/login'. …`); `/logout` leaves for
/// the shell, as `/exit` does. NEGATIVE CONTROL: the same match with an
/// ordinary draft under it is a message, and its turn runs.
#[test]
fn the_0_163_trust_gate_sign_in_and_slash_commands() {
    let p = codex(TRUST_0_163).prompt.expect("the gate");
    assert_eq!(p.kind, PromptKind::Trust);
    assert_eq!(p.select, Select::ArrowsEnter);
    assert_eq!(roles(&p), [Role::Trust, Role::Exit]);
    assert_eq!(p.path.as_deref(), Some("/tmp/at.LJ8G/proj/repo"));
    assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Exit));
    let r = codex(SIGN_IN_0_163);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, false));
    assert_eq!(r.prompt, None);
    let r = codex(SLASH_LOG_0_163);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(r.composer.map(|(_, l)| l), Some(vec!["/log".to_string()]));
    let r = codex(SLASH_LOGIN_0_163);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(r.composer.map(|(_, l)| l), Some(vec!["/login".to_string()]));
    for text in [LOGOUT_0_163, EXIT_0_163] {
        let r = codex(text);
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, false));
        assert_eq!(r.composer, None);
    }
    let typed: Vec<String> = screen(SLASH_LOG_0_163)
        .into_iter()
        .map(|r| {
            if r == "› /log" {
                "› log".to_string()
            } else {
                r
            }
        })
        .collect();
    assert_eq!(phase(&typed), (Phase::Busy, true), "the control");
}

/// CONTEXT AND COMPACTION ON 0.163.0: the pre-turn auto-compaction is a
/// running turn (`• Compacting context (0s • esc to interrupt)` over `└
/// Making room to continue.`); after it, `• Context compacted · 0s` is
/// Codex's own row, never the agent's last words; the warnings view (`f2`)
/// has no composer and is no evidence. The default footer draws no `%
/// context left` item at any fill; with `tui.status_line=[]` the key-hint
/// row draws its own, read at idle and mid-turn; a status line's `Context
/// 52% left` item and the `/status` card's `11% left` are no footer item.
/// NEGATIVE CONTROL: an agent row in the compaction row's place is read as
/// its words.
#[test]
fn compaction_rows_and_the_context_items_are_read() {
    assert_eq!(phase(&screen(COMPACTING_0_163)), (Phase::Busy, true));
    let r = codex(COMPACTED_0_163);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(
        r.said_tail.as_deref(),
        Some("Context-fill answer: input 257000 output 300.")
    );
    let rows = screen(COMPACTED_0_163);
    let answer = rows
        .iter()
        .rposition(|r| r.starts_with("• Context-fill answer"))
        .expect("the answer");
    let mut only = rows.clone();
    only.remove(answer);
    assert_eq!(said_tail(&only), None, "the compaction row is Codex's");
    let mut control = only.clone();
    let compacted = control
        .iter()
        .rposition(|r| r.starts_with("• Context compacted"))
        .expect("the compaction row");
    control[compacted] = "• Context is fine?".to_string();
    assert_eq!(
        said_tail(&control).as_deref(),
        Some("Context is fine?"),
        "the control"
    );
    assert_eq!(
        phase(&screen(COMPACTION_WARNING_0_163)),
        (Phase::Idle, false)
    );
    for (text, left) in [
        (CTX_DEFAULT_0_163, None),
        (CTX_BUSY_0_163, None),
        (COMPACTED_0_163, None),
        (CTX_STATUS_CARD_0_163, None),
        (CTX_STATUS_LINE_ITEMS_0_163, None),
        (CTX_LEFT_0_163, Some(99)),
        (CTX_LEFT_FULL_0_163, Some(0)),
        (CTX_LEFT_BUSY_0_163, Some(100)),
    ] {
        let what = provenance(text).unwrap_or_default();
        assert_eq!(context_left(&screen(text)), left, "{what}");
    }
    assert_eq!(phase(&screen(CTX_LEFT_BUSY_0_163)).0, Phase::Busy);
}

/// THE PICKERS, PERMISSIONS AND RESUME OF 0.163.0: `/model`'s model and
/// effort boxes are the model picker; the effort chosen for the session
/// leaves `• Model changed to …` under the last turn, the footer naming the
/// new model, and the turn's last words stay its answer; `/permissions` is a
/// box of no kind; its `Enable full access?` confirmation is a POPUP drawn
/// centred over the transcript, the composer still under it, and is a box —
/// read as none, its rows under the last end row made the session `busy`;
/// the line full access leaves (`• Permission selection requested: Full
/// Access`) is no turn; a resumed conversation reads idle at its last
/// answer. NEGATIVE CONTROL: the popup's rows with a transcript row under
/// them (no composer zone) are no box.
#[test]
fn the_0_163_pickers_permissions_and_resume_are_read() {
    for text in [MODEL_PICK_0_163, EFFORT_PICK_0_163] {
        assert_eq!(
            codex(text).prompt.map(|p| p.kind),
            Some(PromptKind::ModelPick)
        );
    }
    let r = codex(MODEL_CHANGED_0_163);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(
        footer_model(&screen(MODEL_CHANGED_0_163)),
        Some(("GPT-6-Astra".to_string(), Some("medium".to_string())))
    );
    assert_eq!(
        r.said_tail.as_deref(),
        Some("Hi! I am the local lab stub, not a model. (reply 2)")
    );
    for text in [
        PERMISSIONS_0_163,
        RESUMED_PERMISSIONS_0_163,
        RESUMED_NOFLAGS_PERMISSIONS_0_163,
    ] {
        let p = codex(text).prompt.expect("the menu");
        assert_eq!(p.kind, PromptKind::Other);
        assert!(roles(&p).iter().all(|r| *r == Role::Other), "{p:?}");
    }
    let r = codex(FULL_ACCESS_CONFIRM_0_163);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Prompt, true)
    );
    assert_eq!(r.composer, None);
    let p = r.prompt.expect("the confirmation");
    assert_eq!(p.kind, PromptKind::Other);
    assert_eq!(p.title, "Enable full access?");
    assert_eq!(labels(&p), ["Yes, continue anyway", "Cancel"]);
    assert!(roles(&p).iter().all(|r| *r == Role::Other), "{p:?}");
    let mut under = screen(FULL_ACCESS_CONFIRM_0_163);
    let caret = under
        .iter()
        .rposition(|r| r.starts_with("› Ask Codex"))
        .expect("the composer");
    under.insert(caret, "• a transcript row".to_string());
    assert_eq!(codex_box(&under), None, "the control");
    for text in [
        FULL_ACCESS_APPLIED_0_163,
        RESUMED_0_163,
        RESUMED_AFTER_TURN_0_163,
        CREDITS_IN_USE_0_163,
        STATUS_CHATGPT_0_163,
        STATUS_CHATGPT_CREDITS_IN_USE_0_163,
        STATUS_LAB_0_163,
    ] {
        let what = provenance(text).unwrap_or_default();
        let r = codex(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true),
            "{what}"
        );
        assert!(
            r.said_tail
                .as_deref()
                .is_some_and(|s| s.starts_with("Hi! I am the local lab stub")),
            "{what}: {:?}",
            r.said_tail
        );
    }
}

/// THE TOOL HEADS ARE 0.163.0'S: `• Viewed image <path>` (0.163.0's
/// spelling; the table said `Viewed Image`) and the notices it draws in a
/// turn are Codex's rows, never the agent's last words. NEGATIVE CONTROL:
/// the agent's own row in their place is.
#[test]
fn the_0_163_tool_heads_are_codexs_rows() {
    let ended = |row: &str| {
        rows(&[
            "› look at the image",
            "",
            row,
            "",
            "  Worked for 2s • 11:30 AM",
            "",
            "› Ask Codex to do anything",
            "",
            "  gpt · /x",
        ])
    };
    for row in [
        "• Viewed image /tmp/shot.png",
        "• Context compacted · 0s",
        "• Permission selection requested: Full Access",
        "• Unrecognized command '/login'. Type \"/\" for a list of supported commands.",
    ] {
        assert_eq!(phase(&ended(row)), (Phase::Idle, true), "{row}");
        assert_eq!(said_tail(&ended(row)), None, "{row}");
    }
    assert_eq!(
        said_tail(&ended("• Viewed the image: is it right?")).as_deref(),
        Some("Viewed the image: is it right?"),
        "the control"
    );
}

/// THE AGENT'S WORDS END AT CODEX'S OWN TIP, NOT AT ITS OWN `Tip:`: an
/// answer whose nested paragraph opens `Tip:` past column 2 (a list item's
/// continuation) is read whole, so the question it ends on is still read as
/// one — the stop is the `└ Tip:` Codex draws UNDER an answer (0.163.0,
/// measured) and a warning cell, never an indented row of the answer.
/// NEGATIVE CONTROL: the measured `└ Tip:` under the answer is no part of it.
#[test]
fn an_answers_own_indented_tip_is_its_words() {
    let ended = |tip: &str| {
        rows(&[
            "› plan the migration",
            "",
            "• Here is the plan:",
            "",
            "  - Run the migration first.",
            tip,
            "  Should I run it now?",
            "",
            "  Worked for 2s • 11:30 AM",
            "",
            "› Ask Codex to do anything",
            "",
            "  gpt · /x",
        ])
    };
    let own = ended("    Tip: back the table up before it.");
    assert_eq!(phase(&own), (Phase::Question, true));
    assert_eq!(
        said_tail(&own).as_deref(),
        Some(
            "Here is the plan:\n- Run the migration first.\nTip: back the table up before \
             it.\nShould I run it now?"
        )
    );
    let r = codex(STREAM_CUT_RECOVERED_0_163);
    assert_eq!(
        r.said_tail.as_deref(),
        Some("Hi! I am the local lab stub, not a model. (reply 100)"),
        "the control"
    );
}

/// The index of the first row of `rows` that starts with `prefix`.
fn row_at(rows: &[String], prefix: &str) -> usize {
    rows.iter()
        .position(|r| r.starts_with(prefix))
        .unwrap_or_else(|| panic!("no row {prefix:?}"))
}

/// WHERE THE TRANSCRIPT ENDS, by position, on the measured screens: at a
/// running turn's status row (0.156.1 and 0.163.0: `• Working (… esc to
/// interrupt)`), whatever hangs under it (0.158.0's `└ Tip:` under a status
/// row that carries the background terminals' tail) going with it; at a
/// box's title; at the composer once the turn ended — its end row or its `■`
/// block stays above the cut. NEGATIVE CONTROLS: the same busy screens with
/// the status row cleared end at the composer, and the background
/// terminals' line over an ended turn's composer is cut where the bare
/// composer is not.
#[test]
fn the_transcript_ends_where_the_live_zone_begins() {
    for (text, status) in [
        (BUSY, "• Working (6s"),
        (BUSY_0_163, "• Working (0s"),
        (GOAL_BUSY, "• Working (10m 57s"),
    ] {
        let rows = screen(text);
        let at = row_at(&rows, status);
        assert_eq!(transcript_end(&rows), at, "{status}");
        assert_eq!(CodexReader.transcript_end(&rows), at, "{status}");
        let mut quiet = rows.clone();
        quiet[at].clear();
        let c = composer(&quiet).expect("the composer");
        assert_eq!(transcript_end(&quiet), c, "{status}: the control");
    }
    for text in [BOX_EXEC_0_163, BOX_PATCH_0_163, QUESTION, BOX_EXEC] {
        let rows = screen(text);
        let p = prompt(&rows).expect("a box");
        assert_eq!(transcript_end(&rows), p.span.0, "{}", p.title);
        assert_eq!(rows[p.span.0].trim(), p.title);
    }
    for (text, ended) in [
        (END_OF_TURN_0_163, "  Worked for 1s • 7:14 AM"),
        (INTERRUPTED_0_163, "■ Conversation interrupted"),
        (END_OF_TURN, "  1:39 PM"),
    ] {
        let rows = screen(text);
        let c = composer(&rows).expect("the composer");
        assert_eq!(transcript_end(&rows), c, "{ended}");
        assert!(row_at(&rows, ended) < c, "{ended}");
    }
    let mut ended = screen(END_OF_TURN_0_163);
    let c = composer(&ended).expect("the composer");
    ended[c - 1] = "  1 background terminal running · /ps to view · /stop to close".to_string();
    assert_eq!(transcript_end(&ended), c - 1, "the status line");
    // Claude Code's reader cuts by its own frame: there is none on a Codex
    // screen, so nothing is cut.
    let rows = screen(BUSY_0_163);
    assert_eq!(
        ClaudeReader.transcript_end(&rows),
        rows.iter()
            .rposition(|r| !r.trim().is_empty())
            .expect("drawn")
            + 1
    );
}

/// The heads of Codex's transcript, read on measured screens (0.156.1 and
/// 0.163.0): `›` a person's message; a `•` block the agent's words, or a
/// tool or a notice (by its head, or by the `└` output under it); `✔`/`✗`
/// an approval's echo; an end row and a `■` block what ended a turn; a
/// status row a turn still running, and so does an EMPTY composer (its mark
/// and placeholder alone: the live zone wherever a frame left it). A
/// message's wrapped rows, a tool's output and its diff rows open nothing.
/// NEGATIVE CONTROLS: a `└ Tip:` under a message (measured on 0.163.0) leaves
/// it the agent's words, where `└` output under the same row makes it a
/// tool's; and a composer holding a draft reads as a person's message — why a
/// report cuts the live zone first ([`transcript_end`]).
#[test]
fn the_transcript_heads_are_read_by_their_column_zero_marks() {
    let heads = |rows: &[String]| -> Vec<(String, Head)> {
        (0..rows.len())
            .filter_map(|i| {
                transcript_head(rows, i).map(|h| (rows[i].chars().take(24).collect(), h))
            })
            .collect()
    };
    let cut = |text: &str| {
        let rows = screen(text);
        let end = transcript_end(&rows);
        rows[..end].to_vec()
    };
    let want = |v: &[(&str, Head)]| -> Vec<(String, Head)> {
        v.iter()
            .map(|(r, h)| (r.chars().take(24).collect(), *h))
            .collect()
    };
    let end_of_turn = cut(END_OF_TURN);
    let from = row_at(&end_of_turn, "› Fix the bug");
    assert_eq!(
        heads(&end_of_turn[from..]),
        want(&[
            ("› Fix the bug in calc.py", Head::User),
            ("• I’ll inspect calc.py an", Head::Said),
            ("• Explored", Head::Tool),
            ("• Edited calc.py (+1 -1)", Head::Tool),
            ("• Patched calc.py so add", Head::Said),
            ("  1:38 PM", Head::End),
            ("› Write a 120-word paragr", Head::User),
            ("• Tests matter because th", Head::Said),
            ("  1:39 PM", Head::End),
        ])
    );
    let busy = cut(BUSY);
    let from = row_at(&busy, "✔ You approved");
    assert_eq!(
        heads(&busy[from..]),
        want(&[
            ("✔ You approved codex to r", Head::Tool),
            ("• Ran touch made-by-codex", Head::Tool),
            ("• Created made-by-codex.t", Head::Said),
            ("  1:37 PM", Head::End),
            ("› Fix the bug in calc.py ", Head::User),
            ("• I’ll inspect calc.py an", Head::Said),
            ("• Explored", Head::Tool),
        ])
    );
    let interrupted = cut(INTERRUPTED_0_163);
    let from = row_at(&interrupted, "› Reply with");
    assert_eq!(
        heads(&interrupted[from..]),
        want(&[
            ("› Reply with exactly the ", Head::User),
            ("• hi", Head::Said),
            ("  Worked for 1s • 7:14 AM", Head::End),
            ("› Run this shell command ", Head::User),
            ("✗ You canceled the reques", Head::Tool),
            ("• Failed (exit 1) touch p", Head::Tool),
            ("■ Conversation interrupte", Head::End),
        ])
    );
    // The session card, boxed (0.156.1) or not (0.163.0: its title over its
    // folder): a card, whose greeting is Codex's. NEGATIVE CONTROL: the title
    // quoted with no folder row under it opens nothing.
    for text in [IDLE, IDLE_0_163] {
        let card = screen(text);
        let at = card
            .iter()
            .position(|r| r.starts_with('╭') || r.starts_with("  >_ OpenAI Codex"))
            .expect("the card");
        assert_eq!(transcript_head(&card, at), Some(Head::Tool), "{}", card[at]);
    }
    let quoted = rows(&[
        "  >_ OpenAI Codex (v0.163.0) is the build this folder runs.",
        "  Its card names the folder too.",
    ]);
    assert_eq!(transcript_head(&quoted, 0), None);
    // The archive's lone title over the card (measured in the lab on 0.163.0):
    // a card too, whole; with words after it, none.
    let archived = rows(&[
        "  >_ OpenAI Codex (v0.163.0)",
        "  >_ OpenAI Codex (v0.163.0)",
        "     /private/tmp/at.wZPe/proj",
    ]);
    assert_eq!(transcript_head(&archived, 0), Some(Head::Tool));
    assert_eq!(transcript_head(&archived, 1), Some(Head::Tool));
    let said = rows(&[
        "  >_ OpenAI Codex (v0.163.0) here",
        "  >_ OpenAI Codex (v0.163.0) there",
    ]);
    assert_eq!(transcript_head(&said, 0), None);
    // The status row, read whole (the live zone not cut).
    let busy = screen(BUSY_0_163);
    assert_eq!(
        transcript_head(&busy, row_at(&busy, "• Working (")),
        Some(Head::Status)
    );
    // The controls.
    let tip = rows(&[
        "• Hi! I am the local lab stub, not a model. (reply 100)",
        "  └ Tip: Run /review to get a code review of your current changes.",
    ]);
    assert_eq!(transcript_head(&tip, 0), Some(Head::Said));
    let output = rows(&["• Hi! I am the local lab stub", "  └ (no output)"]);
    assert_eq!(transcript_head(&output, 0), Some(Head::Tool));
    let mut whole = screen(END_OF_TURN_0_163);
    let c = composer(&whole).expect("the composer");
    assert_eq!(transcript_head(&whole, c), Some(Head::Status));
    whole[c] = "› Fix the parser".to_string();
    assert_eq!(transcript_head(&whole, c), Some(Head::User));
    assert!(is_empty_composer("» Ask Codex to do anything"));
    assert!(!is_empty_composer(
        "› Ask Codex to do anything about the parser"
    ));
}

/// 0.163.0 IN THE LAB (2026-10-16: the real Codex on the alternate screen,
/// the lab's stub of the Responses API, the harness off): Plan mode's
/// question is a question box read by its answers' roles, its Esc
/// interrupting the turn; answered by `aterm drive answer`'s digit 2, the turn
/// ends idle on the stub's answer — the `└ Tip:` row Codex hung under it is
/// neither a tool's output nor the agent's words — and dismissed with Esc it
/// ends on `■ Conversation interrupted`, the person's. NEGATIVE CONTROLS: the
/// same answer with `└` output under it is a tool row, with no last words;
/// and the answer turned into a question ends the turn on a question only
/// because the tip is not read as output (it read `idle` before).
#[test]
fn the_0_163_question_in_the_lab_is_asked_answered_and_interrupted() {
    let r = codex(QUESTION_LAB_0_163);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    let p = r.prompt.expect("the question");
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(p.description, "Which rollout should the migration use?");
    assert_eq!(
        labels(&p),
        ["Short-term (Recommended)", "Long-term", "None of the above"]
    );
    assert_eq!(roles(&p), [Role::Answer, Role::Answer, Role::Other]);
    assert_eq!(
        p.cancel.map(|c| c.effect),
        Some(crate::prompt::CancelEffect::Interrupt)
    );

    let answered = codex(QUESTION_ANSWERED_LAB_0_163);
    assert_eq!(
        (answered.phase.clone(), answered.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(
        answered.said_tail.as_deref(),
        Some("Hi! I am the local lab stub, not a model. (reply 4)")
    );
    let rows = screen(QUESTION_ANSWERED_LAB_0_163);
    let tip = row_at(&rows, "  └ Tip:");
    let mut asked = rows.clone();
    let reply = asked
        .iter()
        .rposition(|r| r.starts_with("• Hi! I am the local lab stub"))
        .expect("the answer");
    asked[reply] = "• Shall I write the migration plan now?".to_string();
    assert_eq!(phase(&asked), (Phase::Question, true));
    let mut tool = rows.clone();
    tool[tip] = "  └ (no output)".to_string();
    assert_eq!(said_tail(&tool), None, "the control: output under it");

    let esc = codex(QUESTION_ESC_LAB_0_163);
    assert!(esc.interrupted);
    assert_eq!((esc.phase, esc.phase_authoritative), (Phase::Idle, true));
    assert_eq!(esc.wall, None);
}

/// CODEX PINS THE PROMPT of the turn it shows on the alternate screen's top
/// row once that prompt's own row has scrolled off (0.163.0 in the lab,
/// 2026-10-16: a reply of 70 streamed paragraphs, `› Write seventy paragraphs
/// and ask me a question.` drawn over paragraph 51). That copy is no head of
/// the answer under it: the answer's head scrolled away, and its last words
/// are its visible rows — a reply that ends on a question is a question.
/// Read as the answer's head, the copy (no `•` block) gave no last words, and
/// a screen read whole (a pane short enough that the tail read reaches the
/// top row) read `idle` under the question. NEGATIVE CONTROL: the same screen
/// with its top row cut, as a 40-row tail read of a 50-row pane sees it,
/// reads the same; and the reply that ends on no question is idle.
#[test]
fn a_prompt_pinned_over_the_answer_is_no_head_of_it() {
    let rows = screen(PINNED_QUESTION_LAB_0_163);
    assert!(
        rows[0].starts_with("› Write seventy paragraphs"),
        "the pinned row"
    );
    assert_eq!(phase(&rows), (Phase::Question, true));
    let said = said_tail(&rows).expect("the answer's last words");
    assert!(said.starts_with("Streamed answer paragraph 51,"), "{said}");
    assert!(
        said.ends_with("Shall I write the migration plan now?"),
        "{said}"
    );
    assert_eq!(phase(&rows[1..]), (Phase::Question, true), "the tail read");
    assert_eq!(said_tail(&rows[1..]).as_deref(), Some(said.as_str()));

    let rows = screen(PINNED_PROMPT_LAB_0_163);
    assert_eq!(phase(&rows), (Phase::Idle, true));
    assert!(
        said_tail(&rows)
            .is_some_and(|s| s.ends_with("paragraph 70 of a reply taller than the screen.")),
        "{:?}",
        said_tail(&rows)
    );
}
