// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Programming-ligature run shaping for the terminal grid.

use std::borrow::Borrow;

use aterm_core::terminal::RenderCell;
use aterm_font::shape::{
    Direction, Feature, GlyphBuffer, GlyphInfo, ShapeFace, ShapePlan, UnicodeBuffer,
};
use aterm_grapheme::Script;
use aterm_types::text_shaping::FontFeature;

use crate::StyleBits;

/// Build the feature list for every shaping run.
#[must_use]
pub fn build_feature_list(user: &[FontFeature], ligatures_on: bool) -> Vec<FontFeature> {
    let base = u32::from(ligatures_on);
    let mut features = Vec::with_capacity(2 + user.len());
    features.push(FontFeature::new(*b"liga", base));
    features.push(FontFeature::new(*b"calt", base));
    features.extend_from_slice(user);
    features
}

/// The shaper's form of a [`build_feature_list`] result: each feature over the whole run, in order (the last writer wins).
pub(crate) fn engine_features(features: &[FontFeature]) -> Vec<Feature> {
    features
        .iter()
        .map(|f| Feature::new(aterm_font::Tag::from_bytes(&f.tag), f.value))
        .collect()
}

trust_contract_fn! {
    /// Whether [`crate::Renderer::row_glyph_plan`] should run the shaping pass for a row, vs short-circuit to an all-[`ColumnGlyph::PerCell`] plan.
    #[must_use]
    pub fn should_run_shaping(
        rb_present: bool,
        has_user_features: bool,
        ligatures_globally_off: bool,
        font_has_ligature_features: bool,
    ) -> bool
    clauses [ensures result
        == (rb_present && (has_user_features || (!ligatures_globally_off && font_has_ligature_features)))
        by should_run_shaping_is_gate]
    {
        rb_present && (has_user_features || (!ligatures_globally_off && font_has_ligature_features))
    }
}

verify_island! {
    clean {
        theorem should_run_shaping_is_gate (rb u off lig : Bool) :
            trust_import_aterm_render__ligature_shaping__should_run_shaping rb u off lig
              = (rb && (u || (!off && lig))) := by
          simp only [trust_import_aterm_render__ligature_shaping__should_run_shaping]

        theorem should_run_shaping_no_features_is_legacy (rb off lig : Bool) :
            trust_import_aterm_render__ligature_shaping__should_run_shaping rb false off lig
              = (rb && (!off && lig)) := by
          simp only [trust_import_aterm_render__ligature_shaping__should_run_shaping]
    }
}

/// The minimum run length the planner will attempt to shape.
#[inline]
#[must_use]
pub const fn shaping_min_run(has_user_features: bool) -> usize {
    if has_user_features { 1 } else { 2 }
}

/// Whether a shaping result (`n_out` glyphs over `n_in` cells) maps onto the monospace grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeVerdict {
    /// Not grid-mappable: use the per-cell path.
    Reject,
    /// One glyph per input cell, drawn as one [`ColumnGlyph::Ligated`] per column.
    OneToOne,
    /// `n_in` cells collapsed to one wide glyph, rasterized once and sliced into `n_in` cell tiles.
    Collapsed,
}

/// Classify a shaping result of `n_out` output glyphs over `n_in` input cells into the grid-mappable [`ShapeVerdict`].
#[inline]
#[must_use]
pub const fn classify_shape(n_in: usize, n_out: usize, admit_collapsed: bool) -> ShapeVerdict {
    if n_in >= 1 && n_out == n_in {
        ShapeVerdict::OneToOne
    } else if admit_collapsed && n_out == 1 && n_in >= 2 {
        ShapeVerdict::Collapsed
    } else {
        ShapeVerdict::Reject
    }
}

/// What to draw at one column of a row, resolved by [`plan_row_runs`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnGlyph {
    /// Use the ordinary per-cell dispatch for this column (no ligature touched it).
    PerCell,
    /// Draw this shaped primary-face glyph id at the column's monospace origin.
    Ligated(u16),
    /// Cell `k` of an `n`-cell collapsed (Cascadia N:1) ligature.
    LigatedSlice { gid: u16, k: u16, n: u16 },
}

/// The result of shaping one run, returned by the [`plan_row_runs`] `shape` closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShapedRun {
    /// 1:1 spacer convention (Fira, JetBrains): `Some(gid)` where shaping changed the column's glyph, `None` to keep it per-cell.
    PerColumn(Box<[Option<u16>]>),
    /// N:1 collapse (Cascadia): one wide `gid` over `n` cells, expanded to `n` [`ColumnGlyph::LigatedSlice`] columns.
    Collapsed { gid: u16, n: u16 },
}

/// The most cells an N:1 collapse is admitted for (real programming ligatures span 2–4); a longer run stays per-cell.
pub const LIG_SLICE_MAX: usize = 64;

/// Extract signed cell `k`'s slice of a wide ligature raster as a cell-local tile (`cell_w × height`, left bearing 0).
#[must_use]
pub fn extract_cell_slice(
    raster: &[u8],
    raster_w: usize,
    height: usize,
    xmin: i32,
    cell_w: usize,
    k: i64,
) -> Vec<u8> {
    if cell_w == 0 || height == 0 || raster.len() < raster_w.saturating_mul(height) {
        return Vec::new();
    }
    let mut tile = vec![0u8; cell_w * height];
    let cell_w_i64 = i64::try_from(cell_w).unwrap_or(i64::MAX);
    let base = k.saturating_mul(cell_w_i64).saturating_sub(i64::from(xmin));
    let src_start = usize::try_from(base.max(0)).unwrap_or(usize::MAX);
    let dst_start = if base < 0 {
        usize::try_from(base.unsigned_abs()).unwrap_or(usize::MAX)
    } else {
        0
    };
    let len = raster_w
        .saturating_sub(src_start)
        .min(cell_w.saturating_sub(dst_start));
    if len == 0 {
        return tile;
    }
    for row in 0..height {
        let src = row * raster_w + src_start;
        let dst = row * cell_w + dst_start;
        tile[dst..dst + len].copy_from_slice(&raster[src..src + len]);
    }
    tile
}

/// Whether `cell` may join a ligature shaping run.
#[inline]
#[must_use]
pub fn cell_is_shapeable(cell: &RenderCell, has_cluster: bool, image_covered: bool) -> bool {
    !cell.wide
        && cell.ch != ' '
        && !cell.ch.is_control()
        && !cell.emoji_presentation
        && !cell.text_presentation
        && !has_cluster
        && !image_covered
        && matches!(cell.script, aterm_core::terminal::Script::Normal)
}

/// Bounded segment-property to [`ShapePlan`] memo (at most 4 entries per face slot).
#[derive(Default)]
pub(crate) struct ShapePlanMemo {
    entries: Vec<(SegmentProperties, ShapePlan)>,
}

impl ShapePlanMemo {
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    fn plan_for<'a>(
        &'a mut self,
        face: &ShapeFace<'_>,
        features: &[Feature],
        props: SegmentProperties,
    ) -> &'a ShapePlan {
        let slot = self
            .entries
            .iter()
            .position(|(p, _)| *p == props)
            .unwrap_or_else(|| {
                #[cfg(test)]
                note_shape_plan_build();
                let plan = ShapePlan::new(face, props.direction, props.script, features);
                if self.entries.len() == 4 {
                    self.entries[3] = (props, plan);
                    3
                } else {
                    self.entries.push((props, plan));
                    self.entries.len() - 1
                }
            });
        &self.entries[slot].1
    }
}

/// Reusable scratch buffers for [`map_shaped_run`], avoiding heap allocations on unligated runs.
#[derive(Default)]
pub(crate) struct MapScratch {
    byte_to_idx: Vec<usize>,
    out: Vec<Option<u16>>,
}

/// One row's fixed font instance and feature list.
pub(crate) struct RowShaper<'a> {
    face: ShapeFace<'a>,
    features: &'a [Feature],
    #[cfg(test)]
    plans: ShapePlanMemo,
    #[cfg(test)]
    map_scratch: MapScratch,
}

#[derive(Clone, Copy, PartialEq)]
struct SegmentProperties {
    direction: Direction,
    script: Option<Script>,
}

impl<'a> RowShaper<'a> {
    pub(crate) fn new(face: ShapeFace<'a>, features: &'a [Feature]) -> Self {
        Self {
            face,
            features,
            #[cfg(test)]
            plans: ShapePlanMemo::default(),
            #[cfg(test)]
            map_scratch: MapScratch::default(),
        }
    }

    /// Shape one run and return its grid plan, or `None` unless the shape is grid-mappable ([`classify_shape`]) and changed a glyph vs the plain cmap.
    #[cfg(test)]
    pub(crate) fn shape(
        &mut self,
        run: &str,
        run_chars: &[char],
        admit_collapsed: bool,
        scratch: &mut UnicodeBuffer,
    ) -> Option<ShapedRun> {
        let min_run = if self.features.len() > 2 { 1 } else { 2 };
        if run_chars.len() < min_run {
            return None;
        }
        let shaped = Self::shape_substitute_only_buffer(
            &self.face,
            self.features,
            &mut self.plans,
            run,
            scratch,
        );
        let result = map_shaped_run(
            self.face.face(),
            run,
            run_chars,
            admit_collapsed,
            shaped.glyph_infos(),
            &mut self.map_scratch,
        );
        *scratch = shaped.clear();
        result
    }

    /// Shape one run against external persistent `plans` and `map_scratch`.
    pub(crate) fn shape_cached(
        &self,
        run: &str,
        run_chars: &[char],
        admit_collapsed: bool,
        scratch: &mut UnicodeBuffer,
        plans: &mut ShapePlanMemo,
        map_scratch: &mut MapScratch,
    ) -> Option<ShapedRun> {
        let min_run = if self.features.len() > 2 { 1 } else { 2 };
        if run_chars.len() < min_run {
            return None;
        }
        let shaped =
            Self::shape_substitute_only_buffer(&self.face, self.features, plans, run, scratch);
        let result = map_shaped_run(
            self.face.face(),
            run,
            run_chars,
            admit_collapsed,
            shaped.glyph_infos(),
            map_scratch,
        );
        *scratch = shaped.clear();
        result
    }

    #[cfg(test)]
    fn shape_buffer(&mut self, run: &str, scratch: &mut UnicodeBuffer) -> GlyphBuffer {
        let mut buffer = std::mem::take(scratch);
        buffer.push_str(run);
        buffer.guess_segment_properties();
        let props = SegmentProperties {
            direction: buffer.direction(),
            script: buffer.script(),
        };
        let plan = self.plans.plan_for(&self.face, self.features, props);
        aterm_font::shape::shape_with_plan(&self.face, plan, buffer)
    }

    fn shape_substitute_only_buffer(
        face: &ShapeFace<'_>,
        features: &[Feature],
        plans: &mut ShapePlanMemo,
        run: &str,
        scratch: &mut UnicodeBuffer,
    ) -> GlyphBuffer {
        let mut buffer = std::mem::take(scratch);
        buffer.push_str(run);
        buffer.guess_segment_properties();
        let props = SegmentProperties {
            direction: buffer.direction(),
            script: buffer.script(),
        };
        let plan = plans.plan_for(face, features, props);
        aterm_font::shape::shape_substitute_only_with_plan(face, plan, buffer)
    }
}

#[cfg(test)]
thread_local! {
    static SHAPE_PLANS_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn note_shape_plan_build() {
    SHAPE_PLANS_BUILT.with(|count| count.set(count.get() + 1));
}

#[cfg(test)]
pub(crate) fn take_shape_plan_builds() -> usize {
    SHAPE_PLANS_BUILT.with(|count| count.replace(0))
}

fn map_shaped_run(
    face: &aterm_font::Face<'_>,
    run: &str,
    run_chars: &[char],
    admit_collapsed: bool,
    glyphs: &[GlyphInfo],
    scratch: &mut MapScratch,
) -> Option<ShapedRun> {
    match classify_shape(run_chars.len(), glyphs.len(), admit_collapsed) {
        ShapeVerdict::OneToOne => {}
        ShapeVerdict::Collapsed => {
            // A merged ligature spanning an absurd number of cells falls back to
            // the per-cell path (keeps the child-key slice index bounded).
            if run_chars.len() > LIG_SLICE_MAX {
                return None;
            }
            let gid = u16::try_from(glyphs.first()?.glyph_id)
                .ok()
                .filter(|gid| *gid != 0)?;
            let n = u16::try_from(run_chars.len()).ok()?;
            return Some(ShapedRun::Collapsed { gid, n });
        }
        ShapeVerdict::Reject => return None,
    }
    let n = run_chars.len();
    let ascii = run.len() == n;
    scratch.byte_to_idx.clear();
    if !ascii {
        scratch.byte_to_idx.reserve(n);
        let mut b = 0usize;
        for ch in run_chars {
            scratch.byte_to_idx.push(b);
            b += ch.len_utf8();
        }
    }
    scratch.out.clear();
    scratch.out.resize(n, None);
    for info in glyphs {
        let gid = u16::try_from(info.glyph_id).ok()?;
        let cluster = info.cluster as usize;
        let idx = if ascii {
            if cluster >= n {
                return None;
            }
            cluster
        } else {
            let Ok(idx) = scratch.byte_to_idx.binary_search(&cluster) else {
                return None;
            };
            idx
        };
        if scratch.out[idx].replace(gid).is_some() {
            return None;
        }
    }
    let mut any_changed = false;
    for (slot, &ch) in scratch.out.iter_mut().zip(run_chars) {
        let gid = (*slot)?;
        let cmap = face.glyph_index(ch).map_or(0, |g| g.0);
        if gid == 0 && cmap != 0 {
            return None;
        }
        if gid == cmap {
            *slot = None;
        } else {
            any_changed = true;
        }
    }
    if !any_changed {
        return None;
    }
    Some(ShapedRun::PerColumn(scratch.out.as_slice().into()))
}

/// Whether the primary face's `GSUB` table advertises a programming-ligature feature (`liga` or `calt`).
#[must_use]
pub fn font_has_ligature_features(rb_bytes: &[u8], index: u32) -> bool {
    aterm_font::Face::parse(rb_bytes, index).is_ok_and(|face| face_has_ligature_features(&face))
}

/// [`font_has_ligature_features`] of an already-parsed face.
#[must_use]
pub fn face_has_ligature_features(face: &aterm_font::Face<'_>) -> bool {
    face_gsub_has(face, |t| t == *b"liga" || t == *b"calt")
}

/// Whether the primary face's `GSUB` table advertises `tag` (e.g. `zero`, `ss01`).
#[must_use]
pub fn font_advertises_feature(rb_bytes: &[u8], index: u32, tag: [u8; 4]) -> bool {
    aterm_font::Face::parse(rb_bytes, index).is_ok_and(|face| face_gsub_has(&face, |t| t == tag))
}

fn face_gsub_has(face: &aterm_font::Face<'_>, hit: impl Fn([u8; 4]) -> bool) -> bool {
    face.tables()
        .gsub
        .is_some_and(|gsub| gsub.features.into_iter().any(|f| hit(f.tag.to_bytes())))
}

/// Build the per-column glyph plan for one row of `cells`.
#[allow(clippy::too_many_arguments)]
pub fn plan_row_runs<S, F, R>(
    cells: &[RenderCell],
    cols: usize,
    shapeable: &[bool],
    run_boundary_before: &[bool],
    order: &[u16],
    min_run: usize,
    style_of: S,
    mut shape: F,
    run: &mut String,
    run_chars: &mut Vec<char>,
    out: &mut Vec<ColumnGlyph>,
) where
    S: Fn(usize) -> StyleBits,
    F: FnMut(&str, &[char], StyleBits) -> Option<R>,
    R: Borrow<ShapedRun>,
{
    out.clear();
    if cols >= 268_435_456 {
        return;
    }
    out.resize(cols, ColumnGlyph::PerCell);
    let n = cols.min(cells.len());
    let mut c: usize = 0;
    while c < n {
        if !shapeable.get(c).copied().unwrap_or(false) {
            c = c.saturating_add(1);
            continue;
        }
        let style = style_of(c);
        let start = c;
        while c < n
            && shapeable.get(c).copied().unwrap_or(false)
            && style_of(c) == style
            && (c == start || !run_boundary_before.get(c).copied().unwrap_or(false))
        {
            c = c.saturating_add(1);
        }
        let mut piece = start;
        while piece < c {
            let (end, reversed) = next_piece(cells, order, piece, c);
            plan_piece(
                cells, piece, end, reversed, min_run, style, &mut shape, run, run_chars, out,
            );
            piece = end;
        }
    }
}

fn next_piece(cells: &[RenderCell], order: &[u16], start: usize, end: usize) -> (usize, bool) {
    if order.is_empty() {
        return (end, false);
    }
    if let Some(stretch_end) = logical_rtl_stretch(cells, order, start, end) {
        return (stretch_end, true);
    }
    let mut piece_end = start.saturating_add(1);
    while piece_end < end && logical_rtl_stretch(cells, order, piece_end, end).is_none() {
        piece_end = piece_end.saturating_add(1);
    }
    (piece_end, false)
}

fn logical_rtl_stretch(
    cells: &[RenderCell],
    order: &[u16],
    start: usize,
    end: usize,
) -> Option<usize> {
    let logical = |v: usize| order.get(v).map_or(v, |&l| usize::from(l));
    let rtl_letter = |v: usize| {
        cells.get(v).is_some_and(|cell| {
            matches!(
                aterm_bidi::bidi_class(cell.ch),
                aterm_bidi::BidiClass::R | aterm_bidi::BidiClass::AL
            )
        })
    };
    let backwards = |v: usize| {
        v.checked_add(1).is_some_and(|next| {
            next < end
                && logical(next).checked_add(1) == Some(logical(v))
                && rtl_letter(v)
                && rtl_letter(next)
        })
    };
    if !backwards(start) {
        return None;
    }
    let mut last = start.saturating_add(1);
    while backwards(last) {
        last = last.saturating_add(1);
    }
    Some(last.saturating_add(1))
}

/// Shape the cells `[start, end)` of one coalesced run as one piece and write
/// its columns of `out`.
#[allow(clippy::too_many_arguments)]
fn plan_piece<F, R>(
    cells: &[RenderCell],
    start: usize,
    end: usize,
    reversed: bool,
    min_run: usize,
    style: StyleBits,
    shape: &mut F,
    run: &mut String,
    run_chars: &mut Vec<char>,
    out: &mut [ColumnGlyph],
) where
    F: FnMut(&str, &[char], StyleBits) -> Option<R>,
    R: Borrow<ShapedRun>,
{
    run.clear();
    run_chars.clear();
    let cols = cells.get(start..end).unwrap_or(&[]);
    let mut push = |cell: &RenderCell| {
        run.push(cell.ch);
        run_chars.push(cell.ch);
    };
    if reversed {
        cols.iter().rev().for_each(&mut push);
    } else {
        cols.iter().for_each(&mut push);
    }
    if run_chars.len() < min_run {
        return;
    }
    let column = |i: usize| {
        if reversed {
            end.checked_sub(1)?
                .checked_sub(i)
                .filter(|&col| col >= start)
        } else {
            start.checked_add(i).filter(|&col| col < end)
        }
    };
    match shape(run.as_str(), run_chars.as_slice(), style)
        .as_ref()
        .map(Borrow::borrow)
    {
        Some(ShapedRun::PerColumn(gids)) => {
            for (i, slot) in gids.iter().enumerate() {
                // Only columns shaping changed become Ligated, so procedural and
                // fallback cells keep their own face. `shape` is caller-supplied:
                // `column` and get_mut make an oversized gid array a no-op.
                if let Some(gid) = *slot
                    && let Some(cell) = column(i).and_then(|col| out.get_mut(col))
                {
                    *cell = ColumnGlyph::Ligated(gid);
                }
            }
        }
        Some(&ShapedRun::Collapsed { gid, n }) => {
            for k in 0..(n as usize) {
                if let Some(cell) = start
                    .checked_add(k)
                    .filter(|&col| col < end)
                    .and_then(|col| out.get_mut(col))
                {
                    *cell = ColumnGlyph::LigatedSlice {
                        gid,
                        k: k as u16,
                        n,
                    };
                }
            }
        }
        None => {}
    }
}

#[cfg(test)]
#[path = "ligature_shaping_tests.rs"]
mod tests;
