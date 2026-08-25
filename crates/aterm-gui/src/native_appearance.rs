// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Perceptual appearance system shared by every native tab app and chrome surface.
//!
//! The terminal theme remains the source of identity, but native UI roles are not
//! painted by copying raw cursor/selection bytes.  They are rebuilt in OKLCH,
//! gamut-mapped, and contrast-conditioned for the surface they actually sit on.
//! This keeps vivid terminal schemes expressive without turning controls into neon
//! blocks, and keeps quiet/light schemes from washing the hierarchy away.

#[cfg(test)]
use std::cell::Cell;
use std::f32::consts::TAU;
#[cfg(not(test))]
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use aterm_render::Theme;

/// Accessibility inputs which affect role contrast.  The host can replace the
/// defaults when a platform appearance observer reports a change.
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

/// Sticky "an OS-owned chrome input MOVED and no re-sample has settled the windows
/// for it yet". Set by [`install_preferences`] and by
/// [`crate::chrome_band::install_forced_chrome`]; drained by exactly one consumer,
/// `App::resample_os_preferences`, through [`take_chrome_inputs_moved`].
///
/// WHY A LATCH AND NOT THE `install_*` RETURN VALUE. Those returns are EDGES, and an
/// edge belongs to whoever reads it first. `AppRt::native_appearance_preferences` is
/// also called at window attach, at startup and by the settings preview, and on
/// Windows that read republishes the High-Contrast palette as a documented side
/// effect. So an HC-scheme switch landing between a window attach and the settings
/// `Wake` had its movement consumed by the attach: the `Wake`'s re-sample then saw
/// "nothing moved", skipped the `last_strip_fp` invalidation, and left every
/// PRE-EXISTING window painting the old OS palette until the next tab open/close.
/// Latching makes the answer independent of who happened to read first.
#[cfg(not(test))]
static CHROME_INPUTS_MOVED: AtomicBool = AtomicBool::new(false);

// Same test/production split, and the same rationale, as the snapshot below: libtest
// runs independent Apps on a reused worker pool, so a process-global latch would let
// one worker drain another worker's pending settle.
#[cfg(test)]
thread_local! {
    static TEST_CHROME_INPUTS_MOVED: Cell<bool> = const { Cell::new(false) };
}

/// Record that an OS-owned chrome input moved. Idempotent; see
/// [`CHROME_INPUTS_MOVED`].
pub(crate) fn note_chrome_inputs_moved() {
    #[cfg(test)]
    TEST_CHROME_INPUTS_MOVED.with(|slot| slot.set(true));
    #[cfg(not(test))]
    CHROME_INPUTS_MOVED.store(true, Ordering::Release);
}

/// Consume the latch: `true` when an OS-owned chrome input has moved since the last
/// re-sample settled the windows. THE one caller is `App::resample_os_preferences`.
#[must_use]
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

// Production has one platform appearance observer and therefore one process-wide
// snapshot. Libtest deliberately runs independent Apps on a reused worker pool;
// a process-global test snapshot lets one worker change another worker's layout
// halfway through compile/raster. Preserve the production API and semantics while
// isolating only those independent test hosts by worker thread.
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
/// paint/layout inputs changed, allowing the host to avoid redundant redraws.
///
/// A `true` also arms [`CHROME_INPUTS_MOVED`], so the re-sample that settles the
/// windows still learns about the move even when this edge was consumed by an
/// unrelated caller (window attach, startup, the settings preview).
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

/// Complete semantic palette for a native surface.  No application may invent a
/// component colour outside this vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeRoles {
    pub(crate) surface: [u8; 3],
    pub(crate) text_primary: [u8; 3],
    pub(crate) text_secondary: [u8; 3],
    pub(crate) text_tertiary: [u8; 3],
    pub(crate) accent: [u8; 3],
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

/// Is this chrome background the DARK side?  THE ONE dark/light classifier for
/// chrome — [`crate::tab_bar::bg_is_light`], the same predicate the tab strip's
/// tones and the CSD variant resolve with.
///
/// It is shared, not merely equivalent, because the strip and this role pipeline
/// answer ONE question: which side is this window's chrome on. This pipeline used
/// to answer it with a WCAG relative-luminance split (`< 0.42`) of its own, and
/// the two thresholds disagree across a real band — neutral `#979797`..`#ADADAD`
/// and their chromatic equivalents, where gamma-encoded Rec.601 luma has already
/// crossed 150 while linear luminance has not. That disagreement is exactly how
/// config `window_theme = light` stayed a NO-OP for those themes:
/// [`crate::App::chrome_palette_theme`] classified the terminal theme as "already
/// light", passed it through unforced, and this pipeline then built the DARK side
/// from the very same background.
pub(crate) fn surface_is_dark(bg: [u8; 3]) -> bool {
    !crate::tab_bar::bg_is_light(bg)
}

/// The accent tint an interactive fill carries WHEN THE ACCENT ITSELF IS THE INK
/// on top of it: a selected navigation row paints its label and icon in
/// `accent` on `elevated` washed this far toward `accent`, and pressing that same
/// selected row deepens the wash to [`ACCENT_WASH_PRESSED`].  Owned here rather
/// than in the painter so [`accent_over_its_own_wash`] and the pixels
/// `native_ui` produces cannot drift apart.
pub(crate) const ACCENT_WASH_SELECTED: f32 = 0.13;
/// The deepest accent wash that still carries ACCENT ink (the pressed state of a
/// selected navigation row).  Deeper washes than this exist, but the ink on them
/// is text_primary/secondary, floored against `surface` already.
pub(crate) const ACCENT_WASH_PRESSED: f32 = 0.16;
/// The floor the accent holds AS INK, against the wash it is inked on.  A rule or
/// a fill can be quieter than its surroundings; a LABEL cannot.
const ACCENT_INK_FLOOR: f32 = 3.0;

/// Straight sRGB channel lerp — the mix the native painter builds control fills
/// with ([`crate::native_ui::mix_rgb`] delegates here), as distinct from the
/// perceptual [`mix_oklab`] the role ramps use.  The accent floor below has to
/// measure the EXACT bytes the painter will put on screen.
pub(crate) fn mix_srgb(a: [u8; 3], b: [u8; 3], amount: f32) -> [u8; 3] {
    let amount = amount.clamp(0.0, 1.0);
    std::array::from_fn(|index| {
        (f32::from(a[index]) + (f32::from(b[index]) - f32::from(a[index])) * amount).round() as u8
    })
}

/// Hold the accent to [`ACCENT_INK_FLOOR`] against its OWN wash.
///
/// The selected rail row is the one place the accent is body-sized ink, and its
/// backdrop is built FROM the accent — so flooring the accent against `surface`
/// (which the wash is not) left the SELECTED row the least legible row in the
/// rail: 2.69:1 on the forced-light chrome against 9.63:1 for its unselected
/// siblings, which reads as "disabled", the exact opposite of selected.
///
/// The target moves with the answer, so this is a fixpoint: each pass re-mixes
/// the wash from the current accent and nudges lightness only as far as that wash
/// demands ([`ensure_contrast`] is hue- and chroma-preserving). The wash travels
/// only [`ACCENT_WASH_PRESSED`] of the way toward the accent, so the residual
/// contracts by roughly that factor per pass and the loop settles in two or three;
/// the bound is a guarantee of termination, not a budget.
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

/// A status hue as [`roles`] seeds it, BEFORE the ink floor: one shared semantic
/// angle, at a lightness this chrome side chooses and a chroma the accessibility
/// preferences choose.
fn status_seed(dark: bool, high_contrast: bool, hue_degrees: f32) -> [u8; 3] {
    oklch_to_rgb(Oklch {
        l: if dark { 0.72 } else { 0.49 },
        c: if high_contrast { 0.16 } else { 0.14 },
        h: hue_degrees.to_radians(),
    })
}

/// The `success` family as DECORATION rather than as ink — the same seed
/// [`roles`] conditions into [`NativeRoles::success`], WITHOUT the 4.5:1 floor.
///
/// [`NativeRoles::success`] is an INK role: it labels, it rules, it draws marks,
/// so [`roles`] holds it to 4.5:1 against `surface` and that is right. But
/// [`ensure_contrast`] reaches a target by moving LIGHTNESS, and against a
/// MID-TONE surface — the neutral `#8F8F8F`..`#A3A3A3` band a real terminal
/// theme lands on — 4.5:1 is simply unreachable upward, so the only route left
/// is down and the role arrives as a NEAR-BLACK green (`#000901` on `#777777`).
/// Correct for a label. Useless as a tint: a surface washed toward near-black is
/// not a shade of green, it is a shade of grey, and pouring 14 % of it into the
/// Settings hero banner turned the banner into a slab DARKER than the cards
/// standing on it.
///
/// So decoration takes the hue and leaves the floor behind. Nothing here paints
/// text or a UI mark — a caller that wants either must use the role, and hold
/// its own result to its own surface.
///
/// `dark` is the side of the chrome the DECORATION lands on, which is the
/// caller's own question and not necessarily [`roles`]' answer for the window:
/// in the mid-tone band one step of the surface ramp can cross the classifier.
/// Chroma follows the live accessibility preferences, the same ones
/// [`default_roles`] resolves a palette with.
pub(crate) fn success_tint(dark: bool) -> [u8; 3] {
    status_seed(
        dark,
        current_preferences().normalized().high_contrast,
        SUCCESS_HUE_DEGREES,
    )
}

/// Hold a surface at or above the OPENNESS of the surface it has to stay above,
/// moving lightness only — the same hue- and chroma-preserving nudge
/// [`ensure_contrast`] makes for ink, aimed at a floor that is a LUMINANCE
/// rather than a ratio.
///
/// Contrast against black is `(L + 0.05) / 0.05`, monotone in relative
/// luminance, so "as open as `floor`" IS a contrast target and the existing
/// solver reaches it: the dark pole can never satisfy it, so the search returns
/// the SMALLEST lift that clears the floor, or the colour untouched when it
/// already does.
///
/// This exists because a decorative wash moves lightness whether or not it was
/// asked to. [`crate::native_ui::rainbow_banner_sky`] pours [`success_tint`]
/// into the palette's most open neutral, and when that neutral is `elevated` the
/// base IS the tone the cards standing on the banner are painted in — so the sky
/// starts with no headroom whatsoever. Any wash toward a tint darker than the
/// base sinks the banner below its own cards, and even a LIFTING pour can come
/// back a hair under, because a chromatic mix is not monotone in luminance: near
/// the crossover the green's low red and blue cost more than its green gains.
/// The wash is wanted for its HUE; the lightness it happens to spend is not part
/// of the ask, and this gives it back.
pub(crate) fn at_least_as_open_as(color: [u8; 3], floor: [u8; 3]) -> [u8; 3] {
    ensure_contrast(color, [0, 0, 0], contrast_ratio(floor, [0, 0, 0]))
}

/// Build the native palette.  Neutral roles preserve the theme's perceptual hue;
/// chromatic roles keep the theme accent hue but cap its chroma for large controls.
pub(crate) fn roles(theme: Theme, preferences: AppearancePreferences) -> NativeRoles {
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
    // Floored twice, against both backdrops the accent actually meets: the page
    // surface it rules and fills on, and the selected-row wash it INKS on.
    let accent = ensure_contrast(oklch_to_rgb(accent_seed), surface, ACCENT_INK_FLOOR);
    let accent = accent_over_its_own_wash(accent, elevated);

    let black = [8, 10, 13];
    let white = [250, 251, 253];
    let on_seed = if contrast_ratio(black, accent) >= contrast_ratio(white, accent) {
        black
    } else {
        white
    };
    let on_accent = ensure_contrast(on_seed, accent, 4.5);

    let separator_target = if preferences.high_contrast { 3.0 } else { 1.5 };
    let separator = ensure_contrast(mix_oklab(fg, surface, 0.70), surface, separator_target);
    let control_track = ensure_contrast(mix_oklab(fg, surface, 0.57), surface, 2.0);

    // Status hues use a shared semantic angle, while lightness/chroma and final
    // contrast remain conditioned by this theme.  They are not fixed RGB tokens.
    let danger = ensure_contrast(
        status_seed(dark, preferences.high_contrast, DANGER_HUE_DEGREES),
        surface,
        4.5,
    );
    let success = ensure_contrast(
        status_seed(dark, preferences.high_contrast, SUCCESS_HUE_DEGREES),
        surface,
        4.5,
    );

    NativeRoles {
        surface,
        text_primary,
        text_secondary,
        text_tertiary,
        accent,
        on_accent,
        separator,
        control_track,
        elevated,
        danger,
        success,
    }
}

pub(crate) fn default_roles(theme: Theme) -> NativeRoles {
    roles(theme, current_preferences())
}

/// The authored FORCED-chrome seed palette for config `window_theme = light` —
/// the light counterpart the settings audit found missing: before it existed the
/// native painter had only theme-derived tokens, so `light` under a dark
/// terminal theme changed nothing on Linux. libadwaita light is the reference
/// (`window_bg_color #fafafa`, text `rgba(0,0,0,0.8)`); [`roles`] then derives
/// the full token set from these seeds through the same OKLCH mix + contrast
/// conditioning every terminal theme goes through, so the forced pages get the
/// identical surface/ink discipline instead of a second hand-held ramp.
#[cfg(any(test, target_os = "linux", windows))] // pixel-band forced-palette seam (chrome_palette_theme, Linux+Windows) + tests; dead on macOS lib
pub(crate) const FORCED_LIGHT_CHROME_BG: u32 = 0x00FA_FAFA;
/// Dark text for the forced-light chrome (the libadwaita 80 %-black composite).
#[cfg(any(test, target_os = "linux", windows))] // pixel-band forced-palette seam (chrome_palette_theme, Linux+Windows) + tests; dead on macOS lib
pub(crate) const FORCED_LIGHT_CHROME_FG: u32 = 0x002E_3436;
/// The authored FORCED-chrome seed palette for config `window_theme = dark`
/// (libadwaita dark: `window_bg_color #242424`, near-white text).
#[cfg(any(test, target_os = "linux", windows))] // pixel-band forced-palette seam (chrome_palette_theme, Linux+Windows) + tests; dead on macOS lib
pub(crate) const FORCED_DARK_CHROME_BG: u32 = 0x0024_2424;
/// Near-white text for the forced-dark chrome.
#[cfg(any(test, target_os = "linux", windows))] // pixel-band forced-palette seam (chrome_palette_theme, Linux+Windows) + tests; dead on macOS lib
pub(crate) const FORCED_DARK_CHROME_FG: u32 = 0x00ED_EDED;

/// The floor the carried terminal cursor — the accent SEED — is held to against
/// the forced chrome surface.
///
/// This is a floor on the INPUT, not on the shipped accent. The accent is body
/// ink: a selected navigation row paints its label and icon in it, so the floor
/// that decides legibility is [`ACCENT_INK_FLOOR`], applied by [`roles`] against
/// both the surface and the row's own wash ([`accent_over_its_own_wash`]) — and
/// [`roles`] re-derives lightness and chroma from clamps of its own, so nothing
/// here can substitute for that. What this catches is a degenerate SEED: a dark
/// theme's near-white cursor carried verbatim onto `#FAFAFA` arrives with no
/// usable lightness signal at all, and the hue it hands to [`roles`] is whatever
/// rounding left in a near-neutral. Holding the seed to the same 3.0 the shipped
/// ink must reach keeps the identity that gets carried a real colour.
#[cfg(any(test, target_os = "linux", windows))] // pixel-band forced-palette seam (chrome_palette_theme, Linux+Windows) + tests; dead on macOS lib
const FORCED_CHROME_ACCENT_FLOOR: f32 = ACCENT_INK_FLOOR;

/// The [`Theme`] the chrome painters draw from when config
/// `window_theme = light|dark` forces the side AGAINST the terminal theme's own
/// darkness (the Linux resolution seam, [`crate::App::chrome_palette_theme`]).
/// Surfaces and text are the authored constants above; the ACCENT identity
/// (cursor, and selection as [`roles`]' fallback accent seed) is carried from
/// the terminal theme — "same accent" per the libadwaita reference — with the
/// cursor lightness-nudged ([`ensure_contrast`], hue-preserving) so it stays
/// visible on the forced surface.
#[cfg(any(test, target_os = "linux", windows))] // pixel-band forced-palette seam (chrome_palette_theme, Linux+Windows) + tests; dead on macOS lib
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

/// The pixel-band chrome palette for a `window_theme` policy over a `terminal`
/// theme — the pure core `App::chrome_palette_theme` runs on Linux AND Windows
/// (the `STRIP_IS_CHROME_BAND` platforms), extracted so the resolution is
/// testable on any host (the integration arm is `cfg`-gated per platform, but
/// the LOGIC is one function). `Auto` follows the system (returns the terminal
/// theme unchanged — the caption/CSD painter tracks the OS); a forced value
/// whose darkness already matches keeps the terminal theme (no needless
/// re-authoring); a forced value AGAINST the terminal darkness authors the
/// forced-variant chrome so the band + native pages flip as one with the OS
/// caption (owner decision 2026-08-25: Windows matches Linux).
#[cfg(any(test, target_os = "linux", windows))] // pixel-band forced-palette seam (chrome_palette_theme, Linux+Windows) + tests; dead on macOS lib
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
#[cfg(any(test, target_os = "linux", windows))] // pixel-band forced-palette seam (chrome_palette_theme, Linux+Windows) + tests; dead on macOS lib
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

/// Convert with chroma-preserving gamut mapping.  Lightness and hue remain stable;
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

    /// A preference move that some OTHER caller consumed must still reach the
    /// re-sample that settles the windows.
    ///
    /// `install_preferences` is called at window attach, at startup and by the
    /// settings preview as well as from `App::resample_os_preferences`, and its
    /// `bool` is an EDGE — the first caller to see it takes it. So an accessibility
    /// flip landing between a window attach and the settings `Wake` used to be
    /// consumed by the attach, and the re-sample would then conclude that nothing had
    /// moved, skip the strip-cache invalidation and the chrome re-resolve, and leave
    /// every pre-existing window painting the stale appearance.
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

    /// Every bundled scheme, as its own chrome — the donor set the forced test
    /// below carries onto the authored surfaces.
    fn bundled_themes() -> Vec<(&'static str, Theme)> {
        aterm_types::scheme::builtin_names()
            .into_iter()
            .map(|name| {
                let parts = aterm_types::scheme::builtin(name)
                    .expect("a listed builtin exists")
                    .to_theme_parts();
                (
                    name,
                    Theme {
                        fg: parts.fg,
                        bg: parts.bg,
                        cursor: parts.cursor,
                        selection: parts.selection,
                    },
                )
            })
            .collect()
    }

    /// THE contract, asserted on the pairs that actually meet on screen.
    ///
    /// The accent one is the pair this used to get wrong: `accent`-vs-`surface`
    /// is the pair for a RULE, but a SELECTED navigation row paints its label and
    /// icon in `accent` on a wash built FROM `accent` over `elevated`, and that
    /// pair measured 2.69:1 on the forced-light chrome while the row's own
    /// unselected siblings sat at 9.63:1 — the selected row was the least legible
    /// row in the rail, which reads as "disabled".
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
        }
    }

    /// ONE dark/light predicate decides which side the chrome is on. The strip's
    /// band tones, the CSD variant and this role pipeline all read
    /// [`crate::tab_bar::bg_is_light`]; the WCAG relative-luminance split this
    /// module used to keep for itself disagreed with it across a REAL band of
    /// backgrounds, and that disagreement is the whole mechanism behind
    /// `window_theme = light` doing nothing for those themes (the passthrough in
    /// `chrome_palette_theme` saw "already light" and handed back a theme this
    /// pipeline then built the DARK side from).
    #[test]
    fn the_role_pipeline_and_the_strip_classify_every_background_the_same_way() {
        // The band is real, not hypothetical: the retired split still calls
        // these DARK, while the shipping predicate — and therefore the strip,
        // the header and the pages — call them LIGHT.
        for gray in 0x97..=0xAD_u8 {
            let bg = [gray; 3];
            assert!(
                relative_luminance(bg) < 0.42,
                "#{gray:02X}{gray:02X}{gray:02X} is inside the retired split's dark side"
            );
            assert!(
                !surface_is_dark(bg),
                "#{gray:02X}{gray:02X}{gray:02X} is on the strip's light side"
            );
        }
        // …and no background anywhere can split the two sites apart.
        for r in (0..=255).step_by(15) {
            for g in (0..=255).step_by(15) {
                for b in (0..=255).step_by(15) {
                    let bg = [r as u8, g as u8, b as u8];
                    let packed =
                        (u32::from(bg[0]) << 16) | (u32::from(bg[1]) << 8) | u32::from(bg[2]);
                    assert_eq!(
                        surface_is_dark(bg),
                        crate::tab_bar::theme_is_dark(packed),
                        "{bg:?}: the role pipeline and the strip must be ONE predicate"
                    );
                }
            }
        }
    }

    /// The pixel-band palette resolver both Linux and Windows run (owner
    /// decision 2026-08-25: Windows matches Linux). `Auto` follows the system
    /// (terminal theme untouched); a forced value AGAINST the terminal
    /// darkness authors the forced-variant chrome so the band + native pages
    /// flip as ONE with the OS caption — killing the "light titlebar bolted
    /// onto a dark chrome body" half-flip Windows used to show. A forced value
    /// whose darkness already matches keeps the terminal theme (no re-author).
    /// Runs on every host: the integration arm is cfg-gated per platform, but
    /// this is the logic itself.
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

        // `Theme` has no PartialEq; compare the four fields.
        let same = |a: Theme, b: Theme| {
            a.fg == b.fg && a.bg == b.bg && a.cursor == b.cursor && a.selection == b.selection
        };

        // Auto follows the system on both bands: terminal theme, untouched.
        assert!(same(resolve_chrome_palette(WindowTheme::Auto, dark_term), dark_term));
        assert!(same(resolve_chrome_palette(WindowTheme::Auto, light_term), light_term));

        // Forced LIGHT over a DARK terminal authors a LIGHT chrome — the band
        // no longer keeps the dark terminal palette under a light caption.
        let forced = resolve_chrome_palette(WindowTheme::Light, dark_term);
        assert!(!same(forced, dark_term), "forced light must re-author, not pass the dark band through");
        assert!(
            !crate::tab_bar::theme_is_dark(forced.bg),
            "forced light must yield a LIGHT chrome bg (matches the light caption)"
        );
        // It is exactly the authored forced-light palette.
        assert!(same(forced, forced_chrome_theme(dark_term, false)));

        // Forced DARK over a LIGHT terminal authors a DARK chrome, symmetrically.
        let forced_dark = resolve_chrome_palette(WindowTheme::Dark, light_term);
        assert!(crate::tab_bar::theme_is_dark(forced_dark.bg));
        assert!(same(forced_dark, forced_chrome_theme(light_term, true)));

        // A forced value whose darkness ALREADY matches keeps the terminal
        // theme — no needless re-authoring (a dark terminal + forced dark).
        assert!(same(resolve_chrome_palette(WindowTheme::Dark, dark_term), dark_term));
        assert!(same(resolve_chrome_palette(WindowTheme::Light, light_term), light_term));
    }

    /// The FORCED chrome palettes (`window_theme = light|dark` against the
    /// terminal theme's own darkness) go through the same [`roles`] pipeline
    /// as every terminal theme, so they must hold the identical contract — for
    /// EVERY carried accent, since the terminal theme donates cursor/selection.
    #[test]
    fn forced_chrome_themes_meet_the_roles_contract_for_any_carried_accent() {
        let donors = THEMES
            .into_iter()
            .map(|theme| ("hand-written", theme))
            .chain(bundled_themes());
        for (name, terminal) in donors {
            for dark in [false, true] {
                let forced = forced_chrome_theme(terminal, dark);
                // The authored side really is that side, under THE chrome
                // classifier — the one the roles pipeline, the strip and the CSD
                // variant share.
                assert_eq!(surface_is_dark(packed_rgb(forced.bg)), dark);
                assert!(
                    contrast_ratio(packed_rgb(forced.fg), packed_rgb(forced.bg)) >= 7.0,
                    "authored chrome text is comfortably readable on its own surface"
                );
                // The carried accent SEED arrives as a real colour on the forced
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
        // A terminal theme already on the forced side round-trips its accent
        // identity: the seed constants replace fg/bg but selection is carried
        // verbatim.
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
    fn gamut_mapping_is_total_and_preserves_neutral_endpoints() {
        let mut mapped = std::collections::BTreeSet::new();
        for l in [0.0, 0.25, 0.5, 0.75, 1.0] {
            for h in [0.0, 1.0, 2.0, 3.0, 4.0, 5.0] {
                mapped.insert(oklch_to_rgb(Oklch { l, c: 0.8, h }));
            }
        }
        assert!(
            mapped.len() > 12,
            "gamut mapping preserves chromatic variety"
        );
        assert_eq!(
            oklch_to_rgb(Oklch {
                l: 0.0,
                c: 0.0,
                h: 0.0
            }),
            [0; 3]
        );
        assert_eq!(
            oklch_to_rgb(Oklch {
                l: 1.0,
                c: 0.0,
                h: 0.0
            }),
            [255; 3]
        );
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

    #[test]
    fn libtest_appearance_hosts_are_isolated_by_worker_thread() {
        let original = current_preferences();
        let local = AppearancePreferences {
            high_contrast: true,
            reduced_transparency: true,
            text_scale: 1.5,
        };
        assert!(install_preferences(local));

        let child = std::thread::spawn(|| {
            assert_eq!(
                current_preferences(),
                AppearancePreferences::default(),
                "a new libtest host starts from the platform default"
            );
            let child = AppearancePreferences {
                text_scale: 2.0,
                ..AppearancePreferences::default()
            };
            assert!(install_preferences(child));
            current_preferences()
        })
        .join()
        .expect("isolated appearance host exits cleanly");

        assert_eq!(child.text_scale, 2.0);
        assert_eq!(current_preferences(), local);
        let _ = install_preferences(original);
    }
}
