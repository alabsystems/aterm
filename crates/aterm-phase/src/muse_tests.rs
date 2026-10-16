// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One test per measured Muse Code screen (muse 1.4.9-R6297.1), each with
//! its negative control.

use super::fixtures::*;
use super::*;
use crate::prompt::fixtures::{cursor, rows, screen};
use crate::reader::{ClaudeReader, CodexReader, Program, ScreenReader, identify, read};
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

/// Claude Code's and Codex's readers on Muse Code's screens: never a box,
/// never a question. Claude Code's FRAME test does take Muse Code's composer
/// for its own (its caret is `❯`, its top rule a rule) — the measured reason
/// [`crate::reader::identify`] asks Muse Code's layout first.
#[test]
fn claudes_and_codexs_readers_see_no_box_and_no_question_on_muse_screens() {
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
    for text in [IDLE, BUSY, DRAFT, END_OF_TURN] {
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

/// THE SNIFF CLAIMS NO OTHER PROGRAM'S SCREEN: over every capture of Claude
/// Code, Codex and agy under `src/fixtures/`, Muse Code's layout test says
/// no, and no screen is identified as its — Claude Code's own composer,
/// which its frame test shares, included. The control: it says yes on Muse
/// Code's own (but the one it left).
#[test]
fn the_muse_sniff_claims_no_other_programs_screen() {
    let mut swept = 0;
    for (name, text) in crate::text::fixture_files() {
        let r = screen(&text);
        if name.starts_with("muse-") {
            assert_eq!(is_muse_screen(&r), text != EXITED, "{name}");
            continue;
        }
        swept += 1;
        assert!(!is_muse_screen(&r), "{name}");
        assert_ne!(identify(None, &r).program(), Program::Muse, "{name}");
        assert_eq!(prompt(&r), None, "{name}");
    }
    assert!(swept > 150, "only {swept} other captures");
}
