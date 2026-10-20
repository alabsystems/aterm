// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WALLPAPER PREPARATION, shared by every surface that draws the wallpaper:
//! the terminal present (`App::splice_wallpaper`) and Settings' previews
//! (`tray_raster`'s terminal specimens), so the picture Settings shows is the
//! one a tab draws. Cover-scale the decoded image to a frame's exact pixel
//! size, composite it over the theme background and tone it toward it by
//! `wallpaper_dim` ([`scale_wallpaper_cover`]), and sample one text tint per
//! grid cell ([`wallpaper_cell_tints`]) that [`merge_wallpaper_tint_ink`]
//! lays into the frame's ink channel. Pure: no window, renderer or clock.

use aterm_core::terminal::RenderCell;

/// The decoded wallpaper a frame is prepared from: straight sRGB RGBA8,
/// `w × h`, and the asset's fingerprint.
#[derive(Clone, Copy)]
pub(crate) struct WallpaperSource<'a> {
    pub(crate) rgba: &'a [u8],
    pub(crate) w: usize,
    pub(crate) h: usize,
    pub(crate) fp: u64,
}

/// One frame's wallpaper: the base layer the renderers draw under default
/// backgrounds, and one glyph tint per grid cell.
pub(crate) struct PreparedWallpaper {
    pub(crate) atlas: std::sync::Arc<aterm_core::render::SceneAtlas>,
    /// `rows*cols` entries, row-major ([`wallpaper_cell_tints`]).
    pub(crate) tint: Vec<Option<[u8; 3]>>,
}

/// PREPARE `source` for a frame `frame` (`w × h` pixels) holding a `grid`
/// (`rows × cols`) on the theme background `bg` (`0xRRGGBB`), toned by `dim`,
/// its tint judged against the default foreground `fg`. The atlas's version
/// moves with the asset, the dim and the size, so renderers that key their
/// caches on it convert each generation once.
pub(crate) fn prepare(
    source: WallpaperSource<'_>,
    (w, h): (usize, usize),
    (rows, cols): (usize, usize),
    dim: f32,
    (bg, fg): (u32, u32),
) -> PreparedWallpaper {
    let pixels = scale_wallpaper_cover(source.rgba, source.w, source.h, w, h, dim, bg);
    let tint = wallpaper_cell_tints(&pixels, w, h, rows, cols, fg);
    PreparedWallpaper {
        atlas: std::sync::Arc::new(aterm_core::render::SceneAtlas {
            width: w as u32,
            height: h as u32,
            rgba: pixels,
            version: source.fp ^ u64::from(dim.to_bits()) ^ (((w as u64) << 32) | h as u64),
        }),
        tint,
    }
}

/// One glyph tint per grid cell, sampled at each cell's center from the
/// scaled backdrop (the picture is smooth at cell granularity, so one texel
/// stands in for the cell; padding offsets are within a cell and immaterial).
pub(crate) fn wallpaper_cell_tints(
    rgba: &[u8],
    w: usize,
    h: usize,
    rows: usize,
    cols: usize,
    fallback_fg: u32,
) -> Vec<Option<[u8; 3]>> {
    let mut out = Vec::with_capacity(rows.saturating_mul(cols));
    if w == 0 || h == 0 || rows == 0 || cols == 0 {
        return out;
    }
    for r in 0..rows {
        let y = ((r * 2 + 1) * h / (rows * 2)).min(h - 1);
        for c in 0..cols {
            let x = ((c * 2 + 1) * w / (cols * 2)).min(w - 1);
            let i = (y * w + x) * 4;
            let px =
                (u32::from(rgba[i]) << 16) | (u32::from(rgba[i + 1]) << 8) | u32::from(rgba[i + 2]);
            out.push(wallpaper_text_tint_color(px, fallback_fg));
        }
    }
    out
}

/// Merge wallpaper-derived default-foreground tints into the frame's existing
/// sorted-unique ink channel. Existing producers win on an occupied cell. The
/// two vectors swap at the end so a steady frame reuses resident allocations.
/// This deliberately does not memoize the cell projection: changed terminal
/// glyphs must be observed, so a tinted present still performs the O(rows*cols)
/// eligibility scan.
pub(crate) fn merge_wallpaper_tint_ink(
    cells: &[Vec<RenderCell>],
    rows: usize,
    cols: usize,
    default_fg: [u8; 3],
    tint: &[Option<[u8; 3]>],
    existing: &mut Vec<aterm_core::render::InkCell>,
    scratch: &mut Vec<aterm_core::render::InkCell>,
) {
    scratch.clear();
    scratch.reserve(existing.len().saturating_add(rows.saturating_mul(cols) / 4));
    let mut ex = existing.iter().copied().peekable();
    for (r, row) in cells.iter().enumerate().take(rows) {
        if r > usize::from(u16::MAX) {
            break;
        }
        for (c, cell) in row.iter().enumerate().take(cols) {
            while ex
                .peek()
                .is_some_and(|entry| (usize::from(entry.row), usize::from(entry.col)) < (r, c))
            {
                scratch.push(ex.next().expect("peeked"));
            }
            if ex
                .peek()
                .is_some_and(|entry| usize::from(entry.row) == r && usize::from(entry.col) == c)
            {
                scratch.push(ex.next().expect("peeked"));
                continue;
            }
            // Blanks paint no glyph; a wide continuation has no lead glyph of
            // its own; non-default foreground keeps its SGR color.
            if cell.ch == ' ' || cell.wide || cell.fg != default_fg || c > usize::from(u16::MAX) {
                continue;
            }
            let Some(color) = tint.get(r * cols + c).copied().flatten() else {
                continue;
            };
            scratch.push(aterm_core::render::InkCell {
                row: r as u16,
                col: c as u16,
                color,
            });
        }
    }
    scratch.extend(ex);
    std::mem::swap(existing, scratch);
}

/// WCAG contrast ratio between two relative luminances.
fn wcag_contrast(a: f32, b: f32) -> f32 {
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// The tint pass guarantees at least this contrast against the backdrop
/// texel under the cell (WCAG AA for normal text). Both poles clear it at
/// every ground luminance, so the walk below always terminates readable.
const WALLPAPER_TINT_CONTRAST_FLOOR: f32 = 4.5;

/// The ground luminance below which white ink out-contrasts black ink
/// (`(1.05)/(L+0.05) >= (L+0.05)/0.05` ⇒ `L <= sqrt(1.05*0.05) - 0.05`).
const LIGHT_INK_BELOW: f32 = 0.179;

/// A "cool but readable" glyph tint for text sitting on `backdrop`: the
/// backdrop's own hue, flipped to whichever brightness pole out-contrasts
/// the ground (light ink over the picture's dark areas, dark ink over its
/// bright ones) and then walked — brightness first, saturation after —
/// until it clears [`WALLPAPER_TINT_CONTRAST_FLOOR`]. The letterforms pick
/// up the image's local color and stay legible no matter how light or dark
/// the picture is at that cell. Near-achromatic ground (grey / washed-out
/// areas) takes no hue — an arbitrary one would read as noise — and keeps
/// the ordinary foreground (`None`) where it already clears the floor, else
/// a plain readable neutral of the winning polarity.
pub(crate) fn wallpaper_text_tint_color(backdrop: u32, fallback_fg: u32) -> Option<[u8; 3]> {
    use aterm_effects::color_math::{hsv2rgb, relative_luminance, rgb2hsv};
    let ground = relative_luminance(backdrop);
    let light_ink = ground <= LIGHT_INK_BELOW;
    let (hue, s, _v) = rgb2hsv(backdrop);
    if s < 0.10 {
        if wcag_contrast(relative_luminance(fallback_fg), ground) >= WALLPAPER_TINT_CONTRAST_FLOOR {
            return None;
        }
        let neutral = if light_ink { 0xF0 } else { 0x20 };
        return Some([neutral, neutral, neutral]);
    }
    let mut sat = (s * 0.9).clamp(0.35, 0.70);
    let mut v = if light_ink { 0.95 } else { 0.34 };
    let mut tinted = hsv2rgb(hue, sat, v);
    // Saturated hues can sit dark even at full brightness (pure blue) or
    // light even quite low (yellow), so push brightness toward the pole,
    // then bleed saturation out — sat 0 at the pole is white/black, whose
    // contrast clears the floor on any ground the polarity choice matched.
    for _ in 0..24 {
        if wcag_contrast(relative_luminance(tinted), ground) >= WALLPAPER_TINT_CONTRAST_FLOOR {
            break;
        }
        if light_ink && v < 1.0 {
            v = (v + 0.06).min(1.0);
        } else if !light_ink && v > 0.0 {
            v = (v - 0.06).max(0.0);
        } else {
            sat = (sat - 0.08).max(0.0);
        }
        tinted = hsv2rgb(hue, sat, v);
    }
    Some([
        ((tinted >> 16) & 0xff) as u8,
        ((tinted >> 8) & 0xff) as u8,
        (tinted & 0xff) as u8,
    ])
}

/// Cover-scale `src` (straight sRGB RGBA8, `sw × sh`) to exactly `dw × dh`
/// (center-crop to the destination aspect, then the shared linear-light
/// `resample_rgba`), composite any translucency over the theme background,
/// tone the result toward it by `dim`, and force every texel opaque — a
/// wallpaper replaces the background-opacity glass, never composites with it.
pub(crate) fn scale_wallpaper_cover(
    src: &[u8],
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
    dim: f32,
    bg: u32,
) -> Vec<u8> {
    let scale = (dw as f64 / sw as f64).max(dh as f64 / sh as f64);
    let crop_w = ((dw as f64 / scale).round() as usize).clamp(1, sw);
    let crop_h = ((dh as f64 / scale).round() as usize).clamp(1, sh);
    let x0 = (sw - crop_w) / 2;
    let y0 = (sh - crop_h) / 2;
    let mut crop = Vec::with_capacity(crop_w * crop_h * 4);
    for y in y0..y0 + crop_h {
        let start = (y * sw + x0) * 4;
        crop.extend_from_slice(&src[start..start + crop_w * 4]);
    }
    let mut out = aterm_render::resample_rgba(&crop, crop_w, crop_h, dw, dh);
    let dim = dim.clamp(0.0, 1.0);
    let bg_rgb = [
        ((bg >> 16) & 0xff) as f32,
        ((bg >> 8) & 0xff) as f32,
        (bg & 0xff) as f32,
    ];
    for px in out.as_chunks_mut::<4>().0 {
        let a = f32::from(px[3]) / 255.0;
        for (i, ground) in bg_rgb.iter().enumerate() {
            let composed = f32::from(px[i]) * a + ground * (1.0 - a);
            let toned = composed * (1.0 - dim) + ground * dim;
            px[i] = toned.round().clamp(0.0, 255.0) as u8;
        }
        px[3] = 255;
    }
    out
}

#[cfg(test)]
mod wallpaper_tint_ink_tests {
    use super::merge_wallpaper_tint_ink;
    use aterm_core::render::InkCell;
    use aterm_core::terminal::{RenderCell, UnderlineStyle};

    fn cell(ch: char, fg: [u8; 3], wide: bool) -> RenderCell {
        RenderCell {
            ch,
            fg,
            bg: [0, 0, 0],
            wide,
            emoji_presentation: false,
            text_presentation: false,
            bold: false,
            italic: false,
            underline: UnderlineStyle::None,
            strikethrough: false,
            overline: false,
            underline_color: None,
            overline_color: None,
            script: aterm_core::terminal::Script::Normal,
        }
    }

    fn tuples(ink: &[InkCell]) -> Vec<(u16, u16, [u8; 3])> {
        ink.iter()
            .map(|entry| (entry.row, entry.col, entry.color))
            .collect()
    }

    #[test]
    fn merge_preserves_precedence_order() {
        let default_fg = [200, 200, 200];
        let cells = vec![
            vec![
                cell('a', default_fg, false),
                cell('b', default_fg, false),
                cell(' ', default_fg, false),
                cell('c', default_fg, true),
            ],
            vec![
                cell('d', default_fg, false),
                cell('e', [1, 2, 3], false),
                cell('f', default_fg, false),
                cell('g', default_fg, false),
            ],
        ];
        let tint = vec![
            Some([1, 1, 1]),
            Some([2, 2, 2]),
            Some([3, 3, 3]),
            Some([4, 4, 4]),
            Some([5, 5, 5]),
            Some([6, 6, 6]),
            None,
            Some([7, 7, 7]),
        ];
        let producers = vec![
            InkCell {
                row: 0,
                col: 1,
                color: [90, 90, 90],
            },
            InkCell {
                row: 1,
                col: 0,
                color: [80, 80, 80],
            },
            InkCell {
                row: 2,
                col: 0,
                color: [70, 70, 70],
            },
        ];
        let expected = vec![
            (0, 0, [1, 1, 1]),
            (0, 1, [90, 90, 90]),
            (1, 0, [80, 80, 80]),
            (1, 3, [7, 7, 7]),
            (2, 0, [70, 70, 70]),
        ];
        let mut ink = producers;
        let mut scratch = Vec::new();
        merge_wallpaper_tint_ink(&cells, 2, 4, default_fg, &tint, &mut ink, &mut scratch);
        assert_eq!(tuples(&ink), expected);
    }
}
