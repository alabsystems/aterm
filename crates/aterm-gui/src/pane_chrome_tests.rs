// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use super::*;
use aterm_render::Theme;

const INKS: SeamInks = SeamInks {
    ground: [0x11, 0x13, 0x18],
    band: [0x30, 0x31, 0x35],
    line: [0x4a, 0x4c, 0x4f],
    ring: [0x50, 0xfa, 0x7b],
};

fn cell_box(row: usize, col: usize, rows: usize, cols: usize) -> CellBox {
    CellBox {
        row,
        col,
        rows,
        cols,
    }
}

/// The focus ring of an allocation with no header row.
fn bare(alloc: CellBox) -> FocusRing {
    FocusRing {
        alloc,
        header_row: None,
    }
}

/// A Fluent composite of `rows × cols` seeded by the shipping divider fill.
fn seeded(rows: usize, cols: usize) -> RenderInput {
    with_split_chrome(SplitChrome::Fluent, || {
        let mut dst = RenderInput::empty();
        crate::app_render::fill_divider_grid(&mut dst, rows, cols, Theme::default(), None);
        dst
    })
}

#[test]
fn every_arm_set_has_its_light_box_glyph() {
    assert_eq!(seam_glyph(true, true, false, false), Some('│'));
    assert_eq!(seam_glyph(false, false, true, true), Some('─'));
    assert_eq!(seam_glyph(true, true, false, true), Some('├'));
    assert_eq!(seam_glyph(true, false, true, true), Some('┴'));
    assert_eq!(seam_glyph(true, true, true, true), Some('┼'));
    assert_eq!(seam_glyph(false, false, false, false), None);
}

/// The Fluent gap is the terminal's own ground with no glyph until the seam
/// pass draws it: no seam-tone slab. NEGATIVE CONTROL: the Classic gap is the
/// slab, ground and ink both the seam tone.
#[test]
fn a_fluent_gap_is_the_terminal_ground_and_a_classic_one_the_slab() {
    let theme = Theme::default();
    let fluent = with_split_chrome(SplitChrome::Fluent, || {
        crate::app_render::divider_cell(theme)
    });
    assert_eq!(fluent.bg, crate::settings::u32_rgb(theme.bg));
    assert_eq!(fluent.ch, ' ');
    let classic = with_split_chrome(SplitChrome::Classic, || {
        crate::app_render::divider_cell(theme)
    });
    assert_eq!(classic.bg, classic.fg, "CONTROL: the slab");
    assert_ne!(classic.bg, crate::settings::u32_rgb(theme.bg), "CONTROL");
}

/// Two panes side by side, each under its header: the gap column is one
/// hairline from the grid's top to its bottom, in the seam ink (the focus ring
/// is not a cell colour), on the header band's ground beside the header row
/// and the terminal's below it. No pane cell is touched.
#[test]
fn a_side_by_side_split_gets_one_seam_hairline() {
    let (rows, cols) = (5, 11);
    let mut dst = seeded(rows, cols);
    let layout = SeamLayout {
        allocations: vec![cell_box(0, 0, 5, 5), cell_box(0, 6, 5, 5)],
        headers: vec![cell_box(0, 0, 1, 5), cell_box(0, 6, 1, 5)],
    };
    stamp_seam_lines(&mut dst, rows, cols, INKS, &layout, &[]);
    for r in 0..rows {
        let gap = dst.cells[r][5];
        assert_eq!(gap.ch, '│', "row {r}: a run reaching both grid edges");
        assert_eq!(gap.fg, INKS.line, "row {r}: the seam, whoever has focus");
        let ground = if r == 0 { INKS.band } else { INKS.ground };
        assert_eq!(gap.bg, ground, "row {r}");
        for c in (0..5).chain(6..11) {
            assert_eq!(dst.cells[r][c].ch, ' ', "({r}, {c}) is a pane's");
        }
    }
}

/// A tee: a full-height left pane beside a right column split in two. The
/// hairlines join (`├`) where the horizontal gap meets the vertical one and
/// run to the grid's right edge, all in the seam ink.
#[test]
fn a_tee_joins_its_hairlines() {
    let (rows, cols) = (7, 11);
    let mut dst = seeded(rows, cols);
    let layout = SeamLayout {
        allocations: vec![
            cell_box(0, 0, 7, 5),
            cell_box(0, 6, 3, 5),
            cell_box(4, 6, 3, 5),
        ],
        // The bottom-right pane's header faces the left pane's content.
        headers: vec![cell_box(4, 6, 1, 5)],
    };
    stamp_seam_lines(&mut dst, rows, cols, INKS, &layout, &[]);
    assert_eq!(
        dst.cells[4][5].bg, INKS.ground,
        "a header on one side only: the band stops at the hairline"
    );
    let at = |r: usize, c: usize| (dst.cells[r][c].ch, dst.cells[r][c].fg);
    assert_eq!(at(3, 5), ('├', INKS.line), "the tee");
    assert_eq!(at(3, 7), ('─', INKS.line));
    assert_eq!(
        at(3, 10),
        ('─', INKS.line),
        "the run reaches the right edge"
    );
    assert_eq!(at(5, 5), ('│', INKS.line));
    assert_eq!(at(1, 5), ('│', INKS.line));
}

/// The pixels a set of quads covers, `(x, y)`.
fn covered(quads: &[GlowQuad]) -> std::collections::BTreeSet<(u16, u16)> {
    quads
        .iter()
        .flat_map(|q| (q.y..q.y + q.h).flat_map(move |y| (q.x..q.x + q.w).map(move |x| (x, y))))
        .collect()
}

/// THE FOCUS RING SAYS WHICH SIDE HAS FOCUS. Two panes side by side at the
/// 9×23 Windows cell (hairline at the gap's pixel 4): the left pane focused
/// puts one accent line at the gap's pixel 2, the right pane at pixel 6, each
/// the whole allocation tall, two pixels from the text and one stroke from
/// the hairline; nothing on the window's edges. NEGATIVE CONTROL: the two
/// focus states draw different pixels (the 2026-10-18 build inked the shared
/// hairline itself, the same pixels whichever pane had focus).
#[test]
fn the_ring_is_on_the_focused_panes_side_of_the_hairline() {
    let (rows, cols) = (30, 111);
    let gap = 55u32;
    let ring_of = |alloc: CellBox| {
        let mut out = Vec::new();
        push_focus_ring(
            &mut out,
            bare(alloc),
            (rows, cols),
            (9, 23),
            23,
            [0x50, 0xfa, 0x7b],
        );
        out
    };
    let left = ring_of(cell_box(0, 0, rows, 55));
    let right = ring_of(cell_box(0, 56, rows, 55));
    let xs = |q: &[GlowQuad]| {
        q.iter()
            .map(|q| (q.x, q.w))
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(
        xs(&left),
        [((gap * 9 + 2) as u16, 1)].into(),
        "left focused"
    );
    assert_eq!(
        xs(&right),
        [((gap * 9 + 6) as u16, 1)].into(),
        "right focused"
    );
    for quads in [&left, &right] {
        let px = covered(quads);
        assert_eq!(px.len(), rows * 23, "the allocation's whole height, once");
        assert!(
            px.iter()
                .all(|&(_, y)| (23..23 + rows as u16 * 23).contains(&y)),
            "lifted by the chrome row"
        );
        assert!(quads.iter().all(|q| q.alpha == 255 && q.color == 0x50_fa7b));
        assert!(
            quads
                .iter()
                .all(|q| u32::from(q.y) / 23 == u32::from(q.y + q.h - 1) / 23),
            "one row band per quad"
        );
    }
    assert_ne!(
        covered(&left),
        covered(&right),
        "CONTROL: the ring moves with focus"
    );
    // A lone pane borders no divider: no ring.
    assert!(ring_of(cell_box(0, 0, rows, cols)).is_empty());
}

/// In a 2×2 grid the ring is an L hugging the focused pane's inner corner:
/// the vertical line stops on the horizontal one, both one stroke inside the
/// hairlines, and at 2x every offset doubles.
#[test]
fn the_ring_turns_the_inner_corner_at_every_scale() {
    for (cw, ch, t) in [(9u32, 23u32, 1u32), (18, 46, 2)] {
        // Top-left of a 2x2 grid of 9x40 panes: gaps at column 40, row 9.
        let mut out = Vec::new();
        push_focus_ring(
            &mut out,
            bare(cell_box(0, 0, 9, 40)),
            (19, 81),
            (cw, ch),
            0,
            [1, 2, 3],
        );
        let px = covered(&out);
        let vl = (cw - t) / 2;
        let hl = (ch - t) / 2;
        let x_line = (40 * cw + vl - 2 * t) as u16;
        let y_line = (9 * ch + hl - 2 * t) as u16;
        assert!(
            px.contains(&(x_line, 0)),
            "{cw}: the vertical line from the top"
        );
        assert!(
            px.contains(&(0, y_line)),
            "{cw}: the horizontal line from the left edge"
        );
        assert!(
            px.contains(&(x_line, y_line)),
            "{cw}: they meet at the corner"
        );
        assert!(
            !px.iter()
                .any(|&(x, y)| x > x_line + t as u16 - 1 || y > y_line + t as u16 - 1),
            "{cw}: nothing past the corner"
        );
        assert!(
            !px.contains(&(x_line + t as u16, 0)),
            "{cw}: one stroke clear of the hairline"
        );
    }
}

/// THE RING MEETS THE HEADER'S RULE IN AN L. Two panes side by side, each
/// under a header, either focused, at the 100%, 150% and 200% Windows cells:
/// the ring's side line starts on the top row of the header's accent rule
/// (`crate::pane_header::focus_rule_rows`) and runs to the grid's foot, and on
/// the rule's rows the ring fills every pixel between that line and the
/// pane's edge, where the rule itself begins, so the two strokes join; nothing
/// of the ring stands above the rule or past the line. In a 2×2 grid a lower
/// pane's header takes the place of the ring's top line. NEGATIVE CONTROL:
/// without the header the ring runs up through the header row to its top and
/// leaves the pixels between it and the pane's edge bare on the rule's rows
/// (the 2026-10-18 judge's `focus-ring-header-corner-notch`: 2 px at 100%).
#[test]
fn the_ring_meets_the_header_rule_in_an_l() {
    for (cw, ch) in [(9u32, 23u32), (13, 33), (14, 35), (18, 46)] {
        let t = scrollbar::hairline_stroke(cw, ch);
        let vl = scrollbar::hairline_start(cw, t);
        let (at, thick) = crate::pane_header::focus_rule_rows(ch);
        let grid = (30, 111);
        let bottom = 30 * ch;
        let gap_x = 55 * cw;
        // (allocation, the ring's line x, the pane's edge beside the gap)
        for (alloc, line_x, pane_edge) in [
            (cell_box(0, 0, 30, 55), gap_x + vl - 2 * t, gap_x),
            (cell_box(0, 56, 30, 55), gap_x + vl + 2 * t, gap_x + cw),
        ] {
            let ring = FocusRing {
                alloc,
                header_row: Some(0),
            };
            let px = covered_rects(&focus_ring_rects(ring, grid, (cw, ch)));
            let what = format!("{cw}x{ch} col {}", alloc.col);
            assert!(
                px.iter().all(|&(_, y)| y >= at),
                "{what}: nothing above the rule's top row {at}"
            );
            for y in at..bottom {
                assert!(px.contains(&(line_x, y)), "{what}: the line at y {y}");
            }
            let (lo, hi) = (line_x.min(pane_edge), (line_x + t).max(pane_edge));
            for y in at..at + thick {
                for x in lo..hi {
                    assert!(px.contains(&(x, y)), "{what}: the corner at ({x}, {y})");
                }
            }
            assert!(
                px.iter().all(|&(x, _)| (lo..hi).contains(&x)),
                "{what}: nothing past the line or into the pane"
            );
            // CONTROL: the bare ring passes the rule with a notch.
            let old = covered_rects(&focus_ring_rects(bare(alloc), grid, (cw, ch)));
            assert!(old.contains(&(line_x, 0)), "{what}: CONTROL: from the top");
            let notch = (lo..hi).filter(|&x| !old.contains(&(x, at))).count();
            assert!(notch >= 1, "{what}: CONTROL: a {notch} px notch");
            if cw == 9 {
                assert_eq!(notch, 2, "{what}: CONTROL: 2 px at 100%");
            }
        }
        // A lower pane's header stands in for the ring's top line.
        let lower = FocusRing {
            alloc: cell_box(10, 56, 20, 55),
            header_row: Some(10),
        };
        let px = covered_rects(&focus_ring_rects(lower, grid, (cw, ch)));
        assert!(
            px.iter().all(|&(_, y)| y >= 10 * ch + at),
            "{cw}x{ch}: no line over the header"
        );
        let with_line = covered_rects(&focus_ring_rects(bare(lower.alloc), grid, (cw, ch)));
        assert!(
            with_line.iter().any(|&(_, y)| y < 10 * ch),
            "{cw}x{ch}: CONTROL: the bare ring's top line is in the gap above"
        );
    }
}

/// The pixels a set of rectangles `(x, y, w, h)` covers.
fn covered_rects(rects: &[(u32, u32, u32, u32)]) -> std::collections::BTreeSet<(u32, u32)> {
    rects
        .iter()
        .flat_map(|&(x, y, w, h)| (y..y + h).flat_map(move |py| (x..x + w).map(move |px| (px, py))))
        .collect()
}

/// A retained row keeps what the last frame drew there.
#[test]
fn a_retained_row_is_left_alone() {
    let (rows, cols) = (3, 7);
    let mut dst = seeded(rows, cols);
    let layout = SeamLayout {
        allocations: vec![cell_box(0, 0, 3, 3), cell_box(0, 4, 3, 3)],
        headers: vec![],
    };
    stamp_seam_lines(&mut dst, rows, cols, INKS, &layout, &[false, true, false]);
    assert_eq!(dst.cells[0][3].ch, '│');
    assert_eq!(dst.cells[1][3].ch, ' ', "retained");
    assert_eq!(dst.cells[2][3].ch, '│');
}

/// A real split composed under each chrome: Fluent draws the hairline down
/// the gap column in the seam ink, inks no half-block, and rings the focused
/// pane on its own side of the hairline, moving with focus and gone while the
/// window is unfocused; Classic keeps its slab and ring
/// (`app_render_active_pane_mark_compose_tests`). NEGATIVE CONTROL: the
/// 2026-10-18 build inked the gap cells themselves in the ring colour while
/// focused, so the divider read the same whichever pane had focus.
#[test]
fn a_composed_fluent_split_has_a_hairline_gap_and_no_half_block() {
    with_split_chrome(SplitChrome::Fluent, || {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.split_active_stub_tab(wid);
        app.windows.get_mut(&wid).expect("window").focused = true;
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (usize::from(ws.rows), usize::from(ws.cols))
        };
        assert!(
            app.redraw_compose(wid, rows, cols, false, false, None, 0, Instant::now())
                .is_some()
        );
        let plan = app.active_visible_leaf_plan(wid).expect("split plan");
        let left = CellBox::of(plan.leaves[0].outer_rect());
        let gap = left.col + left.cols;
        let cells = &app.windows[&wid].input_scratch.cells;
        for (r, row) in cells.iter().enumerate().take(rows) {
            assert_eq!(row[gap].ch, '│', "row {r}");
        }
        assert!(
            cells
                .iter()
                .flatten()
                .all(|c| !matches!(c.ch, '▐' | '▌' | '▁' | '▔')),
            "no Classic half-block"
        );
        let inks = SeamInks::derive(
            app.chrome_palette(crate::app_render::ChromeSurface::Grid),
            app.chrome_palette(crate::app_render::ChromeSurface::Header),
        );
        assert!(
            cells.iter().take(rows).all(|row| row[gap].fg == inks.line),
            "the hairline is the seam ink while the window has focus"
        );
        let (cw, ch) = app.win_cell_size(wid);
        let stroke = scrollbar::hairline_stroke(cw as u32, ch as u32);
        let ring = (u32::from(inks.ring[0]) << 16)
            | (u32::from(inks.ring[1]) << 8)
            | u32::from(inks.ring[2]);
        let ring_quads = |app: &App| {
            app.windows[&wid]
                .pane_overlay
                .pending_quads()
                .iter()
                .filter(|q| q.alpha == 255 && q.color == ring)
                .copied()
                .collect::<Vec<_>>()
        };
        // The panes have headers: the ring starts on the focused header's
        // rule (`the_ring_meets_the_header_rule_in_an_l`).
        let header = plan.leaves[1].header.map(CellBox::of).expect("headers");
        let lift = usize::from(app.chrome_rows(wid)) * ch;
        let (rule_at, rule_thick) = crate::pane_header::focus_rule_rows(ch as u32);
        let rule_top = lift + header.row * ch + rule_at as usize;
        // The ring's vertical lines (a stroke wide), as pixel columns, read
        // below the rule's rows: on them the L's fill between a line and its
        // pane's edge is a stroke wide too in a cell seven strokes wide (the
        // headless window's on macOS, where it read as a second line), and is
        // no line.
        let below_rule = rule_top + rule_thick as usize;
        let ring_x = |app: &App| {
            ring_quads(app)
                .iter()
                .filter(|q| u32::from(q.w) == stroke && usize::from(q.y) >= below_rule)
                .map(|q| usize::from(q.x))
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            ring_quads(&app).iter().map(|q| usize::from(q.y)).min(),
            Some(rule_top),
            "the ring's top is the header rule's"
        );
        let gap_x = gap * cw;
        let right_side = ring_x(&app);
        assert_eq!(right_side.len(), 1, "one line: {right_side:?}");
        let x = *right_side.first().expect("a ring");
        assert!(
            x > gap_x + cw / 2 && x < gap_x + cw,
            "the focused (right) pane's side of the gap: {x} in {gap_x}..{}",
            gap_x + cw
        );
        // Focus moves to the left pane: the ring crosses the hairline.
        let left_session = app
            .active_tree(wid)
            .expect("tree")
            .sessions()
            .into_iter()
            .find(|s| Some(*s) != app.front_terminal(wid).map(|t| t.session))
            .expect("the other pane");
        assert!(
            app.active_tree_mut(wid)
                .is_some_and(|tree| tree.set_focus(left_session))
        );
        let active = app.windows[&wid].tabs.active;
        assert!(app.sync_tab_model_from_layout(wid, active));
        app.sync_window(wid);
        let at = Instant::now() + Duration::from_millis(16);
        assert!(
            app.redraw_compose(wid, rows, cols, false, false, None, 0, at)
                .is_some()
        );
        let left_side = ring_x(&app);
        assert_eq!(left_side.len(), 1, "one line: {left_side:?}");
        let x = *left_side.first().expect("a ring");
        assert!(
            x >= gap_x && x < gap_x + cw / 2,
            "the focused (left) pane's side of the gap: {x} in {gap_x}..{}",
            gap_x + cw
        );
        // Unfocused, the same gap is the plain seam and there is no ring.
        app.windows.get_mut(&wid).expect("window").focused = false;
        assert!(
            app.redraw_compose(
                wid,
                rows,
                cols,
                false,
                false,
                None,
                0,
                at + Duration::from_millis(16)
            )
            .is_some()
        );
        assert_eq!(
            app.windows[&wid].input_scratch.cells[rows - 1][gap].fg,
            inks.line
        );
        assert!(ring_x(&app).is_empty(), "no ring in an unfocused window");
    });
}

/// A ROW OF PANE HEADERS IS ONE BAR ACROSS THE DIVIDER: in a composed Fluent
/// split the gap cell of the header row wears the headers' own fill
/// ([`SeamInks::band`], `crate::pane_header::header_colors`), whatever that
/// fill is. NEGATIVE CONTROL: the tab strip's band tone, which the gap took
/// before, is not the header's fill under the Fluent chrome, where the header
/// sits on the pane's ground, so the gap stood in the header row as a grey
/// notch.
#[test]
fn the_gap_between_two_headers_wears_their_fill() {
    with_split_chrome(SplitChrome::Fluent, || {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.split_active_stub_tab(wid);
        app.windows.get_mut(&wid).expect("window").focused = true;
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (usize::from(ws.rows), usize::from(ws.cols))
        };
        assert!(
            app.redraw_compose(wid, rows, cols, false, false, None, 0, Instant::now())
                .is_some()
        );
        let plan = app.active_visible_leaf_plan(wid).expect("split plan");
        let header = plan.leaves[0]
            .header
            .map(CellBox::of)
            .expect("fixture: headers");
        let left = CellBox::of(plan.leaves[0].outer_rect());
        let gap = left.col + left.cols;
        let ws = &app.windows[&wid];
        let row = &ws.input_scratch.cells[ws.input_scratch.cells.len() - rows + header.row];
        let fill = crate::pane_header::header_colors(
            app.chrome_palette(crate::app_render::ChromeSurface::Header),
        )
        .bar_bg;
        assert_eq!(row[gap - 1].bg, fill, "fixture: the left header's own cell");
        assert_eq!(
            row[gap + 1].bg,
            fill,
            "fixture: the right header's own cell"
        );
        assert_eq!(row[gap].bg, fill, "the gap continues the headers' fill");
        let band = crate::chrome_band::band_colors(
            app.chrome_palette(crate::app_render::ChromeSurface::Header),
        )
        .bar_bg;
        assert_ne!(band, fill, "CONTROL: the band's tone is not the header's");
    });
}

// ---- Overlays ----------------------------------------------------------------------

fn inputs() -> OverlayInputs {
    OverlayInputs {
        scrollbar: true,
        hollow: true,
        fluent: true,
        ring: None,
        ring_rgb: [0x50, 0xfa, 0x7b],
        cell: (9, 23),
        pad: 12,
        win_rows: 30,
        win_cols: 111,
        chrome_rows: 1,
        ink: [0xd0, 0xd0, 0xd0],
        ground: INKS.ground,
        animated: true,
    }
}

/// What `quad` shows over `under`: its premultiplied colour plus what its
/// opacity lets through.
fn seen(quad: &GlowQuad, under: [u8; 3]) -> [u8; 3] {
    let a = u32::from(quad.alpha);
    std::array::from_fn(|i| {
        let c = (quad.color >> (16 - 8 * i)) & 0xff;
        (c + (u32::from(under[i]) * (255 - a) + 127) / 255).min(255) as u8
    })
}

fn pane(session: u64, rect: CellBox, focused: bool, offset: u32, history: u32) -> PaneFacts {
    PaneFacts {
        session,
        rect,
        focused,
        offset,
        history,
        alt: false,
        cursor: Some((2, 3)),
        cursor_rgb: [0x50, 0xfa, 0x7b],
        moved: None,
    }
}

/// One frame of `facts` at `now`: the quads, and whether a bar is fading.
fn frame(
    overlay: &mut PaneOverlay,
    pill: &mut crate::scroll_motion::PillFade,
    facts: &[PaneFacts],
    now: Instant,
) -> (Vec<GlowQuad>, bool) {
    overlay.begin();
    for f in facts {
        overlay.note(*f, now);
    }
    let mut shown = false;
    let fp = overlay.finish(inputs(), pill, &mut shown, now);
    assert_eq!(
        fp == 0,
        overlay.pending.is_empty(),
        "fp is 0 exactly when idle"
    );
    (overlay.pending.clone(), shown)
}

fn max_alpha(quads: &[GlowQuad]) -> u8 {
    quads.iter().map(|q| q.alpha).max().unwrap_or(0)
}

/// THE SCROLLBAR'S STATES: nothing at the live bottom; a scrolling pane's thumb
/// at the active opacity, fading after the hold; a view parked in history keeps
/// a quieter resting thumb; every quad in the window's right padding, none on a
/// column of text. (The Fluent thin thumb paints each step at
/// `scrollbar::rest_alpha`.)
#[test]
fn the_bar_is_hidden_live_active_while_scrolling_and_resting_while_parked() {
    let lit = |alpha: u8| scrollbar::rest_alpha(alpha, inputs().fluent);
    let mut overlay = PaneOverlay::default();
    let mut pill = crate::scroll_motion::PillFade::default();
    let full = cell_box(0, 0, 30, 111);
    let t0 = Instant::now();

    let (quads, fading) = frame(&mut overlay, &mut pill, &[pane(1, full, true, 0, 200)], t0);
    assert!(quads.is_empty() && !fading, "live and still: nothing");

    // The view moves 20 lines up (history unchanged): the bar shows, active.
    let t1 = t0 + Duration::from_millis(16);
    let (quads, fading) = frame(&mut overlay, &mut pill, &[pane(1, full, true, 20, 200)], t1);
    assert!(fading, "a moving view fades");
    assert_eq!(max_alpha(&quads), lit(scrollbar::ALPHA_ACTIVE));
    assert!(pill.is_active(t1, true), "the move armed the fade's wakes");
    let gutter = 111 * 9..111 * 9 + 12;
    assert!(
        quads
            .iter()
            .all(|q| gutter.contains(&u32::from(q.x)) && gutter.contains(&u32::from(q.x + q.w - 1))),
        "in the padding, never over text: {quads:?}"
    );

    // Long after the move: the parked view rests at the quieter opacity.
    let t2 = t1 + Duration::from_millis(5_000);
    let (quads, fading) = frame(&mut overlay, &mut pill, &[pane(1, full, true, 20, 200)], t2);
    assert!(!fading);
    assert_eq!(max_alpha(&quads), lit(scrollbar::ALPHA_REST));

    // Output under the parked view moves offset and history together: no flash.
    let t3 = t2 + Duration::from_millis(16);
    let (quads, _) = frame(&mut overlay, &mut pill, &[pane(1, full, true, 25, 205)], t3);
    assert_eq!(
        max_alpha(&quads),
        lit(scrollbar::ALPHA_REST),
        "output is not reading"
    );

    // Back to live: active, then nothing once it has faded.
    let t4 = t3 + Duration::from_millis(16);
    let (quads, _) = frame(&mut overlay, &mut pill, &[pane(1, full, true, 0, 205)], t4);
    assert_eq!(max_alpha(&quads), lit(scrollbar::ALPHA_ACTIVE));
    let t5 = t4 + Duration::from_millis(5_000);
    let (quads, fading) = frame(&mut overlay, &mut pill, &[pane(1, full, true, 0, 205)], t5);
    assert!(quads.is_empty() && !fading, "faded out at the bottom");
}

/// THE RESTING THUMB IN A FLUENT GAP KEEPS A STROKE OF GROUND FROM THE RING.
/// Two panes side by side, the left one parked in history (so its thumb rests
/// in the divider gap), either pane focused, at the 100%, 150% and 200%
/// Windows cells: the thumb is the thin indicator (`Metrics::fine`) over the
/// hairline, opaque in the tone twice the resting opacity lays on the ground
/// (so it is one tone over the line and the ground), and every ring pixel
/// stands more than a stroke from every thumb pixel of its row, so at least a
/// stroke of ground lies between them. NEGATIVE CONTROL: the same thumb
/// centred on the line, as with no ring beside it, comes within a stroke of
/// the ring on one side or the other at every scale (at 100% it touches it).
#[test]
fn a_resting_gap_thumb_keeps_a_stroke_of_ground_from_the_ring() {
    let ring_rgb = [0x50, 0xfa, 0x7b];
    let ring_color = 0x50_fa7b;
    let (left, right) = (cell_box(0, 0, 30, 55), cell_box(0, 56, 30, 55));
    let lit = 2 * scrollbar::ALPHA_REST;
    let tone = scrollbar::over_ground(inputs().ink, inputs().ground, lit, lit).0;
    for (cw, ch) in [(9u32, 23u32), (13, 33), (17, 44)] {
        let m = Metrics::for_cell(cw, ch);
        let line = 55 * cw + scrollbar::hairline_start(cw, m.stroke);
        let focused = |focus_left: bool| bare(if focus_left { left } else { right });
        // The thumb's pixels, and what its body shows over the line, with
        // or without the ring.
        let run = |focus_left: bool, ring: bool| {
            let mut overlay = PaneOverlay::default();
            let mut pill = crate::scroll_motion::PillFade::default();
            let now = Instant::now();
            let inputs = OverlayInputs {
                ring: ring.then(|| focused(focus_left)),
                ring_rgb,
                cell: (cw as usize, ch as usize),
                chrome_rows: 0,
                ..inputs()
            };
            overlay.begin();
            for (session, rect, focused, offset) in
                [(1, left, focus_left, 20), (2, right, !focus_left, 0)]
            {
                overlay.note(
                    PaneFacts {
                        cursor: None,
                        ..pane(session, rect, focused, offset, 200)
                    },
                    now,
                );
            }
            let mut shown = false;
            overlay.finish(inputs, &mut pill, &mut shown, now);
            let thumb: Vec<GlowQuad> = overlay
                .pending
                .iter()
                .filter(|q| !(q.alpha == 255 && q.color == ring_color))
                .copied()
                .collect();
            let alpha = max_alpha(&thumb);
            let body = thumb
                .iter()
                .find(|q| q.alpha == alpha)
                .map(|q| (alpha, seen(q, INKS.line)));
            (covered(&thumb), body)
        };
        // The ring's own pixels, wherever the thumb is.
        let ring_px = |focus_left: bool| {
            let mut out = Vec::new();
            push_focus_ring(
                &mut out,
                focused(focus_left),
                (30, 111),
                (cw, ch),
                0,
                ring_rgb,
            );
            covered(&out)
        };
        // The closest a thumb pixel comes to a ring pixel on the same row.
        let nearest = |ring: &std::collections::BTreeSet<(u16, u16)>,
                       thumb: &std::collections::BTreeSet<(u16, u16)>| {
            thumb
                .iter()
                .flat_map(|&(tx, ty)| {
                    ring.iter()
                        .filter(move |&&(_, ry)| ry == ty)
                        .map(move |&(rx, _)| u32::from(tx.abs_diff(rx)))
                })
                .min()
                .expect("the ring and the thumb share rows")
        };
        let mut crowded = false;
        for focus_left in [true, false] {
            let ring = ring_px(focus_left);
            let (thumb, body) = run(focus_left, true);
            assert!(!ring.is_empty() && !thumb.is_empty(), "{cw}: fixture");
            let xs: std::collections::BTreeSet<u32> =
                thumb.iter().map(|&(x, _)| u32::from(x)).collect();
            assert_eq!(xs.len() as u32, m.fine, "{cw}: the thin indicator");
            assert!(
                xs.contains(&line) && xs.contains(&(line + m.stroke - 1)),
                "{cw} focus_left={focus_left}: over the hairline: {xs:?}"
            );
            assert_eq!(body, Some((255, tone)), "{cw}: lit, one tone over the line");
            assert!(
                nearest(&ring, &thumb) > m.stroke,
                "{cw} focus_left={focus_left}: a stroke of ground from the ring"
            );
            let (centred, _) = run(focus_left, false);
            crowded |= nearest(&ring, &centred) <= m.stroke;
        }
        assert!(crowded, "{cw}: CONTROL: centred, the thumb crowds the ring");
    }
}

/// ONE RESTING IDIOM IN A SPLIT (the Fluent chrome): two panes side by side,
/// both parked in history, so the left pane's thumb rests in the divider gap
/// and the right pane's in the window's padding. Both are the thin indicator
/// at the same width showing the same tone on the ground, at 100% and 200%,
/// and both widen under the pointer: the edge thumb to `Metrics::wide`, the
/// gap thumb beside the focused pane's ring as far as the ring's far side
/// lets it ([`scrollbar::wide_span`]). NEGATIVE CONTROL: the Classic chrome
/// keeps its thin thumb at its own opacity in both places. (The 2026-10-18
/// build drew a lit one-pixel line in the gap and a three-pixel pill at the
/// plain opacity at the edge: the judge's `two-scrollbar-idioms-in-one-split`.)
#[test]
fn a_split_rests_both_its_thumbs_in_one_idiom() {
    let (left, right) = (cell_box(0, 0, 30, 55), cell_box(0, 56, 30, 55));
    let close = |a: [u8; 3], b: [u8; 3]| (0..3).all(|i| a[i].abs_diff(b[i]) <= 1);
    for (cw, ch) in [(9u32, 23u32), (18, 46)] {
        let m = Metrics::for_cell(cw, ch);
        let edge = (111 * cw, 111 * cw + 12 * (cw / 9));
        let gap_lane = scrollbar::lane(
            (0, 0, 30, 55),
            111,
            (cw as usize, ch as usize),
            12 * (cw as usize / 9),
            m,
        )
        .expect("gap lane");
        // Each lane's thumb: (width, tone on the ground), with the pane
        // `hover` under the pointer.
        let run = |fluent: bool, hover: Option<u64>| {
            let mut overlay = PaneOverlay {
                hover,
                ..PaneOverlay::default()
            };
            let mut pill = crate::scroll_motion::PillFade::default();
            let now = Instant::now();
            let inputs = OverlayInputs {
                fluent,
                ring: Some(bare(left)),
                cell: (cw as usize, ch as usize),
                pad: 12 * (cw as usize / 9),
                chrome_rows: 0,
                ..inputs()
            };
            overlay.begin();
            for (session, rect, focused) in [(1, left, true), (2, right, false)] {
                overlay.note(
                    PaneFacts {
                        cursor: None,
                        ..pane(session, rect, focused, 20, 200)
                    },
                    now,
                );
            }
            let mut shown = false;
            overlay.finish(inputs, &mut pill, &mut shown, now);
            let ring = (u32::from(inputs.ring_rgb[0]) << 16)
                | (u32::from(inputs.ring_rgb[1]) << 8)
                | u32::from(inputs.ring_rgb[2]);
            let lane_of = |q: &GlowQuad| u32::from(q.x) >= edge.0;
            // The thumb's body (its most opaque quads; the track under a
            // widened thumb and the caps' rims are fainter): width, and what
            // it shows on the ground.
            let thumb = |at_edge: bool| {
                let quads: Vec<GlowQuad> = overlay
                    .pending
                    .iter()
                    .filter(|q| !(q.alpha == 255 && q.color == ring))
                    .filter(|q| lane_of(q) == at_edge)
                    .copied()
                    .collect();
                let alpha = max_alpha(&quads);
                let body: Vec<&GlowQuad> = quads.iter().filter(|q| q.alpha == alpha).collect();
                let x0 = body.iter().map(|q| q.x).min().expect("a thumb");
                let x1 = body.iter().map(|q| q.x + q.w).max().expect("a thumb");
                (u32::from(x1 - x0), seen(body[0], inputs.ground))
            };
            (thumb(false), thumb(true))
        };
        let (gap, at_edge) = run(true, None);
        assert_eq!(gap.0, at_edge.0, "{cw}: one width");
        assert_eq!(gap.0, m.fine, "{cw}: thin");
        assert!(
            close(gap.1, at_edge.1),
            "{cw}: one ink: {gap:?} {at_edge:?}"
        );
        let lit = 2 * scrollbar::ALPHA_REST;
        let tone = scrollbar::over_ground(inputs().ink, inputs().ground, lit, lit).0;
        assert!(close(at_edge.1, tone), "{cw}: lit: {at_edge:?}");
        let (gap_hover, _) = run(true, Some(1));
        let (_, edge_hover) = run(true, Some(2));
        let beside_ring = scrollbar::wide_span(gap_lane, m, Some(scrollbar::RingSide::Left)).1;
        assert_eq!(
            gap_hover.0,
            beside_ring.min(m.wide),
            "{cw}: the gap thumb widens on the ring's far side"
        );
        assert!(gap_hover.0 > m.fine, "{cw}: wider than at rest");
        assert_eq!(edge_hover.0, m.wide, "{cw}: the edge thumb widens");
        let (gap, at_edge) = run(false, None);
        let classic = scrollbar::over_ground(
            inputs().ink,
            inputs().ground,
            scrollbar::ALPHA_REST,
            scrollbar::ALPHA_REST,
        )
        .0;
        assert_eq!(
            (gap.0, at_edge.0),
            (m.thin, m.thin),
            "{cw}: CONTROL: Classic"
        );
        assert!(
            close(gap.1, classic) && close(at_edge.1, classic),
            "{cw}: CONTROL: Classic's own opacity: {gap:?} {at_edge:?}"
        );
    }
}

/// THE WIDENED GAP THUMB LEAVES THE RING WHOLE (the Fluent chrome): two panes
/// side by side, the left one parked in history and under the pointer, either
/// pane focused, at the 100%, 150% and 200% Windows cells. No pixel of the
/// widened thumb or its track lies on the ring's pixels, a stroke of ground
/// lies between them on every row, and the thumb is one tone over the
/// hairline. NEGATIVE CONTROL: with the ring left out of the overlays'
/// inputs, the same widened bar is centred on the line and covers pixels the
/// ring paints (the 2026-10-18 judge's `J4F-3-gap-thumb-hover-and-two-tone`:
/// the ring turned #A2DFB1 under the thumb and #5BF682 under the track).
#[test]
fn a_hovered_gap_thumb_leaves_the_ring_whole() {
    let ring_rgb = [0x50, 0xfa, 0x7b];
    let ring_color = 0x50_fa7b;
    let (left, right) = (cell_box(0, 0, 30, 55), cell_box(0, 56, 30, 55));
    for (cw, ch) in [(9u32, 23u32), (13, 33), (18, 46)] {
        let m = Metrics::for_cell(cw, ch);
        let focused = |focus_left: bool| bare(if focus_left { left } else { right });
        // The bar's pixels and its thumb body's quads, the left pane hovered.
        let run = |focus_left: bool, ring: bool| {
            let mut overlay = PaneOverlay {
                hover: Some(1),
                ..PaneOverlay::default()
            };
            let mut pill = crate::scroll_motion::PillFade::default();
            let now = Instant::now();
            let inputs = OverlayInputs {
                ring: ring.then(|| focused(focus_left)),
                ring_rgb,
                cell: (cw as usize, ch as usize),
                chrome_rows: 0,
                ..inputs()
            };
            overlay.begin();
            for (session, rect, focused, offset) in
                [(1, left, focus_left, 20), (2, right, !focus_left, 0)]
            {
                overlay.note(
                    PaneFacts {
                        cursor: None,
                        ..pane(session, rect, focused, offset, 200)
                    },
                    now,
                );
            }
            let mut shown = false;
            overlay.finish(inputs, &mut pill, &mut shown, now);
            let bar: Vec<GlowQuad> = overlay
                .pending
                .iter()
                .filter(|q| !(q.alpha == 255 && q.color == ring_color))
                .copied()
                .collect();
            let alpha = max_alpha(&bar);
            let body: Vec<GlowQuad> = bar.iter().filter(|q| q.alpha == alpha).copied().collect();
            (covered(&bar), body)
        };
        let mut covered_ring = false;
        for focus_left in [true, false] {
            let mut out = Vec::new();
            push_focus_ring(
                &mut out,
                focused(focus_left),
                (30, 111),
                (cw, ch),
                0,
                ring_rgb,
            );
            let ring = covered(&out);
            let (bar, body) = run(focus_left, true);
            assert!(!bar.is_empty() && !body.is_empty(), "{cw}: fixture");
            assert!(
                bar.is_disjoint(&ring),
                "{cw} focus_left={focus_left}: the ring stays whole"
            );
            // The ring's pixels by row, so each bar pixel meets only its own
            // row's (test-run speed: a scan of the whole ring per bar pixel
            // was nine tenths of this test).
            let mut ring_rows: std::collections::HashMap<u16, Vec<u16>> =
                std::collections::HashMap::new();
            for &(rx, ry) in &ring {
                ring_rows.entry(ry).or_default().push(rx);
            }
            let nearest = bar
                .iter()
                .flat_map(|&(bx, by)| {
                    ring_rows
                        .get(&by)
                        .into_iter()
                        .flatten()
                        .map(move |&rx| u32::from(bx.abs_diff(rx)))
                })
                .min()
                .expect("the ring and the bar share rows");
            assert!(
                nearest > m.stroke,
                "{cw} focus_left={focus_left}: a stroke of ground from the ring"
            );
            assert!(
                body.iter()
                    .all(|q| q.alpha == 255 && seen(q, INKS.line) == seen(q, INKS.ground)),
                "{cw} focus_left={focus_left}: one tone over the hairline"
            );
            let (centred, _) = run(focus_left, false);
            covered_ring |= !centred.is_disjoint(&ring);
        }
        assert!(
            covered_ring,
            "{cw}: CONTROL: centred, the widened bar covers the ring"
        );
    }
}

/// Reduce Motion hides the bar without a ramp: full until the hold ends, then
/// gone.
#[test]
fn reduce_motion_shows_and_hides_without_a_ramp() {
    let mut overlay = PaneOverlay::default();
    let mut pill = crate::scroll_motion::PillFade::default();
    let full = cell_box(0, 0, 30, 111);
    let t0 = Instant::now();
    let reduced = OverlayInputs {
        animated: false,
        ..inputs()
    };
    let mut step = |offset: u32, at: Instant| {
        overlay.begin();
        overlay.note(pane(1, full, true, offset, 200), at);
        let mut shown = false;
        overlay.finish(reduced, &mut pill, &mut shown, at);
        max_alpha(&overlay.pending)
    };
    step(5, t0);
    let t1 = t0 + Duration::from_millis(16);
    assert_eq!(
        step(0, t1),
        scrollbar::rest_alpha(scrollbar::ALPHA_ACTIVE, reduced.fluent)
    );
    let mid_ramp = t1
        + Duration::from_millis(
            crate::scroll_motion::PILL_HOLD_MS + crate::scroll_motion::PILL_FADE_MS / 2,
        );
    assert_eq!(step(0, mid_ramp), 0, "no half-faded frame");
}

/// Under the pointer the thumb widens over a faint track; dragged, it is the
/// most opaque.
#[test]
fn the_hovered_bar_widens_over_a_track() {
    let mut overlay = PaneOverlay::default();
    let mut pill = crate::scroll_motion::PillFade::default();
    let full = cell_box(0, 0, 30, 111);
    let now = Instant::now();
    overlay.hover = Some(1);
    let (quads, _) = frame(&mut overlay, &mut pill, &[pane(1, full, true, 0, 200)], now);
    let m = Metrics::for_cell(9, 23);
    assert!(
        quads.iter().any(|q| u32::from(q.w) == m.wide),
        "the wide thumb"
    );
    assert!(
        quads.iter().any(|q| q.alpha == scrollbar::ALPHA_TRACK),
        "the track"
    );
    assert_eq!(max_alpha(&quads), scrollbar::ALPHA_HOVER);
    overlay.drag = Some(Drag {
        session: 1,
        grab: 4.0,
    });
    let (quads, _) = frame(&mut overlay, &mut pill, &[pane(1, full, true, 0, 200)], now);
    assert_eq!(max_alpha(&quads), scrollbar::ALPHA_DRAG);
    // A drag whose pane has gone is dropped.
    let (_, _) = frame(&mut overlay, &mut pill, &[], now);
    assert!(overlay.drag.is_none() && overlay.hover.is_none());
}

/// No history, or the alternate screen up: no bar, even hovered.
#[test]
fn a_pane_without_history_or_on_the_alternate_screen_has_no_bar() {
    let mut overlay = PaneOverlay::default();
    let mut pill = crate::scroll_motion::PillFade::default();
    let full = cell_box(0, 0, 30, 111);
    let now = Instant::now();
    overlay.hover = Some(1);
    let (quads, _) = frame(&mut overlay, &mut pill, &[pane(1, full, true, 0, 0)], now);
    assert!(quads.is_empty());
    let alt = PaneFacts {
        alt: true,
        ..pane(1, full, true, 0, 200)
    };
    let (quads, _) = frame(&mut overlay, &mut pill, &[alt], now);
    assert!(quads.is_empty());
}

/// THE UNFOCUSED PANE'S CURSOR: a hollow outline of its own cursor colour on
/// its cursor cell (the chrome rows above counted); the focused pane's cursor
/// is the frame's own and gets none, and a hidden cursor none.
#[test]
fn an_unfocused_pane_shows_a_hollow_cursor_and_the_focused_one_none() {
    let mut overlay = PaneOverlay::default();
    let mut pill = crate::scroll_motion::PillFade::default();
    let now = Instant::now();
    let left = cell_box(1, 0, 29, 55);
    let right = cell_box(1, 56, 29, 55);
    let (quads, _) = frame(
        &mut overlay,
        &mut pill,
        &[pane(1, left, false, 0, 0), pane(2, right, true, 0, 0)],
        now,
    );
    assert_eq!(quads.len(), 4, "four edges: {quads:?}");
    let (x0, y0) = (3 * 9, (1 + 1 + 2) * 23);
    for q in &quads {
        assert_eq!(q.color >> 24, 0);
        assert!(u32::from(q.x) >= x0 && u32::from(q.x + q.w) <= x0 + 9);
        assert!(u32::from(q.y) >= y0 && u32::from(q.y + q.h) <= y0 + 23);
        assert_eq!(u32::from(q.row), y0 / 23);
    }
    let hidden = PaneFacts {
        cursor: None,
        ..pane(1, left, false, 0, 0)
    };
    let (quads, _) = frame(
        &mut overlay,
        &mut pill,
        &[hidden, pane(2, right, true, 0, 0)],
        now,
    );
    assert!(quads.is_empty());
}

/// The splice takes back what it appended last, wherever it is, and appends
/// this frame's: run twice over an unrefilled stream it does not double.
#[test]
fn the_splice_never_doubles_its_quads() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut pending = Vec::new();
    scrollbar::push_bar(&mut pending, (1000, 30, 3, 60), [200; 3], 90, 23);
    let foreign = GlowQuad {
        row: 0,
        x: 5,
        y: 5,
        w: 1,
        h: 1,
        color: 0x10_1010,
        alpha: 0,
        color2: 0x10_1010,
        alpha2: 0,
    };
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.input_scratch.nova_add = vec![foreign];
        ws.pane_overlay.pending = pending.clone();
    }
    app.splice_pane_overlay(wid);
    app.splice_pane_overlay(wid);
    let nova = &app.windows[&wid].input_scratch.nova_add;
    assert_eq!(nova.len(), 1 + pending.len());
    assert_eq!(nova[0], foreign, "another producer's quad stays");
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.pane_overlay.pending.clear();
    }
    app.splice_pane_overlay(wid);
    assert_eq!(app.windows[&wid].input_scratch.nova_add, vec![foreign]);
}

/// The window pixel at terminal-grid pixel `(gx, gy)` of a headless window
/// (frame origin at the window's).
fn window_px(app: &App, wid: WindowId, gx: f64, gy: f64) -> (f64, f64) {
    let ch = app.win_cell_size(wid).1;
    let top = app.win_pad_top(wid) + app.win_head(wid) + usize::from(app.chrome_rows(wid)) * ch;
    (gx + app.win_pad(wid) as f64, gy + top as f64)
}

/// THE POINTER: hovering the bar's gutter widens it; a press on the thumb
/// grabs it and dragging up scrolls that pane into history; the release lets
/// go; a press on the track above the thumb pages up. None of it reaches the
/// grid as a selection.
#[test]
fn the_thumb_drags_its_pane_through_history_and_the_track_pages() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.scrollbar = Some(true);
    // The Windows window padding, the gutter the bar draws in.
    app.windows.get_mut(&wid).expect("window").metrics.pad = 12;
    let session = app.front_terminal(wid).expect("front").session;
    let term = app.pool.get(session).expect("session").term.clone();
    {
        let mut t = crate::term_lock(&term);
        for i in 0..400 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
    }
    let now = Instant::now();
    let refresh = |app: &mut App| {
        let plan = app.active_visible_leaf_plan(wid).expect("plan");
        app.refresh_pane_overlay_for_capture(wid, &plan, now);
    };
    refresh(&mut app);
    let facts = app.windows[&wid].pane_overlay.facts()[0];
    assert!(
        facts.history > 0 && facts.offset == 0,
        "fixture: history at live"
    );
    let inputs = app.pane_overlay_inputs(wid, false).expect("inputs");
    let (cw, ch) = inputs.cell;
    let m = Metrics::for_cell(cw as u32, ch as u32);
    let lane = scrollbar::lane(
        (0, 0, facts.rect.rows, facts.rect.cols),
        inputs.win_cols,
        inputs.cell,
        inputs.pad,
        m,
    )
    .expect("lane");
    let (top, len) =
        scrollbar::thumb(lane, facts.rect.rows as u32, facts.history, 0, m).expect("thumb");
    let x = f64::from(lane.right) - 3.0;

    // Hover over the thumb.
    let (wx, wy) = window_px(&app, wid, x, f64::from(top + len / 2));
    assert!(
        app.scrollbar_pointer_motion(wid, wx, wy),
        "the bar takes the motion"
    );
    assert_eq!(app.windows[&wid].pane_overlay.hover, Some(session));

    // Grab and drag 200 px up.
    app.windows.get_mut(&wid).expect("window").last_cursor_px = (wx, wy);
    assert!(app.scrollbar_press(wid));
    assert!(app.windows[&wid].pane_overlay.drag.is_some());
    let (wx2, wy2) = window_px(&app, wid, x, f64::from(top + len / 2) - 200.0);
    assert!(app.scrollbar_pointer_motion(wid, wx2, wy2));
    let dragged = crate::term_lock(&term).grid().display_offset();
    assert!(dragged > 0, "dragging up scrolls into history");
    assert!(app.scrollbar_release(wid));
    assert!(app.windows[&wid].pane_overlay.drag.is_none());
    assert!(!app.windows[&wid].selecting, "no selection began");

    // A press on the track above the (new) thumb pages up one screen less a line.
    refresh(&mut app);
    let before = crate::term_lock(&term).grid().display_offset();
    let (wx3, wy3) = window_px(&app, wid, x, f64::from(lane.top + 1));
    app.windows.get_mut(&wid).expect("window").last_cursor_px = (wx3, wy3);
    assert!(app.scrollbar_press(wid));
    let after = crate::term_lock(&term).grid().display_offset();
    assert_eq!(
        after,
        (before + facts.rect.rows - 1).min(facts.history as usize)
    );

    // Off the gutter: the grid's.
    let (wx4, wy4) = window_px(&app, wid, 10.0, f64::from(top));
    assert!(!app.scrollbar_pointer_motion(wid, wx4, wy4));
    assert_eq!(app.windows[&wid].pane_overlay.hover, None);
}

/// THE ROW LAW (`docs/DESIGN-render-integrity-2026-09-28.md` §3, clause 1): the
/// bar is a zero-row, zero-column surface. In a Fluent split, hovering an inner
/// pane's bar in its divider gap, dragging that pane through history and
/// letting go draws the bar (so the check judges something) and leaves every
/// pane's grid and the window's grid exactly the size they were.
#[test]
fn the_bar_shows_hovers_and_drags_without_regridding_any_pane() {
    with_split_chrome(SplitChrome::Fluent, || {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.scrollbar = Some(true);
        app.windows.get_mut(&wid).expect("window").metrics.pad = 12;
        let left = app.front_terminal(wid).expect("front").session;
        let term = app.pool.get(left).expect("session").term.clone();
        {
            let mut t = crate::term_lock(&term);
            for i in 0..400 {
                t.process(format!("line {i}\r\n").as_bytes());
            }
        }
        let right = app.split_active_stub_tab(wid);
        app.windows.get_mut(&wid).expect("window").focused = true;
        let t0 = Instant::now();
        let compose = |app: &mut App, at: Instant| {
            let (rows, cols) = {
                let ws = &app.windows[&wid];
                (usize::from(ws.rows), usize::from(ws.cols))
            };
            assert!(
                app.redraw_compose(wid, rows, cols, false, false, None, 0, at)
                    .is_some()
            );
        };
        let dims = |app: &App| {
            let ws = &app.windows[&wid];
            let pane = |sid: u64| {
                let t = crate::term_lock(&app.pool.get(sid).expect("pane").term);
                (t.rows(), t.cols())
            };
            (ws.rows, ws.cols, pane(left), pane(right))
        };
        compose(&mut app, t0);
        let before = dims(&app);

        let facts = *app.windows[&wid]
            .pane_overlay
            .facts()
            .iter()
            .find(|f| f.session == left)
            .expect("the left pane's facts");
        assert!(!facts.focused && facts.history > 0, "fixture");
        let inputs = app.pane_overlay_inputs(wid, true).expect("inputs");
        let (cw, ch) = inputs.cell;
        let m = Metrics::for_cell(cw as u32, ch as u32);
        let r = facts.rect;
        let lane = scrollbar::lane(
            (r.row, r.col, r.rows, r.cols),
            inputs.win_cols,
            inputs.cell,
            inputs.pad,
            m,
        )
        .expect("an inner lane");
        assert!(!lane.edge, "fixture: the left pane's bar is in the gap");
        let (top, len) = scrollbar::thumb(lane, r.rows as u32, facts.history, 0, m).expect("thumb");
        // Through the window's own pointer handlers (what `CursorMoved` and
        // `MouseInput` call), not the scrollbar's entry points.
        use aterm_window::event::{ElementState, MouseButton};
        // On the resting thumb, centred on the divider's hairline.
        let (sx, sw) = scrollbar::span(lane, m.thin, m);
        let x = f64::from(sx) + f64::from(sw) / 2.0;
        let (wx, wy) = window_px(&app, wid, x, f64::from(top + len / 2));
        app.on_cursor_moved(wid, wx, wy);
        assert_eq!(app.windows[&wid].pane_overlay.hover, Some(left), "hovered");
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        assert!(app.windows[&wid].pane_overlay.drag.is_some(), "grabbed");
        let (wx2, wy2) = window_px(&app, wid, x, f64::from(top + len / 2) - 150.0);
        app.on_cursor_moved(wid, wx2, wy2);
        compose(&mut app, t0 + Duration::from_millis(16));
        assert!(
            !app.windows[&wid].pane_overlay.pending.is_empty(),
            "the dragged bar draws"
        );
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert!(app.windows[&wid].pane_overlay.drag.is_none(), "let go");
        compose(&mut app, t0 + Duration::from_millis(32));
        assert!(
            crate::term_lock(&term).grid().display_offset() > 0,
            "dragged"
        );
        let ws = &app.windows[&wid];
        assert!(
            !ws.selecting && ws.held_mouse_button.is_none(),
            "the gesture was the bar's, not the grid's"
        );
        assert_eq!(
            app.front_terminal(wid).map(|t| t.session),
            Some(right),
            "and it moved no focus"
        );
        assert_eq!(dims(&app), before, "no pane and no window re-gridded");
    });
}

/// `scrollbar = false` draws and hit-tests nothing (the legacy pill keeps its
/// place); the Fluent split's hollow cursors still draw.
#[test]
fn the_scrollbar_off_draws_no_bar() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.scrollbar = Some(false);
    assert!(app.pane_overlay_inputs(wid, false).is_none());
    let split = with_split_chrome(SplitChrome::Fluent, || app.pane_overlay_inputs(wid, true));
    assert!(split.is_some_and(|i| !i.scrollbar && i.hollow));
    let classic = with_split_chrome(SplitChrome::Classic, || app.pane_overlay_inputs(wid, true));
    assert!(classic.is_none(), "Classic splits keep no hollow cursor");
}
