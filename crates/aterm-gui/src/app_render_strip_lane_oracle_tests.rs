// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, WindowId, term_lock};
use aterm_core::render::{FrameRefill, FullRefillCause, RenderInput};
use aterm_render::{DirtyDecision, compute_dirty_rows, row_revisions_comparable};

const CELL_H: usize = 16;

/// What a corpus step does to the composed frame after the strip splice —
/// the shipping chrome that runs later in the same seam.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chrome {
    /// Nothing after the strip.
    None,
    /// `splice_find_bar` — overwrites terminal cells in place and bumps
    /// `snapshot_seq`. Must cost the next frame its scoped arm.
    FindBar,
    /// `splice_settings_panel` — a composited card; it writes no cells, so
    /// it must not cost the next frame anything.
    SettingsPanel,
}

struct Step {
    name: &'static str,
    strip: usize,
    engine: fn(&mut aterm_core::terminal::Terminal),
    chrome: Chrome,
}

/// A window with empty scratch and strip state over a terminal with a
/// settled damage session (a fresh terminal's `Damage::Full` publishes no
/// lane and would make the reach floors vacuous).
fn fixture() -> (App, WindowId, usize, usize) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let (rows, cols) = {
        let t = term_lock(&terminal);
        (usize::from(t.rows()), usize::from(t.cols()))
    };
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.rows = u16::try_from(rows).expect("fixture rows");
        ws.cols = u16::try_from(cols).expect("fixture cols");
        ws.input_scratch = RenderInput::empty();
    }
    {
        let mut t = term_lock(&terminal);
        t.process(b"seed one\r\nseed two\r\nseed three\r\n");
        t.take_damage();
    }
    (app, wid, rows, cols)
}

/// One presented frame of the single-pane strip window at the three seams in
/// shipping order: the resident scratch's reclaim/un-splice (hoisted ahead
/// of the frame hold in `redraw_window`), the damage-scoped re-extract under
/// the hold, and the tab-strip prepend. A prepend that cannot be inverted
/// falls back to salvaging the buffers.
fn strip_frame(
    app: &mut App,
    wid: WindowId,
    rows: usize,
    cols: usize,
    strip: usize,
    strip_fp: u64,
) -> FrameRefill {
    strip_frame_with(app, wid, rows, cols, strip, strip_fp, true)
}

/// [`strip_frame`] with the un-splice switchable: `unsplice: false` salvages
/// the surplus tail only, leaving the scratch `strip` rows too tall and its
/// continuity tokens broken.
fn strip_frame_with(
    app: &mut App,
    wid: WindowId,
    rows: usize,
    cols: usize,
    strip: usize,
    strip_fp: u64,
    unsplice: bool,
) -> FrameRefill {
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let refill = {
        let ws = app.windows.get_mut(&wid).expect("fixture window");
        let unspliced = unsplice
            && strip > 0
            && ws
                .input_scratch
                .undo_host_row_prepend(rows, &mut ws.strip_row_pool, strip);
        if !unspliced && strip > 0 && ws.input_scratch.cells.len() > rows {
            let pool = &mut ws.strip_row_pool;
            for buf in ws.input_scratch.cells.drain(rows..) {
                if pool.len() >= strip {
                    break;
                }
                pool.push(buf);
            }
        }
        let mut term = term_lock(&terminal);
        term.cell_frame_damage_scoped_into(&mut ws.input_scratch, rows, cols)
    };
    app.tab_strip_rows = u16::try_from(strip).expect("fixture strip");
    app.splice_tab_strip_with(wid, strip_fp);
    refill
}

/// Reference: a full re-extract (`cell_frame`, which retains nothing) of the
/// same terminal state, taken before the incremental frame consumes the
/// damage session.
fn reference(app: &mut App, wid: WindowId, rows: usize, cols: usize) -> RenderInput {
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let mut term = term_lock(&terminal);
    term.cell_frame(rows, cols)
}

/// Every engine row of the composed frame must equal the same row of a full
/// re-extract at its shifted index, on every channel a scoped refill may
/// retain plus the two it rebuilds.
fn assert_engine_band_matches(
    composed: &RenderInput,
    reference: &RenderInput,
    rows: usize,
    strip: usize,
    what: &str,
) {
    assert_eq!(composed.rows, rows + strip, "{what}: composed frame height");
    assert_eq!(
        composed.cells.len(),
        rows + strip,
        "{what}: composed cell rows"
    );
    for r in 0..rows {
        let f = r + strip;
        assert_eq!(
            composed.cells[f], reference.cells[r],
            "{what}: engine row {r} at composed index {f} is NOT the row a full \
             re-extract resolves — a stale row on the user's screen"
        );
        assert_eq!(
            composed.clusters[f], reference.clusters[r],
            "{what}: engine row {r} clusters diverged at composed index {f}"
        );
        assert_eq!(
            composed.combining[f], reference.combining[r],
            "{what}: engine row {r} combining marks diverged at composed index {f}"
        );
        assert_eq!(
            composed.images[f], reference.images[r],
            "{what}: engine row {r} images diverged at composed index {f}"
        );
        assert_eq!(
            composed.line_sizes[f], reference.line_sizes[r],
            "{what}: engine row {r} line size diverged at composed index {f}"
        );
    }
}

/// The lane-disowned verdict: `compute_dirty_rows` down its exact whole-grid
/// compare, which is the path every frame the stamp gate refuses still takes.
fn brute_force(prev: &RenderInput, cur: &RenderInput, dirty: &mut Vec<bool>) -> DirtyDecision {
    let mut p = prev.clone();
    let mut c = cur.clone();
    p.invalidate_row_revisions();
    c.invalidate_row_revisions();
    compute_dirty_rows(&p, &c, false, None, false, None, CELL_H, dirty)
}

fn corpus() -> Vec<Step> {
    vec![
        // ---- Strip off: the macOS shape. ----
        Step {
            name: "off_idle",
            strip: 0,
            engine: |_| {},
            chrome: Chrome::None,
        },
        Step {
            name: "off_type",
            strip: 0,
            engine: |t| t.process(b"x"),
            chrome: Chrome::None,
        },
        Step {
            name: "off_type_again",
            strip: 0,
            engine: |t| t.process(b"y"),
            chrome: Chrome::None,
        },
        // ---- Strip on, one row. ----
        Step {
            name: "on1_appear",
            strip: 1,
            engine: |_| {},
            chrome: Chrome::None,
        },
        Step {
            name: "on1_idle",
            strip: 1,
            engine: |_| {},
            chrome: Chrome::None,
        },
        Step {
            name: "on1_type",
            strip: 1,
            engine: |t| t.process(b"a"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_type_again",
            strip: 1,
            engine: |t| t.process(b"b"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_home_overwrite",
            strip: 1,
            engine: |t| t.process(b"\x1b[1;1Hoverwrite"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_newline",
            strip: 1,
            engine: |t| t.process(b"\r\nline\r\n"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_erase_line",
            strip: 1,
            engine: |t| t.process(b"\x1b[3;1H\x1b[2K"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_sgr_run",
            strip: 1,
            engine: |t| t.process(b"\x1b[4;1H\x1b[31;44mred on blue\x1b[0m"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_wide_combining",
            strip: 1,
            engine: |t| t.process("\x1b[5;1H漢字 e\u{0301}\u{0302} 🚀".as_bytes()),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_decdwl",
            strip: 1,
            engine: |t| t.process(b"\x1b[6;1H\x1b#6wide"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_decdwl_off",
            strip: 1,
            engine: |t| t.process(b"\x1b[6;1H\x1b#5"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_after_wide",
            strip: 1,
            engine: |t| t.process(b"\x1b[7;1Hplain"),
            chrome: Chrome::None,
        },
        // ---- The chrome that runs after the strip in the shipping seam. ----
        Step {
            name: "on1_settings_panel",
            strip: 1,
            engine: |t| t.process(b"\x1b[8;1Hpanel"),
            chrome: Chrome::SettingsPanel,
        },
        Step {
            name: "on1_after_settings",
            strip: 1,
            engine: |t| t.process(b"\x1b[8;6H!"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_find_bar",
            strip: 1,
            engine: |t| t.process(b"\x1b[9;1Hfindme"),
            chrome: Chrome::FindBar,
        },
        Step {
            name: "on1_after_find_bar",
            strip: 1,
            engine: |t| t.process(b"\x1b[9;8H."),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_settle_after_find",
            strip: 1,
            engine: |t| t.process(b"\x1b[9;9H."),
            chrome: Chrome::None,
        },
        // ---- The strip row count changing mid-session. ----
        Step {
            name: "on2_grow",
            strip: 2,
            engine: |_| {},
            chrome: Chrome::None,
        },
        Step {
            name: "on2_idle",
            strip: 2,
            engine: |_| {},
            chrome: Chrome::None,
        },
        Step {
            name: "on2_type",
            strip: 2,
            engine: |t| t.process(b"\x1b[2;1Htwo-row strip"),
            chrome: Chrome::None,
        },
        Step {
            name: "on2_type_again",
            strip: 2,
            engine: |t| t.process(b"!"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_shrink",
            strip: 1,
            engine: |_| {},
            chrome: Chrome::None,
        },
        Step {
            name: "on1_after_shrink",
            strip: 1,
            engine: |t| t.process(b"\x1b[2;1Hback to one"),
            chrome: Chrome::None,
        },
        Step {
            name: "off_disappear",
            strip: 0,
            engine: |_| {},
            chrome: Chrome::None,
        },
        Step {
            name: "off_after_disappear",
            strip: 0,
            engine: |t| t.process(b"\x1b[2;1Hno strip"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_reappear",
            strip: 1,
            engine: |_| {},
            chrome: Chrome::None,
        },
        // ---- A scrolled-back viewport under the strip: must fail closed. ----
        Step {
            name: "on1_scroll_back",
            strip: 1,
            engine: |t| {
                // Build real scrollback first: `scroll_display` with no
                // history is a no-op.
                for _ in 0..40 {
                    t.process(b"scrollback line\r\n");
                }
                t.grid_mut().scroll_display(3);
            },
            chrome: Chrome::None,
        },
        Step {
            name: "on1_scrolled_output",
            strip: 1,
            engine: |t| t.process(b"streamed while scrolled\r\n"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_scroll_bottom",
            strip: 1,
            engine: |t| {
                t.grid_mut().scroll_to_bottom();
            },
            chrome: Chrome::None,
        },
        Step {
            name: "on1_settle_after_scroll",
            strip: 1,
            engine: |t| t.process(b"\x1b[10;1Hsettled"),
            chrome: Chrome::None,
        },
        // ---- Alt screen: an independent grid and an independent clock. ----
        Step {
            name: "on1_alt_enter",
            strip: 1,
            engine: |t| t.process(b"\x1b[?1049h\x1b[1;1Halt buffer"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_alt_write",
            strip: 1,
            engine: |t| t.process(b"\x1b[2;1Hmore alt"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_alt_write_again",
            strip: 1,
            engine: |t| t.process(b"\x1b[3;1Hstill alt"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_alt_leave",
            strip: 1,
            engine: |t| t.process(b"\x1b[?1049l"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_post_alt",
            strip: 1,
            engine: |t| t.process(b"\x1b[11;1Hprimary again"),
            chrome: Chrome::None,
        },
        // ---- Glyph-free recolours: the sharpest attack on a content stamp. ----
        Step {
            name: "on1_palette_osc4",
            strip: 1,
            engine: |t| t.process(b"\x1b]4;1;rgb:00/ff/00\x07"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_default_bg_osc11",
            strip: 1,
            engine: |t| t.process(b"\x1b]11;rgb:10/20/30\x07"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_reverse_video_on",
            strip: 1,
            engine: |t| t.process(b"\x1b[?5h"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_reverse_video_off",
            strip: 1,
            engine: |t| t.process(b"\x1b[?5l"),
            chrome: Chrome::None,
        },
        Step {
            name: "on1_final_settle",
            strip: 1,
            engine: |t| t.process(b"\x1b[12;1Hdone"),
            chrome: Chrome::None,
        },
    ]
}

#[test]
fn a_strip_frames_retained_rows_match_a_full_re_extract_over_the_corpus() {
    let (mut app, wid, rows, cols) = fixture();

    let mut presented: Option<RenderInput> = None;
    let mut prev_strip = usize::MAX;
    let mut stamped_dirty: Vec<bool> = Vec::new();
    let mut brute_dirty: Vec<bool> = Vec::new();

    // Non-vacuity ledgers.
    let mut scoped_on = 0usize;
    let mut full_on = 0usize;
    let mut scoped_off = 0usize;
    // Which clause refused, over every full-arm frame of the (adversarial)
    // corpus: the attribution must move with the mutation.
    let mut cause_tally: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    let mut lane_on = 0usize;
    let mut lane_reported_work = 0usize;
    let mut prev_offset = 0i32;
    let mut scrolled_frames = 0usize;

    let steps = corpus();
    // Two passes: the second re-enters every step from a different prior
    // state, which is where an order-dependent index bug shows up.
    for pass in 0..2 {
        for (i, step) in steps.iter().enumerate() {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            {
                let mut t = term_lock(&terminal);
                (step.engine)(&mut t);
            }
            let what = format!("pass {pass} step {i} ({})", step.name);
            let reference_frame = reference(&mut app, wid, rows, cols);
            // The strip fingerprint moves with the row count, so the cached
            // strip rows are rebuilt whenever the count changes.
            let fp = u64::try_from(step.strip).unwrap_or(0).wrapping_add(1);
            let refill = strip_frame(&mut app, wid, rows, cols, step.strip, fp);
            match step.chrome {
                Chrome::None => {}
                Chrome::FindBar => {
                    app.search_enter();
                    app.splice_find_bar(wid);
                }
                Chrome::SettingsPanel => app.splice_settings_panel(wid),
            }
            if step.chrome != Chrome::FindBar {
                app.search_exit();
            }

            let composed = app.windows[&wid].input_scratch.clone();

            // ---- The correctness claim. ----
            // (The find-bar step overwrites terminal cells on purpose; it
            // exists for the fail-closed assertion on the next frame.)
            if step.chrome != Chrome::FindBar {
                assert_engine_band_matches(&composed, &reference_frame, rows, step.strip, &what);
            }
            if step.strip > 0 {
                let chrome_rows = app.windows[&wid].cached_strip_rows.clone();
                assert_eq!(
                    chrome_rows.len(),
                    step.strip,
                    "{what}: cached strip row count"
                );
                for (r, want) in chrome_rows.iter().enumerate() {
                    assert_eq!(
                        &composed.cells[r], want,
                        "{what}: composed row {r} is not the painted chrome row"
                    );
                }
                assert_eq!(
                    composed.row_shift, step.strip,
                    "{what}: the frame must record its own prepend"
                );
                if composed.row_rev_lane != 0 {
                    assert_eq!(
                        composed.row_rev.len(),
                        rows + step.strip,
                        "{what}: a live lane must cover the COMPOSED rows"
                    );
                    for r in 0..step.strip {
                        assert_eq!(
                            composed.row_rev[r], 0,
                            "{what}: chrome row {r} must read as UNKNOWN, never as a stamp"
                        );
                    }
                    for r in 0..rows {
                        assert_eq!(
                            composed.row_rev[r + step.strip],
                            reference_frame.row_rev[r],
                            "{what}: engine row {r}'s revision landed at the wrong index"
                        );
                    }
                }
            }

            // ---- The dirty-row claim, against the lane-disowned brute force. ----
            if let Some(prev) = presented.as_ref() {
                let lane = row_revisions_comparable(prev, &composed);
                let got = compute_dirty_rows(
                    prev,
                    &composed,
                    false,
                    None,
                    false,
                    None,
                    CELL_H,
                    &mut stamped_dirty,
                );
                let want = brute_force(prev, &composed, &mut brute_dirty);
                assert_eq!(
                    std::mem::discriminant(&got),
                    std::mem::discriminant(&want),
                    "{what}: dirty decision shape diverged"
                );
                if matches!(got, DirtyDecision::Rows(_)) {
                    assert_eq!(
                        stamped_dirty.len(),
                        brute_dirty.len(),
                        "{what}: dirty length"
                    );
                    for (r, (&g, &w)) in stamped_dirty.iter().zip(&brute_dirty).enumerate() {
                        assert!(
                            g || !w,
                            "{what}: composed row {r} is dirty by CONTENT but clean under \
                             the spliced lane — a stale row on the user's screen"
                        );
                    }
                    if lane {
                        if step.strip > 0 {
                            lane_on += 1;
                        }
                        if stamped_dirty.iter().any(|d| *d) {
                            lane_reported_work += 1;
                        }
                    }
                }
                // A strip-count change invalidates rather than shifts: stamps
                // of frames with different shifts are never compared.
                if prev_strip != step.strip {
                    assert!(
                        !lane,
                        "{what}: the lane stayed comparable across a strip-count change \
                         {prev_strip} -> {} — engine rows moved under their numbers",
                        step.strip
                    );
                }
            }

            match (step.strip > 0, refill) {
                (true, FrameRefill::Scoped { .. }) => scoped_on += 1,
                (true, FrameRefill::Full { cause }) => {
                    full_on += 1;
                    *cause_tally.entry(cause.as_str()).or_insert(0usize) += 1;
                }
                (false, FrameRefill::Scoped { .. }) => scoped_off += 1,
                (false, FrameRefill::Full { cause }) => {
                    *cause_tally.entry(cause.as_str()).or_insert(0usize) += 1;
                }
            }
            // Fail-closed: the frame after a find bar overwrote engine cells
            // retains nothing.
            if step.name == "on1_after_find_bar" {
                assert_eq!(
                    refill,
                    FrameRefill::Full {
                        cause: FullRefillCause::HostMutation
                    },
                    "{what}: the frame after an in-place cell overwrite MUST re-extract in \
                     full, attributed to the HOST that overwrote it"
                );
            }
            // A scrolled-back viewport likewise: the revision lane is indexed
            // by live grid row, so a frame filled at a non-zero display offset
            // is never retained by the next. Counted for non-vacuity below.
            if prev_offset != 0 {
                assert!(
                    matches!(refill, FrameRefill::Full { .. }),
                    "{what}: the frame after a SCROLLED-BACK viewport                          (display_offset {prev_offset}) MUST re-extract in full"
                );
                scrolled_frames += 1;
            }
            prev_offset = composed.display_offset;

            prev_strip = step.strip;
            match presented.as_mut() {
                Some(p) => p.clone_from(&composed),
                None => presented = Some(composed),
            }
        }
    }

    // ---- Non-vacuity: a corpus that fell back to Full on every strip frame
    // would satisfy every equality above and prove nothing. ----
    assert!(
        scoped_on >= 20,
        "the strip-ON corpus reached the damage-scoped arm on only {scoped_on} frames — \
         the oracle is not testing what this fix added (full_on={full_on})"
    );
    assert!(
        full_on >= 4,
        "the strip-ON corpus never exercised the FALL-BACK arm ({full_on}) — the \
         fail-closed direction is untested"
    );
    assert!(
        scoped_off >= 2,
        "the strip-OFF corpus stopped reaching the scoped arm ({scoped_off}) — this fix \
         regressed the platform that already had it"
    );
    assert!(
        lane_on >= 15,
        "the SPLICED revision lane was comparable on only {lane_on} strip frames — vacuous"
    );
    assert!(
        lane_reported_work >= 4,
        "the spliced lane never reported a dirty row ({lane_reported_work}) — vacuous"
    );
    assert!(
        scrolled_frames >= 2,
        "the corpus never presented a SCROLLED-BACK viewport ({scrolled_frames}) — the              offset clause is untested"
    );
    crate::logging::stderr_line!(
        "STRIP-LANE ORACLE: scoped_on={scoped_on} full_on={full_on} scoped_off={scoped_off} \
         lane_on={lane_on} lane_work={lane_reported_work}"
    );
    crate::logging::stderr_line!("STRIP-LANE ORACLE full-arm causes: {cause_tally:?}");
    // The attribution must discriminate: this corpus reporting a single
    // clause would mean the cause is a constant.
    assert!(
        cause_tally.len() >= 3,
        "the corpus refused under only {} distinct clause(s) ({cause_tally:?}) — the \
         per-cause attribution is not discriminating",
        cause_tally.len()
    );
}

/// The scoped share (`frame_refills_scoped` / `frame_refills_full` from
/// `metrics::note_frame_refill`) on a tab-strip window moves off zero: the
/// same steady-state keystroke corpus runs with the salvage-only reclaim and
/// with the un-splice, and both shares are printed.
#[test]
fn the_strip_window_scoped_share_moves_off_zero() {
    fn steady_state_share(
        unsplice: bool,
    ) -> (
        usize,
        usize,
        std::collections::BTreeMap<&'static str, usize>,
    ) {
        let (mut app, wid, rows, cols) = fixture();
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let mut scoped = 0usize;
        let mut full = 0usize;
        let mut causes: std::collections::BTreeMap<&'static str, usize> =
            std::collections::BTreeMap::new();
        // One settling frame, then 64 keystroke echoes — the shape the
        // frontend produces while a human types into a strip window.
        let _ = strip_frame_with(&mut app, wid, rows, cols, 1, 1, unsplice);
        for i in 0..64u8 {
            {
                let mut t = term_lock(&terminal);
                t.process(if i.is_multiple_of(2) { b"\rx" } else { b"\ry" });
            }
            match strip_frame_with(&mut app, wid, rows, cols, 1, 1, unsplice) {
                FrameRefill::Scoped { .. } => scoped += 1,
                FrameRefill::Full { cause } => {
                    full += 1;
                    *causes.entry(cause.as_str()).or_insert(0usize) += 1;
                }
            }
        }
        (scoped, full, causes)
    }

    let (before_scoped, before_full, before_causes) = steady_state_share(false);
    let (after_scoped, after_full, after_causes) = steady_state_share(true);
    crate::logging::stderr_line!(
        "STRIP SCOPED SHARE  before: scoped={before_scoped} full={before_full} \
         ({:.0}%)  after: scoped={after_scoped} full={after_full} ({:.0}%)",
        100.0 * before_scoped as f64 / (before_scoped + before_full) as f64,
        100.0 * after_scoped as f64 / (after_scoped + after_full) as f64,
    );
    crate::logging::stderr_line!(
        "STRIP FULL-ARM CAUSES  before: {before_causes:?}  after: {after_causes:?}"
    );
    // Without the un-splice every full-arm frame books to `host_mutation`,
    // not `scratch_row_count`: `splice_tab_strip_with` bumps `snapshot_seq`
    // when it prepends, and the seq clause is checked ahead of the row-count
    // clause. So a strip window whose full arm dominates under
    // `host_mutation` points at the splice, not a scratch-sizing bug.
    assert_eq!(
        before_causes.keys().copied().collect::<Vec<_>>(),
        vec!["host_mutation"],
        "the pre-fix strip reclaim must refuse under exactly one, NAMED clause"
    );
    assert!(
        after_causes.is_empty(),
        "the spliced lane must leave no full-arm frame to attribute: {after_causes:?}"
    );
    assert_eq!(
        before_scoped, 0,
        "the PRE-FIX reclaim is supposed to score zero scoped frames on a strip \
         window; if it does not, this test is measuring the wrong thing"
    );
    assert!(
        after_scoped >= 60,
        "a settled strip window reached the damage-scoped arm on only \
         {after_scoped}/64 keystroke frames — the fix did not reach the shipping shape"
    );
}

/// A settled strip window takes the scoped arm and retains the row a full
/// re-extract resolves; a strip-count change on the next frame discards the
/// retention.
#[test]
fn a_strip_count_change_invalidates_rather_than_shifts() {
    let (mut app, wid, rows, cols) = fixture();
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&terminal).process(b"\x1b[1;1HROW-ZERO\x1b[2;1HROW-ONE");

    // Settle: two strip-1 frames, the second of which must retain.
    let _ = strip_frame(&mut app, wid, rows, cols, 1, 1);
    term_lock(&terminal).process(b"\x1b[2;9H!");
    let settled_ref = reference(&mut app, wid, rows, cols);
    let refill = strip_frame(&mut app, wid, rows, cols, 1, 1);
    assert!(
        matches!(refill, FrameRefill::Scoped { .. }),
        "a settled strip window must reach the damage-scoped arm; got {refill:?}"
    );
    let one = app.windows[&wid].input_scratch.clone();
    assert_engine_band_matches(&one, &settled_ref, rows, 1, "settled strip-1");
    assert!(
        one.cells[1]
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .starts_with("ROW-ZERO"),
        "the retained top engine row must sit at composed index 1"
    );

    // Grow the strip to 2: composed index 2 is now engine row 0, which the
    // previous frame served at index 1, so stamps must not be compared across.
    let grown_ref = reference(&mut app, wid, rows, cols);
    let _ = strip_frame(&mut app, wid, rows, cols, 2, 2);
    let two = app.windows[&wid].input_scratch.clone();
    assert_engine_band_matches(&two, &grown_ref, rows, 2, "grown strip-2");
    assert!(
        !row_revisions_comparable(&one, &two),
        "the stamp gate compared two frames whose engine rows are one index apart"
    );
    assert_eq!(
        one.row_shift, 1,
        "the strip-1 frame records its own prepend"
    );
    assert_eq!(
        two.row_shift, 2,
        "the strip-2 frame records its own prepend"
    );
}

/// Only an engine fill and a blessed prepend may leave
/// `snapshot_seq == shifted_fill_seq`; any other host cell write bumps the
/// seq alone, so both gates fail closed by default.
#[test]
fn any_host_cell_write_before_the_prepend_disowns_the_lane() {
    let (mut app, wid, rows, cols) = fixture();
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&terminal).process(b"\x1b[1;1Hcontent");

    // A clean prepend keeps the lane.
    let _ = strip_frame(&mut app, wid, rows, cols, 1, 1);
    let clean = app.windows[&wid].input_scratch.clone();
    assert_ne!(clean.row_rev_lane, 0, "a blessed prepend keeps the lane");
    assert_eq!(
        clean.snapshot_seq, clean.shifted_fill_seq,
        "a blessed prepend re-arms the provenance token"
    );

    // A host writes a cell (bumping the seq) before the prepend runs.
    term_lock(&terminal).process(b"\x1b[1;1Hcontent2");
    {
        let ws = app.windows.get_mut(&wid).expect("fixture window");
        let unspliced = ws
            .input_scratch
            .undo_host_row_prepend(rows, &mut ws.strip_row_pool, 1);
        assert!(unspliced, "the clean prepend must be invertible");
        let mut t = term_lock(&terminal);
        let _ = t.cell_frame_damage_scoped_into(&mut ws.input_scratch, rows, cols);
        drop(t);
        ws.input_scratch.cells[0][0].ch = '~';
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
    }
    app.tab_strip_rows = 1;
    app.splice_tab_strip_with(wid, 1);
    let dirty = app.windows[&wid].input_scratch.clone();
    assert_eq!(
        dirty.row_rev_lane, 0,
        "a prepend applied on top of a host cell write must DISOWN the lane"
    );
    assert_eq!(
        dirty.shifted_fill_seq, 0,
        "…and must not leave a provenance token behind"
    );
    assert_eq!(
        dirty.row_shift, 1,
        "…while still recording the physical prepend, so it can be counted"
    );
    assert!(
        !row_revisions_comparable(&clean, &dirty),
        "the stamp gate accepted a frame whose cells a host had overwritten"
    );

    // …and the un-splice refuses it, so the next extract cannot retain rows
    // whose bytes the host changed.
    let ws = app.windows.get_mut(&wid).expect("fixture window");
    let mut pool = Vec::new();
    assert!(
        !ws.input_scratch.undo_host_row_prepend(rows, &mut pool, 1),
        "the un-splice accepted an unblessed prepend"
    );
}
