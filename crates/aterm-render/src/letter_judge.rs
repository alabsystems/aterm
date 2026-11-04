// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Settings card's letter judge, for aterm-gui's tests: it lives here because this crate
//! builds optimized in the dev profile, where the judge's loops run for every frame of every look.
//! Debug and test builds only.

use aterm_core::render::RenderInput;

/// The red, green and blue channels of `0xRRGGBB`.
const fn channels(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// The low 24 bits of a frame pixel: its `0xRRGGBB` colour.
const RGB: u32 = 0x00FF_FFFF;

/// WCAG 2's linear value of each sRGB channel value, the table [`luma`] reads.
fn linear_channels() -> [f32; 256] {
    std::array::from_fn(|v| {
        let c = f32::from(u8::try_from(v).expect("a channel value")) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    })
}

/// WCAG 2 relative luminance of the `0xRRGGBB` colour `rgb`, its channels read in `lin`
/// ([`linear_channels`]).
fn luma(lin: &[f32; 256], rgb: u32) -> f32 {
    let [r, g, b] = channels(rgb);
    0.2126 * lin[usize::from(r)] + 0.7152 * lin[usize::from(g)] + 0.0722 * lin[usize::from(b)]
}

/// WCAG 2 contrast of two colours of relative luminance `la` and `lb`.
fn wcag(la: f32, lb: f32) -> f32 {
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Each channel's median of the `0xRRGGBB` colours `px` (the upper one of an even count:
/// each channel's `len / 2`th smallest value).
fn median_rgb(px: &[u32]) -> Option<u32> {
    if px.is_empty() {
        return None;
    }
    let rank = px.len() / 2;
    let mut median = 0;
    for shift in [16, 8, 0] {
        let channel = |i: usize| ((px[i] >> shift) & 0xff) as u8;
        let value = if px.len() <= 16 {
            // A ring's few: sorted in place.
            let mut sorted = [0_u8; 16];
            for i in 0..px.len() {
                let v = channel(i);
                let mut j = i;
                while j > 0 && sorted[j - 1] > v {
                    sorted[j] = sorted[j - 1];
                    j -= 1;
                }
                sorted[j] = v;
            }
            sorted[rank]
        } else {
            // Each value's count, then the value the count passes `rank` at.
            let mut counts = [0_usize; 256];
            for i in 0..px.len() {
                counts[usize::from(channel(i))] += 1;
            }
            let (mut value, mut seen) = (0, 0);
            loop {
                seen += counts[usize::from(value)];
                if seen > rank {
                    break value;
                }
                value += 1;
            }
        };
        median |= u32::from(value) << shift;
    }
    Some(median)
}

/// A cell's ground: the most common of its `0xRRGGBB` colours `px` (row by row), the first
/// seen of a tie, as the trail lab's `trail-contrast` takes a glyph's ground (its `mode`);
/// `None` for no pixels. `counts` is scratch: each colour and its count, in the order first
/// seen.
fn cell_ground(px: &[u32], counts: &mut Vec<(u32, usize)>) -> Option<u32> {
    counts.clear();
    let mut i = 0;
    while i < px.len() {
        let mut at = 0;
        while at < counts.len() && counts[at].0 != px[i] {
            at += 1;
        }
        if at == counts.len() {
            counts.push((px[i], 1));
        } else {
            counts[at].1 += 1;
        }
        i += 1;
    }
    let &(mut ground, mut most) = counts.first()?;
    for &(rgb, n) in counts.iter() {
        if n > most {
            (ground, most) = (rgb, n);
        }
    }
    Some(ground)
}

/// What every lettered cell the scene's lights or companion changed reads, as the trail
/// lab's `trail-contrast` judges a glyph against its clean twin (`twin`, the card with no
/// light and no companion): its local contrast, what a reader sees round each stroke (each
/// solid ink pixel against the median of the nearest ring, 1 then 2 px out, of pixels that
/// are pure ground in the twin; the worst pixel row's lower median), the one reading the
/// letter guard keeps; its whole-cell contrast (the median of its core ink against the median
/// of its pure ground, as the frame has them) where no solid pixel has ground that near.
/// `(reading, bare, row, col)` for each such letter, `bare` the same glyph read on the twin.
/// A cell's ground is its most common colour in the twin, the first seen of a tie
/// ([`cell_ground`]).
///
/// It runs on every frame of the card's every look, so it lives in this crate, which the dev
/// profile optimizes: each cell's pixels are read into buffers once, each colour's luminance off
/// a table, and the loops index. A letter here is any cell that is neither blank nor the caret:
/// punctuation is read too.
pub fn judged_letters(
    lit: &crate::Frame,
    twin: &crate::Frame,
    input: &RenderInput,
    (cell_w, cell_h): (usize, usize),
) -> Vec<(f32, f32, usize, usize)> {
    /// A glyph's core: this share of the way from the clean ground to its extreme.
    const CORE_SHARE: f32 = 0.70;
    /// A glyph's solid ink: this share of its extreme's ratio against the ground.
    const SOLID_SHARE: f32 = 0.85;

    let lin = linear_channels();
    let near = |a: u32, b: u32| {
        let ([ar, ag, ab], [br, bg, bb]) = (channels(a), channels(b));
        ar.abs_diff(br) <= 12 && ag.abs_diff(bg) <= 12 && ab.abs_diff(bb) <= 12
    };
    let n = cell_w * cell_h;
    // The cell's pixels row by row, in the twin and the lit frame (`0xRRGGBB`), and of
    // the twin's: each one's luminance and contrast against the ground, and the core's.
    let (mut clean, mut shown) = (Vec::with_capacity(n), Vec::with_capacity(n));
    let (mut lum, mut against, mut core) = (vec![0.0; n], vec![0.0; n], vec![false; n]);
    let mut counts: Vec<(u32, usize)> = Vec::new();
    let mut out = Vec::new();
    for (row, cells) in input.cells.iter().enumerate() {
        for (col, cell) in cells.iter().enumerate() {
            let (x0, y0) = (col * cell_w, row * cell_h);
            if matches!(cell.ch, ' ' | '\0')
                || (row, col) == (input.cursor_row, input.cursor_col)
                || x0 + cell_w > lit.width
                || y0 + cell_h > lit.height
            {
                continue;
            }
            clean.clear();
            shown.clear();
            for y in y0..y0 + cell_h {
                clean.extend_from_slice(&twin.pixels[y * twin.width + x0..][..cell_w]);
                shown.extend_from_slice(&lit.pixels[y * lit.width + x0..][..cell_w]);
            }
            if shown == clean {
                continue;
            }
            let mut changed = false;
            let mut i = 0;
            while i < n {
                clean[i] &= RGB;
                shown[i] &= RGB;
                changed = changed || !near(shown[i], clean[i]);
                i += 1;
            }
            if !changed {
                continue;
            }
            let Some(ground) = cell_ground(&clean, &mut counts) else {
                continue;
            };
            let ground_luma = luma(&lin, ground);
            // The extreme: the last pixel no other passes in contrast against the ground.
            let mut extreme = 0;
            let mut i = 0;
            while i < n {
                lum[i] = luma(&lin, clean[i]);
                against[i] = wcag(lum[i], ground_luma);
                if against[i].total_cmp(&against[extreme]).is_ge() {
                    extreme = i;
                }
                i += 1;
            }
            if against[extreme] < 1.5 {
                continue;
            }
            let reach = CORE_SHARE * (lum[extreme] - ground_luma).abs();
            let mut i = 0;
            while i < n {
                core[i] = clean[i] != ground && (lum[i] - ground_luma).abs() >= reach;
                i += 1;
            }
            let solid_bar = SOLID_SHARE * against[extreme];
            let solid = |i: usize| against[i] >= solid_bar;
            let cored = |i: usize| core[i];
            // What the cell reads in `frame` (`px`, its pixels there), by the twin's ground,
            // core and solid ink.
            let read = |frame: &crate::Frame, px: &[u32]| {
                let (mut ink, mut bed) = (Vec::new(), Vec::new());
                for i in 0..n {
                    if clean[i] == ground {
                        bed.push(px[i]);
                    } else if core[i] {
                        ink.push(px[i]);
                    }
                }
                let (ink, bed) = (median_rgb(&ink)?, median_rgb(&bed)?);
                let whole = wcag(luma(&lin, ink), luma(&lin, bed));
                let (w, h) = (frame.width as i32, frame.height as i32);
                let ring_ground = |x: usize, y: usize| {
                    let mut ring = [0_u32; 16];
                    for off in 1..=2_i32 {
                        let mut len = 0;
                        let mut dy = -off;
                        while dy <= off {
                            let mut dx = -off;
                            while dx <= off {
                                let (gx, gy) = (x as i32 + dx, y as i32 + dy);
                                if (dx.abs() == off || dy.abs() == off)
                                    && gx >= 0
                                    && gy >= 0
                                    && gx < w
                                    && gy < h
                                {
                                    let (gx, gy) = (gx as usize, gy as usize);
                                    if twin.pixels[gy * twin.width + gx] & RGB == ground {
                                        ring[len] = frame.pixels[gy * frame.width + gx] & RGB;
                                        len += 1;
                                    }
                                }
                                dx += 1;
                            }
                            dy += 1;
                        }
                        if let Some(median) = median_rgb(&ring[..len]) {
                            return Some(median);
                        }
                    }
                    None
                };
                let local_of = |solid: &dyn Fn(usize) -> bool| {
                    let mut least: Option<f32> = None;
                    let mut readings = Vec::with_capacity(cell_w);
                    for dy in 0..cell_h {
                        readings.clear();
                        for dx in 0..cell_w {
                            let i = dy * cell_w + dx;
                            if solid(i)
                                && let Some(g) = ring_ground(x0 + dx, y0 + dy)
                            {
                                readings.push(wcag(luma(&lin, px[i]), luma(&lin, g)));
                            }
                        }
                        if readings.is_empty() {
                            continue;
                        }
                        readings.sort_by(f32::total_cmp);
                        let reading = readings[(readings.len() - 1) / 2];
                        least = Some(least.map_or(reading, |least| least.min(reading)));
                    }
                    least
                };
                let local = local_of(&solid).or_else(|| local_of(&cored));
                // The ring round the strokes, what a reader sees round each one (the letter
                // guard's one model, `letter_guard::ring`): the whole cell where no solid
                // pixel has ground that near.
                Some(local.unwrap_or(whole))
            };
            if let (Some(reading), Some(bare)) = (read(lit, &shown), read(twin, &clean)) {
                out.push((reading, bare, row, col));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::cell_ground;

    /// A cell's ground is its commonest colour, and of two as common the first seen, as
    /// `trail-contrast` takes one: neither the lower colour nor the one first seen later.
    #[test]
    fn a_cells_ground_is_its_commonest_colour_the_first_seen_of_a_tie() {
        let (dark, light) = (0x10_1010, 0xF0_F0F0);
        let mut counts = Vec::new();
        assert_eq!(
            cell_ground(&[light, dark, dark, light], &mut counts),
            Some(light)
        );
        assert_eq!(
            cell_ground(&[dark, light, light, dark], &mut counts),
            Some(dark)
        );
        assert_eq!(
            cell_ground(&[dark, light, light, light, dark], &mut counts),
            Some(light)
        );
        assert_eq!(cell_ground(&[], &mut counts), None);
    }
}
