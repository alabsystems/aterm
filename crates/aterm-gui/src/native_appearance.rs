// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Perceptual appearance system shared by every native tab app and chrome surface.

#[cfg(test)]
use std::cell::Cell;
use std::f32::consts::TAU;
#[cfg(all(not(test), any(target_os = "macos", windows)))]
use std::sync::atomic::AtomicBool;
#[cfg(not(test))]
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use aterm_render::Theme;

/// Accessibility inputs that affect role contrast.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AppearancePreferences {
    pub(crate) high_contrast: bool,
    pub(crate) reduced_transparency: bool,
    /// System Settings ▸ Accessibility ▸ Display ▸ Differentiate without colour.
    pub(crate) differentiate_without_color: bool,
    /// Native-app text scale, independent of terminal-cell zoom.
    pub(crate) text_scale: f32,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            high_contrast: false,
            reduced_transparency: false,
            differentiate_without_color: false,
            text_scale: 1.0,
        }
    }
}

#[cfg(not(test))]
const HIGH_CONTRAST: u8 = 1 << 0;
#[cfg(not(test))]
const REDUCED_TRANSPARENCY: u8 = 1 << 1;
#[cfg(not(test))]
const DIFFERENTIATE_WITHOUT_COLOR: u8 = 1 << 2;
#[cfg(not(test))]
static ACCESSIBILITY_FLAGS: AtomicU8 = AtomicU8::new(0);
#[cfg(not(test))]
static TEXT_SCALE_BITS: AtomicU32 = AtomicU32::new(1.0_f32.to_bits());

/// A TEXT SCALE INJECTED IN PLACE OF THE OPERATING SYSTEM'S.
#[cfg(not(test))]
static INJECTED_TEXT_SCALE_BITS: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
thread_local! {
    static TEST_INJECTED_TEXT_SCALE: Cell<Option<f32>> = const { Cell::new(None) };
}

/// Inject `scale` in place of the operating system's text scale, or clear the injection (`None`).
pub(crate) fn inject_text_scale(scale: Option<f32>) {
    #[cfg(test)]
    TEST_INJECTED_TEXT_SCALE.with(|slot| slot.set(scale));
    #[cfg(not(test))]
    INJECTED_TEXT_SCALE_BITS.store(scale.map_or(0, f32::to_bits), Ordering::Release);
}

/// The text scale a development build injected ([`inject_text_scale`]).
#[must_use]
pub(crate) fn injected_text_scale() -> Option<f32> {
    #[cfg(test)]
    {
        TEST_INJECTED_TEXT_SCALE.with(Cell::get)
    }
    #[cfg(not(test))]
    {
        let bits = INJECTED_TEXT_SCALE_BITS.load(Ordering::Acquire);
        (bits != 0).then(|| f32::from_bits(bits))
    }
}

fn parse_text_scale(word: &str) -> Option<Option<f32>> {
    if word.eq_ignore_ascii_case("os") {
        return Some(None);
    }
    if word.starts_with('+') {
        return None;
    }
    let scale = word.parse::<f32>().ok().filter(|scale| scale.is_finite())?;
    (0.85..=2.0).contains(&scale).then_some(Some(scale))
}

fn text_scale_request(rest: &str, injection: bool) -> Result<Option<Option<f32>>, &'static str> {
    const USAGE: &str = "usage: text-scale [0.85..2.0|os]";
    let mut words = rest.split_whitespace();
    let set = match (words.next(), words.next()) {
        (None, _) => None,
        (Some(word), None) => Some(parse_text_scale(word).ok_or(USAGE)?),
        (Some(_), Some(_)) => return Err(USAGE),
    };
    if set.is_some() && !injection {
        return Err("text-scale <scale>|os needs a development build (dev-seams)");
    }
    Ok(set)
}

/// The `text-scale` control verb.
pub(crate) fn cmd_text_scale(
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    rest: &str,
) -> String {
    let set = match text_scale_request(rest, cfg!(any(debug_assertions, feature = "dev-seams"))) {
        Ok(set) => set,
        Err(refusal) => return format!("ERR {refusal}\n"),
    };
    match crate::control::control_media::call_main(proxy, |reply| crate::Wake::ControlTextScale {
        set,
        reply,
    }) {
        Ok(line) => format!("OK {line}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// The `text-scale` verb's answer line.
#[must_use]
pub(crate) fn text_scale_status_line() -> String {
    format!(
        "text_scale={} source={}",
        text_scale(),
        if injected_text_scale().is_some() {
            "injected"
        } else {
            "os"
        }
    )
}

/// The display accessibility settings `$ATERM_DEV_ACCESSIBILITY` turns on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DevAccessibility {
    pub(crate) high_contrast: bool,
    pub(crate) reduced_transparency: bool,
    pub(crate) differentiate_without_color: bool,
    pub(crate) reduce_motion: bool,
}

/// The `$ATERM_DEV_ACCESSIBILITY` launch seam ([`aterm_types::dev_seam!`]), read once: a comma
/// list of `high-contrast`, `reduce-transparency`, `differentiate-without-color` and
/// `reduce-motion`, each read as on over what the operating system says (and in a headless
/// instance, which asks it nothing), so a lab instance shows them without a change to the
/// person's own settings. A release build reads no seam: every setting stays the operating
/// system's.
pub(crate) fn dev_accessibility() -> DevAccessibility {
    static SEAM: std::sync::OnceLock<DevAccessibility> = std::sync::OnceLock::new();
    *SEAM.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_DEV_ACCESSIBILITY")
            .and_then(|value| value.to_str().map(parse_dev_accessibility))
            .unwrap_or_default()
    })
}

/// [`dev_accessibility`]'s list; a word it does not name is logged and turns nothing on.
fn parse_dev_accessibility(list: &str) -> DevAccessibility {
    let mut on = DevAccessibility::default();
    for word in list.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        match word {
            "high-contrast" => on.high_contrast = true,
            "reduce-transparency" => on.reduced_transparency = true,
            "differentiate-without-color" => on.differentiate_without_color = true,
            "reduce-motion" => on.reduce_motion = true,
            other => aterm_log::warn!("ATERM_DEV_ACCESSIBILITY names no setting {other:?}"),
        }
    }
    on
}

/// The `$ATERM_TEXT_SCALE` launch seam ([`aterm_types::dev_seam!`]).
pub(crate) fn inject_launch_text_scale() {
    if let Some(Some(scale)) = aterm_types::dev_seam!("ATERM_TEXT_SCALE")
        .and_then(|value| value.to_str().and_then(parse_text_scale))
    {
        inject_text_scale(Some(scale));
    }
}

/// Sticky flag: an OS-owned chrome input moved and no re-sample has settled the windows for it yet.
#[cfg(all(not(test), any(target_os = "macos", windows)))]
static CHROME_INPUTS_MOVED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
thread_local! {
    static TEST_CHROME_INPUTS_MOVED: Cell<bool> = const { Cell::new(false) };
}

/// Record that an OS-owned chrome input moved.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn note_chrome_inputs_moved() {
    #[cfg(test)]
    TEST_CHROME_INPUTS_MOVED.with(|slot| slot.set(true));
    #[cfg(not(test))]
    CHROME_INPUTS_MOVED.store(true, Ordering::Release);
}

/// Consume the latch.
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

#[cfg(test)]
thread_local! {
    static TEST_APPEARANCE: Cell<AppearancePreferences> = const {
        Cell::new(AppearancePreferences {
            high_contrast: false,
            reduced_transparency: false,
            differentiate_without_color: false,
            text_scale: 1.0,
        })
    };
}

/// Install the platform-observed appearance snapshot.
pub(crate) fn install_preferences(mut preferences: AppearancePreferences) -> bool {
    // A development build's injected scale stands over every reading.
    if let Some(scale) = injected_text_scale() {
        preferences.text_scale = scale;
    }
    // So does a development build's accessibility seam, for each setting it turns on.
    let dev = dev_accessibility();
    preferences.high_contrast |= dev.high_contrast;
    preferences.reduced_transparency |= dev.reduced_transparency;
    preferences.differentiate_without_color |= dev.differentiate_without_color;
    let preferences = preferences.normalized();
    let moved = {
        #[cfg(test)]
        {
            TEST_APPEARANCE.with(|snapshot| snapshot.replace(preferences) != preferences)
        }

        #[cfg(not(test))]
        {
            let flags = (u8::from(preferences.high_contrast) * HIGH_CONTRAST)
                | (u8::from(preferences.reduced_transparency) * REDUCED_TRANSPARENCY)
                | (u8::from(preferences.differentiate_without_color) * DIFFERENTIATE_WITHOUT_COLOR);
            let old_flags = ACCESSIBILITY_FLAGS.swap(flags, Ordering::AcqRel);
            let old_scale =
                TEXT_SCALE_BITS.swap(preferences.text_scale.to_bits(), Ordering::AcqRel);
            old_flags != flags || old_scale != preferences.text_scale.to_bits()
        }
    };
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
            differentiate_without_color: flags & DIFFERENTIATE_WITHOUT_COLOR != 0,
            text_scale: f32::from_bits(TEXT_SCALE_BITS.load(Ordering::Acquire)),
        }
        .normalized()
    }
}

#[must_use]
pub(crate) fn text_scale() -> f32 {
    current_preferences().text_scale
}

/// The operating system's accent colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SystemAccent {
    /// The shade for marks and fills on a DARK surface (WinUI's `SystemAccentColorLight2`).
    pub(crate) on_dark: [u8; 3],
    /// The shade for a LIGHT surface (`SystemAccentColorDark1`).
    pub(crate) on_light: [u8; 3],
}

impl SystemAccent {
    /// The shade for a surface whose background is `bg`.
    #[must_use]
    pub(crate) fn for_surface(self, bg: [u8; 3]) -> [u8; 3] {
        if aterm_messages::ink::bg_is_light(bg) {
            self.on_light
        } else {
            self.on_dark
        }
    }

    #[cfg(any(windows, target_os = "macos", test))]
    fn pack(self) -> u64 {
        let rgb = |c: [u8; 3]| (u64::from(c[0]) << 16) | (u64::from(c[1]) << 8) | u64::from(c[2]);
        (rgb(self.on_dark) << 24) | rgb(self.on_light)
    }

    fn unpack(v: u64) -> Self {
        let rgb = |v: u64| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        Self {
            on_dark: rgb(v >> 24),
            on_light: rgb(v & 0x00FF_FFFF),
        }
    }
}

/// [`SYSTEM_ACCENT`]'s "the OS gives no accent" value (48 bits hold a pair).
const NO_SYSTEM_ACCENT: u64 = u64::MAX;

/// The installed [`SystemAccent`], packed; process-wide like the preferences.
#[cfg(not(test))]
static SYSTEM_ACCENT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(NO_SYSTEM_ACCENT);

#[cfg(test)]
thread_local! {
    static TEST_SYSTEM_ACCENT: Cell<Option<SystemAccent>> = const { Cell::new(None) };
}

/// Install the OS accent the platform read (`None`: it gives none).
#[cfg(any(windows, test))]
pub(crate) fn install_system_accent(accent: Option<SystemAccent>) -> bool {
    let moved = {
        #[cfg(test)]
        {
            TEST_SYSTEM_ACCENT.with(|slot| slot.replace(accent) != accent)
        }

        #[cfg(not(test))]
        {
            let packed = accent.map_or(NO_SYSTEM_ACCENT, SystemAccent::pack);
            SYSTEM_ACCENT.swap(packed, Ordering::AcqRel) != packed
        }
    };
    if moved {
        note_chrome_inputs_moved();
    }
    moved
}

/// The installed OS accent, if the platform follows one.
#[must_use]
pub(crate) fn system_accent() -> Option<SystemAccent> {
    #[cfg(test)]
    {
        TEST_SYSTEM_ACCENT.with(Cell::get)
    }

    #[cfg(not(test))]
    {
        match SYSTEM_ACCENT.load(Ordering::Acquire) {
            NO_SYSTEM_ACCENT => None,
            packed => Some(SystemAccent::unpack(packed)),
        }
    }
}

/// macOS's system blue, its control accent where the person picked none ("Multicolor") and
/// where macOS gives none to read: `#0A84FF` on a dark ground, `#007AFF` on a light one.
pub(crate) const MACOS_SYSTEM_BLUE: SystemAccent = SystemAccent {
    on_dark: [0x0A, 0x84, 0xFF],
    on_light: [0x00, 0x7A, 0xFF],
};

/// The control accent where the platform reads none: on a Mac, its system blue (a headless
/// instance asks macOS nothing); elsewhere none, and the chrome's accent stands in.
const CONTROL_ACCENT_DEFAULT: Option<SystemAccent> = if cfg!(target_os = "macos") {
    Some(MACOS_SYSTEM_BLUE)
} else {
    None
};

/// The installed control accent ([`install_control_accent`]), packed as [`SYSTEM_ACCENT`] is.
#[cfg(not(test))]
static CONTROL_ACCENT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(NO_SYSTEM_ACCENT);

#[cfg(test)]
thread_local! {
    static TEST_CONTROL_ACCENT: Cell<Option<SystemAccent>> = const { Cell::new(None) };
}

/// Install macOS's control accent (`NSColor.controlAccentColor`) as the platform read it at
/// launch or when the system colours changed (`None`: it read none, and system blue stands in).
/// It is what a native control wears where AppKit would paint the accent (a switch's on track,
/// the setup screen's spinner and working band), and leads nothing else: the chrome keeps its
/// theme's accent.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn install_control_accent(accent: Option<SystemAccent>) -> bool {
    let moved = {
        #[cfg(test)]
        {
            TEST_CONTROL_ACCENT.with(|slot| slot.replace(accent) != accent)
        }

        #[cfg(not(test))]
        {
            let packed = accent.map_or(NO_SYSTEM_ACCENT, SystemAccent::pack);
            CONTROL_ACCENT.swap(packed, Ordering::AcqRel) != packed
        }
    };
    if moved {
        note_chrome_inputs_moved();
    }
    moved
}

/// The control accent the platform read: macOS's `controlAccentColor` where it was installed,
/// else the OS accent the chrome follows (Windows'), else none.
#[must_use]
pub(crate) fn control_accent() -> Option<SystemAccent> {
    #[cfg(test)]
    let read = TEST_CONTROL_ACCENT.with(Cell::get);
    #[cfg(not(test))]
    let read = match CONTROL_ACCENT.load(Ordering::Acquire) {
        NO_SYSTEM_ACCENT => None,
        packed => Some(SystemAccent::unpack(packed)),
    };
    read.or_else(system_accent)
}

impl AppearancePreferences {
    pub(crate) fn normalized(self) -> Self {
        Self {
            high_contrast: self.high_contrast,
            reduced_transparency: self.reduced_transparency,
            differentiate_without_color: self.differentiate_without_color,
            text_scale: if self.text_scale.is_finite() && self.text_scale > 0.0 {
                self.text_scale.clamp(0.85, 2.0)
            } else {
                1.0
            },
        }
    }
}

/// Complete semantic palette for a native surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeRoles {
    pub(crate) surface: [u8; 3],
    pub(crate) text_primary: [u8; 3],
    pub(crate) text_secondary: [u8; 3],
    pub(crate) text_tertiary: [u8; 3],
    pub(crate) accent: [u8; 3],
    /// The accent as text.
    pub(crate) accent_ink: [u8; 3],
    pub(crate) on_accent: [u8; 3],
    /// The accent a native control wears where its platform paints the OS accent (a switch's on
    /// track, the setup screen's spinner and working band): the OS's own shade for this ground
    /// ([`control_accent`]; macOS's system blue where it reads none), else [`Self::accent`].
    pub(crate) control_accent: [u8; 3],
    pub(crate) separator: [u8; 3],
    /// A resting control's own boundary, 3:1 on the card and the page: the rim of a chip, a
    /// Secondary or Setting button, a field, a slider or a boxed switch, whose fill stands only a
    /// few percent off its ground ([`crate::native_ui`]'s `resting_rim`).
    pub(crate) control_edge: [u8; 3],
    pub(crate) control_track: [u8; 3],
    pub(crate) elevated: [u8; 3],
    pub(crate) danger: [u8; 3],
    pub(crate) success: [u8; 3],
    /// "Not written": what a page has staged and not yet written, and nothing else
    /// ([`NOT_WRITTEN`], held 3:1 on the page's grounds). Fixed: it follows neither the theme's
    /// accent nor the OS's.
    pub(crate) unwritten: [u8; 3],
    /// [`Self::unwritten`] as text, over its own wash too.
    pub(crate) unwritten_ink: [u8; 3],
}

/// The one colour of "not written" (the Tables design's blue, `#5B9EF0` on a dark ground,
/// `#2F6FC4` on a light one): a staged cell, the count of them, the Write bar. It is no accent, so
/// a Mac whose accent is green still shows a change not yet written in this blue.
pub(crate) const NOT_WRITTEN: SystemAccent = SystemAccent {
    on_dark: [0x5B, 0x9E, 0xF0],
    on_light: [0x2F, 0x6F, 0xC4],
};

/// How far toward [`NativeRoles::unwritten`] a staged cell's ground is washed.
pub(crate) const UNWRITTEN_WASH: f32 = 0.24;

#[derive(Clone, Copy, Debug)]
struct Oklch {
    l: f32,
    c: f32,
    h: f32,
}

/// Whether this chrome background is the dark side.
pub(crate) fn surface_is_dark(bg: [u8; 3]) -> bool {
    !crate::tab_bar::bg_is_light(bg)
}

/// How far toward `accent` a selected navigation row's `elevated` fill is washed when the accent
/// itself is the ink (label and icon).
pub(crate) const ACCENT_WASH_SELECTED: f32 = 0.13;
/// The deepest accent wash that still carries accent ink.
pub(crate) const ACCENT_WASH_PRESSED: f32 = 0.16;
const ACCENT_INK_FLOOR: f32 = 3.0;
// Keep headroom above the 4.5 floor for small painted accent labels.
const ACCENT_TEXT_TARGET: f32 = 5.0;
const CARD_CONTRAST_MIN: f32 = 1.2;
// Leave room for the gallery's f64 calculation of the quantized u8 colours.
const CONTRAST_ROUNDING_MARGIN: f32 = 0.000_1;
const LIGHT_ELEVATION_STEP: f32 = 0.035;
const LIGHT_ELEVATION_TOWARD_WHITE: f32 = 0.6;

fn light_elevation(surface: [u8; 3]) -> [u8; 3] {
    let headroom = (1.0 - rgb_to_oklch(surface).l).max(0.0);
    let amount = (LIGHT_ELEVATION_STEP / headroom).min(LIGHT_ELEVATION_TOWARD_WHITE);
    neutral_surface(surface, [255, 255, 255], amount)
}

const LIGHT_CARDS_RISE: bool = cfg!(windows);

fn light_elevated(surface: [u8; 3], fg: [u8; 3]) -> [u8; 3] {
    if LIGHT_CARDS_RISE {
        light_elevation(surface)
    } else {
        neutral_surface(surface, fg, 0.065)
    }
}

/// Keep the preferred elevation when it leaves a readable foreground on both grounds.
/// A near-white page cannot support a lighter card at the floor; near mid-grey, either
/// direction can also lose the only readable foreground. Try the other direction then.
fn card_surface(surface: [u8; 3], preferred: [u8; 3], rises: bool, text_target: f32) -> [u8; 3] {
    let readable = (text_target + CONTRAST_ROUNDING_MARGIN).min(best_endpoint_contrast(&[surface]));
    let card_target = CARD_CONTRAST_MIN + CONTRAST_ROUNDING_MARGIN;
    let holds = |card| {
        contrast_ratio(card, surface) >= card_target
            && best_endpoint_contrast(&[surface, card]) >= readable
    };
    if holds(preferred) {
        return preferred;
    }
    // Search from the page, so an opposite-direction search cannot stop at a
    // candidate that already passes the card floor on the wrong side.
    let source = rgb_to_oklch(surface);
    let pole = if rises { 1.0 } else { 0.0 };
    let preferred = contrast_candidate(source, &[surface], card_target, pole);
    let opposite = contrast_candidate(source, &[surface], card_target, 1.0 - pole);
    for card in [preferred, opposite].into_iter().flatten() {
        if holds(card) {
            return card;
        }
    }
    // An unattainable text target must not discard the card floor. As with the
    // ink solver, keep the best common foreground instead of changing the page.
    match (preferred, opposite) {
        (Some(a), Some(b)) => {
            if best_endpoint_contrast(&[surface, a]) >= best_endpoint_contrast(&[surface, b]) {
                a
            } else {
                b
            }
        }
        (Some(card), None) | (None, Some(card)) => card,
        (None, None) => ensure_contrast(surface, surface, CARD_CONTRAST_MIN),
    }
}

/// Straight sRGB channel lerp.
pub(crate) fn mix_srgb(a: [u8; 3], b: [u8; 3], amount: f32) -> [u8; 3] {
    let amount = amount.clamp(0.0, 1.0);
    std::array::from_fn(|index| {
        (f32::from(a[index]) + (f32::from(b[index]) - f32::from(a[index])) * amount).round() as u8
    })
}

/// `b` laid over `a` at `amount` alpha as an sRGB render target blends it, in linear light: on a
/// dark ground a light ink's wash comes out lighter than [`mix_srgb`] says.
pub(crate) fn mix_linear(a: [u8; 3], b: [u8; 3], amount: f32) -> [u8; 3] {
    let amount = amount.clamp(0.0, 1.0);
    std::array::from_fn(|index| {
        let (a, b) = (srgb_to_linear(a[index]), srgb_to_linear(b[index]));
        (linear_to_srgb(a + (b - a) * amount).clamp(0.0, 1.0) * 255.0).round() as u8
    })
}

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

const DANGER_HUE_DEGREES: f32 = 25.0;
const SUCCESS_HUE_DEGREES: f32 = 145.0;

fn status_seed(dark: bool, high_contrast: bool, hue_degrees: f32) -> [u8; 3] {
    oklch_to_rgb(Oklch {
        l: if dark { 0.72 } else { 0.49 },
        c: if high_contrast { 0.16 } else { 0.14 },
        h: hue_degrees.to_radians(),
    })
}

/// The `success` hue as decoration rather than ink.
pub(crate) fn success_tint(dark: bool) -> [u8; 3] {
    status_seed(
        dark,
        current_preferences().normalized().high_contrast,
        SUCCESS_HUE_DEGREES,
    )
}

/// Hold a surface at or above the openness (luminance) of `floor`.
pub(crate) fn at_least_as_open_as(color: [u8; 3], floor: [u8; 3]) -> [u8; 3] {
    ensure_contrast(color, [0, 0, 0], contrast_ratio(floor, [0, 0, 0]))
}

/// Everything [`solve_roles`] reads: one key has one palette, whenever it is asked for.
#[derive(Clone, Copy, PartialEq)]
struct RolesKey {
    theme: Theme,
    /// What [`crate::chrome_band::ChromePalette::tokens`] derives from.
    seeds: aterm_messages::tokens::ChromeSeeds,
    preferences: AppearancePreferences,
    control: Option<SystemAccent>,
}

/// The palettes a thread keeps solved: a window's chrome, the default theme each text and icon
/// measure paints by ([`crate::native_ui::words_bands`]), a page's theme and the previews'.
const ROLES_KEPT: usize = 6;

/// A thread's kept palettes, oldest overwritten first.
struct KeptRoles {
    solved: Vec<(RolesKey, NativeRoles)>,
    next: usize,
}

thread_local! {
    static KEPT_ROLES: std::cell::RefCell<KeptRoles> = const {
        std::cell::RefCell::new(KeptRoles { solved: Vec::new(), next: 0 })
    };
}

/// Build the native palette. Its contrast searches cost more than the paint or measure that asks
/// for it, and a page asks once a control, so each thread keeps the last [`ROLES_KEPT`] by all
/// they read ([`RolesKey`]).
pub(crate) fn roles(
    p: impl Into<crate::chrome_band::ChromePalette>,
    preferences: AppearancePreferences,
) -> NativeRoles {
    let p = p.into();
    let preferences = preferences.normalized();
    let control = control_accent();
    let key = RolesKey {
        theme: p.theme,
        seeds: p.seeds(),
        preferences,
        control,
    };
    KEPT_ROLES.with_borrow_mut(|kept| {
        if let Some((_, solved)) = kept.solved.iter().find(|(at, _)| *at == key) {
            return *solved;
        }
        let solved = solve_roles(p, preferences, control);
        if kept.solved.len() < ROLES_KEPT {
            kept.solved.push((key, solved));
        } else {
            let next = kept.next;
            kept.solved[next] = (key, solved);
            kept.next = (next + 1) % ROLES_KEPT;
        }
        solved
    })
}

/// The native palette of `p` under normalized `preferences` and the OS's `control` accent.
fn solve_roles(
    p: crate::chrome_band::ChromePalette,
    preferences: AppearancePreferences,
    control: Option<SystemAccent>,
) -> NativeRoles {
    #[cfg(test)]
    crate::work_counts::roles_solved();
    let theme = p.theme;
    let tokens = p.tokens();
    let bg = packed_rgb(theme.bg);
    let fg = packed_rgb(theme.fg);
    let selection = packed_rgb(theme.selection);
    let dark = surface_is_dark(bg);

    let surface = neutral_surface(bg, fg, if dark { 0.075 } else { 0.055 });
    let elevated = if dark {
        neutral_surface(surface, fg, 0.075)
    } else {
        light_elevated(surface, fg)
    };
    let primary_target = if preferences.high_contrast { 7.0 } else { 4.5 };
    let secondary_target = if preferences.high_contrast { 7.0 } else { 4.5 };
    let tertiary_target = if preferences.high_contrast { 4.5 } else { 3.0 };
    let elevated = card_surface(surface, elevated, dark || LIGHT_CARDS_RISE, primary_target);
    let grounds = [surface, elevated];
    let text_primary = ensure_contrast_on(fg, &grounds, primary_target);
    let text_secondary =
        ensure_contrast_on(mix_oklab(fg, surface, 0.27), &grounds, secondary_target);
    let text_tertiary = ensure_contrast_on(mix_oklab(fg, surface, 0.48), &grounds, tertiary_target);

    let mut accent_seed = rgb_to_oklch(packed_rgb(theme.cursor));
    let selection_seed = rgb_to_oklch(selection);
    if tokens.accent_source != aterm_messages::tokens::AccentSource::Neutral {
        accent_seed = rgb_to_oklch(tokens.accent);
    }
    if accent_seed.c < 0.035 && selection_seed.c >= 0.035 {
        accent_seed = selection_seed;
    }
    if accent_seed.c < 0.035 {
        let bg_hue = rgb_to_oklch(bg).h;
        accent_seed.h = (bg_hue + 0.61 * TAU).rem_euclid(TAU);
    }
    accent_seed.c = accent_seed.c.clamp(0.085, if dark { 0.155 } else { 0.145 });
    accent_seed.l = if dark {
        accent_seed.l.clamp(0.66, 0.79)
    } else {
        accent_seed.l.clamp(0.42, 0.58)
    };
    let accent = ensure_contrast(oklch_to_rgb(accent_seed), surface, ACCENT_INK_FLOOR);
    let accent = accent_over_its_own_wash(accent, elevated);
    let ink_target = if preferences.high_contrast {
        7.0
    } else {
        ACCENT_TEXT_TARGET
    };
    let accent_ink = ensure_contrast_on(
        accent,
        &[
            surface,
            elevated,
            mix_srgb(elevated, accent, ACCENT_WASH_PRESSED),
        ],
        ink_target,
    );

    let black = [8, 10, 13];
    let white = [250, 251, 253];
    let on_seed = if contrast_ratio(black, accent) >= contrast_ratio(white, accent) {
        black
    } else {
        white
    };
    let on_accent = ensure_contrast(on_seed, accent, 4.5);
    let control_accent = control
        .or(CONTROL_ACCENT_DEFAULT)
        .map_or(accent, |os| os.for_surface(bg));

    let separator_target = if preferences.high_contrast { 3.0 } else { 1.5 };
    let separator = match tokens.border {
        Some(border) => ensure_contrast(border, surface, separator_target),
        None => ensure_contrast(mix_oklab(fg, surface, 0.70), surface, separator_target),
    };
    let control_track = ensure_contrast(mix_oklab(fg, surface, 0.57), surface, 2.0);
    let control_edge = ensure_contrast_on(separator, &grounds, 3.0);

    #[allow(
        clippy::cast_possible_truncation,
        reason = "an OkLCh hue in degrees, [0, 360), fits f32"
    )]
    let hue = |token: Option<f64>, fallback: f32| token.map_or(fallback, |h| h as f32);
    let danger = ensure_contrast_on(
        status_seed(
            dark,
            preferences.high_contrast,
            hue(tokens.danger_hue, DANGER_HUE_DEGREES),
        ),
        &grounds,
        4.5,
    );
    let success = ensure_contrast_on(
        status_seed(
            dark,
            preferences.high_contrast,
            hue(tokens.success_hue, SUCCESS_HUE_DEGREES),
        ),
        &grounds,
        4.5,
    );

    let unwritten = ensure_contrast_on(NOT_WRITTEN.for_surface(bg), &grounds, ACCENT_INK_FLOOR);
    let unwritten_ink = ensure_contrast_on(
        unwritten,
        &[
            surface,
            elevated,
            mix_srgb(elevated, unwritten, UNWRITTEN_WASH),
        ],
        ink_target,
    );

    NativeRoles {
        surface,
        text_primary,
        text_secondary,
        text_tertiary,
        accent,
        accent_ink,
        on_accent,
        control_accent,
        separator,
        control_edge,
        control_track,
        elevated,
        danger,
        success,
        unwritten,
        unwritten_ink,
    }
}

impl NativeRoles {
    /// The roles of a chrome palette.
    pub(crate) fn from_theme(p: impl Into<crate::chrome_band::ChromePalette>) -> Self {
        default_roles(p)
    }

    /// The grounds of a grouped form, as macOS's grouped form style lays one out (the setup
    /// screen is one): its page and the cards on it. On a light ground the cards rise to near
    /// white and the page sinks until a card stands [`CARD_CONTRAST_MIN`] apart from it, where a
    /// light theme's own cards sink into a lighter page. If those grounds would reduce the
    /// existing text inks' shared contrast, keep the already-solved surface and elevated pair.
    /// On a dark ground the page is the surface and the cards the elevated surface, held that
    /// far apart.
    pub(crate) fn grouped(&self) -> Grouped {
        let apart = CARD_CONTRAST_MIN + 0.01;
        if surface_is_dark(self.surface) {
            let card = if contrast_ratio(self.elevated, self.surface) >= CARD_CONTRAST_MIN {
                self.elevated
            } else {
                ensure_contrast(self.elevated, self.surface, apart)
            };
            return Grouped {
                page: self.surface,
                card,
            };
        }
        let card = mix_srgb(self.surface, [255, 255, 255], GROUPED_CARD_TOWARD_WHITE);
        let grouped = Grouped {
            page: ensure_contrast(self.surface, card, apart),
            card,
        };
        let original = [self.surface, self.elevated];
        let proposed = [grouped.page, grouped.card];
        if [self.text_primary, self.text_secondary, self.text_tertiary]
            .into_iter()
            .all(|ink| least_contrast(ink, &proposed) >= least_contrast(ink, &original))
        {
            grouped
        } else {
            Grouped {
                page: self.surface,
                card: self.elevated,
            }
        }
    }
}

/// How far toward white a light grouped form's cards rise from the page's surface: near white,
/// keeping a trace of a tinted theme's hue.
const GROUPED_CARD_TOWARD_WHITE: f32 = 0.85;

/// A grouped form's grounds ([`NativeRoles::grouped`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Grouped {
    /// The page under the cards.
    pub(crate) page: [u8; 3],
    /// A card's ground.
    pub(crate) card: [u8; 3],
}

pub(crate) fn default_roles(p: impl Into<crate::chrome_band::ChromePalette>) -> NativeRoles {
    roles(p, current_preferences())
}

/// The forced-chrome seed palette for config `window_theme = light`.
pub(crate) const FORCED_LIGHT_CHROME_BG: u32 = 0x00FA_FAFA;
/// Dark text for the forced-light chrome (Adwaita's `#2e3436`).
pub(crate) const FORCED_LIGHT_CHROME_FG: u32 = 0x002E_3436;
/// The forced-chrome seed palette for config `window_theme = dark`.
pub(crate) const FORCED_DARK_CHROME_BG: u32 = 0x0024_2424;
/// Near-white text for the forced-dark chrome.
pub(crate) const FORCED_DARK_CHROME_FG: u32 = 0x00ED_EDED;

const FORCED_CHROME_ACCENT_FLOOR: f32 = ACCENT_INK_FLOOR;

/// The [`Theme`] the chrome painters draw from when config `window_theme = light|dark` forces the
/// side against the terminal theme's darkness.
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

/// The chrome palette for a `window_theme` policy over a `terminal` theme.
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

fn pack_rgb(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

const fn packed_rgb(c: u32) -> [u8; 3] {
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

fn least_contrast(ink: [u8; 3], grounds: &[[u8; 3]]) -> f32 {
    grounds
        .iter()
        .map(|&ground| contrast_ratio(ink, ground))
        .fold(f32::INFINITY, f32::min)
}

fn best_endpoint_contrast(grounds: &[[u8; 3]]) -> f32 {
    least_contrast([0, 0, 0], grounds).max(least_contrast([255, 255, 255], grounds))
}

fn contrast_candidate(
    color: Oklch,
    grounds: &[[u8; 3]],
    target: f32,
    pole: f32,
) -> Option<[u8; 3]> {
    let pole_color = oklch_to_rgb(Oklch { l: pole, ..color });
    if least_contrast(pole_color, grounds) < target {
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
        if least_contrast(candidate, grounds) >= target {
            hi = t;
            best = candidate;
        } else {
            lo = t;
        }
    }
    Some(best)
}

/// `fg` as text on `bg`.
pub(crate) fn ensure_text_contrast(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    let target = if current_preferences().normalized().high_contrast {
        7.0
    } else {
        4.5
    };
    ensure_contrast(fg, bg, target)
}

/// `fg` moved in lightness, its hue kept, until it reads at `target` against `bg`: the rule of
/// [`ensure_text_contrast`] at a ratio the caller names (a state mark's 3:1, a card's 1.2:1
/// separation from its page).
pub(crate) fn ensure_contrast_ratio(fg: [u8; 3], bg: [u8; 3], target: f32) -> [u8; 3] {
    ensure_contrast(fg, bg, target)
}

/// How far apart two colours' OKLCH hues stand, in degrees (0 to 180).
pub(crate) fn hue_distance(a: [u8; 3], b: [u8; 3]) -> f32 {
    let apart = (rgb_to_oklch(a).h - rgb_to_oklch(b).h)
        .to_degrees()
        .rem_euclid(360.0);
    apart.min(360.0 - apart)
}

/// A colour's OKLCH chroma: 0 for a grey.
#[cfg(test)]
pub(crate) fn chroma(rgb: [u8; 3]) -> f32 {
    rgb_to_oklch(rgb).c
}

/// `rgb` with its chroma taken out: the grey of the same OKLCH lightness.
pub(crate) fn without_chroma(rgb: [u8; 3]) -> [u8; 3] {
    let mut color = rgb_to_oklch(rgb);
    color.c = 0.0;
    oklch_to_rgb(color)
}

fn ensure_contrast(fg: [u8; 3], bg: [u8; 3], target: f32) -> [u8; 3] {
    ensure_contrast_on(fg, &[bg], target)
}

/// Search once against every ground: repairing the last must not break an earlier one.
/// If even black and white cannot meet the target (for example 7:1 on mid-grey),
/// retain the best endpoint common contrast without changing the page's policy.
pub(crate) fn ensure_contrast_on(fg: [u8; 3], grounds: &[[u8; 3]], target: f32) -> [u8; 3] {
    let target = target + CONTRAST_ROUNDING_MARGIN;
    if least_contrast(fg, grounds) >= target {
        return fg;
    }
    let source = rgb_to_oklch(fg);
    let dark = contrast_candidate(source, grounds, target, 0.0);
    let light = contrast_candidate(source, grounds, target, 1.0);
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
            if least_contrast(black, grounds) >= least_contrast(white, grounds) {
                black
            } else {
                white
            }
        }
    }
}

#[cfg(test)]
#[path = "native_appearance_tests.rs"]
mod tests;
