// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared themed cells for compact in-grid chrome (find bar, message band, native-tab rows).

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

/// The published palette, packed `0x00RRGGBB`, in [`ForcedChrome`] field order.
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

/// Pack one RGB triple into `0x00RRGGBB`.
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

/// On-theme tones for compact chrome bands ([`aterm_messages::ink::BandInks`]).
pub(crate) type BandColors = aterm_messages::ink::BandInks;

#[cfg(test)]
pub(crate) use aterm_messages::ink::{PRIMARY_HOVER_LIFT, TRACK_TINT, hover_ink};
pub(crate) use aterm_messages::ink::{contrast, ensure_contrast, forced_ink, mix3, warn_mark};

/// A packed `0x00RRGGBB` theme colour as sRGB bytes.
fn rgb(c: u32) -> [u8; 3] {
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
    /// The band's colours for this palette ([`band_colors`]).
    pub(crate) fn colors(self) -> BandColors {
        band_colors(self)
    }

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
            base: bar_base(),
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

/// The base this thread's bands are derived on: the platform's
/// ([`PLATFORM_BAR_BASE`]), or in a test the one
/// [`lay_bands_on_the_theme_blend_for_test`] chose.
fn bar_base() -> BarBase {
    #[cfg(test)]
    if let Some(base) = BAR_BASE_FOR_TEST.get() {
        return base;
    }
    PLATFORM_BAR_BASE
}

#[cfg(test)]
std::thread_local! {
    /// This test thread's band base in place of the platform's ([`bar_base`]).
    static BAR_BASE_FOR_TEST: std::cell::Cell<Option<BarBase>> =
        const { std::cell::Cell::new(None) };
}

/// Derive this test thread's bands on the theme blend.
#[cfg(test)]
pub(crate) fn lay_bands_on_the_theme_blend_for_test() {
    BAR_BASE_FOR_TEST.set(Some(BarBase::Blend));
}

/// Where this platform's band ground comes from.
#[cfg(target_os = "linux")]
pub(crate) const PLATFORM_BAR_BASE: BarBase = BarBase::Fixed {
    light: CSD_HEADERBAR_LIGHT,
    dark: CSD_HEADERBAR_DARK,
};
#[cfg(not(target_os = "linux"))]
pub(crate) const PLATFORM_BAR_BASE: BarBase = BarBase::Blend;

/// The presence tones: the needs border's two alarm hues and the person's dim outline.
pub(crate) use aterm_messages::presence::PresenceTones;

/// The presence tones for `theme` on its band.
pub(crate) fn presence_tones(p: impl Into<ChromePalette>) -> PresenceTones {
    let p = p.into();
    aterm_messages::presence::tones(rgb(p.theme.bg), &band_colors(p), forced_chrome())
}

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
pub(crate) mod hc_fixtures {
    use super::ForcedChrome;

    /// The four stock Windows High Contrast schemes as `GetSysColor` reports them.
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

    /// Install `palette` for `body` on this test thread, restoring the previous value.
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

    /// Under every stock High Contrast scheme the band's ink clears AA against the
    /// band and the find bar's caret clears it against the well, so a remapping that
    /// pairs ink with the wrong surface fails.
    #[test]
    fn forced_band_colors_meet_wcag_aa_on_every_stock_high_contrast_scheme() {
        for (name, palette) in hc_fixtures::STOCK {
            hc_fixtures::with_forced(palette, || {
                let hostile = Theme {
                    fg: 0x0011_1318,
                    bg: 0x0011_1318,
                    cursor: 0x0011_1318,
                    selection: 0x0011_1318,
                };
                let colors = band_colors(hostile);
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

    /// A palette move consumed by another reader still reaches the settling
    /// re-sample: `AppRt::native_appearance_preferences` republishes the palette at
    /// window attach and startup, and must not eat the edge the settings `Wake` needs.
    #[test]
    fn a_consumed_palette_edge_still_reaches_the_resample() {
        let previous = forced_chrome();
        let _ = crate::native_appearance::take_chrome_inputs_moved();

        assert!(install_forced_chrome(Some(hc_fixtures::STOCK[0].1)));
        assert!(
            !install_forced_chrome(Some(hc_fixtures::STOCK[0].1)),
            "the edge is gone: a second install reports no movement"
        );
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

    /// Covers the shipping bit-packing: `install_forced_chrome`/`forced_chrome` use a
    /// thread-local under `cfg(test)`, so the five-slot atomic codec needs its own test.
    #[test]
    fn the_shipping_palette_codec_round_trips_every_channel() {
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
        assert!(
            (0..=0xFF).all(|v| pack([v, v, v]) != FORCED_ABSENT),
            "FORCED_ABSENT must not collide with any packed colour"
        );
    }

    /// The publication protocol order: the gate slot closes first, the four tail slots
    /// follow, and the gate reopens last with the real `window`.
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

    /// The shipping codec on the real atomics: each field lands in its own slot, and
    /// the gate slot alone decides whether a palette is visible.
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

    /// On Linux the band's base tone is the Adwaita headerbar's (`#303030` dark,
    /// `#EBEBEB` light) for every theme of that appearance: the band is the header of
    /// a window the compositor leaves undecorated.
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
            let theme = Theme::from(&scheme);
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

    /// The meter's floor: the fill is the theme's cursor accent (or `warn` on a
    /// Warn/Error row), floored against the band, and the glint is one fixed
    /// perceptual step of that fill away from its words' ink, so the words only gain
    /// contrast under it and the hot head clears 3:1 on the track wherever the fill
    /// does.
    fn meter_floors(name: &str, colors: &BandColors) {
        for fill in [colors.accent, colors.warn] {
            let inks = crate::message_band::MeterInks::of(colors, fill);
            let comet = crate::message_band::MeterInks::comet(colors, fill).0;
            let head = crate::message_band::tone_rgb(aterm_messages::Tone::HEAD, &comet);
            let words = crate::message_band::fill_ink(colors, fill);
            assert_eq!(
                inks.glint,
                crate::message_band::glint_step(fill, words, false),
                "{name}"
            );
            assert!(
                contrast(ensure_contrast(colors.label, fill, 4.5), fill) >= 4.5,
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

    /// THE TONES ARE KEPT FOR THE SAME THEME (`band_colors`): asked again
    /// for the theme last asked for, nothing is derived and the tones are the
    /// derivation's exactly; every other theme, the same theme with the
    /// meter's hues, and the first theme again each derive anew, their tones
    /// their own.
    #[test]
    fn band_tones_are_derived_once_per_theme() {
        let derive = |theme: Theme, ansi: Option<MeterAnsi>| {
            BandColors::derive(
                ThemeInks {
                    bg: rgb(theme.bg),
                    fg: rgb(theme.fg),
                    cursor: rgb(theme.cursor),
                },
                ansi,
                PLATFORM_BAR_BASE,
            )
        };
        let themes: Vec<Theme> = aterm_types::scheme::builtin_names()
            .iter()
            .map(|name| Theme::from(&aterm_types::scheme::builtin(name).expect("a scheme")))
            .collect();
        assert!(themes.len() > 2, "CONTROL: themes to tell apart");
        let ansi = MeterAnsi {
            red: [200, 40, 40],
            green: [40, 200, 40],
            yellow: [200, 200, 40],
            blue: [40, 40, 200],
            cyan: [40, 200, 200],
        };
        let _ = crate::work_counts::take_band_derives();
        for &theme in themes.iter().chain(&themes[..1]) {
            let colors = band_colors(theme);
            assert_eq!(colors, derive(theme, None), "the derivation's own tones");
            assert_eq!(
                crate::work_counts::take_band_derives(),
                1,
                "CONTROL: derived"
            );
            for _ in 0..3 {
                assert_eq!(band_colors(theme), colors, "kept");
            }
            assert_eq!(
                crate::work_counts::take_band_derives(),
                0,
                "kept, not derived"
            );
            let hued = band_colors(ChromePalette {
                theme,
                ansi: Some(ansi),
                authored: AuthoredChrome::NONE,
            });
            assert_eq!(
                hued,
                derive(theme, Some(ansi)),
                "the hues are the key's too"
            );
            assert_eq!(crate::work_counts::take_band_derives(), 1);
            let _ = band_colors(ChromePalette {
                theme,
                ansi: Some(ansi),
                authored: AuthoredChrome {
                    border: Some([0x80; 3]),
                    ..AuthoredChrome::NONE
                },
            });
            assert_eq!(crate::work_counts::take_band_derives(), 1);
        }
        assert_ne!(
            band_colors(themes[0]),
            band_colors(themes[1]),
            "CONTROL: two themes' tones differ"
        );
    }

    #[test]
    fn band_colors_meet_wcag_aa_on_every_builtin_scheme() {
        assert_eq!(
            forced_chrome(),
            None,
            "theme-derived band tones are only defined with no OS-forced palette"
        );
        for name in aterm_types::scheme::builtin_names() {
            let scheme = aterm_types::scheme::builtin(name).expect("listed scheme exists");
            let theme = Theme::from(&scheme);
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
            assert!(
                contrast(colors.caret, colors.field_bg) >= 4.5,
                "{name} caret must meet WCAG-AA in the well"
            );
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
            meter_floors(name, &colors);
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
                let lifted = mix3(colors.accent, rgb(theme.fg), PRIMARY_HOVER_LIFT);
                for (c, &want) in lifted.iter().enumerate() {
                    let (a, f, lit, want) = (
                        f32::from(colors.accent[c]),
                        f32::from(rgb(theme.fg)[c]),
                        f32::from(colors.capsule_primary_hover[c]),
                        f32::from(want),
                    );
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
            if contrast(colors.value, mix3(colors.bar_bg, rgb(theme.fg), 0.05)) >= 4.5 {
                assert_ne!(
                    colors.capsule_hover, colors.bar_bg,
                    "{name}: the hover fill must move off the band"
                );
            }
        }
    }

    /// The fault wash stays in its hull: a Fault echo cross-fades the fill's coverage to
    /// warn (premultiplied, linear light), so on every builtin scheme and stock High
    /// Contrast palette, for both fills a row can wear (accent, warn), each step lies
    /// inside the track–fill–warn triangle with the fill (or track) and warn as its ends.
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
            let theme = Theme::from(&scheme);
            check(name, &band_colors(theme));
        }
        for (name, palette) in hc_fixtures::STOCK {
            hc_fixtures::with_forced(palette, || check(name, &band_colors(Theme::default())));
        }
    }
}
