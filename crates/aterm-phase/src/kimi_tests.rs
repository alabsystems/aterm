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
        footer_only, 8,
        "the approval, question and session-picker frames with no composer"
    );
    assert!(unclaimed > 0, "frames with nothing to tell them by stand");
    assert!(others > 1000, "{others} other programs' frames");
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
    ("2.5.2-box-write", Phase::Prompt, true),
];

/// The screen of the Kimi Code fixture `name` (`kimi-<name>.txt`).
fn frame(name: &str) -> Vec<String> {
    let path = aterm_tempfile::manifest_dir().join(format!("src/fixtures/kimi-{name}.txt"));
    screen(&std::fs::read_to_string(&path).expect("the fixture"))
}

/// EVERY KIMI CODE FRAME READS THE PHASE ITS OWN ROWS DECIDE (measured on 2.3.1 and 2.4.0 at 110
/// columns, and 2.5.2's file approvals at 144): a running turn's frames (held before the first byte with a moon phase alone or a
/// tip, streaming under `Working…`, a draft typed or cleared under it, a message queued under
/// it, a 429 being retried) are busy by the spinner row opening the run of rows over the
/// composer's top border; every frame a turn ended on (a reply, an interrupt, an error, a
/// resume's notice, a plan mode switched on, a slash menu or the exit hint over the idle
/// composer) is idle by the blank row there. A command's approval, a file's, a plan's, a question
/// (its review step too) and the trust prompt drawn in the composer's place are prompts, each read as
/// its box, and no other frame reads a box; the session picker, the person's own, and the frames
/// of an exit decide nothing. Display only: neither program is supervised.
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
    assert!(!Program::Kimi.supervisable(), "display only");
}

/// THE EXIT HINT A FIRST CTRL-C ARMS STANDS AT THE FOOTER'S LEFT AND HIDES NO COMPOSER: the
/// `context: N%` footer still reads past it, so the idle composer, its draft and its context are
/// read. CONTROL: the hint with no context after it is no footer, and the composer it would
/// stand under reads nothing.
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
/// moon phase, standing in an ended turn's transcript over its blank row, is no running turn;
/// and a running turn whose spinner the build no longer draws (its moon phase respelled) decides
/// nothing, never an idle. NEGATIVE CONTROL for the mutants that read a moon phase off any row
/// of the screen and that take a run with no spinner for an ended turn.
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
    assert_eq!(phase(&ended), (Phase::Idle, true));
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
/// a command's approval is a Bash box whose command is its `$ ` row (the read-only rule reads
/// it) and whose description is the row under it; a plan's approval names its plans; a question
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
    assert_eq!(shell.readings(), vec!["echo lab-kimi-tool"]);
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

    for p in [&shell, &plan, &question, &review, &write, &edit, &trust] {
        assert_eq!(p.cancel, None, "{:?}", p.kind);
        assert!(
            p.options.iter().all(|o| o.role == Role::Other),
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
/// read-only rule sees `rm` on a line of its own; a wrap mid-block joins its line; a last row
/// that may be a wrap is read both with and without it. NEGATIVE CONTROL for the mutant that
/// reads the `$ ` row alone, which reads `cat README.md` and a read-only verdict for a box that
/// deletes.
#[test]
fn an_approval_reads_every_line_of_its_command() {
    let measured = prompt(&frame("2.4.0-box-shell")).expect("PRECONDITION");
    assert_eq!(
        (measured.description_rows, measured.readings()),
        (0, vec!["echo lab-kimi-tool".to_string()]),
        "the measured description fits beside its command: no wrap"
    );

    let lines = approval_with(&["   $ cat README.md", "   · rm -rf build"], true);
    let p = prompt(&lines).expect("the approval");
    assert_eq!(p.command_rows, vec!["cat README.md", "rm -rf build"]);
    assert_eq!(p.description, "print a lab marker");
    assert!(
        p.readings()
            .contains(&"cat README.md\nrm -rf build".to_string())
    );
    assert_eq!(p.options.len(), 4, "the options stay");

    let long = format!(
        "cat {}",
        (1..=6)
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
        vec![format!("{long} && rm -rf /private/tmp/at.H2aa/proj")],
        "a deeper row with a row under it is a wrap"
    );
    assert_eq!(
        (p.description.as_str(), p.description_rows),
        ("print a lab marker", 0)
    );

    let full = format!("cat {}", "x".repeat(102 - "cat ".len()));
    let edge = format!("   $ {full}");
    let unsure = approval_with(&[&edge, "     rm -rf build"], false);
    let p = prompt(&unsure).expect("the approval");
    assert_eq!(
        p.description_rows, 1,
        "the last row may be a wrap or the description"
    );
    let readings = p.readings();
    assert!(readings.contains(&full), "{readings:?}");
    assert!(
        readings.contains(&format!("{full} rm -rf build")),
        "{readings:?}"
    );
    assert!(
        readings.contains(&format!("{full}\nrm -rf build")),
        "{readings:?}"
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

/// The column a measured box's rules start at and the cells they span.
fn edges(rows: &[String]) -> (usize, usize) {
    let r = rows.iter().find(|r| rule(r)).expect("a rule");
    (
        crate::phase::leading_spaces(r),
        crate::text::trim(r).chars().count(),
    )
}

/// A FILE'S BOX NAMES ITS FILE ONLY WHEN IT IS DRAWN WHOLE, AND A SCROLLBAR IS NO PART OF A ROW
/// (2.5.2's `renderBody` and `paintScrollbar`, read in its binary): the measured write with its
/// path cut at the box's edge by `...` reads that text as its command and no path; the
/// measured write, edit and command with a fullscreen panel's scrollbar painted at the box's
/// right edge (`┃`, `│`) read as they do without it. NEGATIVE CONTROL for the mutants that take
/// the cut text for the path, or the scrollbar's glyph into a path or a command.
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

    let barred = |rows: &[String], glyph: char| -> Vec<String> {
        let (edge, width) = edges(rows);
        let title = rows
            .iter()
            .position(|r| r.contains('▶'))
            .expect("the title");
        let first = rows
            .iter()
            .position(|r| r.trim_start().starts_with("▶ 1."))
            .expect("the first option");
        rows.iter()
            .enumerate()
            .map(|(i, r)| {
                if i > title && i < first && !r.trim().is_empty() {
                    format!("{r:<0$}{glyph}", edge + width - 1)
                } else {
                    r.clone()
                }
            })
            .collect()
    };
    for (name, glyph) in [("2.5.2-box-write", '┃'), ("2.5.2-box-edit", '│')] {
        let p = prompt(&barred(&frame(name), glyph)).expect(name);
        assert_eq!(
            (p.path.as_deref(), p.command.as_str()),
            (Some(path), path),
            "{name}"
        );
    }
    let shell = prompt(&barred(&frame("2.4.0-box-shell"), '│')).expect("the command");
    assert_eq!(
        (shell.command.as_str(), shell.description.as_str()),
        ("echo lab-kimi-tool", "print a lab marker")
    );
    assert_eq!(shell.readings(), vec!["echo lab-kimi-tool"]);
}
