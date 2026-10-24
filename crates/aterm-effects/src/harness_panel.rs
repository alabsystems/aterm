// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cell ink colours and status-dot lights for the Claude Code agents panel.

use aterm_render::{HaloMode, RainHalo, over_rgb, premul_rgb};

use crate::color_math::{oklab, oklab_to_rgb, relative_luminance};
use crate::cursor_glow::InkRole;
use crate::effect_util::{TWINKLE_FLASH_BOUND_HZ, mix_rgb};
use crate::harness_card::{CardPalette, LINE_DARK_LUMA_FLOOR, lift_to_luminance, print_line};
use crate::rainbow_kitty::meteor::tri;
use crate::ripple::{GLINT_CREST_DARK, GLINT_CREST_LIGHT, glint};

/// How far along the palette a row's name runs from its stripe.
pub const NAME_SPAN: f32 = 0.5;

/// A dot's light radius at no work, in cell widths from the bullet centre.
pub const DOT_RADIUS_MIN: f32 = 0.55;
/// Additional dot light radius at full work, in cell widths.
pub const DOT_RADIUS_GAIN: f32 = 0.95;
/// Maximum dot light radius, in cell widths from the bullet centre.
pub const DOT_RADIUS_MAX: f32 = 1.5;
/// Maximum dot light height, in cell heights from the bullet centre.
pub const DOT_RISE_MAX: f32 = 0.9;
/// Minimum halo peak as a share of its ink at no work.
pub const HALO_PEAK_MIN: f32 = 0.25;
/// Additional halo peak share at full work.
pub const HALO_PEAK_GAIN: f32 = 0.55;
/// Minimum opacity ceiling of a light-ground tint veil at no work.
pub const TINT_ALPHA_MIN: f32 = 0.10;
/// Additional opacity ceiling of a light-ground tint veil at full work.
pub const TINT_ALPHA_GAIN: f32 = 0.20;
/// Minimum opacity ceiling of a light-ground hollow fill at no work.
pub const FILL_ALPHA_MIN: f32 = 0.30;
/// Additional opacity ceiling of a light-ground hollow fill at full work.
pub const FILL_ALPHA_GAIN: f32 = 0.55;
/// Hollow fill radius in cell widths from the bullet centre.
pub const FILL_RADIUS: f32 = 0.42;
const VEIL_ALPHA_FLOOR: u8 = 4;
/// Share of a running dot's light worn while waiting on a tool call.
pub const TOOL_GLOW: f32 = 0.5;
/// Radius of a tool-waiting dot's light inside its ring ([`FILL_RADIUS`]).
pub const TOOL_RADIUS: f32 = FILL_RADIUS;
const _: () = assert!(
    TOOL_RADIUS < DOT_RADIUS_MIN,
    "a tool dot's light stays in its ring"
);
/// Minimum WCAG luminance of a dark-ground dot's ink at no work.
pub const DOT_LUMA_MIN: f32 = LINE_DARK_LUMA_FLOOR;
/// Additional WCAG luminance of a dark-ground dot's ink at full work.
pub const DOT_LUMA_GAIN: f32 = 0.35;
/// Palest WCAG luminance of a running or quiet dot's ring on a light ground.
pub const PRINT_RING_LUMA: f32 = 0.22;
/// Share by which full work deepens a light-ground dot's ring luminance.
pub const DOT_DEEPEN_LIGHT: f32 = 0.15;
/// Minimum WCAG contrast of a light-ground dot's ring against its paper.
pub const PRINT_RING_CONTRAST: f32 = 3.2;
/// Share of a running dot's chroma kept by a quiet dot at full freshness.
pub const QUIET_KEEP: f32 = 0.55;
/// Claude Code's success green on dark and light themes.
pub const CLAUDE_SUCCESS: [u32; 2] = [0x004E_BA65, 0x002C_7A39];
/// Claude Code's error red on dark and light themes.
pub const CLAUDE_ERROR: [u32; 2] = [0x00FF_6B80, 0x00AB_2B3F];
/// Minimum OKLCh hue distance (degrees) from Claude's status green and red.
pub const STATUS_HUE_GUARD_DEG: f32 = 20.0;
/// Extra OKLCh hue margin (degrees) beyond [`STATUS_HUE_GUARD_DEG`] for [`dot_stripes`].
pub const STATUS_HUE_DRIFT_DEG: f32 = 4.0;
/// OKLab chroma threshold below which an ink is treated as neutral.
pub const HUE_CHROMA_GATE: f32 = 0.04;
/// Light radius of a stuck dot, in cell widths.
pub const STUCK_RADIUS: f32 = 0.7;
/// Amber colour of a dot awaiting approval on a dark ground.
pub const AWAITING_AMBER: u32 = 0x00FF_C107;
/// Amber print colour of a dot awaiting approval on a light ground.
pub const AWAITING_AMBER_PRINT: u32 = 0x0096_6C1E;

/// Period of one breath of a stuck dot, in milliseconds.
pub const BREATH_PERIOD_MS: u64 = 16 * BREATH_STEP_MS;
/// Step interval on which a stuck dot's breath is drawn, in milliseconds.
pub const BREATH_STEP_MS: u64 = crate::ripple::FLOW_STEP_MS;
/// Duration a stuck dot breathes before resting at [`BREATH_REST`], in milliseconds.
pub const BREATH_FOR_MS: u64 = 469 * BREATH_STEP_MS;
/// Minimum light share during a stuck dot's breath.
pub const BREATH_FLOOR: f32 = 0.55;
/// Resting light share of a stuck dot when settled or in reduced motion.
pub const BREATH_REST: f32 = BREATH_FLOOR + (1.0 - BREATH_FLOOR) / 2.0;
/// Palest WCAG luminance of a stuck dot's ring on a light ground.
pub const STUCK_PALE_LUMA_LIGHT: f32 = 1.05 / 4.0 - 0.05;

const _: () = {
    assert!(
        1000.0 / (BREATH_PERIOD_MS as f32) <= TWINKLE_FLASH_BOUND_HZ,
        "a stuck dot breathes under the photosensitivity bound"
    );
    assert!(
        BREATH_PERIOD_MS.is_multiple_of(BREATH_STEP_MS),
        "the breath's steps divide its period"
    );
    assert!(
        BREATH_FOR_MS.is_multiple_of(BREATH_STEP_MS),
        "the breath's rest falls on a step"
    );
    assert!(
        DOT_RADIUS_MIN + DOT_RADIUS_GAIN >= DOT_RADIUS_MAX && STUCK_RADIUS <= DOT_RADIUS_MAX,
        "the cap is what bounds a dot's light"
    );
    assert!(
        FILL_RADIUS < DOT_RADIUS_MIN && FILL_RADIUS < STUCK_RADIUS,
        "the hollow's fill lies inside every tint"
    );
    assert!(
        TINT_ALPHA_MIN + TINT_ALPHA_GAIN <= 1.0 && FILL_ALPHA_MIN + FILL_ALPHA_GAIN <= 1.0,
        "a veil's ceiling is an opacity"
    );
};

const STRIPES: usize = 6;

/// Kind of panel cell for ink colouring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelMark {
    /// A workflow bar's filled cell (`▰`, `█`).
    BarFill,
    /// A workflow bar's empty cell (`▱`, `░`).
    BarEmpty,
    /// A letter of a row's name.
    Name,
    /// The `❯` pointer of a selected row.
    Pointer,
}

/// OKLCh chroma and hue (degrees, `0..360`) of a `0x00RRGGBB` colour.
#[must_use]
#[inline]
pub fn chroma_hue(rgb: u32) -> (f32, f32) {
    let [_, a, b] = oklab(rgb);
    (a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0))
}

/// The angle between two hues, `0..=180` degrees.
#[inline]
fn hue_gap(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// OKLCh hue distance (degrees) from [`CLAUDE_SUCCESS`] and [`CLAUDE_ERROR`].
#[must_use]
#[inline]
pub fn status_gap(ink: u32, dark: bool) -> Option<f32> {
    static STATUS_HUES: std::sync::OnceLock<[[f32; 2]; 2]> = std::sync::OnceLock::new();
    let (c, h) = chroma_hue(ink);
    if c < HUE_CHROMA_GATE {
        return None;
    }
    let [hs, he] = STATUS_HUES.get_or_init(|| {
        [
            [
                chroma_hue(CLAUDE_SUCCESS[0]).1,
                chroma_hue(CLAUDE_ERROR[0]).1,
            ],
            [
                chroma_hue(CLAUDE_SUCCESS[1]).1,
                chroma_hue(CLAUDE_ERROR[1]).1,
            ],
        ]
    })[usize::from(!dark)];
    Some(hue_gap(h, hs).min(hue_gap(h, he)))
}

type DotStripes = ([usize; STRIPES], usize);

fn compute_dot_stripes(palette: CardPalette, dark: bool) -> DotStripes {
    let mut out = [0; STRIPES];
    let mut n = 0;
    for k in 0..STRIPES {
        let art = palette.art(palette.stripe_t(k), dark);
        if status_gap(art, dark).is_none_or(|g| g >= STATUS_HUE_GUARD_DEG + STATUS_HUE_DRIFT_DEG) {
            out[n] = k;
            n += 1;
        }
    }
    if n == 0 {
        return ([0, 1, 2, 3, 4, 5], STRIPES);
    }
    (out, n)
}

/// Palette stripe indices safe for dots on a dark or light ground.
#[must_use]
#[inline]
pub fn dot_stripes(palette: CardPalette, dark: bool) -> DotStripes {
    static DOT_STRIPES: std::sync::OnceLock<[[DotStripes; 2]; 3]> = std::sync::OnceLock::new();
    let table = DOT_STRIPES.get_or_init(|| {
        [CardPalette::Cruise, CardPalette::Land, CardPalette::Off]
            .map(|p| [true, false].map(|d| compute_dot_stripes(p, d)))
    });
    let p_idx = match palette {
        CardPalette::Cruise => 0,
        CardPalette::Land => 1,
        CardPalette::Off => 2,
    };
    table[p_idx][usize::from(!dark)]
}

/// Palette position for panel row `k` on a dark or light ground.
#[must_use]
#[inline]
pub fn row_hue(palette: CardPalette, k: usize, dark: bool) -> f32 {
    if k == 0 {
        return palette.stripe_t(0);
    }
    let (stripes, n) = dot_stripes(palette, dark);
    palette.stripe_t(stripes[(k - 1) % n])
}

/// Palette position for letter `j` of a `len`-letter name on a row at `row_t`.
#[must_use]
#[inline]
pub fn name_x(row_t: f32, j: usize, len: usize) -> f32 {
    let along = if len > 1 {
        j as f32 / (len - 1) as f32
    } else {
        0.0
    };
    tri(row_t + NAME_SPAN * along)
}

/// Palette position for cell `i` of a `cells`-wide workflow bar.
#[must_use]
#[inline]
pub const fn bar_x(i: usize, cells: usize) -> f32 {
    if cells > 1 {
        (i as f32 / (cells - 1) as f32).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Mascot ripple crest (`0..=1`) at `d_cells` cells from the bar start at `ms`.
#[must_use]
#[inline]
pub fn crest(d_cells: f32, ms: u64) -> f32 {
    glint(d_cells, ms)
}

/// A linear-light channel back to a byte.
#[inline]
fn srgb(l: f32) -> u32 {
    let l = l.clamp(0.0, 1.0);
    let n = if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (n * 255.0).round().clamp(0.0, 255.0) as u32
}

/// Returns `ink`'s hue at `target`'s WCAG luminance ([`at_luminance`]).
#[must_use]
#[inline]
pub fn match_luminance(ink: u32, target: u32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let lc = crate::color_math::relative_luminance_with(table, ink);
    if lc <= 1e-6 {
        return target & 0x00FF_FFFF;
    }
    at_luminance_with(
        table,
        ink,
        lc,
        crate::color_math::relative_luminance_with(table, target),
    )
}

/// Returns `ink`'s hue scaled or lifted to WCAG luminance `lt` in linear light.
#[must_use]
#[inline]
pub fn at_luminance(ink: u32, lt: f32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let lc = crate::color_math::relative_luminance_with(table, ink);
    at_luminance_with(table, ink, lc, lt)
}

#[inline]
fn at_luminance_with(
    table: &crate::color_math::RelativeLuminanceTable,
    ink: u32,
    lc: f32,
    lt: f32,
) -> u32 {
    let lt = lt.clamp(0.0, 1.0);
    if lc <= 1e-6 {
        let g = srgb(lt);
        return (g << 16) | (g << 8) | g;
    }
    let r = table.linear[((ink >> 16) & 0xff) as usize];
    let g = table.linear[((ink >> 8) & 0xff) as usize];
    let b = table.linear[(ink & 0xff) as usize];
    at_luminance_linear(r, g, b, lc, lt)
}

#[inline]
fn at_luminance_linear(r: f32, g: f32, b: f32, lc: f32, lt: f32) -> u32 {
    let lt = lt.clamp(0.0, 1.0);
    if lc <= 1e-6 {
        let g = srgb(lt);
        return (g << 16) | (g << 8) | g;
    }
    if lt <= lc {
        let s = lt / lc;
        (srgb(r * s) << 16) | (srgb(g * s) << 8) | srgb(b * s)
    } else {
        let k = (lt - lc) / (1.0 - lc).max(1e-6);
        let lift = |c: f32| srgb(c + (1.0 - c) * k);
        (lift(r) << 16) | (lift(g) << 8) | lift(b)
    }
}

/// Scales `ink`'s OKLab chroma by `keep` (`0..=1`) while holding lightness and hue.
#[must_use]
#[inline]
pub fn fade_chroma(ink: u32, keep: f32) -> u32 {
    let keep = if keep.is_finite() {
        keep.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if keep >= 1.0 {
        return ink & 0x00FF_FFFF;
    }
    let [l, a, b] = oklab(ink);
    oklab_to_rgb([l, a * keep, b * keep])
}

/// Returns `ink` at WCAG luminance `lt` while preserving its OKLab hue.
#[must_use]
#[inline]
pub fn at_luminance_hue(ink: u32, lt: f32) -> u32 {
    let lt = lt.clamp(0.0, 1.0);
    let table = crate::color_math::relative_luminance_table();
    let lc = crate::color_math::relative_luminance_with(table, ink);
    if lt <= lc || lc <= 1e-6 {
        return at_luminance_with(table, ink, lc, lt);
    }
    let [l0, a0, b0] = oklab(ink);
    let at = |t: f32| oklab_to_rgb([l0 + (1.0 - l0) * t, a0 * (1.0 - t), b0 * (1.0 - t)]);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (ink & 0x00FF_FFFF, 0x00FF_FFFF);
    for _ in 0..12 {
        let mid = (lo + hi) * 0.5;
        let mid_rgb = at(mid);
        if mid_rgb == lo_rgb {
            lo = mid;
            continue;
        }
        if mid_rgb == hi_rgb {
            hi = mid;
            continue;
        }
        if crate::color_math::relative_luminance_with(table, mid_rgb) < lt {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
        if lo_rgb == hi_rgb {
            break;
        }
    }
    hi_rgb
}

/// A light-ground dot's ring: `fresh` (the dot's hue, darkened by
/// [`at_luminance`]'s scaling, so hue-true) at the palest luminance up to
/// `top` whose contrast with `ground` holds [`PRINT_RING_CONTRAST`] with both
/// under the dot's own tint at its ceiling `veil` (`over_rgb`, the renderer's
/// source-over: the ring lies inside the tint's plateau, never under more of
/// it).
#[inline]
fn print_ring(fresh: u32, ground: u32, veil: u8, top: f32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let g = over_rgb(ground, fresh, veil) & 0x00FF_FFFF;
    let lg = crate::color_math::relative_luminance_with(table, g);
    let lc = crate::color_math::relative_luminance_with(table, fresh);
    let r_lin = table.linear[((fresh >> 16) & 0xff) as usize];
    let g_lin = table.linear[((fresh >> 8) & 0xff) as usize];
    let b_lin = table.linear[(fresh & 0xff) as usize];
    let ring = |l: f32| at_luminance_linear(r_lin, g_lin, b_lin, lc, l);
    let holds_rgb = |ring_rgb: u32| {
        let r = over_rgb(ring_rgb, fresh, veil) & 0x00FF_FFFF;
        let lr = crate::color_math::relative_luminance_with(table, r);
        (lr.max(lg) + 0.05) / (lr.min(lg) + 0.05) >= PRINT_RING_CONTRAST
    };
    let top = top.min(lc).max(0.0);
    let top_rgb = ring(top);
    if holds_rgb(top_rgb) {
        return top_rgb;
    }
    let (mut lo, mut hi) = (0.0f32, top);
    let (mut lo_rgb, mut hi_rgb) = (ring(0.0), top_rgb);
    for _ in 0..12 {
        let mid = (lo + hi) * 0.5;
        let mid_rgb = ring(mid);
        if mid_rgb == lo_rgb {
            lo = mid;
            continue;
        }
        if mid_rgb == hi_rgb {
            hi = mid;
            continue;
        }
        if holds_rgb(mid_rgb) {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
        if lo_rgb == hi_rgb {
            break;
        }
    }
    lo_rgb
}

/// A `0..=1` share as a byte of opacity.
#[inline]
fn alpha_byte(share: f32) -> u8 {
    (share.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Computes the ink colour for a panel cell of kind `mark` at palette position `x`.
#[must_use]
#[inline]
pub fn panel_ink(
    palette: CardPalette,
    mark: PanelMark,
    x: f32,
    dark: bool,
    cell_fg: u32,
    sat: f32,
    crest: f32,
) -> u32 {
    match mark {
        PanelMark::BarFill => {
            let art = palette.art(x, dark);
            let base = if dark {
                lift_to_luminance(art, LINE_DARK_LUMA_FLOOR)
            } else {
                print_line(art)
            };
            let d = crest.clamp(0.0, 1.0);
            if d <= 0.0 {
                base
            } else if dark {
                mix_rgb(base, 0x00FF_FFFF, d * GLINT_CREST_DARK)
            } else {
                mix_rgb(base, 0, d * GLINT_CREST_LIGHT / 2.0)
            }
        }
        PanelMark::BarEmpty | PanelMark::Name | PanelMark::Pointer => {
            let letter = palette.letter(x, dark);
            let fresh = mix_rgb(letter, cell_fg, 1.0 - sat.clamp(0.0, 1.0));
            match_luminance(fresh, cell_fg)
        }
    }
}

/// Activity state of an agent dot for rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DotMood {
    /// Evidence of recent work: lit.
    Running,
    /// No recent evidence: light fades with freshness.
    Quiet,
    /// Waiting on a tool call within budget: dimmed to [`TOOL_GLOW`] inside [`TOOL_RADIUS`].
    Tool,
    /// No evidence for a long time: grey and breathing.
    Stuck,
    /// Awaiting approval: a still amber ring.
    Awaiting,
    /// Done, failed, halted, or idle: Claude's own ink with no light.
    Still,
}

/// Radial light parameters for an agent dot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DotLight {
    pub color: u32,
    pub radius: f32,
    pub strength: f32,
    pub fill: f32,
}

/// Ink override and radial light for an agent dot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DotLook {
    pub ink: Option<u32>,
    pub light: Option<DotLight>,
}

/// Breath factor (`BREATH_FLOOR..=1.0`) of a stuck dot `ms` after breathing started.
#[must_use]
#[inline]
pub fn breath(ms: Option<u64>) -> f32 {
    let Some(ms) = ms.filter(|&ms| ms < BREATH_FOR_MS) else {
        return BREATH_REST;
    };
    let t = (ms - ms % BREATH_STEP_MS) % BREATH_PERIOD_MS;
    let phase = std::f32::consts::TAU * t as f32 / BREATH_PERIOD_MS as f32;
    BREATH_FLOOR + (1.0 - BREATH_FLOOR) * (1.0 - phase.cos()) / 2.0
}

/// Computes the ink and radial light for the dot on panel row `k`.
#[must_use]
#[inline]
#[allow(clippy::too_many_arguments)]
pub fn dot_look(
    mood: DotMood,
    level: f32,
    sat: f32,
    palette: CardPalette,
    k: usize,
    dark: bool,
    ground: u32,
    breath: f32,
    serious: bool,
) -> DotLook {
    match mood {
        DotMood::Still => {
            return DotLook {
                ink: None,
                light: None,
            };
        }
        DotMood::Awaiting => {
            let ink = if dark {
                AWAITING_AMBER
            } else {
                AWAITING_AMBER_PRINT
            };
            return DotLook {
                ink: Some(ink),
                light: None,
            };
        }
        DotMood::Running | DotMood::Quiet | DotMood::Tool | DotMood::Stuck => {}
    }
    let level = if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let sat = if sat.is_finite() {
        sat.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let breath = breath.clamp(BREATH_FLOOR, 1.0);
    let strength_at = |level: f32| {
        if dark {
            HALO_PEAK_MIN + HALO_PEAK_GAIN * level
        } else {
            TINT_ALPHA_MIN + TINT_ALPHA_GAIN * level
        }
    };
    let fill_at = |level: f32| {
        if dark {
            0.0
        } else {
            FILL_ALPHA_MIN + FILL_ALPHA_GAIN * level
        }
    };
    let radius_at = |level: f32| (DOT_RADIUS_MIN + DOT_RADIUS_GAIN * level).min(DOT_RADIUS_MAX);
    let hue = palette.art(row_hue(palette, k, dark), dark);
    let (ink, light) = match mood {
        DotMood::Running | DotMood::Quiet | DotMood::Tool => {
            let quiet = mood == DotMood::Quiet;
            let tool = mood == DotMood::Tool;
            let fresh = fade_chroma(hue, if quiet { QUIET_KEEP * sat } else { 1.0 });
            let fade = match mood {
                DotMood::Quiet => sat,
                DotMood::Tool => TOOL_GLOW,
                _ => 1.0,
            };
            let strength = strength_at(level) * fade;
            let radius = if tool { TOOL_RADIUS } else { radius_at(level) };
            let ink = if dark {
                at_luminance_hue(fresh, DOT_LUMA_MIN + DOT_LUMA_GAIN * level)
            } else {
                let top = PRINT_RING_LUMA * (1.0 - DOT_DEEPEN_LIGHT * level);
                print_ring(fresh, ground, alpha_byte(strength), top)
            };
            (
                ink,
                DotLight {
                    color: fresh,
                    radius,
                    strength,
                    fill: fill_at(level) * fade,
                },
            )
        }
        DotMood::Stuck => {
            let fresh = fade_chroma(hue, sat);
            let ink = if dark {
                at_luminance_hue(fresh, DOT_LUMA_MIN * breath)
            } else {
                let base = InkRole::OverText.ink(fresh);
                let lb = relative_luminance(base);
                let pale = (1.0 - breath) / (1.0 - BREATH_FLOOR);
                at_luminance(base, lb + (STUCK_PALE_LUMA_LIGHT.max(lb) - lb) * pale)
            };
            (
                ink,
                DotLight {
                    color: fresh,
                    radius: STUCK_RADIUS,
                    strength: strength_at(0.0) * breath,
                    fill: 0.0,
                },
            )
        }
        DotMood::Still | DotMood::Awaiting => unreachable!(),
    };
    DotLook {
        ink: Some(ink),
        light: (!serious).then_some(light),
    }
}

/// Window-pixel geometry and clip bounds of an agent dot cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DotPlace {
    pub cx: u16,
    pub cy: u16,
    pub cw: u16,
    pub ch: u16,
    pub row: u16,
    pub clip: (u16, u16),
}

/// Pushes radial halo or veil quads for `light` at `place` onto `halos`.
#[inline]
pub fn dot_light_quads(light: &DotLight, dark: bool, place: DotPlace, halos: &mut Vec<RainHalo>) {
    let (cw, ch) = (f32::from(place.cw), f32::from(place.ch));
    let rx = (light.radius.min(DOT_RADIUS_MAX) * cw).max(1.0);
    let ry = rx.min(DOT_RISE_MAX * ch).max(1.0);
    let alpha = alpha_byte(light.strength);
    let color = light.color & 0x00FF_FFFF;
    if dark {
        if alpha > 0 {
            let peak = premul_rgb(color, alpha);
            push_radial(halos, place, peak, (rx, ry), HaloMode::Add);
        }
        return;
    }
    if alpha >= VEIL_ALPHA_FLOOR {
        let veil = (u32::from(alpha) << 24) | color;
        push_radial(halos, place, veil, (rx, ry), HaloMode::Over);
    }
    let fill = alpha_byte(light.fill);
    if fill >= VEIL_ALPHA_FLOOR {
        let r = (FILL_RADIUS * cw).max(1.0);
        let veil = (u32::from(fill) << 24) | color;
        push_radial(halos, place, veil, (r, r.min(ch / 2.0)), HaloMode::Over);
    }
}

/// One radial light of `color` (the stream's word for `mode`) about the
/// bullet at `place`, `(rx, ry)` pixels, as one quad per row band it reaches
/// inside the clip.
#[inline]
fn push_radial(
    halos: &mut Vec<RainHalo>,
    place: DotPlace,
    color: u32,
    (rx, ry): (f32, f32),
    mode: HaloMode,
) {
    let (cx, cy) = (i32::from(place.cx), i32::from(place.cy));
    let (clip0, clip1) = (i32::from(place.clip.0), i32::from(place.clip.1));
    let ch_px = i32::from(place.ch);
    let band_top = cy - (f32::from(place.ch) / 2.0).round() as i32;
    let (rxi, ryi) = (rx.round() as i32, ry.round() as i32);
    let px = |v: i32| u16::try_from(v.max(0)).unwrap_or(u16::MAX);
    let x = px(cx - rxi);
    let w = px(2 * rxi);
    let rx_px = px(rxi).max(1);
    let ry_px = px(ryi).max(1);
    for band in -1i32..=1 {
        let Some(row) = i32::from(place.row)
            .checked_add(band)
            .and_then(|r| u16::try_from(r).ok())
        else {
            continue;
        };
        let top = band_top + band * ch_px;
        let y0 = top.max(cy - ryi).max(clip0);
        let y1 = (top + ch_px).min(cy + ryi).min(clip1);
        if y1 <= y0 {
            continue;
        }
        halos.push(RainHalo {
            row,
            x,
            y: px(y0),
            w,
            h: px(y1 - y0),
            color,
            cx: place.cx,
            cy: place.cy,
            rx: rx_px,
            ry: ry_px,
            mode,
        });
    }
}

#[cfg(test)]
mod tests;
