// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared themed cells for compact in-grid chrome such as the find bar, config
//! notices, and native-tab backing rows. This is deliberately independent of any
//! status feature: callers provide the content and own its lifecycle.

#[cfg(test)]
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_render::Theme;

use crate::tab_bar::bg_is_light;

// ---- The OS-forced chrome palette (Windows High Contrast) ---------------------------
//
// WHY THIS EXISTS. Windows High Contrast is a CONTRACT, not a theme: while an HC
// scheme is active the OS palette owns every chrome surface, and an app that keeps
// painting its own tones under it is the accessibility defect. aterm already honoured
// half of that contract — `platform_win::apply_chrome_appearance` defers the CAPTION
// to the OS under HC — and the seam that exposed the other half sat four pixels
// below it: the tab strip, the find bar and the config-notice bands all kept aterm's
// theme tones directly under an OS-palette caption. That visible discontinuity is the
// defect this module's palette closes.
//
// WHAT DELIBERATELY DOES **NOT** FOLLOW HC: the terminal GRID. A colour scheme is USER
// CONTENT on a terminal — it is what the user's programs are painting with — and
// Windows Terminal keeps the profile scheme under HC for exactly this reason.
// Repaletting the grid would destroy the output being read rather than make it
// legible. So the split is: CHROME follows the OS, CONTENT follows the user. (An
// earlier framing of this work said "High Contrast never reaches the strip OR the
// grid"; the grid half was wrong and is not implemented.)
//
// WHY THE PALETTE IS A PROCESS-WIDE LATCH AND NOT AN ARGUMENT. Every chrome tone in
// this crate is derived from a `Theme` alone — `band_colors(theme)`,
// `tab_bar::strip_colors(theme)`, `tab_bar::blank_cell(theme)`,
// `tab_bar::strip_bleed_tones(theme)` — and those are called from a dozen paint sites
// across four files, several of them in `#[cfg(test)]`-only helpers. Threading a
// fifth palette argument through all of them to carry a fact that is process-global
// by construction (there is one desktop, one HC scheme) would be a large mechanical
// diff whose only effect is to move the same global one call frame outward. The latch
// is published by exactly ONE writer (`platform_win::resync_forced_chrome_palette`,
// the Windows arm) and is `None` on every other platform, so macOS and Linux paint
// byte-identically to before.

/// The OS-forced chrome palette: the five Win32 system colours every chrome surface
/// is painted from while a High-Contrast scheme is active.
///
/// Stored as plain RGB triples in THEME byte order — the platform arm does the
/// COLORREF (`0x00BBGGRR`) swap on the way in, so nothing downstream of here has to
/// know GDI's byte order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ForcedChrome {
    /// `COLOR_WINDOW` — the document / editable-field surface. Carries the find
    /// bar's inset WELL and the tab strip's hover wash.
    pub window: [u8; 3],
    /// `COLOR_WINDOWTEXT` — THE ink. An HC palette has no secondary text tone,
    /// because dimming text is precisely what an HC user opted out of: the band's
    /// label, value, warn and inactive-tab roles all collapse onto this one colour
    /// and let WEIGHT (bold) carry what hue and dimming used to.
    pub window_text: [u8; 3],
    /// `COLOR_HIGHLIGHT` — the SELECTED surface: the active tab chip, the `↻`
    /// update CTA, and the accent rule.
    pub highlight: [u8; 3],
    /// `COLOR_HIGHLIGHTTEXT` — ink on [`Self::highlight`].
    pub highlight_text: [u8; 3],
    /// `COLOR_BTNFACE` — the control-face surface every chrome BAND is painted on.
    /// Win32's own split is document = `WINDOW`, control = `BTNFACE`; the strip and
    /// the find/notice bands are controls, the find bar's query field is a document.
    /// Every stock HC scheme sets the two equal, so in practice they read as one
    /// surface separated by the seam — which is the HC convention (borders, not
    /// fills).
    pub btn_face: [u8; 3],
}

/// The contrast floor applied on top of an OS-forced palette. Provably INERT on all
/// four stock Windows HC schemes (their `WINDOWTEXT`/`BTNFACE` and
/// `HIGHLIGHTTEXT`/`HIGHLIGHT` pairs are 15:1 or better), so it never overrides what
/// the OS chose. It exists for a hand-edited or third-party HC scheme that pairs two
/// tones the OS itself never would: reaching for pure black/white there is still a
/// high-contrast answer, whereas printing the scheme's own unreadable pair is not.
/// The same 3.0 UI-text floor `tab_bar::STRIP_INK_FLOOR` uses, for the same reason.
const FORCED_INK_FLOOR: f64 = 3.0;

/// Sentinel in slot 0 for "no forced palette" — an RGB triple packs to 24 bits, so
/// `u32::MAX` cannot collide with a real colour.
const FORCED_ABSENT: u32 = u32::MAX;

/// The published palette, packed `0x00RRGGBB`, in [`ForcedChrome`] field order.
///
/// Five relaxed atomics rather than a `Mutex<Option<_>>`: `band_colors` /
/// `strip_colors` are on paint paths (a strip rebuild, a band build, one
/// `blank_cell` per row) and a chrome tone has no business taking a lock there.
///
/// PUBLICATION PROTOCOL. Slot 0 is the GATE: it is written LAST when installing and
/// FIRST when clearing, so a reader either sees `FORCED_ABSENT` (and paints the
/// ordinary theme-derived tones, which are always safe) or a fully-written set — it
/// can never see a new `window` beside a stale `btn_face`. In production the writer
/// and every reader are the same winit main thread, so a torn read is unreachable;
/// the ordering is what makes that statement true for free rather than by assertion.
/// [`published_forced_chrome_is_all_or_nothing`] holds the order to it.
static FORCED_CHROME: [AtomicU32; 5] = [
    AtomicU32::new(FORCED_ABSENT),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

// Production has ONE platform appearance observer and therefore one process-wide
// palette. Libtest deliberately runs independent Apps on a reused worker pool, so a
// process-global test palette would let one worker repaint another worker's strip
// mid-assertion. Same split, and the same rationale, as `native_appearance`'s
// preference snapshot.
#[cfg(test)]
thread_local! {
    static TEST_FORCED_CHROME: Cell<Option<ForcedChrome>> = const { Cell::new(None) };
}

/// Publish (or retract, with `None`) the OS-forced chrome palette. Returns `true`
/// only when the palette actually MOVED, so the host can skip a strip-cache
/// invalidation + repaint it does not need — and arms
/// [`crate::native_appearance::note_chrome_inputs_moved`], so the re-sample that
/// SETTLES the windows still learns about the move when this edge was consumed by
/// some other reader (window attach and startup both republish the palette).
///
/// WRITE side only, hence the `cfg`: the only platform that publishes a palette is
/// Windows (`platform_win::resync_forced_chrome_palette`, itself `cfg(windows)`) and
/// the only other caller is [`hc_fixtures::with_forced`]. The READ side
/// ([`forced_chrome`]) stays unconditional — every platform's band and strip consult
/// it — so a Linux or macOS build keeps painting theme tones and no longer carries a
/// `dead_code` warning for a writer it can never reach.
#[cfg(any(windows, test))]
pub(crate) fn install_forced_chrome(palette: Option<ForcedChrome>) -> bool {
    let moved = {
        #[cfg(test)]
        {
            TEST_FORCED_CHROME.with(|slot| slot.replace(palette) != palette)
        }

        #[cfg(not(test))]
        {
            publish_forced_chrome(palette)
        }
    };
    if moved {
        crate::native_appearance::note_chrome_inputs_moved();
    }
    moved
}

/// The live OS-forced chrome palette, or `None` when the app owns its own tones
/// (every platform but Windows, and Windows whenever High Contrast is off).
#[must_use]
pub(crate) fn forced_chrome() -> Option<ForcedChrome> {
    #[cfg(test)]
    {
        TEST_FORCED_CHROME.with(Cell::get)
    }

    #[cfg(not(test))]
    {
        published_forced_chrome()
    }
}

/// The shipping half of [`install_forced_chrome`]: pack the palette into
/// [`FORCED_CHROME`] under the publication protocol. Split out — and compiled in
/// EVERY configuration, not just `cfg(not(test))` — because the bit-packing and the
/// slot order are the part a channel swap or a shift bug would break silently, and a
/// codec that only exists in the shipping build is a codec no test can reach.
/// (`cfg` for the same reason as [`install_forced_chrome`]: writers are Windows-only.)
#[cfg(any(windows, test))]
fn publish_forced_chrome(palette: Option<ForcedChrome>) -> bool {
    if published_forced_chrome() == palette {
        return false;
    }
    let Some(p) = palette else {
        FORCED_CHROME[0].store(FORCED_ABSENT, Ordering::Release);
        return true;
    };
    for (slot, value) in publish_sequence(p) {
        FORCED_CHROME[slot].store(value, Ordering::Release);
    }
    true
}

/// The publication protocol as DATA: the `(slot, value)` stores, in write order.
///
/// Data rather than a straight line of `store` calls so the invariant the module doc
/// sells — gate slot 0 goes `FORCED_ABSENT` FIRST, the four tail slots follow, and
/// the gate is written LAST with the real `window` — is something a test can read
/// back and hold, instead of a sentence next to code that did the opposite. (It did:
/// the original zipped `FORCED_CHROME.iter()` against all five fields, which opened
/// the gate with `window` BEFORE writing `btn_face`.)
#[cfg(any(windows, test))]
fn publish_sequence(p: ForcedChrome) -> [(usize, u32); 6] {
    [
        (0, FORCED_ABSENT),
        (1, pack(p.window_text)),
        (2, pack(p.highlight)),
        (3, pack(p.highlight_text)),
        (4, pack(p.btn_face)),
        (0, pack(p.window)),
    ]
}

/// The shipping half of [`forced_chrome`]. See [`publish_forced_chrome`] for why it
/// is compiled unconditionally.
fn published_forced_chrome() -> Option<ForcedChrome> {
    let window = FORCED_CHROME[0].load(Ordering::Acquire);
    if window == FORCED_ABSENT {
        return None;
    }
    Some(ForcedChrome {
        window: unpack(window),
        window_text: unpack(FORCED_CHROME[1].load(Ordering::Acquire)),
        highlight: unpack(FORCED_CHROME[2].load(Ordering::Acquire)),
        highlight_text: unpack(FORCED_CHROME[3].load(Ordering::Acquire)),
        btn_face: unpack(FORCED_CHROME[4].load(Ordering::Acquire)),
    })
}

/// Pack one RGB triple into `0x00RRGGBB`. Write side, so `cfg`-gated with its only
/// caller; [`unpack`] is not, because every platform reads.
#[cfg(any(windows, test))]
fn pack(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

fn unpack(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

/// Floor one OS-forced ink against the OS-forced surface it lands on. See
/// [`FORCED_INK_FLOOR`] for why a palette the OS chose is floored at all.
pub(crate) fn forced_ink(ink: [u8; 3], on: [u8; 3]) -> [u8; 3] {
    ensure_contrast(ink, on, FORCED_INK_FLOOR)
}

/// On-theme tones for compact chrome bands.
#[derive(Clone, Copy)]
pub(crate) struct BandColors {
    pub bar_bg: [u8; 3],
    pub label: [u8; 3],
    pub value: [u8; 3],
    pub warn: [u8; 3],
    /// Background of an editable WELL inset in the band (the find bar's query
    /// field). The terminal's own background, so the band reads as a raised panel
    /// with a recessed input in it — and so `value` text in the well keeps the
    /// terminal's own fg/bg contrast rather than the band's smaller one.
    pub field_bg: [u8; 3],
    /// Text caret drawn in that well — the theme's CURSOR colour, contrast-floored
    /// against `field_bg` so it stays visible on a recoloured background.
    pub caret: [u8; 3],
    /// A drawn BORDER for the well, for the case where its fill cannot carry the
    /// boundary on its own — `Some(ink)` exactly when `field_bg == bar_bg`.
    ///
    /// Every stock Windows High-Contrast scheme sets `COLOR_WINDOW == COLOR_BTNFACE`,
    /// so the document/control split the forced mapping honours collapses to one tone
    /// and the well loses its edge entirely: an editable field indistinguishable from
    /// the band around it. HC's own convention is that surfaces are separated by
    /// BORDERS rather than fills, and this is that border — the piece the fill-only
    /// well was missing. `None` on every theme-derived scheme (`field_bg` is
    /// `theme.bg` against a 0.10/0.16 blend, which is what makes the well read as an
    /// inset), so nothing off an OS palette moves.
    pub well_rule: Option<[u8; 3]>,
    /// The FILL of a determinate meter drawn in the band (the status bars'
    /// progress): the theme's cursor accent, contrast-floored against
    /// [`Self::bar_bg`] so a pale cursor on a pale band still reads as a fill.
    /// Under an OS-forced palette it is `COLOR_HIGHLIGHT` — exactly what a native
    /// Win32 progress bar paints its fill with under High Contrast.
    pub accent: [u8; 3],
    /// The TRACK a meter's unfilled remainder is drawn on: a step off
    /// [`Self::bar_bg`] toward the ink, so the empty part reads as a recessed
    /// channel rather than as bare band. Under an OS-forced palette it is the
    /// document surface (`COLOR_WINDOW`), the HC vocabulary's "well".
    pub meter_track: [u8; 3],
}

fn rgb(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

fn blend(a: u32, b: u32, t: f32) -> [u8; 3] {
    mix3(rgb(a), rgb(b), t)
}

/// Linear blend of two RGB triples: `a` toward `b` by `t` ∈ [0,1].
///
/// `pub(crate)` because the tab strip derives its band, its raised card and its
/// seam with exactly this blend. It had a byte-identical private copy; two copies
/// of a colour blend is how two chrome surfaces drift apart by a rounding step.
pub(crate) fn mix3(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let mix = |x: u8, y: u8| (f32::from(x).mul_add(1.0 - t, f32::from(y) * t)).round() as u8;
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
}

/// WCAG relative-contrast ratio between two RGB triples.
///
/// `pub(crate)` because [`ensure_contrast`] is a one-way ratchet — it can only push
/// ink AWAY from its surface — and the tab strip needs to know when that ratchet has
/// overshot (a DIMMED label the floor dragged past full strength is no longer a dim).
/// Answering that needs the ratio itself, not just the floor.
pub(crate) fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
    aterm_types::Rgb::new(a[0], a[1], a[2]).contrast(aterm_types::Rgb::new(b[0], b[1], b[2]))
}

/// Nudge ink `c` toward black/white (whichever the background is not) until it
/// clears `target`:1 against `bg`, in ten steps, returning the best it reached when
/// the target is unreachable. A no-op when `c` already clears the target, so it is
/// safe to wrap an ink that is normally fine and only needs a floor on an
/// exotic user theme.
///
/// `pub(crate)` because the tab strip's inks moved OFF the terminal background and
/// onto the chrome band, where a theme's own fg/bg contrast no longer describes
/// what the reader sees.
pub(crate) fn ensure_contrast(c: [u8; 3], bg: [u8; 3], target: f64) -> [u8; 3] {
    if contrast(c, bg) >= target {
        return c;
    }
    let anchor = if bg_is_light(bg) {
        [0, 0, 0]
    } else {
        [255, 255, 255]
    };
    let mut best = c;
    let mut best_ratio = contrast(c, bg);
    let mut step = 1u8;
    while step <= 10 {
        let mixed = mix3(c, anchor, f32::from(step) / 10.0);
        let ratio = contrast(mixed, bg);
        if ratio > best_ratio {
            best = mixed;
            best_ratio = ratio;
        }
        if ratio >= target {
            return mixed;
        }
        step += 1;
    }
    best
}

/// The band tones under an OS-forced chrome palette ([`ForcedChrome`]) — today,
/// Windows High Contrast.
///
/// The band is a CONTROL surface (`COLOR_BTNFACE`) and the find bar's query field is
/// a DOCUMENT one (`COLOR_WINDOW`), which is Win32's own split and the reason those
/// two system colours exist separately at all. Every ink collapses onto
/// `COLOR_WINDOWTEXT`: `label` is normally a dim of `value`, and an HC scheme has no
/// dim — a user who turned High Contrast on asked for exactly one text colour, and
/// the find panel's hierarchy is carried by weight and position instead. `warn`
/// collapses too: HC deliberately discards hue as a channel, so a yellow-on-band
/// warning tone would either be overruled by the scheme or ignore it.
///
/// Rejected alternative: mapping `warn` to `COLOR_HIGHLIGHT`. That colour means
/// SELECTED in the HC vocabulary (it is what the active tab chip uses), and
/// borrowing it for a severity would make a warning look like a selection.
fn forced_band_colors(hc: ForcedChrome) -> BandColors {
    let on_band = forced_ink(hc.window_text, hc.btn_face);
    let in_well = forced_ink(hc.window_text, hc.window);
    BandColors {
        bar_bg: hc.btn_face,
        label: on_band,
        value: on_band,
        warn: on_band,
        field_bg: hc.window,
        caret: in_well,
        // See [`BandColors::well_rule`]: every stock HC scheme has WINDOW == BTNFACE,
        // so the fill alone leaves the query field with no boundary at all.
        well_rule: (hc.window == hc.btn_face).then_some(in_well),
        accent: hc.highlight,
        meter_track: hc.window,
    }
}

/// The CSD headerbar fills the band sits directly under on Linux — sctk-adwaita's
/// ACTIVE `ColorMap::headerbar` values (`theme.rs` in the vendored winit's
/// decoration crate): `Color::from_rgba8(48, 48, 48)` dark, `(235, 235, 235)`
/// light. Transcribed, not sampled: the frame-audit measured aterm's band at
/// `#303135` (dark) against the CSD's `#303030` — a 2-luma-step mismatch that
/// made the two strips read as separate headers with a visible tone break at
/// their seam. The band's base tone is now the SAME gray family as the titlebar
/// above it, so titlebar + band read as ONE header (the Ptyxis/libadwaita look
/// the audit held up as the reference).
///
/// ACTIVE tones on purpose: the band does not track window focus (no repaint
/// exists on focus change for it), and an active-matched band under an inactive
/// titlebar is the quieter failure than a focus-flickering band.
///
/// # The band/page split is INTENDED — do not "fix" it
///
/// A recurring audit finding: with a MID-TONE terminal background the band and
/// the native page rail below it separate hard. Measured, `background =
/// "#8A8A8A"` under `window_theme = auto`: the band paints `#303030` while the
/// Settings rail derives `#797676` from the same theme — about 3.4:1, a visible
/// tone break running the full width of the window one row under the titlebar.
/// It looks like a bug. It is the correct answer, for one reason:
///
/// **The band belongs to the HEADER, not to the page.** On Linux the surface
/// directly above it is sctk-adwaita's CLIENT-side titlebar, and that titlebar's
/// only knob is `light | dark` — winit's decoration crate exposes no colour, so
/// aterm cannot move it toward a mid-tone theme even if it wanted to. Whatever
/// the band does, the titlebar stays `#303030`/`#EBEBEB`. There are therefore
/// only two seams available and exactly one of them can be closed:
///
///  * band == titlebar, band != page — one solid header sitting on a themed
///    body. The break lands where a break BELONGS: at the boundary between
///    window chrome and window content, which is where every GTK application on
///    the same desktop puts one;
///  * band == page, band != titlebar — a themed strip wedged between an
///    unthemeable grey titlebar and the body, i.e. THREE tones stacked, with the
///    break running through the middle of the header. That is the exact defect
///    the 2026-08 frame audit found and this constant fixed, at a mismatch of
///    two luma steps; re-deriving the band from the theme would reopen it at
///    full strength.
///
/// The band also follows the CHROME-RESOLVED palette, not the terminal one: its
/// caller hands [`band_colors`] `App::chrome_palette_theme`, so config
/// `window_theme = dark` over a LIGHT terminal theme classifies dark here and
/// the band goes `#303030` in step with the CSD variant and the native pages.
/// Band, titlebar and pages therefore move together on the one axis the platform
/// actually gives us; only the terminal GRID keeps its own palette, which is the
/// whole point of a terminal theme.
///
/// What is NOT settled by this note: the split is only correct while the CSD
/// titlebar is real. A desktop with server-side decorations (or a future winit
/// that lets a client colour its headerbar) removes the constraint, and then the
/// band should follow the page. `linux_band_base_tone_is_the_exact_csd_headerbar_gray`
/// is the tripwire — it will fail the moment someone re-derives this.
#[cfg(target_os = "linux")]
pub(crate) const CSD_HEADERBAR_DARK: [u8; 3] = [0x30, 0x30, 0x30];
#[cfg(target_os = "linux")]
pub(crate) const CSD_HEADERBAR_LIGHT: [u8; 3] = [0xEB, 0xEB, 0xEB];

/// Appearance-aware, theme-derived band tones with WCAG-AA text contrast.
///
/// Under an OS-forced chrome palette (Windows High Contrast) this defers wholesale
/// to [`forced_band_colors`] — the OS owns chrome colour then, and a theme-derived
/// blend under an OS-palette caption is the seam that made HC support incoherent.
///
/// LINUX: the band's base tone is NOT theme-derived — it is the exact adwaita
/// headerbar gray of the CSD titlebar directly above it (see
/// [`CSD_HEADERBAR_DARK`]/[`CSD_HEADERBAR_LIGHT`]), picked dark/light by the same
/// [`bg_is_light`] classifier every other chrome surface uses. The inks below are
/// contrast-floored against whatever surface they land on, so a theme's fg keeps
/// clearing AA on the fixed gray exactly as it did on the blend.
pub(crate) fn band_colors(theme: Theme) -> BandColors {
    if let Some(hc) = forced_chrome() {
        return forced_band_colors(hc);
    }
    let light = bg_is_light(rgb(theme.bg));
    #[cfg(target_os = "linux")]
    let bar_bg = if light {
        CSD_HEADERBAR_LIGHT
    } else {
        CSD_HEADERBAR_DARK
    };
    #[cfg(not(target_os = "linux"))]
    let bar_bg = blend(theme.bg, theme.fg, if light { 0.10 } else { 0.16 });
    let warn_base = if light {
        rgb(0x009A_6700)
    } else {
        rgb(0x00F1_FA8C)
    };
    const AA: f64 = 4.5;
    let field_bg = rgb(theme.bg);
    BandColors {
        bar_bg,
        // `label` is the SECONDARY tone, not an optional one: it carries the find
        // panel's whole hint row, its placeholder, and every inactive toggle. Held to
        // the same AA floor as `value` — a dim role still has to be readable, and
        // `value` (bold, full contrast) keeps the hierarchy on its own.
        label: ensure_contrast(
            blend(theme.fg, theme.bg, if light { 0.40 } else { 0.48 }),
            bar_bg,
            AA,
        ),
        value: ensure_contrast(rgb(theme.fg), bar_bg, AA),
        warn: ensure_contrast(warn_base, bar_bg, AA),
        field_bg,
        caret: ensure_contrast(rgb(theme.cursor), field_bg, AA),
        // The theme-derived well is an INSET: `field_bg` is the terminal background
        // and `bar_bg` a 0.10/0.16 step off it, so the fill already draws the edge.
        // The equality guard is not dead — a user theme is free to land on a `bg`
        // that blends to itself.
        well_rule: (field_bg == bar_bg).then(|| ensure_contrast(rgb(theme.fg), field_bg, AA)),
        // A meter fill is a SURFACE, not text: the 3:1 non-text floor (the same
        // one the strip's inks use), so the cursor accent survives on a band it
        // happens to resemble without being dragged to black/white needlessly.
        accent: ensure_contrast(rgb(theme.cursor), bar_bg, 3.0),
        meter_track: mix3(bar_bg, rgb(theme.fg), if light { 0.12 } else { 0.18 }),
    }
}

/// Build one render cell for compact chrome.
pub(crate) fn cell(ch: char, fg: [u8; 3], bg: [u8; 3], bold: bool, seam: bool) -> RenderCell {
    RenderCell {
        ch,
        fg,
        bg,
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold,
        italic: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
        overline: seam,
        underline_color: None,
    }
}

/// A theme-derived blank band cell with a top seam.
#[must_use]
pub(crate) fn blank_cell(theme: Theme) -> RenderCell {
    let colors = band_colors(theme);
    cell(' ', colors.label, colors.bar_bg, false, true)
}

#[cfg(test)]
pub(crate) mod hc_fixtures {
    use super::ForcedChrome;

    /// The four stock Windows High-Contrast schemes, as `GetSysColor` reports them
    /// (`WINDOW`, `WINDOWTEXT`, `HIGHLIGHT`, `HIGHLIGHTTEXT`, `BTNFACE`). Transcribed
    /// from Settings ▸ Accessibility ▸ Contrast themes, where "Background" is
    /// `WINDOW`/`BTNFACE`, "Text" is `WINDOWTEXT`, and "Selected text" is the
    /// `HIGHLIGHT`/`HIGHLIGHTTEXT` pair.
    ///
    /// They are FIXTURES, not an assertion about the live machine: the point of a
    /// test over them is that aterm's chrome stays legible for the palettes real HC
    /// users actually run, without any test needing an HC desktop.
    pub(crate) const STOCK: [(&str, ForcedChrome); 4] = [
        (
            "Aquatic",
            ForcedChrome {
                window: [0x00, 0x00, 0x00],
                window_text: [0xFF, 0xFF, 0xFF],
                highlight: [0x37, 0x00, 0x6E],
                highlight_text: [0xFF, 0xFF, 0xFF],
                btn_face: [0x00, 0x00, 0x00],
            },
        ),
        (
            "Desert",
            ForcedChrome {
                window: [0xFF, 0xFF, 0xFF],
                window_text: [0x00, 0x00, 0x00],
                highlight: [0x37, 0x00, 0x6E],
                highlight_text: [0xFF, 0xFF, 0xFF],
                btn_face: [0xFF, 0xFF, 0xFF],
            },
        ),
        (
            "Dusk",
            ForcedChrome {
                window: [0x2D, 0x32, 0x36],
                window_text: [0xFF, 0xFF, 0xFF],
                highlight: [0x1A, 0xEB, 0xFF],
                highlight_text: [0x00, 0x00, 0x00],
                btn_face: [0x2D, 0x32, 0x36],
            },
        ),
        (
            "Night sky",
            ForcedChrome {
                window: [0x00, 0x00, 0x00],
                window_text: [0xFF, 0xFF, 0xFF],
                highlight: [0x1A, 0xEB, 0xFF],
                highlight_text: [0x00, 0x00, 0x00],
                btn_face: [0x00, 0x00, 0x00],
            },
        ),
    ];

    /// Install `palette` for the duration of `body` on THIS test thread, restoring
    /// whatever was there before (see the thread-local rationale on
    /// `TEST_FORCED_CHROME`). Panic-safe enough for a unit test: a failing assertion
    /// aborts the thread, and the next test on that worker installs its own.
    pub(crate) fn with_forced<R>(palette: ForcedChrome, body: impl FnOnce() -> R) -> R {
        let previous = super::forced_chrome();
        super::install_forced_chrome(Some(palette));
        let out = body();
        super::install_forced_chrome(previous);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under every stock High-Contrast scheme the band's ink clears AA against the
    /// band, and the find bar's caret clears it against the WELL — the same two
    /// properties the theme-derived test below asserts, measured on the OS palette
    /// instead of on aterm's tones. (An HC palette clears these by construction;
    /// the test exists so a re-mapping of the five system colours that quietly
    /// paired ink with the wrong surface fails here.)
    #[test]
    fn forced_band_colors_meet_wcag_aa_on_every_stock_high_contrast_scheme() {
        for (name, palette) in hc_fixtures::STOCK {
            hc_fixtures::with_forced(palette, || {
                // The THEME is irrelevant under a forced palette — that is the whole
                // claim — so pass a deliberately hostile one and prove nothing of it
                // survives into the band.
                let hostile = Theme {
                    fg: 0x0011_1318,
                    bg: 0x0011_1318,
                    cursor: 0x0011_1318,
                    selection: 0x0011_1318,
                };
                let colors = band_colors(hostile);
                assert_eq!(colors.bar_bg, palette.btn_face, "{name}: band is BTNFACE");
                assert_eq!(colors.field_bg, palette.window, "{name}: well is WINDOW");
                for (role, value) in [
                    ("value", colors.value),
                    ("warn", colors.warn),
                    ("label", colors.label),
                ] {
                    assert!(
                        contrast(value, colors.bar_bg) >= 4.5,
                        "{name} {role} must meet WCAG-AA under High Contrast"
                    );
                }
                assert!(
                    contrast(colors.caret, colors.field_bg) >= 4.5,
                    "{name} caret must meet WCAG-AA in the well"
                );
                // THE WELL MUST STILL HAVE AN EDGE. Every stock HC scheme sets
                // WINDOW == BTNFACE, so the fill that normally makes the query field
                // read as an inset collapses into the band and the editable field
                // becomes invisible. HC separates surfaces with BORDERS, so a border
                // is what must appear — and it has to contrast against the fill it is
                // drawn over, or it is decoration.
                if colors.field_bg == colors.bar_bg {
                    let rule = colors.well_rule.unwrap_or_else(|| {
                        panic!(
                            "{name}: well and band share a tone \
                             with nothing drawn to separate them"
                        )
                    });
                    assert!(
                        contrast(rule, colors.field_bg) >= 3.0,
                        "{name} well border must be visible against the well"
                    );
                } else {
                    assert_eq!(
                        colors.well_rule, None,
                        "{name}: a well the fill already separates needs no border"
                    );
                }
            });
        }
    }

    /// Installing and retracting the palette is what the host uses to decide whether
    /// to invalidate the strip cache, so "changed" has to mean changed.
    #[test]
    fn install_forced_chrome_reports_only_real_movement() {
        let previous = forced_chrome();
        let a = hc_fixtures::STOCK[0].1;
        let b = hc_fixtures::STOCK[2].1;
        assert!(install_forced_chrome(Some(a)), "off → on is a change");
        assert!(!install_forced_chrome(Some(a)), "same palette is not");
        assert!(install_forced_chrome(Some(b)), "a different HC scheme is");
        assert_eq!(forced_chrome(), Some(b));
        assert!(install_forced_chrome(None), "on → off is a change");
        assert!(!install_forced_chrome(None), "still off is not");
        assert_eq!(forced_chrome(), None);
        install_forced_chrome(previous);
    }

    /// A palette move that some OTHER reader consumed must still reach the re-sample
    /// that settles the windows. `AppRt::native_appearance_preferences` republishes
    /// the palette as a side effect and is called at window attach and at startup, so
    /// an HC-scheme switch landing between an attach and the settings `Wake` used to
    /// have its `install_forced_chrome` edge eaten by the attach — and the `Wake` then
    /// saw "nothing moved" and left every pre-existing window on the old palette.
    #[test]
    fn a_consumed_palette_edge_still_reaches_the_resample() {
        let previous = forced_chrome();
        let _ = crate::native_appearance::take_chrome_inputs_moved();

        // The window attach reads first and takes the edge.
        assert!(install_forced_chrome(Some(hc_fixtures::STOCK[0].1)));
        assert!(
            !install_forced_chrome(Some(hc_fixtures::STOCK[0].1)),
            "the edge is gone: a second install reports no movement"
        );
        // The re-sample runs later and must still be told to settle the windows.
        assert!(
            crate::native_appearance::take_chrome_inputs_moved(),
            "the palette move was swallowed by the non-resample reader"
        );
        assert!(
            !crate::native_appearance::take_chrome_inputs_moved(),
            "the latch is drained by its one consumer"
        );

        install_forced_chrome(previous);
        let _ = crate::native_appearance::take_chrome_inputs_moved();
    }

    /// The bit-packing that actually SHIPS. `install_forced_chrome`/`forced_chrome`
    /// resolve to a thread-local under `cfg(test)`, so without this the five-slot
    /// atomic codec — the one every real aterm runs — has no coverage at all, and a
    /// channel swap or a shift bug would pass the whole suite.
    #[test]
    fn the_shipping_palette_codec_round_trips_every_channel() {
        // Channel-distinct on purpose: a `[0,1,2] -> [2,1,0]` swap has to show.
        for c in [
            [0x00, 0x00, 0x00],
            [0xFF, 0xFF, 0xFF],
            [0x12, 0x34, 0x56],
            [0xFF, 0x00, 0x00],
            [0x00, 0xFF, 0x00],
            [0x00, 0x00, 0xFF],
        ] {
            assert_eq!(unpack(pack(c)), c, "{c:?} must survive the round trip");
        }
        assert_eq!(
            pack([0x12, 0x34, 0x56]),
            0x0012_3456,
            "packed as 0x00RRGGBB"
        );
        // The sentinel has to be unreachable from a real colour, or "no palette" and
        // white-on-white become the same 32 bits.
        assert!(
            (0..=0xFF).all(|v| pack([v, v, v]) != FORCED_ABSENT),
            "FORCED_ABSENT must not collide with any packed colour"
        );
    }

    /// THE PUBLICATION PROTOCOL, held to the order its doc promises: the gate slot
    /// closes FIRST, the four tail slots are written next, and the gate re-opens LAST
    /// carrying the real `window`. Read off [`publish_sequence`] because the ordering
    /// is unobservable once the (single-threaded) write has finished — the bug it
    /// guards was a loop that opened the gate with `window` before `btn_face` had
    /// been written, which no after-the-fact read can catch.
    #[test]
    fn the_palette_gate_slot_is_written_last() {
        let p = hc_fixtures::STOCK[1].1;
        let seq = publish_sequence(p);
        assert_eq!(
            seq[0],
            (0, FORCED_ABSENT),
            "the gate must CLOSE before any tail slot moves"
        );
        assert_eq!(
            seq[seq.len() - 1],
            (0, pack(p.window)),
            "the gate must re-open LAST, with the real window colour"
        );
        for (i, (slot, _)) in seq[1..seq.len() - 1].iter().enumerate() {
            assert_eq!(*slot, i + 1, "the tail writes slots 1..=4 in field order");
        }
    }

    /// The shipping codec, exercised on the real atomics: every field lands in its own
    /// slot, and the gate slot alone decides whether a palette is visible at all.
    ///
    /// The only test that touches [`FORCED_CHROME`] — production reads go through the
    /// `cfg(test)` thread-local, so nothing else in the suite can observe or disturb
    /// these atomics.
    #[test]
    fn published_forced_chrome_is_all_or_nothing() {
        assert_eq!(published_forced_chrome(), None, "starts absent");
        let mut last = None;
        for (name, palette) in hc_fixtures::STOCK {
            assert!(publish_forced_chrome(Some(palette)), "{name}: moved");
            assert_eq!(
                published_forced_chrome(),
                Some(palette),
                "{name}: every field round-trips through its own slot"
            );
            assert!(!publish_forced_chrome(Some(palette)), "{name}: idempotent");
            last = Some(palette);
        }
        // The gate alone gates: close it and the tail slots become invisible.
        FORCED_CHROME[0].store(FORCED_ABSENT, Ordering::Release);
        assert_eq!(
            published_forced_chrome(),
            None,
            "the gate slot alone decides whether a palette is visible"
        );
        assert!(
            publish_forced_chrome(last),
            "re-publishing re-opens the gate"
        );
        assert!(publish_forced_chrome(None), "…and retracting is a move");
        assert_eq!(published_forced_chrome(), None);
    }

    /// THE DOUBLE-HEADER GRAYS (frame audit): on Linux the band's base tone must
    /// be byte-identical to the CSD headerbar it sits under — `#303030` dark,
    /// `#EBEBEB` light — for EVERY theme of that appearance, not merely close.
    /// A near-miss (`#303135`, the old 0.16 blend on the default dark scheme)
    /// reads as two stacked headers with a tone break at their seam.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_band_base_tone_is_the_exact_csd_headerbar_gray() {
        assert_eq!(
            forced_chrome(),
            None,
            "theme-derived band tones are only defined with no OS-forced palette"
        );
        for name in aterm_types::scheme::builtin_names() {
            let scheme = aterm_types::scheme::builtin(name).expect("listed scheme exists");
            let parts = scheme.to_theme_parts();
            let theme = Theme {
                fg: parts.fg,
                bg: parts.bg,
                cursor: parts.cursor,
                selection: parts.selection,
            };
            let expected = if bg_is_light(rgb(theme.bg)) {
                CSD_HEADERBAR_LIGHT
            } else {
                CSD_HEADERBAR_DARK
            };
            assert_eq!(
                band_colors(theme).bar_bg,
                expected,
                "{name}: the band must sit on the CSD's own gray"
            );
        }
    }

    #[test]
    fn band_colors_meet_wcag_aa_on_every_builtin_scheme() {
        // This test measures aterm's OWN theme-derived tones, which exist only when
        // no OS palette is forcing chrome. Stated rather than assumed: the
        // thread-local default is `None`, and a future test on this worker that
        // leaked a palette would otherwise fail here with a baffling message.
        assert_eq!(
            forced_chrome(),
            None,
            "theme-derived band tones are only defined with no OS-forced palette"
        );
        for name in aterm_types::scheme::builtin_names() {
            let scheme = aterm_types::scheme::builtin(name).expect("listed scheme exists");
            let parts = scheme.to_theme_parts();
            let theme = Theme {
                fg: parts.fg,
                bg: parts.bg,
                cursor: parts.cursor,
                selection: parts.selection,
            };
            let colors = band_colors(theme);
            for (role, value) in [
                ("value", colors.value),
                ("warn", colors.warn),
                ("label", colors.label),
            ] {
                assert!(
                    contrast(value, colors.bar_bg) >= 4.5,
                    "{name} {role} must meet WCAG-AA"
                );
            }
            // The inset well carries the find query + its caret: both must clear AA
            // against the WELL's background, not the band's.
            assert!(
                contrast(colors.caret, colors.field_bg) >= 4.5,
                "{name} caret must meet WCAG-AA in the well"
            );
        }
    }
}
