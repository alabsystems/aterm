// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rewound pane's chrome: a band and a track on its bottom rows, and a 2 px accent border.

use aterm_core::terminal::RenderCell;

use crate::app_render::ChromeSurface;
use crate::chrome_band::BandColors;
use crate::pane_chrome::CellBox;
use crate::rewind::{Faith, RewindView};
use crate::temporal::Span;
use crate::{App, WindowId};

/// How many of a rewound pane's `rows` rows the band and the track take: both from 4 rows, the band alone below that.
pub(crate) const fn band_rows(rows: usize) -> usize {
    if rows >= 4 { 2 } else { 1 }
}

/// The band's row for `view`, `cols` wide.
pub(crate) fn band_row(
    view: &RewindView,
    cols: usize,
    inks: &BandColors,
    text: &str,
) -> Vec<RenderCell> {
    let mut row = crate::settings::blank_row(cols, inks.label, inks.bar_bg, false);
    crate::settings::write_str(&mut row, cols, 1, text, inks.value, inks.bar_bg, false);
    crate::settings::write_str(&mut row, cols, 1, "REWIND", inks.accent, inks.bar_bg, true);
    let word = view.faith.band();
    if let Some(at) = text.find(word) {
        let col = 1 + text[..at].chars().count();
        let ink = match view.faith {
            Faith::Faithful => inks.value,
            Faith::MayDiffer | Faith::Unchecked => inks.warn,
        };
        crate::settings::write_str(&mut row, cols, col, word, ink, inks.bar_bg, true);
    }
    row
}

/// The track's glyph over a stretch a view cannot rebuild or whose replay a witness did not match.
pub(crate) const HATCH: char = '\u{2571}';

/// The track's glyph at the shown instant.
pub(crate) const SHOWN: char = '\u{2588}';

/// The track's glyph over a replayable stretch.
pub(crate) const KEPT: char = '\u{2500}';

/// The track row for `view`, `cols` wide: the ticks the spine names from left to right, [`HATCH`] over its hatched spans and [`SHOWN`] at the shown instant.
pub(crate) fn track_row(view: &RewindView, cols: usize, inks: &BandColors) -> Vec<RenderCell> {
    let mut row = crate::settings::blank_row(cols, inks.label, inks.bar_bg, false);
    let width = cols.saturating_sub(2);
    let track = &view.track;
    if width == 0 || track.to < track.from {
        return row;
    }
    let span = u128::from(track.to.0 - track.from.0) + 1;
    let tick = |k: usize| {
        track.from.0 + u64::try_from(span * k as u128 / width as u128).unwrap_or(u64::MAX)
    };
    let shown_col = (u128::from(view.at.0.saturating_sub(track.from.0)) * width as u128 / span)
        .min(width as u128 - 1) as usize;
    for k in 0..width {
        let (lo, hi) = (tick(k), tick(k + 1).max(tick(k) + 1));
        let hatch = track
            .spans
            .iter()
            .filter(|(a, b, _)| a.0 < hi && b.0 >= lo)
            .map(|(_, _, kind)| *kind)
            .max_by_key(|kind| matches!(kind, Span::Differs));
        let (glyph, ink) = match hatch {
            Some(Span::Differs) => (HATCH, inks.warn),
            Some(Span::Lost) => (HATCH, inks.label),
            None => (KEPT, inks.label),
        };
        let (glyph, ink, bold) = if k == shown_col {
            (SHOWN, inks.accent, true)
        } else {
            (glyph, ink, false)
        };
        crate::settings::write_str(
            &mut row,
            cols,
            1 + k,
            &glyph.to_string(),
            ink,
            inks.bar_bg,
            bold,
        );
    }
    row
}

/// The border around `pane` in grid pixels lifted by `lift`: four `t`-thick strokes.
pub(crate) fn border_rects(
    pane: CellBox,
    (cw, ch): (u32, u32),
    t: u32,
    lift: u32,
) -> Vec<(u32, u32, u32, u32)> {
    let px = |cells: usize, size: u32| {
        u32::try_from(cells)
            .unwrap_or(u32::MAX)
            .saturating_mul(size)
    };
    let (x, y) = (px(pane.col, cw), px(pane.row, ch).saturating_add(lift));
    let (w, h) = (px(pane.cols, cw), px(pane.rows, ch));
    if w < 2 * t || h < 2 * t {
        return Vec::new();
    }
    vec![
        (x, y, w, t),
        (x, y + h - t, w, t),
        (x, y + t, t, h - 2 * t),
        (x + w - t, y + t, t, h - 2 * t),
    ]
}

impl App {
    /// The rewound panes window `wid` shows, with their cells in the terminal grid.
    pub(crate) fn rewound_panes(&self, wid: WindowId) -> Vec<(u64, CellBox)> {
        if !self.pool.any_rewound() {
            return Vec::new();
        }
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return Vec::new();
        };
        plan.leaves
            .iter()
            .filter_map(|leaf| {
                let session = self
                    .view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)?;
                let shown = self.pool.rewind(session)?.shown.is_some();
                shown.then(|| (session, CellBox::of(leaf.rect)))
            })
            .collect()
    }

    /// Whether window row `row` (in terminal grid rows) of window `wid` lies under the band or the track of the rewound `session`'s pane: rows the last frame claimed as chrome, or, before any frame, that pane's bottom band rows.
    pub(crate) fn rewind_band_covers(&self, wid: WindowId, session: u64, row: u16) -> bool {
        let row = usize::from(row);
        self.chrome_owns_terminal_row(wid, row)
            || self.rewound_panes(wid).iter().any(|(s, pane)| {
                *s == session
                    && row >= pane.row
                    && row < pane.row + pane.rows
                    && row - pane.row >= pane.rows.saturating_sub(band_rows(pane.rows))
            })
    }

    /// The Rewind fingerprint of window `wid`'s frame: 0 while no pane it shows is rewound.
    pub(crate) fn rewind_frame_fp(&self, wid: WindowId) -> u64 {
        let effects = match self.rewind_effects_status(wid) {
            None => 0,
            Some(words) => aterm_hash::fnv1a_64(words.as_bytes()),
        };
        self.rewound_panes(wid).iter().fold(0, |fp, (session, _)| {
            let faith = self.rewind_faith(*session).map_or(0, Faith::code);
            fp.rotate_left(7) ^ self.pool.rewind_fp(*session) ^ faith.rotate_left(32) ^ effects
        })
    }

    /// Paint each rewound pane's band and border into window `wid`'s frame, taking back the last frame's border.
    pub(crate) fn splice_rewind_band(&mut self, wid: WindowId) {
        let panes = self.rewound_panes(wid);
        let painted = self
            .windows
            .get(&wid)
            .is_some_and(|ws| !ws.rewind_quads.is_empty());
        if panes.is_empty() && !painted {
            return;
        }
        let inks =
            crate::chrome_band::band_colors(self.chrome_palette(ChromeSurface::OverGrid(wid)));
        let (cw, ch) = self.win_cell_size(wid);
        let (cw32, ch32) = (
            u32::try_from(cw).unwrap_or(0),
            u32::try_from(ch).unwrap_or(0),
        );
        let lift = u32::try_from(usize::from(self.chrome_rows(wid)) * ch).unwrap_or(u32::MAX);
        let (pad, grid_top) = (
            self.win_pad(wid),
            self.win_pad_top(wid) + self.win_head(wid),
        );
        let now_ms = crate::harness_screen::unix_ms_now();
        let offset = if panes.is_empty() {
            0
        } else {
            crate::presence::local_offset_s().unwrap_or(0)
        };
        for (session, _) in &panes {
            self.refresh_rewind_faith(*session);
        }
        let mut rows = Vec::with_capacity(panes.len());
        for (session, pane) in &panes {
            let (Some(view), Some(s)) = (self.pool.rewind(*session), self.pool.get(*session))
            else {
                continue;
            };
            let effects = self.rewind_effects_status(wid);
            let text =
                crate::rewind::band_text(view, now_ms, offset, s.ctx.job_feed.bytes(), effects);
            let track = (band_rows(pane.rows) == 2).then(|| track_row(view, pane.cols, &inks));
            rows.push((*pane, band_row(view, pane.cols, &inks, &text), track));
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let t = (2.0 * ws.scale).round().max(2.0) as u32;
        let strip = ws
            .input_scratch
            .cells
            .len()
            .saturating_sub(usize::from(ws.rows));
        let whole_cols = usize::from(ws.cols);
        crate::pane_chrome::remove_each_once(&mut ws.input_scratch.nova_add, &ws.rewind_quads);
        ws.rewind_quads.clear();
        for (pane, built, track) in rows {
            if pane.rows == 0 || pane.cols == 0 {
                continue;
            }
            let bottom = strip + pane.row + pane.rows - 1;
            let top = bottom - usize::from(track.is_some());
            let lines = track
                .map(|track| (bottom - 1, track))
                .into_iter()
                .chain([(bottom, built)]);
            for (frame_row, built) in lines {
                if frame_row >= ws.input_scratch.cells.len() {
                    continue;
                }
                if pane.col == 0 && pane.cols == whole_cols {
                    ws.input_scratch.cells[frame_row] = built;
                    crate::app_render::clear_find_row_lists(&mut ws.input_scratch, frame_row);
                    crate::app_render::scrub_overlay_row_band(
                        &mut ws.input_scratch,
                        frame_row..frame_row + 1,
                        aterm_render::RowBands::new(grid_top, ch),
                    );
                } else {
                    crate::app_render::splice_find_row_span(
                        &mut ws.input_scratch,
                        frame_row,
                        pane.col,
                        built,
                    );
                    crate::app_render::scrub_overlay_card(
                        &mut ws.input_scratch,
                        crate::tab_menu::MenuRect {
                            row: frame_row,
                            col: pane.col,
                            w: pane.cols,
                            h: 1,
                        },
                        (cw, ch),
                        (pad, grid_top),
                    );
                }
            }
            crate::app_render::claim_chrome_rows(ws, top..bottom + 1);
            for rect in border_rects(pane, (cw32, ch32), t, lift) {
                crate::scrollbar::push_rect(&mut ws.rewind_quads, rect, inks.accent, 255, ch32);
            }
        }
        ws.input_scratch
            .nova_add
            .extend_from_slice(&ws.rewind_quads);
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
    }
}

impl App {
    /// The focused pane's open Rewind view for window `wid`'s accessibility tree, as a slider over its instants.
    #[cfg(a11y_tree)]
    pub(crate) fn rewind_a11y(&self, wid: WindowId) -> Option<crate::a11y_tree::GridRewind> {
        let session = self.rewound_front(wid)?;
        let view = self.pool.rewind(session).filter(|v| v.shown.is_some())?;
        let s = self.pool.get(session)?;
        let words = crate::rewind::band_text(
            view,
            crate::harness_screen::unix_ms_now(),
            crate::presence::local_offset_s().unwrap_or(0),
            s.ctx.job_feed.bytes(),
            self.rewind_effects_status(wid),
        );
        let pane = self
            .rewound_panes(wid)
            .into_iter()
            .find(|(s, _)| *s == session)
            .map(|(_, pane)| pane);
        let bounds = self.grid_a11y_geometry(wid).zip(pane).map(|(g, pane)| {
            let top = (pane.row + pane.rows).saturating_sub(band_rows(pane.rows)) as f64;
            aterm_a11y::model::Rect {
                x0: g.origin_x + pane.col as f64 * g.cell_w,
                y0: g.origin_y + top * g.cell_h,
                x1: g.origin_x + (pane.col + pane.cols) as f64 * g.cell_w,
                y1: g.origin_y + (pane.row + pane.rows) as f64 * g.cell_h,
            }
        });
        Some(crate::a11y_tree::GridRewind {
            state: view.state,
            words,
            bounds,
        })
    }
}
