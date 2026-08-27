// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Visual demo of the PHASER EMITTER cursor ([`aterm_effects::cursor_phaser`]).
//! Ticks the real animator at several charge levels / beam hues and composites
//! its premultiplied additive quads over a synthetic terminal row (font8x8
//! glyphs, themed block cursor filled with the frame's beam-hued fill) to PNGs,
//! so the wings, prism fringe, and text legibility are all inspectable frame by
//! frame.
//!
//!   cargo run -p aterm-effects --example phaser_core_demo -- <out_dir>

use std::time::Duration;

use aterm_time::Instant;

use aterm_effects::cursor_glow::Geom;
use aterm_effects::cursor_phaser::{CursorPhaser, PhaserConfig};

const CW: usize = 14;
const CH: usize = 28;
const COLS: usize = 34;
const ROWS: usize = 7;
const BG: [u8; 3] = [0x10, 0x10, 0x16];
const FG: [u8; 3] = [0xC8, 0xC8, 0xD2];

fn glyph_bitmap(c: char) -> [u8; 8] {
    let i = c as usize;
    if i < 128 {
        font8x8::legacy::BASIC_LEGACY[i].map(|row| row)
    } else {
        [0; 8]
    }
}

/// Draw one 8x8 glyph scaled into a cell (nearest-neighbour), straight into RGB.
fn draw_glyph(img: &mut [u8], w: usize, cell_x: usize, cell_y: usize, c: char, fg: [u8; 3]) {
    let bm = glyph_bitmap(c);
    // Leave a small inset so the glyph sits like a real raster.
    let (gx, gy, gw, gh) = (cell_x + 2, cell_y + 4, CW - 4, CH - 8);
    for py in 0..gh {
        let sy = py * 8 / gh;
        for px in 0..gw {
            let sx = px * 8 / gw;
            if bm[sy] & (1 << sx) != 0 {
                let d = ((gy + py) * w + gx + px) * 3;
                img[d..d + 3].copy_from_slice(&fg);
            }
        }
    }
}

fn fill_rect(img: &mut [u8], w: usize, x: usize, y: usize, rw: usize, rh: usize, rgb: [u8; 3]) {
    for py in y..y + rh {
        for px in x..x + rw {
            let d = (py * w + px) * 3;
            img[d..d + 3].copy_from_slice(&rgb);
        }
    }
}

fn write_png(path: &str, img: &[u8], w: usize, h: usize) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(img).unwrap();
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let geom = Geom {
        cw: CW,
        ch: CH,
        rows: ROWS,
        cols: COLS,
        origin_x: 0,
        origin_y: 0,
        win_w: (COLS * CW) as u16,
        win_h: (ROWS * CH) as u16,
        head: 0,
    };
    let (w, h) = (COLS * CW, ROWS * CH);
    let cfg = PhaserConfig {
        enabled: true,
        intensity: 1.0,
        // No host cursor colour in a raw demo: the theme-polar base stands in,
        // exactly as it does for an embedder that resolves no cursor colour.
        base: None,
    };
    let text = "the quick brown fox phases out";
    let cursor_row = 3u16;
    let cursor_col = text.len() as u16; // cursor right after the typed text

    for (ei, energy) in [0.0f32, 0.35, 0.70, 1.0].iter().enumerate() {
        for (hi, hue) in [0.0f32, 0.33, 0.66].iter().enumerate() {
            let mut cp = CursorPhaser::default();
            let mut quads = Vec::new();
            let t0 = Instant::now();
            // Seed, then advance one frame (deterministic per run).
            cp.tick(
                Some((cursor_row, cursor_col)),
                t0,
                *hue,
                *energy,
                true,
                geom,
                &cfg,
                &mut quads,
            );
            quads.clear();
            let t = t0 + Duration::from_millis(16);
            let frame = cp.tick(
                Some((cursor_row, cursor_col)),
                t,
                *hue,
                *energy,
                true,
                geom,
                &cfg,
                &mut quads,
            );

            // ---- composite ----
            let mut img = vec![0u8; w * h * 3];
            for px in img.as_chunks_mut::<3>().0 {
                px.copy_from_slice(&BG);
            }
            // The typed row + a context row above/below so wash-out is visible.
            for (row, line) in [
                (cursor_row as usize - 1, "context above the charged line"),
                (cursor_row as usize, text),
                (cursor_row as usize + 1, "and context directly underneath"),
            ] {
                for (i, c) in line.chars().enumerate().take(COLS) {
                    draw_glyph(&mut img, w, i * CW, row * CH, c, FG);
                }
            }
            // The block cursor body, filled with the frame's beam-hued fill.
            if let Some(fill) = frame.fill {
                let rgb = [(fill >> 16) as u8, (fill >> 8) as u8, fill as u8];
                fill_rect(
                    &mut img,
                    w,
                    cursor_col as usize * CW,
                    cursor_row as usize * CH,
                    CW,
                    CH,
                    rgb,
                );
            }
            // Additive premultiplied light on top.
            for q in &quads {
                for py in q.y as usize..(q.y + q.h) as usize {
                    for px in q.x as usize..(q.x + q.w) as usize {
                        let d = (py * w + px) * 3;
                        img[d] = img[d].saturating_add((q.color >> 16) as u8);
                        img[d + 1] = img[d + 1].saturating_add((q.color >> 8) as u8);
                        img[d + 2] = img[d + 2].saturating_add(q.color as u8);
                    }
                }
            }

            let path = format!("{dir}/phaser_e{ei}_h{hi}.png");
            write_png(&path, &img, w, h);
            // A 4× zoom crop around the cursor so the wing/fringe detail is
            // inspectable at real cell geometry.
            let zx0 = (cursor_col as usize).saturating_sub(3) * CW;
            let zy0 = (cursor_row as usize).saturating_sub(2) * CH;
            let (zw, zh) = (7 * CW, 4 * CH);
            let mut zoom = vec![0u8; zw * 4 * zh * 4 * 3];
            for py in 0..zh * 4 {
                for px in 0..zw * 4 {
                    let s = ((zy0 + py / 4) * w + zx0 + px / 4) * 3;
                    let d = (py * zw * 4 + px) * 3;
                    zoom[d..d + 3].copy_from_slice(&img[s..s + 3]);
                }
            }
            let zpath = format!("{dir}/phaser_e{ei}_h{hi}_zoom.png");
            write_png(&zpath, &zoom, zw * 4, zh * 4);
            println!(
                "wrote {path} (energy={energy}, hue={hue}, {} quads)",
                quads.len()
            );
        }
    }
}
