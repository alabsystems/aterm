// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Programming-ligature run shaping for the terminal grid.
//!
//! aterm renders text strictly per-cell on a monospace cadence. Ligatures
//! (`=>`, `!=`, `===`, `->`, `<=`, …) need the OpenType `liga`/`calt` features:
//! `aterm_font::shape` shapes a RUN of adjacent same-style cells, and the font substitutes
//! the run's glyphs — for a monospace ligature font (JetBrains Mono / Fira Code)
//! the substitution keeps ONE glyph per input cell (each advance stays one cell),
//! turning the lead cells of a ligature into empty placeholder glyphs and the
//! final cell into the wide ligature glyph (whose negative left bearing overflows
//! back across the run). So a ligature draws on the SAME cells the characters
//! occupied — no cadence change, no cell consumed.
//!
//! This module is the SHARED shaping seam: both the CPU [`crate::Renderer`] row
//! painter and the GPU `encode_frame` consume the SAME per-cell plan (the same
//! [`crate::GlyphKey`] at the same column), so the CPU==GPU byte-identical invariant
//! holds. A run only ligates when the shaper actually changes the glyph ids;
//! otherwise the plan is identical to the plain per-cell path (byte-identical to
//! the pre-ligature renderer).
//!
//! A row aterm-core reordered for bidi reaches the renderer in VISUAL order,
//! with the permutation in `RenderInput::visual_to_logical`. [`plan_row_runs`]
//! reads that map: a stretch of right-to-left letters the reorder laid out
//! backwards is shaped in logical order and its glyphs placed back, so Arabic
//! joining and every `GSUB` rule read the letters as they were written;
//! everything else, and every row the reorder left alone (bidi off, the
//! web modules, a pure left-to-right row), is shaped as the row holds it
//! (TF.6 of docs/AUDIT-third-party-2026-10-15.md; rustybuzz shaped a
//! reordered Arabic run as if it were logical, which swapped `init` and
//! `fina`).

use std::borrow::Borrow;

use aterm_core::terminal::RenderCell;
use aterm_font::shape::{
    Direction, Feature, GlyphBuffer, GlyphInfo, ShapeFace, ShapePlan, UnicodeBuffer,
};
use aterm_grapheme::Script;
use aterm_types::text_shaping::FontFeature;

use crate::StyleBits;

/// Build the feature list applied to every ligature shaping run.
///
/// The base pair is the programming-ligature features `liga`/`calt`, set to `1`
/// when `ligatures_on` (the default), or `0` when ligatures are globally off
/// (`LigatureMode::Disabled`). Setting them to `0` lets a shaping run still apply
/// the user's OTHER features (e.g. `ss01`, `zero`) WITHOUT forming `=>`/`!=`
/// ligatures — the "slashed zero but no ligatures" combination. The caller's
/// OpenType `font_features` (`ss01`, `cv01`, `zero`, stylistic sets, …) are
/// appended AFTER the base pair. The shaper resolves overlapping features by
/// LAST-writer-wins for a given tag, so an explicit user `liga`/`calt` value still
/// overrides the base.
///
/// Every entry applies across the WHOLE shaped run (every cluster), matching how
/// the on/off `liga`/`calt` features are applied, so a [`FontFeature`] (tag and
/// value, no range) says all there is to say. The list is first-party on purpose:
/// the shaping engine is an implementation detail of this crate
/// (docs/AUDIT-third-party-2026-10-15.md, TF.2), and the engine's own feature
/// form is built from it in one place (`engine_features`).
///
/// HOT-PATH NOTE: this is built ONCE when the shaping config is resolved
/// ([`crate::Renderer::set_text_shaping`]) and stored on the renderer — it is
/// NOT called per row/run/cell. When `user` is empty the result is exactly the
/// two-element base list, so the empty-features path costs the same as before.
#[must_use]
pub fn build_feature_list(user: &[FontFeature], ligatures_on: bool) -> Vec<FontFeature> {
    let base = u32::from(ligatures_on);
    let mut features = Vec::with_capacity(2 + user.len());
    features.push(FontFeature::new(*b"liga", base));
    features.push(FontFeature::new(*b"calt", base));
    features.extend_from_slice(user);
    features
}

/// The shaping engine's form of a [`build_feature_list`] result: each feature
/// over the GLOBAL run range, in order (order is precedence, last writer wins).
/// Built once per shaping-config change, beside the first-party list it mirrors.
pub(crate) fn engine_features(features: &[FontFeature]) -> Vec<Feature> {
    features
        .iter()
        .map(|f| Feature::new(aterm_font::Tag::from_bytes(&f.tag), f.value))
        .collect()
}

/// Whether [`crate::Renderer::row_glyph_plan`] should run the shaping pass
/// for a row, vs short-circuit to an all-[`ColumnGlyph::PerCell`] plan. Factored out
/// as a PURE function so `tests/font_oracle.rs` asks the same gate the planner does.
///
/// Run shaping iff the primary-face bytes are retained AND there is OpenType work to
/// do: either the user configured `font_features` (`has_user_features`), OR ligatures
/// are on for a font that advertises `liga`/`calt`. When `has_user_features` is false
/// this reduces EXACTLY to the legacy gate (`rb_present && !globally_off && font_has`),
/// so the no-features path stays byte-identical.
#[must_use]
pub fn should_run_shaping(
    rb_present: bool,
    has_user_features: bool,
    ligatures_globally_off: bool,
    font_has_ligature_features: bool,
) -> bool {
    rb_present && (has_user_features || (!ligatures_globally_off && font_has_ligature_features))
}

/// The minimum run length the planner will attempt to shape: 1 when the user
/// configured `font_features` (a single cell can substitute — `zero` on a lone `0`,
/// `ss01` on one char), else 2 (a single cell cannot ligate). PURE; keeps the two
/// `min_run` derivation sites (planner gate + run gate) from drifting.
#[must_use]
pub fn shaping_min_run(has_user_features: bool) -> usize {
    if has_user_features { 1 } else { 2 }
}

/// The grid-mappability verdict for a shaping result: `n_out` output glyphs over
/// `n_in` input cells (M4 — ligature slicing).
///
/// aterm's grid renders on a strict monospace cadence, so a shaped run is only
/// usable if its glyphs map cleanly onto cells. There are exactly TWO admissible
/// forms; every other shape is rejected back to the per-cell path (byte-identical
/// to no-ligature):
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
    /// Not grid-mappable — use the per-cell path (byte-identical to no-ligature).
    Reject,
    /// One glyph per input cell (Fira/JetBrains spacer convention) — the existing
    /// 1:1 path, drawn as one [`ColumnGlyph::Ligated`] per column.
    OneToOne,
    /// `n_in` cells collapsed to ONE wide glyph (Cascadia) — rasterize once and
    /// slice into `n_in` per-cell tiles. Only when `admit_collapsed`.
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
#[must_use]
pub fn classify_shape(n_in: usize, n_out: usize, admit_collapsed: bool) -> ShapeVerdict {
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
/// `Ligated` carries the primary-face glyph id the shaper produced for this
/// column's cell within its run; the caller blits it as a [`crate::GlyphKey::mono_gid`]
/// at the column's monospace origin (the lead cells of a ligature get the empty
/// placeholder glyph, the final cell the wide ligature glyph). `PerCell` means
/// the column was not part of a ligated run — the caller uses its ordinary
/// per-cell glyph dispatch ([`crate::Renderer::resolve_cell_key`]), so it stays
/// byte-identical to the non-ligature path.
///
/// `LigatedSlice` (M4 — Cascadia N:1 merged ligatures) marks cell `k` of an
/// `n`-cell run that shaping COLLAPSED to a single wide glyph `gid`: the wide
/// glyph is rasterized once and this cell draws its per-cell TILE (slice `k`),
/// a CELL-LOCAL coverage bitmap (`cell_w` wide, left bearing 0) the caller keys
/// with [`crate::Renderer::ligature_slice_key`]. Because each slice is confined
/// to its own cell, per-cell foreground / selection / block-cursor recolour all
/// work over the ligature without spanning neighbours — unlike `Ligated`, whose
/// wide glyph overflows across the run's lead cells.
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

/// The result of shaping one coalesced run, returned by the [`plan_row_runs`]
/// `shape` closure (M4).
///
/// - `PerColumn` — the Fira/JetBrains 1:1 "spacer convention": one gid per input
///   cell, `Some(gid)` for a column whose glyph shaping CHANGED, `None` to keep it
///   on the per-cell path. This is the historical, byte-identical accept.
/// - `Collapsed` — the Cascadia "merged" N:1 convention: the whole run collapsed
///   to ONE wide glyph `gid` spanning `n` input cells. The planner expands it to
///   `n` [`ColumnGlyph::LigatedSlice`] columns; the raster path slices the wide
///   glyph into per-cell tiles. Only produced when `admit_collapsed` is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShapedRun {
    /// 1:1 per-column plan (spacer convention).
    PerColumn(Box<[Option<u16>]>),
    /// N:1 collapse: one wide `gid` over `n` cells (Cascadia merged ligatures).
    Collapsed { gid: u16, n: u16 },
}

/// The largest cell count an N:1 collapse is admitted for. A merged ligature
/// spanning more than this many cells is pathological (real programming
/// ligatures span 2–4 cells); beyond the cap the run falls back to the per-cell
/// path (never worse than today). The cap also keeps the slice index in the
/// child glyph-key encoding ([`crate::Renderer::ligature_slice_key`]) small.
pub const LIG_SLICE_MAX: usize = 64;

/// Extract cell `k`'s slice of a wide ligature raster into a CELL-LOCAL tile
/// (`cell_w × height`, left bearing 0), accounting for the wide glyph's left
/// bearing `xmin` (M4 — Cascadia N:1 slicing). Tile column `j` is source column
/// `k*cell_w + j - xmin` of the raster, or 0 where that falls outside
/// `[0, raster_w)` — so blitting the returned tile at the cell's dest column
/// reproduces the wide glyph's coverage over exactly that cell. Concatenating the
/// tiles for `k = 0..n` (with `n*cell_w >= xmin + raster_w`), dropping the leading
/// `xmin` pad, reproduces the raster exactly
/// (`extract_cell_slice_reassembles_wide_raster`).
///
/// Returns an empty `Vec` for a degenerate `cell_w == 0`, `height == 0`, or a
/// byte slice too short — the caller keeps the per-cell path rather than reading
/// out of bounds.
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

/// Whether `cell` is eligible to join a ligature shaping run.
///
/// A run is contiguous cells that are: drawable (not wide-continuation, not a
/// space, not a control char), NOT an explicit emoji/text-presentation cell,
/// NOT part of a shaped emoji cluster, and NOT image-covered. Spaces and controls BREAK the
/// run (so `a => b` shapes `=>` but not across the spaces); wide/emoji/image
/// cells route to their existing colour/wide paths untouched. The caller also
/// breaks on a STYLE change (bold/italic) and per-frame on the cursor/selection
/// columns so those stay per-cell and correct.
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
        && cell.script == aterm_core::terminal::Script::Normal
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

    /// Shape one run string and return its grid plan. Accepted ONLY when the
    /// shape yields one output glyph per input `char` ([`ShapedRun::PerColumn`])
    /// or, with `admit_collapsed`, one wide glyph for the whole run
    /// ([`ShapedRun::Collapsed`]), AND it changed a glyph vs the plain cmap (the
    /// `!changed` decline, byte-identical to the per-cell path). Each accepted
    /// glyph is placed at its column's monospace origin, so the grid cadence is
    /// preserved by PLACEMENT and the per-glyph advance is not consulted.
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

    /// Shape `run` as given, in LOGICAL order: [`plan_row_runs`] has already
    /// put a visual-order right-to-left stretch back the way it was written.
    /// Direction and script are guessed from the text, as rustybuzz did.
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
    // M4: classify the shape against the grid-mappable forms. `OneToOne`
    // (Fira/JetBrains spacer convention) is the historical accept, drawn as one
    // gid per column below. `Collapsed` (Cascadia N:1) is admitted ONLY when
    // `admit_collapsed`; the single output glyph is the wide ligature gid, which
    // the raster path slices into per-cell tiles ([`extract_cell_slice`]) — so
    // here we return the wide gid + cell count and let the planner expand it to
    // `n` [`ColumnGlyph::LigatedSlice`] columns. Any other count (partial
    // collapse / expansion) is `Reject`. With `admit_collapsed == false` this is
    // byte-identical to the legacy `infos.len() != run_chars.len()` decline.
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
    // Map each output glyph to its INPUT char by `cluster` (the byte offset we
    // pushed). For a per-char run on a monospace font the clusters are the char
    // boundaries in order; build a glyph id per char position.
    let n = run_chars.len();
    // ALL-ASCII FAST PATH: when the run's byte length equals its char count every
    // char is one byte, so the byte offset table would be exactly `0, 1, 2, …` and
    // the cluster IS the char index — `binary_search(&cluster)` returns
    // `Ok(cluster)` iff `cluster < n` and `Err` otherwise, which is precisely the
    // branch below. The ligature runs this path exists for (`=>`, `!=`, `->`, `::`)
    // are all-ASCII, so the table and its allocation are now only built for the
    // mixed-width fallback.
    let ascii = run.len() == n;
    // The table is STRICTLY INCREASING by construction (every entry is the
    // previous plus a non-zero `len_utf8`), so the lookup is a binary search,
    // not a linear `position` scan: the run length is bounded only by `cols`,
    // and a 200-column run of shapeable cells (a `---` separator, a base64
    // blob, one-line JSON) turned an O(n) probe per output glyph into O(n²)
    // per run on every ShapedRunCache miss — i.e. on every newly scrolled-in
    // line. Same unique `Ok(idx)` `position` returned, same `Err` → bail.
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
    // PER-COLUMN accept: a column is `Some(gid)` (drawn as the shaped primary glyph)
    // ONLY when shaping CHANGED its glyph vs the plain cmap glyph; otherwise it is
    // `None` and stays on the per-cell dispatch. This is what keeps an EFFECTIVE
    // feature/ligature on one cell from dragging its neighbours onto the
    // primary-by-glyph-id path:
    //   - a char the primary face LACKS maps to gid 0 == cmap(0) -> None -> the
    //     per-cell path routes it to the fallback face (no `.notdef` tofu);
    //   - a procedurally-rendered or simply non-substituted neighbour has
    //     gid == cmap -> None -> keeps its procedural/per-cell glyph;
    //   - a char the face DOES cover that shaping turned into `.notdef` is a
    //     ligature the face cannot draw (Monocraft's arrows):
    //     the whole run goes back to the per-cell path, never a box.
    // A ligature like `=>` legitimately changes BOTH cells (wide glyph + placeholder),
    // so both stay `Some`. If NO column changed there is nothing to draw specially —
    // return None so the whole run is byte-identical to the per-cell path.
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
/// `false` when there is no `GSUB` table or the bytes don't parse as a face.
#[must_use]
pub fn font_has_ligature_features(rb_bytes: &[u8]) -> bool {
    gsub_has(rb_bytes, |t| t == *b"liga" || t == *b"calt")
}

/// Whether the primary face's `GSUB` table advertises `tag` (e.g. `zero`, `ss01`,
/// `cv01`). A face with no `GSUB` (e.g. Apple's Menlo/Monaco) advertises
/// NOTHING, so a configured feature can never substitute a glyph: the shaper returns
/// the plain cmap ids and the run shaper's `!changed` decline keeps the run
/// per-cell. Used for the GUI's "font_features had no effect on this font" diagnostic
/// so a configured-but-unsupported feature is a warning, not a silent no-op.
#[must_use]
pub fn font_advertises_feature(rb_bytes: &[u8], tag: [u8; 4]) -> bool {
    gsub_has(rb_bytes, |t| t == tag)
}

/// Whether face 0's `GSUB` feature list holds a tag `hit` accepts. LINEAR: FeatureList
/// records are stored in arbitrary, not tag-sorted, order, so a binary `find` could
/// miss a present tag. Read through aterm-font, whose `GSUB` lists are the ones
/// the shaper shapes with (the TF.1 reader corpus pins them, and its
/// `ligature_gate` answer pins this gate).
fn gsub_has(rb_bytes: &[u8], hit: impl Fn([u8; 4]) -> bool) -> bool {
    aterm_font::Face::parse(rb_bytes, 0)
        .ok()
        .and_then(|face| face.tables().gsub)
        .is_some_and(|gsub| gsub.features.into_iter().any(|f| hit(f.tag.to_bytes())))
}

/// Build the per-column glyph plan for one row of `cells`.
///
/// `shapeable[c]` is whether column `c` may join a run (computed by the caller
/// from [`cell_is_shapeable`] PLUS any per-frame exclusion columns — cursor /
/// selection / `CursorDisabled` ligature mode). `run_boundary_before[c]` starts
/// a fresh run at `c` without excluding that cell; pane boundaries need this
/// distinction so a right pane beginning with `=>` can still ligate.
/// `style_of(c)` returns the cell's SGR style bits so a style change BREAKS the
/// run. `order` is the row's bidi visual→logical map
/// (`RenderInput::visual_to_logical`, empty for a row in logical order): a
/// coalesced run is shaped in pieces, a stretch of right-to-left letters the
/// map lays out backwards in logical order ([`logical_rtl_stretch`]) and the
/// rest as the row holds it. `shape(run, chars, style)` shapes one piece in
/// logical order (the caller caches it by that text) and returns a
/// [`ShapedRun`], or `None` if it did not ligate. The result is one
/// [`ColumnGlyph`] per column:
/// `Ligated` for cells inside a 1:1 ligated run, `LigatedSlice` for cells of a
/// collapsed (Cascadia N:1) run, `PerCell` everywhere else.
///
/// SHARED by the CPU and GPU renderers so both place the identical glyph at the
/// identical column — the byte-identical invariant.
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
    // The shaped run is only READ into the column plan here and then dropped, so the
    // closure may hand back anything that borrows as a `ShapedRun` — a bare owned
    // `ShapedRun` (tests) or an `Arc<ShapedRun>` (the renderer's memo, whose cache
    // hits are then a refcount bump, not a deep clone of the boxed gid slice).
    F: FnMut(&str, &[char], StyleBits) -> Option<R>,
    R: Borrow<ShapedRun>,
{
    out.clear();
    // A 268M-column terminal is physically impossible; this dominating guard bounds
    // the resize allocation (and every `start + i < cols` write below).
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
        // Coalesce a maximal run of shapeable cells with the SAME style.
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

/// Whether the cells from `start` (before `end`) open a stretch of
/// right-to-left letters (bidi class R or AL) that `order` lays out
/// backwards — each painted one column left of its logical successor — and
/// if so where the stretch ends. Only such a stretch is reversed for
/// shaping: the reorder put it there, and joining reads it the other way.
/// Neutrals at a right-to-left level (an operator between two Hebrew words)
/// and left-to-right text keep the order the grid shows, which is the order
/// a programming ligature's one-glyph-per-cell convention is drawn in. An
/// empty `order` (a row the reorder left alone) or an ascending one (a
/// logical-order pane composed beside a reordered one) has no such stretch.
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
        return; // too short for the active mode; stays PerCell. (min_run is
        // 2 for plain ligatures, 1 when user features can act on one cell)
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
                // Only columns shaping actually CHANGED become Ligated; the rest
                // stay PerCell so procedural/fallback cells in the run keep their
                // own face. `shape` is a caller-supplied closure; an oversized
                // return must NOT index out of bounds. `column` and get_mut make
                // a bad gid array a no-op instead of a panic.
                if let Some(gid) = *slot
                    && let Some(cell) = column(i).and_then(|col| out.get_mut(col))
                {
                    *cell = ColumnGlyph::Ligated(gid);
                }
            }
        }
        Some(&ShapedRun::Collapsed { gid, n }) => {
            // Cascadia N:1: the wide glyph `gid` spans `n` cells. Mark each cell
            // of the run as slice `k`; the raster path draws the wide glyph's
            // per-cell tile at each column (`extract_cell_slice`). `n` is the
            // shaped run's cell count, so `[start, start+n)` is exactly this
            // piece; the wide glyph is drawn left to right whichever order its
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
                        // The same buffer `shape_buffer` builds, shaped with a
                        // plan built for it alone: the run as given, in
                        // logical order, its direction guessed.
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

    /// The engine form is the first-party list entry for entry, in order (order
    /// is precedence); the shaper applies every one over the whole run.
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

    /// A plain single-`char` cell (the kind a `=>` operator occupies). All
    /// rendition flags off so it is shapeable by default.
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

    /// ITEM C — a ligature run must BREAK across an image cell. Row `= > [img] = >`:
    /// the image cell (column 2) is image-covered, so the two `=>` operators sit on
    /// OPPOSITE sides of it. With a shaping closure that ligates any 2-char `=>` run,
    /// each side ligates independently (columns 0..=1 and 3..=4) and the image
    /// column stays `PerCell` — proving no single run spanned the image cell.
    #[test]
    fn ligature_run_breaks_on_image_cell() {
        let cells = [cell('='), cell('>'), cell('='), cell('='), cell('>')];
        // image_covers is true only for column 2 (the middle operator-shaped cell).
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
        // Columns 0..=1: the first '=>' ligated. Column 2: image cell, PerCell.
        // Columns 3..=4: the second '=>' ligated. If a run had spanned the image
        // cell, the planner would have tried to shape "=>==>" (which our closure
        // declines), leaving EVERYTHING PerCell — the asserts below would fail.
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

    /// A pane edge is a boundary BETWEEN runs, not a request to exclude the
    /// right-hand endpoint. The right pane may legitimately begin with a
    /// programming ligature; its first `=` must remain shapeable with `>`.
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

    /// TF.6: a stretch of right-to-left letters the bidi reorder painted
    /// backwards is shaped in logical order, and each glyph goes back to the
    /// column its letter is painted at; the left-to-right operator after it is
    /// shaped as the row holds it. Visual `ابحرم==` is logical `مرحبا==`.
    #[test]
    fn a_backwards_rtl_stretch_is_shaped_in_logical_order() {
        let visual = "\u{627}\u{628}\u{62D}\u{631}\u{645}==";
        let (seen, out) = plan_pieces(visual, &[4, 3, 2, 1, 0, 5, 6]);
        assert_eq!(seen, ["\u{645}\u{631}\u{62D}\u{628}\u{627}", "=="]);
        assert_eq!(out, ligated(&[104, 103, 102, 101, 100, 100, 101]));
        // NEGATIVE CONTROL: the same cells in logical order (no map, or the
        // identity a logical-order pane is composed with) are one piece,
        // shaped as the row holds them.
        for order in [&[][..], &[0, 1, 2, 3, 4, 5, 6]] {
            let (seen, out) = plan_pieces(visual, order);
            assert_eq!(seen, [visual], "{order:?}");
            assert_eq!(out, ligated(&[100, 101, 102, 103, 104, 105, 106]));
        }
    }

    /// Neutrals at a right-to-left level keep the order the grid shows: in
    /// logical `אב==גד` the operator sits between two Hebrew words, so the
    /// reorder paints the whole token backwards (`דג==בא`), but only the
    /// letters are shaped reversed. A programming ligature's spacer and wide
    /// glyph are drawn left to right, as rustybuzz shaped them.
    #[test]
    fn rtl_level_neutrals_keep_the_painted_order() {
        let (seen, out) = plan_pieces("\u{5D3}\u{5D2}==\u{5D1}\u{5D0}", &[5, 4, 3, 2, 1, 0]);
        assert_eq!(seen, ["\u{5D2}\u{5D3}", "==", "\u{5D0}\u{5D1}"]);
        assert_eq!(out, ligated(&[101, 100, 100, 101, 101, 100]));
    }

    /// M4 — a `Collapsed` shape result expands to `n` `LigatedSlice` columns over
    /// the run, each carrying the wide gid and its cell index `k`. The columns
    /// OUTSIDE the run stay `PerCell` (byte-identical to no-ligature). This is the
    /// planner half of the Cascadia N:1 path (the raster half is
    /// `extract_cell_slice`).
    #[test]
    fn plan_row_runs_expands_collapsed_to_slices() {
        // Row: `x <=> y` with the operator run `<=>` (cols 2..=4) collapsing to
        // one wide glyph. `x`/`y` are shapeable single cells (min_run 2 keeps them
        // per-cell); the spaces break the run.
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
        // Collapse ANY 3-char run to wide gid 700 over 3 cells.
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

    /// M4 — `extract_cell_slice` reassembles the wide raster byte-exactly. For a
    /// raster `raster_w` wide with left bearing `xmin`, concatenating the tiles for
    /// `k = 0..n` (each `cell_w` wide) and reading back the columns that map to
    /// `[0, raster_w)` reproduces every source byte. Non-zero `xmin` (both signs)
    /// shifts which cell each source column lands in — the tile placement law the
    /// blitter relies on.
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
