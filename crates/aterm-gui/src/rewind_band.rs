// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rewound pane's chrome: a one-line band on its bottom row and a 2 px accent border.

use aterm_core::terminal::RenderCell;

use crate::app_render::ChromeSurface;
use crate::chrome_band::BandColors;
use crate::pane_chrome::CellBox;
use crate::rewind::{Faith, RewindView};
use crate::{App, WindowId};

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

    /// The Rewind fingerprint of window `wid`'s frame: 0 while no pane it shows is rewound.
    pub(crate) fn rewind_frame_fp(&self, wid: WindowId) -> u64 {
        self.rewound_panes(wid).iter().fold(0, |fp, (session, _)| {
            let faith = self.rewind_faith(*session).map_or(0, Faith::code);
            fp.rotate_left(7) ^ self.pool.rewind_fp(*session) ^ faith.rotate_left(32)
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
            let text = crate::rewind::band_text(view, now_ms, offset, s.ctx.job_feed.bytes());
            rows.push((*pane, band_row(view, pane.cols, &inks, &text)));
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
        for (pane, built) in rows {
            if pane.rows == 0 || pane.cols == 0 {
                continue;
            }
            let frame_row = strip + pane.row + pane.rows - 1;
            if frame_row >= ws.input_scratch.cells.len() {
                continue;
            }
            if pane.col == 0 && pane.cols == whole_cols {
                ws.input_scratch.cells[frame_row] = built;
                crate::app_render::clear_find_row_lists(&mut ws.input_scratch, frame_row);
            } else {
                crate::app_render::splice_find_row_span(
                    &mut ws.input_scratch,
                    frame_row,
                    pane.col,
                    built,
                );
            }
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
