// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Terminal split dividers, focus ring, hollow unfocused cursors, and overlay scrollbars.

use std::hash::{Hash, Hasher};
use std::time::Instant;

use aterm_core::render::{FxBlend, FxContent, FxLayer, FxSlot, FxSpace};
use aterm_core::terminal::{RenderCell, Terminal};
use aterm_render::rounded_bar::{CapArts, RoundedBar};
use aterm_render::{GlowQuad, RenderInput};

use crate::scrollbar::{self, Metrics};
use crate::tab_model::{LogicalRect, VisibleLeafPlan};
use crate::{App, WindowId};

/// How a split draws its dividers and marks the pane taking keystrokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplitChrome {
    /// One-cell seam slab with a foreground half-block beside the focused pane.
    Classic,
    /// Centred hairline in the gap, accent focus ring, and hollow unfocused cursors.
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
    pub(crate) const fn of(rect: LogicalRect) -> Self {
        CellBox {
            row: rect.origin.y.round().max(0.0) as usize,
            col: rect.origin.x.round().max(0.0) as usize,
            rows: rect.size.height.round().max(0.0) as usize,
            cols: rect.size.width.round().max(0.0) as usize,
        }
    }

    const fn contains(&self, r: usize, c: usize) -> bool {
        r >= self.row && r < self.row + self.rows && c >= self.col && c < self.col + self.cols
    }
}

/// The colours of the Fluent seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SeamInks {
    /// The gap's ground: the terminal background.
    pub ground: [u8; 3],
    /// The ground between two pane headers.
    pub band: [u8; 3],
    /// The hairline.
    pub line: [u8; 3],
    /// The focus ring beside the hairline on the focused pane's side.
    pub ring: [u8; 3],
}

impl SeamInks {
    /// Derives seam inks from the grid and header palettes.
    #[must_use]
    pub(crate) fn derive(
        grid: crate::chrome_band::ChromePalette,
        header: crate::chrome_band::ChromePalette,
    ) -> Self {
        let ground = crate::settings::u32_rgb(grid.theme.bg);
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

/// Blank Fluent gap cell on the terminal background, inked in the seam tone.
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

/// Pane allocations and header rows of a composite.
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

/// Returns the light box-drawing glyph for the given arm directions, if any.
#[must_use]
pub(crate) const fn seam_glyph(up: bool, down: bool, left: bool, right: bool) -> Option<char> {
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

/// Stamps Fluent seam hairlines and header-band gaps into a composite.
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
    /// Returns the Fluent seam inks and layout for `plan`, or `None` under `Classic`.
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
    /// The focused pane's allocation, header row included, in terminal-grid cells.
    pub alloc: CellBox,
    /// Grid row of the pane's header, when it has one.
    pub header_row: Option<usize>,
}

/// Computes the focus-ring rectangles `(x, y, w, h)` in terminal-grid pixels.
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

/// Appends focus-ring quads for `ring` lifted by `lift` pixels and cut per row band.
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

/// Per-pane facts read under the frame's hold on that pane.
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

/// Active scrollbar thumb drag state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Drag {
    pub session: u64,
    pub grab: f64,
}

/// Config, theme, and geometry inputs for building a frame's pane overlays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OverlayInputs {
    /// `scrollbar = true`: draw the overlay scrollbar.
    pub scrollbar: bool,
    /// The Fluent split: draw unfocused panes' hollow cursors.
    pub hollow: bool,
    /// Whether Fluent scrollbar styling is active.
    pub fluent: bool,
    /// Focused pane for the Fluent focus ring, if active.
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
    /// Terminal ground colour under a divider gap.
    pub ground: [u8; 3],
    /// The fade ramps (`MotionEffect::ScrollPill`); `false` hides binary.
    pub animated: bool,
    /// Grid geometry the bars were laid on.
    pub laid: Option<crate::app_mouse::GlassGrid>,
}

/// Per-window pane overlay state across frames and pointer interactions.
#[derive(Debug, Default)]
pub(crate) struct PaneOverlay {
    facts: Vec<PaneFacts>,
    /// Last frame's facts, so a view's move through history is seen.
    prev: Vec<PaneFacts>,
    /// The quads the next splice appends (frame pixels).
    pending: Vec<GlowQuad>,
    /// The quads the last splice appended, taken back by the next.
    painted: Vec<GlowQuad>,
    /// The scrollbars the next splice draws, each under its fx layer id.
    bars: Vec<(u64, RoundedBar)>,
    /// One fx layer a bar of `bars`, in their order: a bar that kept its look keeps its layer, so a settled bar is no damage.
    bar_layers: Vec<(u64, RoundedBar, FxLayer)>,
    /// The last build's `bar_layers`, which the next build takes its kept
    /// layers from (kept across builds for its capacity).
    bar_spare: Vec<(u64, RoundedBar, FxLayer)>,
    /// The fx layer ids the last splice inserted, taken back by the next.
    painted_bars: Vec<u64>,
    /// The bars' cap arts.
    caps: CapArts,
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
        self.bars.clear();
        self.bar_layers.clear();
        self.bar_spare.clear();
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

    /// What this frame's overlays draw, as the quads the retired path drew:
    /// the ring and hollow cursors as they are, and each bar as
    /// [`scrollbar::legacy_bar_quads`] (its fx layer lands those quads'
    /// bytes: `the_scrollbar_layers_are_the_retired_quads_byte_for_byte_on_the_cpu`).
    #[cfg(test)]
    pub(crate) fn pending_quads(&self) -> Vec<GlowQuad> {
        let cell_h = self
            .inputs
            .map_or(1, |i| u32::try_from(i.cell.1).unwrap_or(u32::MAX));
        let mut quads = self.pending.clone();
        for (_, b) in &self.bars {
            scrollbar::legacy_bar_quads(&mut quads, (b.x, b.y, b.w, b.h), b.rgb, b.alpha, cell_h);
        }
        quads
    }

    /// The fx layers this frame's bars draw (the next splice inserts them).
    #[cfg(test)]
    pub(crate) fn bar_layers(&self) -> impl Iterator<Item = &FxLayer> {
        self.bar_layers.iter().map(|(_, _, l)| l)
    }

    /// Returns whether `quad` was appended by the last overlay splice.
    #[cfg(test)]
    pub(crate) fn painted_quad(&self, quad: &GlowQuad) -> bool {
        self.painted.contains(quad)
    }

    /// Builds this frame's overlay quads from its facts and returns their fingerprint.
    pub(crate) fn finish(
        &mut self,
        inputs: OverlayInputs,
        pill: &mut crate::scroll_motion::PillFade,
        shown: &mut bool,
        now: Instant,
    ) -> u64 {
        self.inputs = Some(inputs);
        self.pending.clear();
        self.bars.clear();
        let (cw, ch) = inputs.cell;
        let (cw32, ch32) = (
            u32::try_from(cw).unwrap_or(0),
            u32::try_from(ch).unwrap_or(0),
        );
        let m = Metrics::for_cell(cw32, ch32);
        let lift = u32::try_from(inputs.chrome_rows * ch).unwrap_or(u32::MAX);
        let mut fading = false;
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
                    let on_line = inputs.fluent && !lane.edge;
                    let (x, w, (rgb, alpha)) = if widened {
                        let (tx, tw) = scrollbar::wide_span(lane, m, side);
                        scrollbar::push_bar(
                            &mut self.bars,
                            bar_layer_id(f.session, false),
                            (tx, lane.top + lift, tw, lane.len),
                            inputs.ink,
                            scrollbar::ALPHA_TRACK,
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
                        &mut self.bars,
                        bar_layer_id(f.session, true),
                        (x, top + lift, w, len),
                        rgb,
                        alpha,
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
        self.lay_bars();
        self.fp = if self.pending.is_empty() && self.bars.is_empty() {
            0
        } else {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for q in &self.pending {
                for word in q.damage_words() {
                    word.hash(&mut hasher);
                }
            }
            self.bars.hash(&mut hasher);
            hasher.finish() | 1
        };
        self.fp
    }

    /// One fx layer a bar of `bars`: the layer of a bar that kept its id and
    /// look is the one it had, the rest are built afresh.
    fn lay_bars(&mut self) {
        std::mem::swap(&mut self.bar_layers, &mut self.bar_spare);
        let kept = &mut self.bar_spare;
        for &(id, bar) in &self.bars {
            let layer = match kept.iter().position(|(i, b, _)| *i == id && *b == bar) {
                Some(at) => kept.swap_remove(at).2,
                None => {
                    let (mut items, mut palette) = (Vec::new(), Vec::new());
                    aterm_render::rounded_bar::push_items(
                        bar,
                        &mut self.caps,
                        &mut items,
                        &mut palette,
                    );
                    FxLayer::new(
                        id,
                        std::sync::Arc::new(FxContent::new(items, palette)),
                        FxSlot::Over,
                        FxSpace::Grid,
                        FxBlend::Exact,
                    )
                }
            };
            self.bar_layers.push((id, bar, layer));
        }
        kept.clear();
    }
}

/// The fx layer id of pane `session`'s scrollbar track (`thumb = false`) or thumb.
fn bar_layer_id(session: u64, thumb: bool) -> u64 {
    const SALT: u64 = 0x7363_726F_6C6C_6261;
    (SALT ^ session.rotate_left(17)).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(thumb)
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
    /// Returns the overlay inputs for window `wid`, or `None` when nothing can draw.
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
            laid: self.glass_grid(wid),
        })
    }

    /// Replaces the previous frame's pane overlay quads on window `wid` with this frame's.
    pub(crate) fn splice_pane_overlay(&mut self, wid: WindowId) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let overlay = &mut ws.pane_overlay;
        if overlay.painted.is_empty()
            && overlay.pending.is_empty()
            && overlay.painted_bars.is_empty()
            && overlay.bar_layers.is_empty()
        {
            return;
        }
        remove_each_once(&mut ws.input_scratch.nova_add, &overlay.painted);
        ws.input_scratch
            .nova_add
            .extend_from_slice(&overlay.pending);
        overlay.painted.clone_from(&overlay.pending);
        // The bars draw first among the Over-slot Exact layers, as their quads
        // drew in the nova pass before that slot: under every other producer's
        // Over layer and the card's, which splices after.
        let fx = &mut ws.input_scratch.fx_layers;
        fx.retain(|l| !overlay.painted_bars.contains(&l.id));
        let at = fx
            .iter()
            .position(|l| l.slot == FxSlot::Over && l.blend == FxBlend::Exact)
            .unwrap_or(fx.len());
        fx.splice(at..at, overlay.bar_layers.iter().map(|(_, _, l)| l.clone()));
        overlay.painted_bars.clear();
        overlay
            .painted_bars
            .extend(overlay.bar_layers.iter().map(|(id, ..)| *id));
    }

    /// Re-reads visible panes of `plan` and rebuilds overlays for headless capture.
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

    /// Window pixel `(x, y)` in terminal-grid pixels (the grid's own origin,
    /// the chrome rows above it excluded), unclamped.
    fn grid_pixel(&self, wid: WindowId, x: f64, y: f64) -> (f64, f64) {
        let (fx, fy) = self.window_to_frame(wid, x, y);
        let ch = self.win_cell_size(wid).1;
        let top =
            self.win_pad_top(wid) + self.win_head(wid) + usize::from(self.chrome_rows(wid)) * ch;
        (fx - self.win_pad(wid) as f64, fy - top as f64)
    }

    /// The geometry the last frame laid window `wid`'s bars with, while the
    /// window still has the grid they were laid on ([`App::laid_grid`]):
    /// between a re-grid and the frame that lays them again, `None`, so no
    /// pointer event hovers, grabs, pages or drags a lane laid at a cell,
    /// inset or width the window no longer has.
    fn laid_overlay_inputs(&self, wid: WindowId) -> Option<OverlayInputs> {
        let inputs = self.windows.get(&wid)?.pane_overlay.inputs?;
        self.laid_grid(wid, inputs.laid)?;
        Some(inputs)
    }

    /// The scrollbar under window pixel `(x, y)`: its pane's facts, lane and
    /// thumb, and what part of it is hit.
    fn scrollbar_at(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(PaneFacts, scrollbar::Lane, (u32, u32), scrollbar::Hit)> {
        let inputs = self.laid_overlay_inputs(wid).filter(|i| i.scrollbar)?;
        let ws = self.windows.get(&wid)?;
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

    /// Handles pointer motion over overlay scrollbars in window `wid`.
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

    /// Handles a left mouse press on an overlay scrollbar in window `wid`.
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
        let Some(inputs) = self.laid_overlay_inputs(wid) else {
            return;
        };
        let Some(ws) = self.windows.get(&wid) else {
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
