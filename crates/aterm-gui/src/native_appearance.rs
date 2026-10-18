// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Perceptual appearance system shared by every native tab app and chrome surface.
//!
//! The terminal theme is the source of identity, but native UI roles are not
//! painted from raw cursor/selection bytes: they are rebuilt in OKLCH,
//! gamut-mapped, and contrast-conditioned for the surface they sit on, so vivid
//! schemes stay expressive without neon controls and quiet/light schemes keep
//! their hierarchy.

#[cfg(test)]
use std::cell::Cell;
use std::f32::consts::TAU;
#[cfg(all(not(test), any(target_os = "macos", windows)))]
use std::sync::atomic::AtomicBool;
#[cfg(not(test))]
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use aterm_render::Theme;

/// Accessibility inputs that affect role contrast. The host replaces the defaults when
/// a platform appearance observer reports a change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AppearancePreferences {
    pub(crate) high_contrast: bool,
    pub(crate) reduced_transparency: bool,
    /// Native-app text scale, independent of terminal-cell zoom.
    pub(crate) text_scale: f32,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            high_contrast: false,
            reduced_transparency: false,
            text_scale: 1.0,
        }
    }
}

#[cfg(not(test))]
const HIGH_CONTRAST: u8 = 1 << 0;
#[cfg(not(test))]
const REDUCED_TRANSPARENCY: u8 = 1 << 1;
#[cfg(not(test))]
static ACCESSIBILITY_FLAGS: AtomicU8 = AtomicU8::new(0);
#[cfg(not(test))]
static TEXT_SCALE_BITS: AtomicU32 = AtomicU32::new(1.0_f32.to_bits());

/// Sticky flag: an OS-owned chrome input moved and no re-sample has settled the windows
/// for it yet. Set by [`install_preferences`] and
/// [`crate::chrome_band::install_forced_chrome`]; drained only by
/// `App::resample_os_preferences`, through [`take_chrome_inputs_moved`].
///
/// A latch rather than the `install_*` return value, because that edge goes to whoever
/// reads first: `AppRt::native_appearance_preferences` also runs at window attach, at
/// startup and in the settings preview (on Windows it republishes the High-Contrast
/// palette), and a move one of them consumed would leave existing windows painting the
/// old OS palette.
#[cfg(all(not(test), any(target_os = "macos", windows)))]
static CHROME_INPUTS_MOVED: AtomicBool = AtomicBool::new(false);

// Per worker thread under test, as with the snapshot below: libtest runs
// independent Apps on a reused worker pool, so a process-global latch would let
// one worker drain another's pending settle.
#[cfg(test)]
thread_local! {
    static TEST_CHROME_INPUTS_MOVED: Cell<bool> = const { Cell::new(false) };
}

/// Record that an OS-owned chrome input moved. Idempotent; see
/// [`CHROME_INPUTS_MOVED`].
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn note_chrome_inputs_moved() {
    #[cfg(test)]
    TEST_CHROME_INPUTS_MOVED.with(|slot| slot.set(true));
    #[cfg(not(test))]
    CHROME_INPUTS_MOVED.store(true, Ordering::Release);
}

/// Consume the latch: `true` when an OS-owned chrome input has moved since the
/// last re-sample settled the windows. The one caller is
/// `App::resample_os_preferences`.
#[must_use]
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn take_chrome_inputs_moved() -> bool {
    #[cfg(test)]
    {
        TEST_CHROME_INPUTS_MOVED.with(|slot| slot.replace(false))
    }

    #[cfg(not(test))]
    {
        CHROME_INPUTS_MOVED.swap(false, Ordering::AcqRel)
    }
}

// One process-wide snapshot in production; per worker thread under test, so one test
// cannot change another's layout mid-compile/raster.
#[cfg(test)]
thread_local! {
    static TEST_APPEARANCE: Cell<AppearancePreferences> = const {
        Cell::new(AppearancePreferences {
            high_contrast: false,
            reduced_transparency: false,
            text_scale: 1.0,
        })
    };
}

/// Install the platform-observed appearance snapshot. Returns `true` only when
/// paint/layout inputs changed, so the host can skip redundant redraws; a `true` also
/// arms [`CHROME_INPUTS_MOVED`].
pub(crate) fn install_preferences(preferences: AppearancePreferences) -> bool {
    let preferences = preferences.normalized();
    let moved = {
        #[cfg(test)]
        {
            TEST_APPEARANCE.with(|snapshot| snapshot.replace(preferences) != preferences)
        }

        #[cfg(not(test))]
        {
            let flags = (u8::from(preferences.high_contrast) * HIGH_CONTRAST)
                | (u8::from(preferences.reduced_transparency) * REDUCED_TRANSPARENCY);
            let old_flags = ACCESSIBILITY_FLAGS.swap(flags, Ordering::AcqRel);
            let old_scale =
                TEXT_SCALE_BITS.swap(preferences.text_scale.to_bits(), Ordering::AcqRel);
            old_flags != flags || old_scale != preferences.text_scale.to_bits()
        }
    };
    // Only the hosts that re-sample OS preferences read the latch.
    #[cfg(any(target_os = "macos", windows, test))]
    if moved {
        note_chrome_inputs_moved();
    }
    moved
}

#[must_use]
pub(crate) fn current_preferences() -> AppearancePreferences {
    #[cfg(test)]
    {
        TEST_APPEARANCE.with(Cell::get).normalized()
    }

    #[cfg(not(test))]
    {
        let flags = ACCESSIBILITY_FLAGS.load(Ordering::Acquire);
        AppearancePreferences {
            high_contrast: flags & HIGH_CONTRAST != 0,
            reduced_transparency: flags & REDUCED_TRANSPARENCY != 0,
            text_scale: f32::from_bits(TEXT_SCALE_BITS.load(Ordering::Acquire)),
        }
        .normalized()
    }
}

#[must_use]
pub(crate) fn text_scale() -> f32 {
    current_preferences().text_scale
}

impl AppearancePreferences {
    pub(crate) fn normalized(self) -> Self {
        Self {
            high_contrast: self.high_contrast,
            reduced_transparency: self.reduced_transparency,
            text_scale: if self.text_scale.is_finite() && self.text_scale > 0.0 {
                self.text_scale.clamp(0.85, 2.0)
            } else {
                1.0
            },
        }
    }
}

/// Complete semantic palette for a native surface. No application may invent a
/// component colour outside this vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeRoles {
    pub(crate) surface: [u8; 3],
    pub(crate) text_primary: [u8; 3],
    pub(crate) text_secondary: [u8; 3],
    pub(crate) text_tertiary: [u8; 3],
    pub(crate) accent: [u8; 3],
    /// The accent as text (a selected rail row's or segment's label, an accent heading,
    /// a preview's caption): its hue held to the text floor (4.5:1, 7:1 in high contrast)
    /// on every backdrop it is inked on. [`Self::accent`] keeps its own floor for fills,
    /// rims and bars.
    pub(crate) accent_ink: [u8; 3],
    pub(crate) on_accent: [u8; 3],
    pub(crate) separator: [u8; 3],
    pub(crate) control_track: [u8; 3],
    pub(crate) elevated: [u8; 3],
    pub(crate) danger: [u8; 3],
    pub(crate) success: [u8; 3],
}

#[derive(Clone, Copy, Debug)]
struct Oklch {
    l: f32,
    c: f32,
    h: f32,
}

/// Whether this chrome background is the dark side: [`crate::tab_bar::bg_is_light`], the
/// predicate the tab strip's tones and the CSD variant use. A separate threshold would
/// disagree in the mid-grey band and make `window_theme = light` a no-op there
/// ([`crate::App::chrome_palette_theme`] would see "already light" while this pipeline
/// built the dark side).
pub(crate) fn surface_is_dark(bg: [u8; 3]) -> bool {
    !crate::tab_bar::bg_is_light(bg)
}

/// How far toward `accent` a selected navigation row's `elevated` fill is
/// washed when the accent itself is the ink (label and icon); pressing the
/// row deepens it to [`ACCENT_WASH_PRESSED`]. Owned here so
/// [`accent_over_its_own_wash`] and the pixels `native_ui` produces agree.
pub(crate) const ACCENT_WASH_SELECTED: f32 = 0.13;
/// The deepest accent wash that still carries accent ink (the pressed state of
/// a selected navigation row). Deeper washes carry text_primary/secondary,
/// already floored against `surface`.
pub(crate) const ACCENT_WASH_PRESSED: f32 = 0.16;
/// The floor the accent holds as ink against the wash it is inked on. A rule
/// or fill can be quieter than its surroundings; a label cannot.
const ACCENT_INK_FLOOR: f32 = 3.0;

/// Straight sRGB channel lerp, the mix the native painter builds control fills
/// with ([`crate::native_ui::mix_rgb`] delegates here), as distinct from the
/// perceptual [`mix_oklab`] the role ramps use. The accent floor must measure
/// the exact bytes the painter puts on screen.
pub(crate) fn mix_srgb(a: [u8; 3], b: [u8; 3], amount: f32) -> [u8; 3] {
    let amount = amount.clamp(0.0, 1.0);
    std::array::from_fn(|index| {
        (f32::from(a[index]) + (f32::from(b[index]) - f32::from(a[index])) * amount).round() as u8
    })
}

/// Hold the accent to [`ACCENT_INK_FLOOR`] against its own wash: the selected rail row is
/// the one place the accent is body-sized ink on a backdrop built from the accent, so a
/// floor against `surface` alone can leave it the least legible row.
///
/// A fixpoint: each pass re-mixes the wash from the current accent and nudges lightness
/// only as far as that wash demands. The wash moves only [`ACCENT_WASH_PRESSED`] toward
/// the accent, so the residual shrinks by about that factor per pass and the loop settles
/// in two or three; the bound guarantees termination.
fn accent_over_its_own_wash(accent: [u8; 3], elevated: [u8; 3]) -> [u8; 3] {
    let mut accent = accent;
    for _ in 0..8 {
        let wash = mix_srgb(elevated, accent, ACCENT_WASH_PRESSED);
        if contrast_ratio(accent, wash) >= ACCENT_INK_FLOOR {
            break;
        }
        accent = ensure_contrast(accent, wash, ACCENT_INK_FLOOR);
    }
    accent
}

/// The shared semantic angle of the `danger` family.
const DANGER_HUE_DEGREES: f32 = 25.0;
/// The shared semantic angle of the `success` family.
const SUCCESS_HUE_DEGREES: f32 = 145.0;

/// A status hue as [`roles`] seeds it, before the ink floor: one shared
/// semantic angle, at a lightness this chrome side chooses and a chroma the
/// accessibility preferences choose.
fn status_seed(dark: bool, high_contrast: bool, hue_degrees: f32) -> [u8; 3] {
    oklch_to_rgb(Oklch {
        l: if dark { 0.72 } else { 0.49 },
        c: if high_contrast { 0.16 } else { 0.14 },
        h: hue_degrees.to_radians(),
    })
}

/// The `success` hue as decoration rather than ink: the seed [`roles`] conditions into
/// [`NativeRoles::success`], without that role's 4.5:1 floor. Against a mid-tone surface
/// the floor drives the role near-black, right for a label but a grey, not green, tint.
/// Nothing painted with this is text or a UI mark.
///
/// `dark` is the chrome side the decoration lands on, which in the mid-tone band may
/// differ from [`roles`]' answer for the window. Chroma follows the live accessibility
/// preferences, as in [`default_roles`].
pub(crate) fn success_tint(dark: bool) -> [u8; 3] {
    status_seed(
        dark,
        current_preferences().normalized().high_contrast,
        SUCCESS_HUE_DEGREES,
    )
}

/// Hold a surface at or above the openness (luminance) of `floor`, moving lightness only
/// (the hue- and chroma-preserving nudge [`ensure_contrast`] makes for ink). Contrast
/// against black is monotone in relative luminance, so this is a contrast target: the
/// smallest lift that clears the floor, or `color` untouched when it already does.
///
/// A chromatic wash is not monotone in luminance: [`crate::native_ui::rainbow_banner_sky`]
/// pours [`success_tint`] into the most open neutral and uses this so the banner cannot
/// sink below its cards.
pub(crate) fn at_least_as_open_as(color: [u8; 3], floor: [u8; 3]) -> [u8; 3] {
    ensure_contrast(color, [0, 0, 0], contrast_ratio(floor, [0, 0, 0]))
}

/// Build the native palette. Neutral roles preserve the theme's perceptual hue;
/// chromatic roles keep the chrome accent's hue
/// ([`aterm_messages::tokens::ChromeTokens::accent`]: the authored accent, the
/// cursor, or the ANSI blue or cyan it borrows) but cap its chroma for large
/// controls. A palette whose accent is Neutral (a monochrome theme, or one with
/// no palette known) keeps the derived treatment: the selection's hue, else one
/// opposite the background's. An authored `border_color` is the separator, and
/// the theme's own ANSI red and green set the status hues.
pub(crate) fn roles(
    p: impl Into<crate::chrome_band::ChromePalette>,
    preferences: AppearancePreferences,
) -> NativeRoles {
    let p = p.into();
    let theme = p.theme;
    let tokens = p.tokens();
    let preferences = preferences.normalized();
    let bg = packed_rgb(theme.bg);
    let fg = packed_rgb(theme.fg);
    let selection = packed_rgb(theme.selection);
    let dark = surface_is_dark(bg);

    let surface = neutral_surface(bg, fg, if dark { 0.075 } else { 0.055 });
    let elevated = neutral_surface(surface, fg, if dark { 0.075 } else { 0.065 });
    let primary_target = if preferences.high_contrast { 7.0 } else { 4.5 };
    let secondary_target = if preferences.high_contrast { 7.0 } else { 4.5 };
    let tertiary_target = if preferences.high_contrast { 4.5 } else { 3.0 };
    let text_primary = ensure_contrast(fg, surface, primary_target);
    let text_secondary = ensure_contrast(mix_oklab(fg, surface, 0.27), surface, secondary_target);
    let text_tertiary = ensure_contrast(mix_oklab(fg, surface, 0.48), surface, tertiary_target);

    let mut accent_seed = rgb_to_oklch(packed_rgb(theme.cursor));
    let selection_seed = rgb_to_oklch(selection);
    if tokens.accent_source != aterm_messages::tokens::AccentSource::Neutral {
        accent_seed = rgb_to_oklch(tokens.accent);
    }
    if accent_seed.c < 0.035 && selection_seed.c >= 0.035 {
        accent_seed = selection_seed;
    }
    if accent_seed.c < 0.035 {
        // A neutral theme still gets identity, but the hue is derived from the
        // selection/background relationship rather than a product-wide RGB token.
        let bg_hue = rgb_to_oklch(bg).h;
        accent_seed.h = (bg_hue + 0.61 * TAU).rem_euclid(TAU);
    }
    accent_seed.c = accent_seed.c.clamp(0.085, if dark { 0.155 } else { 0.145 });
    accent_seed.l = if dark {
        accent_seed.l.clamp(0.66, 0.79)
    } else {
        accent_seed.l.clamp(0.42, 0.58)
    };
    // Floored against both backdrops the accent meets: the surface it rules and
    // fills on, and the selected-row wash it inks on.
    let accent = ensure_contrast(oklch_to_rgb(accent_seed), surface, ACCENT_INK_FLOOR);
    let accent = accent_over_its_own_wash(accent, elevated);
    // The accent as text, floored on the page, a card and the deepest wash it is
    // inked on: all three sit on one side of it, so meeting the floor on each meets
    // it on every wash between them.
    let ink_target = if preferences.high_contrast { 7.0 } else { 4.5 };
    let accent_ink = [
        surface,
        elevated,
        mix_srgb(elevated, accent, ACCENT_WASH_PRESSED),
    ]
    .into_iter()
    .fold(accent, |ink, backdrop| {
        ensure_contrast(ink, backdrop, ink_target)
    });

    let black = [8, 10, 13];
    let white = [250, 251, 253];
    let on_seed = if contrast_ratio(black, accent) >= contrast_ratio(white, accent) {
        black
    } else {
        white
    };
    let on_accent = ensure_contrast(on_seed, accent, 4.5);

    let separator_target = if preferences.high_contrast { 3.0 } else { 1.5 };
    let separator = match tokens.border {
        Some(border) => ensure_contrast(border, surface, separator_target),
        None => ensure_contrast(mix_oklab(fg, surface, 0.70), surface, separator_target),
    };
    let control_track = ensure_contrast(mix_oklab(fg, surface, 0.57), surface, 2.0);

    // Status hues take the theme's own ANSI red and green where they carry a
    // colour, else a shared semantic angle; lightness, chroma and final
    // contrast are conditioned by this theme.
    // Held against the cards they mostly sit on as well as the page.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an OkLCh hue in degrees, [0, 360), fits f32"
    )]
    let hue = |token: Option<f64>, fallback: f32| token.map_or(fallback, |h| h as f32);
    let danger = ensure_contrast(
        ensure_contrast(
            status_seed(
                dark,
                preferences.high_contrast,
                hue(tokens.danger_hue, DANGER_HUE_DEGREES),
            ),
            surface,
            4.5,
        ),
        elevated,
        4.5,
    );
    let success = ensure_contrast(
        ensure_contrast(
            status_seed(
                dark,
                preferences.high_contrast,
                hue(tokens.success_hue, SUCCESS_HUE_DEGREES),
            ),
            surface,
            4.5,
        ),
        elevated,
        4.5,
    );

    NativeRoles {
        surface,
        text_primary,
        text_secondary,
        text_tertiary,
        accent,
        accent_ink,
        on_accent,
        separator,
        control_track,
        elevated,
        danger,
        success,
    }
}

impl NativeRoles {
    /// The roles of a chrome palette (a bare [`Theme`] converts with nothing
    /// authored and no palette known), under the live accessibility
    /// preferences ([`default_roles`]).
    pub(crate) fn from_theme(p: impl Into<crate::chrome_band::ChromePalette>) -> Self {
        default_roles(p)
    }
}

pub(crate) fn default_roles(p: impl Into<crate::chrome_band::ChromePalette>) -> NativeRoles {
    roles(p, current_preferences())
}

/// The forced-chrome seed palette for config `window_theme = light`, modelled
/// on libadwaita light (`window_bg_color #fafafa`, text `rgba(0,0,0,0.8)`).
/// [`roles`] derives the full token set from these seeds through the same
/// OKLCH mix and contrast conditioning every terminal theme goes through.
pub(crate) const FORCED_LIGHT_CHROME_BG: u32 = 0x00FA_FAFA;
/// Dark text for the forced-light chrome (Adwaita's `#2e3436`).
pub(crate) const FORCED_LIGHT_CHROME_FG: u32 = 0x002E_3436;
/// The forced-chrome seed palette for config `window_theme = dark`
/// (libadwaita dark: `window_bg_color #242424`, near-white text).
pub(crate) const FORCED_DARK_CHROME_BG: u32 = 0x0024_2424;
/// Near-white text for the forced-dark chrome.
pub(crate) const FORCED_DARK_CHROME_FG: u32 = 0x00ED_EDED;

/// The floor the carried terminal cursor (the accent seed) is held to against
/// the forced chrome surface.
///
/// A floor on the input, not the shipped accent: [`roles`] applies
/// [`ACCENT_INK_FLOOR`] itself, against both the surface and the row's own
/// wash ([`accent_over_its_own_wash`]). This catches a degenerate seed, e.g. a
/// dark theme's near-white cursor carried onto `#FAFAFA`, which would give
/// [`roles`] no usable lightness and a hue that is mostly rounding noise.
const FORCED_CHROME_ACCENT_FLOOR: f32 = ACCENT_INK_FLOOR;

/// The [`Theme`] the chrome painters draw from when config `window_theme = light|dark`
/// forces the side against the terminal theme's darkness
/// ([`crate::App::chrome_palette_theme`]). Surfaces and text are the constants above; the
/// cursor and selection (the accent seeds) are carried from the terminal theme, the cursor
/// lightness-nudged so it stays visible on the forced surface.
pub(crate) fn forced_chrome_theme(terminal: Theme, dark: bool) -> Theme {
    let (bg, fg) = if dark {
        (FORCED_DARK_CHROME_BG, FORCED_DARK_CHROME_FG)
    } else {
        (FORCED_LIGHT_CHROME_BG, FORCED_LIGHT_CHROME_FG)
    };
    let cursor = ensure_contrast(
        packed_rgb(terminal.cursor),
        packed_rgb(bg),
        FORCED_CHROME_ACCENT_FLOOR,
    );
    Theme {
        fg,
        bg,
        cursor: pack_rgb(cursor),
        selection: terminal.selection,
    }
}

/// The chrome palette for a `window_theme` policy over a `terminal` theme: the pure
/// core of `App::chrome_palette_theme`, testable on any host. `Auto` returns the
/// terminal theme (the caption painter tracks the OS), as does a forced value matching
/// its darkness; a forced value against it authors the forced-variant chrome so the
/// band and native pages flip together with the OS caption.
pub(crate) fn resolve_chrome_palette(
    window_theme: crate::app_config::WindowTheme,
    terminal: Theme,
) -> Theme {
    let force_dark = match window_theme {
        crate::app_config::WindowTheme::Auto => return terminal,
        crate::app_config::WindowTheme::Light => false,
        crate::app_config::WindowTheme::Dark => true,
    };
    if crate::tab_bar::theme_is_dark(terminal.bg) == force_dark {
        return terminal;
    }
    forced_chrome_theme(terminal, force_dark)
}

/// Inverse of [`packed_rgb`] (0x00RR_GGBB).
fn pack_rgb(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

fn packed_rgb(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

fn srgb_to_linear(v: u8) -> f32 {
    let value = f32::from(v) / 255.0;
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn rgb_to_oklab(rgb: [u8; 3]) -> [f32; 3] {
    let r = srgb_to_linear(rgb[0]);
    let g = srgb_to_linear(rgb[1]);
    let b = srgb_to_linear(rgb[2]);
    let l = 0.412_221_46_f32.mul_add(r, 0.536_332_55_f32.mul_add(g, 0.051_445_995 * b));
    let m = 0.211_903_5_f32.mul_add(r, 0.680_699_5_f32.mul_add(g, 0.107_396_96 * b));
    let s = 0.088_302_46_f32.mul_add(r, 0.281_718_85_f32.mul_add(g, 0.629_978_7 * b));
    let l = l.cbrt();
    let m = m.cbrt();
    let s = s.cbrt();
    [
        0.210_454_26_f32.mul_add(l, 0.793_617_8_f32.mul_add(m, -0.004_072_047 * s)),
        1.977_998_5_f32.mul_add(l, (-2.428_592_2_f32).mul_add(m, 0.450_593_7 * s)),
        0.025_904_037_f32.mul_add(l, 0.782_771_77_f32.mul_add(m, -0.808_675_77 * s)),
    ]
}

fn rgb_to_oklch(rgb: [u8; 3]) -> Oklch {
    let [l, a, b] = rgb_to_oklab(rgb);
    Oklch {
        l,
        c: a.hypot(b),
        h: b.atan2(a).rem_euclid(TAU),
    }
}

fn oklab_to_linear(lab: [f32; 3]) -> [f32; 3] {
    let [lightness, a, b] = lab;
    let l = (lightness + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m = (lightness - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s = (lightness - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    [
        4.076_741_7_f32.mul_add(l, (-3.307_711_6_f32).mul_add(m, 0.230_969_94 * s)),
        (-1.268_438_f32).mul_add(l, 2.609_757_4_f32.mul_add(m, -0.341_319_38 * s)),
        (-0.004_196_086_3_f32).mul_add(l, (-0.703_418_6_f32).mul_add(m, 1.707_614_7 * s)),
    ]
}

fn raw_oklch_to_srgb(color: Oklch) -> [f32; 3] {
    let a = color.c * color.h.cos();
    let b = color.c * color.h.sin();
    oklab_to_linear([color.l, a, b]).map(linear_to_srgb)
}

fn in_gamut(rgb: [f32; 3]) -> bool {
    rgb.iter()
        .all(|value| (-0.000_01..=1.000_01).contains(value))
}

/// Convert with chroma-preserving gamut mapping. Lightness and hue remain stable;
/// only chroma is reduced when the requested colour is outside sRGB.
fn oklch_to_rgb(mut color: Oklch) -> [u8; 3] {
    color.l = color.l.clamp(0.0, 1.0);
    color.c = color.c.max(0.0);
    let direct = raw_oklch_to_srgb(color);
    let mapped = if in_gamut(direct) {
        direct
    } else {
        let mut lo = 0.0;
        let mut hi = color.c;
        let mut best = raw_oklch_to_srgb(Oklch { c: 0.0, ..color });
        for _ in 0..18 {
            let mid = (lo + hi) * 0.5;
            let candidate = raw_oklch_to_srgb(Oklch { c: mid, ..color });
            if in_gamut(candidate) {
                lo = mid;
                best = candidate;
            } else {
                hi = mid;
            }
        }
        best
    };
    mapped.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn mix_oklab(a: [u8; 3], b: [u8; 3], amount: f32) -> [u8; 3] {
    let amount = amount.clamp(0.0, 1.0);
    let aa = rgb_to_oklab(a);
    let bb = rgb_to_oklab(b);
    let lab: [f32; 3] = std::array::from_fn(|index| aa[index] + (bb[index] - aa[index]) * amount);
    let c = lab[1].hypot(lab[2]);
    oklch_to_rgb(Oklch {
        l: lab[0],
        c,
        h: lab[2].atan2(lab[1]).rem_euclid(TAU),
    })
}

/// Native neutral surfaces keep theme atmosphere but cannot inherit enough chroma
/// to turn the entire application shell into a saturated color field.
fn neutral_surface(a: [u8; 3], b: [u8; 3], amount: f32) -> [u8; 3] {
    let mixed = mix_oklab(a, b, amount);
    let source = rgb_to_oklch(a);
    let mut neutral = rgb_to_oklch(mixed);
    neutral.c = neutral.c.min(source.c * 0.35).min(0.035);
    oklch_to_rgb(neutral)
}

fn relative_luminance(rgb: [u8; 3]) -> f32 {
    0.2126_f32.mul_add(
        srgb_to_linear(rgb[0]),
        0.7152_f32.mul_add(srgb_to_linear(rgb[1]), 0.0722 * srgb_to_linear(rgb[2])),
    )
}

pub(crate) fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f32 {
    let high = relative_luminance(a).max(relative_luminance(b));
    let low = relative_luminance(a).min(relative_luminance(b));
    (high + 0.05) / (low + 0.05)
}

fn contrast_candidate(color: Oklch, bg: [u8; 3], target: f32, pole: f32) -> Option<[u8; 3]> {
    let pole_color = oklch_to_rgb(Oklch { l: pole, ..color });
    if contrast_ratio(pole_color, bg) < target {
        return None;
    }
    let mut lo = 0.0;
    let mut hi = 1.0;
    let mut best = pole_color;
    for _ in 0..18 {
        let t = (lo + hi) * 0.5;
        let candidate = oklch_to_rgb(Oklch {
            l: color.l + (pole - color.l) * t,
            ..color
        });
        if contrast_ratio(candidate, bg) >= target {
            hi = t;
            best = candidate;
        } else {
            lo = t;
        }
    }
    Some(best)
}

/// `fg` as text on `bg`: its hue kept, its lightness moved until it reads at
/// the text floor (4.5:1, 7:1 in high contrast).
pub(crate) fn ensure_text_contrast(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    let target = if current_preferences().normalized().high_contrast {
        7.0
    } else {
        4.5
    };
    ensure_contrast(fg, bg, target)
}

/// Adjust OKLCH lightness by the smallest available amount to meet `target`.
fn ensure_contrast(fg: [u8; 3], bg: [u8; 3], target: f32) -> [u8; 3] {
    if contrast_ratio(fg, bg) >= target {
        return fg;
    }
    let source = rgb_to_oklch(fg);
    let dark = contrast_candidate(source, bg, target, 0.0);
    let light = contrast_candidate(source, bg, target, 1.0);
    match (dark, light) {
        (Some(a), Some(b)) => {
            let da = (rgb_to_oklch(a).l - source.l).abs();
            let db = (rgb_to_oklch(b).l - source.l).abs();
            if da <= db { a } else { b }
        }
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => {
            let black = [0, 0, 0];
            let white = [255, 255, 255];
            if contrast_ratio(black, bg) >= contrast_ratio(white, bg) {
                black
            } else {
                white
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A preference move consumed by another caller (window attach, startup,
    /// settings preview) still reaches the re-sample that settles the windows.
    #[test]
    fn a_consumed_preference_edge_still_reaches_the_resample() {
        let previous = current_preferences();
        let _ = take_chrome_inputs_moved();

        let moved = AppearancePreferences {
            high_contrast: !previous.high_contrast,
            reduced_transparency: !previous.reduced_transparency,
            text_scale: 1.25,
        };
        // The attach reads first and takes the edge.
        assert!(install_preferences(moved), "off → on is a change");
        assert!(
            !install_preferences(moved),
            "the edge is gone: a second install reports no movement"
        );
        // The re-sample runs later and must still be told to settle the windows.
        assert!(
            take_chrome_inputs_moved(),
            "the preference move was swallowed by the non-resample reader"
        );
        assert!(
            !take_chrome_inputs_moved(),
            "the latch is drained by its one consumer"
        );

        install_preferences(previous);
        let _ = take_chrome_inputs_moved();
    }

    const THEMES: [Theme; 6] = [
        Theme {
            fg: 0x00D0_D0D0,
            bg: 0x0011_1318,
            cursor: 0x0050_FA7B,
            selection: 0x0033_415E,
        },
        Theme {
            fg: 0x0024_293A,
            bg: 0x00FA_F9F5,
            cursor: 0x004C_6FFF,
            selection: 0x00D8_E2FF,
        },
        Theme {
            fg: 0x00FF_FFFF,
            bg: 0x0000_0000,
            cursor: 0x00FF_FFFF,
            selection: 0x0040_4040,
        },
        Theme {
            fg: 0x0000_0000,
            bg: 0x00FF_FFFF,
            cursor: 0x0000_0000,
            selection: 0x00C8_C8C8,
        },
        Theme {
            fg: 0x00FF_E8F3,
            bg: 0x0028_0F22,
            cursor: 0x00FF_4FA3,
            selection: 0x006B_244F,
        },
        Theme {
            fg: 0x00E4_F7FF,
            bg: 0x0007_2634,
            cursor: 0x0000_F5FF,
            selection: 0x0015_526B,
        },
    ];

    /// Every bundled scheme as its own chrome: the donor set the forced test
    /// carries onto the authored surfaces.
    fn bundled_themes() -> Vec<(&'static str, Theme)> {
        aterm_types::scheme::builtin_names()
            .into_iter()
            .map(|name| {
                (
                    name,
                    Theme::from(
                        &aterm_types::scheme::builtin(name).expect("a listed builtin exists"),
                    ),
                )
            })
            .collect()
    }

    /// The role contract, asserted on the pairs that actually meet on screen,
    /// including accent ink on a selected row's accent wash over `elevated`
    /// (not just accent against `surface`, which is the pair for a rule).
    fn assert_role_contract(what: &str, r: NativeRoles) {
        assert!(
            contrast_ratio(r.text_primary, r.surface) >= 4.5,
            "{what}: primary text"
        );
        assert!(
            contrast_ratio(r.text_secondary, r.surface) >= 4.5,
            "{what}: secondary text"
        );
        assert!(
            contrast_ratio(r.text_tertiary, r.surface) >= 3.0,
            "{what}: tertiary text"
        );
        assert!(
            contrast_ratio(r.accent, r.surface) >= ACCENT_INK_FLOOR,
            "{what}: accent as a rule on the page"
        );
        for (state, mix) in [
            ("selected", ACCENT_WASH_SELECTED),
            ("pressed", ACCENT_WASH_PRESSED),
        ] {
            let wash = mix_srgb(r.elevated, r.accent, mix);
            let ratio = contrast_ratio(r.accent, wash);
            assert!(
                ratio >= ACCENT_INK_FLOOR,
                "{what}: accent ink on the {state} row's own wash is {ratio:.2}:1"
            );
        }
        assert!(
            contrast_ratio(r.on_accent, r.accent) >= 4.5,
            "{what}: ink on an accent fill"
        );
        assert!(contrast_ratio(r.danger, r.surface) >= 4.5, "{what}: danger");
        assert!(
            contrast_ratio(r.success, r.surface) >= 4.5,
            "{what}: success"
        );
        assert_ne!(r.surface, r.elevated, "{what}: elevation is visible");
    }

    #[test]
    fn roles_meet_their_contrast_contract_on_diverse_themes() {
        for theme in THEMES {
            assert_role_contract(&format!("{theme:?}"), default_roles(theme));
        }
        for (name, theme) in bundled_themes() {
            assert_role_contract(name, default_roles(theme));
            // …and over the full chrome palette, with a hostile authored accent
            // and border (the background itself): the floors still hold.
            let scheme = aterm_types::scheme::builtin(name).expect("a built-in");
            let hostile = packed_rgb(theme.bg);
            for authored in [
                aterm_messages::tokens::AuthoredChrome::NONE,
                aterm_messages::tokens::AuthoredChrome {
                    accent: Some(hostile),
                    border: Some(hostile),
                    ..aterm_messages::tokens::AuthoredChrome::NONE
                },
            ] {
                let palette = crate::chrome_band::ChromePalette {
                    theme,
                    ansi: Some(crate::chrome_band::ansi_of_scheme(&scheme)),
                    authored,
                };
                assert_role_contract(name, default_roles(palette));
            }
        }
    }

    /// THE NATIVE ACCENT IS THE TOKEN ACCENT: on every built-in whose chrome
    /// accent has a colour (its cursor, or the ANSI blue or cyan a grey cursor
    /// borrows), the Settings accent wears that hue (the chroma and lightness
    /// clamps and the floors move lightness and chroma, not hue), and an
    /// authored accent's hue likewise. NEGATIVE CONTROL: a monochrome theme
    /// (grey cursor, grey selection, grey ANSI) has no accent to borrow, so its
    /// roles are exactly the ones derived with no palette known.
    #[test]
    fn native_accent_is_the_token_accent() {
        use aterm_messages::tokens::{AccentSource, AuthoredChrome};
        let hue_gap = |a: [u8; 3], b: [u8; 3]| {
            let (x, y) = (rgb_to_oklch(a).h, rgb_to_oklch(b).h);
            let d = (x - y).rem_euclid(TAU);
            d.min(TAU - d).to_degrees()
        };
        let mut checked = 0;
        for (name, theme) in bundled_themes() {
            let scheme = aterm_types::scheme::builtin(name).expect("a built-in");
            for authored in [
                AuthoredChrome::NONE,
                AuthoredChrome {
                    accent: Some([0xff, 0x00, 0xaa]),
                    ..AuthoredChrome::NONE
                },
            ] {
                let palette = crate::chrome_band::ChromePalette {
                    theme,
                    ansi: Some(crate::chrome_band::ansi_of_scheme(&scheme)),
                    authored,
                };
                let tokens = palette.tokens();
                if tokens.accent_source == AccentSource::Neutral
                    || rgb_to_oklch(tokens.accent).c < 0.035
                {
                    continue;
                }
                let accent = default_roles(palette).accent;
                let gap = hue_gap(accent, tokens.accent);
                assert!(
                    gap < 12.0,
                    "{name} {:?}: accent {accent:02x?} is {gap:.1} degrees off {:02x?}",
                    tokens.accent_source,
                    tokens.accent
                );
                checked += 1;
            }
        }
        assert!(checked > 10, "CONTROL: accents to compare ({checked})");
        let mono = Theme {
            fg: 0x00D0_D0D0,
            bg: 0x0010_1010,
            cursor: 0x00C0_C0C0,
            selection: 0x0040_4040,
        };
        let grey = aterm_messages::ink::AnsiHues {
            red: [0x60; 3],
            green: [0x70; 3],
            yellow: [0x80; 3],
            blue: [0x90; 3],
            cyan: [0xa0; 3],
        };
        let palette = crate::chrome_band::ChromePalette {
            theme: mono,
            ansi: Some(grey),
            authored: AuthoredChrome::NONE,
        };
        assert_eq!(palette.tokens().accent_source, AccentSource::Neutral);
        assert_eq!(default_roles(palette), default_roles(mono));
    }

    /// `Auto` passes the terminal theme through; a forced side against its darkness
    /// re-authors the chrome; a forced side that already matches keeps it.
    #[test]
    fn resolve_chrome_palette_forces_the_whole_band_or_follows_the_system() {
        use crate::app_config::WindowTheme;
        let dark_term = Theme {
            bg: 0x0000_0000,
            fg: 0x00ff_ffff,
            cursor: 0x0000_ff00,
            selection: 0x0033_3333,
        };
        let light_term = Theme {
            bg: 0x00ff_ffff,
            fg: 0x0000_0000,
            cursor: 0x0000_00ff,
            selection: 0x00cc_cccc,
        };

        // Auto follows the system on both bands: terminal theme, untouched.
        assert_eq!(
            resolve_chrome_palette(WindowTheme::Auto, dark_term),
            dark_term
        );
        assert_eq!(
            resolve_chrome_palette(WindowTheme::Auto, light_term),
            light_term
        );

        // Forced light over a dark terminal authors a light chrome.
        let forced = resolve_chrome_palette(WindowTheme::Light, dark_term);
        assert_ne!(
            forced, dark_term,
            "forced light must re-author, not pass the dark band through"
        );
        assert!(
            !crate::tab_bar::theme_is_dark(forced.bg),
            "forced light must yield a LIGHT chrome bg (matches the light caption)"
        );
        // It is exactly the authored forced-light palette.
        assert_eq!(forced, forced_chrome_theme(dark_term, false));

        // Forced dark over a light terminal authors a dark chrome.
        let forced_dark = resolve_chrome_palette(WindowTheme::Dark, light_term);
        assert!(crate::tab_bar::theme_is_dark(forced_dark.bg));
        assert_eq!(forced_dark, forced_chrome_theme(light_term, true));

        // A forced value whose darkness already matches keeps the terminal
        // theme (a dark terminal + forced dark).
        assert_eq!(
            resolve_chrome_palette(WindowTheme::Dark, dark_term),
            dark_term
        );
        assert_eq!(
            resolve_chrome_palette(WindowTheme::Light, light_term),
            light_term
        );
    }

    /// The forced chrome palettes go through [`roles`] like every terminal theme, so they
    /// hold the same contract for every carried accent.
    #[test]
    fn forced_chrome_themes_meet_the_roles_contract_for_any_carried_accent() {
        let donors = THEMES
            .into_iter()
            .map(|theme| ("hand-written", theme))
            .chain(bundled_themes());
        for (name, terminal) in donors {
            for dark in [false, true] {
                let forced = forced_chrome_theme(terminal, dark);
                // The authored side really is that side under the shared chrome
                // classifier.
                assert_eq!(surface_is_dark(packed_rgb(forced.bg)), dark);
                assert!(
                    contrast_ratio(packed_rgb(forced.fg), packed_rgb(forced.bg)) >= 7.0,
                    "authored chrome text is comfortably readable on its own surface"
                );
                // The carried accent seed arrives as a real colour on the forced
                // surface (the shipped accent is floored again by `roles`).
                assert!(
                    contrast_ratio(packed_rgb(forced.cursor), packed_rgb(forced.bg))
                        >= FORCED_CHROME_ACCENT_FLOOR - 0.01
                );
                assert_role_contract(
                    &format!("{name} carried onto the forced dark={dark} chrome"),
                    default_roles(forced),
                );
            }
        }
        // The seed constants replace fg/bg; selection is carried verbatim.
        let light = forced_chrome_theme(THEMES[1], false);
        assert_eq!(light.bg, FORCED_LIGHT_CHROME_BG);
        assert_eq!(light.fg, FORCED_LIGHT_CHROME_FG);
        assert_eq!(light.selection, THEMES[1].selection);
    }

    #[test]
    fn high_contrast_strengthens_secondary_and_separators() {
        for theme in THEMES {
            let normal = default_roles(theme);
            let high = roles(
                theme,
                AppearancePreferences {
                    high_contrast: true,
                    text_scale: 1.0,
                    ..AppearancePreferences::default()
                },
            );
            assert!(contrast_ratio(high.text_secondary, high.surface) >= 7.0);
            assert!(contrast_ratio(high.separator, high.surface) >= 3.0);
            assert!(
                contrast_ratio(high.separator, high.surface)
                    >= contrast_ratio(normal.separator, normal.surface)
            );
        }
    }

    #[test]
    fn text_scale_is_sane_and_bounded() {
        assert_eq!(
            AppearancePreferences::default().normalized().text_scale,
            1.0
        );
        assert_eq!(
            AppearancePreferences {
                text_scale: f32::NAN,
                ..AppearancePreferences::default()
            }
            .normalized()
            .text_scale,
            1.0
        );
        assert_eq!(
            AppearancePreferences {
                text_scale: 9.0,
                ..AppearancePreferences::default()
            }
            .normalized()
            .text_scale,
            2.0
        );
    }
}
