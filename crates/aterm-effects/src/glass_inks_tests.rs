// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The glass tokens' laws (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §4.1, §6 S4): every
//! token reads on every built-in ground under every palette, Day's letters stay vivid, a hue is
//! the column's alone, the warn ink keeps clear of Claude's status hues and has none without a
//! hue palette, the hero is the launch card's settled hero, and a value reads apart from the
//! quiet words.

use super::*;
use crate::harness_card::{CardFrames, CardScene, HarnessCard, LineAt};
use crate::harness_panel::chroma_hue;
use aterm_types::scheme::{builtin, builtin_names};

/// Every palette the glass wears, and none.
const PALETTES: [Option<CardPalette>; 4] = [
    Some(CardPalette::Cruise),
    Some(CardPalette::Land),
    Some(CardPalette::Off),
    None,
];

/// The palette positions a test reads: both ends and the steps between, as columns of a pane.
fn positions() -> impl Iterator<Item = f32> {
    (0..=40).map(|k| field(k, 41))
}

/// A built-in theme's name, ground and ink.
struct Ground {
    name: &'static str,
    ground: u32,
    ink: u32,
}

fn pack(c: aterm_types::Rgb) -> u32 {
    u32::from_be_bytes([0, c.r, c.g, c.b])
}

fn grounds() -> Vec<Ground> {
    let grounds: Vec<Ground> = builtin_names()
        .into_iter()
        .filter_map(|name| {
            let scheme = builtin(name)?;
            Some(Ground {
                name,
                ground: pack(scheme.background),
                ink: pack(scheme.foreground),
            })
        })
        .collect();
    assert!(grounds.len() >= 17, "{} built-ins", grounds.len());
    grounds
}

fn ground(name: &str) -> Ground {
    grounds()
        .into_iter()
        .find(|g| g.name == name)
        .unwrap_or_else(|| panic!("{name} is a built-in"))
}

/// The greys Claude's rules are read in: two of its own and the one aterm assumes where the
/// grid gives none, a third of the way from the ground to the ink.
fn rule_greys(g: &Ground) -> [u32; 3] {
    [0x0088_8888, 0x0099_9999, mix_rgb(g.ground, g.ink, 0.35)]
}

fn reads(c: u32, ground: u32) -> f32 {
    contrast_ratio(c, ground)
}

fn chroma(c: u32) -> f32 {
    chroma_hue(c).0
}

/// EVERY TOKEN READS ON EVERY GROUND UNDER EVERY PALETTE: over the three palettes and none, on
/// every built-in theme's ground with each rule grey, at every column, the words (lit, value,
/// quiet, a letter, the hero, the warn ink) read at 4.5:1 or more, a value at 7:1, the marks and
/// the track at 3:1, and the projected line (a letter) at 4.5:1; the words stand in one order of
/// loudness, a quiet word quieter than a value and a value never louder than a lit word (so a
/// lit fact never dims), on a pane whose own ink reads under the floor too. CONTROLS: the arc's
/// own light, unfloored, misses the marks' floor on Aterm Day; on Solarized Dark the pane's ink
/// floored to the name floor alone reads quieter than a value.
#[test]
fn every_token_reads_on_every_ground_under_every_palette() {
    let mut checked = 0usize;
    for g in grounds() {
        for rule in rule_greys(&g) {
            for palette in PALETTES {
                let inks = GlassInks::of(palette, g.ground, g.ink, rule);
                let what = format!("{} under {palette:?} on rule {rule:06x}", g.name);
                for (token, c, floor) in [
                    ("lit", inks.lit(), NAME_CONTRAST),
                    ("value", inks.value(), VALUE_CONTRAST),
                    ("quiet", inks.quiet(), NAME_CONTRAST),
                    ("warn", inks.warn(), NAME_CONTRAST),
                    ("track", inks.track(), ICON_CONTRAST),
                ] {
                    let k = reads(c, g.ground);
                    assert!(k >= floor, "{what}: {token} {c:06x} at {k:.2}:1");
                    checked += 1;
                }
                let (lit, value, quiet) = (
                    reads(inks.lit(), g.ground),
                    reads(inks.value(), g.ground),
                    reads(inks.quiet(), g.ground),
                );
                assert!(
                    quiet < value && value <= lit,
                    "{what}: quiet {quiet:.2}:1 < value {value:.2}:1 <= lit {lit:.2}:1"
                );
                for t in positions() {
                    for (token, c, floor) in [
                        ("letter", inks.letter(t), NAME_CONTRAST),
                        ("hero", inks.hero(t), NAME_CONTRAST),
                        ("mark", inks.mark(t), ICON_CONTRAST),
                    ] {
                        let k = reads(c, g.ground);
                        assert!(k >= floor, "{what}: {token}({t:.3}) {c:06x} at {k:.2}:1");
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked >= 17 * 3 * 4 * (5 + 41 * 3), "{checked} tokens");
    // A pane whose own ink reads under the floor on paper: its lit ink is floored to it, and the
    // quiet words stand there too rather than past it.
    let soft = GlassInks::of(None, 0x00FF_FFFF, 0x007A_7A7A, 0x0088_8888);
    assert!(
        reads(0x007A_7A7A, 0x00FF_FFFF) < NAME_CONTRAST,
        "the soft ink reads under"
    );
    assert!(reads(soft.quiet(), 0x00FF_FFFF) >= NAME_CONTRAST);
    assert!(reads(soft.quiet(), 0x00FF_FFFF) <= reads(soft.lit(), 0x00FF_FFFF));

    // CONTROL: the arc's raw light on paper, which the marks floor along its hue.
    let day = ground("Aterm Day");
    let raw = (0..=40)
        .map(|k| CardPalette::Cruise.art(field(k, 41), false))
        .filter(|&c| reads(c, day.ground) < ICON_CONTRAST)
        .count();
    assert!(raw > 0, "CONTROL: the raw arc reads 3:1 everywhere on Day");

    // CONTROL: Solarized Dark's ink reads between the name floor and a value's, so floored to
    // the name floor alone it would read quieter than a value at rest.
    let solarized = ground("Solarized Dark");
    let floored = floor_contrast_keeping_hue(solarized.ink, solarized.ground, NAME_CONTRAST);
    let inks = GlassInks::of(None, solarized.ground, solarized.ink, 0x0088_8888);
    assert!(
        reads(floored, solarized.ground) < reads(inks.value(), solarized.ground),
        "CONTROL: Solarized Dark's floored ink reads as loud as a value"
    );
}

/// The most a Day letter that holds less than [`DAY_CHROMA`] may read on its paper: no deeper
/// than its 4.5:1 floor needs, so the gamut [`day_vivid_floor`] holds it to is the floor's own and
/// not one a letter darkened past it would shrink.
const DAY_DEEPEST: f32 = 5.0;

/// DAY'S LETTERS STAY VIVID: on Aterm Day, under Cruise and Land, every letter and mark keeps an
/// OKLCh chroma of 0.12 or more at every column, or, where sRGB holds less at its hue and the
/// lightness it reads at (a teal at 4.5:1 on white holds about 0.09), nearly the most it holds:
/// vivid, never the washed-out mix the owner ruled against; a letter that holds less than 0.12
/// reads no deeper than its floor needs ([`DAY_DEEPEST`]), so its own lightness does not lower
/// the bar. CONTROL: a letter mixed 40% toward Day's paper falls under it.
#[test]
fn day_letters_stay_vivid() {
    let day = ground("Aterm Day");
    let mut full = 0;
    for palette in [CardPalette::Cruise, CardPalette::Land] {
        let inks = GlassInks::of(Some(palette), day.ground, day.ink, 0x0088_8888);
        for t in positions() {
            for (token, c) in [("letter", inks.letter(t)), ("mark", inks.mark(t))] {
                let (k, floor) = (chroma(c), day_vivid_floor(c));
                assert!(
                    k >= floor,
                    "{palette:?} {token}({t:.3}) {c:06x} at C {k:.3}, under {floor:.3}"
                );
                if k < DAY_CHROMA {
                    let deep = reads(c, day.ground);
                    assert!(
                        deep < DAY_DEEPEST,
                        "{palette:?} {token}({t:.3}) {c:06x} at C {k:.3} reads {deep:.2}:1, \
                         darker than its floor needs"
                    );
                }
                full += usize::from(k >= DAY_CHROMA);
            }
        }
    }
    assert!(
        full > 82,
        "most Day letters hold the full {DAY_CHROMA}: {full} of 164"
    );
    let inks = GlassInks::of(Some(CardPalette::Cruise), day.ground, day.ink, 0x0088_8888);
    let washed = (0..=40)
        .map(|k| mix_rgb(inks.letter(field(k, 41)), day.ground, 0.4))
        .filter(|&c| chroma(c) < day_vivid_floor(c))
        .count();
    assert!(washed > 0, "CONTROL: the washed mix keeps vivid everywhere");
}

/// A HUE IS A FUNCTION OF THE COLUMN: `field` runs from 1 at a pane's first column to 0 at its
/// last, strictly down, and a token at a column is the same whatever else the line lays; under
/// Cruise the letters run the spectrum from violet at the left to red at the right. CONTROL:
/// line B's old gap-relative position gave one column two hues as the gap moved.
#[test]
fn hue_is_a_function_of_the_column() {
    for cols in [2usize, 41, 70, 100, 170] {
        assert_eq!(field(0, cols), 1.0, "{cols}: the first column");
        assert_eq!(field(cols - 1, cols), 0.0, "{cols}: the last column");
        assert_eq!(
            field(cols + 5, cols),
            0.0,
            "{cols}: past the span, its last"
        );
        for col in 1..cols {
            assert!(
                field(col, cols) < field(col - 1, cols),
                "{cols}: down at {col}"
            );
        }
    }
    assert_eq!(field(0, 1), 1.0, "a span of one cell");
    let night = ground("Aterm Night");
    for palette in PALETTES {
        let one = GlassInks::of(palette, night.ground, night.ink, 0x0088_8888);
        let again = GlassInks::solve(one.key());
        for col in 0..100 {
            let t = field(col, 100);
            for (a, b) in [
                (one.rule(t), again.rule(t)),
                (one.letter(t), again.letter(t)),
                (one.mark(t), again.mark(t)),
                (one.hero(t), again.hero(t)),
            ] {
                assert_eq!(a, b, "{palette:?} column {col}");
            }
        }
    }
    let cruise = GlassInks::of(
        Some(CardPalette::Cruise),
        night.ground,
        night.ink,
        0x0088_8888,
    );
    let hue = |c: u32| chroma_hue(c).1;
    let (left, right) = (
        hue(cruise.letter(field(0, 100))),
        hue(cruise.letter(field(99, 100))),
    );
    assert!(
        (260.0..330.0).contains(&left),
        "violet at the left: {left:.0}°"
    );
    assert!(
        !(60.0..330.0).contains(&right),
        "red at the right: {right:.0}°"
    );

    // CONTROL: line B's old position, 1 left of the gap and down to 0 across it.
    let old = |col: usize, gap: std::ops::Range<usize>| {
        if col < gap.start {
            1.0
        } else if gap.contains(&col) {
            1.0 - (col - gap.start) as f32 / (gap.len() - 1) as f32
        } else {
            0.0
        }
    };
    let col = 40;
    assert_ne!(
        cruise.rule(old(col, 10..60)),
        cruise.rule(old(col, 30..90)),
        "CONTROL: the old position gave column {col} one hue whatever the gap"
    );
}

/// OFF AND NO PALETTE WARN WITH NO HUE: under Autopilot Off and with no harness theme the warn
/// ink is the lit ink, adding no hue to the pane's own, on every built-in ground (its shape and
/// its weight say the stop). CONTROL: Cruise's and Land's warn carry a hue.
#[test]
fn off_and_no_palette_warn_has_no_hue() {
    for g in grounds() {
        for palette in [Some(CardPalette::Off), None] {
            let inks = GlassInks::of(palette, g.ground, g.ink, 0x0088_8888);
            assert_eq!(inks.warn(), inks.lit(), "{} under {palette:?}", g.name);
        }
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            let inks = GlassInks::of(Some(palette), g.ground, g.ink, 0x0088_8888);
            let c = chroma(inks.warn());
            assert!(
                c >= 0.08,
                "CONTROL: {} under {palette:?} warns at C {c:.3}",
                g.name
            );
        }
    }
}

/// THE WARN INK KEEPS CLEAR OF CLAUDE'S STATUS HUES: under Cruise and Land, on every built-in
/// ground, the warn ink's OKLCh hue stands [`WARN_HUE_GUARD_DEG`] or more from Claude's success
/// green and error red of that polarity. CONTROL: the spectrum's red, which the design rejected
/// for warn, sits on Claude's error red.
#[test]
fn warn_keeps_clear_of_claudes_status_hues() {
    let mut checked = 0;
    for g in grounds() {
        let dark = dark_ground(g.ground);
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            let inks = GlassInks::of(Some(palette), g.ground, g.ink, 0x0088_8888);
            let gap = status_gap(inks.warn(), dark).expect("a warm warn has a hue");
            assert!(
                gap >= WARN_HUE_GUARD_DEG,
                "{} under {palette:?}: warn {:06x} {gap:.1}° from Claude's status hues",
                g.name,
                inks.warn()
            );
            checked += 1;
        }
    }
    assert!(checked >= 17 * 2);
    let near = grounds()
        .iter()
        .filter(|g| {
            let red = CardPalette::Cruise.letter_on(0.0, g.ground);
            status_gap(red, dark_ground(g.ground)).is_some_and(|gap| gap < WARN_HUE_GUARD_DEG)
        })
        .count();
    assert!(
        near > 0,
        "CONTROL: the spectrum's red clears Claude's error red"
    );
}

/// THE WARN INK FALLS BACK TO ITS CLEAREST STEP: where no step of the walk from the warn's
/// anchor keeps [`WARN_HUE_GUARD_DEG`] from Claude's status hues, the warn ink is the step that
/// stands farthest from them, the first that clears where one does, and a step with no hue
/// clears; over a sweep of sRGB grounds, under Cruise and Land, the warn ink clears the guard or
/// stands as far as any step of its walk. CONTROL: on gaps none of which clears, the anchor (what
/// the walk once fell back to) stands nearer than the step chosen.
#[test]
fn warn_falls_back_to_its_clearest_step() {
    let gaps = |c: u32| match c {
        1 => Some(5.0),
        2 => Some(18.0),
        3 => Some(12.0),
        4 => None,
        5 => Some(30.0),
        _ => Some(0.0),
    };
    let none_clear = clearest(1, [3, 2, 6].into_iter(), gaps);
    assert_eq!(none_clear, 2, "the farthest where none clears");
    assert_eq!(
        clearest(1, [3, 5, 2].into_iter(), gaps),
        5,
        "the first that clears"
    );
    assert_eq!(
        clearest(1, [3, 4, 5].into_iter(), gaps),
        4,
        "no hue keeps clear"
    );
    assert_eq!(
        clearest(5, [2].into_iter(), gaps),
        5,
        "an anchor that clears"
    );
    assert_eq!(clearest(6, [6].into_iter(), gaps), 6, "the first of equals");
    assert!(
        gaps(1) < gaps(none_clear),
        "CONTROL: the anchor stands nearer than the step chosen"
    );

    let mut swept = 0;
    for r in (0..=255u32).step_by(51) {
        for g in (0..=255u32).step_by(51) {
            for b in (0..=255u32).step_by(51) {
                let ground = (r << 16) | (g << 8) | b;
                let dark = dark_ground(ground);
                let far = |c: u32| status_gap(c, dark).unwrap_or(f32::INFINITY);
                for (palette, t) in [
                    (CardPalette::Cruise, WARN_CRUISE_T),
                    (CardPalette::Land, WARN_LAND_T),
                ] {
                    let key = GlassKey {
                        palette: Some(palette),
                        ground,
                        ink: 0x0080_8080,
                        rule: 0x0088_8888,
                    };
                    let warn = GlassInks::solve(key).warn();
                    let most = (0..=WARN_STEPS)
                        .map(|k| far(palette.letter_on(t + f32::from(k) * WARN_STEP, ground)))
                        .fold(f32::NEG_INFINITY, f32::max);
                    assert!(
                        far(warn) >= WARN_HUE_GUARD_DEG || far(warn) == most,
                        "{palette:?} on {ground:06x}: warn {warn:06x} {:.1}° from Claude's status \
                         hues, a step of its walk {most:.1}°",
                        far(warn)
                    );
                    swept += 1;
                }
            }
        }
    }
    assert_eq!(swept, 6 * 6 * 6 * 2);
}

/// THE HERO IS THE CARD'S SETTLED HERO: `hero_on` at each of a hero word's letters is exactly the
/// ink the launch card's status line gives that letter at the settled clock, under every palette,
/// on night, on paper, on a mid grey and on every built-in ground. CONTROL: the letter at the
/// same place is another ink.
#[test]
fn hero_on_is_the_cards_settled_hero() {
    let mut grounds_of: Vec<u32> = vec![0x0012_1721, 0x00F8_FAFD, 0x0092_9292];
    grounds_of.extend(grounds().iter().map(|g| g.ground));
    let hero = 12..19u16;
    let at = LineAt {
        row: 5,
        col: 13,
        len: 40,
        hero: hero.clone(),
        wand: None,
    };
    let mut differs = 0;
    for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
        for &ground in &grounds_of {
            let mut card = HarnessCard::new();
            let scene = CardScene {
                origin_x: 10,
                origin_y: 46,
                cw: 9,
                ch: 18,
                row_base: 2,
                pad_left: 10,
                pad_above: 0,
                pad_below: 0,
                top: 3,
                rows: 4,
                cols: 60,
                trail: None,
                kitty_col: None,
                kitty_rows: 2,
                clip: (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
                palette,
                dark: dark_ground(ground),
                ground,
                serious: false,
                coat: 0x00E8_A060,
                eye: 0x0040_C080,
                wand: None,
            };
            let _ = card.draw(
                aterm_time::Instant::now(),
                &scene,
                false,
                &mut CardFrames::new(),
            );
            let mut out = Vec::new();
            card.line_ink_into(None, &at, ground, false, &mut out);
            let n = hero.end - hero.start;
            for i in hero.clone() {
                let c = out[usize::from(i)].color;
                let drawn = u32::from_be_bytes([0, c[0], c[1], c[2]]);
                let k = f32::from(i - hero.start) / f32::from(n - 1);
                assert_eq!(
                    palette.hero_on(k, ground),
                    drawn,
                    "{palette:?} on {ground:06x}: letter {}",
                    i - hero.start
                );
                differs += usize::from(palette.letter_on(k, ground) != drawn);
            }
        }
    }
    assert!(differs > 0, "CONTROL: the plain letter is the hero's ink");
}

/// VALUES READ APART FROM QUIET WORDS: on every built-in theme's ground, under every palette and
/// none, a value stands [`VALUE_APART_L`] or more of OKLab lightness from the quiet words, toward
/// the pane's ink. CONTROL: the old value ink (the rule grey lifted a third toward the ink,
/// floored to 4.5:1) lands within it of the quiet words on Aterm Night.
#[test]
fn values_read_apart_from_quiet_words() {
    let l = |c: u32| oklab(c)[0];
    for g in grounds() {
        for rule in rule_greys(&g) {
            for palette in PALETTES {
                let inks = GlassInks::of(palette, g.ground, g.ink, rule);
                let toward_ink = if l(inks.lit()) > l(g.ground) {
                    l(inks.value()) - l(inks.quiet())
                } else {
                    l(inks.quiet()) - l(inks.value())
                };
                assert!(
                    toward_ink >= VALUE_APART_L,
                    "{} under {palette:?}: value {:06x} and quiet {:06x} {toward_ink:.3} L apart",
                    g.name,
                    inks.value(),
                    inks.quiet()
                );
            }
        }
    }
    let night = ground("Aterm Night");
    let grey = mix_rgb(night.ground, night.ink, 0.35);
    let old = floor_contrast_keeping_hue(mix_rgb(grey, night.ink, 1.0 / 3.0), night.ground, 4.5);
    let quiet = GlassInks::of(None, night.ground, night.ink, grey).quiet();
    let apart = (l(old) - l(quiet)).abs();
    assert!(
        apart < VALUE_APART_L,
        "CONTROL: the old value {old:06x} reads {apart:.3} L from quiet {quiet:06x}"
    );
}
