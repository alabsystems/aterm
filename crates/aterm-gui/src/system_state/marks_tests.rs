// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_appearance::{SystemAccent, install_control_accent};

/// macOS's accent colours (System Settings ▸ Appearance), as the OS gives them.
const MACOS_ACCENTS: [(&str, [u8; 3]); 8] = [
    ("blue", [0x00, 0x7A, 0xFF]),
    ("purple", [0xA5, 0x50, 0xA7]),
    ("pink", [0xF7, 0x4F, 0x9E]),
    ("red", [0xFF, 0x52, 0x57]),
    ("orange", [0xF7, 0x82, 0x1B]),
    ("yellow", [0xFF, 0xC6, 0x00]),
    ("green", [0x62, 0xBA, 0x46]),
    ("graphite", [0x8C, 0x8C, 0x8C]),
];

fn bundled_themes() -> Vec<(&'static str, aterm_render::Theme)> {
    aterm_types::scheme::builtin_names()
        .into_iter()
        .map(|name| {
            (
                name,
                aterm_render::Theme::from(
                    &aterm_types::scheme::builtin(name).expect("a listed builtin exists"),
                ),
            )
        })
        .collect()
}

/// Every bundled theme under every macOS accent, and with none read (a Mac's system blue,
/// elsewhere the theme's own accent).
fn every_palette(mut each: impl FnMut(&str, Roles)) {
    for (name, theme) in bundled_themes() {
        let _ = install_control_accent(None);
        each(&format!("{name}, no accent read"), Roles::from_theme(theme));
        for (accent_name, accent) in MACOS_ACCENTS {
            let _ = install_control_accent(Some(SystemAccent {
                on_dark: accent,
                on_light: accent,
            }));
            each(&format!("{name}, {accent_name}"), Roles::from_theme(theme));
        }
    }
    let _ = install_control_accent(None);
}

#[test]
fn working_never_shares_done_s_green_under_any_theme_or_accent() {
    let mut neutral = 0;
    every_palette(|what, roles| {
        let ground = card_ground(&roles);
        let working = mark_ink(Mark::Working, &roles, ground);
        let done = mark_ink(Mark::Done, &roles, ground);
        if accent_reads_as_success(&roles) {
            neutral += 1;
        }
        // The working ink is the accent only where it stands apart from success.
        assert!(
            hue_distance(working, done) >= GREEN_ACCENT_DEGREES
                || crate::native_appearance::chroma(working) < 0.05,
            "{what}: working {working:?} reads as done {done:?}"
        );
    });
    assert!(
        neutral > 0,
        "some accent (macOS green) takes the neutral ink"
    );
}

/// EVERY MARK READS 3:1 WHEREVER IT SITS (§4.4): on each ground, and on the wash a hovered
/// line lays over it (the text ink at [`crate::native_ui::DISCLOSURE_HOVER_WASH`], blended in
/// sRGB or in linear light). NEGATIVE CONTROL: an ink held to the bare ground alone falls
/// under 3:1 on some hovered line, so the wash is what the hold answers to.
#[test]
fn every_mark_reads_three_to_one_on_every_ground_it_sits_on() {
    let wash = crate::native_ui::DISCLOSURE_HOVER_WASH;
    let mut held_to_the_ground_alone_falls_under = 0;
    every_palette(|what, roles| {
        for looks in [
            Looks::default(),
            Looks {
                high_contrast: true,
                solid: false,
            },
            Looks {
                high_contrast: false,
                solid: true,
            },
        ] {
            for ground in [Ground::List, Ground::Needs, Ground::Failed, Ground::Working] {
                let under = ground_fill(ground, &roles, looks);
                let hovered = [
                    mix_srgb(under, roles.text_primary, wash),
                    crate::native_appearance::mix_linear(under, roles.text_primary, wash),
                ];
                for mark in Mark::ALL {
                    let ink = mark_ink(mark, &roles, under);
                    for (on, ground_rgb) in [
                        ("ground", under),
                        ("hover", hovered[0]),
                        ("linear hover", hovered[1]),
                    ] {
                        assert!(
                            contrast_ratio(ink, ground_rgb) >= MARK_CONTRAST - 0.01,
                            "{what}: {mark:?} on {ground:?}'s {on} ({looks:?}) is {:.2}:1",
                            contrast_ratio(ink, ground_rgb)
                        );
                    }
                    let bare = match mark {
                        Mark::NeedsYou => SYSTEM_ORANGE.on(under),
                        Mark::Failed => SYSTEM_RED.on(under),
                        _ => continue,
                    };
                    let bare = ensure_contrast_ratio(bare, under, MARK_INK_CONTRAST);
                    if hovered
                        .iter()
                        .any(|&wash| contrast_ratio(bare, wash) < MARK_CONTRAST)
                    {
                        held_to_the_ground_alone_falls_under += 1;
                    }
                }
            }
        }
    });
    assert!(
        held_to_the_ground_alone_falls_under > 0,
        "an ink held to its ground alone already clears every hovered line"
    );
}

/// THE PAGE IS A GROUPED FORM (§4.4): every card stands at least 1.2:1 from the page, and in
/// light mode it is lighter than the page, as macOS's white cards on a grey page are, never
/// a darker list sunk into a lighter page. NEGATIVE CONTROL: a light theme's own cards sink
/// on a Mac (SCR-2's list, `#dcdcdc` on `#f1f1f1`), so the grouped form is what lifts them.
#[test]
fn cards_rise_from_the_page_and_stand_apart_from_it() {
    let mut light = 0;
    every_palette(|what, roles| {
        let (card, page) = (card_ground(&roles), page_ground(&roles));
        let ratio = contrast_ratio(card, page);
        assert!(
            ratio >= CARD_SEPARATION,
            "{what}: card {ratio:.3}:1 on the page"
        );
        let luminance = |rgb: [u8; 3]| contrast_ratio(rgb, [0, 0, 0]);
        if !crate::native_appearance::surface_is_dark(roles.surface) {
            light += 1;
            assert!(
                luminance(card) > luminance(page),
                "{what}: card {card:?} is lighter than the page {page:?}"
            );
            if cfg!(target_os = "macos") {
                assert!(
                    luminance(roles.elevated) < luminance(roles.surface),
                    "{what}: a Mac's light cards sink on their own"
                );
            }
            // The same words remain readable on both grouped grounds.
            for ink in [roles.text_primary, roles.text_secondary] {
                for ground in [page, card] {
                    assert!(
                        contrast_ratio(ink, ground) >= 4.5,
                        "{what}: {ink:?} on {ground:?}"
                    );
                }
            }
        }
    });
    assert!(light > 0, "the bundled themes include a light one");
}

/// A CARD IS A RIM AND A MARK (§4.4, D5): the Needs-you and failed cards have no wash, so
/// they read neither olive nor mauve; their rims are macOS's systemOrange and systemRed at
/// .35 over the card, and their marks those colours, at least 3:1. NEGATIVE CONTROL: a
/// working line still has its wash, so the test tells a wash from none.
#[test]
fn a_needs_you_or_failed_card_is_a_rim_and_a_mark_with_no_wash() {
    every_palette(|what, roles| {
        let card = card_ground(&roles);
        for looks in [
            Looks::default(),
            Looks {
                high_contrast: false,
                solid: true,
            },
        ] {
            assert_eq!(ground_fill(Ground::Needs, &roles, looks), card, "{what}");
            assert_eq!(ground_fill(Ground::Failed, &roles, looks), card, "{what}");
        }
        assert_ne!(
            ground_fill(Ground::Working, &roles, Looks::default()),
            card,
            "{what}: a working line keeps its wash"
        );
        for (ground, mark, colour) in [
            (Ground::Needs, Mark::NeedsYou, SYSTEM_ORANGE),
            (Ground::Failed, Mark::Failed, SYSTEM_RED),
        ] {
            assert_eq!(
                ground_rim(ground, &roles, Looks::default()),
                mix_srgb(card, colour.on(card), RIM_STRENGTH),
                "{what}: {ground:?}'s rim"
            );
            let ink = mark_ink(mark, &roles, card);
            assert!(
                hue_distance(ink, colour.on(card)) < 12.0,
                "{what}: {mark:?} {ink:?} keeps {:?}'s hue",
                colour.on(card)
            );
        }
    });
}

/// REDUCE TRANSPARENCY KEEPS WHAT CARRIES MEANING (§4.4): the page's cards have nothing
/// translucent, so the Needs-you and failed cards, their rims and their marks paint the same
/// with it on. NEGATIVE CONTROL: the working wash goes, so the setting reached the painters.
#[test]
fn reduce_transparency_keeps_every_rim_and_mark() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    let roles = Roles::from_theme(aterm_render::Theme::default());
    let rect = LogicalRect::new(0.0, 0.0, 300.0, 60.0);
    let slot = LogicalRect::new(0.0, 0.0, MARK_SLOT, MARK_SLOT);
    let painted = |solid: bool| {
        let _ = install_preferences(AppearancePreferences {
            reduced_transparency: solid,
            ..AppearancePreferences::default()
        });
        let mut prims = Vec::new();
        for ground in [Ground::Needs, Ground::Failed] {
            paint_surface(&mut prims, rect, Some(&surface_value(ground)), &roles);
        }
        for (mark, ground) in [
            (Mark::NeedsYou, Ground::Needs),
            (Mark::Failed, Ground::Failed),
        ] {
            paint_mark(
                &mut prims,
                slot,
                Some(&mark_value(mark, ground)),
                None,
                &roles,
            );
        }
        let mut wash = Vec::new();
        paint_surface(
            &mut wash,
            rect,
            Some(&surface_value(Ground::Working)),
            &roles,
        );
        (prims, wash)
    };
    let (plain, plain_wash) = painted(false);
    let (solid, solid_wash) = painted(true);
    let _ = install_preferences(AppearancePreferences::default());
    assert_eq!(
        format!("{plain:?}"),
        format!("{solid:?}"),
        "the cards, rims and marks stay"
    );
    let card = card_ground(&roles);
    for colour in [SYSTEM_ORANGE, SYSTEM_RED] {
        let rim = crate::widget::rgba(mix_srgb(card, colour.on(card), RIM_STRENGTH), 255);
        assert!(
            solid
                .iter()
                .any(|prim| matches!(prim, DrawPrim::Stroke { color, .. } if *color == rim)),
            "the {colour:?} rim stays"
        );
    }
    assert!(
        !plain_wash.is_empty() && solid_wash.is_empty(),
        "the working wash goes"
    );
}

#[test]
fn marks_and_surfaces_round_trip_their_values() {
    for mark in Mark::ALL {
        assert_eq!(Mark::parse(mark.value()), Some(mark));
        for ground in [Ground::List, Ground::Needs, Ground::Failed, Ground::Working] {
            let value = mark_value(mark, ground);
            let mut prims = Vec::new();
            let roles = Roles::from_theme(aterm_render::Theme::default());
            paint_mark(
                &mut prims,
                LogicalRect::new(0.0, 0.0, MARK_SLOT, MARK_SLOT),
                Some(&value),
                None,
                &roles,
            );
            assert!(!prims.is_empty(), "{value} draws");
        }
    }
    // Distinct shapes: no two marks draw the same prims.
    let roles = Roles::from_theme(aterm_render::Theme::default());
    let shapes: Vec<String> = Mark::ALL
        .into_iter()
        .map(|mark| {
            let mut prims = Vec::new();
            paint_mark(
                &mut prims,
                LogicalRect::new(0.0, 0.0, MARK_SLOT, MARK_SLOT),
                Some(&mark_value(mark, Ground::List)),
                None,
                &roles,
            );
            prims
                .iter()
                .map(|prim| format!("{:?}", std::mem::discriminant(prim)))
                .collect::<Vec<_>>()
                .join(",")
                + &prims.len().to_string()
        })
        .collect();
    for (i, a) in shapes.iter().enumerate() {
        for b in &shapes[i + 1..] {
            assert_ne!(a, b, "two marks share a shape");
        }
    }
}

/// The spokes the working mark draws at `frame`: each one's far end and alpha.
fn spokes(frame: Option<u64>) -> Vec<((i32, i32), u8)> {
    let roles = Roles::from_theme(aterm_render::Theme::default());
    let mut prims = Vec::new();
    paint_mark(
        &mut prims,
        LogicalRect::new(0.0, 0.0, MARK_SLOT, MARK_SLOT),
        Some(&mark_value(Mark::Working, Ground::Working)),
        frame,
        &roles,
    );
    prims
        .iter()
        .filter_map(|prim| match prim {
            #[allow(clippy::cast_possible_truncation, reason = "a test's tenths")]
            DrawPrim::Line { x2, y2, color, .. } => {
                Some((((x2 * 10.0) as i32, (y2 * 10.0) as i32), color[3]))
            }
            _ => None,
        })
        .collect()
}

/// The far end of the spoke drawn in full ink.
fn lead(frame: Option<u64>) -> (i32, i32) {
    spokes(frame)
        .into_iter()
        .max_by_key(|(_, alpha)| *alpha)
        .map(|(end, _)| end)
        .expect("the spinner draws spokes")
}

/// THE WORKING SPINNER STEPS CLOCKWISE ONE SPOKE A FRAME (§4.4, §5.1): the lead spoke at
/// `frame` mod 12, the one it left the next brightest, and frame 12 the same as frame 0. The
/// still glyph (no frame) is frame 0, its lead at twelve o'clock. NEGATIVE CONTROL: two
/// frames a step apart draw different spokes in full ink.
#[test]
fn the_working_spinner_steps_clockwise_one_spoke_a_frame() {
    let still = spokes(None);
    assert_eq!(still.len(), 12);
    assert_eq!(still, spokes(Some(0)));
    assert_eq!(spokes(Some(12)), spokes(Some(0)));
    let (top, one) = (lead(Some(0)), lead(Some(1)));
    assert_ne!(top, one, "a frame moves the lead");
    assert!(
        top.1 < one.1 && top.0 < one.0,
        "the lead steps clockwise from twelve"
    );
    // The spoke the lead just left is the next brightest.
    let at_one = spokes(Some(1));
    let second = at_one
        .iter()
        .filter(|(end, _)| *end != one)
        .max_by_key(|(_, alpha)| *alpha)
        .map(|(end, _)| *end);
    assert_eq!(second, Some(top), "the trail follows the lead");
}
