// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::codex::fixtures::{codex_0_155_trust, working as codex_working};
use crate::prompt::PromptKind;
use crate::prompt::fixtures::{
    API_ERROR_ENOTFOUND, API_RETRYING_DRAFT_MEASURED, API_RETRYING_MEASURED,
    API_RETRYING_QUEUED_80_MEASURED, API_RETRYING_QUEUED_DRAFT_MEASURED,
    API_RETRYING_QUEUED_MEASURED, BOX_BASH_TOUCH, END_529, GOAL_ACTIVE_SUGGESTION, LOGIN_HANDOFF,
    LOGIN_PICKER_WARNED, LOGIN_SUCCESS, MEMORY_BANNER_BUSY, MEMORY_BANNER_IDLE, TRUST,
    bash_one_row, composer, rows, screen,
};
use crate::wall::WallKind;

/// [`ScreenReader::message_drawn`] detects a drawn message with or without words under it.
#[test]
fn a_message_on_the_screen_is_drawn_and_a_launch_holds_none() {
    use crate::prompt::fixtures::{first_turn_under_a_table, launch_under_a_table};
    const END_OF_TURN_ANY: &str = "$ make\nok\n$ ";
    let drawn = |p: Program, rows: &[String]| p.reader().message_drawn(rows);
    {
        use crate::agy::fixtures::{END_OF_TURN, IDLE};
        assert!(crate::agy::fresh(&screen(IDLE)));
        assert!(!drawn(Program::Agy, &screen(IDLE)));
        assert!(drawn(Program::Agy, &screen(END_OF_TURN)));
    }
    {
        use crate::muse::fixtures::{END_OF_TURN, END_OF_TURN_RAN_COMMAND, IDLE, IDLE_RELAUNCHED};
        assert!(!drawn(Program::Muse, &vec![String::new(); 50]));
        assert!(!drawn(Program::Muse, &screen(IDLE)));
        assert!(!drawn(Program::Muse, &screen(IDLE_RELAUNCHED)));
        assert!(drawn(Program::Muse, &screen(END_OF_TURN)));
        assert!(drawn(Program::Muse, &screen(END_OF_TURN_RAN_COMMAND)));
    }
    let launch = launch_under_a_table("❯ Try \"refactor <filepath>\"", "  ? for shortcuts");
    assert!(!drawn(Program::Claude, &launch), "{launch:#?}");
    assert!(drawn(
        Program::Claude,
        &first_turn_under_a_table(&["❯ then add tests"])
    ));
    assert!(drawn(Program::Generic, &screen(END_OF_TURN_ANY)));
    assert!(!drawn(Program::Generic, &vec![String::new(); 50]));
    assert!(!drawn(Program::Generic, &[]));
}

/// A blank screen or the shell's echo of the launch line is none of the program's own
/// ([`ScreenReader::program_drawn`]), while its frames, boxes and launch card are; a program
/// no reader knows is credited with any screen.
#[test]
fn a_launch_screen_before_the_program_draws_is_none_of_its_own() {
    use crate::muse::fixtures::{FIRST_REPLY_UNDER_LAUNCH_LINE, LAUNCH_LINE};
    let drawn = |p: Program, rows: &[String]| p.reader().program_drawn(rows);
    let blank = vec![String::new(); 50];
    let launch = screen(LAUNCH_LINE);
    let agy_launch = screen(crate::agy::fixtures::LAUNCH_LINE);
    let mut card = launch.clone();
    card.extend(["", "  Muse Code 1.4.12"].map(str::to_string));
    for p in [Program::Claude, Program::Agy, Program::Muse] {
        assert!(!drawn(p, &blank), "{p:?}: a blank screen");
        assert!(!drawn(p, &launch), "{p:?}: Muse Code's launch line");
        assert!(!drawn(p, &agy_launch), "{p:?}: agy's launch line");
        assert!(!drawn(p, &[]), "{p:?}: no rows");
    }
    for p in [Program::Codex, Program::Generic] {
        assert!(drawn(p, &blank) && drawn(p, &launch), "{p:?}");
    }
    {
        use crate::muse::fixtures::{BOX_SHELL, BOX_TRUST, BUSY, END_OF_TURN, IDLE, SIGNED_OUT};
        for (what, rows) in [
            ("idle", screen(IDLE)),
            ("busy", screen(BUSY)),
            ("its reply", screen(END_OF_TURN)),
            ("a box", screen(BOX_SHELL)),
            ("its trust box", screen(BOX_TRUST)),
            ("signed out", screen(SIGNED_OUT)),
            ("its launch card", card.clone()),
            (
                "its reply under the launch line",
                screen(FIRST_REPLY_UNDER_LAUNCH_LINE),
            ),
        ] {
            assert!(drawn(Program::Muse, &rows), "muse: {what}: {rows:#?}");
        }
    }
    {
        use crate::agy::fixtures::{BUSY, END_OF_TURN, IDLE, IDLE_FRESH_NOTICE, lab};
        for rows in [IDLE, BUSY, END_OF_TURN, IDLE_FRESH_NOTICE, lab::BOX_SHELL].map(screen) {
            assert!(drawn(Program::Agy, &rows), "agy: {rows:#?}");
        }
        let queried = screen(IDLE_FRESH_NOTICE)[..2].to_vec();
        assert!(
            !drawn(Program::Agy, &queried),
            "the launch line and the shell's echo of agy's queries: {queried:#?}"
        );
    }
    {
        use crate::prompt::fixtures::{first_turn_under_a_table, launch_under_a_table};
        let launch = launch_under_a_table("❯ Try \"refactor <filepath>\"", "  ? for shortcuts");
        for rows in [
            launch,
            first_turn_under_a_table(&["❯ then add tests"]),
            screen(TRUST),
            screen(BOX_BASH_TOUCH),
            screen(LOGIN_HANDOFF),
        ] {
            assert!(drawn(Program::Claude, &rows), "claude: {rows:#?}");
        }
    }
}

/// [`shell_alone`]: a blank screen, no rows and the measured launch lines could be the shell's;
/// a usage panel, a reply under a launch line, or a prompt a blank row splits from a later row
/// are some program's.
#[test]
fn the_shell_alone_leaves_one_unbroken_run() {
    use crate::muse::fixtures::{FIRST_REPLY_UNDER_LAUNCH_LINE, LAUNCH_LINE};
    for (what, rows) in [
        ("blank", vec![String::new(); 50]),
        ("no rows", vec![]),
        ("muse's launch line", screen(LAUNCH_LINE)),
        (
            "agy's launch line",
            screen(crate::agy::fixtures::LAUNCH_LINE),
        ),
        (
            "a prompt alone, under blank rows",
            rows(&["", "", "lab$ ", "", ""]),
        ),
    ] {
        assert!(shell_alone(&rows), "{what}: {rows:#?}");
    }
    for (what, rows) in [
        (
            "claude's usage panel",
            screen(crate::prompt::fixtures::USAGE_REFRESHING),
        ),
        (
            "muse's reply under its launch line",
            screen(FIRST_REPLY_UNDER_LAUNCH_LINE),
        ),
        (
            "one blank row inside",
            rows(&["lab$ agy", "", "  agy 1.4.6"]),
        ),
    ] {
        assert!(!shell_alone(&rows), "{what}: {rows:#?}");
    }
}

fn shell_asking() -> Vec<String> {
    rows(&[
        "% ./configure",
        "checking for gcc... gcc",
        "Overwrite the existing config?",
    ])
}

/// A shell's or a viewer's name reads as no agent's, without case, from a path and past a
/// login shell's `-` (the name table's own reading, aterm-agent-names), and a runtime's leaves
/// the screen to say: the frame here is Claude Code's.
#[test]
fn a_shell_or_viewer_is_no_agent_and_a_runtime_is_read_by_its_screen() {
    let rows = composer("");
    assert_eq!(identify(Some("-ZSH"), &rows).program(), Program::Generic);
    assert_eq!(
        identify(Some("/bin/Less"), &rows).program(),
        Program::Generic
    );
    assert_eq!(identify(Some("node"), &rows).program(), Program::Claude);
}

/// [`Program::ALL`] is every variant by construction (`next_program` and
/// the compile-time walk beside it); each name is its own and
/// [`program_of`] reads it back.
#[test]
fn every_programs_name_reads_back() {
    for p in Program::ALL {
        let named = program_of(p.name());
        if p == Program::Generic {
            assert_eq!(named, None);
        } else {
            assert_eq!(named, Some(p), "{p:?}");
        }
    }
}

/// The program name decides first: an agent's names its reader, and a shell's or a viewer's
/// the generic one. With no name or any other, the screen does: Claude Code's frame, one of
/// its boxes or its `/login` dialog, else the generic reader.
#[test]
fn identify_takes_the_program_name_then_the_screen() {
    let framed = {
        let mut r = rows(&["⏺ Done.", ""]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let p = |name: Option<&str>, r: &[String]| identify(name, r).program();
    assert_eq!(p(Some("claude"), &shell_asking()), Program::Claude);
    assert_eq!(
        p(Some("/opt/bin/codex --full-auto"), &framed),
        Program::Codex
    );
    assert_eq!(p(Some("agy"), &framed), Program::Agy);
    for retired in [
        "/opt/homebrew/bin/gemini --resume x",
        "gemini-cli",
        "-GEMINI",
    ] {
        assert!(retired_agent(retired), "{retired}");
        assert_eq!(program_of(retired), None, "{retired}");
        assert_eq!(
            p(Some(retired), &framed),
            Program::Generic,
            "Gemini CLI is retired: its frame is read as no agent's, never Claude Code's"
        );
    }
    for live in ["agy", "/usr/local/bin/geminis", "node", "claude"] {
        assert!(!retired_agent(live), "{live}");
    }
    assert_eq!(
        p(Some("/usr/local/bin/muse resume x"), &framed),
        Program::Muse
    );
    assert_eq!(p(Some("-zsh"), &framed), Program::Generic);
    assert_eq!(p(Some("bash"), &bash_one_row()), Program::Generic);
    assert_eq!(p(None, &framed), Program::Claude);
    assert_eq!(p(Some("node"), &framed), Program::Claude);
    assert_eq!(p(None, &screen(TRUST)), Program::Claude, "a box, no frame");
    assert_eq!(p(None, &screen(BOX_BASH_TOUCH)), Program::Claude);
    for login in [LOGIN_HANDOFF, LOGIN_PICKER_WARNED, LOGIN_SUCCESS] {
        assert_eq!(p(None, &screen(login)), Program::Claude, "{login}");
    }
    let unknown: Vec<String> = [
        "$ ./sign-in.sh",
        "",
        &"─".repeat(100),
        "  Login",
        "",
        "  Enter your user name:",
        "",
        "  Esc to cancel",
    ]
    .iter()
    .map(|r| (*r).to_string())
    .collect();
    assert_eq!(p(None, &unknown), Program::Generic);
    assert_eq!(p(Some("bash"), &screen(LOGIN_HANDOFF)), Program::Generic);
    assert_eq!(p(None, &codex_0_155_trust()), Program::Generic);
    assert_eq!(p(Some("codex"), &codex_0_155_trust()), Program::Codex);
    assert_eq!(p(None, &shell_asking()), Program::Generic);
    assert_eq!(p(Some("python3"), &shell_asking()), Program::Generic);
    for viewer in ["less", "/usr/bin/vim", "tail -f log", "watch"] {
        assert_eq!(
            p(Some(viewer), &bash_one_row()),
            Program::Generic,
            "{viewer}"
        );
    }
    assert_eq!(
        p(Some("ssh"), &bash_one_row()),
        Program::Claude,
        "the control"
    );
}

/// The sniff's order over every capture: read with no program name, each agent's capture reads
/// as its own program or as none, every agent is named at least once, and Codex never is.
#[test]
fn every_capture_is_identified_as_its_own_program_or_as_none() {
    let mut named = [0usize; Program::ALL.len()];
    for (name, text) in crate::text::fixture_files() {
        let rows = screen(&text);
        let want = match name.split('-').next() {
            Some("agy") => Program::Agy,
            Some("muse") => Program::Muse,
            Some("grok") => Program::Grok,
            Some("kimi") => Program::Kimi,
            _ => Program::Claude,
        };
        let got = identify(None, &rows).program();
        assert!(
            got == want || got == Program::Generic,
            "{name}: read as {got:?}"
        );
        named[got as usize] += 1;
    }
    for agent in [
        Program::Claude,
        Program::Agy,
        Program::Muse,
        Program::Grok,
        Program::Kimi,
    ] {
        assert!(
            named[agent as usize] > 0,
            "every agent named at least once: {named:?}"
        );
    }
    assert_eq!(named[Program::Codex as usize], 0, "{named:?}");
}

/// A reader's default is not evidence: the generic and Codex readers' `idle` is not
/// authoritative; Claude Code's at its composer is (with no composer:
/// [`claude_codes_idle_is_read_at_its_composer`]).
#[test]
fn a_default_idle_is_not_authoritative() {
    let working = codex_working();
    for r in [working, codex_0_155_trust()] {
        let r = read(Some("codex"), &r, None);
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, false));
    }
    let g = read(Some("zsh"), &shell_asking(), None);
    assert_eq!(g.phase, Phase::Idle);
    assert!(!g.phase_authoritative);
    let c = read(Some("claude"), &screen(END_529), None);
    assert_eq!(c.phase, Phase::Idle);
    assert!(c.phase_authoritative, "the control");
}

/// The generic reader never says prompt or question, even on a shell screen ending in `?`.
#[test]
fn the_generic_reader_is_never_a_question_or_a_prompt() {
    let r = shell_asking();
    assert_eq!(ClaudeReader.phase(&r), Phase::Question, "the control");
    assert_eq!(GenericReader.phase(&r), Phase::Idle);
    let reading = read(Some("zsh"), &bash_one_row(), Some(2));
    assert_eq!(reading.program, Program::Generic);
    assert_eq!(reading.phase, Phase::Idle);
    assert_eq!(reading.prompt, None);
    assert_eq!(reading.wall, None);
    assert_eq!(
        ClaudeReader.phase(&bash_one_row()),
        Phase::Prompt,
        "the control"
    );
}

/// The Codex screen reader reports an inert reading with no prompt, wall, composer, or guards.
#[test]
fn the_codex_reader_reads_nothing() {
    let working = codex_working();
    let codex = Program::Codex.reader();
    assert_eq!(codex.program(), Program::Codex);
    for r in [codex_0_155_trust(), working, shell_asking()] {
        let reading = read(Some("codex"), &r, None);
        assert_eq!(reading.phase, Phase::Idle);
        assert!(!reading.phase_authoritative);
        assert_eq!(
            (reading.prompt, reading.wall, reading.composer),
            (None, None, None)
        );
        assert!(!reading.interrupted && !reading.goal_active && !reading.fresh);
        assert_eq!(codex.prompt(&r), None);
        assert_eq!(codex.background_wait(&r), None);
    }
    assert_eq!(
        (codex.caret(), codex.busy_guard(), codex.paste_guard()),
        (None, None, false)
    );
    let claude = read(Some("claude"), &screen(TRUST), None);
    assert_eq!(claude.phase, Phase::Prompt, "the control");
    assert!(claude.prompt.is_some(), "the control");
    assert!(
        Program::Claude.reader().busy_guard().is_some(),
        "the control"
    );
}

/// What a supervisor typing into a program needs of its reader: the composer (Claude Code's
/// only inside its frame) and the row whose leaving ends a turn.
#[test]
fn each_reader_says_how_to_type_into_its_program() {
    let claude = Program::Claude.reader();
    assert_eq!(claude.program(), Program::Claude);
    assert_eq!(Program::Generic.reader().program(), Program::Generic);
    for p in Program::ALL {
        assert_eq!(p.reader().program(), p);
    }
    let idle = {
        let mut r = rows(&["⏺ Done.", ""]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    assert!(claude.composer(&idle).is_some(), "the framed composer");
    assert_eq!(
        claude.composer(&screen(BOX_BASH_TOUCH)),
        None,
        "a box replaces the frame: no composer"
    );
    let guard = claude.busy_guard().expect("claude busy guard");
    let re = aterm_regex::Regex::new(&guard).expect("busy_guard compiles as regex");
    assert!(re.is_match("  esc to interrupt"));
    assert!(re.is_match("  esc\u{00a0}to\u{00a0}interrupt"));
    assert!(idle.iter().all(|row| !re.is_match(row)));
    assert!(!re.is_match("  esc to "));
    let g = GenericReader;
    assert_eq!(
        (g.caret(), g.busy_guard(), g.paste_guard()),
        (None, None, false)
    );
}

/// One read, whole: the 529 end of turn is idle with its wall; the measured `/goal` screen is
/// busy, carrying the armed goal and Claude Code's suggestion, and no wall.
#[test]
fn a_reading_carries_the_wall_and_the_turn_end_facts() {
    let r = read(None, &screen(END_529), Some(2));
    assert_eq!(r.program, Program::Claude);
    assert_eq!(r.phase, Phase::Idle);
    assert_eq!(r.wall.map(|w| w.kind), Some(WallKind::Overloaded));
    assert_eq!(
        r.said_tail.as_deref(),
        Some("Suites are running; I'll report the number when it lands.")
    );

    let g = read(Some("claude"), &screen(GOAL_ACTIVE_SUGGESTION), Some(2));
    assert_eq!(g.phase, Phase::Busy);
    assert!(g.goal_active);
    assert_eq!(g.suggestion.as_deref(), Some("keep going"));
    assert_eq!(g.wall, None);
    assert_eq!(
        read(Some("claude"), &screen(GOAL_ACTIVE_SUGGESTION), None).suggestion,
        None
    );

    let b = read(Some("claude"), &screen(TRUST), None);
    assert_eq!(b.phase, Phase::Prompt);
    assert_eq!(b.prompt.map(|p| p.kind), Some(PromptKind::Trust));
    assert_eq!(b.wall, None);

    let e = read(Some("claude"), &screen(API_ERROR_ENOTFOUND), Some(3));
    assert_eq!(e.phase, Phase::Idle);
    assert_eq!(
        e.wall.map(|w| (w.kind, w.placement)),
        Some((
            WallKind::ApiError {
                code: None,
                retryable: true,
                cause: crate::wall::ApiCause::Unreachable,
            },
            crate::wall::Placement::ErrorRow,
        ))
    );
    let mut retrying = screen(API_ERROR_ENOTFOUND);
    let done = retrying
        .iter()
        .position(|r| r.starts_with("✻ Worked"))
        .expect("the done row");
    retrying[done] =
        "✻ Can't reach the API server · Retrying in 2m · attempt 7/10 (esc to interrupt)"
            .to_string();
    let last = retrying.len() - 1;
    retrying[last] =
        "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt".to_string();
    let busy = read(Some("claude"), &retrying, None);
    assert_eq!(busy.phase, Phase::Busy);
    assert_eq!(busy.wall, None);
    for (name, text) in [
        ("retrying", API_RETRYING_MEASURED),
        ("queued", API_RETRYING_QUEUED_MEASURED),
        ("queued 80", API_RETRYING_QUEUED_80_MEASURED),
        ("draft", API_RETRYING_DRAFT_MEASURED),
        ("queued draft", API_RETRYING_QUEUED_DRAFT_MEASURED),
    ] {
        let r = read(Some("claude"), &screen(text), None);
        assert_eq!((r.phase, r.wall), (Phase::Busy, None), "{name}");
    }
}

/// The reading carries the context the screen shows, the one reading the supervisor's watch
/// takes too ([`ScreenReader::context`]): the indicator at the end of Claude Code's mode row
/// (2.1.281 and later, every build measured) or above the composer (2.1.267/2.1.268).
#[test]
fn a_reading_carries_the_context_the_screen_shows() {
    let mut on_mode_row = rows(&["⏺ Done.", ""]);
    on_mode_row.extend(composer(&crate::prompt::fixtures::right_aligned(
        "  ⏸ manual mode on · ? for shortcuts",
        "0% until auto-compact",
    )));
    let mut above = rows(&["⏺ Done.", "", &format!("{:>120}", "7% until auto-compact")]);
    above.extend(composer("  ? for shortcuts"));
    let mut none = rows(&["⏺ Done.", ""]);
    none.extend(composer("  ? for shortcuts"));
    let claude = Program::Claude.reader();
    for (screen, left) in [(on_mode_row, Some(0)), (above, Some(7)), (none, None)] {
        assert_eq!(read(Some("claude"), &screen, None).context_left, left);
        assert_eq!(claude.context(&screen), left, "the watch's reading");
    }
}

/// Claude Code's watchdog usage-limit retry row reads as [`Phase::Busy`] with no wall.
#[test]
fn the_watchdogs_retry_row_is_a_turn_in_progress() {
    use crate::phase::{Zone, background_only, transcript_end};
    use crate::prompt::fixtures::{
        WALL_WATCHDOG_FULLSCREEN_2_1_301, WALL_WATCHDOG_MIDTURN_2_1_301,
        WALL_WATCHDOG_SESSION_2_1_300, WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300,
        WALL_WATCHDOG_WEEKLY_2_1_300, WALL_WATCHDOG_WEEKLY_50_2_1_300,
        WALL_WATCHDOG_WEEKLY_80_2_1_300,
    };
    let shell = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .map(|r| {
                r.replace(
                    "auto mode on (shift+tab to cycle)",
                    "auto mode on · 1 shell",
                )
            })
            .collect()
    };
    let tick = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .map(|r| {
                r.replace("Retrying in 2m 28s", "Retrying in 2m 27s")
                    .replace("Retrying in 59s", "Retrying in 58s")
                    .replace("Retrying in 3m 56s", "Retrying in 3m 55s")
                    .replace("Retrying in 14m", "Retrying in 13m 59s")
                    .replace("Retrying in 5h", "Retrying in 4h 59m")
            })
            .collect()
    };
    let old = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .map(|r| {
                r.replace("attempt 1/300", "attempt one of 300")
                    .replace("li…", "li")
            })
            .collect()
    };
    for (name, text, footer_busy) in [
        ("session", WALL_WATCHDOG_SESSION_2_1_300, true),
        (
            "session statusline",
            WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300,
            false,
        ),
        ("fullscreen", WALL_WATCHDOG_FULLSCREEN_2_1_301, false),
        ("weekly", WALL_WATCHDOG_WEEKLY_2_1_300, false),
        ("weekly 80", WALL_WATCHDOG_WEEKLY_80_2_1_300, false),
        ("weekly 50", WALL_WATCHDOG_WEEKLY_50_2_1_300, false),
        ("mid-turn", WALL_WATCHDOG_MIDTURN_2_1_301, true),
    ] {
        let rows = screen(text);
        for rows in [rows.clone(), tick(&rows)] {
            let r = read(Some("claude"), &rows, None);
            assert_eq!((&r.phase, &r.wall), (&Phase::Busy, &None), "{name}");
            let busy = busy_signal(&rows).expect(name);
            assert_eq!(
                (busy.zone, busy.rule, busy.soft),
                (Zone::Status, "spinner", false),
                "{name}"
            );
            assert!(!background_only(&shell(&rows)), "{name}: a live turn");
            let row = rows
                .iter()
                .position(|r| r.contains("Retrying in"))
                .expect("the retry row");
            assert!(transcript_end(&rows) <= row, "{name}: the live zone");
        }
        let done = old(&rows);
        let r = read(Some("claude"), &done, None);
        if footer_busy {
            assert_eq!(
                busy_signal(&done).map(|b| b.zone),
                Some(Zone::Footer),
                "{name}"
            );
        } else {
            assert_eq!(r.phase, Phase::Idle, "{name}: the old reading");
            assert!(
                background_only(&shell(&done)),
                "{name}: background work alone"
            );
        }
    }
    let rows = screen(WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300);
    let at = rows
        .iter()
        .position(|r| r.contains("Retrying in"))
        .expect("the retry row");
    let text = rows[at].trim_start_matches(|c: char| c != 'S').to_string();
    let with = |row: String| {
        let mut rows = rows.clone();
        rows[at] = row;
        rows
    };
    for cut in [
        rows[at].replace("attempt 1/300", "attempt 1/3…"),
        rows[at].replace("attempt 1/300", "att…"),
    ] {
        assert_eq!(
            read(Some("claude"), &with(cut.clone()), None).phase,
            Phase::Busy,
            "{cut}"
        );
    }
    for said in [
        format!("⏺ {text}"),
        format!("  ⎿  {text}"),
        format!("     {text}"),
    ] {
        assert_eq!(busy_signal(&with(said.clone())), None, "{said}");
    }
}

/// Claude Code's critical-memory banner is a memory wall under a running spinner (the one
/// wall a hard busy keeps, [`ScreenReader::health_wall`]) and at idle alike.
#[test]
fn the_memory_banner_is_a_wall_even_under_a_busy_spinner() {
    let busy = screen(MEMORY_BANNER_BUSY);
    for program in [Some("claude"), None] {
        let r = read(program, &busy, None);
        assert_eq!(r.program, Program::Claude);
        assert_eq!(r.phase, Phase::Busy, "{program:?}");
        assert!(busy_signal(&busy).is_some_and(|b| !b.soft), "hard busy");
        assert_eq!(
            r.wall.map(|w| w.kind),
            Some(WallKind::Memory),
            "{program:?}"
        );
    }
    let idle = read(Some("claude"), &screen(MEMORY_BANNER_IDLE), None);
    assert_eq!(idle.phase, Phase::Idle);
    assert_eq!(idle.wall.map(|w| w.kind), Some(WallKind::Memory));

    let at = memory_wall(&busy).expect("the banner row").row;
    let mut quoted = busy.clone();
    quoted[at] = String::new();
    let r = read(Some("claude"), &quoted, None);
    assert_eq!(r.phase, Phase::Busy);
    assert_eq!(r.wall, None, "only the draft's quote is left");

    let mut boxed = bash_one_row();
    let top = boxed
        .iter()
        .rposition(|row| row.starts_with('─'))
        .expect("rules")
        - 2;
    let width = boxed[top].chars().count();
    let banner = busy[at].trim_start();
    boxed.insert(top, format!("{banner:>w$}", w = width - 2));
    assert!(memory_wall(&boxed).is_some(), "the control");
    let r = read(Some("claude"), &boxed, None);
    assert_eq!(r.phase, Phase::Prompt);
    assert_eq!(r.wall, None);

    assert_eq!(read(Some("zsh"), &busy, None).wall, None);
    assert_eq!(CodexReader.health_wall(&busy), None);
    assert_eq!(GenericReader.health_wall(&busy), None);
    assert_eq!(ClaudeReader.health_wall(&busy).map(|w| w.row), Some(at));
}

/// Claude Code's `idle` is authoritative only with the REPL drawn whole: the launch frames
/// before it or with it half drawn, and a dialog with no reading, read a non-authoritative
/// `idle` with no composer.
#[test]
fn claude_codes_idle_is_read_at_its_composer() {
    use crate::prompt::fixtures::{
        AUTO_CLASSIFIER_BILLING_NOTICE_MEASURED, LAUNCH_BEFORE_REPL, LAUNCH_REPL_HALF_DRAWN,
        LAUNCH_REPL_READY,
    };
    for (name, text) in [
        ("before the REPL", LAUNCH_BEFORE_REPL),
        ("the REPL half drawn", LAUNCH_REPL_HALF_DRAWN),
        (
            "the classifier-billing notice",
            AUTO_CLASSIFIER_BILLING_NOTICE_MEASURED,
        ),
    ] {
        let r = read(Some("claude"), &screen(text), Some(2));
        assert_eq!(r.program, Program::Claude, "{name}");
        assert_eq!(r.phase, Phase::Idle, "{name}");
        assert!(!r.phase_authoritative, "{name}: {r:?}");
        assert_eq!((r.composer, r.wall), (None, None), "{name}");
        let rows = screen(text);
        let zone = &rows[live_zone_start(&rows, 40)..];
        assert!(
            !read(Some("claude"), zone, None).phase_authoritative,
            "{name}"
        );
    }
    let ready = read(Some("claude"), &screen(LAUNCH_REPL_READY), Some(2));
    assert_eq!(
        (ready.phase.clone(), ready.phase_authoritative),
        (Phase::Idle, true),
        "the control: {ready:?}"
    );
    assert!(ready.composer.is_some() && ready.fresh, "{ready:?}");
    let end = screen(END_529);
    let top = end
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the rule");
    assert_eq!(
        read(Some("claude"), &end[..top], None).wall.map(|w| w.kind),
        Some(WallKind::Overloaded),
        "the wall's reading, its composer cut"
    );
}

/// The tall pane (2.1.284 at 144x50, `API_ERROR_TALL_PANE_MEASURED`): a short
/// transcript with the composer pinned at the foot leaves 35 blank rows above it and
/// the error 41 rows above the last drawn row, outside the grid's last 40.
#[test]
fn a_long_blank_gap_does_not_cut_the_last_thing_said_out_of_the_zone() {
    use crate::prompt::fixtures::API_ERROR_TALL_PANE_MEASURED;
    let rows = screen(API_ERROR_TALL_PANE_MEASURED);
    let error = rows
        .iter()
        .position(|r| r.starts_with("⏺ API Error"))
        .expect("the error row");
    let cursor = rows
        .iter()
        .rposition(|r| r.starts_with('❯'))
        .expect("caret");
    let done = rows
        .iter()
        .position(|r| r.starts_with("✻ Worked for"))
        .expect("the done row");
    let top_rule = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the composer's top rule");
    assert_eq!(
        (rows.len(), error, done, top_rule, cursor),
        (50, 8, 10, 46, 47)
    );
    assert!(rows[done + 1..top_rule].iter().all(|r| r.trim().is_empty()));
    assert_eq!(top_rule - done - 1, 35, "35 blank rows");
    assert!(!rows[rows.len() - 1].trim().is_empty(), "drawn to the foot");
    assert_eq!(
        rows.len() - 1 - error,
        41,
        "41 rows above the last drawn row"
    );
    assert!(rows.len() - 40 > error, "the old cut lies below the error");
    let start = live_zone_start(&rows, 40);
    assert!(start <= error, "{start} > {error}");
    let zone = &rows[start..];
    let r = read_at(Some("claude"), zone, Some(cursor - start), None);
    assert!(r.phase_authoritative, "{r:?}");
    assert_eq!(
        r.wall.as_ref().map(|w| (w.kind, w.placement)),
        Some((
            crate::wall::WallKind::ApiError {
                code: None,
                retryable: true,
                cause: crate::wall::ApiCause::Unreachable
            },
            crate::wall::Placement::ErrorRow
        ))
    );
    let full: Vec<String> = (0..62)
        .map(|i| {
            if i % 2 == 0 {
                format!("row {i}")
            } else {
                String::new()
            }
        })
        .collect();
    assert_eq!(live_zone_start(&full, 40), 61 - 40);
    let mut sep: Vec<String> = (0..62).map(|i| format!("row {i}")).collect();
    for i in [30, 31, 32] {
        sep[i] = String::new();
    }
    assert_eq!(live_zone_start(&sep, 40), 62 - 40);
    sep[33] = String::new();
    assert_eq!(live_zone_start(&sep, 40), 62 - 40 - 3);
    assert_eq!(live_zone_start(&vec![String::new(); 62], 40), 0);
}

/// [`live_zone_reaches_above`]: the measured tall pane's last 40 rows hold the 35-row
/// gap and say so; a full transcript, one-to-three-row separators and a tail that ends
/// on a blank row do not.
#[test]
fn a_tail_that_holds_a_gap_says_the_zone_reaches_above_it() {
    use crate::prompt::fixtures::API_ERROR_TALL_PANE_MEASURED;
    let tall = screen(API_ERROR_TALL_PANE_MEASURED);
    let tail_of = |rows: &[String]| rows[rows.len().saturating_sub(40)..].to_vec();
    assert!(live_zone_reaches_above(&tail_of(&tall), 40));
    let mut full = tall.clone();
    for row in &mut full[11..46] {
        *row = "· earlier".to_string();
    }
    assert!(!live_zone_reaches_above(&tail_of(&full), 40));
    assert_eq!(live_zone_start(&full, 40), 10);
    for gap in 1..BLANK_GAP {
        let mut sep = full.clone();
        for row in &mut sep[30..30 + gap] {
            row.clear();
        }
        assert!(!live_zone_reaches_above(&tail_of(&sep), 40), "gap {gap}");
    }
    let mut four = full.clone();
    for row in &mut four[30..30 + BLANK_GAP] {
        row.clear();
    }
    assert!(live_zone_reaches_above(&tail_of(&four), 40));
    assert_eq!(live_zone_start(&four, 40), 10 - (BLANK_GAP - 1));
    let mut inline = tall.clone();
    inline.extend(std::iter::repeat_n(String::new(), 12));
    assert!(!live_zone_reaches_above(&tail_of(&inline), 40));
    assert!(!live_zone_reaches_above(&vec![String::new(); 40], 40));
    let mut cut = full.clone();
    cut[10].clear();
    assert!(!live_zone_reaches_above(&tail_of(&cut), 40));
    let (mut above_seen, mut cut_seen) = (0, 0);
    for run in 1..=10usize {
        for inside in 0..=3usize {
            let mut rows: Vec<String> = (0..62).map(|i| format!("row {i}")).collect();
            let last40 = rows.len() - 40;
            let bottom = last40 + inside;
            for row in &mut rows[(bottom + 1).saturating_sub(run)..=bottom] {
                row.clear();
            }
            let in_tail = run.min(inside + 1);
            let above = live_zone_start(&rows, 40) < last40;
            let asks = live_zone_reaches_above(&tail_of(&rows), 40);
            assert_eq!(asks, in_tail >= BLANK_GAP, "run {run}, {inside} inside");
            assert_eq!(
                above,
                run >= BLANK_GAP,
                "run {run}, {inside} inside: the zone is above the tail"
            );
            above_seen += usize::from(above);
            cut_seen += usize::from(above && !asks);
        }
    }
    assert!(above_seen > 10, "the sweep saw {above_seen} zones above");
    assert!(
        cut_seen > 0,
        "and some cut ones ({cut_seen}) the tail cannot see"
    );
}

/// The inline renderer's REPL sits above the grid's last 40 rows (2.1.283 at 150x50,
/// the `INLINE_*` fixtures: the box on rows 8-11, 38 blank rows under it).
#[test]
fn an_inline_repl_above_the_last_40_rows_is_read_in_the_live_zone() {
    use crate::prompt::fixtures::{
        INLINE_REPL_HALF_DRAWN, INLINE_REPL_READY, INLINE_TRUST, LAUNCH_REPL_READY,
    };
    let zone = |rows: &[String]| rows[live_zone_start(rows, 40)..].to_vec();
    let tail = |rows: &[String]| rows[rows.len().saturating_sub(40)..].to_vec();
    let ready = screen(INLINE_REPL_READY);
    assert_eq!(ready.len(), 50, "a 50-row pane");
    let caret = ready
        .iter()
        .position(|r| r.starts_with('❯'))
        .expect("the caret row");
    assert!(caret < 10, "the prompt box sits above the last 40 rows");
    let cut = read(Some("claude"), &tail(&ready), None);
    assert_eq!(
        (cut.phase.clone(), cut.phase_authoritative),
        (Phase::Idle, false),
        "the grid's last 40 rows: {cut:?}"
    );
    assert_eq!(live_zone_start(&ready, 40), 0);
    let r = read(Some("claude"), &zone(&ready), Some(2));
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true),
        "{r:?}"
    );
    assert!(r.composer.is_some() && r.fresh, "{r:?}");
    let dialog = read(Some("claude"), &zone(&screen(INLINE_TRUST)), None);
    assert_eq!(
        (dialog.phase.clone(), dialog.prompt.as_ref().map(|p| p.kind)),
        (Phase::Prompt, Some(PromptKind::Trust)),
        "{dialog:?}"
    );
    let half = read(Some("claude"), &zone(&screen(INLINE_REPL_HALF_DRAWN)), None);
    assert_eq!(
        (half.phase, half.phase_authoritative, half.composer),
        (Phase::Idle, false, None),
        "the REPL half drawn is no evidence"
    );
    let full = screen(LAUNCH_REPL_READY);
    assert_eq!(full.len(), 50);
    assert_eq!(live_zone_start(&full, 40), 0, "the banner is in the zone");
    let launch = read(Some("claude"), &zone(&full), Some(45));
    assert!(launch.fresh, "{launch:?}");
    let drawn: Vec<String> = (0..50).map(|i| format!("row {i}")).collect();
    assert_eq!(zone(&drawn), tail(&drawn));
}

/// [`live_zone_start`]'s arithmetic: the last `n` rows counted up from
/// the last row that is not blank (spaces and no-break spaces are blank),
/// never below 0; a screen with nothing drawn starts at 0.
#[test]
fn the_live_zone_counts_drawn_rows() {
    let grid = |drawn: usize, blank: usize| {
        let mut r: Vec<String> = (0..drawn).map(|i| format!("row {i}")).collect();
        r.extend((0..blank).map(|i| [" ", "", "\u{a0}"][i % 3].to_string()));
        r
    };
    for (drawn, blank, n, start) in [
        (50, 0, 40, 10),
        (12, 38, 40, 0),
        (45, 5, 40, 5),
        (60, 40, 40, 20),
        (3, 0, 40, 0),
        (0, 50, 40, 0),
        (0, 0, 40, 0),
        (10, 5, 4, 6),
    ] {
        assert_eq!(
            live_zone_start(&grid(drawn, blank), n),
            start,
            "{drawn} drawn, {blank} blank, n={n}"
        );
    }
}

/// Claude Code's shell mode (`!` as the caret, the `SHELL_MODE*` fixtures) is its prompt
/// box: authoritative idle, though no `❯` composer or frame.
#[test]
fn claude_codes_shell_mode_is_its_prompt_box() {
    use crate::prompt::fixtures::{SHELL_MODE, SHELL_MODE_DRAFT};
    for (name, text, caret) in [
        (
            "shell mode",
            SHELL_MODE,
            "!\u{a0}Try \"write a test for <filepath>\"",
        ),
        ("shell mode, a command typed", SHELL_MODE_DRAFT, "!\u{a0}ls"),
    ] {
        let full = screen(text);
        assert!(
            full.iter().any(|r| r == caret),
            "{name}: the measured caret row"
        );
        for rows in [&full[..], &full[live_zone_start(&full, 40)..]] {
            let r = read(Some("claude"), rows, Some(2));
            assert_eq!(r.program, Program::Claude, "{name}");
            assert_eq!(
                (r.phase.clone(), r.phase_authoritative),
                (Phase::Idle, true),
                "{name}: {r:?}"
            );
            assert_eq!((r.composer, r.wall), (None, None), "{name}");
            assert!(!has_composer_frame(rows), "{name}");
        }
        let at = full.iter().position(|r| r == caret).expect("the caret");
        let bottom = at + 1;
        assert!(crate::phase::is_rule(&full[bottom]), "{name}");
        let mut half = full.clone();
        half[bottom] = String::new();
        let mut unruled = full.clone();
        unruled[at - 1] = String::new();
        let mut word = full.clone();
        word[at] = "!important: not a caret".to_string();
        for (what, rows) in [
            ("no bottom rule", half),
            ("no top rule", unruled),
            ("a word", word),
        ] {
            let r = read(Some("claude"), &rows, None);
            assert_eq!(
                (r.phase.clone(), r.phase_authoritative),
                (Phase::Idle, false),
                "{name}, {what}: {r:?}"
            );
        }
    }
}

/// Claude Code's idle is the prompt box that holds the cursor: an inline relaunch leaves
/// the previous run's box whole above the new launch line, authoritative idle by the
/// frame alone but no evidence with the cursor under the launch line.
#[test]
fn claude_codes_idle_is_the_prompt_box_that_holds_the_cursor() {
    use crate::prompt::fixtures::{
        INLINE_RELAUNCH_BEFORE_REPL, INLINE_RELAUNCH_REPL_HALF_DRAWN, INLINE_RELAUNCH_REPL_READY,
        INLINE_RELAUNCH_TRUST, INLINE_REPL_HALF_DRAWN, INLINE_REPL_READY, INLINE_TRUST,
        LAUNCH_BEFORE_REPL, LAUNCH_REPL_HALF_DRAWN, LAUNCH_REPL_READY, SHELL_MODE,
        SHELL_MODE_DRAFT, cursor,
    };
    let stale = screen(INLINE_RELAUNCH_BEFORE_REPL);
    let (row, col) = cursor(INLINE_RELAUNCH_BEFORE_REPL).expect("measured");
    assert!(
        stale[..row]
            .iter()
            .any(|r| r.contains("Press Ctrl-C again to exit"))
    );
    let framed = read(Some("claude"), &stale, None);
    assert_eq!(
        (framed.phase.clone(), framed.phase_authoritative),
        (Phase::Idle, true),
        "the frame rule: {framed:?}"
    );
    let at = read_at(Some("claude"), &stale, Some(row), Some(col));
    assert_eq!(
        (at.phase.clone(), at.phase_authoritative),
        (Phase::Idle, false),
        "{at:?}"
    );
    let zone = live_zone_start(&stale, 40);
    let zoned = read_at(
        Some("claude"),
        &stale[zone..],
        row.checked_sub(zone),
        Some(col),
    );
    assert!(!zoned.phase_authoritative, "the live zone: {zoned:?}");

    for (name, text, want) in [
        ("launch, before the REPL", LAUNCH_BEFORE_REPL, None),
        ("launch, the REPL half drawn", LAUNCH_REPL_HALF_DRAWN, None),
        ("launch, the REPL", LAUNCH_REPL_READY, Some(Phase::Idle)),
        ("shell mode", SHELL_MODE, Some(Phase::Idle)),
        (
            "shell mode, a command typed",
            SHELL_MODE_DRAFT,
            Some(Phase::Idle),
        ),
        ("inline, the dialog", INLINE_TRUST, Some(Phase::Prompt)),
        ("inline, the REPL half drawn", INLINE_REPL_HALF_DRAWN, None),
        ("inline, the REPL", INLINE_REPL_READY, Some(Phase::Idle)),
        (
            "relaunch, the dialog",
            INLINE_RELAUNCH_TRUST,
            Some(Phase::Prompt),
        ),
        (
            "relaunch, before the REPL",
            INLINE_RELAUNCH_BEFORE_REPL,
            None,
        ),
        (
            "relaunch, half drawn",
            INLINE_RELAUNCH_REPL_HALF_DRAWN,
            None,
        ),
        (
            "relaunch, the REPL",
            INLINE_RELAUNCH_REPL_READY,
            Some(Phase::Idle),
        ),
    ] {
        let rows = screen(text);
        let (row, col) = cursor(text).unwrap_or_else(|| panic!("{name}: a measured cursor"));
        assert!(row < rows.len(), "{name}");
        let zone = live_zone_start(&rows, 40);
        for (cut, r) in [
            (
                "whole",
                read_at(Some("claude"), &rows, Some(row), Some(col)),
            ),
            (
                "zone",
                read_at(
                    Some("claude"),
                    &rows[zone..],
                    row.checked_sub(zone),
                    Some(col),
                ),
            ),
        ] {
            let got = r.phase_authoritative.then(|| r.phase.clone());
            assert_eq!(got, want, "{name} ({cut}): {r:?}");
        }
        let off = read_at(Some("claude"), &rows, None, None);
        if want == Some(Phase::Idle) {
            assert!(!off.phase_authoritative, "{name}: the cursor off the rows");
            let outside = (0..rows.len()).filter(|&r| !crate::phase::prompt_box_holds(&rows, r));
            for r in outside {
                assert!(
                    !read_at(Some("claude"), &rows, Some(r), None).phase_authoritative,
                    "{name}: the cursor on row {r}, outside the box"
                );
            }
        } else {
            assert_eq!(off.phase_authoritative, want.is_some(), "{name}");
        }
    }

    let spinning = rows(&["% claude", "", "✶ Deliberating… (3s · thinking)"]);
    let end = screen(END_529);
    let top = end
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the rule");
    for (name, r, phase) in [
        ("trust dialog", screen(TRUST), Phase::Prompt),
        ("spinner", spinning, Phase::Busy),
        ("question", shell_asking(), Phase::Question),
        ("wall", end[..top].to_vec(), Phase::Idle),
    ] {
        let reading = read_at(Some("claude"), &r, None, None);
        assert_eq!(
            (reading.phase, reading.phase_authoritative),
            (phase, true),
            "{name}"
        );
    }
    for program in [Some("codex"), None] {
        let codex = codex_0_155_trust();
        assert_eq!(
            read_at(program, &codex, None, None),
            read(program, &codex, None),
            "{program:?}: the cursor is Claude Code's rule"
        );
    }
}

/// [`crate::phase::prompt_box_holds`]: a box holds its rows from its top rule to its
/// bottom rule, for the `❯` composer and shell mode's `!` alike; nothing else holds,
/// and a whole box does not stretch down to a half-drawn one under it (the relaunch).
#[test]
fn a_prompt_box_holds_its_rows_and_nothing_else() {
    use crate::phase::prompt_box_holds as holds;
    use crate::prompt::fixtures::{INLINE_RELAUNCH_REPL_HALF_DRAWN, cursor};
    let rule = "─".repeat(40);
    for caret in ["❯ Try \"fix lint errors\"", "!\u{a0}ls", "!"] {
        let r = rows(&[
            "banner",
            "",
            &rule,
            caret,
            "  more of the draft",
            &rule,
            "  footer",
            "",
        ]);
        let held: Vec<usize> = (0..r.len() + 2).filter(|&i| holds(&r, i)).collect();
        assert_eq!(held, [2, 3, 4, 5], "{caret}");
    }
    let unruled = rows(&["banner", "", "❯ a transcript row", &rule, "  footer"]);
    assert!((0..unruled.len()).all(|i| !holds(&unruled, i)));
    let half = rows(&["", &rule, "❯ Try", "──"]);
    assert!((0..half.len()).all(|i| !holds(&half, i)));
    assert!(!holds(&[], 0) && !holds(&rows(&["❯"]), 0));
    let relaunch = screen(INLINE_RELAUNCH_REPL_HALF_DRAWN);
    let (row, _) = cursor(INLINE_RELAUNCH_REPL_HALF_DRAWN).expect("measured");
    let old: Vec<usize> = (0..relaunch.len())
        .filter(|&i| holds(&relaunch, i))
        .collect();
    assert_eq!(old, [8, 9, 10], "only the old box, whole");
    assert!(
        !holds(&relaunch, row),
        "the cursor, on the new box's half-drawn rule"
    );
}

/// The banner's vendor remedy (`— restart and resume with claude --continue`) is cut by
/// [`crate::wall::memory_banner_head`]: `--continue` resumes the directory's newest
/// conversation, which may be a sibling tab's.
#[test]
fn the_memory_banner_head_leaves_the_vendors_continue_out() {
    let banner = memory_wall(&screen(MEMORY_BANNER_BUSY)).expect("the banner");
    assert!(banner.message.contains("--continue"), "{}", banner.message);
    let head = crate::wall::memory_banner_head(&banner.message);
    assert_eq!(head, format!("{} (140.4GB)", crate::anchor!("wall.memory")));
    assert!(!head.contains("--continue"), "{head}");
}

/// agy's text alone cannot bound a message that holds a blank line
/// ([`ScreenReader::bounds_messages`] is `false`).
#[test]
fn agy_alone_cannot_bound_a_message_that_holds_a_blank_line() {
    let rows = crate::prompt::fixtures::screen(crate::agy::fixtures::lab::WORKER_FIRST_PROMPT);
    let act = rows
        .iter()
        .position(|r| r.starts_with("> [aterm harness]"))
        .expect("the Land act");
    let caret = rows
        .iter()
        .rposition(|r| r.trim_end() == ">")
        .expect("the composer");
    let first: Vec<String> = rows[..act - 1]
        .iter()
        .chain(&rows[caret - 1..])
        .cloned()
        .collect();
    let first = &first[..];
    assert_eq!(
        crate::agy::turn_opened_by(first).as_deref(),
        Some("FOREVERLAB-WORKER. write the parser for this repository and land it")
    );
    assert!(
        crate::agy::said_tail(first)
            .is_some_and(|said| said.starts_with("(Started by `s-146719f2750f4a8f6909`")),
        "the fact line read as the reply"
    );
    assert!(!Program::Agy.reader().bounds_messages());
}

/// Only agy reads subsequent indented message rows after a blank line as the reply.
#[test]
fn only_agy_reads_a_messages_rows_as_its_reply() {
    use crate::prompt::fixtures::screen;
    let claude = {
        let mut r = rows(&["❯ Fix the parser.", "", "⏺ Fixed the parser.", ""]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let cases = [
        (Program::Claude, claude, "❯ ", "  "),
        (
            Program::Muse,
            screen(crate::muse::fixtures::END_OF_TURN),
            "❯ ",
            "  ",
        ),
        (
            Program::Agy,
            screen(crate::agy::fixtures::END_OF_TURN),
            "> ",
            "  ",
        ),
    ];
    for (program, rows, caret, indent) in cases {
        let reader = program.reader();
        let said = reader.said_tail(&rows);
        assert!(said.is_some(), "{program:?}: a reply to read");
        let at = rows
            .iter()
            .position(|r| r.starts_with(caret) && r.trim().len() > 1)
            .unwrap_or_else(|| panic!("{program:?}: the message's first row"));
        let mut more = rows.clone();
        more.splice(
            at + 1..at + 1,
            [String::new(), format!("{indent}NEEDS YOU: y")],
        );
        let read = reader.said_tail(&more);
        assert_eq!(
            read == said,
            reader.bounds_messages(),
            "{program:?}: {said:?} then {read:?}"
        );
    }
}

#[test]
fn only_hosted_harnesses_read_a_harness_version() {
    for prog in Program::ALL {
        // Codex owns its version. Hosted harnesses have an independent version;
        // a Codex policy table does not introduce another harness.
        assert_eq!(
            prog.harness_version().is_some(),
            !matches!(prog, Program::Generic | Program::Codex),
            "{prog:?}"
        );
        assert!(
            !prog.harness_table() || prog == Program::Codex || prog.harness_version().is_some(),
            "{prog:?}"
        );
        assert_eq!(
            prog.harness_version_num()
                .map(crate::HarnessVersion::as_str),
            prog.harness_version(),
            "{prog:?}"
        );
        assert_eq!(
            prog.reader().harness_version(),
            prog.harness_version(),
            "{prog:?}"
        );
        assert_eq!(
            prog.reader().harness_version_num(),
            prog.harness_version_num(),
            "{prog:?}"
        );
    }
    assert_eq!(Program::Generic.harness_version(), None);
    assert_eq!(Program::Generic.harness_version_num(), None);
}

/// Claude Code's title runs under one of its two busy frames and a blank (2.1.311 lab `mr`:
/// `◐`/`◑ Claude Code` flipped every 0.96 s through the reply, `✳ Claude Code` once it
/// ended). Its rest glyph, a title with no glyph, an empty title (what `title` answers where
/// the program set none) and Muse Code's spinner say no run: each is a reading, never `None`,
/// since the loop asks `title_runs("")` whether to read the title at all.
#[test]
fn claude_codes_title_says_whether_its_turn_runs() {
    let runs = |t: &str| ClaudeReader.title_runs(t);
    for t in ["◐ Claude Code", "◑ Claude Code", "◑ Auto-prompting…", "◐ "] {
        assert_eq!(runs(t), Some(true), "{t:?}");
    }
    let rest = format!("{} Claude Code", crate::anchor!("title.rest"));
    for t in [
        rest.as_str(),
        "Claude Code",
        "",
        "◐",
        "◐Claude Code",
        "Claude Code ◐",
        " ◑ Claude Code",
        "◓ Claude Code",
        "⠙ proj",
    ] {
        assert_eq!(runs(t), Some(false), "{t:?}");
    }
}

/// Under a status line Claude Code draws a reply with no busy sign on the screen (2.1.311 lab
/// `mr`, MEASURED): the frames mid-reply and with the last line held back are the finished
/// reply's chrome row for row, less its done row. The finished reply reads an idle that is
/// evidence, every `NEEDS YOU` line in what it said, under its done row
/// ([`ScreenReader::end_drawn`]); the two frames before it hold fewer, which is why the loop
/// waits for the title's rest ([`crate::phase::title_runs`]) where no done row is drawn.
#[test]
fn a_reply_drawn_under_a_status_line_says_all_of_it_only_once_done() {
    use crate::prompt::fixtures::{
        STATUSLINE_LAST_LINE_HELD_2_1_311, STATUSLINE_MIDREPLY_2_1_311,
        STATUSLINE_REPLY_DONE_2_1_311,
    };
    let needs = |r: &Reading| {
        r.said_tail
            .as_deref()
            .map_or(0, |t| t.matches("NEEDS YOU:").count())
    };
    let done = read(Some("claude"), &screen(STATUSLINE_REPLY_DONE_2_1_311), None);
    assert_eq!(
        (done.phase.clone(), done.phase_authoritative, needs(&done)),
        (Phase::Idle, true, 4),
        "{done:?}"
    );
    assert!(ClaudeReader.end_drawn(&screen(STATUSLINE_REPLY_DONE_2_1_311)));
    for (name, text, drawn) in [
        ("mid-reply", STATUSLINE_MIDREPLY_2_1_311, 0),
        ("the last line held", STATUSLINE_LAST_LINE_HELD_2_1_311, 3),
    ] {
        let rows = screen(text);
        assert_eq!(needs(&read(Some("claude"), &rows, None)), drawn, "{name}");
        assert!(!ClaudeReader.end_drawn(&rows), "{name}: no done row yet");
        assert!(
            !rows.iter().any(|r| r.contains("esc to interrupt")),
            "{name}: the status line's footer"
        );
    }
}
