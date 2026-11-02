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
    glyphs: Vec<Option<(GlyphKey, Arc<GlyphCoverage>)>>,
}

/// The actual mono rasters resolved for one input, including styled and fallback glyphs.
/// Missing evidence never means full coverage. This view is valid only for its exact text,
/// shape boundaries and cell geometry; it contains no authority inferred from a cell height.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameGlyphInk {
    epoch: GlyphInkEpoch,
    cell: (usize, usize),
    cols: usize,
    wallpaper: bool,
    lcd: bool,
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
    /// composite glyphs return None so a light can yield that word conservatively.
    #[must_use]
    pub fn coverage(&self, row: usize, col: usize) -> Option<(GlyphKey, &GlyphCoverage)> {
        let (key, coverage) = self.rows.get(row)?.glyphs.get(col)?.as_ref()?;
        Some((*key, coverage))
    }

    fn matches(&self, input: &RenderInput, cell: (usize, usize)) -> bool {
        self.epoch.owner.load(Ordering::Relaxed)
            && self.cell == cell
            && self.cols == input.cols
            && self.wallpaper == input.wallpaper.is_some()
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
    /// avoids reviving an old carrier after a route or raster setting roundtrip.
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
        let lcd = self.input_uses_lcd_text(input);
        let old = self.glyph_frame_ink_memo.take();
        let previous = old
            .as_ref()
            .filter(|(key, _)| *key == epoch)
            .and_then(|(_, ink)| ink.frame.as_ref())
            .filter(|frame| frame.lcd == lcd);
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
                let unsupported = cell.wide
                    || cell.ch.is_control()
                    || cell.ch == ' '
                    || stamp.chrome
                    || stamp.image_mask[col]
                    || input.line_size_run_at(row, col).0 != LineSize::SingleWidth
                    || stamp.combining.iter().any(|(at, _)| *at == col)
                    || crate::procedural::covers(cell.ch)
                    || materialized_cell_span(&stamp.cells, col) != 1;
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
                if notdef {
                    glyphs.push(None);
                    continue;
                }
                // Linux's LCD route draws a separate three-channel raster. A mono
                // histogram cannot certify it. Keys which actually fall back to mono
                // (including fallback faces) retain their measured mono evidence.
                #[cfg(all(unix, not(target_os = "macos")))]
                if lcd && self.subpx_image(key).is_some() {
                    glyphs.push(None);
                    continue;
                }
                let (cw, ch, baseline) = (self.cell_w, self.cell_h, self.baseline);
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
                            if *width > 0 {
                                for (y, row) in bytes.chunks_exact(*width).enumerate() {
                                    let py = top.saturating_add(y as i32);
                                    if py < 0 || py >= ch as i32 {
                                        continue;
                                    }
                                    for (x, &alpha) in row.iter().enumerate() {
                                        let px = xmin.saturating_add(x as i32);
                                        if px >= 0 && px < cw as i32 {
                                            visible.push(alpha);
                                        }
                                    }
                                }
                            }
                            Some(Arc::new(GlyphCoverage::of(&visible)))
                        }
                        GlyphImage::Rgba { .. } => None,
                    })
                    .as_ref()
                    .map(Arc::clone);
                glyphs.push(coverage.map(|coverage| (key, coverage)));
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
            wallpaper: input.wallpaper.is_some(),
            lcd,
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
mod tests {
    use super::*;
    use crate::{FaceId, StyleBits, Theme};
    use aterm_core::terminal::Terminal;

    fn input(bytes: &[u8]) -> RenderInput {
        let mut term = Terminal::new(2, 8);
        term.process(bytes);
        term.cell_frame(2, 8)
    }

    fn actual_plan(renderer: &mut Renderer, input: &RenderInput) -> Vec<ColumnGlyph> {
        let mut plan = Vec::new();
        let mut breaks = Vec::new();
        ligature_break_cols_into(input, 0, renderer.text_shaping(), &mut breaks);
        renderer.row_glyph_plan(input, 0, &breaks, &mut plan);
        plan
    }

    /// Both plans come from the actual font/shaper, not an injected glyph-id oracle.
    fn plan_transition(renderer: &mut Renderer, before: &RenderInput, after: &RenderInput) {
        let prior_plan = actual_plan(renderer, before);
        let first = renderer.glyph_ink_for(before);
        assert!(Arc::ptr_eq(&first, &renderer.glyph_ink_for(before)));
        let next_plan = actual_plan(renderer, after);
        let shared = prior_plan.len().min(next_plan.len());
        assert_ne!(
            prior_plan[..shared],
            next_plan[..shared],
            "a real shaped prefix changes"
        );
        assert!(first.for_frame(after, renderer.cell_size()).is_none());
        let fresh = renderer.glyph_ink_for(after);
        let frame = fresh.for_frame(after, renderer.cell_size()).unwrap();
        assert!(!Arc::ptr_eq(
            &first.frame.as_ref().unwrap().rows[0],
            &frame.rows[0]
        ));
        let mut changed_observed = 0;
        for (col, planned) in next_plan.iter().copied().enumerate() {
            if let Some((key, coverage)) = frame.coverage(0, col) {
                let cell = &after.cells[0][col];
                let expected = match planned {
                    ColumnGlyph::Ligated(gid) => renderer.ligature_key(gid, cell_style(cell)),
                    ColumnGlyph::LigatedSlice { gid, k, .. } => {
                        renderer.ligature_slice_key(gid, k, cell_style(cell))
                    }
                    ColumnGlyph::PerCell => renderer.resolve_cell_key_for_span(
                        after.cluster_at(0, col),
                        cell,
                        materialized_cell_span(&after.cells[0], col),
                    ),
                };
                assert_eq!(
                    key, expected,
                    "fresh evidence follows the actual plan at {col}"
                );
                changed_observed +=
                    usize::from(coverage.max > 0 && prior_plan.get(col) != Some(&planned));
            }
        }
        assert!(
            changed_observed > 0,
            "a changed plan column observes real ink with its fresh key"
        );
        assert!(Arc::ptr_eq(&fresh, &renderer.glyph_ink_for(after)));
    }

    #[test]
    fn bidi_map_changes_reject_old_carriers_and_follow_the_actual_font_plan() {
        let mut renderer =
            Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
        renderer.debug_block_on_lazy_fallbacks();
        let mut visual = input(b"       ");
        visual.cursor_visible = false;
        for (cell, ch) in visual.cells[0]
            .iter_mut()
            .zip("\u{627}\u{628}\u{62d}\u{631}\u{645}==".chars())
        {
            cell.ch = ch;
            assert_ne!(
                renderer.font.lookup_glyph_index(ch),
                0,
                "fixture draws {ch}"
            );
        }
        visual.visual_to_logical.clear();
        let mut logical = visual.clone();
        logical.visual_to_logical = vec![vec![4, 3, 2, 1, 0, 5, 6, 7]];
        plan_transition(&mut renderer, &visual, &logical);
    }

    fn image(z_index: i32) -> aterm_core::grid::extra::ImageRef {
        use aterm_core::grid::extra::{ImageData, ImageFormat, ImageScaling};
        aterm_core::grid::extra::ImageRef {
            image: Arc::new(ImageData {
                bytes: Vec::new(),
                format: ImageFormat::Unknown,
                cols: 1,
                rows: 1,
                z_index,
                band_lift_px: 0,
                cell_offset_px: (0, 0),
                scaling: ImageScaling::Fit,
                source_rect: None,
            }),
            cell_row: 0,
            cell_col: 0,
            kitty: None,
        }
    }

    #[test]
    fn image_z_and_effective_cols_invalidate_the_actual_neighboring_ligature() {
        let mut renderer = Renderer::from_bytes(
            include_bytes!("../../tests/fixtures/jetbrains-mono.ttf"),
            24.0,
            Theme::default(),
        )
        .unwrap();
        renderer.debug_block_on_lazy_fallbacks();
        let mut behind = input(b"==");
        behind.cursor_visible = false;
        behind.images = vec![vec![(1, image(-1))]];
        let mut covering = behind.clone();
        covering.images[0][0].1 = image(0);
        plan_transition(&mut renderer, &behind, &covering);
        let fresh = renderer.glyph_ink_for(&covering);
        assert!(
            fresh
                .for_frame(&covering, renderer.cell_size())
                .unwrap()
                .coverage(0, 1)
                .is_none()
        );
        let mut narrow = behind.clone();
        narrow.cols = 1;
        assert_eq!(
            behind.cells, narrow.cells,
            "a narrower extraction retains materialized cells"
        );
        plan_transition(&mut renderer, &behind, &narrow);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn lcd_rasters_refuse_mono_authority_and_revoke_carriers_across_route_roundtrips() {
        let mut renderer =
            Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
        renderer.debug_block_on_lazy_fallbacks();
        let text = input(b"M");
        let mono = renderer.glyph_ink_for(&text);
        let (key, coverage) = mono
            .for_frame(&text, renderer.cell_size())
            .unwrap()
            .coverage(0, 0)
            .unwrap();
        assert!(coverage.max > 0);
        if crate::hinted::HintMode::portable_forced() {
            assert!(!renderer.set_font_subpixel("rgb"));
            assert_eq!(
                renderer.font_subpixel(),
                "off",
                "portable mode has no LCD route to certify"
            );
            aterm_spec::host::could_not_run(
                "LCD route control needs the native rasterizer, not forced portable coverage",
            );
        }
        assert!(renderer.set_font_subpixel("rgb"));
        assert!(renderer.input_uses_lcd_text(&text));
        assert!(
            renderer.subpx_image(key).is_some(),
            "the actual draw route has a separate LCD raster"
        );
        assert!(mono.for_frame(&text, renderer.cell_size()).is_none());
        let rgb = renderer.glyph_ink_for(&text);
        assert!(
            rgb.for_frame(&text, renderer.cell_size())
                .unwrap()
                .coverage(0, 0)
                .is_none()
        );
        assert!(renderer.set_font_subpixel("bgr"));
        assert!(rgb.for_frame(&text, renderer.cell_size()).is_none());
        let bgr = renderer.glyph_ink_for(&text);
        renderer.set_background_opacity(0.9);
        assert!(bgr.for_frame(&text, renderer.cell_size()).is_none());
        let translucent = renderer.glyph_ink_for(&text);
        assert!(
            translucent
                .for_frame(&text, renderer.cell_size())
                .unwrap()
                .coverage(0, 0)
                .unwrap()
                .1
                .max
                > 0
        );
        renderer.set_background_opacity(1.0);
        assert!(translucent.for_frame(&text, renderer.cell_size()).is_none());
        assert!(renderer.set_font_subpixel("off"));
        let restored = renderer.glyph_ink_for(&text);
        assert!(
            restored
                .for_frame(&text, renderer.cell_size())
                .unwrap()
                .coverage(0, 0)
                .unwrap()
                .1
                .max
                > 0
        );
        assert!(
            mono.for_frame(&text, renderer.cell_size()).is_none(),
            "a settings roundtrip cannot revive old evidence"
        );

        let mut fallback =
            Renderer::from_bytes(crate::embedded_symbols_font(), 24.0, Theme::default()).unwrap();
        fallback.set_fallback_bytes(crate::embedded_font()).unwrap();
        fallback.debug_block_on_lazy_fallbacks();
        assert!(fallback.set_font_subpixel("rgb"));
        let z = input(b"Z");
        let ink = fallback.glyph_ink_for(&z);
        let (key, coverage) = ink
            .for_frame(&z, fallback.cell_size())
            .unwrap()
            .coverage(0, 0)
            .unwrap();
        assert_eq!(key.source, FaceId::Fallback);
        assert!(fallback.subpx_image(key).is_none());
        assert!(
            coverage.max > 0,
            "the genuinely mono fallback remains supported on an LCD frame"
        );
    }

    #[test]
    fn observed_rows_reuse_rasters_and_reject_changed_style_size_and_blend() {
        let mut renderer =
            Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
        renderer.debug_block_on_lazy_fallbacks();
        let regular = input(b"W");
        let first = renderer.glyph_ink_for(&regular);
        let repeated = renderer.glyph_ink_for(&regular);
        assert!(
            Arc::ptr_eq(&first, &repeated),
            "an animation-only frame reuses its observation"
        );
        let (regular_key, _) = first
            .for_frame(&regular, renderer.cell_size())
            .unwrap()
            .coverage(0, 0)
            .unwrap();

        let italic = input(b"\x1b[3mW");
        assert!(first.for_frame(&italic, renderer.cell_size()).is_none());
        let styled = renderer.glyph_ink_for(&italic);
        let (italic_key, coverage) = styled
            .for_frame(&italic, renderer.cell_size())
            .unwrap()
            .coverage(0, 0)
            .unwrap();
        assert!(italic_key.style.contains(StyleBits::ITALIC));
        assert_ne!(
            regular_key, italic_key,
            "the actual italic raster replaces regular evidence"
        );
        assert!(coverage.max > 0);
        let (_, again) = renderer.glyph_ink_for(&italic).frame.as_ref().unwrap().rows[0].glyphs[0]
            .as_ref()
            .unwrap()
            .clone();
        let original = &styled.frame.as_ref().unwrap().rows[0].glyphs[0]
            .as_ref()
            .unwrap()
            .1;
        assert!(
            Arc::ptr_eq(original, &again),
            "coverage is shared from its raster entry"
        );

        let old_cell = renderer.cell_size();
        renderer.set_px(26.0);
        let resized = renderer.glyph_ink_for(&italic);
        assert!(styled.for_frame(&italic, renderer.cell_size()).is_none());
        assert_ne!(old_cell, renderer.cell_size());
        let before = resized.frame.as_ref().unwrap().epoch();
        renderer.set_text_blending(TextBlending::Linear);
        let blended = renderer.glyph_ink_for(&italic);
        assert_ne!(before, blended.frame.as_ref().unwrap().epoch());
        assert!(!blended.frame.as_ref().unwrap().corrected());
    }

    #[test]
    fn actual_fallback_coverage_and_renderer_ownership_are_observed() {
        let mut renderer =
            Renderer::from_bytes(crate::embedded_symbols_font(), 24.0, Theme::default()).unwrap();
        assert_eq!(
            renderer.font.lookup_glyph_index('Z'),
            0,
            "the primary fixture cannot draw Z"
        );
        renderer.set_fallback_bytes(crate::embedded_font()).unwrap();
        renderer.debug_block_on_lazy_fallbacks();
        let text = input(b"Z");
        let observed = renderer.glyph_ink_for(&text);
        let frame = observed.for_frame(&text, renderer.cell_size()).unwrap();
        let (key, coverage) = frame.coverage(0, 0).unwrap();
        assert_eq!(
            key.source,
            FaceId::Fallback,
            "evidence comes from the actual fallback raster"
        );
        assert!(coverage.max > 0);

        let mut other =
            Renderer::from_bytes(crate::embedded_symbols_font(), 24.0, Theme::default()).unwrap();
        other.set_fallback_bytes(crate::embedded_font()).unwrap();
        other.debug_block_on_lazy_fallbacks();
        let peer = other.glyph_ink_for(&text);
        assert_ne!(
            frame.epoch(),
            peer.frame.as_ref().unwrap().epoch(),
            "equal local counters in a different renderer do not identify the same custody"
        );
    }

    #[test]
    fn clipped_coverage_follows_line_height_baseline_and_activate_roundtrips() {
        let mut renderer =
            Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
        renderer.debug_block_on_lazy_fallbacks();
        let text = input(b"_g");
        let original_cell = renderer.cell_size();
        let original = renderer.glyph_ink_for(&text);
        let first_frame = original.for_frame(&text, original_cell).unwrap();
        let first_epoch = first_frame.epoch();
        let first = Arc::clone(&first_frame.rows[0].glyphs[0].as_ref().unwrap().1);

        renderer.set_line_height(1.5);
        let tall = renderer.glyph_ink_for(&text);
        let tall_frame = tall.for_frame(&text, renderer.cell_size()).unwrap();
        assert_ne!(first_epoch, tall_frame.epoch());
        assert!(original.for_frame(&text, renderer.cell_size()).is_none());
        assert!(!Arc::ptr_eq(
            &first,
            &tall_frame.rows[0].glyphs[0].as_ref().unwrap().1
        ));

        renderer.set_adjust_baseline(4);
        let shifted = renderer.glyph_ink_for(&text);
        let shifted_frame = shifted.for_frame(&text, renderer.cell_size()).unwrap();
        assert_ne!(tall_frame.epoch(), shifted_frame.epoch());
        assert!(!Arc::ptr_eq(
            &tall_frame.rows[0].glyphs[0].as_ref().unwrap().1,
            &shifted_frame.rows[0].glyphs[0].as_ref().unwrap().1
        ));

        renderer.set_adjust_baseline(0);
        renderer.set_line_height(1.0);
        renderer.activate_px(26.0);
        let other_size = renderer.glyph_ink_for(&text);
        assert!(other_size.for_frame(&text, renderer.cell_size()).is_some());
        renderer.activate_px(24.0);
        let restored = renderer.glyph_ink_for(&text);
        let restored_frame = restored.for_frame(&text, renderer.cell_size()).unwrap();
        assert_eq!(renderer.cell_size(), original_cell);
        assert_ne!(
            first_epoch,
            restored_frame.epoch(),
            "reset geometry does not revive a stale epoch"
        );
        let last = &restored_frame.rows[0].glyphs[0].as_ref().unwrap().1;
        assert!(!Arc::ptr_eq(&first, last));
        assert_eq!(
            *first, **last,
            "the same final raster geometry measures the same visible coverage"
        );
    }

    #[test]
    fn pending_font_publication_cannot_lend_coverage_until_the_face_is_stable() {
        let mut renderer =
            Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
        renderer.debug_block_on_lazy_fallbacks();
        renderer.debug_land_lazy_parses_on_poll(false);
        let text = input(b"W");
        let stable = renderer.glyph_ink_for(&text);
        assert!(
            stable
                .for_frame(&text, renderer.cell_size())
                .unwrap()
                .coverage(0, 0)
                .is_some()
        );
        let (publish, pending) = std::sync::mpsc::channel();
        renderer.fallback_parse_rx = Some(pending);
        let waiting = renderer.glyph_ink_for(&text);
        assert!(
            waiting.for_frame(&text, renderer.cell_size()).is_none(),
            "a queued face can land during snapshot settling, so pending evidence grants no glass"
        );
        publish.send(Vec::new()).unwrap();
        let settled = renderer.glyph_ink_for(&text);
        assert!(
            settled
                .for_frame(&text, renderer.cell_size())
                .unwrap()
                .coverage(0, 0)
                .is_some(),
            "a completed ordinary face remains observable instead of permanent suppression"
        );
    }

    #[test]
    fn an_unresolved_scalar_does_not_borrow_the_notdef_raster() {
        let mut renderer =
            Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
        renderer.debug_block_on_lazy_fallbacks();
        renderer.set_runtime_font_discovery(false);
        let mut text = input(b"WW");
        text.cells[0][1].ch = '\u{10ffff}';
        assert_eq!(renderer.font.lookup_glyph_index(text.cells[0][1].ch), 0);
        let ink = renderer.glyph_ink_for(&text);
        let frame = ink.for_frame(&text, renderer.cell_size()).unwrap();
        assert!(
            frame.coverage(0, 0).is_some(),
            "ordinary actual ink still has evidence"
        );
        assert!(
            frame.coverage(0, 1).is_none(),
            "a placeholder is not the unresolved glyph's evidence"
        );
    }

    #[test]
    fn a_legacy_only_mapping_does_not_certify_the_unicode_notdef_raster() {
        // Keep all real mapping bodies and outlines. Making the Mac Roman record the
        // Unicode one leaves Omega only in the enumerated legacy map, while W still draws.
        let mut bytes = crate::font::with_cmap_record_last(crate::embedded_font(), 1, 0)
            .expect("the embedded face has a Mac Roman record");
        let u16_at =
            |data: &[u8], at: usize| u16::from_be_bytes(data[at..at + 2].try_into().unwrap());
        let u32_at = |data: &[u8], at: usize| {
            usize::try_from(u32::from_be_bytes(data[at..at + 4].try_into().unwrap())).unwrap()
        };
        let record = (0..usize::from(u16_at(&bytes, 4)))
            .map(|i| 12 + 16 * i)
            .find(|&at| &bytes[at..at + 4] == b"cmap")
            .unwrap();
        let cmap = u32_at(&bytes, record + 8);
        let unicode: Vec<bool> = {
            let face = aterm_font::Face::parse(&bytes, 0).unwrap();
            face.tables()
                .cmap
                .unwrap()
                .subtables
                .into_iter()
                .map(|subtable| subtable.is_unicode())
                .collect()
        };
        let mut mac_records = 0;
        for (i, was_unicode) in unicode.into_iter().enumerate() {
            let at = cmap + 4 + 8 * i;
            let was_mac = u16_at(&bytes, at) == 1 && u16_at(&bytes, at + 2) == 0;
            if was_mac || was_unicode {
                let platform = if was_mac {
                    mac_records += 1;
                    0u16
                } else {
                    1u16
                };
                bytes[at..at + 2].copy_from_slice(&platform.to_be_bytes());
                bytes[at + 2..at + 4].copy_from_slice(&0u16.to_be_bytes());
            }
        }
        assert_eq!(mac_records, 1);
        let missing = '\u{03a9}';
        assert!(
            aterm_font::Face::parse(&bytes, 0)
                .unwrap()
                .glyph_index(missing)
                .is_none()
        );
        assert!(!crate::procedural::covers(missing) && !crate::procedural::covers_symbol(missing));
        for seq in [
            b"WW".as_slice(),
            b"\x1b[3mWW".as_slice(),
            b"\x1b[1mWW".as_slice(),
        ] {
            let mut renderer = Renderer::from_bytes(&bytes, 24.0, Theme::default()).unwrap();
            renderer.debug_force_fontdue();
            renderer.set_runtime_font_discovery(false);
            let mut shaping = renderer.text_shaping().clone();
            shaping.ligature_mode = aterm_types::text_shaping::LigatureMode::Disabled;
            renderer.set_text_shaping(shaping);
            assert_ne!(
                renderer.font.lookup_glyph_index(missing),
                0,
                "the legacy map names real ink"
            );
            assert_eq!(
                renderer.font.unicode_glyph_index(missing),
                0,
                "the raster's map has no glyph"
            );
            assert_eq!(
                renderer.font.rasterize(missing, 24.0),
                renderer.font.rasterize_indexed(0, 24.0)
            );
            let mut text = input(seq);
            text.cursor_visible = false;
            text.cells[0][1].ch = missing;
            assert!(
                actual_plan(&mut renderer, &text)
                    .iter()
                    .all(|glyph| *glyph == ColumnGlyph::PerCell)
            );
            let key = renderer.resolve_cell_key_for_span(None, &text.cells[0][1], 1);
            assert_eq!(
                (key.source, key.glyph_class),
                (FaceId::Primary, GlyphClass::Mono)
            );
            assert!(
                renderer
                    .glyph_image(key)
                    .bytes()
                    .iter()
                    .any(|&alpha| alpha > 0),
                "the actual tofu raster has ink"
            );
            let observed = renderer.glyph_ink_for(&text);
            let frame = observed.for_frame(&text, renderer.cell_size()).unwrap();
            assert!(
                frame
                    .coverage(0, 0)
                    .is_some_and(|(_, coverage)| coverage.max > 0),
                "ordinary Unicode ink stays admitted"
            );
            assert!(
                frame.coverage(0, 1).is_none(),
                "legacy coverage cannot certify a Unicode placeholder"
            );
        }
    }
}
