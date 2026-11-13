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
#[path = "marks_tests.rs"]
mod tests;
