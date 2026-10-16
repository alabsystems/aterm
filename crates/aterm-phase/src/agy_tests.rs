// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One test per measured agy screen (agy 1.4.0), each with its negative
//! control.

use super::fixtures::*;
use super::*;
use crate::prompt::fixtures::{cursor, provenance, rows, screen};
use crate::reader::{ClaudeReader, CodexReader, Program, ScreenReader, identify, read};
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

    // Under the tip: the row above it must be the spinner.
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

/// One ctrl+c at the idle composer turns the footer into `press ctrl+c again
/// to exit`: agy is still idle at its composer. After the second, agy has
/// left: its last frame stays above its resume line and the shell's prompt,
/// and nothing on that screen is agy's to read — no composer, no evidence,
/// and the screen names no program. NEGATIVE CONTROL: the frame it left,
/// with its footer back and the shell's rows gone, is a composer again.
#[test]
fn one_ctrl_c_is_still_idle_and_an_exited_agy_is_read_as_nothing() {
    let r = agy(EXIT_CONFIRM);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert_eq!(
        r.said_tail.as_deref(),
        Some("You selected Blue for the logo.")
    );
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
/// trust box and its sign-in wall — but the screen agy left. A screen with
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
        if file.contains("-exited-") {
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
/// Code, Codex and Muse Code under `src/fixtures/`, agy's layout test says
/// no, the screen is never identified as agy's, and agy's reader reads no
/// box on it. The control: it says yes on agy's own (but the one agy left).
#[test]
fn the_agy_sniff_claims_no_other_programs_screen() {
    let mut swept = 0;
    for (name, text) in crate::text::fixture_files() {
        let r = screen(&text);
        if name.starts_with("agy-") {
            assert_eq!(is_agy_screen(&r), text != EXITED, "{name}");
            continue;
        }
        swept += 1;
        assert!(!is_agy_screen(&r), "{name}");
        assert_ne!(identify(None, &r).program(), Program::Agy, "{name}");
        assert_eq!(prompt(&r), None, "{name}");
    }
    assert!(swept > 150, "only {swept} other captures");
}
