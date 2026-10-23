// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Renderer-agnostic overlay drawing primitives ([`DrawPrim`]) and [`TrayInput`] frame bundle.

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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TextWeight {
    Regular,
    Bold,
}

/// The face a [`DrawPrim::Text`] rasterizes in ([`crate::tray_raster`] picks the font).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
/// just before `render_input`, so an uncommitted Settings candidate neither mutates a live
/// renderer nor borrows another Settings view's state.
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

/// The only constructor of [`DrawPrim::Text`]. Its size is a [`StepPx`],
/// mintable only by [`TypeStep`](crate::type_scale::TypeStep), so every chrome
/// text site maps to a named step of the 5-step type scale. `weight` picks the
/// Mono bold sibling; `face` picks Mono or the Ui/UiBold face.
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
    /// the turn (clockwise from 12 o'clock) in `fill`. Its one user is the
    /// Appearance button icon, a half-filled ring.
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
}

/// Translate every prim by `(dx, dy)` frame px, e.g. to rasterize a floating card into a
/// buffer cropped to its paint bounds.
pub(crate) fn translate_prims(prims: &mut [DrawPrim], dx: f32, dy: f32) {
    for p in prims {
        match p {
            DrawPrim::Panel { x, y, .. }
            | DrawPrim::Capsule { x, y, .. }
            | DrawPrim::Stroke { x, y, .. }
            | DrawPrim::ClipPush { x, y, .. }
            | DrawPrim::BandIcon { x, y, .. } => {
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
