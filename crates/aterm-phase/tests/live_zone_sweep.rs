// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIVE ZONE IS BLIND TO THE BLANK ROWS UNDER WHAT IS DRAWN.
//!
//! The server's agent verdict and a supervisor whose tail read ends on a blank
//! row read a screen's LIVE ZONE: its last 40 DRAWN rows and the blank rows
//! under them ([`aterm_phase::live_zone_start`]). The grid's last 40 rows were
//! the zone before the review of 2026-09-26, which found Claude Code's inline
//! renderer drawing its REPL at the top of a 50-row pane, blank rows below it,
//! and the prompt box's caret above the last 40 rows: never read, `agent=`
//! stuck at `unknown`.
//!
//! This sweep holds the zone to what that change may and may not move, over
//! every fixture FILE under `aterm-phase/src/fixtures/` and (in the aterm
//! workspace) `aterm-agent/src/supervise/policy/fixtures/`, read by Claude
//! Code's reader, Codex's and the screen-identified one:
//!
//! * a screen drawn to its last row (a fullscreen REPL, a Codex composer, a
//!   box drawn to the bottom) has exactly the zone the grid's last 40 rows
//!   gave — so every reading of it is what it was;
//! * the same screen in a taller pane — 7, 30 and 60 blank rows added below
//!   it — reads exactly as it reads in a pane as tall as what is drawn: the
//!   blank rows under the drawing decide nothing, whatever the program.

use std::path::{Path, PathBuf};

use aterm_phase::live_zone_start;
use aterm_phase::prompt::fixtures as fx;

/// The zone the server's agent verdict reads (`presence::CLASSIFY_ROWS`).
const ZONE_ROWS: usize = 40;

/// A fixture file's rows, loaded as the panic sweep loads them: the
/// provenance line dropped, a saved waiter capture's `== …` head and `exit=`
/// tail dropped.
fn file_rows(text: &str) -> Vec<String> {
    let mut r = fx::screen(text);
    if r.first().is_some_and(|l| l.starts_with("== ")) {
        r.remove(0);
    }
    if r.last().is_some_and(|l| l.starts_with("exit=")) {
        r.pop();
    }
    r
}

fn screens() -> Vec<(String, Vec<String>)> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut dirs = vec![crate_dir.join("src/fixtures")];
    let agent = crate_dir.join("../aterm-agent/src/supervise/policy/fixtures");
    if agent.is_dir() {
        dirs.push(agent);
    } else {
        eprintln!(
            "live zone sweep: {} absent (not in the aterm workspace): its captures are not swept",
            agent.display()
        );
    }
    let mut out = Vec::new();
    for dir in dirs {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|e| e.expect("a fixture entry").path())
            .filter(|p| p.is_file())
            .collect();
        paths.sort();
        assert!(!paths.is_empty(), "{}: no fixtures", dir.display());
        for p in paths {
            let text =
                std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            let name = p
                .strip_prefix(crate_dir)
                .unwrap_or(&p)
                .display()
                .to_string();
            out.push((name, file_rows(&text)));
        }
    }
    out
}

fn zone(rows: &[String]) -> &[String] {
    &rows[live_zone_start(rows, ZONE_ROWS)..]
}

/// Every reading a caller takes of a zone, as one comparable string: the
/// reader named by the program (Claude Code's, Codex's) and the one the
/// screen identifies, with and without a cursor column.
fn readings(rows: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for program in [Some("claude"), Some("codex"), None] {
        for cursor in [None, Some(2)] {
            out.push(format!(
                "{program:?} {cursor:?} {:?}",
                aterm_phase::read(program, rows, cursor)
            ));
        }
    }
    out
}

fn blank(r: &str) -> bool {
    r.trim().is_empty()
}

#[test]
fn a_screen_drawn_to_its_last_row_has_the_zone_the_last_40_rows_gave() {
    let mut drawn_to_bottom = 0;
    for (name, rows) in screens() {
        if rows.last().is_some_and(|r| !blank(r)) {
            drawn_to_bottom += 1;
            assert_eq!(
                live_zone_start(&rows, ZONE_ROWS),
                rows.len().saturating_sub(ZONE_ROWS),
                "{name}"
            );
        }
    }
    assert!(drawn_to_bottom > 50, "only {drawn_to_bottom} such screens");
}

#[test]
fn blank_rows_under_the_drawing_decide_nothing() {
    let (mut swept, mut moved_from_the_old_cut) = (0, Vec::new());
    for (name, rows) in screens() {
        // The screen in a pane exactly as tall as what is drawn on it.
        let drawn = rows.iter().rposition(|r| !blank(r)).map_or(0, |i| i + 1);
        let tight = &rows[..drawn];
        let want = readings(zone(tight));
        for pad in [0, 7, 30, 60] {
            let mut tall = rows.clone();
            tall.extend(std::iter::repeat_n(String::new(), pad));
            let got = readings(zone(&tall));
            for (w, g) in want.iter().zip(&got) {
                assert_eq!(w, g, "{name} with {pad} blank rows below");
            }
            // What the grid's last 40 rows read instead (for the report of
            // what the zone changed: only screens with blank rows under a
            // drawing taller than what the old cut kept).
            let old = &tall[tall.len().saturating_sub(ZONE_ROWS)..];
            if readings(old) != got {
                moved_from_the_old_cut.push(format!("{name} +{pad}"));
            }
            swept += 1;
        }
    }
    eprintln!(
        "live zone sweep: {swept} screens; {} read otherwise on the grid's last {ZONE_ROWS} rows: {moved_from_the_old_cut:#?}",
        moved_from_the_old_cut.len()
    );
    assert!(swept > 200, "only {swept} screens swept");
}
