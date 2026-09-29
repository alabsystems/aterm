// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared themed cells for compact in-grid chrome such as the find bar, config
//! notices, and native-tab backing rows. This is deliberately independent of any
//! status feature: callers provide the content and own its lifecycle.

#[cfg(test)]
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

use aterm_core::terminal::RenderCell;
use aterm_messages::ink::{AnsiHues, BarBase, ThemeInks};
use aterm_render::Theme;

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
/// is painted from while a High-Contrast scheme is active — the engine's
/// [`aterm_messages::ink::ForcedPalette`] (ruling 324), under the name every call site
/// here has always used. Stored in THEME byte order: the platform arm does the
/// COLORREF (`0x00BBGGRR`) swap on the way in.
pub(crate) type ForcedChrome = aterm_messages::ink::ForcedPalette;

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

/// On-theme tones for compact chrome bands — the engine's plain-RGB
/// [`aterm_messages::ink::BandInks`] (ruling 324), under the name every call site here
/// has always used. The band's painter names its word inks by SLOT, each one of these
/// (`BandInks::slot`): `Ink::Label` → `label`, `Value` → `value`, `Warn` → `warn`,
/// `Error` → `error`, `Accent` → `accent`.
pub(crate) type BandColors = aterm_messages::ink::BandInks;

// The chrome colour helpers live in the engine (ruling 324: one implementation for the
// band, the tab strip, the find bar, the notices, Settings and presence), re-exported
// under the paths their call sites have always used.
#[cfg(test)]
pub(crate) use aterm_messages::ink::{
    HOVER_RISE, PRIMARY_HOVER_LIFT, TRACK_TINT, ensure_contrast_either, hover_ink,
};
pub(crate) use aterm_messages::ink::{contrast, ensure_contrast, forced_ink, mix3, warn_mark};

/// A packed `0x00RRGGBB` theme colour as sRGB bytes.
fn rgb(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
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
/// to the forced mapping (`BandInks::forced`) — the OS owns chrome colour then, and a
/// theme-derived blend under an OS-palette caption is the seam that made HC support
/// incoherent.
///
/// LINUX: the band's base tone is NOT theme-derived — it is the exact adwaita
/// headerbar gray of the CSD titlebar directly above it (see
/// [`CSD_HEADERBAR_DARK`]/[`CSD_HEADERBAR_LIGHT`]), picked dark/light by the same
/// [`crate::tab_bar::bg_is_light`] classifier every other chrome surface uses. The inks below are
/// contrast-floored against whatever surface they land on, so a theme's fg keeps
/// clearing AA on the fixed gray exactly as it did on the blend.
pub(crate) fn band_colors(theme: Theme) -> BandColors {
    band_colors_with(theme, None)
}

/// The theme's ANSI blue and cyan (slots 4 and 6), which a band meter
/// borrows when the cursor is near-grey (ruling 250).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MeterAnsi {
    pub blue: [u8; 3],
    pub cyan: [u8; 3],
}

impl MeterAnsi {
    /// A scheme's blue and cyan (the tests' builtin grounds; the app reads
    /// its configured palette, [`Self::of_palette`]).
    #[cfg(test)]
    pub(crate) fn of_scheme(s: &aterm_types::ColorScheme) -> Self {
        let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
        Self {
            blue: rgb(s.ansi[4]),
            cyan: rgb(s.ansi[6]),
        }
    }

    /// A terminal palette's blue and cyan.
    pub(crate) fn of_palette(p: &aterm_types::ColorPalette) -> Self {
        let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
        Self {
            blue: rgb(p.get(4)),
            cyan: rgb(p.get(6)),
        }
    }
}

/// What the message band is painted from: the chrome theme and, when the
/// host knows it, the terminal palette's blue and cyan (ruling 250). A bare
/// [`Theme`] converts with no palette: its meter keeps the cursor accent.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BandPalette {
    pub theme: Theme,
    pub ansi: Option<MeterAnsi>,
}

impl From<Theme> for BandPalette {
    fn from(theme: Theme) -> Self {
        Self { theme, ansi: None }
    }
}

impl BandPalette {
    /// The band's colours for this palette.
    pub(crate) fn colors(self) -> BandColors {
        band_colors_with(self.theme, self.ansi)
    }
}

/// [`band_colors`], with the terminal palette's blue and cyan for the meter
/// (ruling 250): where the theme's cursor is near-grey, the meter's fill is
/// the blue — or the cyan, where only the cyan carries the band's words —
/// chosen by `aterm_messages::palette::MeterHue`, the one host-agnostic rule.
/// A hue the 3:1 floor would darken into brown keeps its own pastel and a
/// darker edge instead (ruling 264, `aterm_messages::palette::keeps_pastel`).
///
/// A pure MAPPING (ruling 324): the latch's forced palette, or the engine's
/// derivation (`BandInks::derive`) from the theme's background, foreground and
/// cursor, the ANSI pair, and this platform's band base.
pub(crate) fn band_colors_with(theme: Theme, ansi: Option<MeterAnsi>) -> BandColors {
    if let Some(hc) = forced_chrome() {
        return BandColors::forced(hc);
    }
    BandColors::derive(
        ThemeInks {
            bg: rgb(theme.bg),
            fg: rgb(theme.fg),
            cursor: rgb(theme.cursor),
        },
        ansi.map(|a| AnsiHues {
            blue: a.blue,
            cyan: a.cyan,
        }),
        PLATFORM_BAR_BASE,
    )
}

/// Where this platform's band ground comes from: on Linux the CSD headerbar grey
/// ([`CSD_HEADERBAR_DARK`]/[`CSD_HEADERBAR_LIGHT`]), elsewhere the theme blend.
#[cfg(target_os = "linux")]
const PLATFORM_BAR_BASE: BarBase = BarBase::Fixed {
    light: CSD_HEADERBAR_LIGHT,
    dark: CSD_HEADERBAR_DARK,
};
#[cfg(not(target_os = "linux"))]
const PLATFORM_BAR_BASE: BarBase = BarBase::Blend;

/// The PRESENCE tones: the rim's three hues and the story dot — the engine's
/// (`aterm_messages::presence::PresenceTones`, design ruling 348), under the
/// name the host has always used.
pub(crate) use aterm_messages::presence::PresenceTones;

/// The presence hues as TEXT inks on the band — the engine's
/// (`aterm_messages::presence::inks`): [`presence_tones`]' hues held to the AA
/// 4.5:1 text floor against the band they print on. What
/// [`crate::message_band::paint_presence_row`] paints the hand and phase slots
/// in; the rim keeps the 3:1 tones.
pub(crate) fn presence_inks(theme: Theme) -> PresenceTones {
    aterm_messages::presence::inks(presence_tones(theme), band_colors(theme).bar_bg)
}

/// The presence tones for `theme` on its band — or, under an OS-forced chrome
/// palette (this host's High Contrast latch), the HC vocabulary's own words
/// for them — the engine's (`aterm_messages::presence::tones`).
pub(crate) fn presence_tones(theme: Theme) -> PresenceTones {
    aterm_messages::presence::tones(rgb(theme.bg), &band_colors(theme), forced_chrome())
}

/// Build one render cell for compact chrome — the renderer's (`aterm_render::band`,
/// design ruling 331: the one table the native window and the web module share).
pub(crate) use aterm_render::band::cell;

/// A theme-derived blank band cell with a top seam.
#[must_use]
pub(crate) fn blank_cell(theme: Theme) -> RenderCell {
    let colors = band_colors(theme);
    cell(' ', colors.label, colors.bar_bg, false, true)
}

/// CLOSE a band's content-facing TOP edge, in place, across the WHOLE row and in
/// ONE tone. The top edge's counterpart to [`crate::tab_bar::seal_strip_bottom`].
///
/// WHY A BAND CANNOT JUST STAMP THE SEAM WHEN IT BLANKS THE ROW. It does — via
/// [`blank_cell`] or `settings::blank_row` — and then it paints its words over it.
/// Every writer in this crate builds the cell it writes FROM SCRATCH
/// (`settings::write_str` calls [`cell`] with `seam: false`) and no chrome text
/// carries an overline of its own, so the rule survives exactly where the band
/// happens to have no words. On screen that is not a rule: a title row comes out as
/// three disconnected stubs — the left margin, the gap before the right-aligned
/// aside, the right margin — which reads as rendering debris rather than as the
/// band's boundary. Drawn HERE, across the FINISHED row after every write, so no
/// future field added to a band can chip it again.
///
/// `ink` is the seam's OWN tone rather than each cell's `fg`, because a title row
/// deliberately carries more than one: a warn-coloured title beside a
/// label-coloured aside. A rule left to the cells beneath it would run bright for
/// the title's twenty cells and dim for the rest — a seam is a STRUCTURAL edge and
/// must not brighten under the words it happens to pass beneath.
pub(crate) fn seal_band_top(row: &mut [RenderCell], ink: [u8; 3]) {
    for cell in row.iter_mut() {
        cell.overline = true;
        cell.overline_color = Some(ink);
    }
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
                // The meter is ONE palette ink on the WINDOW well (ruling
                // 137: the fill is HIGHLIGHT under High Contrast, whatever
                // the row's severity), and the words on it HIGHLIGHTTEXT.
                assert_eq!(colors.accent, palette.highlight, "{name}");
                assert_eq!(colors.meter_track, palette.window, "{name}");
                assert_eq!(
                    colors.on_accent,
                    forced_ink(palette.highlight_text, palette.highlight),
                    "{name}"
                );
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
                // The message band's hovered capsule is HIGHLIGHT under HC, and
                // its ink HIGHLIGHTTEXT floored on it — the selected-surface pair
                // the scheme itself pairs.
                assert_eq!(colors.capsule_hover, palette.highlight, "{name}");
                assert!(
                    contrast(colors.capsule_hover_ink, colors.capsule_hover) >= 4.5,
                    "{name} hovered capsule ink must meet WCAG-AA under High Contrast"
                );
                assert_eq!(colors.capsule_primary_hover, palette.highlight, "{name}");
                assert!(
                    contrast(
                        colors.capsule_primary_hover_ink,
                        colors.capsule_primary_hover
                    ) >= 4.5,
                    "{name} hovered Primary ink must meet WCAG-AA under High Contrast"
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
            let expected = if crate::tab_bar::bg_is_light(rgb(theme.bg)) {
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

    /// THE GLINT'S FLOOR (design §10.7, amended by rulings 137, 158 and
    /// 242): the fill is the theme's cursor accent (or `warn` on a Warn/Error
    /// row) — ruling 55's "cursor trail theme", floored against the band as
    /// THEIRS floors it — and the glint is ONE fixed perceptual step of that
    /// fill AWAY from the ink its words wear on it, so the words only gain
    /// contrast under it (never a flip) and the hot head clears 3:1 on the
    /// track wherever the fill itself does. The tail's gradient and the warn
    /// wash are decorative transients, and exempt.
    fn meter_floors(name: &str, colors: &BandColors) {
        for fill in [colors.accent, colors.warn] {
            let inks = crate::message_band::MeterInks::of(colors, fill);
            // The HEAD is a comet's: its inks are the comet's (a bar has none).
            let comet = crate::message_band::MeterInks::comet(colors, fill).0;
            let head = crate::message_band::tone_rgb(aterm_messages::Tone::HEAD, &comet);
            let words = crate::message_band::fill_ink(colors, fill);
            // The glint IS the fill's one step away from those words.
            assert_eq!(
                inks.glint,
                crate::message_band::glint_step(fill, words, false),
                "{name}"
            );
            // One of black and white always clears AA on the fill, so the
            // floor that may cross to the far one always meets it.
            assert!(
                contrast(ensure_contrast_either(colors.label, fill, 4.5), fill) >= 4.5,
                "{name}: no ink reaches AA on {fill:?}"
            );
            assert!(
                contrast(words, inks.glint) >= contrast(words, fill) - 0.02,
                "{name}: the glint of {fill:?} must never cost its words contrast: \
                 {:.2}:1 on the glint, {:.2}:1 on the fill",
                contrast(words, inks.glint),
                contrast(words, fill)
            );
            if contrast(comet.fill, colors.meter_track) >= 3.0 {
                assert!(
                    contrast(head, colors.meter_track) >= 3.0,
                    "{name}: the comet head of {fill:?} must clear 3:1 against the track: \
                     {head:?} on {:?}",
                    colors.meter_track
                );
            }
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
            // THE MESSAGE BAND'S CAPSULES (design §2.2, §7.3): the hovered
            // chip's label clears AA on the hover fill, and the Primary chip's
            // ink — `bar_bg` on the `accent` fill — clears the 3:1 non-text
            // floor the meter's fill is held to (the same surface, inverted).
            assert!(
                contrast(colors.capsule_hover_ink, colors.capsule_hover) >= 4.5,
                "{name} hovered capsule label must meet WCAG-AA"
            );
            assert_eq!(
                colors.capsule_hover_ink,
                hover_ink(colors.value, colors.capsule_hover),
                "{name}: the hovered label is the value ink lifted on the risen fill"
            );
            assert!(
                contrast(colors.bar_bg, colors.accent) >= 3.0,
                "{name} Primary capsule ink must clear 3:1 on the accent fill"
            );
            // The indicator wears the cursor accent (ruling 137 — ruling 55's
            // "cursor trail theme" wins over the branch's own progress hue);
            // its glint and hot head keep their floor.
            meter_floors(name, &colors);
            // …and the LIT Primary keeps that floor with the same ink, and
            // is a visible step off the resting accent wherever the theme's
            // ink is not the accent itself (review, 2026-09-22: a hovered
            // `Apply now` must read as lit, never as disabled).
            assert_eq!(colors.capsule_primary_hover_ink, colors.bar_bg, "{name}");
            assert!(
                contrast(
                    colors.capsule_primary_hover_ink,
                    colors.capsule_primary_hover
                ) >= 3.0,
                "{name} lit Primary ink must clear 3:1 on the lit fill"
            );
            if colors.accent != rgb(theme.fg) {
                assert_ne!(
                    colors.capsule_primary_hover, colors.accent,
                    "{name}: the lit Primary must move off the resting accent"
                );
                // …and UNMISTAKABLY (ruling 20): at least `PRIMARY_HOVER_LIFT`
                // of the way from the accent to the ink on every channel, up
                // to the rounding of two `mix3` calls — the 3:1 floor can only
                // push it further from the band, never back toward the accent
                // (the floor is measured against `bar_bg`, and the assertion
                // above pins that the floor held).
                let lifted = mix3(colors.accent, rgb(theme.fg), PRIMARY_HOVER_LIFT);
                for (c, &want) in lifted.iter().enumerate() {
                    let (a, f, lit, want) = (
                        f32::from(colors.accent[c]),
                        f32::from(rgb(theme.fg)[c]),
                        f32::from(colors.capsule_primary_hover[c]),
                        f32::from(want),
                    );
                    // Progress along the accent→ink axis, signed by that axis.
                    let toward = (f - a).signum();
                    assert!(
                        (lit - a) * toward + 1.0 >= (want - a) * toward,
                        "{name}: channel {c} of the lit Primary ({lit}) is short of \
                         {PRIMARY_HOVER_LIFT} of the way from the accent ({a}) to \
                         the ink ({f}): wanted {want}"
                    );
                }
            }
            assert_ne!(
                colors.capsule_primary_hover, colors.capsule_hover,
                "{name}: the lit Primary is not the grey hover"
            );
            // …and the hover fill is a REAL step off the band on every scheme
            // where the ink allows one, so a hovered chip is visibly raised.
            if contrast(colors.value, mix3(colors.bar_bg, rgb(theme.fg), 0.05)) >= 4.5 {
                assert_ne!(
                    colors.capsule_hover, colors.bar_bg,
                    "{name}: the hover fill must move off the band"
                );
            }
        }
    }

    /// THE FAULT WASH STAYS IN ITS HULL (ruling 244, which withdraws the
    /// OkLCh warming of ruling 156): a Fault echo hands the fill's coverage
    /// to warn as a premultiplied cross-fade — `fill · (1 − u)` of the fill
    /// and `fill · u` of warn, in linear light — so on every builtin scheme,
    /// on both fills a row can wear (the accent, and warn itself), and on
    /// every stock High Contrast palette, every step of it lies inside the
    /// track–fill–warn triangle (no lime between a green fill and amber, no
    /// magenta between blue and ochre), and its ends are the fill (or the
    /// track) and warn themselves.
    #[test]
    fn the_fault_wash_stays_inside_the_track_fill_warn_hull() {
        let check = |name: &str, colors: &BandColors| {
            for fill_ink in [colors.accent, colors.warn] {
                let inks = crate::message_band::MeterInks::of(colors, fill_ink);
                let corners = [inks.track, inks.fill, inks.warn];
                for cover in [255u8, 128, 1] {
                    for u in 0..=255u16 {
                        let w = u8::try_from((u16::from(cover) * u + 127) / 255).unwrap();
                        let t = aterm_messages::Tone {
                            fill: cover - w,
                            lift: 0,
                            warn: w,
                        };
                        let rgb = crate::message_band::tone_rgb(t, &inks);
                        let d = crate::message_band::hull_distance(rgb, corners);
                        assert!(d < 0.01, "{name}: {t:?} → {rgb:?} is {d:.4} outside");
                    }
                }
                let at = |fill, warn| {
                    crate::message_band::tone_rgb(
                        aterm_messages::Tone {
                            fill,
                            lift: 0,
                            warn,
                        },
                        &inks,
                    )
                };
                assert_eq!(at(255, 0), inks.fill, "{name}");
                assert_eq!(at(0, 255), inks.warn, "{name}");
                assert_eq!(at(0, 0), inks.track, "{name}");
            }
        };
        for name in aterm_types::scheme::builtin_names() {
            let scheme = aterm_types::scheme::builtin(name).expect("listed scheme exists");
            let parts = scheme.to_theme_parts();
            let theme = Theme {
                fg: parts.fg,
                bg: parts.bg,
                cursor: parts.cursor,
                selection: parts.selection,
            };
            check(name, &band_colors(theme));
        }
        for (name, palette) in hc_fixtures::STOCK {
            hc_fixtures::with_forced(palette, || check(name, &band_colors(Theme::default())));
        }
    }
}
