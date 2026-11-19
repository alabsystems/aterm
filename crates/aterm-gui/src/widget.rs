// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Renderer-agnostic overlay drawing primitives ([`DrawPrim`]) and [`TrayInput`] frame bundle.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use aterm_render::RenderInput;

use crate::type_scale::StepPx;

/// RGBA8, straight (non-premultiplied) alpha. `a < 255` is the frosted-card glass.
pub(crate) type Rgba = [u8; 4];

pub(crate) const fn rgba(c: [u8; 3], a: u8) -> Rgba {
    [c[0], c[1], c[2], a]
}

/// The weight of a chrome text run. `Bold` draws from the real bold sibling of the user's
/// terminal face when it covers the glyph (`tray_raster::select_chrome_face`), else the
/// regular face; never a synthetic dilation. Orthogonal to [`TextFace`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum TextWeight {
    Regular,
    Bold,
}

/// The face a [`DrawPrim::Text`] rasterizes in ([`crate::tray_raster`] picks the font).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum TextFace {
    Mono,
    Ui,
    UiBold,
    Display(u8),
    /// The setup screen lockup's words, set alike whatever terminal font the person chose: SF
    /// Mono's static Medium for [`TextWeight::Regular`] and its static Bold for
    /// [`TextWeight::Bold`], each a step heavier than the design's weights so this unsmoothed
    /// raster matches its smoothed render (the terminal face's bold sibling, which
    /// [`Self::Mono`] draws from, is missing for a variable face such as SF Mono, whose weights
    /// are instances of the one file), else the bundled Iosevka Term Regular and Bold where SF
    /// Mono's faces are missing.
    Brand(TextWeight),
}

/// One variable-font axis request: an OpenType `tag` and its value as `f32` bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SemanticVariation {
    pub(crate) tag: u32,
    pub(crate) value_bits: u32,
}

impl SemanticVariation {
    pub(crate) const fn new(tag: u32, value: f32) -> Self {
        Self {
            tag,
            value_bits: value.to_bits(),
        }
    }

    pub(crate) const fn value(self) -> f32 {
        f32::from_bits(self.value_bits)
    }
}

/// The exact, view-authored terminal face candidate a Settings specimen renders with. It
/// lives in the draw IR, not global state, so two Settings views never borrow each other's
/// uncommitted font when their frames rasterize out of order. Empty slots follow the
/// committed renderer's face.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SemanticFontCandidate {
    /// The font family: the person's own font, never a display face.
    pub(crate) regular: Option<String>,
    pub(crate) bold: Option<String>,
    pub(crate) italic: Option<String>,
    pub(crate) bold_italic: Option<String>,
    /// Ordered broad-script fallbacks. The parked semantic-font worker resolves and parses
    /// them; paint sees only the completed renderer.
    pub(crate) fallback: Vec<String>,
    pub(crate) symbol: Option<String>,
    pub(crate) emoji: Option<String>,
    /// Parsed, normalized variable-font requests, as float bits so the whole request is
    /// hashable across the worker generation handshake.
    pub(crate) variations: Vec<SemanticVariation>,
    pub(crate) synthetic_styles: bool,
    /// The display layer drawn over the font (`display_font`): registry ids
    /// in registry order, empty for none.
    pub(crate) display: Vec<&'static str>,
}

impl Default for SemanticFontCandidate {
    fn default() -> Self {
        Self {
            regular: None,
            bold: None,
            italic: None,
            bold_italic: None,
            fallback: Vec::new(),
            symbol: None,
            emoji: None,
            variations: Vec::new(),
            synthetic_styles: true,
            display: Vec::new(),
        }
    }
}

impl SemanticFontCandidate {
    pub(crate) const fn is_host(&self) -> bool {
        self.regular.is_none()
            && self.bold.is_none()
            && self.italic.is_none()
            && self.bold_italic.is_none()
            && self.fallback.is_empty()
            && self.symbol.is_none()
            && self.emoji.is_none()
            && self.variations.is_empty()
            && self.synthetic_styles
            && self.display.is_empty()
    }

    /// What the sample draws its letters in, as one family string: the
    /// display layer (`display:<ids>`) when one is on, else the font family.
    pub(crate) fn drawn_family(&self) -> Option<String> {
        if self.display.is_empty() {
            self.regular.clone()
        } else {
            Some(format!(
                "{}{}",
                aterm_render::DISPLAY_FACE_SCHEME,
                self.display.join("+")
            ))
        }
    }

    pub(crate) fn authored_slots(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("regular", self.regular.as_deref()),
            ("bold", self.bold.as_deref()),
            ("italic", self.italic.as_deref()),
            ("bold italic", self.bold_italic.as_deref()),
        ]
        .into_iter()
        .filter_map(|(slot, family)| family.map(|family| (slot, family)))
    }

    pub(crate) fn variation_requests(&self) -> Vec<(u32, f32)> {
        self.variations
            .iter()
            .map(|variation| (variation.tag, variation.value()))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum SpecimenTextBlending {
    Linear,
    #[default]
    LinearCorrected,
}

/// The wallpaper a terminal specimen draws under its default backgrounds, as
/// a tab draws it: the decoded image (`w × h` straight sRGB RGBA8, `fp` its
/// asset fingerprint), prepared at raster time for the specimen's exact frame
/// ([`crate::wallpaper_present::prepare`]), toned by `dim`, and, with `tint`,
/// its default-foreground text tinted toward the picture under each cell.
#[derive(Clone)]
pub(crate) struct SpecimenWallpaper {
    pub(crate) rgba: Arc<[u8]>,
    pub(crate) w: usize,
    pub(crate) h: usize,
    pub(crate) fp: u64,
    pub(crate) dim: f32,
    pub(crate) tint: bool,
}

impl SpecimenWallpaper {
    /// Everything the prepared pixels depend on but the frame: the asset, its
    /// size, the dim and the tint.
    pub(crate) const fn identity(&self) -> (u64, usize, usize, u32, bool) {
        (self.fp, self.w, self.h, self.dim.to_bits(), self.tint)
    }
}

impl PartialEq for SpecimenWallpaper {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl std::fmt::Debug for SpecimenWallpaper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpecimenWallpaper")
            .field("fp", &self.fp)
            .field("w", &self.w)
            .field("h", &self.h)
            .field("dim", &self.dim)
            .field("tint", &self.tint)
            .finish_non_exhaustive()
    }
}

/// Complete renderer state for one bounded, real terminal-frame specimen. `input` is an
/// immutable engine snapshot; every other field is reapplied to an isolated renderer fork
/// just before `try_render_input`, so an uncommitted Settings candidate neither mutates a
/// live renderer nor borrows another Settings view's state.
#[derive(Clone, Debug)]
pub(crate) struct TerminalSpecimenSpec {
    pub(crate) input: Arc<RenderInput>,
    pub(crate) input_fingerprint: u64,
    /// The cursor scene's moving parts, played onto `input` at raster time
    /// once the renderer's cell metrics are known; `None` for a still sample.
    pub(crate) motion: Option<Arc<crate::settings_preview::CursorScene>>,
    /// Host-prepared renderer source from `ViewCx`; raster forks this private snapshot and
    /// never consults process-global state.
    pub(crate) prepared_font: crate::tray_raster::PreparedSemanticFont,
    pub(crate) theme: aterm_render::Theme,
    pub(crate) font_px: f32,
    pub(crate) line_height: f32,
    pub(crate) baseline_adjust: i32,
    pub(crate) ligatures: bool,
    pub(crate) merged_ligatures: bool,
    pub(crate) cursor_break_ligatures: bool,
    pub(crate) synthetic_styles: bool,
    pub(crate) underline_position: i32,
    pub(crate) underline_thickness: i32,
    pub(crate) underline_skip_descenders: bool,
    pub(crate) text_blending: SpecimenTextBlending,
    pub(crate) font_thicken: bool,
    pub(crate) stem_gamma: f32,
    pub(crate) variations: Vec<SemanticVariation>,
    pub(crate) minimum_contrast: f32,
    pub(crate) selection_foreground: Option<u32>,
    pub(crate) selection_inactive: bool,
    /// The wallpaper under the specimen's default backgrounds, as a tab draws
    /// it; `None` draws the theme background.
    pub(crate) wallpaper: Option<SpecimenWallpaper>,
}

impl TerminalSpecimenSpec {
    /// Feed the specimen's identity into `state`: the frame by its
    /// `input_fingerprint` (the input's identity, as the raster's frame cache
    /// keys it), the motion by its scene's fingerprint, the font by its
    /// candidate, its snapshot and whether its renderer is ready (what its
    /// `PartialEq` compares), the wallpaper by its identity, and every other
    /// setting by value. The pattern names every field, so a new one does not
    /// compile until it is placed here.
    fn fingerprint_into(&self, state: &mut impl Hasher) {
        let Self {
            input: _,
            input_fingerprint,
            motion,
            prepared_font,
            theme,
            font_px,
            line_height,
            baseline_adjust,
            ligatures,
            merged_ligatures,
            cursor_break_ligatures,
            synthetic_styles,
            underline_position,
            underline_thickness,
            underline_skip_descenders,
            text_blending,
            font_thicken,
            stem_gamma,
            variations,
            minimum_contrast,
            selection_foreground,
            selection_inactive,
            wallpaper,
        } = self;
        input_fingerprint.hash(state);
        motion.as_ref().map(|scene| scene.fingerprint).hash(state);
        let snapshot = &prepared_font.snapshot;
        (
            &prepared_font.candidate,
            &snapshot.status,
            snapshot.ready_epoch,
            snapshot.pending,
            prepared_font.is_ready(),
        )
            .hash(state);
        theme.hash(state);
        f32_bits(
            state,
            &[*font_px, *line_height, *stem_gamma, *minimum_contrast],
        );
        (baseline_adjust, underline_position, underline_thickness).hash(state);
        (
            ligatures,
            merged_ligatures,
            cursor_break_ligatures,
            synthetic_styles,
            underline_skip_descenders,
            font_thicken,
            selection_inactive,
        )
            .hash(state);
        (text_blending, variations, selection_foreground).hash(state);
        wallpaper
            .as_ref()
            .map(SpecimenWallpaper::identity)
            .hash(state);
    }
}

/// The only constructor of [`DrawPrim::Text`]. Its size is a [`StepPx`],
/// mintable only by [`TypeStep`](crate::type_scale::TypeStep), so every chrome
/// text site maps to a named step of the type scale.
pub(crate) fn text_prim(
    x: f32,
    baseline: f32,
    s: String,
    size: StepPx,
    weight: TextWeight,
    face: TextFace,
    color: Rgba,
) -> DrawPrim {
    DrawPrim::Text {
        x,
        baseline,
        s,
        px: size.get(),
        color,
        weight,
        face,
    }
}

/// One renderer-agnostic primitive, in frame pixel coordinates (origin top-left).
#[derive(Clone, Debug)]
pub(crate) enum DrawPrim {
    /// The card: a flat translucent `fill` (the whole prim list is rasterized to
    /// one RGBA overlay by `tray_raster`).
    Panel {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        fill: Rgba,
    },
    /// A ring: a faint full-circle `track` and a bold arc filling `frac` of
    /// the turn in `fill`, clockwise from `start` turns past 12 o'clock (0 is
    /// 12 o'clock, 0.25 is 3 o'clock), so an arc can turn in place.
    Ring {
        cx: f32,
        cy: f32,
        r_outer: f32,
        thickness: f32,
        track: Rgba,
        start: f32,
        frac: f32,
        fill: Rgba,
    },
    /// A horizontal rounded bar: `frac` of `w` filled in `fill` over a faint
    /// `track`. It draws the Settings switch and slider tracks.
    Capsule {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        frac: f32,
        fill: Rgba,
        track: Rgba,
    },
    /// A filled disc: a switch thumb, a status or badge dot, a tab icon's dot.
    Dot {
        cx: f32,
        cy: f32,
        r: f32,
        color: Rgba,
    },
    /// Free-positioned text, `px` tall, positioned by baseline; `Mono` figures are
    /// tabular. Construct only via [`text_prim`].
    Text {
        x: f32,
        /// The baseline y (frame px). Row-centred sites derive it from the
        /// cap-height centering rule (`tray_raster::row_baseline`); mixed-size
        /// runs on one visual row share a single baseline.
        baseline: f32,
        s: String,
        px: f32,
        color: Rgba,
        weight: TextWeight,
        face: TextFace,
    },
    /// A complete tiny terminal frame rendered by the shipping CPU renderer from one exact
    /// `RenderInput` (backgrounds, attributes, selection, shaping, CJK, emoji and all).
    TerminalSpecimen {
        x: f32,
        y: f32,
        spec: Box<TerminalSpecimenSpec>,
    },
    /// An outlined rounded rect: a stroke of `width` centered on the rect edge (focus
    /// rings, borders, separators, drawn icons, the text caret). Thin (`width <= 1`)
    /// axis-aligned strokes snap to device pixels; `radius > 0` uses the analytic SDF; a
    /// `radius == 0` degenerate (`w` or `h` ~= `width`) stroke is a snapped line.
    Stroke {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        width: f32,
        color: Rgba,
    },
    /// An anti-aliased round-capped segment (native pictograms).
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        width: f32,
        color: Rgba,
    },
    /// An anti-aliased band along the circle of radius `r` about (`cx`, `cy`), `width` across,
    /// from `from` to `to` turns clockwise from 12 o'clock (a span of `to − from`, at most a
    /// turn), each end a half-disc `width` across (as a [`DrawPrim::Line`]'s cap). It covers
    /// each pixel once, so a translucent arc has no seam where a chain of round-capped lines
    /// would lay its joints over each other (the stats dial's glow).
    Arc {
        cx: f32,
        cy: f32,
        r: f32,
        width: f32,
        from: f32,
        to: f32,
        color: Rgba,
    },
    /// A dashed stroke `width` thick, its dashes `on` long and `off` apart:
    /// the rect's outline, inside the rect as an unrounded
    /// [`DrawPrim::Stroke`] draws it, with every corner solid. A rect too
    /// thin to have an inside (at most two `width`s across) is one band
    /// along its longer side, at least `width` thick: a line from `(x, y)`
    /// when `w` or `h` is 0, the stroke right of or below it. A negative `w`
    /// or `h` spans the rect on the other side of `(x, y)`. Each edge or
    /// band starts with a dash at its left or top end. Every length snaps to
    /// whole device pixels, a dash or gap to at least one, and each pixel is
    /// inked once; `on <= 0` draws nothing and `off <= 0` a solid stroke. The Deck draws a
    /// stalled live edge's line with it.
    Dashed {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        width: f32,
        on: f32,
        off: f32,
        color: Rgba,
    },
    /// A filled HSV colour disk (the settings colour wheel): per-pixel polar hue (0° at 12
    /// o'clock, clockwise, as [`DrawPrim::Ring`] arcs) and saturation (radius), scaled by
    /// `value`, with a ~1px anti-aliased rim.
    HsvDisk {
        cx: f32,
        cy: f32,
        r: f32,
        value: f32,
    },
    /// Push a rectangular clip (frame px): every later prim is clipped to the
    /// intersection of all pushed rects until the matching [`DrawPrim::ClipPop`].
    ClipPush { x: f32, y: f32, w: f32, h: f32 },
    /// Pop the most recent [`DrawPrim::ClipPush`]. Unbalanced pops are ignored.
    ClipPop,
    /// Fade every later prim until the matching [`DrawPrim::FadePop`] over the
    /// band `y..y + h` (frame px): what it draws above `y` is untouched, and
    /// from `y` down its opacity falls, eased out, from `from` to `to`, `to`
    /// past the band — a scrolling list's peek in the strip under its room
    /// ([`crate::native_ui::UiNode::room`]).
    FadePush { y: f32, h: f32, from: f32, to: f32 },
    /// Pop the most recent [`DrawPrim::FadePush`]. Unbalanced pops are ignored.
    FadePop,
    /// Keep every later prim at its own sub-pixel place until the matching
    /// [`DrawPrim::SubpixelPop`]: elsewhere the raster puts a panel's and a rounded
    /// stroke's edges on the device grid, so a rim is whole pixels wherever layout put it;
    /// marks that glide by fractions of a pixel (the Deck's strips) keep their motion.
    SubpixelPush,
    /// Pop the most recent [`DrawPrim::SubpixelPush`]. Unbalanced pops are ignored.
    SubpixelPop,
    /// One of the message band's drawn icons in a `size`-point square at `(x, y)`,
    /// rasterized at device scale (Settings ▸ Messages' severity column).
    BandIcon {
        x: f32,
        y: f32,
        size: f32,
        icon: aterm_render::BandIcon,
        color: Rgba,
    },
    /// A bitmap in the `w`×`h` rect at `(x, y)`: `art`'s bake at `device` pixels, drawn one to
    /// one from the rect's top left (never resampled; where the canvas's scale is the one it
    /// was baked for it fills the rect exactly), composited in sRGB as its art was drawn, at
    /// `alpha` (255 opaque, 0 nothing). Only a bake already kept draws
    /// ([`ImageArt::kept`]): the raster never bakes.
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        art: ImageArt,
        device: (u32, u32),
        alpha: u8,
    },
}

impl DrawPrim {
    /// Feed what decides this prim's pixels into `state`, so equal prims hash
    /// equally and a prim moved by one ulp, inked another colour or saying
    /// another word hashes apart: the variant, each `f32` by its bits, text
    /// by its bytes, colours by their channels. Each arm names every field,
    /// so a new variant or field does not compile until it is placed here.
    pub(crate) fn fingerprint_into(&self, state: &mut impl Hasher) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Panel {
                x,
                y,
                w,
                h,
                radius,
                fill,
            } => {
                f32_bits(state, &[*x, *y, *w, *h, *radius]);
                fill.hash(state);
            }
            Self::Ring {
                cx,
                cy,
                r_outer,
                thickness,
                track,
                start,
                frac,
                fill,
            } => {
                f32_bits(state, &[*cx, *cy, *r_outer, *thickness, *start, *frac]);
                (track, fill).hash(state);
            }
            Self::Capsule {
                x,
                y,
                w,
                h,
                frac,
                fill,
                track,
            } => {
                f32_bits(state, &[*x, *y, *w, *h, *frac]);
                (fill, track).hash(state);
            }
            Self::Dot { cx, cy, r, color } => {
                f32_bits(state, &[*cx, *cy, *r]);
                color.hash(state);
            }
            Self::Text {
                x,
                baseline,
                s,
                px,
                color,
                weight,
                face,
            } => {
                f32_bits(state, &[*x, *baseline, *px]);
                (s, color, weight, face).hash(state);
            }
            Self::TerminalSpecimen { x, y, spec } => {
                f32_bits(state, &[*x, *y]);
                spec.fingerprint_into(state);
            }
            Self::Stroke {
                x,
                y,
                w,
                h,
                radius,
                width,
                color,
            } => {
                f32_bits(state, &[*x, *y, *w, *h, *radius, *width]);
                color.hash(state);
            }
            Self::Line {
                x1,
                y1,
                x2,
                y2,
                width,
                color,
            } => {
                f32_bits(state, &[*x1, *y1, *x2, *y2, *width]);
                color.hash(state);
            }
            Self::Arc {
                cx,
                cy,
                r,
                width,
                from,
                to,
                color,
            } => {
                f32_bits(state, &[*cx, *cy, *r, *width, *from, *to]);
                color.hash(state);
            }
            Self::Dashed {
                x,
                y,
                w,
                h,
                width,
                on,
                off,
                color,
            } => {
                f32_bits(state, &[*x, *y, *w, *h, *width, *on, *off]);
                color.hash(state);
            }
            Self::HsvDisk { cx, cy, r, value } => f32_bits(state, &[*cx, *cy, *r, *value]),
            Self::ClipPush { x, y, w, h } => f32_bits(state, &[*x, *y, *w, *h]),
            Self::FadePush { y, h, from, to } => f32_bits(state, &[*y, *h, *from, *to]),
            Self::ClipPop | Self::FadePop | Self::SubpixelPush | Self::SubpixelPop => {}
            Self::BandIcon {
                x,
                y,
                size,
                icon,
                color,
            } => {
                f32_bits(state, &[*x, *y, *size]);
                (icon, color).hash(state);
            }
            Self::Image {
                x,
                y,
                w,
                h,
                art,
                device,
                alpha,
            } => {
                f32_bits(state, &[*x, *y, *w, *h]);
                (art, device, alpha).hash(state);
            }
        }
    }
}

/// Hash each value by its bits, so values one ulp apart hash apart.
fn f32_bits(state: &mut impl Hasher, values: &[f32]) {
    for v in values {
        state.write_u32(v.to_bits());
    }
}

/// What a [`DrawPrim::Image`] shows. Each is baked from vector art at the
/// device size it is drawn at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ImageArt {
    /// The setup screen's rainbow road at a moment of the run
    /// (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4.5): its box is the road's length of the
    /// design and its whole height.
    Road(aterm_effects::rainbow_road::RoadArt),
    /// The cat standing on the setup screen's road, in a pose, at its lean.
    Cat(aterm_effects::rainbow_road::CatArt),
}

impl ImageArt {
    /// The art's authored width ÷ height: a caller sizes its rect from it, so
    /// the bake is never stretched.
    pub(crate) fn aspect(self) -> f32 {
        match self {
            Self::Road(road) => road.aspect(),
            Self::Cat(_) => aterm_effects::rainbow_road::CatArt::ASPECT,
        }
    }

    /// Straight-alpha RGBA8 rows of the art at exactly `w`×`h` device pixels.
    pub(crate) fn bake(self, w: u32, h: u32) -> Vec<u8> {
        match self {
            Self::Road(road) => road.bake(w, h),
            Self::Cat(cat) => cat.bake(w, h),
        }
        .pixels()
        .to_vec()
    }

    /// The art's bake at exactly `w`×`h` device pixels, if one is kept. Nothing bakes here: the
    /// raster draws only what is kept, and the setup screen bakes what it will draw ahead, on
    /// its own worker (`system_state::prebake`).
    pub(crate) fn kept(self, w: u32, h: u32) -> Option<std::sync::Arc<[u8]>> {
        let key = (self, w, h);
        with_baked_images(|kept| {
            let at = kept.images.iter().position(|(baked, _)| *baked == key)?;
            // The newest last: a bake drawn is the last evicted.
            let image = kept.images.remove(at);
            let pixels = std::sync::Arc::clone(&image.1);
            kept.images.push(image);
            Some(pixels)
        })
    }

    /// Whether a bake of the art at `w`×`h` device pixels is kept now.
    pub(crate) fn is_kept(self, w: u32, h: u32) -> bool {
        with_baked_images(|kept| kept.images.iter().any(|(baked, _)| *baked == (self, w, h)))
    }

    /// Keep `pixels`, the art baked at `w`×`h` device pixels, evicting the bakes drawn least
    /// recently while the kept ones' bytes pass [`IMAGES_BUDGET`] (never this one).
    pub(crate) fn keep(self, w: u32, h: u32, pixels: std::sync::Arc<[u8]>) {
        let key = (self, w, h);
        with_baked_images(|kept| {
            kept.images.retain(|(baked, _)| *baked != key);
            kept.images.push((key, pixels));
            let mut bytes: usize = kept.images.iter().map(|(_, pixels)| pixels.len()).sum();
            while bytes > IMAGES_BUDGET && kept.images.len() > 1 {
                let (_, evicted) = kept.images.remove(0);
                bytes -= evicted.len();
            }
        });
    }

    /// Where the art draws, baked at [`ArtMask::HEIGHT`] pixels tall: its visible edges,
    /// whatever size it is drawn at. The last [`MASKS_KEPT`] arts' masks are kept.
    pub(crate) fn mask(self) -> std::sync::Arc<ArtMask> {
        if let Some(mask) = with_baked_images(|kept| {
            kept.masks
                .iter()
                .find(|(art, _)| *art == self)
                .map(|(_, mask)| std::sync::Arc::clone(mask))
        }) {
            return mask;
        }
        let h = ArtMask::HEIGHT;
        let w = (h as f32 * self.aspect()).round().max(1.0) as u32;
        let mask = std::sync::Arc::new(ArtMask::new(
            w,
            h,
            self.bake(w, h)
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[3]),
        ));
        with_baked_images(|kept| {
            kept.masks.retain(|(art, _)| *art != self);
            if kept.masks.len() >= MASKS_KEPT {
                kept.masks.remove(0);
            }
            kept.masks.push((self, std::sync::Arc::clone(&mask)));
        });
        mask
    }
}

/// How many bytes of baked [`ImageArt`] bitmaps may be kept at most. What is kept is what the
/// live System State views want and last drew (`system_state::prebake` drops the rest as each
/// drawing pass records what it drew, and all but each view's last drawn moment once no view is
/// on screen): a view keeps at most the moment it drew and the one it wants, each two images,
/// the road and the cat. The longest road ([`aterm_effects::rainbow_road::MAX_LENGTH`] at Mono 22
/// on a 2x display) is 7.8 MB and its cat 0.5 MB, so a view at its widest keeps 17 MB on a 2x
/// display, less across a move to a display of another scale (one moment at each); a page at an
/// ordinary width keeps a few MB. This cap is only a backstop for several wide views at once:
/// past it the bake drawn least recently goes first.
pub(crate) const IMAGES_BUDGET: usize = 32 << 20;

/// How many arts' ink masks are kept: an ink audit of the page reads the images it draws, two a
/// moment over the two moments a page holds. Each is a few hundred KB at most.
const MASKS_KEPT: usize = 4;

/// A bake kept: the art and its device width and height, and its pixels.
type BakedImage = ((ImageArt, u32, u32), std::sync::Arc<[u8]>);

/// The bakes kept, newest (or most recently drawn) last, and the arts' ink masks.
#[derive(Default)]
struct Kept {
    images: Vec<BakedImage>,
    masks: Vec<(ImageArt, std::sync::Arc<ArtMask>)>,
}

/// The bakes, for the process: the setup screen's bake worker fills them and every raster
/// thread reads them.
#[cfg(not(test))]
static BAKED_IMAGES: std::sync::Mutex<Kept> = std::sync::Mutex::new(Kept {
    images: Vec::new(),
    masks: Vec::new(),
});

#[cfg(test)]
thread_local! {
    /// A test thread's own bakes, so tests running side by side never evict each other's.
    static BAKED_IMAGES: std::sync::Mutex<Kept> = std::sync::Mutex::new(Kept::default());
}

/// Run `f` on the bakes kept.
fn with_baked_images<T>(f: impl FnOnce(&mut Kept) -> T) -> T {
    #[cfg(not(test))]
    {
        f(&mut BAKED_IMAGES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
    #[cfg(test)]
    {
        BAKED_IMAGES.with(|kept| {
            f(&mut kept
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner))
        })
    }
}

/// Keep only the bakes of `wanted` (each its art and device size), and the ink masks of their
/// arts: the rest no live System State view wants or drew. Empty, it drops every bake and mask.
pub(crate) fn retain_images(wanted: &[(ImageArt, u32, u32)]) {
    with_baked_images(|kept| {
        kept.images.retain(|(key, _)| wanted.contains(key));
        kept.masks
            .retain(|(art, _)| wanted.iter().any(|(wanted, _, _)| wanted == art));
    });
}

/// The bytes of bakes kept now (a test's read).
#[cfg(test)]
pub(crate) fn images_kept_bytes() -> usize {
    with_baked_images(|kept| kept.images.iter().map(|(_, pixels)| pixels.len()).sum())
}

/// Which pixels of an [`ImageArt`] are drawn, at a reference size: for each row, how many
/// drawn pixels lie before each column (`w + 1` counts a row), so [`ArtMask::drawn`] finds a
/// row's first and last drawn column in a span by two binary searches.
pub(crate) struct ArtMask {
    w: u32,
    h: u32,
    drawn_before: Vec<u16>,
}

/// Where in an art's box its drawn pixels lie, as shares of the box (0 at its
/// left or top edge, 1 at its right or bottom edge).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ArtSpan {
    pub(crate) left: f32,
    pub(crate) top: f32,
    pub(crate) right: f32,
    pub(crate) bottom: f32,
}

#[cfg(test)]
impl ArtSpan {
    /// The whole box.
    pub(crate) const BOX: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };
}

impl ArtMask {
    /// The reference bake's height: 1/128 of the art is finer than a device
    /// pixel of any brand mark drawn.
    const HEIGHT: u32 = 128;
    /// A pixel at least this opaque is drawn; the anti-aliased fringe below
    /// it is no edge anyone sees.
    const DRAWN: u8 = 0x40;

    /// The mask of a `w × h` art whose pixels' alphas `alpha` yields row by row.
    fn new(w: u32, h: u32, alpha: impl Iterator<Item = u8>) -> Self {
        assert!(w < u32::from(u16::MAX), "an art mask row counts in u16");
        let mut drawn_before = Vec::with_capacity(((w + 1) * h) as usize);
        let mut alpha = alpha;
        for _ in 0..h {
            let mut count = 0_u16;
            drawn_before.push(count);
            for a in alpha.by_ref().take(w as usize) {
                count += u16::from(a >= Self::DRAWN);
                drawn_before.push(count);
            }
        }
        Self { w, h, drawn_before }
    }

    /// What the art draws within `part` of its box: the span of its drawn
    /// pixels there, or `None` where it draws nothing.
    pub(crate) fn drawn(&self, part: ArtSpan) -> Option<ArtSpan> {
        let (w, h) = (self.w as f32, self.h as f32);
        let cols = (part.left * w).floor().max(0.0) as u32..(part.right * w).ceil().min(w) as u32;
        let rows = (part.top * h).floor().max(0.0) as u32..(part.bottom * h).ceil().min(h) as u32;
        let (c0, c1) = (cols.start as usize, cols.end as usize);
        let mut seen: Option<(u32, u32, u32, u32)> = None;
        if c0 < c1 {
            let stride = self.w as usize + 1;
            for row in rows {
                let before = &self.drawn_before[row as usize * stride..][..stride];
                let (at_start, at_end) = (before[c0], before[c1]);
                if at_start == at_end {
                    continue;
                }
                // The first column whose pixel is drawn moves the count past `at_start`; the
                // last is the last column the count is still below `at_end` at.
                let first = c0 + before[c0 + 1..=c1].partition_point(|&n| n == at_start);
                let last = c0 + before[c0..c1].partition_point(|&n| n < at_end) - 1;
                let (first, last) = (first as u32, last as u32);
                seen = Some(seen.map_or((first, row, last, row), |(l, t, r, b)| {
                    (l.min(first), t.min(row), r.max(last), b.max(row))
                }));
            }
        }
        seen.map(|(l, t, r, b)| ArtSpan {
            left: (l as f32 / w).max(part.left),
            top: (t as f32 / h).max(part.top),
            right: ((r + 1) as f32 / w).min(part.right),
            bottom: ((b + 1) as f32 / h).min(part.bottom),
        })
    }
}

/// Scale every length of `prims` by `k` about the origin: a painter that lays in device pixels
/// (the deck, [`crate::stats_deck`]) hands its prims to a tray that lays in logical points and
/// multiplies by the window's scale itself. A terminal specimen is moved, never scaled: it sizes
/// itself from its own spec, and the deck never draws one.
pub(crate) fn scale_prims(prims: &mut [DrawPrim], k: f32) {
    for p in prims {
        match p {
            DrawPrim::Panel {
                x, y, w, h, radius, ..
            } => {
                for v in [x, y, w, h, radius] {
                    *v *= k;
                }
            }
            DrawPrim::Ring {
                cx,
                cy,
                r_outer,
                thickness,
                ..
            } => {
                for v in [cx, cy, r_outer, thickness] {
                    *v *= k;
                }
            }
            DrawPrim::Capsule { x, y, w, h, .. } | DrawPrim::ClipPush { x, y, w, h } => {
                for v in [x, y, w, h] {
                    *v *= k;
                }
            }
            DrawPrim::Image { x, y, w, h, .. } => {
                for v in [x, y, w, h] {
                    *v *= k;
                }
            }
            DrawPrim::Dot { cx, cy, r, .. } | DrawPrim::HsvDisk { cx, cy, r, .. } => {
                for v in [cx, cy, r] {
                    *v *= k;
                }
            }
            DrawPrim::Text {
                x, baseline, px, ..
            } => {
                for v in [x, baseline, px] {
                    *v *= k;
                }
            }
            DrawPrim::TerminalSpecimen { x, y, .. } => {
                *x *= k;
                *y *= k;
            }
            DrawPrim::Stroke {
                x,
                y,
                w,
                h,
                radius,
                width,
                ..
            } => {
                for v in [x, y, w, h, radius, width] {
                    *v *= k;
                }
            }
            DrawPrim::Line {
                x1,
                y1,
                x2,
                y2,
                width,
                ..
            } => {
                for v in [x1, y1, x2, y2, width] {
                    *v *= k;
                }
            }
            DrawPrim::Arc {
                cx, cy, r, width, ..
            } => {
                for v in [cx, cy, r, width] {
                    *v *= k;
                }
            }
            DrawPrim::Dashed {
                x,
                y,
                w,
                h,
                width,
                on,
                off,
                ..
            } => {
                for v in [x, y, w, h, width, on, off] {
                    *v *= k;
                }
            }
            DrawPrim::FadePush { y, h, .. } => {
                *y *= k;
                *h *= k;
            }
            DrawPrim::BandIcon { x, y, size, .. } => {
                for v in [x, y, size] {
                    *v *= k;
                }
            }
            DrawPrim::ClipPop
            | DrawPrim::FadePop
            | DrawPrim::SubpixelPush
            | DrawPrim::SubpixelPop => {}
        }
    }
}

/// Translate every prim by `(dx, dy)` frame px, e.g. to rasterize a floating card into a
/// buffer cropped to its paint bounds.
pub(crate) fn translate_prims(prims: &mut [DrawPrim], dx: f32, dy: f32) {
    for p in prims {
        match p {
            DrawPrim::Panel { x, y, .. }
            | DrawPrim::Capsule { x, y, .. }
            | DrawPrim::Stroke { x, y, .. }
            | DrawPrim::Dashed { x, y, .. }
            | DrawPrim::ClipPush { x, y, .. }
            | DrawPrim::BandIcon { x, y, .. }
            | DrawPrim::Image { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            DrawPrim::Line { x1, y1, x2, y2, .. } => {
                *x1 += dx;
                *y1 += dy;
                *x2 += dx;
                *y2 += dy;
            }
            DrawPrim::Text { x, baseline, .. } => {
                *x += dx;
                *baseline += dy;
            }
            DrawPrim::TerminalSpecimen { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            DrawPrim::Ring { cx, cy, .. }
            | DrawPrim::Arc { cx, cy, .. }
            | DrawPrim::Dot { cx, cy, .. }
            | DrawPrim::HsvDisk { cx, cy, .. } => {
                *cx += dx;
                *cy += dy;
            }
            DrawPrim::FadePush { y, .. } => *y += dy,
            DrawPrim::ClipPop
            | DrawPrim::FadePop
            | DrawPrim::SubpixelPush
            | DrawPrim::SubpixelPop => {}
        }
    }
}

/// RGB → HSV (`h` in 0..1 turns, `s`/`v` in 0..1), the inverse of [`hsv_to_rgb`]; places
/// the Tab Color wheel's marker on the disk the raster paints.
pub(crate) fn rgb_to_hsv(rgb: [u8; 3]) -> (f32, f32, f32) {
    let [r, g, b] = rgb.map(|c| f32::from(c) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let h = if delta <= f32::EPSILON {
        0.0
    } else if (max - r).abs() <= f32::EPSILON {
        ((g - b) / delta).rem_euclid(6.0) / 6.0
    } else if (max - g).abs() <= f32::EPSILON {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };
    let s = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    (h, s, max)
}

/// HSV → RGB (`h` in 0..1 turns, `s`/`v` in 0..1), shared by the colour wheel's
/// commit math and the [`DrawPrim::HsvDisk`] raster so the two never disagree.
pub(crate) fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let f = h.fract();
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match h as u32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    ]
}

/// The whole open tray for one frame: the card rect and every enabled card's positioned
/// prims.
#[derive(Clone, Debug, Default)]
pub(crate) struct TrayInput {
    pub prims: Vec<DrawPrim>,
    /// The card rect `(x, y, w, h)`.
    pub card: (f32, f32, f32, f32),
}

#[cfg(test)]
#[path = "widget_tests.rs"]
mod tests;
