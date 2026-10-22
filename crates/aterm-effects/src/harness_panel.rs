// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AGENTS PANEL'S GLASS, pure: what colour each cell of Claude Code's
//! agents panel (`◯ general-purpose  …  4m 28s · ↓ 115.4k tokens`, `◯ sweep
//! ▰▰▰▰▱▱  3/4 · …`) is inked in, and what light each agent's dot wears,
//! while the harness's theme is on (owner, 2026-10-19: "for more bling, aterm
//! bling the subagents display at the bottom. I want to see rainbow" — "and
//! some more informative coloring about the agent status in those dots …
//! I want to know if the subagent is stuck or running and how much work it is
//! doing in some visual indication of brightness and glow and maybe
//! saturation"). The host (`aterm-gui`'s `claude_panel`) places the cells and
//! folds the dots' states (`aterm_agent::harness::agents`); nothing here reads
//! a clock, a screen or a config.
//!
//! # The rainbow
//!
//! The main row takes the palette's first stripe; the task rows below it take
//! the palette's DOT HUES top down ([`row_hue`], [`dot_stripes`]: Cruise's
//! spectrum, Land's sunset, Off's moonbow — the card's and the border's
//! palette, `CardPalette` — less every stripe whose hue lies near Claude's own
//! success green or error red), so the panel reads as one vertical rainbow
//! and a dot never wears done's or failed's colour. Each row's NAME runs on
//! from its row's hue across [`NAME_SPAN`] of the palette ([`name_x`]), the
//! whole rainbow included; a workflow's BAR runs the whole palette from its
//! first cell to its last ([`bar_x`]), so a fuller bar shows more of the
//! rainbow. Text (names, the `❯` pointer, the bar's empty cells) keeps
//! CLAUDE'S LIGHTNESS ([`match_luminance`]): the hue is the palette's, the
//! luminance the one Claude drew the cell in, so Claude's contrast and its
//! dim/bold hierarchy stand on every theme by construction. The bar's filled
//! cells are shapes and go vivid (the card's line ink, lifted on dark; the
//! card's print on light), and only they carry the crest of Claude's mascot
//! ripple ([`crest`], `crate::ripple`): text never shimmers.
//!
//! # The dots
//!
//! Each agent's or workflow's bullet says what the fold says of it
//! ([`dot_look`]): HUE = which agent (its row's hue); SATURATION = freshness
//! (a quiet dot keeps [`QUIET_KEEP`] of its chroma times its freshness, a
//! stuck one the fold's tenth: nearly grey; faded in OKLab, so the hue never
//! turns); BRIGHTNESS and GLOW = how much work (its ink lifted, and a light
//! about it that grows with the work rate). A running dot reads LIT — its `◯`
//! filled with light; a quiet one's light fades with its freshness; one
//! WAITING ON A TOOL CALL of its own, inside that call's budget, keeps its
//! full hue with a dimmer light ([`TOOL_GLOW`]) held INSIDE its ring
//! ([`TOOL_RADIUS`]: a lit core, no glow past the ring, on paper its hollow
//! half lit) — live, but doing no token work, and told by its light's shape
//! from a running dot (whose light spreads past the ring) and from an
//! awaiting one (no light); a STUCK one is grey, hollow and BREATHES
//! ([`breath`], 0.5 Hz, settling after [`BREATH_FOR_MS`]); one AWAITING approval is a still amber ring with no
//! light (light is work, and it does none); one Claude marked done or
//! failed, halted or waiting keeps Claude's own ink and no light
//! ([`DotMood::Still`]). So LIT says live and UNLIT says a status, whatever
//! the hue: the amber lies within 5° of Land's gold and Cruise's print gold. On a dark ground the light is an additive
//! halo over the bullet ([`RainHalo`], [`HaloMode::Add`], never wider than
//! [`DOT_RADIUS_MAX`] cells, so it never reaches the pointer, the tree or the
//! name); on a light one, where added light is invisible, the dot is printed:
//! its ring the hue as a print ink solved to hold [`PRINT_RING_CONTRAST`]
//! against the paper, its hollow filled with the hue by the work, and round it
//! a soft tint of the hue (a source-over veil, [`HaloMode::Over`]) whose
//! radius grows with the work exactly as the dark halo's does
//! ([`dot_light_quads`]). Serious mode keeps the state ink and drops every
//! light and every breath.
//!
//! # Photosensitivity
//!
//! The crest passes a cell at most twice a ripple cycle (`crate::ripple`'s
//! bound); the breath is one swell every [`BREATH_PERIOD_MS`] (0.5 Hz):
//! both under the crate's certified `TWINKLE_FLASH_BOUND_HZ` (const-asserted).

use aterm_render::{HaloMode, RainHalo, over_rgb, premul_rgb};

use crate::color_math::{oklab, oklab_to_rgb, relative_luminance};
use crate::cursor_glow::InkRole;
use crate::effect_util::{TWINKLE_FLASH_BOUND_HZ, mix_rgb};
use crate::harness_card::{CardPalette, LINE_DARK_LUMA_FLOOR, lift_to_luminance, print_line};
use crate::rainbow_kitty::meteor::tri;
use crate::ripple::{GLINT_CREST_DARK, GLINT_CREST_LIGHT, glint};

/// How far along the palette a row's name runs from its stripe.
pub const NAME_SPAN: f32 = 0.5;

/// A dot's light at no work, in cell widths from the bullet's centre.
pub const DOT_RADIUS_MIN: f32 = 0.55;
/// How much wider full work makes it.
pub const DOT_RADIUS_GAIN: f32 = 0.95;
/// The widest a dot's light ever is, in cell widths: the pointer, the tree
/// glyph and the name all start 1.5 cells from the bullet's centre.
pub const DOT_RADIUS_MAX: f32 = 1.5;
/// A dot's light is at most this many cell HEIGHTS tall from its centre.
pub const DOT_RISE_MAX: f32 = 0.9;
/// A halo's peak, as a share of its ink, at no work…
pub const HALO_PEAK_MIN: f32 = 0.25;
/// …and how much more full work adds.
pub const HALO_PEAK_GAIN: f32 = 0.55;
/// A light ground's round tint about a dot (a source-over veil): its opacity
/// ceiling at no work…
pub const TINT_ALPHA_MIN: f32 = 0.10;
/// …and how much more full work adds.
pub const TINT_ALPHA_GAIN: f32 = 0.20;
/// The light filling a dot's hollow on a light ground (a lit dot): its
/// opacity ceiling at no work…
pub const FILL_ALPHA_MIN: f32 = 0.30;
/// …and how much more full work adds.
pub const FILL_ALPHA_GAIN: f32 = 0.55;
/// The hollow's fill, in cell widths from the bullet's centre: inside the
/// `◯`'s ring, which runs about 0.36 to 0.5 cell widths out in the bundled
/// faces (JetBrains Mono with Noto Sans Symbols 2, measured in the preview).
pub const FILL_RADIUS: f32 = 0.42;
/// A veil's ceiling under this (of 255) is not drawn: a ceiling of `0` reads
/// as none at all (`aterm_render::halo_over_cap`), and so faint a veil shows
/// nothing.
const VEIL_ALPHA_FLOOR: u8 = 4;
/// A dot waiting on its own tool call wears this share of a running dot's
/// light at the same work (its strength, and on paper its hollow's fill): a
/// vivid ring that waits, dimmer than any running dot and, unlike a stuck
/// one, in its full hue and still.
pub const TOOL_GLOW: f32 = 0.5;
/// …and holds that light inside its ring: a core of the hollow's own radius
/// ([`FILL_RADIUS`]), never the halo a running dot's work spreads past the
/// ring ([`DOT_RADIUS_MIN`] and up). The SHAPE tells it apart where a hue
/// cannot: on dark from a running dot of a near hue at a lower level (its
/// halo's alpha alone was 32 to a running dot's 64), on paper from an
/// awaiting ring (Cruise's gold Tool ring and the awaiting amber print lie
/// 0.051 apart in OKLab), whose hollow stays bare.
pub const TOOL_RADIUS: f32 = FILL_RADIUS;
const _: () = assert!(
    TOOL_RADIUS < DOT_RADIUS_MIN,
    "a tool dot's light stays in its ring"
);
/// A dark-ground dot's ink is drawn at this WCAG luminance at no work (the
/// card line's floor: about 5:1 on a near-black ground)…
pub const DOT_LUMA_MIN: f32 = LINE_DARK_LUMA_FLOOR;
/// …and this much more at full work.
pub const DOT_LUMA_GAIN: f32 = 0.35;
/// The palest a running or quiet dot's ring is on a light ground, as a WCAG
/// luminance: a print ink of its hue, never a pastel…
pub const PRINT_RING_LUMA: f32 = 0.22;
/// …which full work deepens by this share of its luminance.
pub const DOT_DEEPEN_LIGHT: f32 = 0.15;
/// The contrast a light-ground dot's ring holds against its paper, the two
/// under the dot's own tint at its ceiling: a graphic's 3:1 and a margin.
pub const PRINT_RING_CONTRAST: f32 = 3.2;
/// The share of its chroma a QUIET dot keeps at full freshness: its chroma is
/// `QUIET_KEEP · freshness` of a running dot's, so quiet reads paler from its
/// first second (90 s quiet, freshness 0.54: 0.30 of it).
pub const QUIET_KEEP: f32 = 0.55;
/// Claude Code's success green, on a dark and on a light theme (read in the
/// 2.1.271 bundle's theme tables: `success:"rgb(78,186,101)"`,
/// `success:"rgb(44,122,57)"`): a done agent's bullet.
pub const CLAUDE_SUCCESS: [u32; 2] = [0x004E_BA65, 0x002C_7A39];
/// Claude Code's error red, dark then light (`error:"rgb(255,107,128)"`,
/// `error:"rgb(171,43,63)"`): a failed agent's bullet.
pub const CLAUDE_ERROR: [u32; 2] = [0x00FF_6B80, 0x00AB_2B3F];
/// The nearest a live dot's ink or light comes to the hue (OKLCh, degrees) of
/// Claude's success green or error red: a dot never mimics done or failed.
pub const STATUS_HUE_GUARD_DEG: f32 = 20.0;
/// What a stripe's own hue keeps beyond [`STATUS_HUE_GUARD_DEG`] to be a dot
/// hue ([`dot_stripes`]): more than the most a dot's lift and fade turn a hue
/// (both hue-true in OKLab but for the gamut's edge, 3.7° at most).
pub const STATUS_HUE_DRIFT_DEG: f32 = 4.0;
/// Under this OKLab chroma an ink has no hue to mistake (Off's moonbow lies
/// at 0.015 to 0.028).
pub const HUE_CHROMA_GATE: f32 = 0.04;
/// A stuck dot's light, in cell widths (fixed: a stuck agent does no work).
pub const STUCK_RADIUS: f32 = 0.7;
/// The amber of a dot awaiting approval on a dark ground: Claude's own
/// `auto mode` pill's (measured on 2.1.304: `ESC[38;2;255;193;7m`), its dark
/// theme's `warning` (2.1.271: `warning:"rgb(255,193,7)"`).
pub const AWAITING_AMBER: u32 = 0x00FF_C107;
/// …and on a light one: Claude's light theme's `warning`
/// (`warning:"rgb(150,108,30)"`), as its done and failed keep its
/// `success` and `error`: 4.2:1 on Linen, an ochre ring where the over-text
/// ink of the amber (`#5A4402`) read as a dark ring beside the stuck one's.
pub const AWAITING_AMBER_PRINT: u32 = 0x0096_6C1E;

/// One breath of a stuck dot, in ms (sixteen steps, 0.49 Hz).
pub const BREATH_PERIOD_MS: u64 = 16 * BREATH_STEP_MS;
/// The step a breath is drawn on, in ms: the flow's
/// ([`crate::ripple::FLOW_STEP_MS`]), so a panel whose bar crests while a
/// dot breathes — and a card flowing beside it — steps once for all of
/// them (the host starts the breath's course on the crest's grid).
pub const BREATH_STEP_MS: u64 = crate::ripple::FLOW_STEP_MS;
/// How long a stuck dot breathes before it rests ([`BREATH_REST`]), a
/// minute on the steps' grid; the host re-arms it when the window regains
/// focus.
pub const BREATH_FOR_MS: u64 = 469 * BREATH_STEP_MS;
/// The breath's floor: a stuck dot's light never falls under this share.
pub const BREATH_FLOOR: f32 = 0.55;
/// The breath at rest (settled, reduced motion, serious mode): the middle of
/// its swing.
pub const BREATH_REST: f32 = BREATH_FLOOR + (1.0 - BREATH_FLOOR) / 2.0;
/// The palest a stuck dot's breath takes its ring on a LIGHT ground, as a
/// WCAG luminance: 4:1 on white, so the ring holds 3:1 (a graphic's floor)
/// on any paper at luminance 0.74 or more (Aterm Day 0.95, Linen 0.90), its
/// faint tint over it too. A paler trough (the ring mixed 45 % toward
/// white) fell to 2.6:1 on Linen.
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

/// How many stripes a palette has (`CardPalette::stripe_t` wraps after them).
const STRIPES: usize = 6;

/// What a panel cell is, for its ink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelMark {
    /// A workflow bar's filled cell (`▰`, `█`): a shape, vivid, crested.
    BarFill,
    /// A workflow bar's empty cell (`▱`, `░`): Claude's dim, in the hue.
    BarEmpty,
    /// A letter of a row's name.
    Name,
    /// The `❯` of a selected row.
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

/// How far (OKLCh hue, degrees) `ink` lies from the nearer of Claude's
/// success green and error red ([`CLAUDE_SUCCESS`], [`CLAUDE_ERROR`]) on a
/// dark or a light theme; `None` where `ink` has no hue to mistake (chroma
/// under [`HUE_CHROMA_GATE`]).
#[must_use]
#[inline]
pub fn status_gap(ink: u32, dark: bool) -> Option<f32> {
    let (c, h) = chroma_hue(ink);
    if c < HUE_CHROMA_GATE {
        return None;
    }
    let i = usize::from(!dark);
    Some(
        [CLAUDE_SUCCESS[i], CLAUDE_ERROR[i]]
            .into_iter()
            .map(|s| hue_gap(h, chroma_hue(s).1))
            .fold(f32::MAX, f32::min),
    )
}

/// THE DOT HUES: the stripes of `palette` a dot may wear on a dark or a light
/// ground, in palette order (the first `n` of the array): each whose art lies
/// [`STATUS_HUE_GUARD_DEG`] and [`STATUS_HUE_DRIFT_DEG`] or more from
/// Claude's status hues ([`status_gap`]), so no ink its dot wears comes
/// within the guard. Cruise keeps its orange, yellow (gold in print), blue
/// and violet: its red lies 15° or less from Claude's error red, its green 6°
/// or less from its success green. Land keeps its vermilion, orange, gold
/// and apricot on dark, its orange and gold in print; Off, colourless, all
/// six. Every stripe where none keeps clear.
#[must_use]
#[inline]
pub fn dot_stripes(palette: CardPalette, dark: bool) -> ([usize; STRIPES], usize) {
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

/// Row `k`'s place on `palette` on a dark or a light ground (`k` counted down
/// the panel from its first row, the main row): the main row's is the
/// palette's first stripe; task row `k` takes the `k − 1`th of the dot hues
/// ([`dot_stripes`]), wrapping, so its dot, its pointer and its name's first
/// letter share one hue, and adjacent rows differ.
#[must_use]
#[inline]
pub fn row_hue(palette: CardPalette, k: usize, dark: bool) -> f32 {
    if k == 0 {
        return palette.stripe_t(0);
    }
    let (stripes, n) = dot_stripes(palette, dark);
    palette.stripe_t(stripes[(k - 1) % n])
}

/// Letter `j` of a `len`-letter name on a row at `row_t`: from the row's
/// stripe on across [`NAME_SPAN`] of the palette, folded back at its end
/// (Cruise's violet runs back toward blue, never jumps to red).
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

/// Cell `i` of a `cells`-wide bar: the whole palette, first cell to last.
#[must_use]
#[inline]
pub const fn bar_x(i: usize, cells: usize) -> f32 {
    if cells > 1 {
        (i as f32 / (cells - 1) as f32).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The crest of Claude's mascot ripple at `d_cells` cells from the bar's
/// first cell, `ms` into the ripple's clock (`crate::ripple::glint`, the
/// ring train): `0..=1` in eighths.
#[must_use]
#[inline]
pub fn crest(d_cells: f32, ms: u64) -> f32 {
    glint(d_cells, ms)
}

/// `0x00RRGGBB` channel `sh` in linear light (the WCAG transfer the crate's
/// [`relative_luminance`] uses).
#[inline]
fn lin(rgb: u32, sh: u32) -> f32 {
    let n = ((rgb >> sh) & 0xff) as f32 / 255.0;
    if n <= 0.039_28 {
        n / 12.92
    } else {
        ((n + 0.055) / 1.055).powf(2.4)
    }
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

/// `ink`'s hue with `target`'s luminance — CLAUDE'S LIGHTNESS ([`at_luminance`]).
/// A black ink takes the target itself (no hue to keep).
#[must_use]
#[inline]
pub fn match_luminance(ink: u32, target: u32) -> u32 {
    if relative_luminance(ink) <= 1e-6 {
        return target & 0x00FF_FFFF;
    }
    at_luminance(ink, relative_luminance(target))
}

/// `ink`'s hue at WCAG luminance `lt`, in closed form in linear light (`Lc`
/// the ink's luminance): scaled by `lt / Lc` where `lt` is darker (every
/// channel alike, so the hue holds), lifted toward white by
/// `k = (lt − Lc) / (1 − Lc)` where it is lighter. Exact but for each
/// channel's rounding to a byte; a black ink lifts to grey.
#[must_use]
#[inline]
pub fn at_luminance(ink: u32, lt: f32) -> u32 {
    let (lc, lt) = (relative_luminance(ink), lt.clamp(0.0, 1.0));
    if lc <= 1e-6 {
        let g = srgb(lt);
        return (g << 16) | (g << 8) | g;
    }
    let map = |f: &dyn Fn(f32) -> f32| {
        (srgb(f(lin(ink, 16))) << 16) | (srgb(f(lin(ink, 8))) << 8) | srgb(f(lin(ink, 0)))
    };
    if lt <= lc {
        let s = lt / lc;
        map(&|c| c * s)
    } else {
        let k = (lt - lc) / (1.0 - lc).max(1e-6);
        map(&|c| c + (1.0 - c) * k)
    }
}

/// `ink` with its OKLab chroma times `keep` (`0..=1`), its OKLab lightness
/// and hue held: a fade toward grey that never turns the hue (where sRGB
/// holds the colour; [`oklab_to_rgb`] clamps at the gamut's edge).
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

/// `ink` at WCAG luminance `lt`, its OKLab hue held: where `lt` is darker,
/// [`at_luminance`]'s scaling (every linear channel alike, so the hue is
/// exact); where lighter, a mix toward white IN OKLAB (lightness up, chroma
/// down in step, the hue held but for the gamut's edge), bisected to the
/// luminance. [`at_luminance`]'s lift mixes in linear light, which turns an
/// orange 13° toward red at luminance 0.65: a dot brightened that way could
/// drift into Claude's error red.
#[must_use]
#[inline]
pub fn at_luminance_hue(ink: u32, lt: f32) -> u32 {
    let lt = lt.clamp(0.0, 1.0);
    let lc = relative_luminance(ink);
    if lt <= lc || lc <= 1e-6 {
        return at_luminance(ink, lt);
    }
    let [l0, a0, b0] = oklab(ink);
    let at = |t: f32| oklab_to_rgb([l0 + (1.0 - l0) * t, a0 * (1.0 - t), b0 * (1.0 - t)]);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..12 {
        let mid = (lo + hi) / 2.0;
        if relative_luminance(at(mid)) < lt {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at(hi)
}

/// WCAG contrast ratio of two colours.
#[inline]
fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// A light-ground dot's ring: `fresh` (the dot's hue, darkened by
/// [`at_luminance`]'s scaling, so hue-true) at the palest luminance up to
/// `top` whose contrast with `ground` holds [`PRINT_RING_CONTRAST`] with both
/// under the dot's own tint at its ceiling `veil` (`over_rgb`, the renderer's
/// source-over: the ring lies inside the tint's plateau, never under more of
/// it). Bisected, 12 steps; black where nothing holds.
#[inline]
fn print_ring(fresh: u32, ground: u32, veil: u8, top: f32) -> u32 {
    let ring = |l: f32| at_luminance(fresh, l);
    let holds = |l: f32| {
        let r = over_rgb(ring(l), fresh, veil) & 0x00FF_FFFF;
        let g = over_rgb(ground, fresh, veil) & 0x00FF_FFFF;
        contrast(r, g) >= PRINT_RING_CONTRAST
    };
    let top = top.min(relative_luminance(fresh)).max(0.0);
    if holds(top) {
        return ring(top);
    }
    let (mut lo, mut hi) = (0.0f32, top);
    for _ in 0..12 {
        let mid = (lo + hi) / 2.0;
        if holds(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    ring(lo)
}

/// A `0..=1` share as a byte of opacity.
#[inline]
fn alpha_byte(share: f32) -> u8 {
    (share.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The colour of a panel cell of kind `mark` at palette position `x`
/// ([`name_x`], [`bar_x`], [`row_hue`]) on a dark or a light ground, Claude
/// having drawn it in `cell_fg`:
///
/// * TEXT ([`PanelMark::Name`], [`PanelMark::Pointer`], [`PanelMark::BarEmpty`]):
///   the palette's legible letter ink (`CardPalette::letter`), mixed toward
///   `cell_fg` by `1 − sat` (a quiet row's name greys with its dot), at
///   `cell_fg`'s luminance ([`match_luminance`]). Never crested.
/// * A FILLED BAR CELL ([`PanelMark::BarFill`]): the palette's art at full
///   chroma — on dark lifted to the card line's floor, on light the card's
///   one-pixel print (its luma held to the print bar) — and, where `crest`
///   is the ripple's field at the cell (`0..=1`, [`crest`]), mixed toward
///   white by `crest · GLINT_CREST_DARK` on dark, deepened toward black by
///   `crest · GLINT_CREST_LIGHT / 2` on light: a crest never lowers the
///   cell's contrast on paper.
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

/// What the fold says of a dot, as its look needs it (the host maps
/// `aterm_agent::harness::agents::DotState` onto it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DotMood {
    /// Evidence of work lately: lit.
    Running,
    /// No evidence for a while: its light fades with its freshness.
    Quiet,
    /// No evidence for a while, but waiting on a tool call of its own inside
    /// that call's budget: fresh, its light dimmed by [`TOOL_GLOW`] and held
    /// inside its ring ([`TOOL_RADIUS`]).
    Tool,
    /// No evidence for long: grey, breathing.
    Stuck,
    /// Awaiting approval: a still amber ring.
    Awaiting,
    /// Claude marked it done or failed, its clock halted, or it waits with
    /// nothing in flight (idle, paused): Claude's own ink, no light.
    Still,
}

/// A dot's light: `color` (straight) at `strength` (`0..=1`: a dark
/// ground's halo peak, a light ground's tint ceiling, of the colour),
/// `radius` cell widths from the bullet's centre, and on a light ground the
/// hollow's fill at `fill` (`0..=1`, its ceiling; none on dark, where the
/// halo fills the hollow).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DotLight {
    pub color: u32,
    pub radius: f32,
    pub strength: f32,
    pub fill: f32,
}

/// One dot's look: its bullet's ink (`None`: Claude's own) and its light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DotLook {
    pub ink: Option<u32>,
    pub light: Option<DotLight>,
}

/// The breath of a stuck dot `ms` after it began to breathe, on the
/// [`BREATH_STEP_MS`] grid: `BREATH_FLOOR + (1 − BREATH_FLOOR)·(1 − cos(2πt/T))/2`
/// (`T` = [`BREATH_PERIOD_MS`]), so it starts at its floor and swells to
/// `1` once a period; [`BREATH_REST`] with no breath (`None`) and from
/// [`BREATH_FOR_MS`] on.
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

/// The dot of the panel's row `k` (its hue, [`row_hue`]) in mood `mood`,
/// its work `level` and freshness `sat` (`0..=1`, the fold's), on a dark or a
/// light ground (`ground`, the pane's own: the paper a light ring is solved
/// against); `breath` the stuck dot's swell ([`breath`]); `serious` drops the
/// light.
///
/// * [`DotMood::Running`], [`DotMood::Quiet`]: the row's hue, a quiet one's
///   chroma faded to `QUIET_KEEP · sat` of it ([`fade_chroma`]: hue-true). On
///   dark drawn at luminance `DOT_LUMA_MIN + DOT_LUMA_GAIN·level`
///   ([`at_luminance_hue`]: a yellow dims at rest as a blue lifts, so
///   brightness reads as work on every hue); its light the fresh hue itself,
///   an additive halo `DOT_RADIUS_MIN + DOT_RADIUS_GAIN·level` cells wide (at
///   most [`DOT_RADIUS_MAX`]) peaking at `HALO_PEAK_MIN + HALO_PEAK_GAIN·level`.
///   On light the ring is the hue as a print ink ([`PRINT_RING_LUMA`],
///   deepened by `DOT_DEEPEN_LIGHT·level`, then as dark as it must be to hold
///   [`PRINT_RING_CONTRAST`] on `ground` under its own tint); its light a
///   round tint of the same radius at `TINT_ALPHA_MIN + TINT_ALPHA_GAIN·level`
///   and its hollow filled at `FILL_ALPHA_MIN + FILL_ALPHA_GAIN·level`. A
///   quiet dot's light, every part, is times its freshness.
/// * [`DotMood::Tool`]: a running dot's ink, in its full hue, at its level;
///   its light [`TOOL_GLOW`] of a running dot's at that level (its strength,
///   and on paper its hollow's fill), [`TOOL_RADIUS`] wide: a core inside the
///   ring, never a halo past it — so it is fresh (never the stuck dot's
///   grey), never breathes, is dimmer than any running dot, and its light's
///   shape tells it from a running dot and from an awaiting ring.
/// * [`DotMood::Stuck`]: the hue faded to its freshness (the fold's
///   `SAT_STUCK`, nearly grey), its lightness swung by the breath (on dark
///   drawn at luminance `DOT_LUMA_MIN·breath`, on light paled from the
///   over-text ink at its swell to [`STUCK_PALE_LUMA_LIGHT`] at its floor, so
///   it holds 3:1 on paper); its light [`STUCK_RADIUS`] wide at the strength
///   of no work times the breath, its hollow never filled, so a stuck dot is
///   never brighter than a running one (on Off's colourless moonbow too).
/// * [`DotMood::Awaiting`]: [`AWAITING_AMBER`] ([`AWAITING_AMBER_PRINT`] on light),
///   UNLIT — a still, hollow ring beside Claude's done and failed ones, so
///   it is never a running dot of a near hue (every running dot is lit) —
///   and never breathing.
/// * [`DotMood::Still`]: nothing — Claude's ink, no light.
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
            // No light: light is work, and an agent awaiting approval does
            // none. Lit (as it once was, at a level-0.3 light) it was a
            // running dot of a near hue — Land's gold lies 5° from the amber,
            // Cruise's print gold 0.4° — and only its form tells them apart.
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
            // The breath swings the ring's own lightness, so the pulse reads in
            // the glyph and not only in its faint light. On paper it pales
            // from the over-text ink at its swell to STUCK_PALE_LUMA_LIGHT at
            // its floor, never past it: the ring stays legible.
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

/// Where a dot is on the glass, in the light streams' window pixels: its
/// bullet cell's centre `(cx, cy)`, the cell `(cw, ch)`, the bullet's frame
/// row `row` (the streams' row tag; its band `[cy − ch/2, cy + ch/2)`), and
/// the pixel rows `clip` the light may reach (the pane's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DotPlace {
    pub cx: u16,
    pub cy: u16,
    pub cw: u16,
    pub ch: u16,
    pub row: u16,
    pub clip: (u16, u16),
}

/// The quads of dot light `light` at `place`, pushed onto `halos` (the
/// light stream over the glyphs), each a [`RainHalo`] split one quad per row
/// band it reaches (the stream's one-band rule; the bands about the bullet's
/// row share its centre), never wider than [`DOT_RADIUS_MAX`] cell widths nor
/// taller than [`DOT_RISE_MAX`] cell heights from the bullet's centre, nor
/// past the pane's clip:
///
/// * on a dark ground one additive halo ([`HaloMode::Add`]): its peak the
///   light's colour premultiplied by its strength, falling to nothing at its
///   radius;
/// * on a light ground, where added light is invisible, two source-over
///   veils ([`HaloMode::Over`], each its ceiling in the colour's high byte,
///   `aterm_render::halo_over_cap`): the ROUND TINT, at the light's radius
///   (the dark halo's) and strength, then the hollow's FILL, [`FILL_RADIUS`]
///   round, at the light's fill. A veil whose ceiling would be under
///   `VEIL_ALPHA_FLOOR` is not drawn.
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
    for band in -1i32..=1 {
        let top = band_top + band * ch_px;
        let y0 = top.max(cy - ryi).max(clip0);
        let y1 = (top + ch_px).min(cy + ryi).min(clip1);
        let Some(row) = i32::from(place.row)
            .checked_add(band)
            .and_then(|r| u16::try_from(r).ok())
        else {
            continue;
        };
        if y1 <= y0 {
            continue;
        }
        halos.push(RainHalo {
            row,
            x: px(cx - rxi),
            y: px(y0),
            w: px(2 * rxi),
            h: px(y1 - y0),
            color,
            cx: place.cx,
            cy: place.cy,
            rx: px(rxi).max(1),
            ry: px(ryi).max(1),
            mode,
        });
    }
}

#[cfg(test)]
mod tests;
