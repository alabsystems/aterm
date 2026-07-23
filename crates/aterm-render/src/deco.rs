// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Font-metric line decorations (W7): the PURE policy layer behind underline /
//! strikethrough / overline / undercurl geometry.
//!
//! Three root fixes live here, each stated as a proven invariant:
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
//!   pattern_rects_are_partition_invariant`, Tier-1 of the `deco_phase`
//!   derived ty model). The historical code restarted the phase at every cell
//!   seam (dash/dot patterns reset per glyph, the curly wave per cell).
//!
//! * **Descender ink-skip only ERASES.** [`keep_spans_after_ink`] subtracts a
//!   1px-dilated glyph-ink column set from an underline span; it can only
//!   remove coverage, never add or recolour it (coverage-monotonicity,
//!   `tests/deco_lines.rs::ink_skip_*`), and a cell with no descender ink
//!   takes the identical code path as the feature turned off.

use aterm_core::terminal::UnderlineStyle;

/// Decoration metrics read from a font at face build, as PER-EM fractions
/// (scaled by the live `px` at every derivation, like the renderer's
/// `TypoLineMetrics`). `None` fields mean the face lacks that table entry —
/// the resolver then uses the historical heuristic.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct DecoTables {
    /// `post` table underline as `(position_em, thickness_em)`. Position
    /// follows the FreeType convention: the CENTER of the stroke relative to
    /// the baseline, negative below it.
    pub underline: Option<(f32, f32)>,
    /// OS/2 `yStrikeoutPosition`/`ySize` as `(position_em, thickness_em)`.
    /// Position is the TOP of the stroke relative to the baseline, positive
    /// above it (the OpenType OS/2 definition).
    pub strikeout: Option<(f32, f32)>,
}

/// The RESOLVED per-cell decoration bands, in cell-relative pixel rows.
///
/// # Invariant (proven)
///
/// For every input to [`resolve_deco_metrics`] (any `cell_h >= 1`, any
/// baseline, any table values, any adjust deltas):
/// `1 <= underline_t <= cell_h`, `underline_y + underline_t <= cell_h`, and
/// the same for the strike band — decorations can NEVER leave their cell.
/// Machine-checked over an adversarial lattice by
/// `tests/deco_lines.rs::resolved_bands_always_inside_the_cell`.
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

/// Resolve the pixel decoration bands for a `cell_h`-tall cell with `baseline`
/// px of ascent, from the font's tables (when present) or the historical
/// heuristic (when absent — byte-identical to the pre-W7 hardcoded bands).
/// `adjust_pos` shifts the underline down (+) / up (−) and `adjust_thick`
/// fattens/thins it — the `adjust_underline_position` /
/// `adjust_underline_thickness` config escape hatches. Everything is clamped
/// in-cell LAST, so no input can push a band out of the cell (see
/// [`DecoMetrics`]).
#[must_use]
pub fn resolve_deco_metrics(
    cell_h: usize,
    baseline: i32,
    px: f32,
    tables: Option<DecoTables>,
    adjust_pos: i32,
    adjust_thick: i32,
) -> DecoMetrics {
    // The renderer guarantees cell_h >= 1 (`cell_h_baseline` floors at 1);
    // guard anyway so the in-cell invariant is total.
    let ch = cell_h.max(1);
    let base = baseline.max(0) as usize;
    let legacy_t = (ch / 15).max(1);
    // Scale a per-em thickness to px: round to nearest, floor at 1 so a thin
    // face never yields an invisible line. `as i64` saturates on non-finite /
    // huge floats, keeping the arithmetic total.
    let thick_px = |em: f32| ((em * px).round() as i64).max(1);
    // Underline: table position is the stroke CENTER relative to the baseline
    // (negative below); heuristic is "a hair below the baseline" (legacy).
    let (uy, ut) = match tables.and_then(|t| t.underline) {
        Some((pos_em, th_em)) if th_em > 0.0 => {
            let t = thick_px(th_em);
            let center = baseline as f32 - pos_em * px;
            ((center - t as f32 / 2.0).round() as i64, t)
        }
        _ => ((base + legacy_t) as i64, legacy_t as i64),
    };
    let (underline_y, underline_t) =
        clamp_band(uy + adjust_pos as i64, ut + adjust_thick as i64, ch);
    // Strikethrough: table position is the stroke TOP relative to the baseline
    // (positive above); heuristic is a third of the ascent above the baseline.
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

/// Clamp a `(top, thickness)` band into a `cell_h`-tall cell: thickness into
/// `[1, cell_h]` first, then the top into `[0, cell_h - thickness]` — the
/// order that guarantees `top + thickness <= cell_h` for ALL inputs.
fn clamp_band(top: i64, thickness: i64, cell_h: usize) -> (usize, usize) {
    let t = thickness.clamp(1, cell_h as i64) as usize;
    let y = top.clamp(0, (cell_h - t) as i64) as usize;
    (y, t)
}

/// Whether the DOTTED underline pattern is ON at absolute column `x`: square
/// dots of side `thickness` separated by equal gaps, phased from `x == 0` —
/// a pure function of `x`, so the pattern never resets at a cell seam.
#[must_use]
pub fn dotted_on(x: usize, thickness: usize) -> bool {
    let t = thickness.max(1);
    (x / t).is_multiple_of(2)
}

/// Whether the DASHED underline pattern is ON at absolute column `x`: dashes
/// of ~2/3 duty over a `max(cell_w/2, 2)` period, phased from `x == 0` — a
/// pure function of `x` (given the row's cell advance), so a dash crosses
/// cell seams unbroken.
#[must_use]
pub fn dashed_on(x: usize, cell_w: usize) -> bool {
    let p = (cell_w / 2).max(2);
    let d = (2 * p).div_ceil(3).max(1);
    x % p < d
}

/// Which half-phase of the LEGACY square-wave curly fallback absolute column
/// `x` is in (`true` = raised segment). Only used when the AA undercurl mask
/// is unsupported at this cell size ([`undercurl_supported`]); phased from
/// `x == 0` like every other pattern.
#[must_use]
pub fn squarewave_up(x: usize, cell_h: usize) -> bool {
    let seg = (cell_h / 6).max(2);
    (x / seg) % 2 == 1
}

/// Group the columns of `[x0, x0 + w)` where `on(x)` holds into maximal
/// `(start, len)` spans. Because `on` is a pure function of the ABSOLUTE
/// column, the union of spans over any partition of a run equals the spans
/// over the whole run (the phase-continuity theorem; proven in
/// `tests/deco_lines.rs`).
pub fn pattern_spans_into(
    out: &mut Vec<(usize, usize)>,
    x0: usize,
    w: usize,
    on: impl Fn(usize) -> bool,
) {
    let mut x = x0;
    let end = x0 + w;
    while x < end {
        if on(x) {
            let s = x;
            while x < end && on(x) {
                x += 1;
            }
            out.push((s, x - s));
        } else {
            x += 1;
        }
    }
}

/// The maximum deco-atlas texture dimension both renderers agree on (the
/// downlevel/WebGL2 `max_texture_dimension_2d`). The GPU sprite atlas packs
/// [`DECO_ATLAS_SPRITES`] cell-wide sprites in one row.
pub const DECO_ATLAS_MAX_DIM: usize = 2048;

/// Number of sprites in the shared deco atlas: the 8 sparkle-word glyphs plus
/// the undercurl tile (slot [`UNDERCURL_SPRITE`]). The GPU const-asserts its
/// sprite table against this so the layouts can never drift.
pub const DECO_ATLAS_SPRITES: usize = 9;

/// The undercurl's sprite slot in the shared deco atlas.
pub const UNDERCURL_SPRITE: usize = 8;

/// Whether the AA undercurl mask path is usable at this BASE cell size — i.e.
/// the shared deco atlas fits the GPU's texture-size cap. When `false`, BOTH
/// renderers fall back to the legacy square-wave rects (shared predicate, so
/// CPU/GPU can never disagree on which path draws).
#[must_use]
pub fn undercurl_supported(cell_w: usize, cell_h: usize) -> bool {
    cell_w > 0
        && cell_h > 0
        && DECO_ATLAS_SPRITES * cell_w <= DECO_ATLAS_MAX_DIM
        && cell_h <= DECO_ATLAS_MAX_DIM
}

/// The anti-aliased cosine undercurl coverage tile for one BASE-width cell:
/// row-major `cell_w * cell_h` bytes of coverage `0..=255`, exactly ONE wave
/// period per tile so adjacent tiles continue the wave seamlessly. The wave
/// fills the band from the resolved underline top to the cell bottom, with
/// the resolved underline thickness as stroke width.
///
/// # Invariants (proven, `tests/deco_lines.rs`)
///
/// * **Period exactness** — the tile is sampled via [`undercurl_tile_col`],
///   which is `cw`-periodic in the absolute column BY CONSTRUCTION (integer
///   `%`), so the wave's value at a pixel is a pure function of absolute x.
/// * **Amplitude bounds** — every nonzero coverage byte lies within the
///   `[underline_y, cell_h)` band rows (`undercurl_stays_inside_its_band`):
///   the amplitude is derived as `(band_h - t)/2 - 0.5` so the stroke's AA
///   fringe never exits the band (and therefore never the cell).
#[must_use]
pub fn undercurl_coverage(cell_w: usize, cell_h: usize, deco: DecoMetrics) -> Vec<u8> {
    let mut buf = vec![0u8; cell_w * cell_h];
    if cell_w == 0 || cell_h == 0 {
        return buf;
    }
    let t = deco.underline_t.clamp(1, cell_h) as f32;
    let band_top = deco.underline_y.min(cell_h - 1) as f32;
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

/// The BASE-width mask column an absolute framebuffer column `x` samples from
/// an undercurl tile, on a row whose on-screen cell advance is `rcw` (2× the
/// base `cell_w` on DEC double-width rows) with the grid inset by `pad`.
///
/// The wave TILES per cell advance (`(x - pad) % rcw`) and NEAREST-samples the
/// base-width sprite exactly like the GPU atlas sampler (and the sparkle-word
/// precedent), so CPU and GPU read the same coverage byte. Pure in the
/// ABSOLUTE column: the cell index never appears, so the wave's phase cannot
/// reset at a seam — and it is `rcw`-periodic by integer `%` (period
/// exactness, proven in `tests/deco_lines.rs`).
#[must_use]
pub fn undercurl_tile_col(x: usize, pad: usize, rcw: usize, base_cw: usize) -> usize {
    debug_assert!(x >= pad && rcw > 0 && base_cw > 0);
    let tx = (x - pad) % rcw.max(1);
    ((((tx as f32) + 0.5) / rcw as f32) * base_cw as f32) as usize % base_cw.max(1)
}

/// The cell-relative `[y0, y1)` row band an underline STYLE occupies, from the
/// resolved metrics — the rows descender ink-skip probes for glyph ink. `None`
/// for `UnderlineStyle::None`. Shared by both renderers so the skip band can
/// never diverge.
#[must_use]
pub fn underline_band(
    style: UnderlineStyle,
    cell_h: usize,
    deco: DecoMetrics,
) -> Option<(usize, usize)> {
    let t = deco.underline_t.clamp(1, cell_h.max(1));
    let uy = deco.underline_y.min(cell_h.saturating_sub(t));
    match style {
        UnderlineStyle::None => None,
        UnderlineStyle::Single | UnderlineStyle::Dotted | UnderlineStyle::Dashed => {
            Some((uy, uy + t))
        }
        UnderlineStyle::Double => {
            let gap = (2 * t).max(2);
            Some((uy.saturating_sub(gap), uy + t))
        }
        UnderlineStyle::Curly => Some((uy, cell_h)),
    }
}

/// Descender ink-skip core: given per-column glyph-ink presence over a cell's
/// underline span (`ink[i]` = ink in the probed band at column `i`), push the
/// maximal `(start, len)` spans of columns KEPT after removing every column
/// within a 1px dilation of ink.
///
/// # Invariants (proven, `tests/deco_lines.rs`)
///
/// * **Coverage-monotone** — kept spans are a SUBSET of `[0, ink.len())`: the
///   skip can only zero underline coverage, never add it.
/// * **No-ink identity** — an all-false `ink` yields the single full span, so
///   a cell with no descender ink renders byte-identically to the feature
///   being off.
/// * **Dilation exactness** — column `i` is skipped iff `ink` holds at
///   `i-1`, `i`, or `i+1`.
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

/// Intersect the rect `[rx, rx + rw)` (x-extent) with each `(start, len)` span,
/// pushing the surviving `[x, y, w, h]` sub-rects. Used to apply descender
/// ink-skip to already-emitted underline rects: output coverage ⊆ input
/// coverage by construction.
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
