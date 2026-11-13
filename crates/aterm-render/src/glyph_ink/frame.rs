// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual input glyph evidence. Rows reuse their resolved plan while unchanged;
//! coverage belongs to the raster cache entry, rather than a second growing cache.

use super::{GlyphCoverage, GlyphImage, GlyphInk, GlyphKey, Renderer, TextBlending};
use crate::{
    ColumnGlyph, FaceId, GlyphClass, cell_style, ligature_break_cols_into, materialized_cell_span,
};
use aterm_core::grid::LineSize;
use aterm_core::render::{LineSizeSpan, PaneSelection, RenderInput, SelectionClip};
use aterm_core::selection::TextSelection;
use aterm_core::terminal::RenderCell;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Renderer identity under which coverage and its remap are current.
#[derive(Clone, Debug)]
pub struct GlyphInkEpoch {
    owner: Arc<AtomicBool>,
    face: u64,
    raster: u64,
    px: u32,
    baseline: i32,
    blending: TextBlending,
}

impl PartialEq for GlyphInkEpoch {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
            && self.face == other.face
            && self.raster == other.raster
            && self.px == other.px
            && self.baseline == other.baseline
            && self.blending == other.blending
    }
}
impl Eq for GlyphInkEpoch {}
impl Hash for GlyphInkEpoch {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.owner).hash(state);
        self.face.hash(state);
        self.raster.hash(state);
        self.px.hash(state);
        self.baseline.hash(state);
        (self.blending == TextBlending::LinearCorrected).hash(state);
    }
}
pub(crate) type FrameInkMemo = (GlyphInkEpoch, Arc<GlyphInk>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct RowStamp {
    cols: usize,
    cells: Vec<RenderCell>,
    visual_to_logical: Vec<u16>,
    clusters: Vec<(usize, Box<str>)>,
    combining: Vec<(usize, Box<[char]>)>,
    line: LineSize,
    spans: Vec<LineSizeSpan>,
    image_mask: Vec<bool>,
    chrome: bool,
    breaks: Vec<usize>,
}

impl RowStamp {
    fn new(input: &RenderInput, row: usize, breaks: Vec<usize>) -> Self {
        Self {
            cols: input.cols,
            cells: input.cells.get(row).cloned().unwrap_or_default(),
            visual_to_logical: input.visual_to_logical_row(row).to_vec(),
            clusters: input.clusters.get(row).cloned().unwrap_or_default(),
            combining: input.combining.get(row).cloned().unwrap_or_default(),
            line: input
                .line_sizes
                .get(row)
                .copied()
                .unwrap_or(LineSize::SingleWidth),
            spans: input.line_size_spans.get(row).cloned().unwrap_or_default(),
            image_mask: (0..input.cols)
                .map(|col| {
                    crate::image_covers(input.images.get(row).map_or(&[], Vec::as_slice), col)
                })
                .collect(),
            chrome: input.chrome_raster(row).is_some(),
            breaks,
        }
    }

    fn matches(&self, input: &RenderInput, row: usize) -> bool {
        self.cols == input.cols
            && self.cells.as_slice() == input.cells.get(row).map_or(&[][..], Vec::as_slice)
            && self.visual_to_logical.as_slice() == input.visual_to_logical_row(row)
            && self.clusters.as_slice() == input.clusters.get(row).map_or(&[][..], Vec::as_slice)
            && self.combining.as_slice() == input.combining.get(row).map_or(&[][..], Vec::as_slice)
            && self.line
                == input
                    .line_sizes
                    .get(row)
                    .copied()
                    .unwrap_or(LineSize::SingleWidth)
            && self.spans.as_slice()
                == input
                    .line_size_spans
                    .get(row)
                    .map_or(&[][..], Vec::as_slice)
            && self.chrome == input.chrome_raster(row).is_some()
            && self
                .image_mask
                .iter()
                .copied()
                .eq((0..input.cols).map(|col| {
                    crate::image_covers(input.images.get(row).map_or(&[], Vec::as_slice), col)
                }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RowInk {
    stamp: RowStamp,
    glyphs: Vec<Option<(GlyphKey, Arc<GlyphCoverage>, InkRows)>>,
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static NARROW_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A test's negative control: `f` run with a wide glyph holding no evidence, as before
/// after-land F2 (each of its cells `None`). Debug and test builds only.
#[cfg(any(test, debug_assertions))]
pub fn with_narrow_evidence_only<R>(f: impl FnOnce() -> R) -> R {
    let was = NARROW_ONLY.with(|c| c.replace(true));
    let out = f();
    NARROW_ONLY.with(|c| c.set(was));
    out
}

/// The widest glyph, in cells, the frame's evidence reads.
#[inline]
fn wide_span() -> u8 {
    #[cfg(any(test, debug_assertions))]
    if NARROW_ONLY.with(std::cell::Cell::get) {
        return 1;
    }
    2
}

/// Where a raster lays ink in each pixel row of its cell ([`FrameGlyphInk::ink_rows`]): one
/// `[y, x0, x1]` a row that holds ink, from the cell's top left, `x1` end exclusive, top to
/// bottom. A row's span runs from its first inked pixel to its last, so a letter's counters and
/// the gaps between its strokes lie inside it: the letter's silhouette, row by row. A wide
/// glyph's cell is both its cells, from its lead's top left.
pub type InkRows = Arc<[[i16; 3]]>;

/// The actual mono rasters resolved for one input, including styled and fallback glyphs.
/// Missing evidence never means full coverage. This view is valid only for its exact text,
/// shape boundaries and cell geometry; it contains no authority inferred from a cell height.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameGlyphInk {
    epoch: GlyphInkEpoch,
    cell: (usize, usize),
    cols: usize,
    cursor: (usize, usize, bool),
    selection: TextSelection,
    selection_clip: Option<SelectionClip>,
    selections: Vec<PaneSelection>,
    rows: Vec<Arc<RowInk>>,
}

impl FrameGlyphInk {
    /// Exact raster/blending identity for a consumer's bounded memo.
    #[must_use]
    pub fn epoch(&self) -> GlyphInkEpoch {
        self.epoch.clone()
    }

    /// Whether the renderer remaps coverage in linear light for this frame.
    #[must_use]
    pub fn corrected(&self) -> bool {
        self.epoch.blending == TextBlending::LinearCorrected
    }

    /// The current cell's actual raster key and coverage. Unknown, colour and unsupported
    /// composite glyphs return None so a light can yield that word conservatively. A wide
    /// glyph's is its lead cell's, read over both its cells where its raster is fitted to them;
    /// its right half returns None.
    #[must_use]
    pub fn coverage(&self, row: usize, col: usize) -> Option<(GlyphKey, &GlyphCoverage)> {
        let (key, coverage, _) = self.rows.get(row)?.glyphs.get(col)?.as_ref()?;
        Some((*key, coverage))
    }

    /// Where the current cell's actual raster lays its ink, row by row in its cell ([`InkRows`]);
    /// `None` exactly where [`Self::coverage`] is.
    #[must_use]
    pub fn ink_rows(&self, row: usize, col: usize) -> Option<&InkRows> {
        let (_, _, rows) = self.rows.get(row)?.glyphs.get(col)?.as_ref()?;
        Some(rows)
    }

    fn matches(&self, input: &RenderInput, cell: (usize, usize)) -> bool {
        self.epoch.owner.load(Ordering::Relaxed)
            && self.cell == cell
            && self.cols == input.cols
            && self.rows.len() == input.rows
            && self.cursor == (input.cursor_row, input.cursor_col, input.cursor_visible)
            && self.selection == input.selection
            && self.selection_clip == input.selection_clip
            && self.selections == input.selections
            && self
                .rows
                .iter()
                .enumerate()
                .all(|(row, ink)| ink.stamp.matches(input, row))
    }
}

impl GlyphInk {
    /// Current input evidence, or None when the host did not supply it or the input changed
    /// after observation. A table of some other ASCII glyph is never substituted here.
    #[must_use]
    pub fn for_frame(&self, input: &RenderInput, cell: (usize, usize)) -> Option<&FrameGlyphInk> {
        self.frame
            .as_ref()
            .filter(|frame| frame.matches(input, cell))
    }
}

impl Renderer {
    /// Revoke retained carriers as well as this renderer's row memo. Replacing the token
    /// avoids reviving an old carrier after a raster, shaping or blending setting roundtrip.
    pub(crate) fn invalidate_glyph_frame_ink(&mut self) {
        self.glyph_ink_identity.store(false, Ordering::Relaxed);
        self.glyph_ink_identity = Arc::new(AtomicBool::new(true));
        self.glyph_frame_ink_memo = None;
    }

    /// The active face/raster/size/baseline/blending identity for a retained host frame.
    #[must_use]
    pub fn glyph_ink_epoch(&self) -> GlyphInkEpoch {
        GlyphInkEpoch {
            owner: Arc::clone(&self.glyph_ink_identity),
            face: self.font_epoch,
            raster: self.glyph_generation,
            px: self.px_q,
            baseline: self.baseline,
            blending: self.text_blending,
        }
    }

    /// The current input's actual glyphs beside the existing ASCII guard table. Unchanged rows
    /// share their previous evidence; each actual raster's histogram is cached with that raster.
    /// Face, raster, size, baseline, shaping and blending changes invalidate its evidence.
    /// Pending face publication and an unresolved primary placeholder confer no coverage;
    /// a later stable observation can protect ordinary and fallback ink normally.
    pub fn glyph_ink_for(&mut self, input: &RenderInput) -> Arc<GlyphInk> {
        // Snapshot rendering can land these jobs and draw again in the same call. An
        // observation made while a different face can still arrive is not stable ink.
        let _ = self.fallback_parse_pending();
        let base = self.glyph_ink();
        if self.fallback_parse_pending() {
            self.glyph_frame_ink_memo = None;
            return base;
        }
        let epoch = self.glyph_ink_epoch();
        let old = self.glyph_frame_ink_memo.take();
        let previous = old
            .as_ref()
            .filter(|(key, _)| *key == epoch)
            .and_then(|(_, ink)| ink.frame.as_ref());
        let mut rows = Vec::with_capacity(input.rows);
        let (mut breaks, mut plan) = (Vec::new(), Vec::new());
        for row in 0..input.rows {
            ligature_break_cols_into(input, row, &self.shaping, &mut breaks);
            if let Some(ink) = previous.and_then(|frame| frame.rows.get(row))
                && ink.stamp.matches(input, row)
                && ink.stamp.breaks == breaks
            {
                rows.push(Arc::clone(ink));
                continue;
            }
            self.row_glyph_plan(input, row, &breaks, &mut plan);
            let stamp = RowStamp::new(input, row, breaks.clone());
            let mut glyphs = Vec::with_capacity(stamp.cells.len().min(input.cols));
            for (col, cell) in stamp.cells.iter().take(input.cols).enumerate() {
                // A wide glyph's evidence is its lead's, over both its cells; its right half
                // holds none of its own.
                let span = materialized_cell_span(&stamp.cells, col);
                let unsupported = cell.wide
                    || cell.ch.is_control()
                    || cell.ch == ' '
                    || stamp.chrome
                    || stamp.image_mask[col]
                    || input.line_size_run_at(row, col).0 != LineSize::SingleWidth
                    || stamp.combining.iter().any(|(at, _)| *at == col)
                    || crate::procedural::covers(cell.ch)
                    || span > wide_span();
                if unsupported {
                    glyphs.push(None);
                    continue;
                }
                let key = match plan.get(col).copied().unwrap_or(ColumnGlyph::PerCell) {
                    ColumnGlyph::Ligated(gid) => self.ligature_key(gid, cell_style(cell)),
                    ColumnGlyph::LigatedSlice { gid, k, .. } => {
                        self.ligature_slice_key(gid, k, cell_style(cell))
                    }
                    ColumnGlyph::PerCell => self.resolve_cell_key_for_span(
                        input.cluster_at(row, col),
                        cell,
                        materialized_cell_span(&stamp.cells, col),
                    ),
                };
                let notdef = match key.source {
                    FaceId::Primary if key.glyph_class == GlyphClass::Mono => {
                        self.font.unicode_glyph_index(cell.ch) == 0
                    }
                    FaceId::BoldPrimary if key.glyph_class == GlyphClass::Mono => self
                        .bold_font
                        .as_ref()
                        .is_none_or(|font| font.unicode_glyph_index(cell.ch) == 0),
                    FaceId::Primary | FaceId::BoldPrimary
                        if key.glyph_class == GlyphClass::MonoGid =>
                    {
                        key.ch_or_id == 0
                    }
                    _ => false,
                };
                // A wide glyph is read only where its raster is fitted to its two cells (the
                // key's span), the box its evidence is read in.
                if notdef || (span == 2 && key.cell_span != 2) {
                    glyphs.push(None);
                    continue;
                }
                let (cw, ch, baseline) = (self.cell_w, self.cell_h, self.baseline);
                // The box the raster is read in: its cell, or both cells of a wide glyph's.
                let cw = if key.cell_span == 2 { 2 * cw } else { cw };
                let entry = self.cached_glyph(key);
                let coverage = entry
                    .coverage
                    .get_or_init(|| match &entry.img {
                        GlyphImage::Mono {
                            bytes,
                            width,
                            height,
                            xmin,
                            ymin,
                            ..
                        } => {
                            // The judges read a glyph inside its cell. A full pixel in an italic
                            // overhang or below the clipped cell cannot stand in for its visible ink.
                            let top = baseline
                                .saturating_sub(*height as i32)
                                .saturating_sub(*ymin);
                            let mut visible = Vec::new();
                            let mut spans: Vec<[i16; 3]> = Vec::new();
                            if *width > 0 {
                                for (y, row) in bytes.chunks_exact(*width).enumerate() {
                                    let py = top.saturating_add(y as i32);
                                    if py < 0 || py >= ch as i32 {
                                        continue;
                                    }
                                    let mut span = (i32::MAX, i32::MIN);
                                    for (x, &alpha) in row.iter().enumerate() {
                                        let px = xmin.saturating_add(x as i32);
                                        if px >= 0 && px < cw as i32 {
                                            visible.push(alpha);
                                            if alpha > 0 {
                                                span = (span.0.min(px), span.1.max(px + 1));
                                            }
                                        }
                                    }
                                    if span.0 < span.1
                                        && let (Ok(y), Ok(x0), Ok(x1)) = (
                                            i16::try_from(py),
                                            i16::try_from(span.0),
                                            i16::try_from(span.1),
                                        )
                                    {
                                        spans.push([y, x0, x1]);
                                    }
                                }
                            }
                            Some((Arc::new(GlyphCoverage::of(&visible)), InkRows::from(spans)))
                        }
                        GlyphImage::Rgba { .. } => None,
                    })
                    .as_ref()
                    .map(|(coverage, rows)| (Arc::clone(coverage), Arc::clone(rows)));
                glyphs.push(coverage.map(|(coverage, rows)| (key, coverage, rows)));
            }
            rows.push(Arc::new(RowInk { stamp, glyphs }));
        }
        if self.fallback_parse_pending() || epoch != self.glyph_ink_epoch() {
            // A lazy face became available during resolution. The renderer's later snapshot
            // will settle it; this mixed observation grants no glass coverage in this frame.
            return base;
        }
        let frame = FrameGlyphInk {
            epoch: epoch.clone(),
            cell: self.cell_size(),
            cols: input.cols,
            cursor: (input.cursor_row, input.cursor_col, input.cursor_visible),
            selection: input.selection.clone(),
            selection_clip: input.selection_clip,
            selections: input.selections.clone(),
            rows,
        };
        if let Some((old_epoch, ink)) = old
            && old_epoch == epoch
            && ink.frame.as_ref() == Some(&frame)
        {
            self.glyph_frame_ink_memo = Some((epoch, Arc::clone(&ink)));
            return ink;
        }
        let mut ink = (*base).clone();
        ink.frame = Some(frame);
        let ink = Arc::new(ink);
        self.glyph_frame_ink_memo = Some((epoch, Arc::clone(&ink)));
        ink
    }
}

#[cfg(all(test, feature = "embedded-font"))]
#[path = "frame_tests.rs"]
mod tests;
