// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account unit's inks and the Accounts card's (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
//! §1.2, §1.4, §4.1): the unit's name, figure, reset, joins and a projected stop's words on the
//! pane's rule, and the card's drawn rows and warn words, each in the glass's tokens under every
//! palette, over every built-in scheme.

use super::*;
use crate::autopilot_menu::{MenuRow, NoteTone, PlotCell, PlotInk};
use crate::native_appearance::{SystemAccent, install_system_accent};
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

/// A card's drawn row and its stop note, every ink the outlook draws on the card: two read
/// levels, now's `▕`, the projected line, `→` and a name, `┄`, `╳` and `↺`.
fn card_rows() -> Vec<MenuRow> {
    let cell = |ch: char, ink: PlotInk| PlotCell {
        ch,
        ink,
        busy: false,
    };
    let plot = vec![
        cell('\u{2583}', PlotInk::Read),
        cell('\u{2585}', PlotInk::Read),
        cell('\u{2595}', PlotInk::Axis),
        cell('\u{1fb78}', PlotInk::Ahead),
        cell('\u{1fb76}', PlotInk::Ahead),
        cell('\u{2192}', PlotInk::Mark),
        cell('a', PlotInk::Value),
        cell('\u{2504}', PlotInk::Track),
        cell('\u{2594}', PlotInk::Ahead),
        cell('\u{2573}', PlotInk::Warn),
        cell('\u{21ba}', PlotInk::Mark),
    ];
    vec![
        MenuRow::plot(
            plot,
            "Work, the last day as read on this Mac and the next 7 days.",
        ),
        MenuRow::Note {
            text: "\u{2573} out ~Wed 22:00\u{2013}Thu 09:00".into(),
            spoken: "Out from about Wednesday 22:00 until Thursday 09:00.".into(),
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

/// A CARD THAT WEARS THE GLASS STANDS ON THE PANE'S GROUND, AND A STOP ON A WASH OF THE WARN INK
/// (§1.4, §4.1, review 2026-11-07): over every built-in scheme under Cruise and Land, every cell
/// of the card inside its border (its words, its rules, its drawn rows) has the pane's ground,
/// the ground the glass's inks are solved on, so Day's letters are not floored on the band's grey
/// into olive and brown; a warn cell (`╳`, a lane's out line) alone stands on a wash of the warn
/// ink, so it reads apart from the projected line's letters even where a letter takes the warn's
/// hue (Land's orange). With no palette the card keeps the band's grey. Negative controls: the
/// card on the band's grey under a palette (S6 before this review) misses the pane's ground; a
/// warn cell with no wash shares the letters' ground.
#[test]
fn a_glass_card_stands_on_the_panes_ground_and_a_stop_on_a_warn_wash() {
    use aterm_effects::harness_card::CardPalette;
    let rect = crate::tab_menu::MenuRect {
        row: 0,
        col: 0,
        w: 40,
        h: 4,
    };
    let mut checked = 0usize;
    for name in builtin_names() {
        let Some(scheme) = builtin(name) else {
            continue;
        };
        let theme = aterm_render::Theme::from(&scheme);
        let pane = rgb_of(theme.bg);
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            let painted = crate::autopilot_menu::paint(
                rect,
                &card_rows(),
                Default::default(),
                None,
                theme,
                Some(palette),
            );
            let mut washes = 0;
            for line in &painted[1..3] {
                for cell in &line[1..rect.w - 1] {
                    if cell.ch == '\u{2573}' && line == &painted[1] {
                        assert_ne!(cell.bg, pane, "{name} {palette:?}: a stop on its wash");
                        washes += 1;
                    } else {
                        assert_eq!(cell.bg, pane, "{name} {palette:?}: {:?}", cell.ch);
                    }
                    checked += 1;
                }
            }
            assert_eq!(washes, 1, "{name} {palette:?}");
        }
        let plain =
            crate::autopilot_menu::paint(rect, &card_rows(), Default::default(), None, theme, None);
        let grey = crate::tab_menu::menu_colors(theme).card_bg;
        assert!(
            plain[1][1..rect.w - 1].iter().all(|c| c.bg == grey),
            "{name}: no palette, the band's grey"
        );
    }
    assert!(checked > 0);
}

/// T20 ON THE ACCOUNTS CARD: over every built-in scheme, under every palette and none, with its
/// own accent and each macOS accent, the card's drawn cells wear the glass's tokens on the
/// card's ground, the pane's own under a palette (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
/// §1.4, §4.1): a read level its column's mark at 3:1 or more (4.5:1 on paper), the projected
/// line its column's letter at 4.5:1, `┄` the track at 3:1, `→` and `↺` the lit ink, a stop's
/// words the warn ink at 4.5:1 and bold, `╳` bold on a wash of a hued warn ink and floored on it
/// to 4.5:1; on Aterm Day under Cruise and Land the levels and the line keep the vivid law
/// ([`day_vivid`]). The system accent reads nowhere on the card: every accent paints the same
/// cells. Negative controls: the accent meter the card wore before (`legible_on(accent,
/// card_bg)`) is not the column's mark under some accent, and on Day under the graphite accent
/// it is grey, below the vivid law.
#[test]
fn the_cards_outlook_inks_read_on_every_ground_and_day_stays_vivid() {
    use aterm_effects::glass_inks::{GlassInks, field};
    use aterm_effects::harness_card::CardPalette;
    let accents: Vec<(&str, Option<[u8; 3]>)> = std::iter::once(("its own", None))
        .chain(MAC_ACCENTS.iter().map(|&(name, c)| (name, Some(c))))
        .collect();
    let palettes = [
        Some(CardPalette::Cruise),
        Some(CardPalette::Land),
        Some(CardPalette::Off),
        None,
    ];
    let rect = crate::tab_menu::MenuRect {
        row: 0,
        col: 0,
        w: 40,
        h: 4,
    };
    let (mut checked, mut days, mut dull, mut accent_meters) = (0usize, 0usize, 0usize, 0usize);
    for name in builtin_names() {
        let Some(scheme) = builtin(name) else {
            continue;
        };
        let theme = aterm_render::Theme::from(&scheme);
        for palette in palettes {
            let vivid = name == "Aterm Day"
                && matches!(palette, Some(CardPalette::Cruise | CardPalette::Land));
            let mut first: Option<Vec<Vec<RenderCell>>> = None;
            for &(accent_name, accent) in &accents {
                install_system_accent(accent.map(|c| SystemAccent {
                    on_dark: c,
                    on_light: c,
                }));
                let what = format!("{name}, {palette:?}, {accent_name} accent");
                let menu = match palette {
                    Some(_) => crate::tab_menu::glass_menu_colors(theme),
                    None => crate::tab_menu::menu_colors(theme),
                };
                let glass = GlassInks::of(
                    palette,
                    pack_rgb(menu.card_bg),
                    theme.fg & 0x00FF_FFFF,
                    pack_rgb(menu.rule),
                );
                let light = !glass.dark();
                let painted = crate::autopilot_menu::paint(
                    rect,
                    &card_rows(),
                    Default::default(),
                    None,
                    theme,
                    palette,
                );
                for (col, cell) in painted[1].iter().enumerate() {
                    let at = field(col, rect.w);
                    let k = reads(cell.fg, cell.bg);
                    let (want, floor, bold) = match cell.ch {
                        '\u{2583}' | '\u{2585}' => {
                            let meter = legible_on(menu.band.accent, menu.card_bg);
                            accent_meters += usize::from(meter != rgb_of(glass.mark(at)));
                            if vivid {
                                assert!(day_vivid(pack_rgb(cell.fg)), "{what}: a level on Day");
                                dull += usize::from(!day_vivid(pack_rgb(meter)));
                                days += 1;
                            }
                            let floor = if light { NAME_CONTRAST } else { ICON_CONTRAST };
                            (glass.mark(at), floor, false)
                        }
                        '\u{1fb78}' | '\u{1fb76}' | '\u{2594}' => {
                            if vivid {
                                assert!(day_vivid(pack_rgb(cell.fg)), "{what}: the line on Day");
                            }
                            (glass.letter(at), NAME_CONTRAST, false)
                        }
                        '\u{2504}' => (glass.track(), ICON_CONTRAST, false),
                        '\u{2192}' | '\u{21ba}' => (glass.lit(), NAME_CONTRAST, false),
                        '\u{2573}' => {
                            // On a wash of the warn ink where it has a hue, floored on it.
                            let hued = glass.warn() != glass.lit();
                            let ground = if hued {
                                crate::chrome_band::mix3(
                                    menu.card_bg,
                                    rgb_of(glass.warn()),
                                    crate::autopilot_menu::WARN_WASH,
                                )
                            } else {
                                menu.card_bg
                            };
                            assert_eq!(cell.bg, ground, "{what}: the warn wash");
                            let ink = aterm_effects::color_math::floor_contrast_keeping_hue(
                                glass.warn(),
                                pack_rgb(ground),
                                NAME_CONTRAST,
                            );
                            (ink, NAME_CONTRAST, true)
                        }
                        _ => continue,
                    };
                    assert_eq!(cell.fg, rgb_of(want), "{what}: {:?} in its token", cell.ch);
                    assert!(k >= floor, "{what}: {:?} at {k:.2}:1", cell.ch);
                    assert_eq!(cell.bold, bold, "{what}: {:?} bold", cell.ch);
                    checked += 1;
                }
                let note: String = painted[2].iter().map(|c| c.ch).collect();
                assert!(note.contains("\u{2573} out ~Wed"), "{what}: {note:?}");
                for cell in painted[2]
                    .iter()
                    .filter(|c| c.ch != ' ' && c.ch != '\u{2502}')
                {
                    assert_eq!(cell.fg, rgb_of(glass.warn()), "{what}: the warn words");
                    assert!(cell.bold, "{what}: the warn words are bold");
                    let k = reads(cell.fg, menu.card_bg);
                    assert!(k >= NAME_CONTRAST, "{what}: card warn words {k:.2}:1");
                }
                let drawn: Vec<Vec<RenderCell>> = painted[1..3].to_vec();
                match &first {
                    None => first = Some(drawn),
                    Some(own) => assert_eq!(own, &drawn, "{what}: the accent reads on the card"),
                }
            }
        }
    }
    install_system_accent(None);
    let schemes = builtin_names().len();
    assert!(
        checked >= schemes * palettes.len() * accents.len() * 9,
        "{checked} card cells"
    );
    assert!(days > 0, "Aterm Day's levels were looked at");
    // CONTROLS: the accent meter the card wore before S6 is not the glass's mark, and it takes
    // the washed-out look the owner ruled against (2026-10-17) on Day under a grey accent.
    assert!(accent_meters > 0, "CONTROL: the accent meter is the mark");
    assert!(dull > 0, "CONTROL: the accent meter stays vivid on Day");
}
