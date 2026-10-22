// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The band's colours.
//!
//! [`crate::paint`] decides the band's structure: which character in which cell, in
//! which ink slot or chip form, every surface tone as a fraction of the row. This module
//! resolves that structure to colours (the contrast floors, the crisp ink over a fill,
//! the side holds, the glint's rail, the pastel edge line, the chip inks, an echo's
//! fade) over a plain-RGB palette, [`BandInks`], derived from a theme's background,
//! foreground and cursor ([`BandInks::derive`]) or from an OS-forced High Contrast
//! palette ([`BandInks::forced`]). [`paint_band`] paints and resolves a whole band; the
//! host maps each [`InkedCell`] onto its own cell type and each [`RowRaster`] onto its
//! renderer's raster.
//!
//! # Every bit kept
//!
//! The floating point here keeps the macOS host's arithmetic to the bit: fused
//! multiply-adds stay fused, unfused sums stay unfused, and [`contrast`] is the WCAG
//! ratio in `aterm_types::Rgb::contrast`'s own arithmetic, the engine's one ratio (the
//! palette's picks read it too). The side decisions (which end of the scale a word or a
//! fill sits toward) read the fused [`luminance`]. Where a pedantic lint asks for a
//! rewrite that could move a bit, the code keeps its spelling and says why.

use crate::animate::{BandMotion, FineTone, Tone};
use crate::glass::{CapsuleRole, Presentation};
use crate::paint::{
    self, ChipFace, ChipForm, Face, Fill, Geometry, Hover, Icon, Ink, RowPaint, RowSurface,
};
pub use crate::palette::luminance;
use crate::palette::{enc, keeps_pastel, lin, oklch, outline_borrows_blue, pastel_edge};
use crate::tokens::{AuthoredChrome, ChromeSeeds, ChromeTokens};

// ---- The palette ------------------------------------------------------------

/// On-theme tones for the band and its compact chrome kin (the find bar, the
/// config notices), as plain sRGB bytes. The painter names its word inks by
/// slot, each one of these (`BandInks::slot`): `Ink::Label` → [`Self::label`],
/// `Value` → [`Self::value`], `Warn` → [`Self::warn`], `Error` →
/// [`Self::error`], `Accent` → [`Self::accent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BandInks {
    /// The band's own ground: the terminal background a step toward its ink
    /// (0.10 on a light theme, 0.16 on a dark one), or the platform's fixed
    /// headerbar grey ([`BarBase::Fixed`]); `COLOR_BTNFACE` under an
    /// OS-forced palette.
    pub bar_bg: [u8; 3],
    /// The band's secondary ink (an excerpt, the stats, a blank cell's ink,
    /// the find panel's hint row), held to the same AA floor as
    /// [`Self::value`]; `COLOR_WINDOWTEXT` under an OS-forced palette.
    pub label: [u8; 3],
    /// The band's words: the theme's foreground floored to AA on
    /// [`Self::bar_bg`]; `COLOR_WINDOWTEXT` under an OS-forced palette.
    pub value: [u8; 3],
    /// A warning's words and glyph: the theme's ANSI yellow (or a fixed fallback)
    /// floored to AA on the band and at least [`STATUS_LIGHTNESS_GAP`] lighter than
    /// [`Self::error`]; `COLOR_WINDOWTEXT` under an OS-forced palette.
    pub warn: [u8; 3],
    /// An error row's words and glyph: the theme's ANSI red (or a fixed fallback)
    /// floored to AA on the band like [`Self::warn`], still a colour after the
    /// floor and a lightness step below warn, so `✕ Tests failed` and
    /// `⚠ Misspelled setting` differ in more than glyph and hue. The one ink
    /// under an OS-forced palette, as `warn` is.
    pub error: [u8; 3],
    /// Background of an editable well inset in the band (the find bar's query field):
    /// the terminal's own background, so the well reads as recessed and `value` text in
    /// it keeps the terminal's own contrast rather than the band's smaller one.
    pub field_bg: [u8; 3],
    /// Text caret drawn in that well: the theme's cursor colour,
    /// contrast-floored against `field_bg` so it stays visible.
    pub caret: [u8; 3],
    /// A drawn border for the well where its fill cannot carry the boundary on its
    /// own: `Some(ink)` exactly when `field_bg == bar_bg`. Every stock Windows
    /// High-Contrast scheme sets `COLOR_WINDOW == COLOR_BTNFACE`, and HC separates
    /// surfaces by borders. `None` on every theme-derived scheme, where the 0.10/0.16
    /// blend already makes the well an inset.
    pub well_rule: Option<[u8; 3]>,
    /// The band's content-facing edge (the hairline over a find panel or a
    /// banner): [`Self::label`] unless the theme authors a `border_color`,
    /// which is floored on the band ([`crate::tokens::ChromeTokens::seam_on`]).
    /// `COLOR_WINDOWTEXT` under an OS-forced palette, like `label`.
    pub seam: [u8; 3],
    /// The fill of a determinate meter drawn in the band (the message band's
    /// full-row meter): the theme's cursor accent, contrast-floored against
    /// [`Self::bar_bg`] so a pale cursor on a pale band still reads as a
    /// fill. Under an OS-forced palette it is `COLOR_HIGHLIGHT`, what a native
    /// Win32 progress bar paints its fill with under High Contrast.
    pub accent: [u8; 3],
    /// The fill a message-band meter wears, floored to 3:1 on the band: [`Self::accent`]
    /// unless the cursor is near-grey ([`crate::palette`]), where the bar borrows the
    /// theme's ANSI blue, or its cyan where only cyan carries the band's words
    /// ([`BandInks::derive`]). Where the floor would darken the hue into brown
    /// ([`crate::palette::keeps_pastel`], e.g. Catppuccin Latte's rosewater), the fill is
    /// the theme's own pastel and [`Self::meter_edge`] draws its boundary.
    /// `COLOR_HIGHLIGHT` under an OS-forced palette.
    pub meter: [u8; 3],
    /// The darker edge line a pastel fill ends in: the pastel deepened until
    /// it stands 3:1 from [`Self::meter_track`]
    /// ([`crate::palette::pastel_edge`]), drawn over the fill's last pixels by
    /// the row's raster ([`RowRaster::line`]). `None` on every fill the 3:1
    /// floor already carries (every built-in scheme but Catppuccin Latte) and
    /// under an OS-forced palette.
    pub meter_edge: Option<[u8; 3]>,
    /// The track a meter's unfilled remainder is drawn on: [`Self::bar_bg`]
    /// mixed [`TRACK_TINT`] toward [`Self::meter`], so the empty part of a
    /// metered row reads as the bar's own channel and never as a chip's ground
    /// ([`Self::chip_ground`]). Under an OS-forced palette it is the document
    /// surface (`COLOR_WINDOW`), the HC vocabulary's "well".
    pub meter_track: [u8; 3],
    /// A resting Secondary chip's fill on an unmetered row: a step off
    /// [`Self::bar_bg`] toward the ink. Under an OS-forced palette
    /// `COLOR_WINDOW`, like the track.
    pub chip_ground: [u8; 3],
    /// The hue a metered row's outlined Primary rings and labels in:
    /// [`Self::meter`], unless that hue floored to AA for its label reads
    /// brown ([`crate::palette::reads_brown`], e.g. Catppuccin Latte's
    /// rosewater), where the outline borrows the theme's ANSI blue. The fill
    /// keeps [`Self::meter`]. `COLOR_HIGHLIGHT` under an OS-forced palette.
    pub ring: [u8; 3],
    /// The fill of the message band's capsule under the pointer:
    /// [`Self::bar_bg`] moved toward the ink, at least 0.30 and on until it
    /// stands [`HOVER_RISE`]:1 from a resting chip's [`Self::chip_ground`],
    /// while [`Self::capsule_hover_ink`] still clears WCAG AA on it. Under an
    /// OS-forced palette it is `COLOR_HIGHLIGHT`: HC's own word for "the
    /// thing the pointer is on".
    pub capsule_hover: [u8; 3],
    /// The ink on [`Self::capsule_hover`]: [`Self::value`] theme-derived (full
    /// contrast, whatever role the chip's resting ink had), lifted toward its
    /// own end where the risen fill needs it for AA ([`hover_ink`]);
    /// `COLOR_HIGHLIGHTTEXT` under an OS-forced palette.
    pub capsule_hover_ink: [u8; 3],
    /// The fill of the message band's Primary capsule under the pointer:
    /// [`Self::accent`] moved [`PRIMARY_HOVER_LIFT`] toward the theme's ink,
    /// lit, not dimmed (a grey hover would read as disabled). Floored so
    /// [`Self::capsule_primary_hover_ink`] keeps the 3:1 non-text floor the
    /// accent itself is held to. `COLOR_HIGHLIGHT` under an OS-forced
    /// palette, like every hovered chip there.
    pub capsule_primary_hover: [u8; 3],
    /// The ink on [`Self::capsule_primary_hover`]: [`Self::bar_bg`]
    /// theme-derived (the Primary's resting ink, so only the fill moves), and
    /// `COLOR_HIGHLIGHTTEXT` under an OS-forced palette.
    pub capsule_primary_hover_ink: [u8; 3],
    /// What the lit Primary moves [`PRIMARY_HOVER_LIFT`] toward: the theme's
    /// ink. The band lifts the chip from the accent it actually wears (the
    /// accent deepened to AA for its label), so a deepened rest and the lit
    /// form stay a whole lift apart. `COLOR_HIGHLIGHT` under an OS-forced
    /// palette, unread there.
    pub primary_lift_toward: [u8; 3],
    /// The ink a word takes on the [`Self::accent`] fill of a metered row under an
    /// OS-forced palette: `COLOR_HIGHLIGHTTEXT`, the system's pairing for
    /// `COLOR_HIGHLIGHT`. Theme-derived it is [`Self::bar_bg`], unread: the band floors
    /// each word's own ink against the fill instead.
    pub on_accent: [u8; 3],
}

/// The OS-forced chrome palette: the five Win32 system colours every chrome
/// surface is painted from while a High-Contrast scheme is active, and the
/// five CSS `forced-colors` system colours a browser host reads (`Canvas`,
/// `CanvasText`, `Highlight`, `HighlightText`, `ButtonFace`), which map onto
/// them one to one.
///
/// Stored as plain RGB triples in theme byte order: the platform arm does the
/// COLORREF (`0x00BBGGRR`) swap on the way in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ForcedPalette {
    /// `COLOR_WINDOW`, the document / editable-field surface: the find bar's
    /// inset well and the tab strip's hover wash.
    pub window: [u8; 3],
    /// `COLOR_WINDOWTEXT`, the one ink. An HC palette has no secondary text
    /// tone (dimming text is what an HC user opted out of): the band's label,
    /// value, warn, error and inactive-tab roles all collapse onto this colour
    /// and let weight (bold) carry the hierarchy.
    pub window_text: [u8; 3],
    /// `COLOR_HIGHLIGHT`, the selected surface: the active tab chip, the `↻`
    /// update CTA, and the accent rule.
    pub highlight: [u8; 3],
    /// `COLOR_HIGHLIGHTTEXT` — ink on [`Self::highlight`].
    pub highlight_text: [u8; 3],
    /// `COLOR_BTNFACE`, the control-face surface every chrome band is painted
    /// on. Win32's own split is document = `WINDOW`, control = `BTNFACE`; the
    /// strip and the find/notice bands are controls, the find bar's query
    /// field is a document. Every stock HC scheme sets the two equal, so they
    /// read as one surface separated by borders.
    pub btn_face: [u8; 3],
}

/// The theme colours the band is derived from, as sRGB bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ThemeInks {
    /// The terminal background.
    pub bg: [u8; 3],
    /// The terminal foreground.
    pub fg: [u8; 3],
    /// The cursor: the meter's hue.
    pub cursor: [u8; 3],
}

/// The theme's ANSI hues the band borrows: blue and cyan (slots 4 and 6)
/// for a meter when the cursor is near-grey, red and yellow (slots 1 and 3)
/// as the seeds of its error and warn inks, and red and green (slot 2) as the
/// chrome's danger and success hues ([`crate::tokens::ChromeTokens`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AnsiHues {
    /// ANSI slot 1.
    pub red: [u8; 3],
    /// ANSI slot 2.
    pub green: [u8; 3],
    /// ANSI slot 3.
    pub yellow: [u8; 3],
    /// ANSI slot 4.
    pub blue: [u8; 3],
    /// ANSI slot 6.
    pub cyan: [u8; 3],
}

/// Where the band's own ground comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BarBase {
    /// The terminal background blended toward its ink: 0.10 on a light
    /// theme, 0.16 on a dark one.
    Blend,
    /// A platform's fixed header tone the band sits directly under (Linux's
    /// client-side headerbar grey), picked light or dark by [`bg_is_light`].
    Fixed {
        /// On a light theme.
        light: [u8; 3],
        /// On a dark theme.
        dark: [u8; 3],
    },
}

/// The band's fallback warn hue on a light ground, where the theme's ANSI pair
/// cannot seed it ([`status_inks`]).
const WARN_LIGHT: [u8; 3] = [0x9A, 0x67, 0x00];
/// …on a dark ground.
const WARN_DARK: [u8; 3] = [0xF1, 0xFA, 0x8C];
/// The band's fallback error hue on a light ground.
const ERROR_LIGHT: [u8; 3] = [0xCF, 0x22, 0x2E];
/// …on a dark ground.
const ERROR_DARK: [u8; 3] = [0xFF, 0x55, 0x55];

/// The least `OkLCh` lightness by which the band's warn ink sits above its
/// error ink: warn lifted on a band whose words are light, error deepened on
/// one whose words are dark. Hue alone is the cue a colour-weak reader loses,
/// so the two never share a lightness.
pub const STATUS_LIGHTNESS_GAP: f64 = 0.10;

/// The least `OkLab` distance between the band's warn and error inks. A
/// theme's own pair that the floor leaves closer than this (two warm hues a
/// short way apart, or a brown yellow beside a red) gives way to the fixed
/// pair.
pub const STATUS_DISTANCE: f64 = 0.12;

/// The seed of a status ink (warn or error): the theme's own ANSI slot where
/// it carries a colour (`OkLCh` chroma at least
/// [`crate::palette::BORROW_CHROMA`], the line a meter borrows a hue at), so
/// a Solarized or Nord band warns in its own yellow and red; otherwise the
/// fixed fallback for the ground's polarity.
fn status_seed(slot: Option<[u8; 3]>, fallback: [u8; 3]) -> [u8; 3] {
    slot.filter(|c| crate::palette::chroma(*c) >= crate::palette::BORROW_CHROMA)
        .unwrap_or(fallback)
}

/// The band's warn and error inks, `(warn, error)`.
///
/// Each seed ([`status_seed`]) is floored to AA on `bar_bg` by moving its
/// `OkLCh` lightness at constant hue and chroma ([`lightness_floor`]): an
/// sRGB mix toward white washes a red into a grey-pink and leaves warn and
/// error on one contrast line, differing in hue alone. The pair is then
/// pulled [`STATUS_LIGHTNESS_GAP`] apart in lightness, always away from the
/// band, so neither loses contrast. The result is judged, not the seed: where
/// the theme's floored error has no colour left (chroma under
/// [`crate::palette::NEAR_GREY_CHROMA`]) or the pair stands closer than
/// [`STATUS_DISTANCE`], the fixed pair for the ground's polarity is floored
/// the same way instead.
fn status_inks(ansi: Option<AnsiHues>, bar_bg: [u8; 3], light: bool) -> ([u8; 3], [u8; 3]) {
    let (warn_fallback, error_fallback) = if light {
        (WARN_LIGHT, ERROR_LIGHT)
    } else {
        (WARN_DARK, ERROR_DARK)
    };
    let theirs = status_pair(
        status_seed(ansi.map(|a| a.yellow), warn_fallback),
        status_seed(ansi.map(|a| a.red), error_fallback),
        bar_bg,
    );
    let (warn, error) = theirs;
    if crate::palette::chroma(error) >= crate::palette::NEAR_GREY_CHROMA
        && crate::palette::oklab_distance(warn, error) >= STATUS_DISTANCE
    {
        theirs
    } else {
        status_pair(warn_fallback, error_fallback, bar_bg)
    }
}

/// One warn/error pair floored to AA on `bar_bg` and held
/// [`STATUS_LIGHTNESS_GAP`] apart (see [`status_inks`]).
fn status_pair(warn: [u8; 3], error: [u8; 3], bar_bg: [u8; 3]) -> ([u8; 3], [u8; 3]) {
    const AA: f64 = 4.5;
    let up = lifts(bar_bg);
    let warn = lightness_floor(warn, bar_bg, AA, up);
    let error = lightness_floor(error, bar_bg, AA, up);
    let (lw, le) = (oklch(warn).0, oklch(error).0);
    if up && lw < le + STATUS_LIGHTNESS_GAP {
        (
            at_lightness(warn, (le + STATUS_LIGHTNESS_GAP).min(1.0)),
            error,
        )
    } else if !up && le > lw - STATUS_LIGHTNESS_GAP {
        (
            warn,
            at_lightness(error, (lw - STATUS_LIGHTNESS_GAP).max(0.0)),
        )
    } else {
        (warn, error)
    }
}

/// Whether an ink on `bg` reads by being lighter than it: white reaches at
/// least the contrast black does.
fn lifts(bg: [u8; 3]) -> bool {
    contrast(WHITE, bg) >= contrast(BLACK, bg)
}

/// `c` at `OkLCh` lightness `l`, its hue and chroma kept (chroma given up
/// only to stay in gamut, [`from_oklch`]).
fn at_lightness(c: [u8; 3], l: f64) -> [u8; 3] {
    let (_, ch, h) = oklch(c);
    from_oklch(l, ch, h)
}

/// `c` floored to `target`:1 on `bg` by the least move of its `OkLCh`
/// lightness, up (`up`) or down, at constant hue and chroma: a red stays a
/// red rather than washing to pink. Where even the end of that walk falls
/// short (a mid-grey ground), [`ensure_contrast`] finishes the job.
fn lightness_floor(c: [u8; 3], bg: [u8; 3], target: f64, up: bool) -> [u8; 3] {
    if contrast(c, bg) >= target {
        return c;
    }
    let (l, ch, h) = oklch(c);
    let (mut short, mut far) = (l, if up { 1.0 } else { 0.0 });
    for _ in 0..24 {
        let mid = f64::midpoint(short, far);
        if contrast(from_oklch(mid, ch, h), bg) >= target {
            far = mid;
        } else {
            short = mid;
        }
    }
    ensure_contrast(from_oklch(far, ch, h), bg, target)
}

impl BandInks {
    /// The palette's ink for one of the painter's word-ink slots: each maps
    /// 1:1 onto a [`BandInks`] ink.
    #[inline]
    #[must_use]
    pub(crate) const fn slot(&self, ink: Ink) -> [u8; 3] {
        match ink {
            Ink::Label => self.label,
            Ink::Value => self.value,
            Ink::Warn => self.warn,
            Ink::Error => self.error,
            Ink::Accent => self.accent,
        }
    }

    /// Appearance-aware, theme-derived band tones with WCAG-AA text contrast,
    /// and the meter's hue: where the theme's cursor is near-grey and the host
    /// knows the terminal palette's blue and cyan (`ansi`), the meter's fill
    /// is the blue, or the cyan where only the cyan carries the band's words,
    /// chosen by [`crate::palette::MeterHue`]. A hue the 3:1 floor would darken into brown
    /// keeps its own pastel and a darker edge instead
    /// ([`crate::palette::keeps_pastel`]). `base` says where the band's ground
    /// comes from ([`BarBase`]).
    ///
    /// [`Self::from_tokens`] with nothing authored.
    #[must_use]
    pub fn derive(theme: ThemeInks, ansi: Option<AnsiHues>, base: BarBase) -> Self {
        Self::from_tokens(
            theme,
            &ChromeTokens::derive(&ChromeSeeds {
                bg: theme.bg,
                fg: theme.fg,
                cursor: theme.cursor,
                ansi,
                authored: AuthoredChrome::NONE,
                system_accent: None,
                base,
            }),
        )
    }

    /// The band tones over the theme's chrome tokens: the band's ground is
    /// [`ChromeTokens::bar_bg`], its accent and meter wear
    /// [`ChromeTokens::accent`] floored to 3:1 on it, and its seam is the
    /// authored border when there is one. `theme` must be the colours `t` was
    /// derived from.
    #[must_use]
    #[allow(
        clippy::items_after_statements,
        reason = "the host's arithmetic verbatim, bit parity, ruling 324"
    )]
    pub fn from_tokens(theme: ThemeInks, t: &ChromeTokens) -> Self {
        let mut c = {
            let light = t.light;
            let bar_bg = t.bar_bg;
            let (warn, error) = status_inks(t.ansi, bar_bg, light);
            const AA: f64 = 4.5;
            let field_bg = theme.bg;
            let value = ensure_contrast(theme.fg, bar_bg, AA);
            // A meter fill is a surface, not text: the 3:1 non-text floor, so
            // the accent survives on a band it resembles without being
            // dragged to black or white needlessly.
            let accent = t.accent_on(bar_bg, crate::tokens::ACCENT_FLOOR);
            let chip_ground = mix3(bar_bg, theme.fg, if light { 0.12 } else { 0.18 });
            let meter_track = mix3(bar_bg, accent, TRACK_TINT);
            // `label` is the secondary tone, not an optional one: it carries
            // the find panel's hint row, its placeholder and every inactive
            // toggle. Held to the same AA floor as `value`; `value` (bold,
            // full contrast) keeps the hierarchy on its own.
            let label = ensure_contrast(
                mix3(theme.fg, theme.bg, if light { 0.40 } else { 0.48 }),
                bar_bg,
                AA,
            );
            Self {
                bar_bg,
                label,
                value,
                warn,
                error,
                field_bg,
                caret: ensure_contrast(theme.cursor, field_bg, AA),
                // The blend makes the well an inset (`BandInks::well_rule`), but a user
                // theme may land on a `bg` that blends to itself.
                well_rule: (field_bg == bar_bg).then(|| ensure_contrast(theme.fg, field_bg, AA)),
                seam: if t.border.is_some() {
                    t.seam_on(bar_bg)
                } else {
                    label
                },
                accent,
                meter: accent,
                meter_edge: None,
                meter_track,
                chip_ground,
                ring: accent,
                capsule_hover: capsule_hover_fill(bar_bg, theme.fg, value, chip_ground),
                capsule_hover_ink: hover_ink(
                    value,
                    capsule_hover_fill(bar_bg, theme.fg, value, chip_ground),
                ),
                // Toward the ink is away from the band on every theme (the band
                // is a step off `bg`, the ink its opposite), so the lit accent
                // can only gain contrast for the `bar_bg` ink on it; the floor
                // covers a theme whose accent sits between the two.
                capsule_primary_hover: ensure_contrast(
                    mix3(accent, theme.fg, PRIMARY_HOVER_LIFT),
                    bar_bg,
                    3.0,
                ),
                capsule_primary_hover_ink: bar_bg,
                primary_lift_toward: theme.fg,
                on_accent: bar_bg,
            }
        };
        // The meter wears the accent: the cursor when it has a colour, else
        // the blue or cyan [`crate::palette::MeterHue`] borrows (ruling 250), or the authored
        // accent. `c.meter` is already that hue floored to 3:1.
        let hue = t.accent;
        if let Some(a) = t.ansi {
            // The outline's hue: a warm meter floored to AA for its label
            // reads brown; the outline borrows the theme's blue.
            let label = |hue: [u8; 3]| keep_side(hue, c.bar_bg, 4.5);
            let blue = ensure_contrast(a.blue, c.bar_bg, 3.0);
            if outline_borrows_blue(label(c.meter), blue, label(blue)) {
                c.ring = blue;
            }
        }
        // The pastel fill: where the floor turned the hue brown, the fill is the
        // hue itself (its track tinted toward it as any track is) and a darker
        // edge line carries the boundary. The outline keeps what it measured on
        // the floored hue.
        if keeps_pastel(hue, c.meter) {
            c.meter = hue;
            c.meter_track = mix3(c.bar_bg, hue, TRACK_TINT);
            c.meter_edge = Some(pastel_edge(hue, c.meter_track));
        }
        c
    }

    /// The band tones under an OS-forced chrome palette ([`ForcedPalette`]),
    /// i.e. Windows High Contrast.
    ///
    /// The band is a control surface (`COLOR_BTNFACE`) and the find bar's
    /// query field a document one (`COLOR_WINDOW`), Win32's own split. Every
    /// ink collapses onto `COLOR_WINDOWTEXT`: an HC scheme has no dim, and a
    /// user who turned High Contrast on asked for exactly one text colour.
    /// `warn` collapses too: HC discards hue as a channel. `warn` is not
    /// mapped to `COLOR_HIGHLIGHT`, which means "selected" in HC (the active
    /// tab chip uses it), so a warning would look like a selection.
    #[must_use]
    pub fn forced(hc: ForcedPalette) -> Self {
        let on_band = forced_ink(hc.window_text, hc.btn_face);
        let in_well = forced_ink(hc.window_text, hc.window);
        Self {
            bar_bg: hc.btn_face,
            label: on_band,
            value: on_band,
            warn: on_band,
            error: on_band,
            field_bg: hc.window,
            caret: in_well,
            // See `BandInks::well_rule`.
            well_rule: (hc.window == hc.btn_face).then_some(in_well),
            seam: on_band,
            accent: hc.highlight,
            meter: hc.highlight,
            meter_edge: None,
            meter_track: hc.window,
            chip_ground: hc.window,
            ring: hc.highlight,
            capsule_hover: hc.highlight,
            capsule_hover_ink: forced_ink(hc.highlight_text, hc.highlight),
            capsule_primary_hover: hc.highlight,
            capsule_primary_hover_ink: forced_ink(hc.highlight_text, hc.highlight),
            primary_lift_toward: hc.highlight,
            on_accent: forced_ink(hc.highlight_text, hc.highlight),
        }
    }
}

// ---- The chrome helpers -------------------------------------------------------

/// The contrast floor applied on top of an OS-forced palette. Inert on all
/// four stock Windows HC schemes (their `WINDOWTEXT`/`BTNFACE` and
/// `HIGHLIGHTTEXT`/`HIGHLIGHT` pairs are 15:1 or better), so it never
/// overrides what the OS chose; it exists for a hand-edited or third-party
/// scheme that pairs two tones the OS never would. The same 3.0 UI-text
/// floor the tab strip's inks use.
pub(crate) const FORCED_INK_FLOOR: f64 = 3.0;

/// How far the lit Primary's fill moves from the resting accent toward the
/// theme's ink (`mix3` `t`): enough to read as lit on its own, without
/// comparing with the resting chip.
pub const PRIMARY_HOVER_LIFT: f32 = 0.45;

/// How far a meter's track is mixed from the band toward the meter's hue:
/// the bar's own channel, faintly its colour.
pub const TRACK_TINT: f32 = 0.10;

/// The least contrast a hovered chip's fill stands from a resting chip's.
pub const HOVER_RISE: f64 = 1.5;

/// Is this background a light one? Light when black text has more contrast
/// on it than white: the WCAG pole, where WCAG 2 relative luminance crosses
/// √0.0525 − 0.05 ≈ 0.1791 (greys `#767676` and lighter are light, `#757575`
/// and darker are dark). The ink this implies — black on light, white on
/// dark — clears √21 ≈ 4.58:1 on every colour, which a luma threshold did
/// not (luma > 150 put white on `#939393` at 3.07:1 and on `#01ff00` at
/// 1.37:1). Measured in [`contrast`]'s unfused arithmetic. No 8-bit colour
/// ties (the closest, `#cf0dcc`, differs by 2.4e-7); a tie would be dark, the
/// side [`ensure_contrast`] picks on one. The one dark/light classifier for
/// all chrome, the renderer's polarity, and theme admission.
#[inline]
#[must_use]
pub fn bg_is_light(bg: [u8; 3]) -> bool {
    contrast(BLACK, bg) > contrast(WHITE, bg)
}

/// Linear blend of two RGB triples: `a` toward `b` by `t` ∈ \[0,1\], the one
/// blend every chrome surface derives its tones with, so no two surfaces
/// drift apart by a rounding step.
#[inline]
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the host's arithmetic verbatim, bit parity, ruling 324"
)]
pub fn mix3(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let mix = |x: u8, y: u8| (f32::from(x).mul_add(1.0 - t, f32::from(y) * t)).round() as u8;
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
}

/// WCAG relative-contrast ratio between two RGB triples, in `aterm_types::Rgb::contrast`'s
/// arithmetic token for token: an unfused weighted sum, its identity clamps, and the
/// `lum1 > lum2` selection. It must stay unfused: a fused luminance differs by one ulp on
/// a third of pairs, though never across the 3:1 or 4.5:1 line the palette decides at.
///
/// Public because [`ensure_contrast`] is a one-way ratchet (it can only push ink away
/// from its surface) and the tab strip needs to know when it has overshot (a dimmed
/// label the floor dragged past full strength is no longer a dim).
#[inline]
#[must_use]
pub fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
    let lum1 = rgb_luminance(a).clamp(0.0, 1.0);
    let lum2 = rgb_luminance(b).clamp(0.0, 1.0);
    let (lighter, darker) = if lum1 > lum2 {
        (lum1, lum2)
    } else {
        (lum2, lum1)
    };
    let lighter = lighter.clamp(0.0, 1.0);
    let darker = darker.clamp(0.0, 1.0);
    (lighter + 0.05) / (darker + 0.05)
}

/// `aterm_types::Rgb`'s relative luminance per WCAG 2.0: unfused (see
/// [`contrast`]).
#[inline]
fn rgb_luminance(c: [u8; 3]) -> f64 {
    #[inline]
    fn linearize(c: u8) -> f64 {
        let c = f64::from(c) / 255.0;
        let l = if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        };
        l.clamp(0.0, 1.0)
    }
    let r = linearize(c[0]).clamp(0.0, 1.0);
    let g = linearize(c[1]).clamp(0.0, 1.0);
    let b = linearize(c[2]).clamp(0.0, 1.0);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Nudge ink `c` toward black or white until it clears `target`:1 against
/// `bg`, in ten steps. A no-op when `c` already clears the target, so it is
/// safe to wrap an ink that is normally fine and only needs a floor on an
/// exotic user theme.
///
/// The anchor is chosen by the contrast it can REACH, not by the surface's
/// polarity: the ink first moves away from `bg` on its own side (lighter ink
/// toward white, darker toward black), which keeps a floored ink beside the
/// unfloored ones it sits with; where that pole cannot clear `target`
/// against `bg`, it crosses to the other pole. The surface's polarity
/// ([`bg_is_light`]) is the pole crossover, but the anchor is still chosen
/// by what the ink's own side can reach, so a floored ink stays beside its
/// unfloored siblings. (A floor anchored on the former luma split failed:
/// on a mid-grey bar, `#939393`, it walked toward white, which tops out at
/// 3.07:1, while black reaches 6.8:1.) One of black and white always
/// clears √21 ≈ 4.58:1, so a target at or under AA is always met. When
/// neither pole can clear `target`, the best ratio either walk reached is
/// returned.
#[must_use]
pub fn ensure_contrast(c: [u8; 3], bg: [u8; 3], target: f64) -> [u8; 3] {
    const BLACK: [u8; 3] = [0, 0, 0];
    const WHITE: [u8; 3] = [255, 255, 255];
    let start = contrast(c, bg);
    if start >= target {
        return c;
    }
    let (lc, lb) = (rgb_luminance(c), rgb_luminance(bg));
    let own_side = if lc > lb {
        WHITE
    } else if lc < lb {
        BLACK
    } else if contrast(WHITE, bg) >= contrast(BLACK, bg) {
        WHITE
    } else {
        BLACK
    };
    let other = if own_side == WHITE { BLACK } else { WHITE };
    // The ink's own side first, unless only the other pole can clear the
    // target; a pole that cannot reach it is walked only for its best.
    let order = if contrast(own_side, bg) >= target || contrast(other, bg) < target {
        [own_side, other]
    } else {
        [other, own_side]
    };
    let mut best = c;
    let mut best_ratio = start;
    for anchor in order {
        for step in 1..=10u8 {
            let mixed = mix3(c, anchor, f32::from(step) / 10.0);
            let ratio = contrast(mixed, bg);
            if ratio >= target {
                return mixed;
            }
            if ratio > best_ratio {
                best = mixed;
                best_ratio = ratio;
            }
        }
    }
    best
}

/// Floor one OS-forced ink against the OS-forced surface it lands on, at 3:1.
/// See `FORCED_INK_FLOOR` for why a palette the OS chose is floored at all.
#[inline]
#[must_use]
pub fn forced_ink(ink: [u8; 3], on: [u8; 3]) -> [u8; 3] {
    ensure_contrast(ink, on, FORCED_INK_FLOOR)
}

/// The hovered capsule's fill: `bar_bg` moved toward `fg`, 0.30 and on in
/// steps of 0.05 until it stands [`HOVER_RISE`]:1 from `rest` (a resting
/// chip's fill), while `ink` still clears WCAG AA on it. Where no step does
/// both: 0.30, stepped back by 0.05 until `ink` clears AA. A step of 0 is
/// `bar_bg` itself, which `ink` (= `value`) clears by construction; the floor
/// is on the surface because the ink is already at full contrast.
fn capsule_hover_fill(bar_bg: [u8; 3], fg: [u8; 3], ink: [u8; 3], rest: [u8; 3]) -> [u8; 3] {
    const AA: f64 = 4.5;
    for step in 6..=14u8 {
        let fill = mix3(bar_bg, fg, f32::from(step) * 0.05);
        // The ink may lift toward its own end to hold AA on the risen fill
        // ([`hover_ink`]), never cross to the other side of it.
        if bg_is_light(fill) != bg_is_light(bar_bg) || contrast(hover_ink(ink, fill), fill) < AA {
            break;
        }
        if contrast(fill, rest) >= HOVER_RISE {
            return fill;
        }
    }
    for step in (0..=6u8).rev() {
        let fill = mix3(bar_bg, fg, f32::from(step) * 0.05);
        if contrast(ink, fill) >= AA {
            return fill;
        }
    }
    bar_bg
}

/// The band's warn hue as a non-text mark on `ground` (Settings ▸ Messages'
/// severity column): the fixed amber the band's warn words fall back to,
/// floored to the 3:1 non-text contrast on that ground.
#[must_use]
pub fn warn_mark(ground: [u8; 3]) -> [u8; 3] {
    let base = if bg_is_light(ground) {
        WARN_LIGHT
    } else {
        WARN_DARK
    };
    ensure_contrast(base, ground, 3.0)
}

/// The ink on a hovered chip's `fill` ([`BandInks::capsule_hover`]): `value`,
/// lifted toward its own end until it clears AA on the risen fill.
#[inline]
#[must_use]
pub fn hover_ink(value: [u8; 3], fill: [u8; 3]) -> [u8; 3] {
    ensure_contrast(value, fill, 4.5)
}

// ---- The meter's inks ---------------------------------------------------------

/// The inks a row's surface resolves its [`Tone`]s against: the track, the
/// row's fill (the cursor accent, `warn` on a Warn/Error row, `HIGHLIGHT`
/// under High Contrast), the glint (one fixed perceptual step of that fill
/// away from its words' ink, [`glint_step`]) and the fault hue a Fault echo's
/// wash hands the fill to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeterInks {
    /// The track.
    pub track: [u8; 3],
    /// The fill.
    pub fill: [u8; 3],
    /// The glint.
    pub glint: [u8; 3],
    /// The fault hue.
    pub warn: [u8; 3],
}

impl MeterInks {
    /// The inks of a row whose fill wears `fill`.
    ///
    /// The glint (and the Complete echo's sweep, the same light) is the fill
    /// moved one fixed perceptual step away from the ink its words wear on it
    /// ([`glint_step`]): the words only ever gain contrast under it, so no
    /// letter flips as it passes.
    #[must_use]
    pub fn of(c: &BandInks, fill: [u8; 3]) -> Self {
        // Where the step would be too faint to see, the row draws it in its rail band.
        Self {
            track: c.meter_track,
            fill,
            glint: glint_step(fill, fill_ink(c, fill), false),
            warn: c.warn,
        }
    }

    /// The comet keeps its words' side: the inks of a busy row's surface (the
    /// comet, its still track and its echoes) and the anchor its words floor
    /// toward.
    ///
    /// The comet sweeps under every word on the row; in the full accent its
    /// head would cross the luminance where only black or white still reads,
    /// flipping each letter it passed. So each of the comet's inks (the fill,
    /// the head with no lift, and a Fault echo's warn) is moved in linear
    /// light toward the far end from the anchor, its hue kept, by the least
    /// amount that keeps the anchor `COMET_SIDE_AA` (4.6:1) from it: on a dark
    /// band a deep, saturated accent the words ride in a slowly brightening
    /// light ink (`floor_toward`). A determinate bar is not a comet: it keeps
    /// the full accent, and its words ride it dark.
    #[must_use]
    pub fn comet(c: &BandInks, fill: [u8; 3]) -> (Self, [u8; 3]) {
        let anchor = words_anchor(c);
        let fill = keep_side(fill, anchor, COMET_SIDE_AA);
        (
            Self {
                // The comet runs on the neutral channel: the determinate bar's
                // track is tinted toward its hue, but a comet's own light is
                // the hue, and its gradient keeps two-level pixel steps.
                track: c.chip_ground,
                fill,
                glint: fill,
                warn: keep_side(c.warn, anchor, COMET_SIDE_AA),
            },
            anchor,
        )
    }

    /// A tone's colour against these inks (the track mixed toward the fill by
    /// `fill`, that toward the glint by `lift`, plus `warn`'s share of warn
    /// over the track) in linear light, with one rounding at the end (the
    /// pixel raster's own mix).
    #[must_use]
    pub fn rgb(&self, t: FineTone) -> [u8; 3] {
        LinInks::of(self).rgb(t)
    }
}

/// The end of the scale the words on a determinate fill `fill` ride toward:
/// whichever of black and white reads better on it (one of them always
/// clears √21 ≈ 4.58:1).
#[must_use]
pub(crate) fn fill_anchor(fill: [u8; 3]) -> [u8; 3] {
    if contrast(BLACK, fill) >= contrast(WHITE, fill) {
        BLACK
    } else {
        WHITE
    }
}

/// The crisp ink on a fill: the ink every word a determinate row writes over
/// its fill wears. It is the theme's own ink that reads best on the row's
/// resting fill among those on the fill's words' side (`fill_anchor`): its
/// background (the terminal's, `field_bg`, or the band's, `bar_bg`) or its
/// foreground (`value`). Where no candidate sits on that side (a fill at the
/// crossover) the one nearest it does. Chosen once per row from the fill's
/// ink, never from the tone under a cell, so the edge, the glint, a glide or
/// an echo passing under a word never changes which ink it is; the per-cell
/// floor (`floor_word`) stays as the minimum.
#[must_use]
pub fn fill_ink(c: &BandInks, fill: [u8; 3]) -> [u8; 3] {
    let dark = fill_anchor(fill) == BLACK;
    let lum = luminance(fill);
    let candidates = [c.field_bg, c.bar_bg, c.value];
    let on_side = |ink: &&[u8; 3]| {
        if dark {
            luminance(**ink) <= lum
        } else {
            luminance(**ink) >= lum
        }
    };
    let best = candidates
        .iter()
        .filter(on_side)
        .max_by(|a, b| contrast(**a, fill).total_cmp(&contrast(**b, fill)));
    let nearest = || {
        let key = |ink: &&[u8; 3]| luminance(**ink);
        if dark {
            candidates.iter().min_by(|a, b| key(a).total_cmp(&key(b)))
        } else {
            candidates.iter().max_by(|a, b| key(a).total_cmp(&key(b)))
        }
    };
    best.or_else(nearest).copied().unwrap_or(c.bar_bg)
}

/// A determinate row's word ink on `under`: floored to [`WORD_AA`] by the
/// least move toward `near` (`floor_toward`, continuous in the ground), or
/// toward the other end where `near` itself cannot clear it.
#[must_use]
pub(crate) fn floor_word(ink: [u8; 3], under: [u8; 3], near: [u8; 3]) -> [u8; 3] {
    let anchor = if contrast(near, under) >= WORD_AA {
        near
    } else {
        opposite(near)
    };
    floor_toward(ink, under, anchor, WORD_AA)
}

/// The other end of the scale from `anchor`.
#[inline]
const fn opposite(anchor: [u8; 3]) -> [u8; 3] {
    if anchor[0] == BLACK[0] && anchor[1] == BLACK[1] && anchor[2] == BLACK[2] {
        WHITE
    } else {
        BLACK
    }
}

/// How far a busy row's comet may move off its track: every tone it draws
/// leaves the row's words' anchor ([`words_anchor`]) at least this far from
/// it (AA with a hair of margin for the byte rounding of the mixes between).
pub(crate) const COMET_SIDE_AA: f64 = 4.6;

/// White.
pub(crate) const WHITE: [u8; 3] = [255, 255, 255];
/// Black.
pub(crate) const BLACK: [u8; 3] = [0, 0, 0];

/// The end of the scale a row's words sit toward: white when the band's
/// inks are lighter than its track (a dark band), black otherwise.
#[must_use]
pub(crate) fn words_anchor(c: &BandInks) -> [u8; 3] {
    if luminance(c.label) >= luminance(c.meter_track) {
        WHITE
    } else {
        BLACK
    }
}

/// `ink` moved in linear light toward the far end from `anchor` (its
/// chromaticity kept, only its lightness moved) by the least amount that
/// leaves `anchor` at least `target`:1 on it; unchanged when it already is.
/// `anchor` is the words' pure end on a busy row, or a chip's own ink
/// ([`chip_inks`]): the far end is black from an anchor lighter than `ink`,
/// white otherwise.
#[must_use]
#[allow(
    clippy::manual_midpoint,
    reason = "the host's arithmetic verbatim, bit parity, ruling 324"
)]
pub(crate) fn keep_side(ink: [u8; 3], anchor: [u8; 3], target: f64) -> [u8; 3] {
    if contrast(anchor, ink) >= target {
        return ink;
    }
    let away = if luminance(anchor) > luminance(ink) {
        BLACK
    } else {
        WHITE
    };
    let at = |s: f64| -> [u8; 3] {
        [0, 1, 2].map(|k| enc(lin(ink[k]).mul_add(1.0 - s, lin(away[k]) * s)))
    };
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if contrast(anchor, at(mid)) >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    at(hi)
}

/// `ink` floored to `target`:1 on `bg` by the least move toward `anchor`: a
/// continuous function of the ground, so a word under a moving tone
/// brightens and dims with it and never changes side (the comet's words,
/// [`MeterInks::comet`]). `anchor` itself where even it falls short, which
/// the comet's guard ([`hold_side`]) never lets happen.
pub(crate) fn floor_toward(ink: [u8; 3], bg: [u8; 3], anchor: [u8; 3], target: f64) -> [u8; 3] {
    if contrast(ink, bg) >= target {
        return ink;
    }
    if contrast(anchor, bg) < target {
        return anchor;
    }
    let (mut lo, mut hi) = (0u8, 255u8);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if contrast(mix_u8(ink, anchor, mid), bg) >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix_u8(ink, anchor, hi)
}

/// A ground held on its words' side: `bg` pulled back toward `track` (a comet
/// cell's track, or a determinate row's resting fill for its glint) by the
/// least amount that leaves `anchor` `target`:1 from it. The comet's inks
/// already keep that side ([`MeterInks::comet`]); an sRGB mix between two of
/// them can still dip past it on a light band (luminance is not linear in the
/// bytes), and this is where that is caught.
pub(crate) fn hold_side(bg: [u8; 3], track: [u8; 3], anchor: [u8; 3], target: f64) -> [u8; 3] {
    if contrast(anchor, bg) >= target {
        return bg;
    }
    // In linear light: the held tone stays on the straight line between the
    // two, inside the hull of the row's inks.
    let (mut lo, mut hi) = (0u8, 255u8);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if contrast(anchor, lin_mix(bg, track, f64::from(mid) / 255.0)) >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lin_mix(bg, track, f64::from(hi) / 255.0)
}

/// `a` toward `b` by `t`/255 — integer, so every target mixes the same byte.
#[inline]
pub(crate) fn mix_u8(a: [u8; 3], b: [u8; 3], t: u8) -> [u8; 3] {
    let t = u32::from(t);
    let mix = |x: u8, y: u8| {
        u8::try_from((u32::from(x) * (255 - t) + u32::from(y) * t + 127) / 255).unwrap_or(255)
    };
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
}

/// `OkLCh` → sRGB bytes, giving up chroma (never lightness or hue) until the
/// colour is inside the sRGB gamut.
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::manual_midpoint,
    clippy::many_single_char_names,
    reason = "Björn Ottosson's published OkLab matrices, digit for digit; the host's arithmetic verbatim, bit parity, ruling 324"
)]
fn from_oklch(l: f64, c: f64, h: f64) -> [u8; 3] {
    let linear = |c: f64| -> [f64; 3] {
        let (a, b) = (c * h.to_radians().cos(), c * h.to_radians().sin());
        let lp = 0.2158037573f64.mul_add(b, 0.3963377774f64.mul_add(a, l));
        let mp = (-0.0638541728f64).mul_add(b, (-0.1055613458f64).mul_add(a, l));
        let sp = (-1.2914855480f64).mul_add(b, (-0.0894841775f64).mul_add(a, l));
        let (lp, mp, sp) = (lp.powi(3), mp.powi(3), sp.powi(3));
        [
            0.2309699292f64.mul_add(sp, 4.0767416621f64.mul_add(lp, -3.3077115913 * mp)),
            (-0.3413193965f64).mul_add(sp, (-1.2684380046f64).mul_add(lp, 2.6097574011 * mp)),
            1.7076147010f64.mul_add(sp, (-0.0041960863f64).mul_add(lp, -0.7034186147 * mp)),
        ]
    };
    let fits = |v: [f64; 3]| v.iter().all(|x| (-1e-6..=1.0 + 1e-6).contains(x));
    let mut rgb = linear(c);
    if !fits(rgb) {
        let (mut lo, mut hi) = (0.0f64, c);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if fits(linear(mid)) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        rgb = linear(lo);
    }
    rgb.map(enc)
}

// ---- One row resolved ---------------------------------------------------------

/// One row's pixel raster in window pixels, before the host maps it onto its
/// frame: the ground one colour per window pixel column (empty for none: the
/// flat look), the rail band's colours (`None` keeps the ground; empty for no
/// rail), the columns whose chips keep their own fill, and the ink split.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RowRaster {
    /// One colour per window pixel column, `[0, win_w)`.
    pub ground: Vec<[u8; 3]>,
    /// The rail band's colour per window pixel column (`None`: the ground).
    pub rail: Vec<Option<[u8; 3]>>,
    /// Cell columns `[start, end)` that keep their own fill.
    pub own: Vec<(u16, u16)>,
    /// `(col, window x, ink, ground)`: the cell the fill's edge falls in,
    /// the window pixel the edge is at, and the fill side's ink and ground.
    pub split: Option<(u16, u32, [u8; 3], [u8; 3])>,
    /// The one window pixel the fill's edge antialiases (a mix of the fill
    /// and the track), when the edge is inside the row.
    pub edge: Option<u32>,
    /// The window pixels `[start, end)` a pastel fill's darker edge line is
    /// drawn over ([`BandInks::meter_edge`]): the fill's last pixels,
    /// antialiased at both ends. `None` on every other row.
    pub line: Option<(u32, u32)>,
    /// Outlined capsules: `(start, end, ring, inner)`, cell columns, the
    /// ring's colour and the ground inside it.
    pub rings: Vec<(u16, u16, [u8; 3], [u8; 3])>,
    /// The cells whose glyph is a drawn band icon.
    pub icons: Vec<(u16, Icon)>,
}

/// One resolved cell: its character, ink, ground and weight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InkedCell {
    /// The character.
    pub ch: char,
    /// The ink.
    pub fg: [u8; 3],
    /// The ground.
    pub bg: [u8; 3],
    /// Bold.
    pub bold: bool,
    /// Text presentation (the glyph cell).
    pub text_presentation: bool,
    /// The right half of a wide glyph: no glyph of its own (`ch` is a
    /// space), the cell before it draws across it — the renderer's
    /// `RenderCell::wide`. Only the presence row paints one (a CJK or emoji
    /// role, `presence::paint_row`); the band's words are one cell each.
    pub continuation: bool,
}

/// One resolved row: its cells, whether it has a surface (its gutters then
/// continue its painted edge cells), and its pixel raster (`None` where the
/// cells say it all).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// Exactly the presentation's `cols` cells.
    pub cells: Vec<InkedCell>,
    /// The row has a surface.
    pub metered: bool,
    /// The row's pixel raster.
    pub raster: Option<RowRaster>,
}

/// Paint every committed band row of `p` (`paint::paint`) and resolve each
/// on `c` (`resolve`): `hover` lights the chip under the pointer (or the
/// body's `Details ›`) on its row, `geom` maps each surface onto the window,
/// and `forced` says an OS-forced palette is up (the host's High Contrast
/// latch, read once per paint).
#[must_use]
pub fn paint_band(
    p: &Presentation,
    hover: Option<Hover>,
    geom: Geometry,
    motion: &BandMotion,
    forced: bool,
    c: &BandInks,
) -> Vec<Resolved> {
    paint::paint(p, hover, geom, motion, forced)
        .iter()
        .map(|rp| resolve(rp, c))
        .collect()
}

/// The inks a row's words wear, chosen once for the row, never per cell from
/// the tone passing under it, so an edge, a glint, a comet or an echo moving
/// under a word never changes its ink.
pub(crate) struct RowInks<'a> {
    band: [u8; 3],
    /// Each column's surface (the mean over its pixels, what the cell record
    /// carries), and whether it lies on the fill's side of the row; `None` on
    /// an unmetered row.
    meter: Option<&'a [([u8; 3], bool)]>,
    hc: bool,
    /// The High Contrast ink for a word on the fill (`HIGHLIGHTTEXT`).
    hc_fill_ink: [u8; 3],
    /// How a word's ink is chosen on this row.
    plan: InkPlan,
}

/// A row's ink rule.
#[derive(Clone, Copy)]
pub(crate) enum InkPlan {
    /// The band's own inks, untouched: an unmetered row.
    Band,
    /// A busy row: every role's ink floored once toward `anchor` against the
    /// hottest tone the comet can reach (`hot`) and its `track` (the side hold
    /// keeps every tone between them on the far side).
    Busy {
        anchor: [u8; 3],
        hot: [u8; 3],
        track: [u8; 3],
    },
    /// A determinate row: over the fill, the row's one crisp ink floored
    /// against every tone the fill side can take; over the track, each role's
    /// ink floored against the track toward the band's anchor.
    Bar {
        crisp: [u8; 3],
        track: [u8; 3],
        track_anchor: [u8; 3],
    },
}

impl RowInks<'_> {
    /// The background under column `x` (the cell record's).
    fn at(&self, x: usize) -> [u8; 3] {
        self.meter
            .and_then(|m| m.get(x))
            .map_or(self.band, |&(bg, _)| bg)
    }

    /// Whether column `x` lies on the fill's side of the row.
    fn on_fill(&self, x: usize) -> bool {
        self.meter
            .and_then(|m| m.get(x))
            .is_some_and(|&(_, fill)| fill)
    }

    /// `ink` as a word at column `x` wears it: the row's one ink for its
    /// side ([`InkPlan`]); under High Contrast the system's `HIGHLIGHTTEXT`
    /// on the fill and the forced ink floor elsewhere.
    fn ink(&self, x: usize, ink: [u8; 3]) -> [u8; 3] {
        if self.meter.is_none() {
            return ink;
        }
        if self.hc {
            return if self.on_fill(x) {
                self.hc_fill_ink
            } else {
                forced_ink(ink, self.at(x))
            };
        }
        match self.plan {
            InkPlan::Band => ink,
            InkPlan::Busy { anchor, hot, track } => floor_toward(
                floor_toward(ink, hot, anchor, WORD_AA),
                track,
                anchor,
                WORD_AA,
            ),
            InkPlan::Bar {
                crisp,
                track,
                track_anchor,
            } => {
                if self.on_fill(x) {
                    crisp
                } else {
                    floor_word(ink, track, track_anchor)
                }
            }
        }
    }
}

/// The contrast every word on a metered row is held to against the surface
/// under it: WCAG AA, the floor the band's own inks meet on `bar_bg`.
pub const WORD_AA: f64 = 4.5;

/// A chip face's `(ink, fill)`: under High Contrast the system's inks (a lit
/// chip its hover pair, a resting one WINDOWTEXT on the band); an outlined
/// Primary the inside's ground and its accent label ([`outlined_inks`]);
/// every other chip its own AA pair ([`chip_inks`]).
fn chip_face_inks(c: &BandInks, chip: ChipFace) -> ([u8; 3], [u8; 3]) {
    match chip.form {
        ChipForm::Forced if chip.lit => match chip.role {
            CapsuleRole::Primary => (c.capsule_primary_hover_ink, c.capsule_primary_hover),
            CapsuleRole::Secondary | CapsuleRole::Details => (c.capsule_hover_ink, c.capsule_hover),
        },
        ChipForm::Forced => (c.value, c.bar_bg),
        ChipForm::Outlined => {
            let o = outlined_inks(c, chip.lit);
            (o.label, o.inner)
        }
        ChipForm::Solid => {
            let (fg, bg, _) = chip_inks(c, chip.role, chip.lit, chip.metered);
            (fg, bg)
        }
    }
}

/// Resolve one engine-painted row on this palette: its surface's tones to
/// colours (at pixel resolution where the look is graded, [`RowRaster`]),
/// each cell's slot or chip face to its ink, chosen once for the row, the
/// chips' rings, the drawn icon, and last an echo's fade.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one row's colours read its inks, its plan, its cells, its raster and its fade in one order"
)]
pub(crate) fn resolve(rp: &RowPaint<'_>, c: &BandInks) -> Resolved {
    let hc = rp.forced;
    // The meter is the row: the fill's ink is the cursor accent, `warn` on a
    // Warn/Error row; under High Contrast the system's HIGHLIGHT whatever the
    // severity (the forced `warn` is WINDOWTEXT, an ink, not a surface) and
    // the words on it HIGHLIGHTTEXT.
    let fill = match rp.fill {
        Fill::Meter => c.meter,
        Fill::Warn => c.warn,
    };
    let (inks, side) = inks_for(c, fill, rp.busy, hc);
    // The glint in the lowest pixels, where the fill's words leave it no room
    // at full height.
    let glint_rail = !rp.busy && !hc && glint_needs_rail(c, fill, &inks);
    let rail_glint_ink = glint_rail.then(|| glint_step(fill, fill_ink(c, fill), true));
    let lin = LinInks::of(&inks);
    // Each cell's record: the mean tone over its pixels, and its side.
    let tones: Option<Vec<([u8; 3], bool)>> = rp.columns.as_ref().map(|columns| {
        columns
            .iter()
            .map(|col| {
                let mut t = col.tone;
                if glint_rail {
                    t.lift = 0;
                }
                let rgb = lin.rgb(t);
                match side {
                    // Mixed in linear light between two inks already on the
                    // words' side, a comet tone never leaves it: no hold.
                    Some(_) => (rgb, false),
                    None if hc => (rgb, col.on_fill),
                    None => {
                        let rgb = if col.on_fill && t.warn > 0 {
                            hold_side(rgb, inks.fill, fill_anchor(inks.fill), COMET_SIDE_AA)
                        } else {
                            rgb
                        };
                        (rgb, col.on_fill)
                    }
                }
            })
            .collect()
    });
    // The row's inks, chosen once.
    let plan = if matches!(rp.kind, RowSurface::Plain) {
        InkPlan::Band
    } else if let Some(anchor) = side {
        // Floored against the hottest tone the comet can reach (its head, held
        // on the words' side) and its track: nothing it draws between them is
        // nearer the anchor (linear light), so no word brightens or dims as the
        // head passes. A still busy row draws its unlit track only: no comet
        // to floor against.
        let still = matches!(rp.kind, RowSurface::Busy { still: true });
        InkPlan::Busy {
            anchor,
            hot: if still {
                inks.track
            } else {
                hold_side(inks.fill, inks.track, anchor, WORD_AA)
            },
            track: inks.track,
        }
    } else {
        // A stalled bar's fill is its dim slate (`Tone::STALLED`): its own
        // state, its own ink.
        let stalled = matches!(rp.kind, RowSurface::Bar { stalled: true });
        InkPlan::Bar {
            crisp: if stalled {
                crisp_on_stalled(c, fill, &inks)
            } else {
                crisp_on_fill(c, fill, &inks)
            },
            track: inks.track,
            track_anchor: words_anchor(c),
        }
    };
    let on = RowInks {
        band: c.bar_bg,
        meter: tones.as_deref(),
        hc,
        hc_fill_ink: c.on_accent,
        plan,
    };
    // Every cell: a blank one keeps the band's label on its ground, a word
    // its slot's ink as the row's plan floors it, a chip its own face.
    let mut row: Vec<InkedCell> = rp
        .cells
        .iter()
        .enumerate()
        .map(|(x, cell)| {
            let (fg, bg) = match cell.face {
                Face::Blank => (c.label, on.at(x)),
                Face::Word(ink) => (on.ink(x, c.slot(ink)), on.at(x)),
                Face::Chip(chip) => chip_face_inks(c, chip),
            };
            InkedCell {
                ch: cell.ch,
                fg,
                bg,
                bold: cell.bold,
                text_presentation: cell.text_presentation,
                continuation: false,
            }
        })
        .collect();
    let owned = |x: usize| {
        rp.owned
            .iter()
            .any(|&(a, b)| (usize::from(a)..usize::from(b)).contains(&x))
    };
    // The pixel raster: the ground pixel by pixel in linear light, its edge
    // antialiased over one pixel (each pixel is the mean of the profile over
    // it); the glint in the lowest pixels where the words leave it no room at
    // full height. Which window pixels lie on the fill's side (the fade's
    // anchors):
    let mut fill_side: Vec<bool> = Vec::new();
    let mut raster = rp.raster_ground.then(|| {
        let mut r = RowRaster::default();
        let mut memo: Option<(FineTone, [u8; 3])> = None;
        let mut rgb_of = |t: FineTone| match memo {
            Some((k, v)) if k == t => v,
            _ => {
                let v = lin.rgb(t);
                memo = Some((t, v));
                v
            }
        };
        if rp.raster_ground {
            r.ground
                .reserve(usize::try_from(rp.span.win_w()).unwrap_or(0));
        }
        let mut rail_px_row: Vec<Option<[u8; 3]>> = Vec::new();
        for (xw, (t, on_fill)) in (0u64..).zip(rp.pixels()) {
            // The fill's coverage — handed to warn by a Fault's wash.
            fill_side.push(on_fill);
            let base = FineTone {
                lift: if glint_rail { 0 } else { t.lift },
                ..t
            };
            let mut rgb = rgb_of(base);
            if side.is_none() && t.warn > 0 && !hc && rp.edge_px.is_none_or(|e| xw < e) {
                rgb = hold_side(rgb, inks.fill, fill_anchor(inks.fill), COMET_SIDE_AA);
            }
            r.ground.push(rgb);
            if let Some(g) = rail_glint_ink {
                let lift = f64::from(t.lift) / (255.0 * 256.0);
                rail_px_row.push((t.lift > 0).then(|| lin_mix(rgb, g, lift)));
            }
        }
        if glint_rail {
            r.rail = rail_px_row;
        }
        r.own.clone_from(&rp.owned);
        r.rings = rp
            .rings
            .iter()
            .map(|&(start, end, lit)| {
                let o = outlined_inks(c, lit);
                (start, end, o.ring, o.inner)
            })
            .collect();
        r.edge = rp.edge_px.and_then(|e| u32::try_from(e).ok());
        // The split cell's fill side wears the row's crisp ink on the ground
        // just left of the edge.
        if let (Some(s), InkPlan::Bar { crisp, .. }) = (rp.split, plan) {
            let under = r.ground.get(s.under_px).copied().unwrap_or(inks.fill);
            r.split = Some((s.col, s.x, crisp, under));
        }
        // The pastel fill's edge: the fill's last pixels in the palette's
        // darker edge ink (the boundary a pastel cannot carry on its own),
        // drawn after the split took the fill's own ground for its glyph. Each
        // pixel trades the fill for the edge by the line's coverage of it
        // (antialiased at both ends), in linear light, scaled by the fill's own
        // strength there, so a stalled slate or a Fault's wash takes the line
        // with it.
        if let Some(edge_ink) = c.meter_edge
            && fill == c.meter
            && let Some(line) = rp.edge_line()
        {
            let (edge_lin, fill_lin) = (edge_ink.map(self::lin), inks.fill.map(self::lin));
            for &(p, k) in &line.coverage {
                let Some(px) = r.ground.get_mut(p) else {
                    continue;
                };
                let old = *px;
                *px = [0, 1, 2]
                    .map(|i| enc((edge_lin[i] - fill_lin[i]).mul_add(k, self::lin(old[i]))));
            }
            r.line = Some((line.first, line.end));
        }
        r
    });
    if let Some((col, icon)) = rp.icon {
        raster
            .get_or_insert_with(RowRaster::default)
            .icons
            .push((col, icon));
    }
    // An echo's fade: the row fades as one layer, in linear light, toward the
    // band; for its first half (the words' opacity at least one half) the
    // ground under the words is held on their side and the words floored on
    // it, so they keep AA to the midpoint and never change side; the second
    // half fades that composed state to the band.
    if rp.fade > 0 {
        let alpha = 1.0 - f64::from(rp.fade) / 255.0;
        let track_anchor = words_anchor(c);
        let anchor_of = |on_fill: bool| match (side, on_fill) {
            (Some(a), _) => a,
            (None, true) => fill_anchor(fill),
            (None, false) => track_anchor,
        };
        for (x, cell) in row.iter_mut().enumerate() {
            let a = anchor_of(on.on_fill(x));
            if owned(x) {
                cell.fg = lin_mix(cell.fg, c.bar_bg, 1.0 - alpha);
                cell.bg = lin_mix(cell.bg, c.bar_bg, 1.0 - alpha);
                continue;
            }
            let (bg, fg) = faded_pair(cell.bg, cell.fg, c.bar_bg, alpha, a);
            cell.bg = bg;
            cell.fg = fg;
        }
        if let Some(r) = raster.as_mut() {
            for (xw, px) in r.ground.iter_mut().enumerate() {
                let on_fill = fill_side.get(xw).copied().unwrap_or(false);
                *px = faded_ground(*px, c.bar_bg, alpha, anchor_of(on_fill));
            }
            for px in r.rail.iter_mut().flatten() {
                *px = lin_mix(*px, c.bar_bg, 1.0 - alpha);
            }
            if let Some((_, _, ink, bg)) = r.split.as_mut() {
                let a = anchor_of(true);
                let (b, i) = faded_pair(*bg, *ink, c.bar_bg, alpha, a);
                *bg = b;
                *ink = i;
            }
        }
    }
    Resolved {
        cells: row,
        metered: tones.is_some(),
        raster,
    }
}

/// The inks a row's surface resolves against, from what the painter says of
/// the row (its fill's ink, whether it is busy, and the forced palette) and,
/// on a busy row, the anchor its words floor toward. A busy row's surface
/// keeps its words' side (the comet, its track and its echoes); a
/// determinate row's Fault wash keeps the fill's words' side, its hue kept
/// (High Contrast keeps its raw warn).
#[must_use]
pub fn inks_for(c: &BandInks, fill: [u8; 3], busy: bool, hc: bool) -> (MeterInks, Option<[u8; 3]>) {
    if busy && !hc {
        let (inks, anchor) = MeterInks::comet(c, fill);
        (inks, Some(anchor))
    } else {
        let inks = MeterInks::of(c, fill);
        let warn = if hc {
            inks.warn
        } else {
            keep_side(inks.warn, fill_anchor(fill), COMET_SIDE_AA)
        };
        (MeterInks { warn, ..inks }, None)
    }
}

/// The one ink every word wears over a determinate row's fill: the row's
/// crisp ink ([`fill_ink`]), floored once against every tone the fill's side
/// can take (the fill, its glint where drawn at full height, a Fault's warn
/// wash), so no edge, glint, glide or echo passing under a word ever changes
/// its ink. A stalled bar is a state, not a motion: its dim slate takes its
/// own ink, `crisp_on_stalled`.
#[must_use]
pub fn crisp_on_fill(c: &BandInks, fill: [u8; 3], inks: &MeterInks) -> [u8; 3] {
    let anchor = fill_anchor(fill);
    let mut crisp = fill_ink(c, fill);
    let mut grounds = vec![inks.fill, inks.warn];
    if delta_e(inks.glint, fill) >= 6.0 {
        grounds.push(inks.glint);
    }
    for _ in 0..2 {
        for g in &grounds {
            crisp = floor_word(crisp, *g, anchor);
        }
    }
    crisp
}

/// The one ink over a stalled bar's dim slate ([`crisp_on_fill`]'s
/// counterpart for the stall's state).
fn crisp_on_stalled(c: &BandInks, fill: [u8; 3], inks: &MeterInks) -> [u8; 3] {
    let stalled = LinInks::of(inks).rgb(Tone::STALLED.into());
    floor_word(fill_ink(c, fill), stalled, fill_anchor(stalled))
}

/// A ground and the ink on it at layer opacity `alpha`: above one half, the
/// ground fades in linear light but is held on the words' side of `anchor`
/// ([`hold_side`]) and the ink, faded alike, is floored on it toward
/// `anchor`, so the words keep AA and never flip; below one half, that state
/// at one half fades on toward `band` as one layer.
pub(crate) fn faded_pair(
    bg: [u8; 3],
    fg: [u8; 3],
    band: [u8; 3],
    alpha: f64,
    anchor: [u8; 3],
) -> ([u8; 3], [u8; 3]) {
    let a1 = alpha.max(0.5);
    let bg1 = faded_ground(bg, band, a1, anchor);
    let fg1 = floor_toward(lin_mix(fg, band, 1.0 - a1), bg1, anchor, WORD_AA);
    if alpha >= 0.5 {
        (bg1, fg1)
    } else {
        let t = 1.0 - 2.0 * alpha;
        (lin_mix(bg1, band, t), lin_mix(fg1, band, t))
    }
}

/// A ground pixel at layer opacity `alpha` (see [`faded_pair`]).
pub(crate) fn faded_ground(g: [u8; 3], band: [u8; 3], alpha: f64, anchor: [u8; 3]) -> [u8; 3] {
    let a1 = alpha.max(0.5);
    let held = if contrast(anchor, g) >= COMET_SIDE_AA {
        hold_side(lin_mix(g, band, 1.0 - a1), g, anchor, COMET_SIDE_AA)
    } else {
        lin_mix(g, band, 1.0 - a1)
    };
    if alpha >= 0.5 {
        held
    } else {
        lin_mix(held, band, 1.0 - 2.0 * alpha)
    }
}

/// `a` toward `b` by `t` (0–1) in linear light.
#[must_use]
pub fn lin_mix(a: [u8; 3], b: [u8; 3], t: f64) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|k| enc(lin(a[k]).mul_add(1.0 - t, lin(b[k]) * t)))
}

/// A row's [`MeterInks`] in linear light, for the pixel raster: a tone is the
/// track mixed toward the fill by `fill`, that toward the glint by `lift`,
/// plus `warn`'s share of warn over the track, in linear light so a gradient
/// has no dark band between its ends and a Fault's cross-fade stays inside
/// the track–fill–warn hull.
pub(crate) struct LinInks {
    track: [f64; 3],
    fill: [f64; 3],
    glint: [f64; 3],
    warn: [f64; 3],
}

impl LinInks {
    pub(crate) fn of(k: &MeterInks) -> Self {
        Self {
            track: k.track.map(lin),
            fill: k.fill.map(lin),
            glint: k.glint.map(lin),
            warn: k.warn.map(lin),
        }
    }

    pub(crate) fn rgb(&self, t: FineTone) -> [u8; 3] {
        const FULL: f64 = 255.0 * 256.0;
        let (f, l, w) = (
            f64::from(t.fill) / FULL,
            f64::from(t.lift) / FULL,
            f64::from(t.warn) / FULL,
        );
        [0, 1, 2].map(|i| {
            let base = (self.fill[i] - self.track[i]).mul_add(f, self.track[i]);
            let base = (self.glint[i] - base).mul_add(l, base);
            enc((self.warn[i] - self.track[i]).mul_add(w, base))
        })
    }
}

/// `OkLab` `(L, a, b)` of an sRGB colour.
fn oklab(c: [u8; 3]) -> (f64, f64, f64) {
    let (l, ch, h) = oklch(c);
    (l, ch * h.to_radians().cos(), ch * h.to_radians().sin())
}

/// The `OkLab` distance between two colours, ×100 (a ΔE of 6 is a glint the
/// eye sees on any fill).
#[must_use]
pub fn delta_e(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (l0, a0, b0) = oklab(a);
    let (l1, a1, b1) = oklab(b);
    100.0 * ((l1 - l0).powi(2) + (a1 - a0).powi(2) + (b1 - b0).powi(2)).sqrt()
}

/// The glint's one perceptual step: the fill moved `GLINT_DL` in `OkLab`
/// lightness away from the ink its words wear on it (so the words only ever
/// gain contrast under it and never change side), with a small chroma lift.
/// `rail`: the other way, for the rail band under the words, where no ink
/// sits.
#[must_use]
pub fn glint_step(fill: [u8; 3], words: [u8; 3], rail: bool) -> [u8; 3] {
    let (l, ch, h) = oklch(fill);
    let away = if oklch(words).0 < l { 1.0 } else { -1.0 };
    let dir = if rail { -away } else { away };
    from_oklch(GLINT_DL.mul_add(dir, l).clamp(0.0, 1.0), ch * 1.12, h)
}

/// The glint's lightness step in `OkLab`, within 0.07–0.10.
const GLINT_DL: f64 = 0.085;

/// Whether a row's glint belongs in the rail band: its full-height step
/// away from the fill's words would read under ΔE 6 (a fill already near the
/// end of the scale its words are not on).
fn glint_needs_rail(c: &BandInks, fill: [u8; 3], inks: &MeterInks) -> bool {
    let _ = c;
    delta_e(inks.glint, fill) < 6.0
}

/// An outlined Primary's inks: the ring, the ground inside it and the label
/// on that ground.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutlinedInks {
    /// The ring.
    pub ring: [u8; 3],
    /// The ground inside it.
    pub inner: [u8; 3],
    /// The label on that ground.
    pub label: [u8; 3],
}

/// The inks of an outlined Primary on a metered row. At rest: the ring is the
/// row's accent (the meter's hue, already 3:1 on the band, or the theme's
/// blue where that hue floored for its label reads brown, [`BandInks::ring`]),
/// the inside is the band's own ground, and the label is that accent moved in
/// linear light, its hue kept, by the least amount that reads at AA on the
/// inside (`keep_side`). Lit: the ring lifts [`PRIMARY_HOVER_LIFT`] toward the
/// theme's ink, the inside takes up to an `OUTLINE_LIT_TINT` tint of the
/// accent (the most its label still clears AA on), and the ring holds 3:1 on
/// it.
#[must_use]
pub fn outlined_inks(c: &BandInks, lit: bool) -> OutlinedInks {
    // The lit tint steps back toward the band until the label can clear AA
    // on it (a near-white accent on a dark band tints the inside toward a
    // grey no ink reads on at full strength); a tint of 0 is the band.
    let (inner, label) = (0..=6u8)
        .rev()
        .map(|k| {
            let t = if lit {
                OUTLINE_LIT_TINT * f64::from(k) / 6.0
            } else {
                0.0
            };
            let inner = lin_mix(c.bar_bg, c.ring, t);
            (inner, keep_side(c.ring, inner, WORD_AA))
        })
        .find(|&(inner, label)| contrast(label, inner) >= WORD_AA)
        .unwrap_or((c.bar_bg, keep_side(c.ring, c.bar_bg, WORD_AA)));
    let ring = if lit {
        mix3(c.ring, c.primary_lift_toward, PRIMARY_HOVER_LIFT)
    } else {
        c.ring
    };
    OutlinedInks {
        ring: keep_side(ring, inner, 3.0),
        inner,
        label,
    }
}

/// How far a lit outlined Primary's inside moves from the band toward the
/// accent, in linear light: a tint that says "under the pointer" and leaves
/// the bar the only solid accent on the row.
const OUTLINE_LIT_TINT: f64 = 0.18;

/// A chip's `(ink, fill, bold)` off High Contrast, every label at AA on its
/// own fill, over the band, the track or the fill, resting or lit. A Primary
/// keeps its polarity, the band's ink on the accent: where that pair falls
/// short (3.69:1 on Solarized Light, 4.28:1 on GitHub Light) the accent
/// deepens in linear light, its hue kept, by the least amount that clears AA
/// ([`keep_side`]), rather than turning the label black. The lit Primary is
/// lifted from it and deepened the same way. A Primary on a metered row is
/// outlined instead ([`outlined_inks`]). Every other label is lifted to AA by
/// [`ensure_contrast`].
#[must_use]
pub(crate) fn chip_inks(
    c: &BandInks,
    role: CapsuleRole,
    lit: bool,
    metered: bool,
) -> ([u8; 3], [u8; 3], bool) {
    let accent = keep_side(c.accent, c.bar_bg, WORD_AA);
    let (fg, bg, bold) = match (role, lit) {
        // Lifted from the accent the chip wears, so a deepened rest keeps a
        // whole lift between it and its lit form (lifting the raw accent and
        // deepening it on its own can leave the two nearly equal).
        (CapsuleRole::Primary, true) => {
            let ink = c.capsule_primary_hover_ink;
            let lit = mix3(accent, c.primary_lift_toward, PRIMARY_HOVER_LIFT);
            (ink, keep_side(lit, ink, WORD_AA), true)
        }
        (CapsuleRole::Secondary | CapsuleRole::Details, true) => {
            (c.capsule_hover_ink, c.capsule_hover, false)
        }
        (CapsuleRole::Primary, false) => (c.bar_bg, accent, true),
        (CapsuleRole::Secondary, false) if metered => (c.value, c.bar_bg, false),
        (CapsuleRole::Secondary, false) => (c.value, c.chip_ground, false),
        (CapsuleRole::Details, false) => (c.label, c.bar_bg, false),
    };
    (ensure_contrast(fg, bg, WORD_AA), bg, bold)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The former dark/light rule, verbatim: Rec. 601 luma above 150 in
    /// unfused `f32`. Kept only as the negative control the pole is
    /// measured against.
    fn luma_is_light(c: [u8; 3]) -> bool {
        0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2]) > 150.0
    }

    /// The pre-fix floor: the anchor chosen by the surface's luma alone.
    fn luma_anchored_floor(c: [u8; 3], bg: [u8; 3], target: f64) -> [u8; 3] {
        let anchor = if luma_is_light(bg) {
            [0, 0, 0]
        } else {
            [255, 255, 255]
        };
        (1..=10u8)
            .map(|step| mix3(c, anchor, f32::from(step) / 10.0))
            .find(|m| contrast(*m, bg) >= target)
            .unwrap_or(anchor)
    }

    /// Over a lattice of surfaces (every grey and a colour cube) and inks, at
    /// 3:1 and AA, `ensure_contrast` clears `target` wherever black or white
    /// does, never loses the contrast it started with, and leaves an ink that
    /// already clears it untouched.
    #[test]
    fn the_floor_is_met_whenever_either_pole_can_meet_it() {
        let greys = (0..=255u8).map(|g| [g, g, g]);
        let cube = (0..=255u8)
            .step_by(51)
            .flat_map(|r| (0..=255u8).step_by(51).map(move |g| (r, g)))
            .flat_map(|(r, g)| (0..=255u8).step_by(51).map(move |b| [r, g, b]));
        let surfaces: Vec<[u8; 3]> = greys.chain(cube).collect();
        let inks: Vec<[u8; 3]> = (0..=255u8)
            .step_by(17)
            .flat_map(|v| [[v, v, v], [v, 255 - v, v / 2], [255 - v, v / 3, v]])
            .collect();
        for &bg in &surfaces {
            for target in [3.0, 4.5] {
                let reachable =
                    contrast([0, 0, 0], bg) >= target || contrast([255, 255, 255], bg) >= target;
                for &c in &inks {
                    let got = ensure_contrast(c, bg, target);
                    let (before, after) = (contrast(c, bg), contrast(got, bg));
                    if before >= target {
                        assert_eq!(got, c, "{c:?} on {bg:?} already cleared {target}");
                    }
                    if reachable {
                        assert!(
                            after >= target,
                            "{c:?} on {bg:?}: {after:.2} < {target} though a pole clears it"
                        );
                    }
                    assert!(after >= before, "{c:?} on {bg:?} lost contrast");
                }
            }
        }
    }

    /// Warn and error speak the theme's own ANSI yellow and red (slots 3 and
    /// 1), floored to AA on the band at their own hue, where those slots
    /// carry a colour; a monochrome palette's grey slots leave the fixed
    /// fallbacks in place.
    #[test]
    fn warn_and_error_seed_from_the_themes_ansi_yellow_and_red() {
        let theme = ThemeInks {
            bg: [0xfd, 0xf6, 0xe3],
            fg: [0x58, 0x6e, 0x75],
            cursor: [0x58, 0x6e, 0x75],
        };
        let solarized = AnsiHues {
            red: [0xdc, 0x32, 0x2f],
            green: [0x85, 0x99, 0x00],
            yellow: [0xb5, 0x89, 0x00],
            blue: [0x26, 0x8b, 0xd2],
            cyan: [0x2a, 0xa1, 0x98],
        };
        let c = BandInks::derive(theme, Some(solarized), BarBase::Blend);
        let hue = |ink: [u8; 3]| crate::palette::oklch(ink).2;
        assert!((hue(c.warn) - hue(solarized.yellow)).abs() < 3.0);
        assert!((hue(c.error) - hue(solarized.red)).abs() < 3.0);
        assert!(contrast(c.warn, c.bar_bg) >= 4.5 && contrast(c.error, c.bar_bg) >= 4.5);
        let mono = AnsiHues {
            red: [0x80; 3],
            yellow: [0xa0; 3],
            ..solarized
        };
        let m = BandInks::derive(theme, Some(mono), BarBase::Blend);
        let none = BandInks::derive(theme, None, BarBase::Blend);
        assert_eq!((m.warn, m.error), (none.warn, none.error));
        assert_ne!(none.error, c.error, "the fallback is not the theme's red");
    }

    /// Every built-in theme, on every band ground a host draws: the error ink
    /// keeps a colour after its floor, warn and error stand apart in lightness
    /// (warn the lighter on either polarity: lifted above error on a dark
    /// band, error pushed below it on a light one) and in `OkLab`, and both
    /// clear AA.
    #[test]
    fn warn_and_error_stay_coloured_and_a_lightness_step_apart_on_every_builtin() {
        use crate::palette::{NEAR_GREY_CHROMA, chroma, oklab_distance};
        let bytes = |c: aterm_types::Rgb| [c.r, c.g, c.b];
        let bases = [
            BarBase::Blend,
            BarBase::Fixed {
                light: [0xeb; 3],
                dark: [0x30; 3],
            },
        ];
        // Byte rounding of the lightness moves: well under the gap itself.
        let slack = 0.01;
        let names = aterm_types::scheme::builtin_names();
        assert!(names.len() > 1);
        for name in names {
            let scheme = aterm_types::scheme::builtin(name).expect("a listed built-in");
            let fg = bytes(scheme.foreground);
            let theme = ThemeInks {
                bg: bytes(scheme.background),
                fg,
                cursor: scheme.cursor.map_or(fg, bytes),
            };
            let ansi = AnsiHues {
                red: bytes(scheme.ansi[1]),
                green: bytes(scheme.ansi[2]),
                yellow: bytes(scheme.ansi[3]),
                blue: bytes(scheme.ansi[4]),
                cyan: bytes(scheme.ansi[6]),
            };
            for base in bases {
                let c = BandInks::derive(theme, Some(ansi), base);
                let (lw, le) = (oklch(c.warn).0, oklch(c.error).0);
                let gap = lw - le;
                assert!(
                    chroma(c.error) >= NEAR_GREY_CHROMA,
                    "{name} {base:?}: error {:?} washed out",
                    c.error
                );
                assert!(
                    gap >= STATUS_LIGHTNESS_GAP - slack,
                    "{name} {base:?}: warn L {lw:.3} / error L {le:.3}"
                );
                assert!(
                    oklab_distance(c.warn, c.error) >= STATUS_DISTANCE,
                    "{name} {base:?}: warn {:?} and error {:?} too close",
                    c.warn,
                    c.error
                );
                assert!(contrast(c.warn, c.bar_bg) >= 4.5, "{name} {base:?}: warn");
                assert!(contrast(c.error, c.bar_bg) >= 4.5, "{name} {base:?}: error");
            }
        }
    }

    /// Negative control for the property above: Nord's red floored by an sRGB
    /// mix toward white ([`ensure_contrast`]) comes out a grey-pink under the
    /// near-grey line and closer to the floored yellow than [`STATUS_DISTANCE`];
    /// the lightness floor keeps it a red.
    #[test]
    fn an_srgb_mix_floor_washes_a_red_the_lightness_floor_keeps() {
        use crate::palette::{NEAR_GREY_CHROMA, chroma, oklab_distance};
        let (bg, fg) = ([0x2e, 0x34, 0x40], [0xd8, 0xde, 0xe9]);
        let (red, yellow) = ([0xbf, 0x61, 0x6a], [0xeb, 0xcb, 0x8b]);
        let bar = mix3(bg, fg, 0.16);
        let mixed_red = ensure_contrast(red, bar, 4.5);
        let mixed_yellow = ensure_contrast(yellow, bar, 4.5);
        assert!(chroma(mixed_red) < NEAR_GREY_CHROMA);
        assert!(oklab_distance(mixed_red, mixed_yellow) < STATUS_DISTANCE);
        assert!(chroma(lightness_floor(red, bar, 4.5, true)) >= NEAR_GREY_CHROMA);
    }

    /// Negative control for the lattice law: on a mid-grey bar (`#939393`, the
    /// message band of a mid-tone theme) the luma-anchored floor pushes the
    /// secondary label (`#535353`) to white, 3.07:1, under AA, though black
    /// clears it; [`ensure_contrast`] reaches AA on the label's own (dark) side.
    #[test]
    fn a_mid_grey_bar_floors_its_label_on_the_side_that_reaches_aa() {
        let (bar, label) = ([0x93; 3], [0x53; 3]);
        assert!(contrast(luma_anchored_floor(label, bar, 4.5), bar) < 4.5);
        let got = ensure_contrast(label, bar, 4.5);
        assert!(contrast(got, bar) >= 4.5);
        assert!(got[0] < label[0], "the label stays darker than its bar");
    }

    /// Every colour a pole-ink law is checked on: every grey, an RGB
    /// lattice (every 15th level per channel, ends included), the colour
    /// the luma rule failed worst (`#01ff00`) and the nearest tie
    /// (`#cf0dcc`).
    fn pole_sweep() -> Vec<[u8; 3]> {
        let levels: Vec<u8> = (0..=255u8).step_by(15).collect();
        let mut out: Vec<[u8; 3]> = (0..=255u8).map(|g| [g, g, g]).collect();
        for &r in &levels {
            for &g in &levels {
                for &b in &levels {
                    out.push([r, g, b]);
                }
            }
        }
        out.extend([[0x01, 0xff, 0x00], [0xcf, 0x0d, 0xcc]]);
        out
    }

    /// Does `is_light`'s pole ink (black on light, white on dark) clear
    /// √21 and read at least as well as the other pole on `c`?
    fn pole_ink_reads_best(is_light: fn([u8; 3]) -> bool, c: [u8; 3]) -> bool {
        let (ink, other) = if is_light(c) {
            (BLACK, WHITE)
        } else {
            (WHITE, BLACK)
        };
        contrast(ink, c) >= 4.58 && contrast(ink, c) >= contrast(other, c)
    }

    /// THE POLE INK READS BEST: on every swept surface the ink
    /// [`bg_is_light`] implies clears √21 ≈ 4.58:1 and out-contrasts the
    /// other pole. Negative control: the former luma rule breaks the same
    /// law on the sweep — white on `#939393` at under 3.1:1 and on
    /// `#01ff00` at under 1.4:1.
    #[test]
    fn bg_is_light_picks_the_pole_ink_that_reads_best() {
        let sweep = pole_sweep();
        for &c in &sweep {
            assert!(pole_ink_reads_best(bg_is_light, c), "{c:02x?}");
        }
        assert!(!luma_is_light([0x93; 3]));
        assert!(contrast(WHITE, [0x93; 3]) < 3.1);
        assert!(!luma_is_light([0x01, 0xff, 0x00]));
        assert!(contrast(WHITE, [0x01, 0xff, 0x00]) < 1.4);
        let luma_failures = sweep
            .iter()
            .filter(|&&c| !pole_ink_reads_best(luma_is_light, c))
            .count();
        assert!(luma_failures > 0, "the sweep must catch the luma rule");
    }

    /// THE GREYS TURN LIGHT ONCE, AT THE POLE: along the grey ramp the
    /// classifier changes value exactly once, dark to light, between
    /// `#757575` and `#767676` — the first grey where black (4.62:1) beats
    /// white (4.54:1). The former luma rule called `#767676` dark.
    #[test]
    fn the_greys_turn_light_once_at_the_pole() {
        let ramp: Vec<bool> = (0..=255u8).map(|g| bg_is_light([g; 3])).collect();
        let flips = ramp.windows(2).filter(|w| w[0] != w[1]).count();
        assert_eq!(flips, 1);
        assert!(!ramp[0] && ramp[255], "dark to light, never back");
        assert!(!bg_is_light([0x75; 3]));
        assert!(bg_is_light([0x76; 3]));
        assert!(contrast(BLACK, [0x76; 3]) > contrast(WHITE, [0x76; 3]));
        assert!(!bg_is_light([0x11, 0x13, 0x18]));
        assert!(bg_is_light([0xff; 3]));
        assert!(!luma_is_light([0x76; 3]), "the former rule called it dark");
    }

    /// The forced mapping (Windows High Contrast, CSS forced-colors): the band
    /// is BTNFACE, the well WINDOW, the meter HIGHLIGHT on a WINDOW track,
    /// every ink WINDOWTEXT, the lit chips HIGHLIGHT with HIGHLIGHTTEXT; a
    /// well that shares the band's tone gets a border, and nothing
    /// theme-derived survives (no pastel edge).
    #[test]
    fn the_forced_palette_maps_each_system_colour_to_its_role() {
        let hc = ForcedPalette {
            window: [0, 0, 0],
            window_text: [0xff, 0xff, 0xff],
            highlight: [0x1a, 0xeb, 0xff],
            highlight_text: [0, 0, 0],
            btn_face: [0, 0, 0],
        };
        let c = BandInks::forced(hc);
        assert_eq!(c.bar_bg, hc.btn_face);
        assert_eq!(c.field_bg, hc.window);
        for ink in [c.label, c.value, c.warn, c.error, c.caret] {
            assert_eq!(ink, hc.window_text);
        }
        for surface in [
            c.accent,
            c.meter,
            c.ring,
            c.capsule_hover,
            c.capsule_primary_hover,
        ] {
            assert_eq!(surface, hc.highlight);
        }
        assert_eq!((c.meter_track, c.chip_ground), (hc.window, hc.window));
        for ink in [
            c.capsule_hover_ink,
            c.capsule_primary_hover_ink,
            c.on_accent,
        ] {
            assert_eq!(ink, hc.highlight_text);
        }
        assert_eq!(c.well_rule, Some(hc.window_text));
        assert_eq!(c.meter_edge, None);
        // A hostile pair the OS would never ship is floored, never printed.
        let hostile = BandInks::forced(ForcedPalette {
            window_text: [0x10, 0x10, 0x10],
            ..hc
        });
        assert!(contrast(hostile.value, hostile.bar_bg) >= FORCED_INK_FLOOR);
        // A well with its own tone needs no border.
        let inset = BandInks::forced(ForcedPalette {
            window: [0x20, 0x20, 0x20],
            ..hc
        });
        assert_eq!(inset.well_rule, None);
    }
}
