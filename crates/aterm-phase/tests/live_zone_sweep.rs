// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIVE ZONE IS BLIND TO THE BLANK ROWS UNDER WHAT IS DRAWN, AND COUNTS
//! A BLANK GAP AS ONE ROW.
//!
//! The server's agent verdict and a supervisor whose tail read ends on a blank
//! row (or holds a gap) read a screen's LIVE ZONE: its last 40 DRAWN rows and
//! the blank rows under them, a run of [`aterm_phase::BLANK_GAP`] blank rows
//! or more counting as one ([`aterm_phase::live_zone_start`]). The grid's last
//! 40 rows are not the zone: Claude Code's inline renderer can draw its REPL at
//! the top of a 50-row pane with blank rows below, the prompt box's caret above
//! the last 40 rows; and a measured tall pane drew the error on row 8 of 50, 35
//! blank rows, the composer pinned to the foot.
//!
//! This sweep holds the zone to what those changes may not move, over every
//! fixture FILE under `aterm-phase/src/fixtures/` and (in the aterm
//! workspace) `aterm-agent/src/supervise/policy/fixtures/`, read by Claude
//! Code's reader, Codex's and the screen-identified one: the same screen in a
//! taller pane — 7, 30 and 60 blank rows added below it — reads exactly as it
//! reads in a pane as tall as what is drawn. The zone's arithmetic and the
//! tail's gap test are `reader.rs`'s unit tests.

use aterm_phase::prompt::PromptKind;
use aterm_phase::prompt::fixtures as fx;
use aterm_phase::{Program, live_zone_start};

use crate::fixture_files::{file_rows, fixture_screens};

/// The zone the server's agent verdict reads (`presence::CLASSIFY_ROWS`).
const ZONE_ROWS: usize = 40;

fn zone(rows: &[String]) -> &[String] {
    &rows[live_zone_start(rows, ZONE_ROWS)..]
}

/// Every reading a caller takes of a zone, as one comparable string: each
/// program's reader ([`Program::ALL`]) and the one the screen identifies
/// (`None`), with and without a cursor column.
fn readings(rows: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for program in Program::ALL.map(Some).into_iter().chain([None]) {
        for cursor in [None, Some(2)] {
            let reading = match program {
                Some(p) => p.reader().read(rows, cursor),
                None => aterm_phase::read(None, rows, cursor),
            };
            out.push(format!("{program:?} {cursor:?} {reading:?}"));
        }
    }
    out
}

fn blank(r: &str) -> bool {
    r.trim().is_empty()
}

/// THE PLAN DIALOG'S GAIN, NAMED: `claude-2.1.281-plan-approval.txt` is 44
/// rows drawn to its foot with a 13-row gap, so its zone begins on row 0 and
/// the last 40 rows begin on row 4. Its head — the `Ready to code?` frame —
/// sat above that cut: read there it was a box with no head (`Other`, its
/// title a body row); read in the zone it is the whole plan approval. The
/// supervisor's tail read now sees it whole too, so the approval policy's
/// plan rule can apply where it used to escalate a box of no kind.
/// NEGATIVE CONTROL: the same rows with the gap filled read as they did,
/// through either window.
#[test]
fn the_plan_approval_is_read_whole_through_its_zone() {
    let rows = file_rows(fx::PLAN_APPROVAL);
    let last40 = rows.len() - ZONE_ROWS;
    assert_eq!(
        live_zone_start(&rows, ZONE_ROWS),
        0,
        "the gap reaches row 0"
    );
    assert!(last40 > 0, "the last 40 rows begin below it");
    let kind = |z: &[String]| {
        aterm_phase::read(Some("claude"), z, None)
            .prompt
            .map(|p| (p.kind, p.head_off_screen))
    };
    assert_eq!(kind(&rows[last40..]), Some((PromptKind::Other, true)));
    assert_eq!(kind(&rows), Some((PromptKind::PlanExit, false)));
    // CONTROL: no gap, and the last 40 rows are the zone.
    let mut filled = rows.clone();
    for row in filled.iter_mut().filter(|r| blank(r)) {
        *row = "· x".to_string();
    }
    let start = live_zone_start(&filled, ZONE_ROWS);
    assert_eq!(start, last40);
    assert_eq!(kind(&filled[last40..]), kind(&filled[start..]));
}

#[test]
fn blank_rows_under_the_drawing_decide_nothing() {
    const PADS: [usize; 4] = [0, 7, 30, 60];
    let screens = fixture_screens("live zone sweep");
    let moved: Vec<String> = crate::on_every_core(&screens, |(name, rows)| {
        // The screen in a pane exactly as tall as what is drawn on it.
        let drawn = rows.iter().rposition(|r| !blank(r)).map_or(0, |i| i + 1);
        let tight = &rows[..drawn];
        let want = readings(zone(tight));
        PADS.iter()
            .filter_map(|&pad| {
                let mut tall = rows.clone();
                tall.extend(std::iter::repeat_n(String::new(), pad));
                let got = readings(zone(&tall));
                want.iter().zip(&got).find(|(w, g)| w != g).map(|(w, g)| {
                    format!("{name} with {pad} blank rows below:\n  tight {w}\n  tall  {g}")
                })
            })
            .collect::<Vec<_>>()
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(moved.is_empty(), "{}", moved.join("\n"));
    let swept = screens.len() * PADS.len();
    assert!(swept > 200, "only {swept} screens swept");
}
