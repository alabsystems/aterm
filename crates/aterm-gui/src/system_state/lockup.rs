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
mod tests {
    use super::*;

    fn spec(px: f32, scale: f32, done: u32, total: u32) -> LockupSpec {
        LockupSpec {
            kitty: true,
            px,
            scale,
            alab: true,
            done,
            total,
            pose: PetGlyphId::PetSit,
        }
    }

    #[test]
    fn lockup_values_round_trip() {
        for value in [
            spec(22.0, 2.0, 11, 16),
            LockupSpec {
                kitty: false,
                alab: false,
                pose: PetGlyphId::PetLoaf,
                ..spec(17.0, 1.0, 0, 0)
            },
        ] {
            assert_eq!(LockupSpec::parse(Some(&value.value())), value);
        }
        // Unreadable numbers fall back; a count past the total is the total.
        let read = LockupSpec::parse(Some("kitty,NaN,0,1,20,16,99"));
        assert_eq!(
            (read.px, read.scale, read.done, read.total),
            (22.0, 2.0, 16, 16)
        );
        assert_eq!(read.pose, POSES[0]);
    }

    #[test]
    fn the_caret_runs_from_the_words_to_the_far_end_as_steps_finish() {
        let width = 852.0;
        let start = spec(22.0, 2.0, 0, 16);
        let half = spec(22.0, 2.0, 8, 16);
        let end = spec(22.0, 2.0, 16, 16);
        let (a, b, c) = (
            start.caret_x(width),
            half.caret_x(width),
            end.caret_x(width),
        );
        assert!(a < b && b < c, "{a} {b} {c}");
        assert!(a >= start.words_end(), "the caret starts past the words");
        // The kitty fits at the far end, whichever pose it is in.
        let advance = start.advance();
        assert!(c + caret_width(advance) + end.kitty_width() <= width + 0.01);
        // Halfway is halfway along the caret's path.
        assert!(((b - a) - (c - b)).abs() < 0.01);
        // Nothing known yet: the start.
        assert_eq!(spec(22.0, 2.0, 0, 0).caret_x(width), a);
    }

    #[test]
    fn the_kitty_keeps_its_face_and_whole_device_pixels_on_every_display() {
        for (px, room) in [(17.0, 40.0), (22.0, 48.0)] {
            for scale in [1.0_f32, 2.0, 3.0] {
                let s = spec(px, scale, 0, 1);
                let device = s.kitty_height() * scale;
                assert!((device - device.round()).abs() < 0.001, "{px} {scale}");
                if scale >= 2.0 {
                    assert!(device >= KITTY_MIN_DEVICE_PX - 0.01, "{px} {scale}");
                }
                assert!(
                    s.needed_height() <= room,
                    "the lockup keeps its height: {px} {scale}"
                );
                assert!(s.least_width() > s.words_end());
            }
        }
    }

    /// The first stripe's panels `paint_ribbon` draws past the words, left to right.
    fn past_the_words(dark: bool, words_end: f32, caret_x: f32) -> Vec<(f32, [u8; 4])> {
        let surface = if dark { [30, 30, 30] } else { [231, 231, 231] };
        let mut prims = Vec::new();
        paint_ribbon(
            &mut prims, surface, dark, 20.0, 10.0, 0.0, words_end, caret_x,
        );
        let top = 20.0 - 5.0;
        let mut past: Vec<(f32, [u8; 4])> = prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Panel { x, y, fill, .. }
                    if (*y - top).abs() < 0.01 && *x >= words_end - 0.01 =>
                {
                    Some((*x, *fill))
                }
                _ => None,
            })
            .collect();
        past.sort_by(|a, b| a.0.total_cmp(&b.0));
        past
    }

    /// THE RIBBON IS THE BRAND'S TRAIL, NOT A NYAN BAND (§4.5, D4): past the words it rises from
    /// the trail's own strength toward the caret, like a comet's tail, and at the caret it is
    /// never half again as strong as the trail; and it draws nothing past the words until the
    /// caret stands 4 pt clear of them. NEGATIVE CONTROL: with room, it does draw there.
    #[test]
    fn the_ribbon_rises_like_a_comet_from_the_trail_and_never_piles_up_at_the_start() {
        for dark in [true, false] {
            let (under, filled) = if dark {
                (UNDER_WORDS_DARK, FILLED_DARK)
            } else {
                (UNDER_WORDS_LIGHT, FILLED_LIGHT)
            };
            assert!(filled > under && filled <= under * 1.5, "dark: {dark}");
            assert!(past_the_words(dark, 200.0, 200.0 + LEAST_PAST - 0.5).is_empty());
            let past = past_the_words(dark, 200.0, 600.0);
            assert!(past.len() > 4, "the ribbon past the words draws");
            let surface = if dark { [30, 30, 30] } else { [231, 231, 231] };
            let palette = aterm_effects::harness_card::CardPalette::Cruise;
            let rgb = palette.art(palette.stripe_t(0), dark);
            let rgb = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
            let apart = |fill: [u8; 4]| {
                (0..3)
                    .map(|i| (i32::from(fill[i]) - i32::from(surface[i])).abs())
                    .sum::<i32>()
            };
            let rises: Vec<i32> = past.iter().map(|(_, fill)| apart(*fill)).collect();
            assert!(
                rises.windows(2).all(|pair| pair[0] <= pair[1])
                    && rises[0] < rises[rises.len() - 1],
                "dark {dark}: it rises toward the caret: {rises:?}"
            );
            let at = |a: f32| {
                apart(rgba(
                    crate::native_appearance::mix_srgb(surface, rgb, a),
                    255,
                ))
            };
            let (first, last) = (past[0].1, past[past.len() - 1].1);
            assert!(
                apart(first) <= at(under + 0.02),
                "dark {dark}: it starts at the trail's"
            );
            assert!(
                apart(last) <= at(filled),
                "dark {dark}: and ends at its full strength"
            );
        }
    }

    /// HIGH CONTRAST AND SERIOUS MODE SHOW ONE PROGRESS (§4.9's rubric): the caret stays after
    /// the words, and the capsule after it fills as steps finish, full at done. NEGATIVE CONTROL:
    /// with the kitty the caret is what moves.
    #[test]
    fn the_words_alone_show_one_progress_and_a_full_track_at_done() {
        let width = 852.0;
        let text = |done| LockupSpec {
            kitty: false,
            ..spec(22.0, 2.0, done, 16)
        };
        let carets: Vec<f32> = [0, 8, 16].map(|done| text(done).caret_x(width)).to_vec();
        assert!(
            carets.iter().all(|x| (x - carets[0]).abs() < 0.01),
            "{carets:?}"
        );
        let kitty: Vec<f32> = [0, 16]
            .map(|done| spec(22.0, 2.0, done, 16).caret_x(width))
            .to_vec();
        assert!(
            kitty[1] > kitty[0] + 100.0,
            "with the kitty the caret moves"
        );
        let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
        let capsule = |done| {
            let mut prims = Vec::new();
            paint_lockup(
                &mut prims,
                LogicalRect::new(0.0, 0.0, width, 48.0),
                Some(&text(done).value()),
                &roles,
            );
            let capsules: Vec<(f32, f32, f32)> = prims
                .iter()
                .filter_map(|prim| match prim {
                    DrawPrim::Capsule { x, w, frac, .. } => Some((*x, *w, *frac)),
                    _ => None,
                })
                .collect();
            assert_eq!(capsules.len(), 1, "one progress");
            capsules[0]
        };
        let (start, half, done) = (capsule(0), capsule(8), capsule(16));
        assert_eq!(
            (start.0, start.1),
            (done.0, done.1),
            "the track stands still"
        );
        assert_eq!((start.2, half.2, done.2), (0.0, 0.5, 1.0), "and fills");
        assert!(done.0 + done.1 >= width - 0.01, "to the end of the lockup");
    }

    #[test]
    fn a_mac_outside_alab_says_aterm_alone() {
        let alab = spec(22.0, 2.0, 0, 1);
        let elsewhere = LockupSpec {
            alab: false,
            ..alab
        };
        assert_eq!(alab.words(), "aterm · ALab");
        assert_eq!(elsewhere.words(), "aterm");
        assert!(elsewhere.least_width() < alab.least_width());
    }
}
