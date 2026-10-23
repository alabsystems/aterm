// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Screen classification and prompt parsing tests for Muse Code captures.

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

fn edited(rows: &[String], head: &str, with: &str) -> Vec<String> {
    let at = rows
        .iter()
        .rposition(|r| r.starts_with(head))
        .unwrap_or_else(|| panic!("a row starting {head:?}"));
    let mut out = rows.to_vec();
    out[at] = with.to_string();
    out
}

fn captures() -> Vec<(String, String)> {
    crate::text::fixture_files()
        .into_iter()
        .filter(|(name, _)| name.starts_with("muse-"))
        .collect()
}

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
    let mut ruled = rows.clone();
    let message = ruled
        .iter()
        .position(|r| r.starts_with("❯ toolstart"))
        .expect("the message");
    ruled[message - 1] = "─".repeat(40);
    assert_eq!(composer_draft(&ruled), None);
    assert_eq!(phase(&ruled), (Phase::Prompt, true));
}

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
            "bash-ran",
            TOOL_ROW_BASH_RAN,
            "◆ Ran ls · ✓ · 0.2s · ctrl+o",
        ),
        (
            "bash-ran-described",
            TOOL_ROW_BASH_RAN_DESCRIBED,
            "◆ Ran command · Count the README's lines · ✓ · 0.2s · ctrl+o",
        ),
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
    for text in [TOOL_ROW_BASH_RAN, TOOL_ROW_BASH_RAN_DESCRIBED] {
        for said in [
            "Counted the README's lines: one.",
            "Ran the tests: all 580 pass.",
            "Ran ls and found one file. Nothing left: the parser is committed and pushed.",
            "Ran ls · ✓",
            "Ran the suite · it took · ctrl+o",
        ] {
            let rows = edited(&screen(text), "◆ Ran ", &format!("◆ {said}"));
            assert_eq!(phase(&rows), (Phase::Idle, true), "{said}");
            assert_eq!(said_tail(&rows).as_deref(), Some(said));
        }
    }
}

/// Measured Muse 1.4.12 rows lose their expander at narrow widths. A checkmark
/// and duration still identify the tool row; ordinary reply endings do not.
#[test]
fn a_shell_row_cut_to_a_narrow_pane_is_over_with_no_words() {
    for (cols, text, row) in [
        (
            144,
            TOOL_ROW_BASH_RAN_DESCRIBED,
            "◆ Ran command · Count the README's lines · ✓ · 0.2s · ctrl+o",
        ),
        (
            50,
            TOOL_ROW_BASH_RAN_DESCRIBED_50,
            "◆ Ran command · Count the REA… · ✓ · 0.1s · ctrl+o",
        ),
        (
            40,
            TOOL_ROW_BASH_RAN_DESCRIBED_40,
            "◆ Ran command · Count the RE… · ✓ · 0.1s",
        ),
        (
            30,
            TOOL_ROW_BASH_RAN_DESCRIBED_30,
            "◆ Ran · Count the … · ✓ · 0.2s",
        ),
    ] {
        let rows = screen(text);
        assert!(rows.iter().any(|r| r == row), "{cols}: the row measured");
        if cols < 144 {
            assert_eq!(row.chars().count(), cols, "cut to the pane's width");
        }
        assert_eq!(phase(&rows), (Phase::Idle, true), "{cols}");
        assert_eq!(said_tail(&rows), None, "{cols}");
        let r = muse(text);
        assert_eq!(
            (r.phase, r.phase_authoritative, r.said_tail, r.wall),
            (Phase::Idle, true, None, None),
            "{cols}"
        );
    }
    let narrow = screen(TOOL_ROW_BASH_RAN_DESCRIBED_40);
    for said in [
        "Ran the suite · it took · 3s",
        "Ran the tests · ✓",
        "Ran the build · ✓ · fast",
    ] {
        let rows = edited(&narrow, "◆ Ran ", &format!("◆ {said}"));
        assert_eq!(phase(&rows), (Phase::Idle, true), "{said}");
        assert_eq!(said_tail(&rows).as_deref(), Some(said));
    }
}

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
        let mut recording = rows.clone();
        recording[3] = format!("── Recording {}", "─".repeat(w - 13));
        assert_eq!(composer_rules(&recording), Some((3, 5)), "CONTROL {w}");
        assert!(!is_idle_voice_rule(&recording[3]), "CONTROL {w}");
    }
}

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

#[test]
fn a_relaunched_session_is_fresh_under_its_new_banner() {
    let rows = screen(IDLE_RELAUNCHED);
    assert_eq!((phase(&rows), fresh(&rows)), ((Phase::Idle, true), true));
    let starship = edited(&rows, "$ muse-fake", "❯ muse --approval-mode untrusted");
    assert!(fresh(&starship));
    let quoted = edited(&rows, "$ muse-fake", "❯ what does the banner say?");
    assert!(!fresh(&quoted), "the control");

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

const LONG_TERM: &str = "Decide yourself: choose the option that best serves long term \
                         correctness and total value even at the cost of short term \
                         deliverables. Say which you chose and why in one line and continue \
                         without asking again. (Answered by the aterm harness by policy. The \
                         user was not asked.)";

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

fn finished_branch() -> String {
    format!(
        "└ ◆ Q3 background wait  ran 1s, exit 0{}1m 40s",
        " ".repeat(100)
    )
}

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
        let mut both = asks.clone();
        both.insert(at, "├ ◇ Watch the lab socket  watching  14s".to_string());
        assert_eq!(phase(&both), (Phase::Busy, true));
        assert_eq!(background_wait(&both), Some("background work running"));
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
    assert!(said_whole(&drawn) && MuseReader.said_whole(&drawn));
    assert!(said_tail(&drawn).is_some_and(|t| t.starts_with("NEEDS YOU: the signing")));
}

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
    let plain = turn(&[]);
    assert_eq!(turn_said_before(&plain), None);
    assert!(turn_whole(&plain) && said_tail(&plain).is_some());
}

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
    let cut = rows[user + 1..].to_vec();
    assert!(said_tail(&cut).is_some(), "{cut:#?}");
    assert_eq!(turn_opened_by(&cut), None);
}

#[test]
fn muse_codes_unbranched_tool_rows_are_no_words_of_the_agents() {
    let rows = screen(END_OF_TURN_1_4_10);
    let reply = "◆ Hi! I am the local lab stub";
    let mut measured = Vec::new();
    for (name, text) in captures() {
        for row in screen(&text) {
            let Some(head) = row.strip_prefix("◆ ") else {
                continue;
            };
            if head.starts_with("Fetched ") || head.starts_with("Invalid work ID") {
                assert!(tool_row(head), "{name}: {head}");
                let ended = edited(&rows, reply, &row);
                assert_eq!(phase(&ended), (Phase::Idle, true), "{name}: {head}");
                assert_eq!(said_tail(&ended), None, "{name}: {head}");
                measured.push(head.to_string());
            }
        }
    }
    for head in [
        "Fetched http://127.0.0.1:50581/lab-probe — denied",
        "Fetched http://127.0.0.1:50581/lab-probe-approved — 404",
        "Invalid work ID — rejected · ctrl+o",
    ] {
        assert!(
            measured.iter().any(|m| m == head),
            "measured: {measured:#?}"
        );
    }
    for said in [
        "Fetched the page; the parser is done.",
        "Fetched http://127.0.0.1:50581/lab-probe — denied, so I stopped there.",
        "Fetched http://127.0.0.1:50581/lab-probe — 4044",
        "Invalid work ID was the bug, and it is fixed.",
        "Invalid work ID — rejected because the id was stale. Shall I retry?",
        "Wrote notes.md — 404",
        "Fetched http://127.0.0.1:50581/lab-probe — maybe",
        "Fetched both.",
        "Fetched origin — denied",
        "Fetched http://127.0.0.1:50581/lab-probe — ok.",
        "Invalid work ID — 404",
    ] {
        assert!(!tool_row(said), "{said}");
        let ended = edited(&rows, reply, &format!("◆ {said}"));
        assert_eq!(said_tail(&ended).as_deref(), Some(said), "{said}");
        assert!(
            matches!(phase(&ended), (Phase::Idle | Phase::Question, true)),
            "{said}"
        );
    }
    for first in ["Fetched both.", "Fetched https://example.com/spec."] {
        let mut ended = edited(&rows, reply, &format!("◆ {first}"));
        let at = ended
            .iter()
            .position(|r| r == &format!("◆ {first}"))
            .expect("the reply");
        for (k, row) in [
            "",
            "  NEEDS YOU: the deploy key for prod, which only you hold.",
            "",
            "  Nothing left: the parser is committed and pushed to origin/main.",
        ]
        .into_iter()
        .enumerate()
        {
            ended.insert(at + 1 + k, row.to_string());
        }
        assert_eq!(
            said_tail(&ended).as_deref(),
            Some(
                format!(
                    "{first}\nNEEDS YOU: the deploy key for prod, which only you hold.\n\
                     Nothing left: the parser is committed and pushed to origin/main."
                )
                .as_str()
            ),
            "{ended:#?}"
        );
    }
}

#[test]
fn a_queued_prompt_is_no_part_of_the_turn_and_its_run_goes_on() {
    let rows = screen(QUEUED_INPUT);
    assert!(
        rows.iter().any(|r| r == "◆ Queued input · ↑ to edit"),
        "measured"
    );
    assert!(
        rows.iter()
            .any(|r| r == "└ write the landing report round 2")
    );
    let footer = rows
        .iter()
        .rposition(|r| r.contains("· Auto-review"))
        .expect("the footer");
    assert!(rows[footer].ends_with("  steering the running turn"));
    assert_eq!(phase(&rows), (Phase::Busy, false), "the run goes on");
    assert_eq!(muse(QUEUED_INPUT).phase, Phase::Busy);
    let at_rest = |words: &str| {
        let mut r = rows.clone();
        r[footer] = format!("  lab-stub · high · /private/tmp/at.lab0/proj · Auto-review{words}");
        r
    };
    let words = "Here is the report.\n• check 1 passed\n• check 2 passed\n\
                 Nothing left: the parser is committed and pushed to origin/main.";
    for rest in ["", "    no active run", " steering the running turn"] {
        let ended = at_rest(rest);
        assert_eq!(phase(&ended), (Phase::Idle, true), "{rest:?}");
        assert_eq!(said_tail(&ended).as_deref(), Some(words), "{rest:?}");
    }
    for (said, runs) in [
        ("    queued for the next turn", true),
        ("    replacing the running turn", true),
        ("    interrupting run", true),
    ] {
        assert_eq!(phase(&at_rest(said)).0 == Phase::Busy, runs, "{said}");
    }
    let mut reply = at_rest("");
    let q = reply
        .iter()
        .position(|r| r == "◆ Queued input · ↑ to edit")
        .expect("the row");
    reply[q] = "◆ Queued input · ↑ to edit is how to change it; nothing else is left.".to_string();
    reply.remove(q + 1);
    assert_eq!(
        said_tail(&reply).as_deref(),
        Some("Queued input · ↑ to edit is how to change it; nothing else is left.")
    );
}

#[test]
fn muse_codes_title_says_whether_its_run_is_active() {
    let spinner = anchor_text("muse.title.spinner");
    assert_eq!(spinner.chars().count(), 10);
    for frame in spinner.chars() {
        assert!(title_runs(&format!("{frame} proj")), "{frame}");
    }
    for title in [
        "proj",
        "⠿ proj",
        "⠙proj",
        "proj ⠙",
        "✳ Claude Code",
        "",
        "⠙",
    ] {
        assert!(!title_runs(title), "{title:?}");
    }
    assert_eq!(MuseReader.title_runs("⠙ proj"), Some(true));
    assert_eq!(MuseReader.title_runs("proj"), Some(false));
    for p in Program::ALL {
        if p != Program::Muse {
            assert_eq!(p.reader().title_runs("⠙ proj"), None, "{}", p.name());
        }
    }
}

#[test]
fn the_bare_status_row_after_its_glyph_cycled_is_idle_with_no_evidence() {
    let rows = screen(BUSY_FIRST_FRAME_CYCLED);
    assert!(rows.iter().any(|r| r == "◈ Thinking"), "measured");
    assert!(composer(&rows).is_some());
    assert_eq!(phase(&rows), (Phase::Idle, false));
    let first = edited(&rows, "◈ Thinking", "◇ Thinking");
    assert_eq!(phase(&first), (Phase::Busy, true));
}
