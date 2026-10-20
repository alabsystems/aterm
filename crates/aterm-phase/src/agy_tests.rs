// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One test per measured agy screen (agy 1.4.0), each with its negative
//! control.

use super::fixtures::*;
use super::*;
use crate::prompt::fixtures::{cursor, provenance, rows, screen};
use crate::reader::{AgyReader, ClaudeReader, CodexReader, Program, ScreenReader, identify, read};
use crate::wall::WallKind;

fn agy(text: &str) -> crate::reader::Reading {
    read(Some("agy"), &screen(text), None)
}

fn roles(p: &PromptV2) -> Vec<Role> {
    p.options.iter().map(|o| o.role).collect()
}

fn labels(p: &PromptV2) -> Vec<&str> {
    p.options.iter().map(|o| o.label.as_str()).collect()
}

/// `rows` with the row that starts with `head` replaced by `with`.
fn edited(rows: &[String], head: &str, with: &str) -> Vec<String> {
    let at = rows
        .iter()
        .rposition(|r| r.starts_with(head))
        .unwrap_or_else(|| panic!("a row starting {head:?}"));
    let mut out = rows.to_vec();
    out[at] = with.to_string();
    out
}

/// The screens agy LEFT — its last frame, its resume line and the shell
/// under them — signed in and in the lab (one ended by SIGTERM with its
/// background work running, the panel's rows in its last frame): nothing on
/// them is agy's to read.
fn left(text: &str) -> bool {
    [EXITED, lab::EXIT, lab::BG_EXITED].contains(&text)
}

/// Every agy capture under `src/fixtures/`, by file name.
fn captures() -> Vec<(String, String)> {
    crate::text::fixture_files()
        .into_iter()
        .filter(|(name, _)| name.starts_with("agy-"))
        .collect()
}

/// A fresh session at its composer — after the notices were dismissed, at
/// 100 columns, and right after the trust box with the shell's echo of agy's
/// terminal queries on the row under the launch line and a dismissable
/// card over the composer — is idle as evidence: no message yet, the footer
/// `? for shortcuts`. The composer is the caret and NOTHING (no
/// placeholder), on the row the cursor parks on, at column 2 (measured).
/// NEGATIVE CONTROL: the busy screen's composer is the same empty caret row,
/// and the phase is busy.
#[test]
fn a_fresh_session_is_idle_at_its_composer() {
    for text in [IDLE, IDLE_100X30, IDLE_FRESH_NOTICE] {
        let r = agy(text);
        let what = provenance(text).unwrap_or_default();
        assert_eq!(r.program, Program::Agy);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true),
            "{what}"
        );
        assert!(r.fresh, "{what}");
        assert_eq!(
            (r.prompt, r.wall, r.said_tail),
            (None, None, None),
            "{what}"
        );
        assert!(!r.interrupted, "{what}");
        let (row, col) = cursor(text).expect("measured");
        assert_eq!(r.composer, Some((row, vec![String::new()])), "{what}");
        assert_eq!(col, 2, "{what}");
    }
    let busy = agy(BUSY);
    assert_eq!(busy.phase, Phase::Busy, "the control");
    assert_eq!(busy.composer.map(|(_, l)| l), Some(vec![String::new()]));
    assert!(!busy.fresh, "a message was sent");
}

/// THE SPINNER ROW OVER THE OPEN COMPOSER IS BUSY, the tip under it or not;
/// the turn has no last words yet. `esc to cancel` alone is NOT a turn
/// running (a menu and a box draw it too): with the spinner row gone it is
/// busy without evidence. NEGATIVE CONTROL: the spinner gone and the footer
/// back to `? for shortcuts` is the turn's end, as evidence.
#[test]
fn the_spinner_row_is_busy_and_esc_to_cancel_alone_is_not() {
    for text in [BUSY, BUSY_TIP] {
        let r = agy(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Busy, true)
        );
        assert_eq!(r.said_tail, None, "no turn has ended");
        assert_eq!(r.composer.map(|(_, l)| l), Some(vec![String::new()]));
    }
    let rows = screen(BUSY);
    let spinner = rows
        .iter()
        .position(|r| is_spinner_row(r))
        .expect("the spinner row");
    let mut gone = rows.clone();
    gone[spinner] = String::new();
    assert_eq!(phase(&gone), (Phase::Busy, false));
    let ended = edited(&gone, "esc to cancel", "? for shortcuts");
    assert_eq!(phase(&ended), (Phase::Idle, true), "the control");

    // Under the tip: with no spinner row in the turn, the tip is no turn
    // running.
    let tip = screen(BUSY_TIP);
    let at = tip.iter().position(|r| is_spinner_row(r)).expect("spinner");
    let mut said = tip.clone();
    said[at] = "  of the planet.".to_string();
    assert_eq!(phase(&said), (Phase::Busy, false));
}

/// A turn ends on its reply, with no row marking the end: the spinner gone
/// and `? for shortcuts` back. Its last words are the reply's paragraphs,
/// the indent stripped. NEGATIVE CONTROLS: a reply whose last words ask is a
/// question; the same rows with the spinner over the composer are a turn
/// still running, with no last words.
#[test]
fn a_turn_ends_on_its_reply() {
    let r = agy(END_OF_TURN);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    let said = r.said_tail.expect("the reply");
    assert!(said.starts_with("Ocean tides—the rhythmic"), "{said}");
    assert!(said.ends_with("dampen these global rhythms."), "{said}");
    assert!(!r.fresh && !r.interrupted);

    let rows = screen(END_OF_TURN);
    let asks = edited(
        &rows,
        "  and ocean floor depth",
        "  and ocean floor depth. Shall I add a diagram?",
    );
    assert_eq!(phase(&asks), (Phase::Question, true));
    let top = rows
        .iter()
        .rposition(|r| plain_rule_width(r).is_some())
        .and_then(|bottom| {
            rows[..bottom]
                .iter()
                .rposition(|r| plain_rule_width(r).is_some())
        })
        .expect("the composer's top rule");
    let mut running = rows.clone();
    running[top - 1] = "⣟  Generating...".to_string();
    let running = edited(&running, "? for shortcuts", "esc to cancel");
    assert_eq!(phase(&running), (Phase::Busy, true));
    assert_eq!(said_tail(&running), None);
}

/// Esc in a turn, and each box refused (`No, cancel`, `No, deny creation`,
/// `No, reject this change`), end the turn on agy's `⎿  Interrupted · What
/// should Antigravity CLI do instead?` — idle as evidence, and the PERSON's:
/// the next message is theirs. The vendor's question is no last words.
/// NEGATIVE CONTROLS: a turn that ended by itself is not interrupted; nor is
/// one whose interrupt a newer message answered, nor a reply that says the
/// words.
#[test]
fn an_interrupted_turn_is_the_persons() {
    for text in [INTERRUPTED, CANCELLED, DECLINED, REJECTED] {
        let r = agy(text);
        let what = provenance(text).unwrap_or_default();
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true),
            "{what}"
        );
        assert!(r.interrupted, "{what}");
        assert_eq!((r.said_tail, r.wall), (None, None), "{what}");
    }
    for text in [CANCELLED, DECLINED, REJECTED] {
        assert!(
            screen(text)
                .iter()
                .any(|r| r.contains(anchor_text("agy.turn.interrupted"))),
            "the box's refusal ends on the same row"
        );
    }
    assert!(!agy(END_OF_TURN).interrupted, "the control");

    let rows = screen(INTERRUPTED);
    let at = rows
        .iter()
        .position(|r| r.contains("⎿  Interrupted"))
        .expect("the interrupt row");
    let mut answered = rows.clone();
    answered.splice(
        at + 1..at + 1,
        [
            "─".repeat(60),
            "> Go on, but shorter.".to_string(),
            String::new(),
            "  Lighthouses guided ships for centuries.".to_string(),
            String::new(),
        ],
    );
    assert!(!interrupted(&answered), "a newer message answered it");
    assert_eq!(
        said_tail(&answered).as_deref(),
        Some("Lighthouses guided ships for centuries.")
    );
    let mut quoted = rows.clone();
    quoted[at] = "  Interrupted · What should Antigravity CLI do instead?".to_string();
    assert!(!interrupted(&quoted), "the words in a reply");
}

/// A turn that failed ends on `⚠ <message>` and `Error ID: <id>`, agy's
/// own rows: idle as evidence, NO wall (no kind fits it: the one measured
/// came of a dead proxy and names none), no last words of the agent, no
/// interrupt. The footer names no model while the model list is unknown, and
/// still reads. NEGATIVE CONTROL: a reply under the error is the turn's last
/// words.
#[test]
fn a_failed_turn_is_an_ended_turn_with_no_wall() {
    let r = agy(ERROR_TURN);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert_eq!((r.wall, r.said_tail), (None, None));
    assert!(!r.interrupted);
    let rows = screen(ERROR_TURN);
    assert_eq!(rows.last().map(String::as_str), Some("? for shortcuts"));
    let at = rows
        .iter()
        .position(|r| r.starts_with("Error ID:"))
        .expect("the error id");
    let mut replied = rows.clone();
    replied.splice(
        at + 1..at + 1,
        [String::new(), "  Retrying now.".to_string()],
    );
    assert_eq!(said_tail(&replied).as_deref(), Some("Retrying now."));
}

/// A typed draft is the composer's text, the cursor after it (measured), and
/// the footer's left side BLANK while it is held: idle as evidence, and the
/// session still fresh. NEGATIVE CONTROL: the same blank footer with the
/// composer empty says nothing.
#[test]
fn an_unsent_draft_is_the_composers() {
    let r = agy(DRAFT);
    let (row, col) = cursor(DRAFT).expect("measured");
    assert_eq!(r.composer, Some((row, vec!["hello there".to_string()])));
    assert_eq!(col, 2 + "hello there".len());
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert!(r.fresh);
    let cleared = edited(&screen(DRAFT), "> hello there", ">");
    assert_eq!(phase(&cleared), (Phase::Idle, false));
}

/// The shell-command box, at 120 and 100 columns: a Bash prompt for the
/// command under `Requesting permission for:`, its four options by their
/// roles — the one-shot run, the conversation's grant, the grant persisted
/// to settings.json, the refusal — the first focused, chosen by digit. Its
/// Esc is the turn's `esc to cancel`: it stops the turn and refuses nothing.
/// The box replaces the composer, and no wall is read under it. NEGATIVE
/// CONTROLS: a second cursor, or a gap in the numbering, and no option
/// carries a role.
#[test]
fn the_exec_box_is_a_bash_prompt_with_its_roles() {
    for (text, command) in [(BOX_EXEC, "touch x.txt"), (BOX_EXEC_100X30, "ls -la")] {
        let r = agy(text);
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
        assert_eq!((r.composer, r.wall), (None, None));
        let p = r.prompt.expect("the box");
        assert_eq!(p.kind, PromptKind::Bash);
        assert_eq!(p.title, anchor_text("agy.title.command"));
        assert_eq!(p.command, command);
        assert_eq!(p.readings(), vec![command.to_string()]);
        assert_eq!(
            roles(&p),
            vec![Role::Once, Role::Session, Role::Persist, Role::Deny]
        );
        assert_eq!(labels(&p)[0], anchor_text("agy.box.once"));
        assert_eq!(p.focused().map(|o| o.n), Some(Some(1)));
        assert_eq!(p.select, Select::Digits);
        assert_eq!(
            p.question.map(|q| screen(text)[q].clone()),
            Some("Run this command?".to_string())
        );
        let cancel = p.cancel.expect("esc");
        assert_eq!(cancel.effect, CancelEffect::Interrupt);
    }
    let rows = screen(BOX_EXEC);
    let two = rows
        .iter()
        .position(|r| r.starts_with("  2. "))
        .expect("option 2");
    let mut cursors = rows.clone();
    cursors[two] = rows[two].replacen("  2.", "> 2.", 1);
    let p = prompt(&cursors).expect("still a box");
    assert!(
        roles(&p).iter().all(|&r| r == Role::Other),
        "{:?}",
        roles(&p)
    );
    let mut gap = rows.clone();
    gap[two] = rows[two].replacen("2.", "5.", 1);
    let p = prompt(&gap).expect("still a box");
    assert!(
        roles(&p).iter().all(|&r| r == Role::Other),
        "{:?}",
        roles(&p)
    );
}

/// A box whose title scrolled off the rows read is named by nothing (an
/// Other prompt, its head off the screen), and its options take the approval
/// boxes' roles where one of their one-shot allows is among them — `Yes, run
/// command` here. NEGATIVE CONTROL: with no one-shot allow among them, no
/// option carries a role.
#[test]
fn a_box_whose_title_scrolled_off_is_named_by_its_one_shot_allow() {
    let rows = screen(BOX_EXEC);
    let at = rows
        .iter()
        .position(|r| r == "Requesting permission for:")
        .expect("the body");
    let cut = rows[at..].to_vec();
    assert_eq!(phase(&cut), (Phase::Prompt, true));
    let p = prompt(&cut).expect("still a box");
    assert_eq!((p.kind, p.head_off_screen), (PromptKind::Other, true));
    assert_eq!(
        roles(&p),
        vec![Role::Once, Role::Session, Role::Persist, Role::Deny]
    );
    let unnamed = edited(&cut, "> 1. Yes, run command", "> 1. Yes, run it");
    let p = prompt(&unnamed).expect("still a box");
    assert!(
        roles(&p).iter().all(|&r| r == Role::Other),
        "{:?}",
        roles(&p)
    );
}

/// A tool row's own output, drawn right under its `●` row, is no words of
/// the agent: a turn that ends on it has no last words. NEGATIVE CONTROL:
/// the same rows with a blank row between the tool row and them are the
/// agent's reply.
#[test]
fn a_tool_rows_output_is_no_last_words() {
    let rows = screen(END_OF_TURN);
    let user = rows
        .iter()
        .position(|r| r.starts_with("> Write about 300 words"))
        .expect("the message");
    let mut output = rows.clone();
    output.insert(user + 2, "● Ran (ls -la) (ctrl+o to expand)".to_string());
    assert_eq!(phase(&output), (Phase::Idle, true));
    assert_eq!(said_tail(&output), None);
    let mut replied = output.clone();
    replied.insert(user + 3, String::new());
    let said = said_tail(&replied).expect("the reply");
    assert!(said.starts_with("Ocean tides—the rhythmic"), "{said}");
}

/// A long option WRAPS with its tail at column 0 of the next row (measured):
/// the tail is the option above's, so `(Persist to settings.json)` closes
/// option 3's label, and option 4 is still `No, cancel`. NEGATIVE CONTROL:
/// with the tail row gone, option 3's label stops at `(Persist to` — the
/// join is what restores it.
#[test]
fn a_wrapped_options_tail_belongs_to_the_option_above() {
    let p = agy(BOX_EXEC_WRAPPED).prompt.expect("the box");
    assert_eq!(p.options.len(), 4);
    assert_eq!(
        roles(&p),
        vec![Role::Once, Role::Session, Role::Persist, Role::Deny]
    );
    assert!(
        labels(&p)[2]
            .ends_with("'env | cut -d= -f1 | sort > envnames.txt' (Persist to settings.json)"),
        "{}",
        labels(&p)[2]
    );
    assert_eq!(labels(&p)[3], "No, cancel");
    assert_eq!(p.command, "env | cut -d= -f1 | sort > envnames.txt");
    let rows = screen(BOX_EXEC_WRAPPED);
    let tail = rows
        .iter()
        .position(|r| r == "settings.json)")
        .expect("the tail");
    let mut cut = rows.clone();
    cut.remove(tail);
    let p = prompt(&cut).expect("the box");
    assert!(labels(&p)[2].ends_with("(Persist to"), "{}", labels(&p)[2]);
}

/// The file boxes: `Create file` is a Write prompt for its path, with
/// `Reason: outside workspace` as its description and the persisting grant
/// `Yes, and always allow non-workspace access` beside the one-shot and the
/// refusal; inside the workspace it offers the two alone. `Pending edit` is
/// an Edit prompt for its path, and its `shift+tab to auto-approve file
/// edits` hint is no option. NEGATIVE CONTROL: Claude Code's reader sees no
/// box on any of them.
#[test]
fn the_file_boxes_are_write_and_edit_prompts() {
    let path = "/private/tmp/at.XXXXXX/work/x.txt";
    for (text, kind, want, reason) in [
        (
            BOX_CREATE,
            PromptKind::Write,
            vec![Role::Once, Role::Persist, Role::Deny],
            "outside workspace",
        ),
        (
            BOX_CREATE_WORKSPACE,
            PromptKind::Write,
            vec![Role::Once, Role::Deny],
            "",
        ),
        (BOX_EDIT, PromptKind::Edit, vec![Role::Once, Role::Deny], ""),
    ] {
        let p = agy(text).prompt.expect("the box");
        let what = provenance(text).unwrap_or_default();
        assert_eq!(p.kind, kind, "{what}");
        assert_eq!((p.path.as_deref(), p.command.as_str()), (Some(path), path));
        assert_eq!(p.description, reason, "{what}");
        assert_eq!(roles(&p), want, "{what}");
        assert_eq!(ClaudeReader.prompt(&screen(text)), None, "the control");
    }
    let p = agy(BOX_CREATE).prompt.expect("the box");
    assert_eq!(labels(&p)[1], anchor_text("agy.create.persist"));
    let p = agy(BOX_EDIT).prompt.expect("the box");
    assert!(labels(&p).iter().all(|l| !l.contains("shift+tab")));
    assert_eq!(p.options.len(), 2);
}

/// agy's ask-the-user tool: a Question prompt, its question the text after
/// `Question 1/1:`, the model's answers [`Role::Answer`] and `Write-in...`
/// (the person's own words) [`Role::Other`], none recommended, chosen by
/// digit; its key row's `esc Skip` leaves it unanswered. NEGATIVE CONTROLS:
/// Claude Code's reader sees no box on it, and once answered the `? …` and
/// `> Blue` rows it leaves are transcript — no box, and the reply under them
/// is the turn's last words.
#[test]
fn the_question_tool_is_a_question_with_answers() {
    let r = agy(QUESTION);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    let p = r.prompt.expect("the question");
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(
        p.description,
        "Which color would you like to choose for the logo?"
    );
    assert_eq!(labels(&p), vec!["Red", "Green", "Blue", "Write-in..."]);
    assert_eq!(
        roles(&p),
        vec![Role::Answer, Role::Answer, Role::Answer, Role::Other]
    );
    assert_eq!(p.recommended(), None);
    assert_eq!(p.select, Select::Digits);
    let cancel = p.cancel.expect("esc");
    assert_eq!(
        (cancel.verb.as_str(), cancel.effect),
        ("skip", CancelEffect::Back)
    );
    assert_ne!(ClaudeReader.phase(&screen(QUESTION)), Phase::Prompt);

    let answered = agy(EXIT_CONFIRM);
    assert_eq!(answered.prompt, None);
    assert_eq!(
        answered.said_tail.as_deref(),
        Some("You selected Blue for the logo.")
    );
}

/// The trust box agy draws in a new folder: a Trust prompt for its folder,
/// its two options UNNUMBERED — `Yes, I trust this folder` the trust, `No,
/// exit` the exit — the first focused, chosen by the cursor and Enter (no
/// digit names them); no Esc offered. NEGATIVE CONTROLS: both rows focused
/// and no option carries a role; Claude Code's reader, whose own trust
/// dialog says the same words in another frame, sees no box on it.
#[test]
fn the_trust_box_is_a_trust_prompt_chosen_by_enter() {
    let r = agy(BOX_TRUST);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    let p = r.prompt.expect("the box");
    assert_eq!(p.kind, PromptKind::Trust);
    assert_eq!(p.title, "Accessing workspace:");
    assert_eq!(p.path.as_deref(), Some("/tmp/at.XXXXXX/work"));
    assert_eq!(
        labels(&p),
        vec![anchor_text("agy.trust.yes"), anchor_text("agy.trust.no")]
    );
    assert_eq!(roles(&p), vec![Role::Trust, Role::Exit]);
    assert!(p.options.iter().all(|o| o.n.is_none()));
    assert!(p.options[0].focused && !p.options[1].focused);
    assert_eq!(p.select, Select::ArrowsEnter);
    assert_eq!(p.cancel, None);

    let both = edited(&screen(BOX_TRUST), "  No, exit", "> No, exit");
    let p = prompt(&both).expect("still a box");
    assert!(roles(&p).iter().all(|&r| r == Role::Other));
    assert_eq!(ClaudeReader.prompt(&screen(BOX_TRUST)), None, "the control");
}

/// `/` opens the command menu UNDER the composer, and the footer reads `esc
/// to cancel` with no turn running: the composer is still the composer, its
/// draft the `/` (the cursor after it, measured) — the menu scrolled, an `↑
/// N more` over its rows, too — and no box is up, but nothing is evidence,
/// the menu taking the footer's place. NEGATIVE CONTROLS: a row of no shape
/// the menu draws under the composer leaves no composer read; the menu
/// closed and `? for shortcuts` back is idle as evidence.
#[test]
fn the_command_menu_is_no_box_and_no_evidence() {
    let r = agy(SLASH_MENU);
    let (row, col) = cursor(SLASH_MENU).expect("measured");
    assert_eq!(r.composer, Some((row, vec!["/".to_string()])));
    assert_eq!(col, 3);
    assert_eq!(r.prompt, None);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, false));
    let rows = screen(SLASH_MENU);
    let bottom = row + 1;
    let mut scrolled = rows.clone();
    scrolled.insert(bottom + 1, "   ↑ 1 more".to_string());
    assert_eq!(
        composer_draft(&scrolled),
        Some((row, vec!["/".to_string()]))
    );
    assert_eq!(phase(&scrolled), (Phase::Idle, false));
    scrolled[bottom + 1] = "   1 more above".to_string();
    assert_eq!(composer_draft(&scrolled), None, "the control");
    let mut closed = rows[..=bottom].to_vec();
    closed.push(
        rows.last()
            .expect("the footer")
            .replacen("esc to cancel", "? for shortcuts", 1),
    );
    closed[row] = ">".to_string();
    assert_eq!(phase(&closed), (Phase::Idle, true));
}

/// AN EXIT PENDING IS IDLE BUT NEVER EVIDENCE: one ctrl+c (signed in) or one
/// ctrl+d (the lab) at the empty composer turns the footer's left side into
/// `press <key> again to exit`, that key again ends agy, and the hint does
/// not time out (each stood 45 s, measured): agy is still at its composer —
/// empty, the cursor on its caret row, its last words read, its layout
/// naming it — but idle NOT as evidence, so nothing is typed into an agy
/// about to exit. The footer is read by the binary's template (`press %s
/// again to exit`): any one key word, the model's name after a gap.
/// NEGATIVE CONTROLS: the same screens with the footer back to `? for
/// shortcuts` are idle as evidence; `press` with no key, another verb, the
/// words run on, and the vendor's other capitalisation are no exit pending.
#[test]
fn an_exit_pending_is_idle_but_never_evidence() {
    for (text, hint, said) in [
        (
            EXIT_CONFIRM,
            "agy.footer.exit",
            "You selected Blue for the logo.",
        ),
        (
            lab::EXIT_CONFIRM_CTRL_D,
            "agy.footer.exit_ctrl_d",
            "Hi! I am the local lab stub, not a model. (reply 2)",
        ),
    ] {
        let what = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let footer = rows.iter().rposition(|r| !blank(r)).expect("the footer");
        assert!(rows[footer].starts_with(anchor_text(hint)), "{what}");
        assert_eq!(footer_of(&rows[footer]), Footer::ExitPending, "{what}");
        let r = agy(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, false),
            "{what}"
        );
        let (row, col) = cursor(text).expect("measured");
        assert_eq!(r.composer, Some((row, vec![String::new()])), "{what}");
        assert_eq!(col, 2, "{what}");
        assert_eq!(r.said_tail.as_deref(), Some(said), "{what}");
        assert_eq!((r.prompt, r.wall), (None, None), "{what}");
        assert!(!r.fresh, "{what}");
        assert_eq!(identify(None, &rows).program(), Program::Agy, "{what}");
        let mut back = rows.clone();
        back[footer] = rows[footer].replacen(anchor_text(hint), anchor_text("agy.footer.idle"), 1);
        assert_eq!(phase(&back), (Phase::Idle, true), "the control: {what}");
    }
    for (row, pending) in [
        ("press ctrl+d again to exit", true),
        (
            "press ctrl+c again to exit                 Gemini 4 Argon · medium",
            true,
        ),
        ("press esc again to exit Gemini 3.1 Pro · low", true),
        ("press  again to exit", false),
        ("press ctrl+d twice to exit", false),
        ("press ctrl+d again to exited", false),
        ("Press ctrl+d again to exit", false),
        ("press ctrl+d", false),
    ] {
        assert_eq!(footer_of(row) == Footer::ExitPending, pending, "{row:?}");
    }
}

/// ONE CTRL+C OVER A HELD DRAFT ARMS THE EXIT TOO (the lab, agy 1.4.0): the
/// draft stays in the composer and the footer's left side becomes `press
/// ctrl+c again to exit` — idle NOT as evidence, the draft read as the
/// composer's, agy's layout still naming it. NEGATIVE CONTROL: the same
/// screen with the footer's left side gone (the right side alone, as while a
/// draft is held) is a held draft, idle as evidence.
#[test]
fn one_ctrl_c_over_a_draft_arms_the_exit_and_keeps_the_draft() {
    let text = lab::CTRL_C_DRAFT;
    let rows = screen(text);
    let footer = rows.iter().rposition(|r| !blank(r)).expect("the footer");
    let hint = anchor_text("agy.footer.exit");
    assert!(rows[footer].starts_with(hint));
    assert_eq!(footer_of(&rows[footer]), Footer::ExitPending);
    let r = agy(text);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, false)
    );
    let caret = footer - 2;
    assert_eq!(rows[caret], "> x");
    assert_eq!(r.composer, Some((caret, vec!["x".to_string()])));
    assert_eq!(
        r.said_tail.as_deref(),
        Some("Hi! I am the local lab stub, not a model. (reply 3)")
    );
    assert_eq!((r.prompt, r.wall), (None, None));
    assert_eq!(identify(None, &rows).program(), Program::Agy);
    let mut held = rows.clone();
    held[footer] = rows[footer].replacen(hint, &" ".repeat(hint.chars().count()), 1);
    assert_eq!(footer_of(&held[footer]), Footer::Blank);
    let h = read(Some("agy"), &held, None);
    assert_eq!((h.phase, h.phase_authoritative), (Phase::Idle, true));
    assert_eq!(h.composer, Some((caret, vec!["x".to_string()])));
}

/// After the second ctrl+c agy has left: its last frame stays above its
/// resume line and the shell's prompt, and nothing on that screen is agy's
/// to read — no composer, no evidence, and the screen names no program.
/// NEGATIVE CONTROL: the frame it left, with its footer back and the shell's
/// rows gone, is a composer again.
#[test]
fn an_exited_agy_is_read_as_nothing() {
    let gone = agy(EXITED);
    assert_eq!((gone.phase, gone.phase_authoritative), (Phase::Idle, false));
    assert_eq!(gone.composer, None);
    assert_eq!(identify(None, &screen(EXITED)).program(), Program::Generic);
    let rows = screen(EXITED);
    let bottom = rows
        .iter()
        .rposition(|r| plain_rule_width(r).is_some())
        .expect("the frame's bottom rule");
    let mut left = rows[..=bottom].to_vec();
    left.push("? for shortcuts".to_string());
    assert_eq!(phase(&left), (Phase::Idle, true), "the control");
}

/// SIGNED OUT, agy draws its login-method menu instead of a composer: a
/// WALL (the login is gone), never a box — no prompt is read, so no option
/// of it (`Google OAuth` opens a browser on the person's desktop) is ever
/// pressed — idle as evidence on its wall. NEGATIVE CONTROLS: the menu with
/// its `Select login method:` row gone is no wall, and still no box.
#[test]
fn the_sign_in_wall_is_a_wall_and_never_a_box() {
    let rows = screen(SIGNED_OUT);
    let r = agy(SIGNED_OUT);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    let w = r.wall.expect("the wall");
    assert_eq!(w.kind, WallKind::Auth);
    assert!(
        w.message.ends_with(anchor_text("agy.signin.wall")),
        "{}",
        w.message
    );
    assert_eq!((r.prompt, r.composer), (None, None));
    assert_eq!(prompt(&rows), None);
    assert_eq!(crate::reader::AgyReader.prompt(&rows), None);
    assert!(
        rows.iter()
            .any(|r| r.contains(anchor_text("agy.signin.oauth")))
    );
    let unnamed = edited(&rows, " Select login method:", "");
    assert_eq!(wall(&unnamed), None);
    assert_eq!(prompt(&unnamed), None);
}

/// Claude Code's and Codex's readers on agy's screens: never a box, never a
/// question — agy's `>` boxes and composer are no frame of theirs.
#[test]
fn claudes_and_codexs_readers_see_no_box_and_no_question_on_agy_screens() {
    for (name, text) in captures() {
        let r = screen(&text);
        for reader in [&ClaudeReader as &dyn ScreenReader, &CodexReader] {
            let phase = reader.phase(&r);
            assert!(
                !matches!(phase, Phase::Prompt | Phase::Question),
                "{:?} on {name}: {phase:?}",
                reader.program()
            );
            assert_eq!(reader.prompt(&r), None, "{name}");
        }
    }
}

/// A shell, a pager or a viewer showing agy's screen is not agy: the
/// program's NAME wins. With no name, or a runtime's, EVERY screen agy draws
/// names it — its composer idle, busy and at a turn's end, each box, its
/// trust box, its first run's onboarding, its survey and its sign-in wall —
/// but the screen agy left. A screen with
/// agy's words and no frame names nothing. NEGATIVE CONTROL: the same box
/// under `agy`.
#[test]
fn a_shell_quoting_agy_is_not_agy() {
    let r = screen(BOX_EXEC);
    for shell in ["zsh", "-bash", "less", "cat"] {
        let reading = read(Some(shell), &r, None);
        assert_eq!(reading.program, Program::Generic, "{shell}");
        assert_eq!(reading.prompt, None, "{shell}");
    }
    assert_eq!(identify(Some("agy"), &r).program(), Program::Agy);
    for (file, text) in captures() {
        if left(&text) {
            continue;
        }
        for name in [None, Some("node")] {
            assert_eq!(
                identify(name, &screen(&text)).program(),
                Program::Agy,
                "{name:?}: {file}"
            );
        }
        assert_eq!(
            identify(Some("zsh"), &screen(&text)).program(),
            Program::Generic,
            "the name wins"
        );
    }
    let quoted = rows(&[
        "% grep -h Generating log.txt",
        "⣾  Generating...",
        "> Write about 300 words on how ocean tides work.",
        "%",
    ]);
    assert_eq!(identify(None, &quoted).program(), Program::Generic);
}

/// agy is named `agy`, by its path and arguments too, and its busy row is
/// the spinner row: one on each busy screen. NEGATIVE CONTROLS: the
/// footer's `esc to cancel`, a reply's row, the blank braille cell and a
/// frame with one blank after it are no spinner row.
#[test]
fn agy_is_named_and_busy_on_its_spinner_row() {
    assert_eq!(crate::reader::program_of("agy"), Some(Program::Agy));
    assert_eq!(
        crate::reader::program_of("/opt/bin/agy --conversation=x"),
        Some(Program::Agy)
    );
    for text in [BUSY, BUSY_TIP] {
        let spinners = screen(text).iter().filter(|r| is_spinner_row(r)).count();
        assert_eq!(spinners, 1, "{}", provenance(text).unwrap_or_default());
    }
    for row in [
        "esc to cancel                    Gemini 4 Argon · medium",
        "  Ocean tides—the rhythmic rising",
        "⠀  blank braille is no frame",
        "⣾ Generating... (one blank)",
    ] {
        assert!(!is_spinner_row(row), "{row}");
    }
}

/// THE SNIFF CLAIMS NO OTHER PROGRAM'S SCREEN: over every capture of Claude
/// Code, Muse Code and Gemini CLI under `src/fixtures/`, agy's layout test says
/// no, the screen is never identified as agy's, and agy's reader reads no
/// box on it. The control: it says yes on agy's own (but the one agy left).
#[test]
fn the_agy_sniff_claims_no_other_programs_screen() {
    let mut swept = 0;
    for (name, text) in crate::text::fixture_files() {
        let r = screen(&text);
        if name.starts_with("agy-") {
            assert_eq!(is_agy_screen(&r), !left(&text), "{name}");
            continue;
        }
        swept += 1;
        assert!(!is_agy_screen(&r), "{name}");
        assert_ne!(identify(None, &r).program(), Program::Agy, "{name}");
        assert_eq!(prompt(&r), None, "{name}");
        assert_eq!(background_wait(&r), None, "{name}");
    }
    assert!(swept > 150, "only {swept} other captures");
}

/// A SHELL BOX WHOSE OPTIONS QUOTE A MULTI-LINE COMMAND (measured on the
/// person's own agy, signed in, a real task): options 2 and 3 carry the
/// command's prefix over rows of their own at column 0, a blank row among
/// them. The box is read whole — its four options from the `1.` row down,
/// each tail joined to its label (no blank in it), the one-shot allow, the
/// conversation grant, the kept grant, the cancel — as evidence. NEGATIVE
/// CONTROL: with no `1.` row above the blank the options cannot be placed,
/// and no role is read.
#[test]
fn a_box_whose_options_quote_a_multi_line_command_is_read_whole() {
    let r = agy(BOX_EXEC_MULTILINE);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    let p = r.prompt.expect("the box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.options.len(), 4);
    assert_eq!(
        roles(&p),
        vec![Role::Once, Role::Session, Role::Persist, Role::Deny]
    );
    assert!(p.options[0].focused);
    assert!(
        p.options[1].label.contains("import struct data = open(")
            && !p.options[1].label.contains("  "),
        "{:?}",
        p.options[1].label
    );
    assert!(p.options[2].label.ends_with("(Persist to settings.json)"));
    let rows = screen(BOX_EXEC_MULTILINE);
    let headless = edited(&rows, "> 1. Yes, run command", "");
    assert!(
        prompt(&headless).is_none_or(|p| roles(&p).iter().all(|&r| r == Role::Other)),
        "the control"
    );
}

/// AN ANSWER AGY SAVES IS KEPT, WHATEVER ITS WORDS: `… (Persist to
/// settings.json)` is a kept grant (or refusal) and `… in this
/// conversation` a session one — never the one-shot allow, and the press
/// gate refuses a saved one by its suffix too ([`crate::prompt::settles_later`]),
/// for a label that does not say `always allow` (agy's binary's `Yes, grant
/// permission for <target> (Persist to settings.json)`). NEGATIVE CONTROL:
/// the measured box keeps its roles.
#[test]
fn agys_saved_and_conversation_answers_are_never_one_shot() {
    let rows = screen(BOX_EXEC);
    let saved = edited(
        &rows,
        "  3. ",
        "  3. Yes, grant permission for touch (Persist to settings.json)",
    );
    let talk = edited(&rows, "  2. ", "  2. Yes, allow touch in this conversation");
    let p = prompt(&saved).expect("the box");
    assert_eq!(roles(&p)[2], Role::Persist);
    assert!(crate::prompt::settles_later(&p.options[2].label));
    assert_eq!(roles(&prompt(&talk).expect("the box"))[1], Role::Session);
    assert_eq!(
        roles(&prompt(&rows).expect("the box")),
        vec![Role::Once, Role::Session, Role::Persist, Role::Deny],
        "the control"
    );
}

/// The row the composer's top rule is on.
fn top_rule(rows: &[String]) -> usize {
    composer(rows).expect("the composer") - 1
}

/// AGY MARKS THE ANSWER IT RECOMMENDS AS A PREFIX (UNMEASURED: its
/// ask-the-user tool tells the model to open the option with
/// `(Recommended)`, by its binary; built by editing the measured question):
/// `  2. (Recommended) Green` is still an answer, and the one recommended.
/// NEGATIVE CONTROLS: the measured question marks none; the mark inside a
/// label marks nothing.
#[test]
fn agys_recommended_answer_opens_its_label() {
    let rows = screen(QUESTION);
    let marked = edited(&rows, "  2. Green", "  2. (Recommended) Green");
    let p = prompt(&marked).expect("the question");
    assert_eq!(
        roles(&p),
        vec![Role::Answer, Role::Answer, Role::Answer, Role::Other]
    );
    assert_eq!(
        p.recommended().map(|o| (o.n, o.label.as_str())),
        Some((Some(2), "(Recommended) Green"))
    );
    assert_eq!(
        prompt(&rows).expect("the question").recommended(),
        None,
        "the control"
    );
    let inside = edited(&rows, "  2. Green", "  2. Green, not (Recommended) teal");
    assert_eq!(prompt(&inside).expect("the question").recommended(), None);
}

/// A QUESTION NO DIGIT ANSWERS CARRIES NO ROLES, so it escalates
/// (UNMEASURED: agy's binary's marks on the measured question): a
/// multi-select's checkboxes (`> 1. [ ] Red`, `> 1. [x] Red` — a digit only
/// toggles one), the person's own answer kept on a row (`  2. ✓ Green`), or
/// options that do not end on `Write-in...`. NEGATIVE CONTROL: the measured
/// question keeps its Answer roles.
#[test]
fn a_question_no_digit_answers_carries_no_roles() {
    let rows = screen(QUESTION);
    for edit in [
        edited(&rows, "> 1. Red", "> 1. [ ] Red"),
        edited(&rows, "> 1. Red", "> 1. [x] Red"),
        edited(&rows, "  2. Green", "  2. ✓ Green"),
        edited(&rows, "  4. Write-in...", ""),
    ] {
        let p = prompt(&edit).expect("still a box");
        assert_eq!(p.kind, PromptKind::Question);
        assert!(
            roles(&p).iter().all(|&r| r == Role::Other),
            "{:?}: {:?}",
            labels(&p),
            roles(&p)
        );
    }
    assert_eq!(
        roles(&prompt(&rows).expect("the question")),
        vec![Role::Answer, Role::Answer, Role::Answer, Role::Other],
        "the control"
    );
}

/// A TURN THAT FAILED ON ONE OF AGY'S WALLS (UNMEASURED: its binary's words
/// in the measured failure's rows, `⚠ <message>` over `Error ID:`): `Our
/// servers are experiencing high traffic …` is an Overloaded wall on the
/// turn's idle end; `You have exhausted your quota on this model.` a usage
/// wall and `Your AI credits balance is too low to continue.` a spend wall,
/// each LIMITED as evidence, no reset named — a banner, under no call.
/// NEGATIVE CONTROLS: the measured failure is no wall
/// (`a_failed_turn_is_an_ended_turn_with_no_wall`); the words with no `Error
/// ID:` under them, or not in column 0, are none; a spinner over them is a
/// turn running on, with no wall.
#[test]
fn agys_turn_walls_are_read_by_their_words() {
    let rows = screen(ERROR_TURN);
    for (message, kind) in [
        (
            "Our servers are experiencing high traffic right now, please try again in a minute.",
            WallKind::Overloaded,
        ),
        (
            "You have exhausted your quota on this model.",
            WallKind::UsageSession,
        ),
        (
            "Your AI credits balance is too low to continue.",
            WallKind::Spend,
        ),
    ] {
        let failed = edited(
            &rows,
            "⚠ failed to construct executor",
            &format!("⚠ {message}"),
        );
        let r = read(Some("agy"), &failed, None);
        let w = r.wall.clone().expect("the wall");
        assert_eq!(
            (w.kind, w.message.as_str(), w.reset.as_deref()),
            (kind, message, None)
        );
        assert_eq!((w.placement, w.under_call), (Placement::Banner, false));
        assert!(failed[w.row].starts_with("⚠ "), "{message}");
        let want = if kind.reads_limited() {
            Phase::Limited {
                message: message.to_string(),
                reset: None,
            }
        } else {
            Phase::Idle
        };
        assert_eq!((r.phase, r.phase_authoritative), (want, true), "{message}");
        assert_eq!((r.said_tail, r.interrupted), (None, false), "{message}");

        let no_id = edited(&failed, "Error ID:", "");
        assert_eq!(wall(&no_id), None, "{message}");
        assert_eq!(phase(&no_id), (Phase::Idle, true), "{message}");
        let indented = edited(&failed, "⚠ ", &format!("  ⚠ {message}"));
        assert_eq!(wall(&indented), None, "{message}");
        let mut running = failed.clone();
        running.insert(top_rule(&failed), "⣾  Generating...".to_string());
        assert_eq!(wall(&running), None, "{message}");
        assert_eq!(phase(&running), (Phase::Busy, true), "{message}");
    }
    assert!(WallKind::UsageSession.reads_limited() && WallKind::Spend.reads_limited());
    assert!(!WallKind::Overloaded.reads_limited());
}

/// A PERMISSION MODE'S HINT IS NO DRAFT (UNMEASURED: agy's binary's four
/// hints on the measured idle composer): the caret row showing one — whole,
/// or cut with `...` at a narrow pane's edge — is the EMPTY composer, idle as
/// evidence, and the reader takes no text of it for a placeholder to type
/// over. NEGATIVE CONTROLS: a typed draft is the draft; a cut that lost its
/// ` mode: `, a hint with words typed after it, and a hint over a second
/// line are drafts.
#[test]
fn a_permission_modes_hint_is_an_empty_composer() {
    let rows = screen(IDLE);
    let (row, _) = cursor(IDLE).expect("measured");
    assert!(!crate::reader::AgyReader.placeholder());
    for id in [
        "agy.mode.hint.accept_edits",
        "agy.mode.hint.auto",
        "agy.mode.hint.plan",
        "agy.mode.hint.best_of_n",
    ] {
        let hint = anchor_text(id);
        for shown in [hint.to_string(), format!("{}...", &hint[..28])] {
            let on = edited(&rows, ">", &format!("> {shown}"));
            assert_eq!(
                composer_draft(&on),
                Some((row, vec![String::new()])),
                "{shown}"
            );
            assert_eq!(phase(&on), (Phase::Idle, true), "{shown}");
        }
    }
    let plan = anchor_text("agy.mode.hint.plan");
    for typed in ["Plan mo...".to_string(), format!("{plan} and then some")] {
        let on = edited(&rows, ">", &format!("> {typed}"));
        assert_eq!(
            composer_draft(&on),
            Some((row, vec![typed.clone()])),
            "{typed}"
        );
    }
    let mut two = edited(&rows, ">", &format!("> {plan}"));
    two.insert(row + 1, "  and a second line".to_string());
    assert_eq!(
        composer_draft(&two),
        Some((row, vec![plan.to_string(), "and a second line".to_string()]))
    );
    assert_eq!(
        composer_draft(&screen(DRAFT)).map(|(_, l)| l),
        Some(vec!["hello there".to_string()]),
        "the control"
    );
}

/// A NARROW PANE WRAPS A BOX'S KEY ROW onto column-2 rows of its own
/// (UNMEASURED: the measured boxes' key rows split in two): the box is still
/// read — the edit box an Edit with its accept and its refusal, its key row
/// read whole, its span to the row it wrapped onto — and the question's
/// `esc Skip`, wrapped onto the second row, is still its Esc. NEGATIVE
/// CONTROL: a column-2 run that does not open `↑/↓ Navigate` is no key row,
/// and no box is read.
#[test]
fn a_wrapped_key_row_is_read_whole() {
    let rows = screen(BOX_EDIT);
    let at = rows
        .iter()
        .position(|r| is_key_row(r))
        .expect("the key row");
    let mut wrapped = rows.clone();
    wrapped.splice(
        at..=at,
        [
            "  ↑/↓ Navigate · tab Amend".to_string(),
            "  f full diff".to_string(),
        ],
    );
    let r = read(Some("agy"), &wrapped, None);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    let p = r.prompt.expect("the box");
    assert_eq!(
        (p.kind, roles(&p)),
        (PromptKind::Edit, vec![Role::Once, Role::Deny])
    );
    assert_eq!(
        p.footer.as_deref(),
        Some("↑/↓ Navigate · tab Amend f full diff")
    );
    assert_eq!(p.span.1, at + 1);

    let question = screen(QUESTION);
    let k = question
        .iter()
        .position(|r| is_key_row(r))
        .expect("the key row");
    let mut skip = question.clone();
    skip.splice(
        k..=k,
        [
            "  ↑/↓ Navigate · enter Select ·".to_string(),
            "  esc Skip".to_string(),
        ],
    );
    let cancel = prompt(&skip).expect("the question").cancel.expect("esc");
    assert_eq!(
        (cancel.verb.as_str(), cancel.effect),
        ("skip", CancelEffect::Back)
    );

    let mut headless = wrapped.clone();
    headless[at] = "  Navigate · tab Amend".to_string();
    assert_eq!(prompt(&headless), None, "the control");
    assert_eq!(phase(&headless), (Phase::Idle, false));
}

/// THE PENDING EDIT'S ESC DOES NOTHING in agy 1.4.0 (its binary): the Edit
/// box offers no Esc, though its footer reads `esc to cancel`; and after one
/// agy draws `Esc disabled during file edits — press 1 to accept or 2 to
/// reject.` under the key row (UNMEASURED: built on the measured box, at
/// column 0 and at the key row's column 2), where the box is still read
/// whole, its key row its own. NEGATIVE CONTROLS: the shell box's Esc is
/// still the turn's interrupt; a row of other words in that place leaves no
/// box read.
#[test]
fn the_pending_edits_esc_is_disabled() {
    let rows = screen(BOX_EDIT);
    let p = prompt(&rows).expect("the box");
    assert_eq!((p.kind, p.cancel), (PromptKind::Edit, None));
    let exec = prompt(&screen(BOX_EXEC)).expect("the box");
    assert_eq!(
        exec.cancel.map(|c| c.effect),
        Some(CancelEffect::Interrupt),
        "the control"
    );
    let keys = rows
        .iter()
        .position(|r| is_key_row(r))
        .expect("the key row");
    let said = "Esc disabled during file edits — press 1 to accept or 2 to reject.";
    assert!(said.starts_with(anchor_text("agy.edit.esc_disabled")));
    for indent in ["", "  "] {
        let mut esc = rows.clone();
        esc.insert(keys + 1, format!("{indent}{said}"));
        let r = read(Some("agy"), &esc, None);
        assert_eq!(
            (r.phase, r.phase_authoritative),
            (Phase::Prompt, true),
            "{indent:?}"
        );
        let p = r.prompt.expect("the box");
        assert_eq!(
            (p.kind, roles(&p), p.cancel),
            (PromptKind::Edit, vec![Role::Once, Role::Deny], None),
            "{indent:?}"
        );
        assert_eq!(
            p.footer.as_deref(),
            Some(crate::text::trim(&rows[keys])),
            "{indent:?}"
        );
    }
    let mut other = rows.clone();
    other.insert(keys + 1, "Esc pressed during the edit.".to_string());
    assert_eq!(prompt(&other), None, "the control");
}

/// THE FOOTER'S RIGHT SIDE ALONE IS A BLANK LEFT SIDE, whatever it names
/// (UNMEASURED: a model with no effort badge, `Gemini 2.5 Flash`, and
/// `(initializing...)` under the measured draft): the draft held is idle as
/// evidence. The sniff stays strict: such a footer does not name the screen
/// agy's, as the measured `<model> · <effort>` does. NEGATIVE CONTROLS: the
/// empty composer over it says nothing; words at column 0 this grammar has
/// not measured say nothing.
#[test]
fn a_footer_naming_the_model_alone_is_blank() {
    let rows = screen(DRAFT);
    assert!(is_agy_screen(&rows), "the measured footer");
    for right in ["Gemini 2.5 Flash", "(initializing...)"] {
        let mut draft = rows.clone();
        *draft.last_mut().expect("the footer") = format!("{}{right}", " ".repeat(97));
        assert_eq!(phase(&draft), (Phase::Idle, true), "{right}");
        assert_eq!(
            composer_draft(&draft).map(|(_, l)| l),
            Some(vec!["hello there".to_string()])
        );
        assert!(!is_agy_screen(&draft), "{right}");
        let empty = edited(&draft, "> hello there", ">");
        assert_eq!(phase(&empty), (Phase::Idle, false), "{right}");
    }
    let mut other = rows.clone();
    *other.last_mut().expect("the footer") = "loading models".to_string();
    assert_eq!(phase(&other), (Phase::Idle, false), "the control");
}

/// A DRAFT WITH A BLANK LINE IN IT (UNMEASURED: the measured draft with a
/// second paragraph, its rows indented under the caret's text): the composer
/// holds all of it, the blank line an empty line, idle as evidence. NEGATIVE
/// CONTROL: a row under the caret at column 0 that is no rule ends no frame —
/// and the menu's, the boxes' and the exited screen's controls hold as
/// before.
#[test]
fn a_blank_line_in_a_draft_is_a_line_of_it() {
    let rows = screen(DRAFT);
    let (row, _) = cursor(DRAFT).expect("measured");
    let mut two = rows.clone();
    two.splice(
        row + 1..row + 1,
        [String::new(), "  and a second paragraph".to_string()],
    );
    assert_eq!(
        composer_draft(&two),
        Some((
            row,
            vec![
                "hello there".to_string(),
                String::new(),
                "and a second paragraph".to_string()
            ]
        ))
    );
    assert_eq!(phase(&two), (Phase::Idle, true));
    let mut broken = two.clone();
    broken[row + 2] = "and a second paragraph".to_string();
    assert_eq!(composer_draft(&broken), None, "the control");
    assert_eq!(phase(&broken), (Phase::Idle, false));
}

/// A TURN RUNS WHILE ITS SPINNER ROW IS ANYWHERE IN THE TURN (UNMEASURED: a
/// row drawn between the spinner and the composer, a queued message's, say):
/// busy as evidence, with no last words. The footer's `Press ↑ to edit queued
/// messages` is a running turn's own hint: with no spinner in sight it is
/// busy NOT authoritatively, as `esc to cancel` is, and names the screen
/// agy's. NEGATIVE CONTROLS: the turn's end stays idle; a spinner row above
/// the last message is an earlier turn's, and no turn running; a footer of
/// other words says nothing.
#[test]
fn a_spinner_anywhere_in_the_turn_is_busy() {
    let rows = screen(BUSY);
    let spinner = rows
        .iter()
        .position(|r| is_spinner_row(r))
        .expect("the spinner row");
    let mut between = rows.clone();
    between.insert(spinner + 1, "  Queued: and add a summary".to_string());
    assert_eq!(phase(&between), (Phase::Busy, true));
    assert_eq!(said_tail(&between), None);

    let mut queued = rows.clone();
    queued[spinner] = String::new();
    let footer = queued.last_mut().expect("the footer");
    *footer = footer.replacen("esc to cancel", "Press ↑ to edit queued messages", 1);
    assert_eq!(phase(&queued), (Phase::Busy, false));
    assert!(is_agy_screen(&queued));
    let mut other = queued.clone();
    *other.last_mut().expect("the footer") = "Press ↑ to see history".to_string();
    assert_eq!(phase(&other), (Phase::Idle, false), "the control");

    let ended = screen(END_OF_TURN);
    assert_eq!(phase(&ended), (Phase::Idle, true), "the control");
    let user = ended
        .iter()
        .position(|r| r.starts_with("> Write about 300 words"))
        .expect("the message");
    let mut earlier = ended.clone();
    earlier.insert(user - 1, "⣾  Generating...".to_string());
    assert_eq!(phase(&earlier), (Phase::Idle, true), "an earlier turn's");
}

/// THE NOTICE CARD OVER THE COMPOSER IS NO WORDS OF THE AGENT: the measured
/// card's rows (` ┃ Meet Gemini 4 Argon` … ` ┃ esc to dismiss`, its bar in
/// column 1; [`IDLE_FRESH_NOTICE`]) over the composer under a turn's reply
/// leave the reply its last words, idle as evidence. NEGATIVE CONTROL: the
/// same rows with the bar in column 0, no card agy draws, leave the turn no
/// last words.
#[test]
fn the_notice_card_is_no_last_words() {
    let card: Vec<String> = screen(IDLE_FRESH_NOTICE)
        .into_iter()
        .filter(|r| r.starts_with(" ┃"))
        .collect();
    assert_eq!(card.len(), 4);
    let rows = screen(END_OF_TURN);
    let top = top_rule(&rows);
    let mut carded = rows.clone();
    carded.splice(top..top, card.clone());
    let said = said_tail(&carded).expect("the reply");
    assert!(
        said.starts_with("Ocean tides—the rhythmic")
            && said.ends_with("dampen these global rhythms."),
        "{said}"
    );
    assert_eq!(phase(&carded), (Phase::Idle, true));
    let mut flush = rows.clone();
    flush.splice(top..top, card.iter().map(|r| r.trim_start().to_string()));
    assert_eq!(said_tail(&flush), None, "the control");
}

/// A SUBAGENT'S ALERT IS A PROMPT NOTHING HERE ANSWERS (UNMEASURED: its
/// binary's words, `<agent> needs approval for <tool>` and `<agent> has a
/// question`, as a row of the turn over the measured composer): a prompt as
/// evidence — while the turn's spinner runs too — read as an Other box titled
/// by the alert, with NO option and its head on the screen, so a policy
/// escalates it; no wall is read under it. NEGATIVE CONTROLS: the measured
/// turn's end has no alert; an alert above the last message is an earlier
/// turn's.
#[test]
fn a_subagents_alert_is_a_prompt_with_no_option() {
    let rows = screen(END_OF_TURN);
    let top = top_rule(&rows);
    let user = rows
        .iter()
        .position(|r| r.starts_with("> Write about 300 words"))
        .expect("the message");
    for alert in [
        "● researcher needs approval for run_command",
        "● researcher has a question",
    ] {
        let mut up = rows.clone();
        up.insert(top, alert.to_string());
        let r = read(Some("agy"), &up, None);
        assert_eq!(
            (r.phase, r.phase_authoritative),
            (Phase::Prompt, true),
            "{alert}"
        );
        assert_eq!(r.wall, None, "{alert}");
        let p = r.prompt.expect("the alert");
        assert_eq!(
            (p.kind, p.title.as_str(), p.head_off_screen),
            (PromptKind::Other, alert, false)
        );
        assert!(p.options.is_empty() && p.cancel.is_none() && p.recommended().is_none());
        let mut busy = up.clone();
        busy.insert(top + 1, "⣾  Working...".to_string());
        assert_eq!(phase(&busy), (Phase::Prompt, true), "{alert}");
        let mut earlier = rows.clone();
        earlier.insert(user - 1, alert.to_string());
        assert_eq!(phase(&earlier), (Phase::Idle, true), "{alert}");
        assert_eq!(prompt(&earlier), None, "{alert}");
    }
    // A reply's row (two columns in) that names an alert's words is no alert.
    let mut said = rows.clone();
    said.insert(
        top,
        "  The reviewer has a question about the plan.".to_string(),
    );
    assert_ne!(phase(&said).0, Phase::Prompt);
    let r = agy(END_OF_TURN);
    assert_eq!((r.phase, r.prompt), (Phase::Idle, None), "the control");
}

// ---- the lab's captures: agy 1.4.0 under the Gemini API provider -------

/// What one lab capture must read as, by agy's reader under its own name.
struct Lab {
    text: &'static str,
    /// [`Phase::name`].
    phase: &'static str,
    evidence: bool,
    prompt: Option<PromptKind>,
    /// [`WallKind::name`].
    wall: Option<&'static str>,
    survey: bool,
    /// What the sniff names with no program name.
    sniffed: Program,
    /// The agent's own background work ([`background_wait`]).
    background: Option<&'static str>,
}

const fn lab(text: &'static str, phase: &'static str, evidence: bool) -> Lab {
    Lab {
        text,
        phase,
        evidence,
        prompt: None,
        wall: None,
        survey: false,
        sniffed: Program::Agy,
        background: None,
    }
}

/// Every lab capture, and what it must read as.
fn lab_readings() -> Vec<Lab> {
    let boxed = |text, kind| Lab {
        prompt: Some(kind),
        ..lab(text, "prompt", true)
    };
    let walled = |text, phase, wall| Lab {
        wall: Some(wall),
        ..lab(text, phase, true)
    };
    let working = |text, what| Lab {
        background: Some(what),
        ..lab(text, "busy", true)
    };
    vec![
        boxed(lab::FIRST_RUN_SCHEME, PromptKind::Other),
        boxed(lab::FIRST_RUN_TERMS, PromptKind::Other),
        boxed(lab::FIRST_RUN_TRUST, PromptKind::Trust),
        lab(lab::IDLE, "idle", true),
        lab(lab::DRAFT, "idle", true),
        lab(lab::BUSY_THINKING, "busy", true),
        lab(lab::BUSY_STREAMING, "busy", true),
        lab(lab::END_OF_TURN, "idle", true),
        lab(lab::REPLY_WRAPPED_50, "question", true),
        lab(lab::INTERRUPTED, "idle", true),
        lab(lab::SLASH_MENU, "idle", false),
        boxed(lab::BOX_SHELL, PromptKind::Bash),
        boxed(lab::BOX_WRITE, PromptKind::Write),
        boxed(lab::BOX_EDIT, PromptKind::Edit),
        boxed(lab::QUESTION, PromptKind::Question),
        lab(lab::QUESTION_ANSWERED, "idle", true),
        Lab {
            survey: true,
            ..lab(lab::SURVEY, "idle", false)
        },
        Lab {
            sniffed: Program::Generic,
            ..lab(lab::EXIT, "idle", false)
        },
        lab(lab::EXIT_CONFIRM_CTRL_D, "idle", false),
        lab(lab::CTRL_C_DRAFT, "idle", false),
        walled(lab::ERROR_429, "limited", "spend"),
        walled(lab::ERROR_401, "idle", "auth"),
        walled(lab::ERROR_400_KEY, "idle", "auth"),
        lab(lab::RETRY_503, "busy", true),
        walled(lab::ERROR_503, "idle", "overloaded"),
        walled(lab::ERROR_500, "idle", "api-error"),
        walled(lab::ERROR_403_DISABLED, "idle", "api-error"),
        walled(lab::ERROR_403, "idle", "api-error"),
        walled(lab::ERROR_404, "idle", "api-error"),
        walled(lab::ERROR_400_LOCATION, "idle", "api-error"),
        walled(lab::ERROR_504, "idle", "api-error"),
        walled(lab::ERROR_429_BARE, "limited", "api-error"),
        walled(lab::ERROR_429_PER_MINUTE, "limited", "api-error"),
        walled(lab::ERROR_429_LONG, "limited", "spend"),
        working(lab::BG_TASK, TASK_RUNNING),
        working(lab::BG_TIMER, TASK_RUNNING),
        working(lab::BG_SUBAGENT, SUBAGENT_WORKING),
        working(lab::BG_TASK_AND_SUBAGENT, SUBAGENT_WORKING),
        working(lab::BG_DAEMON, TASK_RUNNING),
        working(lab::BG_DRAFT, TASK_RUNNING),
        working(lab::BG_PANEL_OFF_TASK, TASK_RUNNING),
        working(lab::BG_PANEL_OFF_SUBAGENT, SUBAGENT_WORKING),
        working(lab::BG_PANEL_OFF_DAEMON, TASK_RUNNING),
        lab(lab::BG_BUSY, "busy", true),
        lab(lab::BG_TASK_ENDED, "idle", true),
        Lab {
            sniffed: Program::Generic,
            ..lab(lab::BG_EXITED, "idle", false)
        },
    ]
}

/// THE BACKGROUND-WORK PANEL IS NO PART OF THE COMPOSER, AND ITS WORK IS THE
/// AGENT'S OWN (agy 1.4.0, measured in the lab on 2026-10-17): under the
/// composer's bottom rule agy draws one row an item — a command task or a
/// timer `  ● [hh:mm:ss] <what> running`, a subagent `  ● Agent(<kind>:
/// <name>)  Working · Ns` — then a second rule, then the footer counting
/// them. The composer reads over it as without it: the empty caret on the
/// row the cursor parks on, a held draft the draft, the spinner of a turn
/// sent meanwhile busy as evidence and no background work (the turn is the
/// agent's live one), the reply the last words. An ended turn over it is
/// the agent's own background work, named, and busy as evidence — as Claude
/// Code's reader reads a shell, a workflow or a background agent it left
/// running, over a question it ended on too (the hard rule) — so no
/// continuation is typed into a session agy wakes itself. Each of the
/// panel's rows says so alone (its footer's count stripped), and so does
/// the count alone (agy's setting that hides the panel, `hideActiveItems`:
/// measured, [`lab::BG_PANEL_OFF_TASK`] and its siblings). Before,
/// every one of these read as no composer: idle NOT as evidence, the draft
/// unseen, the running turn idle. NEGATIVE CONTROLS: the panel gone and the
/// count gone (as the work's end leaves the screen: the question then a
/// question), the panel with no running row and no count, a task's row
/// stamped with no clock, a count of `0`, a panel no rule closes or one
/// closed by a rule of another width, and the screen agy left with the
/// panel's rows in its last frame read no background work. Claude Code's
/// reader reads none of it.
#[test]
fn the_background_panel_is_no_part_of_the_composer_and_its_work_is_the_agents() {
    let uncounted = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .map(|r| {
                r.replace(" · 1 task(s) · /tasks", "")
                    .replace(" · 1 subagent(s)", "")
            })
            .collect()
    };
    for (text, what) in [
        (lab::BG_TASK, TASK_RUNNING),
        (lab::BG_TIMER, TASK_RUNNING),
        (lab::BG_SUBAGENT, SUBAGENT_WORKING),
        (lab::BG_TASK_AND_SUBAGENT, SUBAGENT_WORKING),
        (lab::BG_DAEMON, TASK_RUNNING),
    ] {
        let name = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let r = agy(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Busy, true),
            "{name}"
        );
        let (row, col) = cursor(text).expect("measured");
        assert_eq!(r.composer, Some((row, vec![String::new()])), "{name}");
        assert_eq!(col, 2, "{name}");
        assert_eq!(r.said_tail.as_deref(), Some("Tool call done."), "{name}");
        assert_eq!(background_wait(&rows), Some(what), "{name}");
        assert_eq!(ClaudeReader.background_wait(&rows), None, "{name}");
        // The panel's rows with no count under them.
        let counted_not = uncounted(&rows);
        assert_ne!(counted_not, rows, "{name}: the count stripped");
        assert_eq!(phase(&counted_not), (Phase::Busy, true), "{name}");
        assert_eq!(background_wait(&counted_not), Some(what), "{name}");
        // The footer's count with no panel over it.
        let panel: Vec<usize> = (0..rows.len())
            .filter(|&i| is_panel_row(&rows[i]))
            .collect();
        let close = panel.last().expect("a panel row") + 1;
        let bare: Vec<String> = rows
            .iter()
            .enumerate()
            .filter(|&(i, _)| !panel.contains(&i) && i != close)
            .map(|(_, r)| r.clone())
            .collect();
        assert_eq!(phase(&bare), (Phase::Busy, true), "{name}");
        assert_eq!(background_wait(&bare), Some(what), "{name}");
        // THE CONTROLS. No panel and no count: the turn's end as without it.
        let plain = uncounted(&bare);
        assert_eq!(phase(&plain), (Phase::Idle, true), "{name}");
        assert_eq!(background_wait(&plain), None, "{name}");
        assert_eq!(said_tail(&plain), r.said_tail, "{name}");
        // The panel's rows saying nothing runs, and no count.
        let mut idle = uncounted(&rows);
        for &i in &panel {
            idle[i] = idle[i]
                .replace(" running", " ended")
                .replace("Working", "Idle");
        }
        assert_eq!(phase(&idle), (Phase::Idle, true), "{name}");
        assert_eq!(background_wait(&idle), None, "{name}");
        assert!(composer(&idle).is_some(), "{name}");
        // A count of none is no work.
        let none: Vec<String> = bare
            .iter()
            .map(|r| {
                r.replace(" 1 task(s)", " 0 task(s)")
                    .replace(" 1 subagent(s)", " 0 subagent(s)")
            })
            .collect();
        assert_ne!(none, bare, "{name}: the count zeroed");
        assert_eq!(phase(&none), (Phase::Idle, true), "{name}");
        assert_eq!(background_wait(&none), None, "{name}");
        // A panel no rule closes is no frame of agy's, nor one closed by a
        // rule of another width (the transcript's separators are 60).
        let mut open = rows.clone();
        open.remove(close);
        let mut narrow = rows.clone();
        narrow[close] = "─".repeat(60);
        for shape in [&open, &narrow] {
            assert_eq!(composer(shape), None, "{name}");
            assert_eq!(phase(shape), (Phase::Idle, false), "{name}");
            assert_eq!(background_wait(shape), None, "{name}");
        }
    }
    // A task's row is stamped with the clock: `[hh:mm:ss]`, nothing else.
    let rows = uncounted(&screen(lab::BG_TASK));
    for stamp in ["[17:50]", "[17-50-42]", "[1750:42]", "[ab:cd:ef]"] {
        let unclocked: Vec<String> = rows
            .iter()
            .map(|r| r.replace("[17:50:42]", stamp))
            .collect();
        assert_ne!(unclocked, rows, "{stamp}");
        assert_eq!(phase(&unclocked), (Phase::Idle, true), "{stamp}");
        assert_eq!(background_wait(&unclocked), None, "{stamp}");
    }
    // A question the turn ended on: the work holds it, as a shell left
    // running holds Claude Code's (the hard rule); the work gone, it asks.
    let rows = screen(lab::BG_TASK);
    let asks = edited(
        &rows,
        "  Tool call done.",
        "  Should I wait for it to finish?",
    );
    assert_eq!(phase(&asks), (Phase::Busy, true));
    assert_eq!(background_wait(&asks), Some(TASK_RUNNING));
    let r = read(Some("agy"), &asks, None);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Busy, true));
    let panel: Vec<usize> = (0..asks.len())
        .filter(|&i| is_panel_row(&asks[i]))
        .collect();
    let close = panel.last().expect("a panel row") + 1;
    let asked: Vec<String> = asks
        .iter()
        .enumerate()
        .filter(|&(i, _)| !panel.contains(&i) && i != close)
        .map(|(_, r)| r.replace(" · 1 task(s) · /tasks", ""))
        .collect();
    assert_eq!(phase(&asked), (Phase::Question, true), "the control");
    assert_eq!(background_wait(&asked), None, "the control");
    // A draft held over the panel is the draft, the cursor after it.
    let r = agy(lab::BG_DRAFT);
    let (row, col) = cursor(lab::BG_DRAFT).expect("measured");
    assert_eq!(
        r.composer,
        Some((row, vec!["busy while a task runs".to_string()]))
    );
    assert_eq!(col, 2 + "busy while a task runs".len());
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Busy, true));
    // A turn sent meanwhile: its spinner, the agent's live turn.
    let busy = screen(lab::BG_BUSY);
    let r = agy(lab::BG_BUSY);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Busy, true));
    assert_eq!(background_wait(&busy), None);
    assert_eq!(busy.iter().filter(|r| is_spinner_row(r)).count(), 1);
    let (row, _) = cursor(lab::BG_BUSY).expect("measured");
    assert_eq!(r.composer, Some((row, vec![String::new()])));
    // The work over, and agy gone with its work running.
    let r = agy(lab::BG_TASK_ENDED);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert_eq!(background_wait(&screen(lab::BG_TASK_ENDED)), None);
    let gone = screen(lab::BG_EXITED);
    assert!(
        gone.iter().any(|r| is_panel_row(r)),
        "the panel's rows stand"
    );
    let r = agy(lab::BG_EXITED);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, false));
    assert_eq!((r.composer, r.said_tail), (None, None));
    assert_eq!(background_wait(&gone), None);
}

/// WHAT THE SCREEN SHOWS OF THE AGENT'S OWN BACKGROUND WORK, WHATEVER ITS
/// TURN DOES ([`background_shown`], the live upgrade's quiet restart's screen
/// witness): every panel row of the lab's captures — a task, a timer, a
/// subagent, a daemon task, both at once, under a draft, and under a turn
/// running (whose reading is busy with no background work named) — and the
/// footer's count alone where the panel is turned off (`hideActiveItems`,
/// measured), and a subagent's row in a state the reader names no work for
/// (`Processing...`, measured for a moment as it starts) with the count
/// stripped. NEGATIVE CONTROLS: the work over (no panel, no count), an idle
/// and a fresh screen, a box, the screen agy left with the panel's rows in
/// its last frame, and a count of `0` show none.
#[test]
fn every_panel_row_and_every_count_shows_background_work() {
    for (text, what) in [
        (lab::BG_TASK, TASK_RUNNING),
        (lab::BG_TIMER, TASK_RUNNING),
        (lab::BG_SUBAGENT, SUBAGENT_WORKING),
        (lab::BG_TASK_AND_SUBAGENT, SUBAGENT_WORKING),
        (lab::BG_DAEMON, TASK_RUNNING),
        (lab::BG_DRAFT, TASK_RUNNING),
        (lab::BG_BUSY, TASK_RUNNING),
        (lab::BG_PANEL_OFF_TASK, TASK_RUNNING),
        (lab::BG_PANEL_OFF_SUBAGENT, SUBAGENT_WORKING),
        (lab::BG_PANEL_OFF_DAEMON, TASK_RUNNING),
    ] {
        assert_eq!(
            background_shown(&screen(text)),
            Some(what),
            "{:?}",
            provenance(text)
        );
    }
    let processing: Vec<String> = screen(lab::BG_SUBAGENT)
        .iter()
        .map(|r| {
            r.replace("Working · 1s", "Processing... · 0s")
                .replace(" · 1 subagent(s)", "")
        })
        .collect();
    assert_eq!(background_shown(&processing), Some(BACKGROUND_ITEM));
    assert_eq!(
        background_wait(&processing),
        None,
        "the reader names no work for a state it does not know"
    );
    let zero: Vec<String> = screen(lab::BG_PANEL_OFF_TASK)
        .iter()
        .map(|r| r.replace(" · 1 task(s)", " · 0 task(s)"))
        .collect();
    for (name, rows) in [
        ("ended", screen(lab::BG_TASK_ENDED)),
        ("idle", screen(lab::IDLE)),
        ("end of turn", screen(lab::END_OF_TURN)),
        ("box", screen(lab::BOX_SHELL)),
        ("exited", screen(lab::BG_EXITED)),
        ("zero", zero),
    ] {
        assert_eq!(background_shown(&rows), None, "{name}");
    }
}

/// A LIMIT OR A WALL THE TURN ENDED ON STANDS OVER THE AGENT'S OWN
/// BACKGROUND WORK (agy 1.4.0's lab screens): the panel and its running task
/// spliced under the composer of a turn that ended on the stub's 500, on
/// the key's 401 and on a per-day 429 read as those walls still — idle on
/// the error and on the key (so the retry ladder and the login's escalation
/// run), limited until the reset — while [`background_wait`] names the
/// work, as Claude Code's reader reads the wall a turn ended on behind a
/// shell it left running (`phase::background_wall`). Read busy, the ladder
/// and the escalation would never run. NEGATIVE CONTROL: the same splice
/// under a turn that ended on no wall reads busy.
#[test]
fn a_wall_the_turn_ended_on_stands_over_the_background_work() {
    let splice = |text: &str| -> Vec<String> {
        let mut rows = screen(text);
        let f = frame(&rows).expect("the composer");
        let width = plain_rule_width(&rows[f.bottom]).expect("the bottom rule");
        rows.insert(
            f.bottom + 1,
            "  ● [17:50:42] sleep 150; echo LAB-BG-DONE running".to_string(),
        );
        rows.insert(f.bottom + 2, "─".repeat(width));
        rows
    };
    for (text, read_as, kind) in [
        (lab::ERROR_500, "idle", "api-error"),
        (lab::ERROR_401, "idle", "auth"),
        (lab::ERROR_429_LONG, "limited", "spend"),
    ] {
        let name = provenance(text).unwrap_or_default();
        let rows = splice(text);
        let r = read(Some("agy"), &rows, None);
        assert_eq!(
            (r.phase.name(), r.phase_authoritative),
            (read_as, true),
            "{name}"
        );
        assert_eq!(r.wall.map(|w| w.kind.name()), Some(kind), "{name}");
        assert!(r.composer.is_some(), "{name}");
        assert_eq!(background_wait(&rows), Some(TASK_RUNNING), "{name}");
    }
    let ended = splice(lab::BG_TASK_ENDED);
    assert_eq!(phase(&ended), (Phase::Busy, true), "the control");
    assert_eq!(background_wait(&ended), Some(TASK_RUNNING), "the control");
}

/// ONLY AN ENDED TURN THE READER VOUCHES FOR IS HELD BY THE WORK, AND THE
/// PANEL'S ROWS SIT TWO COLUMNS IN (the review of the hard rule's guards).
/// An exit pending (`press ctrl+c again to exit`) over a running task is
/// `idle` NOT as evidence still, no background work named: agy is one key
/// from gone, and read busy its exit would be waited through. A `● ` row one
/// column in or four under the bottom rule is no panel row of agy's (one
/// item, `● ` two columns in, measured), so the screen has no frame and no
/// work is named. NEGATIVE CONTROL: the measured screen, its task's row two
/// columns in under `? for shortcuts`, is busy on its task.
#[test]
fn an_exit_pending_over_work_is_no_evidence_and_a_panel_row_sits_two_columns_in() {
    let rows = screen(lab::BG_TASK);
    assert_eq!(phase(&rows), (Phase::Busy, true), "the control");
    assert_eq!(background_wait(&rows), Some(TASK_RUNNING), "the control");
    let footer = rows
        .iter()
        .rposition(|r| r.starts_with("? for shortcuts"))
        .expect("the footer");
    let mut exiting = rows.clone();
    exiting[footer] = exiting[footer].replacen("? for shortcuts", "press ctrl+c again to exit", 1);
    assert_eq!(phase(&exiting), (Phase::Idle, false));
    assert_eq!(background_wait(&exiting), None);
    let r = read(Some("agy"), &exiting, None);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, false));
    let task = rows
        .iter()
        .position(|r| is_panel_row(r))
        .expect("the task's row");
    for indent in [1, 4] {
        let mut shifted = rows.clone();
        shifted[task] = format!("{}{}", " ".repeat(indent), rows[task].trim_start());
        assert!(!is_panel_row(&shifted[task]), "{indent}");
        assert_eq!(background_wait(&shifted), None, "{indent}");
        assert_eq!(composer(&shifted), None, "{indent}");
        assert_eq!(phase(&shifted), (Phase::Idle, false), "{indent}");
    }
}

/// EVERY LAB CAPTURE READS AS IT MUST — its phase and whether that is
/// evidence, its box's kind, its wall's, its survey, the program its layout
/// names and the background work it shows — and the table covers every `agy-1.4.0-lab-*` file, so a
/// capture added later is never left unread. NEGATIVE CONTROL: the generic
/// reader, handed the same screens, reads none of it.
#[test]
fn every_lab_capture_reads_as_it_must() {
    let table = lab_readings();
    let files: Vec<(String, String)> = captures()
        .into_iter()
        .filter(|(name, _)| name.starts_with("agy-1.4.0-lab-"))
        .collect();
    assert_eq!(files.len(), table.len(), "one row per lab capture");
    for (name, text) in &files {
        assert!(
            table.iter().any(|l| l.text == text),
            "{name} has no row in the table"
        );
    }
    for l in &table {
        let what = provenance(l.text).unwrap_or_default();
        let rows = screen(l.text);
        let r = agy(l.text);
        assert_eq!(
            (r.phase.name(), r.phase_authoritative),
            (l.phase, l.evidence),
            "{what}"
        );
        assert_eq!(r.prompt.as_ref().map(|p| p.kind), l.prompt, "{what}");
        assert_eq!(r.wall.as_ref().map(|w| w.kind.name()), l.wall, "{what}");
        assert_eq!(r.survey, l.survey, "{what}");
        assert_eq!(identify(None, &rows).program(), l.sniffed, "{what}");
        assert_eq!(background_wait(&rows), l.background, "{what}");
        assert_eq!(AgyReader.background_wait(&rows), l.background, "{what}");
        let generic = read(Some("zsh"), &rows, None);
        assert_eq!(
            (generic.prompt, generic.wall, generic.survey),
            (None, None, false),
            "the control: {what}"
        );
    }
}

/// AGY'S SURVEY IS A RATING CARD — never its composer, never a box: drawn in
/// the composer's place, its question and `[1] Good  [2] Fine  [3] Bad  [0]
/// Skip` at column 1 over the footer, it is read as a session survey whose
/// `0` may be pressed, guarded on its options row as the screen shows it
/// (its question's ASCII `'` could not be quoted), with no composer, no box,
/// no last words and an idle that is NO evidence, so nothing is typed into
/// it (a digit typed for a turn would rate). The turn under it ended on a
/// 401, which is read once the survey has gone and the composer is back.
/// NEGATIVE CONTROLS: the card's rows quoted two columns in (a reply), its
/// question row gone, its options without `[0] Skip`, and a composer drawn
/// under it are no survey.
#[test]
fn the_survey_is_a_rating_card_never_the_composer_or_a_box() {
    let rows = screen(lab::SURVEY);
    let r = agy(lab::SURVEY);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, false)
    );
    assert_eq!((r.composer, r.prompt, r.said_tail), (None, None, None));
    assert_eq!(r.wall, None);
    assert!(r.survey);
    let card = survey(&rows).expect("the card");
    assert_eq!(card.kind, SurveyKind::Session);
    assert_eq!((card.dismissable, card.withheld()), (true, None));
    assert_eq!(
        crate::text::trim(&rows[card.head]),
        anchor_text("agy.survey.question")
    );
    assert_eq!(rows[card.options], " [1] Good  [2] Fine  [3] Bad  [0] Skip");
    assert_eq!(
        card.guard,
        r"^.\[1\].Good..\[2\].Fine..\[3\].Bad..\[0\].Skip"
    );
    assert!(
        !card.guard.contains('\''),
        "the manager's command quotes it"
    );
    assert_eq!(
        crate::reader::AgyReader.survey_block(&rows),
        Some(card.clone())
    );
    assert_eq!(ClaudeReader.survey_block(&rows), None);

    let mut quoted = rows.clone();
    quoted[card.head] = format!(" {}", rows[card.head]);
    quoted[card.options] = format!(" {}", rows[card.options]);
    assert_eq!(survey(&quoted), None, "a reply's copy");
    let mut headless = rows.clone();
    headless[card.head] = String::new();
    assert_eq!(survey(&headless), None, "no question");
    let unskippable = edited(&rows, " [1] Good", " [1] Good  [2] Fine  [3] Bad");
    assert_eq!(survey(&unskippable), None, "no [0] Skip");
    let mut composed = rows[..=card.options].to_vec();
    composed.extend([
        "─".repeat(144),
        ">".to_string(),
        "─".repeat(144),
        "? for shortcuts".to_string(),
    ]);
    assert_eq!(survey(&composed), None, "a composer under it");

    // The survey gone with `0`: the composer back over the turn's 401.
    let at = rows
        .iter()
        .position(|r| r.starts_with(anchor_text("agy.error.id")))
        .expect("the error id");
    let mut back = rows[..=at].to_vec();
    back.extend([
        String::new(),
        "─".repeat(144),
        ">".to_string(),
        "─".repeat(144),
        "? for shortcuts".to_string(),
    ]);
    let gone = read(Some("agy"), &back, None);
    assert!(!gone.survey);
    assert_eq!(gone.wall.map(|w| w.kind), Some(WallKind::Auth));
}

/// A TURN THE GEMINI API FAILED IS A WALL, by the status agy printed: 401
/// and a 400 whose reason is `API_KEY_INVALID` the login, a 429 that says
/// the quota is exceeded a spend wall (limited, as evidence), a 503 that
/// says overloaded an overload, a 500 an API error that agy did not retry.
/// The wall is the `⚠` row's, a banner whose reset is a 429's RetryInfo
/// delay as a span (none on the others), its message the block's rows
/// joined. NEGATIVE CONTROLS: the signed-in failure with no
/// status is no wall; the same blocks with the words that name their kind
/// changed are API errors by their status; a reply under the block, or a
/// newer message, ends the turn elsewhere; the retry under the spinner is
/// busy, with no wall.
#[test]
fn a_failed_turn_is_a_wall_by_the_status_the_api_answered() {
    for (text, kind, reset) in [
        (lab::ERROR_401, WallKind::Auth, None),
        (lab::ERROR_400_KEY, WallKind::Auth, None),
        (lab::ERROR_429, WallKind::Spend, Some("in 3600s")),
        (lab::ERROR_503, WallKind::Overloaded, None),
        (
            lab::ERROR_500,
            WallKind::ApiError {
                code: Some(500),
                retryable: false,
                cause: ApiCause::Server,
            },
            None,
        ),
    ] {
        let what = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let r = agy(text);
        let w = r.wall.clone().expect("the wall");
        assert_eq!(w.kind, kind, "{what}");
        assert!(rows[w.row].starts_with("⚠ agent executor error"), "{what}");
        assert!(
            w.message
                .starts_with("agent executor error: generating and executing: Error "),
            "{what}"
        );
        assert!(!w.message.contains("Error ID"), "{what}");
        assert_eq!(
            (w.placement, w.reset.as_deref(), w.under_call),
            (Placement::Banner, reset, false),
            "{what}"
        );
        assert_eq!((r.said_tail, r.interrupted), (None, false), "{what}");
        let limited = matches!(r.phase, Phase::Limited { .. });
        assert_eq!(limited, kind.reads_limited(), "{what}");
        assert!(r.phase_authoritative, "{what}");
    }
    assert_eq!(agy(ERROR_TURN).wall, None, "no status: the control");

    let swap = |text: &str, from: &str, to: &str| -> Vec<String> {
        let rows = screen(text);
        assert!(rows.iter().any(|r| r.contains(from)), "{from}");
        rows.iter().map(|r| r.replace(from, to)).collect()
    };
    for (rows, code, retryable) in [
        (
            swap(lab::ERROR_400_KEY, "API_KEY_INVALID", "BAD_REQUEST"),
            400,
            false,
        ),
        (swap(lab::ERROR_503, "overloaded", "unavailable"), 503, true),
        (
            swap(
                lab::ERROR_429,
                "exceeded your current quota",
                "sent too many",
            ),
            429,
            true,
        ),
    ] {
        assert_eq!(
            wall(&rows).map(|w| w.kind),
            Some(WallKind::ApiError {
                code: Some(code),
                retryable,
                cause: ApiCause::Server,
            })
        );
    }
    let rate = swap(
        lab::ERROR_429,
        "exceeded your current quota",
        "sent too many",
    );
    assert!(
        matches!(phase(&rate).0, Phase::Limited { .. }),
        "a 429 still limits"
    );

    let rows = screen(lab::ERROR_401);
    let id = rows
        .iter()
        .position(|r| r.starts_with(anchor_text("agy.error.id")))
        .expect("the error id");
    let mut replied = rows.clone();
    replied.splice(
        id + 1..id + 1,
        [String::new(), "  Retrying now.".to_string()],
    );
    assert_eq!(wall(&replied), None, "a reply ended the turn");
    assert_eq!(said_tail(&replied).as_deref(), Some("Retrying now."));
    let mut asked = rows.clone();
    asked.splice(
        id + 1..id + 1,
        [
            String::new(),
            "─".repeat(60),
            "> carry on".to_string(),
            String::new(),
            "  Done.".to_string(),
        ],
    );
    assert_eq!(wall(&asked), None, "a newer message");
    let busy = agy(lab::RETRY_503);
    assert_eq!((busy.phase, busy.wall), (Phase::Busy, None));
}

/// THE FIRST RUN'S ONBOARDING SCREENS ARE BOXES NO POLICY PRESSES: the colour
/// scheme and the terms are prompts of NO KIND whose every option (the
/// schemes, the consent's ticked box, the buttons) carries no role, chosen
/// by the cursor and Enter, with no Esc, no wall and no composer — the
/// person's taste and the person's consent. The trust box after them reads
/// as the signed-in one does. NEGATIVE CONTROLS: with its heading gone a
/// screen is no box, and nothing is evidence; the signed-in trust box is the
/// same prompt the lab's is, roles and all.
#[test]
fn the_first_run_screens_are_boxes_of_no_kind() {
    let scheme = agy(lab::FIRST_RUN_SCHEME).prompt.expect("the picker");
    assert_eq!(
        (scheme.kind, scheme.title.as_str()),
        (PromptKind::Other, anchor_text("agy.onboarding.scheme"))
    );
    assert_eq!(
        labels(&scheme),
        vec![
            "terminal",
            "light",
            "solarized light",
            "colorblind-friendly light",
            "dark",
            "solarized dark",
            "colorblind-friendly dark",
            "tokyo night",
            "[Next]",
        ]
    );
    assert_eq!(scheme.focused().map(|o| o.label.as_str()), Some("terminal"));
    let terms = agy(lab::FIRST_RUN_TERMS).prompt.expect("the terms");
    assert_eq!(
        (terms.kind, terms.title.as_str()),
        (PromptKind::Other, anchor_text("agy.onboarding.terms"))
    );
    assert_eq!(terms.options.len(), 3);
    assert!(
        labels(&terms)[0].starts_with("[x] Yes, I agree to help improve Antigravity CLI")
            && labels(&terms)[0].ends_with("whenever I want via my settings."),
        "{}",
        labels(&terms)[0]
    );
    assert_eq!(labels(&terms)[1..], ["[Previous]", "[Done]"]);
    for p in [&scheme, &terms] {
        assert!(roles(p).iter().all(|&r| r == Role::Other), "{:?}", roles(p));
        assert!(p.options.iter().all(|o| o.n.is_none()));
        assert_eq!((p.select, p.cancel.clone()), (Select::ArrowsEnter, None));
        assert!(!p.head_off_screen);
    }
    for text in [lab::FIRST_RUN_SCHEME, lab::FIRST_RUN_TERMS] {
        let r = agy(text);
        assert_eq!((r.wall, r.composer), (None, None));
        let rows = screen(text);
        let head = rows
            .iter()
            .position(|r| ONBOARDING.iter().any(|h| r.starts_with(h)))
            .expect("the heading");
        let mut unnamed = rows.clone();
        unnamed[head] = String::new();
        assert_eq!(prompt(&unnamed), None, "the control");
        assert_eq!(phase(&unnamed), (Phase::Idle, false));
    }

    let lab_trust = agy(lab::FIRST_RUN_TRUST).prompt.expect("the trust box");
    let signed_in = agy(BOX_TRUST).prompt.expect("the trust box");
    assert_eq!(lab_trust.kind, PromptKind::Trust);
    assert_eq!(
        (labels(&lab_trust), roles(&lab_trust), lab_trust.select),
        (labels(&signed_in), roles(&signed_in), signed_in.select)
    );
    assert_eq!(lab_trust.path.as_deref(), Some("/private/tmp/at.tPt6/proj"));
}

/// UNDER AN API KEY THE SCREENS ARE THE SIGNED-IN ONES (module header): each
/// box reads with the same kind, labels, roles, select and Esc as its
/// signed-in capture, the fresh composer is idle and fresh as evidence, and
/// an Esc is the person's interrupt — the logo's account row, the model's
/// name and the spinner's word aside. NEGATIVE CONTROL: where they differ,
/// the reader says so — signed out, agy draws a sign-in WALL; with the
/// provider seeded, its first run draws boxes and no wall.
#[test]
fn the_lab_screens_read_as_the_signed_in_ones() {
    for (signed_in, keyed) in [
        (BOX_EXEC, lab::BOX_SHELL),
        (BOX_CREATE_WORKSPACE, lab::BOX_WRITE),
        (BOX_EDIT, lab::BOX_EDIT),
        (QUESTION, lab::QUESTION),
    ] {
        let a = agy(signed_in).prompt.expect("signed in");
        let b = agy(keyed).prompt.expect("the lab");
        let what = provenance(keyed).unwrap_or_default();
        assert_eq!(a.kind, b.kind, "{what}");
        assert_eq!(roles(&a), roles(&b), "{what}");
        assert_eq!((a.select, &a.cancel), (b.select, &b.cancel), "{what}");
        if a.kind != PromptKind::Bash {
            assert_eq!(labels(&a), labels(&b), "{what}");
        }
    }
    for text in [IDLE, lab::IDLE] {
        let r = agy(text);
        assert_eq!(
            (r.phase, r.phase_authoritative, r.fresh),
            (Phase::Idle, true, true)
        );
    }
    assert!(
        screen(lab::IDLE)
            .iter()
            .any(|r| r.ends_with("Gemini API key"))
    );
    for text in [INTERRUPTED, lab::INTERRUPTED] {
        assert!(agy(text).interrupted);
    }
    let thinking = screen(lab::BUSY_THINKING);
    assert!(thinking.iter().any(|r| r.starts_with("⣽  tick 2...")));

    let out = agy(SIGNED_OUT);
    assert_eq!(out.wall.map(|w| w.kind), Some(WallKind::Auth));
    for text in [lab::FIRST_RUN_SCHEME, lab::FIRST_RUN_TERMS] {
        assert_eq!(agy(text).wall, None);
    }
}

/// WHAT ESC DOES ON A BOX THAT NAMES NONE ([`esc`]), as measured in the lab
/// on 1.4.0 (2026-10-16): on the trust box it QUITS agy (back at the shell
/// within two seconds of the Esc; the box left alone stood) — `Exit`; on the
/// terms screen it goes back to the colour scheme, as its `[Previous]`
/// button does — `Back`. A box that names its Esc says so itself: the
/// command box's footer `esc to cancel` stops the turn (`Interrupt`), the
/// question's key row `esc Skip` leaves it (`Back`). NEGATIVE CONTROLS: the
/// colour-scheme screen, whose Esc changed nothing, says nothing; the terms
/// screen with its `[Previous]` gone says nothing either, so the button is
/// what the reading rests on; and the reader's own hook answers as [`esc`]
/// does ([`ScreenReader::esc`]), where the default reads only a named Esc.
#[test]
fn esc_on_a_box_that_names_none_is_what_agy_was_measured_to_do() {
    use crate::reader::AgyReader;
    let of = |text: &str| esc(&prompt(&screen(text)).expect("a box"));
    assert_eq!(of(BOX_TRUST), Some(CancelEffect::Exit));
    assert_eq!(of(lab::FIRST_RUN_TRUST), Some(CancelEffect::Exit));
    assert_eq!(of(lab::FIRST_RUN_TERMS), Some(CancelEffect::Back));
    assert_eq!(of(BOX_EXEC), Some(CancelEffect::Interrupt));
    assert_eq!(of(lab::FIRST_RUN_SCHEME), None, "the control");
    let terms = screen(lab::FIRST_RUN_TERMS);
    let gone = edited(
        &terms,
        "    [Previous]",
        &terms
            .iter()
            .find(|r| r.starts_with("    [Previous]"))
            .expect("the buttons")
            .replace("[Previous]", "          "),
    );
    let p = prompt(&gone).expect("still the terms screen");
    assert_eq!(esc(&p), None, "the control: no [Previous]");
    for text in [
        BOX_TRUST,
        lab::FIRST_RUN_TERMS,
        lab::FIRST_RUN_SCHEME,
        BOX_EXEC,
    ] {
        let p = prompt(&screen(text)).expect("a box");
        assert_eq!(AgyReader.esc(&p), esc(&p), "the reader's hook");
    }
    let p = prompt(&screen(BOX_TRUST)).expect("a box");
    assert_eq!(
        ClaudeReader.esc(&PromptV2 {
            kind: PromptKind::Other,
            ..p.clone()
        }),
        Some(CancelEffect::Back),
        "Claude Code's reader: Esc leaves every box"
    );
    assert_eq!(CodexReader.esc(&p), None, "the default: only a named Esc");
}

/// THE STATUSES MEASURED IN THE LAB AFTER THE FIRST FIVE map by what agy
/// does with them (its retries, counted in the stub's log) and what time
/// does: a 403 `PERMISSION_DENIED` (with and without its `SERVICE_DISABLED`
/// reason), a 404 `NOT_FOUND` model and a 400 `FAILED_PRECONDITION` are API
/// errors agy did not retry; a 504 one it retried eight times, as it does a
/// bare 429 (no RetryInfo), which limits; a per-minute 429 (its RetryInfo
/// 47 s, more than agy waits) is a rate, an API error 429 that limits, and a
/// per-day one of 41397 s a spend wall — each 429 with a RetryInfo carries
/// its delay as the reset, rounded up to whole seconds. NEGATIVE CONTROLS:
/// the per-day quota's id renamed per-minute is the rate, and the per-minute
/// one renamed per-day the spend wall; the per-minute quota with a per-day
/// (or daily) one beside it, before or after it, is the spend wall, and with
/// a second per-minute one beside it still the rate; with its RetryInfo's
/// word changed no reset is read; a delay wrapped mid-number still reads
/// whole, and a delay in another unit, or none, reads nothing.
#[test]
fn the_measured_statuses_map_by_what_agy_and_time_do_with_them() {
    let api = |code: u16, retryable: bool| WallKind::ApiError {
        code: Some(code),
        retryable,
        cause: ApiCause::Server,
    };
    for (text, kind, reset) in [
        (lab::ERROR_403_DISABLED, api(403, false), None),
        (lab::ERROR_403, api(403, false), None),
        (lab::ERROR_404, api(404, false), None),
        (lab::ERROR_400_LOCATION, api(400, false), None),
        (lab::ERROR_504, api(504, true), None),
        (lab::ERROR_429_BARE, api(429, true), None),
        (lab::ERROR_429_PER_MINUTE, api(429, true), Some("in 48s")),
        (lab::ERROR_429_LONG, WallKind::Spend, Some("in 41398s")),
    ] {
        let what = provenance(text).unwrap_or_default();
        let r = agy(text);
        let w = r.wall.clone().expect("the wall");
        assert_eq!(
            (w.kind, w.reset.as_deref(), w.placement),
            (kind, reset, Placement::Banner),
            "{what}"
        );
        assert_eq!(
            matches!(r.phase, Phase::Limited { .. }),
            kind.reads_limited(),
            "{what}"
        );
        if let Phase::Limited { reset: limited, .. } = &r.phase {
            assert_eq!(limited.as_deref(), reset, "{what}");
        }
        assert!(r.phase_authoritative, "{what}");
    }
    assert!(
        agy(lab::ERROR_403_DISABLED)
            .wall
            .is_some_and(|w| w.message.contains("SERVICE_DISABLED")),
        "the reason is in the message the person is told"
    );

    let swap = |text: &str, from: &str, to: &str| -> Vec<String> {
        let rows = screen(text);
        assert!(rows.iter().any(|r| r.contains(from)), "{from}");
        rows.iter().map(|r| r.replace(from, to)).collect()
    };
    let minute = swap(lab::ERROR_429_LONG, "PerDayPer", "PerMinutePer");
    assert_eq!(wall(&minute).map(|w| w.kind), Some(api(429, true)));
    let day = swap(lab::ERROR_429_PER_MINUTE, "PerMinutePer", "PerDayPer");
    assert_eq!(wall(&day).map(|w| w.kind), Some(WallKind::Spend));
    // A per-day violation beside the per-minute one is the day's (Google's
    // own client: a `PerDay` or `Daily` quota id is terminal before anything
    // else is read), whichever of the two the SDK prints first.
    for second in [
        "GenerateRequestsPerDayPerProjectPerModel-FreeTier",
        "GenerateContentDailyRequestsPerProject",
    ] {
        let both = swap(
            lab::ERROR_429_PER_MINUTE,
            "quotaValue:5]]]",
            &format!("quotaValue:5] map[quotaId:{second} quotaValue:250]]]"),
        );
        assert_eq!(
            wall(&both).map(|w| w.kind),
            Some(WallKind::Spend),
            "{second}"
        );
        let first = swap(
            lab::ERROR_429_PER_MINUTE,
            "violations:[map[",
            &format!("violations:[map[quotaId:{second} quotaValue:250] map["),
        );
        assert_eq!(
            wall(&first).map(|w| w.kind),
            Some(WallKind::Spend),
            "{second}"
        );
    }
    let two_minutes = swap(
        lab::ERROR_429_PER_MINUTE,
        "quotaValue:5]]]",
        "quotaValue:5] map[quotaId:GenerateContentInputTokensPerModelPerMinute-FreeTier quotaValue:250000]]]",
    );
    assert_eq!(
        wall(&two_minutes).map(|w| w.kind),
        Some(api(429, true)),
        "the control: two per-minute quotas are a rate"
    );
    let unread = swap(lab::ERROR_429_LONG, "retryDelay:", "retryAfter:");
    assert_eq!(
        wall(&unread).and_then(|w| w.reset),
        None,
        "no RetryInfo read"
    );

    for (message, delay) in [
        ("map[@type:…RetryInfo retryDelay:3600s]]", Some("in 3600s")),
        ("retryDelay:41397.204181s]]", Some("in 41398s")),
        ("retryDelay:413 97.204181s]]", Some("in 41398s")),
        ("retryDelay:12.000s]", Some("in 12s")),
        ("retryDelay:30m]", None),
        ("retryDelay:s]", None),
        ("no delay at all", None),
    ] {
        assert_eq!(retry_delay(message).as_deref(), delay, "{message}");
    }
}

/// AGY'S `not signed in` SPLASH — the words its 1.4.0 binary holds for a
/// screen that says so with no login menu under it (`Welcome back! You are
/// currently not signed in.`, `No authentication methods available.. You are
/// currently not signed in.`; a first run was seen to flash one before its
/// colour picker, and the lab could not draw it again on 1.4.0 in six
/// launches under its seeds: a first run, an untrusted one, a plain start, a
/// dead proxy and a cold copy of the binary, every byte of each recorded) —
/// is no box, no wall and no evidence: idle NOT authoritatively, with no
/// composer, survey or last words, and not named agy's by its layout.
/// NEGATIVE CONTROL: the sign-in wall, whose login menu stands under the
/// same words, is the login wall.
#[test]
fn the_not_signed_in_splash_is_no_box_and_no_evidence() {
    for words in [
        "Welcome back! You are currently not signed in.",
        "No authentication methods available.. You are currently not signed in.",
    ] {
        let rows = rows(&["", words, "", ""]);
        let r = read(Some("agy"), &rows, None);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, false),
            "{words}"
        );
        assert_eq!(
            (r.prompt, r.wall, r.composer, r.said_tail),
            (None, None, None, None),
            "{words}"
        );
        assert!(!r.survey, "{words}");
        assert!(!is_agy_screen(&rows), "{words}");
    }
    let signed_out = agy(SIGNED_OUT);
    assert_eq!(signed_out.wall.map(|w| w.kind), Some(WallKind::Auth));
}
