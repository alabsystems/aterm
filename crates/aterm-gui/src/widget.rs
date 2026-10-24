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
    /// the turn (clockwise from 12 o'clock) in `fill`.
    Ring {
        cx: f32,
        cy: f32,
        r_outer: f32,
        thickness: f32,
        track: Rgba,
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
    /// A dashed stroke `width` thick, its dashes `on` long and `off` apart:
    /// the rect's outline, inside the rect as an unrounded
    /// [`DrawPrim::Stroke`] draws it, with every corner solid. A rect too
    /// thin to have an inside (at most two `width`s across) is one band
    /// along its longer side, at least `width` thick: a line from `(x, y)`
    /// when `w` or `h` is 0, the stroke right of or below it. A negative `w`
    /// or `h` spans the rect on the other side of `(x, y)`. Each edge or
    /// band starts with a dash at its left or top end. Every length snaps to
    /// whole device pixels, a dash or gap to at least one, and each pixel is
    /// inked once; `on <= 0` draws nothing and `off <= 0` a solid stroke.
    /// Walls, hollow buckets, the free-space outline, projections, missing
    /// days.
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
    /// One of the message band's drawn icons in a `size`-point square at `(x, y)`,
    /// rasterized at device scale (Settings ▸ Messages' severity column).
    BandIcon {
        x: f32,
        y: f32,
        size: f32,
        icon: aterm_render::BandIcon,
        color: Rgba,
    },
    /// A bitmap in the `w`×`h` rect at `(x, y)`, baked from `art` at the rect's exact device
    /// size (cached per size), so it is never resampled.
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        art: ImageArt,
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
                frac,
                fill,
            } => {
                f32_bits(state, &[*cx, *cy, *r_outer, *thickness, *frac]);
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
            Self::ClipPop | Self::FadePop => {}
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
            Self::Image { x, y, w, h, art } => {
                f32_bits(state, &[*x, *y, *w, *h]);
                art.hash(state);
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
    /// The aterm · ALab rainbow kitty: the pet roster's tabby in the `PetStretchHind` pose of
    /// `assets/aterm-rainbow-kitty.png`, in the brand coat, its keyline pale on a dark ground.
    BrandKitty { dark: bool },
}

impl ImageArt {
    const BRAND_POSE: aterm_effects::pet_glyphs_gen::PetGlyphId =
        aterm_effects::pet_glyphs_gen::PetGlyphId::PetStretchHind;

    /// The art's authored width ÷ height: a caller sizes its rect from it, so
    /// the bake is never stretched.
    pub(crate) fn aspect(self) -> f32 {
        match self {
            Self::BrandKitty { .. } => aterm_effects::pet_baker::PetBaker::aspect(Self::BRAND_POSE),
        }
    }

    /// Straight-alpha RGBA8 rows of the art at exactly `w`×`h` device pixels.
    pub(crate) fn bake(self, w: u32, h: u32) -> Vec<u8> {
        match self {
            Self::BrandKitty { dark } => aterm_effects::pet_baker::bake_pose(
                Self::BRAND_POSE,
                &aterm_effects::cat_baker::ResolvedFills::from_indices(13, 4, dark),
                w,
                h,
            )
            .pixels()
            .to_vec(),
        }
    }

    /// Where the art draws, baked once per process at [`ArtMask::HEIGHT`]
    /// pixels tall: its visible edges, whatever size it is drawn at.
    pub(crate) fn mask(self) -> &'static ArtMask {
        static MASKS: [std::sync::OnceLock<ArtMask>; 2] = [
            const { std::sync::OnceLock::new() },
            const { std::sync::OnceLock::new() },
        ];
        let slot = match self {
            Self::BrandKitty { dark } => usize::from(dark),
        };
        MASKS[slot].get_or_init(|| {
            let h = ArtMask::HEIGHT;
            let w = (h as f32 * self.aspect()).round().max(1.0) as u32;
            ArtMask {
                w,
                h,
                alpha: self
                    .bake(w, h)
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| pixel[3])
                    .collect(),
            }
        })
    }
}

/// Which pixels of an [`ImageArt`] are drawn, at a reference size.
pub(crate) struct ArtMask {
    w: u32,
    h: u32,
    alpha: Vec<u8>,
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

    /// What the art draws within `part` of its box: the span of its drawn
    /// pixels there, or `None` where it draws nothing.
    pub(crate) fn drawn(&self, part: ArtSpan) -> Option<ArtSpan> {
        let (w, h) = (self.w as f32, self.h as f32);
        let cols = (part.left * w).floor().max(0.0) as u32..(part.right * w).ceil().min(w) as u32;
        let rows = (part.top * h).floor().max(0.0) as u32..(part.bottom * h).ceil().min(h) as u32;
        let mut seen: Option<(u32, u32, u32, u32)> = None;
        for row in rows {
            for col in cols.clone() {
                if self.alpha[(row * self.w + col) as usize] < Self::DRAWN {
                    continue;
                }
                seen = Some(seen.map_or((col, row, col, row), |(l, t, r, b)| {
                    (l.min(col), t.min(row), r.max(col), b.max(row))
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
            | DrawPrim::Dot { cx, cy, .. }
            | DrawPrim::HsvDisk { cx, cy, .. } => {
                *cx += dx;
                *cy += dy;
            }
            DrawPrim::FadePush { y, .. } => *y += dy,
            DrawPrim::ClipPop | DrawPrim::FadePop => {}
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
mod tests {
    use std::hash::Hasher;

    use super::*;

    fn fp(p: &DrawPrim) -> u64 {
        let mut state = std::collections::hash_map::DefaultHasher::new();
        p.fingerprint_into(&mut state);
        state.finish()
    }

    fn spec() -> TerminalSpecimenSpec {
        TerminalSpecimenSpec {
            input: Arc::new(RenderInput::default()),
            input_fingerprint: 1,
            motion: None,
            prepared_font: crate::tray_raster::PreparedSemanticFont::unavailable(
                SemanticFontCandidate::default(),
            ),
            theme: aterm_render::Theme::default(),
            font_px: 14.0,
            line_height: 1.0,
            baseline_adjust: 0,
            ligatures: true,
            merged_ligatures: false,
            cursor_break_ligatures: true,
            synthetic_styles: true,
            underline_position: 0,
            underline_thickness: 0,
            underline_skip_descenders: true,
            text_blending: SpecimenTextBlending::default(),
            font_thicken: false,
            stem_gamma: 1.0,
            variations: Vec::new(),
            minimum_contrast: 1.0,
            selection_foreground: None,
            selection_inactive: false,
            wallpaper: None,
        }
    }

    /// One prim of every variant (`nudged` names each, so a new variant
    /// does not compile until it has a row there and a sample here).
    fn samples() -> Vec<DrawPrim> {
        let ink = [10, 20, 30, 200];
        let other = [40, 50, 60, 255];
        vec![
            DrawPrim::Panel {
                x: 1.5,
                y: 2.5,
                w: 30.0,
                h: 12.0,
                radius: 3.0,
                fill: ink,
            },
            DrawPrim::Ring {
                cx: 8.0,
                cy: 8.0,
                r_outer: 6.0,
                thickness: 1.5,
                track: ink,
                frac: 0.5,
                fill: other,
            },
            DrawPrim::Capsule {
                x: 1.0,
                y: 2.0,
                w: 40.0,
                h: 6.0,
                frac: 0.25,
                fill: ink,
                track: other,
            },
            DrawPrim::Dot {
                cx: 3.0,
                cy: 4.0,
                r: 2.0,
                color: ink,
            },
            DrawPrim::Text {
                x: 4.0,
                baseline: 11.0,
                s: "CPU 42%".into(),
                px: 11.0,
                color: ink,
                weight: TextWeight::Regular,
                face: TextFace::Ui,
            },
            DrawPrim::TerminalSpecimen {
                x: 2.0,
                y: 3.0,
                spec: Box::new(spec()),
            },
            DrawPrim::Stroke {
                x: 1.0,
                y: 1.0,
                w: 20.0,
                h: 10.0,
                radius: 2.0,
                width: 1.0,
                color: ink,
            },
            DrawPrim::Line {
                x1: 1.0,
                y1: 2.0,
                x2: 9.0,
                y2: 7.0,
                width: 1.5,
                color: ink,
            },
            DrawPrim::Dashed {
                x: 1.0,
                y: 2.0,
                w: 20.0,
                h: 10.0,
                width: 1.0,
                on: 1.5,
                off: 1.0,
                color: ink,
            },
            DrawPrim::HsvDisk {
                cx: 20.0,
                cy: 20.0,
                r: 16.0,
                value: 0.8,
            },
            DrawPrim::ClipPush {
                x: 0.5,
                y: 0.5,
                w: 50.0,
                h: 30.0,
            },
            DrawPrim::ClipPop,
            DrawPrim::FadePush {
                y: 10.0,
                h: 4.0,
                from: 1.0,
                to: 0.25,
            },
            DrawPrim::FadePop,
            DrawPrim::BandIcon {
                x: 2.0,
                y: 3.0,
                size: 9.0,
                icon: aterm_render::BandIcon::Info,
                color: ink,
            },
            DrawPrim::Image {
                x: 2.0,
                y: 3.0,
                w: 24.0,
                h: 12.0,
                art: ImageArt::BrandKitty { dark: true },
            },
        ]
    }

    /// The least changes a field's value can take.
    trait Nudge: Sized {
        fn nudges(&self) -> Vec<Self>;
    }

    impl Nudge for f32 {
        /// One ulp up.
        fn nudges(&self) -> Vec<Self> {
            vec![Self::from_bits(self.to_bits() + 1)]
        }
    }

    impl Nudge for Rgba {
        /// Each channel's low bit, alpha included.
        fn nudges(&self) -> Vec<Self> {
            (0..4)
                .map(|i| {
                    let mut c = *self;
                    c[i] ^= 1;
                    c
                })
                .collect()
        }
    }

    impl Nudge for String {
        /// One character more, and the first one swapped.
        fn nudges(&self) -> Vec<Self> {
            let mut swapped = self.clone().into_bytes();
            swapped[0] ^= 1;
            vec![
                format!("{self}."),
                Self::from_utf8(swapped).expect("an ASCII sample"),
            ]
        }
    }

    impl Nudge for TextWeight {
        fn nudges(&self) -> Vec<Self> {
            [Self::Regular, Self::Bold]
                .into_iter()
                .filter(|w| w != self)
                .collect()
        }
    }

    impl Nudge for TextFace {
        fn nudges(&self) -> Vec<Self> {
            [
                Self::Mono,
                Self::Ui,
                Self::UiBold,
                Self::Display(0),
                Self::Display(1),
            ]
            .into_iter()
            .filter(|f| f != self)
            .collect()
        }
    }

    impl Nudge for aterm_render::BandIcon {
        fn nudges(&self) -> Vec<Self> {
            [Self::Info, Self::Success]
                .into_iter()
                .filter(|i| i != self)
                .collect()
        }
    }

    impl Nudge for ImageArt {
        fn nudges(&self) -> Vec<Self> {
            let Self::BrandKitty { dark } = *self;
            vec![Self::BrandKitty { dark: !dark }]
        }
    }

    impl Nudge for Box<TerminalSpecimenSpec> {
        /// Each setting the specimen's pixels depend on, the font's readiness
        /// alone included. `input` is not read: `input_fingerprint` is its
        /// identity. `motion` hashes by its scene's fingerprint, and no scene
        /// is built here.
        fn nudges(&self) -> Vec<Self> {
            let edits: &[fn(&mut TerminalSpecimenSpec)] = &[
                |s| s.input_fingerprint += 1,
                |s| s.prepared_font.candidate.regular = Some("Menlo".into()),
                |s| s.prepared_font.snapshot.status.push('.'),
                |s| s.prepared_font.snapshot.ready_epoch += 1,
                |s| s.prepared_font.snapshot.pending ^= true,
                |s| {
                    let mut ready = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
                        &s.prepared_font.candidate,
                    );
                    ready.snapshot = s.prepared_font.snapshot.clone();
                    assert!(ready.is_ready() && !s.prepared_font.is_ready());
                    assert!(ready.matches(&s.prepared_font.candidate));
                    s.prepared_font = ready;
                },
                |s| s.theme.bg ^= 1,
                |s| s.font_px = s.font_px.nudges()[0],
                |s| s.line_height = s.line_height.nudges()[0],
                |s| s.baseline_adjust += 1,
                |s| s.ligatures ^= true,
                |s| s.merged_ligatures ^= true,
                |s| s.cursor_break_ligatures ^= true,
                |s| s.synthetic_styles ^= true,
                |s| s.underline_position += 1,
                |s| s.underline_thickness += 1,
                |s| s.underline_skip_descenders ^= true,
                |s| s.text_blending = SpecimenTextBlending::Linear,
                |s| s.font_thicken ^= true,
                |s| s.stem_gamma = s.stem_gamma.nudges()[0],
                |s| {
                    s.variations
                        .push(SemanticVariation::new(0x7767_6874, 500.0))
                },
                |s| s.minimum_contrast = s.minimum_contrast.nudges()[0],
                |s| s.selection_foreground = Some(0x11_22_33),
                |s| s.selection_inactive ^= true,
                |s| {
                    s.wallpaper = Some(SpecimenWallpaper {
                        rgba: Arc::from(vec![0_u8; 4]),
                        w: 1,
                        h: 1,
                        fp: 7,
                        dim: 0.5,
                        tint: false,
                    });
                },
            ];
            edits
                .iter()
                .map(|edit| {
                    let mut s = self.clone();
                    edit(&mut s);
                    s
                })
                .collect()
        }
    }

    /// One copy of `$p` per least change of each field of variant `$v`. The
    /// pattern names every field (no `..`), so one left out does not compile.
    macro_rules! nudged {
        ($p:expr, $v:ident($($f:ident),*)) => {{
            let p: &DrawPrim = $p;
            assert!(matches!(p, DrawPrim::$v { $($f: _),* }));
            let mut out = Vec::new();
            $(
                if let DrawPrim::$v { $f, .. } = p {
                    for value in Nudge::nudges($f) {
                        let mut q = p.clone();
                        if let DrawPrim::$v { $f, .. } = &mut q {
                            *$f = value;
                        }
                        out.push((stringify!($f), q));
                    }
                }
            )*
            out
        }};
    }

    fn nudged(p: &DrawPrim) -> Vec<(&'static str, DrawPrim)> {
        match p {
            DrawPrim::Panel { .. } => nudged!(p, Panel(x, y, w, h, radius, fill)),
            DrawPrim::Ring { .. } => {
                nudged!(p, Ring(cx, cy, r_outer, thickness, track, frac, fill))
            }
            DrawPrim::Capsule { .. } => nudged!(p, Capsule(x, y, w, h, frac, fill, track)),
            DrawPrim::Dot { .. } => nudged!(p, Dot(cx, cy, r, color)),
            DrawPrim::Text { .. } => {
                nudged!(p, Text(x, baseline, s, px, color, weight, face))
            }
            DrawPrim::TerminalSpecimen { .. } => nudged!(p, TerminalSpecimen(x, y, spec)),
            DrawPrim::Stroke { .. } => {
                nudged!(p, Stroke(x, y, w, h, radius, width, color))
            }
            DrawPrim::Line { .. } => nudged!(p, Line(x1, y1, x2, y2, width, color)),
            DrawPrim::Dashed { .. } => {
                nudged!(p, Dashed(x, y, w, h, width, on, off, color))
            }
            DrawPrim::HsvDisk { .. } => nudged!(p, HsvDisk(cx, cy, r, value)),
            DrawPrim::ClipPush { .. } => nudged!(p, ClipPush(x, y, w, h)),
            DrawPrim::FadePush { .. } => nudged!(p, FadePush(y, h, from, to)),
            DrawPrim::BandIcon { .. } => nudged!(p, BandIcon(x, y, size, icon, color)),
            DrawPrim::Image { .. } => nudged!(p, Image(x, y, w, h, art)),
            DrawPrim::ClipPop | DrawPrim::FadePop => Vec::new(),
        }
    }

    #[test]
    fn equal_prims_fingerprint_equally() {
        let (a, b) = (samples(), samples());
        for (a, b) in a.iter().zip(&b) {
            assert_eq!(fp(a), fp(b), "{a:?}");
        }
    }

    #[test]
    fn every_variant_fingerprints_apart() {
        let fps: std::collections::BTreeSet<u64> = samples().iter().map(fp).collect();
        assert_eq!(fps.len(), samples().len());
    }

    #[test]
    fn a_least_change_to_any_field_moves_the_fingerprint() {
        let mut missed = Vec::new();
        for p in samples() {
            let base = fp(&p);
            for (field, q) in nudged(&p) {
                if fp(&q) == base {
                    missed.push(format!("{field} of {p:?} → {q:?}"));
                }
            }
        }
        assert!(
            missed.is_empty(),
            "changes the fingerprint misses: {missed:#?}"
        );
    }
}
