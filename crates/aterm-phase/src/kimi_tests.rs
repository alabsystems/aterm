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
    assert_eq!(
        (r.phase, r.phase_authoritative),
        (Phase::Idle, true),
        "the composer with a blank row over it is idle"
    );
    assert_eq!(r.wall, None, "an idle composer is no wall");
}

#[test]
fn kimi_s_screens_are_its_own_by_name_and_by_sight() {
    assert_eq!(program_of("kimi"), Some(Program::Kimi));
    assert_eq!(program_of("kimi-code"), Some(Program::Kimi));
    assert_eq!(program_of("kimi-alab"), Some(Program::Kimi));
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
    let dir = aterm_tempfile::manifest_dir().join("src/fixtures");
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
        footer_only, 19,
        "the approval, question and session-picker frames with no composer"
    );
    assert!(unclaimed > 0, "frames with nothing to tell them by stand");
    assert!(others > 1000, "{others} other programs' frames");
}

#[test]
fn turn_end_reads_the_reply_launch_and_tool_evidence() {
    let ended = screen(super::fixtures::END_OF_TURN_240);
    assert_eq!(
        said_tail(&ended).as_deref(),
        Some("lab held reply, sent after 30 s")
    );
    assert!(said_whole(&ended));
    assert_eq!(turn_messages_before(&ended), None);
    assert_eq!(turn_said_before(&ended), None);
    assert!(!turn_ran_tools(&ended));
    assert!(turn_opens_above(&ended));
    assert_eq!(turn_opened_by(&ended).as_deref(), Some("lab draft one"));
    assert!(!interrupted(&ended));
    assert!(!fresh(&ended));
    assert!(opening(&ended));
    assert!(message_drawn(&ended));
    let without_launch: Vec<String> = ended
        .iter()
        .filter(|row| !row.contains("Welcome to Kimi Code!"))
        .cloned()
        .collect();
    assert!(!opening(&without_launch), "CONTROL: no launch card");

    let launch = screen(IDLE);
    assert!(fresh(&launch));
    assert!(!opening(&launch));
    assert!(!message_drawn(&launch));
    let tools = screen(BOX_SHELL_240);
    assert!(turn_ran_tools(&tools));
    let interrupted_screen = frame("2.4.0-interrupted-esc");
    assert!(interrupted(&interrupted_screen));
}

/// Every Kimi Code fixture by name (`kimi-<version>-<name>.txt`), with the phase its rows decide
/// and whether they decide it (`phase`). A new fixture needs its row here.
const PHASES: &[(&str, Phase, bool)] = &[
    ("2.3.1-draft", Phase::Idle, true),
    ("2.3.1-hand-built-concurrent-limit", Phase::Idle, true),
    ("2.3.1-hand-built-wall-quota", Phase::Idle, true),
    ("2.3.1-idle", Phase::Idle, true),
    ("2.3.1-signed-out-turn", Phase::Idle, true),
    ("2.4.0-auth-401", Phase::Idle, true),
    ("2.4.0-box-plan", Phase::Prompt, true),
    ("2.4.0-box-plan-approved", Phase::Idle, true),
    ("2.4.0-box-shell", Phase::Prompt, true),
    ("2.4.0-box-shell-approved", Phase::Idle, true),
    ("2.4.0-box-shell-reject", Phase::Prompt, true),
    ("2.4.0-box-shell-rejected", Phase::Idle, true),
    ("2.4.0-busy-ctrl-c-after-draft", Phase::Idle, true),
    ("2.4.0-busy-ctrl-c-draft", Phase::Busy, true),
    ("2.4.0-busy-draft", Phase::Busy, true),
    ("2.4.0-busy-held", Phase::Busy, true),
    ("2.4.0-busy-held-late", Phase::Busy, true),
    ("2.4.0-busy-held-tip", Phase::Busy, true),
    ("2.4.0-busy-queued", Phase::Busy, true),
    ("2.4.0-busy-streaming", Phase::Busy, true),
    ("2.4.0-continued", Phase::Idle, true),
    ("2.4.0-ctrl-c-draft", Phase::Idle, true),
    ("2.4.0-ctrl-c-draft-late", Phase::Idle, true),
    ("2.4.0-draft", Phase::Idle, true),
    ("2.4.0-draft-multiline", Phase::Idle, true),
    ("2.4.0-end-of-turn", Phase::Idle, true),
    ("2.4.0-end-of-turn-streamed", Phase::Idle, true),
    ("2.4.0-enter-swallowed", Phase::Idle, true),
    ("2.4.0-exit-confirm", Phase::Idle, true),
    ("2.4.0-exited-clean", Phase::Idle, false),
    ("2.4.0-exited-ctrl-c", Phase::Idle, false),
    ("2.4.0-exited-ctrl-d", Phase::Idle, false),
    ("2.4.0-exited-resume", Phase::Idle, false),
    ("2.4.0-exited-sigterm", Phase::Idle, false),
    ("2.4.0-idle", Phase::Idle, true),
    ("2.4.0-idle-plan-mode", Phase::Idle, true),
    ("2.4.0-interrupted-ctrl-c", Phase::Idle, true),
    ("2.4.0-interrupted-esc", Phase::Idle, true),
    ("2.4.0-killed", Phase::Idle, false),
    ("2.4.0-limit-429-final", Phase::Idle, true),
    ("2.4.0-limit-429-retrying", Phase::Busy, true),
    ("2.4.0-multiline-sent", Phase::Idle, true),
    ("2.4.0-paste-bracketed", Phase::Idle, true),
    ("2.4.0-paste-raw-burst", Phase::Idle, true),
    ("2.4.0-question", Phase::Prompt, true),
    ("2.4.0-question-answered", Phase::Idle, true),
    ("2.4.0-question-review", Phase::Prompt, true),
    ("2.4.0-queued-ran", Phase::Idle, true),
    ("2.4.0-resumed", Phase::Idle, true),
    ("2.4.0-resumed-picked", Phase::Idle, true),
    ("2.4.0-session-picker", Phase::Idle, false),
    ("2.4.0-slash-menu", Phase::Idle, true),
    ("2.4.0-text-cr-after-6s", Phase::Idle, true),
    ("2.4.0-text-cr-one-write", Phase::Idle, true),
    ("2.4.0-trust", Phase::Prompt, true),
    ("2.4.1-local-stub-reply", Phase::Idle, true),
    ("2.5.2-box-edit", Phase::Prompt, true),
    ("2.5.2-box-plan-forged", Phase::Prompt, true),
    ("2.5.2-box-shell-backspaced", Phase::Prompt, true),
    ("2.5.2-box-shell-backspaced-push", Phase::Prompt, true),
    ("2.5.2-box-shell-carriage-return", Phase::Prompt, true),
    ("2.5.2-box-shell-carriage-return-mid", Phase::Prompt, true),
    ("2.5.2-box-shell-empty-line", Phase::Prompt, true),
    ("2.5.2-box-shell-long-word", Phase::Prompt, true),
    ("2.5.2-box-shell-option-shaped", Phase::Prompt, true),
    ("2.5.2-box-shell-option-shaped-cr", Phase::Prompt, true),
    ("2.5.2-box-shell-read", Phase::Prompt, true),
    ("2.5.2-box-shell-spaced", Phase::Prompt, true),
    ("2.5.2-box-write", Phase::Prompt, true),
    ("2.5.2-fullscreen-box-plan", Phase::Prompt, true),
    ("2.5.2-fullscreen-box-shell", Phase::Prompt, true),
    (
        "2.5.2-fullscreen-box-shell-feedback-wide",
        Phase::Prompt,
        true,
    ),
    ("2.5.2-fullscreen-box-shell-overflow", Phase::Prompt, true),
    ("2.5.2-fullscreen-box-shell-squeezed", Phase::Prompt, true),
    ("2.5.2-fullscreen-box-write", Phase::Prompt, true),
    ("2.5.2-fullscreen-box-write-scrolled", Phase::Prompt, true),
    ("2.5.2-fullscreen-question", Phase::Prompt, true),
];

/// The screen of the Kimi Code fixture `name` (`kimi-<name>.txt`).
fn frame(name: &str) -> Vec<String> {
    let path = aterm_tempfile::manifest_dir().join(format!("src/fixtures/kimi-{name}.txt"));
    screen(&std::fs::read_to_string(&path).expect("the fixture"))
}

/// EVERY KIMI CODE FRAME READS THE PHASE ITS OWN ROWS DECIDE (measured on 2.3.1 and 2.4.0 at 110
/// columns, and 2.5.2's approvals, inline and fullscreen, at 144): a running turn's frames (held
/// before the first byte with a moon phase alone or a tip, streaming under `Working…`, a draft
/// typed or cleared under it, a message queued under
/// it, a 429 being retried) are busy by the spinner row opening the run of rows over the
/// composer's top border; every frame a turn ended on (a reply, an interrupt, an error, a
/// resume's notice, a plan mode switched on, a slash menu or the exit hint over the idle
/// composer) is idle by the blank row there. A command's approval, a file's, a plan's, a question
/// (its review step too) and the trust prompt drawn in the composer's place are prompts, each read
/// as its box, and no other frame reads a box; the session picker, the person's own, and the frames
/// of an exit decide nothing.
#[test]
fn every_kimi_frame_reads_the_phase_its_own_rows_decide() {
    let names: std::collections::BTreeSet<String> = crate::text::fixture_files()
        .into_iter()
        .filter_map(|(name, _)| {
            name.strip_prefix("kimi-")
                .and_then(|n| n.strip_suffix(".txt"))
                .map(str::to_string)
        })
        .collect();
    let table: std::collections::BTreeSet<String> =
        PHASES.iter().map(|(n, ..)| (*n).to_string()).collect();
    assert_eq!(names, table, "every fixture has its decided phase");
    for (name, want, decided) in PHASES {
        let rows = frame(name);
        assert_eq!(phase(&rows), (want.clone(), *decided), "{name}");
        let r = read(Some("kimi"), &rows, None);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (want.clone(), *decided),
            "{name}: the reading"
        );
        if *want == Phase::Busy {
            assert_eq!(r.wall, None, "{name}: a running turn reads no wall");
        }
        if *want == Phase::Prompt {
            assert!(r.prompt.is_some(), "{name}: the box");
        } else {
            assert_eq!(prompt(&rows), None, "{name}: no box");
        }
    }
    assert_eq!(
        read(Some("kimi"), &frame("2.4.0-limit-429-final"), None)
            .wall
            .map(|w| w.kind),
        Some(WallKind::UsageSession),
        "the turn's end decides, and its wall is read"
    );
    assert_eq!(
        phase(&[]),
        (Phase::Idle, false),
        "no screen decides nothing"
    );
    assert!(Program::Kimi.supervisable());
}

/// THE EXIT HINT A FIRST CTRL-C OR CTRL-D ARMS STANDS AT THE FOOTER'S LEFT AND HIDES NO
/// COMPOSER: the `context: N%` footer still reads past it, so the idle composer, its draft and its
/// context are read (the Ctrl-D hint HAND-BUILT from the measured Ctrl-C frame: 2.4.0's exit by
/// Ctrl-D drew it, 2.5.2's and 2.5.3's binaries arm it the same way). CONTROLS: the hint with no
/// context after it is no footer, and the composer it would stand under reads nothing; and the
/// `Shutting down…` an exit draws in the hint's place is no hint, so the ending program's
/// composer decides nothing.
#[test]
fn the_exit_hint_at_the_footers_left_hides_no_composer() {
    let rows = frame("2.4.0-exit-confirm");
    assert!(
        rows.iter()
            .any(|r| r.contains("Press Ctrl+C again to exit"))
    );
    assert_eq!(composer(&rows).map(|(_, l)| l), Some(vec![String::new()]));
    assert_eq!(context_left(&rows), Some(74), "context: 26% used");
    assert!(is_kimi_screen(&rows), "by sight");
    let respelled = |to: &str| -> Vec<String> {
        rows.iter()
            .map(|r| r.replace("Press Ctrl+C again to exit", to))
            .collect()
    };
    let ctrl_d = respelled("Press Ctrl+D again to exit");
    assert_eq!(composer(&ctrl_d).map(|(_, l)| l), Some(vec![String::new()]));
    assert_eq!(context_left(&ctrl_d), Some(74), "Ctrl-D: context: 26% used");
    assert_eq!(phase(&ctrl_d), (Phase::Idle, true), "Ctrl-D");
    let ending = respelled("Shutting down\u{2026} (running shutdown hooks)");
    assert_eq!(composer_rows(&ending), None, "CONTROL: an exit's footer");
    assert_eq!(phase(&ending), (Phase::Idle, false), "CONTROL: an exit");
    let bare: Vec<String> = rows
        .iter()
        .map(|r| match r.find("context: ") {
            Some(at) if r.contains("Press Ctrl+C") => r[..at].to_string(),
            _ => r.clone(),
        })
        .collect();
    assert_eq!(composer_rows(&bare), None, "CONTROL: no footer");
    assert_eq!(phase(&bare), (Phase::Idle, false), "CONTROL");
}

/// THE SPINNER COUNTS ONLY WHERE IT OPENS THE RUN OVER THE COMPOSER: a reply row that opens with a
/// moon phase, standing in an ended turn's transcript over its blank row, is no running turn
/// (nor an ended one: a spinner over the transcript is where 2.5's agent swarm draws its turn's,
/// so the blank row decides nothing); and a running turn whose spinner the build no longer draws
/// (its moon phase respelled) decides nothing, never an idle. NEGATIVE CONTROL for the mutants
/// that read a moon phase off any row of the screen and that take a run with no spinner for an
/// ended turn.
#[test]
fn the_spinner_counts_only_where_it_opens_the_run_over_the_composer() {
    let mut ended = frame("2.4.0-end-of-turn");
    let reply = ended
        .iter()
        .position(|r| r.contains("● lab held reply"))
        .expect("the reply");
    ended.splice(
        reply + 1..reply + 1,
        [
            "  🌓 · Tip: the waxing moon it drew while the turn ran".to_string(),
            "  ⠧ Working… · Tip: and the braille spinner after it".to_string(),
        ],
    );
    assert!(
        composer_rows(&ended).is_some(),
        "PRECONDITION: the composer"
    );
    assert_eq!(phase(&ended), (Phase::Idle, false));
    // A run with no spinner opening it decides nothing.
    let held = frame("2.4.0-busy-held");
    assert_eq!(phase(&held), (Phase::Busy, true), "PRECONDITION");
    let respelled: Vec<String> = held.iter().map(|r| r.replace('🌓', "o")).collect();
    assert_eq!(phase(&respelled), (Phase::Idle, false));
    assert!(!read(Some("kimi"), &respelled, None).phase_authoritative);
    // CONTROL: the queued message's block between the spinner and the border keeps it busy.
    assert_eq!(
        phase(&frame("2.4.0-busy-queued")),
        (Phase::Busy, true),
        "CONTROL"
    );
}

/// NO OTHER PROGRAM'S FRAME DECIDES A PHASE THROUGH KIMI CODE'S READING: over the whole fixture
/// corpus, every frame of another program reads nothing as evidence by `phase`. CONTROL: Kimi
/// Code's own frames decide theirs (`every_kimi_frame_reads_the_phase_its_own_rows_decide`).
#[test]
fn no_other_programs_frame_decides_a_kimi_phase() {
    let mut others = 0usize;
    for (name, text) in crate::text::fixture_files() {
        if name.starts_with("kimi-") || !name.ends_with(".txt") {
            continue;
        }
        others += 1;
        assert!(!phase(&screen(&text)).1, "{name}");
    }
    assert!(others > 1000, "{others} other programs' frames");
}

/// The options of `p` as `(number, label, focused)`.
fn options(p: &PromptV2) -> Vec<(Option<u8>, &str, bool)> {
    p.options
        .iter()
        .map(|o| (o.n, o.label.as_str(), o.focused))
        .collect()
}

/// EACH BOX IN THE COMPOSER'S PLACE READS AS THE BOX IT DRAWS (measured on 2.4.0 at 110 columns):
/// a command's approval is a Bash box whose command is its `$ ` row and whose description is
/// the row under it, the read-only rule reading the command with and without it; a plan's approval names its plans; a question
/// names its question and its options up to their descriptions, and its review step its last
/// question; all three are picked by their digits. The trust prompt names its folder and its two
/// unnumbered options, picked by arrows and Enter, its key row the one under its title. No box
/// offers an Esc read as cancelling it, and no option carries a role: nothing answers them.
#[test]
fn each_box_in_the_composers_place_reads_as_the_box_it_draws() {
    let shell = prompt(&frame("2.4.0-box-shell")).expect("the command's approval");
    assert_eq!(shell.kind, PromptKind::Bash);
    assert_eq!(shell.title, "Run this command?");
    assert_eq!(shell.command, "echo lab-kimi-tool");
    assert_eq!(
        shell.readings(),
        vec![
            "echo lab-kimi-tool",
            "echo lab-kimi-tool print a lab marker",
            "echo lab-kimi-toolprint a lab marker"
        ]
    );
    assert_eq!(shell.description, "print a lab marker");
    assert_eq!(
        options(&shell),
        vec![
            (Some(1), "Approve once", true),
            (Some(2), "Approve for this session", false),
            (Some(3), "Reject", false),
            (Some(4), "Reject with feedback", false),
        ]
    );
    assert_eq!(shell.select, Select::Digits);
    assert_eq!(
        shell.footer.as_deref(),
        Some("↑/↓ select · 1/2/3/4 choose · ↵ confirm")
    );
    assert_eq!(
        prompt(&frame("2.4.0-box-shell-reject")).map(|p| p.command),
        Some("echo lab-kimi-reject".to_string())
    );

    let plan = prompt(&frame("2.4.0-box-plan")).expect("the plan's approval");
    assert_eq!(plan.kind, PromptKind::PlanExit);
    assert_eq!(plan.title, "Ready to build with this plan?");
    assert_eq!(plan.command, "");
    assert_eq!(
        options(&plan),
        vec![
            (Some(1), "Lab plan A (Recommended)", true),
            (Some(2), "Lab plan B", false),
            (Some(3), "Reject", false),
            (Some(4), "Revise", false),
        ]
    );

    let question = prompt(&frame("2.4.0-question")).expect("the question");
    assert_eq!(question.kind, PromptKind::Question);
    assert_eq!(
        question.description,
        "Which lab colour should the marker use?"
    );
    assert_eq!(
        options(&question),
        vec![
            (Some(1), "Red (Recommended)", true),
            (Some(2), "Blue", false),
            (Some(3), "Other", false),
        ]
    );
    let review = prompt(&frame("2.4.0-question-review")).expect("the review step");
    assert_eq!(review.kind, PromptKind::Question);
    assert_eq!(review.description, "Ready to submit your answers?");
    assert_eq!(
        options(&review),
        vec![(Some(1), "Submit", true), (Some(2), "Cancel", false)]
    );

    let write = prompt(&frame("2.5.2-box-write")).expect("a file's write");
    let edit = prompt(&frame("2.5.2-box-edit")).expect("a file's edits");
    for (p, kind, title) in [
        (&write, PromptKind::Write, "Write this file?"),
        (&edit, PromptKind::Edit, "Apply these edits?"),
    ] {
        assert_eq!((p.kind, p.title.as_str()), (kind, title));
        assert_eq!(
            p.path.as_deref(),
            Some("/private/tmp/at.H2aa/proj/notes-for-the-lab.txt"),
            "{title}: the block's first row names the file, past a diff's counts"
        );
        assert_eq!(p.command, "/private/tmp/at.H2aa/proj/notes-for-the-lab.txt");
        assert!(p.command_rows.is_empty(), "{title}");
        assert_eq!(p.options.len(), 4, "{title}");
        assert_eq!(
            p.focused().map(|o| (o.n, o.label.as_str())),
            Some((Some(1), "Approve once"))
        );
        assert_eq!(
            p.footer.as_deref(),
            Some("↑/↓ select · 1/2/3/4 choose · ↵ confirm · ctrl+e preview")
        );
    }

    let trust = prompt(&frame("2.4.0-trust")).expect("the trust prompt");
    assert_eq!(trust.kind, PromptKind::Trust);
    assert_eq!(trust.command, "/private/tmp/at.H2aa/proj");
    assert_eq!(trust.path.as_deref(), Some("/private/tmp/at.H2aa/proj"));
    assert_eq!(
        options(&trust),
        vec![(None, "Trust and continue", true), (None, "Exit", false)]
    );
    assert_eq!(trust.select, Select::ArrowsEnter);
    assert_eq!(
        trust.footer.as_deref(),
        Some("↑↓ navigate · Enter select · Esc exit")
    );

    for (p, roles) in [
        (
            &shell,
            vec![Role::Once, Role::Session, Role::Deny, Role::Deny],
        ),
        (
            &plan,
            vec![Role::Other, Role::Other, Role::Deny, Role::Other],
        ),
        (&question, vec![Role::Answer, Role::Answer, Role::Other]),
        (&review, vec![Role::Answer, Role::Other]),
        (
            &write,
            vec![Role::Once, Role::Session, Role::Deny, Role::Deny],
        ),
        (
            &edit,
            vec![Role::Once, Role::Session, Role::Deny, Role::Deny],
        ),
        (&trust, vec![Role::Trust, Role::Exit]),
    ] {
        assert_eq!(p.cancel, None, "{:?}", p.kind);
        assert_eq!(
            p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
            roles,
            "{:?}",
            p.kind
        );
    }
}

/// A BOX IS READ ONLY BETWEEN ITS RULES OVER THE FOOTER, UNDER ITS OWN TITLE: the session
/// picker on the same rules is none; the measured command's approval with a reply's row under
/// its lower rule, with its title reworded to one the panel draws for no tool, or with its upper
/// rule gone reads no box. NEGATIVE CONTROL for the mutants that read a box off the rules alone,
/// off a title anywhere, or off the lower rule wherever it stands.
#[test]
fn a_box_is_read_only_between_its_rules_over_the_footer_under_its_own_title() {
    assert_eq!(prompt(&frame("2.4.0-session-picker")), None, "the picker");
    let card = frame("2.4.0-box-shell");
    assert_eq!(phase(&card), (Phase::Prompt, true), "PRECONDITION");
    let lower = card.iter().rposition(|r| rule(r)).expect("the lower rule");
    let mut trailed = card.clone();
    trailed.insert(lower + 1, " ● a reply's row".to_string());
    assert_eq!(prompt(&trailed), None, "a row under the lower rule");
    let reworded: Vec<String> = card
        .iter()
        .map(|r| r.replace("Run this command?", "Run this command now?"))
        .collect();
    assert_eq!(prompt(&reworded), None, "a title no tool's panel draws");
    assert_eq!(phase(&reworded), (Phase::Idle, false));
    let spaced: Vec<String> = card
        .iter()
        .map(|r| r.replace("Run this command?", "Approve web fetch?"))
        .collect();
    assert_eq!(prompt(&spaced), None, "a tool's name is one word");
    let upper = card[..lower]
        .iter()
        .rposition(|r| rule(r))
        .expect("the upper rule");
    let mut opened = card.clone();
    opened[upper] = String::new();
    assert_eq!(prompt(&opened), None, "no upper rule");
}

/// The measured command's approval with its `$ ` row replaced by `rows`, the description row
/// under it kept or `described` false to drop it.
fn approval_with(rows: &[&str], described: bool) -> Vec<String> {
    let mut card = frame("2.4.0-box-shell");
    let at = card
        .iter()
        .position(|r| r.trim_start().starts_with("$ "))
        .expect("the command's row");
    let under = if described { 0 } else { 1 };
    card.splice(at..=at + under, rows.iter().map(|r| (*r).to_string()));
    card
}

/// AN APPROVAL READS EVERY LINE OF ITS COMMAND (2.5.2's `renderShellDisplayBlock`: the first
/// line after `$ `, each later one after `· `, a long line wrapping two columns deeper, the
/// description last at that same depth): a later line is its own row of the reading, so the
/// read-only rule sees `rm` on a line of its own; the measured empty line, its `·` alone, is a
/// line of its own; every wrap joins its line, read with a space and with none (the measured run
/// of spaces dropped at a wrap under a row with room for its first word, the measured word cut
/// at the box's edge and the measured line broken at a carriage return within it, each hiding
/// `;git push --force` behind a read), and a wrap that opens like an option at the unfocused
/// options' column is the command's, the options being the block over the key row (measured);
/// the last row at the description's depth is read with and without it, as its last line's
/// wrap and never as a line of its own (the measured description); a blank row inside the block
/// (the measured carriage returns, at a line's end or twice in it), a row it holds at no depth of
/// its own, more than [`MAX_WRAPS`] wraps, or Kimi's `Dangerous:` row over it (the measured
/// backspaces that draw `cat README.md` over `rm -rf build-output`) leaves the command unread.
/// NEGATIVE CONTROL for the mutants that read the `$ ` row alone, stop at a bare mark or a blank
/// row, read a stray row, join a wrap one way only, take an option-shaped wrap for an option,
/// read past the bound, read under a danger row or read the description as a line of its own,
/// each of which reads a read-only verdict for a box that pushes or deletes, or no verdict for a
/// measured `echo`.
#[test]
fn an_approval_reads_every_line_of_its_command() {
    let read = |rows: &[String]| prompt(rows).map(|p| p.command_rows);
    let measured = prompt(&frame("2.4.0-box-shell")).expect("PRECONDITION");
    assert_eq!(
        (measured.description_rows, measured.readings()),
        (
            1,
            vec![
                "echo lab-kimi-tool".to_string(),
                "echo lab-kimi-tool print a lab marker".to_string(),
                "echo lab-kimi-toolprint a lab marker".to_string()
            ]
        ),
        "the measured description, read off and as a wrap both ways"
    );

    let lines = approval_with(&["   $ cat README.md", "   · rm -rf build"], true);
    let p = prompt(&lines).expect("the approval");
    assert_eq!(
        p.command_rows,
        vec!["cat README.md", "rm -rf build", "print a lab marker"]
    );
    assert_eq!(p.description, "print a lab marker");
    for reading in [
        "cat README.md\nrm -rf build",
        "cat README.md\nrm -rf build print a lab marker",
    ] {
        assert!(p.readings().contains(&reading.to_string()), "{reading:?}");
    }
    assert_eq!(p.options.len(), 4, "the options stay");

    let empty = prompt(&frame("2.5.2-box-shell-empty-line")).expect("the measured approval");
    assert_eq!(
        empty.command_rows,
        vec![
            "cat README.md",
            "",
            "git push --force",
            "Read the readme, then push"
        ]
    );
    let readings = empty.readings();
    assert!(
        readings.contains(&"cat README.md\n\ngit push --force".to_string())
            && !readings
                .iter()
                .any(|r| r.ends_with("\nRead the readme, then push")),
        "{readings:?}"
    );

    let spaced = prompt(&frame("2.5.2-box-shell-spaced")).expect("the measured approval");
    assert_eq!(
        (spaced.command_rows.clone(), spaced.description_rows),
        (
            vec!["cat README.md".to_string(), ";git push --force".to_string()],
            1
        ),
        "a row at the description's depth under a row with room for its first word"
    );
    assert_eq!(
        spaced.readings(),
        vec![
            "cat README.md",
            "cat README.md ;git push --force",
            "cat README.md;git push --force"
        ]
    );

    // The measured wrap that opens like an option is the command's: the options are the block
    // over the key row; under the measured blank row of two carriage returns, the command is
    // unread.
    let p = prompt(&frame("2.5.2-box-shell-option-shaped")).expect("the measured approval");
    assert_eq!(
        (p.command_rows.clone(), options(&p).len()),
        (
            vec![
                "cat README.md".to_string(),
                "1. ;git push --force".to_string()
            ],
            4
        ),
        "a wrap shaped like an option"
    );
    assert!(
        p.readings()
            .contains(&"cat README.md 1. ;git push --force".to_string())
    );
    let p = prompt(&frame("2.5.2-box-shell-option-shaped-cr")).expect("the measured approval");
    assert_eq!(
        (p.command_rows.clone(), options(&p).len()),
        (Vec::<String>::new(), 4),
        "a blank row between"
    );

    let word = "a".repeat(138);
    let cut = prompt(&frame("2.5.2-box-shell-long-word")).expect("the measured approval");
    assert_eq!(
        cut.command_rows,
        vec![format!("echo {word}"), "#;git push --force".to_string()],
        "PRECONDITION: a word cut at the box's edge"
    );
    assert!(
        cut.readings()
            .contains(&format!("echo {word}#;git push --force")),
        "the cut word read whole: {:?}",
        cut.readings()
    );
    let broken =
        prompt(&frame("2.5.2-box-shell-carriage-return-mid")).expect("the measured approval");
    assert_eq!(
        broken.readings(),
        vec![
            "cat README.md",
            "cat README.md #;git push --force",
            "cat README.md#;git push --force"
        ],
        "a line broken at its carriage return, read with no space too"
    );

    let returned = frame("2.5.2-box-shell-carriage-return");
    assert_eq!(
        read(&returned),
        Some(Vec::new()),
        "a blank row inside the block"
    );
    let at = returned
        .iter()
        .position(|r| r.trim() == "$ cat README.md")
        .expect("the first line");
    let mut closed = returned.clone();
    closed.remove(at + 1);
    assert_eq!(
        read(&closed),
        Some(vec![
            "cat README.md".to_string(),
            "git push --force".to_string()
        ]),
        "CONTROL: closed up, both lines"
    );

    let spoofed = frame("2.5.2-box-shell-backspaced");
    assert_eq!(read(&spoofed), Some(Vec::new()), "under Kimi's danger row");
    assert_eq!(
        read(&undangered(&spoofed)),
        Some(vec!["cat README.md".to_string()]),
        "CONTROL: without it, the line drawn"
    );

    let stray = approval_with(&["   $ cat README.md", "   rm -rf build"], true);
    let p = prompt(&stray).expect("the approval");
    assert_eq!(
        (p.command.as_str(), p.readings().len()),
        ("", 0),
        "a row at the lines' depth with no mark is no line of the block"
    );

    let long = format!(
        "cat {}",
        (1..=5)
            .map(|i| format!("lab-notes-{i:02}.txt"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let first = format!("   $ {long}");
    let wrapped = approval_with(
        &[
            &first,
            "     && rm -rf /private/tmp/at.H2aa/proj",
            "     print a lab marker",
        ],
        false,
    );
    let p = prompt(&wrapped).expect("the approval");
    assert_eq!(
        p.command_rows,
        vec![
            format!("{long} && rm -rf /private/tmp/at.H2aa/proj"),
            "print a lab marker".to_string()
        ],
        "a deeper row with a row under it is a wrap"
    );
    assert_eq!(
        (p.description.as_str(), p.description_rows),
        ("print a lab marker", 1)
    );

    let full = format!("cat {}", "x".repeat(102 - "cat ".len()));
    let edge = format!("   $ {full}");
    let unsure = approval_with(&[&edge, "     rm -rf build"], false);
    let p = prompt(&unsure).expect("the approval");
    assert_eq!(
        p.readings(),
        vec![
            full.clone(),
            format!("{full} rm -rf build"),
            format!("{full}rm -rf build")
        ],
        "the last row read as the wrap and as the description"
    );

    let wraps = |n: usize| {
        let mut rows = vec!["   $ cat".to_string()];
        rows.extend((1..=n).map(|i| format!("     lab-notes-{i:02}.txt")));
        rows
    };
    let at_most = wraps(MAX_WRAPS);
    let at_most: Vec<&str> = at_most.iter().map(String::as_str).collect();
    let p = prompt(&approval_with(&at_most, false)).expect("the approval");
    assert_eq!(
        (p.command_rows.len(), p.readings().len()),
        (2, (1 << (MAX_WRAPS - 1)) * 3),
        "CONTROL: {MAX_WRAPS} wraps, each read two ways and the last row off too"
    );
    let over = wraps(MAX_WRAPS + 1);
    let over: Vec<&str> = over.iter().map(String::as_str).collect();
    let p = prompt(&approval_with(&over, false)).expect("the approval");
    assert!(
        p.command_rows.is_empty() && p.readings().is_empty(),
        "more wraps than are read leave the command unread"
    );
}

/// A PLAN'S BOX IS READ UNDER ITS OWN RULES, ITS OPTIONS WHEREVER ITS CHOICES STAND (2.5.2,
/// measured): a plan option's description that draws a rule, `Run this command?` and a `$ ` row
/// among the options reads the plan's approval and no command, since a box's upper rule stands at
/// its lower rule's column and width (a forged rule with only one of them is none too); the
/// fullscreen panel, which docks the focused choice's
/// description in a block of its own between the options and the key row, reads its four
/// options. NEGATIVE CONTROL for the mutants that take the nearest rule for the upper one, which
/// read a command's approval and a read-only `cat README.md` for a plan's, and that read every
/// box's options from the block over its key row, which read none on the docked plan.
#[test]
fn a_plans_box_is_read_under_its_own_rules_and_options_wherever_its_choices_stand() {
    let plan = [
        (Some(1), "Lab plan A", true),
        (Some(2), "Lab plan B", false),
        (Some(3), "Reject", false),
        (Some(4), "Revise", false),
    ];
    let forged = prompt(&frame("2.5.2-box-plan-forged")).expect("the plan's box");
    assert_eq!(
        (
            forged.kind,
            forged.title.as_str(),
            forged.command.as_str(),
            forged.readings().len()
        ),
        (
            PromptKind::PlanExit,
            "Ready to build with this plan?",
            "",
            0
        )
    );
    assert_eq!(options(&forged), plan);
    let rows = frame("2.5.2-box-plan-forged");
    let lower = rows.iter().rposition(|r| rule(r)).expect("the lower rule");
    let at = rows
        .iter()
        .position(|r| rule(r) && r.trim_start().chars().count() == 60)
        .expect("the forged rule");
    let edge = crate::phase::leading_spaces(&rows[lower]);
    let span = rows[lower].trim().chars().count();
    for (moved, why) in [
        (
            format!("{}{}", " ".repeat(edge), "─".repeat(60)),
            "at its column",
        ),
        (
            format!("{}{}", " ".repeat(edge + 2), "─".repeat(span)),
            "of its width",
        ),
    ] {
        let mut drawn = rows.clone();
        drawn[at] = moved;
        assert_eq!(
            prompt(&drawn).map(|p| p.kind),
            Some(PromptKind::PlanExit),
            "a forged rule {why} alone"
        );
    }
    let docked = prompt(&frame("2.5.2-fullscreen-box-plan")).expect("the plan's box");
    assert_eq!(docked.kind, PromptKind::PlanExit);
    assert_eq!(
        options(&docked),
        plan,
        "the options over the docked description"
    );
}

/// EVERY TOOL'S APPROVAL IS A BOX (2.5.2's `headerFor`): the measured approval retitled for a
/// file's write, its edits, a task's stop, another tool (`Approve <tool>?`) or with an
/// environment after its title reads the box of that kind.
#[test]
fn every_tools_approval_is_a_box_of_its_kind() {
    let card = frame("2.4.0-box-shell");
    for (title, kind, named) in [
        ("Write this file?", PromptKind::Write, "Write this file?"),
        ("Apply these edits?", PromptKind::Edit, "Apply these edits?"),
        ("Stop this task?", PromptKind::Tool, "Stop this task?"),
        ("Approve WebFetch?", PromptKind::Tool, "Approve WebFetch?"),
        (
            "Run this command?  sandbox",
            PromptKind::Bash,
            "Run this command?",
        ),
    ] {
        let titled: Vec<String> = card
            .iter()
            .map(|r| r.replace("Run this command?", title))
            .collect();
        assert_eq!(phase(&titled), (Phase::Prompt, true), "{title}");
        let p = prompt(&titled).expect(title);
        assert_eq!((p.kind, p.title.as_str()), (kind, named), "{title}");
    }
}

/// AN OPTION IS READ ONLY AT ITS OWN COLUMN (2.4.0's frames, 2.5.2's renderers): an option's
/// description, a question's body or a review step's answer that opens like a numbered option
/// adds none; a multi-select question's `[✓]`/`[ ]` rows are its options, numbered in order from
/// the first it shows. NEGATIVE CONTROL for the mutant that reads every row's words whatever
/// their column, which adds a second option 1.
#[test]
fn an_option_is_read_only_at_its_own_column() {
    let plan: Vec<String> = frame("2.4.0-box-plan")
        .iter()
        .map(|r| r.replace("the first way", "1. write the parser, then the tests"))
        .collect();
    assert_eq!(
        options(&prompt(&plan).expect("the plan")),
        vec![
            (Some(1), "Lab plan A (Recommended)", true),
            (Some(2), "Lab plan B", false),
            (Some(3), "Reject", false),
            (Some(4), "Revise", false),
        ]
    );

    let mut asked: Vec<String> = frame("2.4.0-question")
        .iter()
        .map(|r| r.replace("the lab default", "1. the lab default"))
        .collect();
    let at = asked
        .iter()
        .position(|r| r.trim_start().starts_with("? "))
        .expect("the question's row");
    asked.insert(at + 1, "    2. Blue is the other one".to_string());
    let q = prompt(&asked).expect("the question");
    assert_eq!(
        options(&q),
        vec![
            (Some(1), "Red (Recommended)", true),
            (Some(2), "Blue", false),
            (Some(3), "Other", false),
        ]
    );
    assert_eq!(q.description, "Which lab colour should the marker use?");

    let review: Vec<String> = frame("2.4.0-question-review")
        .iter()
        .map(|r| r.replace("→  Red (Recommended)", "→  1. Rewrite the parser"))
        .collect();
    let r = prompt(&review).expect("the review step");
    assert_eq!(
        options(&r),
        vec![(Some(1), "Submit", true), (Some(2), "Cancel", false)]
    );
    assert_eq!(r.description, "Ready to submit your answers?");

    let multi: Vec<String> = frame("2.4.0-question")
        .iter()
        .map(|r| {
            r.replace("→ [1] Red", "[✓] Red")
                .replace("  [2] Blue", "[ ] Blue")
                .replace("  [3] Other", "[ ] Other")
        })
        .collect();
    let m = prompt(&multi).expect("a multi-select question");
    assert_eq!(
        options(&m),
        vec![
            (Some(1), "Red (Recommended)", false),
            (Some(2), "Blue", false),
            (Some(3), "Other", false),
        ]
    );
    let mut scrolled = multi.clone();
    let hint = scrolled
        .iter()
        .rposition(|r| r.contains("esc cancel"))
        .expect("the key row");
    scrolled.insert(hint - 1, "    showing 3-5 of 7".to_string());
    assert_eq!(
        prompt(&scrolled)
            .expect("a scrolled multi-select question")
            .options
            .iter()
            .map(|o| o.n)
            .collect::<Vec<_>>(),
        vec![Some(3), Some(4), Some(5)]
    );
}

/// `rows` with the scrollbar wiped from every row of the box's body, between its title and its
/// first option.
fn unbarred(rows: &[String]) -> Vec<String> {
    let title = rows
        .iter()
        .position(|r| r.contains("Run this command?"))
        .expect("the title");
    let first = rows
        .iter()
        .position(|r| r.trim_start().starts_with("1.") || r.trim_start().starts_with("\u{25b6} 1."))
        .expect("the first option");
    rows.iter()
        .enumerate()
        .map(|(i, r)| {
            if i > title && i < first {
                r.trim_end_matches(['│', '┃', '█']).trim_end().to_string()
            } else {
                r.clone()
            }
        })
        .collect()
}

/// `rows` with Kimi's `Dangerous: <label>` row over a command blanked.
fn undangered(rows: &[String]) -> Vec<String> {
    rows.iter()
        .map(|r| {
            if r.trim_start().starts_with("Dangerous: ") {
                String::new()
            } else {
                r.clone()
            }
        })
        .collect()
}

/// `rows` with one more transcript row over the box, its upper rule off the screen's second row.
fn lowered(rows: &[String]) -> Vec<String> {
    let mut rows = rows.to_vec();
    rows.insert(1, " \u{25cf} a transcript row".to_string());
    rows
}

/// The column a measured box's rules start at and the cells they span.
fn edges(rows: &[String]) -> (usize, usize) {
    let r = rows.iter().find(|r| rule(r)).expect("a rule");
    (
        crate::phase::leading_spaces(r),
        crate::text::trim(r).chars().count(),
    )
}

/// A FILE'S BOX NAMES ITS FILE ONLY WHEN IT IS DRAWN WHOLE (2.5.2's `renderBody`, read in its
/// binary): the measured write with its path cut at the box's edge by `...` reads that text as
/// its command and no path, and so does one of two-cell glyphs whose cells reach the edge, and
/// one whose `...` stands short of the edge: Kimi stops a cut before a grapheme that does not
/// fit (four cells to it for `ကျွေ`) and counts an asat (U+103A) a cell the grid draws as none,
/// so a cut row may end cells short of the edge. NEGATIVE CONTROL: the same path drawn whole
/// is named; the short cut kills the mutant that judges a cut by the row's reach.
#[test]
fn a_file_box_names_its_file_only_when_drawn_whole() {
    let path = "/private/tmp/at.H2aa/proj/notes-for-the-lab.txt";
    let write = frame("2.5.2-box-write");
    let (edge, width) = edges(&write);
    let at = write
        .iter()
        .position(|r| r.trim() == path)
        .expect("the path's row");
    let lead = crate::phase::leading_spaces(&write[at]);
    let long = format!("/private/tmp/at.H2aa/proj/{}", "deep/".repeat(width / 5));
    let cut: String = long.chars().take(edge + width - lead - 3).collect();
    let mut cut_write = write.clone();
    cut_write[at] = format!("{}{cut}...", " ".repeat(lead));
    let p = prompt(&cut_write).expect("the write");
    assert_eq!(
        (p.kind, p.path),
        (PromptKind::Write, None),
        "a cut path names no file"
    );
    assert_eq!(p.command, format!("{cut}..."), "the box shows what it drew");

    // A path of two-cell glyphs is cut where its cells, not its characters, reach the edge.
    let mut wide = String::from("/private/tmp/at.H2aa/proj/");
    for glyph in "日本/".chars().cycle() {
        if crate::question::cells(&format!("{wide}{glyph}")) > edge + width - lead - 3 {
            break;
        }
        wide.push(glyph);
    }
    assert!(
        lead + wide.chars().count() + 3 + 2 < edge + width,
        "PRECONDITION: its characters fall short of the edge"
    );
    let mut cut_wide = write.clone();
    cut_wide[at] = format!("{}{wide}...", " ".repeat(lead));
    let p = prompt(&cut_wide).expect("the write");
    assert_eq!(
        (p.path, p.command),
        (None, format!("{wide}...")),
        "a path of two-cell glyphs cut at the edge"
    );

    // Three asats Kimi counted as a cell each: its `...` stands three cells short of the edge.
    let asats = "/private/tmp/at.H2aa/proj/မြန်မာ-ကင်မရာ-ဓာတ်ပုံ/";
    let short: String = format!("{asats}{}", "b".repeat(width))
        .chars()
        .take_while({
            let mut taken = String::new();
            move |ch| {
                taken.push(*ch);
                crate::question::cells(&taken) <= edge + width - lead - 3 - 3
            }
        })
        .collect();
    assert!(
        lead + crate::question::cells(&short) + 3 + 2 < edge + width,
        "PRECONDITION: its cells fall short of the edge"
    );
    let mut cut_short = write.clone();
    cut_short[at] = format!("{}{short}...", " ".repeat(lead));
    let p = prompt(&cut_short).expect("the write");
    assert_eq!(
        (p.path, p.command),
        (None, format!("{short}...")),
        "a cut whose `...` stands short of the edge names no file"
    );
    assert_eq!(
        prompt(&write).and_then(|p| p.path).as_deref(),
        Some(path),
        "NEGATIVE CONTROL: the path drawn whole is named"
    );
}

/// A FULLSCREEN PANEL READS ITS COMMAND AND FILE ONLY OFF A BODY PROVEN DRAWN WHOLE (2.5.2 under
/// `tui_mode = "fullscreen"`, measured): its dock shows at most six body rows and scrolls a body
/// taller than that under a scrollbar, its focused key row then ending `· shift+↑/↓ scroll`. A
/// command whose rows all fit reads every line and its description, as inline. The six-line
/// command whose last two lines (`rm -rf build-output` the last) and description are hidden
/// below reads no command, so the read-only rule judges none of it rather than the four
/// read-only lines it shows; so does the first frame of a dock squeezed under its body's rows,
/// whose key row, judged off the previous layout, still ends `↵ confirm` over `cat README.md`
/// with `git push --force` hidden, by its scrollbar and by its upper rule on the screen's second
/// row, each alone; so does the squeezed dock's on its feedback line, whose key row shows
/// nothing, beside rows of two-cell glyphs (`mv` and `rm` hidden below); and so does a body that
/// fits under the feedback line's key row, an unfocused panel's, or one cut at the box's edge. A
/// body row that reaches the scrollbar's column by its cells (a two-cell glyph's too), or ends in
/// any of a bar's glyphs short of it, is drawn in part. A write whose file block overflows names
/// no file and reads no command, at the top of its scroll and scrolled to its foot alike. The
/// question reads as inline: its question, its five options numbered in order, its key row.
/// NEGATIVE CONTROL: with every mark but one wiped, each shell frame reads the rows it shows,
/// and a row reaching the rules' last cell is read; they kill the mutants that read past the
/// hint, the bar or the dock's place, take the feedback line's, an unfocused or a cut key row
/// for the focused one, judge a row's reach in characters or its glyph only at the bar's column,
/// or drop one of the bar's glyphs.
#[test]
fn a_fullscreen_panel_reads_its_command_and_file_only_off_a_body_drawn_whole() {
    let read = |rows: &[String]| prompt(rows).map(|p| p.command_rows);
    let approvals = vec![
        (Some(1), "Approve once", true),
        (Some(2), "Approve for this session", false),
        (Some(3), "Reject", false),
        (Some(4), "Reject with feedback", false),
    ];
    let shell = prompt(&frame("2.5.2-fullscreen-box-shell")).expect("the command");
    assert_eq!(shell.kind, PromptKind::Bash);
    assert_eq!(
        shell.command_rows,
        [
            "cat README.md",
            "git push --force",
            "Read the readme, then push"
        ]
    );
    assert_eq!(shell.command, "cat README.md git push --force");
    assert_eq!(shell.description, "Read the readme, then push");
    assert!(
        shell
            .readings()
            .contains(&"cat README.md\ngit push --force".to_string()),
        "{:?}",
        shell.readings()
    );
    assert_eq!(options(&shell), approvals);
    assert_eq!(
        shell.footer.as_deref(),
        Some("↑/↓ select · 1/2/3/4 choose · ↵ confirm")
    );

    let overflow = frame("2.5.2-fullscreen-box-shell-overflow");
    let over = prompt(&overflow).expect("the overflowing command");
    assert_eq!(
        (over.kind, over.title.as_str()),
        (PromptKind::Bash, "Run this command?")
    );
    assert_eq!((over.command.as_str(), over.description.as_str()), ("", ""));
    assert!(over.command_rows.is_empty() && over.readings().is_empty());
    assert_eq!(options(&over), approvals);
    assert_eq!(
        over.footer.as_deref(),
        Some("↑/↓ select · 1/2/3/4 choose · ↵ confirm · shift+↑/↓ scroll")
    );
    let hint = " \u{b7} shift+\u{2191}/\u{2193} scroll";
    let focused = "   \u{2191}/\u{2193} select \u{b7} 1/2/3/4 choose \u{b7} \u{21b5} confirm";
    let unhinted: Vec<String> = overflow.iter().map(|r| r.replace(hint, "")).collect();
    assert_ne!(
        unhinted, overflow,
        "PRECONDITION: the key row offers the scroll"
    );
    let lines = [
        "cat README.md",
        "ls -la",
        "git status",
        "git log --oneline -3",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(
        read(&undangered(&unbarred(&overflow))),
        Some(Vec::new()),
        "the hint withholds the command"
    );
    assert_eq!(
        read(&undangered(&unbarred(&unhinted))),
        Some(lines),
        "CONTROL: without it, the rows it shows"
    );

    // The first frame of a dock squeezed under its body's rows: its key row whole, its scrollbar
    // and its place on the screen's second row withholding the command, each alone.
    let squeezed = frame("2.5.2-fullscreen-box-shell-squeezed");
    assert!(
        squeezed[1].trim_start().starts_with('─')
            && squeezed
                .iter()
                .any(|r| r.trim().ends_with("\u{21b5} confirm"))
            && squeezed.iter().filter(|r| r.ends_with('┃')).count() == 2,
        "PRECONDITION: the upper rule on the second row, the key row whole, a thumb alone"
    );
    assert_eq!(read(&squeezed), Some(Vec::new()), "the squeezed frame");
    assert_eq!(
        read(&unbarred(&squeezed)),
        Some(Vec::new()),
        "the dock's place withholds the command"
    );
    let low = lowered(&squeezed);
    assert_eq!(read(&low), Some(Vec::new()), "the scrollbar withholds it");
    let bare = unbarred(&low);
    assert_eq!(
        read(&bare),
        Some(vec!["cat README.md".to_string()]),
        "CONTROL: with neither, the rows it shows"
    );

    // A row reaching the scrollbar's column (the grid drew it wider than Kimi counted, its bar
    // past the screen's edge) is drawn in part, and so is one ending in a bar's glyph short of it
    // (drawn narrower); one reaching the rules' last cell is read.
    let (edge, width) = edges(&bare);
    let at = bare
        .iter()
        .position(|r| r.trim_start().starts_with("$ "))
        .expect("the first line");
    let to = |cells: usize, glyph: char| {
        let mut rows = bare.clone();
        let pad =
            cells - crate::question::cells(&glyph.to_string()) - crate::question::cells(&bare[at]);
        rows[at] = format!("{}{}{glyph}", bare[at], " ".repeat(pad));
        assert_eq!(crate::question::cells(&rows[at]), cells, "PRECONDITION");
        rows
    };
    assert_eq!(
        read(&to(edge + width + 1, 'x')),
        Some(Vec::new()),
        "the bar's column"
    );
    assert_eq!(
        read(&to(edge + width + 1, '日')),
        Some(Vec::new()),
        "a two-cell glyph reaching the bar's column, its characters short of it"
    );
    for glyph in ['│', '┃', '█'] {
        assert_eq!(
            read(&to(edge + width - 2, glyph)),
            Some(Vec::new()),
            "the bar's {glyph} short of it"
        );
    }
    assert_eq!(
        read(&to(edge + width, 'x')).map(|r| r.len()),
        Some(1),
        "CONTROL: the rules' last cell"
    );

    // The feedback line's key row shows nothing, over a squeezed dock beside rows of two-cell
    // glyphs: no command. CONTROL: given the focused key row back, the rows it shows.
    let wide = frame("2.5.2-fullscreen-box-shell-feedback-wide");
    let feedback = "Type feedback \u{b7} \u{21b5} submit.";
    assert!(
        wide.iter().any(|r| r.trim() == feedback)
            && wide.iter().filter(|r| r.ends_with('┃')).count() == 2,
        "PRECONDITION: the feedback line's key row, a thumb alone"
    );
    let p = prompt(&wide).expect("the feedback line's box");
    assert_eq!(
        (p.kind, p.command.as_str(), p.readings().len()),
        (PromptKind::Bash, "", 0)
    );
    assert_eq!(p.footer.as_deref(), Some(feedback));
    let mut refocused = unbarred(&lowered(&wide));
    assert_eq!(
        read(&refocused),
        Some(Vec::new()),
        "the key row withholds it"
    );
    let keys = refocused
        .iter()
        .position(|r| r.trim() == feedback)
        .expect("the feedback line's key row");
    refocused[keys] = focused.to_string();
    assert_eq!(
        read(&refocused),
        Some(
            ["cat 说明文档.md", "ls 项目目录/"]
                .map(String::from)
                .to_vec()
        ),
        "CONTROL: the rows it shows"
    );

    // A body drawn whole is unproven under any key row but the focused one drawn to its end: the
    // feedback line's, an unfocused panel's, one cut at the box's edge.
    let fit = frame("2.5.2-fullscreen-box-shell");
    let at = fit
        .iter()
        .position(|r| r.trim_start().starts_with("\u{2191}/\u{2193} select"))
        .expect("PRECONDITION: the key row");
    for row in [
        format!("   {feedback}"),
        "   esc back to approval".to_string(),
        "   \u{2191}/\u{2193} select \u{b7} 1/2/3/4 choose \u{b7} \u{21b5} con...".to_string(),
    ] {
        let mut rows = fit.clone();
        rows[at] = row.clone();
        assert_eq!(read(&rows), Some(Vec::new()), "{row:?}");
    }

    for (name, path_drawn) in [
        ("2.5.2-fullscreen-box-write", true),
        ("2.5.2-fullscreen-box-write-scrolled", false),
    ] {
        let rows = frame(name);
        assert_eq!(
            rows.iter().any(|r| r
                .trim_start()
                .starts_with("/private/tmp/at.HOk4/proj/notes-for-the-lab.txt ")),
            path_drawn,
            "PRECONDITION: {name}'s path row"
        );
        let write = prompt(&rows).expect(name);
        assert_eq!(
            (write.kind, write.path.as_deref(), write.command.as_str()),
            (PromptKind::Write, None, ""),
            "{name}"
        );
        assert_eq!(options(&write), approvals, "{name}");
        assert_eq!(
            write.footer.as_deref(),
            Some("↑/↓ select · 1/2/3/4 choose · ↵ confirm · ctrl+e preview · shift+↑/↓ scroll"),
            "{name}"
        );
    }

    let question = prompt(&frame("2.5.2-fullscreen-question")).expect("the question");
    assert_eq!(question.kind, PromptKind::Question);
    assert_eq!(
        question.description,
        "Which checks should the lab run before landing?"
    );
    assert_eq!(
        options(&question),
        vec![
            (Some(1), "Format", false),
            (Some(2), "Lint", false),
            (Some(3), "Tests", false),
            (Some(4), "Cells", false),
            (Some(5), "Other", false),
        ]
    );
    assert_eq!(question.select, Select::Digits);
    assert_eq!(
        question.footer.as_deref(),
        Some("↑↓ select  1-5 / ↵ toggle  ←/→/tab switch  esc cancel")
    );
}

/// EACH CLAUSE OF THE SPINNER AND THE RESUME NOTICE IS ITS OWN: a moon phase with a word glued to
/// it, a braille glyph before another word than `Working…` (2.5's compaction draws `Compacting…`
/// there, read from 2.5.3's code), and a row that quotes the resume notice after words of its own
/// each decide nothing (respelled from the measured 2.4.0 frames). CONTROLS: the measured frames
/// themselves decide busy, busy and idle. NEGATIVE CONTROLS for the mutants that take any moon
/// phase, any braille glyph, or a notice anywhere on the run's last row.
#[test]
fn each_clause_of_the_spinner_and_the_resume_notice_is_its_own() {
    for (name, from, to, want) in [
        ("2.4.0-busy-held", "🌓", "🌓·", Phase::Busy),
        (
            "2.4.0-busy-streaming",
            "Working\u{2026}",
            "Compacting\u{2026}",
            Phase::Busy,
        ),
        (
            "2.4.0-resumed-picked",
            "Resumed session (",
            "Quoted: Resumed session (",
            Phase::Idle,
        ),
    ] {
        let measured = frame(name);
        assert_eq!(phase(&measured), (want, true), "CONTROL: {name}");
        let respelled: Vec<String> = measured.iter().map(|r| r.replace(from, to)).collect();
        assert_ne!(respelled, measured, "PRECONDITION: {name} respelled");
        assert_eq!(phase(&respelled), (Phase::Idle, false), "{name}: {to}");
    }
}

/// A RUN WHOSE START IS NOT ON THE SCREEN DECIDES NOTHING: the measured busy frame cut at its
/// composer's top border (a pane as tall as the composer and its footer) shows no blank row over
/// the border, so it reads nothing, never the idle an empty run would; nor does the frame cut at
/// its spinner row, whose run may start above the screen. CONTROL: the same frame cut at the blank
/// row over its spinner is busy. NEGATIVE CONTROL for the mutant that takes an empty slice over
/// the border for the blank row a turn's end leaves.
#[test]
fn a_run_whose_start_is_not_on_the_screen_decides_nothing() {
    let held = frame("2.4.0-busy-held");
    assert_eq!(phase(&held), (Phase::Busy, true), "PRECONDITION");
    let (top, _, _) = composer_rows(&held).expect("the composer");
    assert_eq!(
        phase(&held[top..]),
        (Phase::Idle, false),
        "cut at the border"
    );
    assert_eq!(
        phase(&held[top - 1..]),
        (Phase::Idle, false),
        "cut at the spinner"
    );
    assert_eq!(
        phase(&held[top - 2..]),
        (Phase::Busy, true),
        "CONTROL: the blank row seen"
    );
}

/// A SPINNER OVER THE TRANSCRIPT KEEPS THE BLANK ROW OVER THE COMPOSER FROM READING IDLE: 2.5's
/// agent swarm draws its turn's moon spinner in its own panel's status row, not over the composer,
/// while it waits or runs a tool (`shouldPlaceActivitySpinnerInAgentSwarm`, `renderActivityPrefix`
/// and `renderStatusLabel` read from 2.5.3's code; the rows HAND-BUILT, the panel unmeasured), so
/// an ended turn's frame with that panel spinning over its blank row decides nothing. CONTROL: the
/// same panel done (` ✓ Completed.`, its status row once the call ends) leaves the frame idle.
/// NEGATIVE CONTROL for the mutant that reads the blank row idle whatever spins above it.
#[test]
fn a_spinner_over_the_transcript_keeps_the_blank_row_from_reading_idle() {
    let ended = frame("2.4.0-end-of-turn");
    assert_eq!(phase(&ended), (Phase::Idle, true), "PRECONDITION");
    let reply = ended
        .iter()
        .position(|r| r.contains("● lab held reply"))
        .expect("the reply");
    let with_panel = |status: &str| -> Vec<String> {
        let mut rows = ended.clone();
        rows.splice(
            reply + 1..reply + 1,
            [
                String::new(),
                "   \u{2500}\u{2500} Agent Swarm \u{2014} lab subagents \u{2014} lab-model \u{2500}\u{2500}"
                    .to_string(),
                format!("   {status}"),
                "   lab-1 \u{2500}\u{2500} lab-2 \u{2500}\u{2500} lab-3".to_string(),
            ],
        );
        rows
    };
    let spinning = with_panel("🌓 Working\u{2026}  \u{2588}\u{2588}\u{2591}\u{2591}");
    assert_eq!(phase(&spinning), (Phase::Idle, false));
    assert!(!read(Some("kimi"), &spinning, None).phase_authoritative);
    let done = with_panel("\u{2713} Completed.  \u{2588}\u{2588}\u{2588}\u{2588}");
    assert_eq!(phase(&done), (Phase::Idle, true), "CONTROL: the swarm done");
}
