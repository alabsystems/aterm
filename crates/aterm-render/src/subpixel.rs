// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LINUX SUBPIXEL-RGB TEXT, stage 1 of `docs/RFC-linux-subpixel-text.md`:
//! per-channel (LCD) glyph coverage for the CPU compositor path only.
//!
//! The desktop reference this chases (GNOME Terminal / foot with fontconfig
//! `rgba=rgb`) encodes 3× horizontal resolution into the R/G/B subpixels of an
//! LCD: each output pixel carries THREE coverage samples, one per channel, and
//! the panel's physical subpixel geometry turns the chroma fringes back into
//! sharpness. This module produces exactly that coverage: the same skrifa
//! outline the [`crate::hinted`] seam draws (LCD hint target, FreeType's
//! `FT_LOAD_TARGET_LCD` twin), rasterized at 3× horizontal resolution by the
//! same [`crate::raster`] coverage fill, then run through FreeType's default FIR5
//! LCD filter and folded into 3-bytes-per-texel per-channel coverage.
//!
//! STAGE-1 SCOPE (the RFC's §4 step 1, deliberately narrow so every state
//! ships): CPU compositor only (the GPU backend keeps its R8 grayscale atlas
//! untouched — stage 2 is the dual-source blend), primary-family text glyphs
//! only, opaque frames only (translucency/wallpaper fall back to grayscale per
//! frame), 1× cell scale only (DECDWL/DECDHL rows stay grayscale). Everything
//! outside the gate renders through the unchanged grayscale path — the
//! shared [`crate::GlyphImage`] store, the goldens, and macOS/Windows are
//! byte-identical whether the flag is on or off.
//!
//! Gated to `cfg(all(unix, not(target_os = "macos")))` at the module
//! declaration, exactly like [`crate::hinted`]: subpixel is the LINUX gap
//! (macOS removed subpixel OS-wide; Windows has no aterm LCD story yet).

use skrifa::{
    FontRef, MetadataProvider,
    instance::{LocationRef, Size},
    outline::{DrawSettings, Engine, HintingInstance, HintingOptions, SmoothMode, Target},
};

use crate::hinted::PathPen;

/// How per-channel coverage maps onto the panel's subpixel order. Resolved at
/// renderer construction from `ATERM_FONT_SUBPIXEL` (the `font_subpixel`
/// config key's env alias, which wins) and settable live like `font_hinting`;
/// DEFAULT [`SubpixelMode::Off`] — this whole path is opt-in while stage 1 is
/// judged on real screens (the RFC's kill criterion).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SubpixelMode {
    /// Grayscale coverage everywhere — the shipped default, byte-identical to
    /// the pre-subpixel renderer.
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
    /// Parse one mode spelling — shared by the env alias and the
    /// `font_subpixel` config key (via `Renderer::set_font_subpixel`).
    /// Unrecognized = [`SubpixelMode::Off`] (the default — forgiving in the
    /// same direction as `font_hinting`, whose unrecognized spellings resolve
    /// to ITS default).
    pub(crate) fn parse(s: &str) -> Self {
        match s.trim() {
            "rgb" | "on" | "1" | "true" => Self::Rgb,
            "bgr" => Self::Bgr,
            _ => Self::Off,
        }
    }

    /// Parse `ATERM_FONT_SUBPIXEL`. Unset or unrecognized = Off;
    /// `ATERM_RASTERIZER=fontdue` (the byte-stable portable path the
    /// golden/parity tests export) forces Off, exactly like the hint seam.
    pub(crate) fn from_env() -> Self {
        if crate::hinted::HintMode::fontdue_forced() {
            return Self::Off;
        }
        match std::env::var("ATERM_FONT_SUBPIXEL").ok().as_deref() {
            Some(s) => Self::parse(s),
            None => Self::Off,
        }
    }

    /// The canonical spelling the getter round-trips.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Rgb => "rgb",
            Self::Bgr => "bgr",
        }
    }
}

/// The skrifa hinting options for the subpixel raster: the autohinter with the
/// LCD smooth target — FreeType's `FT_LOAD_TARGET_LCD` pairing, which keeps
/// vertical grid fit while leaving horizontal stem phases for the 3× subpixel
/// resolution to resolve (snapping them, as the grayscale `full` mode does, is
/// exactly what subpixel rendering exists to avoid). One fixed target on
/// purpose: the desktop reference pairs `rgba=rgb` with one hint discipline,
/// not a matrix. `font_hinting = "off"` disables hinting for the subpixel
/// raster too (the caller passes no instance).
pub(crate) fn lcd_hint_options() -> HintingOptions {
    HintingOptions {
        engine: Engine::Auto(None),
        target: Target::Smooth {
            mode: SmoothMode::Lcd,
            // Same rationale as the grayscale seam: no vertical supersampling
            // in this analytic rasterizer. Interpreter paths only.
            symmetric_rendering: false,
            preserve_linear_metrics: false,
        },
    }
}

/// FreeType's default FIR5 LCD filter (`FT_LCD_FILTER_DEFAULT`), fixed-point
/// over 256: spreads each subpixel sample across its neighbours so the chroma
/// fringes stay below the visibility threshold while the luminance keeps the
/// 3× resolution. Sums to 256, so solid ink stays solid (255 → 255).
const FIR5: [u32; 5] = [8, 77, 86, 77, 8];

/// Rasterize glyph `gid` of the face at `px` as PER-CHANNEL (subpixel)
/// coverage: `(width, height, xmin, ymin, bytes)` with **3 bytes per texel**
/// in FRAMEBUFFER channel order (byte 0 → the `0x00RR0000` channel, 1 → GG,
/// 2 → BB; `bgr` swaps which SUBPIXEL feeds R and B). Placement follows the
/// [`crate::hinted::hinted_glyph_raster`] metric convention exactly — the
/// blit anchor is `(cell_x + xmin, baseline - height - ymin)` — except the
/// ink box is widened by 1 px on each side for the FIR5 filter spread (the
/// filter reaches 2 subpixels past the ink, which is why `xmin` is one less
/// than the grayscale raster's).
///
/// `hint`, when present, must be an LCD-target instance built from
/// [`lcd_hint_options`]; `None` draws the unhinted outline at `px` (the
/// `font_hinting = "off"` pairing). `None` return = no outline (space) or a
/// parse/draw failure — the caller falls back to the grayscale blit, so this
/// is always a safe enhancement, exactly the hint seam's contract.
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
    let font = FontRef::from_index(bytes, index).ok()?;
    let glyph_id = skrifa::GlyphId::from(gid);
    let outline = font.outline_glyphs().get(glyph_id)?;
    let mut pen = PathPen::default();
    // Pedantic OFF on the hinted path (a bytecode error degrades to the
    // unhinted outline, same as the grayscale seam).
    match hint {
        Some(h) => outline
            .draw(DrawSettings::hinted(h, false), &mut pen)
            .ok()?,
        None => outline
            .draw(
                DrawSettings::unhinted(Size::new(px), LocationRef::default()),
                &mut pen,
            )
            .ok()?,
    };
    if pen.is_blank() {
        // No outline (space and friends): nothing to blit — the grayscale
        // path draws nothing for these either, so `None` is exact.
        return None;
    }
    // 1 px of filter padding each side: the FIR5 spread reaches 2 subpixels
    // (2/3 px) past the ink box. That pad also happens to be this path's
    // [`crate::variation::RASTER_PAD`] — 3 subpixel samples of slack at 3×
    // horizontal resolution — so the fitted outline can never sit ON the
    // rasterizer's boundary and the accumulator-smear that ate the grayscale
    // '2' at ppem 19 has no way in here. Do not narrow it back to the ink box.
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

    /// The metric convention holds: 3 bytes per texel, the ink box padded by
    /// exactly 1 px each side relative to floor/ceil of the outline bounds,
    /// and sane 12px boxes over printable ASCII.
    #[test]
    fn triplet_layout_and_pad_ascii_12px() {
        let hint = lcd_instance(12.0);
        for ch in '!'..='~' {
            let Some(gid) = unicode_gid(dejavu(), 0, ch) else {
                continue;
            };
            let Some((w, h, _xmin, _ymin, bytes)) =
                subpixel_glyph_raster(dejavu(), 0, gid, 12.0, Some(&hint), false)
            else {
                continue;
            };
            assert_eq!(bytes.len(), w * h * 3, "3 bytes/texel for {ch:?}");
            assert!(w <= 34 && h <= 32, "sane 12px ink box for {ch:?}: {w}x{h}");
        }
    }

    /// A space has no outline: the raster declines (`None`) and the caller's
    /// grayscale path draws nothing — the blank-glyph convention.
    #[test]
    fn space_declines() {
        let hint = lcd_instance(12.0);
        let gid = unicode_gid(dejavu(), 0, ' ').unwrap();
        assert!(subpixel_glyph_raster(dejavu(), 0, gid, 12.0, Some(&hint), false).is_none());
    }

    /// THE POINT of the module: stem edges carry per-channel structure — some
    /// texel has R != B (the chroma-encoded fractional position gnome-terminal
    /// shows) — while a grayscale raster is R == G == B by construction.
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

    /// Solid ink interior stays solid: FIR5 sums to 256, so a texel deep
    /// inside a stem is (255, 255, 255) — subpixel must not thin solid
    /// strokes, only re-encode their edges. Judged at 32px, where a DejaVu
    /// '0' stem is ~3 px wide and MUST contain texels whose whole 5-subpixel
    /// filter window is inside the ink (at body sizes a 1–1.5 px stem has no
    /// such texel — every sample sees an edge, which is precisely the
    /// resolution subpixel encodes).
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

    /// Determinism: the atlas-cache assumption, same as the grayscale seam.
    #[test]
    fn raster_is_deterministic() {
        let hint = lcd_instance(12.0);
        let gid = unicode_gid(dejavu(), 0, 'm').unwrap();
        let a = subpixel_glyph_raster(dejavu(), 0, gid, 12.0, Some(&hint), false).unwrap();
        let b = subpixel_glyph_raster(dejavu(), 0, gid, 12.0, Some(&hint), false).unwrap();
        assert_eq!(a, b);
    }

    /// The unhinted draw (the `font_hinting = "off"` pairing) also rasterizes.
    #[test]
    fn unhinted_subpixel_rasterizes() {
        let gid = unicode_gid(dejavu(), 0, 'x').unwrap();
        assert!(subpixel_glyph_raster(dejavu(), 0, gid, 12.0, None, false).is_some());
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
