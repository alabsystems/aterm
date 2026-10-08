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
//! This sweep holds the zone to what those changes may and may not move, over
//! every fixture FILE under `aterm-phase/src/fixtures/` and (in the aterm
//! workspace) `aterm-agent/src/supervise/policy/fixtures/`, read by Claude
//! Code's reader, Codex's and the screen-identified one:
//!
//! * a screen drawn to its last row with no blank run of a gap's length
//!   reaching into its last 40 rows (a fullscreen REPL on a full transcript, a
//!   Codex composer, a box drawn to the bottom) has exactly the zone the
//!   grid's last 40 rows gave — so every reading of it is what it was;
//! * one WITH such a run has a zone that reaches above them, and reads the
//!   same in every field but the row indexes — except for the named
//!   [`GAINS`], the screens the gap was cut for or whose reading it improved:
//!   the list is the whole of what changed, so a fourth change fails here;
//! * the same screen in a taller pane — 7, 30 and 60 blank rows added below
//!   it — reads exactly as it reads in a pane as tall as what is drawn: the
//!   blank rows under the drawing decide nothing, whatever the program;
//! * a supervisor that reads the last 40 rows is told, by
//!   [`aterm_phase::live_zone_reaches_above`], whenever they hold a gap — the
//!   only zones above them it cannot see are those whose gap their top row
//!   cuts.

use std::path::{Path, PathBuf};

use aterm_phase::prompt::PromptKind;
use aterm_phase::prompt::fixtures as fx;
use aterm_phase::{BLANK_GAP, Reading, live_zone_reaches_above, live_zone_start};

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

/// Whether the zone of `rows`, a screen taller than the zone and drawn to its
/// last row, must reach above its last `ZONE_ROWS` rows — decided by the
/// SCREEN, not by the function under test: some maximal run of `BLANK_GAP`
/// blank rows or more ENDS inside those rows (its top may lie above them: a
/// run that straddles the boundary is a gap however few of its rows are
/// inside — the walk counts it as one row all the same).
fn gap_reaches_into_the_tail(rows: &[String]) -> bool {
    let first = rows.len().saturating_sub(ZONE_ROWS);
    let mut run = 0;
    for (i, r) in rows.iter().enumerate() {
        if blank(r) {
            run += 1;
        } else {
            run = 0;
        }
        let ends_here = rows.get(i + 1).is_none_or(|next| !blank(next));
        if ends_here && run >= BLANK_GAP && i >= first {
            return true;
        }
    }
    false
}

/// A reading with the ROW INDEXES it carries zeroed — the wall's row, the
/// prompt's span, question row and option rows, the composer's caret row —
/// since a zone that starts higher numbers every row differently and nothing
/// else about the screen has moved.
fn shape(mut r: Reading) -> String {
    if let Some(w) = r.wall.as_mut() {
        w.row = 0;
    }
    let mut dialog = None;
    if let Some(p) = r.prompt.as_mut() {
        p.span = (0, 0);
        p.question = p.question.map(|_| 0);
        for o in &mut p.options {
            o.row = 0;
        }
        dialog = p.question_dialog.take().map(|q| {
            format!(
                "{:?} {} {:?} {} {:?}",
                q.tabs,
                q.submit_tab,
                q.auto_continue,
                q.width,
                std::mem::discriminant(&q.form)
            )
        });
    }
    if let Some((row, _)) = r.composer.as_mut() {
        *row = 0;
    }
    format!("{r:?} {dialog:?}")
}

/// Every reading a caller takes of a zone, [`shape`]d, for the readers and
/// cursors of [`readings`].
fn shapes(rows: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for program in [Some("claude"), Some("codex"), None] {
        for cursor in [None, Some(2)] {
            out.push(format!(
                "{program:?} {cursor:?} {}",
                shape(aterm_phase::read(program, rows, cursor))
            ));
        }
    }
    out
}

/// The screens whose reading the gap changed, and what each gained. The list
/// is exact:
/// `a_screen_drawn_to_its_last_row_has_the_zone_the_last_40_rows_gave` fails
/// on a change that is not here and on an entry that changed nothing.
const GAINS: [(&str, &str); 3] = [
    (
        "claude-2.1.283-launch-repl-ready.txt",
        "the fullscreen launch's banner is in the zone: `fresh`",
    ),
    (
        "claude-2.1.284-api-error-tall-pane-measured.txt",
        "the error is in the zone: a wall",
    ),
    (
        "claude-2.1.281-plan-approval.txt",
        "the plan dialog's head is in the zone: a whole `PlanExit` box, not one whose title \
         was cut",
    ),
];

#[test]
fn a_screen_drawn_to_its_last_row_has_the_zone_the_last_40_rows_gave() {
    let (mut drawn_to_bottom, mut gapped, mut changed) = (0, Vec::new(), Vec::new());
    for (name, rows) in screens() {
        if rows.last().is_some_and(|r| !blank(r)) {
            drawn_to_bottom += 1;
            let last40 = rows.len().saturating_sub(ZONE_ROWS);
            let start = live_zone_start(&rows, ZONE_ROWS);
            // The zone reaches above the last 40 rows exactly where a gap
            // reaches into them — and never below them.
            assert_eq!(
                start < last40,
                gap_reaches_into_the_tail(&rows) && rows.len() > ZONE_ROWS,
                "{name}: the zone starts on row {start}, the last 40 rows on row {last40}"
            );
            if start < last40 {
                // What the readers say of the screen — every field but the
                // row indexes — is what they said of the last 40 rows, but
                // for the gains the gap was cut for.
                if shapes(&rows[last40..]) != shapes(&rows[start..]) {
                    changed.push(name.clone());
                }
                gapped.push(name);
            }
        }
    }
    assert!(drawn_to_bottom > 50, "only {drawn_to_bottom} such screens");
    assert!(!gapped.is_empty(), "no screen with a gap swept");
    let named: Vec<&str> = GAINS.iter().map(|(g, _)| *g).collect();
    for name in &changed {
        assert!(
            named.iter().any(|g| name.ends_with(g)),
            "{name} read otherwise on the zone than on the last 40 rows, and is no named gain: \
             {GAINS:#?}"
        );
    }
    for g in named {
        assert!(
            changed.iter().any(|name| name.ends_with(g)),
            "{g} is a named gain and changed nothing: {changed:#?} of {gapped:#?}"
        );
    }
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

/// THE SUPERVISOR'S TAIL READ KNOWS WHEN IT HOLDS LESS THAN THE ZONE: on every
/// screen taller than the zone and drawn to its last row, the tail asks
/// [`live_zone_reaches_above`] for the whole read exactly when it holds a gap
/// (four blank rows in a row inside the last 40 rows) — decided here by
/// counting them, not by the walk — and the measured tall pane is one. The
/// only zones above the tail the tail cannot see are the ones whose gap the
/// tail's top row cuts (fewer than a gap's rows inside it): every miss over
/// every fixture file is one of those, and how many is reported.
#[test]
fn the_tail_sees_every_gap_inside_it() {
    let (mut above, mut cut, mut swept) = (0, Vec::new(), 0);
    let mut tall_pane_asked = false;
    for (name, rows) in screens() {
        if rows.len() <= ZONE_ROWS || rows.last().is_none_or(|r| blank(r)) {
            continue;
        }
        swept += 1;
        let last40 = rows.len() - ZONE_ROWS;
        let asks = live_zone_reaches_above(&rows[last40..], ZONE_ROWS);
        let gap_in_tail = rows[last40..]
            .windows(BLANK_GAP)
            .any(|w| w.iter().all(|r| blank(r)));
        assert_eq!(asks, gap_in_tail, "{name}: the tail's own gap");
        let needs = live_zone_start(&rows, ZONE_ROWS) < last40;
        above += usize::from(needs);
        if needs && !asks {
            // A run the tail's top row cuts: blank on both sides of it.
            assert!(
                blank(&rows[last40]) && blank(&rows[last40 - 1]),
                "{name}: the zone is above the tail and the tail cannot see it"
            );
            cut.push(name.clone());
        }
        if name.ends_with("claude-2.1.284-api-error-tall-pane-measured.txt") {
            tall_pane_asked = asks && needs;
        }
    }
    eprintln!(
        "live zone sweep: {swept} tall screens drawn to their last row; {above} have a zone above \
         the tail; {} of them a gap the tail's top row cuts: {cut:#?}",
        cut.len()
    );
    assert!(tall_pane_asked, "the measured tall pane");
    assert!(above > 0 && swept > 20, "{above} of {swept}");
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
