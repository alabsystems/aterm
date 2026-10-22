// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One test per measured Muse Code screen (muse 1.4.9-R6297.1 and
//! 1.4.10-R6427.1), each with its negative control.

use super::fixtures::*;
use super::*;
use crate::prompt::fixtures::{cursor, rows, screen};
use crate::reader::{
    AgyReader, ClaudeReader, CodexReader, MuseReader, Program, ScreenReader, identify, read,
};
use crate::wall::WallKind;

fn muse(text: &str) -> crate::reader::Reading {
    read(Some("muse"), &screen(text), None)
}

fn roles(p: &PromptV2) -> Vec<Role> {
    p.options.iter().map(|o| o.role).collect()
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

/// Every Muse Code capture under `src/fixtures/`, by file name.
fn captures() -> Vec<(String, String)> {
    crate::text::fixture_files()
        .into_iter()
        .filter(|(name, _)| name.starts_with("muse-"))
        .collect()
}

/// A fresh session at its composer is idle as evidence: the banner, no
/// message yet. The composer is the caret and NOTHING (no placeholder), on
/// the row the cursor parks on, at column 2 (measured). NEGATIVE CONTROL:
/// the busy screen's composer is the same empty caret row, and the phase is
/// busy.
#[test]
fn a_fresh_session_is_idle_at_its_composer() {
    let r = muse(IDLE);
    assert_eq!(r.program, Program::Muse);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert!(r.fresh);
    assert_eq!((r.prompt, r.wall, r.said_tail), (None, None, None));
    let (row, col) = cursor(IDLE).expect("measured");
    assert_eq!(r.composer, Some((row, vec![String::new()])));
    assert_eq!(col, 2);
    let busy = muse(BUSY);
    assert_eq!(busy.phase, Phase::Busy, "the control");
    assert_eq!(busy.composer.map(|(_, l)| l), Some(vec![String::new()]));
}

/// THE STATUS ROW OVER THE OPEN COMPOSER IS BUSY (`◇ Thinking (2s · esc to
/// interrupt)`, the diamond cycling ◇ ◈ ◆ and the verb `Working` first): no
/// last words yet, no turn's end — on the live `◈` capture too, whose frames
/// once read `unknown` between the busy ones. NEGATIVE CONTROLS: a glyph
/// muse was never measured drawing is no status row; the status row gone with
/// nothing under the message is a turn sent whose row is not drawn yet —
/// busy, NOT as evidence; the status row turned into the reply is the turn's
/// end, as evidence.
#[test]
fn the_status_row_is_busy() {
    for text in [BUSY, BUSY_ALT_GLYPH] {
        let r = muse(text);
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Busy, true));
        assert_eq!(r.said_tail, None);
        assert!(!r.fresh);
    }
    let rows = screen(BUSY);
    for row in [
        "◆ Thinking (3s · esc to interrupt)",
        "◇ Thinking (3s · esc to interrupt)",
        "◈ Thinking (3s · esc to interrupt)",
        "◈ Working (0s · esc to interrupt)",
    ] {
        let other = edited(&rows, "◇ Thinking", row);
        assert_eq!(phase(&other), (Phase::Busy, true), "{row}");
    }
    let unmeasured = edited(&rows, "◇ Thinking", "◉ Thinking (3s · esc to interrupt)");
    assert_ne!(phase(&unmeasured), (Phase::Busy, true), "the control");
    let sent = edited(&rows, "◇ Thinking", "");
    assert_eq!(phase(&sent), (Phase::Busy, false));
    let ended = edited(&rows, "◇ Thinking", "◆ echo: hello there");
    assert_eq!(phase(&ended), (Phase::Idle, true), "the control");
}

/// A turn ends on its reply (`◆ <text>`, the diamond bold): idle as
/// evidence, the reply the last words. NEGATIVE CONTROLS: a reply that asks
/// is a question; and a row of no shape this grammar measured between the
/// reply and the composer — where an unmeasured box would be drawn — leaves
/// `idle` with no evidence.
#[test]
fn a_turn_ends_on_its_reply() {
    let r = muse(END_OF_TURN);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert_eq!(r.said_tail.as_deref(), Some("echo: hello there"));
    assert!(!r.fresh && !r.interrupted);
    let rows = screen(END_OF_TURN);
    let asks = edited(&rows, "◆ echo:", "◆ echo: hello there?");
    assert_eq!(phase(&asks), (Phase::Question, true));
    let reply = rows
        .iter()
        .position(|r| r.starts_with("◆ echo:"))
        .expect("the reply");
    let mut unknown = rows.clone();
    unknown[reply + 1] = "Something unmeasured".to_string();
    assert_eq!(phase(&unknown), (Phase::Idle, false));
    assert_eq!(said_tail(&unknown), None);
}

/// A typed draft is the composer's text, the cursor after it (measured): idle
/// as evidence, the session still fresh. NEGATIVE CONTROL: the idle
/// composer's text is empty.
#[test]
fn an_unsent_draft_is_the_composers() {
    let r = muse(DRAFT);
    let (row, col) = cursor(DRAFT).expect("measured");
    assert_eq!(r.composer, Some((row, vec!["hello there".to_string()])));
    assert_eq!(col, 2 + "hello there".len());
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert!(r.fresh);
    assert_eq!(
        muse(IDLE).composer.map(|(_, l)| l),
        Some(vec![String::new()])
    );
}

/// Esc in a turn RETRACTS it: the message leaves the transcript and comes
/// back as the composer's draft, and no marker is drawn. The screen is then
/// EXACTLY the screen of a person who typed that draft after the turn
/// before — so it is read as one: an ended turn with a draft, no interrupt
/// (the default). NEGATIVE CONTROL: the end-of-turn screen with the same
/// draft typed is row for row the same screen.
#[test]
fn a_retracted_turn_is_a_draft_no_reader_can_tell_from_a_typed_one() {
    let r = muse(INTERRUPTED);
    let (row, col) = cursor(INTERRUPTED).expect("measured");
    assert_eq!(r.composer, Some((row, vec!["third prompt".to_string()])));
    assert_eq!(col, 2 + "third prompt".len());
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert!(!r.interrupted);
    assert_eq!(r.said_tail.as_deref(), Some("echo: hello there"));
    let typed = edited(&screen(END_OF_TURN), "❯", "❯ third prompt");
    assert_eq!(typed, screen(INTERRUPTED), "the same screen");
}

/// `/` opens the command menu BETWEEN the caret row and the bottom rule
/// (measured), the footer unchanged: the composer's draft is the `/` alone,
/// not the menu's rows — its `↑ N more` once scrolled included — and a
/// session with no turn yet is still fresh, the startup warning rows above
/// it notwithstanding. NEGATIVE CONTROL: rows of the same shape under a
/// draft that does not open with `/` are the draft's.
#[test]
fn the_command_menu_is_not_the_draft() {
    let r = muse(SLASH_MENU);
    let (row, col) = cursor(SLASH_MENU).expect("measured");
    assert_eq!(r.composer, Some((row, vec!["/".to_string()])));
    assert_eq!(col, 3);
    assert!(r.fresh);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert_eq!(r.prompt, None);
    let mut scrolled = screen(SLASH_MENU);
    scrolled.insert(row + 1, "  ↑ 3 more".to_string());
    assert_eq!(
        composer_draft(&scrolled),
        Some((row, vec!["/".to_string()]))
    );
    let rows = edited(&scrolled, "❯ /", "❯ look at");
    let (_, lines) = composer_draft(&rows).expect("the composer");
    assert_eq!(lines[0], "look at");
    assert!(lines.len() > 2 && lines[1] == "↑ 3 more", "{lines:?}");
    assert!(lines[2].starts_with("/bug"), "{lines:?}");
}

/// One ctrl+c at an empty composer turns the footer into `Press Ctrl-C again
/// to quit`: still idle at its composer. After the second, Muse Code erased
/// its composer and printed its resume line: no composer, no evidence, and
/// the screen names no program. CONTROL: the first screen is the
/// end-of-turn screen but for its footer, and reads as that one does.
#[test]
fn one_ctrl_c_is_still_idle_and_an_exited_muse_is_read_as_nothing() {
    let r = muse(EXIT_CONFIRM);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert_eq!(r.composer.map(|(_, l)| l), Some(vec![String::new()]));
    let gone = muse(EXITED);
    assert_eq!((gone.phase, gone.phase_authoritative), (Phase::Idle, false));
    assert_eq!(gone.composer, None);
    assert_eq!(identify(None, &screen(EXITED)).program(), Program::Generic);
    let footer = screen(EXIT_CONFIRM);
    let idle = screen(END_OF_TURN);
    assert_eq!(footer[..footer.len() - 1], idle[..idle.len() - 1]);
    assert_eq!(phase(&footer), phase(&idle), "the control");
}

/// The trust box Muse Code draws in a new folder: a Trust prompt for its
/// folder, `1  Trust and continue` the trust and `2  Quit` the exit, the
/// first focused (`⟩`), chosen by the cursor (or its digit) and then Enter,
/// its Esc quitting Muse Code. NEGATIVE CONTROLS: both rows focused and no
/// option carries a role; Claude Code's reader sees no box on it.
#[test]
fn the_trust_box_is_a_trust_prompt_chosen_by_enter() {
    let r = muse(BOX_TRUST);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    let p = r.prompt.expect("the box");
    assert_eq!(p.kind, PromptKind::Trust);
    assert_eq!(p.title, anchor_text("muse.trust.title"));
    assert_eq!(p.path.as_deref(), Some("/private/tmp/at.XXXXXX/mwork"));
    let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, vec!["Trust and continue", "Quit"]);
    assert_eq!(roles(&p), vec![Role::Trust, Role::Exit]);
    assert!(p.options[0].focused && !p.options[1].focused);
    assert_eq!(p.select, Select::ArrowsEnter);
    assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Exit));
    let both = edited(&screen(BOX_TRUST), "  2  Quit", "⟩ 2  Quit");
    let p = prompt(&both).expect("still a box");
    assert!(roles(&p).iter().all(|&r| r == Role::Other));
    assert_eq!(ClaudeReader.prompt(&screen(BOX_TRUST)), None, "the control");
}

/// SIGNED OUT, Muse Code draws its login menu instead of a composer: a WALL
/// (the login is gone), never a box — no prompt is read, so no option of it
/// (`Log in with browser` opens a browser on the person's desktop) is ever
/// pressed — idle as evidence on its wall. NEGATIVE CONTROL: the menu with
/// its key row gone is no wall.
#[test]
fn the_sign_in_wall_is_a_wall_and_never_a_box() {
    let rows = screen(SIGNED_OUT);
    let r = muse(SIGNED_OUT);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    let w = r.wall.expect("the wall");
    assert_eq!(w.kind, WallKind::Auth);
    assert!(w.message.starts_with(anchor_text("muse.signin.browser")));
    assert_eq!((r.prompt, r.composer), (None, None));
    assert_eq!(prompt(&rows), None);
    assert_eq!(crate::reader::MuseReader.prompt(&rows), None);
    let keyless = edited(&rows, "  ↓↑", "");
    assert_eq!(wall(&keyless), None);
    assert_eq!(phase(&keyless), (Phase::Idle, false));
}

/// Claude Code's, Codex's and agy's readers on Muse Code's screens — its
/// approval boxes and its question among them: never a box, never a
/// question, and agy's layout test claims none. Claude
/// Code's FRAME test does take Muse Code's composer for its own (its caret
/// is `❯`, its top rule a rule) — the measured reason
/// [`crate::reader::identify`] asks Muse Code's layout first — so where the
/// reply's own words end on a question, as Muse Code's reader reads them
/// too (the 50-column capture's `(1 or 2)?`), it reads that prose question
/// and no more. THE CONTROL: Muse Code's own reader sees the box on every
/// box screen.
#[test]
fn the_other_readers_see_no_box_and_no_question_on_muse_screens() {
    let mut boxes = 0;
    for (name, text) in captures() {
        let r = screen(&text);
        let asks = MuseReader.phase(&r) == Phase::Question && MuseReader.prompt(&r).is_none();
        for reader in [&ClaudeReader as &dyn ScreenReader, &CodexReader, &AgyReader] {
            let phase = reader.phase(&r);
            assert!(
                phase != Phase::Prompt && (phase != Phase::Question || asks),
                "{:?} on {name}: {phase:?}",
                reader.program()
            );
            assert_eq!(reader.prompt(&r), None, "{name}");
        }
        assert!(!crate::agy::is_agy_screen(&r), "{name}");
        if MuseReader.prompt(&r).is_some() {
            boxes += 1;
        }
    }
    assert_eq!(
        boxes, 58,
        "the trust boxes, the approval boxes, the questions, the reviews"
    );
    for text in [IDLE, BUSY, DRAFT, END_OF_TURN, QUESTION, USAGE_WALL] {
        let r = screen(text);
        assert!(crate::phase::has_composer_frame(&r), "Claude Code's frame");
        assert_eq!(identify(None, &r).program(), Program::Muse);
    }
}

/// A shell, a pager or a viewer showing Muse Code's screen is not Muse Code:
/// the program's NAME wins. With no name, or a runtime's, EVERY screen it
/// draws names it — its composer idle, busy, with a draft or a menu, its
/// trust box and its sign-in wall — but the screen it left. A screen with
/// its words and no composer under its labelled rule names nothing.
/// NEGATIVE CONTROL: the same composer under `muse`.
#[test]
fn a_shell_quoting_muse_is_not_muse() {
    let r = screen(END_OF_TURN);
    for shell in ["zsh", "-bash", "less", "cat"] {
        assert_eq!(
            read(Some(shell), &r, None).program,
            Program::Generic,
            "{shell}"
        );
    }
    assert_eq!(identify(Some("muse"), &r).program(), Program::Muse);
    for (file, text) in captures() {
        if file.contains("-exited-") {
            assert_eq!(identify(None, &screen(&text)).program(), Program::Generic);
            continue;
        }
        for name in [None, Some("node")] {
            assert_eq!(
                identify(name, &screen(&text)).program(),
                Program::Muse,
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
        "% grep -h Thinking log.txt",
        "◇ Thinking (2s · esc to interrupt)",
        "── Voice input (⌥ + v to start) ──",
        "%",
    ]);
    assert_eq!(identify(None, &quoted).program(), Program::Generic);
}

/// THE SNIFF CLAIMS NO OTHER PROGRAM'S SCREEN, AND THE READER READS NO BOX
/// OR WALL ON ONE: over every capture of Claude Code, agy and Gemini CLI under
/// `src/fixtures/`, Muse Code's layout test says no, no screen is identified
/// as its — Claude Code's own composer, which its frame test shares,
/// included — and its reader finds no box, no prompt phase, no wall of its
/// own shapes, no retry and no interrupt there. Claude Code's AskUserQuestion
/// (`Enter to select · … · Esc to cancel`) is no question of Muse Code's.
/// The control: it says yes on Muse Code's own (but the screens it left).
#[test]
fn the_muse_sniff_claims_no_other_programs_screen() {
    let mut swept = 0;
    for (name, text) in crate::text::fixture_files() {
        let r = screen(&text);
        if name.starts_with("muse-") {
            assert_eq!(is_muse_screen(&r), !name.contains("-exited-"), "{name}");
            continue;
        }
        swept += 1;
        assert!(!is_muse_screen(&r), "{name}");
        assert_ne!(identify(None, &r).program(), Program::Muse, "{name}");
        assert_eq!(prompt(&r), None, "{name}");
        assert_ne!(phase(&r).0, Phase::Prompt, "{name}");
        assert!(approval_box(&r).is_none(), "{name}");
        assert!(
            frame(&r).is_none_or(|f| question_box(&r, f).is_none()),
            "{name}"
        );
        assert_eq!(wall(&r), None, "{name}");
        assert_eq!(retrying(&r), None, "{name}");
        assert!(!interrupted(&r), "{name}");
        assert_eq!(background_wait(&r), None, "{name}");
    }
    assert!(swept > 150, "only {swept} other captures");
}

/// What one screen says, to compare two builds' screens of one shape: the
/// phase and its evidence, freshness, the box's roles, the composer's row
/// count, whether there are last words.
type Shape = (Phase, bool, bool, Option<Vec<Role>>, Option<usize>, bool);

fn shape(text: &str) -> Shape {
    let r = muse(text);
    (
        r.phase,
        r.phase_authoritative,
        r.fresh,
        r.prompt.as_ref().map(roles),
        r.composer.map(|(_, l)| l.len()),
        r.said_tail.is_some(),
    )
}

/// THE 1.4.9 SCREENS READ THE SAME ON 1.4.10: idle, a draft, a running turn,
/// a turn's end, the `/` menu, one ctrl+c, the exit and the trust box, each
/// captured on both builds (1.4.10's footer names the effort too:
/// `lab-stub · high · <path> · Auto-review`), read alike. NEGATIVE CONTROL:
/// the comparison tells shapes apart — the running turn is not its end.
#[test]
fn the_1_4_9_screens_read_the_same_on_1_4_10() {
    for (old, new) in [
        (IDLE, IDLE_1_4_10),
        (DRAFT, DRAFT_1_4_10),
        (BUSY, BUSY_1_4_10),
        (END_OF_TURN, END_OF_TURN_1_4_10),
        (SLASH_MENU, SLASH_MENU_1_4_10),
        (EXIT_CONFIRM, EXIT_CONFIRM_1_4_10),
        (EXITED, EXITED_1_4_10),
        (BOX_TRUST, TRUST_1_4_10),
    ] {
        assert_eq!(
            shape(old),
            shape(new),
            "{}",
            new.lines().next().unwrap_or("")
        );
    }
    assert_ne!(shape(BUSY_1_4_10), shape(END_OF_TURN_1_4_10), "the control");
    assert_eq!(
        muse(DRAFT_1_4_10).composer.map(|(_, l)| l),
        Some(vec!["a draft, not sent".to_string()])
    );
    let p = muse(TRUST_1_4_10).prompt.expect("the box");
    assert_eq!(p.path.as_deref(), Some("/private/tmp/at.lab0/proj"));
}

/// THE SHELL BOX (1.4.10) IS A BASH PROMPT drawn in the composer's frame
/// with no caret row: its command, `Yes, proceed` the one-shot allow and
/// `No, and tell Muse Code …` the refusal, the first focused, their one-key
/// marks (`(y)`, `(esc)`) stripped, chosen by the cursor and Enter, its Esc
/// the refusal. While it is up there is no composer, no last words and no
/// wall, and the status row `◆ Calling tools (… esc to interrupt)` above it
/// does not make it busy. NEGATIVE CONTROLS: with the caret row back in the
/// box's place the screen is that running turn (busy); with the cursor on
/// both options no option carries a role.
#[test]
fn the_shell_box_is_a_bash_prompt_chosen_by_the_cursor_and_enter() {
    let r = muse(BOX_SHELL);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Prompt, true)
    );
    assert_eq!(
        (r.composer.clone(), r.said_tail.clone(), r.wall.clone()),
        (None, None, None)
    );
    let p = r.prompt.expect("the box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.title, anchor_text("muse.box.shell"));
    assert_eq!(p.command, "touch probe.txt");
    assert_eq!(p.readings(), vec!["touch probe.txt".to_string()]);
    let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![anchor_text("muse.box.once"), anchor_text("muse.box.deny")]
    );
    assert_eq!(roles(&p), vec![Role::Once, Role::Deny]);
    assert!(p.options[0].focused && !p.options[1].focused);
    assert_eq!(p.select, Select::ArrowsEnter);
    let cancel = p.cancel.expect("the (esc) option");
    assert_eq!(
        (cancel.key.as_str(), cancel.effect),
        ("esc", CancelEffect::Reject)
    );
    assert_eq!((p.footer, p.head_off_screen), (None, false));

    let rows = screen(BOX_SHELL);
    let top = rows
        .iter()
        .position(|r| r.starts_with("── Voice input"))
        .expect("the frame");
    let bottom = rows
        .iter()
        .rposition(|r| plain_rule_width(r).is_some())
        .expect("the bottom rule");
    let mut composer = rows[..=top].to_vec();
    composer.push("❯".to_string());
    composer.extend_from_slice(&rows[bottom..]);
    assert!(approval_box(&composer).is_none());
    assert_eq!(phase(&composer), (Phase::Busy, true), "the control");
    let both = edited(
        &rows,
        "  2. No,",
        "⟩ 2. No, and tell Muse Code what to do differently (esc)",
    );
    let p = prompt(&both).expect("still a box");
    assert!(roles(&p).iter().all(|&r| r == Role::Other));
    assert_eq!(phase(&both), (Phase::Prompt, true));
    // The message's `❯` row under a rule is still no composer: the box's
    // frame lies between it and the bottom rule.
    let mut ruled = rows.clone();
    let message = ruled
        .iter()
        .position(|r| r.starts_with("❯ toolstart"))
        .expect("the message");
    ruled[message - 1] = "─".repeat(40);
    assert_eq!(composer_draft(&ruled), None);
    assert_eq!(phase(&ruled), (Phase::Prompt, true));
}

/// THE FILE-ACCESS BOX (1.4.10) IS A WRITE OF ITS PATH: a write under
/// `.agents/` and an edit of the same file draw the same box, `write path:
/// <absolute path>` its body, with the shell box's options and roles.
/// NEGATIVE CONTROLS: the same box naming a `read path:` — a body this
/// grammar has not measured — and the binary's network box are boxes of no
/// kind, no option of them carrying a role.
#[test]
fn the_file_access_box_is_a_write_of_its_path() {
    for text in [BOX_WRITE, BOX_EDIT] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Prompt, true)
        );
        let p = r.prompt.expect("the box");
        assert_eq!(p.kind, PromptKind::Write);
        assert_eq!(p.title, anchor_text("muse.box.file"));
        assert_eq!(
            p.path.as_deref(),
            Some("/private/tmp/at.lab0/proj/.agents/probe-write.md")
        );
        assert_eq!(Some(p.command.as_str()), p.path.as_deref());
        assert!(p.command_rows.is_empty());
        assert_eq!(roles(&p), vec![Role::Once, Role::Deny]);
    }
    let rows = screen(BOX_WRITE);
    let read = edited(
        &rows,
        "  write path:",
        "  read path: /private/tmp/at.lab0/proj/.agents/probe-write.md",
    );
    let network = edited(
        &rows,
        "Would you like",
        "Would you like to allow this network access?",
    );
    for other in [read, network] {
        assert_eq!(phase(&other), (Phase::Prompt, true));
        let p = prompt(&other).expect("a box");
        assert_eq!((p.kind, p.path.as_deref()), (PromptKind::Other, None));
        assert!(roles(&p).iter().all(|&r| r == Role::Other));
    }
}

/// ANY YES BUT `Yes, proceed` IS A STANDING GRANT: the binary's `Yes, and
/// don't ask again for this command in this session` (not drawn in the lab)
/// grants the rest of the session, a [`Role::Session`], and any other yes is
/// a [`Role::Persist`] — neither of which any rule presses — each one-key
/// mark stripped. NEGATIVE CONTROLS: a label of no yes or no is
/// [`Role::Other`], and the one-shot allow and the refusal keep their roles
/// beside them.
#[test]
fn any_other_yes_is_a_standing_grant() {
    let rows = screen(BOX_SHELL);
    let at = rows
        .iter()
        .position(|r| r.starts_with("  2. No,"))
        .expect("the refusal");
    let mut four = rows.clone();
    four[at] = "  2. Yes, and don't ask again for this command in this session (a)".to_string();
    four.insert(at + 1, "  3. Show the whole command".to_string());
    four.insert(
        at + 2,
        "  4. No, and tell Muse Code what to do differently (esc)".to_string(),
    );
    let p = prompt(&four).expect("the box");
    assert_eq!(
        roles(&p),
        vec![Role::Once, Role::Session, Role::Other, Role::Deny]
    );
    assert_eq!(
        p.options[1].label,
        "Yes, and don't ask again for this command in this session"
    );
    let forever = edited(
        &four,
        "  2. Yes,",
        "  2. Yes, and don't ask again for this command (a)",
    );
    assert_eq!(
        roles(&prompt(&forever).expect("the box")),
        vec![Role::Once, Role::Persist, Role::Other, Role::Deny]
    );
}

/// AN ANSWERED BOX ENDS THE TURN ON THE MODEL'S REPLY (1.4.10): Enter on the
/// shell box ran the command (`◆ Ran command · …`), Enter on the file box
/// wrote the file (`◆ Wrote … (+1)` and its diff), Esc refused each (`◆ Ran
/// touch probe.txt — denied` over `└ approval aborted`) — and the reply
/// under each is idle as evidence, its words the last words, no box left.
/// NEGATIVE CONTROL: with the reply gone the turn ends on Muse Code's own
/// tool row, which is no word of the agent's.
#[test]
fn an_answered_box_ends_the_turn_on_the_reply() {
    for text in [
        BOX_SHELL_APPROVED,
        BOX_SHELL_DENIED,
        BOX_WRITE_APPROVED,
        BOX_EDIT_DENIED,
    ] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true)
        );
        assert_eq!((r.prompt.clone(), r.wall.clone()), (None, None));
        assert!(
            r.said_tail
                .as_deref()
                .is_some_and(|s| s.starts_with("Tool call done. Result:")),
            "{:?}",
            r.said_tail
        );
        assert!(!r.interrupted);
    }
    let rows = screen(BOX_SHELL_DENIED);
    let reply = rows
        .iter()
        .position(|r| r.starts_with("◆ Tool call done"))
        .expect("the reply");
    let mut tool_row_last = rows.clone();
    tool_row_last.remove(reply);
    assert_eq!(phase(&tool_row_last), (Phase::Idle, true));
    assert_eq!(said_tail(&tool_row_last), None, "the control");
}

/// A TURN ENDED ON MUSE CODE'S READ ROW IS OVER, WITH NO WORDS (1.4.10,
/// measured live 2026-10-21, `docs/measured/forever-lab-2026-10-20.md`
/// "Residuals, live"): the turn's `read_file` call drawn as `  Read
/// README.md`, two columns in and no `◆`, under a blank row, with no reply
/// under it. FAILS WITHOUT THE FIX: no `◆` head under the message, so the
/// grammar read nothing it knew (idle NOT as evidence) and the supervisor
/// never judged the turn — the session waited for ever. It is idle as
/// evidence with no last words, as a turn ended on `◆ Ran command · …` is;
/// a message wrapped onto a second row over the blank changes nothing; a
/// recap under the read row is Muse Code's own; and the same read row over
/// a reply (the measured `Read /etc/hosts` over `◆ Tool call done. …`)
/// leaves the reply the last words. NEGATIVE CONTROLS: the read row under a
/// status row is busy; a row two columns in that opens with `Read ` but
/// names no path is no read row, and neither is the read row with the
/// message's own wrapped row directly over it (no blank between): nothing
/// measured, idle NOT as evidence, as before.
#[test]
fn a_turn_ended_on_its_read_row_is_over_with_no_words() {
    const READ: &str = "  Read README.md";
    let rows = screen(END_OF_TURN_READ_ROW);
    let at = rows.iter().position(|r| r == READ).expect("the read row");
    assert!(message_drawn(&rows));
    assert_eq!(phase(&rows), (Phase::Idle, true));
    assert_eq!(said_tail(&rows), None);
    let r = muse(END_OF_TURN_READ_ROW);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative, r.prompt.clone()),
        (Phase::Idle, true, None)
    );
    assert_eq!((r.wall.clone(), r.said_tail.clone()), (None, None));
    let with = |edits: &[(usize, &str)]| {
        let mut out = rows.clone();
        for &(i, row) in edits {
            out.insert(i, row.to_string());
        }
        out
    };
    let wrapped = {
        let mut out = rows.clone();
        out.insert(at - 1, "  and every test it needs".to_string());
        out
    };
    assert_eq!(phase(&wrapped), (Phase::Idle, true), "{wrapped:#?}");
    let recap = with(&[(at + 1, ""), (at + 2, "◆ Recap: the parser is next")]);
    assert_eq!(phase(&recap), (Phase::Idle, true));
    assert_eq!(said_tail(&recap), None);
    const REPLY: &str = "Tool call done. Result: Read text file README.md.";
    let replied = with(&[(at + 1, ""), (at + 2, &format!("◆ {REPLY}"))]);
    assert_eq!(phase(&replied), (Phase::Idle, true));
    assert_eq!(said_tail(&replied).as_deref(), Some(REPLY));
    // NEGATIVE CONTROLS.
    let running = with(&[(at + 1, ""), (at + 2, "◇ Thinking (3s · esc to interrupt)")]);
    assert_eq!(phase(&running), (Phase::Busy, true));
    let mut sentence = rows.clone();
    sentence[at] = "  Read the docs before you start".to_string();
    assert_eq!(phase(&sentence), (Phase::Idle, false));
    let mut glued = rows.clone();
    glued[at - 1] = "  and every test it needs".to_string();
    assert_eq!(phase(&glued), (Phase::Idle, false));
    assert_eq!(said_tail(&glued), None);
}

/// EVERY TOOL ROW THE LAB MEASURED ENDS A TURN WITH NO WORDS (1.4.10, `lab
/// muse-captures --only tool-row`, 2026-10-22): a turn the stub answered
/// with one call and, after its result, a reply holding none is over, idle
/// as evidence, and said nothing — on a search row two columns in (`
/// Searched "parser" (1 match)`, `(no matches)`), as on the read row, and on
/// each `◆` row of Muse Code's own: `Wrote`/`Edited` over a diff, `Ran … —
/// failed` over its branch, `Read memory … — failed` and `Used tool
/// operation` over a branch in column 0, `Saved memory …`, `Read goal …`.
/// FAILS WITHOUT THE FIX: the search row was nothing the grammar knew (idle
/// NOT as evidence: the turn never judged), a branch in column 0 was taken
/// for the reply's head (the same), and `Saved memory notes.md` or `Read
/// goal goal — no effect · ctrl+o` were read as the agent's last words.
/// `write_todos` draws nothing: its end reads as a message not answered yet
/// (busy, not evidence), and nothing in sight tells the two apart.
/// NEGATIVE CONTROLS: a sentence two columns in that opens `Searched `, one
/// whose pattern is not in double quotes though a count follows it, and a
/// search row with no count, are no tool rows; the agent's own sentences
/// opening `Saved memory`, `Read goal` and `Used tool` are its words; a
/// branch row under the message with no `◆` row over it is nothing known.
#[test]
fn a_turn_ended_on_any_measured_tool_row_is_over_with_no_words() {
    for (name, text, row) in [
        ("search", TOOL_ROW_SEARCH, "  Searched \"parser\" (1 match)"),
        (
            "search-none",
            TOOL_ROW_SEARCH_NONE,
            "  Searched \"no-such-word-here\" (no matches)",
        ),
        ("write", TOOL_ROW_WRITE, "◆ Wrote notes.txt (+1)"),
        ("edit", TOOL_ROW_EDIT, "◆ Edited README.md (+1 −1)"),
        ("bash", TOOL_ROW_BASH, "◆ Ran ls — failed"),
        (
            "read-memory",
            TOOL_ROW_READ_MEMORY,
            "◆ Read memory notes.md — failed",
        ),
        ("add-memory", TOOL_ROW_ADD_MEMORY, "◆ Saved memory notes.md"),
        (
            "goal",
            TOOL_ROW_GOAL,
            "◆ Read goal goal — no effect · ctrl+o",
        ),
        ("work-list", TOOL_ROW_WORK_LIST, "◆ Used tool operation"),
    ] {
        let rows = screen(text);
        assert!(rows.iter().any(|r| r == row), "{name}: the row measured");
        assert!(message_drawn(&rows), "{name}");
        assert_eq!(phase(&rows), (Phase::Idle, true), "{name}");
        assert_eq!(said_tail(&rows), None, "{name}");
        let r = muse(text);
        assert_eq!(
            (r.phase, r.phase_authoritative, r.said_tail, r.wall),
            (Phase::Idle, true, None, None),
            "{name}"
        );
    }
    let todos = screen(TOOL_ROW_TODOS);
    assert!(message_drawn(&todos));
    assert_eq!(
        phase(&todos),
        (Phase::Busy, false),
        "nothing drawn under it"
    );
    // NEGATIVE CONTROLS.
    let search = screen(TOOL_ROW_SEARCH);
    for not in [
        "  Searched the repository for the parser module",
        "  Searched the parser (1 match)",
        "  Searched \"parser\"",
        "  Searched \"parser\" (some matches)",
        "    Searched \"parser\" (1 match)",
    ] {
        let rows = edited(&search, "  Searched ", not);
        assert_eq!(phase(&rows), (Phase::Idle, false), "{not}");
    }
    for (text, head, said) in [
        (
            TOOL_ROW_ADD_MEMORY,
            "◆ Saved memory",
            "Saved memory of the parser layout for the next turn.",
        ),
        (
            TOOL_ROW_GOAL,
            "◆ Read goal",
            "Read goal of the sprint again before you start.",
        ),
        (
            TOOL_ROW_WORK_LIST,
            "◆ Used tool",
            "Used tool calls to check the parser.",
        ),
    ] {
        let mut rows = edited(&screen(text), head, &format!("◆ {said}"));
        if let Some(at) = rows.iter().position(|r| r.starts_with("└ ")) {
            rows.remove(at);
        }
        assert_eq!(phase(&rows), (Phase::Idle, true), "{said}");
        assert_eq!(said_tail(&rows).as_deref(), Some(said));
    }
    let mut bare = screen(TOOL_ROW_READ_MEMORY);
    bare.retain(|r| !r.starts_with("◆ Read memory"));
    assert!(bare.iter().any(|r| r == "└ memory file not found"));
    assert_eq!(phase(&bare), (Phase::Idle, false), "a branch alone");
}

/// THE STATUS ROW'S FIRST FRAME IS A TURN RUNNING (measured 2026-10-22, the
/// lab: `◇ Thinking` bare, 0.02 s after the first prompt's Enter, for
/// 0.04-0.19 s before `◇ Working (0s · esc to interrupt)`): never a turn
/// ended with no words. Before, it read idle with nothing said, and the
/// forever lab's r10 ladder on `ba4e9e2bd3` took it for the harness's first
/// point: the first reply named two short turns, and every rung of the
/// back-off ladder came a step late. NEGATIVE CONTROLS: a reply whose words
/// are `Thinking` (`◆`), another verb or a longer row, another glyph, the
/// row indented, and the bare row with a reply drawn under it are no first
/// frame; the timed status row reads running as before.
#[test]
fn the_status_rows_first_frame_is_a_turn_running() {
    let rows = screen(BUSY_FIRST_FRAME);
    assert!(rows.iter().any(|r| r == "◇ Thinking"), "the frame measured");
    assert!(message_drawn(&rows));
    assert_eq!(phase(&rows), (Phase::Busy, true));
    assert_eq!(said_tail(&rows), None);
    let r = muse(BUSY_FIRST_FRAME);
    assert_eq!(
        (r.phase, r.phase_authoritative, r.said_tail, r.wall),
        (Phase::Busy, true, None, None)
    );
    // NEGATIVE CONTROLS.
    for (not, read) in [
        ("◆ Thinking", (Phase::Idle, true)),
        ("◇ Thinking about the parser", (Phase::Idle, false)),
        ("◇ Pondering", (Phase::Idle, false)),
        ("◈ Thinking", (Phase::Idle, false)),
        ("  ◇ Thinking", (Phase::Idle, false)),
        ("◇ Working (0s · esc to interrupt)", (Phase::Busy, true)),
    ] {
        let rows = edited(&screen(BUSY_FIRST_FRAME), "◇ Thinking", not);
        assert_eq!(phase(&rows), read, "{not}");
    }
    let mut under = screen(BUSY_FIRST_FRAME);
    let at = under
        .iter()
        .position(|r| r == "◇ Thinking")
        .expect("the row");
    under.splice(
        at + 1..at + 1,
        [String::new(), "◆ Hi! I am the local lab stub.".to_string()],
    );
    assert_eq!(phase(&under), (Phase::Idle, true), "a reply under it");
    assert_eq!(
        said_tail(&under).as_deref(),
        Some("Hi! I am the local lab stub.")
    );
}

/// THE AGENT'S OWN SENTENCE THAT OPENS LIKE A TOOL ROW IS ITS WORDS
/// (`docs/measured/forever-lab-2026-10-20.md` P2): a reply `Wrote the parser
/// module and its unit tests in this turn.` (or `Edited the config …`) is the
/// agent's last words, where a reader matching the heads alone left the turn
/// none. NEGATIVE CONTROL: every `Ran `, `Wrote `, `Edited `, `Read memory
/// `, `Saved memory `, `Read goal ` and `Used tool ` row the captures
/// measured — a path, a cut path, a name, its `(+1)`, `— denied` or `·
/// ctrl+o` tail, any command — is still Muse Code's own row, no word of the
/// agent's.
#[test]
fn a_sentence_that_opens_like_a_tool_row_is_the_agents_words() {
    let rows = screen(END_OF_TURN_1_4_10);
    for said in [
        "Wrote the parser module and its unit tests in this turn.",
        "Edited the config so the tests find the stub.",
    ] {
        let reply = edited(&rows, "◆ Hi! I am the local lab stub", &format!("◆ {said}"));
        assert_eq!(phase(&reply), (Phase::Idle, true), "{said}");
        assert_eq!(said_tail(&reply).as_deref(), Some(said));
    }
    let mut measured = 0;
    for (name, text) in captures() {
        for row in screen(&text) {
            let Some(head) = row.strip_prefix("◆ ") else {
                continue;
            };
            if [
                "Ran ",
                "Wrote ",
                "Edited ",
                "Read memory ",
                "Saved memory ",
                "Read goal ",
                "Used tool ",
            ]
            .iter()
            .any(|h| head.starts_with(h))
            {
                measured += 1;
                assert!(tool_row(head), "{name}: {head}");
                let tool_last = edited(&rows, "◆ Hi! I am the local lab stub", &row);
                assert_eq!(said_tail(&tool_last), None, "{name}: {head}");
            }
        }
    }
    assert!(measured >= 20, "the captures' tool rows: {measured}");
    // The turn's earlier replies are read by the same rows: the agent's
    // sentence above a tool call is its words, the tool's row is not.
    let at = rows
        .iter()
        .position(|r| r.starts_with("◆ Hi!"))
        .expect("the reply");
    for (before, words) in [
        (
            "◆ Wrote the parser; NEEDS YOU: the signing credential.",
            Some("Wrote the parser; NEEDS YOU: the signing credential."),
        ),
        ("◆ Wrote .agents/probe-write.md (+1)", None),
    ] {
        let mut drawn = rows[..at].to_vec();
        drawn.extend(
            [
                before,
                "",
                "◆ Ran command · Push the branch · ✓ · 2.3s · ctrl+o",
                "",
                "◆ Nothing left: all of it is merged.",
            ]
            .map(str::to_string),
        );
        drawn.extend(rows[at + 1..].iter().cloned());
        assert_eq!(turn_said_before(&drawn).as_deref(), words, "{before}");
    }
}

/// A REPLY THAT NAMES A FILE AFTER A TOOL'S HEAD IS THE AGENT'S WORDS (the
/// review of `b9/resid4-lab`): `Saved memory notes.md with the parser
/// layout. Shall I push the branch now?` is a question, and a need in a
/// later paragraph of `Saved memory notes.md now holds the parser layout.`
/// is read — where a first word holding a `.` was enough to read the whole
/// reply as Muse Code's tool row, its question plain idle and its need lost,
/// for `Read memory`, `Saved memory`, `Read goal`, `Used tool`, `Wrote` and
/// `Edited` alike. A tool row names one path or name and its own tail alone
/// ([`names_a_path`]). NEGATIVE CONTROLS: each measured row's shape — a
/// name alone, a diff's count, an outcome, the expander, a cut path — is
/// still Muse Code's own row with no words; an outcome no capture measured,
/// or a count that is no diff's, is the agent's words.
#[test]
fn a_reply_naming_a_file_after_a_tool_head_is_the_agents_words() {
    let rows = screen(END_OF_TURN_1_4_10);
    let head = "◆ Hi! I am the local lab stub";
    for said in [
        "Saved memory notes.md with the parser layout. Shall I push the branch now?",
        "Read memory notes.md first, then wrote the parser. Shall I push it?",
        "Read goal goal.md says land the parser. Shall I push it?",
        "Used tool operation.json to list the work. Shall I push it?",
        "Wrote notes.md with the parser layout. Shall I push it?",
        "Edited README.md so it names the stub. Shall I push it?",
        "Edited README.md — it now names the stub. Shall I push it?",
        "Wrote src/parser.rs (+40) and its tests. Shall I push it?",
    ] {
        let reply = edited(&rows, head, &format!("◆ {said}"));
        assert_eq!(phase(&reply), (Phase::Question, true), "{said}");
        assert_eq!(said_tail(&reply).as_deref(), Some(said));
    }
    let at = rows
        .iter()
        .position(|r| r.starts_with(head))
        .expect("the reply");
    for first in [
        "◆ Saved memory notes.md now holds the parser layout.",
        "◆ Read memory notes.md first, then wrote the parser.",
    ] {
        let mut need = rows.clone();
        need[at] = first.to_string();
        need.splice(
            at + 1..at + 1,
            [
                String::new(),
                "  NEEDS YOU: the deploy key for the staging push.".to_string(),
            ],
        );
        let words = format!(
            "{}\nNEEDS YOU: the deploy key for the staging push.",
            first.trim_start_matches("◆ ")
        );
        assert_eq!(said_tail(&need), Some(words), "{first}");
    }
    // NEGATIVE CONTROLS: the measured shapes are Muse Code's own rows
    for row in [
        "◆ Saved memory notes.md",
        "◆ Read memory notes.md — failed",
        "◆ Read goal goal — no effect · ctrl+o",
        "◆ Used tool operation",
        "◆ Used tool operation — denied · ctrl+o",
        "◆ Wrote notes.txt (+1)",
        "◆ Wrote notes.txt (+1) · ctrl+o",
        "◆ Edited README.md (+1 −1)",
        "◆ Edited .agents/a-protected-note-with-a-rather-lo… — denied",
    ] {
        let tool = edited(&rows, head, row);
        assert_eq!(phase(&tool), (Phase::Idle, true), "{row}");
        assert_eq!(said_tail(&tool), None, "{row}");
    }
    for said in [
        "Edited README.md — fixed",
        "Wrote notes.txt (+one)",
        "Saved memory notes.md (+1",
    ] {
        let reply = edited(&rows, head, &format!("◆ {said}"));
        assert_eq!(said_tail(&reply).as_deref(), Some(said), "{said}");
    }
}

/// MUSE CODE'S OWN ROW OVER ITS OUTCOME BRANCH IS NO WORDS (the review of
/// `b9/resid4-lab`): a turn ended on any `◆` row with a `└ ` branch in
/// column 0 right under it — `◆ Monitor … — failed`, `◆ Stopped terminal
/// tool operation — failed`, `◆ Scheduled …`, `◆ Unscheduled …`, `◆
/// Message not sent to …`, each with its branch as the captures measured
/// them — is over and said nothing ([`branched`]), and the turn's other
/// words pass such a block over. Where the branch was skipped to the head
/// with no more, Muse Code's row and its tool's output were read as the
/// agent's last words, as evidence, and a need or a question in them acted
/// on. NEGATIVE CONTROLS: the agent's reply with an indented `└` row is its
/// words; a `└ ` row with a blank over it is no branch and no part of the
/// reply above it (nothing known); the agent's own `◆` words above a tool
/// call stay its words.
#[test]
fn muse_codes_own_row_over_its_outcome_branch_is_no_words() {
    let measured = [
        (
            "◆ Monitor Watch the lab socket — failed",
            "└ network access denied",
        ),
        (
            "◆ Stopped terminal tool operation — failed",
            "└ unified exec session 424242 is not live",
        ),
        (
            "◆ Scheduled 7 3 * * *: lab probe",
            "└ 109c3489 · Every day at 3:07 AM · once",
        ),
        (
            "◆ Unscheduled 109c3489",
            "└ Removed scheduled job 109c3489.",
        ),
        (
            "◆ Message not sent to lab-no-such-session · ✗ · not found · ctrl+o",
            "└ \"hello from the lab stub\"",
        ),
    ];
    let captured: Vec<String> = [BOX_NETWORK_DIGIT_APPROVED, END_OF_TURN_WORK_PANEL]
        .iter()
        .flat_map(|t| screen(t))
        .collect();
    let base = screen(TOOL_ROW_WORK_LIST);
    for (head, branch) in measured {
        assert!(
            captured.windows(2).any(|w| w[0] == head && w[1] == branch),
            "measured: {head}"
        );
        let rows = edited(&edited(&base, "◆ Used tool", head), "└ ", branch);
        assert_eq!(phase(&rows), (Phase::Idle, true), "{head}");
        assert_eq!(said_tail(&rows), None, "{head}");
        let r = read(Some("muse"), &rows, None);
        assert_eq!(
            (r.phase, r.phase_authoritative, r.said_tail),
            (Phase::Idle, true, None),
            "{head}"
        );
    }
    // The turn's other words pass the block over.
    let end = screen(END_OF_TURN_1_4_10);
    let at = end
        .iter()
        .position(|r| r.starts_with("◆ Hi!"))
        .expect("the reply");
    let before = |block: &[&str]| {
        let mut rows = end.clone();
        rows.splice(
            at..at,
            block
                .iter()
                .map(|r| (*r).to_string())
                .chain([String::new()]),
        );
        turn_said_before(&rows)
    };
    let (head, branch) = measured[0];
    assert_eq!(before(&[head, branch]), None, "the monitor's row");
    // NEGATIVE CONTROLS.
    assert_eq!(
        before(&["◆ I will watch the lab socket."]).as_deref(),
        Some("I will watch the lab socket.")
    );
    let mut tree = end.clone();
    tree.splice(at + 1..at + 1, ["  └ src/parser.rs".to_string()]);
    assert_eq!(phase(&tree), (Phase::Idle, true));
    assert_eq!(
        said_tail(&tree).as_deref(),
        Some("Hi! I am the local lab stub, not a model. (reply 1)\n└ src/parser.rs")
    );
    let mut stray = end.clone();
    stray.splice(at + 1..at + 1, [String::new(), "└ stray".to_string()]);
    assert_eq!(
        phase(&stray),
        (Phase::Idle, false),
        "a branch under a blank"
    );
    assert_eq!(said_tail(&stray), None);
}

/// THE QUESTION (1.4.10) IS A PROMPT OVER AN OPEN COMPOSER, whatever glyph
/// its head cycles: its question, the model's options — each label less its
/// description, `Red (Recommended)` the recommended one by the model's own
/// suffix — as answers, `None of the above` (which Muse Code adds) the
/// person's own words, chosen by the cursor and Enter, Down moving the `⟩`;
/// its Esc stops the turn. While it is up the composer under it is not read
/// (the question's key row names Enter, the arrows, Tab and Esc), and there
/// are no last words. NEGATIVE CONTROLS: a `◆` head with
/// the key row gone is no question and no reply either (a reply read there
/// was idle as evidence) — no evidence of anything; the binary's key row of
/// several questions is a question of no roles.
#[test]
fn the_question_is_a_prompt_answered_by_the_cursor_and_enter() {
    for (text, focus) in [(QUESTION, 1), (QUESTION_DOWN, 2), (QUESTION_NONE, 3)] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Prompt, true)
        );
        assert_eq!(
            (r.composer.clone(), r.said_tail.clone(), r.wall.clone()),
            (None, None, None)
        );
        let p = r.prompt.expect("the question");
        assert_eq!(p.kind, PromptKind::Question);
        assert_eq!(p.title, "Request user input Color");
        assert_eq!(p.description, "Which color should the probe use?");
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Red (Recommended)",
                "Blue",
                anchor_text("muse.question.none")
            ]
        );
        assert_eq!(roles(&p), vec![Role::Answer, Role::Answer, Role::Other]);
        assert_eq!(p.focused().and_then(|o| o.n), Some(focus));
        assert_eq!(
            p.recommended().map(|o| o.label.as_str()),
            Some("Red (Recommended)")
        );
        assert_eq!(p.select, Select::ArrowsEnter);
        assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Interrupt));
        assert!(p.question_dialog.is_none() && !p.head_off_screen);
    }
    let rows = screen(QUESTION);
    for glyph in ['◇', '◆'] {
        let other = edited(
            &rows,
            "◈ Request",
            &format!("{glyph} Request user input Color — running (1s)"),
        );
        assert_eq!(phase(&other), (Phase::Prompt, true), "{glyph}");
        let keyless = edited(&other, "  Enter to select", "");
        assert_eq!(
            phase(&keyless),
            (Phase::Idle, false),
            "{glyph}: the control"
        );
        assert_eq!(said_tail(&keyless), None);
    }
    let several = edited(
        &rows,
        "  Enter to select",
        "  Enter to select · ↑/↓ to move · ←/→ to switch question · Tab for an optional note · \
         Esc to interrupt",
    );
    assert_eq!(phase(&several), (Phase::Prompt, true));
    let p = prompt(&several).expect("a question");
    assert_eq!(p.kind, PromptKind::Question);
    assert!(roles(&p).iter().all(|&r| r == Role::Other));
}

/// THE QUESTION IN A NARROW TAB IS THE SAME QUESTION (measured at 60 and 72
/// columns): its key row wrapped onto two rows is read as one, the
/// descriptions drawn under their labels (60) or in a column of their own
/// beside them (72) are no part of a label, and it is a prompt as evidence
/// with its roles, `Crimson red, …` recommended, guarded on the question's
/// own row. NEGATIVE CONTROLS: with the key row's second row gone, or its
/// first, the screen holds no question (no evidence of anything); and the
/// key row's words wrapped onto three rows of their own above the options'
/// blank are read too, while the same words split by a blank row are not.
#[test]
fn the_question_in_a_narrow_tab_is_the_same_question() {
    let crimson = "Crimson red, the warm and loud one (Recommended)";
    for text in [QUESTION_WRAPPED, QUESTION_WRAPPED_72] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Prompt, true)
        );
        assert_eq!((r.composer.clone(), r.said_tail.clone()), (None, None));
        let p = r.prompt.expect("the question");
        assert_eq!(p.kind, PromptKind::Question);
        assert_eq!(p.title, "Request user input Shade");
        assert_eq!(
            p.description,
            "Which shade should the probe use for the long label test?"
        );
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![crimson, "Blue", anchor_text("muse.question.none")]
        );
        assert_eq!(roles(&p), vec![Role::Answer, Role::Answer, Role::Other]);
        assert_eq!(p.recommended().map(|o| o.label.as_str()), Some(crimson));
        assert_eq!(p.footer.as_deref(), Some(anchor_text("muse.question.keys")));
        let rows = screen(text);
        assert_eq!(
            p.question.map(|k| rows[k].as_str()),
            Some("  Which shade should the probe use for the long label test?")
        );
        let end = rows
            .iter()
            .rposition(|r| r.starts_with("  ") && r.trim_end().ends_with("interrupt"))
            .expect("the key row's last row");
        assert_eq!(p.span.1, end);
        for cut in [end, end - 1] {
            let mut cut_rows = rows.clone();
            cut_rows.remove(cut);
            assert_eq!(phase(&cut_rows), (Phase::Idle, false), "row {cut} cut");
            assert_eq!(prompt(&cut_rows), None);
        }
    }
    let rows = screen(QUESTION);
    let at = rows
        .iter()
        .position(|r| r.starts_with("  Enter to select"))
        .expect("the key row");
    let mut three = rows.clone();
    three.splice(
        at..=at,
        [
            "  Enter to select · ↑/↓ to move ·",
            "  Tab for an optional note ·",
            "  Esc to interrupt",
        ]
        .map(String::from),
    );
    assert_eq!(phase(&three), (Phase::Prompt, true));
    assert!(prompt(&three).is_some_and(|p| roles(&p)[0] == Role::Answer));
    let mut split = three.clone();
    split[at + 1] = String::new();
    assert_eq!(prompt(&split), None, "the control");
}

/// `(Recommended)` IS THE MODEL'S OWN SUFFIX, and `None of the above` is
/// never an answer: with the suffix gone no option is recommended, and the
/// row Muse Code adds stays the person's words. NEGATIVE CONTROL: the
/// measured question's recommended option is `Red (Recommended)`.
#[test]
fn a_question_with_no_suffix_recommends_nothing() {
    let rows = screen(QUESTION);
    let plain = edited(
        &rows,
        "  ⟩ 1. Red",
        "  ⟩ 1. Red                Warm and loud.",
    );
    let p = prompt(&plain).expect("the question");
    assert_eq!(p.recommended(), None);
    assert_eq!(roles(&p), vec![Role::Answer, Role::Answer, Role::Other]);
    assert!(
        prompt(&rows).is_some_and(|p| p.recommended().is_some()),
        "the control"
    );
}

/// AN ANSWERED QUESTION IS NO BOX (1.4.10): Enter on `Blue` sent
/// `selected_label`, Enter on `None of the above` the free text `None of the
/// above` at once (no text field opened), each leaving `Structured user
/// input answered — Color: …` over the reply — idle as evidence. NEGATIVE
/// CONTROL: the question's screen itself is a prompt.
#[test]
fn an_answered_question_is_no_box() {
    for (text, answer) in [
        (QUESTION_ANSWERED, "Color: Blue"),
        (QUESTION_NONE_ENTER, "Color: None of the above"),
    ] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true)
        );
        assert_eq!(r.prompt, None);
        let row = format!("Structured user input answered — {answer}");
        assert!(screen(text).iter().any(|r| r.trim() == row), "{answer}");
        assert!(
            r.said_tail
                .is_some_and(|s| s.contains("\"status\":\"answered\""))
        );
    }
    assert_eq!(muse(QUESTION).phase, Phase::Prompt, "the control");
}

/// A REFUSED KEY IS A SIGN-IN WALL (1.4.10, a 401, never retried): `◆ your
/// API key from META_API_KEY was rejected — update or unset it` as the
/// turn's last row is [`WallKind::Auth`] — a person signs in with `muse
/// login` or mends the key — idle as evidence on its wall, no last words.
/// So are its two siblings in the binary's strings. NEGATIVE CONTROL: the
/// same words inside a reply are the agent's words, no wall.
#[test]
fn a_refused_key_is_an_auth_wall() {
    let r = muse(KEY_REJECTED);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    let w = r.wall.expect("the wall");
    assert_eq!(
        (w.kind, w.placement, w.reset),
        (WallKind::Auth, Placement::ErrorRow, None)
    );
    assert_eq!(w.message, anchor_text("muse.wall.key"));
    assert_eq!(r.said_tail, None);
    let rows = screen(KEY_REJECTED);
    for sibling in ["muse.wall.saved_key", "muse.wall.no_login"] {
        let other = edited(
            &rows,
            "◆ your API key",
            &format!("◆ {}", anchor_text(sibling)),
        );
        assert_eq!(
            wall(&other).map(|w| w.kind),
            Some(WallKind::Auth),
            "{sibling}"
        );
    }
    let quoted = edited(
        &rows,
        "◆ your API key",
        &format!("◆ It said: {}", anchor_text("muse.wall.key")),
    );
    assert_eq!(wall(&quoted), None, "the control");
    assert!(said_tail(&quoted).is_some());
}

/// A FAILED MODEL CALL IS AN API WALL (1.4.10): `◆ model failed: API error
/// 429: … (after 3 provider attempts)` is a rate limit — limited, as a 429
/// reads for every program — and `… API error 500: …` a server error the
/// vendor retries, idle on its wall; neither is the agent's words. A status
/// that is no retry's (400) is not retryable, and a failure naming no status
/// is. NEGATIVE CONTROL: a reply that quotes such an error past its head is
/// no wall.
#[test]
fn a_failed_model_call_is_an_api_wall() {
    let api = |code, retryable| WallKind::ApiError {
        code,
        retryable,
        cause: ApiCause::Server,
    };
    let r = muse(RATE_LIMIT_FINAL);
    assert!(
        matches!(r.phase, Phase::Limited { reset: None, .. }),
        "{:?}",
        r.phase
    );
    assert!(r.phase_authoritative);
    let w = r.wall.expect("the wall");
    assert_eq!(w.kind, api(Some(429), true));
    assert!(
        w.message.ends_with("(after 3 provider attempts)"),
        "{}",
        w.message
    );
    let r = muse(SERVER_ERROR_FINAL);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(r.wall.map(|w| w.kind), Some(api(Some(500), true)));
    assert_eq!(r.said_tail, None);
    let rows = screen(SERVER_ERROR_FINAL);
    let bad = edited(
        &rows,
        "◆ model failed",
        "◆ model failed: API error 400: bad request (invalid_request_error)",
    );
    assert_eq!(wall(&bad).map(|w| w.kind), Some(api(Some(400), false)));
    let unnamed = edited(&rows, "◆ model failed", "◆ model failed: connection failed");
    assert_eq!(wall(&unnamed).map(|w| w.kind), Some(api(None, true)));
    let quoted = edited(
        &rows,
        "◆ model failed",
        "◆ The log says model failed: API error 500: it broke",
    );
    assert_eq!(wall(&quoted), None, "the control");
    assert_eq!(phase(&quoted), (Phase::Idle, true));
}

/// THE USAGE WALL (1.4.10, a 429 `usage_limit_reached` with `resets_at`):
/// `◆ Usage limit reached · /upgrade (<url>) for increased limits, or wait
/// for usage to reset at 1:06 PM`, its time wrapped onto the next row, is
/// limited until that time, the session window (the row names none). No
/// path opens its URL: it is read as text. The binary's form with no reset
/// names none. NEGATIVE CONTROLS: the title alone in a reply is no wall, nor
/// is the whole wall quoted past a reply's head.
#[test]
fn the_usage_wall_is_limited_until_its_reset() {
    let r = muse(USAGE_WALL);
    let w = r.wall.clone().expect("the wall");
    assert_eq!(
        (w.kind, w.reset.as_deref()),
        (WallKind::UsageSession, Some("1:06 PM"))
    );
    assert_eq!(
        r.phase,
        Phase::Limited {
            message: w.message.clone(),
            reset: w.reset.clone()
        }
    );
    assert!(r.phase_authoritative);
    assert_eq!(r.said_tail, None);
    let rows = screen(USAGE_WALL);
    let at = rows
        .iter()
        .position(|r| r.starts_with("◆ Usage limit"))
        .expect("the wall");
    let mut bare = rows.clone();
    bare[at] = "◆ Usage limit reached · /upgrade (https://example.invalid/) for increased limits"
        .to_string();
    bare.remove(at + 1);
    assert_eq!(
        wall(&bare).map(|w| (w.kind, w.reset)),
        Some((WallKind::UsageSession, None))
    );
    let mut said = rows.clone();
    said[at] = "◆ Usage limit reached · nothing to worry about".to_string();
    said.remove(at + 1);
    assert_eq!(wall(&said), None, "the control");
    assert_eq!(phase(&said), (Phase::Idle, true));
    let mut quoted = bare.clone();
    quoted[at] = format!("◆ It said: {}", &bare[at]["◆ ".len()..]);
    assert_eq!(
        wall(&quoted),
        None,
        "the control: the wall's words past a reply's head"
    );
}

/// A LOCAL COMMAND'S ROWS UNDER A WALL ARE NO PART OF ITS NOTICE (1.4.10,
/// measured in the W2 lab, `forever --harness muse --mode wall-hid`, the
/// harness's own 40-row read): `/rules` entered at the empty composer under
/// the usage wall prints `Rules loaded this session (in precedence order):`,
/// `user: none` and `project: none`, two in, a block of their own under a
/// blank row. The wall still reads limited until the time it names, its
/// message the notice's own words, no last words of the worker's; and so
/// with three such blocks (the lab's third point) and under the refused
/// key's wall, whose message must be the vendor's whole. Read before as the
/// turn's last block whole, the rows were folded into the message
/// (`reset=3:34 AM Rules loaded …`): the reset named no time, so the wait
/// fell back to 30 min from the wall's first read, and the key's wall was no
/// wall at all, its words the worker's. NEGATIVE CONTROLS: a notice that
/// really wraps is read whole — the measured `3:34` over `  AM` is `3:34
/// AM`, and a break before the time (`reset at` over `  3:34 AM`) names it
/// too; a reply's paragraphs stay its words.
#[test]
fn a_local_commands_rows_under_a_wall_are_no_part_of_its_notice() {
    const NOTICE: &str = "Usage limit reached · /upgrade (https://accountscenter.meta.com/muse_code/?ep=xgrade) for increased limits, or wait for usage to reset at 3:34 AM";
    const RULES: [&str; 4] = [
        "  Rules loaded this session (in precedence order):",
        "  user: none",
        "  project: none",
        "",
    ];
    let rows = screen(USAGE_WALL_RULES_UNDER);
    let thrice = inserted(
        &inserted(&rows, "── Voice input", &RULES),
        "── Voice input",
        &RULES,
    );
    for (name, rows) in [("one /rules", &rows), ("three", &thrice)] {
        let w = wall(rows).unwrap_or_else(|| panic!("{name}: the wall"));
        assert_eq!(
            (w.kind, w.message.as_str(), w.reset.as_deref()),
            (WallKind::UsageSession, NOTICE, Some("3:34 AM")),
            "{name}"
        );
        let r = read(Some("muse"), rows, None);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (
                Phase::Limited {
                    message: NOTICE.to_string(),
                    reset: Some("3:34 AM".to_string())
                },
                true
            ),
            "{name}"
        );
        assert_eq!(r.said_tail, None, "{name}");
    }
    let mut key = edited(
        &rows,
        "◆ Usage limit",
        "◆ your API key from META_API_KEY was rejected — update or unset it",
    );
    key.retain(|r| r != "  AM");
    let w = wall(&key).expect("the key's wall");
    assert_eq!(
        (w.kind, w.message.as_str()),
        (
            WallKind::Auth,
            "your API key from META_API_KEY was rejected — update or unset it"
        )
    );
    assert_eq!(
        remedy(&w.message).as_deref(),
        Some("update or unset META_API_KEY")
    );
    assert_eq!(said_tail(&key), None);
    // The controls: a notice wrapped onto the row under it, no blank
    // between, is one notice.
    let at = rows
        .iter()
        .position(|r| r.starts_with("◆ Usage limit"))
        .expect("the wall");
    let mut early = rows.clone();
    early[at] = rows[at]
        .strip_suffix(" 3:34")
        .expect("the measured row ends on the hour")
        .to_string();
    early[at + 1] = "  3:34 AM".to_string();
    assert_eq!(
        wall(&early).map(|w| (w.message, w.reset)),
        Some((NOTICE.to_string(), Some("3:34 AM".to_string()))),
        "the control: the time on the row under the notice"
    );
    let mut reply = edited(
        &edited(
            &key,
            "◆ your API key",
            "◆ The parser is in and its tests pass.",
        ),
        "  Rules loaded",
        "  Its docs follow next.",
    );
    reply.retain(|r| !["  user: none", "  project: none"].contains(&r.as_str()));
    assert_eq!(
        said_tail(&reply).as_deref(),
        Some("The parser is in and its tests pass.\nIts docs follow next."),
        "the control: a reply's paragraphs are its words"
    );
}

/// A RETRIED REQUEST IS A RUNNING TURN, AND NO WALL (1.4.10): under the
/// status row, `└ Meta Model API rate limited (HTTP 429) · retrying in 5s ·
/// attempt 2/3` (a 500's `server error (HTTP 500)` the same) — busy as
/// evidence, its retry read ([`retrying`]): the status, the wait, the
/// attempt to come of how many. NEGATIVE CONTROLS: with the status row gone
/// the retry row alone says nothing — no retry, no evidence; a running turn
/// with no retry row has none.
#[test]
fn a_retried_request_is_a_running_turn_and_no_wall() {
    for (text, want) in [
        (
            RATE_LIMIT_RETRY,
            Retry {
                code: Some(429),
                wait: "5s".to_string(),
                attempt: 2,
                of: 3,
            },
        ),
        (
            SERVER_ERROR_RETRY,
            Retry {
                code: Some(500),
                wait: "1s".to_string(),
                attempt: 2,
                of: 2,
            },
        ),
    ] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Busy, true)
        );
        assert_eq!((r.wall.clone(), r.said_tail.clone()), (None, None));
        let rows = screen(text);
        assert_eq!(retrying(&rows), Some(want));
        let gone = edited(&rows, "◆ Thinking", "");
        assert_eq!(retrying(&gone), None);
        assert_eq!(phase(&gone), (Phase::Idle, false));
    }
    assert_eq!(retrying(&screen(BUSY_1_4_10)), None, "the control");
}

/// A MODEL'S TURN STOPPED WITH ESC (1.4.10) keeps the text it had received
/// and ends on `◆ Interrupted · Report issues with /feedback`: interrupted,
/// no last words — the next message is the person's — idle as evidence once
/// the run has stopped, and busy with no evidence while the footer still
/// says `interrupting run`. NEGATIVE CONTROLS: a turn's end is no interrupt,
/// and 1.4.9's retracted echo turn draws no marker (a draft).
#[test]
fn a_model_turn_stopped_with_esc_is_interrupted() {
    let r = muse(INTERRUPTED_1_4_10);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert!(r.interrupted);
    assert_eq!(r.said_tail, None);
    let r = muse(INTERRUPTING);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Busy, false)
    );
    assert!(r.interrupted);
    let settled = screen(INTERRUPTED_1_4_10);
    let stopping = screen(INTERRUPTING);
    assert_eq!(
        settled[..settled.len() - 1],
        stopping[..stopping.len() - 1],
        "the footer alone differs"
    );
    for text in [END_OF_TURN_1_4_10, INTERRUPTED, END_OF_TURN] {
        assert!(!muse(text).interrupted, "the control");
    }
}

/// `/usage` AND `/status` DRAW CARDS UNDER A TURN'S END (1.4.10, `┌ … ┘` in
/// column 0, which Esc does not close): the turn still ended on its reply,
/// idle as evidence, the reply the last words. NEGATIVE CONTROL: a card
/// under a message with no reply says nothing — idle, no evidence.
#[test]
fn a_card_under_a_turns_end_is_no_part_of_it() {
    for text in [USAGE_CARD, STATUS_CARD] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true)
        );
        assert_eq!(
            r.said_tail.as_deref(),
            Some("Hi! I am the local lab stub, not a model. (reply 1)")
        );
        assert_eq!(r.prompt, None);
    }
    let replyless: Vec<String> = screen(USAGE_CARD)
        .into_iter()
        .filter(|r| !r.starts_with("◆ Hi!"))
        .collect();
    assert_eq!(phase(&replyless), (Phase::Idle, false), "the control");
}

/// THE SIGNED-IN CARDS NAME THE ACCOUNT AND THE WEEKLY QUOTA (1.4.11,
/// measured 2026-10-21 signed in with a Meta account): the `/status`
/// card's `ACCOUNT` row is the address the footer's account slot shows
/// and its `BILLING` row the tier; the `/usage` card's `Weekly 71% used`
/// is the footer's `weekly 29% left`. NEGATIVE CONTROLS: the lab's
/// API-key cards draw neither an `ACCOUNT` row nor a subscription block
/// (no account, no quota, never a guess); the screen is still just idle.
#[test]
fn the_signed_in_cards_name_the_account_and_the_weekly_quota() {
    let rows = screen(USAGE_SIGNED_IN);
    assert_eq!(
        status_card_account(&rows),
        Some((
            "redacted-user@example.test".to_owned(),
            Some("Muse Code Max Usage".to_owned()),
        ))
    );
    assert_eq!(
        usage_card_weekly(&rows),
        Some(UsageCardWeekly {
            text: "weekly 29% left".to_owned(),
            reset: Some("Oct 25 at 5:00 PM".to_owned()),
        })
    );
    // The `/usage` card alone (its status card scrolled away) still names
    // the tier on its own subscription line.
    let first_card_end = rows
        .iter()
        .position(|r| r.starts_with('└'))
        .expect("the status card's foot");
    let usage_only: Vec<String> = rows[..2]
        .iter()
        .chain(rows[first_card_end + 1..].iter())
        .cloned()
        .collect();
    assert_eq!(status_card_account(&usage_only), None);
    assert_eq!(
        subscription_card_plan(&usage_only).as_deref(),
        Some("Muse Code Max Usage")
    );
    // A later account-bearing status must not borrow its plan from an
    // earlier account's status, or from an anonymous usage card. The
    // matching card's own billing row remains a positive control.
    let at = rows
        .iter()
        .rposition(|row| is_idle_voice_rule(row))
        .expect("the composer top rule");
    let status = vec![
        "┌".to_owned() + &"─".repeat(83) + "┐",
        format!("│{:<83}│", "  MUSE CODE 1.4.11 / second-session IDLE"),
        format!(
            "│{:<83}│",
            "  ACCOUNT        Second User (second@example.test)"
        ),
        "└".to_owned() + &"─".repeat(83) + "┘",
    ];
    let mut mixed = rows.clone();
    drop(mixed.splice(at..at, status.clone()));
    assert_eq!(
        status_card_account(&mixed),
        Some(("second@example.test".into(), None)),
        "an account's plan cannot come from another card"
    );
    mixed.insert(
        at + status.len() - 1,
        format!("│{:<83}│", "  BILLING        Subscription · Muse Code Pro"),
    );
    assert_eq!(
        status_card_account(&mixed),
        Some(("second@example.test".into(), Some("Muse Code Pro".into())))
    );
    let r = muse(USAGE_SIGNED_IN);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    for text in [USAGE_CARD, STATUS_CARD] {
        let rows = screen(text);
        assert_eq!(status_card_account(&rows), None, "the API-key control");
        assert_eq!(usage_card_weekly(&rows), None, "the API-key control");
    }
}

/// THE TOP RULE AT REST IS THE IDLE VOICE LABEL, and no other voice state
/// is: the window's branding may take the resting label's cells, never
/// the words of a recording. NEGATIVE CONTROLS: Muse Code's other
/// measured rules (the busy screen's, a plain bottom rule) are none.
#[test]
fn the_top_rule_at_rest_is_the_idle_voice_label() {
    let rows = screen(IDLE);
    let rule = rows
        .iter()
        .find(|r| r.starts_with("── Voice input"))
        .expect("the measured rule");
    assert!(is_idle_voice_rule(rule));
    for other in [
        "── Recording ──────────────────────────────",
        "──────────────────────────────────────────",
        "❯ Voice input (⌥ + v to start)",
    ] {
        assert!(!is_idle_voice_rule(other), "the control: {other:?}");
    }
}

/// A NARROW PANE'S COMPOSER IS READ, WHATEVER ITS LABEL LEAVES OF THE RULE
/// (1.4.11 at 40 columns down to 26, the lab of 2026-10-22): under 40
/// columns the top rule's label leaves fewer than a rule's ten `─` — one at
/// 33 columns, none at 32 — and below that it is cut with `…`; at every
/// width the composer is the frame, idle as evidence, its caret the row the
/// cursor parks on, and the screen Muse Code's. The idle label stands whole
/// down to 32 columns, and cut it is no idle label. Read as no rule from 39
/// columns down, the frame was lost there, and with it every reader of it
/// and the window's harness (`status agent=unknown`).
///
/// NEGATIVE CONTROLS: a rule is still ten `─` — the same rows with the
/// label blanked out are a frame at 40 columns alone; a `Recording` label
/// in the idle one's place is the frame's top rule and no idle label.
#[test]
fn a_narrow_panes_composer_is_read_whatever_its_label_leaves() {
    let frames = crate::prompt::fixtures::reflow_frames(IDLE_NARROW);
    let widths: Vec<u16> = frames.iter().map(|(w, _)| *w).collect();
    assert_eq!(widths, [40, 39, 38, 36, 34, 33, 32, 30, 26], "the sweep");
    let (row, col) = cursor(IDLE_NARROW).expect("measured");
    assert_eq!((row, col), (4, 2), "the caret row, two columns in");
    for (cols, rows) in &frames {
        let w = usize::from(*cols);
        let top = &rows[3];
        assert!(top.starts_with("── Voice input ("), "{w}: {top:?}");
        assert_eq!(is_rule(top), w >= 40, "{w}: ten `─` from 40 columns");
        assert_eq!(composer_rules(rows), Some((3, 5)), "{w}: the frame");
        let r = read(Some("muse"), rows, None);
        assert_eq!(r.program, Program::Muse, "{w}");
        assert_eq!(
            (r.phase, r.phase_authoritative),
            (Phase::Idle, true),
            "{w}: idle as evidence"
        );
        assert_eq!(r.composer, Some((row, vec![String::new()])), "{w}");
        assert_eq!(identify(None, rows).program(), Program::Muse, "{w}");
        assert_eq!(
            is_idle_voice_rule(top),
            w >= 32,
            "{w}: the idle label whole, never cut"
        );
        // CONTROL: blanked, the label admits nothing, and a rule is ten `─`.
        let blanked: String = top
            .chars()
            .map(|c| if c == '─' { c } else { ' ' })
            .collect();
        let mut bare = rows.clone();
        bare[3] = blanked;
        assert_eq!(
            composer_rules(&bare).is_some(),
            w >= 40,
            "CONTROL {w}: an unlabelled rule of fewer than ten `─` is none"
        );
        // CONTROL: another voice state's label in its place.
        let mut recording = rows.clone();
        recording[3] = format!("── Recording {}", "─".repeat(w - 13));
        assert_eq!(composer_rules(&recording), Some((3, 5)), "CONTROL {w}");
        assert!(!is_idle_voice_rule(&recording[3]), "CONTROL {w}");
    }
}

/// CARDS ALONE ABOVE THE OPEN COMPOSER ARE A SESSION AT REST (1.4.10, the lab
/// of 2026-10-21): `/status` entered three times under the usage wall pushed
/// the notice out of sight, the composer open and empty under the cards,
/// each card saying the run `FAILED`. Idle as evidence, with no words, wall,
/// question or box read: the screen a remembered limit resumes at. Read
/// before as a screen nothing measured says (idle, no evidence), where the
/// turn-end policy decided nothing for good. A card entered while a turn
/// runs is drawn ABOVE the turn's status row, which stays over the composer
/// (the card says `RUNNING`), and once the turn ends its reply is drawn
/// whole under the card. NEGATIVE CONTROLS: that screen is busy and its end
/// idle on its reply; the cards with a status row under them are busy; the
/// newest card saying `RUNNING` with no status row under it, which nothing
/// measured draws, is no evidence (a `/usage` card under it, naming no
/// state, changes nothing); cards under a message with no reply are none
/// either; the footer saying `interrupting run` is busy, no evidence; and
/// blank rows alone say nothing.
#[test]
fn cards_alone_above_the_open_composer_are_a_session_at_rest() {
    let r = muse(USAGE_WALL_STATUS_CARDS);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(
        (r.wall.clone(), r.said_tail.clone(), r.prompt.clone()),
        (None, None, None)
    );
    assert!(!r.interrupted && !r.fresh);
    let rows = screen(USAGE_WALL_STATUS_CARDS);
    assert_eq!(card_state(&rows), Some("FAILED"));
    assert_eq!(
        composer_draft(&rows).map(|(_, d)| d),
        Some(vec![String::new()]),
        "the composer open and empty under the cards"
    );
    assert_eq!(background_wait(&rows), None);
    // The controls. A card entered while a turn runs: above the status row.
    let r = muse(STATUS_MID_TURN);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Busy, true)
    );
    assert_eq!(card_state(&screen(STATUS_MID_TURN)), Some("RUNNING"));
    let r = muse(STATUS_MID_TURN_ENDED);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert!(
        r.said_tail
            .as_deref()
            .is_some_and(|t| t.starts_with("tick 1 tick 2") && t.ends_with("tick 30")),
        "{:?}",
        r.said_tail
    );
    assert_eq!(card_state(&screen(STATUS_CARD)), Some("COMPLETED"));
    let status = inserted(
        &rows,
        "── Voice input",
        &["◇ Thinking (3s · esc to interrupt)", ""],
    );
    assert_eq!(
        phase(&status),
        (Phase::Busy, true),
        "a status row under them"
    );
    let running: Vec<String> = rows
        .iter()
        .map(|r| r.replace("        FAILED │", "       RUNNING │"))
        .collect();
    assert_eq!(card_state(&running), Some("RUNNING"));
    assert_eq!(
        phase(&running),
        (Phase::Idle, false),
        "RUNNING, no status row"
    );
    let usage_after = inserted(
        &running,
        "── Voice input",
        &[
            "┌───────────────────────┐",
            "│  Session usage        │",
            "└───────────────────────┘",
            "",
        ],
    );
    assert_eq!(
        phase(&usage_after),
        (Phase::Idle, false),
        "a /usage card names no state"
    );
    let sent = inserted(&rows, "┌", &["❯ carry on with the task", ""]);
    assert_eq!(phase(&sent), (Phase::Idle, false), "cards under a message");
    let footer = rows.len() - 1;
    let mut stopping = rows.clone();
    stopping[footer].push_str("                    interrupting run");
    assert_eq!(phase(&stopping), (Phase::Busy, false), "still stopping");
    let at = rows
        .iter()
        .position(|r| r.starts_with("── Voice"))
        .expect("the frame");
    let bare: Vec<String> = std::iter::repeat_n(String::new(), at)
        .chain(rows[at..].iter().cloned())
        .collect();
    assert_eq!(phase(&bare), (Phase::Idle, false), "blank rows alone");
}

/// A SESSION WITH NO TURN YET IS ONE UNDER ITS CARDS TOO (1.4.10, the lab of
/// 2026-10-21): `/status`, `/usage` and `/status` twice more at a fresh
/// session's empty composer pushed its banner out of sight, each `/status`
/// card saying `IDLE`, the `/usage` card counting `Turns 0`. No turn yet,
/// idle as evidence: nobody has asked it anything, and the screen is all
/// the evidence there is (no transcript of Muse Code's is read). So too in a
/// 24-row window of it, its last card alone. Read before as a session at
/// rest after a turn (`Carded`, not fresh), where the turn-end policy typed
/// a continuation into the session nobody had given a task. The cards name
/// the run the session rests on: `COMPLETED` and `FAILED` are a turn's end
/// at rest (the `FAILED` one hiding the wall its notice was: [`end_hidden`]),
/// `CANCELLED` a person's Esc ([`interrupted`]; measured: Esc on a first
/// turn, then `/status` three times). NEGATIVE CONTROLS: `RUNNING` and a
/// state nothing measured are no evidence, and so is `IDLE` over a count of
/// tokens spent (nothing measured draws it); a `/usage` card alone is at rest
/// where it counts a turn, and no evidence at `Turns 0`, which a session
/// whose first turn a person stopped counts too (its `/status` card says
/// `0 turns`).
#[test]
fn a_fresh_sessions_cards_alone_are_no_turn_yet() {
    let rows = screen(FRESH_STATUS_CARDS);
    let window = rows[rows.len() - 24..].to_vec();
    for (what, rows) in [("the measured screen", &rows), ("its 24 rows", &window)] {
        let r = read(Some("muse"), rows, None);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative, r.fresh),
            (Phase::Idle, true, true),
            "{what}: no turn yet"
        );
        assert_eq!(
            (r.wall, r.said_tail, r.prompt),
            (None, None, None),
            "{what}"
        );
        assert!(!r.interrupted && !end_hidden(rows), "{what}");
        assert_eq!(card_state(rows), Some("IDLE"), "{what}");
    }
    assert_eq!(usage_turns(&rows), Some(0));
    assert_eq!(
        composer_draft(&rows).map(|(_, d)| d),
        Some(vec![String::new()])
    );
    // The runs a turn was taken on.
    for (name, text, hidden, stopped) in [
        ("completed", COMPLETED_STATUS_CARDS, false, false),
        ("failed", LIMIT_UNNAMED_STATUS_CARDS, true, false),
        (
            "failed at the usage wall",
            USAGE_WALL_STATUS_CARDS,
            true,
            false,
        ),
        ("cancelled", INTERRUPTED_STATUS_CARDS, false, true),
    ] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative, r.fresh),
            (Phase::Idle, true, false),
            "{name}: at rest after a turn"
        );
        assert_eq!(
            (r.wall, r.said_tail, r.prompt),
            (None, None, None),
            "{name}"
        );
        let rows = screen(text);
        assert_eq!(
            (end_hidden(&rows), r.interrupted),
            (hidden, stopped),
            "{name}"
        );
    }
    assert_eq!(
        usage_turns(&screen(INTERRUPTED_STATUS_CARDS)),
        None,
        "no /usage card in sight"
    );
    // The controls.
    let state = |word: &str| -> Vec<String> {
        rows.iter()
            .map(|r| r.replace(" IDLE │", &format!(" {word} │")))
            .collect()
    };
    for word in ["RUNNING", "PAUSED"] {
        let other = state(word);
        assert_eq!(card_state(&other), Some(word));
        assert_eq!(
            (phase(&other), fresh(&other), end_hidden(&other)),
            ((Phase::Idle, false), false, false),
            "{word}: no evidence"
        );
    }
    // `IDLE` is no turn yet only over no token spent, as measured: over a
    // count of tokens, which nothing measured draws, it is no evidence.
    assert!(status_spent_none(&rows) && status_spent_none(&screen(INTERRUPTED_STATUS_CARDS)));
    assert!(!status_spent_none(&screen(COMPLETED_STATUS_CARDS)));
    let spent: Vec<String> = rows
        .iter()
        .map(|r| r.replace("0 tokens · 0 turns ·", "33,412 tokens · 1 turn ·"))
        .collect();
    assert_eq!(card_state(&spent), Some("IDLE"));
    assert_eq!(
        (phase(&spent), fresh(&spent), end_hidden(&spent)),
        ((Phase::Idle, false), false, false),
        "IDLE over tokens spent: no evidence"
    );
    // A /usage card alone: its count says whether a turn was taken.
    let at = rows
        .iter()
        .position(|r| r.is_empty())
        .expect("the blank row under the /usage card");
    let usage_alone: Vec<String> = rows[..=at]
        .iter()
        .chain(&rows[rows.len() - 4..])
        .cloned()
        .collect();
    assert_eq!(card_state(&usage_alone), None);
    assert_eq!(usage_turns(&usage_alone), Some(0));
    assert_eq!(
        (phase(&usage_alone), fresh(&usage_alone)),
        ((Phase::Idle, false), false),
        "Turns 0: no evidence"
    );
    let one: Vec<String> = usage_alone
        .iter()
        .map(|r| r.replace("Turns         0 │", "Turns         1 │"))
        .collect();
    assert_eq!(usage_turns(&one), Some(1));
    assert_eq!(
        (
            phase(&one),
            fresh(&one),
            end_hidden(&one),
            interrupted(&one)
        ),
        ((Phase::Idle, true), false, true, false),
        "a turn taken: at rest, its end out of sight"
    );
    // An unreadable newest /status head blocks both older status states
    // and an older /usage count. A partial redraw says nothing of the end.
    for (name, older) in [
        ("idle", rows),
        ("failed", screen(USAGE_WALL_STATUS_CARDS)),
        ("completed", screen(COMPLETED_STATUS_CARDS)),
        ("cancelled", screen(INTERRUPTED_STATUS_CARDS)),
        ("usage", one),
    ] {
        for newest in [
            "│  MUSE CODE 1.4.10 / partial RUNNING",
            "│  MUSE CODE 1.4.10 / partial running │",
            "│  MUSE CODE 1.4.10 / partial │",
        ] {
            let other = inserted(
                &older,
                "── Voice input",
                &["┌─────────┐", newest, "└─────────┘", ""],
            );
            assert_eq!(card_state(&other), None, "{name}: {newest}");
            assert_eq!(
                (
                    phase(&other),
                    fresh(&other),
                    end_hidden(&other),
                    interrupted(&other)
                ),
                ((Phase::Idle, false), false, false, false),
                "{name}: {newest} cannot lend evidence from an older card"
            );
        }
    }
}

/// NO SCREEN WITH A BOX ON IT READS IDLE: every capture of a box or a
/// question before its answer is a prompt as evidence, some option of it
/// carrying a role — but the boxes read with none, whose every option is
/// [`Role::Other`], a person's (module header): the network box that names
/// no URL (a command's host, a Unix socket), a call of several questions, a
/// multi-select and a review.
/// NEGATIVE CONTROL: every capture after the answer reads no box and is
/// idle.
#[test]
fn no_screen_with_a_box_reads_idle() {
    let persons = [
        "-box-network-command",
        "-box-unix-socket",
        "-question-multi",
        "-question-1-of-2",
        "-review",
    ];
    let (mut up, mut roleless, mut after) = (0, 0, 0);
    for (name, text) in captures() {
        if !(name.contains("-box-") || name.contains("-question")) {
            continue;
        }
        let r = muse(&text);
        if [
            "-approved",
            "-denied",
            "-answered",
            "-enter",
            "-interrupted",
        ]
        .iter()
        .any(|s| name.contains(s))
        {
            after += 1;
            assert_eq!((r.prompt, r.phase), (None, Phase::Idle), "{name}");
            continue;
        }
        up += 1;
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Prompt, true),
            "{name}"
        );
        let p = r.prompt.expect(&name);
        if persons.iter().any(|s| name.contains(s)) {
            roleless += 1;
            assert!(p.options.iter().all(|o| o.role == Role::Other), "{name}");
        } else {
            assert!(p.options.iter().any(|o| o.role != Role::Other), "{name}");
        }
    }
    assert_eq!((up, roleless, after), (58, 21, 18));
}

/// THE TRUST BOX IS A BOX NO TYPED TEXT MAY REACH (a typed `y` answered it,
/// measured on 1.4.9): on both builds it is a prompt with no composer — no
/// caret row for a guard on typed text to anchor to — and its Esc quits.
/// NEGATIVE CONTROL: the idle screen right after it has one.
#[test]
fn the_trust_box_has_no_composer_to_type_into() {
    for text in [BOX_TRUST, TRUST_1_4_10] {
        let r = muse(text);
        assert_eq!((r.phase, r.composer), (Phase::Prompt, None));
        assert!(!screen(text).iter().any(|row| row.starts_with('❯')));
        assert_eq!(
            r.prompt.and_then(|p| p.cancel).map(|c| c.effect),
            Some(CancelEffect::Exit)
        );
    }
    assert!(muse(IDLE_1_4_10).composer.is_some(), "the control");
}

/// THE SHELL BOX ASKING FOR ONE STAGE OF A PIPELINE (1.4.10) is a Bash
/// prompt of the WHOLE command: the stage rows under it (`Skipped Stage
/// 1/3: …`, `Stage 3/3`, `Current argv: ["sort"]`) are its notes, no part
/// of the command; `Allow this stage once` is the one-shot allow and `Abort
/// the entire command`, its `(esc)`, the refusal. NEGATIVE CONTROLS: the
/// plain shell box has no notes, and a row that only begins with `Stage`
/// and no number is the command's.
#[test]
fn the_staged_shell_box_is_a_bash_prompt_of_its_whole_command() {
    let r = muse(BOX_SHELL_STAGE);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Prompt, true)
    );
    let p = r.prompt.expect("the box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.command, "env | cut -d= -f1 | sort > envnames.txt");
    assert_eq!(
        p.notes,
        vec![
            "Skipped Stage 1/3: matched mode:on_request".to_string(),
            "Stage 3/3".to_string(),
            "Current argv: [\"sort\"]".to_string(),
        ]
    );
    let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            anchor_text("muse.box.stage_once"),
            anchor_text("muse.box.stage_abort")
        ]
    );
    assert_eq!(roles(&p), vec![Role::Once, Role::Deny]);
    assert!(p.options[0].focused);
    assert_eq!(
        p.cancel.map(|c| c.effect),
        Some(CancelEffect::Reject),
        "Esc aborts the command"
    );
    assert!(muse(BOX_SHELL).prompt.expect("the box").notes.is_empty());
    let staged = edited(
        &screen(BOX_SHELL_STAGE),
        "  Skipped Stage",
        "  Stage manager",
    );
    let p = prompt(&staged).expect("the box");
    assert_eq!(
        p.command,
        "env | cut -d= -f1 | sort > envnames.txt Stage manager"
    );
    assert_eq!(p.notes.len(), 2);
}

/// A BOX WHOSE BODY WRAPS IS READ WHOLE (measured at 60 columns): the file
/// box whose path went under a bare `write path:` row is a Write of that
/// whole path, its rows joined with nothing, with its roles; the shell box's
/// command is its `$` row and the rows indented under it, joined with a
/// blank (a word broken at the edge keeps that blank, as on Codex's and
/// agy's boxes). NEGATIVE CONTROLS: the bare `write path:` with no path
/// under it, and a `write paths:` row, are boxes of no kind whose options
/// carry no role; the box drawn at 144 columns reads the same path off one
/// row.
#[test]
fn a_box_whose_body_wraps_is_read_whole() {
    let r = muse(BOX_WRITE_WRAPPED);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Prompt, true)
    );
    let p = r.prompt.expect("the box");
    assert_eq!(p.kind, PromptKind::Write);
    assert_eq!(
        p.path.as_deref(),
        Some(
            "/private/tmp/at.lab0/proj/.agents/a-protected-note-with-a-rather-long-file-name-for-wrapping.md"
        )
    );
    assert_eq!(Some(p.command.as_str()), p.path.as_deref());
    assert_eq!(roles(&p), vec![Role::Once, Role::Deny]);
    let rows = screen(BOX_WRITE_WRAPPED);
    let at = rows
        .iter()
        .position(|r| r.trim_end() == "  write path:")
        .expect("the bare label row");
    let mut bare = rows.clone();
    bare.drain(at + 1..at + 3);
    let renamed = edited(&rows, "  write path:", "  write paths:");
    for other in [bare, renamed] {
        assert_eq!(phase(&other), (Phase::Prompt, true));
        let p = prompt(&other).expect("still a box");
        assert_eq!((p.kind, p.path.as_deref()), (PromptKind::Other, None));
        assert!(roles(&p).iter().all(|&r| r == Role::Other));
    }
    assert_eq!(
        prompt(&screen(BOX_WRITE)).and_then(|p| p.path),
        Some("/private/tmp/at.lab0/proj/.agents/probe-write.md".to_string())
    );

    let r = muse(BOX_SHELL_WRAPPED);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Prompt, true)
    );
    let p = r.prompt.expect("the box");
    assert_eq!(p.kind, PromptKind::Bash);
    let (head, tail) = ("b".repeat(33), "b".repeat(17));
    assert_eq!(
        p.command_rows,
        vec![
            "touch".to_string(),
            format!("probe-with-a-long-name-{head}"),
            format!("{tail}.txt && echo two words here"),
        ]
    );
    assert_eq!(
        p.command,
        format!("touch probe-with-a-long-name-{head} {tail}.txt && echo two words here")
    );
    assert_eq!(roles(&p), vec![Role::Once, Role::Deny]);
}

/// AN AWAY RECAP IS NO PART OF A TURN (1.4.10): the finished report under
/// `◆ Recap: ok` ends its turn as evidence, its last row the token; and with
/// the recap drawn UNDER that reply — where it is drawn while a message
/// typed after a pause still stands in the composer — the turn still ends on
/// the reply, its words the last words. So does a reply under `◆ Worked for
/// 5m 27s · …` and a recap (measured live). NEGATIVE CONTROL: with the reply
/// gone the recap alone is no turn's end (idle, no evidence, no last words).
#[test]
fn an_away_recap_is_no_part_of_the_turn() {
    let worked = muse(END_OF_TURN_WORKED_RECAP);
    assert_eq!(
        (worked.phase, worked.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(
        worked.said_tail.as_deref(),
        Some(
            "Tool call done. Result: \
             {\"status\":\"answered\",\"answers\":[{\"id\":\"color\",\"selected_label\":\"Blue\"}]}"
        )
    );
    let r = muse(END_OF_TURN_FINISHED);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    let said = r.said_tail.expect("the report");
    assert_eq!(said.lines().last(), Some("ATERM-TASK-DONE"));
    let rows = screen(END_OF_TURN_FINISHED);
    let recap = rows
        .iter()
        .position(|r| r.starts_with("◆ Recap: "))
        .expect("the recap");
    let mut under: Vec<String> = rows.clone();
    let row = under.remove(recap);
    let top = under
        .iter()
        .rposition(|r| r.starts_with("── Voice input"))
        .expect("the composer");
    under.insert(top, row);
    assert_eq!(under[top + 2], "❯");
    under[top + 2] = "❯ keep going".to_string();
    assert_eq!(phase(&under), (Phase::Idle, true));
    assert_eq!(said_tail(&under).as_deref(), Some(said.as_str()));
    assert_eq!(
        composer_draft(&under).map(|(_, l)| l),
        Some(vec!["keep going".to_string()])
    );
    let message = under
        .iter()
        .rposition(|r| r.starts_with("❯ report and finish"))
        .expect("the message");
    let alone: Vec<String> = under
        .iter()
        .enumerate()
        .filter(|&(i, r)| !(i > message && (r.starts_with("◆ Done") || r.starts_with("  ATERM"))))
        .map(|(_, r)| r.clone())
        .collect();
    assert_eq!(phase(&alone), (Phase::Idle, false), "the control");
    assert_eq!(said_tail(&alone), None);
}

/// A LONG DRAFT IS READ WHOLE (1.4.10): wrapped between words, its
/// continuation rows indented two, every row of it is the composer's, and
/// the rows joined are the text typed. An Enter in the same burst as the
/// text was a newline: `keep going`, then the `X` typed after it on a row of
/// its own. NEGATIVE CONTROL: the idle composer's draft is empty.
#[test]
fn a_long_draft_and_a_burst_enter_are_read_whole() {
    let typed = "keep going. Standing rules: never push to main without the merge contract \
                 green, run the tests for what you touched, keep the shared tree clean, commit \
                 each finished slice with its own message, and say what you measured and what \
                 you did not, in plain words without hedging, before ending the turn here";
    let r = muse(DRAFT_WRAPPED);
    let (_, lines) = r.composer.expect("the composer");
    assert_eq!(lines.len(), 3);
    assert_eq!(lines.join(" "), typed);
    assert_eq!(
        lines.last().map(String::as_str),
        Some("before ending the turn here")
    );
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    let burst = muse(DRAFT_BURST_ENTER);
    assert_eq!(
        burst.composer.map(|(_, l)| l),
        Some(vec!["keep going".to_string(), "X".to_string()])
    );
    assert_eq!(
        muse(IDLE_1_4_10).composer.map(|(_, l)| l),
        Some(vec![String::new()]),
        "the control"
    );
}

/// MUSE CODE'S OWN REMEDY for each key wall is its own clause: the measured
/// 401's `update or unset it`, with its `it` named (`META_API_KEY`, which
/// beats the saved login), and the binary's two siblings' `run \`muse
/// login\` …`. NEGATIVE CONTROLS: the signed-out menu's wall and a 429 name
/// no remedy of Muse Code's.
#[test]
fn each_key_wall_names_muse_codes_own_remedy() {
    let wall = |text: &str| muse(text).wall.expect("a wall").message;
    assert_eq!(
        remedy(&wall(KEY_REJECTED)).as_deref(),
        Some("update or unset META_API_KEY")
    );
    assert_eq!(
        remedy(anchor_text("muse.wall.saved_key")).as_deref(),
        Some("run `muse login` or add a new key")
    );
    assert_eq!(
        remedy(anchor_text("muse.wall.no_login")).as_deref(),
        Some("run `muse login` or set META_API_KEY")
    );
    for text in [SIGNED_OUT, RATE_LIMIT_FINAL] {
        assert_eq!(remedy(&wall(text)), None, "the control");
    }
}

/// `rows` with `with` put in before the row that starts with `at`.
fn inserted(rows: &[String], at: &str, with: &[&str]) -> Vec<String> {
    let k = rows
        .iter()
        .position(|r| r.starts_with(at))
        .unwrap_or_else(|| panic!("a row starting {at:?}"));
    let mut out = rows[..k].to_vec();
    out.extend(with.iter().map(|s| (*s).to_string()));
    out.extend(rows[k..].iter().cloned());
    out
}

/// A DRAWER OR A PICKER takes the composer's keys (the binary's key rows:
/// `↑↓ move · enter confirm · esc cancel` over the rule, `enter use - tab
/// switch - esc close - type filter` under the caret): no composer, no
/// draft, a prompt as evidence with no box to answer — nothing is typed
/// into it. NEGATIVE CONTROLS: the same screen with no drawer has its
/// composer, and a row of Claude Code's (`esc to cancel`) or of a question
/// (`Esc to interrupt`) is no drawer's.
#[test]
fn a_drawer_holds_the_composer() {
    let rows = screen(END_OF_TURN);
    let above = inserted(
        &rows,
        "── Voice input",
        &[
            "  Export",
            "  ⟩ Transcript",
            "    Trajectory",
            "  ↑↓ move · enter confirm · esc cancel",
            "",
        ],
    );
    let caret = composer(&rows).expect("the composer");
    let mut inside = rows.clone();
    inside.splice(
        caret + 1..caret + 1,
        [
            "  ⟩ fake-1",
            "    fake-2",
            "  enter use - tab switch - esc close - type filter",
        ]
        .map(String::from),
    );
    for drawn in [&above, &inside] {
        assert_eq!(phase(drawn), (Phase::Prompt, true));
        assert_eq!(
            (composer(drawn), composer_draft(drawn), prompt(drawn)),
            (None, None, None)
        );
        assert_eq!(said_tail(drawn), None);
    }
    assert_eq!(composer(&rows), Some(caret), "the control");
    for row in [
        "  Continuing automatically at 3pm · esc to cancel",
        "  Enter to select · ↑/↓ to move · Esc to interrupt",
    ] {
        let quoted = inserted(&rows, "── Voice input", &[row, ""]);
        assert!(composer(&quoted).is_some(), "{row}");
    }
}

/// THE SIDE CHAT is another conversation (the binary's `Side Chat · …` top
/// rule, `Side chat mode` footer): no composer of this one, and no evidence.
/// Every label the binary draws on the top rule names Muse Code, and a box
/// under any of them is the same box. NEGATIVE CONTROL: the composer under
/// `Voice input (⌥ + v to start)` is this conversation's.
#[test]
fn the_side_chat_is_no_composer_and_every_label_names_muse() {
    let rows = screen(END_OF_TURN);
    let top = rows
        .iter()
        .position(|r| r.starts_with("── Voice input"))
        .expect("the rule");
    let mut side = rows.clone();
    side[top] = format!("── Side Chat · main {}", "─".repeat(60));
    let mut footed = rows.clone();
    let last = footed.len() - 1;
    footed[last] = "  echo · /work · Side chat mode".to_string();
    for s in [&side, &footed] {
        assert_eq!((composer(s), phase(s)), (None, (Phase::Idle, false)));
    }
    assert_eq!(identify(None, &side).program(), Program::Muse);
    let boxed = screen(BOX_COMMAND);
    let at = boxed
        .iter()
        .position(|r| r.starts_with("── Voice input"))
        .expect("the box's rule");
    for label in ["Hold ⌥ v to talk", "Recording 0:03", "Voice offline"] {
        let mut l = rows.clone();
        l[top] = format!("── {label} {}", "─".repeat(60));
        assert_eq!(identify(None, &l).program(), Program::Muse, "{label}");
        let mut b = boxed.clone();
        b[at] = format!("── {label} {}", "─".repeat(60));
        assert_eq!(
            prompt(&b).map(|p| roles(&p)),
            Some(vec![Role::Once, Role::Deny]),
            "{label}"
        );
    }
    assert_eq!(composer(&rows), Some(top + 1), "the control");
}

/// A TURN FAILURE IN THE BINARY'S WORDS is a wall: a login gone (`Your saved
/// Meta credentials are invalid. …`) a sign-in wall, a quota spent (`You've
/// used your weekly quota (resets at 9:00 PM).`, a bare `Usage limit
/// reached.`) a usage wall, limited until its reset; a fault only a person
/// fixes (`A configuration problem stopped this turn.`) is no evidence and
/// no last words. NEGATIVE CONTROL: the reply itself is no wall.
#[test]
fn a_turn_failure_is_a_wall_or_no_evidence() {
    let rows = screen(END_OF_TURN);
    let reply = |text: &str| edited(&rows, "◆ echo", text);
    let auth = reply(
        "◆ Your saved Meta credentials are invalid. Your message was not sent. Quit Muse Code and \
         run `muse login`.",
    );
    assert_eq!(wall(&auth).map(|w| w.kind), Some(WallKind::Auth));
    assert_eq!(said_tail(&auth), None);
    for (spent, reset) in [
        (
            "◆ You've used your weekly quota (resets at 9:00 PM).",
            Some("9:00 PM"),
        ),
        ("◆ Usage limit reached.", None),
    ] {
        let s = reply(spent);
        let w = wall(&s).expect(spent);
        assert_eq!(
            (w.kind, w.reset.as_deref()),
            (WallKind::UsageSession, reset)
        );
        assert!(
            matches!(phase(&s), (Phase::Limited { .. }, true)),
            "{spent}"
        );
    }
    let fault = reply("◆ A configuration problem stopped this turn. Fix it in /settings.");
    assert_eq!(
        (phase(&fault), said_tail(&fault), wall(&fault)),
        ((Phase::Idle, false), None, None)
    );
    assert_eq!(
        (wall(&rows), phase(&rows)),
        (None, (Phase::Idle, true)),
        "the control"
    );
}

/// THE NOTICES its background work leaves under a reply (the binary's `◆
/// Finished <name> · <time>`, `◆ Message from …`) are no words of the
/// reply, as its measured `Worked for` and `Recap` rows are not
/// ([`fixtures::END_OF_TURN_FOOTNOTES_HINT`]: a finished report's
/// `ATERM-TASK-DONE` is its last words). NEGATIVE CONTROL: a reply that
/// begins `Finished` or `Worked for` with no time after it is a reply.
#[test]
fn background_notices_are_no_replys_words() {
    let rows = screen(END_OF_TURN);
    for notice in [
        "◆ Finished nightly-build · 1m 30s",
        "◆ Message from s-2: the build passed",
    ] {
        let noted = inserted(&rows, "── Voice input", &[notice, ""]);
        assert_eq!(
            said_tail(&noted).as_deref(),
            Some("echo: hello there"),
            "{notice}"
        );
    }
    let said = muse(END_OF_TURN_FOOTNOTES_HINT)
        .said_tail
        .expect("the report");
    assert!(said.starts_with("Top finding:"), "{said}");
    assert!(said.ends_with("ATERM-TASK-DONE"), "{said}");
    for reply in [
        "◆ Finished the work · today",
        "◆ Worked for the team all day",
    ] {
        let worded = edited(&rows, "◆ echo", reply);
        assert_eq!(
            said_tail(&worded).as_deref(),
            reply.strip_prefix("◆ "),
            "the control"
        );
    }
}

/// A REPLY TALLER THAN THE SCREEN, its head and its message scrolled away,
/// is read by its rows in sight: idle as evidence, its last words those
/// rows. NEGATIVE CONTROL: a row in column 0 among them is a row this
/// grammar does not know, and no evidence.
#[test]
fn a_reply_taller_than_the_screen_is_read_by_its_rows_in_sight() {
    let rows = screen(END_OF_TURN);
    let at = rows
        .iter()
        .position(|r| r.starts_with("◆ echo"))
        .expect("the reply");
    let mut tall: Vec<String> = [
        "  the report's middle",
        "  - all checks passed",
        "",
        "  ATERM-TASK-DONE",
        "",
    ]
    .map(String::from)
    .to_vec();
    tall.extend(rows[at + 1..].iter().cloned());
    assert_eq!(phase(&tall), (Phase::Idle, true));
    assert_eq!(
        said_tail(&tall).as_deref(),
        Some("the report's middle\n- all checks passed\nATERM-TASK-DONE")
    );
    let mut odd = tall.clone();
    odd[1] = "some other row".to_string();
    assert_eq!(
        (phase(&odd), said_tail(&odd)),
        ((Phase::Idle, false), None),
        "the control"
    );
}

/// A SESSION OPENS ON ITS BANNER: Muse Code run again in a tab after it
/// exited ([`fixtures::IDLE_RELAUNCHED`], measured), the earlier session's
/// rows above, is fresh — under a shell's `$` line or a prompt that launches
/// it — and its live zone starts under the banner, so a status row an
/// earlier reply quoted holds no wait. NEGATIVE CONTROLS: a banner under a
/// message's `❯` row is a quote, and with no composer the zone is the
/// whole screen.
#[test]
fn a_relaunched_session_is_fresh_under_its_new_banner() {
    let rows = screen(IDLE_RELAUNCHED);
    assert_eq!((phase(&rows), fresh(&rows)), ((Phase::Idle, true), true));
    let starship = edited(&rows, "$ muse-fake", "❯ muse --approval-mode untrusted");
    assert!(fresh(&starship));
    let quoted = edited(&rows, "$ muse-fake", "❯ what does the banner say?");
    assert!(!fresh(&quoted), "the control");

    // Eligibility depends on the prefix, including the prefix of a later
    // candidate: a banner need not change for the newest reading to change.
    for (before, changed_row, replacement, wanted_before, wanted_after) in [
        (
            vec!["$ muse", "  Muse Code 1.4.10"],
            0,
            "  quoted reply",
            Some(1),
            None,
        ),
        (
            vec!["  quoted reply", "  Muse Code 1.4.10"],
            0,
            "$ muse",
            None,
            Some(1),
        ),
        (
            vec![
                "$ muse",
                "  Muse Code 1.4.9",
                "",
                "  quoted reply",
                "",
                "  Muse Code 1.4.10",
            ],
            3,
            "$ muse",
            Some(1),
            Some(5),
        ),
    ] {
        let before: Vec<String> = before.into_iter().map(str::to_owned).collect();
        let reading = launch_card_scan(&before);
        assert_eq!(reading.0, wanted_before);
        assert!(launch_card_unmoved(&before, reading.0, &[]));
        let mut after = before;
        after[changed_row] = replacement.to_owned();
        assert_eq!(launch_card_scan(&after).0, wanted_after);
        assert_ne!(launch_card_scan(&after), reading);
        assert!(!launch_card_unmoved(&after, reading.0, &[changed_row]));
    }
    // A keystroke in the composer under the banner keeps the reading, with
    // a banner and with none; and on every row of the screen, a reading
    // kept is the reading a scan takes.
    let composer = rows
        .iter()
        .rposition(|r| r.starts_with('❯'))
        .expect("the composer");
    let typed = edited(&rows, "❯", "❯ typed");
    assert!(launch_card_unmoved(
        &typed,
        launch_card_scan(&rows).0,
        &[composer]
    ));
    let gone = edited(&rows, "  Muse Code 1.4.9", "");
    assert_eq!(launch_card_scan(&gone).0, None, "CONTROL: no banner");
    assert!(launch_card_unmoved(
        &edited(&gone, "❯", "❯ typed"),
        None,
        &[composer]
    ));
    for invalid in [rows.len(), usize::MAX] {
        assert!(!launch_card_unmoved(
            &rows,
            launch_card_scan(&rows).0,
            &[invalid]
        ));
        assert!(!launch_card_unmoved(&gone, None, &[invalid]));
    }
    assert!(!launch_card_unmoved(&[], None, &[usize::MAX]));
    for before in [&rows, &quoted, &gone] {
        let reading = launch_card_scan(before);
        for i in 0..before.len() {
            for replacement in [
                "",
                "  quoted reply",
                "$ muse",
                "  Muse Code 1.4.11",
                "❯ typed",
            ] {
                let mut after = before.clone();
                after[i] = replacement.to_owned();
                if launch_card_unmoved(&after, reading.0, &[i]) {
                    assert_eq!(
                        launch_card_scan(&after),
                        reading,
                        "row {i}: {replacement:?}"
                    );
                }
            }
        }
    }

    let busy = screen(BUSY);
    let message = busy
        .iter()
        .position(|r| r.starts_with("❯ hello there"))
        .expect("the message");
    let mut earlier = busy.clone();
    earlier.splice(
        message..message,
        [
            "❯ what does the status row say?",
            "",
            "◆ It says:",
            "  ◇ Thinking (2s · esc to interrupt)",
            "",
        ]
        .map(String::from),
    );
    let zone = MuseReader.busy_zone_start(&earlier);
    let quote = earlier
        .iter()
        .position(|r| r.contains("◇ Thinking"))
        .expect("the quote");
    let live = earlier
        .iter()
        .rposition(|r| r.starts_with("◇ Thinking"))
        .expect("the status row");
    assert!(quote < zone && zone <= live, "{quote} < {zone} <= {live}");
    assert_eq!(phase(&earlier), (Phase::Busy, true));
    assert_eq!(
        MuseReader.busy_zone_start(&screen(EXITED)),
        0,
        "the control"
    );
}

/// A BOX CAUGHT MID-DRAW, its title row not drawn yet under the labelled
/// rule, is no box yet: no prompt to escalate and hold. NEGATIVE CONTROLS:
/// the whole box carries its roles, and a box whose rule is above the rows
/// read is still a box, its head off the screen.
#[test]
fn a_box_caught_mid_draw_is_no_box_yet() {
    let rows = screen(BOX_COMMAND);
    let mid = edited(&rows, "Would you like to run", "");
    assert_eq!(prompt(&mid), None);
    assert_eq!(phase(&mid), (Phase::Idle, false));
    assert_eq!(
        prompt(&rows).map(|p| roles(&p)),
        Some(vec![Role::Once, Role::Deny]),
        "the control"
    );
    let first = rows
        .iter()
        .position(|r| r.starts_with("⟩ 1. "))
        .expect("an option");
    assert!(prompt(&rows[first - 1..]).is_some_and(|p| p.head_off_screen));
}

/// SEVERAL QUESTIONS ARE A PERSON'S (measured on 1.4.9): the first of two
/// (`  1 of 2` over a key row adding `←/→ to switch question`) is a question
/// with no answer role, and so is the review of their answers (`Review
/// answers before submit`, the answers its description, `Submit answers`
/// and `Interrupt turn` its options), whose Esc goes back; neither leaves a
/// composer to type into. The cursor moved onto an answer (to edit it)
/// leaves the review read, no option focused. A standing grant that begins
/// `Always allow` is kept, never the one-shot allow. NEGATIVE CONTROL: the
/// single question carries its roles.
#[test]
fn several_questions_and_their_review_are_a_persons() {
    for text in [BOX_QUESTION_1_OF_2, BOX_QUESTION_REVIEW] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative, r.composer.clone()),
            (Phase::Prompt, true, None)
        );
        let p = r.prompt.expect("the question");
        assert_eq!(p.kind, PromptKind::Question);
        assert!(p.options.iter().all(|o| o.role == Role::Other), "{p:?}");
    }
    let rows = screen(BOX_QUESTION_REVIEW);
    let review = prompt(&rows).expect("the review");
    assert!(is_review(&review));
    assert_eq!(review.title, "Request user input Colour, Size");
    assert_eq!(
        review.description,
        "Colour: Red (Recommended) Size: Small (Recommended)"
    );
    let labels: Vec<&str> = review.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, ["Submit answers", "Interrupt turn"]);
    assert_eq!(
        review.options.iter().position(|o| o.focused),
        Some(0),
        "Submit focused"
    );
    assert_eq!(review.cancel.map(|c| c.effect), Some(CancelEffect::Back));
    let mut editing = edited(&rows, "  ⟩ Submit answers", "    Submit answers");
    editing = edited(&editing, "    Size: Small", "  ⟩ Size: Small (Recommended)");
    let r = read(Some("muse"), &editing, None);
    assert_eq!((r.phase, r.composer), (Phase::Prompt, None));
    let moved = r.prompt.expect("still the review");
    assert_eq!(moved.description, review.description);
    assert_eq!(moved.options.iter().position(|o| o.focused), None);
    assert_eq!(
        prompt(&screen(BOX_COMMAND_STAGED)).map(|p| roles(&p)),
        Some(vec![Role::Once, Role::Persist, Role::Deny])
    );
    assert_eq!(
        prompt(&screen(BOX_QUESTION)).map(|p| roles(&p)),
        Some(vec![Role::Answer, Role::Answer, Role::Other]),
        "the control"
    );
}

/// The harness's long-term policy text, as `aterm-agent` writes it into a
/// question's note (`policy::question::LONG_TERM_TEXT`).
const LONG_TERM: &str = "Decide yourself: choose the option that best serves long term \
                         correctness and total value even at the cost of short term \
                         deliverables. Say which you chose and why in one line and continue \
                         without asking again. (Answered by the aterm harness by policy. The \
                         user was not asked.)";

/// THE NETWORK BOX (1.4.10, a web fetch and a monitor's WebSocket of the
/// lab's own stub) IS A NETWORK REQUEST OF ITS URL: `full URL: …` its
/// command (a URL broken at a narrow tab's edge joined with nothing),
/// `network: <host:port> <protocol>` its note, `Yes, proceed` the one-shot
/// allow, `Yes, don't ask again this session` a session grant, `Always allow
/// this network destination` a standing one and `No, …` the refusal — each
/// label less its one-key mark and the destination drawn beside it, the
/// always grant's `(http)` that a 60-column tab carried onto a row of its
/// own in column 0 no option of its own. Chosen by the cursor and Enter; its
/// Esc the refusal. NEGATIVE CONTROLS: the box's title over a body with no
/// `full URL:` row is a box of no kind and no roles, and so is the
/// tool-action box's title over the network box's body (no `tool action:`
/// row under it); the network box of a command (`requested by:` and the
/// command, measured) is one too, its command read (its wrapped tail joined
/// with a blank) and no command row, where those two read none.
#[test]
fn the_network_box_is_a_network_request_of_its_url() {
    for (text, url) in [
        (BOX_NETWORK, "http://127.0.0.1:50581/lab-probe"),
        (
            BOX_NETWORK_NARROW,
            "http://127.0.0.1:50581/lab-probe-a-rather-long-path-for-the-narrow-tab",
        ),
        (BOX_NETWORK_MONITOR, "ws://127.0.0.1:50581/lab-ws"),
    ] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative, r.composer.clone()),
            (Phase::Prompt, true, None),
            "{url}"
        );
        let p = r.prompt.expect("the box");
        assert_eq!(p.kind, PromptKind::NetworkRequest, "{url}");
        assert_eq!(p.title, anchor_text("muse.box.network"));
        assert_eq!((p.command.as_str(), p.path.as_deref()), (url, None));
        assert_eq!(p.notes, vec!["network: 127.0.0.1:50581 http".to_string()]);
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                anchor_text("muse.box.once"),
                anchor_text("muse.box.session"),
                anchor_text("muse.box.always"),
                anchor_text("muse.box.deny"),
            ],
            "{url}"
        );
        assert_eq!(
            roles(&p),
            vec![Role::Once, Role::Session, Role::Persist, Role::Deny]
        );
        assert!(p.options[0].focused);
        assert_eq!(p.select, Select::ArrowsEnter);
        assert_eq!(
            p.cancel.map(|c| (c.verb, c.effect)),
            Some((
                anchor_text("muse.box.deny").to_string(),
                CancelEffect::Reject
            ))
        );
    }
    let rows = screen(BOX_NETWORK);
    let bodiless: Vec<String> = rows
        .iter()
        .filter(|r| !r.trim_start().starts_with("full URL:"))
        .cloned()
        .collect();
    let tool = edited(&rows, "Would you like", anchor_text("muse.box.tool"));
    // Measured: the network box a sandboxed `pip install` drew (`requested
    // by:` and the command, no `full URL:` row), its labels less the
    // destination beside them.
    let command = screen(BOX_NETWORK_COMMAND);
    let labels: Vec<String> = prompt(&command)
        .expect("the box")
        .options
        .into_iter()
        .map(|o| o.label)
        .collect();
    assert_eq!(
        labels,
        [
            anchor_text("muse.box.once"),
            anchor_text("muse.box.session"),
            anchor_text("muse.box.always"),
            anchor_text("muse.box.deny"),
        ]
    );
    let pip = "python3 -m pip install --quiet --target /tmp/pylibs pyte 2>&1 | tail -n 3; \
               PYTHONPATH=/tmp/pylibs python3 -c \"import pyte; print('pyte ok')\"";
    for (other, what) in [(bodiless, ""), (tool, ""), (command, pip)] {
        assert_eq!(phase(&other), (Phase::Prompt, true));
        let p = prompt(&other).expect("a box");
        assert_eq!(p.kind, PromptKind::Other);
        assert!(roles(&p).iter().all(|&r| r == Role::Other));
        assert_eq!(
            (p.command.as_str(), p.command_rows.len(), p.path),
            (what, 0, None)
        );
    }
}

/// THE TOOL-ACTION BOX (1.4.10, measured outside the lab's jail: a local
/// MCP server's tool, stdio and streamable HTTP alike) IS A TOOL CALL OF ITS
/// TOOL: `tool action: <tool>` its command (no path, no notes), `Yes,
/// proceed` the one-shot allow, `Yes, don't ask again this session` a
/// session grant, `Always allow this MCP tool` a standing one and `No, …`
/// the refusal — each label less its one-key mark and the ``tool `<tool>` ``
/// drawn beside it, which a 60-column tab cuts at its edge. The first
/// focused as drawn, the session grant after one Down; chosen by the cursor
/// and Enter; its Esc the refusal. Read alike at 144x50, 100x30 and 60
/// columns. NEGATIVE CONTROLS: with its `tool action:` row gone it is a box
/// of no kind and no roles, and so is that row under the network box's
/// title; the screens after the one-shot allow and after Esc are no box,
/// each idle on the reply.
#[test]
fn the_tool_action_box_is_a_tool_call_of_its_tool() {
    for (text, tool, focus) in [
        (BOX_TOOL_ACTION, "mcp__probe__ping", 0),
        (BOX_TOOL_ACTION_100X30, "mcp__probe__ping", 0),
        (BOX_TOOL_ACTION_NARROW, "mcp__probe__ping", 0),
        (BOX_TOOL_ACTION_DOWN, "mcp__probe__ping", 1),
        (BOX_TOOL_ACTION_HTTP, "mcp__probehttp__ping", 0),
    ] {
        let name = text.lines().next().unwrap_or_default();
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative, r.composer.clone()),
            (Phase::Prompt, true, None),
            "{name}"
        );
        let p = r.prompt.expect("the box");
        assert_eq!(p.kind, PromptKind::Tool, "{name}");
        assert_eq!(p.title, anchor_text("muse.box.tool"));
        assert_eq!(
            (p.command.as_str(), p.path.as_deref(), p.notes.len()),
            (tool, None, 0),
            "{name}"
        );
        assert!(p.command_rows.is_empty(), "{name}");
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                anchor_text("muse.box.once"),
                anchor_text("muse.box.session"),
                anchor_text("muse.box.always_tool"),
                anchor_text("muse.box.deny"),
            ],
            "{name}"
        );
        assert_eq!(
            roles(&p),
            [Role::Once, Role::Session, Role::Persist, Role::Deny],
            "{name}"
        );
        let focused: Vec<usize> = (0..p.options.len())
            .filter(|&k| p.options[k].focused)
            .collect();
        assert_eq!(focused, [focus], "{name}");
        assert_eq!(p.select, Select::ArrowsEnter);
        assert_eq!(
            p.cancel.map(|c| (c.key, c.verb, c.effect)),
            Some((
                "esc".to_string(),
                anchor_text("muse.box.deny").to_string(),
                CancelEffect::Reject
            )),
            "{name}"
        );
    }
    let rows = screen(BOX_TOOL_ACTION);
    let rowless: Vec<String> = rows
        .iter()
        .filter(|r| !r.trim_start().starts_with(anchor_text("muse.box.tool_row")))
        .cloned()
        .collect();
    let retitled = edited(&rows, "Would you like", anchor_text("muse.box.network"));
    for other in [rowless, retitled] {
        assert_eq!(phase(&other), (Phase::Prompt, true));
        let p = prompt(&other).expect("still a box");
        assert_eq!((p.kind, p.command.as_str()), (PromptKind::Other, ""));
        assert!(roles(&p).iter().all(|&r| r == Role::Other), "the control");
    }
    for (text, said) in [
        (
            BOX_TOOL_ACTION_APPROVED,
            "Done. Tool result: probe ping ran",
        ),
        (
            BOX_TOOL_ACTION_DENIED,
            "Done. Tool result: tool denied: approval aborted",
        ),
    ] {
        let r = muse(text);
        assert_eq!((r.prompt, r.phase), (None, Phase::Idle), "{said}");
        let tail = r.said_tail.expect("the reply");
        assert!(tail.starts_with(said), "{tail}");
    }
}

/// THE UNIX-SOCKET BOX (1.4.10, measured outside the lab's jail: `bash` with
/// `unix_socket_paths`, under the default `proxy-only` sandbox network) IS A
/// BOX OF NO KIND, A PERSON'S: the network title over `requested by:` and
/// the command, as the network box of a command draws it, naming no socket,
/// its options `Allow once` and `Reject` (their one-key marks stripped) with
/// no roles — the first focused as drawn, `Reject` after one Down (with `└
/// last event 39s ago` under the status row), the command wrapped at 60
/// columns read the same — chosen by the cursor and Enter, and its Esc
/// `Reject`, a refusal by its label (Muse Code answered it `Unix socket
/// access was not approved`, anchor `muse.box.socket_refused`; read before
/// as leaving the box undecided). Its command is read, the 60-column wrap
/// joined with a blank, and no path. NEGATIVE CONTROLS: `Reject` relabelled
/// leaves its Esc no refusal, the box under another title reads no command,
/// and the screens after `Allow once` and after Esc are no box, each idle on
/// the reply.
#[test]
fn the_unix_socket_box_is_a_persons_box_whose_esc_refuses() {
    for (text, focus) in [
        (BOX_UNIX_SOCKET, 0),
        (BOX_UNIX_SOCKET_DOWN, 1),
        (BOX_UNIX_SOCKET_NARROW, 0),
    ] {
        let name = text.lines().next().unwrap_or_default();
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative, r.composer.clone()),
            (Phase::Prompt, true, None),
            "{name}"
        );
        let p = r.prompt.expect("the box");
        assert_eq!(p.kind, PromptKind::Other, "{name}");
        assert_eq!(p.title, anchor_text("muse.box.network"));
        assert_eq!(
            (p.command.as_str(), p.path.as_deref(), p.command_rows.len()),
            (
                "printf 'ping' | /usr/bin/nc -U -w 2 /private/tmp/at.muse-sock/b6.sock",
                None,
                0
            ),
            "{name}"
        );
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Allow once", anchor_text("muse.box.reject")]);
        assert!(roles(&p).iter().all(|&r| r == Role::Other), "{name}");
        assert!(p.options[focus].focused, "{name}");
        assert!(!p.options[1 - focus].focused, "{name}");
        assert_eq!(p.select, Select::ArrowsEnter);
        assert_eq!(
            p.cancel,
            Some(Cancel {
                key: "esc".to_string(),
                verb: anchor_text("muse.box.reject").to_string(),
                effect: CancelEffect::Reject,
            }),
            "{name}"
        );
    }
    let relabelled = edited(
        &screen(BOX_UNIX_SOCKET),
        "  2. Reject",
        "  2. Not now (esc)",
    );
    let p = prompt(&relabelled).expect("still the box");
    assert_eq!(
        p.cancel.map(|c| (c.verb, c.effect)),
        Some(("Not now".to_string(), CancelEffect::Back)),
        "the control"
    );
    let retitled = edited(
        &screen(BOX_UNIX_SOCKET),
        anchor_text("muse.box.network"),
        anchor_text("muse.box.tool"),
    );
    let p = prompt(&retitled).expect("still a box");
    assert_eq!(
        (p.kind, p.command.as_str()),
        (PromptKind::Other, ""),
        "the control: no command under another title"
    );
    let refused = format!(
        "Done. Tool result: tool failed: {}",
        anchor_text("muse.box.socket_refused")
    );
    for (text, said) in [
        (BOX_UNIX_SOCKET_APPROVED, "Done. Tool result: {"),
        (BOX_UNIX_SOCKET_DENIED, refused.as_str()),
    ] {
        let r = muse(text);
        assert_eq!((r.prompt, r.phase), (None, Phase::Idle), "{said}");
        let tail = r.said_tail.expect("the reply");
        assert!(tail.starts_with(said), "{tail}");
    }
}

/// THE BACKGROUND-WORK PANEL UNDER THE COMPOSER IS NO PART OF IT (1.4.10: a
/// monitor, a backgrounded shell): between the bottom rule and the footer it
/// draws `main · ↓ to select` (`main` alone while a draft is typed) over a
/// branch per piece of work, and the composer over it reads as without it —
/// a turn's end at its empty caret (busy as evidence while its work runs,
/// idle once that work failed: [`the_panels_running_work_is_the_agents_own`]),
/// a draft the draft, a running turn busy, a box a box. Before it was read,
/// each of these read as no composer at all, and the network box over it as
/// idle. NEGATIVE CONTROLS: a head with no branch under it, a branch with no
/// head, and a head of another word leave no composer to type into (not
/// idle as evidence).
#[test]
fn the_work_panel_under_the_composer_is_no_part_of_it() {
    for (text, phase) in [
        (END_OF_TURN_WORK_PANEL, Phase::Idle),
        (END_OF_TURN_WORK_RUNNING, Phase::Busy),
    ] {
        let r = muse(text);
        assert_eq!((r.phase.clone(), r.phase_authoritative), (phase, true));
        assert_eq!(r.composer.map(|(_, l)| l), Some(vec![String::new()]));
        assert_eq!(r.prompt, None);
    }
    let draft = muse(DRAFT_WORK_RUNNING);
    assert_eq!(
        draft.composer.map(|(_, l)| l),
        Some(vec!["carry on with the task".to_string()])
    );
    let busy = muse(BUSY_WORK_RUNNING);
    assert_eq!((busy.phase, busy.phase_authoritative), (Phase::Busy, true));
    assert_eq!(muse(BOX_NETWORK_MONITOR).phase, Phase::Prompt);
    let rows = screen(END_OF_TURN_WORK_RUNNING);
    let headless: Vec<String> = rows
        .iter()
        .filter(|r| !r.starts_with("main"))
        .cloned()
        .collect();
    let branchless: Vec<String> = rows
        .iter()
        .filter(|r| !r.starts_with("└ "))
        .cloned()
        .collect();
    let renamed = edited(&rows, "main", "side · ↓ to select");
    for broken in [headless, branchless, renamed] {
        assert_eq!(composer(&broken), None);
        assert_eq!(phase(&broken), (Phase::Idle, false), "not as evidence");
    }
}

/// The branch a backgrounded shell's panel showed once the shell ended
/// (measured on 1.4.10, 2026-10-17: the row the lab's log kept, standing
/// 8-11 s before the panel went), 144 columns wide.
fn finished_branch() -> String {
    format!(
        "└ ◆ Q3 background wait  ran 1s, exit 0{}1m 40s",
        " ".repeat(100)
    )
}

/// THE PANEL'S RUNNING WORK IS THE AGENT'S OWN (1.4.10, measured 2026-10-16
/// and 2026-10-17): a turn's end over a branch whose state is `running` (a
/// backgrounded shell) or `watching` (a monitor) is BUSY as evidence, its
/// background work named ([`background_wait`]), as Claude Code's reader
/// reads a shell or a monitor left running — Muse Code wakes itself when
/// the work ends (`◆ Finished · … · ✓`, then a turn of its own), so nothing
/// is typed into it meanwhile. A question the turn ended on is held by a
/// shell, as a shell Claude Code left running holds its question (the hard
/// rule), and stands over a monitor (the soft rule); a shell and a monitor
/// together name the shell, and hold the question. The composer and the
/// reply's last words read as before, a draft the draft; a running turn
/// over the panel is the turn, not background work. NEGATIVE CONTROLS: the
/// panel gone after the work ended ([`END_OF_TURN_BG_FINISHED`]), a branch
/// that finished (`ran 1s, exit 0`, measured) or failed, a state no build
/// drew, a branch whose time is no time, and a screen with no panel read no
/// background work, and the turn's end idle as evidence.
#[test]
fn the_panels_running_work_is_the_agents_own() {
    for text in [
        END_OF_TURN_WORK_RUNNING,
        END_OF_TURN_WORK_HINT,
        END_OF_TURN_BG_RUNNING,
    ] {
        let rows = screen(text);
        let r = muse(text);
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Busy, true));
        assert_eq!(r.composer.map(|(_, l)| l), Some(vec![String::new()]));
        assert!(
            r.said_tail
                .is_some_and(|t| t.starts_with("Tool call done."))
        );
        assert_eq!(background_wait(&rows), Some("background work running"));
        assert_eq!(
            MuseReader.background_wait(&rows),
            Some("background work running")
        );
        // A question the turn ended on (the reply's two rows made one that
        // asks) is held by the shell, the work still named.
        let head = rows
            .iter()
            .rposition(|r| r.starts_with("◆ Tool call done."))
            .expect("the reply");
        let mut asks = rows.clone();
        asks[head] = "◆ Should I wait for it to finish?".to_string();
        asks[head + 1] = String::new();
        assert_eq!(phase(&asks), (Phase::Busy, true));
        assert_eq!(background_wait(&asks), Some("background work running"));
        assert_eq!(
            said_tail(&asks).as_deref(),
            Some("Should I wait for it to finish?")
        );
        // A monitor watching in the shell's place: the question stands, the
        // work named; the plain end is busy.
        let at = rows
            .iter()
            .rposition(|r| r.starts_with("└ "))
            .expect("a branch");
        let watching = |rows: &[String]| {
            let row = rows[at]
                .replace(
                    "Wait in the background  running ",
                    "Watch the lab socket  watching",
                )
                .replace(
                    "Q3 background wait  running ",
                    "Watch the lab socket  watching",
                );
            assert_ne!(row, rows[at], "the branch rewritten");
            edited(rows, "└ ", &row)
        };
        assert_eq!(phase(&watching(&asks)), (Phase::Question, true));
        assert_eq!(
            background_wait(&watching(&asks)),
            Some("a monitor watching")
        );
        assert_eq!(phase(&watching(&rows)), (Phase::Busy, true));
        // A monitor's branch over the shell's: the shell named, the question
        // held.
        let mut both = asks.clone();
        both.insert(at, "├ ◇ Watch the lab socket  watching  14s".to_string());
        assert_eq!(phase(&both), (Phase::Busy, true));
        assert_eq!(background_wait(&both), Some("background work running"));
        // The controls: the branch over, or of a shape no build drew.
        let failed = rows[at].replace("running ", "failed  ");
        let named = rows[at].replace("running", "pending");
        let timeless = crate::text::trim_end(&rows[at])
            .rsplit_once("  ")
            .map(|(head, _)| format!("{head}  soon"))
            .expect("a time");
        for branch in [finished_branch(), failed, named, timeless] {
            let over = edited(&rows, "└ ", &branch);
            assert_eq!(phase(&over), (Phase::Idle, true), "{branch}");
            assert_eq!(background_wait(&over), None, "{branch}");
            let over = edited(&asks, "└ ", &branch);
            assert_eq!(phase(&over), (Phase::Question, true), "{branch}");
        }
    }
    let draft = screen(DRAFT_WORK_RUNNING);
    assert_eq!(phase(&draft), (Phase::Busy, true));
    assert_eq!(background_wait(&draft), Some("background work running"));
    assert_eq!(
        composer_draft(&draft).map(|(_, l)| l),
        Some(vec!["carry on with the task".to_string()])
    );
    let busy = screen(BUSY_WORK_RUNNING);
    assert_eq!(phase(&busy), (Phase::Busy, true));
    assert_eq!(background_wait(&busy), None, "the running turn's own");
    // A monitor watching, its box answered: its turn's end over the panel.
    let monitor = screen(END_OF_TURN_WORK_RUNNING);
    let at = monitor
        .iter()
        .rposition(|r| r.starts_with("└ "))
        .expect("a branch");
    let watching = edited(
        &monitor,
        "└ ",
        &monitor[at].replace(
            "Wait in the background  running ",
            "Watch the lab socket  watching",
        ),
    );
    assert_eq!(phase(&watching), (Phase::Busy, true));
    assert_eq!(background_wait(&watching), Some("a monitor watching"));
    // The work over: no panel, the turn Muse Code took on its end idle.
    let r = muse(END_OF_TURN_BG_FINISHED);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert_eq!(background_wait(&screen(END_OF_TURN_BG_FINISHED)), None);
    for text in [
        END_OF_TURN_1_4_10,
        END_OF_TURN_WORK_PANEL,
        BOX_NETWORK_MONITOR,
    ] {
        assert_eq!(background_wait(&screen(text)), None);
    }
    assert_eq!(muse(END_OF_TURN_1_4_10).phase, Phase::Idle);
}

/// A LIMIT OR A WALL THE TURN ENDED ON STANDS OVER THE AGENT'S OWN
/// BACKGROUND WORK (1.4.10's screens): the panel and a running shell's
/// branch spliced under the composer of a turn that ended on the final 500
/// and on the usage wall read as those walls still — idle on the error (so
/// the retry ladder runs), limited until the reset — while
/// [`background_wait`] names the work, as Claude Code's reader reads the
/// wall a turn ended on behind a shell it left running
/// (`phase::background_wall`). Read busy, the ladder would never run.
/// NEGATIVE CONTROL: the same splice under a turn that ended on no wall
/// reads busy.
#[test]
fn a_wall_the_turn_ended_on_stands_over_the_background_work() {
    let splice = |text: &str| -> Vec<String> {
        let mut rows = screen(text);
        let footer = rows.iter().rposition(|r| !blank(r)).expect("the footer");
        rows.insert(
            footer,
            format!("└ ◈ Q3 background wait  running{}2s", " ".repeat(100)),
        );
        rows.insert(footer, "main · ↓ to select".to_string());
        rows
    };
    let api = splice(SERVER_ERROR_FINAL);
    let r = read(Some("muse"), &api, None);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert!(
        matches!(r.wall.map(|w| w.kind), Some(WallKind::ApiError { .. })),
        "the 500"
    );
    assert!(r.composer.is_some());
    assert_eq!(background_wait(&api), Some("background work running"));
    let usage = splice(USAGE_WALL);
    let r = read(Some("muse"), &usage, None);
    assert!(
        matches!(r.phase, Phase::Limited { .. }) && r.phase_authoritative,
        "{:?}",
        r.phase
    );
    assert_eq!(r.wall.map(|w| w.kind), Some(WallKind::UsageSession));
    assert_eq!(background_wait(&usage), Some("background work running"));
    let ended = splice(END_OF_TURN_1_4_10);
    assert_eq!(phase(&ended), (Phase::Busy, true), "the control");
    assert_eq!(
        background_wait(&ended),
        Some("background work running"),
        "the control"
    );
}

/// THE PANEL'S BRANCHES IN EITHER ORDER, AND A TURN'S END THAT IS NO
/// EVIDENCE (the review of the hard rule's guards). A shell's branch drawn
/// ABOVE a monitor's (`├ ` then `└ `) holds the question the turn ended on
/// as a lone shell does, the shell named: read without its `├ ` row it
/// would be a lone monitor, the soft rule, and the question answered over a
/// running shell. A turn that stopped on a fault only a person can fix
/// (`A configuration problem stopped this turn.`) over a running shell is
/// `idle` NOT as evidence still, no background work named: only an ended
/// turn the reader vouches for is held busy by the work. NEGATIVE CONTROLS:
/// the monitor alone under the question is the question; the fault with no
/// panel reads the same as over it.
#[test]
fn a_shell_over_a_monitor_holds_the_question_and_a_fault_is_no_evidence_over_work() {
    let rows = screen(END_OF_TURN_BG_RUNNING);
    let head = rows
        .iter()
        .rposition(|r| r.starts_with("◆ Tool call done."))
        .expect("the reply");
    let ended_on = |words: &str| {
        let mut out = rows.clone();
        out[head] = words.to_string();
        out[head + 1] = String::new();
        out
    };
    let asks = ended_on("◆ Should I deploy it now?");
    let at = asks
        .iter()
        .rposition(|r| r.starts_with("└ "))
        .expect("the shell's branch");
    let shell_first = {
        let mut out = asks.clone();
        out[at] = out[at].replacen("└ ", "├ ", 1);
        out.insert(
            at + 1,
            "└ ◇ Watch the lab socket  watching  14s".to_string(),
        );
        out
    };
    assert_eq!(phase(&shell_first), (Phase::Busy, true));
    assert_eq!(
        background_wait(&shell_first),
        Some("background work running")
    );
    assert_eq!(
        MuseReader.background_wait(&shell_first),
        Some("background work running")
    );
    let monitor_alone = {
        let mut out = asks.clone();
        out[at] = "└ ◇ Watch the lab socket  watching  14s".to_string();
        out
    };
    assert_eq!(
        phase(&monitor_alone),
        (Phase::Question, true),
        "the control"
    );
    assert_eq!(background_wait(&monitor_alone), Some("a monitor watching"));
    // A fault over the running shell: no evidence, no work named.
    let fault = ended_on("◆ A configuration problem stopped this turn. Fix it in /settings.");
    assert_eq!(phase(&fault), (Phase::Idle, false));
    assert_eq!(background_wait(&fault), None);
    let r = read(Some("muse"), &fault, None);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, false));
    let plain = edited(
        &screen(END_OF_TURN),
        "◆ echo",
        "◆ A configuration problem stopped this turn. Fix it in /settings.",
    );
    assert_eq!(phase(&plain), (Phase::Idle, false), "the control");
}

/// THE PANEL'S POINTER UNDER A REPLY IS NO PART OF IT (1.4.10, measured: a
/// fresh session's first backgrounded shell): `  (↓ to view terminals)`,
/// drawn as a block of its own under the reply a moment after it, is
/// skipped as a card is — the turn ends on the reply as evidence, its words
/// the last words — so a report ending `ATERM-TASK-DONE` still ends on the
/// token and a reply asking still asks with the pointer under it. While the
/// shell runs that end is the agent's own background work (busy as
/// evidence, a question's too, as a shell Claude Code left running holds
/// its question); once its branch is over it is idle, or the question. Before, the pointer was read as the reply's last row: the token and
/// the question were hidden. NEGATIVE CONTROLS: the same row with no blank
/// row above it is the reply's own last row, and a pointer naming no kind
/// is no pointer.
#[test]
fn the_panels_pointer_under_a_reply_is_no_part_of_it() {
    let r = muse(END_OF_TURN_WORK_HINT);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Busy, true));
    assert_eq!(r.composer.map(|(_, l)| l), Some(vec![String::new()]));
    let said = r.said_tail.expect("the reply");
    assert!(said.starts_with("Tool call done. Result: {"), "{said}");
    assert!(said.ends_with("\", ~"), "{said}");
    let rows = screen(END_OF_TURN_WORK_HINT);
    let at = |head: &str| {
        rows.iter()
            .position(|r| r.starts_with(head))
            .unwrap_or_else(|| panic!("a row {head:?}"))
    };
    let (reply, hint) = (at("◆ Tool call done."), at("  (↓ to view terminals)"));
    let with_reply = |words: &[&str]| -> Vec<String> {
        let mut out: Vec<String> = rows[..reply].to_vec();
        out.extend(words.iter().map(ToString::to_string));
        out.extend(rows[hint - 1..].iter().cloned());
        out
    };
    let done = with_reply(&["◆ Done: the probe is written.", "  ATERM-TASK-DONE"]);
    assert_eq!(phase(&done), (Phase::Busy, true), "its shell runs");
    assert_eq!(
        said_tail(&done).as_deref(),
        Some("Done: the probe is written.\nATERM-TASK-DONE")
    );
    let over = edited(&done, "└ ", &finished_branch());
    assert_eq!(phase(&over), (Phase::Idle, true), "its shell over");
    assert_eq!(said_tail(&over), said_tail(&done));
    let asks = with_reply(&["◆ Should I wait for it to finish?"]);
    assert_eq!(phase(&asks), (Phase::Busy, true), "its shell runs");
    assert_eq!(
        said_tail(&asks).as_deref(),
        Some("Should I wait for it to finish?")
    );
    let over = edited(&asks, "└ ", &finished_branch());
    assert_eq!(phase(&over), (Phase::Question, true), "its shell over");
    // The controls.
    let mut joined = done.clone();
    let blank = joined
        .iter()
        .position(|r| r.starts_with("  (↓ to view"))
        .expect("the pointer")
        - 1;
    joined.remove(blank);
    assert_eq!(
        said_tail(&joined).as_deref(),
        Some("Done: the probe is written.\nATERM-TASK-DONE\n(↓ to view terminals)")
    );
    let kindless = edited(&done, "  (↓ to view terminals)", "  (↓ to view )");
    assert_eq!(
        said_tail(&kindless).map(|s| s.lines().last().map(str::to_string)),
        Some(Some("(↓ to view )".to_string()))
    );
}

/// DIGITS AND `y` ANSWER AN APPROVAL BOX AT ONCE (measured on 1.4.10), and
/// only MOVE A QUESTION'S CURSOR: `2` refused a shell box and the next box
/// came up, read as itself; `y` ran the next; `1` ran the third and the turn
/// ended; `4` refused a network box and `1` ran another — each answered box
/// gone and the turn's end read. On a question `2` and `3` moved the `⟩` and
/// answered nothing. The harness keeps to the cursor and Enter on both
/// (`Select::ArrowsEnter`: the one-shot allow is the option focused as drawn,
/// and its Enter is guarded on that row). A key typed before a box is drawn
/// lands in the composer: the network box's run screen holds the draft `1`.
/// NEGATIVE CONTROL: the box screens before any key are the boxes.
#[test]
fn digits_and_y_answer_a_box_at_once_and_move_a_questions_cursor() {
    for (text, command, refused_above) in [
        (
            BOX_SHELL_DIGIT_2,
            "touch probe-b.txt",
            "◆ Ran touch probe-a.txt — denied",
        ),
        (
            BOX_SHELL_KEY_Y,
            "touch probe-c.txt",
            "◆ Ran command · Create the second probe · ✓",
        ),
    ] {
        let r = muse(text);
        let p = r.prompt.expect("the next box");
        assert_eq!((p.kind, p.command.as_str()), (PromptKind::Bash, command));
        assert_eq!(p.select, Select::ArrowsEnter);
        assert!(
            screen(text)
                .iter()
                .any(|row| row.starts_with(refused_above)),
            "{command}"
        );
    }
    for text in [BOX_SHELL_DIGIT_APPROVED, BOX_NETWORK_DIGIT_DENIED] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true)
        );
        assert_eq!(r.prompt, None);
    }
    assert!(
        screen(BOX_NETWORK_DIGIT_DENIED)
            .iter()
            .any(|row| row.starts_with("◆ Fetched http://127.0.0.1:50581/lab-probe — denied"))
    );
    let typed = muse(BOX_NETWORK_DIGIT_APPROVED);
    assert_eq!(typed.prompt, None);
    assert_eq!(typed.composer.map(|(_, l)| l), Some(vec!["1".to_string()]));
    let moved = prompt(&screen(QUESTION_MULTI_DIGIT)).expect("still the question");
    let focused: Vec<&str> = moved
        .options
        .iter()
        .filter(|o| o.focused)
        .map(|o| o.label.as_str())
        .collect();
    assert_eq!(focused, vec!["Large"]);
    let note = prompt(&screen(QUESTION_NOTE_OPEN)).expect("still the question");
    assert!(
        note.options[2].focused,
        "`3` moved it onto None of the above"
    );
    for text in [BOX_SHELL, BOX_NETWORK] {
        assert_eq!(muse(text).phase, Phase::Prompt, "the control");
    }
}

/// A CALL OF SEVERAL QUESTIONS AND A MULTI-SELECT ARE QUESTIONS OF NO ROLES
/// (1.4.10): several questions draw one at a time, `1 of 2` over the key row
/// that names `←/→ to switch question` (Left goes back to an answered one
/// with no mark of its answer); a multi-select draws `[ ]` boxes, a `Submit
/// answer (N checked)` row and the key row `Enter to toggle · Submit row to
/// continue · …`; each goes to a review. Both read whole as questions —
/// their head's headers the title, `1 of 2` the note, the key row the footer
/// — in a 60-column tab too, and are handed to a person: no option carries a
/// role. Before they were read, several questions read as no box at all.
/// NEGATIVE CONTROL: the single question's key row on the same screen, its
/// `1 of 2` row gone, gives its answers their roles.
#[test]
fn several_questions_and_a_multi_select_are_questions_of_no_roles() {
    for (text, position) in [
        (QUESTION_MULTI, Some("1 of 2")),
        (QUESTION_MULTI_SECOND, Some("2 of 2")),
        (QUESTION_MULTI_DIGIT, Some("2 of 2")),
        (QUESTION_MULTI_NARROW, Some("1 of 2")),
        (QUESTION_MULTI_NOTE_TYPED, Some("1 of 2")),
        (QUESTION_MULTISELECT, None),
        (QUESTION_MULTISELECT_TOGGLED, None),
        (QUESTION_MULTISELECT_NARROW, None),
    ] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative, r.composer.clone()),
            (Phase::Prompt, true, None),
            "{position:?}"
        );
        let p = r.prompt.expect("the question");
        assert_eq!(p.kind, PromptKind::Question);
        assert!(roles(&p).iter().all(|&r| r == Role::Other), "{:?}", p.title);
        assert_eq!(
            p.notes,
            position.map(str::to_string).into_iter().collect::<Vec<_>>()
        );
        let keys = if position.is_some() {
            anchor_text("muse.question.keys_multi")
        } else {
            anchor_text("muse.question.keys_toggle")
        };
        assert_eq!(p.footer.as_deref(), Some(keys));
        assert!(!is_review(&p));
    }
    let p = prompt(&screen(QUESTION_MULTI)).expect("the question");
    assert_eq!(p.title, "Request user input Color, Size");
    assert_eq!(p.description, "Which color should the probe use?");
    let ms = prompt(&screen(QUESTION_MULTISELECT)).expect("the question");
    assert_eq!(
        ms.options.last().map(|o| o.label.as_str()),
        Some("Submit answer (0 checked)")
    );
    let rows: Vec<String> = screen(QUESTION_MULTI)
        .into_iter()
        .filter(|r| r.trim() != "1 of 2")
        .collect();
    let single = edited(
        &rows,
        "  Enter to select",
        &format!("  {}", anchor_text("muse.question.keys")),
    );
    assert_eq!(
        roles(&prompt(&single).expect("the question")),
        vec![Role::Answer, Role::Answer, Role::Other],
        "the control"
    );
}

/// THE REVIEW (1.4.10) IS A QUESTION OF NO ROLES WHOSE ESC GOES BACK: the
/// answers of several questions, of a multi-select, or of one question
/// answered with a note are shown before they are sent — `Review answers
/// before submit · Enter to edit or submit · ↑/↓ to move · Esc to go back`
/// (two rows in a 60-column tab), the answers four columns in (a long one
/// wrapped between words, its rows four in too) and `⟩ Submit answers` /
/// `Interrupt turn`, unnumbered. It reads as [`is_review`], its answers
/// joined with one blank its description, the cursor where drawn, no role
/// on either option. Before it was read, the review read as no box at all.
/// NEGATIVE CONTROLS: with its key row gone it is no box and not idle as
/// evidence; once submitted it is a turn's end.
#[test]
fn the_review_is_a_question_of_no_roles_whose_esc_goes_back() {
    for (text, answers, at) in [
        (
            QUESTION_MULTI_REVIEW,
            "Color: None of the above; note: the policy text goes here Size: Large".to_string(),
            0,
        ),
        (
            QUESTION_MULTI_REVIEW_NARROW,
            "Color: Red (Recommended) Size: Small (Recommended)".to_string(),
            0,
        ),
        (
            QUESTION_MULTI_REVIEW_INTERRUPT,
            "Color: Red (Recommended) Size: Small (Recommended)".to_string(),
            1,
        ),
        (
            QUESTION_MULTISELECT_REVIEW,
            "Checks: Lint (Recommended)".to_string(),
            0,
        ),
        (
            QUESTION_NOTE_REVIEW,
            "Color: None of the above; note: Decide yourself: choose the option that best \
             serves long term correctness."
                .to_string(),
            0,
        ),
        (
            QUESTION_NOTE_LONG_REVIEW,
            format!("Color: None of the above; note: {LONG_TERM}"),
            0,
        ),
    ] {
        let r = muse(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative, r.composer.clone()),
            (Phase::Prompt, true, None),
            "{answers}"
        );
        let p = r.prompt.expect("the review");
        assert!(is_review(&p), "{answers}");
        assert_eq!(p.description, answers);
        let labels: Vec<(Option<u8>, &str, Role)> = p
            .options
            .iter()
            .map(|o| (o.n, o.label.as_str(), o.role))
            .collect();
        assert_eq!(
            labels,
            vec![
                (None, anchor_text("muse.question.submit"), Role::Other),
                (None, anchor_text("muse.question.interrupt"), Role::Other),
            ]
        );
        assert_eq!(
            p.options.iter().position(|o| o.focused),
            Some(at),
            "{answers}"
        );
        assert_eq!(
            p.cancel.map(|c| c.effect),
            Some(CancelEffect::Back),
            "{answers}"
        );
    }
    let rows = screen(QUESTION_MULTI_REVIEW);
    let keyless: Vec<String> = rows
        .iter()
        .filter(|r| !r.trim_start().starts_with("Review answers"))
        .cloned()
        .collect();
    assert_eq!(prompt(&keyless), None);
    assert_eq!(phase(&keyless), (Phase::Idle, false));
    for text in [
        QUESTION_MULTI_ANSWERED,
        QUESTION_MULTISELECT_ANSWERED,
        QUESTION_NOTE_LONG_ANSWERED,
    ] {
        let r = muse(text);
        assert_eq!((r.prompt, r.phase), (None, Phase::Idle), "the control");
    }
}

/// THE QUESTION'S NOTE (1.4.10) IS READ WHOLE ([`question_notes`]): Tab
/// opens `Note (optional): ▌` under the focused option, six columns in; the
/// text typed into it, the cursor stripped — the harness's 276-byte
/// long-term text wrapped between words onto rows six in, joined with one
/// blank — and the field's row to guard a key on. It is no part of any
/// label: the question keeps its options and roles. Esc in the field, or Tab,
/// CLOSES it and CLEARS the text (Tab reopened it empty). NEGATIVE CONTROLS:
/// the question with no note drawn, after Esc, has none.
#[test]
fn the_note_is_read_whole_and_esc_or_tab_clears_it() {
    for (text, note) in [
        (QUESTION_NOTE_OPEN, ""),
        (
            QUESTION_NOTE_TYPED,
            "Decide yourself: choose the option that best serves long term correctness.",
        ),
        (QUESTION_NOTE_LONG, LONG_TERM),
        (QUESTION_NOTE_SHORT, "short words"),
        (QUESTION_NOTE_TAB_CLEARED, ""),
        (QUESTION_MULTI_NOTE_TYPED, "the policy text goes here"),
    ] {
        let rows = screen(text);
        let n = question_notes(&rows).expect("the note");
        assert_eq!(n.text, note);
        assert!(
            rows[n.row].trim_start().starts_with("Note (optional):"),
            "{note}"
        );
        let p = prompt(&rows).expect("the question");
        assert_eq!(p.options.len(), 3, "{note}");
        assert!(p.options.iter().all(|o| !o.label.contains("Note")));
    }
    let long = prompt(&screen(QUESTION_NOTE_LONG)).expect("the question");
    assert_eq!(roles(&long), vec![Role::Answer, Role::Answer, Role::Other]);
    for text in [QUESTION_NOTE_ESC, QUESTION, QUESTION_MULTI] {
        assert_eq!(question_notes(&screen(text)), None, "the control");
    }
}

/// A META ACCOUNT'S WALLS, from the binary's strings alone (the lab never
/// logs in: `META_API_KEY` beats any login, and `muse login` is refused
/// there): a payment method missing on the account or for Muse Code, and an
/// account not yet set up for the Model API, each ending `then log in
/// again`, are sign-in walls, each with Muse Code's own remedy.
/// NEGATIVE CONTROL: the same words inside a reply are the agent's.
#[test]
fn a_meta_accounts_walls_are_sign_in_walls() {
    let rows = screen(KEY_REJECTED);
    for (id, fix) in [
        (
            "muse.wall.payment_api",
            "add one at https://dev.meta.ai, then log in again",
        ),
        (
            "muse.wall.payment_muse",
            "subscribe at https://accountscenter.meta.com/muse_code/?ep=no_payg, then log in again",
        ),
        (
            "muse.wall.onboarding",
            "finish onboarding at https://dev.meta.ai, then log in again",
        ),
    ] {
        let walled = edited(&rows, "◆ your API key", &format!("◆ {}", anchor_text(id)));
        let w = wall(&walled).expect("the wall");
        assert_eq!(w.kind, WallKind::Auth, "{id}");
        assert_eq!(remedy(&w.message).as_deref(), Some(fix), "{id}");
        let quoted = edited(
            &rows,
            "◆ your API key",
            &format!("◆ The page said: {}", anchor_text(id)),
        );
        assert_eq!(wall(&quoted), None, "the control: {id}");
    }
}

/// A SIGKILLed session RESUMED (the harness's relaunch on exit, 2026-10-17):
/// its replayed turns, then Muse Code's own abnormal-end warning above the
/// composer. The screen reads idle, authoritatively, its composer empty, so
/// the relaunch's continuation is typed into it.
#[test]
fn a_resumed_session_reads_idle_under_its_abnormal_end_warning() {
    let rows = screen(RESUMED_AFTER_CRASH);
    assert_eq!(phase(&rows), (Phase::Idle, true));
    let (_, draft) = composer_draft(&rows).expect("a composer");
    assert!(draft.iter().all(|l| l.trim().is_empty()), "{draft:?}");
    assert!(prompt(&rows).is_none(), "no box");
    assert_eq!(
        said_tail(&rows).as_deref(),
        Some("Hi! I am the local lab stub, not a model. (reply 6)"),
        "the warning is no word of the agent's"
    );
    // NEGATIVE CONTROLS: the same block in other words is read as the
    // reply's indented rows, as every such block was, and so is a reply's
    // own paragraph saying so (under its head, no blank row above).
    let warned = rows
        .iter()
        .position(|r| r.trim_start().starts_with("warning: the previous run"))
        .expect("the warning");
    let mut other = rows.clone();
    other[warned] = "  warning: something unmeasured happened".to_string();
    let tail = said_tail(&other).expect("a reply");
    assert!(
        tail.ends_with("something unmeasured happened\ncleanly."),
        "{tail}"
    );
    let mut inside = rows.clone();
    inside.remove(warned - 1);
    let tail = said_tail(&inside).expect("a reply");
    assert!(tail.ends_with("resumed\ncleanly."), "{tail}");
}

/// A REPLY TALLER THAN THE SCREEN IS NO WHOLE REPLY ([`said_whole`]): its
/// `◆` head scrolled above the rows read ([`Turn::Tail`]), its last words
/// are the rows in sight, and the need its head said is not in them — so a
/// reader of a landing knows to read the rest (the recheck of d88a7bf289
/// found only Claude Code's cut read, and a tall report here land on its
/// last rows). NEGATIVE CONTROL: the same reply with its head on the rows is
/// whole ([`Turn::Ended`]), the need in its words.
#[test]
fn a_reply_whose_head_scrolled_away_is_no_whole_reply() {
    let rows = screen(END_OF_TURN_1_4_10);
    let at = rows
        .iter()
        .position(|r| r.starts_with("◆ Hi!"))
        .expect("the reply");
    let mut drawn = rows[..at].to_vec();
    drawn.push("◆ NEEDS YOU: the signing credential".to_string());
    drawn.extend((0..40).map(|i| format!("  - merged item {i}")));
    drawn.push("  Nothing left: all of it is merged.".to_string());
    drawn.extend(rows[at + 1..].iter().cloned());
    let cut = drawn[drawn.len() - 2 * rows.len()..].to_vec();
    let tail = said_tail(&cut).expect("the rows in sight");
    assert!(
        tail.ends_with("Nothing left: all of it is merged.") && !tail.contains("NEEDS YOU"),
        "{tail}"
    );
    assert!(!said_whole(&cut) && !MuseReader.said_whole(&cut));
    // NEGATIVE CONTROL: the head on the rows.
    assert!(said_whole(&drawn) && MuseReader.said_whole(&drawn));
    assert!(said_tail(&drawn).is_some_and(|t| t.starts_with("NEEDS YOU: the signing")));
}

/// A NEED SAID BEFORE A TOOL CALL IS THE TURN'S ([`turn_said_before`]): the
/// `◆` reply above a tool's row, under the person's `❯` message, is read
/// with the turn its last reply ends — a `NEEDS YOU:` there stands however
/// the last reply says nothing is left (the recheck of ea564676a3 found the
/// turn's earlier words read on Claude Code alone). The tool's own row is
/// no words of the agent. The turn is whole with its message on the rows
/// ([`turn_whole`]); with it above them it is not, and the history read
/// above stops at the message ([`turn_history_stop`]). NEGATIVE CONTROL: the
/// same turn with no reply before the call holds no other words.
#[test]
fn a_need_said_before_a_tool_call_is_read_with_the_turn() {
    let rows = screen(END_OF_TURN_1_4_10);
    let at = rows
        .iter()
        .position(|r| r.starts_with("◆ Hi!"))
        .expect("the reply");
    let turn = |before: &[&str]| {
        let mut drawn = rows[..at].to_vec();
        drawn.extend(before.iter().map(|r| (*r).to_string()));
        drawn.extend(
            [
                "◆ Ran command · Push the branch · ✓ · 2.3s · ctrl+o",
                "",
                "◆ Nothing left: all of it is merged.",
            ]
            .map(str::to_string),
        );
        drawn.extend(rows[at + 1..].iter().cloned());
        drawn
    };
    let drawn = turn(&[
        "◆ NEEDS YOU: the signing credential, to sign the release.",
        "  I will push the branch now.",
        "",
    ]);
    let need =
        "NEEDS YOU: the signing credential, to sign the release.\nI will push the branch now.";
    assert_eq!(turn_said_before(&drawn).as_deref(), Some(need));
    assert_eq!(MuseReader.turn_said_before(&drawn).as_deref(), Some(need));
    assert_eq!(
        said_tail(&drawn).as_deref(),
        Some("Nothing left: all of it is merged.")
    );
    assert!(turn_whole(&drawn) && MuseReader.turn_whole(&drawn));
    // The message above the rows: the turn is cut, and the read above stops
    // at it.
    let user = drawn
        .iter()
        .position(|r| r.starts_with("❯ carry on"))
        .expect("the message");
    let cut = drawn[user + 1..].to_vec();
    assert!(said_whole(&cut) && !turn_whole(&cut) && !MuseReader.turn_whole(&cut));
    assert!(turn_history_stop(&drawn[user]) && MuseReader.turn_history_stop(&drawn[user]));
    for row in &drawn[user + 1..at + 3] {
        assert!(!turn_history_stop(row), "{row}");
    }
    // NEGATIVE CONTROL: no reply before the call.
    let plain = turn(&[]);
    assert_eq!(turn_said_before(&plain), None);
    assert!(turn_whole(&plain) && said_tail(&plain).is_some());
}

/// THE MESSAGE THAT OPENED THE TURN IS READ OFF ITS `❯` ROW
/// ([`turn_opened_by`]): the row's text past its `❯`, and the rows it
/// wrapped onto (indented, up to a blank row), joined — what tells a
/// successor whether its land act opened the turn whose defense stands (the
/// review of b8/residuals found a defense said under a person's earlier
/// message landed on Muse Code, nothing reading the message). NEGATIVE
/// CONTROLS: the message above the rows, and the composer's own `❯`, read
/// none.
#[test]
fn the_message_that_opened_the_turn_is_read_off_its_row() {
    let rows = screen(END_OF_TURN_1_4_10);
    assert_eq!(
        turn_opened_by(&rows).as_deref(),
        Some("carry on with the task")
    );
    assert_eq!(
        MuseReader.turn_opened_by(&rows).as_deref(),
        Some("carry on with the task")
    );
    let user = rows
        .iter()
        .position(|r| r.starts_with("❯ carry on"))
        .expect("the message");
    let mut wrapped = rows.clone();
    wrapped.insert(user + 1, "  and push it".to_string());
    assert!(said_tail(&wrapped).is_some());
    assert_eq!(
        turn_opened_by(&wrapped).as_deref(),
        Some("carry on with the task and push it")
    );
    // NEGATIVE CONTROLS.
    let cut = rows[user + 1..].to_vec();
    assert!(said_tail(&cut).is_some(), "{cut:#?}");
    assert_eq!(turn_opened_by(&cut), None);
}
