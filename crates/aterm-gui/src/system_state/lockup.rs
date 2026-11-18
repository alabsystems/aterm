// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The brand lockup as the run's progress (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4.5):
//! a teal drawn chevron and `aterm · ALab` (`aterm` alone on a Mac that is not ALab's) at the
//! bottom left, and after the words the rainbow road ([`aterm_effects::rainbow_road`]): a
//! six-stripe ribbon grown out of a point, travelled to the run's progress, the cat standing on
//! it in the pose of the moment, and ahead of its nose dot lanes converging on a teal star. The
//! lockup is the design's 200-unit box at `px / 26` points a unit, under [`design::HEADROOM`]
//! units of room for the cat's ears, its node reaching [`design::GLOW_BELOW`] units further down
//! over the words under it for the road's glow; the chevron and the words are drawn as lines and
//! text in the brand face ([`TextFace::Brand`]: SF Mono as the design sets it, `aterm` bold, the
//! separator and `ALab` medium and quieter, as heavy as the approved render reads, each
//! character 0.3 units apart as the design spaces them), the road and the cat as two baked
//! images. Each step of the run rebakes the road, whose ribbon runs to just past the cat, and the
//! cat only where its pose or its lean in whole degrees changes; a pose change between steps
//! rebakes only the cat, unless it is to or from the loaf, which comes with the done road.
//! Serious Mode and High Contrast draw the words alone in the terminal's mono face, a caret
//! after them and a plain capsule the one progress. One paint-only node; VoiceOver hears it as
//! the page's progress indicator.
//!
//! The images are never baked where they are drawn: the view asks [`super::prebake`] for the
//! moment it wants, and until that moment's images are kept it paints the road and the cat it
//! last painted, as they were baked (their width and scale), clipped to the node, where they
//! were laid out after the same words at the same size on the same ground (so the road starts
//! where these words end); else the words alone ([`LockupSpec::paint`]).

use aterm_effects::rainbow_road::{self as design, CatArt, CatPose, RoadArt};

use super::prebake::Key;
use crate::native_ui::LogicalRect;
use crate::settings::Roles;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, ImageArt, TextFace, TextWeight, rgba, text_prim};

/// The app icon's teal on a dark ground (`❯` and the caret of `>a|`).
const TEAL_DARK: [u8; 3] = [0x5E, 0xEA, 0xD4];
/// The icon's teal printed on a light ground.
const TEAL_LIGHT: [u8; 3] = [0x0D, 0x94, 0x88];
/// The words after the prompt on an ALab Mac.
pub(crate) const ALAB_WORDS: &str = "aterm · ALab";
/// The words after the prompt elsewhere.
pub(crate) const WORDS: &str = "aterm";
/// The road's length moves in steps of this many units: a live resize rebakes the road at each
/// step, not at every point of width.
const LENGTH_STEP: f32 = 8.0;
/// The space the design adds after each character of the words (its `letter-spacing`), in units.
const LETTER_SPACING: f32 = 0.3;

/// What a lockup node draws, from its value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LockupSpec {
    /// The road and the cat show after the words (not in Serious Mode, under High Contrast or in
    /// a pane too narrow for the road).
    pub(crate) road: bool,
    /// The words' size.
    pub(crate) px: f32,
    /// The display's device pixels per point.
    pub(crate) scale: f32,
    /// `aterm · ALab`, else `aterm`.
    pub(crate) alab: bool,
    /// The page paints on a dark ground: the images are baked for it alone.
    pub(crate) dark: bool,
    pub(crate) done: u32,
    pub(crate) total: u32,
    /// The cat's pose; the loaf is the finished run (the whole road, sparkles, no dots).
    pub(crate) pose: CatPose,
}

/// Which ink a run of the words takes.
#[derive(Clone, Copy)]
enum Ink {
    /// `aterm`: the primary ink.
    Primary,
    /// ` · `: the quietest.
    Separator,
    /// `ALab`: between them.
    Secondary,
}

/// A lockup's road and cat as baked: the moment they show, laid out `width` points wide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Baked {
    pub(crate) spec: LockupSpec,
    pub(crate) width: f32,
}

impl Baked {
    /// The road's and the cat's images in a lockup whose top left is `(x, y)`: each its art, its
    /// rect on whole device pixels of the scale it is baked at, and its device size.
    pub(crate) fn images(self, x: f32, y: f32) -> [(ImageArt, LogicalRect, (u32, u32)); 2] {
        self.spec.images(x, y, self.width)
    }

    /// The images to bake, at their device sizes.
    pub(crate) fn keys(self) -> [Key; 2] {
        self.images(0.0, 0.0).map(|(art, _, (w, h))| (art, w, h))
    }

    /// Whether both images are baked and kept.
    pub(crate) fn ready(self) -> bool {
        super::prebake::ready(&self.keys())
    }

    fn value(self) -> String {
        format!("{},{}", self.spec.value(), self.width)
    }

    fn parse(value: &str) -> Option<Self> {
        let (spec, width) = value.rsplit_once(',')?;
        let width = width
            .parse::<f32>()
            .ok()
            .filter(|w| w.is_finite() && *w > 0.0)?;
        Some(Self {
            spec: LockupSpec::parse(Some(spec)),
            width,
        })
    }
}

/// What a lockup node paints: the moment's words, and the road and the cat of the moment whose
/// images are kept (the one wanted once they are, else the one painted last), or none.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LockupPaint {
    pub(crate) spec: LockupSpec,
    pub(crate) images: Option<Baked>,
    /// The moment's own images are still baking: what is painted is the last moment's, or the
    /// words alone. A reader that takes a settled frame waits for it to clear.
    pub(crate) pending: bool,
}

impl LockupPaint {
    /// The node value the painter reads back: `!` before it while the moment's images bake.
    pub(crate) fn value(self) -> String {
        let pending = if self.pending { "!" } else { "" };
        match self.images {
            Some(images) => format!("{pending}{};{}", self.spec.value(), images.value()),
            None => format!("{pending}{}", self.spec.value()),
        }
    }

    pub(crate) fn parse(value: Option<&str>) -> Self {
        let value = value.unwrap_or_default();
        let (pending, value) = match value.strip_prefix('!') {
            Some(value) => (true, value),
            None => (false, value),
        };
        let (spec, images) = match value.split_once(';') {
            Some((spec, images)) => (spec, Baked::parse(images)),
            None => (value, None),
        };
        Self {
            spec: LockupSpec::parse(Some(spec)),
            images,
            pending,
        }
    }

    /// The line the page's paint audit adds for a lockup node `key` whose value is `value`
    /// (`inspect app/v1 view … audit`): whether its moment's images still bake, which the
    /// System State gallery waits out before it takes a frame.
    pub(crate) fn audit_line(key: &str, value: Option<&str>) -> String {
        let paint = Self::parse(value);
        let drawn = match paint.images {
            Some(images) if images.spec == paint.spec => "moment",
            Some(_) => "last",
            None => "words",
        };
        format!(
            "paint-lockup key={key:?} road={} drawn={drawn} pending={}",
            paint.spec.road, paint.pending
        )
    }
}

impl LockupSpec {
    /// The spec's part of a node value.
    pub(crate) fn value(self) -> String {
        let pose = CatPose::ALL
            .iter()
            .position(|pose| *pose == self.pose)
            .unwrap_or(0);
        format!(
            "{},{},{},{},{},{},{},{}",
            if self.road { "road" } else { "text" },
            self.px,
            self.scale,
            u8::from(self.alab),
            u8::from(self.dark),
            self.done,
            self.total,
            pose
        )
    }

    fn parse(value: Option<&str>) -> Self {
        let mut parts = value.unwrap_or_default().split(',');
        let road = parts.next() == Some("road");
        let number = |part: Option<&str>, fallback: f32| {
            part.and_then(|part| part.parse::<f32>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .unwrap_or(fallback)
        };
        let px = number(parts.next(), 22.0);
        let scale = number(parts.next(), 2.0).max(1.0);
        let alab = parts.next() == Some("1");
        let dark = parts.next() == Some("1");
        let whole = |part: Option<&str>| part.and_then(|part| part.parse::<u32>().ok());
        let done = whole(parts.next()).unwrap_or(0);
        let total = whole(parts.next()).unwrap_or(0);
        let pose = parts
            .next()
            .and_then(|part| part.parse::<usize>().ok())
            .and_then(|at| CatPose::ALL.get(at).copied())
            .unwrap_or(CatPose::ALL[0]);
        Self {
            road,
            px,
            scale,
            alab,
            dark,
            done: done.min(total),
            total,
            pose,
        }
    }

    /// The words after the prompt.
    pub(crate) const fn words(self) -> &'static str {
        if self.alab { ALAB_WORDS } else { WORDS }
    }

    /// The words' runs with the road, each its weight and ink: `aterm` strong, the separator
    /// quietest and `ALab` between.
    fn runs(self) -> &'static [(&'static str, TextWeight, Ink)] {
        if self.alab {
            &[
                ("aterm", TextWeight::Bold, Ink::Primary),
                (" · ", TextWeight::Regular, Ink::Separator),
                ("ALab", TextWeight::Regular, Ink::Secondary),
            ]
        } else {
            &[("aterm", TextWeight::Bold, Ink::Primary)]
        }
    }

    /// How far the run is: done of all, 0 before anything is known; all of it once done.
    pub(crate) fn progress(self) -> f32 {
        if self.pose == CatPose::Loaf {
            1.0
        } else if self.total == 0 {
            0.0
        } else {
            (self.done as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }

    /// Points a design unit: the design is laid out for a 26 px mono face.
    fn unit(self) -> f32 {
        self.px / design::DESIGN_PX
    }

    /// The design box's top, below the node's: the room the cat's ears may reach into.
    fn headroom(self) -> f32 {
        design::HEADROOM * self.unit()
    }

    /// The height a lockup needs: the design's whole box under its headroom with the road, else
    /// the words.
    pub(crate) fn needed_height(self) -> f32 {
        if self.road {
            (design::DESIGN_H + design::HEADROOM) * self.unit()
        } else {
            self.px * 1.3
        }
    }

    /// How far under its box the road's glow reaches ([`design::GLOW_BELOW`]): a lockup node
    /// with the road reaches that much further down, over the words under it.
    pub(crate) fn glow_below(self) -> f32 {
        design::GLOW_BELOW * self.unit()
    }

    /// One mono advance at this size.
    fn advance(self) -> f32 {
        crate::tray_raster::measure_text("m", self.px, TextWeight::Regular).max(self.px * 0.5)
    }

    /// The words' size on the type scale.
    fn words_px(self) -> f32 {
        TypeStep::Body.px(self.px).get()
    }

    /// Each character of a run of the words with the road, in the brand face, with its
    /// advance: its own width and the design's letter spacing after it.
    fn run_chars(self, text: &str, weight: TextWeight) -> impl Iterator<Item = (char, f32)> {
        let (px, space) = (self.words_px(), LETTER_SPACING * self.unit());
        text.chars().map(move |ch| {
            let mut one = [0; 4];
            let width = crate::tray_raster::ui_text_width_for(
                TextFace::Brand(weight),
                ch.encode_utf8(&mut one),
                px,
            );
            (ch, width + space)
        })
    }

    /// One run of the words' width with the road.
    fn run_width(self, text: &str, weight: TextWeight) -> f32 {
        self.run_chars(text, weight)
            .map(|(_, advance)| advance)
            .sum()
    }

    /// Where the words end, from the node's left edge: with the road, after the chevron's
    /// room, each run in its own weight.
    fn words_end(self) -> f32 {
        if !self.road {
            let words =
                crate::tray_raster::measure_text(self.words(), self.px, TextWeight::Regular);
            return self.advance() * 2.0 + words;
        }
        design::WORDS_X * self.unit()
            + self
                .runs()
                .iter()
                .map(|(text, weight, _)| self.run_width(text, *weight))
                .sum::<f32>()
    }

    /// Where the road starts, from the node's left edge: a little past the words.
    fn road_x(self) -> f32 {
        self.words_end() + design::ROAD_GAP * self.unit()
    }

    /// The least width that draws it whole: the words and the least road (or the caret and
    /// the capsule's least length).
    pub(crate) fn least_width(self) -> f32 {
        if self.road {
            (self.road_x() + design::LEAST_LENGTH * self.unit()).ceil()
        } else {
            let advance = self.advance();
            (self.caret_x() + caret_width(advance) + 12.0 + 24.0).ceil()
        }
    }

    /// The words-alone caret's left edge: just past the words, where it stays; the capsule after
    /// it is the one progress.
    pub(crate) fn caret_x(self) -> f32 {
        self.words_end() + self.advance() * 0.2
    }

    /// The road's art for a lockup `width` wide: as long as the width holds, in whole steps of
    /// [`LENGTH_STEP`], from the least road to the longest.
    fn road_art(self, width: f32) -> RoadArt {
        let room = (width - self.road_x()) / self.unit();
        let length = ((room / LENGTH_STEP).floor() * LENGTH_STEP)
            .clamp(design::LEAST_LENGTH, design::MAX_LENGTH);
        let done = self.pose == CatPose::Loaf;
        RoadArt {
            length: length as u16,
            permille: (self.progress() * 1000.0).round() as u16,
            done,
            dark: self.dark,
        }
    }

    /// The road's and the cat's images in a lockup whose top left is `(x, y)`, `width` wide,
    /// each on whole device pixels and at the whole device size it is baked at (returned beside
    /// it): the road from just past the words over the box's whole height and the glow under it,
    /// the cat with its feet on it at the run's progress, leaning with it.
    fn images(self, x: f32, y: f32, width: f32) -> [(ImageArt, LogicalRect, (u32, u32)); 2] {
        let s = self.scale;
        let road = self.road_art(width);
        let road_h = (design::ROAD_H * self.unit() * s).round().max(1.0);
        let road_w = (road_h * road.aspect()).round().max(1.0);
        // Device pixels a unit, as the road is baked.
        let unit = road_h / design::ROAD_H;
        let left = ((x + self.road_x()) * s).round();
        let top = ((y + self.headroom()) * s).round();
        let walk = road.road();
        let cat = CatArt::on(walk, self.pose, self.dark);
        let cat_h = (design::CAT_H * unit).round().max(1.0);
        let cat_w = (cat_h * CatArt::ASPECT).round().max(1.0);
        let (fx, fy) = walk.feet();
        let cat_x = left + ((fx - design::ROAD_X - design::CAT_FEET.0) * unit).round();
        let cat_y = top + ((fy - design::CAT_FEET.1) * unit).round();
        let rect = |x: f32, y: f32, w: f32, h: f32| LogicalRect::new(x / s, y / s, w / s, h / s);
        [
            (
                ImageArt::Road(road),
                rect(left, top, road_w, road_h),
                (road_w as u32, road_h as u32),
            ),
            (
                ImageArt::Cat(cat),
                rect(cat_x, cat_y, cat_w, cat_h),
                (cat_w as u32, cat_h as u32),
            ),
        ]
    }

    /// What a view paints for this moment in a lockup `width` wide, having last painted `last`'s
    /// images: this moment's own once both are kept, else `last`'s while they are and were laid
    /// out after these words at this size on this ground (their width and scale as baked), else
    /// none, the words alone until a bake lands. Laid out after other words (`aterm` became
    /// `aterm · ALab` mid-run) or at another size (a resize past a size step), the last road
    /// starts where those words ended and would lie over these or stand apart from them. It
    /// never bakes: the view asks [`super::prebake::Requester::want`] for this moment's
    /// ([`Self::wanted`]) and is woken when they land.
    pub(crate) fn paint(self, width: f32, last: Option<Baked>) -> LockupPaint {
        let wanted = self.wanted(width);
        let images = if self.road {
            if wanted.ready() {
                Some(wanted)
            } else {
                last.filter(|last| {
                    let laid = last.spec;
                    laid.road
                        && laid.dark == self.dark
                        && laid.alab == self.alab
                        && laid.px == self.px
                        && last.ready()
                })
            }
        } else {
            None
        };
        LockupPaint {
            spec: self,
            images,
            pending: self.road && images != Some(wanted),
        }
    }

    /// This moment's images in a lockup `width` wide.
    pub(crate) const fn wanted(self, width: f32) -> Baked {
        Baked { spec: self, width }
    }
}

/// The caret's width for a mono `advance`.
fn caret_width(advance: f32) -> f32 {
    (advance * 0.55).round().max(2.0)
}

/// The lockup (§4.5), filling `rect`.
pub(crate) fn paint_lockup(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &Roles,
) {
    let paint = LockupPaint::parse(value);
    // It stands on the page, under the hero, not on a card.
    let page = super::marks::page_ground(roles);
    let dark = crate::native_appearance::surface_is_dark(page);
    let teal = crate::native_appearance::ensure_text_contrast(
        if dark { TEAL_DARK } else { TEAL_LIGHT },
        page,
    );
    if paint.spec.road {
        paint_road(prims, rect, paint, dark, teal, roles);
    } else {
        paint_words_alone(prims, rect, paint.spec, teal, roles);
    }
}

/// The design's lockup: the chevron and the words at its bottom left, and the road and the cat
/// whose images are kept, clipped to the node, which reaches under the box for the road's glow (a
/// road kept from a wider page or another display is drawn as it was baked until this one's
/// lands). Images baked for the other ground draw
/// nothing: the page's appearance changed, and the view asks for this one's.
fn paint_road(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    paint: LockupPaint,
    dark: bool,
    teal: [u8; 3],
    roles: &Roles,
) {
    let spec = paint.spec;
    let unit = spec.unit();
    let baseline = rect.y + spec.headroom() + design::BASELINE * unit;
    let point = |(x, y): (f32, f32)| (rect.x + x * unit, baseline + y * unit);
    for pair in design::CHEVRON.windows(2) {
        let ((x1, y1), (x2, y2)) = (point(pair[0]), point(pair[1]));
        prims.push(DrawPrim::Line {
            x1,
            y1,
            x2,
            y2,
            width: design::CHEVRON_STROKE * unit,
            color: rgba(teal, 255),
        });
    }
    let size = TypeStep::Body.px(spec.px);
    let mut x = rect.x + design::WORDS_X * unit;
    for (text, weight, ink) in spec.runs() {
        let color = match ink {
            Ink::Primary => roles.text_primary,
            Ink::Separator => roles.text_tertiary,
            Ink::Secondary => roles.text_secondary,
        };
        // A character at a time, the design's letter spacing after each.
        for (ch, advance) in spec.run_chars(text, *weight) {
            if !ch.is_whitespace() {
                prims.push(text_prim(
                    x,
                    baseline,
                    ch.to_string(),
                    size,
                    *weight,
                    TextFace::Brand(*weight),
                    rgba(color, 255),
                ));
            }
            x += advance;
        }
    }
    let Some(images) = paint.images.filter(|images| images.spec.dark == dark) else {
        return;
    };
    prims.push(DrawPrim::ClipPush {
        x: rect.x,
        y: rect.y,
        w: rect.width,
        h: rect.height,
    });
    for (art, at, device) in images.images(rect.x, rect.y) {
        prims.push(DrawPrim::Image {
            x: at.x,
            y: at.y,
            w: at.width,
            h: at.height,
            art,
            device,
            alpha: 255,
        });
    }
    prims.push(DrawPrim::ClipPop);
}

/// Serious Mode and High Contrast: the prompt and the words, the caret after them, and a plain
/// capsule after it, the one progress.
fn paint_words_alone(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    spec: LockupSpec,
    teal: [u8; 3],
    roles: &Roles,
) {
    let px = spec.px;
    let advance = spec.advance();
    let cap = px * 0.7;
    let baseline = crate::tray_raster::row_baseline(rect.y, rect.height, px);
    let x0 = rect.x;
    let caret_x = x0 + spec.caret_x();
    let caret_w = caret_width(advance);
    let band_mid = baseline - px * 0.36;
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
        x0 + advance * 2.0,
        baseline,
        spec.words().to_owned(),
        TypeStep::Body.px(px),
        TextWeight::Regular,
        crate::widget::TextFace::Mono,
        rgba(roles.text_primary, 255),
    ));
    prims.push(DrawPrim::Panel {
        x: caret_x,
        y: baseline - px * 0.95,
        w: caret_w,
        h: px * 1.25,
        radius: 0.0,
        fill: rgba(teal, 255),
    });
}

#[cfg(test)]
#[path = "lockup_tests.rs"]
mod tests;
