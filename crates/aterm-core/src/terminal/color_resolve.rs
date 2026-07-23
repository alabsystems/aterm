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

/// Host-configurable STYLE-attribute policy for color resolution (W5).
///
/// * `bold_is_bright` — whether SGR 1 promotes indexed colors 0–7 to their
///   bright 8–15 siblings (the classic xterm behavior). Defaults to `true`
///   (the historical unconditional promotion); config `bold_is_bright = false`
///   keeps bold a pure weight change.
/// * `faint_opacity` — how much of the foreground SGR 2 (dim/faint) retains,
///   `0.0..=1.0`. The fg is blended TOWARD THE CELL BACKGROUND in linear
///   light by this fraction (see [`dim_toward_bg`]), so faint text recedes on
///   BOTH dark and light themes. Defaults to [`DIM_FACTOR`] (0.5).
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

/// Resolve the foreground RGB color for a cell, applying all style attributes.
///
/// Applies in order: raw lookup -> bold-to-bright -> dim -> inverse -> hidden.
///
/// # Arguments
///
/// * `cell` - The cell to resolve colors for
/// * `extra` - Extended cell data (needed for RGB color lookup)
/// * `palette` - The color palette for indexed color resolution
/// * `default_fg` - Default foreground color (from terminal settings)
/// * `default_bg` - Default background color (from terminal settings)
/// * `reverse_video` - Terminal-level DECSCNM mode (DECSET mode 5)
#[must_use]
pub fn resolve_fg_color(
    cell: &Cell,
    extra: Option<&CellExtra>,
    palette: &ColorPalette,
    default_fg: Rgb,
    default_bg: Rgb,
    reverse_video: bool,
) -> Rgb {
    let fg_rgb = extra.and_then(CellExtra::fg_rgb);
    let bg_rgb = extra.and_then(CellExtra::bg_rgb);
    let (fg, _) = resolve_both(
        *cell,
        fg_rgb,
        bg_rgb,
        palette,
        default_fg,
        default_bg,
        reverse_video,
        StyleResolveOpts::default(),
    );
    fg
}

/// Resolve the background RGB color for a cell, applying all style attributes.
///
/// See [`resolve_fg_color`] for attribute application order.
#[must_use]
pub fn resolve_bg_color(
    cell: &Cell,
    extra: Option<&CellExtra>,
    palette: &ColorPalette,
    default_fg: Rgb,
    default_bg: Rgb,
    reverse_video: bool,
) -> Rgb {
    let fg_rgb = extra.and_then(CellExtra::fg_rgb);
    let bg_rgb = extra.and_then(CellExtra::bg_rgb);
    let (_, bg) = resolve_both(
        *cell,
        fg_rgb,
        bg_rgb,
        palette,
        default_fg,
        default_bg,
        reverse_video,
        StyleResolveOpts::default(),
    );
    bg
}

/// Resolve the foreground RGB color from pre-resolved RGB values.
///
/// Use this when RGB values are already retrieved from the unified grid lookup
/// (ring buffer + HashMap) rather than from `CellExtra` alone.
#[must_use]
pub fn resolve_fg_color_raw(
    cell: &Cell,
    fg_rgb: Option<[u8; 3]>,
    bg_rgb: Option<[u8; 3]>,
    palette: &ColorPalette,
    default_fg: Rgb,
    default_bg: Rgb,
    reverse_video: bool,
) -> Rgb {
    let (fg, _) = resolve_both(
        *cell,
        fg_rgb,
        bg_rgb,
        palette,
        default_fg,
        default_bg,
        reverse_video,
        StyleResolveOpts::default(),
    );
    fg
}

/// Resolve the background RGB color from pre-resolved RGB values.
///
/// See [`resolve_fg_color_raw`] for details.
#[must_use]
pub fn resolve_bg_color_raw(
    cell: &Cell,
    fg_rgb: Option<[u8; 3]>,
    bg_rgb: Option<[u8; 3]>,
    palette: &ColorPalette,
    default_fg: Rgb,
    default_bg: Rgb,
    reverse_video: bool,
) -> Rgb {
    let (_, bg) = resolve_both(
        *cell,
        fg_rgb,
        bg_rgb,
        palette,
        default_fg,
        default_bg,
        reverse_video,
        StyleResolveOpts::default(),
    );
    bg
}

/// Resolve both foreground and background colors for a cell.
///
/// Returns `(fg, bg)` with all style attributes applied.
///
/// **Note:** This uses `CellExtra` for RGB lookup, which only checks the
/// HashMap. Prefer [`resolve_colors_raw`] with pre-resolved RGB values from
/// `grid.fg_rgb_at()` / `grid.bg_rgb_at()` to include ring buffer lookups.
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

/// Resolve both foreground and background colors from pre-resolved RGB values.
///
/// Use this when RGB values are already retrieved from the unified grid lookup
/// (ring buffer + HashMap) via `grid.fg_rgb_at()` / `grid.bg_rgb_at()`.
#[must_use]
pub fn resolve_colors_raw(
    cell: &Cell,
    fg_rgb: Option<[u8; 3]>,
    bg_rgb: Option<[u8; 3]>,
    palette: &ColorPalette,
    default_fg: Rgb,
    default_bg: Rgb,
    reverse_video: bool,
) -> (Rgb, Rgb) {
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

/// Like [`resolve_colors_raw`], with an explicit host style policy
/// ([`StyleResolveOpts`]: `bold_is_bright` + `faint_opacity`). The render
/// extraction path ([`render_row`](super::Terminal::render_row)) passes the
/// terminal's configured policy; the default-opts wrappers above stay
/// byte-identical for callers that don't care.
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

    // 3. Dim (SGR 2): blend fg toward the cell BACKGROUND in linear light.
    // (The old "multiply toward black" made faint text DARKER — i.e. HEAVIER —
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
            // then walk the neighboring f32 values until we have the FIRST value
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

/// SGR 2 (dim/faint): blend `fg` TOWARD `bg` by `1 - opacity`, in LINEAR light.
///
/// `opacity` is the fraction of the foreground retained (clamped to `0..=1`):
/// `1.0` returns `fg` unchanged, `0.0` lands on `bg` (invisible). The blend is
/// per-channel in linear light, so the result's WCAG relative luminance is
/// exactly the same linear interpolation:
/// `L(dim) = L(bg) + opacity·(L(fg) − L(bg))` (up to u8 quantization).
///
/// # Invariant (proven)
///
/// `contrast(dim_toward_bg(fg, bg, t), bg) <= contrast(fg, bg)` for every
/// `fg`/`bg` pair and `t ∈ [0,1]` — dim REDUCES contrast on BOTH polarities
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
    // Resolve the OnceLock ONCE per cell, not once per channel/transfer.
    let tables = transfer_tables();
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

/// W5(e) PROOF — the dim contrast law, machine-checked (L0, always-on).
///
/// Invariant: `contrast(dim_toward_bg(fg, bg, t), bg) <= contrast(fg, bg)`
/// for all `fg`,`bg` and `t ∈ [0,1]` — SGR 2 REDUCES WCAG contrast on BOTH
/// polarities. In real arithmetic this is exact: the linear-light blend makes
/// relative luminance itself the lerp `L(dim) = L(bg) + t·(L(fg) − L(bg))`,
/// so `|L(dim) − L(bg)| = t·|L(fg) − L(bg)| ≤ |L(fg) − L(bg)|` and the WCAG
/// ratio (monotone in that distance) can only shrink. u8 quantization adds a
/// bounded perturbation, checked here empirically:
///
/// * GRAYSCALE (single-channel, all directions agree): EXACT over the full
///   exhaustive 256×256×|t| domain — zero tolerance.
/// * RGB (channels may move in opposing directions): EXACT for every pair
///   with `contrast(fg,bg) >= 1.05`, and within `+0.005` (one u8 step of
///   luminance) for near-iso-luminant pairs, over a dense 8³-per-color
///   lattice. (Measured worst excess: 4.9e-3, only below contrast 1.05.)
///
/// The pre-fix behavior (gamma-space multiply toward black) is kept as the
/// NEGATIVE CONTROL: it *raises* contrast for dark-on-light text.
#[cfg(test)]
mod dim_contrast_law {
    use super::{dim_toward_bg, srgb_to_linear};
    use aterm_types::Rgb;

    fn luminance(c: Rgb) -> f32 {
        0.2126 * srgb_to_linear(c.r) + 0.7152 * srgb_to_linear(c.g) + 0.0722 * srgb_to_linear(c.b)
    }

    fn contrast(a: Rgb, b: Rgb) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    const OPACITIES: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

    /// Grayscale: EXHAUSTIVE over all 256×256 pairs × 5 opacities, exact.
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
        let vals: Vec<u8> = (0..8u16).map(|i| (i * 255 / 7) as u8).collect();
        let mut colors = Vec::with_capacity(512);
        for &r in &vals {
            for &g in &vals {
                for &b in &vals {
                    colors.push(Rgb { r, g, b });
                }
            }
        }
        let mut dark_on_light_checked = false;
        let mut light_on_dark_checked = false;
        for &fg in &colors {
            for &bg in &colors {
                let c0 = contrast(fg, bg);
                for t in [0.25f32, 0.5] {
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
                // Track that BOTH polarities are genuinely exercised.
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
    }

    /// NEGATIVE CONTROL: the pre-fix gamma-space multiply toward black RAISES
    /// contrast for dark text on a light background (the audited polarity bug),
    /// so the law above genuinely distinguishes the fix from the bug.
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

    fn cell_with_flags(flags: CellFlags) -> Cell {
        let mut cell = Cell::default();
        cell.set_flags(flags);
        cell
    }

    fn cell_with_indexed_fg(index: u8, flags: CellFlags) -> Cell {
        let mut cell = Cell::default();
        cell.set_fg(PackedColor::indexed(index));
        cell.set_flags(flags);
        cell
    }

    fn cell_with_rgb_fg(r: u8, g: u8, b: u8, flags: CellFlags) -> (Cell, CellExtra) {
        let mut cell = Cell::default();
        cell.set_fg(PackedColor::rgb(r, g, b));
        cell.set_flags(flags);
        let mut extra = CellExtra::default();
        extra.set_fg_rgb(Some([r, g, b]));
        (cell, extra)
    }

    #[test]
    fn test_no_flags_returns_default_colors() {
        let cell = Cell::default();
        let pal = palette();
        let (fg, bg) = resolve_colors(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, false);
        assert_eq!(fg, DEFAULT_FG);
        assert_eq!(bg, DEFAULT_BG);
    }

    #[test]
    fn test_dim_ansi_color() {
        let cell = cell_with_indexed_fg(1, CellFlags::DIM);
        let pal = palette();
        let fg = resolve_fg_color(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, false);
        // W5(e): dim is the linear-light blend toward the cell bg (the default
        // bg here), at the default faint opacity.
        assert_eq!(fg, dim_toward_bg(pal.get(1), DEFAULT_BG, DIM_FACTOR));
        // On the black default bg the blend still darkens red text.
        let raw_red = pal.get(1);
        assert!(fg.r < raw_red.r, "dim red recedes toward black bg: {fg:?}");
    }

    #[test]
    fn test_dim_true_rgb() {
        let (cell, extra) = cell_with_rgb_fg(200, 100, 50, CellFlags::DIM);
        let pal = palette();
        let fg = resolve_fg_color(&cell, Some(&extra), &pal, DEFAULT_FG, DEFAULT_BG, false);
        let expected = dim_toward_bg(
            Rgb {
                r: 200,
                g: 100,
                b: 50,
            },
            DEFAULT_BG,
            DIM_FACTOR,
        );
        assert_eq!(fg, expected);
        // Halving in LINEAR light lands brighter than the old gamma-space
        // multiply (100, 50, 25) — pin the polarity fix's actual output.
        assert!(
            fg.r > 100,
            "linear-light dim is lighter than gamma dim: {fg:?}"
        );
    }

    /// W5(e) THE POLARITY FIX: on a LIGHT background, dim must make text
    /// LIGHTER (recede toward bg), not darker/heavier. The old gamma multiply
    /// toward black did the opposite — this is its regression test.
    #[test]
    fn test_dim_recedes_on_light_theme() {
        let (cell, extra) = cell_with_rgb_fg(40, 40, 40, CellFlags::DIM);
        let pal = palette();
        let light_bg = Rgb {
            r: 240,
            g: 240,
            b: 240,
        };
        // The cell sets no bg, so the light DEFAULT bg is what it sits on —
        // and therefore the blend target.
        let fg = resolve_fg_color(&cell, Some(&extra), &pal, DEFAULT_FG, light_bg, false);
        assert!(
            fg.r > 40 && fg.g > 40 && fg.b > 40,
            "dim dark-on-light text must move TOWARD the light bg (got {fg:?})"
        );
        // And the old behavior (multiply toward black) is the negative control:
        let old = Rgb {
            r: 20,
            g: 20,
            b: 20,
        };
        assert_ne!(fg, old, "the gamma-multiply polarity bug must be gone");
    }

    #[test]
    fn test_bold_promotes_ansi_to_bright() {
        let cell = cell_with_indexed_fg(3, CellFlags::BOLD);
        let pal = palette();
        let fg = resolve_fg_color(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, false);
        assert_eq!(fg, pal.get(11));
    }

    #[test]
    fn test_bold_no_change_already_bright() {
        let cell = cell_with_indexed_fg(10, CellFlags::BOLD);
        let pal = palette();
        let fg = resolve_fg_color(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, false);
        assert_eq!(fg, pal.get(10));
    }

    #[test]
    fn test_bold_no_change_true_rgb() {
        let (cell, extra) = cell_with_rgb_fg(100, 200, 50, CellFlags::BOLD);
        let pal = palette();
        let fg = resolve_fg_color(&cell, Some(&extra), &pal, DEFAULT_FG, DEFAULT_BG, false);
        assert_eq!(
            fg,
            Rgb {
                r: 100,
                g: 200,
                b: 50
            }
        );
    }

    #[test]
    fn test_bold_dim_dim_wins() {
        let cell = cell_with_indexed_fg(1, CellFlags::BOLD.union(CellFlags::DIM));
        let pal = palette();
        let fg = resolve_fg_color(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, false);
        let expected = dim_toward_bg(pal.get(1), DEFAULT_BG, DIM_FACTOR);
        assert_eq!(fg, expected);
    }

    /// W5(f): `bold_is_bright = false` keeps SGR 1 a pure weight change — the
    /// indexed 0–7 fg is NOT promoted to its bright sibling. The default policy
    /// (true) stays byte-identical to the historical unconditional promotion.
    #[test]
    fn test_bold_is_bright_off_keeps_base_color() {
        let cell = cell_with_indexed_fg(3, CellFlags::BOLD);
        let pal = palette();
        let opts_off = StyleResolveOpts {
            bold_is_bright: false,
            ..StyleResolveOpts::default()
        };
        let (fg, _) = resolve_colors_raw_opts(
            &cell, None, None, &pal, DEFAULT_FG, DEFAULT_BG, false, opts_off,
        );
        assert_eq!(fg, pal.get(3), "no promotion when the policy is off");
        let (fg_on, _) = resolve_colors_raw_opts(
            &cell,
            None,
            None,
            &pal,
            DEFAULT_FG,
            DEFAULT_BG,
            false,
            StyleResolveOpts::default(),
        );
        assert_eq!(fg_on, pal.get(11), "default policy still promotes");
    }

    #[test]
    fn test_inverse_swaps_colors() {
        let cell = cell_with_flags(CellFlags::INVERSE);
        let pal = palette();
        let (fg, bg) = resolve_colors(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, false);
        assert_eq!(fg, DEFAULT_BG);
        assert_eq!(bg, DEFAULT_FG);
    }

    #[test]
    fn test_decscnm_reverses() {
        let cell = Cell::default();
        let pal = palette();
        let (fg, bg) = resolve_colors(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, true);
        assert_eq!(fg, DEFAULT_BG);
        assert_eq!(bg, DEFAULT_FG);
    }

    #[test]
    fn test_decscnm_plus_inverse_cancel() {
        let cell = cell_with_flags(CellFlags::INVERSE);
        let pal = palette();
        let (fg, bg) = resolve_colors(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, true);
        assert_eq!(fg, DEFAULT_FG);
        assert_eq!(bg, DEFAULT_BG);
    }

    #[test]
    fn test_hidden_fg_equals_bg() {
        let cell = cell_with_flags(CellFlags::HIDDEN);
        let pal = palette();
        let (fg, bg) = resolve_colors(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, false);
        assert_eq!(fg, bg);
        assert_eq!(fg, DEFAULT_BG);
    }

    #[test]
    fn test_hidden_plus_inverse() {
        let cell = cell_with_flags(CellFlags::HIDDEN.union(CellFlags::INVERSE));
        let pal = palette();
        let (fg, bg) = resolve_colors(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, false);
        assert_eq!(fg, DEFAULT_FG);
        assert_eq!(bg, DEFAULT_FG);
    }

    #[test]
    fn test_all_flags_precedence() {
        let flags = CellFlags::BOLD
            .union(CellFlags::DIM)
            .union(CellFlags::INVERSE)
            .union(CellFlags::HIDDEN);
        let cell = cell_with_indexed_fg(2, flags);
        let pal = palette();
        let (fg, bg) = resolve_colors(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, false);
        let dimmed = dim_toward_bg(pal.get(2), DEFAULT_BG, DIM_FACTOR);
        assert_eq!(fg, dimmed);
        assert_eq!(bg, dimmed);
    }

    #[test]
    fn test_custom_default_colors() {
        let custom_fg = Rgb {
            r: 200,
            g: 200,
            b: 200,
        };
        let custom_bg = Rgb {
            r: 30,
            g: 30,
            b: 30,
        };
        let cell = Cell::default();
        let pal = palette();
        let (fg, bg) = resolve_colors(&cell, None, &pal, custom_fg, custom_bg, false);
        assert_eq!(fg, custom_fg);
        assert_eq!(bg, custom_bg);
    }

    #[test]
    fn test_extended_palette_color() {
        let cell = cell_with_indexed_fg(128, CellFlags::empty());
        let pal = palette();
        let fg = resolve_fg_color(&cell, None, &pal, DEFAULT_FG, DEFAULT_BG, false);
        assert_eq!(fg, pal.get(128));
        assert_eq!(
            fg,
            Rgb {
                r: 175,
                g: 0,
                b: 215
            }
        );
    }
}
