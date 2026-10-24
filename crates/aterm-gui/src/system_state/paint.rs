// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the System State page draws itself (`docs/DESIGN-system-state-2026-10-21.md`
//! §3.1, §3.3): the aterm · ALab brand lockup, a row's chip and the run's meter.
//! Each is a paint-only custom node whose value says what to draw; its words
//! live on the node's accessible parent.

use crate::native_ui::LogicalRect;
use crate::settings::Roles;
use crate::type_scale::TypeStep;
use crate::widget::{ArtSpan, DrawPrim, ImageArt, TextFace, TextWeight, rgba, text_prim};

/// The app icon's teal on a dark ground (`❯` and the caret of `>a|`).
const TEAL_DARK: [u8; 3] = [0x5E, 0xEA, 0xD4];
/// The icon's teal printed on a light ground.
const TEAL_LIGHT: [u8; 3] = [0x0D, 0x94, 0x88];
/// The words after the prompt.
pub(crate) const BRAND_WORDS: &str = "aterm · ALab";
/// The kitty is baked at least this many device pixels tall where the lockup
/// has room for it (2x displays and up): below it the pet baker drops the
/// face's detail.
const KITTY_MIN_DEVICE_PX: f32 = 56.0;
/// The kitty is never taller than this many mono capitals: on a 1x display
/// the device-pixel floor gives way here, so the kitty stays a mark beside
/// the words and the lockup keeps its stated height.
const KITTY_MAX_CAPS: f32 = 2.4;
/// How strong the ribbon is over the page, mixed in sRGB as the brand image
/// is (`assets/aterm-rainbow-kitty.png` reads about 24% on its dark ground).
const RIBBON_DARK: f32 = 0.24;
const RIBBON_LIGHT: f32 = 0.30;
/// The left edge of the kitty's `h`-tall box behind a caret at `caret_x`,
/// `caret_w` wide, measured from the pixels the art draws: its drawn left edge
/// (the raised tail and the stretched hind foot) stands at the caret's middle,
/// so the caret covers its haunch and the tail rises past the caret's top, as
/// in `assets/aterm-rainbow-kitty.png`, and none of it shows over the letters.
fn kitty_x(caret_x: f32, caret_w: f32, h: f32, art: ImageArt) -> f32 {
    let drawn = art.mask().drawn(ArtSpan::BOX).unwrap_or(ArtSpan::BOX);
    caret_x + caret_w * 0.5 - drawn.left * h * art.aspect()
}

/// The caret's width for a mono `advance`.
fn caret_width(advance: f32) -> f32 {
    (advance * 0.55).round().max(2.0)
}

/// What a brand node draws, from its value (`kitty,<px>,<scale>` or
/// `text,<px>,<scale>`): the kitty or the words alone, the mono size, and the
/// display's device pixels per point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BrandSpec {
    pub(crate) kitty: bool,
    pub(crate) px: f32,
    pub(crate) scale: f32,
}

impl BrandSpec {
    /// The node value the painter reads back.
    pub(crate) fn value(self) -> String {
        format!(
            "{},{},{}",
            if self.kitty { "kitty" } else { "text" },
            self.px,
            self.scale
        )
    }

    pub(crate) fn parse(value: Option<&str>) -> Self {
        let mut parts = value.unwrap_or_default().split(',');
        let kitty = parts.next() == Some("kitty");
        let number = |part: Option<&str>, fallback: f32| {
            part.and_then(|part| part.parse::<f32>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .unwrap_or(fallback)
        };
        let px = number(parts.next(), 22.0);
        let scale = number(parts.next(), 2.0).max(1.0);
        Self { kitty, px, scale }
    }

    /// The mono face's capital height at this size (the Latin norm).
    fn cap(self) -> f32 {
        self.px * 0.7
    }

    /// The kitty's height in points: about 1.9 capitals, at least
    /// [`KITTY_MIN_DEVICE_PX`] device pixels, and at most
    /// [`KITTY_MAX_CAPS`] capitals (which only a 1x display reaches).
    pub(crate) fn kitty_height(self) -> f32 {
        (self.cap() * 1.9)
            .max(KITTY_MIN_DEVICE_PX / self.scale)
            .min(self.cap() * KITTY_MAX_CAPS)
    }

    /// The height a brand node needs: the words and, when it shows, the
    /// kitty standing on their baseline.
    pub(crate) fn needed_height(self) -> f32 {
        let words = self.px * 1.3;
        if self.kitty {
            words.max(self.kitty_height() + self.px * 0.3 + 2.0)
        } else {
            words
        }
    }

    /// One mono advance at this size.
    fn advance(self) -> f32 {
        crate::tray_raster::measure_text("m", self.px, TextWeight::Regular).max(self.px * 0.5)
    }

    /// The painted width: prompt, words, caret and kitty.
    pub(crate) fn width(self) -> f32 {
        let advance = self.advance();
        let words = crate::tray_raster::measure_text(BRAND_WORDS, self.px, TextWeight::Regular);
        let caret_x = advance * 2.0 + words + advance * 0.2;
        let kitty = if self.kitty {
            let (h, art) = (self.kitty_height(), ImageArt::BrandKitty { dark: true });
            kitty_x(caret_x, caret_width(advance), h, art) + h * art.aspect()
        } else {
            caret_x + advance * 0.55
        };
        kitty.ceil()
    }
}

/// THE BRAND LOCKUP (§3.1): `❯ aterm · ALab▌` in the terminal's mono face,
/// the prompt and the caret in the icon's teal, a dim six-stripe ribbon behind
/// the words that fades in under their first letter and ends flush at the
/// caret, and the tabby behind the caret, which covers its haunch while the
/// tail rises past the caret's top, as the brand image draws them.
/// Text only for Serious Mode and high contrast.
pub(crate) fn paint_brand(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &Roles,
) {
    let spec = BrandSpec::parse(value);
    let dark = crate::native_appearance::surface_is_dark(roles.surface);
    let teal = crate::native_appearance::ensure_text_contrast(
        if dark { TEAL_DARK } else { TEAL_LIGHT },
        roles.surface,
    );
    let px = spec.px;
    let size = TypeStep::Body.px(px);
    let advance = spec.advance();
    let cap = spec.cap();
    let words_w = crate::tray_raster::measure_text(BRAND_WORDS, px, TextWeight::Regular);
    // The baseline: the words centred in the node, lowered where the kitty
    // standing on it needs more room above.
    let centred = crate::tray_raster::row_baseline(rect.y, rect.height, px);
    let baseline = if spec.kitty {
        centred
            .max(rect.y + spec.kitty_height() - px * 0.12 + 1.0)
            .min(rect.bottom() - px * 0.3)
    } else {
        centred
    };
    let x0 = rect.x;
    let words_x = x0 + advance * 2.0;
    let caret_x = words_x + words_w + advance * 0.2;
    let caret_w = caret_width(advance);
    let caret_top = baseline - px * 0.95;
    let caret_h = px * 1.25;

    // The ribbon goes with the kitty: Serious Mode and high contrast draw
    // the words alone, on the page.
    if spec.kitty {
        paint_ribbon(prims, roles.surface, dark, px, baseline, words_x..caret_x);
    }

    // The prompt: a heavy chevron one advance wide, drawn, so no font needs
    // to carry `❯`.
    let stroke = (px * 0.11).max(1.5);
    let (left, right) = (x0 + advance * 0.22, x0 + advance * 0.72);
    let (top, middle, bottom) = (baseline - cap * 0.78, baseline - cap * 0.39, baseline);
    for (x1, y1, x2, y2) in [(left, top, right, middle), (right, middle, left, bottom)] {
        prims.push(DrawPrim::Line {
            x1,
            y1,
            x2,
            y2,
            width: stroke,
            color: rgba(teal, 255),
        });
    }
    prims.push(text_prim(
        words_x,
        baseline,
        BRAND_WORDS.to_owned(),
        size,
        TextWeight::Regular,
        TextFace::Mono,
        rgba(roles.text_primary, 255),
    ));

    if spec.kitty {
        let h = spec.kitty_height();
        let art = ImageArt::BrandKitty { dark };
        let w = h * art.aspect();
        // Placed by the pixels it draws, as the brand image is: the caret
        // covers its haunch, and the tail rises past the caret's top.
        prims.push(DrawPrim::Image {
            x: kitty_x(caret_x, caret_w, h, art),
            y: baseline + px * 0.12 - h,
            w,
            h,
            art,
        });
    }
    // The caret last: it stands over the kitty's haunch.
    prims.push(DrawPrim::Panel {
        x: caret_x,
        y: caret_top,
        w: caret_w,
        h: caret_h,
        radius: 0.0,
        fill: rgba(teal, 255),
    });
}

/// THE RIBBON (§3.1): six flat stripes, R O Y G B V, as tall as the x-height
/// and centred on it, behind the words over `span`. It fades in over 24pt
/// from the first letter and stops at the caret. Each stripe is its colour
/// mixed into the page's `surface` in sRGB and painted opaque: the raster
/// composites in linear light, where a 24% alpha reads twice as strong as the
/// brand image's band.
fn paint_ribbon(
    prims: &mut Vec<DrawPrim>,
    surface: [u8; 3],
    dark: bool,
    px: f32,
    baseline: f32,
    span: std::ops::Range<f32>,
) {
    use crate::native_appearance::mix_srgb;
    let (words_x, caret_x) = (span.start, span.end);
    let band_h = px * 0.56;
    let band_top = baseline - px * 0.36 - band_h / 2.0;
    let stripe_h = band_h / 6.0;
    let alpha = if dark { RIBBON_DARK } else { RIBBON_LIGHT };
    let fade = 24.0;
    let band_w = (caret_x - words_x).max(0.0);
    for k in 0..6 {
        let palette = aterm_effects::harness_card::CardPalette::Cruise;
        let rgb = palette.art(palette.stripe_t(k), dark);
        let rgb = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
        let y = band_top + stripe_h * k as f32;
        // The fade: a few steps of rising alpha, each a panel.
        let steps = 6;
        let step_w = (fade / steps as f32).min(band_w / steps as f32);
        for step in 0..steps {
            let a = alpha * (step as f32 + 0.5) / steps as f32;
            prims.push(DrawPrim::Panel {
                x: words_x + step_w * step as f32,
                y,
                w: step_w,
                h: stripe_h,
                radius: 0.0,
                fill: rgba(mix_srgb(surface, rgb, a), 255),
            });
        }
        let solid_x = words_x + step_w * steps as f32;
        if caret_x > solid_x {
            prims.push(DrawPrim::Panel {
                x: solid_x,
                y,
                w: caret_x - solid_x,
                h: stripe_h,
                radius: 0.0,
                fill: rgba(mix_srgb(surface, rgb, alpha), 255),
            });
        }
    }
}

/// What a row's chip says (§3.3). The node's value is [`Chip::value`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Chip {
    /// Read as wanted: a check, no words.
    InPlace,
    /// Managed and differs; `busy` while its step runs.
    WillChange { busy: bool },
    /// macOS waits for a person.
    NeedsYou,
    /// The last attempt failed.
    Failed,
    /// The group is left as is: the chip column says so, quietly.
    LeftAsIs,
}

impl Chip {
    pub(crate) fn value(self) -> &'static str {
        match self {
            Self::InPlace => "in-place",
            Self::WillChange { busy: false } => "will-change",
            Self::WillChange { busy: true } => "will-change,busy",
            Self::NeedsYou => "needs-you",
            Self::Failed => "failed",
            Self::LeftAsIs => "left",
        }
    }

    pub(crate) fn parse(value: Option<&str>) -> Option<Self> {
        Some(match value? {
            "in-place" => Self::InPlace,
            "will-change" => Self::WillChange { busy: false },
            "will-change,busy" => Self::WillChange { busy: true },
            "needs-you" => Self::NeedsYou,
            "failed" => Self::Failed,
            "left" => Self::LeftAsIs,
            _ => return None,
        })
    }

    /// The chip's painted words (`None` for the check).
    pub(crate) fn words(self) -> Option<&'static str> {
        match self {
            Self::InPlace => None,
            Self::WillChange { .. } => Some("Will change"),
            Self::NeedsYou => Some("Needs you"),
            Self::Failed => Some("Failed"),
            Self::LeftAsIs => Some("Left as is"),
        }
    }

    /// What a screen reader hears for it, in the row's label.
    pub(crate) fn spoken(self) -> &'static str {
        match self {
            Self::InPlace => "in place",
            Self::WillChange { busy: false } => "will change",
            Self::WillChange { busy: true } => "changing now",
            Self::NeedsYou => "needs you",
            Self::Failed => "failed",
            Self::LeftAsIs => "left as is",
        }
    }

    fn size() -> crate::type_scale::StepPx {
        crate::native_ui::native_type_px(TypeStep::Caption)
    }

    /// The chip's painted width: its pill (words, padding and, while busy,
    /// the ring), the check's square, or the quiet words.
    pub(crate) fn width(self) -> f32 {
        let px = Self::size().get();
        match self {
            Self::InPlace => {
                crate::native_ui::native_type_px(TypeStep::Body)
                    .get()
                    .round()
                    + 4.0
            }
            Self::LeftAsIs => {
                crate::tray_raster::ui_text_width_for(TextFace::Ui, "Left as is", px).ceil()
            }
            Self::WillChange { .. } | Self::NeedsYou | Self::Failed => {
                let words = self.words().unwrap_or_default();
                let ring = if matches!(self, Self::WillChange { busy: true }) {
                    px + 4.0
                } else {
                    0.0
                };
                (crate::tray_raster::ui_text_width_for(TextFace::UiBold, words, px) + 18.0 + ring)
                    .ceil()
            }
        }
    }
}

/// A ROW'S CHIP, right-aligned in its rect: the check in the success green;
/// `Will change` on an accent wash (a full accent pill with a ring while its
/// step runs); `Needs you` in the band's warn amber; `Failed` in the danger
/// red; `Left as is` as quiet words.
pub(crate) fn paint_chip(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &Roles,
) {
    use crate::native_appearance::{ensure_text_contrast, mix_srgb};
    let Some(chip) = Chip::parse(value) else {
        return;
    };
    let ground = roles.elevated;
    let size = Chip::size();
    let px = size.get();
    match chip {
        Chip::InPlace => {
            let square = crate::native_ui::native_type_px(TypeStep::Body)
                .get()
                .round();
            prims.push(DrawPrim::BandIcon {
                x: rect.right() - square - 2.0,
                y: rect.y + ((rect.height - square) / 2.0).max(0.0),
                size: square,
                icon: aterm_render::BandIcon::Success,
                color: rgba(ensure_text_contrast(roles.success, ground), 255),
            });
        }
        Chip::LeftAsIs => {
            let words = "Left as is";
            let width = crate::tray_raster::ui_text_width_for(TextFace::Ui, words, px);
            prims.push(text_prim(
                rect.right() - width,
                crate::tray_raster::row_baseline(rect.y, rect.height, px),
                words.to_owned(),
                size,
                TextWeight::Regular,
                TextFace::Ui,
                rgba(crate::native_ui::readable_secondary(roles), 255),
            ));
        }
        Chip::WillChange { .. } | Chip::NeedsYou | Chip::Failed => {
            let words = chip.words().unwrap_or_default();
            let (fill, ink) = match chip {
                Chip::WillChange { busy: true } => (roles.accent, roles.on_accent),
                Chip::WillChange { busy: false } => {
                    let fill = mix_srgb(ground, roles.accent, 0.16);
                    (fill, ensure_text_contrast(roles.accent_ink, fill))
                }
                Chip::NeedsYou => {
                    let amber = crate::chrome_band::warn_mark(ground);
                    let fill = mix_srgb(ground, amber, 0.24);
                    (fill, ensure_text_contrast(amber, fill))
                }
                _ => {
                    let fill = mix_srgb(ground, roles.danger, 0.18);
                    (fill, ensure_text_contrast(roles.danger, fill))
                }
            };
            let w = chip.width().min(rect.width);
            let h = (px + 9.0).min(rect.height);
            let x = rect.right() - w;
            let y = rect.y + (rect.height - h) / 2.0;
            prims.push(DrawPrim::Panel {
                x,
                y,
                w,
                h,
                radius: h / 2.0,
                fill: rgba(fill, 255),
            });
            let mut text_x = x + 9.0;
            if matches!(chip, Chip::WillChange { busy: true }) {
                let r = px / 2.0;
                prims.push(DrawPrim::Ring {
                    cx: text_x + r,
                    cy: y + h / 2.0,
                    r_outer: r,
                    thickness: (px * 0.16).max(1.5),
                    track: rgba(ink, 70),
                    frac: 0.3,
                    fill: rgba(ink, 255),
                });
                text_x += px + 4.0;
            }
            prims.push(text_prim(
                text_x,
                crate::tray_raster::row_baseline(y, h, px),
                words.to_owned(),
                size,
                TextWeight::Bold,
                TextFace::UiBold,
                rgba(ink, 255),
            ));
        }
    }
}

/// The run's METER: a thin capsule, `done` of `total` filled in the accent.
/// The node's value is `done,total`.
pub(crate) fn paint_meter(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &Roles,
) {
    let Some((done, total)) = value.and_then(|value| {
        let (done, total) = value.split_once(',')?;
        Some((done.parse::<f32>().ok()?, total.parse::<f32>().ok()?))
    }) else {
        return;
    };
    let frac = if total > 0.0 {
        (done / total).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let h = rect.height.min(4.0);
    prims.push(DrawPrim::Capsule {
        x: rect.x,
        y: rect.y + (rect.height - h) / 2.0,
        w: rect.width,
        h,
        frac,
        fill: rgba(roles.accent, 255),
        track: rgba(roles.control_track, 255),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_values_round_trip_and_the_kitty_keeps_its_face_on_every_display() {
        for spec in [
            BrandSpec {
                kitty: true,
                px: 22.0,
                scale: 2.0,
            },
            BrandSpec {
                kitty: false,
                px: 17.0,
                scale: 1.0,
            },
        ] {
            assert_eq!(BrandSpec::parse(Some(&spec.value())), spec);
        }
        for (px, room) in [(17.0, 44.0), (22.0, 56.0)] {
            for scale in [1.0, 2.0, 3.0] {
                let spec = BrandSpec {
                    kitty: true,
                    px,
                    scale,
                };
                let device = spec.kitty_height() * scale;
                // The floor holds wherever the ceiling leaves room for it
                // (every 2x and 3x lockup); a 1x one keeps the kitty a mark.
                if scale >= 2.0 {
                    assert!(device >= KITTY_MIN_DEVICE_PX - 0.01, "{px} {scale}");
                }
                assert!(spec.kitty_height() <= px * 0.7 * KITTY_MAX_CAPS + 0.01);
                assert!(spec.needed_height() >= spec.kitty_height());
                assert!(
                    spec.needed_height() <= room,
                    "the lockup keeps its stated height: {px} {scale}"
                );
            }
        }
        // An unreadable value draws the full lockup at the default size.
        assert_eq!(
            BrandSpec::parse(Some("kitty,NaN,0")),
            BrandSpec {
                kitty: true,
                px: 22.0,
                scale: 2.0
            }
        );
    }

    #[test]
    fn chips_round_trip_and_only_the_check_has_no_words() {
        for chip in [
            Chip::InPlace,
            Chip::WillChange { busy: false },
            Chip::WillChange { busy: true },
            Chip::NeedsYou,
            Chip::Failed,
            Chip::LeftAsIs,
        ] {
            assert_eq!(Chip::parse(Some(chip.value())), Some(chip));
            assert_eq!(chip.words().is_none(), chip == Chip::InPlace);
            assert!(!chip.spoken().is_empty());
        }
        assert_eq!(Chip::parse(Some("toggle")), None);
    }
}
