// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Centralized color resolution for terminal cells.

use crate::grid::{Cell, CellExtra, CellFlags, PackedColors};
use aterm_types::{ColorPalette, DIM_FACTOR, Rgb};
use std::sync::OnceLock;

/// Host-configurable style-attribute policy for color resolution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyleResolveOpts {
    /// SGR 1 promotes indexed 0–7 to bright 8–15 (default `true`).
    pub bold_is_bright: bool,
    /// Fraction of foreground retained by SGR 2, blended toward background (default `0.5`).
    pub faint_opacity: f32,
    /// Theme-pair contrast floor in `1.0..=21.0` (default `1.0`, off).
    pub indexed_minimum_contrast: f32,
}

impl Default for StyleResolveOpts {
    #[inline]
    fn default() -> Self {
        Self {
            bold_is_bright: true,
            faint_opacity: DIM_FACTOR,
            indexed_minimum_contrast: 1.0,
        }
    }
}

/// Maximum theme-pair contrast floor requested against the theme's default background (`3:1`).
pub const THEME_BACKGROUND_FLOOR: f32 = 3.0;

/// Maximum theme-pair contrast floor requested for default palette chromatic text (`4.5:1`).
pub const DEFAULT_PALETTE_TEXT_FLOOR: f32 = 4.5;

/// What the theme-pair floor asks of one cell ([`theme_pair_floor`]).
#[derive(Debug, Clone, Copy, PartialEq)]
struct PairFloor {
    /// The WCAG ratio the drawn foreground is held to.
    min: f32,
    /// The cell is drawn on the page (the theme's own background): a rescue
    /// by the smallest nudge ([`floor_contrast`]), which keeps a muted slot
    /// muted.
    on_page: bool,
}

impl PairFloor {
    /// `fg` floored against `bg`; `inks` are the theme's default foreground
    /// and background.
    #[inline]
    fn apply(self, fg: Rgb, bg: Rgb, inks: [Rgb; 2]) -> Rgb {
        if self.on_page {
            floor_contrast(fg, bg, self.min)
        } else {
            floor_contrast_on_colour(fg, bg, self.min, inks)
        }
    }
}

/// Whether `ch` is a shape drawn edge to edge rather than a letter.
#[must_use]
#[inline]
pub(crate) const fn is_shape_glyph(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{259F}' | '\u{E0A0}'..='\u{E0D7}')
}

fn theme_pair_floor(
    cell: Cell,
    colors: PackedColors,
    reverse_video: bool,
    ratio: f32,
    palette: &ColorPalette,
) -> Option<PairFloor> {
    if ratio.is_nan() || ratio <= 1.0 || cell.flags().contains(CellFlags::HIDDEN) {
        return None;
    }
    let fg_theme = colors.fg_is_default() || (colors.fg_is_indexed() && colors.fg_index() < 16);
    let bg_theme = colors.bg_is_default() || (colors.bg_is_indexed() && colors.bg_index() < 16);
    if !fg_theme || !bg_theme || (colors.fg_is_default() && colors.bg_is_default()) {
        return None;
    }
    if colors.fg_is_indexed() && colors.bg_is_indexed() && colors.fg_index() == colors.bg_index() {
        return None;
    }
    if !cell.is_complex() && is_shape_glyph(cell.char()) {
        return None;
    }
    let drawn_on_page = !cell.flags().contains(CellFlags::INVERSE)
        && if reverse_video {
            colors.fg_is_default()
        } else {
            colors.bg_is_default()
        };
    if !drawn_on_page {
        return Some(PairFloor {
            min: ratio.min(21.0),
            on_page: false,
        });
    }
    let (ink_indexed, ink) = if reverse_video {
        (colors.bg_is_indexed(), colors.bg_index())
    } else {
        (colors.fg_is_indexed(), colors.fg_index())
    };
    let aterm_own = ink_indexed
        && (1..=6).contains(&ink)
        && palette.get(ink) == ColorPalette::default_color(ink);
    let cap = if aterm_own {
        DEFAULT_PALETTE_TEXT_FLOOR
    } else {
        THEME_BACKGROUND_FLOOR
    };
    Some(PairFloor {
        min: ratio.min(cap),
        on_page: true,
    })
}

/// When the theme-pair floor is active on a light theme whose white slots
/// (7, 15) are dark text colors (darker than `page` and at least
/// [`THEME_BACKGROUND_FLOOR`] from it), substitute `page` on any non-page
/// colored cell where `page` yields higher contrast.
#[allow(
    clippy::too_many_arguments,
    reason = "the drawn pair, its slots and the page, all already in hand at both call sites"
)]
fn white_on_colour(
    cell: Cell,
    ratio: f32,
    slot: Option<u8>,
    ground_slot: Option<u8>,
    white: Rgb,
    ground: Rgb,
    page: Rgb,
) -> Rgb {
    if !matches!(slot, Some(7 | 15))
        || ground_slot == slot
        || ratio.is_nan()
        || ratio <= 1.0
        || cell.flags().contains(CellFlags::HIDDEN)
        || (!cell.is_complex() && is_shape_glyph(cell.char()))
    {
        return white;
    }
    let (lw, lp, lg) = (
        wcag_luminance(white),
        wcag_luminance(page),
        wcag_luminance(ground),
    );
    let dark_white = lw < lp && wcag_contrast(lw, lp) >= THEME_BACKGROUND_FLOOR;
    if dark_white && wcag_contrast(lp, lg) > wcag_contrast(lw, lg) {
        page
    } else {
        white
    }
}

/// Floor `fg`'s WCAG contrast against `bg` to `min`.
#[must_use]
pub fn floor_contrast(fg: Rgb, bg: Rgb, min: f32) -> Rgb {
    let lb = wcag_luminance(bg);
    if wcag_contrast(wcag_luminance(fg), lb) >= min {
        return fg;
    }
    let pole = pole_of(lb);
    first_clearing_tenth(fg, pole, lb, min).map_or_else(|| grey(pole), |(_, nudged)| nudged)
}

/// The WCAG contrast ratio of `a` against `b` (1 for equal inks, 21 for black on white).
#[must_use]
#[inline]
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    wcag_contrast(wcag_luminance(a), wcag_luminance(b))
}

/// WCAG relative luminance on this module's transfer tables.
#[inline]
fn wcag_luminance(c: Rgb) -> f32 {
    let tables = transfer_tables();
    0.2126 * tables.to_linear[usize::from(c.r)]
        + 0.7152 * tables.to_linear[usize::from(c.g)]
        + 0.0722 * tables.to_linear[usize::from(c.b)]
}

/// The WCAG contrast ratio of two relative luminances.
#[inline]
const fn wcag_contrast(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The pole a background of luminance `lb` contrasts with more: black (`0`)
/// or white (`255`).
#[inline]
const fn pole_of(lb: f32) -> u8 {
    if (lb + 0.05) * (lb + 0.05) >= 0.05 * 1.05 {
        0
    } else {
        255
    }
}

#[inline]
const fn grey(v: u8) -> Rgb {
    Rgb { r: v, g: v, b: v }
}

/// The smallest step `k` of `1..=10` whose sRGB mix of `fg` toward the grey
/// `pole` by `k` tenths clears `min` against a background of luminance `lb`,
/// with that mix: [`floor_contrast`]'s nudge.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a lerp between two bytes, rounded, stays in 0..=255"
)]
fn first_clearing_tenth(fg: Rgb, pole: u8, lb: f32, min: f32) -> Option<(u8, Rgb)> {
    let mix = |f: u8, t: f32| -> u8 {
        let f = f32::from(f);
        (f + (f32::from(pole) - f) * t).round() as u8
    };
    (1..=10u8).find_map(|step| {
        let t = f32::from(step) / 10.0;
        let candidate = Rgb {
            r: mix(fg.r, t),
            g: mix(fg.g, t),
            b: mix(fg.b, t),
        };
        (wcag_contrast(wcag_luminance(candidate), lb) >= min).then_some((step, candidate))
    })
}

const HALFWAY_TENTHS: u8 = 5;

const MUD_CHROMA: f64 = 0.035;

/// [`floor_contrast`] for text on a colored cell (`inks` = theme default `[fg, bg]`).
#[must_use]
pub fn floor_contrast_on_colour(fg: Rgb, bg: Rgb, min: f32, inks: [Rgb; 2]) -> Rgb {
    let lb = wcag_luminance(bg);
    let lf = wcag_luminance(fg);
    if wcag_contrast(lf, lb) >= min {
        return fg;
    }
    let pole = pole_of(lb);
    let black = pole == 0;
    let [a, b] = inks;
    let (la, lb_ink) = (wcag_luminance(a), wcag_luminance(b));
    let (ink, li) = if (la < lb_ink) == black {
        (a, la)
    } else {
        (b, lb_ink)
    };
    let beyond = if black { li < lb } else { li > lb };
    let flips = if black { lf >= lb } else { lf <= lb };
    if flips {
        return if beyond && wcag_contrast(li, lb) >= min {
            ink
        } else {
            grey(pole)
        };
    }
    let keeps_itself = |step: u8, nudged: Rgb| {
        step < HALFWAY_TENTHS || aterm_types::scheme_signature::chroma_hue(nudged).0 >= MUD_CHROMA
    };
    match first_clearing_tenth(fg, pole, lb, min) {
        Some((step, nudged)) if keeps_itself(step, nudged) => nudged,
        found => {
            let nudged = found.map_or_else(|| grey(pole), |(_, nudged)| nudged);
            if beyond && wcag_contrast(li, lb) >= wcag_contrast(wcag_luminance(nudged), lb) {
                ink
            } else {
                nudged
            }
        }
    }
}

/// Resolve foreground and background colors for a cell.
#[must_use]
#[inline]
pub fn resolve_colors(
    cell: &Cell,
    extra: Option<&CellExtra>,
    palette: &ColorPalette,
    default_fg: Rgb,
    default_bg: Rgb,
    reverse_video: bool,
) -> (Rgb, Rgb) {
    let fg_rgb = extra.and_then(CellExtra::fg_rgb);
    let bg_rgb = extra.and_then(CellExtra::bg_rgb);
    resolve_both(
        *cell,
        fg_rgb,
        bg_rgb,
        palette,
        default_fg,
        default_bg,
        reverse_video,
        StyleResolveOpts::default(),
    )
}

/// Resolve colors from pre-resolved RGB values under an explicit [`StyleResolveOpts`] policy.
#[must_use]
#[inline]
#[allow(
    clippy::too_many_arguments,
    reason = "one call site per frame path; a params struct would just relocate the arity"
)]
pub fn resolve_colors_raw_opts(
    cell: &Cell,
    fg_rgb: Option<[u8; 3]>,
    bg_rgb: Option<[u8; 3]>,
    palette: &ColorPalette,
    default_fg: Rgb,
    default_bg: Rgb,
    reverse_video: bool,
    opts: StyleResolveOpts,
) -> (Rgb, Rgb) {
    resolve_both(
        *cell,
        fg_rgb,
        bg_rgb,
        palette,
        default_fg,
        default_bg,
        reverse_video,
        opts,
    )
}

/// Core resolution logic — returns (fg, bg) tuple.
#[allow(
    clippy::too_many_arguments,
    reason = "internal fan-in point for the public wrappers"
)]
#[inline]
fn resolve_both(
    cell: Cell,
    fg_rgb: Option<[u8; 3]>,
    bg_rgb: Option<[u8; 3]>,
    palette: &ColorPalette,
    default_fg: Rgb,
    default_bg: Rgb,
    reverse_video: bool,
    opts: StyleResolveOpts,
) -> (Rgb, Rgb) {
    resolve_both_with::<true>(
        cell,
        fg_rgb,
        bg_rgb,
        palette,
        default_fg,
        default_bg,
        reverse_video,
        opts,
    )
}

/// [`resolve_both`], with a light theme's dark whites ([`white_on_colour`])
/// taken as drawn (`LIGHT_WHITES = false`) for the tests' reference.
#[allow(
    clippy::too_many_arguments,
    reason = "internal fan-in point for the public wrappers"
)]
fn resolve_both_with<const LIGHT_WHITES: bool>(
    cell: Cell,
    fg_rgb: Option<[u8; 3]>,
    bg_rgb: Option<[u8; 3]>,
    palette: &ColorPalette,
    default_fg: Rgb,
    default_bg: Rgb,
    reverse_video: bool,
    opts: StyleResolveOpts,
) -> (Rgb, Rgb) {
    let flags = cell.flags();
    let colors = cell.colors();

    let (mut fg, mut bg) = raw_resolve(colors, fg_rgb, bg_rgb, palette, default_fg, default_bg);
    let mut fg_slot = colors.fg_is_indexed().then(|| colors.fg_index());
    let bg_slot = colors.bg_is_indexed().then(|| colors.bg_index());

    if opts.bold_is_bright
        && flags.contains(CellFlags::BOLD)
        && !flags.contains(CellFlags::DIM)
        && colors.fg_is_indexed()
    {
        let idx = colors.fg_index();
        if idx < 8 {
            fg = palette.get(idx + 8);
            fg_slot = Some(idx + 8);
        }
    }

    let effective_inverse = flags.contains(CellFlags::INVERSE) != reverse_video;
    let ratio = opts.indexed_minimum_contrast;
    let floor = theme_pair_floor(cell, colors, reverse_video, ratio, palette);
    let inks = [default_fg, default_bg];
    if !effective_inverse {
        if LIGHT_WHITES {
            fg = white_on_colour(cell, ratio, fg_slot, bg_slot, fg, bg, default_bg);
        }
        if let Some(floor) = floor {
            fg = floor.apply(fg, bg, inks);
        }
    }

    if flags.contains(CellFlags::DIM) {
        fg = dim_toward_bg(fg, bg, opts.faint_opacity);
    }

    if effective_inverse {
        std::mem::swap(&mut fg, &mut bg);
        if LIGHT_WHITES {
            fg = white_on_colour(cell, ratio, bg_slot, fg_slot, fg, bg, default_bg);
        }
        if let Some(floor) = floor {
            fg = floor.apply(fg, bg, inks);
        }
    }

    if flags.contains(CellFlags::HIDDEN) {
        fg = bg;
    }

    (fg, bg)
}

/// Precomputed sRGB <-> linear-light transfer tables avoiding per-cell `powf`.
#[inline]
fn srgb_to_linear_direct(c: u8) -> f32 {
    let n = f32::from(c) / 255.0;
    encoded_unit_to_linear(n)
}

#[inline]
fn encoded_unit_to_linear(n: f32) -> f32 {
    if n <= 0.04045 {
        n / 12.92
    } else {
        ((n + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "input is clamped to [0,1]; encoded*255 rounds into 0..=255"
)]
fn linear_to_srgb_direct(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let n = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (n * 255.0).round() as u8
}

const LINEAR_BUCKETS: usize = 8192;

struct TransferTables {
    to_linear: [f32; 256],
    /// `thresholds[q - 1]` is the first non-negative f32 that rounds to sRGB byte `q` (`1..=255`).
    thresholds: [f32; 255],
    /// Result at each bucket's lower edge. An sRGB byte boundary is wider than an
    /// 1/8192 linear bucket; the correction below is therefore normally zero or one
    /// comparison (the defensive reverse correction keeps the law honest if the
    /// transfer constants ever change).
    bucket_base: [u8; LINEAR_BUCKETS],
}

impl TransferTables {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "all loop indices are bounded by the 8-bit transfer domain"
    )]
    fn new() -> Self {
        let mut to_linear = [0.0; 256];
        for (value, out) in to_linear.iter_mut().enumerate() {
            *out = srgb_to_linear_direct(value as u8);
        }

        let mut thresholds = [0.0; 255];
        for q in 1..=255u16 {
            let encoded_boundary = (f32::from(q) - 0.5) / 255.0;
            let mut bits = encoded_unit_to_linear(encoded_boundary).to_bits();
            while linear_to_srgb_direct(f32::from_bits(bits)) < q as u8 {
                bits += 1;
            }
            while bits > 0 && linear_to_srgb_direct(f32::from_bits(bits - 1)) >= q as u8 {
                bits -= 1;
            }
            thresholds[usize::from(q - 1)] = f32::from_bits(bits);
        }

        let mut bucket_base = [0u8; LINEAR_BUCKETS];
        let mut q = 0usize;
        for (bucket, out) in bucket_base.iter_mut().enumerate() {
            let lower = bucket as f32 / LINEAR_BUCKETS as f32;
            while q < thresholds.len() && lower >= thresholds[q] {
                q += 1;
            }
            *out = q as u8;
        }
        Self {
            to_linear,
            thresholds,
            bucket_base,
        }
    }
}

#[inline]
fn transfer_tables() -> &'static TransferTables {
    static TABLES: OnceLock<TransferTables> = OnceLock::new();
    TABLES.get_or_init(TransferTables::new)
}

/// sRGB channel (0..=255) → linear light (0.0..=1.0), the IEC 61966-2-1 EOTF.
#[cfg(test)]
#[inline]
fn srgb_to_linear(c: u8) -> f32 {
    transfer_tables().to_linear[usize::from(c)]
}

/// Linear light (`0.0..=1.0`) → sRGB channel (`0..=255`).
#[cfg(test)]
#[inline]
fn linear_to_srgb(l: f32) -> u8 {
    linear_to_srgb_with(transfer_tables(), l)
}

#[inline]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "clamped finite input maps to the bounded bucket and byte ranges"
)]
fn linear_to_srgb_with(tables: &TransferTables, l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let bucket = ((l * LINEAR_BUCKETS as f32) as usize).min(LINEAR_BUCKETS - 1);
    let mut q = usize::from(tables.bucket_base[bucket]);
    while q < tables.thresholds.len() && l >= tables.thresholds[q] {
        q += 1;
    }
    while q > 0 && l < tables.thresholds[q - 1] {
        q -= 1;
    }
    q as u8
}

/// SGR 2 (dim/faint): blend `fg` toward `bg` retaining `opacity` (`0.0..=1.0`).
#[must_use]
#[inline]
pub fn dim_toward_bg(fg: Rgb, bg: Rgb, opacity: f32) -> Rgb {
    dim_toward_bg_with(fg, bg, opacity, FAINT_SRGB_BLEND)
}

/// Whether light faint ink starts from the sRGB blend before the contrast floor.
pub const FAINT_SRGB_BLEND: bool = cfg!(windows);

/// [`dim_toward_bg`] with the light-ink start chosen by `srgb_blend`.
pub(crate) fn dim_toward_bg_with(fg: Rgb, bg: Rgb, opacity: f32, srgb_blend: bool) -> Rgb {
    let t = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        DIM_FACTOR
    };
    let tables = transfer_tables();
    let luminance = |c: Rgb| {
        0.2126 * tables.to_linear[usize::from(c.r)]
            + 0.7152 * tables.to_linear[usize::from(c.g)]
            + 0.0722 * tables.to_linear[usize::from(c.b)]
    };
    let (lf, lb) = (luminance(fg), luminance(bg));
    let start = if srgb_blend && lf > lb && t > 0.0 && t < 1.0 {
        srgb_blend_mix(tables, fg, bg, (lf, lb), t)
    } else {
        t
    };
    let t = faint_mix(lf, lb, t, start);
    let mix = |f: u8, b: u8| -> u8 {
        let lf = tables.to_linear[usize::from(f)];
        let lb = tables.to_linear[usize::from(b)];
        linear_to_srgb_with(tables, lb + (lf - lb) * t)
    };
    Rgb {
        r: mix(fg.r, bg.r),
        g: mix(fg.g, bg.g),
        b: mix(fg.b, bg.b),
    }
}

/// The linear-light mix whose luminance is the sRGB-space blend's: each
/// channel `bg + t·(fg − bg)` in sRGB, taken to linear light (between table
/// entries, along the chord), weighted into a relative luminance `Ls`, and
/// returned as `(Ls − lb) / (lf − lb)` for `(lf, lb)` the ends' luminances
/// (`lf != lb`).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a clamped 0..=255 channel value indexes the 256-entry table"
)]
fn srgb_blend_mix(tables: &TransferTables, fg: Rgb, bg: Rgb, ends: (f32, f32), t: f32) -> f32 {
    let (lf, lb) = ends;
    let linear_at = |encoded: f32| {
        let encoded = encoded.clamp(0.0, 255.0);
        let below = (encoded as usize).min(254);
        let (low, high) = (tables.to_linear[below], tables.to_linear[below + 1]);
        low + (high - low) * (encoded - below as f32)
    };
    let channel = |ink: u8, ground: u8| {
        let ground = f32::from(ground);
        linear_at(ground + (f32::from(ink) - ground) * t)
    };
    let ls =
        0.2126 * channel(fg.r, bg.r) + 0.7152 * channel(fg.g, bg.g) + 0.0722 * channel(fg.b, bg.b);
    (ls - lb) / (lf - lb)
}

const FAINT_FLOOR_BASE: f32 = 9.0;

const FAINT_FLOOR_MIN_CONTRAST: f32 = 1.25;

fn faint_target(full: f32, t: f32) -> f32 {
    let half = t.to_bits() == 0.5f32.to_bits();
    let root = if half { full.sqrt() } else { 0.0 };
    let share = if half { root } else { full.powf(t) };
    if full < FAINT_FLOOR_MIN_CONTRAST {
        return share;
    }
    let (readable, ceiling) = if half {
        (3.0, root * root.sqrt())
    } else {
        (FAINT_FLOOR_BASE.powf(t), full.powf(f32::midpoint(1.0, t)))
    };
    share.max(readable.min(ceiling))
}

fn faint_mix(lf: f32, lb: f32, opacity: f32, start: f32) -> f32 {
    if opacity <= 0.0 || opacity >= 1.0 {
        return opacity;
    }
    let (hi, lo) = (lf.max(lb), lf.min(lb));
    if hi - lo <= f32::EPSILON {
        return opacity;
    }
    let start = if start.is_finite() {
        start.clamp(0.0, opacity)
    } else {
        opacity
    };
    let full = (hi + 0.05) / (lo + 0.05);
    let target = faint_target(full, opacity);
    let at_start = if lf >= lb {
        1.0 + start * (full - 1.0)
    } else {
        (lb + 0.05) / (lb + start * (lf - lb) + 0.05)
    };
    if at_start >= target {
        return start;
    }
    let needed = if lf < lb {
        (lb + 0.05) / target - 0.05
    } else {
        target * (lb + 0.05) - 0.05
    };
    let floor = ((needed - lb) / (lf - lb)).clamp(0.0, 1.0);
    start.max(floor)
}

/// Resolve raw cell colors from packed representation.
#[inline]
fn raw_resolve(
    colors: PackedColors,
    fg_rgb: Option<[u8; 3]>,
    bg_rgb: Option<[u8; 3]>,
    palette: &ColorPalette,
    default_fg: Rgb,
    default_bg: Rgb,
) -> (Rgb, Rgb) {
    let fg = if colors.fg_is_default() {
        default_fg
    } else if colors.fg_is_indexed() {
        palette.get(colors.fg_index())
    } else {
        fg_rgb.map_or(default_fg, |[r, g, b]| Rgb { r, g, b })
    };

    let bg = if colors.bg_is_default() {
        default_bg
    } else if colors.bg_is_indexed() {
        palette.get(colors.bg_index())
    } else {
        bg_rgb.map_or(default_bg, |[r, g, b]| Rgb { r, g, b })
    };

    (fg, bg)
}

/// Machine-checked dim contrast and floor invariants across grayscale and RGB lattices.
#[cfg(test)]
#[path = "color_resolve_dim_contrast_law_tests.rs"]
mod dim_contrast_law;

#[cfg(test)]
#[path = "color_resolve_tests.rs"]
mod tests;
