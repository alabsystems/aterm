// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account unit's outlook inks (`docs/DESIGN-accounts-outlook-2026-10-25.md` §3.4, T20): the
//! bar's meter and track and a projected stop's words, on the pane's rule in the glass's tokens
//! (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §4.1) under every palette, and on the
//! Accounts card under the macOS accents, over every built-in scheme.

use super::*;
use crate::autopilot_menu::{MenuRow, NoteTone, PlotCell, PlotInk};
use crate::native_appearance::{SystemAccent, install_system_accent};
use aterm_agent::harness::outlook::{BarCell, Glance, VerdictWords};
use aterm_effects::cli_marks::{ICON_CONTRAST, NAME_CONTRAST};
use aterm_effects::color_math::contrast_ratio;
use aterm_effects::glass_inks::day_vivid;
use aterm_types::scheme::{builtin, builtin_names};

fn reads(a: [u8; 3], b: [u8; 3]) -> f32 {
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    contrast_ratio(pack(a), pack(b))
}

fn chroma(c: [u8; 3]) -> f64 {
    aterm_messages::palette::oklch(c).1
}

/// The system accents macOS offers (System Settings → Appearance), as `NSColor.controlAccentColor`
/// gives them.
const MAC_ACCENTS: [(&str, [u8; 3]); 8] = [
    ("blue", [0x00, 0x7a, 0xff]),
    ("purple", [0xa5, 0x50, 0xa7]),
    ("pink", [0xf7, 0x4f, 0x9e]),
    ("red", [0xff, 0x52, 0x57]),
    ("orange", [0xf7, 0x82, 0x1b]),
    ("yellow", [0xff, 0xc6, 0x00]),
    ("green", [0x62, 0xba, 0x46]),
    ("graphite", [0x8c, 0x8c, 0x8c]),
];

/// The accent that stays grey by design: no chroma floor holds for it.
const GREY_ACCENT: &str = "graphite";

/// The accents whose meter on the card's ground, darker than the pane's on Day, is floored below
/// the chroma floor: yellow keeps C 0.098 there (a dark ochre that reads at 4.5:1, not the
/// washed-out mix), while it keeps the floor on the pane's rule.
const DULL_ON_CARD: [&str; 2] = [GREY_ACCENT, "yellow"];

/// Six cells used, two more by the reset, the tick, one left.
fn bar() -> [BarCell; 10] {
    std::array::from_fn(|i| match i {
        0..6 => BarCell::Used,
        6..8 => BarCell::More,
        8 => BarCell::Tick,
        _ => BarCell::Left,
    })
}

/// The account unit of scenario B (a stop) or A (covered) of §2.4, as the resolver hands it.
fn unit(stop: bool) -> FooterFacts {
    let ten = bar();
    let verdict = if stop {
        VerdictWords {
            full: "\u{2573} ~Wed 22:00\u{2013}Thu 09:00".into(),
            short: "\u{2573} ~Wed 22:00".into(),
            shortest: Some("\u{2573} ~Wed 22:00".into()),
            warn: true,
            kept_narrow: true,
        }
    } else {
        VerdictWords {
            full: "continuous 7d".into(),
            short: "continuous 7d".into(),
            shortest: None,
            warn: false,
            kept_narrow: false,
        }
    };
    FooterFacts {
        subscription: Some(SubscriptionDisplay {
            email: Some("me@example.test".into()),
            plan: Some("Max 20x".into()),
            weekly: Some("weekly 42% left".into()),
            ..SubscriptionDisplay::default()
        }),
        glance: Some(Glance {
            bar: Some((ten, std::array::from_fn(|i| ten[i * 2]))),
            reset: Some("Thu 09:00".into()),
            five: Some("5h 62% left".into()),
            verdict: Some(verdict),
            ..Glance::default()
        }),
        ..FooterFacts::default()
    }
}

/// A card's bar row and its stop note, every ink the outlook draws on the card.
fn card_rows() -> Vec<MenuRow> {
    let cell = |ch: char, ink: PlotInk| PlotCell {
        ch,
        ink,
        busy: false,
    };
    let mut plot: Vec<PlotCell> = bar()
        .iter()
        .map(|c| {
            let ink = match c {
                BarCell::Left => PlotInk::Track,
                _ => PlotInk::Meter,
            };
            cell(c.glyph(), ink)
        })
        .collect();
    plot.push(cell(' ', PlotInk::Value));
    plot.push(cell('\u{2573}', PlotInk::Warn));
    vec![
        MenuRow::plot(plot, "me: 58% used, about 78% by its reset"),
        MenuRow::Note {
            text: "\u{2573}  Stop ~Wed 22:00 \u{2013} Thu 09:00 (11h): every account out".into(),
            spoken: "A stop is expected from about Wednesday 22:00 to Thursday 09:00, when every account is out.".into(),
            tone: NoteTone::Warn,
        },
    ]
}

/// The rule inks [`compose_rule_spans`] is drawn over: two greys and the pane's own ink.
fn rule_greys(blank: RenderCell) -> [RenderCell; 3] {
    [[0x88; 3], [0x99; 3], blank.fg].map(|fg| RenderCell {
        ch: '\u{2500}',
        fg,
        ..blank
    })
}

/// T20: THE OUTLOOK'S INKS READ ON EVERY GROUND, AND DAY STAYS VIVID. Over every built-in scheme,
/// under every palette and none, on three rule greys: the bar's `━ ╍ ┽` wear the marks ink of
/// their column ([`GlassInks::mark`]), at 3:1 or more and at 4.5:1 on a light ground, its `─ ┄`
/// the track ink at 3:1 or more, a stop's words the warn ink at 4.5:1 or more; on Aterm Day
/// under Cruise and Land the meter keeps the glass's vivid law
/// ([`aterm_effects::glass_inks::day_vivid`]: OKLCh C 0.12, or nearly the most sRGB holds at its
/// hue and lightness where that is less); the bar never takes the warn ink, and wherever
/// the warn ink is used its words lead with `╳`, a projected stop's mark (a stop is said, not
/// coloured). The system accent reads nowhere on the rule
/// (`lines_b_d_and_e_ignore_the_system_accent`). Negative control: the dashes mixed toward the
/// ground (`mix(meter, bg, 0.4)`) wash out on Day.
#[test]
fn the_outlooks_inks_read_on_every_ground_and_day_stays_vivid() {
    use aterm_effects::harness_card::CardPalette;
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    let palettes = [
        Some(CardPalette::Cruise),
        Some(CardPalette::Land),
        Some(CardPalette::Off),
        None,
    ];
    let room = 200;
    let at = rule_at_room(room);
    let (mut meters, mut tracks, mut warns, mut days) = (0usize, 0usize, 0usize, 0usize);
    let mut washed = 0usize;
    for name in builtin_names() {
        let Some(scheme) = builtin(name) else {
            continue;
        };
        let blank = RenderCell {
            ch: ' ',
            fg: rgb(scheme.foreground),
            bg: rgb(scheme.background),
            ..RenderCell::default()
        };
        let dark = crate::claude_card::dark_ground(blank);
        for palette in palettes {
            let what = format!("{name}, {palette:?}");
            let vivid = name == "Aterm Day"
                && matches!(palette, Some(CardPalette::Cruise | CardPalette::Land));
            for rule in rule_greys(blank) {
                let glass = rule_glass(palette, rule, blank);
                for stop in [false, true] {
                    let facts = unit(stop);
                    let fit = footer::fit_rule(&facts, room, None);
                    let paint =
                        compose_rule_spans(&fit, None, rule, blank, (&glass, at), room, None);
                    let set = footer::set_facts(&fit.segments);
                    let mut said = String::new();
                    let mut bar_cells = 0;
                    for glyph in &set.glyphs {
                        let col = paint.facts_at + 1 + glyph.cell;
                        let cell = paint.cells[col];
                        assert_eq!(cell.ch, glyph.ch, "{what}: the composed cell");
                        let k = reads(cell.fg, blank.bg);
                        if glyph.meter {
                            bar_cells += 1;
                            if matches!(glyph.ch, '\u{2500}' | '\u{2504}') {
                                assert!(k >= ICON_CONTRAST, "{what}: track {k:.2}:1");
                                tracks += 1;
                            } else {
                                let floor = if dark { ICON_CONTRAST } else { NAME_CONTRAST };
                                assert!(k >= floor, "{what}: meter {k:.2}:1");
                                assert_eq!(
                                    cell.fg,
                                    rgb_of(glass.mark(at.t(col))),
                                    "{what}: the bar keeps its column's marks ink, never the warn"
                                );
                                if vivid {
                                    let c = chroma(cell.fg);
                                    assert!(
                                        day_vivid(pack_rgb(cell.fg)),
                                        "{what}: the meter on Day at OKLCH C {c:.3}, {:?}",
                                        cell.fg
                                    );
                                    let mixed = mix(cell.fg, blank.bg, 0.4);
                                    washed += usize::from(!day_vivid(pack_rgb(mixed)));
                                    days += 1;
                                }
                                meters += 1;
                            }
                        } else if glyph.warn {
                            assert!(k >= NAME_CONTRAST, "{what}: warn {k:.2}:1");
                            assert_eq!(cell.fg, rgb_of(glass.warn()), "{what}: the warn ink");
                            said.push(glyph.ch);
                            warns += 1;
                        }
                    }
                    assert_eq!(bar_cells, 10, "{what}: the bar is drawn");
                    if stop {
                        // `said` holds the warn ink's glyphs, its blanks not among them.
                        assert!(
                            said.starts_with('\u{2573}') && said.contains("~Wed22:00"),
                            "{what}: the warn ink says the stop in words: {said:?}"
                        );
                    } else {
                        assert!(said.is_empty(), "{what}: no warn ink without a stop");
                    }
                }
            }
        }
    }
    let schemes = builtin_names().len();
    assert!(schemes >= 17, "{schemes} built-ins");
    assert!(
        meters >= schemes * palettes.len() * 3 * 2 * 7 && tracks > 0 && warns > 0,
        "{meters} meter, {tracks} track, {warns} warn cells"
    );
    assert!(days > 0, "Aterm Day's meter was looked at");
    // CONTROL: the washed-out look the owner ruled against (2026-10-17): the dashes mixed toward
    // Day's ground lose their chroma below the floor.
    assert!(
        washed > 0,
        "CONTROL: a mix toward Day's ground keeps its chroma"
    );
}

/// T20 ON THE ACCOUNTS CARD: over every built-in scheme, with its own accent and each macOS
/// accent, the card's bar `━ ╍ ┽` takes the meter ink at 4.5:1 or more on the card's ground, its
/// `─ ┄` the track ink at 3:1, `╳` and a stop's words at 4.5:1; on Aterm Day the meter keeps an
/// OKLCH chroma of 0.10 or more for every accent but those dull on the card. The card still
/// reads the system accent (S6 moves it onto the glass's tokens). Negative control: the dashes
/// mixed toward the ground (`mix(meter, bg, 0.4)`) wash out on Day.
#[test]
fn the_cards_outlook_inks_read_on_every_ground_and_day_stays_vivid() {
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    let accents: Vec<(&str, Option<[u8; 3]>)> = std::iter::once(("its own", None))
        .chain(MAC_ACCENTS.iter().map(|&(name, c)| (name, Some(c))))
        .collect();
    let mut checked = 0usize;
    for name in builtin_names() {
        let Some(scheme) = builtin(name) else {
            continue;
        };
        let theme = aterm_render::Theme::from(&scheme);
        for &(accent_name, accent) in &accents {
            install_system_accent(accent.map(|c| SystemAccent {
                on_dark: c,
                on_light: c,
            }));
            let what = format!("{name}, {accent_name} accent");
            let vivid = name == "Aterm Day" && accent_name != GREY_ACCENT;
            let menu = crate::tab_menu::menu_colors(theme);
            let rect = crate::tab_menu::MenuRect {
                row: 0,
                col: 0,
                w: 40,
                h: 4,
            };
            let rows = card_rows();
            let painted =
                crate::autopilot_menu::paint(rect, &rows, Default::default(), None, theme);
            let card_meter = legible_on(menu.band.accent, menu.card_bg);
            for cell in &painted[1] {
                let k = reads(cell.fg, menu.card_bg);
                match cell.ch {
                    '\u{2501}' | '\u{254d}' | '\u{253d}' => {
                        assert!(k >= NAME_CONTRAST, "{what}: card meter {k:.2}:1");
                        assert_eq!(cell.fg, card_meter, "{what}: the card's bar is the meter");
                        if vivid && !DULL_ON_CARD.contains(&accent_name) {
                            let c = chroma(cell.fg);
                            assert!(c >= 0.10, "{what}: card meter on Day at C {c:.3}");
                        }
                    }
                    '\u{2500}' | '\u{2504}' => {
                        assert!(k >= ICON_CONTRAST, "{what}: card track {k:.2}:1");
                    }
                    '\u{2573}' => assert!(k >= NAME_CONTRAST, "{what}: card stop {k:.2}:1"),
                    _ => {}
                }
                checked += 1;
            }
            let note: String = painted[2].iter().map(|c| c.ch).collect();
            assert!(note.contains("\u{2573}  Stop ~Wed"), "{what}: {note:?}");
            for cell in painted[2]
                .iter()
                .filter(|c| c.ch != ' ' && c.ch != '\u{2502}')
            {
                let k = reads(cell.fg, menu.card_bg);
                assert!(k >= NAME_CONTRAST, "{what}: card warn words {k:.2}:1");
            }
        }
    }
    install_system_accent(None);
    let schemes = builtin_names().len();
    assert!(checked >= schemes * accents.len(), "{checked} card cells");

    // CONTROL: the washed-out look the owner ruled against (2026-10-17): the dashes mixed toward
    // Day's ground lose their chroma below the floor.
    let day = builtin("Aterm Day").expect("a signature theme");
    let band = crate::chrome_band::band_colors(aterm_render::Theme::from(&day));
    let ground = rgb(day.background);
    let washed = mix(legible_on(band.accent, ground), ground, 0.4);
    assert!(
        chroma(washed) < 0.10,
        "CONTROL: a mix toward Day's ground at C {:.3}",
        chroma(washed)
    );
}
