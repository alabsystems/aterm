// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CHROME TOKENS: the semantic colours every chrome surface of aterm draws
//! from, derived once from a theme on plain sRGB bytes.
//!
//! Each surface family (the message band, the tab strip, the find panel, the
//! native Settings page, the pane dividers) keeps its own surface-specific
//! ramps, and its bit-for-bit arithmetic (rulings 324 and 438). What they share
//! comes from here, so no two surfaces disagree about it:
//!
//! * **polarity**: [`ChromeTokens::light`], the WCAG pole of the background
//!   ([`crate::ink::bg_is_light`]);
//! * **the accent**: [`ChromeTokens::accent`] and where it came from
//!   ([`AccentSource`]). The theme's authored `accent_color`; else its cursor
//!   when the cursor has a colour; else the ANSI blue or cyan the message
//!   band's meter borrows ([`MeterHue`], ruling 250); else the grey cursor.
//!   The accent is unfloored: each surface floors it on its own ground
//!   ([`ChromeTokens::accent_on`]);
//! * **the seam**: a divider or a band's edge ([`ChromeTokens::seam_on`]),
//!   the authored `border_color` or the ground stepped [`SEAM_T`] toward the
//!   ink, held to [`SEAM_FLOOR`];
//! * **the search match**: find's highlight on every match but the current
//!   one ([`ChromeTokens::search_match`]);
//! * **the status hues**: the theme's own ANSI red and green as `OkLCh` hues,
//!   when they carry a colour;
//! * **the authored overrides** ([`AuthoredChrome`]), which the host has
//!   already resolved (`aterm.toml` over the theme file).
//!
//! The crate stays wasm-clean: the host maps its theme's chrome keys onto
//! [`AuthoredChrome`], and the shared floor constants come from
//! `aterm_types::scheme`.

use crate::ink::{AnsiHues, BarBase, bg_is_light, contrast, ensure_contrast, mix3};
use crate::palette::{BORROW_CHROMA, MeterHue, chroma, near_grey, oklab_distance, oklch};

/// The chrome colours a theme or the user's config authored, already resolved
/// (config over theme). `None` leaves a token derived.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AuthoredChrome {
    /// `accent_color`: the chrome accent.
    pub accent: Option<[u8; 3]>,
    /// `active_tab_color`: the selected tab's background.
    pub active_tab: Option<[u8; 3]>,
    /// `border_color`: dividers and the seams of the chrome bands.
    pub border: Option<[u8; 3]>,
    /// `search_match_color`: find's highlight on every match but the current.
    pub search_match: Option<[u8; 3]>,
}

impl AuthoredChrome {
    /// Nothing authored: every token derived.
    pub const NONE: Self = Self {
        accent: None,
        active_tab: None,
        border: None,
        search_match: None,
    };
}

/// What the tokens are derived from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChromeSeeds {
    /// The background of the surface's theme.
    pub bg: [u8; 3],
    /// Its foreground.
    pub fg: [u8; 3],
    /// Its cursor.
    pub cursor: [u8; 3],
    /// The terminal palette's hues, when the host knows them.
    pub ansi: Option<AnsiHues>,
    /// The authored overrides.
    pub authored: AuthoredChrome,
    /// The operating system's accent colour for a surface on this side of the
    /// light/dark split (Windows' Personalization ▸ Colours), when the host
    /// follows it: the accent wherever none is authored. `None` elsewhere.
    pub system_accent: Option<[u8; 3]>,
    /// Where the band's ground comes from on this platform.
    pub base: BarBase,
}

/// Where [`ChromeTokens::accent`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccentSource {
    /// The theme's or the config's `accent_color`.
    Authored,
    /// The operating system's accent colour ([`ChromeSeeds::system_accent`]).
    System,
    /// The theme's cursor, which has a colour.
    Cursor,
    /// The ANSI blue (slot 4), borrowed for a near-grey cursor.
    Blue,
    /// The ANSI cyan (slot 6), borrowed where only it carries the words.
    Cyan,
    /// No colour to borrow: the near-grey cursor itself.
    Neutral,
}

impl AccentSource {
    /// The source as `aterm ctl theme` reports it.
    #[inline]
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Authored => "authored",
            Self::System => "system",
            Self::Cursor => "cursor",
            Self::Blue => "blue",
            Self::Cyan => "cyan",
            Self::Neutral => "neutral",
        }
    }
}

/// How far a derived seam steps off its ground toward the theme's ink: enough
/// to read as an edge at 12 px type, not enough to compete with the accent.
pub const SEAM_T: f32 = 0.30;

/// The least contrast a seam stands from the ground it is drawn on: a
/// boundary, not text (the native separator's floor).
pub const SEAM_FLOOR: f64 = aterm_types::scheme::MIN_BORDER_CONTRAST;

/// The non-text floor an accent mark is held to on its ground (WCAG 1.4.11).
pub const ACCENT_FLOOR: f64 = aterm_types::scheme::MIN_ACCENT_CONTRAST;

/// The least contrast a search-match tint stands from the background: the
/// step that makes "this cell is a match" visible as a surface change.
pub const MATCH_RISE: f64 = aterm_types::scheme::MIN_SEARCH_MATCH_CONTRAST;

/// The least `OkLab` distance between a search-match tint and the selection
/// the current match is painted in, so the current match reads as a different
/// mark from the others. Each floor here is the theme checker's own constant,
/// so what `aterm theme check` warns below is what the chrome raises to.
pub const MATCH_DISTINCT_OKLAB: f64 = aterm_types::scheme::MIN_SEARCH_MATCH_DISTANCE;

/// The tint ramp: twentieths of the way from the background toward its seed,
/// up to twelve of them (0.6). Past that the tint stops reading as a tint of
/// the background and starts competing with the text on it.
const MATCH_STEPS: (u8, f32) = (12, 20.0);

/// The shared chrome colours of one theme ([`ChromeTokens::derive`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromeTokens {
    /// Whether the background is light ([`bg_is_light`]): the one polarity.
    pub light: bool,
    /// The theme's background.
    pub bg: [u8; 3],
    /// The theme's foreground.
    pub fg: [u8; 3],
    /// The theme's cursor.
    pub cursor: [u8; 3],
    /// The band's ground for the seeds' [`BarBase`]: the fixed platform tone
    /// for the polarity, else the background a step toward the ink (0.10 on a
    /// light theme, 0.16 on a dark one).
    pub bar_bg: [u8; 3],
    /// The accent hue, UNFLOORED: each surface floors it on its own ground
    /// ([`Self::accent_on`]).
    pub accent: [u8; 3],
    /// Where [`Self::accent`] came from.
    pub accent_source: AccentSource,
    /// The authored seam colour, if any ([`Self::seam_on`] floors it).
    pub border: Option<[u8; 3]>,
    /// The authored active-tab background, if any.
    pub active_tab: Option<[u8; 3]>,
    /// The `OkLCh` hue (degrees) of the theme's ANSI red, when it has colour
    /// ([`BORROW_CHROMA`]): the seed of a danger tone.
    pub danger_hue: Option<f64>,
    /// …of its ANSI green: the seed of a success tone.
    pub success_hue: Option<f64>,
    /// The terminal palette's hues the tokens were derived with.
    pub ansi: Option<AnsiHues>,
    /// The authored search-match tint, if any ([`Self::search_match`]).
    authored_search: Option<[u8; 3]>,
}

impl ChromeTokens {
    /// The tokens of one theme. Pure: the same seeds give the same tokens.
    #[must_use]
    pub fn derive(s: &ChromeSeeds) -> Self {
        let light = bg_is_light(s.bg);
        let bar_bg = match s.base {
            BarBase::Fixed { light: l, dark: d } => {
                if light {
                    l
                } else {
                    d
                }
            }
            BarBase::Blend => mix3(s.bg, s.fg, if light { 0.10 } else { 0.16 }),
        };
        let (accent, accent_source) = if let Some(authored) = s.authored.accent {
            (authored, AccentSource::Authored)
        } else if let Some(system) = s.system_accent {
            (system, AccentSource::System)
        } else if !near_grey(s.cursor) {
            (s.cursor, AccentSource::Cursor)
        } else if let Some(a) = s.ansi {
            // The words the band writes on its meter, exactly as the band
            // passes them, so the accent and the meter pick the same hue.
            let words = [s.bg, bar_bg, ensure_contrast(s.fg, bar_bg, 4.5)];
            match MeterHue::pick(s.cursor, a.blue, a.cyan, &words) {
                MeterHue::Blue => (a.blue, AccentSource::Blue),
                MeterHue::Cyan => (a.cyan, AccentSource::Cyan),
                MeterHue::Cursor => (s.cursor, AccentSource::Neutral),
            }
        } else {
            (s.cursor, AccentSource::Neutral)
        };
        let hue_of = |c: [u8; 3]| (chroma(c) >= BORROW_CHROMA).then(|| oklch(c).2);
        Self {
            light,
            bg: s.bg,
            fg: s.fg,
            cursor: s.cursor,
            bar_bg,
            accent,
            accent_source,
            border: s.authored.border,
            active_tab: s.authored.active_tab,
            danger_hue: s.ansi.and_then(|a| hue_of(a.red)),
            success_hue: s.ansi.and_then(|a| hue_of(a.green)),
            ansi: s.ansi,
            authored_search: s.authored.search_match,
        }
    }

    /// The accent floored to `floor`:1 on `ground`.
    #[inline]
    #[must_use]
    pub fn accent_on(&self, ground: [u8; 3], floor: f64) -> [u8; 3] {
        ensure_contrast(self.accent, ground, floor)
    }

    /// The seam drawn on `ground`: the authored `border_color`, else `ground`
    /// stepped [`SEAM_T`] toward the foreground; either held to
    /// [`SEAM_FLOOR`] on `ground`.
    #[inline]
    #[must_use]
    pub fn seam_on(&self, ground: [u8; 3]) -> [u8; 3] {
        let seam = self.border.unwrap_or_else(|| mix3(ground, self.fg, SEAM_T));
        ensure_contrast(seam, ground, SEAM_FLOOR)
    }

    /// The authored search-match tint, if any.
    #[inline]
    #[must_use]
    pub const fn authored_search_match(&self) -> Option<[u8; 3]> {
        self.authored_search
    }

    /// Find's highlight on every visible match but the current one, which is
    /// the live selection and is painted in `selection`.
    ///
    /// Authored, it is that colour lifted to [`MATCH_RISE`]:1 off the
    /// background (the author chose it; `aterm theme check` warns when it sits
    /// on the selection). Derived, it is the background stepped toward a seed
    /// until it stands [`MATCH_RISE`]:1 off the background and
    /// [`MATCH_DISTINCT_OKLAB`] from the selection. The seed is the accent
    /// floored to [`ACCENT_FLOOR`] on the background (so an accent that hugs
    /// the background still has somewhere to go), then the foreground for a
    /// theme whose accent sits on its selection. Where no seed satisfies both,
    /// the accent's first tint that clears the rise wins: visible beats
    /// distinct.
    #[must_use]
    pub fn search_match(&self, selection: [u8; 3]) -> [u8; 3] {
        let bg = self.bg;
        if let Some(authored) = self.authored_search {
            return ensure_contrast(authored, bg, MATCH_RISE);
        }
        let accent = self.accent_on(bg, ACCENT_FLOOR);
        let (steps, per) = MATCH_STEPS;
        let ramp = |seed: [u8; 3]| {
            (1..=steps)
                .map(move |step| mix3(bg, seed, f32::from(step) / per))
                .filter(move |tint| contrast(*tint, bg) >= MATCH_RISE)
        };
        let distinct = |tint: &[u8; 3]| oklab_distance(*tint, selection) >= MATCH_DISTINCT_OKLAB;
        [accent, self.fg]
            .into_iter()
            .find_map(|seed| ramp(seed).find(distinct))
            .or_else(|| ramp(accent).next())
            .unwrap_or_else(|| {
                let deepest = mix3(bg, accent, f32::from(steps) / per);
                ensure_contrast(deepest, bg, MATCH_RISE)
            })
    }
}
