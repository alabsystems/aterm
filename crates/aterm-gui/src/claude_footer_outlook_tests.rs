// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account unit's inks (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.2, §4.1): its
//! name, figure, reset, joins and a projected stop's words on the pane's rule in the glass's
//! tokens under every palette, and the outlook's bar on the Accounts card under the macOS
//! accents (`docs/DESIGN-accounts-outlook-2026-10-25.md` §3.4, T20), over every built-in scheme.

use super::*;
use crate::autopilot_menu::{MenuRow, NoteTone, PlotCell, PlotInk};
use crate::native_appearance::{SystemAccent, install_system_accent};
use aterm_agent::harness::outlook::BarCell;
use aterm_agent::harness::quota_view::QuotaView;
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

/// The account unit with a projected stop (its verdict in the warn ink) or none, as the resolver
/// hands it.
fn unit(stop: bool) -> FooterFacts {
    let mut view = QuotaView::sample();
    if !stop {
        view.tail.verdict = None;
    }
    FooterFacts {
        account: Some("main".into()),
        subscription: Some(SubscriptionDisplay {
            email: Some("me@example.test".into()),
            plan: Some("Max 20x".into()),
            weekly: Some("weekly 63% used".into()),
            ..SubscriptionDisplay::default()
        }),
        quota: Some(view),
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

/// THE UNIT'S INKS READ ON EVERY GROUND, AND DAY STAYS VIVID. Over every built-in scheme, under
/// every palette and none, on three rule greys, each word of the account unit wears its role's
/// token ([`footer::Tone`]): the name and the reset's `↺` the lit ink, the figure and the time the
/// value ink, the joins the quiet ink, each at 4.5:1 or more; a stop's words the warn ink, bold,
/// at 4.5:1 or more, leading with `╳` (a stop is said, not coloured), and no warn ink without
/// one; the unit's `⎆` its column's marks ink, which on Aterm Day under Cruise and Land keeps the
/// glass's vivid law ([`aterm_effects::glass_inks::day_vivid`]). Negative control: the mark mixed
/// toward the ground (`mix(mark, bg, 0.4)`) washes out on Day.
#[test]
fn the_units_inks_read_on_every_ground_and_day_stays_vivid() {
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
    let (mut lits, mut values, mut quiets, mut warns, mut days) = (0, 0, 0, 0, 0usize);
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
                    for glyph in &set.glyphs {
                        let col = paint.facts_at + 1 + glyph.cell;
                        let cell = paint.cells[col];
                        assert_eq!(cell.ch, glyph.ch, "{what}: the composed cell");
                        let k = reads(cell.fg, blank.bg);
                        if glyph.mark {
                            if glyph.ch == footer::ACCOUNT_MARK && vivid {
                                let c = chroma(cell.fg);
                                assert!(
                                    day_vivid(pack_rgb(cell.fg)),
                                    "{what}: the unit's mark on Day at OKLCH C {c:.3}, {:?}",
                                    cell.fg
                                );
                                let mixed = mix(cell.fg, blank.bg, 0.4);
                                washed += usize::from(!day_vivid(pack_rgb(mixed)));
                                days += 1;
                            }
                            continue;
                        }
                        let (ink, count) = match glyph.tone {
                            footer::Tone::Lit => (glass.lit(), &mut lits),
                            footer::Tone::Value => (glass.value(), &mut values),
                            footer::Tone::Quiet => (glass.quiet(), &mut quiets),
                            footer::Tone::Warn => {
                                assert!(cell.bold, "{what}: the warn ink is bold");
                                said.push(glyph.ch);
                                (glass.warn(), &mut warns)
                            }
                        };
                        assert_eq!(cell.fg, rgb_of(ink), "{what}: {:?}'s token", glyph.ch);
                        assert!(k >= NAME_CONTRAST, "{what}: {:?} at {k:.2}:1", glyph.ch);
                        *count += 1;
                    }
                    if stop {
                        // `said` holds the warn ink's glyphs, its blanks not among them.
                        assert!(
                            said.starts_with('\u{2573}') && said.contains("~Thu12Nov05:00"),
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
        lits > 0 && values > 0 && quiets > 0 && warns > 0,
        "{lits} lit, {values} value, {quiets} quiet, {warns} warn cells"
    );
    assert!(days > 0, "Aterm Day's mark was looked at");
    // CONTROL: the washed-out look the owner ruled against (2026-10-17): the mark mixed toward
    // Day's ground loses its chroma below the floor.
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
