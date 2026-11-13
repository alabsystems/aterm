// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::progress::Overall;
use std::collections::BTreeMap;

fn row(phase: Phase, done: u64, total: u64) -> ProgramProgress {
    ProgramProgress {
        phase,
        bytes_done: done,
        bytes_total: total,
        build: None,
        bumped: false,
        error: None,
    }
}

fn file(rows: &[(&str, ProgramProgress)], overall: Overall) -> ProgressFile {
    ProgressFile {
        v: crate::progress::PROGRESS_VERSION,
        pid: Some(1),
        pass: "net".into(),
        started_unix: 0,
        generation: None,
        heartbeat_unix: 0,
        overall,
        queue: Vec::new(),
        programs: rows
            .iter()
            .map(|(n, r)| ((*n).to_string(), r.clone()))
            .collect::<BTreeMap<_, _>>(),
        ended_unix: None,
    }
}

fn pass(file: ProgressFile) -> Show {
    Show::Pass {
        file,
        focus: None,
        idle: "Checking for updates…".into(),
    }
}

const WIDE: Look = Look {
    cols: 120,
    frame: 2,
    utf8: true,
};

const GIB: u64 = 1 << 30;

/// At 80 columns — a terminal's default — the time left is never the part cut: the
/// rate goes first, then the bar narrows.
#[test]
fn the_time_left_survives_an_eighty_column_terminal() {
    let f = file(
        &[("trust", row(Phase::Download, GIB, 2 * GIB))],
        Overall {
            programs_done: 2,
            programs_total: 12,
            bytes_done: GIB,
            bytes_total: 3 * GIB,
        },
    );
    let eighty = Look {
        cols: 80,
        frame: 2,
        utf8: true,
    };
    let line = render_line(&pass(f), Some(18.0 * 1024.0 * 1024.0), &eighty);
    assert!(line.contains("trust") && line.ends_with(" left"), "{line}");
    assert!(line.chars().count() < 80, "{line}");
}

/// A program still downloading outranks one being set up; a pass with nothing in
/// flight says its idle sentence.
#[test]
fn the_line_features_the_program_in_flight_else_the_idle_sentence() {
    let f = file(
        &[
            ("ay", row(Phase::Link, 0, 0)),
            ("trust", row(Phase::Verify, 0, 0)),
            ("zz", row(Phase::Done, 0, 0)),
        ],
        Overall::default(),
    );
    let line = render_line(&pass(f), None, &WIDE);
    assert!(line.contains("trust") && !line.contains("ay"), "{line}");
    let idle = file(&[("zz", row(Phase::Done, 0, 0))], Overall::default());
    assert!(render_line(&pass(idle), None, &WIDE).ends_with("Checking for updates…"));
    assert!(render_line(&Show::Busy("Waiting…".into()), None, &WIDE).ends_with(" Waiting…"));
    assert_eq!(render_line(&Show::Nothing, None, &WIDE), "");
}

/// An archive fetched ahead and waiting for the pass moves nothing: the program being
/// unpacked, or one being set up, is the one featured — the waiting download only
/// when nothing else is in flight.
#[test]
fn a_download_waiting_for_the_pass_yields_to_the_program_in_hand() {
    let f = file(
        &[
            ("ay", row(Phase::Download, 2, 2)),
            ("trust", row(Phase::Extract, 3, 4)),
        ],
        Overall::default(),
    );
    assert_eq!(
        render_line(&pass(f), None, &WIDE),
        "⠹ Unpacking trust  ▕████████████    ▏ 75%"
    );
    let f = file(
        &[
            ("ay", row(Phase::Download, 2, 2)),
            ("trust", row(Phase::Link, 0, 0)),
        ],
        Overall::default(),
    );
    assert_eq!(render_line(&pass(f), None, &WIDE), "⠹ Setting up trust…");
    let f = file(&[("ay", row(Phase::Download, 2, 2))], Overall::default());
    assert!(
        render_line(&pass(f), None, &WIDE).starts_with("⠹ Downloading ay"),
        "nothing else in flight"
    );
}

/// A focus names the program a pending stub waits for, whatever else is in flight, and
/// never shows the pass's position or its whole download's time left, which are not that
/// program's.
#[test]
fn a_focus_features_its_own_program() {
    let overall = Overall {
        programs_done: 2,
        programs_total: 12,
        bytes_done: GIB,
        bytes_total: 20 * GIB,
    };
    let f = file(
        &[
            ("trust", row(Phase::Download, 1, 2)),
            ("ty", row(Phase::Queued, 0, 0)),
        ],
        overall,
    );
    let show = Show::Pass {
        file: f,
        focus: Some("ty".into()),
        idle: String::new(),
    };
    let line = render_line(&show, None, &WIDE);
    assert!(line.contains("ty") && !line.contains("trust"), "{line}");
    let f = file(&[("ty", row(Phase::Download, 1 << 20, 2 << 20))], overall);
    let show = Show::Pass {
        file: f,
        focus: Some("ty".into()),
        idle: String::new(),
    };
    let line = render_line(&show, Some(f64::from(1u32 << 20)), &WIDE);
    assert!(line.contains("ty") && line.ends_with("5 s left"), "{line}");
    assert!(!line.contains("of 12"), "not the pass's position: {line}");
}

/// ASCII terminals get ASCII glyphs; narrow ones lose the bar, then get cut with a
/// visible ellipsis — never a line that wraps.
#[test]
fn ascii_and_narrow_terminals() {
    let f = file(&[("trust", row(Phase::Download, 1, 2))], Overall::default());
    let ascii = Look {
        cols: 80,
        frame: 1,
        utf8: false,
    };
    let line = render_line(&pass(f.clone()), None, &ascii);
    assert!(
        line.is_ascii() && line.contains("[########        ]"),
        "{line}"
    );
    let narrow = Look {
        cols: 30,
        frame: 0,
        utf8: true,
    };
    let line = render_line(&pass(f), None, &narrow);
    assert_eq!(line.chars().count(), 29, "{line}");
    assert!(line.ends_with('…'), "{line}");
    assert!(!line.contains('▕'), "{line}");
}

/// An untrusted name the store would never admit is not echoed to the terminal.
#[test]
fn a_hostile_program_name_is_not_echoed() {
    let f = file(
        &[("\u{1b}]0;pwned\u{7}", row(Phase::Verify, 0, 0))],
        Overall::default(),
    );
    let line = render_line(&pass(f), None, &WIDE);
    assert!(
        !line.contains('\u{1b}') && !line.contains("pwned"),
        "{line:?}"
    );
}

#[test]
fn the_bar_fills_in_eighths_and_never_overflows() {
    assert_eq!(bar(0, 8, 4, true), "▕    ▏");
    assert_eq!(bar(1, 8, 4, true), "▕▌   ▏");
    assert_eq!(bar(8, 8, 4, true), "▕████▏");
    assert_eq!(bar(99, 8, 4, true), "▕████▏");
    assert_eq!(bar(4, 8, 4, false), "[##  ]");
    assert_eq!(bar(0, 0, 4, false), "[    ]");
}

/// The rate waits for bytes to move for three seconds, averages over the last five,
/// and starts over when the count goes down (a new pass).
#[test]
fn the_rate_settles_then_averages_over_the_window() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let mut rate = Rate::default();
    assert_eq!(rate.note(at(0), 0), None);
    assert_eq!(rate.note(at(1), 100), None, "moving, not settled");
    assert_eq!(rate.note(at(3), 300), None, "two seconds of movement");
    let r = rate.note(at(4), 400).expect("settled");
    assert!((r - 100.0).abs() < 1e-9, "{r}");
    for s in 5..=14 {
        rate.note(at(s), 400 + (s - 4) * 1000);
    }
    let r = rate.note(at(15), 400 + 11 * 1000).expect("settled");
    assert!((r - 1000.0).abs() < 1.0, "{r}");
    assert_eq!(rate.note(at(16), 10), None, "a new pass starts over");
}
