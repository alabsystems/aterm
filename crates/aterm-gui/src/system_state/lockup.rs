// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The brand lockup as the run's progress (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4.5):
//! `❯ aterm · ALab▌` in the terminal's mono face (`❯ aterm▌` on a Mac that is not ALab's), the
//! prompt and caret in the app icon's teal, the six-stripe ribbon from the first letter to the
//! caret, dim as the brand image's trail and rising toward the caret like a comet's, the caret
//! at `words_end + p·(W − words_end − kitty_w)` for `p` steps done of all, a 2 pt hairline track
//! past it, and the kitty standing on the baseline behind the caret in the pose of the moment.
//! Serious Mode and High Contrast draw the words alone, the caret still after them and a plain
//! capsule the one progress. One paint-only node; VoiceOver hears it as the page's progress
//! indicator.

use aterm_effects::pet_glyphs_gen::PetGlyphId;

use super::kitty::POSES;
use crate::native_ui::LogicalRect;
use crate::settings::Roles;
use crate::type_scale::TypeStep;
use crate::widget::{ArtSpan, DrawPrim, ImageArt, TextFace, TextWeight, rgba, text_prim};

/// The app icon's teal on a dark ground (`❯` and the caret of `>a|`).
const TEAL_DARK: [u8; 3] = [0x5E, 0xEA, 0xD4];
/// The icon's teal printed on a light ground.
const TEAL_LIGHT: [u8; 3] = [0x0D, 0x94, 0x88];
/// The words after the prompt on an ALab Mac.
pub(crate) const ALAB_WORDS: &str = "aterm · ALab";
/// The words after the prompt elsewhere.
pub(crate) const WORDS: &str = "aterm";
/// The kitty is baked at least this many device pixels tall where the lockup has room for it
/// (2x displays and up): below it the pet baker drops the face's detail.
const KITTY_MIN_DEVICE_PX: f32 = aterm_effects::pet_baker::FACE_DETAIL_MIN_H as f32;
/// The kitty is never taller than this many mono capitals: on a 1x display the device-pixel
/// floor gives way here, so the kitty stays a mark beside the words.
const KITTY_MAX_CAPS: f32 = 2.4;
/// How strong the ribbon is under the words, mixed into the page in sRGB as the brand image is
/// (`assets/aterm-rainbow-kitty.png` reads about 24% on its dark ground): the words stay text.
const UNDER_WORDS_DARK: f32 = 0.24;
const UNDER_WORDS_LIGHT: f32 = 0.30;
/// How strong the ribbon is at the caret, where it is the progress: a little over the trail
/// under the words, so it stays the brand's rainbow kitty and never a nyan cat's bright band.
const FILLED_DARK: f32 = 0.34;
const FILLED_LIGHT: f32 = 0.38;
/// The ribbon's height, in mono sizes: about the x-height, as the brand image's trail is.
const BAND_H: f32 = 0.45;
/// The least gap between the words and the caret the ribbon past the words is drawn in: at the
/// run's start no stub of it piles up against the last letter with the caret and the kitty.
const LEAST_PAST: f32 = 4.0;
/// The most slices the ribbon past the words rises in: each a step of strength no eye parts
/// from the next.
const PAST_STEPS: f32 = 16.0;

/// What a lockup node draws, from its value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LockupSpec {
    /// The kitty and the ribbon show (not Serious Mode, not High Contrast).
    pub(crate) kitty: bool,
    /// The mono size.
    pub(crate) px: f32,
    /// The display's device pixels per point.
    pub(crate) scale: f32,
    /// `aterm · ALab`, else `aterm`.
    pub(crate) alab: bool,
    pub(crate) done: u32,
    pub(crate) total: u32,
    /// The kitty's pose, one of [`POSES`].
    pub(crate) pose: PetGlyphId,
}

impl LockupSpec {
    /// The node value the painter reads back.
    pub(crate) fn value(self) -> String {
        let pose = POSES
            .iter()
            .position(|pose| *pose == self.pose)
            .unwrap_or(0);
        format!(
            "{},{},{},{},{},{},{}",
            if self.kitty { "kitty" } else { "text" },
            self.px,
            self.scale,
            u8::from(self.alab),
            self.done,
            self.total,
            pose
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
        let alab = parts.next() == Some("1");
        let whole = |part: Option<&str>| part.and_then(|part| part.parse::<u32>().ok());
        let done = whole(parts.next()).unwrap_or(0);
        let total = whole(parts.next()).unwrap_or(0);
        let pose = parts
            .next()
            .and_then(|part| part.parse::<usize>().ok())
            .and_then(|at| POSES.get(at).copied())
            .unwrap_or(POSES[0]);
        Self {
            kitty,
            px,
            scale,
            alab,
            done: done.min(total),
            total,
            pose,
        }
    }

    /// The words after the prompt.
    pub(crate) const fn words(self) -> &'static str {
        if self.alab { ALAB_WORDS } else { WORDS }
    }

    /// How far the run is: done of all, 0 before anything is known.
    pub(crate) fn progress(self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }

    /// The mono face's capital height at this size (the Latin norm).
    fn cap(self) -> f32 {
        self.px * 0.7
    }

    /// The kitty's height in points: about 1.9 capitals, at least [`KITTY_MIN_DEVICE_PX`]
    /// device pixels, at most [`KITTY_MAX_CAPS`] capitals, and a whole number of device pixels.
    pub(crate) fn kitty_height(self) -> f32 {
        let h = (self.cap() * 1.9)
            .max(KITTY_MIN_DEVICE_PX / self.scale)
            .min(self.cap() * KITTY_MAX_CAPS);
        (h * self.scale).round() / self.scale
    }

    /// The widest pose's width at the kitty's height: the caret's path leaves room for it, so a
    /// pose change never moves the caret.
    pub(crate) fn kitty_width(self) -> f32 {
        let h = self.kitty_height();
        POSES
            .iter()
            .map(|pose| h * aterm_effects::pet_baker::PetBaker::aspect(*pose))
            .fold(0.0, f32::max)
    }

    /// The height a lockup needs: the words and, when it shows, the kitty on their baseline.
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

    /// Where the words end, from the node's left edge.
    fn words_end(self) -> f32 {
        let words = crate::tray_raster::measure_text(self.words(), self.px, TextWeight::Regular);
        self.advance() * 2.0 + words
    }

    /// The least width that draws it whole: prompt, words, caret and kitty (or the capsule's
    /// least length) at the start of the run.
    pub(crate) fn least_width(self) -> f32 {
        let advance = self.advance();
        let caret_x = self.words_end() + advance * 0.2;
        let tail = if self.kitty {
            caret_width(advance) + self.kitty_width()
        } else {
            caret_width(advance) + 12.0 + 24.0
        };
        (caret_x + tail).ceil()
    }

    /// The caret's left edge for a lockup `width` wide: just past the words at the start, its
    /// kitty at the far end once everything is done. With the words alone the caret stays after
    /// them, and the capsule is the one progress.
    pub(crate) fn caret_x(self, width: f32) -> f32 {
        self.caret_x_at(width, self.progress())
    }

    /// The caret's left edge at `progress` (`0.0..=1.0`) of the run.
    fn caret_x_at(self, width: f32, progress: f32) -> f32 {
        let advance = self.advance();
        let start = self.words_end() + advance * 0.2;
        if !self.kitty {
            return start;
        }
        let end = width - caret_width(advance) - self.kitty_width();
        start + progress.clamp(0.0, 1.0) * (end - start).max(0.0)
    }
}

/// The caret's width for a mono `advance`.
fn caret_width(advance: f32) -> f32 {
    (advance * 0.55).round().max(2.0)
}

/// The left edge of the kitty's `h`-tall box behind a caret at `caret_x`, `caret_w` wide, placed
/// by the pixels the art draws: its drawn left edge stands at the caret's middle, so the caret
/// covers its haunch, as in `assets/aterm-rainbow-kitty.png`, and none of it shows over the
/// letters.
fn kitty_x(caret_x: f32, caret_w: f32, h: f32, art: ImageArt) -> f32 {
    let drawn = art.mask().drawn(ArtSpan::BOX).unwrap_or(ArtSpan::BOX);
    caret_x + caret_w * 0.5 - drawn.left * h * art.aspect()
}

/// The lockup (§4.5), filling `rect`.
pub(crate) fn paint_lockup(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &Roles,
) {
    let spec = LockupSpec::parse(value);
    // It stands on the page, under the hero, not on a card.
    let page = super::marks::page_ground(roles);
    let dark = crate::native_appearance::surface_is_dark(page);
    let teal = crate::native_appearance::ensure_text_contrast(
        if dark { TEAL_DARK } else { TEAL_LIGHT },
        page,
    );
    let px = spec.px;
    let size = TypeStep::Body.px(px);
    let advance = spec.advance();
    let cap = spec.cap();
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
    let words_end = x0 + spec.words_end();
    let caret_x = x0 + spec.caret_x(rect.width);
    let caret_w = caret_width(advance);
    let caret_top = baseline - px * 0.95;
    let caret_h = px * 1.25;
    let band_h = px * BAND_H;
    let band_mid = baseline - px * 0.36;

    if spec.kitty {
        // The track the caret has still to cover, then the ribbon it covered.
        let track_x = caret_x + caret_w;
        if rect.right() > track_x {
            prims.push(DrawPrim::Panel {
                x: track_x,
                y: band_mid - 1.0,
                w: rect.right() - track_x,
                h: 2.0,
                radius: 1.0,
                fill: rgba(roles.control_track, 255),
            });
        }
        paint_ribbon(
            prims, page, dark, band_mid, band_h, words_x, words_end, caret_x,
        );
    } else {
        // Words alone: the progress is a plain capsule after the caret.
        let x = caret_x + caret_w + 12.0;
        if rect.right() > x {
            prims.push(DrawPrim::Capsule {
                x,
                y: band_mid - 2.0,
                w: rect.right() - x,
                h: 4.0,
                frac: spec.progress(),
                fill: rgba(crate::native_ui::readable_secondary(roles), 255),
                track: rgba(roles.control_track, 255),
            });
        }
    }

    // The prompt: a heavy chevron one advance wide, drawn, so no font needs to carry `❯`.
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
        spec.words().to_owned(),
        size,
        TextWeight::Regular,
        TextFace::Mono,
        rgba(roles.text_primary, 255),
    ));

    if spec.kitty {
        let h = spec.kitty_height();
        let art = ImageArt::Kitty {
            pose: spec.pose,
            dark,
        };
        let (dw, dh) = super::kitty::device_size(art, h, spec.scale);
        let (w, h) = (dw as f32 / spec.scale, dh as f32 / spec.scale);
        // Whole device pixels, so the bake lands one to one.
        let x = ((kitty_x(caret_x, caret_w, h, art)) * spec.scale).round() / spec.scale;
        let y = ((baseline + px * 0.12 - h) * spec.scale).round() / spec.scale;
        prims.push(DrawPrim::Image {
            x,
            y,
            w,
            h,
            art,
            alpha: 255,
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

/// The ribbon: six flat stripes, R O Y G B V, about the x-height and centred on it, from the
/// first letter to the caret: dim behind the words, where they stay text, as the brand image's
/// trail is, and past them, where it is the progress, rising from that strength to its strongest
/// at the caret, like a comet's tail; none past the words until the caret stands
/// [`LEAST_PAST`] clear of them. Each stripe is its colour mixed into the page's `surface` in
/// sRGB and painted opaque.
#[allow(
    clippy::too_many_arguments,
    reason = "the band's place and its three edges, as the lockup measures them"
)]
fn paint_ribbon(
    prims: &mut Vec<DrawPrim>,
    surface: [u8; 3],
    dark: bool,
    band_mid: f32,
    band_h: f32,
    words_x: f32,
    words_end: f32,
    caret_x: f32,
) {
    use crate::native_appearance::mix_srgb;
    let band_top = band_mid - band_h / 2.0;
    let stripe_h = band_h / 6.0;
    let (under, filled) = if dark {
        (UNDER_WORDS_DARK, FILLED_DARK)
    } else {
        (UNDER_WORDS_LIGHT, FILLED_LIGHT)
    };
    let fade = 24.0;
    let palette = aterm_effects::harness_card::CardPalette::Cruise;
    for k in 0..6 {
        let rgb = palette.art(palette.stripe_t(k), dark);
        let rgb = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
        let y = band_top + stripe_h * k as f32;
        let mut panel = |x: f32, w: f32, a: f32| {
            if w > 0.0 {
                prims.push(DrawPrim::Panel {
                    x,
                    y,
                    w,
                    h: stripe_h,
                    radius: 0.0,
                    fill: rgba(mix_srgb(surface, rgb, a), 255),
                });
            }
        };
        // Under the words, fading in over the first 24 pt.
        let under_end = words_end.min(caret_x);
        let steps = 6;
        let step_w = (fade / steps as f32).min((under_end - words_x).max(0.0) / steps as f32);
        for step in 0..steps {
            let a = under * (step as f32 + 0.5) / steps as f32;
            panel(words_x + step_w * step as f32, step_w, a);
        }
        let solid_x = words_x + step_w * steps as f32;
        panel(solid_x, under_end - solid_x, under);
        // Past the words, the progress, rising toward the caret.
        let past = caret_x - under_end;
        if past >= LEAST_PAST {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a count of slices in 1..=16"
            )]
            let slices = (past / 4.0).ceil().clamp(1.0, PAST_STEPS) as u8;
            let slice = past / f32::from(slices);
            for at in 0..slices {
                let along = (f32::from(at) + 0.5) / f32::from(slices);
                panel(
                    under_end + slice * f32::from(at),
                    slice,
                    past_strength(under, filled, along),
                );
            }
        }
    }
}

/// The ribbon's strength `along` (`0.0..=1.0`) the way from the words to the caret: the trail's
/// own strength at the words, its full strength at the caret.
fn past_strength(under: f32, filled: f32, along: f32) -> f32 {
    under + (filled - under) * along.clamp(0.0, 1.0)
}

#[cfg(test)]
#[path = "lockup_tests.rs"]
mod tests;
