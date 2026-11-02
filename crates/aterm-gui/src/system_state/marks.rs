// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The setup screen's state tokens and the surfaces it paints itself
//! (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4.4): each state's mark and ink, the list's
//! and the card's grounds (a grouped form's, [`crate::settings::Roles::grouped`]), and a card's
//! rim. Every state has its own shape, so colour is never the only signal; the accent macOS
//! paints controls in marks only work under way, success only work done, macOS's systemOrange
//! what needs the person and its systemRed what failed. A card is a rim and a mark, never a wash.

use super::lines::Mark;
use crate::native_appearance::{contrast_ratio, ensure_contrast_ratio, hue_distance, mix_srgb};
use crate::native_ui::LogicalRect;
use crate::settings::Roles;
use crate::widget::{DrawPrim, rgba};

/// A mark's ink keeps at least this contrast with the ground under it (WCAG's non-text floor).
pub(crate) const MARK_CONTRAST: f32 = 3.0;

/// The page's cards stand apart from the page by at least this much (§4.4), as
/// [`crate::settings::Roles::grouped`] holds them.
#[cfg(test)]
const CARD_SEPARATION: f32 = 1.2;

/// An accent within this many degrees of OKLCH hue of the success green reads as it: the working
/// spinner then takes a neutral ink, its shape alone telling work under way from work done.
pub(crate) const GREEN_ACCENT_DEGREES: f32 = 40.0;

/// How strong a card's state colour is in its rim, over the card.
const RIM_STRENGTH: f32 = 0.35;

/// A colour macOS defines for each appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SystemColor {
    pub(crate) dark: [u8; 3],
    pub(crate) light: [u8; 3],
}

impl SystemColor {
    /// The shade for a ground: its dark one on a dark ground.
    pub(crate) fn on(self, ground: [u8; 3]) -> [u8; 3] {
        if crate::native_appearance::surface_is_dark(ground) {
            self.dark
        } else {
            self.light
        }
    }
}

/// macOS's systemOrange: what needs the person.
pub(crate) const SYSTEM_ORANGE: SystemColor = SystemColor {
    dark: [0xFF, 0x9F, 0x0A],
    light: [0xFF, 0x95, 0x00],
};

/// macOS's systemRed: what failed.
pub(crate) const SYSTEM_RED: SystemColor = SystemColor {
    dark: [0xFF, 0x45, 0x3A],
    light: [0xFF, 0x3B, 0x30],
};

/// What a mark's ink is held to against its ground: [`MARK_CONTRAST`] with a little to spare,
/// so the pixels it paints, rounded to the screen's, still clear it.
const MARK_INK_CONTRAST: f32 = MARK_CONTRAST + 0.1;

/// The width of a stroke a mark draws: two points, so a level or upright stroke covers whole
/// pixels and paints its ink itself, never only a blend of it with the ground.
const MARK_STROKE: f32 = 2.0;

/// The mark's size, centred in its slot.
pub(crate) const MARK_PT: f32 = 16.0;

/// The slot a mark sits in.
pub(crate) const MARK_SLOT: f32 = 20.0;

/// What the page reads of the person's display settings for its tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Looks {
    /// High Contrast: 2 pt rims, and every line says its state in words.
    pub(crate) high_contrast: bool,
    /// Reduce Transparency: no working wash (the page has nothing translucent; a card's rim and
    /// mark, which carry its state, stay).
    pub(crate) solid: bool,
}

impl Looks {
    /// The person's settings now.
    pub(crate) fn now() -> Self {
        let preferences = crate::native_appearance::current_preferences().normalized();
        Self {
            high_contrast: preferences.high_contrast,
            solid: preferences.reduced_transparency,
        }
    }
}

/// Where a mark or a surface sits: the list's card, the Needs-you card, a failed item's wash,
/// or a working line's wash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ground {
    List,
    Needs,
    Failed,
    Working,
}

impl Ground {
    const fn value(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Needs => "needs",
            Self::Failed => "failed",
            Self::Working => "working",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::List, Self::Needs, Self::Failed, Self::Working]
            .into_iter()
            .find(|ground| ground.value() == value)
    }
}

/// Whether the accent work under way wears reads as the success green (§4.4's any-accent rule).
pub(crate) fn accent_reads_as_success(roles: &Roles) -> bool {
    hue_distance(roles.control_accent, roles.success) < GREEN_ACCENT_DEGREES
}

/// The ground of the page's cards: a grouped form's card, near white over a sinking page in
/// light mode, the elevated surface in dark mode, at least 1.2:1 from the page.
pub(crate) fn card_ground(roles: &Roles) -> [u8; 3] {
    roles.grouped().card
}

/// The page under the cards ([`card_ground`]): what the lockup and the hero stand on.
pub(crate) fn page_ground(roles: &Roles) -> [u8; 3] {
    roles.grouped().page
}

/// A ground's fill: the card, and under a working line its wash, the accent at .08 over the
/// card (none under Reduce Transparency). The Needs-you and failed cards have no wash: a card is
/// its rim and its marks.
pub(crate) fn ground_fill(ground: Ground, roles: &Roles, looks: Looks) -> [u8; 3] {
    let card = card_ground(roles);
    match ground {
        Ground::Working if !looks.solid => mix_srgb(card, working_hue(roles), 0.08),
        Ground::List | Ground::Needs | Ground::Failed | Ground::Working => card,
    }
}

/// A ground's rim: the card's separator, systemOrange (Needs you) or systemRed (failed) at .35
/// over the card, at least 3:1 against the page under High Contrast.
fn ground_rim(ground: Ground, roles: &Roles, looks: Looks) -> [u8; 3] {
    let card = card_ground(roles);
    let rim = match ground {
        Ground::List | Ground::Working => roles.separator,
        Ground::Needs => mix_srgb(card, SYSTEM_ORANGE.on(card), RIM_STRENGTH),
        Ground::Failed => mix_srgb(card, SYSTEM_RED.on(card), RIM_STRENGTH),
    };
    if looks.high_contrast {
        ensure_contrast_ratio(rim, page_ground(roles), MARK_CONTRAST)
    } else {
        rim
    }
}

/// The hue work under way is marked in: the accent macOS paints controls in, unless it reads as
/// success green.
fn working_hue(roles: &Roles) -> [u8; 3] {
    if accent_reads_as_success(roles) {
        crate::native_appearance::without_chroma(roles.text_secondary)
    } else {
        roles.control_accent
    }
}

/// A mark's ink on `ground`, at least [`MARK_CONTRAST`] against it and against the wash a
/// hovered line lays over it ([`crate::native_ui::DISCLOSURE_HOVER_WASH`] of the text ink), so a
/// mark under the pointer reads as well as one at rest.
pub(crate) fn mark_ink(mark: Mark, roles: &Roles, ground: [u8; 3]) -> [u8; 3] {
    let ink = match mark {
        Mark::Pending | Mark::Waiting | Mark::Off => roles.text_tertiary,
        Mark::Working => working_hue(roles),
        Mark::NeedsYou => SYSTEM_ORANGE.on(ground),
        Mark::Done => crate::native_appearance::ensure_text_contrast(roles.success, ground),
        Mark::Failed => SYSTEM_RED.on(ground),
    };
    let wash = crate::native_ui::DISCLOSURE_HOVER_WASH;
    let hovered = [
        ground,
        mix_srgb(ground, roles.text_primary, wash),
        crate::native_appearance::mix_linear(ground, roles.text_primary, wash),
    ];
    crate::native_appearance::ensure_contrast_on(ink, &hovered, MARK_INK_CONTRAST)
}

/// The painter's value for a mark on a ground.
pub(crate) fn mark_value(mark: Mark, ground: Ground) -> String {
    format!("{},{}", mark.value(), ground.value())
}

/// The painter's value of the working spinner on the list's card: what the motion tests and the
/// motion bench draw.
#[cfg(any(test, feature = "bench-support"))]
pub(crate) fn working_spinner() -> String {
    mark_value(Mark::Working, Ground::List)
}

/// How many spokes the working spinner has, and how many steps one turn of it takes.
pub(crate) const SPINNER_SPOKES: u64 = 12;

/// A state's mark, [`MARK_PT`] across, centred in `rect` (§4.4's shapes). The working spinner's
/// lead spoke stands at `frame` (mod [`SPINNER_SPOKES`]), each spoke behind it fainter, so it
/// steps clockwise one spoke a frame; with no `frame` (a page that may not move) it is the still
/// glyph, the lead at twelve o'clock.
pub(crate) fn paint_mark(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    frame: Option<u64>,
    roles: &Roles,
) {
    let Some((mark, ground)) = value.and_then(|value| {
        let (mark, ground) = value.split_once(',')?;
        Some((Mark::parse(mark)?, Ground::parse(ground)?))
    }) else {
        return;
    };
    let looks = Looks::now();
    let under = ground_fill(ground, roles, looks);
    let ink = mark_ink(mark, roles, under);
    let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let r = (MARK_PT / 2.0).min(rect.width / 2.0).min(rect.height / 2.0);
    let ring = |prims: &mut Vec<DrawPrim>, thickness: f32| {
        prims.push(DrawPrim::Ring {
            cx,
            cy,
            r_outer: r,
            thickness,
            track: rgba(ink, 255),
            start: 0.0,
            frac: 0.0,
            fill: rgba(ink, 255),
        });
    };
    let line =
        |prims: &mut Vec<DrawPrim>, (x1, y1): (f32, f32), (x2, y2): (f32, f32), w: f32, c| {
            prims.push(DrawPrim::Line {
                x1,
                y1,
                x2,
                y2,
                width: w,
                color: c,
            });
        };
    let rim = if looks.high_contrast { 2.0 } else { 1.5 };
    match mark {
        Mark::Pending => ring(prims, rim),
        Mark::Waiting => {
            ring(prims, rim);
            for dx in [-r * 0.42, 0.0, r * 0.42] {
                prims.push(DrawPrim::Dot {
                    cx: cx + dx,
                    cy,
                    r: (r * 0.14).max(1.0),
                    color: rgba(ink, 255),
                });
            }
        }
        Mark::Working => {
            // Twelve spokes, the lead in full ink and each one behind it a step fainter.
            let lead = frame.unwrap_or(0) % SPINNER_SPOKES;
            for spoke in 0..SPINNER_SPOKES {
                let behind = (lead + SPINNER_SPOKES - spoke) % SPINNER_SPOKES;
                #[allow(clippy::cast_precision_loss, reason = "a spoke of twelve")]
                let (turn, behind) = (
                    spoke as f32 / SPINNER_SPOKES as f32,
                    behind as f32 / (SPINNER_SPOKES - 1) as f32,
                );
                let angle = turn * std::f32::consts::TAU;
                let (sin, cos) = angle.sin_cos();
                let (inner, outer) = (r * 0.45, r * 0.95);
                let fade = 1.0 - behind * 0.75;
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "an alpha in 0..=255"
                )]
                let alpha = (255.0 * fade).round() as u8;
                line(
                    prims,
                    (cx + sin * inner, cy - cos * inner),
                    (cx + sin * outer, cy - cos * outer),
                    (r * 0.2).max(MARK_STROKE),
                    rgba(ink, alpha),
                );
            }
        }
        Mark::NeedsYou => {
            ring(prims, 2.0);
            prims.push(DrawPrim::Dot {
                cx,
                cy,
                r: r * 0.36,
                color: rgba(ink, 255),
            });
        }
        Mark::Done | Mark::Failed => {
            prims.push(DrawPrim::Dot {
                cx,
                cy,
                r,
                color: rgba(ink, 255),
            });
            // The glyph on the disc: whichever of the ground or the page's primary ink stands
            // further from it.
            let on = [under, roles.text_primary]
                .into_iter()
                .max_by(|a, b| contrast_ratio(*a, ink).total_cmp(&contrast_ratio(*b, ink)))
                .unwrap_or(under);
            let on = rgba(on, 255);
            if mark == Mark::Done {
                line(
                    prims,
                    (cx - r * 0.42, cy + r * 0.02),
                    (cx - r * 0.1, cy + r * 0.34),
                    r * 0.24,
                    on,
                );
                line(
                    prims,
                    (cx - r * 0.1, cy + r * 0.34),
                    (cx + r * 0.45, cy - r * 0.3),
                    r * 0.24,
                    on,
                );
            } else {
                line(prims, (cx, cy - r * 0.5), (cx, cy + r * 0.1), r * 0.24, on);
                prims.push(DrawPrim::Dot {
                    cx,
                    cy: cy + r * 0.45,
                    r: r * 0.14,
                    color: on,
                });
            }
        }
        Mark::Off => line(
            prims,
            (cx - r * 0.55, cy),
            (cx + r * 0.55, cy),
            MARK_STROKE,
            rgba(ink, 255),
        ),
    }
}

/// The painter's value for a surface.
pub(crate) fn surface_value(ground: Ground) -> String {
    ground.value().to_owned()
}

/// One of the page's surfaces, filling `rect`: the list's inset card or the Needs-you card,
/// radius 12 with a hairline rim (2 pt under High Contrast), and nothing lit along its top; or
/// a working line's wash, a band of radius 8 the page sets a step inside the list.
pub(crate) fn paint_surface(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &Roles,
) {
    let Some(ground) = value.and_then(Ground::parse) else {
        return;
    };
    let looks = Looks::now();
    let fill = ground_fill(ground, roles, looks);
    if ground == Ground::Working {
        if !looks.solid {
            prims.push(DrawPrim::Panel {
                x: rect.x,
                y: rect.y,
                w: rect.width,
                h: rect.height,
                radius: 8.0_f32.min(rect.height / 2.0),
                fill: rgba(fill, 255),
            });
        }
        return;
    }
    let radius = 12.0_f32.min(rect.height / 2.0);
    prims.push(DrawPrim::Panel {
        x: rect.x,
        y: rect.y,
        w: rect.width,
        h: rect.height,
        radius,
        fill: rgba(fill, 255),
    });
    let width = if looks.high_contrast { 2.0 } else { 1.0 };
    prims.push(DrawPrim::Stroke {
        x: rect.x + width / 2.0,
        y: rect.y + width / 2.0,
        w: (rect.width - width).max(0.0),
        h: (rect.height - width).max(0.0),
        radius: (radius - width / 2.0).max(0.0),
        width,
        color: rgba(ground_rim(ground, roles, looks), 255),
    });
}

#[cfg(test)]
mod tests {
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
}
