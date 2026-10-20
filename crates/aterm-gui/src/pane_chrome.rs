// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The terminal area's own chrome on Windows: split dividers, the focused
//! pane's ring, a hollow cursor in every unfocused pane, and the overlay
//! scrollbar ([`crate::scrollbar`]).
//!
//! # Dividers ([`SplitChrome`])
//!
//! A split keeps its one-cell gaps (the layout, sizing and drag-to-resize do
//! not change). `Classic` (macOS, Linux) fills a gap with the seam tone and
//! inks a half-block beside the focused pane in the foreground. `Fluent`
//! (Windows) leaves the gap on the terminal background and draws one hairline
//! down its middle ([`stamp_seam_lines`]): a light box-drawing glyph per gap
//! cell, so the line is one device pixel at the default cell, joins itself at
//! every corner and tee, and text on either side keeps half a cell of air from
//! it. The hairline is always the seam tone; between two pane headers the gap
//! continues their fill, so a row of headers reads as one bar with a divider
//! in it. Cells, not overlay quads: the seam is part of every composite (the
//! glass, `image plain`, recordings) exactly as the slab was.
//!
//! # The focus ring ([`push_focus_ring`])
//!
//! The pane taking keystrokes is marked on ITS side of each divider it
//! borders: an accent line of the hairline's weight, one stroke clear of the
//! shared hairline, inside the gap (Windows Terminal's focused-pane border).
//! A gap cell has one glyph, and a gap between two panes borders both, so the
//! ring cannot be the hairline's own colour: inked that way (as it was until
//! the 2026-10-18 judges' `split-hairline-always-accent`) a two-pane split's
//! divider was accent along its whole length whichever side had focus. The
//! ring is pixel paint with the overlays below, so it moves with focus. A pane
//! with a header has the header's accent rule as the ring's top edge: the
//! side lines start on the rule and turn into it, an L at each corner
//! ([`focus_ring_rects`]). An inner pane's scrollbar thumb in the same gap
//! covers the hairline and grows away from the ring, at rest
//! ([`scrollbar::rest_span`]) and widened under the pointer with its track
//! ([`scrollbar::wide_span`]), so a stroke of ground keeps them apart at every
//! scale and the ring stays whole; it is one tone over the hairline and the
//! ground ([`scrollbar::over_ground`]).
//!
//! # Overlays ([`PaneOverlay`])
//!
//! The scrollbar, the focus ring and the unfocused panes' hollow cursors are
//! pixel paint, not cells: source-over [`GlowQuad`]s appended to the frame's `nova_add` stream
//! (grid-interior pixels, one quad per row band) after the strip splice
//! ([`App::splice_pane_overlay`]), taken back by the next splice the way
//! `crate::claude_card` takes back its light. Their facts are read under the
//! frame's own terminal holds ([`PaneFacts::read`]), the quads and their
//! fingerprint built before the repaint decision ([`PaneOverlay::finish`],
//! folded into the key's `pill_fp`), so an animating fade or a pointer that
//! only hovers still presents, and an idle window keys `0`.

use std::hash::{Hash, Hasher};
use std::time::Instant;

use aterm_core::terminal::{RenderCell, Terminal};
use aterm_render::{GlowQuad, RenderInput};

use crate::scrollbar::{self, Metrics};
use crate::tab_model::{LogicalRect, VisibleLeafPlan};
use crate::{App, WindowId};

/// How a split draws its dividers and marks the pane taking keystrokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplitChrome {
    /// A one-cell slab in the seam tone, a foreground half-block beside the
    /// focused pane (macOS and Linux, unchanged).
    Classic,
    /// A hairline centred in the gap on the terminal background, an accent
    /// line on the focused pane's own side of it ([`push_focus_ring`]), and a
    /// hollow cursor in every other pane (Windows).
    Fluent,
}

#[cfg(test)]
thread_local! {
    /// This test thread's split chrome in place of the platform's.
    static SPLIT_CHROME_FOR_TEST: std::cell::Cell<Option<SplitChrome>> =
        const { std::cell::Cell::new(None) };
}

/// This platform's split chrome: `Fluent` on Windows, `Classic` elsewhere.
#[must_use]
pub(crate) fn split_chrome() -> SplitChrome {
    #[cfg(test)]
    if let Some(chrome) = SPLIT_CHROME_FOR_TEST.get() {
        return chrome;
    }
    if cfg!(windows) {
        SplitChrome::Fluent
    } else {
        SplitChrome::Classic
    }
}

/// Run `f` with this test thread's split chrome pinned to `chrome`.
#[cfg(test)]
pub(crate) fn with_split_chrome<R>(chrome: SplitChrome, f: impl FnOnce() -> R) -> R {
    let was = SPLIT_CHROME_FOR_TEST.replace(Some(chrome));
    let out = f();
    SPLIT_CHROME_FOR_TEST.set(was);
    out
}

/// One rectangle of window cells, `rows × cols` from `(row, col)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CellBox {
    pub row: usize,
    pub col: usize,
    pub rows: usize,
    pub cols: usize,
}

impl CellBox {
    /// A plan rectangle in cells, rounded as the compositor places it.
    #[must_use]
    pub(crate) fn of(rect: LogicalRect) -> Self {
        CellBox {
            row: rect.origin.y.round().max(0.0) as usize,
            col: rect.origin.x.round().max(0.0) as usize,
            rows: rect.size.height.round().max(0.0) as usize,
            cols: rect.size.width.round().max(0.0) as usize,
        }
    }

    fn contains(&self, r: usize, c: usize) -> bool {
        r >= self.row && r < self.row + self.rows && c >= self.col && c < self.col + self.cols
    }
}

/// The colours of the Fluent seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SeamInks {
    /// The gap's ground: the terminal background.
    pub ground: [u8; 3],
    /// The ground between two pane headers: the header row's own fill
    /// (`crate::pane_header::header_colors`), so a row of headers reads as one
    /// bar across the divider whatever that fill is.
    pub band: [u8; 3],
    /// The hairline.
    pub line: [u8; 3],
    /// The focus ring beside the hairline on the side of the pane taking
    /// keystrokes ([`push_focus_ring`]).
    pub ring: [u8; 3],
}

impl SeamInks {
    /// The inks on the grid palette `grid`, with the header band of `header`:
    /// the seam token (`ChromeTokens::seam_on`, held to 1.5:1) and the accent at
    /// the 3:1 a focus indicator needs. Under an OS-forced palette (Windows High
    /// Contrast) the line is the window text and the ring the highlight, each
    /// held to 3:1 on the terminal's own ground.
    #[must_use]
    pub(crate) fn derive(
        grid: crate::chrome_band::ChromePalette,
        header: crate::chrome_band::ChromePalette,
    ) -> Self {
        let ground = crate::settings::u32_rgb(grid.theme.bg);
        // The headers' own fill, not the band's: a header row on the pane's
        // ground left the gap between two headers a grey notch in it.
        let band = crate::pane_header::header_colors(header).bar_bg;
        match crate::chrome_band::forced_chrome() {
            Some(hc) => SeamInks {
                ground,
                band,
                line: crate::chrome_band::ensure_contrast(hc.window_text, ground, 3.0),
                ring: crate::chrome_band::ensure_contrast(hc.highlight, ground, 3.0),
            },
            None => {
                let tokens = grid.tokens();
                SeamInks {
                    ground,
                    band,
                    line: tokens.seam_on(ground),
                    ring: tokens.accent_on(ground, 3.0),
                }
            }
        }
    }
}

/// The gap cell a Fluent composite is seeded with: blank on the terminal
/// background, inked (for when [`stamp_seam_lines`] gives it a glyph) in the
/// seam tone. `crate::app_render::divider_cell`'s Fluent arm.
#[must_use]
pub(crate) fn seam_ground_cell(p: crate::chrome_band::ChromePalette) -> RenderCell {
    let inks = SeamInks::derive(p, p);
    RenderCell {
        ch: ' ',
        fg: inks.line,
        bg: inks.ground,
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold: false,
        italic: false,
        underline: aterm_core::terminal::UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
        overline_color: None,
        script: aterm_core::terminal::Script::Normal,
    }
}

/// Where a composite's panes are: each leaf's whole allocation (its header
/// row included) and the header rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SeamLayout {
    pub allocations: Vec<CellBox>,
    pub headers: Vec<CellBox>,
}

impl SeamLayout {
    /// The layout of `plan`.
    #[must_use]
    pub(crate) fn of(plan: &VisibleLeafPlan) -> Self {
        SeamLayout {
            allocations: plan
                .leaves
                .iter()
                .map(|leaf| CellBox::of(leaf.outer_rect()))
                .collect(),
            headers: plan
                .leaves
                .iter()
                .filter_map(|leaf| leaf.header.map(CellBox::of))
                .collect(),
        }
    }
}

/// The light box-drawing glyph with arms `up`, `down`, `left`, `right`, or
/// `None` for none (a lone gap cell stays blank).
#[must_use]
pub(crate) fn seam_glyph(up: bool, down: bool, left: bool, right: bool) -> Option<char> {
    Some(match (up, down, left, right) {
        (false, false, false, false) => return None,
        (true, true, false, false) => '│',
        (false, false, true, true) => '─',
        (true, false, false, false) => '╵',
        (false, true, false, false) => '╷',
        (false, false, true, false) => '╴',
        (false, false, false, true) => '╶',
        (false, true, false, true) => '┌',
        (false, true, true, false) => '┐',
        (true, false, false, true) => '└',
        (true, false, true, false) => '┘',
        (true, true, false, true) => '├',
        (true, true, true, false) => '┤',
        (false, true, true, true) => '┬',
        (true, false, true, true) => '┴',
        (true, true, true, true) => '┼',
    })
}

/// Draw the Fluent seam into a freshly filled composite: every gap cell (a
/// cell in no allocation) of a row this frame writes gets the hairline glyph
/// its neighbouring gaps call for, in the seam ink, and the header band's
/// ground between two header rows. The focus ring is not a cell colour
/// ([`push_focus_ring`]).
///
/// Runs after the divider fill and before the pane blits and header paint,
/// which write only inside allocations, so nothing here is overwritten. A
/// run that meets the grid's edge carries on to it (the line under the strip
/// reaches the strip). `keep` is the fill's retention verdict: a retained row
/// already holds last frame's seam and the same layout.
pub(crate) fn stamp_seam_lines(
    dst: &mut RenderInput,
    rows: usize,
    cols: usize,
    inks: SeamInks,
    layout: &SeamLayout,
    keep: &[bool],
) {
    if layout.allocations.len() < 2 {
        return;
    }
    let gap = |r: usize, c: usize| {
        r < rows && c < cols && !layout.allocations.iter().any(|a| a.contains(r, c))
    };
    let header = |r: usize, c: usize| layout.headers.iter().any(|h| h.contains(r, c));
    for r in 0..rows.min(dst.cells.len()) {
        if matches!(keep.get(r), Some(true)) {
            continue;
        }
        for c in 0..cols {
            if !gap(r, c) {
                continue;
            }
            let n_up = r > 0 && gap(r - 1, c);
            let n_down = gap(r + 1, c);
            let n_left = c > 0 && gap(r, c - 1);
            let n_right = gap(r, c + 1);
            let vertical = n_up || n_down;
            let horizontal = n_left || n_right;
            let glyph = seam_glyph(
                if r == 0 { vertical } else { n_up },
                if r + 1 == rows { vertical } else { n_down },
                if c == 0 { horizontal } else { n_left },
                if c + 1 == cols { horizontal } else { n_right },
            );
            // Between two headers the gap carries their band across; beside one
            // (a header row facing another pane's content) it stays the ground,
            // or the band would jut past the hairline into that pane.
            let beside_header = c > 0 && header(r, c - 1) && header(r, c + 1);
            let Some(cell) = dst.cells[r].get_mut(c) else {
                continue;
            };
            if let Some(ch) = glyph {
                cell.ch = ch;
            }
            cell.fg = inks.line;
            cell.bg = if beside_header {
                inks.band
            } else {
                inks.ground
            };
        }
    }
}

impl App {
    /// The Fluent seam's inks and layout for `plan`; `None` under
    /// [`SplitChrome::Classic`], which keeps the slab.
    pub(crate) fn fluent_seam(&self, plan: &VisibleLeafPlan) -> Option<(SeamInks, SeamLayout)> {
        (split_chrome() == SplitChrome::Fluent && plan.leaves.len() > 1)
            .then(|| (self.seam_inks(), SeamLayout::of(plan)))
    }

    /// The Fluent seam's inks on this window's palettes.
    fn seam_inks(&self) -> SeamInks {
        SeamInks::derive(
            self.chrome_palette(crate::app_render::ChromeSurface::Grid),
            self.chrome_palette(crate::app_render::ChromeSurface::Header),
        )
    }
}

/// The pane a Fluent split's focus ring surrounds ([`push_focus_ring`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FocusRing {
    /// The focused pane's allocation, header row included, in terminal-grid
    /// cells.
    pub alloc: CellBox,
    /// The grid row of the pane's header, when it has one: the header's accent
    /// rule (`crate::pane_header::focus_rule_rows`) is then the ring's top edge.
    pub header_row: Option<usize>,
}

/// THE FOCUS RING of a Fluent split, as rectangles `(x, y, w, h)` in
/// terminal-grid pixels of a `grid_rows × grid_cols` grid of `cw × ch` cells
/// (the chrome rows above excluded): on each side of the focused allocation
/// that borders a divider gap, an accent line of the hairline's weight on the
/// pane's own side of that gap's hairline, one stroke clear of it
/// (`scrollbar::hairline_start`, the renderer's light-line placement). The
/// lines run the allocation's length and meet in an inner corner where a
/// vertical and a horizontal divider cross; a side on the window's edge has no
/// divider and no line.
///
/// A pane with a header (`ring.header_row`) has no top line: the header's
/// accent rule (`crate::pane_header::focus_rule_rows`, drawn across the pane's
/// columns by its raster) is the ring's top edge. The side lines start on the
/// rule's top row, and on the rule's rows a stub joins each to the rule's end
/// at the pane's edge, so line and rule meet in an L. They used to pass each
/// other: the side line ran up through the header to its top, and the rule
/// began three pixels to its right at 100% (the 2026-10-18 judge's
/// `focus-ring-header-corner-notch`). A divider above a header gets no second
/// accent line over the rule.
///
/// At the 9 px Windows cell the hairline is the gap's pixel 4, the ring pixel
/// 2 beside a pane on its left and pixel 6 beside one on its right: it moves
/// four pixels when focus crosses the divider, and keeps two pixels of air
/// from the text and one (pixel 3 or 5) from a resting scrollbar thumb, which
/// covers the hairline and grows away from it ([`scrollbar::rest_span`]).
#[must_use]
pub(crate) fn focus_ring_rects(
    ring: FocusRing,
    (grid_rows, grid_cols): (usize, usize),
    (cw, ch): (u32, u32),
) -> Vec<(u32, u32, u32, u32)> {
    let alloc = ring.alloc;
    if cw == 0 || ch == 0 || alloc.rows == 0 || alloc.cols == 0 {
        return Vec::new();
    }
    let t = scrollbar::hairline_stroke(cw, ch);
    let (vl, hl) = (
        scrollbar::hairline_start(cw, t),
        scrollbar::hairline_start(ch, t),
    );
    let px = |cells: usize, size: u32| {
        u32::try_from(cells)
            .unwrap_or(u32::MAX)
            .saturating_mul(size)
    };
    let (left, right) = (alloc.col > 0, alloc.col + alloc.cols < grid_cols);
    let (top, bottom) = (alloc.row > 0, alloc.row + alloc.rows < grid_rows);
    // The ring's box: each side on its line inside the neighbouring gap, or on
    // the grid's edge where there is none.
    let x0 = if left {
        px(alloc.col - 1, cw) + vl + 2 * t
    } else {
        px(alloc.col, cw)
    };
    let x1 = if right {
        px(alloc.col + alloc.cols, cw) + vl - t
    } else {
        px(alloc.col + alloc.cols, cw)
    };
    // A header's accent rule, `(top, thickness)`: the box's top edge.
    let rule = ring.header_row.map(|row| {
        let (at, thick) = crate::pane_header::focus_rule_rows(ch);
        (px(row, ch).saturating_add(at), thick)
    });
    let y0 = match rule {
        Some((at, _)) => at,
        None if top => px(alloc.row - 1, ch) + hl + 2 * t,
        None => px(alloc.row, ch),
    };
    let y1 = if bottom {
        px(alloc.row + alloc.rows, ch) + hl - t
    } else {
        px(alloc.row + alloc.rows, ch)
    };
    if x1 <= x0 + t || y1 <= y0 + t {
        return Vec::new();
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let mut rects = Vec::with_capacity(4);
    if left {
        rects.push((x0, y0, t, h));
    }
    if right {
        rects.push((x1 - t, y0, t, h));
    }
    match rule {
        // The rule runs across the pane's columns; each side line turns into
        // it across the gap's pixels between them.
        Some((at, thick)) => {
            let (pane_x0, pane_x1) = (px(alloc.col, cw), px(alloc.col + alloc.cols, cw));
            if left && pane_x0 > x0 + t {
                rects.push((x0 + t, at, pane_x0 - (x0 + t), thick));
            }
            if right && x1 - t > pane_x1 {
                rects.push((pane_x1, at, x1 - t - pane_x1, thick));
            }
        }
        None if top => rects.push((x0, y0, w, t)),
        None => {}
    }
    if bottom {
        rects.push((x0, y1 - t, w, t));
    }
    rects
}

/// Append the focus ring of `ring` ([`focus_ring_rects`]) as opaque `rgb`
/// quads, lifted `lift` pixels by the chrome rows above the grid and cut per
/// row band.
pub(crate) fn push_focus_ring(
    out: &mut Vec<GlowQuad>,
    ring: FocusRing,
    grid: (usize, usize),
    (cw, ch): (u32, u32),
    lift: u32,
    rgb: [u8; 3],
) {
    for (x, y, w, h) in focus_ring_rects(ring, grid, (cw, ch)) {
        scrollbar::push_rect(out, (x, y.saturating_add(lift), w, h), rgb, 255, ch);
    }
}

/// The side of `lane`'s divider hairline a focus-ring stroke among `ring`
/// ([`focus_ring_rects`]) runs on beside the lane's track, for the resting
/// thumb to grow away from ([`scrollbar::rest_span`]); `None` at the window's
/// edge and in a gap the ring does not reach.
fn ring_side(
    lane: scrollbar::Lane,
    ring: &[(u32, u32, u32, u32)],
    m: Metrics,
) -> Option<scrollbar::RingSide> {
    if lane.edge {
        return None;
    }
    let gap_x = lane.right.saturating_sub(lane.room);
    let line = gap_x + scrollbar::hairline_start(lane.room, m.stroke);
    let (top, bottom) = (lane.top, lane.top + lane.len);
    ring.iter()
        .find(|&&(x, y, w, h)| x >= gap_x && x + w <= lane.right && y < bottom && y + h > top)
        .map(|&(x, ..)| {
            if x < line {
                scrollbar::RingSide::Left
            } else {
                scrollbar::RingSide::Right
            }
        })
}

// ---- The overlays -------------------------------------------------------------------

/// One visible terminal pane as the overlays see it, read under the frame's
/// hold on that pane ([`PaneFacts::read`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneFacts {
    pub session: u64,
    /// The pane's content cells in the terminal grid (chrome rows excluded).
    pub rect: CellBox,
    /// The pane takes the keyboard (its cursor is the frame's own).
    pub focused: bool,
    /// Lines scrolled back, and lines of history on the main screen.
    pub offset: u32,
    pub history: u32,
    /// The alternate screen is up (it has no history to show).
    pub alt: bool,
    /// The cursor's cell in the pane's viewport, when it is shown and on it.
    pub cursor: Option<(usize, usize)>,
    pub cursor_rgb: [u8; 3],
    /// When the view last moved through history (not with output).
    pub moved: Option<Instant>,
}

impl PaneFacts {
    /// Read pane `session` at `rect` from its locked terminal.
    #[must_use]
    pub(crate) fn read(session: u64, rect: CellBox, focused: bool, term: &Terminal) -> Self {
        let grid = term.grid();
        let offset = grid.display_offset();
        let cursor = term.cursor();
        let row = usize::from(cursor.row).saturating_add(offset);
        PaneFacts {
            session,
            rect,
            focused,
            offset: u32::try_from(offset).unwrap_or(u32::MAX),
            history: u32::try_from(grid.scrollback_lines()).unwrap_or(u32::MAX),
            alt: term.is_alternate_screen(),
            cursor: (term.cursor_visible() && row < rect.rows)
                .then_some((row, usize::from(cursor.col))),
            cursor_rgb: crate::app_render::terminal_cursor_rgb(term),
            moved: None,
        }
    }
}

/// A thumb drag in flight: the pane, and how far below the thumb's top it was
/// grabbed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Drag {
    pub session: u64,
    pub grab: f64,
}

/// What a frame's overlays are drawn from: config, theme and geometry read
/// before the window borrow ([`App::pane_overlay_inputs`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OverlayInputs {
    /// `scrollbar = true`: draw the overlay scrollbar.
    pub scrollbar: bool,
    /// The Fluent split: draw unfocused panes' hollow cursors.
    pub hollow: bool,
    /// The Fluent chrome (Windows): a resting thumb is WinUI's thin panning
    /// indicator, one width and one ink at the window's edge and in a divider
    /// gap ([`scrollbar::rest_width`], [`scrollbar::rest_alpha`]), and in a gap
    /// it keeps clear of the ring ([`scrollbar::rest_span`]).
    pub fluent: bool,
    /// The Fluent split's focused pane while the window has the keyboard and
    /// `split_focus_mark` is on: the focus ring ([`push_focus_ring`])
    /// surrounds it. `None` draws no ring.
    pub ring: Option<FocusRing>,
    /// The ring's ink ([`SeamInks::ring`]).
    pub ring_rgb: [u8; 3],
    pub cell: (usize, usize),
    pub pad: usize,
    /// The terminal grid's rows and columns (the chrome rows excluded).
    pub win_rows: usize,
    pub win_cols: usize,
    /// Rows the strip splice puts above the terminal grid.
    pub chrome_rows: usize,
    /// The thumb's ink: the terminal foreground.
    pub ink: [u8; 3],
    /// The terminal's ground under a divider gap ([`SeamInks::ground`]): a
    /// Fluent thumb on the gap's hairline is precomposed over it, so it is one
    /// tone over the line and the ground ([`scrollbar::over_ground`]).
    pub ground: [u8; 3],
    /// The fade ramps (`MotionEffect::ScrollPill`); `false` hides binary.
    pub animated: bool,
}

/// A window's overlay state: this frame's facts, the quads they make, what
/// the last splice appended, and the pointer's part in it.
#[derive(Debug, Default)]
pub(crate) struct PaneOverlay {
    facts: Vec<PaneFacts>,
    /// Last frame's facts, so a view's move through history is seen.
    prev: Vec<PaneFacts>,
    /// The quads the next splice appends (frame pixels).
    pending: Vec<GlowQuad>,
    /// The quads the last splice appended, taken back by the next.
    painted: Vec<GlowQuad>,
    /// The pane whose bar is under the pointer.
    pub(crate) hover: Option<u64>,
    /// The thumb being dragged.
    pub(crate) drag: Option<Drag>,
    /// The scrollbar geometry the facts were last built with.
    inputs: Option<OverlayInputs>,
    /// Fingerprint of `pending` for the repaint key: `0` when nothing draws.
    pub(crate) fp: u64,
}

impl PaneOverlay {
    /// Start a frame's facts (last frame's are kept to compare against).
    pub(crate) fn begin(&mut self) {
        std::mem::swap(&mut self.facts, &mut self.prev);
        self.facts.clear();
    }

    /// Record one pane's facts for this frame.
    pub(crate) fn note(&mut self, mut facts: PaneFacts, now: Instant) {
        let before = self.prev.iter().find(|p| p.session == facts.session);
        facts.moved = match before {
            // A move through history: the offset changed while the history
            // did not (output under a parked view moves both together).
            Some(p) if p.offset != facts.offset && p.history == facts.history => Some(now),
            Some(p) => p.moved,
            None => None,
        };
        self.facts.push(facts);
    }

    /// Forget every fact and draw nothing (a route with no terminal panes).
    pub(crate) fn clear(&mut self) {
        self.facts.clear();
        self.prev.clear();
        self.pending.clear();
        self.hover = None;
        self.drag = None;
        self.inputs = None;
        self.fp = 0;
    }

    /// This frame's facts.
    #[cfg(test)]
    pub(crate) fn facts(&self) -> &[PaneFacts] {
        &self.facts
    }

    /// The quads this frame's overlays draw (the next splice appends them).
    #[cfg(test)]
    pub(crate) fn pending_quads(&self) -> &[GlowQuad] {
        &self.pending
    }

    /// Whether `quad` is one the last splice appended (a test telling the
    /// overlays from the effects that share their stream).
    #[cfg(test)]
    pub(crate) fn painted_quad(&self, quad: &GlowQuad) -> bool {
        self.painted.contains(quad)
    }

    /// Build this frame's quads from its facts and return their fingerprint.
    /// A pane whose view moved re-arms the window's fade clock (`pill`), so
    /// the hold and ramp wake the loop; `shown` says whether any bar is still
    /// fading, the erase-frame flag the wake handler reads.
    pub(crate) fn finish(
        &mut self,
        inputs: OverlayInputs,
        pill: &mut crate::scroll_motion::PillFade,
        shown: &mut bool,
        now: Instant,
    ) -> u64 {
        self.inputs = Some(inputs);
        self.pending.clear();
        let (cw, ch) = inputs.cell;
        let (cw32, ch32) = (
            u32::try_from(cw).unwrap_or(0),
            u32::try_from(ch).unwrap_or(0),
        );
        let m = Metrics::for_cell(cw32, ch32);
        let lift = u32::try_from(inputs.chrome_rows * ch).unwrap_or(u32::MAX);
        let mut fading = false;
        // Drop a drag or hover whose pane is gone.
        if self
            .drag
            .is_some_and(|d| !self.facts.iter().any(|f| f.session == d.session))
        {
            self.drag = None;
        }
        if self
            .hover
            .is_some_and(|h| !self.facts.iter().any(|f| f.session == h))
        {
            self.hover = None;
        }
        // The focus ring first; a thumb, resting or widened, keeps clear of
        // its strokes (`ring`).
        let grid = (inputs.win_rows, inputs.win_cols);
        let ring = inputs.ring.map_or_else(Vec::new, |ring| {
            push_focus_ring(
                &mut self.pending,
                ring,
                grid,
                (cw32, ch32),
                lift,
                inputs.ring_rgb,
            );
            focus_ring_rects(ring, grid, (cw32, ch32))
        });
        for i in 0..self.facts.len() {
            let f = self.facts[i];
            if f.moved == Some(now) {
                pill.touch(now);
            }
            if inputs.scrollbar
                && !f.alt
                && f.history > 0
                && let Some(lane) = scrollbar::lane(
                    (f.rect.row, f.rect.col, f.rect.rows, f.rect.cols),
                    inputs.win_cols,
                    inputs.cell,
                    inputs.pad,
                    m,
                )
            {
                let activity = f.moved.map_or(0, |t| {
                    let ms = u64::try_from(now.saturating_duration_since(t).as_millis())
                        .unwrap_or(u64::MAX);
                    crate::scroll_motion::pill_alpha(ms, inputs.animated)
                });
                fading |= activity > 0;
                let dragging = self.drag.is_some_and(|d| d.session == f.session);
                let hovering = self.hover == Some(f.session);
                let base = if dragging {
                    scrollbar::ALPHA_DRAG
                } else if hovering {
                    scrollbar::ALPHA_HOVER
                } else if f.offset > 0 {
                    scrollbar::ALPHA_REST
                } else {
                    0
                };
                let active = (u32::from(scrollbar::ALPHA_ACTIVE) * u32::from(activity) / 255) as u8;
                let alpha = base.max(active);
                let rows = u32::try_from(f.rect.rows).unwrap_or(0);
                if alpha > 0
                    && let Some((top, len)) = scrollbar::thumb(lane, rows, f.history, f.offset, m)
                {
                    let widened = dragging || hovering;
                    let side = ring_side(lane, &ring, m);
                    // A Fluent thumb on a gap's hairline is one tone over the
                    // line and the ground: precomposed over what it would
                    // show over the ground ([`scrollbar::over_ground`]).
                    let on_line = inputs.fluent && !lane.edge;
                    let (x, w, (rgb, alpha)) = if widened {
                        // Under the pointer: the wide thumb, centred on a
                        // faint track drawn under it, both kept off the ring
                        // in a gap beside it ([`scrollbar::wide_span`]).
                        let (tx, tw) = scrollbar::wide_span(lane, m, side);
                        scrollbar::push_bar(
                            &mut self.pending,
                            (tx, lane.top + lift, tw, lane.len),
                            inputs.ink,
                            scrollbar::ALPHA_TRACK,
                            ch32,
                        );
                        let w = scrollbar::span(lane, m.wide, m).1.min(tw);
                        let paint = if on_line {
                            let (track, _) = scrollbar::over_ground(
                                inputs.ink,
                                inputs.ground,
                                scrollbar::ALPHA_TRACK,
                                1,
                            );
                            scrollbar::over_ground(inputs.ink, track, alpha, 1)
                        } else {
                            (inputs.ink, alpha)
                        };
                        (tx + tw.saturating_sub(w) / 2, w, paint)
                    } else {
                        // At rest: one width and one ink in every lane under
                        // the Fluent chrome, grown away from the ring in a gap.
                        let (x, w) = scrollbar::rest_span(
                            lane,
                            scrollbar::rest_width(m, inputs.fluent),
                            m,
                            side,
                        );
                        let alpha = scrollbar::rest_alpha(alpha, inputs.fluent);
                        let paint = if on_line {
                            scrollbar::over_ground(
                                inputs.ink,
                                inputs.ground,
                                alpha,
                                scrollbar::rest_alpha(scrollbar::ALPHA_REST, true),
                            )
                        } else {
                            (inputs.ink, alpha)
                        };
                        (x, w, paint)
                    };
                    scrollbar::push_bar(
                        &mut self.pending,
                        (x, top + lift, w, len),
                        rgb,
                        alpha,
                        ch32,
                    );
                }
            }
            if inputs.hollow
                && !f.focused
                && let Some((r, c)) = f.cursor
                && c < f.rect.cols
            {
                let x = u32::try_from((f.rect.col + c) * cw).unwrap_or(u32::MAX);
                let y = u32::try_from((f.rect.row + r) * ch).unwrap_or(u32::MAX);
                scrollbar::push_outline(
                    &mut self.pending,
                    (x, y + lift, cw32, ch32),
                    (cw32 / 9).max(1),
                    f.cursor_rgb,
                    HOLLOW_ALPHA,
                    ch32,
                );
            }
        }
        if inputs.scrollbar {
            *shown = fading;
        }
        self.fp = if self.pending.is_empty() {
            0
        } else {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for q in &self.pending {
                for word in q.damage_words() {
                    word.hash(&mut hasher);
                }
            }
            hasher.finish() | 1
        };
        self.fp
    }
}

/// The unfocused panes' hollow cursor: the pane's own cursor colour, a touch
/// under opaque so it reads as a mark, not a block.
const HOLLOW_ALPHA: u8 = 210;

/// Remove each quad of `taken` from `stream` once, wherever it still is (a
/// producer that refilled the stream since holds none of it).
fn remove_each_once(stream: &mut Vec<GlowQuad>, taken: &[GlowQuad]) {
    for q in taken {
        if let Some(at) = stream.iter().rposition(|s| s == q) {
            stream.remove(at);
        }
    }
}

impl App {
    /// What this frame's overlays of window `wid` draw from, or `None` when
    /// nothing can draw: the scrollbar off and no Fluent split.
    pub(crate) fn pane_overlay_inputs(
        &self,
        wid: WindowId,
        multi_pane: bool,
    ) -> Option<OverlayInputs> {
        let scrollbar = self.config.scrollbar_or_default();
        let hollow = multi_pane && split_chrome() == SplitChrome::Fluent;
        if !scrollbar && !hollow {
            return None;
        }
        let ws = self.windows.get(&wid)?;
        let focus = self.motion_focus(wid, ws.focused);
        // The ring marks a pane only in the window with the keyboard, as the
        // Classic split's half-block does (`ActivePaneMark::resolve`).
        let ring = (hollow && ws.focused && self.config.split_focus_mark_or_default())
            .then(|| self.active_visible_leaf_plan(wid))
            .flatten()
            .filter(|plan| plan.leaves.len() > 1)
            .and_then(|plan| {
                plan.leaves
                    .iter()
                    .find(|leaf| leaf.focused)
                    .map(|leaf| FocusRing {
                        alloc: CellBox::of(leaf.outer_rect()),
                        header_row: leaf.header.map(|header| CellBox::of(header).row),
                    })
            });
        Some(OverlayInputs {
            scrollbar,
            hollow,
            fluent: split_chrome() == SplitChrome::Fluent,
            ring,
            ring_rgb: ring.map_or([0; 3], |_| self.seam_inks().ring),
            cell: self.win_cell_size(wid),
            pad: self.win_pad(wid),
            win_rows: usize::from(ws.rows),
            win_cols: usize::from(ws.cols),
            chrome_rows: usize::from(self.chrome_rows(wid)),
            ink: crate::settings::u32_rgb(
                self.chrome_palette(crate::app_render::ChromeSurface::Grid)
                    .theme
                    .fg,
            ),
            ground: crate::settings::u32_rgb(
                self.chrome_palette(crate::app_render::ChromeSurface::Grid)
                    .theme
                    .bg,
            ),
            animated: self
                .effect_policy(crate::motion::MotionEffect::ScrollPill, focus)
                .animate(crate::motion::MotionEffect::ScrollPill),
        })
    }

    /// Take back the overlays the last splice appended to window `wid`'s frame
    /// and append this frame's, after the strip splice (their rows count the
    /// chrome rows) and before the bands and cards that paint over them.
    pub(crate) fn splice_pane_overlay(&mut self, wid: WindowId) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let overlay = &mut ws.pane_overlay;
        if overlay.painted.is_empty() && overlay.pending.is_empty() {
            return;
        }
        remove_each_once(&mut ws.input_scratch.nova_add, &overlay.painted);
        ws.input_scratch
            .nova_add
            .extend_from_slice(&overlay.pending);
        overlay.painted.clone_from(&overlay.pending);
    }

    /// Re-read every visible terminal pane of `plan` and rebuild the overlays,
    /// for a capture that builds its frame without the redraw's holds
    /// (`image` on a headless window). Try-locks: a contended pane keeps last
    /// frame's facts.
    pub(crate) fn refresh_pane_overlay_for_capture(
        &mut self,
        wid: WindowId,
        plan: &VisibleLeafPlan,
        now: Instant,
    ) {
        let multi = plan.leaves.len() > 1;
        let Some(inputs) = self.pane_overlay_inputs(wid, multi) else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.pane_overlay.clear();
            }
            return;
        };
        let mut read = Vec::with_capacity(plan.leaves.len());
        for leaf in &plan.leaves {
            let Some(session) = self
                .view_store
                .get(leaf.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                continue;
            };
            let rect = CellBox::of(leaf.rect);
            let facts = self.pool.get(session).and_then(|s| {
                crate::term_try_lock(&s.term)
                    .map(|term| PaneFacts::read(session, rect, leaf.focused, &term))
            });
            read.push((session, facts));
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let overlay = &mut ws.pane_overlay;
        overlay.begin();
        for (session, facts) in read {
            let facts =
                facts.or_else(|| overlay.prev.iter().copied().find(|p| p.session == session));
            if let Some(facts) = facts {
                overlay.note(facts, now);
            }
        }
        overlay.finish(inputs, &mut ws.scroll_pill, &mut ws.pill_shown, now);
    }

    // ---- The pointer ----------------------------------------------------------------

    /// Window pixel `(x, y)` in terminal-grid pixels (the grid's own origin,
    /// the chrome rows above it excluded), unclamped.
    fn grid_pixel(&self, wid: WindowId, x: f64, y: f64) -> (f64, f64) {
        let (fx, fy) = self.window_to_frame(wid, x, y);
        let ch = self.win_cell_size(wid).1;
        let top =
            self.win_pad_top(wid) + self.win_head(wid) + usize::from(self.chrome_rows(wid)) * ch;
        (fx - self.win_pad(wid) as f64, fy - top as f64)
    }

    /// The scrollbar under window pixel `(x, y)`: its pane's facts, lane and
    /// thumb, and what part of it is hit.
    fn scrollbar_at(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(PaneFacts, scrollbar::Lane, (u32, u32), scrollbar::Hit)> {
        let ws = self.windows.get(&wid)?;
        let inputs = ws.pane_overlay.inputs.filter(|i| i.scrollbar)?;
        let (gx, gy) = self.grid_pixel(wid, x, y);
        let (cw, ch) = inputs.cell;
        let m = Metrics::for_cell(u32::try_from(cw).ok()?, u32::try_from(ch).ok()?);
        ws.pane_overlay.facts.iter().find_map(|f| {
            if f.alt || f.history == 0 {
                return None;
            }
            let lane = scrollbar::lane(
                (f.rect.row, f.rect.col, f.rect.rows, f.rect.cols),
                inputs.win_cols,
                inputs.cell,
                inputs.pad,
                m,
            )?;
            let rows = u32::try_from(f.rect.rows).ok()?;
            let thumb = scrollbar::thumb(lane, rows, f.history, f.offset, m)?;
            let hit = scrollbar::hit(lane, thumb, gx, gy, m)?;
            Some((*f, lane, thumb, hit))
        })
    }

    /// Request a redraw of window `wid`.
    fn redraw_overlay(&self, wid: WindowId) {
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
    }

    /// Pointer motion over the overlay scrollbars of window `wid` at window
    /// pixel `(x, y)`: a thumb drag in flight scrolls its pane; otherwise the
    /// bar under the pointer is the hovered one (a change repaints). Returns
    /// whether the motion is the scrollbar's (no grid hover, no report).
    pub(crate) fn scrollbar_pointer_motion(&mut self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        if let Some(drag) = ws.pane_overlay.drag {
            self.drag_scrollbar(wid, drag, x, y);
            return true;
        }
        let hover = self.scrollbar_at(wid, x, y).map(|(f, ..)| f.session);
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.pane_overlay.hover != hover
        {
            ws.pane_overlay.hover = hover;
            self.redraw_overlay(wid);
        }
        hover.is_some()
    }

    /// The pointer left window `wid`: no bar is hovered.
    pub(crate) fn scrollbar_pointer_left(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.pane_overlay.drag.is_none()
            && ws.pane_overlay.hover.take().is_some()
        {
            self.redraw_overlay(wid);
        }
    }

    /// A left press at the pointer: on a thumb it starts a drag; on the track
    /// beside the window edge it pages toward the pointer. Returns whether the
    /// press was the scrollbar's.
    pub(crate) fn scrollbar_press(&mut self, wid: WindowId) -> bool {
        let Some((x, y)) = self.windows.get(&wid).map(|ws| ws.last_cursor_px) else {
            return false;
        };
        let Some((facts, _, _, hit)) = self.scrollbar_at(wid, x, y) else {
            return false;
        };
        match hit {
            scrollbar::Hit::Thumb { grab } => {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.pane_overlay.drag = Some(Drag {
                        session: facts.session,
                        grab,
                    });
                    ws.pane_overlay.hover = Some(facts.session);
                }
            }
            scrollbar::Hit::Track { above } => {
                let page = facts.rect.rows.saturating_sub(1).max(1);
                self.scroll_pane(wid, facts.session, |offset| {
                    if above {
                        offset.saturating_add(page)
                    } else {
                        offset.saturating_sub(page)
                    }
                });
            }
        }
        self.redraw_overlay(wid);
        true
    }

    /// A left release: ends a thumb drag. Returns whether one was in flight.
    pub(crate) fn scrollbar_release(&mut self, wid: WindowId) -> bool {
        let ended = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.pane_overlay.drag.take())
            .is_some();
        if ended {
            // The pointer may have left the bar mid-drag: re-resolve the hover.
            if let Some((x, y)) = self.windows.get(&wid).map(|ws| ws.last_cursor_px) {
                let hover = self.scrollbar_at(wid, x, y).map(|(f, ..)| f.session);
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.pane_overlay.hover = hover;
                }
            }
            self.redraw_overlay(wid);
        }
        ended
    }

    /// One drag step: put the dragged thumb's top under the pointer, less the
    /// grab, and scroll its pane to the offset that puts it there.
    fn drag_scrollbar(&mut self, wid: WindowId, drag: Drag, x: f64, y: f64) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let Some(inputs) = ws.pane_overlay.inputs else {
            return;
        };
        let Some(facts) = ws
            .pane_overlay
            .facts
            .iter()
            .copied()
            .find(|f| f.session == drag.session)
        else {
            return;
        };
        let (cw, ch) = inputs.cell;
        let (Ok(cw), Ok(ch)) = (u32::try_from(cw), u32::try_from(ch)) else {
            return;
        };
        let m = Metrics::for_cell(cw, ch);
        let Some(lane) = scrollbar::lane(
            (
                facts.rect.row,
                facts.rect.col,
                facts.rect.rows,
                facts.rect.cols,
            ),
            inputs.win_cols,
            inputs.cell,
            inputs.pad,
            m,
        ) else {
            return;
        };
        let (_, gy) = self.grid_pixel(wid, x, y);
        let rows = u32::try_from(facts.rect.rows).unwrap_or(0);
        let target = scrollbar::offset_for_top(lane, rows, facts.history, gy - drag.grab, m);
        self.scroll_pane(wid, drag.session, |_| target as usize);
    }

    /// Move pane `session`'s view to the offset `to` makes of its current one,
    /// as a reader's own motion (`Terminal::scroll_display`, custody recorded),
    /// and wake the fade. The facts take the new offset at once, so the next
    /// pointer event in the same frame hit-tests the thumb where it now is.
    fn scroll_pane(&mut self, wid: WindowId, session: u64, to: impl FnOnce(usize) -> usize) {
        let Some(term) = self.pool.get(session).map(|s| s.term.clone()) else {
            return;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // A glide in flight on this pane would ease back over the new place.
        if ws
            .scroll_glide
            .as_ref()
            .is_some_and(|glide| std::sync::Arc::ptr_eq(&glide.term, &term))
        {
            ws.scroll_glide = None;
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
        }
        let offset = {
            let mut term = crate::term_lock(&term);
            let (cur, history) = (term.grid().display_offset(), term.grid().scrollback_lines());
            let target = to(cur).min(history);
            if target != cur {
                let delta = i64::try_from(target).unwrap_or(i64::MAX)
                    - i64::try_from(cur).unwrap_or(i64::MAX);
                term.scroll_display(i32::try_from(delta).unwrap_or(0));
            }
            term.grid().display_offset()
        };
        if let Some(f) = ws
            .pane_overlay
            .facts
            .iter_mut()
            .find(|f| f.session == session)
        {
            f.offset = u32::try_from(offset).unwrap_or(u32::MAX);
        }
        ws.scroll_pill.touch(Instant::now());
        self.redraw_overlay(wid);
    }
}

#[cfg(test)]
#[path = "pane_chrome_tests.rs"]
mod tests;
