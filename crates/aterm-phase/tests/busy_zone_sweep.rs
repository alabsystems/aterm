// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIVE-ZONE SWEEP: over every measured screen, the live zone
//! ([`aterm_phase::busy_zone_start`]) keeps every row the busy reading reads.
//!
//! A wait on the busy row (`await gone esc.to.interrupt rows <start> …`, the
//! supervisor's) and the live upgrade's busy footer look only at the rows
//! from the live zone's first row down: a dead process's `esc to interrupt`
//! footer above them stops counting (`/exit`, then a fresh `claude` with
//! no `clear`, held every whole-screen wait for its 20 s step). The one
//! misreading that cut may never make is the other way round — a LIVE busy
//! row left out, a running turn waited on as over. So
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

use crate::fixture_files::fixture_screens;

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
    let screens = fixture_screens("busy zone sweep");
    // One job per tail cut: a tall capture's cuts are most of the sweep.
    let cuts: Vec<(&str, &[String], usize)> = screens
        .iter()
        .flat_map(|(name, rows)| (0..rows.len()).map(move |k| (name.as_str(), &rows[..], k)))
        .collect();
    let bad: Vec<String> = crate::on_every_core(&cuts, |&(name, rows, k)| {
        check(&format!("{name} [{k}..]"), &rows[k..]).err()
    })
    .into_iter()
    .flatten()
    .collect();
    let framed = screens
        .iter()
        .filter(|(_, rows)| aterm_phase::phase::composer_rules(rows).is_some())
        .count();
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
    assert!(screens.iter().any(|(n, _)| {
        std::path::Path::new(n).ends_with("claude-2.1.283-exit-relaunch-stale-busy.txt")
    }));
}
