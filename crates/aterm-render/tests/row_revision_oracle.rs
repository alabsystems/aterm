// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Differential oracle for the per-row revision lane.
//!
//! `compute_dirty_rows` is the single source of truth for the dirty row set
//! (the CPU damage path and the GPU scissored repaint both read it). The
//! revision lane answers its cell half by comparing the engine's per-row
//! revision instead of every cell; this file pins that answer to the
//! brute-force one. A missed stamp is a stale frame, so every row the
//! reference calls dirty must be dirty under the lane (over-reporting only
//! costs a repaint), and the `DirtyDecision` shapes must agree.
//!
//! The reference is a shipping path, not a debug flag:
//! `RenderInput::invalidate_row_revisions` disowns the lane on a clone, which
//! sends `compute_dirty_rows` down the whole-grid compare every refused frame
//! takes. Non-vacuity is asserted at the end: per-mutator arming counts, and
//! that the steady-state steps reached the lane.

use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_render::{DirtyDecision, compute_dirty_rows, row_revisions_comparable};
use std::collections::BTreeMap;

const ROWS: usize = 12;
const COLS: usize = 24;
const CELL_H: usize = 16;

/// One corpus step: a name (for the arming census) and the mutation.
struct Step {
    name: &'static str,
    /// Engine-side mutation, applied under the "lock" before the extract.
    engine: fn(&mut Terminal),
    /// Host-side mutation of the freshly filled snapshot, applied after the
    /// extract; like the shipping producers, it bumps `snapshot_seq` on a cell
    /// write.
    host: fn(&mut RenderInput),
}

fn no_host(_: &mut RenderInput) {}

/// The stream-fade shape: tint a cell's foreground in place, bump the seq.
fn host_stream_fade(input: &mut RenderInput) {
    if let Some(cell) = input.cells.get_mut(2).and_then(|row| row.get_mut(3)) {
        cell.fg = [cell.fg[0].wrapping_add(9), cell.fg[1], cell.fg[2]];
    }
    input.snapshot_seq = input.snapshot_seq.wrapping_add(1);
}

/// The prediction-ghost shape: write a speculative glyph past the cursor.
fn host_prediction_ghost(input: &mut RenderInput) {
    if let Some(cell) = input.cells.get_mut(0).and_then(|row| row.get_mut(5)) {
        cell.ch = '~';
        cell.italic = true;
    }
    input.snapshot_seq = input.snapshot_seq.wrapping_add(1);
}

/// The sparkle/deco shape: a non-cell channel (the word decorations) moves and
/// the seq does not; the lane stays correct when only non-cell channels move.
fn host_sparkle(input: &mut RenderInput) {
    input.word_decorations.clear();
}

/// The IME-preedit shape: overwrite a run of cells at the caret.
fn host_preedit(input: &mut RenderInput) {
    if let Some(row) = input.cells.get_mut(1) {
        for (i, cell) in row.iter_mut().take(4).enumerate() {
            cell.ch = char::from(b'A' + u8::try_from(i).unwrap_or(0));
            cell.underline = aterm_core::terminal::UnderlineStyle::Single;
        }
    }
    input.snapshot_seq = input.snapshot_seq.wrapping_add(1);
}

/// The tab-strip splice shape: shift every per-row channel down by one and
/// prepend a host-painted row as `aterm_gui::app_render::prepend_strip_rows`
/// does, including `note_host_row_prepend`, which gives the chrome row the `0`
/// sentinel and moves every engine stamp to its new index. The full strip
/// lifecycle is oracled in `aterm_gui::app_render::strip_lane_oracle`; this
/// step makes this corpus meet a spliced frame from every prior state.
fn host_strip_splice(input: &mut RenderInput) {
    let blank = input.cells.first().and_then(|r| r.first()).copied();
    let Some(blank) = blank else { return };
    let blessed = input.host_prepend_blessing();
    input.cells.insert(0, vec![blank; input.cols]);
    input.clusters.insert(0, Vec::new());
    input.combining.insert(0, Vec::new());
    input.images.insert(0, Vec::new());
    input
        .line_sizes
        .insert(0, aterm_core::grid::LineSize::SingleWidth);
    input.line_size_spans.insert(0, Vec::new());
    input.default_bg_spans.insert(0, Vec::new());
    input.rows += 1;
    input.snapshot_seq = input.snapshot_seq.wrapping_add(1);
    input.note_host_row_prepend(1, blessed);
}

fn corpus() -> Vec<Step> {
    vec![
        Step {
            name: "idle",
            engine: |_| {},
            host: no_host,
        },
        Step {
            name: "type_one_char",
            engine: |t| t.process(b"x"),
            host: no_host,
        },
        Step {
            name: "type_again_same_row",
            engine: |t| t.process(b"y"),
            host: no_host,
        },
        Step {
            name: "cursor_home_then_overwrite",
            engine: |t| t.process(b"\x1b[1;1Hoverwrite"),
            host: no_host,
        },
        Step {
            name: "newline_scroll",
            engine: |t| t.process(b"\r\nline\r\n"),
            host: no_host,
        },
        Step {
            name: "erase_line",
            engine: |t| t.process(b"\x1b[3;1H\x1b[2K"),
            host: no_host,
        },
        Step {
            name: "erase_screen",
            engine: |t| t.process(b"\x1b[2J"),
            host: no_host,
        },
        Step {
            name: "insert_lines",
            engine: |t| t.process(b"\x1b[4;1H\x1b[2L"),
            host: no_host,
        },
        Step {
            name: "delete_lines",
            engine: |t| t.process(b"\x1b[4;1H\x1b[1M"),
            host: no_host,
        },
        Step {
            name: "scroll_region_up",
            engine: |t| t.process(b"\x1b[2;8r\x1b[8;1H\n\n"),
            host: no_host,
        },
        Step {
            name: "scroll_region_reset",
            engine: |t| t.process(b"\x1b[r"),
            host: no_host,
        },
        Step {
            name: "sgr_colour_run",
            engine: |t| t.process(b"\x1b[5;1H\x1b[31;44mred on blue\x1b[0m"),
            host: no_host,
        },
        Step {
            name: "palette_osc4",
            engine: |t| t.process(b"\x1b]4;1;rgb:00/ff/00\x07"),
            host: no_host,
        },
        Step {
            name: "default_bg_osc11",
            engine: |t| t.process(b"\x1b]11;rgb:10/20/30\x07"),
            host: no_host,
        },
        Step {
            name: "reverse_video_on",
            engine: |t| t.process(b"\x1b[?5h"),
            host: no_host,
        },
        Step {
            name: "reverse_video_off",
            engine: |t| t.process(b"\x1b[?5l"),
            host: no_host,
        },
        Step {
            name: "wide_and_combining",
            engine: |t| t.process("\x1b[6;1H漢字 e\u{0301}\u{0302} 🚀".as_bytes()),
            host: no_host,
        },
        Step {
            name: "decdwl_double_width",
            engine: |t| t.process(b"\x1b[7;1H\x1b#6wide line"),
            host: no_host,
        },
        Step {
            name: "decdwl_off",
            engine: |t| t.process(b"\x1b[7;1H\x1b#5"),
            host: no_host,
        },
        Step {
            name: "cursor_hide_show",
            engine: |t| t.process(b"\x1b[?25l\x1b[?25h"),
            host: no_host,
        },
        Step {
            name: "alt_screen_enter",
            engine: |t| t.process(b"\x1b[?1049h\x1b[1;1Halt buffer"),
            host: no_host,
        },
        Step {
            name: "alt_screen_write",
            engine: |t| t.process(b"\x1b[2;1Hmore alt"),
            host: no_host,
        },
        Step {
            name: "alt_screen_scroll",
            engine: |t| t.process(b"\x1b[12;1H\n\n"),
            host: no_host,
        },
        Step {
            name: "alt_screen_leave",
            engine: |t| t.process(b"\x1b[?1049l"),
            host: no_host,
        },
        Step {
            name: "scroll_back_into_history",
            engine: |t| {
                t.grid_mut().scroll_display(3);
            },
            host: no_host,
        },
        Step {
            name: "scrolled_back_output",
            engine: |t| t.process(b"streamed while scrolled\r\n"),
            host: no_host,
        },
        Step {
            name: "scroll_to_bottom",
            engine: |t| {
                t.grid_mut().scroll_to_bottom();
            },
            host: no_host,
        },
        Step {
            name: "host_stream_fade",
            engine: |t| t.process(b"fade"),
            host: host_stream_fade,
        },
        Step {
            name: "host_stream_fade_erase",
            engine: |_| {},
            host: host_stream_fade,
        },
        Step {
            name: "host_prediction_ghost",
            engine: |_| {},
            host: host_prediction_ghost,
        },
        Step {
            name: "host_prediction_ghost_clear",
            engine: |t| t.process(b"\x1b[1;6H "),
            host: no_host,
        },
        Step {
            name: "host_preedit",
            engine: |_| {},
            host: host_preedit,
        },
        Step {
            name: "host_sparkle",
            engine: |t| t.process(b"\x1b[9;1Hsparkle words here"),
            host: host_sparkle,
        },
        Step {
            name: "host_strip_splice",
            engine: |t| t.process(b"\x1b[10;1Hunder the strip"),
            host: host_strip_splice,
        },
        Step {
            name: "after_strip_splice",
            engine: |t| t.process(b"\x1b[10;5Hafter"),
            host: no_host,
        },
        Step {
            name: "reset_ris",
            engine: |t| t.process(b"\x1bc"),
            host: no_host,
        },
        Step {
            name: "post_reset_write",
            engine: |t| t.process(b"back to normal"),
            host: no_host,
        },
    ]
}

/// The brute-force verdict: the same call on lane-disowned clones, so
/// `compute_dirty_rows` takes its exact whole-grid compare.
fn reference(prev: &RenderInput, cur: &RenderInput, dirty: &mut Vec<bool>) -> DirtyDecision {
    let mut p = prev.clone();
    let mut c = cur.clone();
    p.invalidate_row_revisions();
    c.invalidate_row_revisions();
    compute_dirty_rows(&p, &c, false, None, false, None, CELL_H, dirty)
}

#[test]
fn stamped_dirty_rows_match_the_brute_force_oracle_over_a_mutation_corpus() {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"seed line one\r\nseed line two\r\nseed line three\r\n");

    // A resident scratch the engine refills, and a resident copy of the last
    // presented frame (the GPU's `prev_input` / the CPU cache's `input`).
    let mut scratch = RenderInput::empty();
    term.cell_frame_into(&mut scratch, ROWS, COLS);
    term.take_damage();
    let mut presented = scratch.clone();

    let mut stamped_dirty: Vec<bool> = Vec::new();
    let mut reference_dirty: Vec<bool> = Vec::new();
    let mut armed: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut stamp_lane_steps = 0usize;
    let mut stamp_lane_nonempty = 0usize;
    let mut lane_by_step: BTreeMap<&'static str, usize> = BTreeMap::new();

    let steps = corpus();
    // Two passes: the second re-enters every mutator from a different prior
    // state, where an order-dependent stamp bug shows.
    for pass in 0..2 {
        for step in &steps {
            (step.engine)(&mut term);
            let rows = usize::from(term.rows());
            let cols = usize::from(term.cols());
            term.cell_frame_into(&mut scratch, rows, cols);
            term.take_damage();
            (step.host)(&mut scratch);

            let lane = row_revisions_comparable(&presented, &scratch);
            let got = compute_dirty_rows(
                &presented,
                &scratch,
                false,
                None,
                false,
                None,
                CELL_H,
                &mut stamped_dirty,
            );
            let want = reference(&presented, &scratch, &mut reference_dirty);

            assert_eq!(
                std::mem::discriminant(&got),
                std::mem::discriminant(&want),
                "pass {pass} step {}: decision shape diverged",
                step.name
            );
            if matches!(got, DirtyDecision::Rows(_)) {
                assert_eq!(
                    stamped_dirty.len(),
                    reference_dirty.len(),
                    "pass {pass} step {}: dirty length diverged",
                    step.name
                );
                for (r, (&g, &w)) in stamped_dirty.iter().zip(&reference_dirty).enumerate() {
                    assert!(
                        g || !w,
                        "pass {pass} step {}: row {r} is dirty by content but CLEAN under \
                         the revision lane — this is a stale frame on the user's screen",
                        step.name
                    );
                }
                if lane {
                    stamp_lane_steps += 1;
                    *lane_by_step.entry(step.name).or_default() += 1;
                    if stamped_dirty.iter().any(|d| *d) {
                        stamp_lane_nonempty += 1;
                    }
                    // Exactness too: a lane that degraded into "everything is
                    // dirty" would pass the superset check.
                    let extra = stamped_dirty
                        .iter()
                        .zip(&reference_dirty)
                        .filter(|(g, w)| **g && !**w)
                        .count();
                    assert!(
                        extra <= rows,
                        "pass {pass} step {}: {extra} over-reported rows",
                        step.name
                    );
                }
            }
            *armed.entry(step.name).or_default() += 1;
            presented.clone_from(&scratch);
            let _ = cols;
        }
    }

    // Non-vacuity 1: every mutator in the corpus ran.
    for step in &steps {
        assert!(
            armed.get(step.name).copied().unwrap_or(0) >= 2,
            "corpus step {} never armed — it proves nothing",
            step.name
        );
    }
    // Non-vacuity 2: each steady-state frame, the class the lane exists to make
    // cheap, reached the stamp lane (a raw total could be met by fallbacks).
    for name in [
        "idle",
        "type_one_char",
        "type_again_same_row",
        "cursor_home_then_overwrite",
        "erase_line",
        "sgr_colour_run",
        "decdwl_double_width",
    ] {
        assert!(
            lane_by_step.get(name).copied().unwrap_or(0) >= 1,
            "steady-state step {name} NEVER reached the revision lane — the oracle \
             is not testing what D-2 added"
        );
    }
    // …and the lane reported real work, not merely gate hits.
    assert!(
        stamp_lane_nonempty >= 4,
        "the revision lane never reported a dirty row ({stamp_lane_nonempty}) — vacuous"
    );
    assert!(
        stamp_lane_steps >= 20,
        "the revision lane was reachable on only {stamp_lane_steps} frames — vacuous"
    );
}

/// The mark-clock's hazard: a second write to an already-damaged row, straddling
/// an extract, leaves the damage bitset unchanged, so a fold keyed on the bits
/// alone would give both snapshots the same stamp.
#[test]
fn a_second_write_to_an_already_damaged_row_changes_its_revision() {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[1;1Hbaseline");
    term.take_damage();

    // Damage row 0 and extract without consuming (the standalone-renderer shape).
    term.process(b"\x1b[1;1HAAA");
    let first = term.cell_frame(ROWS, COLS);
    // Same row, different content, same bitset.
    term.process(b"\x1b[1;1HBBB");
    term.take_damage();
    let second = term.cell_frame(ROWS, COLS);

    assert_ne!(
        first.cells[0], second.cells[0],
        "fixture is vacuous: the two frames must differ in row 0"
    );
    assert_ne!(
        first.row_rev[0], second.row_rev[0],
        "row 0 changed but its revision did not — the stamp lane would report it clean"
    );

    let mut dirty = Vec::new();
    let d = compute_dirty_rows(
        &first, &second, false, None, false, None, CELL_H, &mut dirty,
    );
    assert!(
        matches!(d, DirtyDecision::Rows(_)),
        "expected a row verdict"
    );
    assert!(dirty[0], "row 0 must be dirty");
}

/// A foreign consumer taking the damage between two extracts cannot erase a row
/// change from the lane: `take_damage` clears the bits for everyone, so the fold
/// happens on the way out.
#[test]
fn a_foreign_take_damage_between_extracts_cannot_lose_a_row() {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    // Burn the fresh terminal's full-damage session first: a full-damage
    // extract publishes no lane.
    term.process(b"warm");
    term.take_damage();
    term.process(b"\x1b[1;1Hbaseline\r\nsecond row");

    // Extract while the damage is pending, so the extract's own fold stamps
    // every row; rows still on the `0` sentinel would take the exact compare
    // and prove nothing about the fold.
    let mut scratch = RenderInput::empty();
    term.cell_frame_into(&mut scratch, ROWS, COLS);
    term.take_damage();
    let presented = scratch.clone();
    assert!(
        presented.row_rev.iter().all(|rev| *rev != 0),
        "fixture is vacuous: every row must carry a real stamp before the foreign take"
    );

    // A change nobody extracts, consumed by somebody else entirely.
    term.process(b"\x1b[2;1HCHANGED");
    term.take_damage();

    term.cell_frame_into(&mut scratch, ROWS, COLS);
    term.take_damage();

    assert_ne!(
        presented.cells[1], scratch.cells[1],
        "fixture is vacuous: row 1 must differ"
    );
    assert!(
        row_revisions_comparable(&presented, &scratch),
        "fixture is vacuous: this pair must reach the stamp lane"
    );
    let mut dirty = Vec::new();
    let d = compute_dirty_rows(
        &presented, &scratch, false, None, false, None, CELL_H, &mut dirty,
    );
    assert!(matches!(d, DirtyDecision::Rows(_)));
    assert!(
        dirty[1],
        "row 1 changed while a foreign consumer held the damage session — \
         the lane must still report it"
    );
}

/// An idle frame after a typed row reports no dirty rows; a fold that
/// double-stamped (at the extract and at `take_damage`) would re-report every
/// typed row forever.
#[test]
fn an_idle_frame_after_a_typed_row_reports_no_dirty_rows() {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[1;1Hbaseline");
    term.take_damage();

    let mut scratch = RenderInput::empty();
    term.process(b"\x1b[1;1Htyped");
    term.cell_frame_into(&mut scratch, ROWS, COLS);
    term.take_damage();
    let presented = scratch.clone();

    // The very next frame: nothing happened.
    term.cell_frame_into(&mut scratch, ROWS, COLS);
    term.take_damage();

    assert!(
        row_revisions_comparable(&presented, &scratch),
        "fixture is vacuous: this pair must reach the stamp lane"
    );
    let mut dirty = Vec::new();
    let d = compute_dirty_rows(
        &presented, &scratch, false, None, false, None, CELL_H, &mut dirty,
    );
    assert!(matches!(d, DirtyDecision::Rows(_)));
    assert!(
        dirty.iter().all(|d| !*d),
        "an unchanged frame must gate-hit; got {:?}",
        dirty
            .iter()
            .enumerate()
            .filter(|(_, d)| **d)
            .map(|(r, _)| r)
            .collect::<Vec<_>>()
    );
}

/// Two terminals mint revisions from independent clocks, so one's stamps are
/// never read as the other's.
#[test]
fn two_terminals_never_share_a_revision_lane() {
    let mut a = Terminal::new(ROWS as u16, COLS as u16);
    let mut b = Terminal::new(ROWS as u16, COLS as u16);
    a.process(b"\x1b[1;1Hterminal A content");
    b.process(b"\x1b[1;1Hterminal B content");
    let fa = a.cell_frame(ROWS, COLS);
    let fb = b.cell_frame(ROWS, COLS);
    assert!(
        !row_revisions_comparable(&fa, &fb),
        "distinct terminals must not share a comparable revision lane"
    );
    let mut dirty = Vec::new();
    let d = compute_dirty_rows(&fa, &fb, false, None, false, None, CELL_H, &mut dirty);
    assert!(matches!(d, DirtyDecision::Rows(_)));
    assert!(dirty[0], "row 0 differs in content and must be dirty");
}
