// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Centralized color resolution for terminal cells.
//!
//! Resolves raw cell colors (default/indexed/RGB) into final RGB values
//! with all style attributes applied: bold-to-bright, dim, inverse,
//! DECSCNM (reverse video), and hidden.
//!
//! This is the single source of truth for color resolution. All frontends
//! (bridge, FFI/Swift, GPU) should use these functions instead of
//! reimplementing attribute handling.

use crate::grid::{Cell, CellExtra, CellFlags, PackedColors};
use aterm_types::{ColorPalette, DIM_FACTOR, Rgb};
use std::sync::OnceLock;

/// Host-configurable style-attribute policy for color resolution.
///
/// * `bold_is_bright` — whether SGR 1 promotes indexed colors 0–7 to their
///   bright 8–15 siblings (the classic xterm behavior). Defaults to `true`
///   (the historical unconditional promotion); config `bold_is_bright = false`
///   keeps bold a pure weight change.
/// * `faint_opacity` — how much of the foreground SGR 2 (dim/faint) retains,
///   `0.0..=1.0`. The fg is blended toward the CELL background in linear
///   light by at least this fraction, raised where needed so faint text keeps
///   `contrast^opacity` and a readable `9^opacity` (3:1 at the default)
///   capped below the colour's own contrast (see [`dim_toward_bg`]). Faint
///   text recedes on both dark and light themes. Defaults to [`DIM_FACTOR`]
///   (0.5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyleResolveOpts {
    /// SGR 1 promotes indexed 0–7 to bright 8–15 (default `true`).
    pub bold_is_bright: bool,
    /// Fraction of fg retained by SGR 2, blended toward bg (default 0.5).
    pub faint_opacity: f32,
}

impl Default for StyleResolveOpts {
    fn default() -> Self {
        Self {
            bold_is_bright: true,
            faint_opacity: DIM_FACTOR,
        }
    }
}

/// Resolve both foreground and background colors for a cell.
///
/// Returns `(fg, bg)` with all style attributes applied.
///
/// **Note:** This uses `CellExtra` for RGB lookup, which only checks the
/// HashMap. Prefer [`resolve_colors_raw_opts`] with pre-resolved RGB values
/// from `grid.fg_rgb_at()` / `grid.bg_rgb_at()` to include ring buffer lookups.
#[must_use]
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

/// Resolve colors from pre-resolved RGB values under an explicit host style
/// policy ([`StyleResolveOpts`]: `bold_is_bright` + `faint_opacity`). The
/// render extraction path ([`render_row`](super::Terminal::render_row)) passes
/// the terminal's configured policy.
#[must_use]
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
    let flags = cell.flags();
    let colors = cell.colors();

    // 1. Raw color lookup (default -> indexed -> RGB)
    let (mut fg, mut bg) = raw_resolve(colors, fg_rgb, bg_rgb, palette, default_fg, default_bg);

    // 2. Bold-to-bright: indexed 0-7 -> 8-15 when BOLD (not DIM), if the host
    // policy keeps the promotion on (`bold_is_bright`, default true).
    // Standard terminal behavior per ECMA-48: when both BOLD and DIM are
    // set, dim wins — no bright promotion.
    if opts.bold_is_bright
        && flags.contains(CellFlags::BOLD)
        && !flags.contains(CellFlags::DIM)
        && colors.fg_is_indexed()
    {
        let idx = colors.fg_index();
        if idx < 8 {
            fg = palette.get(idx + 8);
        }
    }

    // 3. Dim (SGR 2): blend fg toward the cell background in linear light.
    // (The old "multiply toward black" made faint text darker — i.e. heavier —
    // on light themes, the inversion of the attribute. Blending toward bg is
    // theme-independent: it always recedes toward the surface it sits on.)
    // Applied before the inverse swap, like the old multiply.
    if flags.contains(CellFlags::DIM) {
        fg = dim_toward_bg(fg, bg, opts.faint_opacity);
    }

    // 4. Inverse: XOR with DECSCNM
    // Cell INVERSE flag and terminal reverse_video cancel each other out.
    let effective_inverse = flags.contains(CellFlags::INVERSE) != reverse_video;
    if effective_inverse {
        std::mem::swap(&mut fg, &mut bg);
    }

    // 5. Hidden: fg = bg (after inverse)
    if flags.contains(CellFlags::HIDDEN) {
        fg = bg;
    }

    (fg, bg)
}

/// The transfer functions are exact but include `powf`; keep them off the per-cell
/// render path. `Terminal::cell_frame_into` resolves every faint cell on every
/// candidate redraw, so three inverse + up to six forward `powf`s per cell showed up
/// directly in samples of an event-loop wake storm. The process-lifetime tables below
/// preserve the old f32 result byte-for-byte while reducing that hot path to lookups,
/// arithmetic, and at most a one-step threshold correction.
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
    /// `thresholds[q - 1]` is the first non-negative f32 that the historical
    /// transfer function rounds to byte `q` (q=1..=255). Deriving the exact f32
    /// transition, rather than merely sampling a curve, makes the fast quantizer
    /// byte-identical even at rounding boundaries.
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
            // Invert the half-byte rounding boundary for a close starting point,
            // then walk the neighboring f32 values until we have the first value
            // accepted by the exact legacy function. Usually this takes one probe.
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

/// Linear light (0.0..=1.0) → sRGB channel (0..=255), rounding exactly as the
/// former direct transfer function without evaluating `powf` per rendered channel.
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

/// SGR 2 (dim/faint): blend `fg` toward `bg` by `1 - opacity`, in linear light.
///
/// `opacity` is the fraction of the foreground retained (clamped to `0..=1`):
/// `1.0` returns `fg` unchanged, `0.0` lands on `bg` (invisible). The blend is
/// per-channel in linear light, so the result's WCAG relative luminance is
/// the linear interpolation `L(dim) = L(bg) + m·(L(fg) − L(bg))` (up to u8
/// quantization), where the mix `m` is `opacity` raised, when needed, to
/// reach [`faint_target`] ([`faint_mix`]). With `C = contrast(fg, bg)` and
/// `t = opacity`, the target is the largest of two floors:
///
/// * the log-share floor `C^t`: faint keeps `t` of the contrast in log
///   space, which dominates for body text;
/// * the readable floor `9^t` (3:1 at the default 0.5, 1:1 at 0), capped by
///   the recession ceiling `C^((1+t)/2)` so faint never reaches the colour's
///   own contrast. It applies only where `C >= 1.25`; below that the pair is
///   not legible text either way.
///
/// The floors are what make one opacity read alike on both polarities. A
/// luminance lerp halves light ink on a dark ground and keeps most of its
/// contrast (16.4:1 → 8.7:1 on Aterm Night), but on a light ground it lands
/// dark ink in the mid-greys, where the ratio collapses (14.35:1 → 1.9:1
/// for Aterm Day's fg). At the default 0.5, Aterm Day's fg goes 14.35:1 →
/// 3.78:1 (the log share), colour 7 goes 5.76:1 → 3.0:1 (the readable
/// floor) and colour 8 goes 3.50:1 → 2.57:1 (the ceiling). The floors only
/// ever raise the mix, so a dark theme's faint ink, already above them,
/// is unchanged except on its low-contrast slots.
///
/// # Invariant (proven)
///
/// `contrast(dim_toward_bg(fg, bg, t), bg) <= contrast(fg, bg)` for every
/// `fg`/`bg` pair and `t ∈ [0,1]` — dim reduces contrast on both polarities
/// (dark-on-light and light-on-dark), unlike the old gamma-space multiply
/// toward black, which made faint text HIGHER-contrast (darker/heavier) on
/// light backgrounds. Exhaustively machine-checked over all grayscale pairs
/// and a dense RGB lattice in the `dim_contrast_law` tests below; the old
/// multiply is kept there as the negative control.
#[must_use]
pub fn dim_toward_bg(fg: Rgb, bg: Rgb, opacity: f32) -> Rgb {
    let t = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        DIM_FACTOR
    };
    // Resolve the OnceLock once per cell, not once per channel/transfer.
    let tables = transfer_tables();
    let luminance = |c: Rgb| {
        0.2126 * tables.to_linear[usize::from(c.r)]
            + 0.7152 * tables.to_linear[usize::from(c.g)]
            + 0.0722 * tables.to_linear[usize::from(c.b)]
    };
    let t = faint_mix(luminance(fg), luminance(bg), t);
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

/// SGR 2's readable floor is `FAINT_FLOOR_BASE^opacity`: 3:1 at the
/// default 0.5 (WCAG's non-text and large-text bar), 1:1 at 0.
const FAINT_FLOOR_BASE: f32 = 9.0;

/// Below this undimmed contrast the pair is not legible text, normal or
/// faint; only the log-share floor applies. Raising such a pair's mix would
/// only amplify cross-hue u8 quantization.
const FAINT_FLOOR_MIN_CONTRAST: f32 = 1.25;

/// The contrast SGR 2 aims for when the undimmed pair has contrast `full`
/// (>= 1) at opacity `t` (in `0..1`): the larger of the log-share floor
/// `full^t` and the readable floor `FAINT_FLOOR_BASE^t`, the latter capped
/// at `full^((1+t)/2)` so faint keeps at most `(1+t)/2` of the contrast in
/// log space and always stays fainter than the colour itself.
fn faint_target(full: f32, t: f32) -> f32 {
    let share = full.powf(t);
    if full < FAINT_FLOOR_MIN_CONTRAST {
        return share;
    }
    share.max(
        FAINT_FLOOR_BASE
            .powf(t)
            .min(full.powf(f32::midpoint(1.0, t))),
    )
}

/// The linear-light mix SGR 2 uses for an fg of relative luminance `lf` on
/// a bg of `lb` at `opacity` (already in `0..=1`): `opacity`, raised to the
/// smallest mix whose WCAG contrast reaches [`faint_target`] — the larger
/// of the log-share floor `contrast^opacity` and the readable floor
/// `9^opacity` capped at `contrast^((1+opacity)/2)`. Theme-independent and
/// bounded: the result stays in `opacity..=1`, `0` stays invisible and `1`
/// stays the fg, and since the luminance is a lerp in the mix the floor is
/// solved exactly rather than searched.
fn faint_mix(lf: f32, lb: f32, opacity: f32) -> f32 {
    if opacity <= 0.0 || opacity >= 1.0 {
        return opacity;
    }
    let (hi, lo) = (lf.max(lb), lf.min(lb));
    if hi - lo <= f32::EPSILON {
        return opacity;
    }
    let full = (hi + 0.05) / (lo + 0.05);
    // Light ink at 9:1 or more: the lerp's contrast 1 + t(full - 1) is at
    // least 1 + 8t >= 9^t (the chord of a convex function) and at least
    // full^t (weighted AM-GM), so it already meets the target. This keeps
    // dark-theme body text free of per-cell powf.
    if lf >= lb && full >= FAINT_FLOOR_BASE {
        return opacity;
    }
    let target = faint_target(full, opacity);
    // The contrast the plain mix already has. Skipping the solve when that
    // is enough keeps light ink clear of the solve's f32 cancellation,
    // which could otherwise nudge a channel across a quantizer boundary.
    let at_t = if lf >= lb {
        1.0 + opacity * (full - 1.0)
    } else {
        (lb + 0.05) / (lb + opacity * (lf - lb) + 0.05)
    };
    if at_t >= target {
        return opacity;
    }
    // The luminance the faint ink needs to reach `target` against `lb`.
    let needed = if lf < lb {
        (lb + 0.05) / target - 0.05
    } else {
        target * (lb + 0.05) - 0.05
    };
    let floor = ((needed - lb) / (lf - lb)).clamp(0.0, 1.0);
    opacity.max(floor)
}

/// Resolve raw cell colors from packed representation.
///
/// Handles the three-tier resolution: default -> indexed palette -> RGB.
/// The `fg_rgb` and `bg_rgb` parameters are pre-resolved from the unified
/// grid lookup (ring buffer + HashMap) or from CellExtra directly.
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
        // RGB — use pre-resolved value from unified lookup
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

/// The dim contrast law, machine-checked (L0, always on).
///
/// Invariant: `contrast(dim_toward_bg(fg, bg, t), bg) <= contrast(fg, bg)`
/// for all `fg`, `bg` and `t ∈ [0,1]`: SGR 2 reduces WCAG contrast on both
/// polarities. In real arithmetic it is exact, since the linear-light blend
/// makes relative luminance the lerp `L(dim) = L(bg) + t·(L(fg) − L(bg))`. u8
/// quantization adds a bounded perturbation, checked here:
///
/// * grayscale: exact over the exhaustive 256×256×|t| domain, zero tolerance;
/// * RGB (channels may move in opposing directions): exact for pairs with
///   `contrast(fg,bg) >= 1.05`, within `+0.005` (one u8 step of luminance)
///   for near-iso-luminant pairs, over a dense 8³-per-color lattice.
///
/// The faint floors are checked here too: faint reaches its target (within
/// u8 quantization), stays strictly fainter than its colour, and never mixes
/// less than `opacity`. Each floor term has a negative control showing what
/// goes wrong without it, and a gamma-space multiply toward black is the
/// negative control for the law itself: it raises contrast for dark-on-light
/// text.
#[cfg(test)]
mod dim_contrast_law {
    use super::{dim_toward_bg, faint_target, srgb_to_linear};
    use aterm_types::Rgb;

    fn luminance(c: Rgb) -> f32 {
        0.2126 * srgb_to_linear(c.r) + 0.7152 * srgb_to_linear(c.g) + 0.0722 * srgb_to_linear(c.b)
    }

    fn contrast(a: Rgb, b: Rgb) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    const OPACITIES: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

    /// The interior opacities the lattice tests sweep.
    const INTERIOR: [f32; 3] = [0.25, 0.5, 0.75];

    /// The 8-values-per-channel lattice: 512 colours, 262k ordered pairs.
    fn lattice() -> Vec<Rgb> {
        let vals: Vec<u8> = (0..8u16).map(|i| (i * 255 / 7) as u8).collect();
        let mut colors = Vec::with_capacity(512);
        for &r in &vals {
            for &g in &vals {
                for &b in &vals {
                    colors.push(Rgb { r, g, b });
                }
            }
        }
        colors
    }

    /// The plain linear-light lerp at mix `m`, quantized like the engine.
    fn blend(fg: Rgb, bg: Rgb, m: f32) -> Rgb {
        let channel = |f: u8, b: u8| {
            let (lf, lb) = (srgb_to_linear(f), srgb_to_linear(b));
            super::linear_to_srgb(lb + (lf - lb) * m)
        };
        Rgb {
            r: channel(fg.r, bg.r),
            g: channel(fg.g, bg.g),
            b: channel(fg.b, bg.b),
        }
    }

    /// A reference faint ink: the engine's shape (light ink at 9:1 or more
    /// keeps the lerp, the plain mix is kept when it already reaches the
    /// target, else the exact solve), with the target supplied by the
    /// caller so a control can swap one term out.
    fn faint_with(fg: Rgb, bg: Rgb, t: f32, target: impl Fn(f32, f32) -> f32) -> Rgb {
        let (lf, lb) = (luminance(fg), luminance(bg));
        let (hi, lo) = (lf.max(lb), lf.min(lb));
        if t <= 0.0 || t >= 1.0 || hi - lo <= f32::EPSILON {
            return blend(fg, bg, t);
        }
        let full = (hi + 0.05) / (lo + 0.05);
        if lf >= lb && full >= 9.0 {
            return blend(fg, bg, t);
        }
        let target = target(full, t);
        let at_t = if lf >= lb {
            1.0 + t * (full - 1.0)
        } else {
            (lb + 0.05) / (lb + t * (lf - lb) + 0.05)
        };
        if at_t >= target {
            return blend(fg, bg, t);
        }
        let needed = if lf < lb {
            (lb + 0.05) / target - 0.05
        } else {
            target * (lb + 0.05) - 0.05
        };
        blend(fg, bg, t.max(((needed - lb) / (lf - lb)).clamp(0.0, 1.0)))
    }

    /// The faint target without the `C >= 1.25` gate (a control).
    fn ungated_target(full: f32, t: f32) -> f32 {
        full.powf(t)
            .max(9f32.powf(t).min(full.powf(f32::midpoint(1.0, t))))
    }

    /// The pre-floor target: the log share alone (a control).
    fn share_target(full: f32, t: f32) -> f32 {
        full.powf(t)
    }

    /// A readable floor with no recession ceiling: `min(C, 3)` (a control).
    fn naive_target(full: f32, t: f32) -> f32 {
        full.powf(t).max(full.min(3.0))
    }

    /// Grayscale: exhaustive over all 256×256 pairs × 5 opacities, exact.
    #[test]
    fn dim_never_increases_contrast_grayscale_exhaustive() {
        let mut reduced = 0u32; // non-vacuity: strict reductions must occur
        for f in 0..=255u8 {
            for b in 0..=255u8 {
                let fg = Rgb { r: f, g: f, b: f };
                let bg = Rgb { r: b, g: b, b: b };
                let c0 = contrast(fg, bg);
                for t in OPACITIES {
                    let c1 = contrast(dim_toward_bg(fg, bg, t), bg);
                    assert!(
                        c1 <= c0,
                        "dim raised contrast: fg={f} bg={b} t={t}: {c1} > {c0}"
                    );
                    if c1 < c0 {
                        reduced += 1;
                    }
                }
            }
        }
        assert!(reduced > 100_000, "non-vacuity: dim must actually reduce");
    }

    /// Full-color lattice (8 values/channel = 512 colors, 262k pairs): exact
    /// wherever the pair carries real contrast (>= 1.05), and within one u8
    /// quantization step of luminance (0.005 ratio) even for near-iso-luminant
    /// cross-hue pairs.
    #[test]
    fn dim_reduces_contrast_both_polarities_rgb_lattice() {
        let colors = lattice();
        let mut dark_on_light_checked = false;
        let mut light_on_dark_checked = false;
        let mut ungated_excess = 0u32;
        for &fg in &colors {
            for &bg in &colors {
                let c0 = contrast(fg, bg);
                // Control for FAINT_FLOOR_MIN_CONTRAST: without the gate the
                // readable floor drives near-iso-luminant cross-hue pairs
                // toward the fg, where quantization breaks the tolerance.
                if c0 < 1.25 && contrast(faint_with(fg, bg, 0.75, ungated_target), bg) > c0 + 0.005
                {
                    ungated_excess += 1;
                }
                for t in INTERIOR {
                    let c1 = contrast(dim_toward_bg(fg, bg, t), bg);
                    if c0 >= 1.05 {
                        assert!(c1 <= c0, "dim raised real contrast: {fg:?}/{bg:?} t={t}");
                    } else {
                        assert!(
                            c1 <= c0 + 0.005,
                            "quantization excess out of bound: {fg:?}/{bg:?} t={t}"
                        );
                    }
                }
                // Track that both polarities are genuinely exercised.
                if luminance(fg) < luminance(bg) && c0 > 4.0 {
                    dark_on_light_checked = true;
                }
                if luminance(fg) > luminance(bg) && c0 > 4.0 {
                    light_on_dark_checked = true;
                }
            }
        }
        assert!(
            dark_on_light_checked && light_on_dark_checked,
            "non-vacuity"
        );
        assert!(
            ungated_excess > 0,
            "control: the ungated floor must break the quantization bound"
        );
    }

    /// Faint ink stays readable on the light themes: at the default opacity
    /// the fg and every palette slot reach their target (within u8
    /// quantization), so any colour at 4.4:1 or more is at about 3:1 faint,
    /// and each stays fainter than its colour. Controls: the bare lerp drops
    /// the fg under 2:1, and the pre-floor log share alone leaves Aterm
    /// Day's colour 7 under 2.5:1. On a dark theme the fg is at 9:1 or more
    /// and keeps exactly the lerp.
    #[test]
    fn faint_keeps_its_share_of_contrast_on_light_themes() {
        use super::faint_mix;
        let t = aterm_types::DIM_FACTOR;
        let mut floored = 0u32;
        for name in ["Aterm Day", "Linen", "Default Light"] {
            let scheme = aterm_types::scheme::builtin(name).expect("built-in");
            let (fg, bg) = (scheme.foreground, scheme.background);
            assert!(
                contrast(blend(fg, bg, t), bg) < 2.0,
                "{name}: control, the bare lerp is what was weak"
            );
            for (slot, ink) in std::iter::once(fg).chain(scheme.ansi).enumerate() {
                let full = contrast(ink, bg);
                if full < 1.25 {
                    continue;
                }
                let faint = contrast(dim_toward_bg(ink, bg, t), bg);
                let target = faint_target(full, t);
                assert!(faint >= target * 0.98, "{name} #{slot}: {faint} < {target}");
                assert!(faint < full, "{name} #{slot}: faint still recedes");
                if full >= 4.4 {
                    assert!(faint >= 2.94, "{name} #{slot}: faint at {faint}:1");
                }
                if target > full.powf(t) + 0.01 {
                    floored += 1;
                }
            }
        }
        assert!(
            floored > 10,
            "non-vacuity: the readable floor binds on palettes"
        );
        let day = aterm_types::scheme::builtin("Aterm Day").expect("built-in");
        let (seven, bg) = (day.ansi[7], day.background);
        assert!(
            contrast(faint_with(seven, bg, t, share_target), bg) < 2.5,
            "control: the log share alone leaves colour 7 under 2.5:1"
        );
        assert!(contrast(dim_toward_bg(seven, bg, t), bg) >= 2.94);
        for name in ["Aterm Night", "Hearth", "Default"] {
            let scheme = aterm_types::scheme::builtin(name).expect("built-in");
            let (fg, bg) = (scheme.foreground, scheme.background);
            assert!(
                luminance(fg) > luminance(bg) && contrast(fg, bg) >= 9.0,
                "{name}"
            );
            assert_eq!(dim_toward_bg(fg, bg, t), blend(fg, bg, t), "{name}");
        }
        // The ends stay put: invisible at 0, the fg at 1.
        assert!(faint_mix(0.02, 0.9, 0.0).abs() < f32::EPSILON);
        assert!((faint_mix(0.02, 0.9, 1.0) - 1.0).abs() < f32::EPSILON);
    }

    /// Faint is strictly fainter than its colour wherever the pair is text
    /// (`C >= 1.25`), and the readable floor never lifts it past the
    /// recession ceiling `C^((1+t)/2)`. Control: a floor of `min(C, 3)`
    /// with no ceiling leaves some such pair un-dimmed.
    #[test]
    fn faint_stays_fainter_than_its_colour() {
        let colors = lattice();
        let (mut floor_binds, mut naive_undimmed) = (0u32, 0u32);
        for &fg in &colors {
            for &bg in &colors {
                let full = contrast(fg, bg);
                if full < 1.25 {
                    continue;
                }
                for t in INTERIOR {
                    let faint = contrast(dim_toward_bg(fg, bg, t), bg);
                    assert!(faint < full, "fg={fg:?} bg={bg:?} t={t}: {faint} >= {full}");
                    let cap = contrast(blend(fg, bg, t), bg).max(full.powf(f32::midpoint(1.0, t)));
                    assert!(
                        faint <= cap * 1.02,
                        "fg={fg:?} bg={bg:?} t={t}: {faint} > {cap}"
                    );
                    if faint_target(full, t) > full.powf(t) + 0.01 {
                        floor_binds += 1;
                    }
                    if full <= 3.0 && contrast(faint_with(fg, bg, t, naive_target), bg) >= full {
                        naive_undimmed += 1;
                    }
                }
            }
        }
        assert!(floor_binds > 10_000, "non-vacuity: {floor_binds}");
        assert!(
            naive_undimmed > 0,
            "control: the ceiling must be load-bearing"
        );
    }

    /// The faint mix is never below `opacity`, so the luminance distance it
    /// keeps is at least `t` of the colour's (within one u8 step): the
    /// premise of matrix rain's plain-blend bound.
    #[test]
    fn faint_mix_never_drops_below_opacity() {
        let colors = lattice();
        let mut raised = 0u32;
        for &fg in &colors {
            for &bg in &colors {
                let (lf, lb) = (luminance(fg), luminance(bg));
                for t in INTERIOR {
                    let ld = luminance(dim_toward_bg(fg, bg, t));
                    assert!(
                        (ld - lb).abs() >= t * (lf - lb).abs() - 0.005,
                        "fg={fg:?} bg={bg:?} t={t}"
                    );
                    if (ld - lb).abs() > t * (lf - lb).abs() + 0.01 {
                        raised += 1;
                    }
                }
            }
        }
        assert!(raised > 10_000, "non-vacuity: the floors raise the mix");
    }

    /// The engine's shortcuts keep their reference bytes: light ink at 9:1
    /// or more, and any pair whose plain mix already reaches the target,
    /// keep the linear-light lerp; every other pair gets the exact floor
    /// solve. Compares public outputs, quantization included, against a
    /// reference written from the rule.
    #[test]
    fn faint_fast_paths_keep_their_polarity_references() {
        let rule = |full: f32, t: f32| {
            if full < 1.25 {
                return full.powf(t);
            }
            full.powf(t)
                .max(9f32.powf(t).min(full.powf(f32::midpoint(1.0, t))))
        };
        let colors = lattice();
        // The old floor's f32 cancellation could push this light-ink pair
        // past a blue-channel quantizer boundary. It must remain the ordinary
        // linear result, which it now reaches through the at_t shortcut.
        let fg = Rgb {
            r: 109,
            g: 255,
            b: 36,
        };
        let bg = Rgb {
            r: 0,
            g: 255,
            b: 182,
        };
        assert!(luminance(fg) > luminance(bg));
        assert_eq!(dim_toward_bg(fg, bg, 0.75), blend(fg, bg, 0.75));
        let (mut light_ink, mut dark_ink, mut light_floored) = (0, 0, 0);
        for &fg in &colors {
            for &bg in &colors {
                assert_eq!(dim_toward_bg(fg, bg, 0.0), bg);
                assert_eq!(dim_toward_bg(fg, bg, 1.0), fg);
                let (lf, lb) = (luminance(fg), luminance(bg));
                let lighter = lf >= lb;
                let full = (lf.max(lb) + 0.05) / (lf.min(lb) + 0.05);
                for t in INTERIOR {
                    let at_t = if lighter {
                        1.0 + t * (full - 1.0)
                    } else {
                        (lb + 0.05) / (lb + t * (lf - lb) + 0.05)
                    };
                    let plain = (lighter && full >= 9.0)
                        || at_t >= rule(full, t)
                        || (lf - lb).abs() <= f32::EPSILON;
                    let expected = if plain {
                        blend(fg, bg, t)
                    } else {
                        faint_with(fg, bg, t, rule)
                    };
                    assert_eq!(
                        dim_toward_bg(fg, bg, t),
                        expected,
                        "fg={fg:?} bg={bg:?} t={t}"
                    );
                    if lighter && !plain {
                        light_floored += 1;
                    }
                }
                if lighter {
                    light_ink += 1;
                } else {
                    dark_ink += 1;
                }
            }
        }
        assert!(
            light_ink > 100_000 && dark_ink > 100_000,
            "non-vacuity: both full RGB lattices"
        );
        assert!(
            light_floored > 0,
            "non-vacuity: light ink can now be floored"
        );
    }

    /// Negative control: a gamma-space multiply toward black raises contrast
    /// for dark text on a light background, so the law above distinguishes the
    /// linear-light blend from it.
    #[test]
    fn old_gamma_multiply_violates_the_law() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        fn old_dim(c: Rgb) -> Rgb {
            Rgb {
                r: (f32::from(c.r) * 0.5) as u8,
                g: (f32::from(c.g) * 0.5) as u8,
                b: (f32::from(c.b) * 0.5) as u8,
            }
        }
        let fg = Rgb {
            r: 60,
            g: 60,
            b: 60,
        };
        let bg = Rgb {
            r: 240,
            g: 240,
            b: 240,
        };
        assert!(
            contrast(old_dim(fg), bg) > contrast(fg, bg),
            "control: the old multiply must exhibit the polarity inversion"
        );
        assert!(
            contrast(dim_toward_bg(fg, bg, 0.5), bg) < contrast(fg, bg),
            "the fix reduces contrast on the same pair"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Cell, CellExtra, CellFlags, PackedColor};

    const DEFAULT_FG: Rgb = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    const DEFAULT_BG: Rgb = Rgb { r: 0, g: 0, b: 0 };

    fn palette() -> ColorPalette {
        ColorPalette::new()
    }

    #[test]
    fn dim_transfer_tables_are_byte_exact() {
        for c in 0..=255u8 {
            assert_eq!(
                srgb_to_linear(c).to_bits(),
                srgb_to_linear_direct(c).to_bits()
            );
        }

        // Dense coverage plus every exact f32 transition and its immediate
        // neighbors: the fast quantizer must preserve the historical byte at the
        // points where an approximation would be most likely to disagree.
        for i in 0..=131_072u32 {
            let l = i as f32 / 131_072.0;
            assert_eq!(linear_to_srgb(l), linear_to_srgb_direct(l), "l={l:?}");
        }
        for &threshold in &transfer_tables().thresholds {
            let bits = threshold.to_bits();
            for candidate in [bits.saturating_sub(1), bits, bits.saturating_add(1)] {
                let l = f32::from_bits(candidate);
                assert_eq!(linear_to_srgb(l), linear_to_srgb_direct(l), "l={l:?}");
            }
        }
    }

    /// A cell with `flags` and a foreground that is the default (`None`), an
    /// indexed color or an RGB triple, resolved through the shipping wrapper.
    fn resolve(
        fg: Option<Result<u8, [u8; 3]>>,
        flags: CellFlags,
        default_bg: Rgb,
        reverse_video: bool,
    ) -> (Rgb, Rgb) {
        let mut cell = Cell::default();
        let mut extra = CellExtra::default();
        match fg {
            None => {}
            Some(Ok(index)) => cell.set_fg(PackedColor::indexed(index)),
            Some(Err([r, g, b])) => {
                cell.set_fg(PackedColor::rgb(r, g, b));
                extra.set_fg_rgb(Some([r, g, b]));
            }
        }
        cell.set_flags(flags);
        resolve_colors(
            &cell,
            Some(&extra),
            &palette(),
            DEFAULT_FG,
            default_bg,
            reverse_video,
        )
    }

    /// Style attributes apply in order: raw lookup, bold-to-bright, dim (a blend
    /// toward the cell's bg), inverse, hidden; DECSCNM reverses the pair.
    #[test]
    fn style_attributes_resolve_in_order() {
        let pal = palette();
        let light_bg = Rgb::new(240, 240, 240);
        let rgb = Rgb::new(200, 100, 50);
        let dim = |c: Rgb, bg: Rgb| dim_toward_bg(c, bg, DIM_FACTOR);
        let all = CellFlags::BOLD
            .union(CellFlags::DIM)
            .union(CellFlags::INVERSE)
            .union(CellFlags::HIDDEN);
        #[rustfmt::skip]
        let cases: [(&str, Option<Result<u8, [u8; 3]>>, CellFlags, Rgb, bool, (Rgb, Rgb)); 14] = [
            ("plain", None, CellFlags::empty(), DEFAULT_BG, false, (DEFAULT_FG, DEFAULT_BG)),
            ("custom bg", None, CellFlags::empty(), light_bg, false, (DEFAULT_FG, light_bg)),
            ("dim ansi", Some(Ok(1)), CellFlags::DIM, DEFAULT_BG, false, (dim(pal.get(1), DEFAULT_BG), DEFAULT_BG)),
            ("dim rgb", Some(Err([200, 100, 50])), CellFlags::DIM, DEFAULT_BG, false, (dim(rgb, DEFAULT_BG), DEFAULT_BG)),
            ("dim on light", Some(Err([40, 40, 40])), CellFlags::DIM, light_bg, false, (dim(Rgb::new(40, 40, 40), light_bg), light_bg)),
            ("bold promotes", Some(Ok(3)), CellFlags::BOLD, DEFAULT_BG, false, (pal.get(11), DEFAULT_BG)),
            ("bold bright", Some(Ok(10)), CellFlags::BOLD, DEFAULT_BG, false, (pal.get(10), DEFAULT_BG)),
            ("bold rgb", Some(Err([200, 100, 50])), CellFlags::BOLD, DEFAULT_BG, false, (rgb, DEFAULT_BG)),
            ("bold+dim", Some(Ok(1)), CellFlags::BOLD.union(CellFlags::DIM), DEFAULT_BG, false, (dim(pal.get(1), DEFAULT_BG), DEFAULT_BG)),
            ("inverse", None, CellFlags::INVERSE, DEFAULT_BG, false, (DEFAULT_BG, DEFAULT_FG)),
            ("decscnm", None, CellFlags::empty(), DEFAULT_BG, true, (DEFAULT_BG, DEFAULT_FG)),
            ("decscnm+inverse", None, CellFlags::INVERSE, DEFAULT_BG, true, (DEFAULT_FG, DEFAULT_BG)),
            ("hidden", None, CellFlags::HIDDEN, DEFAULT_BG, false, (DEFAULT_BG, DEFAULT_BG)),
            ("hidden+inverse", None, CellFlags::HIDDEN.union(CellFlags::INVERSE), DEFAULT_BG, false, (DEFAULT_FG, DEFAULT_FG)),
        ];
        for (name, fg, flags, bg, reverse, want) in cases {
            assert_eq!(resolve(fg, flags, bg, reverse), want, "{name}");
        }
        let dimmed = dim(pal.get(2), DEFAULT_BG);
        assert_eq!(
            resolve(Some(Ok(2)), all, DEFAULT_BG, false),
            (dimmed, dimmed),
            "every flag at once"
        );
        let (on_light, _) = resolve(Some(Err([40, 40, 40])), CellFlags::DIM, light_bg, false);
        assert!(
            on_light.r > 40,
            "dim dark-on-light text moves TOWARD the light bg, not toward black: {on_light:?}"
        );
    }

    /// `bold_is_bright = false` keeps SGR 1 a pure weight change: indexed 0–7
    /// fg is not promoted to its bright sibling.
    #[test]
    fn test_bold_is_bright_off_keeps_base_color() {
        let mut cell = Cell::default();
        cell.set_fg(PackedColor::indexed(3));
        cell.set_flags(CellFlags::BOLD);
        let pal = palette();
        let opts_off = StyleResolveOpts {
            bold_is_bright: false,
            ..StyleResolveOpts::default()
        };
        let (fg, _) = resolve_colors_raw_opts(
            &cell, None, None, &pal, DEFAULT_FG, DEFAULT_BG, false, opts_off,
        );
        assert_eq!(fg, pal.get(3), "no promotion when the policy is off");
    }
}
