// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared themed cells for compact in-grid chrome (find bar, native-tab rows).

#[cfg(test)]
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

use aterm_core::terminal::RenderCell;
use aterm_messages::ink::{AnsiHues, BarBase, ThemeInks};
use aterm_messages::tokens::{AuthoredChrome, ChromeSeeds, ChromeTokens};
use aterm_render::Theme;

/// The OS-forced chrome palette.
pub(crate) type ForcedChrome = aterm_messages::ink::ForcedPalette;

/// Sentinel in slot 0 for "no forced palette" — an RGB triple packs to 24 bits, so
/// `u32::MAX` cannot collide with a real colour.
const FORCED_ABSENT: u32 = u32::MAX;

/// The published palette, packed `0x00RRGGBB`, in [`ForcedChrome`] field order. Atomics
/// rather than a `Mutex<Option<_>>`, since `band_colors` / `strip_colors` run on paint
/// paths.
static FORCED_CHROME: [AtomicU32; 5] = [
    AtomicU32::new(FORCED_ABSENT),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

#[cfg(test)]
thread_local! {
    static TEST_FORCED_CHROME: Cell<Option<ForcedChrome>> = const { Cell::new(None) };
}

/// Publish (or retract, with `None`) the OS-forced chrome palette.
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

/// The live OS-forced chrome palette, or `None` when the app owns its own tones.
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
/// [`FORCED_CHROME`] under the publication protocol.
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

/// The publication protocol as data: the `(slot, value)` stores in write order (gate
/// slot 0 to `FORCED_ABSENT` first, the four tail slots, then the gate with the real
/// `window` last), so a test can read the order back.
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

/// The shipping half of [`forced_chrome`]; not `cfg`-gated, because every platform reads.
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

const fn unpack(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

/// On-theme tones for compact chrome bands ([`aterm_messages::ink::BandInks`]).
pub(crate) type BandColors = aterm_messages::ink::BandInks;

pub(crate) use aterm_messages::ink::{contrast, ensure_contrast, forced_ink, mix3, warn_mark};

/// A packed `0x00RRGGBB` theme colour as sRGB bytes.
const fn rgb(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

/// The Adwaita headerbar fills of the band on Linux.
#[cfg(target_os = "linux")]
pub(crate) const CSD_HEADERBAR_DARK: [u8; 3] = [0x30, 0x30, 0x30];
#[cfg(target_os = "linux")]
pub(crate) const CSD_HEADERBAR_LIGHT: [u8; 3] = [0xEB, 0xEB, 0xEB];

/// Appearance-aware, theme-derived band tones with WCAG-AA text contrast.
pub(crate) fn band_colors(p: impl Into<ChromePalette>) -> BandColors {
    if let Some(hc) = forced_chrome() {
        return BandColors::forced(hc);
    }
    let p = p.into();
    let inks = p.inks();
    let seeds = p.seeds();
    std::thread_local! {
        static LAST: std::cell::Cell<Option<(ChromeSeeds, ThemeInks, BandColors)>> =
            const { std::cell::Cell::new(None) };
    }
    LAST.with(|last| match last.get() {
        Some((was, was_inks, colors)) if was == seeds && was_inks == inks => colors,
        _ => {
            #[cfg(test)]
            crate::work_counts::band_derived();
            let colors = BandColors::from_tokens(inks, &p.tokens());
            last.set(Some((seeds, inks, colors)));
            colors
        }
    })
}

/// The theme's ANSI hues the band borrows.
pub(crate) type MeterAnsi = AnsiHues;

/// A scheme's borrowed hues.
#[cfg(test)]
pub(crate) fn ansi_of_scheme(s: &aterm_types::ColorScheme) -> MeterAnsi {
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    AnsiHues {
        red: rgb(s.ansi[1]),
        green: rgb(s.ansi[2]),
        yellow: rgb(s.ansi[3]),
        blue: rgb(s.ansi[4]),
        cyan: rgb(s.ansi[6]),
    }
}

/// A terminal palette's borrowed hues.
pub(crate) fn ansi_of_palette(p: &aterm_types::ColorPalette) -> MeterAnsi {
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    AnsiHues {
        red: rgb(p.get(1)),
        green: rgb(p.get(2)),
        yellow: rgb(p.get(3)),
        blue: rgb(p.get(4)),
        cyan: rgb(p.get(6)),
    }
}

/// What every chrome surface paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ChromePalette {
    pub theme: Theme,
    pub ansi: Option<MeterAnsi>,
    pub authored: AuthoredChrome,
}

impl From<Theme> for ChromePalette {
    fn from(theme: Theme) -> Self {
        Self {
            theme,
            ansi: None,
            authored: AuthoredChrome::NONE,
        }
    }
}

impl ChromePalette {
    /// The theme's background, foreground and cursor as bytes.
    pub(crate) fn inks(&self) -> ThemeInks {
        ThemeInks {
            bg: rgb(self.theme.bg),
            fg: rgb(self.theme.fg),
            cursor: rgb(self.theme.cursor),
        }
    }

    /// Seeds from which chrome tokens are derived.
    pub(crate) fn seeds(&self) -> ChromeSeeds {
        let inks = self.inks();
        ChromeSeeds {
            bg: inks.bg,
            fg: inks.fg,
            cursor: inks.cursor,
            ansi: self.ansi,
            authored: self.authored,
            system_accent: crate::native_appearance::system_accent()
                .map(|accent| accent.for_surface(inks.bg)),
            base: PLATFORM_BAR_BASE,
        }
    }

    /// The chrome tokens of this palette (`aterm_messages::tokens`).
    pub(crate) fn tokens(&self) -> ChromeTokens {
        let seeds = self.seeds();
        std::thread_local! {
            static LAST: std::cell::Cell<Option<(ChromeSeeds, ChromeTokens)>> =
                const { std::cell::Cell::new(None) };
        }
        LAST.with(|last| match last.get() {
            Some((was, tokens)) if was == seeds => tokens,
            _ => {
                let tokens = ChromeTokens::derive(&seeds);
                last.set(Some((seeds, tokens)));
                tokens
            }
        })
    }
}

/// A theme's authored chrome colours as the tokens take them.
pub(crate) fn authored_of(c: &aterm_types::scheme::ChromeColors) -> AuthoredChrome {
    let bytes = |c: Option<aterm_types::Rgb>| c.map(|c| [c.r, c.g, c.b]);
    AuthoredChrome {
        accent: bytes(c.accent),
        active_tab: bytes(c.active_tab),
        border: bytes(c.border),
        search_match: bytes(c.search_match),
    }
}

/// Where this platform's band ground comes from.
#[cfg(target_os = "linux")]
pub(crate) const PLATFORM_BAR_BASE: BarBase = BarBase::Fixed {
    light: CSD_HEADERBAR_LIGHT,
    dark: CSD_HEADERBAR_DARK,
};
#[cfg(not(target_os = "linux"))]
pub(crate) const PLATFORM_BAR_BASE: BarBase = BarBase::Blend;

/// Build one render cell for compact chrome.
pub(crate) use aterm_render::band::cell;

/// A theme-derived blank band cell with a top seam.
#[must_use]
pub(crate) fn blank_cell(p: impl Into<ChromePalette>) -> RenderCell {
    let colors = band_colors(p);
    cell(' ', colors.label, colors.bar_bg, false, true)
}

/// Close a band's content-facing top edge in place, across the whole row in one tone.
pub(crate) fn seal_band_top(row: &mut [RenderCell], ink: [u8; 3]) {
    for cell in row.iter_mut() {
        cell.overline = true;
        cell.overline_color = Some(ink);
    }
}

#[cfg(test)]
#[path = "chrome_band_hc_fixtures_tests.rs"]
pub(crate) mod hc_fixtures;

#[cfg(test)]
#[path = "chrome_band_tests.rs"]
mod tests;
