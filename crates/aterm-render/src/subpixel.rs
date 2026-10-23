// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-channel (LCD) subpixel glyph coverage for the Linux CPU compositor path.
//!
//! Fills outlines at 3× horizontal resolution, runs FreeType's default FIR5 LCD
//! filter, and folds samples into 3 bytes per texel.

use aterm_font::autohint::{HintingInstance, Target};

use crate::hinted::PathPen;

/// Panel subpixel channel order (`font_subpixel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SubpixelMode {
    /// Grayscale coverage everywhere.
    #[default]
    Off,
    /// Horizontal RGB stripes.
    Rgb,
    /// Horizontal BGR stripes (R and B channels swapped at raster time).
    Bgr,
}

impl SubpixelMode {
    /// Parses a `font_subpixel` config value, defaulting to [`SubpixelMode::Off`].
    pub(crate) fn parse(s: &str) -> Self {
        match s.trim() {
            "rgb" | "on" | "1" | "true" => Self::Rgb,
            "bgr" => Self::Bgr,
            _ => Self::Off,
        }
    }

    /// Initial renderer mode (`Off`).
    #[inline]
    pub(crate) const fn initial() -> Self {
        Self::Off
    }

    /// Canonical string representation.
    #[inline]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Rgb => "rgb",
            Self::Bgr => "bgr",
        }
    }
}

/// Returns the autohinter LCD target (`Target::Lcd`) for subpixel rasterization.
#[inline]
pub(crate) const fn lcd_hint_options() -> Target {
    Target::Lcd
}

/// FreeType's default FIR5 LCD filter (`FT_LCD_FILTER_DEFAULT`), fixed-point over 256.
const FIR5: [u32; 5] = [8, 77, 86, 77, 8];

/// Scales an `aterm-font` outline from design units into pixel coordinates in a [`PathPen`].
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

/// Rasterizes glyph `gid` at `px` into `(width, height, xmin, ymin, bytes)` with 3 bytes per texel.
///
/// Uses `hint` when provided, or the unhinted outline otherwise; returns `None` when the
/// glyph has no outline or cannot be rasterized.
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
        return None;
    }
    let x_min = pen.min_x.floor() - 1.0;
    let x_max = pen.max_x.ceil() + 1.0;
    let y_min = pen.min_y.floor();
    let y_max = pen.max_y.ceil();
    let (w, h) = ((x_max - x_min) as i32, (y_max - y_min) as i32);
    if w <= 2 || h <= 0 || w > 4098 || h > 4096 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    let w_sub = w * 3;
    let mut ras = crate::raster::Rasterizer::new(w_sub, h);
    pen.fill(&mut ras, x_min, y_max, 3.0);
    let mut cov = vec![0u8; w_sub * h];
    ras.for_each_pixel(|i, a| {
        if let Some(slot) = cov.get_mut(i) {
            *slot = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    });
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

    /// A space has no outline: the raster declines (`None`).
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

    /// Solid interior texels remain 255 across all three channels after FIR5 filtering.
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

    /// BGR swaps the R and B channels of the RGB coverage.
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

    /// Unhinted LCD total ink matches the grayscale unhinted outline's ink within rounding.
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
