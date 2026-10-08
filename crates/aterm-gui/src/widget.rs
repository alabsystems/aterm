// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The tray draw IR: renderer-agnostic drawing primitives ([`DrawPrim`]) and the
//! [`TrayInput`] one-frame bundle that carries them. The GPU overlay pass and the
//! CPU/softbuffer fallback consume the same `DrawPrim` list, so every overlay card
//! lays out once and renders identically on both paths. All chrome text goes
//! through [`text_prim`], so every string maps to a named step of the type scale.

use std::sync::Arc;

use aterm_render::RenderInput;

use crate::type_scale::StepPx;

/// RGBA8, straight (non-premultiplied) alpha. `a < 255` is the frosted-card glass.
pub(crate) type Rgba = [u8; 4];

pub(crate) fn rgba(c: [u8; 3], a: u8) -> Rgba {
    [c[0], c[1], c[2], a]
}

/// The weight of a chrome text run. `Bold` draws from the real bold sibling of
/// the user's terminal face when it covers the glyph
/// (`tray_raster::select_chrome_face`), else the regular face; never a synthetic
/// dilation. Orthogonal to [`TextFace`]: `UiBold` is the UI face's synthesized
/// semibold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TextWeight {
    Regular,
    Bold,
}

/// The face a [`DrawPrim::Text`] rasterizes in ([`crate::tray_raster`] picks the
/// font). `Mono` is the terminal face (the user's primary plus DejaVu coverage
/// fallback), for code, hex readouts, the preview's terminal body and glyph art;
/// `Ui` is the native proportional system face (SF Pro on macOS, mono elsewhere)
/// for panel chrome; `UiBold` is the UI face with synthesized semibold. `Mono`
/// is the default; a prim opts into the UI face.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TextFace {
    Mono,
    Ui,
    UiBold,
}

/// The exact, view-authored terminal face candidate carried by every
/// renderer-native specimen run. Kept in the draw IR, not global state, so two
/// Settings views never borrow each other's uncommitted font when their frames
/// rasterize out of order. Empty slots follow the committed renderer's face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SemanticVariation {
    pub(crate) tag: u32,
    pub(crate) value_bits: u32,
}

impl SemanticVariation {
    pub(crate) fn new(tag: u32, value: f32) -> Self {
        Self {
            tag,
            value_bits: value.to_bits(),
        }
    }

    pub(crate) fn value(self) -> f32 {
        f32::from_bits(self.value_bits)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SemanticFontCandidate {
    pub(crate) regular: Option<String>,
    pub(crate) bold: Option<String>,
    pub(crate) italic: Option<String>,
    pub(crate) bold_italic: Option<String>,
    /// Ordered broad-script fallbacks. Resolution and parsing belong to the
    /// parked semantic-font worker; paint sees only the completed renderer.
    pub(crate) fallback: Vec<String>,
    pub(crate) symbol: Option<String>,
    pub(crate) emoji: Option<String>,
    /// Parsed, normalized variable-font requests. Float bits make the complete
    /// request stable and hashable across the worker generation handshake.
    pub(crate) variations: Vec<SemanticVariation>,
    pub(crate) synthetic_styles: bool,
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
        }
    }
}

impl SemanticFontCandidate {
    pub(crate) fn is_host(&self) -> bool {
        self.regular.is_none()
            && self.bold.is_none()
            && self.italic.is_none()
            && self.bold_italic.is_none()
            && self.fallback.is_empty()
            && self.symbol.is_none()
            && self.emoji.is_none()
            && self.variations.is_empty()
            && self.synthetic_styles
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

/// Complete renderer state for one bounded, real terminal-frame specimen.
///
/// The input is an immutable engine snapshot. Every other field is reapplied to
/// an isolated semantic renderer fork immediately before `render_input`, so an
/// uncommitted Settings candidate can neither mutate a live terminal renderer
/// nor borrow state from another Settings view.
#[derive(Clone, Debug)]
pub(crate) struct TerminalSpecimenSpec {
    pub(crate) input: Arc<RenderInput>,
    pub(crate) input_fingerprint: u64,
    /// Exact host-prepared renderer source injected through `ViewCx`. Raster
    /// forks this private snapshot and never consults process-global state.
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
}

/// The only constructor of [`DrawPrim::Text`]. Its size is a [`StepPx`],
/// mintable only by [`TypeStep`], so every chrome text site maps to a named step
/// of the 5-step type scale. `weight` picks the Mono bold sibling; `face` picks
/// Mono or the Ui/UiBold face. `every_text_prim_goes_through_the_funnel`
/// enforces the funnel.
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

/// One renderer-agnostic primitive, in FRAME pixel coordinates (origin top-left).
/// Angles for arcs are clockwise fractions of a full turn starting at 12 o'clock.
#[derive(Clone, Debug)]
pub(crate) enum DrawPrim {
    /// The card: a flat translucent `fill` (the whole prim list is rasterized to
    /// one RGBA overlay — see `tray_raster`'s module doc).
    Panel {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        fill: Rgba,
    },
    /// A concentric gauge: a faint full-circle `track` (capacity), a bold `sys`
    /// arc (whole-machine usage) and an optional thinner `tab` arc inside (this
    /// tab's slice). `dashed` marks an unobtainable inner arc (per-tab GPU) so it
    /// reads as "—", not a fabricated value.
    Ring {
        cx: f32,
        cy: f32,
        r_outer: f32,
        thickness: f32,
        track: Rgba,
        sys_frac: f32,
        sys_color: Rgba,
        tab_frac: Option<f32>,
        tab_color: Rgba,
        dashed_tab: bool,
    },
    /// A horizontal rounded capacity bar (disk): `frac` of `w` filled in `fill` over a
    /// faint `track`.
    Capsule {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        frac: f32,
        fill: Rgba,
        track: Rgba,
    },
    /// A status dot (network health).
    Dot {
        cx: f32,
        cy: f32,
        r: f32,
        color: Rgba,
    },
    /// Free-positioned text, `px` tall, positioned by baseline. `face` selects
    /// the font (see [`TextFace`]); `Mono` figures are tabular. `weight` picks
    /// the Mono bold sibling. Construct only via [`text_prim`], which pins the
    /// size to the type scale.
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
    /// A complete tiny terminal frame rendered by the shipping CPU renderer.
    /// This preserves cell backgrounds, ANSI attributes, selection,
    /// decorations, cursor/shaping interaction, CJK, symbol, and colour-emoji
    /// routing as one exact `RenderInput`.
    TerminalSpecimen {
        x: f32,
        y: f32,
        spec: Box<TerminalSpecimenSpec>,
    },
    /// One premultiplied, saturating-add light rectangle emitted by the shared
    /// `aterm-effects` cursor engine. The preview lowers `GlowQuad` into this IR
    /// rather than approximating an effect with translucent panels.
    AdditiveRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        /// Packed `0x00RRGGBB`, already premultiplied by effect coverage.
        premul: u32,
    },
    /// One radial cursor-effect halo emitted by the shared effects engine.
    /// Geometry remains logical here and scales with the rest of the semantic
    /// surface; the rasterizer uses aterm-render's integer halo kernel.
    EffectHalo {
        halo: aterm_render::RainHalo,
        offset_x: f32,
        offset_y: f32,
    },
    /// One procedural EMBERFORGE patch emitted by `CursorGlow`. The rasterizer
    /// evaluates aterm-render's integer fire field, keeping the effect's Add/Over
    /// blend inside the semantic preview.
    EffectFire {
        patch: aterm_render::FirePatch,
        offset_x: f32,
        offset_y: f32,
    },
    /// An outlined rounded rect: a stroke of `width` centered on the rect edge,
    /// for focus rings, framed inputs, borders, swatch rings, separators, drawn
    /// icons and the text caret. Thin (`width <= 1`) axis-aligned strokes snap to
    /// device pixels; a rounded (`radius > 0`) outline uses the analytic SDF; a
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
    /// An anti-aliased round-capped segment, used by native pictograms so
    /// direction affordances stay crisp at 1× and Retina scale.
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        width: f32,
        color: Rgba,
    },
    /// A filled HSV colour disk (the settings colour wheel): per-pixel polar hue
    /// (0° at 12 o'clock, clockwise, as [`DrawPrim::Ring`] arcs) and saturation
    /// (radius), scaled by `value`, with a ~1px anti-aliased rim. A raster op in
    /// the shared CPU rasterizer, so it matches on both backends.
    HsvDisk {
        cx: f32,
        cy: f32,
        r: f32,
        value: f32,
    },
    /// Push a rectangular clip (frame px): every later prim is clipped to the
    /// intersection of all pushed rects until the matching
    /// [`DrawPrim::ClipPop`]. Used by the scrolling body, sticky headers, the
    /// scrollbar and menus that must stay inside a card's rounded corners.
    ClipPush { x: f32, y: f32, w: f32, h: f32 },
    /// Pop the most recent [`DrawPrim::ClipPush`]. Unbalanced pops are ignored.
    ClipPop,
    /// One of the message band's drawn icons (`aterm_render::BandIcon`) in a
    /// `size`-point square at `(x, y)`, in `color`, rasterized at device scale so
    /// Settings ▸ Messages' severity column uses the band's own marks.
    BandIcon {
        x: f32,
        y: f32,
        size: f32,
        icon: aterm_render::BandIcon,
        color: Rgba,
    },
}

/// Translate every prim by `(dx, dy)` frame px. Used to rasterize a floating card
/// (the About dialog) into a buffer cropped to its paint bounds.
pub(crate) fn translate_prims(prims: &mut [DrawPrim], dx: f32, dy: f32) {
    for p in prims {
        match p {
            DrawPrim::Panel { x, y, .. }
            | DrawPrim::Capsule { x, y, .. }
            | DrawPrim::Stroke { x, y, .. }
            | DrawPrim::ClipPush { x, y, .. }
            | DrawPrim::BandIcon { x, y, .. }
            | DrawPrim::AdditiveRect { x, y, .. } => {
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
            DrawPrim::EffectHalo {
                offset_x, offset_y, ..
            }
            | DrawPrim::EffectFire {
                offset_x, offset_y, ..
            } => {
                *offset_x += dx;
                *offset_y += dy;
            }
            DrawPrim::Ring { cx, cy, .. }
            | DrawPrim::Dot { cx, cy, .. }
            | DrawPrim::HsvDisk { cx, cy, .. } => {
                *cx += dx;
                *cy += dy;
            }
            DrawPrim::ClipPop => {}
        }
    }
}

/// RGB → HSV (`h` in 0..1 turns, `s`/`v` in 0..1), the inverse of
/// [`hsv_to_rgb`]; places the Tab Color wheel's marker on the disk the raster
/// paints.
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

/// The whole open tray for one frame: the card rect + all enabled cards' prims,
/// positioned. Rides as `RenderInput.tray`; `None` ⇒ no tray.
#[derive(Clone, Debug, Default)]
pub(crate) struct TrayInput {
    pub prims: Vec<DrawPrim>,
    /// The card rect `(x, y, w, h)` for the blur/scissor region.
    pub card: (f32, f32, f32, f32),
}

#[cfg(test)]
mod tests {
    /// TYPE-SCALE TOTALITY, the funnel half: [`text_prim`] is the ONLY
    /// `DrawPrim::Text` constructor in the crate (the type half is
    /// `type_scale::StepPx`'s private field — only `TypeStep` mints sizes, so
    /// with this funnel every chrome text site maps to a named step and no
    /// orphan multiplier can compile). Scans every `src/*.rs` line mentioning
    /// `DrawPrim::Text`: pattern uses (`..`, `matches!`, `if let`) and comments
    /// are fine anywhere; CONSTRUCTION lines are allowed only in widget.rs (the
    /// funnel body) and tray_raster.rs (the rasterizer's consuming match arm).
    #[test]
    fn every_text_prim_goes_through_the_funnel() {
        // Built at runtime so this test's own source never contains the
        // contiguous token it scans for.
        let needle = format!("DrawPrim{}", ":\u{3a}Text").replace('\u{3a}', ":");
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sanctioned_sites = 0usize;
        for entry in std::fs::read_dir(&src_dir).expect("read src/") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let sanctioned = matches!(name.as_str(), "widget.rs" | "tray_raster.rs");
            let content = std::fs::read_to_string(&path).expect("read source");
            for (i, line) in content.lines().enumerate() {
                if !line.contains(&needle) {
                    continue;
                }
                let t = line.trim_start();
                let is_comment =
                    t.starts_with("//") || t.starts_with("///") || t.starts_with("//!");
                // PATTERNS, not constructions. The `..` / `matches!` / `if let`
                // spellings were the ones this scanner knew; a `let`-else
                // destructuring spreads its fields over the FOLLOWING lines, so
                // the needle's own line carries none of those markers and a
                // reader of prims got flagged as a writer of them. A binding
                // form is the discriminator: a construction is never the first
                // token after `let`.
                let is_pattern = line.contains("..")
                    || line.contains("matches!")
                    || line.contains("if let")
                    || t.starts_with("let ")
                    || t.starts_with("} = ");
                if is_comment || is_pattern {
                    continue;
                }
                assert!(
                    sanctioned,
                    "{name}:{}: a Text prim is constructed outside the text_prim funnel — \
                     route it through widget::text_prim (TypeStep-sized, baseline-positioned)",
                    i + 1
                );
                sanctioned_sites += 1;
            }
        }
        // Exactly the funnel body + the rasterizer's match arm.
        assert_eq!(
            sanctioned_sites, 2,
            "expected exactly the text_prim constructor and the tray_raster consumer"
        );
    }
}
