// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixtures::{
    AUTH_401_240, BOX_SHELL_240, CONCURRENT_LIMIT, DRAFT, IDLE, LIMIT_429_FINAL_240,
    LIMIT_429_RETRYING_240, QUESTION_240, SIGNED_OUT_TURN, WALL_QUOTA,
};
use super::*;
use crate::prompt::fixtures::{bash_one_row, composer as claude_composer, rows, screen};
use crate::reader::{Program, identify, program_of, read};
use crate::wall::{Placement, WallKind};

#[test]
fn the_measured_context_counters_keep_the_241_composer_readable() {
    let measured = screen(super::fixtures::STUB_REPLY);
    assert_eq!(composer(&measured).expect("measured composer").1, vec![""]);
    assert_eq!(read(Some("kimi"), &measured, None).context_left, Some(85));
    for (row, used) in [
        ("context: 0%", 0),
        ("context: 0% (0/97.7k)", 0),
        ("context: 15% (13.7k/97.7k)", 15),
        ("context: 25% (1/4)", 25),
    ] {
        assert_eq!(context_row(row), Some(used), "{row}");
    }
    for row in [
        "context: +15%",
        "context: +15% (13.7k/97.7k)",
        "context: -15% (13.7k/97.7k)",
        "context: % (13.7k/97.7k)",
        "context: 15% then approve",
        "context: 15% (hello/world)",
        "context: 15% (1/2/3)",
        "context: 15% (1.2.3/2)",
        "context: 15% (1/2) trailing words",
        "context: 15% (1/2",
        "context: fifteen% (1/2)",
    ] {
        assert_eq!(context_row(row), None, "{row}");
    }
}

#[test]
fn the_quota_exhausted_turn_is_a_wall_and_the_concurrency_throttle_is_none() {
    let rows = screen(WALL_QUOTA);
    let w = wall(&rows).expect("the quota 429 is a wall");
    assert_eq!(w.kind, WallKind::UsageSession, "kimi names no window");
    assert_eq!(w.placement, Placement::ErrorRow);
    assert!(
        w.message
            .starts_with("Error: You exceeded your current token quota")
    );
    let r = read(Some("kimi"), &rows, None);
    assert_eq!(r.program, Program::Kimi);
    assert_eq!(r.wall.map(|w| w.kind), Some(WallKind::UsageSession));

    // Measured 2.4.0 final 429: both `Error: [provider.rate_limit] 429 …` and the indented
    // `/export-debug-zip` advice soft-wrap back to column 1 (` details.`, ` publicly.`).
    let measured_429 = screen(LIMIT_429_FINAL_240);
    let mw = wall(&measured_429).expect("the measured 2.4.0 soft-wrapped quota 429 is a wall");
    assert_eq!(mw.kind, WallKind::UsageSession);
    assert_eq!(mw.placement, Placement::ErrorRow);
    assert!(
        mw.message.contains("billing details. If this persists")
            && mw.message.ends_with("publicly."),
        "soft-wrapped continuations join cleanly: {}",
        mw.message
    );
    assert_eq!(
        read(Some("kimi"), &measured_429, None).wall.map(|w| w.kind),
        Some(WallKind::UsageSession)
    );

    // Negative: while Kimi Code is still retrying (`Retrying (6/10) · …`), and when a later turn
    // ends on a custom provider's 401 (`[provider.auth_error]`) under an earlier quota wall,
    // neither screen is a wall.
    assert_eq!(wall(&screen(LIMIT_429_RETRYING_240)), None);
    assert_eq!(wall(&screen(AUTH_401_240)), None);

    // Negative: a column-1 row under a short `Error:` that had room to spare, or a turn glyph at
    // column 1 right under a full-width `Error:` with no blank row between, is no continuation.
    let mut short_then_col1 = screen(SIGNED_OUT_TURN);
    let err_at = short_then_col1
        .iter()
        .position(|r| r.starts_with("   Error: "))
        .expect("error row");
    short_then_col1[err_at] = "   Error: You exceeded your current quota".to_string();
    short_then_col1.insert(err_at + 1, " Answered fine.".to_string());
    assert_eq!(wall(&short_then_col1), None);

    let mut glyph_under_wrap = measured_429.clone();
    let pub_at = glyph_under_wrap
        .iter()
        .rposition(|r| r == " publicly.")
        .expect("wrapped tail");
    glyph_under_wrap[pub_at] = " ● Answered fine.".to_string();
    assert_eq!(wall(&glyph_under_wrap), None);

    // Negative: the 429 a request over the account's concurrency answers is a throttle Kimi Code
    // retries, no spent quota: no wall, so nothing a rotation would act on.
    let throttle = screen(CONCURRENT_LIMIT);
    assert!(
        throttle
            .iter()
            .any(|r| r.contains("concurrent request limit"))
    );
    assert_eq!(wall(&throttle), None);
    assert_eq!(read(Some("kimi"), &throttle, None).wall, None);
}

#[test]
fn a_balance_and_a_lost_login_are_walls_of_their_own_kinds() {
    let with = |line: &str| {
        let mut rows = screen(SIGNED_OUT_TURN);
        let at = rows
            .iter()
            .position(|r| r.starts_with("   Error: "))
            .expect("the error row");
        rows[at] = format!("   Error: {line}");
        rows
    };
    assert_eq!(
        wall(&screen(SIGNED_OUT_TURN)).map(|w| (w.kind, w.message)),
        Some((
            WallKind::Auth,
            "Error: LLM not set, send \"/login\" to login".to_string()
        )),
        "the measured signed-out turn"
    );
    for (line, kind) in [
        (
            "Insufficient balance, please recharge your account",
            WallKind::Spend,
        ),
        ("Your account is in arrears", WallKind::Spend),
        ("OAuth token refused: re-login required", WallKind::Auth),
        ("No token for \"kimi-code\"", WallKind::Auth),
    ] {
        assert_eq!(wall(&with(line)).map(|w| w.kind), Some(kind), "{line}");
    }
    // Negative: any other error, and the quota words anywhere but the last error row.
    assert_eq!(wall(&with("connection reset by peer")), None);
    let mut earlier = screen(WALL_QUOTA);
    let at = earlier
        .iter()
        .position(|r| r.starts_with("   Error: "))
        .expect("the error row");
    earlier.insert(at + 1, String::new());
    earlier.insert(at + 2, "   The next turn answered fine.".to_string());
    assert_eq!(
        wall(&earlier),
        None,
        "a later row stands under the old error"
    );
}

#[test]
fn the_composer_reads_its_draft_and_the_footer_its_context() {
    let idle = screen(IDLE);
    let (caret, lines) = composer(&idle).expect("the composer");
    assert!(idle[caret].contains("│ >"));
    assert_eq!(lines, vec![String::new()]);
    let draft = screen(DRAFT);
    assert_eq!(
        composer(&draft).map(|(_, l)| l),
        Some(vec!["hello".to_string()])
    );
    let r = read(Some("kimi"), &idle, None);
    assert_eq!(r.context_left, Some(100), "context: 0% used");
    assert!(!r.phase_authoritative, "a busy turn is not measured");
    assert_eq!(r.wall, None, "an idle composer is no wall");
}

#[test]
fn kimi_s_screens_are_its_own_by_name_and_by_sight() {
    assert_eq!(program_of("kimi"), Some(Program::Kimi));
    assert_eq!(program_of("kimi-code"), Some(Program::Kimi));
    for fixture in [
        IDLE,
        DRAFT,
        SIGNED_OUT_TURN,
        WALL_QUOTA,
        BOX_SHELL_240,
        QUESTION_240,
    ] {
        let rows = screen(fixture);
        assert!(is_kimi_screen(&rows));
        assert_eq!(identify(None, &rows).program(), Program::Kimi);
        assert_eq!(identify(Some("node"), &rows).program(), Program::Kimi);
    }
    // A box in the composer's place still stands over the footer, and its context reads.
    let boxed = screen(BOX_SHELL_240);
    assert_eq!(composer_rows(&boxed), None, "no composer under the box");
    assert_eq!(context_left(&boxed), Some(78), "context: 22% used");
    assert_eq!(read(None, &boxed, None).context_left, Some(78));
    // Negative: a shell's own screen stays generic, and a pager showing Kimi Code's screen is
    // no Kimi Code.
    let shell = screen("$ ls\nCargo.toml  src\n$ ");
    assert!(!is_kimi_screen(&shell));
    assert_eq!(identify(None, &shell).program(), Program::Generic);
    assert_eq!(
        identify(Some("less"), &screen(IDLE)).program(),
        Program::Generic
    );
}

/// Kimi Code's welcome box as a Claude Code transcript quotes it, `indent` columns in: five
/// under its Bash tool's `⎿` (`aterm ctl text` of a Kimi tab), two in its own reply.
fn quoted_welcome(indent: usize) -> Vec<String> {
    let pad = " ".repeat(indent);
    let edge = "─".repeat(40);
    vec![
        format!("{pad}╭{edge}╮"),
        format!("{pad}│{}│", " ".repeat(40)),
        format!("{pad}│  ▐█▛█▛█▌  Welcome to Kimi Code!{}│", " ".repeat(8)),
        format!("{pad}│  ▐█████▌  Run /login or /provider to get started.│"),
        format!("{pad}╰{edge}╯"),
    ]
}

/// A CLAUDE CODE SCREEN QUOTING KIMI CODE'S WELCOME IS STILL CLAUDE CODE'S, whatever runtime hosts
/// it (none named, `node` for an npm install, `ssh`, `tmux`): the welcome box in its Bash tool's
/// output or its reply, above its own composer, and a box of its own under such a quote, read as
/// its box. CONTROL: the welcome box with no Claude Code frame under it is Kimi Code's by sight,
/// and Claude Code's screens unquoted are its own.
#[test]
fn a_claude_code_screen_quoting_kimi_codes_welcome_is_still_claude_codes() {
    let mut tool = rows(&["⏺ Bash(aterm ctl @s-kimi text | head -5)"]);
    let mut quote = quoted_welcome(5);
    quote[0] = format!("  ⎿  {}", quote[0].trim_start());
    tool.extend(quote);
    tool.extend(rows(&["", "⏺ That tab is Kimi Code at its welcome.", ""]));
    let mut reply = rows(&["⏺ Its screen opens so:", ""]);
    reply.extend(quoted_welcome(2));
    reply.push(String::new());
    for (what, above) in [("tool output", &tool), ("a reply", &reply)] {
        let mut claude = above.clone();
        claude.extend(claude_composer("  ? for shortcuts"));
        for program in [None, Some("node"), Some("ssh"), Some("tmux")] {
            assert_eq!(
                identify(program, &claude).program(),
                Program::Claude,
                "{what} under {program:?}"
            );
        }
        let mut boxed = above.clone();
        boxed.extend(bash_one_row());
        let reading = read(Some("node"), &boxed, None);
        assert_eq!(reading.program, Program::Claude, "{what}: its box");
        assert!(reading.prompt.is_some(), "{what}: its box is read");

        assert!(
            is_kimi_screen(above),
            "CONTROL: the quote alone is Kimi Code's by sight"
        );
        assert_eq!(identify(None, above).program(), Program::Kimi, "CONTROL");
    }
    assert_eq!(
        identify(Some("node"), &claude_composer("  ? for shortcuts")).program(),
        Program::Claude
    );
    let unquoted = read(Some("node"), &bash_one_row(), None);
    assert!(
        unquoted.prompt.is_some(),
        "CONTROL: Claude Code's box alone"
    );
}

/// ITS FOOTER CLAIMS NO OTHER PROGRAM'S SCREEN: over the whole measured fixture corpus, the
/// `context: N%` footer stands on Kimi Code's frames alone, so a frame whose composer a box
/// replaced identifies by the footer at its foot, and no other program's fixture is read as a
/// Kimi Code screen. NEGATIVE CONTROL: Kimi Code frames that draw neither composer, footer
/// nor welcome (its trust box, its exits) are claimed by nothing, and the corpus holds such
/// frames and over a thousand of the other programs'.
#[test]
fn its_footer_claims_no_other_programs_screen() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("the fixtures")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".txt"))
        .collect();
    names.sort();
    assert!(names.len() > 1000, "{} fixtures", names.len());
    let (mut footer_only, mut unclaimed, mut others) = (0usize, 0usize, 0usize);
    for name in &names {
        let rows = screen(&std::fs::read_to_string(dir.join(name)).expect("a fixture"));
        if name.starts_with("kimi-") {
            if composer_rows(&rows).is_none() && context_left(&rows).is_some() {
                footer_only += 1;
                assert_eq!(
                    identify(None, &rows).program(),
                    Program::Kimi,
                    "{name}: its footer names it"
                );
            } else if identify(None, &rows).program() != Program::Kimi {
                unclaimed += 1;
            }
        } else {
            others += 1;
            assert!(!is_kimi_screen(&rows), "{name}: no other program's screen");
        }
    }
    assert_eq!(
        footer_only, 6,
        "the approval, question and session-picker frames with no composer"
    );
    assert!(unclaimed > 0, "frames with nothing to tell them by stand");
    assert!(others > 1000, "{others} other programs' frames");
}
