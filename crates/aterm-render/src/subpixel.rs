// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux subpixel-RGB text, stage 1 of `docs/RFC-linux-subpixel-text.md`: per-channel
//! (LCD) glyph coverage for the CPU compositor path only.
//!
//! As with GNOME Terminal or foot under fontconfig `rgba=rgb`, each output pixel
//! carries three coverage samples, one per subpixel, at 3× horizontal resolution. The
//! outline is the [`crate::hinted`] seam's (LCD hint target, FreeType's
//! `FT_LOAD_TARGET_LCD` twin), or the unhinted one when hinting is off or the LCD
//! hinter declines the face; [`crate::raster`] fills it at 3×, then FreeType's default
//! FIR5 LCD filter runs and the samples fold into 3 bytes per texel.
//!
//! Stage 1 is deliberately narrow: CPU compositor only (the GPU keeps its R8 grayscale
//! atlas; stage 2 is the dual-source blend), primary-family text glyphs, opaque frames
//! (translucency and wallpaper fall back to grayscale per frame), 1× cell scale
//! (DECDWL/DECDHL rows stay grayscale). Everything outside the gate takes the grayscale
//! path, so the shared [`crate::GlyphImage`] store, the goldens, and macOS/Windows are
//! byte-identical with the flag on or off.
//!
//! Built for `cfg(all(unix, not(target_os = "macos")))`, and for macOS unit tests,
//! which call the raster directly with the embedded font: macOS removed subpixel
//! rendering OS-wide and Windows has no aterm LCD path yet.

use aterm_font::autohint::{HintingInstance, Target};

use crate::hinted::PathPen;

/// How per-channel coverage maps onto the panel's subpixel order. Off at renderer
/// construction and set live from the `font_subpixel` config key: opt-in while stage 1
/// is judged on real screens (the RFC's kill criterion).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SubpixelMode {
    /// Grayscale coverage everywhere: the default.
    #[default]
    Off,
    /// Horizontal RGB stripes (the overwhelmingly common LCD geometry, and
    /// what fontconfig `rgba=rgb` means).
    Rgb,
    /// Horizontal BGR stripes: the same coverage with the R and B channels
    /// swapped at raster time.
    Bgr,
}

impl SubpixelMode {
    /// Parse one `font_subpixel` spelling (via `Renderer::set_font_subpixel`); anything
    /// unrecognized is [`SubpixelMode::Off`], as `font_hinting` falls back to its default.
    pub(crate) fn parse(s: &str) -> Self {
        match s.trim() {
            "rgb" | "on" | "1" | "true" => Self::Rgb,
            "bgr" => Self::Bgr,
            _ => Self::Off,
        }
    }

    /// The mode a renderer is built with: always [`SubpixelMode::Off`] (the
    /// config setter opts in; the portable test path keeps it off).
    #[inline]
    pub(crate) const fn initial() -> Self {
        Self::Off
    }

    /// The canonical spelling the getter round-trips.
    #[inline]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Rgb => "rgb",
            Self::Bgr => "bgr",
        }
    }
}

/// The hinter target for the subpixel raster: the autohinter's LCD target (FreeType's
/// `FT_LOAD_TARGET_LCD`), which grid-fits vertically and leaves horizontal stem phases
/// to the 3× resolution; snapping them, as grayscale `full` does, is what subpixel
/// rendering exists to avoid. `font_hinting = "off"` disables hinting here too (the
/// caller passes no instance).
#[inline]
pub(crate) const fn lcd_hint_options() -> Target {
    Target::Lcd
}

/// FreeType's default FIR5 LCD filter (`FT_LCD_FILTER_DEFAULT`), fixed-point over 256:
/// spreads each subpixel sample across its neighbours so chroma fringes stay below
/// visibility while luminance keeps the 3× resolution. Sums to 256, so solid ink stays
/// solid.
const FIR5: [u32; 5] = [8, 77, 86, 77, 8];

/// Records an aterm-font outline (design units) into a [`PathPen`] in pixels, both
/// y up, at `px / units_per_em`: the scale the grayscale unhinted raster draws with.
struct Scaled<'a> {
    pen: &'a mut PathPen,
    scale: f32,
}

impl aterm_font::OutlineSink for Scaled<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let s = self.scale;
        self.pen.move_to(x * s, y * s);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let s = self.scale;
        self.pen.line_to(x * s, y * s);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let s = self.scale;
        self.pen.quad_to(x1 * s, y1 * s, x * s, y * s);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let s = self.scale;
        self.pen
            .curve_to(x1 * s, y1 * s, x2 * s, y2 * s, x * s, y * s);
    }
    fn close(&mut self) {
        self.pen.close();
    }
}

/// Rasterize glyph `gid` of the face at `px` as per-channel (subpixel) coverage:
/// `(width, height, xmin, ymin, bytes)` with 3 bytes per texel in framebuffer channel
/// order (byte 0 → the `0x00RR0000` channel, 1 → GG, 2 → BB; `bgr` swaps which subpixel
/// feeds R and B). Placement follows [`crate::hinted::hinted_glyph_raster`] (blit anchor
/// `(cell_x + xmin, baseline - height - ymin)`), except the ink box is widened by 1 px
/// each side for the FIR5 spread, so `xmin` is one less than the grayscale raster's.
///
/// `hint`, when present, must be an LCD-target instance from [`lcd_hint_options`].
/// `None` draws the unhinted outline (with `font_hinting = "off"`, or for a face the
/// LCD instance builder declines), read through aterm-font at the face's default
/// instance like the grayscale unhinted raster. Returns `None` for no outline (space)
/// or a parse/draw failure, and the caller falls back to the grayscale blit.
pub(crate) fn subpixel_glyph_raster(
    bytes: &[u8],
    index: u32,
    gid: u16,
    px: f32,
    hint: Option<&HintingInstance>,
    bgr: bool,
) -> Option<(usize, usize, i32, i32, Vec<u8>)> {
    if !px.is_finite() || px <= 0.0 {
        return None;
    }
    let mut pen = PathPen::default();
    match hint {
        Some(h) => {
            h.draw(bytes, index, gid, &mut pen)?;
        }
        None => {
            let face = aterm_font::Face::parse(bytes, index).ok()?;
            let scale = px / f32::from(face.units_per_em());
            // `None`: no outline (space and friends), or a glyph id past the face
            // or malformed; nothing to blit either way.
            face.outline_glyph(
                aterm_font::GlyphId(gid),
                &mut Scaled {
                    pen: &mut pen,
                    scale,
                },
            )?;
        }
    }
    if pen.is_blank() {
        // No outline (space and friends): nothing to blit — the grayscale
        // path draws nothing for these either, so `None` is exact.
        return None;
    }
    // 1 px of filter padding each side: the FIR5 spread reaches 2 subpixels (2/3 px)
    // past the ink box. The pad is also this path's [`crate::variation::RASTER_PAD`]
    // (3 samples of slack at 3×), keeping the outline off the rasterizer's boundary;
    // do not narrow it to the ink box.
    let x_min = pen.min_x.floor() - 1.0;
    let x_max = pen.max_x.ceil() + 1.0;
    let y_min = pen.min_y.floor();
    let y_max = pen.max_y.ceil();
    let (w, h) = ((x_max - x_min) as i32, (y_max - y_min) as i32);
    // Same sanity caps as the hinted raster (the +2 pad keeps a 4096-wide
    // grayscale-legal glyph legal here too).
    if w <= 2 || h <= 0 || w > 4098 || h > 4096 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    let w_sub = w * 3;
    // Fill at 3× horizontal resolution: the same outline, x scaled AFTER the
    // translate so subpixel k of pixel column i is the sample at
    // `x_min + (3i + k)/3` — 1 texel = 1 physical subpixel.
    let mut ras = crate::raster::Rasterizer::new(w_sub, h);
    pen.fill(&mut ras, x_min, y_max, 3.0);
    let mut cov = vec![0u8; w_sub * h];
    ras.for_each_pixel(|i, a| {
        if let Some(slot) = cov.get_mut(i) {
            *slot = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    });
    // FIR5 along the subpixel axis (per row, zero beyond the edges — the 1 px
    // pad guarantees no ink reaches them), then fold triplets into
    // framebuffer-channel-ordered texels.
    let mut out = vec![0u8; w * h * 3];
    let mut filtered = vec![0u8; w_sub];
    for j in 0..h {
        let row = &cov[j * w_sub..(j + 1) * w_sub];
        for (i, slot) in filtered.iter_mut().enumerate() {
            let mut acc = 0u32;
            for (k, &f) in FIR5.iter().enumerate() {
                let s = i as isize + k as isize - 2;
                if s >= 0 && (s as usize) < w_sub {
                    acc += f * u32::from(row[s as usize]);
                }
            }
            *slot = ((acc + 128) >> 8).min(255) as u8;
        }
        let orow = &mut out[j * w * 3..(j + 1) * w * 3];
        for i in 0..w {
            // Physical subpixel order left→middle→right; on an RGB panel that
            // IS (R, G, B). BGR panels light the same stripes under swapped
            // colours, so R and B trade coverage.
            let (l, m, r) = (filtered[3 * i], filtered[3 * i + 1], filtered[3 * i + 2]);
            let (cr, cg, cb) = if bgr { (r, m, l) } else { (l, m, r) };
            orow[3 * i] = cr;
            orow[3 * i + 1] = cg;
            orow[3 * i + 2] = cb;
        }
    }
    Some((w, h, x_min as i32, y_min as i32, out))
}

#[cfg(all(test, feature = "embedded-font"))]
mod tests {
    use super::*;
    use crate::hinted::{HintBank, unicode_gid};

    fn dejavu() -> &'static [u8] {
        crate::embedded_font()
    }

    fn lcd_instance(px: f32) -> std::sync::Arc<HintingInstance> {
        let bytes = dejavu();
        HintBank::default()
            .instance_with(
                bytes.as_ptr() as usize,
                bytes,
                0,
                px,
                Some(lcd_hint_options()),
                &[],
                0,
            )
            .expect("LCD instance builds for the embedded face")
    }

    /// A space has no outline: the raster declines (`None`) and the caller's
    /// grayscale path draws nothing — the blank-glyph convention.
    #[test]
    fn space_declines() {
        let hint = lcd_instance(12.0);
        let gid = unicode_gid(dejavu(), 0, ' ').unwrap();
        assert!(subpixel_glyph_raster(dejavu(), 0, gid, 12.0, Some(&hint), false).is_none());
    }

    /// Stem edges carry per-channel structure (some texel has R != B), where a grayscale
    /// raster is R == G == B by construction.
    #[test]
    fn stem_edges_carry_channel_structure() {
        let hint = lcd_instance(15.0);
        let gid = unicode_gid(dejavu(), 0, 'l').unwrap();
        let (w, h, .., bytes) =
            subpixel_glyph_raster(dejavu(), 0, gid, 15.0, Some(&hint), false).unwrap();
        let fringed = (0..w * h)
            .filter(|i| {
                let (r, b) = (bytes[3 * i], bytes[3 * i + 2]);
                r.abs_diff(b) > 16
            })
            .count();
        assert!(
            fringed > 0,
            "an 'l' stem at 15px must have R!=B fringe texels ({w}x{h}: {bytes:?})"
        );
    }

    /// Solid ink stays solid (FIR5 sums to 256): subpixel re-encodes edges and never
    /// thins strokes. Judged at 32px, where a DejaVu '0' stem (~3 px) has texels whose
    /// whole 5-subpixel window is ink; at body sizes every sample sees an edge.
    #[test]
    fn solid_interior_is_full_in_every_channel() {
        let hint = lcd_instance(32.0);
        let gid = unicode_gid(dejavu(), 0, '0').unwrap();
        let (w, h, .., bytes) =
            subpixel_glyph_raster(dejavu(), 0, gid, 32.0, Some(&hint), false).unwrap();
        let solid = (0..w * h)
            .filter(|i| bytes[3 * i..3 * i + 3].iter().all(|&c| c == 255))
            .count();
        assert!(
            solid > 0,
            "'0' at 32px must keep fully-covered texels ({w}x{h})"
        );
    }

    /// BGR is exactly the R/B swap of RGB — same coverage, re-ordered.
    #[test]
    fn bgr_swaps_r_and_b() {
        let hint = lcd_instance(12.0);
        let gid = unicode_gid(dejavu(), 0, 'g').unwrap();
        let rgb = subpixel_glyph_raster(dejavu(), 0, gid, 12.0, Some(&hint), false).unwrap();
        let bgr = subpixel_glyph_raster(dejavu(), 0, gid, 12.0, Some(&hint), true).unwrap();
        assert_eq!((rgb.0, rgb.1, rgb.2, rgb.3), (bgr.0, bgr.1, bgr.2, bgr.3));
        for i in 0..rgb.0 * rgb.1 {
            assert_eq!(rgb.4[3 * i], bgr.4[3 * i + 2]);
            assert_eq!(rgb.4[3 * i + 1], bgr.4[3 * i + 1]);
            assert_eq!(rgb.4[3 * i + 2], bgr.4[3 * i]);
        }
    }

    /// The unhinted LCD raster fills the outline the grayscale unhinted raster
    /// fills, at the same scale: the 3× fill and FIR5 both conserve ink, so a
    /// glyph's LCD ink (channel sum over 3·255) is its grayscale ink to within
    /// edge rounding (measured on these glyphs: 0.17% at worst). A scale slip
    /// (design units left unscaled, or `px` applied twice) misses by a factor.
    #[test]
    fn unhinted_subpixel_ink_is_the_unhinted_outline() {
        for px in [12.0f32, 24.0] {
            for ch in ['x', 'M', '0', 'g', '@', 'i', '.', '_'] {
                let gid = unicode_gid(dejavu(), 0, ch).unwrap();
                let (.., lcd) = subpixel_glyph_raster(dejavu(), 0, gid, px, None, false).unwrap();
                let (.., gray) =
                    crate::variation::varied_glyph_raster(dejavu(), 0, &[], gid, px).unwrap();
                let lcd_ink = lcd.iter().map(|&v| f32::from(v)).sum::<f32>() / (3.0 * 255.0);
                let gray_ink = gray.iter().map(|&v| f32::from(v)).sum::<f32>() / 255.0;
                assert!(
                    (lcd_ink - gray_ink).abs() <= 0.01 * gray_ink + 0.05,
                    "{ch:?} @{px}px: unhinted LCD ink {lcd_ink} vs grayscale {gray_ink}"
                );
            }
        }
    }

    /// Spelling table: env/config parse and the canonical round-trip.
    #[test]
    fn mode_spellings() {
        use SubpixelMode::*;
        for (s, m) in [
            ("rgb", Rgb),
            ("on", Rgb),
            ("1", Rgb),
            ("true", Rgb),
            ("bgr", Bgr),
            ("off", Off),
            ("0", Off),
            ("", Off),
            ("vrgb", Off), // vertical geometries are NOT stage 1
            ("anything", Off),
        ] {
            assert_eq!(SubpixelMode::parse(s), m, "spelling {s:?}");
        }
        assert_eq!(Rgb.as_str(), "rgb");
        assert_eq!(Bgr.as_str(), "bgr");
        assert_eq!(Off.as_str(), "off");
    }
}
