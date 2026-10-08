// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIVE-ZONE SWEEP: over every measured screen, the live zone
//! ([`aterm_phase::busy_zone_start`]) keeps every row the busy reading reads.
//!
//! A wait on the busy row (`await gone esc.to.interrupt rows <start> …`, the
//! supervisor's) and the live upgrade's busy footer look only at the rows
//! from the live zone's first row down: a dead process's `esc to interrupt`
//! footer above them stops counting (live, 2026-09-27: `/exit`, then a
//! fresh `claude` with no `clear`, held every whole-screen wait for its
//! 20 s step). The one misreading that cut may never make is the other way
//! round — a LIVE busy row left out, a running turn waited on as over. So
//! for every fixture FILE under `aterm-phase/src/fixtures/` and (in the
//! aterm workspace) `aterm-agent/src/supervise/policy/fixtures/`, whole and
//! at every tail cut:
//!
//! * the zone starts on the screen (`<= rows.len()`);
//! * with Claude Code's composer frame and no exited process's resume hint
//!   under its top rule, the zone starts at or above the top rule — the
//!   frame and its footer are in it — and at or above the box's first row
//!   when a box is up with it;
//! * the busy reading ([`aterm_phase::busy_signal`]) is the same with every
//!   row above the zone blanked: nothing it reads is above the zone.
//!
//! And with no frame, a box's first row is never under the zone's start.

use std::path::{Path, PathBuf};

use aterm_phase::prompt::fixtures as fx;

/// A fixture file's rows, read as `panic_sweep.rs` reads them: the
/// provenance line (`# …`) dropped, and a saved waiter capture's `== …`
/// head and `exit=` tail.
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

fn dir_screens(dir: &Path, into: &mut Vec<(String, Vec<String>)>) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("a fixture entry").path())
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "{}: no fixtures", dir.display());
    for p in paths {
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        into.push((name, file_rows(&text)));
    }
}

fn screens() -> Vec<(String, Vec<String>)> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    dir_screens(&crate_dir.join("src/fixtures"), &mut out);
    let agent = crate_dir.join("../aterm-agent/src/supervise/policy/fixtures");
    if agent.is_dir() {
        dir_screens(&agent, &mut out);
    }
    out
}

/// `rows` with every row above `start` blank.
fn blank_above(rows: &[String], start: usize) -> Vec<String> {
    rows.iter()
        .enumerate()
        .map(|(i, r)| if i < start { String::new() } else { r.clone() })
        .collect()
}

/// Whether an exited process's resume hint sits at or under `row`.
fn exited_under(rows: &[String], row: usize) -> bool {
    let notice = aterm_phase::anchor("exit.resume");
    rows[row..].iter().any(|r| r.trim_end() == notice)
}

fn check(name: &str, rows: &[String]) -> Result<(), String> {
    let start = aterm_phase::busy_zone_start(rows);
    if start > rows.len() {
        return Err(format!(
            "{name}: the zone starts at {start}, past {} rows",
            rows.len()
        ));
    }
    let boxed = aterm_phase::prompt_box_first_row(rows);
    match aterm_phase::phase::composer_rules(rows) {
        Some((top, _)) if !exited_under(rows, top) => {
            if start > top {
                return Err(format!(
                    "{name}: the zone starts at {start}, under the top rule {top}"
                ));
            }
            if let Some(b) = boxed.filter(|&b| start > b) {
                return Err(format!(
                    "{name}: the zone starts at {start}, under the box at {b}"
                ));
            }
            let whole = aterm_phase::busy_signal(rows);
            let zone = aterm_phase::busy_signal(&blank_above(rows, start));
            if whole != zone {
                return Err(format!(
                    "{name}: the busy reading moved when the rows above {start} were blanked: \
                     {whole:?} -> {zone:?}"
                ));
            }
        }
        Some(_) => {}
        None => {
            if let Some(b) = boxed.filter(|&b| start > b && !exited_under(rows, b)) {
                return Err(format!(
                    "{name}: the zone starts at {start}, under the box at {b}"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn the_live_zone_keeps_every_row_the_busy_reading_reads() {
    let screens = screens();
    let mut bad = Vec::new();
    let mut framed = 0;
    for (name, rows) in &screens {
        framed += usize::from(aterm_phase::phase::composer_rules(rows).is_some());
        for k in 0..rows.len() {
            if let Err(e) = check(&format!("{name} [{k}..]"), &rows[k..]) {
                bad.push(e);
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} violations:\n{}",
        bad.len(),
        bad.join("\n")
    );
    // Non-vacuity: the sweep read real framed screens, the stale captures
    // among them.
    assert!(
        screens.len() > 100 && framed > 30,
        "{} screens, {framed} framed",
        screens.len()
    );
    assert!(
        screens
            .iter()
            .any(|(n, _)| n == "claude-2.1.283-exit-relaunch-stale-busy.txt")
    );
}
