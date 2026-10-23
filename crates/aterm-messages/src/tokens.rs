// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Semantic chrome color tokens derived from theme seeds and user overrides.

use crate::ink::{AnsiHues, BarBase, bg_is_light, contrast, ensure_contrast, mix3};
use crate::palette::{BORROW_CHROMA, MeterHue, chroma, near_grey, oklab_distance, oklch};

/// User- or theme-authored chrome color overrides.
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
    /// Operating-system accent color when the host follows it.
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

/// Interpolation step from a surface's ground toward the theme foreground for a derived seam.
pub const SEAM_T: f32 = 0.30;

/// Minimum contrast ratio for a seam against its ground.
pub const SEAM_FLOOR: f64 = aterm_types::scheme::MIN_BORDER_CONTRAST;

/// The non-text floor an accent mark is held to on its ground (WCAG 1.4.11).
pub const ACCENT_FLOOR: f64 = aterm_types::scheme::MIN_ACCENT_CONTRAST;

/// Minimum contrast ratio for a search-match tint against the background.
pub const MATCH_RISE: f64 = aterm_types::scheme::MIN_SEARCH_MATCH_CONTRAST;

/// Minimum `OkLab` distance between a search-match tint and the selection color.
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
    /// Message-band ground color for the seeds' [`BarBase`].
    pub bar_bg: [u8; 3],
    /// Unfloored accent color; surfaces floor it via [`Self::accent_on`].
    pub accent: [u8; 3],
    /// Where [`Self::accent`] came from.
    pub accent_source: AccentSource,
    /// The authored seam colour, if any ([`Self::seam_on`] floors it).
    pub border: Option<[u8; 3]>,
    /// The authored active-tab background, if any.
    pub active_tab: Option<[u8; 3]>,
    /// `OkLCh` hue (in degrees) of the theme's ANSI red when chromatic.
    pub danger_hue: Option<f64>,
    /// `OkLCh` hue (in degrees) of the theme's ANSI green when chromatic.
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

    /// Returns the seam color on `ground`, floored to [`SEAM_FLOOR`].
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

    /// Returns the find highlight color for non-current matches against `selection`.
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
