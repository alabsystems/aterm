// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Underline, strikethrough, overline and undercurl geometry: the pure policy both
//! renderers share.
//!
//! Four shared policies live here:
//!
//! * **Font tables drive the bands.** [`resolve_deco_metrics`] turns the
//!   font's `post` underline and OS/2 strikeout tables (captured per-em at
//!   face build, [`DecoTables`]) into pixel bands, falling back to the
//!   historical `cell_h/15` heuristic when a table is absent — and CLAMPS the
//!   result into the cell for EVERY input (the in-cell invariant,
//!   `tests/deco_lines.rs::resolved_bands_always_inside_the_cell`). In-cell
//!   clamping is also what keeps the CPU per-row and GPU whole-frame
//!   decoration draw orders equivalent: a decoration can never bleed into a
//!   neighbouring row band.
//!
//! * **Pattern phase is a pure function of ABSOLUTE x.** The dotted / dashed /
//!   square-wave predicates ([`dotted_on`] / [`dashed_on`] / [`squarewave_up`])
//!   and the undercurl tile sampler ([`undercurl_tile_col`]) take the absolute
//!   framebuffer column, never a per-cell origin, so a pattern's value at a
//!   pixel is independent of HOW the run was partitioned into cells — the
//!   phase-continuity theorem (`tests/deco_lines.rs::
//!   pattern_rects_are_partition_invariant`). The historical code restarted
//!   the phase at every cell seam (dash/dot patterns reset per glyph, the
//!   curly wave per cell).
//!
//! * **Descender ink-skip only ERASES.** [`keep_spans_after_ink`] subtracts a
//!   1px-dilated glyph-ink column set from an underline span; it can only
//!   remove coverage, never add or recolour it (coverage-monotonicity,
//!   `tests/deco_lines.rs::ink_skip_*`), and a cell with no descender ink
//!   takes the identical code path as the feature turned off.
//!
//! * **…and only where descenders exist.** [`ink_skip_applies_to_width`] holds
//!   the skip to SINGLE-column cells. A double-width cell is an ideograph or a
//!   fullwidth form, scripts with no descenders whose feet merely rest on the
//!   baseline — carving around those turned an IME composition rule into a row
//!   of dashes under every character (`tests/deco_lines.rs::
//!   cjk_composition_underline_is_continuous`). Narrow cells are untouched by
//!   the rule, so the Latin behaviour the feature exists for is preserved by
//!   construction.

use aterm_core::terminal::UnderlineStyle;

/// Decoration metrics read at face build, as per-em fractions scaled by the live `px`.
/// `None` means the face lacks that entry and the heuristic applies.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct DecoTables {
    /// `post` underline as `(position_em, thickness_em)`: the centre of the stroke relative
    /// to the baseline, negative below (the FreeType convention).
    pub underline: Option<(f32, f32)>,
    /// OS/2 `yStrikeoutPosition`/`ySize` as `(position_em, thickness_em)`: the top of the
    /// stroke relative to the baseline, positive above.
    pub strikeout: Option<(f32, f32)>,
}

/// Resolved per-cell decoration bands, in cell-relative pixel rows.
///
/// For every input to [`resolve_deco_metrics`]: `1 <= underline_t <= cell_h`,
/// `underline_y + underline_t <= cell_h`, and the same for the strike band
/// (`tests/deco_lines.rs::resolved_bands_always_inside_the_cell`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecoMetrics {
    /// Top row of the single underline, relative to the cell top.
    pub underline_y: usize,
    /// Underline thickness in px (`>= 1`).
    pub underline_t: usize,
    /// Top row of the strikethrough stroke, relative to the cell top.
    pub strike_y: usize,
    /// Strikethrough thickness in px (`>= 1`).
    pub strike_t: usize,
}

/// Resolve the decoration bands for a `cell_h`-tall cell with `baseline` px of ascent, from
/// the font's tables or, without them, the `cell_h/15` heuristic. `adjust_pos` moves the
/// underline down (+) or up (−) and `adjust_thick` changes its thickness (the
/// `adjust_underline_position` / `adjust_underline_thickness` config keys). Clamping
/// in-cell comes last, so no input can push a band out of the cell.
#[must_use]
pub fn resolve_deco_metrics(
    cell_h: usize,
    baseline: i32,
    px: f32,
    tables: Option<DecoTables>,
    adjust_pos: i32,
    adjust_thick: i32,
) -> DecoMetrics {
    // `cell_h_baseline` floors at 1; guard anyway so the in-cell invariant is total.
    let ch = cell_h.max(1);
    let base = baseline.max(0) as usize;
    let legacy_t = (ch / 15).max(1);
    // Per-em thickness to px, floored at 1 so a thin face never yields an invisible line;
    // `as i64` saturates on non-finite input.
    let thick_px = |em: f32| ((em * px).round() as i64).max(1);
    // Underline: the table gives the stroke centre (negative below the baseline); the
    // fallback sits just below the baseline.
    let (uy, ut) = match tables.and_then(|t| t.underline) {
        Some((pos_em, th_em)) if th_em > 0.0 => {
            let t = thick_px(th_em);
            let center = baseline as f32 - pos_em * px;
            ((center - t as f32 / 2.0).round() as i64, t)
        }
        _ => ((base + legacy_t) as i64, legacy_t as i64),
    };
    let (underline_y, underline_t) = clamp_band(
        uy.saturating_add(i64::from(adjust_pos)),
        ut.saturating_add(i64::from(adjust_thick)),
        ch,
    );
    // Strikethrough: the table gives the stroke top above the baseline; the fallback is a
    // third of the ascent above it.
    let (sy, st) = match tables.and_then(|t| t.strikeout) {
        Some((pos_em, th_em)) if th_em > 0.0 => {
            let t = thick_px(th_em);
            ((baseline as f32 - pos_em * px).round() as i64, t)
        }
        _ => (
            base.saturating_sub((base / 3).max(1)) as i64,
            legacy_t as i64,
        ),
    };
    let (strike_y, strike_t) = clamp_band(sy, st, ch);
    DecoMetrics {
        underline_y,
        underline_t,
        strike_y,
        strike_t,
    }
}

/// Clamp a `(top, thickness)` band into the cell: thickness into `[1, cell_h]` first, then
/// top into `[0, cell_h - thickness]`, which guarantees `top + thickness <= cell_h`.
#[inline]
const fn clamp_band(top: i64, thickness: i64, cell_h: usize) -> (usize, usize) {
    const fn nonnegative(value: i64) -> usize {
        if value <= 0 {
            0
        } else if value as i128 > usize::MAX as i128 {
            usize::MAX
        } else {
            value as usize
        }
    }
    let thick_nonneg = nonnegative(thickness);
    let top_nonneg = nonnegative(top);
    let t = if thick_nonneg < 1 {
        1
    } else if thick_nonneg > cell_h {
        cell_h
    } else {
        thick_nonneg
    };
    let max_y = cell_h - t;
    let y = if top_nonneg < max_y {
        top_nonneg
    } else {
        max_y
    };
    (y, t)
}

/// Whether the dotted underline is on at absolute column `x`: square dots of side
/// `thickness` with equal gaps, phased from `x == 0`.
#[must_use]
#[inline]
pub const fn dotted_on(x: usize, thickness: usize) -> bool {
    let t = if thickness > 1 { thickness } else { 1 };
    (x / t).is_multiple_of(2)
}

/// Whether the dashed underline is on at absolute column `x`: ~2/3 duty over a
/// `max(cell_w/2, 2)` period, so a dash crosses cell seams unbroken.
#[must_use]
#[inline]
pub const fn dashed_on(x: usize, cell_w: usize) -> bool {
    let half = cell_w / 2;
    let p = if half > 2 { half } else { 2 };
    let div = (2 * p).div_ceil(3);
    let d = if div > 1 { div } else { 1 };
    x % p < d
}

/// Whether absolute column `x` is in the raised half of the square-wave curly fallback,
/// used when [`undercurl_supported`] is false.
#[must_use]
#[inline]
pub const fn squarewave_up(x: usize, cell_h: usize) -> bool {
    let sixth = cell_h / 6;
    let seg = if sixth > 2 { sixth } else { 2 };
    (x / seg) % 2 == 1
}

/// The deco atlas size cap both renderers share (the WebGL2 `max_texture_dimension_2d`);
/// the atlas packs [`DECO_ATLAS_SPRITES`] cell-wide sprites in one row.
pub const DECO_ATLAS_MAX_DIM: usize = 2048;

/// Sprites in the shared deco atlas: the 7 sparkle-word glyphs plus the undercurl tile
/// ([`UNDERCURL_SPRITE`]). aterm-gpu const-asserts its sprite table against this.
pub const DECO_ATLAS_SPRITES: usize = 8;

/// The undercurl's sprite slot in the shared deco atlas.
pub const UNDERCURL_SPRITE: usize = 7;

/// Whether the AA undercurl mask fits the shared deco atlas at this base cell size. When
/// `false`, both renderers draw the square-wave fallback.
#[must_use]
#[inline]
pub const fn undercurl_supported(cell_w: usize, cell_h: usize) -> bool {
    cell_w > 0
        && cell_h > 0
        && cell_w <= DECO_ATLAS_MAX_DIM / DECO_ATLAS_SPRITES
        && cell_h <= DECO_ATLAS_MAX_DIM
}

/// The cell-relative rows `[top, cell_h)` the undercurl wave fills: from the
/// resolved underline top to the cell bottom, raised — never past the cell
/// top — until it is at least `max(t + 3, 4)` rows tall for stroke `t`.
///
/// WHY THE FLOOR. The wave's amplitude is `(band_h - t)/2 - 0.5`, so a band of
/// `t + 1` rows or fewer has NO amplitude and the curly underline drew as a
/// straight one — LSP diagnostics and spell-check marks indistinguishable from
/// a plain underline. That was every size from 10 to 32 px for Ubuntu Mono
/// (its `post` underline sits 0.138 em below a 0.189 em descent: 16 px gives
/// cell 18, underline row 17, a one-row band), most sizes of Liberation Mono,
/// and the bundled JetBrains Mono at 13 and 14 px. `t + 3` rows is the
/// smallest band whose amplitude is a whole pixel for any `t`, and it lifts
/// the crest (which falls mid-cell, under the glyph) at most two rows above
/// the single underline. A roomy cell's band is unchanged.
///
/// The one band both renderers and the descender ink-skip probe use: the
/// CPU blend loop, the GPU's full-cell curl sprite (the tile carries no
/// coverage outside it) and [`underline_band`].
#[must_use]
#[inline]
pub const fn undercurl_band(cell_h: usize, deco: DecoMetrics) -> (usize, usize) {
    if cell_h == 0 {
        return (0, 0);
    }
    let t = if deco.underline_t < 1 {
        1
    } else if deco.underline_t > cell_h {
        cell_h
    } else {
        deco.underline_t
    };
    let floor = if t + 3 > 4 { t + 3 } else { 4 };
    let cap1 = cell_h - 1;
    let cap2 = cell_h.saturating_sub(floor);
    let mut top = deco.underline_y;
    if top > cap1 {
        top = cap1;
    }
    if top > cap2 {
        top = cap2;
    }
    (top, cell_h)
}

/// The anti-aliased cosine undercurl coverage tile for one BASE-width cell:
/// row-major `cell_w * cell_h` bytes of coverage `0..=255`, exactly ONE wave
/// period per tile so adjacent tiles continue the wave seamlessly. The wave
/// fills [`undercurl_band`], with the resolved underline thickness as stroke
/// width.
///
/// # Invariants (proven, `tests/deco_lines.rs`)
///
/// * **Period exactness** — the tile is sampled via [`undercurl_tile_col`],
///   which is `cw`-periodic in the absolute column BY CONSTRUCTION (integer
///   `%`), so the wave's value at a pixel is a pure function of absolute x.
/// * **Amplitude bounds** — every nonzero coverage byte lies within the
///   [`undercurl_band`] rows (`undercurl_stays_inside_its_band`): the
///   amplitude is derived as `(band_h - t)/2 - 0.5` so the stroke's AA
///   fringe never exits the band (and therefore never the cell).
/// * **It waves** — wherever the cell has `t + 3` rows, the amplitude is at
///   least one pixel (`undercurl_waves_however_low_the_font_puts_its_underline`).
#[must_use]
pub fn undercurl_coverage(cell_w: usize, cell_h: usize, deco: DecoMetrics) -> Vec<u8> {
    let mut buf = vec![0u8; cell_w * cell_h];
    if cell_w == 0 || cell_h == 0 {
        return buf;
    }
    let t = deco.underline_t.clamp(1, cell_h) as f32;
    let band_top = undercurl_band(cell_h, deco).0 as f32;
    let band_h = cell_h as f32 - band_top;
    let mid = band_top + band_h / 2.0;
    let amp = ((band_h - t) / 2.0 - 0.5).max(0.0);
    let half = t / 2.0 + 0.5;
    for x in 0..cell_w {
        let phase = std::f32::consts::TAU * ((x as f32 + 0.5) / cell_w as f32);
        let center = mid + amp * phase.cos();
        for y in 0..cell_h {
            let cov = (half - ((y as f32 + 0.5) - center).abs()).clamp(0.0, 1.0);
            if cov > 0.0 {
                buf[y * cell_w + x] = (cov * 255.0).round() as u8;
            }
        }
    }
    buf
}

/// The base-width undercurl tile column that absolute framebuffer column `x` samples, on a
/// row with cell advance `rcw` (2× `base_cw` on DEC double-width rows) and grid inset
/// `pad`. The wave tiles per advance and nearest-samples like the GPU atlas sampler, so CPU
/// and GPU read the same byte; no cell index enters, so the phase never resets at a seam.
#[must_use]
#[inline]
pub const fn undercurl_tile_col(x: usize, pad: usize, rcw: usize, base_cw: usize) -> usize {
    debug_assert!(x >= pad && rcw > 0 && base_cw > 0);
    let rcw_pos = if rcw > 1 { rcw } else { 1 };
    let bcw_pos = if base_cw > 1 { base_cw } else { 1 };
    let tx = (x - pad) % rcw_pos;
    ((((tx as f32) + 0.5) / rcw as f32) * base_cw as f32) as usize % bcw_pos
}

/// The cell-relative `[y0, y1)` rows an underline style occupies (the band descender
/// ink-skip probes), shared by both renderers; `None` for `UnderlineStyle::None`.
#[must_use]
#[inline]
pub const fn underline_band(
    style: UnderlineStyle,
    cell_h: usize,
    deco: DecoMetrics,
) -> Option<(usize, usize)> {
    let ch = if cell_h > 1 { cell_h } else { 1 };
    let t = if deco.underline_t < 1 {
        1
    } else if deco.underline_t > ch {
        ch
    } else {
        deco.underline_t
    };
    let max_uy = cell_h.saturating_sub(t);
    let uy = if deco.underline_y < max_uy {
        deco.underline_y
    } else {
        max_uy
    };
    match style {
        UnderlineStyle::None => None,
        UnderlineStyle::Single | UnderlineStyle::Dotted | UnderlineStyle::Dashed => {
            Some((uy, uy + t))
        }
        UnderlineStyle::Double => {
            let t2 = t.saturating_mul(2);
            let gap = if t2 > 2 { t2 } else { 2 };
            Some((uy.saturating_sub(gap), uy + t))
        }
        UnderlineStyle::Curly => Some(undercurl_band(cell_h, deco)),
    }
}

/// Whether descender ink-skip may carve the underline of a cell whose glyph occupies `cols`
/// columns: single-column cells only.
///
/// # The defect this closes (composition underlines under CJK)
///
/// The skip's subject is a descender TAIL: the narrow stroke of `g/y/j/p/q`
/// that pierces the rule, which the probe finds by looking at the row(s)
/// strictly below the baseline. An ideograph puts ink in those same rows for a
/// completely different reason — its foot AA-overshoots the baseline by a pixel
/// or two — and the probe cannot tell the two apart from one row of coverage.
///
/// Measured with the embedded face (`tests/deco_lines.rs::
/// cjk_composition_underline_is_continuous`), the lead cell of an IME-composed
/// `日` had its underline cut into THREE fragments at every size from 13px up
/// — a short dash at each margin and a stub between the character's two
/// vertical strokes (18px: kept spans `[(0,2), (7,7), (19,3)]` of a 22px
/// advance). A user composing Japanese saw that under every character, so the
/// composition rule — the ONLY thing marking the preedit as uncommitted — was
/// a dotted mess instead of a line.
///
/// Depth cannot separate the two cases at a fixed threshold: measured on the
/// embedded face, Latin tails reach 2 rows below the baseline at 12px and 6 at
/// 32px, while CJK feet reach 1 and 2 — the ranges overlap once you cross font
/// sizes, and any em-fraction cut sits on rasterizer hinting noise. Column
/// width does separate them, exactly and by definition.
///
/// # Why this cannot regress Latin
///
/// Latin, Greek, Cyrillic — every script with a descender — is narrow. This
/// predicate is FALSE only for cells the terminal itself measured at 2 columns,
/// so no narrow cell changes code path, and the descender behaviour the feature
/// exists for is byte-identical by construction.
///
/// # Known, deliberate limit
///
/// Fullwidth Latin (`ｇ`, U+FF47) is wide AND has a descender, so its tail is
/// no longer carved around. CJK faces draw those inside the em square, the
/// character is vanishingly rare in terminal text, and accepting one cosmetic
/// miss is the price of a rule that is provably inert for every narrow cell.
#[must_use]
#[inline]
pub const fn ink_skip_applies_to_width(cols: usize) -> bool {
    cols <= 1
}

/// Descender ink-skip: given per-column ink presence over a cell's underline span, push the
/// maximal `(start, len)` spans kept after removing every column within 1px of ink.
///
/// Kept spans are a subset of `[0, ink.len())` (the skip only erases), an all-false `ink`
/// yields the one full span, and column `i` is skipped iff `ink` holds at `i-1`, `i` or
/// `i+1` (`tests/deco_lines.rs`).
#[inline]
pub fn keep_spans_after_ink(ink: &[bool], out: &mut Vec<(usize, usize)>) {
    let w = ink.len();
    let skipped = |i: usize| ink[i] || (i > 0 && ink[i - 1]) || (i + 1 < w && ink[i + 1]);
    let mut x = 0;
    while x < w {
        if skipped(x) {
            x += 1;
        } else {
            let s = x;
            while x < w && !skipped(x) {
                x += 1;
            }
            out.push((s, x - s));
        }
    }
}

/// Intersect the rect's x-extent with each `(start, len)` span, pushing the surviving
/// `[x, y, w, h]` sub-rects: ink-skip applied to already-emitted underline rects.
#[inline]
pub fn intersect_rect_spans(out: &mut Vec<[usize; 4]>, rect: [usize; 4], spans: &[(usize, usize)]) {
    let [rx, ry, rw, rh] = rect;
    for &(sx, sw) in spans {
        let lo = rx.max(sx);
        let hi = (rx + rw).min(sx + sw);
        if lo < hi {
            out.push([lo, ry, hi - lo, rh]);
        }
    }
}
