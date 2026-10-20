// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Centralized color resolution for terminal cells.
//!
//! Resolves raw cell colors (default/indexed/RGB) into final RGB values
//! with all style attributes applied: bold-to-bright, the theme-pair
//! contrast floor, dim, inverse, DECSCNM (reverse video), and hidden.
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
///   `0.0..=1.0`. The fg is blended toward the CELL background — light ink
///   as far as an sRGB (alpha) blend at this opacity, dark ink a linear-light
///   one — raised where needed so faint text keeps `contrast^opacity` and a
///   readable `9^opacity` (3:1 at the default) capped below the colour's own
///   contrast (see [`dim_toward_bg`]). Faint text recedes on both dark and
///   light themes. Defaults to [`DIM_FACTOR`] (0.5).
/// * `indexed_minimum_contrast` — the WCAG ratio a foreground is held to when
///   both of a cell's colours come from the theme (see
///   [`theme_pair_floor`]); while it is on, a light theme's dark white on a
///   colour is drawn in the page colour ([`white_on_colour`]). `1.0`, the
///   default, is off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyleResolveOpts {
    /// SGR 1 promotes indexed 0–7 to bright 8–15 (default `true`).
    pub bold_is_bright: bool,
    /// Fraction of fg retained by SGR 2, blended toward bg (default 0.5).
    pub faint_opacity: f32,
    /// The theme-pair contrast floor, `1.0..=21.0` (default `1.0`, off).
    pub indexed_minimum_contrast: f32,
}

impl Default for StyleResolveOpts {
    fn default() -> Self {
        Self {
            bold_is_bright: true,
            faint_opacity: DIM_FACTOR,
            indexed_minimum_contrast: 1.0,
        }
    }
}

/// The most the theme-pair floor asks of text on the theme's OWN background
/// (WCAG's bar for large text and UI marks): enough to rescue a slot that
/// vanishes there (black on a dark theme, a near-white on a light one),
/// little enough to keep the muted tones a theme chose for that background,
/// which its admission measures (`aterm_types::scheme::contrast_report`).
pub const THEME_BACKGROUND_FLOOR: f32 = 3.0;

/// The most the theme-pair floor asks of aterm's OWN chromatic colours —
/// slots 1–6 still at [`ColorPalette::default_color`] — as text on the page:
/// WCAG's body-text bar. aterm chose those for text, so they are held to it;
/// Windows' blue (`#2973D0`) is tuned as a background, where pwsh draws every
/// directory name, and is 3.9:1 as text, which this lifts to `#3E81D5`
/// (4.7:1) while the directories keep their ground. A theme's or a program's
/// own slot keeps [`THEME_BACKGROUND_FLOOR`].
pub const DEFAULT_PALETTE_TEXT_FLOOR: f32 = 4.5;

/// What the theme-pair floor asks of one cell ([`theme_pair_floor`]).
#[derive(Debug, Clone, Copy, PartialEq)]
struct PairFloor {
    /// The WCAG ratio the drawn foreground is held to.
    min: f32,
    /// The cell is drawn on the page (the theme's own background): a rescue
    /// by the smallest nudge ([`floor_contrast`]), which keeps a muted slot
    /// muted. Off the page the text is on a colour and may have to change
    /// polarity ([`floor_contrast_on_colour`]).
    on_page: bool,
}

impl PairFloor {
    /// `fg` floored against `bg`; `inks` are the theme's default foreground
    /// and background.
    fn apply(self, fg: Rgb, bg: Rgb, inks: [Rgb; 2]) -> Rgb {
        if self.on_page {
            floor_contrast(fg, bg, self.min)
        } else {
            floor_contrast_on_colour(fg, bg, self.min, inks)
        }
    }
}

/// Whether `ch` is a shape drawn edge to edge rather than a letter: box
/// drawing and block elements (U+2500–U+259F) and the powerline separators
/// (U+E0A0–U+E0D7, private use). A program colours these to meet a
/// neighbouring cell — a separator's ink is the next segment's background, a
/// half block's ink is the other half pixel — so a contrast floor would put a
/// seam in the picture, not make anything legible.
#[must_use]
#[inline]
pub(crate) fn is_shape_glyph(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{259F}' | '\u{E0A0}'..='\u{E0D7}')
}

/// What the theme-pair floor holds this cell's foreground to, or `None`
/// where it does not apply. Windows Terminal's `adjustIndistinguishableColors`
/// (`indexed`) rule, on WCAG contrast: it applies when both colours are the
/// theme's own — the default foreground or background, or palette slots
/// 0–15 — and are not both the defaults (the theme's body text, its own
/// business). Text on the theme's background is held to at most
/// [`THEME_BACKGROUND_FLOOR`] (aterm's own chromatic colours to
/// [`DEFAULT_PALETTE_TEXT_FLOOR`]); text on a coloured cell, a pair no theme
/// author tunes (pwsh's directories are its default foreground on slot 4),
/// to the full `ratio`. Truecolor and the 256-colour cube are a program's own
/// choice and stay exact, as do shapes ([`is_shape_glyph`]) and hidden text.
/// `reverse_video` is DECSCNM, which swaps the page itself: the theme's
/// background is drawn as the default foreground colour. A cell's own SGR 7
/// draws its foreground colour as a block that is never the page, so only an
/// un-inverted cell whose drawn background is a default sits on the page.
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
    // One slot for both (`ESC[34;44m`) is a deliberate solid fill. A default
    // that merely equals a slot (Default Light's text is its color0) is not.
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
    // The slot drawn as ink on the page: the cell's foreground, or under
    // DECSCNM, which swaps every un-inverted cell, its background.
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

/// A light theme's own white as text on a colour, while the theme-pair floor
/// is on: the page colour where that reads better on the cell than the white.
///
/// A light theme often draws white (slots 7 and 15) as a dark text colour,
/// because a program that writes white on the page means text there
/// (PSReadLine draws numbers `ESC[97m`, members and types `ESC[37m`):
/// Default Light's are `#6B7280` and `#474D57`, Windows Terminal's VS Code
/// Light Modern's `#555555` and `#A5A5A5`. On a coloured cell a program means
/// light text — a test runner's `ESC[97;41m FAIL`, white on black
/// (`ESC[97;40m`) — and the dark white put dark text on a dark cell
/// (`#474D57` on `#C4262E`, 1.49:1). So a white `slot` whose colour is darker
/// than `page` and at least [`THEME_BACKGROUND_FLOOR`] from it (the theme
/// made it text) is drawn as `page` on any `ground` that is not its own slot
/// (`ground_slot`) where `page` has the more contrast; on the page itself it
/// keeps its colour (`page` on `page` is 1:1). A dark theme's whites are
/// lighter than its page, and a light theme's light whites (Solarized
/// Light's `#BBB5A2`, 1.9:1) are under the bar, so both keep their colours.
/// Shapes ([`is_shape_glyph`]) and hidden text keep theirs, as under the
/// floor.
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

/// Floor `fg`'s WCAG contrast against `bg` to `min`: `fg` itself when it
/// clears it, else `fg` blended in sRGB toward black or white — whichever
/// pole `bg` contrasts with more ([`pole_of`]) — by the smallest tenth that
/// clears it ([`first_clearing_tenth`]), else that pole.
/// `aterm_render::floor_fg_contrast`'s law (the renderer's
/// `minimum_contrast`), on this module's transfer tables, so the two floors
/// agree on a colour. Unlike the renderer, which cannot tell SGR 8 from a
/// pair that happens to match, `fg == bg` is floored too: the caller
/// ([`theme_pair_floor`]) knows hidden text and same-slot fills apart.
#[must_use]
pub fn floor_contrast(fg: Rgb, bg: Rgb, min: f32) -> Rgb {
    let lb = wcag_luminance(bg);
    if wcag_contrast(wcag_luminance(fg), lb) >= min {
        return fg;
    }
    let pole = pole_of(lb);
    first_clearing_tenth(fg, pole, lb, min).map_or_else(|| grey(pole), |(_, nudged)| nudged)
}

/// WCAG relative luminance on this module's transfer tables.
fn wcag_luminance(c: Rgb) -> f32 {
    let tables = transfer_tables();
    0.2126 * tables.to_linear[usize::from(c.r)]
        + 0.7152 * tables.to_linear[usize::from(c.g)]
        + 0.0722 * tables.to_linear[usize::from(c.b)]
}

/// The WCAG contrast ratio of two relative luminances.
fn wcag_contrast(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The pole a background of luminance `lb` contrasts with more: black (`0`)
/// or white (`255`). contrast(black, bg) = (L+0.05)/0.05 beats
/// contrast(white, bg) = 1.05/(L+0.05) iff (L+0.05)² >= 0.05·1.05.
fn pole_of(lb: f32) -> u8 {
    if (lb + 0.05) * (lb + 0.05) >= 0.05 * 1.05 {
        0
    } else {
        255
    }
}

fn grey(v: u8) -> Rgb {
    Rgb { r: v, g: v, b: v }
}

/// The smallest step `k` of `1..=10` whose sRGB mix of `fg` toward the grey
/// `pole` by `k` tenths clears `min` against a background of luminance `lb`,
/// with that mix: [`floor_contrast`]'s nudge. `None` when no tenth does.
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

/// The tenths of the way to the pole from which [`floor_contrast`]'s nudge
/// keeps less of the text's own lightness than it takes from the pole
/// (`#D0D0D0` five and six tenths toward black is `#686868` and `#535353`).
const HALFWAY_TENTHS: u8 = 5;

/// The `OkLab` chroma under which a nudged colour has no hue left to show:
/// the greys a grey or grey-tinted text is nudged to (`#535353` 0, Gruvbox's
/// cream text `#5E5847` 0.028), where a coloured text nudged as far keeps
/// 0.044 (cyan's `#385D65`) to 0.106 (bright green's `#206431`).
const MUD_CHROMA: f64 = 0.035;

/// [`floor_contrast`] for text on a COLOURED cell, where clearing `min` can
/// mean changing polarity (the default foreground on a yellow or green
/// segment). The smallest nudge, exactly as [`floor_contrast`], while the
/// text stays on its side of the cell's luminance and the nudge keeps it
/// itself: a nudge under [`HALFWAY_TENTHS`], or one that keeps the text's hue
/// (at least [`MUD_CHROMA`]): red on bright yellow is a darker red, green on
/// white a darker green. Otherwise the smallest nudge ends at a grey that
/// only just passes (`#535353` on `#CDCD00`, 4.52:1, beside `#111318`'s
/// 10.9:1; the light default text on a lighter cell, `#535353` on bright
/// green and `#686868` on bright yellow and white), so it takes instead the
/// theme's own ink on the pole's side (of `inks`, its text and background
/// colours, the darker toward black, the lighter toward white), the colour
/// such a cell's text takes wherever it flips. When the text has to cross to
/// the other side of the cell — `fg` is on the far side from the pole `bg`
/// contrasts with more, or level with `bg` — the ink when it lies beyond
/// `bg` toward the pole and clears `min`, else the pole; when it stays on
/// its side, the ink when it lies beyond `bg` and is at least as legible as
/// the grey, else the grey. The same bound as [`floor_contrast`]: `fg`
/// itself when it clears `min`, else at least `min` or the best pole.
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

    // 1. Raw color lookup (default -> indexed -> RGB)
    let (mut fg, mut bg) = raw_resolve(colors, fg_rgb, bg_rgb, palette, default_fg, default_bg);
    let mut fg_slot = colors.fg_is_indexed().then(|| colors.fg_index());
    let bg_slot = colors.bg_is_indexed().then(|| colors.bg_index());

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
            fg_slot = Some(idx + 8);
        }
    }

    // 2b. The theme-pair contrast floor, on the pair as drawn: here for an
    // unswapped cell, so SGR 2 below recedes from a legible colour; after the
    // swap for an inverse one. Off (`None`) by default. A light theme's dark
    // white on a colour first takes the page colour (`white_on_colour`).
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

    // 3. Dim (SGR 2): blend fg toward the cell background (see
    // `dim_toward_bg`). (The old "multiply toward black" made faint text
    // darker — i.e. heavier — on light themes, the inversion of the
    // attribute. Blending toward bg is theme-independent: it always recedes
    // toward the surface it sits on.) Applied before the inverse swap, like
    // the old multiply.
    if flags.contains(CellFlags::DIM) {
        fg = dim_toward_bg(fg, bg, opts.faint_opacity);
    }

    // 4. Inverse: XOR with DECSCNM
    // Cell INVERSE flag and terminal reverse_video cancel each other out.
    if effective_inverse {
        std::mem::swap(&mut fg, &mut bg);
        if LIGHT_WHITES {
            fg = white_on_colour(cell, ratio, bg_slot, fg_slot, fg, bg, default_bg);
        }
        if let Some(floor) = floor {
            fg = floor.apply(fg, bg, inks);
        }
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

/// SGR 2 (dim/faint): recede `fg` toward `bg`, keeping `opacity` of it.
///
/// `opacity` is the fraction of the foreground retained (clamped to `0..=1`):
/// `1.0` returns `fg` unchanged, `0.0` lands on `bg` (invisible). The blend
/// is per-channel in linear light at a mix `m` ([`faint_mix`]), so the
/// result keeps the colour's hue and its WCAG relative luminance is
/// `L(dim) = L(bg) + m·(L(fg) − L(bg))` (up to u8 quantization). With
/// `t = opacity`, `m` starts from whichever of two blends recedes further:
///
/// * the linear-light lerp, `m = t`;
/// * the sRGB-space blend `bg + t·(fg − bg)`, by its luminance
///   ([`srgb_blend_mix`]): the alpha blend Windows Terminal, iTerm2 and
///   Ghostty draw faint text with.
///
/// On Windows ([`FAINT_SRGB_BLEND`]) light ink on a dark ground takes the
/// sRGB blend, which recedes as far as those terminals do. The lerp kept most
/// of the contrast there: PSReadLine draws its inline prediction bright white
/// and faint (`ESC[97;2;3m`), and on Default it was `#BCBCBC` (9.8:1) beside
/// typed text at `#E5E5E5`, so the suggestion read as typed; it is `#898989`
/// (5.3:1), and Default's faint text `#717172` (3.8:1, was 6.5:1). macOS and
/// Linux keep the lerp, the faint they have always drawn. Dark ink on a light
/// ground keeps the lerp everywhere, the further of the two there.
///
/// Then `m` is raised, when needed, to reach [`faint_target`]. With
/// `C = contrast(fg, bg)`, the target is the larger of two floors:
///
/// * the log-share floor `C^t`: faint keeps `t` of the contrast in log
///   space, which dominates for dark body text on a light ground;
/// * the readable floor `9^t` (3:1 at the default 0.5, 1:1 at 0), capped by
///   the recession ceiling `C^((1+t)/2)` so faint never reaches the colour's
///   own contrast. It applies only where `C >= 1.25`; below that the pair is
///   not legible text either way.
///
/// The floors are what make one opacity read alike on both polarities: the
/// lerp lands dark ink on a light ground in the mid-greys, where the ratio
/// collapses (14.35:1 → 1.9:1 for Aterm Day's fg). At the default 0.5, Aterm
/// Day's fg goes 14.35:1 → 3.78:1 (the log share), colour 7 goes 5.76:1 →
/// 3.0:1 (the readable floor) and colour 8 goes 3.50:1 → 2.57:1 (the
/// ceiling).
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
    dim_toward_bg_with(fg, bg, opacity, FAINT_SRGB_BLEND)
}

/// Whether light faint ink starts from the sRGB (alpha) blend
/// ([`dim_toward_bg`]): on Windows, where Windows Terminal draws faint that
/// way beside aterm; macOS and Linux keep the linear-light lerp.
pub const FAINT_SRGB_BLEND: bool = cfg!(windows);

/// [`dim_toward_bg`] with the light-ink start chosen by `srgb_blend` (the
/// platform's is [`FAINT_SRGB_BLEND`]), so tests check both shapes on every
/// host.
pub(crate) fn dim_toward_bg_with(fg: Rgb, bg: Rgb, opacity: f32, srgb_blend: bool) -> Rgb {
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
    let (lf, lb) = (luminance(fg), luminance(bg));
    // Only light ink recedes further by the sRGB blend; dark ink's sRGB
    // blend sits nearer the ink than the lerp.
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
/// (`lf != lb`). Table lookups only, no `powf`. The transfer curve and each
/// chord are convex, so every channel lands at or below its linear lerp:
/// for light ink (`lf > lb`) the result is at most `t`, and it may fall
/// below `0` where channels move in opposite directions ([`faint_mix`]
/// clamps it).
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
/// log space and always stays fainter than the colour itself. At the
/// default 0.5 the powers are square roots (`full^0.5`, `3`, `full^0.75`),
/// so the common case costs no `powf` per faint cell.
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

/// The linear-light mix SGR 2 uses for an fg of relative luminance `lf` on
/// a bg of `lb` at `opacity` (already in `0..=1`): `start` (the mix
/// [`dim_toward_bg`] begins at: `opacity`, or for light ink the sRGB
/// blend's, [`srgb_blend_mix`]) clamped to `0..=opacity`, raised to the
/// smallest mix whose WCAG contrast reaches [`faint_target`] — the larger
/// of the log-share floor `contrast^opacity` and the readable floor
/// `9^opacity` capped at `contrast^((1+opacity)/2)`. Theme-independent and
/// bounded: the result stays in `0..=1`, never below `opacity` for dark
/// ink; `0` stays invisible and `1` stays the fg; and since the luminance
/// is a lerp in the mix the floor is solved exactly rather than searched.
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
    // The contrast the starting mix already has. Skipping the solve when
    // that is enough keeps light ink clear of the solve's f32 cancellation,
    // which could otherwise nudge a channel across a quantizer boundary.
    let at_start = if lf >= lb {
        1.0 + start * (full - 1.0)
    } else {
        (lb + 0.05) / (lb + start * (lf - lb) + 0.05)
    };
    if at_start >= target {
        return start;
    }
    // The luminance the faint ink needs to reach `target` against `lb`.
    let needed = if lf < lb {
        (lb + 0.05) / target - 0.05
    } else {
        target * (lb + 0.05) - 0.05
    };
    let floor = ((needed - lb) / (lf - lb)).clamp(0.0, 1.0);
    start.max(floor)
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
    use super::{dim_toward_bg_with, faint_target, srgb_to_linear};
    use aterm_types::Rgb;

    /// The law is checked on the sRGB-blend shape on every host (Windows
    /// draws it); [`the_platform_picks_its_faint_shape`] covers the lerp
    /// shape macOS and Linux draw.
    fn dim_toward_bg(fg: Rgb, bg: Rgb, opacity: f32) -> Rgb {
        dim_toward_bg_with(fg, bg, opacity, true)
    }

    /// `dim_toward_bg` is the sRGB blend on Windows and the lerp elsewhere,
    /// and the lerp shape keeps the law too: never more contrast than the
    /// colour, never less mix than `t`, and the floors on both polarities.
    /// On it PSReadLine's ghost stays the lerp's `#BCBCBC` (the faint macOS
    /// and Linux have always drawn).
    #[test]
    fn the_platform_picks_its_faint_shape() {
        let (white, page) = (Rgb::new(255, 255, 255), Rgb::new(0x11, 0x13, 0x18));
        let t = aterm_types::DIM_FACTOR;
        assert_eq!(
            super::dim_toward_bg(white, page, t),
            dim_toward_bg_with(white, page, t, cfg!(windows))
        );
        assert_eq!(
            dim_toward_bg_with(white, page, t, false),
            blend(white, page, t)
        );
        assert_ne!(
            dim_toward_bg_with(white, page, t, true),
            blend(white, page, t)
        );
        let colors = lattice();
        for &fg in &colors {
            for &bg in &colors {
                let (lf, lb) = (luminance(fg), luminance(bg));
                let full = contrast(fg, bg);
                for t in INTERIOR {
                    let dim = dim_toward_bg_with(fg, bg, t, false);
                    let c = contrast(dim, bg);
                    assert!(c <= full * 1.01 + 0.01, "fg={fg:?} bg={bg:?} t={t}");
                    let ld = luminance(dim);
                    assert!(
                        (ld - lb).abs() >= t * (lf - lb).abs() - 0.005,
                        "fg={fg:?} bg={bg:?} t={t}: the lerp shape mixes at least t"
                    );
                    assert!(
                        c >= faint_target(full, t) * 0.97 - 0.01,
                        "fg={fg:?} bg={bg:?} t={t}"
                    );
                }
            }
        }
    }

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

    /// The mix faint starts from: `t`, or for light ink the sRGB blend's
    /// ([`super::srgb_blend_mix`]) when that recedes further.
    fn start_mix(fg: Rgb, bg: Rgb, t: f32) -> f32 {
        let (lf, lb) = (luminance(fg), luminance(bg));
        if lf > lb && t > 0.0 && t < 1.0 {
            super::srgb_blend_mix(super::transfer_tables(), fg, bg, (lf, lb), t).clamp(0.0, t)
        } else {
            t
        }
    }

    /// The sRGB-space blend's relative luminance, from the exact transfer
    /// function (`powf`), per channel before quantization.
    fn srgb_blend_luminance(fg: Rgb, bg: Rgb, t: f32) -> f32 {
        let channel = |f: u8, b: u8| {
            let (f, b) = (f32::from(f), f32::from(b));
            super::encoded_unit_to_linear((b + (f - b) * t) / 255.0)
        };
        0.2126 * channel(fg.r, bg.r) + 0.7152 * channel(fg.g, bg.g) + 0.0722 * channel(fg.b, bg.b)
    }

    /// A reference faint ink: the engine's shape (the starting mix is kept
    /// when it already reaches the target, else the exact solve), with the
    /// target supplied by the caller so a control can swap one term out.
    fn faint_with(fg: Rgb, bg: Rgb, t: f32, target: impl Fn(f32, f32) -> f32) -> Rgb {
        let (lf, lb) = (luminance(fg), luminance(bg));
        let (hi, lo) = (lf.max(lb), lf.min(lb));
        if t <= 0.0 || t >= 1.0 || hi - lo <= f32::EPSILON {
            return blend(fg, bg, t);
        }
        let start = start_mix(fg, bg, t);
        let full = (hi + 0.05) / (lo + 0.05);
        let target = target(full, t);
        let at_start = if lf >= lb {
            1.0 + start * (full - 1.0)
        } else {
            (lb + 0.05) / (lb + start * (lf - lb) + 0.05)
        };
        if at_start >= target {
            return blend(fg, bg, start);
        }
        let needed = if lf < lb {
            (lb + 0.05) / target - 0.05
        } else {
            target * (lb + 0.05) - 0.05
        };
        blend(
            fg,
            bg,
            start.max(((needed - lb) / (lf - lb)).clamp(0.0, 1.0)),
        )
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
    /// Day's colour 7 under 2.5:1. Dark ink keeps the lerp as its start, so
    /// these are the bytes they were. (Dark themes:
    /// `faint_light_ink_recedes_as_an_alpha_blend`.)
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
        for name in ["Aterm Day", "Linen", "Default Light"] {
            let scheme = aterm_types::scheme::builtin(name).expect("built-in");
            for ink in std::iter::once(scheme.foreground).chain(scheme.ansi) {
                if luminance(ink) < luminance(scheme.background) {
                    assert!(
                        (start_mix(ink, scheme.background, t) - t).abs() < f32::EPSILON,
                        "{name}: dark ink starts from the lerp"
                    );
                }
            }
        }
        // The ends stay put: invisible at 0, the fg at 1.
        assert!(faint_mix(0.02, 0.9, 0.0, 0.0).abs() < f32::EPSILON);
        assert!((faint_mix(0.02, 0.9, 1.0, 1.0) - 1.0).abs() < f32::EPSILON);
    }

    /// Light ink on a dark ground recedes as the sRGB-space (alpha) blend
    /// does — Windows Terminal's and iTerm2's faint — not as the linear lerp,
    /// which kept most of its contrast. PSReadLine draws its inline
    /// prediction bright white and faint (`ESC[97;2;3m`): on Default it is
    /// 4.5..5.5:1, at least 2.5:1 from typed text in colour 7, and a dark
    /// theme's body text is within 3% of its alpha blend's contrast and
    /// under 0.6 of the lerp's. Control: the lerp drew the ghost `#BCBCBC`,
    /// under 1.6:1 from colour 7, so the suggestion read as typed.
    #[test]
    fn faint_light_ink_recedes_as_an_alpha_blend() {
        let t = aterm_types::DIM_FACTOR;
        let page = Rgb::new(0x11, 0x13, 0x18);
        let (white, seven) = (Rgb::new(255, 255, 255), Rgb::new(229, 229, 229));
        let lerp_ghost = blend(white, page, t);
        assert_eq!(lerp_ghost, Rgb::new(0xBC, 0xBC, 0xBC), "control: the lerp");
        assert!(contrast(lerp_ghost, seven) < 1.6, "control: read as typed");
        let ghost = dim_toward_bg(white, page, t);
        let c = contrast(ghost, page);
        assert!((4.5..5.5).contains(&c), "{ghost:?}: {c}:1");
        assert!(contrast(ghost, seven) >= 2.5, "{ghost:?} beside colour 7");
        for name in ["Aterm Night", "Hearth", "Default"] {
            let scheme = aterm_types::scheme::builtin(name).expect("built-in");
            let (fg, bg) = (scheme.foreground, scheme.background);
            assert!(luminance(fg) > luminance(bg), "{name}");
            let faint = contrast(dim_toward_bg(fg, bg, t), bg);
            let alpha = (srgb_blend_luminance(fg, bg, t) + 0.05) / (luminance(bg) + 0.05);
            assert!(
                (faint - alpha).abs() <= alpha * 0.03,
                "{name}: {faint} vs the alpha blend's {alpha}"
            );
            assert!(
                faint < 0.6 * contrast(blend(fg, bg, t), bg),
                "{name}: {faint} recedes"
            );
            assert!(faint >= faint_target(contrast(fg, bg), t) * 0.98, "{name}");
        }
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

    /// The faint mix is never below the mix it starts from, so the luminance
    /// distance it keeps (within one u8 step) is at least `t` of the colour's
    /// for dark ink, and at least the sRGB blend's for light ink — which is
    /// never more than `t` of it (the transfer curve is convex), and often
    /// well under. Matrix rain derives its bound from this faint ink.
    #[test]
    fn faint_mix_never_drops_below_its_start() {
        let colors = lattice();
        let (mut raised, mut receded) = (0u32, 0u32);
        for &fg in &colors {
            for &bg in &colors {
                let (lf, lb) = (luminance(fg), luminance(bg));
                for t in INTERIOR {
                    let start = start_mix(fg, bg, t);
                    if lf > lb {
                        let unclamped =
                            super::srgb_blend_mix(super::transfer_tables(), fg, bg, (lf, lb), t);
                        assert!(unclamped <= t + 1e-5, "fg={fg:?} bg={bg:?} t={t}");
                        if start < t - 0.05 {
                            receded += 1;
                        }
                    } else {
                        assert!((start - t).abs() < f32::EPSILON);
                    }
                    let ld = luminance(dim_toward_bg(fg, bg, t));
                    assert!(
                        (ld - lb).abs() >= start * (lf - lb).abs() - 0.005,
                        "fg={fg:?} bg={bg:?} t={t}"
                    );
                    if (ld - lb).abs() > start * (lf - lb).abs() + 0.01 {
                        raised += 1;
                    }
                }
            }
        }
        assert!(raised > 10_000, "non-vacuity: the floors raise the mix");
        assert!(receded > 10_000, "non-vacuity: the alpha blend recedes");
    }

    /// The engine keeps its reference bytes: any pair whose starting mix
    /// already reaches the target keeps that mix (the lerp for dark ink, the
    /// sRGB blend's for light ink); every other pair gets the exact floor
    /// solve. Compares public outputs, quantization included, against a
    /// reference written from the rule ([`faint_with`]).
    #[test]
    fn faint_keeps_its_reference_bytes() {
        let colors = lattice();
        // The old floor's f32 cancellation could push this light-ink pair
        // past a blue-channel quantizer boundary: it must land on the
        // reference's bytes.
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
        assert_eq!(
            dim_toward_bg(fg, bg, 0.75),
            faint_with(fg, bg, 0.75, faint_target)
        );
        let (mut light_ink, mut dark_ink, mut light_floored) = (0, 0, 0);
        for &fg in &colors {
            for &bg in &colors {
                assert_eq!(dim_toward_bg(fg, bg, 0.0), bg);
                assert_eq!(dim_toward_bg(fg, bg, 1.0), fg);
                let (lf, lb) = (luminance(fg), luminance(bg));
                let lighter = lf >= lb;
                let full = (lf.max(lb) + 0.05) / (lf.min(lb) + 0.05);
                for t in INTERIOR {
                    let start = start_mix(fg, bg, t);
                    let at_start = if lighter {
                        1.0 + start * (full - 1.0)
                    } else {
                        (lb + 0.05) / (lb + start * (lf - lb) + 0.05)
                    };
                    let plain =
                        at_start >= faint_target(full, t) || (lf - lb).abs() <= f32::EPSILON;
                    let expected = faint_with(fg, bg, t, faint_target);
                    if plain {
                        let kept = if (lf - lb).abs() <= f32::EPSILON {
                            t
                        } else {
                            start
                        };
                        assert_eq!(expected, blend(fg, bg, kept), "fg={fg:?} bg={bg:?}");
                    }
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
        assert!(light_floored > 0, "non-vacuity: light ink is floored");
    }

    /// The default opacity's square roots are the powers they stand for:
    /// [`faint_target`] at 0.5 agrees with `full^0.5`, `9^0.5` and
    /// `full^0.75` to f32 rounding, over the whole contrast range.
    #[test]
    fn faint_target_square_roots_are_its_powers() {
        let rule = |full: f32, t: f32| {
            if full < 1.25 {
                return full.powf(t);
            }
            full.powf(t)
                .max(9f32.powf(t).min(full.powf(f32::midpoint(1.0, t))))
        };
        for i in 0..=2000u16 {
            let full = 1.0 + f32::from(i) * 0.01;
            let (got, want) = (faint_target(full, 0.5), rule(full, 0.5));
            assert!((got - want).abs() <= want * 1e-6, "{full}: {got} vs {want}");
            assert_eq!(faint_target(full, 0.25), rule(full, 0.25), "{full}");
        }
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

    fn contrast(a: Rgb, b: Rgb) -> f32 {
        let l = |c: Rgb| {
            0.2126 * srgb_to_linear(c.r)
                + 0.7152 * srgb_to_linear(c.g)
                + 0.0722 * srgb_to_linear(c.b)
        };
        let (la, lb) = (l(a), l(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// Law: the floor reaches `min` whenever the background admits it, and
    /// otherwise the best either pole can (black or white); a colour that
    /// already clears `min` comes back unchanged. Over a 6-per-channel RGB
    /// lattice (46,656 ordered pairs, `fg == bg` included) at three ratios,
    /// for the page's floor and for the coloured cell's with Default's,
    /// Default Light's and two mid-grey inks.
    #[test]
    fn floor_contrast_reaches_the_ratio_or_the_best_pole() {
        let vals = [0u8, 51, 102, 153, 204, 255];
        let mut lattice = Vec::new();
        for r in vals {
            for g in vals {
                for b in vals {
                    lattice.push(Rgb::new(r, g, b));
                }
            }
        }
        let inks = [
            [Rgb::new(0xD0, 0xD0, 0xD0), Rgb::new(0x11, 0x13, 0x18)],
            [Rgb::new(0x2B, 0x2D, 0x33), Rgb::new(0xF6, 0xF7, 0xF9)],
            [Rgb::new(0x80, 0x80, 0x80), Rgb::new(0x90, 0x90, 0x90)],
        ];
        let (black, white) = (Rgb::new(0, 0, 0), Rgb::new(255, 255, 255));
        for &bg in &lattice {
            let best = contrast(black, bg).max(contrast(white, bg));
            for &fg in &lattice {
                for min in [3.0f32, 4.5, 7.0] {
                    let floors = std::iter::once(floor_contrast(fg, bg, min))
                        .chain(inks.map(|inks| floor_contrast_on_colour(fg, bg, min, inks)));
                    for out in floors {
                        if contrast(fg, bg) >= min {
                            assert_eq!(out, fg, "{fg:?} on {bg:?} at {min}: kept");
                        } else {
                            let got = contrast(out, bg);
                            assert!(
                                got >= min.min(best) - 1e-4,
                                "{fg:?} on {bg:?} at {min}: {out:?} is {got:.3}:1, best {best:.3}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// On a coloured cell, text that must change polarity to be legible
    /// takes the theme's own ink on the far side, not the first grey that
    /// passes: Default's text on xterm's yellow and green, and on the
    /// Windows palette's, is Default's background colour (10:1 or more),
    /// Default Light's text on its blue is Default Light's background; with
    /// no theme ink that clears the ratio, the pole. Text that keeps its
    /// polarity and needs only a small nudge is nudged exactly as on the
    /// page. Control: the page's smallest nudge leaves `#535353` on
    /// `#CDCD00` (4.52:1) and `#3E3E3E` on `#00CD00`.
    #[test]
    fn a_flip_on_a_coloured_cell_takes_the_theme_ink() {
        let (ink, page) = (Rgb::new(0xD0, 0xD0, 0xD0), Rgb::new(0x11, 0x13, 0x18));
        let dark = [ink, page];
        let (yellow, green) = (Rgb::new(0xCD, 0xCD, 0), Rgb::new(0, 0xCD, 0));
        assert_eq!(floor_contrast(ink, yellow, 4.5), Rgb::new(0x53, 0x53, 0x53));
        assert_eq!(floor_contrast(ink, green, 4.5), Rgb::new(0x3E, 0x3E, 0x3E));
        for cell in [
            yellow,
            green,
            Rgb::new(0xE5, 0xC0, 0x7B),
            Rgb::new(0x98, 0xC3, 0x79),
            Rgb::new(0x56, 0xB6, 0xC2),
        ] {
            let out = floor_contrast_on_colour(ink, cell, 4.5, dark);
            assert_eq!(out, page, "on {cell:?}");
            assert!(contrast(out, cell) >= 7.0, "on {cell:?}");
        }
        let (light_ink, light_page) = (Rgb::new(0x2B, 0x2D, 0x33), Rgb::new(0xF6, 0xF7, 0xF9));
        let blue = Rgb::new(0x0B, 0x5C, 0xC4);
        assert_eq!(
            floor_contrast_on_colour(light_ink, blue, 4.5, [light_ink, light_page]),
            light_page
        );
        // Through the resolver: `ESC[43m` and `ESC[7;33m` on Default (the
        // default text on yellow, and the page drawn on a yellow block's
        // text colour) take the page colour.
        let on = StyleResolveOpts {
            indexed_minimum_contrast: 4.5,
            ..StyleResolveOpts::default()
        };
        let pal = ColorPalette::new();
        for (fg, bg, flags) in [
            (
                PackedColor::DEFAULT_FG,
                PackedColor::indexed(3),
                CellFlags::empty(),
            ),
            (
                PackedColor::indexed(3),
                PackedColor::DEFAULT_BG,
                CellFlags::INVERSE,
            ),
        ] {
            let cell = Cell::with_style('x', fg, bg, flags);
            let got = resolve_colors_raw_opts(&cell, None, None, &pal, ink, page, false, on);
            assert_eq!(got, (page, pal.get(3)), "{fg:?} on {bg:?} {flags:?}");
        }
        // No ink that clears: the pole.
        let greys = [Rgb::new(0x80, 0x80, 0x80), Rgb::new(0x90, 0x90, 0x90)];
        assert_eq!(
            floor_contrast_on_colour(ink, yellow, 4.5, greys),
            Rgb::new(0, 0, 0)
        );
        // Same polarity (dark text on a light cell, light on a dark one):
        // the page's nudge.
        for (fg, cell) in [
            (Rgb::new(0x8A, 0x61, 0x00), Rgb::new(0xBC, 0xC9, 0xE0)),
            (Rgb::new(0x29, 0x73, 0xD0), page),
            (ink, Rgb::new(0x29, 0x73, 0xD0)),
        ] {
            assert_eq!(
                floor_contrast_on_colour(fg, cell, 4.5, dark),
                floor_contrast(fg, cell, 4.5),
                "{fg:?} on {cell:?}"
            );
        }
    }

    /// Default's light text on a cell lighter than itself — white, bright
    /// white and the bright green, yellow and cyan — keeps its polarity, but
    /// the nudge carries it halfway to black or further, to a mud grey
    /// (`#535353` or `#686868`, 5.0 to 6.1:1). It takes Default's own dark
    /// ink instead, as on a cell it must flip on, so one row of labels on
    /// the palette is one ink, 10:1 or more. A nudge that keeps the text's
    /// hue stays, however far it goes: red on bright yellow and green on
    /// white are darker shades of themselves. Control: the page's smallest
    /// nudge, the old rule on these cells, gives the greys.
    #[test]
    fn a_halfway_nudge_on_a_coloured_cell_takes_the_theme_ink() {
        let (ink, page) = (Rgb::new(0xD0, 0xD0, 0xD0), Rgb::new(0x11, 0x13, 0x18));
        let dark = [ink, page];
        let mut cells = vec![
            (Rgb::new(0xE5, 0xE5, 0xE5), 0x53),
            (Rgb::new(0xFF, 0xFF, 0xFF), 0x68),
            (Rgb::new(0x50, 0xFA, 0x7B), 0x53),
            (Rgb::new(0xF1, 0xFA, 0x8C), 0x68),
            (Rgb::new(0x8B, 0xE9, 0xFD), 0x53),
        ];
        for slot in [7, 10, 11, 14, 15] {
            let cell = ColorPalette::default_color(slot);
            let grey = floor_contrast(ink, cell, 4.5).r;
            cells.push((cell, grey));
        }
        for (cell, mud) in cells {
            let nudged = floor_contrast(ink, cell, 4.5);
            assert_eq!(nudged, Rgb::new(mud, mud, mud), "control on {cell:?}");
            assert!(nudged.r <= 0x68, "halfway or further on {cell:?}");
            let out = floor_contrast_on_colour(ink, cell, 4.5, dark);
            assert_eq!(out, page, "on {cell:?}");
            assert!(contrast(out, cell) >= 10.0, "on {cell:?}");
            assert!(contrast(out, cell) > contrast(nudged, cell), "on {cell:?}");
        }
        // Through the resolver: `ESC[47m` and `ESC[102m` on Default.
        let on = StyleResolveOpts {
            indexed_minimum_contrast: 4.5,
            ..StyleResolveOpts::default()
        };
        let pal = ColorPalette::new();
        for slot in [7, 10] {
            let cell = Cell::with_style(
                'x',
                PackedColor::DEFAULT_FG,
                PackedColor::indexed(slot),
                CellFlags::empty(),
            );
            let got = resolve_colors_raw_opts(&cell, None, None, &pal, ink, page, false, on);
            assert_eq!(got, (page, pal.get(slot)), "on slot {slot}");
        }
        // A nudge that keeps the text's hue stays: red on bright yellow
        // (three tenths) and green on white (five) are darker shades of
        // themselves, not the ink.
        let white = Rgb::new(0xE5, 0xE5, 0xE5);
        for (fg, cell, hue) in [
            (Rgb::new(0xE0, 0x6C, 0x75), Rgb::new(0xF1, 0xFA, 0x8C), 0),
            (Rgb::new(0x98, 0xC3, 0x79), white, 1),
            (Rgb::new(0x50, 0xFA, 0x7B), white, 1),
        ] {
            let darker = floor_contrast(fg, cell, 4.5);
            let channels = [darker.r, darker.g, darker.b];
            assert_eq!(
                channels
                    .iter()
                    .enumerate()
                    .max_by_key(|&(_, &v)| v)
                    .map(|(i, _)| i),
                Some(hue),
                "{fg:?} on {cell:?} keeps its hue: {darker:?}"
            );
            assert_eq!(floor_contrast_on_colour(fg, cell, 4.5, dark), darker);
        }
    }

    /// aterm's own chromatic colours (slots 1–6 at their defaults) are held
    /// to 4.5:1 as text on Default's page by the theme-pair floor, and stay
    /// in their hue: on Windows blue `#2973D0` (3.9:1) draws `#3E81D5`, and
    /// faint blue 3:1 or more, while pwsh's directory names keep `#2973D0`
    /// as their ground with near-white text. A theme's own slot at the same
    /// 3.9:1 keeps its colour ([`THEME_BACKGROUND_FLOOR`]). Control: at
    /// [`THEME_BACKGROUND_FLOOR`] alone Windows' blue stays at 3.9:1.
    #[test]
    fn aterm_own_colours_are_held_to_body_text_on_the_page() {
        let (ink, page) = (Rgb::new(0xD0, 0xD0, 0xD0), Rgb::new(0x11, 0x13, 0x18));
        let on = StyleResolveOpts {
            indexed_minimum_contrast: 4.5,
            ..StyleResolveOpts::default()
        };
        let run = |pal: &ColorPalette, fg: PackedColor, bg: PackedColor, flags: CellFlags| {
            let cell = Cell::with_style('x', fg, bg, flags);
            resolve_colors_raw_opts(&cell, None, None, pal, ink, page, false, on)
        };
        let own = ColorPalette::new();
        let db = PackedColor::DEFAULT_BG;
        for slot in 1..=6u8 {
            let (fg, _) = run(&own, PackedColor::indexed(slot), db, CellFlags::empty());
            assert!(contrast(fg, page) >= 4.5, "slot {slot}: {fg:?}");
        }
        let windows_blue = Rgb::new(0x29, 0x73, 0xD0);
        assert!(
            contrast(
                floor_contrast(windows_blue, page, THEME_BACKGROUND_FLOOR),
                page
            ) < 4.0
        );
        let mut themed = ColorPalette::new();
        themed.set(4, windows_blue);
        let (fg, _) = run(&themed, PackedColor::indexed(4), db, CellFlags::empty());
        if cfg!(windows) {
            assert_eq!(fg, Rgb::new(0x3E, 0x81, 0xD5), "aterm's own blue, lifted");
            let (faint, _) = run(&themed, PackedColor::indexed(4), db, CellFlags::DIM);
            assert!(contrast(faint, page) >= 2.98, "faint blue: {faint:?}");
            let (dir, ground) = run(
                &themed,
                PackedColor::DEFAULT_FG,
                PackedColor::indexed(4),
                CellFlags::BOLD,
            );
            assert_eq!(ground, windows_blue);
            assert!(contrast(dir, ground) >= 4.5 && dir.r >= 0xF0, "{dir:?}");
        } else {
            assert_eq!(fg, windows_blue, "a theme's 3.9:1 slot keeps its tone");
        }
        // A theme's own blue a hair off aterm's, on every platform.
        let theirs = Rgb::new(0x29, 0x73, 0xD1);
        themed.set(4, theirs);
        let (fg, _) = run(&themed, PackedColor::indexed(4), db, CellFlags::empty());
        assert_eq!(fg, theirs, "a theme's 3.9:1 slot keeps its tone");
    }

    /// The theme-pair floor, on Default's dark colours with the palette blue
    /// macOS keeps (`#3B8EEA`). pwsh's directory style (the default
    /// foreground on slot 4, bold) is 2.18:1 unfloored and clears the full
    /// ratio floored; black on the theme's background is lifted only to
    /// [`THEME_BACKGROUND_FLOOR`]; an inverse pair is floored as drawn; faint
    /// recedes from the floored colour. A program's truecolor and 256-colour
    /// cube, shapes, hidden text and the theme's own default pair stay exact.
    /// Negative control: the floor off (`1.0`, the engine default) changes
    /// nothing.
    #[test]
    fn theme_pairs_are_floored_and_a_programs_own_colours_are_not() {
        let page = Rgb::new(0x11, 0x13, 0x18);
        let ink = Rgb::new(0xD0, 0xD0, 0xD0);
        let mut pal = ColorPalette::new();
        pal.set(0, Rgb::new(0, 0, 0));
        pal.set(4, Rgb::new(0x3B, 0x8E, 0xEA));
        let blue = pal.get(4);
        let on = StyleResolveOpts {
            indexed_minimum_contrast: 4.5,
            ..StyleResolveOpts::default()
        };
        let off = StyleResolveOpts::default();
        let d = PackedColor::DEFAULT_FG;
        let db = PackedColor::DEFAULT_BG;
        let ix = PackedColor::indexed;
        let run_screen =
            |ch: char, fg: PackedColor, bg: PackedColor, flags: CellFlags, opts, decscnm| {
                let cell = Cell::with_style(ch, fg, bg, flags);
                let fg_rgb = (fg.0 >> 24 == 1).then(|| {
                    let (r, g, b) = fg.rgb_components();
                    [r, g, b]
                });
                resolve_colors_raw_opts(&cell, fg_rgb, None, &pal, ink, page, decscnm, opts)
            };
        let run = |ch: char, fg: PackedColor, bg: PackedColor, flags: CellFlags, opts| {
            run_screen(ch, fg, bg, flags, opts, false)
        };

        // pwsh's `ESC[44;1m` directory names.
        let (fg, bg) = run('t', d, ix(4), CellFlags::BOLD, off);
        assert_eq!((fg, bg), (ink, blue), "off: the old 2.18:1");
        assert!(contrast(fg, bg) < 2.5);
        let (fg, bg) = run('t', d, ix(4), CellFlags::BOLD, on);
        assert_eq!(bg, blue, "only the foreground moves");
        assert!(contrast(fg, bg) >= 4.5, "{fg:?} on {bg:?}");

        // Black on the theme's own background: rescued to 3:1, not 4.5:1.
        let (fg, _) = run('x', ix(0), db, CellFlags::empty(), on);
        let c = contrast(fg, page);
        assert!((THEME_BACKGROUND_FLOOR..4.5).contains(&c), "{fg:?}: {c:.2}");

        // `ESC[7;34m`: the page colour drawn on blue, floored as drawn.
        let (fg, bg) = run('x', ix(4), db, CellFlags::INVERSE, on);
        assert_eq!(bg, blue);
        assert!(contrast(fg, bg) >= 4.5, "{fg:?} on {bg:?}");

        // `ESC[7;44m`: blue drawn on a block of the text colour, which is not
        // the page, so held to the full ratio as `ESC[44m`'s pair is.
        let (fg, bg) = run('x', d, ix(4), CellFlags::INVERSE, on);
        assert_eq!(bg, ink);
        assert!(contrast(fg, bg) >= 4.5, "{fg:?} on {bg:?}");

        // DECSCNM swaps the page: `ESC[44m` then draws blue on the page (the
        // default foreground colour), held to the page's cap only (3:1, or
        // 4.5:1 where this blue is aterm's own default, as off Windows);
        // `ESC[7;30m` draws black on a block of the default background, not
        // the page.
        let page_cap = if blue == ColorPalette::default_color(4) {
            DEFAULT_PALETTE_TEXT_FLOOR
        } else {
            THEME_BACKGROUND_FLOOR
        };
        let (fg, bg) = run_screen('x', d, ix(4), CellFlags::empty(), on, true);
        assert_eq!(bg, ink);
        let c = contrast(fg, bg);
        assert!((page_cap..page_cap + 1.5).contains(&c), "{fg:?}: {c:.2}");
        let (fg, bg) = run_screen('x', ix(0), db, CellFlags::INVERSE, on, true);
        assert_eq!(bg, page);
        assert!(contrast(fg, bg) >= 4.5, "{fg:?} on {bg:?}");

        // Faint recedes from the floored colour, toward the cell's bg.
        let (floored, _) = run('t', d, ix(4), CellFlags::empty(), on);
        let (faint, _) = run('t', d, ix(4), CellFlags::DIM, on);
        assert_eq!(faint, dim_toward_bg(floored, blue, DIM_FACTOR));

        // A default that merely equals a slot is floored (Default Light's
        // text is its color0, so `ESC[40m` hid it); one slot for both is a
        // deliberate fill and stays.
        let (fg, bg) = resolve_colors_raw_opts(
            &Cell::with_style('t', d, ix(4), CellFlags::empty()),
            None,
            None,
            &pal,
            blue,
            page,
            false,
            on,
        );
        assert!(contrast(fg, bg) >= 4.5, "{fg:?} on {bg:?}");

        // Exact: truecolor, the 256-colour cube, a half block, a powerline
        // separator, hidden text, a one-slot fill and the theme's default pair.
        let exact = [
            (
                't',
                PackedColor::rgb(0xD0, 0xD0, 0xD0),
                ix(4),
                CellFlags::empty(),
                ink,
            ),
            ('t', ix(17), ix(4), CellFlags::empty(), pal.get(17)),
            ('\u{2580}', d, ix(4), CellFlags::empty(), ink),
            ('\u{E0B0}', d, ix(4), CellFlags::empty(), ink),
            ('t', ix(4), ix(4), CellFlags::HIDDEN, blue),
            ('t', ix(4), ix(4), CellFlags::empty(), blue),
        ];
        for (ch, fg, bg, flags, want) in exact {
            assert_eq!(
                run(ch, fg, bg, flags, on).0,
                want,
                "{ch:?} {fg:?} {flags:?}"
            );
        }
        let dim_default = Rgb::new(0x20, 0x22, 0x28);
        let cell = Cell::with_style('t', d, db, CellFlags::empty());
        let (fg, _) =
            resolve_colors_raw_opts(&cell, None, None, &pal, dim_default, page, false, on);
        assert_eq!(fg, dim_default, "the theme's own body text is its business");
    }

    /// A built-in theme as the engine holds it: its 16 slots on a palette,
    /// its text colour and its page colour.
    fn builtin_palette(name: &str) -> (ColorPalette, Rgb, Rgb) {
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in");
        let mut pal = ColorPalette::new();
        for (slot, colour) in (0u8..).zip(scheme.ansi) {
            pal.set(slot, colour);
        }
        (pal, scheme.foreground, scheme.background)
    }

    /// Default Light draws white as dark text for its page (`#6B7280` and
    /// `#474D57`), so a program's white on a colour was dark on dark. Under
    /// the theme-pair floor it is the page colour wherever that reads better:
    /// white and bright white on black (`ESC[37;40m`, `ESC[97;40m`) are
    /// `#F6F7F9` at 12.8:1, where the floor's smallest nudge left the greys
    /// `#979CA6` (5.00:1) and `#91949A` (4.53:1), and bright white on a
    /// program's own 24-bit or 256-colour cell, which the floor leaves alone,
    /// is `#F6F7F9` at 6.6:1, where it was `#474D57` at 1.2:1. The test-runner
    /// badges (`97;41`, `97;42`, `97;44`, `37;41`, `1;37;41`; the first three
    /// are also pwsh's `White` on `DarkRed`, `DarkGreen` and `DarkBlue`),
    /// already the page colour under the floor, stay so, and black on green
    /// (`30;42`, 2.71:1 to 4.74:1) is the floor's: black is no white. pwsh's
    /// `White on Black` reaches the terminal as bright white on the page (the
    /// console host sends `Black` as the default background) and keeps the
    /// slate, 7.9:1. With the floor off every cell keeps the palette's
    /// colours (the `97;41` FAIL badge at 1.49:1). The page, shapes, hidden
    /// text and a one-slot fill keep the white. NEGATIVE CONTROL: without the
    /// rule (`resolve_both_with::<false>`) the floored greys and the 1.2:1
    /// slate come back.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one table of drawn pairs, each with its before and after"
    )]
    fn a_light_themes_dark_white_reads_light_on_a_colour() {
        let (pal, ink, page) = builtin_palette("Default Light");
        let (slate, grey) = (pal.get(15), pal.get(7));
        assert_eq!(slate, Rgb::new(0x47, 0x4D, 0x57));
        assert_eq!(grey, Rgb::new(0x6B, 0x72, 0x80));
        let opts = |ratio: f32| StyleResolveOpts {
            indexed_minimum_contrast: ratio,
            ..StyleResolveOpts::default()
        };
        let teal = [0x00, 0x5F, 0x87];
        let run = |light_whites: bool,
                   ch: char,
                   fg: PackedColor,
                   bg: PackedColor,
                   flags: CellFlags,
                   ratio: f32,
                   decscnm: bool| {
            let cell = Cell::with_style(ch, fg, bg, flags);
            let bg_rgb = (bg.0 >> 24 == 1).then_some(teal);
            if light_whites {
                resolve_colors_raw_opts(&cell, None, bg_rgb, &pal, ink, page, decscnm, opts(ratio))
            } else {
                resolve_both_with::<false>(
                    cell,
                    None,
                    bg_rgb,
                    &pal,
                    ink,
                    page,
                    decscnm,
                    opts(ratio),
                )
            }
        };
        let ix = PackedColor::indexed;
        let [r, g, b] = teal;
        let rgb = PackedColor::rgb(r, g, b);
        let (none, bold) = (CellFlags::empty(), CellFlags::BOLD);
        // (label, fg, bg, flags, floor off: fg and contrast, floored: fg and
        // contrast, floored without the rule)
        let cases = [
            ("97;41", ix(15), ix(1), none, slate, 1.49, page, 5.35, page),
            ("97;42", ix(15), ix(2), none, slate, 1.68, page, 4.74, page),
            ("97;44", ix(15), ix(4), none, slate, 1.35, page, 5.86, page),
            ("37;41", ix(7), ix(1), none, grey, 1.19, page, 5.35, page),
            ("1;37;41", ix(7), ix(1), bold, slate, 1.49, page, 5.35, page),
            (
                "97;40",
                ix(15),
                ix(0),
                none,
                slate,
                1.62,
                page,
                12.84,
                Rgb::new(0x91, 0x94, 0x9A),
            ),
            (
                "37;40",
                ix(7),
                ix(0),
                none,
                grey,
                2.85,
                page,
                12.84,
                Rgb::new(0x97, 0x9C, 0xA6),
            ),
            (
                "97 on 24-bit",
                ix(15),
                rgb,
                none,
                slate,
                1.21,
                page,
                6.56,
                slate,
            ),
            (
                "97 on 256:24",
                ix(15),
                ix(24),
                none,
                slate,
                1.21,
                page,
                6.56,
                slate,
            ),
            ("30;42", ix(0), ix(2), none, ink, 2.71, page, 4.74, page),
        ];
        for (label, fg, bg, flags, off_fg, off_c, on_fg, on_c, control) in cases {
            let (got, ground) = run(true, 'x', fg, bg, flags, 1.0, false);
            assert_eq!(got, off_fg, "{label}, the floor off");
            let c = contrast(got, ground);
            assert!((c - off_c).abs() < 0.01, "{label}, the floor off: {c:.2}");
            let (got, ground) = run(true, 'x', fg, bg, flags, 4.5, false);
            assert_eq!(got, on_fg, "{label}, floored");
            let c = contrast(got, ground);
            assert!((c - on_c).abs() < 0.01, "{label}, floored: {c:.2}");
            let (without, _) = run(false, 'x', fg, bg, flags, 4.5, false);
            assert_eq!(without, control, "{label}: CONTROL, the floor alone");
        }
        assert!(contrast(Rgb::new(0x91, 0x94, 0x9A), ink) < 4.6);

        // An SGR 7 block and DECSCNM draw a slot's white on the text colour.
        let d = PackedColor::DEFAULT_FG;
        let db = PackedColor::DEFAULT_BG;
        assert_eq!(
            run(true, 'x', d, ix(7), CellFlags::INVERSE, 4.5, false),
            (page, ink)
        );
        assert_eq!(run(true, 'x', d, ix(7), none, 4.5, true), (page, ink));
        // White on a white of another slot is text on a colour; one slot for
        // both is a fill.
        assert_eq!(run(true, 'x', ix(7), ix(15), none, 4.5, false).0, page);
        assert_eq!(run(true, 'x', ix(15), ix(15), none, 4.5, false).0, slate);
        // The page, an SGR 7 white block, shapes and hidden text keep theirs.
        for (ch, fg, bg, flags, want) in [
            ('x', ix(15), db, none, (slate, page)),
            ('x', ix(15), db, CellFlags::INVERSE, (page, slate)),
            ('\u{2580}', ix(15), ix(4), none, (slate, pal.get(4))),
            ('\u{E0B0}', ix(15), ix(4), none, (slate, pal.get(4))),
            ('\u{2500}', ix(7), ix(4), none, (grey, pal.get(4))),
            (
                'x',
                ix(15),
                ix(1),
                CellFlags::HIDDEN,
                (pal.get(1), pal.get(1)),
            ),
        ] {
            assert_eq!(run(true, ch, fg, bg, flags, 4.5, false), want, "{ch:?}");
        }
    }

    /// The rule moves nothing but a light theme's dark white on a colour, and
    /// only under the floor: over every built-in theme, the default colours,
    /// the 16 slots and a 24-bit cell, plain, bold, inverse, faint and hidden,
    /// with and without DECSCNM, a dark theme, Solarized Light (whose whites
    /// are light colours, 1.9:1 on its page) and the floor off resolve
    /// exactly as without it. A cell on the page never moves, its background
    /// never moves, and a moved white is at least as legible as before, or
    /// at the floor. Aterm Day, Linen, Default Light, Gruvbox Light, GitHub
    /// Light and Catppuccin Latte each move some cell (non-vacuity).
    #[test]
    fn only_a_light_themes_dark_white_on_a_colour_moves() {
        let flag_sets = [
            CellFlags::empty(),
            CellFlags::BOLD,
            CellFlags::INVERSE,
            CellFlags::BOLD.union(CellFlags::INVERSE),
            CellFlags::DIM,
            CellFlags::HIDDEN,
        ];
        let colours: Vec<PackedColor> = std::iter::once(PackedColor::DEFAULT_FG)
            .chain((0..16).map(PackedColor::indexed))
            .collect();
        let grounds: Vec<PackedColor> = std::iter::once(PackedColor::DEFAULT_BG)
            .chain((0..16).map(PackedColor::indexed))
            .chain(std::iter::once(PackedColor::rgb(0x00, 0x5F, 0x87)))
            .collect();
        let mut moved = std::collections::BTreeMap::new();
        for name in aterm_types::scheme::builtin_names() {
            let (pal, ink, page) = builtin_palette(name);
            let light = wcag_luminance(page) > wcag_luminance(ink);
            for &fg in &colours {
                for &bg in &grounds {
                    let bg_rgb = (bg.0 >> 24 == 1).then_some([0x00, 0x5F, 0x87]);
                    for flags in flag_sets {
                        for decscnm in [false, true] {
                            for ratio in [1.0f32, 4.5] {
                                let cell = Cell::with_style('x', fg, bg, flags);
                                let opts = StyleResolveOpts {
                                    indexed_minimum_contrast: ratio,
                                    ..StyleResolveOpts::default()
                                };
                                let args = (cell, None, bg_rgb, &pal, ink, page, decscnm, opts);
                                let new = resolve_colors_raw_opts(
                                    &args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7,
                                );
                                let old = resolve_both_with::<false>(
                                    args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7,
                                );
                                if new == old {
                                    continue;
                                }
                                let what = format!("{name}: {fg:?} on {bg:?} {flags:?} {decscnm}");
                                assert!(light && name != "Solarized Light", "{what}");
                                assert!(ratio > 1.0, "{what}: the floor off");
                                assert_eq!(new.1, old.1, "{what}: the ground stays");
                                let on_page = bg == PackedColor::DEFAULT_BG
                                    && !flags.contains(CellFlags::INVERSE)
                                    && !decscnm;
                                assert!(!on_page, "{what}: the page keeps its white");
                                if !flags.contains(CellFlags::DIM) {
                                    let (c_new, c_old) =
                                        (contrast(new.0, new.1), contrast(old.0, old.1));
                                    assert!(
                                        c_new >= c_old.min(4.5) - 1e-3,
                                        "{what}: {c_new:.2} < {c_old:.2}"
                                    );
                                }
                                *moved.entry(name).or_insert(0u32) += 1;
                            }
                        }
                    }
                }
            }
        }
        for name in [
            "Aterm Day",
            "Linen",
            "Default Light",
            "Gruvbox Light",
            "GitHub Light",
            "Catppuccin Latte",
        ] {
            assert!(
                moved.get(name).copied().unwrap_or(0) > 0,
                "{name}: {moved:?}"
            );
        }
    }
}
