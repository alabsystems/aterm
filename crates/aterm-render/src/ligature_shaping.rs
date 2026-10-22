// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Programming-ligature run shaping for the terminal grid.
//!
//! `aterm_font::shape` shapes a run of adjacent same-style cells with `liga`/`calt`. A
//! monospace ligature font keeps one glyph per input cell: empty placeholders on the lead
//! cells and, on the last, the wide ligature glyph, which overflows back across the run.
//! So a ligature draws on the cells its characters occupy and the cadence never changes.
//!
//! The CPU [`crate::Renderer`] and the GPU renderer consume the same per-cell plan, so
//! their output matches. A run whose glyph ids shaping leaves unchanged draws exactly as
//! the plain per-cell path.
//!
//! A row aterm-core reordered for bidi arrives in visual order, with the permutation in
//! `RenderInput::visual_to_logical`. [`plan_row_runs`] shapes a stretch of right-to-left
//! letters the reorder laid out backwards in logical order and puts its glyphs back, so
//! Arabic joining and every `GSUB` rule read the letters as written. Everything else, and
//! every row the reorder left alone, is shaped as the row holds it.

use std::borrow::Borrow;

use aterm_core::terminal::RenderCell;
use aterm_font::shape::{
    Direction, Feature, GlyphBuffer, GlyphInfo, ShapeFace, ShapePlan, UnicodeBuffer,
};
use aterm_grapheme::Script;
use aterm_types::text_shaping::FontFeature;

use crate::StyleBits;

/// Build the feature list for every shaping run: `liga`/`calt` at `1`, or `0` when
/// ligatures are off (so a run can apply `zero` or `ss01` without forming `=>`), then the
/// user's `font_features`. Every entry applies over the whole run and the later entry wins
/// for a tag, so an explicit user `liga`/`calt` overrides the base. The list is
/// first-party; `engine_features` builds the shaper's form of it.
///
/// Built once per shaping config ([`crate::Renderer::set_text_shaping`]), not per run.
#[must_use]
pub fn build_feature_list(user: &[FontFeature], ligatures_on: bool) -> Vec<FontFeature> {
    let base = u32::from(ligatures_on);
    let mut features = Vec::with_capacity(2 + user.len());
    features.push(FontFeature::new(*b"liga", base));
    features.push(FontFeature::new(*b"calt", base));
    features.extend_from_slice(user);
    features
}

/// The shaper's form of a [`build_feature_list`] result: each feature over the whole run,
/// in order (the last writer wins). Built once per shaping-config change.
pub(crate) fn engine_features(features: &[FontFeature]) -> Vec<Feature> {
    features
        .iter()
        .map(|f| Feature::new(aterm_font::Tag::from_bytes(&f.tag), f.value))
        .collect()
}

trust_contract_fn! {
    /// Whether [`crate::Renderer::row_glyph_plan`] should run the shaping pass
    /// for a row, vs short-circuit to an all-[`ColumnGlyph::PerCell`] plan. Factored out
    /// as a PURE function so `tests/font_oracle.rs` asks the same gate the planner does.
    ///
    /// Run shaping iff the primary-face bytes are retained AND there is OpenType work to
    /// do: either the user configured `font_features` (`has_user_features`), OR ligatures
    /// are on for a font that advertises `liga`/`calt`. When `has_user_features` is false
    /// this reduces EXACTLY to the legacy gate (`rb_present && !globally_off && font_has`),
    /// so the no-features path stays byte-identical — proved for ALL inputs by
    /// `should_run_shaping_no_features_is_legacy`.
    ///
    /// # Invariant (proven)
    ///
    /// `should_run_shaping(rb, u, off, lig) == (rb && (u || (!off && lig)))` for
    /// every input, kernel-checked against the compiled body by
    /// `should_run_shaping_is_gate`.
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

// Spec `shaping-gate` (SPECS.md), the `should_run_shaping` half: for every flag
// combination shaping runs iff the primary bytes are present and there is
// OpenType work (user features, or ligatures on for a font that has them); with
// no user features that is exactly the legacy gate. Kernel-checked against the
// compiled body.
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

/// The minimum run length the planner will attempt to shape: 1 when the user
/// configured `font_features` (a single cell can substitute — `zero` on a lone `0`,
/// `ss01` on one char), else 2 (a single cell cannot ligate). PURE; keeps the two
/// `min_run` derivation sites (planner gate + run gate) from drifting.
#[inline]
#[must_use]
pub const fn shaping_min_run(has_user_features: bool) -> usize {
    if has_user_features { 1 } else { 2 }
}

/// Whether a shaping result (`n_out` glyphs over `n_in` cells) maps onto the monospace grid.
///
/// - [`ShapeVerdict::OneToOne`]: `n_out == n_in`, the spacer convention of Fira Code and
///   JetBrains Mono (empty placeholders on the lead cells, the wide glyph on the last).
/// - [`ShapeVerdict::Collapsed`]: `n_out == 1`, `n_in >= 2`, Cascadia's merged convention.
///   Admitted only with `admit_collapsed` (config, default off), since it needs the
///   raster-slicing path ([`extract_cell_slice`]).
///
/// - [`ShapeVerdict::OneToOne`] — `n_out == n_in`: the Fira/JetBrains "spacer
///   convention" every shipped ligature font uses (one advance per input cell:
///   empty placeholder glyphs on the leads, the wide glyph on the final cell).
///   This is the ONLY form the renderer draws today.
/// - [`ShapeVerdict::Collapsed`] — `n_out == 1` and `n_in >= 2`: the Cascadia
///   "merged" (N:1) convention where the whole ligature collapses to ONE wide
///   glyph. Admitted ONLY when `admit_collapsed` is set (a config flag, default
///   off), because it requires the raster-slicing render path (rasterize the wide
///   glyph once, slice its coverage at `cell_w` boundaries — [`extract_cell_slice`]).
///
/// Anything else — a partial collapse (`1 < n_out < n_in`), an expansion
/// (`n_out > n_in`), or a collapse while `admit_collapsed` is off — is
/// [`ShapeVerdict::Reject`]. This is the CONSERVATIVE gate: it never lets a
/// non-grid-mappable shape reach the blitter. Bound to the `LigatureGate` ty
/// model over the whole small-count lattice by
/// `classify_shape_conforms_to_ligature_gate_model` (Tier-1: the model's own
/// `Classify` rule judges every real verdict), and caught at `Buggy=1` (a gate
/// that admits collapse without the flag) by the derived ty model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeVerdict {
    /// Not grid-mappable: use the per-cell path.
    Reject,
    /// One glyph per input cell, drawn as one [`ColumnGlyph::Ligated`] per column.
    OneToOne,
    /// `n_in` cells collapsed to one wide glyph, rasterized once and sliced into `n_in`
    /// cell tiles. Only with `admit_collapsed`.
    Collapsed,
}

/// Classify a shaping result of `n_out` output glyphs over `n_in` input cells into
/// the grid-mappable [`ShapeVerdict`] (M4). PURE, so the gate is unit-tested and
/// bound to the `LigatureGate` ty model independent of a live shaper.
///
/// With `admit_collapsed == false` this reduces EXACTLY to the legacy gate
/// (`accept iff n_out == n_in`), so the no-flag path stays byte-identical. The
/// `Collapsed` branch is the ONLY
/// behaviour the flag adds, and it admits EXACTLY the `N:1` (`N >= 2`) case.
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
///
/// `Ligated` is blitted as a [`crate::GlyphKey::mono_gid`] whose wide glyph overflows back
/// across the run's lead cells. A `LigatedSlice` tile is cell-local (keyed by
/// [`crate::Renderer::ligature_slice_key`]), so per-cell foreground, selection and
/// block-cursor recolour work over the ligature. `PerCell` takes the ordinary dispatch
/// ([`crate::Renderer::resolve_cell_key`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnGlyph {
    /// Use the ordinary per-cell dispatch for this column (no ligature touched it).
    PerCell,
    /// Draw this shaped primary-face glyph id at the column's monospace origin.
    Ligated(u16),
    /// Cell `k` of an `n`-cell collapsed (Cascadia N:1) ligature: draw slice `k`
    /// of the wide glyph `gid` as a cell-local tile at this column.
    LigatedSlice { gid: u16, k: u16, n: u16 },
}

/// The result of shaping one run, returned by the [`plan_row_runs`] `shape` closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShapedRun {
    /// 1:1 spacer convention (Fira, JetBrains): `Some(gid)` where shaping changed the
    /// column's glyph, `None` to keep it per-cell.
    PerColumn(Box<[Option<u16>]>),
    /// N:1 collapse (Cascadia): one wide `gid` over `n` cells, expanded to `n`
    /// [`ColumnGlyph::LigatedSlice`] columns. Only with `admit_collapsed`.
    Collapsed { gid: u16, n: u16 },
}

/// The most cells an N:1 collapse is admitted for (real programming ligatures span 2–4);
/// a longer run stays per-cell. Keeps the slice index in
/// [`crate::Renderer::ligature_slice_key`] small.
pub const LIG_SLICE_MAX: usize = 64;

/// Extract cell `k`'s slice of a wide ligature raster as a cell-local tile (`cell_w ×
/// height`, left bearing 0). Tile column `j` is raster column `k*cell_w + j - xmin`, or 0
/// outside `[0, raster_w)`, so the tile at the cell's origin reproduces the wide glyph's
/// coverage over that cell, and the tiles for `k = 0..n`, minus the leading `xmin` pad,
/// reassemble the raster (`extract_cell_slice_reassembles_wide_raster`).
///
/// Empty for `cell_w == 0`, `height == 0` or a too-short `raster`; the caller then keeps
/// the per-cell path.
#[must_use]
pub fn extract_cell_slice(
    raster: &[u8],
    raster_w: usize,
    height: usize,
    xmin: i32,
    cell_w: usize,
    k: usize,
) -> Vec<u8> {
    if cell_w == 0 || height == 0 || raster.len() < raster_w.saturating_mul(height) {
        return Vec::new();
    }
    let mut tile = vec![0u8; cell_w * height];
    // Dest column j of the tile maps to raster source column `base + j`.
    let base = (k as i64) * (cell_w as i64) - xmin as i64;
    for row in 0..height {
        let src_row = row * raster_w;
        let dst_row = row * cell_w;
        for j in 0..cell_w {
            let src_col = base + j as i64;
            if src_col >= 0 && (src_col as usize) < raster_w {
                tile[dst_row + j] = raster[src_row + src_col as usize];
            }
        }
    }
    tile
}

/// Whether `cell` may join a ligature shaping run: not a wide continuation, space or
/// control, not emoji/text-presentation, not in a shaped emoji cluster, not image-covered.
/// Spaces break runs, so `a => b` shapes only `=>`. The caller also breaks runs on a
/// style change and at the cursor and selection columns.
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
        // A script cell (aterm's own chrome) is drawn glyph by glyph, small.
        && matches!(cell.script, aterm_core::terminal::Script::Normal)
}

/// One row's fixed font instance and feature list. The renderer already parses
/// each face once per row; reuse its shaping plans as well. Newly streamed text
/// misses the run cache, but every ordinary Latin token can use the same plan.
/// No plan survives its face or crosses a font/feature/variation change.
pub(crate) struct RowShaper<'a> {
    face: ShapeFace<'a>,
    features: &'a [Feature],
    plans: Vec<(SegmentProperties, ShapePlan)>,
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
            plans: Vec::new(),
        }
    }

    /// Shape one run and return its grid plan, or `None` unless the shape is grid-mappable
    /// ([`classify_shape`]) and changed a glyph vs the plain cmap. Each glyph is placed at
    /// its column's monospace origin, so the cadence comes from placement, not advances.
    pub(crate) fn shape(
        &mut self,
        run: &str,
        run_chars: &[char],
        admit_collapsed: bool,
        scratch: &mut UnicodeBuffer,
    ) -> Option<ShapedRun> {
        // Nothing ligates in one cell, but user features past the base `liga`/`calt`
        // pair can substitute one (`zero` on a lone `0`).
        let min_run = if self.features.len() > 2 { 1 } else { 2 };
        if run_chars.len() < min_run {
            return None;
        }
        let shaped = self.shape_buffer(run, scratch);
        let result = map_shaped_run(
            self.face.face(),
            run,
            run_chars,
            admit_collapsed,
            shaped.glyph_infos(),
        );
        // Clear resets segment properties as well as text, so a Latin run
        // cannot lend its direction/script to the next Arabic or common run.
        // Always return the allocation, including unmappable/rejected shapes.
        *scratch = shaped.clear();
        result
    }

    /// Shape `run` in the order given, which is logical: [`plan_row_runs`] has already put
    /// a backwards right-to-left stretch back in written order. Direction and script are
    /// guessed from the text.
    fn shape_buffer(&mut self, run: &str, scratch: &mut UnicodeBuffer) -> GlyphBuffer {
        let mut buffer = std::mem::take(scratch);
        buffer.push_str(run);
        buffer.guess_segment_properties();
        let props = SegmentProperties {
            direction: buffer.direction(),
            script: buffer.script(),
        };
        let slot = self
            .plans
            .iter()
            .position(|(p, _)| *p == props)
            .unwrap_or_else(|| {
                #[cfg(test)]
                note_shape_plan_build();
                let plan = ShapePlan::new(&self.face, props.direction, props.script, self.features);
                // A bounded row-local memo: keep the first three script families
                // and replace the fourth for a row mixing many scripts. Eviction
                // changes only compilation work, never the selected plan's inputs.
                if self.plans.len() == 4 {
                    self.plans[3] = (props, plan);
                    3
                } else {
                    self.plans.push((props, plan));
                    self.plans.len() - 1
                }
            });
        aterm_font::shape::shape_with_plan(&self.face, &self.plans[slot].1, buffer)
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
) -> Option<ShapedRun> {
    // `OneToOne` is drawn as one gid per column below. `Collapsed` (only with
    // `admit_collapsed`) returns the wide gid and cell count; the planner expands it to
    // `n` `LigatedSlice` columns and the raster path slices the glyph. Anything else
    // stays per-cell.
    match classify_shape(run_chars.len(), glyphs.len(), admit_collapsed) {
        ShapeVerdict::OneToOne => {}
        ShapeVerdict::Collapsed => {
            // A merged ligature spanning an absurd number of cells falls back to
            // the per-cell path (keeps the child-key slice index bounded).
            if run_chars.len() > LIG_SLICE_MAX {
                return None;
            }
            // `Collapsed` guarantees exactly one output glyph over `n >= 2` cells.
            // A collapse onto `.notdef` would draw a box over the whole run.
            let gid = u16::try_from(glyphs.first()?.glyph_id)
                .ok()
                .filter(|gid| *gid != 0)?;
            let n = u16::try_from(run_chars.len()).ok()?;
            return Some(ShapedRun::Collapsed { gid, n });
        }
        ShapeVerdict::Reject => return None,
    }
    // Map each output glyph to its input char by `cluster` (the pushed byte offset).
    let n = run_chars.len();
    // All-ASCII fast path: when byte length equals char count the cluster is the char
    // index and no offset table is built. Ligature runs (`=>`, `!=`, `::`) take it.
    let ascii = run.len() == n;
    // The table is strictly increasing, so lookup is a binary search: a run can be as
    // long as the row (a `---` rule, a base64 blob), and a linear scan per glyph would be
    // O(n²) on every shaped-run cache miss.
    let mut byte_to_idx: Vec<usize> = Vec::new();
    if !ascii {
        byte_to_idx.reserve_exact(n);
        let mut b = 0usize;
        for ch in run_chars {
            byte_to_idx.push(b);
            b += ch.len_utf8();
        }
    }
    // Start every column at `None`, populate each column once with its shaped
    // gid here, then demote the UNCHANGED columns to `None` in the cmap pass
    // below.
    let mut out: Vec<Option<u16>> = vec![None; n];
    for info in glyphs {
        let gid = u16::try_from(info.glyph_id).ok()?;
        let cluster = info.cluster as usize;
        // Find the char index whose byte offset == this cluster.
        let idx = if ascii {
            if cluster >= n {
                return None; // cluster past the run's chars — bail to per-cell
            }
            cluster
        } else {
            let Ok(idx) = byte_to_idx.binary_search(&cluster) else {
                return None; // cluster didn't land on a char boundary — bail to per-cell
            };
            idx
        };
        if out[idx].replace(gid).is_some() {
            return None; // non-1:1 cluster collision — bail to per-cell
        }
    }
    // A column keeps its shaped gid only when shaping changed it from the plain cmap
    // glyph; otherwise it is `None` and stays per-cell. So a char the primary face lacks
    // (gid 0 == cmap 0) still reaches the fallback face (no `.notdef` tofu), and an
    // unsubstituted or procedural neighbour keeps its glyph. A char the face covers that
    // shaping turned into `.notdef` is a ligature the face cannot draw (Monocraft's
    // arrows): the whole run goes per-cell rather than draw a box. If nothing changed,
    // the whole run declines to the per-cell path.
    let mut any_changed = false;
    for (slot, &ch) in out.iter_mut().zip(run_chars) {
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
    Some(ShapedRun::PerColumn(out.into_boxed_slice()))
}

/// Whether the primary face's `GSUB` table advertises a programming-ligature
/// feature (`liga` or `calt`) — the only features the run shaper turns on.
///
/// A font with neither feature can produce NO substitution under those features,
/// so the shaper would return exactly the cmap glyph ids the per-cell path already
/// uses: shaping such a run is provably a no-op (we'd always hit the `!changed`
/// decline). Computing this ONCE at face build time lets the planner short-circuit
/// the whole run-coalescing + shaping path for non-ligature fonts — byte-identical
/// output, no per-frame shaping cost.
///
/// `false` when there is no `GSUB` table or face `index` of the bytes doesn't
/// parse.
#[must_use]
pub fn font_has_ligature_features(rb_bytes: &[u8], index: u32) -> bool {
    aterm_font::Face::parse(rb_bytes, index).is_ok_and(|face| face_has_ligature_features(&face))
}

/// [`font_has_ligature_features`] of an already-parsed face: the one answer
/// the renderer's shaping gate and the font picker's "has ligatures" fact
/// share.
#[must_use]
pub fn face_has_ligature_features(face: &aterm_font::Face<'_>) -> bool {
    face_gsub_has(face, |t| t == *b"liga" || t == *b"calt")
}

/// Whether the primary face's `GSUB` table advertises `tag` (e.g. `zero`, `ss01`). A face
/// without `GSUB` (Menlo, Monaco) advertises nothing, so no configured feature can
/// substitute; this backs the GUI's "font_features had no effect" warning.
#[must_use]
pub fn font_advertises_feature(rb_bytes: &[u8], index: u32, tag: [u8; 4]) -> bool {
    aterm_font::Face::parse(rb_bytes, index).is_ok_and(|face| face_gsub_has(&face, |t| t == tag))
}

/// Whether the face's `GSUB` feature list holds a tag `hit` accepts. LINEAR: FeatureList
/// records are stored in arbitrary, not tag-sorted, order, so a binary `find` could
/// miss a present tag. Read through aterm-font, whose `GSUB` lists are the ones
/// the shaper shapes with (the TF.1 reader corpus pins them, and its
/// `ligature_gate` answer pins this gate).
fn face_gsub_has(face: &aterm_font::Face<'_>, hit: impl Fn([u8; 4]) -> bool) -> bool {
    face.tables()
        .gsub
        .is_some_and(|gsub| gsub.features.into_iter().any(|f| hit(f.tag.to_bytes())))
}

/// Build the per-column glyph plan for one row of `cells`: `Ligated` in a 1:1 ligated run,
/// `LigatedSlice` in a collapsed (N:1) run, `PerCell` elsewhere.
///
/// `shapeable[c]` says whether column `c` may join a run ([`cell_is_shapeable`] plus the
/// caller's per-frame exclusions: cursor, selection, `CursorDisabled` mode).
/// `run_boundary_before[c]` starts a fresh run at `c` without excluding it, so a right
/// pane that begins with `=>` can still ligate. A change in `style_of` also breaks a run.
///
/// `order` is the row's bidi visual→logical map (`RenderInput::visual_to_logical`, empty
/// for a row in logical order). A run is shaped in pieces: a stretch of right-to-left
/// letters the map lays out backwards ([`logical_rtl_stretch`]) in logical order, the rest
/// as the row holds it. `shape` shapes one piece in logical order (the caller caches it by
/// that text) and returns `None` if it did not ligate.
///
/// Shared by the CPU and GPU renderers so both draw the same glyph at the same column.
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
    // Anything that borrows as a `ShapedRun` will do: an owned one (tests) or the
    // renderer memo's `Arc<ShapedRun>`, so a cache hit is a refcount bump.
    F: FnMut(&str, &[char], StyleBits) -> Option<R>,
    R: Borrow<ShapedRun>,
{
    out.clear();
    // A 268M-column terminal is physically impossible; this guard bounds the resize
    // allocation.
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
        // Coalesce a maximal run of shapeable cells with the same style.
        let style = style_of(c);
        let start = c;
        while c < n
            && shapeable.get(c).copied().unwrap_or(false)
            && style_of(c) == style
            && (c == start || !run_boundary_before.get(c).copied().unwrap_or(false))
        {
            c = c.saturating_add(1);
        }
        // Shape it piece by piece: each right-to-left stretch the bidi step
        // laid out backwards in logical order, the cells around it as given.
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

/// The piece of the coalesced run `[start, end)` that begins at `start`: where
/// it ends, and whether it is a right-to-left stretch to shape reversed
/// ([`logical_rtl_stretch`]). A row in logical order (`order` empty, every
/// pure left-to-right row) is one piece, with no per-cell work.
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

/// Whether the cells from `start` (before `end`) open a stretch of right-to-left letters
/// (bidi class R or AL) that `order` lays out backwards, each painted one column left of
/// its logical successor, and if so where the stretch ends. Only such a stretch is
/// reversed for shaping: the reorder put it there, and joining reads it the other way.
/// Neutrals at a right-to-left level (an operator between two Hebrew words) and
/// left-to-right text keep the order the grid shows, the order a programming ligature's
/// one-glyph-per-cell convention is drawn in. An empty `order` (a row the reorder left
/// alone) or an ascending one (a logical-order pane composed beside a reordered one) has
/// no such stretch.
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
    // Column `v` and `v + 1` (both before `end`) are two right-to-left
    // letters painted backwards. The map comparison goes first: it is free.
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
/// its columns of `out`. A `reversed` piece is handed to `shape` in logical
/// order (last column first) and each glyph goes back to the column its
/// character is painted at.
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
        return; // too short for this mode; stays PerCell
    }
    // The column of the piece's `i`th character in logical order.
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
            // N:1: the wide glyph spans the piece's `n` cells; column `start + k` draws
            // slice `k` of it (`extract_cell_slice`), left to right whichever order the
            // characters were shaped in.
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
mod tests {
    use super::*;
    use aterm_core::terminal::{RenderCell, UnderlineStyle};

    fn infos(buffer: &GlyphBuffer) -> Vec<(u32, u32)> {
        buffer
            .glyph_infos()
            .iter()
            .map(|g| (g.glyph_id, g.cluster))
            .collect()
    }

    fn positions(buffer: &GlyphBuffer) -> Vec<(i32, i32, i32, i32)> {
        buffer
            .glyph_positions()
            .iter()
            .map(|g| (g.x_advance, g.y_advance, g.x_offset, g.y_offset))
            .collect()
    }

    #[test]
    fn row_shaper_matches_fresh_plans_across_segment_and_font_changes() {
        let mut fonts = vec![
            include_bytes!("../assets/DejaVuSansMono.ttf").to_vec(),
            include_bytes!("../tests/fixtures/jetbrains-mono.ttf").to_vec(),
        ];
        if cfg!(target_os = "macos")
            && let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf")
        {
            fonts.push(bytes);
        }
        let feature_sets = [
            engine_features(&build_feature_list(&[], true)),
            engine_features(&build_feature_list(&[FontFeature::new(*b"zero", 1)], true)),
            engine_features(&build_feature_list(&[FontFeature::new(*b"ss01", 1)], false)),
        ];
        let runs = [
            "function",
            "=>",
            "000",
            "office",
            "éè",
            "Русский",
            "العربية",
            "עברית",
            "देवनागरी",
            "Ελληνικά",
            "漢字",
            "e\u{301}",
            "!=",
            "",
            "0",
            "x",
            "tail",
        ];
        let mut scratch = UnicodeBuffer::new();
        let mut changed_glyphs = false;
        for bytes in &fonts {
            for features in &feature_sets {
                for weight in [350.0, 700.0] {
                    let mut face = aterm_font::Face::parse(bytes, 0).expect("fixture face");
                    face.set_variation(aterm_font::Tag::from_bytes(b"wght"), weight);
                    let mut shaper = RowShaper::new(ShapeFace::new(face.clone()), features);
                    let fresh_face = ShapeFace::new(face);
                    for run in runs.iter().chain(runs.iter().rev()) {
                        // The buffer `shape_buffer` builds (the run as given, its
                        // direction guessed), shaped with a plan built for it alone.
                        let mut fresh = UnicodeBuffer::new();
                        fresh.push_str(run);
                        fresh.guess_segment_properties();
                        let expected = aterm_font::shape::shape(&fresh_face, features, fresh);
                        let actual = shaper.shape_buffer(run, &mut scratch);
                        assert_eq!(infos(&actual), infos(&expected), "glyphs: {run}");
                        assert_eq!(positions(&actual), positions(&expected), "positions: {run}");
                        scratch = actual.clear();
                        assert!(shaper.plans.len() <= 4);
                        assert!(scratch.is_empty());
                        assert_eq!(scratch.direction(), Direction::Invalid);
                        assert_eq!(scratch.script(), None);
                        let chars = run.chars().collect::<Vec<_>>();
                        changed_glyphs |= shaper.shape(run, &chars, true, &mut scratch).is_some();
                    }
                }
            }
        }
        assert!(
            changed_glyphs,
            "the corpus must include actual substitutions"
        );
    }

    /// The feature list: the base `liga`/`calt` pair first (on when ligatures
    /// are on, zeroed when off), then every user feature in order with its
    /// exact value. Order is the contract: a user `liga=0` lands AFTER the base
    /// `liga=1`, and the shaper is last-writer-wins.
    #[test]
    fn build_feature_list_puts_the_base_pair_first_then_user_features() {
        let user = [FontFeature::new(*b"cv01", 2), FontFeature::new(*b"liga", 0)];
        #[expect(clippy::type_complexity, reason = "a test's case table")]
        let cases: [(&[FontFeature], bool, &[([u8; 4], u32)]); 3] = [
            (&[], true, &[(*b"liga", 1), (*b"calt", 1)]),
            (
                &user,
                true,
                &[(*b"liga", 1), (*b"calt", 1), (*b"cv01", 2), (*b"liga", 0)],
            ),
            (
                &user[..1],
                false,
                &[(*b"liga", 0), (*b"calt", 0), (*b"cv01", 2)],
            ),
        ];
        for (user, on, want) in cases {
            let got: Vec<([u8; 4], u32)> = build_feature_list(user, on)
                .iter()
                .map(|f| (f.tag, f.value))
                .collect();
            assert_eq!(got, want, "ligatures_on={on} user={user:?}");
        }
    }

    /// The engine form mirrors the first-party list entry for entry, in order (order is
    /// precedence).
    #[test]
    fn engine_features_mirror_the_list_over_the_whole_run() {
        let list = build_feature_list(
            &[FontFeature::new(*b"cv01", 2), FontFeature::new(*b"liga", 0)],
            true,
        );
        let engine = engine_features(&list);
        assert_eq!(engine.len(), list.len());
        for (e, f) in engine.iter().zip(&list) {
            assert_eq!(e.tag.to_bytes(), f.tag);
            assert_eq!(e.value, f.value);
        }
    }

    /// A plain single-`char` cell with every rendition flag off.
    fn cell(ch: char) -> RenderCell {
        RenderCell {
            ch,
            fg: [0, 0, 0],
            bg: [0, 0, 0],
            wide: false,
            emoji_presentation: false,
            text_presentation: false,
            bold: false,
            italic: false,
            underline: UnderlineStyle::None,
            strikethrough: false,
            overline: false,
            underline_color: None,
            overline_color: None,
            script: aterm_core::terminal::Script::Normal,
        }
    }

    #[test]
    fn explicit_presentation_cells_are_not_shapeable() {
        let mut c = cell('\u{1F600}');
        c.text_presentation = true;
        assert!(
            !cell_is_shapeable(&c, false, false),
            "VS15 must stay on the presentation-aware per-cell resolver"
        );
        c.text_presentation = false;
        c.emoji_presentation = true;
        assert!(
            !cell_is_shapeable(&c, false, false),
            "VS16 must stay on the presentation-aware per-cell resolver"
        );
    }

    /// A ligature run breaks at an image-covered cell: the `=>` on each side ligates alone.
    #[test]
    fn ligature_run_breaks_on_image_cell() {
        let cells = [cell('='), cell('>'), cell('='), cell('='), cell('>')];
        let shapeable: Vec<bool> = (0..cells.len())
            .map(|c| cell_is_shapeable(&cells[c], false, c == 2))
            .collect();
        // Closure ligates any "=>" by emitting a distinctive (non-cmap) gid pair.
        let shape = |run: &str, chars: &[char], _style: StyleBits| -> Option<ShapedRun> {
            if run == "=>" && chars.len() == 2 {
                Some(ShapedRun::PerColumn(
                    vec![Some(900u16), Some(901u16)].into_boxed_slice(),
                ))
            } else {
                None
            }
        };
        let mut out = Vec::new();
        let mut run = String::new();
        let mut run_chars: Vec<char> = Vec::new();
        plan_row_runs(
            &cells,
            cells.len(),
            &shapeable,
            &[],
            &[],
            2,
            |_c| StyleBits::REGULAR,
            shape,
            &mut run,
            &mut run_chars,
            &mut out,
        );
        // Had one run spanned the image cell, the closure would decline "=>==>" and leave
        // every column PerCell.
        assert_eq!(out[0], ColumnGlyph::Ligated(900));
        assert_eq!(out[1], ColumnGlyph::Ligated(901));
        assert_eq!(
            out[2],
            ColumnGlyph::PerCell,
            "the image-covered cell must stay per-cell"
        );
        assert_eq!(out[3], ColumnGlyph::Ligated(900));
        assert_eq!(out[4], ColumnGlyph::Ligated(901));
    }

    /// A pane edge starts a new run without excluding its first cell, so a right pane that
    /// begins with `=>` still ligates.
    #[test]
    fn run_boundary_preserves_right_pane_initial_ligature() {
        let cells = [cell('x'), cell('x'), cell('='), cell('>')];
        let shapeable = vec![true; cells.len()];
        let run_boundary_before = [false, false, true, false];
        let shape = |run: &str, chars: &[char], _style: StyleBits| -> Option<ShapedRun> {
            (run == "=>" && chars.len() == 2)
                .then(|| ShapedRun::PerColumn(vec![Some(910), Some(911)].into_boxed_slice()))
        };
        let mut out = Vec::new();
        let (mut run, mut run_chars) = (String::new(), Vec::new());
        plan_row_runs(
            &cells,
            cells.len(),
            &shapeable,
            &run_boundary_before,
            &[],
            2,
            |_c| StyleBits::REGULAR,
            shape,
            &mut run,
            &mut run_chars,
            &mut out,
        );

        assert_eq!(out[0], ColumnGlyph::PerCell);
        assert_eq!(out[1], ColumnGlyph::PerCell);
        assert_eq!(out[2], ColumnGlyph::Ligated(910));
        assert_eq!(out[3], ColumnGlyph::Ligated(911));
    }

    /// Plan one row of `text` (every cell shapeable, one style) with the
    /// bidi map `order`, through a shaper that gives the `i`th character of
    /// each piece it is handed glyph `100 + i`. Returns the pieces and plan.
    fn plan_pieces(text: &str, order: &[u16]) -> (Vec<String>, Vec<ColumnGlyph>) {
        let cells: Vec<RenderCell> = text.chars().map(cell).collect();
        let shapeable = vec![true; cells.len()];
        let mut seen = Vec::new();
        let shape = |run: &str, chars: &[char], _s: StyleBits| -> Option<ShapedRun> {
            seen.push(run.to_string());
            let gids = (0..chars.len()).map(|i| u16::try_from(100 + i).ok());
            Some(ShapedRun::PerColumn(gids.collect()))
        };
        let mut out = Vec::new();
        let (mut run, mut run_chars) = (String::new(), Vec::new());
        plan_row_runs(
            &cells,
            cells.len(),
            &shapeable,
            &[],
            order,
            2,
            |_c| StyleBits::REGULAR,
            shape,
            &mut run,
            &mut run_chars,
            &mut out,
        );
        (seen, out)
    }

    fn ligated(gids: &[u16]) -> Vec<ColumnGlyph> {
        gids.iter().map(|&g| ColumnGlyph::Ligated(g)).collect()
    }

    /// A stretch of right-to-left letters the bidi reorder painted backwards is shaped in
    /// logical order, and each glyph goes back to the column its letter is painted at; the
    /// left-to-right operator after it is shaped as the row holds it. Visual `ابحرم==` is
    /// logical `مرحبا==`.
    #[test]
    fn a_backwards_rtl_stretch_is_shaped_in_logical_order() {
        let visual = "\u{627}\u{628}\u{62D}\u{631}\u{645}==";
        let (seen, out) = plan_pieces(visual, &[4, 3, 2, 1, 0, 5, 6]);
        assert_eq!(seen, ["\u{645}\u{631}\u{62D}\u{628}\u{627}", "=="]);
        assert_eq!(out, ligated(&[104, 103, 102, 101, 100, 100, 101]));
        // Negative control: the same cells in logical order (no map, or the identity a
        // logical-order pane is composed with) are one piece, shaped as the row holds them.
        for order in [&[][..], &[0, 1, 2, 3, 4, 5, 6]] {
            let (seen, out) = plan_pieces(visual, order);
            assert_eq!(seen, [visual], "{order:?}");
            assert_eq!(out, ligated(&[100, 101, 102, 103, 104, 105, 106]));
        }
    }

    /// Neutrals at a right-to-left level keep the order the grid shows: in logical `אב==גד`
    /// the operator sits between two Hebrew words, so the reorder paints the whole token
    /// backwards (`דג==בא`), but only the letters are shaped reversed. A programming
    /// ligature's spacer and wide glyph are drawn left to right.
    #[test]
    fn rtl_level_neutrals_keep_the_painted_order() {
        let (seen, out) = plan_pieces("\u{5D3}\u{5D2}==\u{5D1}\u{5D0}", &[5, 4, 3, 2, 1, 0]);
        assert_eq!(seen, ["\u{5D2}\u{5D3}", "==", "\u{5D0}\u{5D1}"]);
        assert_eq!(out, ligated(&[101, 100, 100, 101, 101, 100]));
    }

    /// A `Collapsed` result expands to `n` `LigatedSlice` columns; columns outside the run
    /// stay `PerCell`.
    #[test]
    fn plan_row_runs_expands_collapsed_to_slices() {
        // `x <=> y`: the `<=>` run (cols 2..=4) collapses; spaces break the run and the lone
        // `x`/`y` stay per-cell (min_run 2).
        let cells = [
            cell('x'),
            cell(' '),
            cell('<'),
            cell('='),
            cell('>'),
            cell(' '),
            cell('y'),
        ];
        let shapeable: Vec<bool> = cells
            .iter()
            .map(|c| cell_is_shapeable(c, false, false))
            .collect();
        // Collapse any 3-char run to wide gid 700 over 3 cells.
        let shape = |_run: &str, chars: &[char], _s: StyleBits| -> Option<ShapedRun> {
            (chars.len() == 3).then_some(ShapedRun::Collapsed { gid: 700, n: 3 })
        };
        let mut out = Vec::new();
        let (mut run, mut run_chars) = (String::new(), Vec::new());
        plan_row_runs(
            &cells,
            cells.len(),
            &shapeable,
            &[],
            &[],
            2,
            |_c| StyleBits::REGULAR,
            shape,
            &mut run,
            &mut run_chars,
            &mut out,
        );
        assert_eq!(out[0], ColumnGlyph::PerCell, "lone 'x' stays per-cell");
        assert_eq!(out[1], ColumnGlyph::PerCell, "space");
        assert_eq!(
            out[2],
            ColumnGlyph::LigatedSlice {
                gid: 700,
                k: 0,
                n: 3
            }
        );
        assert_eq!(
            out[3],
            ColumnGlyph::LigatedSlice {
                gid: 700,
                k: 1,
                n: 3
            }
        );
        assert_eq!(
            out[4],
            ColumnGlyph::LigatedSlice {
                gid: 700,
                k: 2,
                n: 3
            }
        );
        assert_eq!(out[5], ColumnGlyph::PerCell, "space");
        assert_eq!(out[6], ColumnGlyph::PerCell, "lone 'y' stays per-cell");
    }

    /// The `extract_cell_slice` tiles reassemble the wide raster byte-exactly, for left
    /// bearings of both signs.
    #[test]
    fn extract_cell_slice_reassembles_wide_raster() {
        let (raster_w, height, cell_w) = (23usize, 4usize, 8usize);
        // Distinct non-zero byte per pixel so a dropped/duplicated column shows.
        let raster: Vec<u8> = (0..raster_w * height)
            .map(|i| (i % 251 + 1) as u8)
            .collect();
        for &xmin in &[0i32, 3, 8, -2, -9] {
            // Cover the whole raster: cells must span [min(0,xmin), xmin+raster_w).
            let lo_cell = (xmin.min(0)).div_euclid(cell_w as i32);
            let hi = xmin + raster_w as i32;
            let n = ((hi - lo_cell * cell_w as i32) as usize).div_ceil(cell_w) + 1;
            // Rebuild the raster from the tiles: source column s lives in tile
            // k = (s + xmin).div_euclid(cell_w) at tile column (s + xmin) - k*cell_w.
            for row in 0..height {
                for s in 0..raster_w {
                    let dest = s as i64 + xmin as i64; // run-origin column
                    let k = dest.div_euclid(cell_w as i64);
                    let j = (dest - k * cell_w as i64) as usize;
                    let kk = (k - lo_cell as i64) as usize;
                    assert!(kk < n);
                    let tile =
                        extract_cell_slice(&raster, raster_w, height, xmin, cell_w, k as usize);
                    assert_eq!(tile.len(), cell_w * height);
                    assert_eq!(
                        tile[row * cell_w + j],
                        raster[row * raster_w + s],
                        "xmin={xmin} row={row} s={s} must round-trip through tile k={k}"
                    );
                }
            }
        }
        // Degenerate inputs return empty (caller keeps the per-cell path).
        assert!(extract_cell_slice(&raster, raster_w, height, 0, 0, 0).is_empty());
        assert!(extract_cell_slice(&raster, raster_w, 0, 0, cell_w, 0).is_empty());
        assert!(extract_cell_slice(&[], raster_w, height, 0, cell_w, 0).is_empty());
    }

    #[test]
    fn map_shaped_run_rejects_duplicate_cluster_collision() {
        fn glyph(glyph_id: u32, cluster: u32) -> GlyphInfo {
            let mut info = GlyphInfo::default();
            info.glyph_id = glyph_id;
            info.cluster = cluster;
            info
        }

        let bytes = include_bytes!("../tests/fixtures/jetbrains-mono.ttf");
        let face = aterm_font::Face::parse(bytes, 0).expect("fixture face");
        let run = "==";
        let run_chars = ['=', '='];

        // Positive control: distinct 1:1 clusters (0 and 1) map cleanly.
        let one_to_one = [glyph(900, 0), glyph(901, 1)];
        assert_eq!(
            map_shaped_run(&face, run, &run_chars, false, &one_to_one),
            Some(ShapedRun::PerColumn(
                vec![Some(900), Some(901)].into_boxed_slice()
            ))
        );

        // A 2-in/2-out run where both glyphs share cluster 0 (e.g. a 2->1
        // ligature followed by a 1->2 split) is not per-column 1:1 and must be
        // rejected to PerCell rather than overwriting column 0 and leaving
        // column 1 unpopulated.
        let colliding = [glyph(900, 0), glyph(901, 0)];
        assert_eq!(
            map_shaped_run(&face, run, &run_chars, false, &colliding),
            None
        );
    }
}
