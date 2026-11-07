// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The run sign read beside the screen: a turn the window title runs that the screen does not
//! show ([`crate::phase::title_live`], [`ScreenReader::phase_seen`], [`ScreenReader::read_seen`]).

use crate::muse::fixtures::IDLE as MUSE_IDLE;
use crate::phase::{
    Busy, Zone, background_rest, background_rest_seen, background_wait, background_wait_seen,
    busy_seen, busy_signal, delegated_shown, title_live, title_rests, worker_phase,
    worker_phase_seen,
};
use crate::prompt::fixtures::{
    MEMORY_BANNER_IDLE, STATUSLINE_LAST_LINE_HELD_2_1_311, STATUSLINE_MIDREPLY_2_1_311,
    STATUSLINE_REPLY_DONE_2_1_311, screen,
};
use crate::{ClaudeReader, MuseReader, Phase, Program, Reading, ScreenReader, read_at, read_seen};

const WATCHDOG_STATUSLINE: &str =
    include_str!("fixtures/claude-2.1.300-wall-watchdog-session-statusline.txt");

/// Claude Code 2.1.318 under aterm's usage tap, a status line (MEASURED 2026-11-06): a long reply
/// streaming, its spinner row still drawn, and an earlier turn's end in the same session (`❯ hi`,
/// before the long reply was asked for), its done row drawn.
const TAP_MID_TURN_2_1_318: &str =
    include_str!("fixtures/claude-2.1.318-tap-row-week-mid-turn.txt");
const TAP_DONE_2_1_318: &str = include_str!("fixtures/claude-2.1.318-tap-row-week.txt");

/// The titles Claude Code writes while its REPL is busy (2.1.311 lab `mr`).
const RUNNING: [&str; 2] = ["◐ Claude Code", "◑ Claude Code"];

/// Titles that say no run: the rest glyph, a title with no glyph (its status in the terminal's
/// tab), an empty title (one never set), and none at all (a host that sends none).
const NOT_RUNNING: [Option<&str>; 4] = [Some("✳ Claude Code"), Some("Claude Code"), Some(""), None];

fn needs(r: &Reading) -> usize {
    r.said_tail
        .as_deref()
        .map_or(0, |t| t.matches("NEEDS YOU:").count())
}

/// `rows` with `old` replaced by `new` on the one row that holds it.
fn edited(rows: &[String], old: &str, new: &str) -> Vec<String> {
    let mut out = rows.to_vec();
    let row = out
        .iter_mut()
        .find(|r| r.contains(old))
        .unwrap_or_else(|| panic!("a row holding {old:?}"));
    *row = row.replace(old, new);
    out
}

/// `rows` with the row that is exactly `old` replaced by `new`.
fn replaced(rows: &[String], old: &str, new: &str) -> Vec<String> {
    let mut out = rows.to_vec();
    let row = out
        .iter_mut()
        .find(|r| r.as_str() == old)
        .unwrap_or_else(|| panic!("a row {old:?}"));
    *row = new.to_string();
    out
}

/// The composer's caret row, where Claude Code holds the cursor at rest.
fn caret(rows: &[String]) -> Option<usize> {
    rows.iter().rposition(|r| r.starts_with('❯'))
}

/// `rows` with `extra` drawn under the last row.
fn under(rows: &[String], extra: &[&str]) -> Vec<String> {
    let mut out = rows.to_vec();
    out.extend(extra.iter().map(|r| (*r).to_string()));
    out
}

#[test]
fn claude_codes_rest_glyph_is_its_title_at_rest() {
    assert!(title_rests("✳ Claude Code"));
    assert!(title_rests("✳ Auto-prompting…"));
    for t in [
        "◐ Claude Code",
        "◑ Claude Code",
        "Claude Code",
        "",
        "✳",
        "✳Claude Code",
        " ✳ Claude Code",
    ] {
        assert!(!title_rests(t), "{t:?}");
    }
}

/// The two 2.1.311 frames of a reply still being drawn under a status line read Busy, the
/// title's own busy (`title: running`), under a running title; the finished frame under it
/// reads idle with every `NEEDS YOU` line, its done row the end the title cannot overrule.
#[test]
fn a_running_title_holds_a_reply_drawn_under_a_status_line_busy() {
    for (name, text, title) in [
        ("mid-reply", STATUSLINE_MIDREPLY_2_1_311, RUNNING[0]),
        ("mid-reply", STATUSLINE_MIDREPLY_2_1_311, RUNNING[1]),
        (
            "the last line held",
            STATUSLINE_LAST_LINE_HELD_2_1_311,
            RUNNING[1],
        ),
        (
            "the last line held",
            STATUSLINE_LAST_LINE_HELD_2_1_311,
            RUNNING[0],
        ),
    ] {
        let rows = screen(text);
        let runs = ClaudeReader.runs(Some(title));
        assert_eq!(runs, Some(true), "{title}");
        let r = read_seen(Some("claude"), &rows, None, None, Some(title));
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative, r.wall.is_none()),
            (Phase::Busy, true, true),
            "{name} under {title}: {r:?}"
        );
        assert_eq!(ClaudeReader.phase_seen(&rows, runs), Phase::Busy, "{name}");
        let busy = busy_seen(&rows, runs);
        assert_eq!(
            busy,
            Some(Busy {
                zone: Zone::Title,
                rule: "running",
                soft: false
            }),
            "{name}"
        );
        assert_eq!(
            busy.map(|b| b.to_string()).as_deref(),
            Some("title: running")
        );
        assert_eq!(
            busy_signal(&rows),
            None,
            "{name}: the screen alone shows nothing"
        );
    }
    let done = screen(STATUSLINE_REPLY_DONE_2_1_311);
    for title in RUNNING {
        let r = read_seen(Some("claude"), &done, caret(&done), None, Some(title));
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative, needs(&r)),
            (Phase::Idle, true, 4),
            "the done row under {title}"
        );
        assert!(!title_live(&done, Some(true)));
    }
}

/// Negative control: under a title that says no run, every frame reads exactly as the screen
/// alone reads it, so the title is what holds the two frames busy.
#[test]
fn a_title_that_says_no_run_reads_as_the_screen_alone() {
    for text in [
        STATUSLINE_MIDREPLY_2_1_311,
        STATUSLINE_LAST_LINE_HELD_2_1_311,
        STATUSLINE_REPLY_DONE_2_1_311,
        WATCHDOG_STATUSLINE,
    ] {
        let rows = screen(text);
        let alone = read_at(Some("claude"), &rows, caret(&rows), None);
        for title in NOT_RUNNING {
            let runs = ClaudeReader.runs(title);
            assert_ne!(runs, Some(true), "{title:?}");
            assert_eq!(
                read_seen(Some("claude"), &rows, caret(&rows), None, title),
                alone,
                "{title:?}"
            );
            assert_eq!(worker_phase_seen(&rows, runs), worker_phase(&rows));
            assert_eq!(busy_seen(&rows, runs), busy_signal(&rows));
            assert_eq!(background_wait_seen(&rows, runs), background_wait(&rows));
        }
    }
    let mid = screen(STATUSLINE_MIDREPLY_2_1_311);
    let alone = read_at(Some("claude"), &mid, caret(&mid), None);
    assert_eq!(
        (alone.phase, alone.phase_authoritative),
        (Phase::Idle, true),
        "today's misread"
    );
}

/// The watchdog's retry row under a status line (2.1.300) reads as it does with no title: the
/// row's own busy, then its wall. A running title adds nothing to a turn the screen shows.
#[test]
fn a_turn_the_screen_shows_reads_as_the_screen_shows_it_under_a_running_title() {
    let rows = screen(WATCHDOG_STATUSLINE);
    for title in RUNNING {
        assert_eq!(
            read_seen(Some("claude"), &rows, None, None, Some(title)),
            read_at(Some("claude"), &rows, None, None),
            "{title}"
        );
        assert_eq!(busy_seen(&rows, Some(true)), busy_signal(&rows));
    }
}

/// Claude Code's title runs while work it handed off runs (a background agent, a workflow),
/// its turn over: where the screen shows such work, the title says nothing of the turn and the
/// screen's reading stands. A shell or a monitor is no such work.
#[test]
fn handed_off_work_on_the_screen_leaves_the_reading_to_the_screen() {
    let mid = screen(STATUSLINE_MIDREPLY_2_1_311);
    let asked = edited(&mid, "placeholder wor.", "placeholder words, or more?");
    let asked = replaced(&asked, "  -", "");
    assert_eq!(worker_phase(&asked), Phase::Question, "the control frame");
    assert_eq!(worker_phase_seen(&asked, Some(true)), Phase::Busy);
    let panel = under(
        &mid,
        &[
            "",
            "  ⏺ main",
            "  ◯ general-purpose  bling probe                                    21s · ↓ 6.2k tokens",
        ],
    );
    let pill = edited(
        &asked,
        "(shift+tab to cycle)",
        "(shift+tab to cycle) · 1 local agent",
    );
    let workflow = under(
        &mid,
        &["  ◯ stall-probe  ▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱  1/2 · 8s · ↓ 12.7k tokens"],
    );
    for (name, rows) in [
        ("panel", &panel),
        ("agent pill", &pill),
        ("workflow", &workflow),
    ] {
        assert!(delegated_shown(rows), "{name}");
        assert!(!title_live(rows, Some(true)), "{name}");
        assert_eq!(
            worker_phase_seen(rows, Some(true)),
            worker_phase(rows),
            "{name}"
        );
        assert_eq!(
            read_seen(Some("claude"), rows, None, None, Some(RUNNING[0])),
            read_at(Some("claude"), rows, None, None),
            "{name}"
        );
    }
    assert_eq!(worker_phase(&panel), Phase::Idle);
    assert_eq!(worker_phase(&pill), Phase::Question);
    let shells = edited(
        &mid,
        "(shift+tab to cycle)",
        "(shift+tab to cycle) · 2 shells",
    );
    assert!(!delegated_shown(&shells), "a shell runs no title");
    assert!(title_live(&shells, Some(true)));
}

/// A count the footer's pill draws is handed-off work by its noun, whatever words qualify it: a
/// task, an agent, a workflow, a session or a team (`1 MCP task`, `2 background tasks`, `◇ 1
/// cloud session`, `1 local agent`) leaves the reading to the screen, while a monitor, qualified
/// (`1 Artifact comment monitor`, 2.1.286's label) or not, runs no title, so a reply drawn under a
/// status line beside it is the title's busy, as beside a shell.
#[test]
fn a_qualified_monitor_in_the_pill_is_no_handed_off_work() {
    let mid = screen(STATUSLINE_MIDREPLY_2_1_311);
    let pill = |count: &str| {
        edited(
            &mid,
            "(shift+tab to cycle)",
            &format!("(shift+tab to cycle) · {count}"),
        )
    };
    for count in ["1 Artifact comment monitor", "1 monitor", "3 monitors"] {
        let rows = pill(count);
        assert!(!delegated_shown(&rows), "{count}");
        assert!(title_live(&rows, Some(true)), "{count}");
        assert_eq!(
            busy_seen(&rows, Some(true)).map(|b| b.zone),
            Some(Zone::Title),
            "{count}"
        );
        let r = read_seen(Some("claude"), &rows, None, None, Some(RUNNING[0]));
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Busy, true),
            "{count}: {r:?}"
        );
    }
    for count in [
        "1 MCP task",
        "2 background tasks",
        "◇ 1 cloud session",
        "1 local agent",
    ] {
        let rows = pill(count);
        assert!(delegated_shown(&rows), "{count}");
        assert!(!title_live(&rows, Some(true)), "{count}");
        assert_eq!(
            read_seen(Some("claude"), &rows, None, None, Some(RUNNING[0])),
            read_at(Some("claude"), &rows, None, None),
            "{count}"
        );
    }
}

/// A live reply with a shell pill is no break of the agent's own background work while the
/// title runs a turn (the shell runs no title): the title's busy, and no background wait. Under
/// a title at rest it is the break it reads today.
#[test]
fn a_shell_pill_under_a_running_title_is_a_live_turn_not_a_break() {
    let mid = screen(STATUSLINE_MIDREPLY_2_1_311);
    let shells = edited(
        &mid,
        "(shift+tab to cycle)",
        "(shift+tab to cycle) · 2 shells",
    );
    assert_eq!(worker_phase(&shells), Phase::Busy);
    assert_eq!(background_wait(&shells), Some("a shell running"));
    assert_eq!(background_wait_seen(&shells, Some(true)), None);
    assert_eq!(ClaudeReader.background_wait_seen(&shells, Some(true)), None);
    assert_eq!(
        busy_seen(&shells, Some(true)).map(|b| b.zone),
        Some(Zone::Title)
    );
    assert_eq!(
        background_rest(&shells),
        Some(Phase::Idle),
        "the screen alone: a turn over under a shell"
    );
    assert_eq!(background_rest_seen(&shells, Some(true)), None);
    assert_eq!(ClaudeReader.background_rest_seen(&shells, Some(true)), None);
    for runs in [Some(false), None] {
        assert_eq!(background_wait_seen(&shells, runs), Some("a shell running"));
        assert_eq!(background_rest_seen(&shells, runs), Some(Phase::Idle));
        assert_eq!(
            ClaudeReader.background_rest_seen(&shells, runs),
            Some(Phase::Idle)
        );
    }
}

/// A finished turn is never held busy by its title: a done row, an interrupt, a wall, a limit
/// and a box each stand as the screen reads them under a running title, across every Claude
/// Code capture; and the title turns only an Idle or a Question into a Busy.
#[test]
fn a_finished_turn_is_never_held_busy_by_a_running_title() {
    let done = screen(STATUSLINE_REPLY_DONE_2_1_311);
    let cut = done
        .iter()
        .take_while(|r| !r.starts_with("  NEEDS YOU:"))
        .cloned()
        .chain(["  ⎿  Interrupted · What should Claude do instead?", ""].map(str::to_string))
        .chain(done.iter().skip_while(|r| !r.starts_with('─')).cloned())
        .collect::<Vec<_>>();
    assert!(
        crate::turn::interrupted(&cut),
        "the control frame: {cut:#?}"
    );
    assert_eq!(
        worker_phase_seen(&cut, Some(true)),
        worker_phase(&cut),
        "an interrupt"
    );
    let not_cut = edited(
        &cut,
        "  ⎿  Interrupted · What should Claude do instead?",
        "  more placeholder words.",
    );
    assert_eq!(
        worker_phase_seen(&not_cut, Some(true)),
        Phase::Busy,
        "the interrupt is what ends it"
    );
    let mut held = 0;
    for (name, text) in crate::text::fixture_files() {
        let rows = screen(&text);
        if crate::identify(None, &rows).program() != Program::Claude {
            continue;
        }
        let phase = worker_phase(&rows);
        let seen = worker_phase_seen(&rows, Some(true));
        let ended = crate::parse_prompt(&rows).is_some()
            || matches!(phase, Phase::Limited { .. } | Phase::Prompt)
            || crate::wall::turn_wall(&rows).is_some()
            || crate::phase::done_row_drawn(&rows)
            || crate::turn::interrupted(&rows);
        if ended {
            assert_eq!(seen, phase, "{name}: a finished turn held busy");
        }
        if seen != phase {
            assert!(
                matches!(phase, Phase::Idle | Phase::Question) && seen == Phase::Busy,
                "{name}: {phase:?} read as {seen:?}"
            );
            held += 1;
        }
        assert_eq!(worker_phase_seen(&rows, Some(false)), phase, "{name}");
        assert_eq!(worker_phase_seen(&rows, None), phase, "{name}");
    }
    assert!(held >= 2, "the two mid-reply frames at least: {held}");
}

/// Claude Code's critical-memory banner is no end of a turn: it draws it over a live turn too,
/// where the reading keeps it as its one wall (`memory-banner-busy.txt`). A reply drawn under a
/// status line beside it reads Busy under a running title, the banner its wall, so the memory
/// restart a turn's end takes waits for the reply's end. Under a title at rest it reads as the
/// screen alone does: Idle on the banner. NEGATIVE CONTROL: a wall a turn ends on (a limit's)
/// beside the same reply still stands as read.
#[test]
fn the_memory_banner_beside_a_reply_under_a_status_line_is_no_end_of_it() {
    let banner = screen(MEMORY_BANNER_IDLE)
        .into_iter()
        .find(|r| r.trim_start().starts_with("Critical memory usage ("))
        .expect("the banner row");
    for text in [
        STATUSLINE_MIDREPLY_2_1_311,
        STATUSLINE_LAST_LINE_HELD_2_1_311,
    ] {
        let rows = screen(text);
        let top = crate::phase::composer_rules(&rows).expect("a composer").0;
        let mut shown = rows.clone();
        shown.insert(top, banner.clone());
        let wall = crate::wall(&shown).expect("the banner reads");
        assert_eq!(wall.kind, crate::WallKind::Memory, "the control frame");
        assert!(crate::wall::turn_wall(&shown).is_none());
        for title in RUNNING {
            assert!(title_live(&shown, Some(true)), "{title}");
            let r = read_seen(Some("claude"), &shown, caret(&shown), None, Some(title));
            assert_eq!(
                (r.phase.clone(), r.phase_authoritative),
                (Phase::Busy, true),
                "{title}: {r:?}"
            );
            assert_eq!(r.wall.map(|w| w.kind), Some(crate::WallKind::Memory));
        }
        let alone = read_at(Some("claude"), &shown, caret(&shown), None);
        assert_eq!(
            (alone.phase.clone(), alone.wall.as_ref().map(|w| w.kind)),
            (Phase::Idle, Some(crate::WallKind::Memory)),
            "the screen alone: the misread a memory restart would act on"
        );
        for title in NOT_RUNNING {
            assert_eq!(
                read_seen(Some("claude"), &shown, caret(&shown), None, title),
                alone,
                "{title:?}"
            );
        }
        let limited = under(
            &rows,
            &["  You've hit your limit · resets 3am (Europe/London)"],
        );
        assert!(
            crate::wall::turn_wall(&limited).is_some(),
            "the control wall"
        );
        assert!(!title_live(&limited, Some(true)), "a limit stands as read");
    }
    let idle = screen(MEMORY_BANNER_IDLE);
    assert_eq!(worker_phase(&idle), Phase::Idle);
    assert_eq!(worker_phase_seen(&idle, Some(true)), Phase::Busy);
}

/// A box is never a turn only the title runs, even one Claude Code draws over its composer's
/// frame (its auto-mode setup, a deferred question): the busy the lights and the slash press read
/// there is the screen's own under a running title.
#[test]
fn a_box_over_the_composer_is_never_a_title_run() {
    let mut boxed = 0;
    for (name, text) in crate::text::fixture_files() {
        let rows = screen(&text);
        if crate::parse_prompt(&rows).is_none() || crate::phase::composer_frame(&rows).is_none() {
            continue;
        }
        boxed += 1;
        assert!(!title_live(&rows, Some(true)), "{name}");
        assert_eq!(busy_seen(&rows, Some(true)), busy_signal(&rows), "{name}");
    }
    assert!(
        boxed >= 2,
        "the auto-mode setup and the deferred question: {boxed}"
    );
}

/// Every reader reads a capture with no title exactly as it reads it today.
#[test]
fn no_title_reads_every_capture_as_today() {
    for (name, text) in crate::text::fixture_files() {
        let rows = screen(&text);
        let reader = crate::identify(None, &rows);
        for title in [None, Some("")] {
            let runs = reader.runs(title);
            assert_eq!(runs, None, "{name}");
            assert_eq!(
                reader.read_seen(&rows, None, None, runs),
                reader.read_at(&rows, None, None),
                "{name}"
            );
            assert_eq!(
                reader.read_titled(&rows, None, runs),
                reader.read(&rows, None),
                "{name}"
            );
            assert_eq!(
                reader.phase_seen(&rows, runs),
                reader.phase(&rows),
                "{name}"
            );
            assert_eq!(
                reader.background_wait_seen(&rows, runs),
                reader.background_wait(&rows),
                "{name}"
            );
            assert_eq!(
                reader.background_rest_seen(&rows, runs),
                reader.background_rest(&rows),
                "{name}"
            );
        }
    }
}

/// Claude Code 2.1.318 under aterm's usage tap: mid-turn its spinner row stays drawn, so a running
/// title adds nothing to the turn the screen shows, and at a turn's end (the session's first,
/// before the long reply was asked for) its done row stands as read under a running title. The
/// same reply with that spinner row and its tip gone (the shape 2.1.311 drew under a status line
/// once the reply's first line was drawn) shows no busy sign on the grid: the screen alone reads
/// it Idle, and a running title holds it Busy.
#[test]
fn claude_code_2_1_318_under_the_usage_tap_reads_its_title_beside_the_screen() {
    let mid = screen(TAP_MID_TURN_2_1_318);
    assert_eq!(
        busy_signal(&mid).map(|b| b.zone),
        Some(Zone::Status),
        "the control frame: its spinner row"
    );
    for title in RUNNING {
        assert_eq!(
            read_seen(Some("claude"), &mid, caret(&mid), None, Some(title)),
            read_at(Some("claude"), &mid, caret(&mid), None),
            "{title}"
        );
        assert_eq!(busy_seen(&mid, Some(true)), busy_signal(&mid));
    }
    let done = screen(TAP_DONE_2_1_318);
    assert!(!title_live(&done, Some(true)), "its done row");
    for title in RUNNING {
        let r = read_seen(Some("claude"), &done, caret(&done), None, Some(title));
        assert_eq!(r, read_at(Some("claude"), &done, caret(&done), None));
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    }
    let drawn: Vec<String> = mid
        .iter()
        .filter(|r| !r.starts_with("✢ Mulling") && !r.contains("Tip: Start with"))
        .cloned()
        .collect();
    assert_eq!(
        drawn.len() + 2,
        mid.len(),
        "the spinner row and its tip cut"
    );
    let alone = read_at(Some("claude"), &drawn, caret(&drawn), None);
    assert_eq!(
        (alone.phase.clone(), busy_signal(&drawn)),
        (Phase::Idle, None),
        "the screen alone"
    );
    for title in RUNNING {
        let r = read_seen(Some("claude"), &drawn, caret(&drawn), None, Some(title));
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative, r.wall.is_none()),
            (Phase::Busy, true, true),
            "{title}: {r:?}"
        );
        assert_eq!(
            busy_seen(&drawn, Some(true))
                .map(|b| b.to_string())
                .as_deref(),
            Some("title: running")
        );
    }
    for title in NOT_RUNNING {
        assert_eq!(
            read_seen(Some("claude"), &drawn, caret(&drawn), None, title),
            alone,
            "{title:?}"
        );
    }
}

/// Muse Code takes the default rule from its own title: its idle composer under its spinner's
/// title reads Busy, under a title at rest idle, and a turn its status row shows is no title's
/// run (`title_live` reads only what the rows alone read idle or a question).
#[test]
fn muse_codes_spinner_title_holds_its_idle_busy() {
    let rows = screen(MUSE_IDLE);
    assert_eq!(MuseReader.phase(&rows), Phase::Idle, "the control frame");
    let spinning = MuseReader.runs(Some("⠙ proj"));
    assert_eq!(spinning, Some(true));
    assert_eq!(MuseReader.phase_seen(&rows, spinning), Phase::Busy);
    assert_eq!(
        read_seen(Some("muse"), &rows, None, None, Some("⠙ proj")).phase,
        Phase::Busy
    );
    for title in [Some("proj"), Some(""), None] {
        let runs = MuseReader.runs(title);
        assert_eq!(MuseReader.phase_seen(&rows, runs), Phase::Idle, "{title:?}");
    }
    // A run its status row shows is no run only the title says: the supervisor waits that row
    // gone, and only an idle or a question its title holds is a title's run.
    assert!(MuseReader.title_live(&rows, spinning), "the idle composer");
    let busy = screen(crate::muse::fixtures::BUSY);
    assert_eq!(
        MuseReader.phase(&busy),
        Phase::Busy,
        "the busy control frame"
    );
    assert!(
        !MuseReader.title_live(&busy, spinning),
        "its status row shows the run"
    );
    assert_eq!(MuseReader.phase_seen(&busy, spinning), Phase::Busy);
    for p in Program::ALL {
        if !matches!(p, Program::Claude | Program::Muse) {
            assert_eq!(p.reader().runs(Some("⠙ proj")), None, "{}", p.name());
        }
    }
}
