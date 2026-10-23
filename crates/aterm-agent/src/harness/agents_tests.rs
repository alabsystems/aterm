// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The panel's fold ([`super`]).

use super::*;
use crate::harness::footer::agents_panel_under;
use aterm_phase::agents_panel::{one_cell, parse_row};
use std::path::Path;

fn fixture(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-phase/src/fixtures")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn panel_of(rows: &[String]) -> AgentsPanel {
    let bottom = aterm_phase::phase::composer_bottom(rows).expect("a composer");
    agents_panel_under(rows, bottom).expect("a panel")
}

fn panel(rows: &[String]) -> AgentsPanel {
    let mut p = AgentsPanel::EMPTY;
    for (i, row) in rows.iter().enumerate() {
        let parsed = parse_row(row, one_cell).unwrap_or_else(|| panic!("a panel row: {row:?}"));
        assert!(p.push(u16::try_from(i).unwrap(), parsed));
    }
    p
}

fn duration(s: u64) -> String {
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m {}s", s / 3600, s / 60 % 60, s % 60),
    }
}

fn agent(name: &str, activity: &str, elapsed_s: u64, tokens: &str) -> String {
    format!(
        "  ◯ {name}  {activity}            {} · ↓ {tokens} tokens",
        duration(elapsed_s)
    )
}

fn only(track: &PanelTrack) -> Dot {
    let [dot] = track.dots() else {
        panic!("one dot: {:?}", track.dots());
    };
    *dot
}

const T0: u64 = 1_000_000_000;

#[test]
fn the_wait_bg_pair_reads_116_tokens_a_second() {
    let (a, b) = (
        panel_of(&fixture("wait_bg.out")),
        panel_of(&fixture("wait_bg2.out")),
    );
    assert_eq!(a.rows().len(), 1);
    assert_eq!(a.rows()[0].kind, RowKind::Workflow);
    let mut track = PanelTrack::default();
    track.fold(&a, &[], T0);
    assert_eq!(only(&track).rate, 0.0, "one sample is no rate");
    track.fold(&b, &[], T0 + 56_000);
    let dot = only(&track);
    assert_eq!(track.tracked(), 1);
    assert!((dot.rate - 6_500.0 / 56.0).abs() < 0.5, "{dot:?}");
    assert!((dot.level - 0.66).abs() < 0.01, "{dot:?}");
    assert_eq!(
        (dot.state, dot.sat, dot.blind),
        (DotState::Running, 1.0, false)
    );
    let mut still = PanelTrack::default();
    still.fold(&a, &[], T0);
    still.fold(&a, &[], T0 + 56_000);
    assert_eq!(only(&still).rate, 0.0);
}

#[test]
fn the_measured_sleeping_agent_reads_quiet() {
    let (a, b) = (
        panel_of(&fixture("claude-2.1.304-agents-panel.txt")),
        panel_of(&fixture("claude-2.1.304-agents-panel-later.txt")),
    );
    let mut track = PanelTrack::default();
    track.fold(&a, &[], T0);
    let first = only(&track);
    assert_eq!(
        (first.state, first.kind, first.row),
        (DotState::Running, RowKind::Agent, a.rows()[1].row)
    );
    track.fold(&b, &[], T0 + 45_000);
    let dot = only(&track);
    assert_eq!(track.tracked(), 1);
    assert_eq!(
        (dot.state, dot.rate, dot.quiet_ms),
        (DotState::Quiet, 0.0, 45_000)
    );
    assert!(dot.sat < 1.0 && dot.sat > SAT_QUIET_FLOOR, "{dot:?}");
    assert!(!dot.blind);
}

#[test]
fn a_row_that_does_nothing_goes_quiet_then_stuck() {
    let run = |tokens: &dyn Fn(u64) -> String, until: u64| {
        let mut track = PanelTrack::default();
        let mut seen = Vec::new();
        for s in 0..=until {
            let row = agent("general-purpose", "Running the suite", 300 + s, &tokens(s));
            track.fold(&panel(&[row]), &[], T0 + s * 1000);
            seen.push(only(&track));
        }
        seen
    };
    let dots = run(&|_| "12.4k".into(), 200);
    assert_eq!(dots[19].state, DotState::Running);
    assert_eq!(dots[20].state, DotState::Quiet);
    assert!(
        (dots[85].sat - (1.0 - 0.5 * (1.0 - SAT_QUIET_FLOOR))).abs() < 1e-4,
        "{:?}",
        dots[85]
    );
    assert_eq!(dots[149].state, DotState::Quiet);
    assert_eq!(dots[150].state, DotState::Stuck);
    assert_eq!(
        (dots[200].state, dots[200].sat),
        (DotState::Stuck, SAT_STUCK)
    );
    assert_eq!(dots[200].quiet_ms, 200_000);

    let moving = run(&|s| format!("{}.{}k", 12 + s / 10, s % 10), 200);
    assert!(moving.iter().all(|d| d.state == DotState::Running));
    assert!(moving[200].rate > 50.0, "{:?}", moving[200]);

    let blind = run(&|_| "1.2m".into(), 200);
    assert!(blind[200].blind);
    assert_eq!(blind[200].state, DotState::Quiet);
}

#[test]
fn a_frozen_row_halts_at_its_due_time() {
    let mut track = PanelTrack::default();
    let row = [agent("Explore", "Reading the footer readers", 42, "3.1k")];
    track.fold(&panel(&row), &[], T0);
    assert_eq!(only(&track).state, DotState::Running);
    let due = track.due().expect("a turn is due");
    assert_eq!(due, T0 + 2 * 1000 + HALT_SLACK_MS);
    track.retime(due - 1);
    assert_eq!(only(&track).state, DotState::Running);
    track.retime(due);
    assert_eq!(only(&track).state, DotState::Halted);
    let row = [agent("Explore", "Reading the footer readers", 46, "3.1k")];
    track.fold(&panel(&row), &[], T0 + 4_000);
    assert_eq!(only(&track).state, DotState::Running);
}

#[test]
fn a_compaction_drop_restarts_the_rate_and_counts_as_work() {
    let mut track = PanelTrack::default();
    let fold = |track: &mut PanelTrack, s: u64, tokens: &str| {
        let row = [agent("general-purpose", "Long survey", 600 + s, tokens)];
        track.fold(&panel(&row), &[], T0 + s * 1000);
        only(track)
    };
    fold(&mut track, 0, "400.0k");
    assert!(fold(&mut track, 10, "401.0k").rate > 0.0);
    assert_eq!(fold(&mut track, 40, "401.0k").state, DotState::Quiet);
    let dropped = fold(&mut track, 41, "20.0k");
    assert_eq!(
        (dropped.state, dropped.rate, dropped.quiet_ms),
        (DotState::Running, 0.0, 0)
    );
    assert!(fold(&mut track, 45, "21.0k").rate > 0.0);
}

#[test]
fn rows_of_one_name_keep_their_own_history_as_the_window_scrolls() {
    let a = |s: u64, tokens: &str| agent("general-purpose", "Sweeping the crates", 600 + s, tokens);
    let b = |s: u64| agent("general-purpose", "Reading one file", 120 + s, "5.0k");
    let mut track = PanelTrack::default();
    track.fold(&panel(&[a(0, "300.0k"), b(0)]), &[], T0);
    track.fold(&panel(&[b(30)]), &[], T0 + 30_000);
    let [only_b] = track.dots() else { panic!() };
    assert_eq!(
        (only_b.state, only_b.quiet_ms),
        (DotState::Quiet, 30_000),
        "{only_b:?}"
    );
    assert_eq!(track.tracked(), 2, "A is absent, not dropped");
    track.fold(&panel(&[a(40, "310.0k"), b(40)]), &[], T0 + 40_000);
    let [back_a, b_again] = track.dots() else {
        panic!()
    };
    assert!((back_a.rate - 250.0).abs() < 1.0, "{back_a:?}");
    assert_eq!(b_again.quiet_ms, 40_000);
    assert_eq!(track.tracked(), 2);

    let mut a_only = PanelTrack::default();
    a_only.fold(&panel(&[a(0, "300.0k")]), &[], T0);
    let a_track = a_only.tracks.iter().flatten().next().unwrap();
    let b_row = parse_row(&b(30), one_cell).unwrap();
    assert_eq!(a_track.ordinal, 0);
    assert_eq!(a_track.fit(&b_row, 0, T0 + 30_000), None);
    let word = parse_row("  ◯ general-purpose  Reading one file   waiting", one_cell).unwrap();
    let mut words = PanelTrack::default();
    words.fold(
        &panel(&["  ◯ general-purpose  Sweeping the crates   waiting".into()]),
        &[],
        T0,
    );
    let w = words.tracks.iter().flatten().next().unwrap();
    assert!(w.fit(&word, 0, T0 + 1_000).is_some());
    assert_eq!(w.fit(&word, 1, T0 + 1_000), None);
}

#[test]
fn absent_tracks_drop_and_the_table_stays_bounded() {
    let mut track = PanelTrack::default();
    track.fold(
        &panel(&[agent("reviewer", "Reviewing", 5, "1.0k")]),
        &[],
        T0,
    );
    track.fold(&AgentsPanel::EMPTY, &[], T0 + ABSENT_MS - 1);
    assert_eq!((track.tracked(), track.dots().len()), (1, 0));
    track.fold(&AgentsPanel::EMPTY, &[], T0 + ABSENT_MS);
    assert_eq!(track.tracked(), 0);

    let mut track = PanelTrack::default();
    for n in 0..20u64 {
        let row = agent(&format!("worker-{n}"), "Working", 5, "1.0k");
        track.fold(&panel(&[row]), &[], T0 + n * 1000);
        assert!(track.tracked() <= TRACKS_MAX);
    }
    assert_eq!(track.tracked(), TRACKS_MAX);
    assert_eq!(track.dots().len(), 1);
}

#[test]
fn claude_coloured_rows_are_settled_and_words_say_their_state() {
    let row = [agent("general-purpose", "Done reviewing", 95, "40.1k")];
    let mut marked = PanelTrack::default();
    marked.fold(&panel(&row), &[Tint::Marked], T0);
    marked.retime(T0 + STUCK_MS * 2);
    assert_eq!(
        (only(&marked).state, only(&marked).sat),
        (DotState::Settled, 1.0)
    );
    assert_eq!(marked.due(), None);
    let mut plain = PanelTrack::default();
    plain.fold(&panel(&row), &[Tint::Plain], T0);
    assert_eq!(only(&plain).state, DotState::Running);

    for (row, state) in [
        (
            "  ◯ lead-teammate  Planning the split   awaiting approval",
            DotState::Awaiting,
        ),
        (
            "  ◯ lead-teammate  Planning the split   idle",
            DotState::Idle,
        ),
        (
            "  ◯ lead-teammate  Planning the split   waiting",
            DotState::Waiting,
        ),
        (
            "  ⏸ nightly-sweep  ▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  1/4 · 3m02s · ↓ 12.4k tokens",
            DotState::Paused,
        ),
    ] {
        let mut track = PanelTrack::default();
        track.fold(&panel(&[row.into()]), &[], T0);
        track.retime(T0 + STUCK_MS * 2);
        assert_eq!(only(&track).state, state, "{row}");
    }
}

#[test]
fn only_task_rows_are_dots_and_the_fingerprint_moves_with_the_looks() {
    let rows = vec![
        "  ⏺ main".to_owned(),
        agent("general-purpose", "Searching", 30, "10.0k"),
        "  ◯ 2 idle agents".to_owned(),
        "  ↓ 3 more".to_owned(),
        "  ◯ nightly-sweep  ▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  1/4 · 3m02s · ↓ 12.4k tokens".to_owned(),
    ];
    let mut track = PanelTrack::default();
    track.fold(&panel(&rows), &[], T0);
    let kinds: Vec<_> = track.dots().iter().map(|d| (d.row, d.kind)).collect();
    assert_eq!(kinds, [(1, RowKind::Agent), (4, RowKind::Workflow)]);
    assert_eq!(track.dots()[0].bullet, Span { start: 2, end: 3 });

    let print = track.fingerprint();
    track.retime(T0 + 1_000);
    assert_eq!(track.fingerprint(), print, "nothing that shows moved");
    track.retime(T0 + QUIET_MS);
    assert_ne!(track.fingerprint(), print, "a state turned");
    assert_eq!(level(0.0), 0.0);
    assert_eq!(level(1_000.0), 1.0);
    assert_eq!(level(1e9), 1.0);
}

#[test]
fn agents_of_one_name_launched_together_keep_their_own_tracks() {
    let k = |n: u64| format!("{}.{}k", n / 1000, n % 1000 / 100);
    let row = |start: u64, t: u64, tokens: u64| {
        agent("general-purpose", "Working", (t - start) / 1000, &k(tokens))
    };
    let mut lcg: u64 = 0x2545_F491_4F6C_DD1D;
    let mut renders = Vec::new();
    for s in 1..120u64 {
        renders.push(s * 1000 + 700);
        lcg = lcg
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        renders.push(s * 1000 + (lcg >> 33) % 1000);
    }
    renders.sort_unstable();
    let err = |cost: Option<u64>| cost.map(|c| c & ((1 << 56) - 1));
    let mut track = PanelTrack::default();
    let (mut traded, mut error_alone_trades) = (0, 0);
    let mut slots: Option<[usize; 2]> = None;
    for &t in &renders {
        let (a, b) = (50_000 + 300 * t / 1000, 2_000 + 20 * t / 1000);
        let p = panel(&[row(0, t, a), row(400, t, b)]);
        let now = T0 + t + 5;
        if let Some([ta, tb]) = slots {
            let tracks = [ta, tb].map(|s| track.tracks[s].as_ref().unwrap());
            let row_a = &p.rows()[0];
            if let (Some(cross), Some(own)) = (
                err(tracks[1].fit(row_a, 0, now)),
                err(tracks[0].fit(row_a, 0, now)),
            ) && cross < own
            {
                error_alone_trades += 1;
            }
        }
        track.fold(&p, &[], now);
        let [da, db] = track.dots() else { panic!() };
        if t > 20_000 && !(da.rate > 150.0 && db.rate < 100.0) {
            traded += 1;
        }
        let first = [
            usize::from(track.dot_track[0]),
            usize::from(track.dot_track[1]),
        ];
        assert_eq!(*slots.get_or_insert(first), first, "at {t} ms");
    }
    assert_eq!(traded, 0, "folds whose dots read the other agent's history");
    assert!(error_alone_trades > 0, "the error alone would have traded");
    assert_eq!(track.tracked(), 2);
}

#[test]
fn rows_back_after_a_long_absence_each_get_their_own_track() {
    let mut track = PanelTrack::default();
    track.fold(&panel(&[agent("builder", "Building", 5, "1.0k")]), &[], T0);
    track.fold(
        &panel(&[
            agent("reviewer", "Reviewing", 1, "2.0k"),
            agent("builder", "Building", 6, "1.0k"),
        ]),
        &[],
        T0 + 1_000,
    );
    assert_eq!(track.dot_track[..2], [1, 0], "rows and slots cross");
    let gone = (ABSENT_MS + 5_000) / 1000;
    track.fold(
        &panel(&[
            agent("reviewer", "Reviewing", 1 + gone, "2.5k"),
            agent("builder", "Building", 6 + gone, "1.5k"),
        ]),
        &[],
        T0 + 1_000 + gone * 1000,
    );
    let rows: Vec<u16> = track.dots().iter().map(|d| d.row).collect();
    assert_eq!(rows, [0, 1], "{:?}", track.dots());
    assert_eq!(track.tracked(), 2);
    assert_ne!(track.dot_track[0], track.dot_track[1]);
}

#[test]
fn a_row_with_no_status_goes_quiet_by_its_text() {
    let fold_for = |text: &dyn Fn(u64) -> String, until: u64| {
        let mut track = PanelTrack::default();
        for s in 0..=until {
            let row = format!("  ◯ general-purpose  {}", text(s));
            track.fold(&panel(&[row]), &[], T0 + s * 1000);
        }
        only(&track)
    };
    let still = fold_for(&|_| "deploy: green · 3 checks".into(), 200);
    assert_eq!(
        (still.state, still.blind, still.quiet_ms),
        (DotState::Quiet, true, 200_000)
    );
    let moving = fold_for(&|s| format!("deploy: {s} checks"), 200);
    assert_eq!(moving.state, DotState::Running);

    let mut track = PanelTrack::default();
    for s in 0..=300u64 {
        let row = "  ◯ lead-teammate  Planning the split   waiting".to_owned();
        track.fold(&panel(&[row]), &[], T0 + s * 1000);
    }
    assert_eq!(only(&track).state, DotState::Waiting);
    let resumed = agent("lead-teammate", "Planning the split", 301, "12.0k");
    track.fold(&panel(&[resumed]), &[], T0 + 301_000);
    assert_eq!(
        (only(&track).state, only(&track).quiet_ms),
        (DotState::Running, 0)
    );
}

use crate::harness::flight::{
    FileFlight, Flight, FlightFold, FlightTable, Flyer, RowKey, ToolName, combine, place,
};

fn bash_at(now: u64) -> Flight {
    let until = T0 + 430_000;
    Flight::Tool {
        call: 7,
        tool: ToolName::of("Bash"),
        age_ms: now - T0,
        left_ms: until.saturating_sub(now),
    }
}

fn quiet_row(track: &mut PanelTrack, now: u64, flight: Flight) {
    let row = agent(
        "general-purpose",
        "long probe",
        (now - T0) / 1000 + 2,
        "6.2k",
    );
    track.fold_flown(&panel(&[row]), &[], &[flight], now);
}

#[test]
fn a_long_call_in_flight_reads_tool_not_stuck() {
    let mut track = PanelTrack::default();
    let mut control = PanelTrack::default();
    for s in [0, 10, 100, 149, 150, 160, 300] {
        let now = T0 + s * 1000;
        quiet_row(&mut track, now, bash_at(now));
        quiet_row(&mut control, now, Flight::Model);
    }
    let dot = only(&track);
    assert_eq!(dot.state, DotState::Tool);
    assert_eq!(dot.sat, 1.0, "fresh");
    assert_eq!(dot.level, 0.0, "no tokens moved: no glow");
    let waits = dot.waits.expect("names its tool");
    assert_eq!((waits.tool.as_str(), waits.for_ms), ("Bash", 300_000));
    assert_eq!(only(&control).state, DotState::Stuck);
    assert_eq!(only(&control).waits, None);
    let (mut a, mut b) = (PanelTrack::default(), PanelTrack::default());
    for s in [0, 19] {
        let now = T0 + s * 1000;
        quiet_row(&mut a, now, bash_at(now));
        quiet_row(&mut b, now, Flight::Model);
        assert_eq!(only(&a).state, DotState::Running);
        assert_eq!(only(&a), only(&b));
    }
}

#[test]
fn a_call_older_than_the_panels_clock_says_its_own_age() {
    let flown = |age_ms: u64| Flight::Tool {
        call: 7,
        tool: ToolName::of("Bash"),
        age_ms,
        left_ms: 430_000 - age_ms,
    };
    let row = |secs: u64| agent("general-purpose", "long probe", secs, "6.2k");
    for (age_at_5s, want) in [(200_000, 225_000), (5_000, 30_000)] {
        let mut track = PanelTrack::default();
        track.fold_flown(
            &panel(&[row(age_at_5s / 1000)]),
            &[],
            &[flown(age_at_5s)],
            5_000,
        );
        let now = 30_000;
        track.fold_flown(
            &panel(&[row((age_at_5s + 25_000) / 1000)]),
            &[],
            &[flown(age_at_5s + 25_000)],
            now,
        );
        let dot = only(&track);
        assert_eq!(dot.state, DotState::Tool, "{age_at_5s}");
        assert_eq!(dot.waits.map(|w| w.for_ms), Some(want), "{age_at_5s}");
        track.retime(now + 1_500);
        assert_eq!(only(&track).waits.map(|w| w.for_ms), Some(want + 1_500));
    }
}

#[test]
fn a_call_past_its_own_budget_reads_stuck() {
    let at = |s: u64| {
        let mut track = PanelTrack::default();
        quiet_row(&mut track, T0, bash_at(T0));
        let now = T0 + s;
        quiet_row(&mut track, now, bash_at(now));
        (only(&track).state, track.due())
    };
    assert_eq!(at(429_999).0, DotState::Tool);
    assert_eq!(at(430_000).0, DotState::Stuck);
    let (_, due) = at(100_000);
    assert_eq!(due, Some(T0 + 100_000 + 3_000), "the halt check");
    let mut control = PanelTrack::default();
    quiet_row(&mut control, T0, Flight::Model);
    quiet_row(&mut control, T0 + 100_000, Flight::Model);
    assert_eq!(only(&control).state, DotState::Quiet);
    assert_eq!(control.due(), Some(T0 + 101_000), "a quiet dot fades");
    let mut near = PanelTrack::default();
    quiet_row(&mut near, T0, bash_at(T0));
    quiet_row(&mut near, T0 + 429_000, bash_at(T0 + 429_000));
    assert_eq!(near.due(), Some(T0 + 430_000), "the budget's end");
}

#[test]
fn an_unknown_flight_keeps_todays_states() {
    for flight in [Flight::Unknown, Flight::Model] {
        let (mut flown, mut plain) = (PanelTrack::default(), PanelTrack::default());
        for s in (0..400).step_by(10) {
            let now = T0 + s * 1000;
            quiet_row(&mut flown, now, flight);
            let row = agent("general-purpose", "long probe", s + 2, "6.2k");
            plain.fold(&panel(&[row]), &[], now);
            assert_eq!(flown.dots(), plain.dots(), "{flight:?} at {s} s");
            assert_eq!(flown.due(), plain.due());
            assert_eq!(flown.fingerprint(), plain.fingerprint());
        }
    }
}

#[test]
fn an_answered_call_is_evidence() {
    let mut track = PanelTrack::default();
    quiet_row(&mut track, T0, bash_at(T0));
    quiet_row(&mut track, T0 + 90_000, Flight::Unknown);
    assert_eq!(only(&track).state, DotState::Quiet, "unsure: the screen's");
    quiet_row(&mut track, T0 + 100_000, Flight::Model);
    assert_eq!(only(&track).state, DotState::Running, "answered");
    quiet_row(&mut track, T0 + 249_000, Flight::Model);
    assert_eq!(only(&track).state, DotState::Quiet);
    quiet_row(&mut track, T0 + 250_000, Flight::Model);
    assert_eq!(only(&track).state, DotState::Stuck);
    let mut control = PanelTrack::default();
    quiet_row(&mut control, T0, bash_at(T0));
    quiet_row(&mut control, T0 + 100_000, bash_at(T0 + 100_000));
    assert_eq!(only(&control).quiet_ms, 100_000, "the same call");
}

#[test]
fn the_files_can_move_while_the_screen_does_not() {
    let mut track = PanelTrack::default();
    quiet_row(&mut track, T0, bash_at(T0));
    quiet_row(&mut track, T0 + 200_000, bash_at(T0 + 200_000));
    assert_eq!(only(&track).state, DotState::Tool);
    let mut control = track.clone();
    track.fly(&[Flight::Model], T0 + 201_000);
    assert_eq!(only(&track).state, DotState::Running, "answered");
    control.retime(T0 + 201_000);
    assert_eq!(only(&control).state, DotState::Tool);
}

fn flight_file(name: &str) -> FlightFold {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/harness/testdata/flight")
        .join(name);
    let mut fold = FlightFold::new();
    for row in std::fs::read_to_string(path).unwrap().lines() {
        fold.fold_bytes(row.as_bytes());
    }
    fold
}

fn fold_screen(
    track: &mut PanelTrack,
    screen: &[String],
    t: u64,
    flights: Option<(&FlightTable, i64)>,
) {
    let panel = panel_of(screen);
    let keys: Vec<Option<RowKey>> = panel
        .rows()
        .iter()
        .map(|row| RowKey::of(&screen[usize::from(row.row)], row))
        .collect();
    match flights {
        Some((table, wall)) => track.fold_flown(&panel, &[], &place(&keys, table, wall), t),
        None => track.fold(&panel, &[], t),
    }
}

#[test]
fn the_measured_long_call_reads_tool_not_stuck() {
    let (early, late) = (
        fixture("claude-2.1.304-agents-panel-long-bash-8s-120col.txt"),
        fixture("claude-2.1.304-agents-panel-long-bash-4m57s-120col.txt"),
    );
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/harness/testdata/flight/claude-2.1.304-long-bash.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut long = FlightFold::new();
    for row in text.lines().take(3) {
        long.fold_bytes(row.as_bytes());
    }
    assert!(matches!(long.flight(), FileFlight::Tool(_)));
    let start = long.start_ms().unwrap();
    let table = FlightTable {
        flyers: vec![Flyer::agent(
            "general-purpose",
            Some("long probe"),
            start,
            long.flight(),
        )],
    };
    let (mut flown, mut screen_only) = (PanelTrack::default(), PanelTrack::default());
    fold_screen(&mut flown, &early, T0, Some((&table, start + 8_400)));
    fold_screen(&mut screen_only, &early, T0, None);
    fold_screen(
        &mut flown,
        &late,
        T0 + 289_000,
        Some((&table, start + 297_400)),
    );
    fold_screen(&mut screen_only, &late, T0 + 289_000, None);
    let long_dot = |t: &PanelTrack| t.dots()[0];
    assert_eq!(long_dot(&screen_only).state, DotState::Stuck, "control");
    assert_eq!(long_dot(&flown).state, DotState::Tool);
    let waits = long_dot(&flown).waits.unwrap();
    assert_eq!(waits.tool.as_str(), "Bash");
    assert!(
        (297_000..298_500).contains(&waits.for_ms),
        "{}",
        waits.for_ms
    );
}

#[test]
fn the_measured_workflow_reads_tool_not_stuck() {
    let (early, late) = (
        fixture("claude-2.1.304-agents-panel-workflow-long-bash-8s-120col.txt"),
        fixture("claude-2.1.304-agents-panel-workflow-long-bash-2m46s-120col.txt"),
    );
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/harness/testdata/flight/claude-2.1.304-workflow-long.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut long = FlightFold::new();
    for row in text.lines().take(4) {
        long.fold_bytes(row.as_bytes());
    }
    let quick = flight_file("claude-2.1.304-workflow-quick.jsonl");
    let start = 1_792_512_327_967i64;
    let table = FlightTable {
        flyers: vec![Flyer::workflow(
            "stall-probe",
            start,
            combine([long.flight(), quick.flight()]),
        )],
    };
    let (mut flown, mut screen_only) = (PanelTrack::default(), PanelTrack::default());
    fold_screen(&mut flown, &early, T0, Some((&table, start + 8_400)));
    fold_screen(&mut screen_only, &early, T0, None);
    fold_screen(
        &mut flown,
        &late,
        T0 + 158_000,
        Some((&table, start + 166_400)),
    );
    fold_screen(&mut screen_only, &late, T0 + 158_000, None);
    assert_eq!(screen_only.dots()[0].state, DotState::Stuck, "control");
    assert_eq!(flown.dots()[0].state, DotState::Tool);
}
