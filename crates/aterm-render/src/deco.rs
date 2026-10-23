// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Underline, strikethrough, overline and undercurl geometry.

use aterm_core::terminal::UnderlineStyle;

/// Decoration metrics read at face build, as per-em fractions scaled by the live `px`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct DecoTables {
    /// `post` underline as `(position_em, thickness_em)`.
    pub underline: Option<(f32, f32)>,
    /// OS/2 `yStrikeoutPosition`/`ySize` as `(position_em, thickness_em)`.
    pub strikeout: Option<(f32, f32)>,
}

/// Resolved per-cell decoration bands, in cell-relative pixel rows.
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

/// Resolve decoration bands for a `cell_h`-tall cell with `baseline` ascent from font tables or the `cell_h/15` fallback.
#[must_use]
pub fn resolve_deco_metrics(
    cell_h: usize,
    baseline: i32,
    px: f32,
    tables: Option<DecoTables>,
    adjust_pos: i32,
    adjust_thick: i32,
) -> DecoMetrics {
    let ch = cell_h.max(1);
    let base = baseline.max(0) as usize;
    let legacy_t = (ch / 15).max(1);
    let thick_px = |em: f32| ((em * px).round() as i64).max(1);
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

/// Whether the dotted underline is on at absolute column `x`.
#[must_use]
#[inline]
pub const fn dotted_on(x: usize, thickness: usize) -> bool {
    let t = if thickness > 1 { thickness } else { 1 };
    (x / t).is_multiple_of(2)
}

/// Whether the dashed underline is on at absolute column `x`.
#[must_use]
#[inline]
pub const fn dashed_on(x: usize, cell_w: usize) -> bool {
    let half = cell_w / 2;
    let p = if half > 2 { half } else { 2 };
    let div = (2 * p).div_ceil(3);
    let d = if div > 1 { div } else { 1 };
    x % p < d
}

/// Whether absolute column `x` is in the raised half of the square-wave curly fallback, used when [`undercurl_supported`] is false.
#[must_use]
#[inline]
pub const fn squarewave_up(x: usize, cell_h: usize) -> bool {
    let sixth = cell_h / 6;
    let seg = if sixth > 2 { sixth } else { 2 };
    (x / seg) % 2 == 1
}

/// The deco atlas size cap both renderers share (the WebGL2 `max_texture_dimension_2d`).
pub const DECO_ATLAS_MAX_DIM: usize = 2048;

/// Sprites in the shared deco atlas.
pub const DECO_ATLAS_SPRITES: usize = 8;

/// The undercurl's sprite slot in the shared deco atlas.
pub const UNDERCURL_SPRITE: usize = 7;

/// Whether the AA undercurl mask fits the shared deco atlas at this base cell size.
#[must_use]
#[inline]
pub const fn undercurl_supported(cell_w: usize, cell_h: usize) -> bool {
    cell_w > 0
        && cell_h > 0
        && cell_w <= DECO_ATLAS_MAX_DIM / DECO_ATLAS_SPRITES
        && cell_h <= DECO_ATLAS_MAX_DIM
}

/// The cell-relative rows `[top, cell_h)` the undercurl wave fills.
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

/// The anti-aliased cosine undercurl coverage tile for one BASE-width cell.
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

/// The base-width undercurl tile column sampled at framebuffer column `x` for advance `rcw` and inset `pad`.
#[must_use]
#[inline]
pub const fn undercurl_tile_col(x: usize, pad: usize, rcw: usize, base_cw: usize) -> usize {
    debug_assert!(x >= pad && rcw > 0 && base_cw > 0);
    let rcw_pos = if rcw > 1 { rcw } else { 1 };
    let bcw_pos = if base_cw > 1 { base_cw } else { 1 };
    let tx = (x - pad) % rcw_pos;
    ((((tx as f32) + 0.5) / rcw as f32) * base_cw as f32) as usize % bcw_pos
}

/// The cell-relative `[y0, y1)` rows an underline style occupies (the band descender ink-skip probes), shared by both renderers.
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

/// Whether descender ink-skip may carve the underline of a cell whose glyph occupies `cols` columns.
#[must_use]
#[inline]
pub const fn ink_skip_applies_to_width(cols: usize) -> bool {
    cols <= 1
}

/// Descender ink-skip.
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

/// Intersect the rect's x-extent with each `(start, len)` span, pushing the surviving `[x, y, w, h]` sub-rects.
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
