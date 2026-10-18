// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The chrome tokens ([`crate::tokens`]) on every built-in theme.

use crate::ink::{
    AnsiHues, BandInks, BarBase, TRACK_TINT, ThemeInks, contrast, ensure_contrast, keep_side, mix3,
};
use crate::palette::{
    BORROW_CHROMA, MeterHue, chroma, keeps_pastel, near_grey, oklab_distance, oklch,
    outline_borrows_blue, pastel_edge,
};
use crate::tokens::{
    AccentSource, AuthoredChrome, ChromeSeeds, ChromeTokens, MATCH_DISTINCT_OKLAB, MATCH_RISE,
    SEAM_FLOOR, SEAM_T,
};

fn bytes(c: aterm_types::Rgb) -> [u8; 3] {
    [c.r, c.g, c.b]
}

/// One built-in: its theme inks, ANSI hues and selection band.
struct Builtin {
    name: &'static str,
    theme: ThemeInks,
    ansi: AnsiHues,
    selection: [u8; 3],
}

fn builtins() -> Vec<Builtin> {
    let names = aterm_types::scheme::builtin_names();
    assert!(names.len() > 2, "CONTROL: built-ins to sweep");
    names
        .iter()
        .map(|&name| {
            let s = aterm_types::scheme::builtin(name).expect("a listed built-in");
            let fg = bytes(s.foreground);
            Builtin {
                name,
                theme: ThemeInks {
                    bg: bytes(s.background),
                    fg,
                    cursor: s.cursor.map_or(fg, bytes),
                },
                ansi: AnsiHues {
                    red: bytes(s.ansi[1]),
                    green: bytes(s.ansi[2]),
                    yellow: bytes(s.ansi[3]),
                    blue: bytes(s.ansi[4]),
                    cyan: bytes(s.ansi[6]),
                },
                selection: bytes(s.selection_band()),
            }
        })
        .collect()
}

/// The two band grounds a host draws: the blend, and Linux's CSD greys.
const BASES: [BarBase; 2] = [
    BarBase::Blend,
    BarBase::Fixed {
        light: [0xeb; 3],
        dark: [0x30; 3],
    },
];

fn seeds(theme: ThemeInks, ansi: Option<AnsiHues>, authored: AuthoredChrome) -> ChromeSeeds {
    ChromeSeeds {
        bg: theme.bg,
        fg: theme.fg,
        cursor: theme.cursor,
        ansi,
        authored,
        base: BarBase::Blend,
    }
}

fn tokens(theme: ThemeInks, ansi: Option<AnsiHues>, authored: AuthoredChrome) -> ChromeTokens {
    ChromeTokens::derive(&seeds(theme, ansi, authored))
}

/// THE ACCENT FOLLOWS THE THEME: a cursor with a colour is the accent; a
/// near-grey one borrows the ANSI blue or cyan, and the accent is exactly that
/// slot; an authored accent wins over either. NEGATIVE CONTROL: a theme whose
/// cursor and ANSI blue and cyan are all grey has no colour to borrow, so its
/// accent is Neutral (the grey cursor), not Blue.
#[test]
fn accent_rule_follows_the_theme() {
    let mut seen = [false; 3];
    for b in builtins() {
        let t = tokens(b.theme, Some(b.ansi), AuthoredChrome::NONE);
        if near_grey(b.theme.cursor) {
            match t.accent_source {
                AccentSource::Blue => assert_eq!(t.accent, b.ansi.blue, "{}", b.name),
                AccentSource::Cyan => assert_eq!(t.accent, b.ansi.cyan, "{}", b.name),
                other => panic!("{}: a grey cursor borrows, got {other:?}", b.name),
            }
            seen[1] = true;
        } else {
            assert_eq!(t.accent_source, AccentSource::Cursor, "{}", b.name);
            assert_eq!(t.accent, b.theme.cursor, "{}", b.name);
            seen[0] = true;
        }
        let authored = AuthoredChrome {
            accent: Some([0xff, 0x00, 0xaa]),
            ..AuthoredChrome::NONE
        };
        let a = tokens(b.theme, Some(b.ansi), authored);
        assert_eq!(
            (a.accent_source, a.accent),
            (AccentSource::Authored, [0xff, 0x00, 0xaa]),
            "{}",
            b.name
        );
        // Without the palette a grey cursor has nothing to borrow.
        let bare = tokens(b.theme, None, AuthoredChrome::NONE);
        if near_grey(b.theme.cursor) {
            assert_eq!(bare.accent_source, AccentSource::Neutral, "{}", b.name);
            seen[2] = true;
        }
    }
    assert_eq!(seen, [true; 3], "CONTROL: every branch is exercised");
    let mono_theme = ThemeInks {
        bg: [0x10; 3],
        fg: [0xd0; 3],
        cursor: [0xc0; 3],
    };
    let mono = AnsiHues {
        red: [0x60; 3],
        green: [0x70; 3],
        yellow: [0x80; 3],
        blue: [0x90; 3],
        cyan: [0xa0; 3],
    };
    let t = tokens(mono_theme, Some(mono), AuthoredChrome::NONE);
    assert_eq!(
        (t.accent_source, t.accent),
        (AccentSource::Neutral, [0xc0; 3])
    );
}

/// THE BAND'S METER IS THE TOKEN ACCENT, and the parity proof: on every
/// built-in, on both band grounds, the meter, its ring and its edge are what
/// the band derived before the tokens existed, computed here from the palette
/// primitives (ruling 250's pick, ruling 260's outline, ruling 264's pastel);
/// and the accent is that same hue, floored.
#[test]
fn band_meter_is_the_token_accent() {
    for b in builtins() {
        let (theme, a) = (b.theme, b.ansi);
        for base in BASES {
            let c = BandInks::derive(theme, Some(a), base);
            let bar_bg = c.bar_bg;
            let value = ensure_contrast(theme.fg, bar_bg, 4.5);
            let hue = MeterHue::pick(theme.cursor, a.blue, a.cyan, &[theme.bg, bar_bg, value]).of(
                theme.cursor,
                a.blue,
                a.cyan,
            );
            let floored = ensure_contrast(hue, bar_bg, 3.0);
            let pastel = keeps_pastel(hue, floored);
            let meter = if pastel { hue } else { floored };
            let blue = ensure_contrast(a.blue, bar_bg, 3.0);
            let ring = if outline_borrows_blue(
                keep_side(floored, bar_bg, 4.5),
                blue,
                keep_side(blue, bar_bg, 4.5),
            ) {
                blue
            } else {
                floored
            };
            let edge = pastel.then(|| pastel_edge(hue, mix3(bar_bg, hue, TRACK_TINT)));
            assert_eq!(c.meter, meter, "{} {base:?}: meter", b.name);
            assert_eq!(c.ring, ring, "{} {base:?}: ring", b.name);
            assert_eq!(c.meter_edge, edge, "{} {base:?}: edge", b.name);
            assert_eq!(c.accent, floored, "{} {base:?}: one accent", b.name);
        }
    }
}

/// THE BAND'S SEAM IS ITS LABEL UNLESS A BORDER IS AUTHORED: unauthored, the
/// hairline over a panel keeps the label tone it always had; an authored
/// border is that border floored on the band, which is not the label.
#[test]
fn band_seam_is_the_label_unless_a_border_is_authored() {
    for b in builtins() {
        let c = BandInks::derive(b.theme, Some(b.ansi), BarBase::Blend);
        assert_eq!(c.seam, c.label, "{}", b.name);
    }
    let base = &builtins()[0];
    let authored = AuthoredChrome {
        border: Some([0x80; 3]),
        ..AuthoredChrome::NONE
    };
    let t = tokens(base.theme, Some(base.ansi), authored);
    let c = BandInks::from_tokens(base.theme, &t);
    assert_eq!(c.seam, t.seam_on(c.bar_bg));
    assert_ne!(c.seam, c.label, "CONTROL: the border is not the label");
}

/// A SEAM CLEARS ITS FLOOR on every ground, authored or not; unauthored, it
/// is the plain step toward the ink wherever that step already clears it.
#[test]
fn seam_on_clears_its_floor() {
    let mut plain = 0;
    let mut lifted = 0;
    for b in builtins() {
        for authored in [
            AuthoredChrome::NONE,
            AuthoredChrome {
                border: Some(b.theme.bg),
                ..AuthoredChrome::NONE
            },
        ] {
            let t = tokens(b.theme, Some(b.ansi), authored);
            let grounds = (0u8..=51).map(|i| [i * 5; 3]).chain([b.theme.bg, t.bar_bg]);
            for g in grounds {
                let seam = t.seam_on(g);
                assert!(
                    contrast(seam, g) >= SEAM_FLOOR,
                    "{}: seam {seam:02x?} on {g:02x?}",
                    b.name
                );
                let step = mix3(g, b.theme.fg, SEAM_T);
                if authored.border.is_none() && contrast(step, g) >= SEAM_FLOOR {
                    assert_eq!(seam, step, "{}: on {g:02x?}", b.name);
                    plain += 1;
                } else {
                    lifted += 1;
                }
            }
        }
    }
    assert!(plain > 0 && lifted > 0, "CONTROL: both arms exercised");
}

/// FIND'S HIGHLIGHT IS VISIBLE AND APART FROM THE SELECTION: on every
/// built-in the tint stands [`MATCH_RISE`]:1 off the background and
/// [`MATCH_DISTINCT_OKLAB`] from the selection the current match wears. An
/// authored tint equal to the background is lifted to the rise. NEGATIVE
/// CONTROL: an authored tint equal to the selection is kept as authored (no
/// distinctness search overrides the author; `theme check` warns instead).
#[test]
fn search_match_is_visible_and_apart_from_the_selection() {
    for b in builtins() {
        let t = tokens(b.theme, Some(b.ansi), AuthoredChrome::NONE);
        let tint = t.search_match(b.selection);
        let rise = contrast(tint, b.theme.bg);
        assert!(
            rise >= MATCH_RISE,
            "{}: highlight {tint:02x?} is {rise:.2}:1 off the background",
            b.name
        );
        let apart = oklab_distance(tint, b.selection);
        assert!(
            apart >= MATCH_DISTINCT_OKLAB,
            "{}: highlight {tint:02x?} is {apart:.3} from the selection",
            b.name
        );
        let on_bg = tokens(
            b.theme,
            Some(b.ansi),
            AuthoredChrome {
                search_match: Some(b.theme.bg),
                ..AuthoredChrome::NONE
            },
        );
        assert!(contrast(on_bg.search_match(b.selection), b.theme.bg) >= MATCH_RISE);
        let on_selection = tokens(
            b.theme,
            Some(b.ansi),
            AuthoredChrome {
                search_match: Some(b.selection),
                ..AuthoredChrome::NONE
            },
        );
        assert_eq!(
            on_selection.search_match(b.selection),
            ensure_contrast(b.selection, b.theme.bg, MATCH_RISE),
            "{}",
            b.name
        );
    }
}

/// THE STATUS HUES COME FROM THE PALETTE: a chromatic ANSI red or green is
/// a hue; a grey slot is none.
#[test]
fn status_hues_come_from_the_palette() {
    let mut chromatic = 0;
    for b in builtins() {
        let t = tokens(b.theme, Some(b.ansi), AuthoredChrome::NONE);
        for (slot, hue) in [(b.ansi.red, t.danger_hue), (b.ansi.green, t.success_hue)] {
            if chroma(slot) >= BORROW_CHROMA {
                assert_eq!(hue, Some(oklch(slot).2), "{}", b.name);
                chromatic += 1;
            } else {
                assert_eq!(hue, None, "{}", b.name);
            }
        }
        let bare = tokens(b.theme, None, AuthoredChrome::NONE);
        assert_eq!((bare.danger_hue, bare.success_hue), (None, None));
    }
    assert!(chromatic > 0, "CONTROL: built-ins with a coloured red");
    let theme = builtins()[0].theme;
    let grey = AnsiHues {
        red: [0x80; 3],
        green: [0x90; 3],
        yellow: [0xa0; 3],
        blue: [0xb0; 3],
        cyan: [0xc0; 3],
    };
    let t = tokens(theme, Some(grey), AuthoredChrome::NONE);
    assert_eq!((t.danger_hue, t.success_hue), (None, None));
}
