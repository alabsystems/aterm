// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `dog_gallery` — dog-roster contact sheets for human / visual-LLM QA, the
//! breed twin of `kitty_gallery`. Bakes every authored breed through the REAL
//! bake path ([`aterm_effects::dog_baker::bake_breed`]) on a light and a dark
//! ground, at reference and terminal-small art heights, plus one coat sweep.
//!
//! ```text
//! cargo run -q -p aterm-effects --example dog_gallery -- [out_dir]
//! (default out_dir: target/dog_gallery)
//! ```

use aterm_effects::cat_baker::ResolvedFills;
use aterm_effects::dog_baker::{DogBaker, bake_breed};
use aterm_effects::dog_glyphs_gen::{DOG_GLYPHS, DOG_HEADS};
use aterm_scene::Tile;

const DARK_BG: [u8; 3] = [0x1A, 0x1B, 0x26];
const LIGHT_BG: [u8; 3] = [0xFA, 0xFA, 0xF4];

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/dog_gallery".to_string());
    std::fs::create_dir_all(&dir).expect("create out dir");
    for (tag, bg, dark) in [("light", LIGHT_BG, false), ("dark", DARK_BG, true)] {
        page_heads(&dir, tag, bg, dark, 96, "heads");
        page_heads(&dir, tag, bg, dark, 26, "heads_small");
        page_coats(&dir, tag, bg, dark);
    }
    println!("dog_gallery: wrote sheets to {dir}");
}

/// All breeds at one art height, roster coats sweeping so each breed shows a
/// different ramp stop.
fn page_heads(dir: &str, tag: &str, bg: [u8; 3], dark: bool, art_h: u32, name: &str) {
    let tiles: Vec<Tile> = DOG_HEADS
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            let coat = ((i * 5 + 3) % 16) as u8;
            let w = (art_h as f32 * DogBaker::aspect(id)).round() as u32;
            bake_breed(id, &ResolvedFills::from_indices(coat, 2, dark), w, art_h)
        })
        .collect();
    montage(&tiles, 5, bg).save(&format!("{dir}/{name}_{tag}.png"));
}

/// One breed swept across the whole 16-stop coat ramp.
fn page_coats(dir: &str, tag: &str, bg: [u8; 3], dark: bool) {
    let id = DOG_HEADS[0];
    let art_h = 72u32;
    let w = (art_h as f32 * DogBaker::aspect(id)).round() as u32;
    let tiles: Vec<Tile> = (0u8..16)
        .map(|coat| bake_breed(id, &ResolvedFills::from_indices(coat, 2, dark), w, art_h))
        .collect();
    montage(&tiles, 8, bg).save(&format!("{dir}/coats_{tag}.png"));
}

struct Canvas {
    w: usize,
    h: usize,
    rgb: Vec<u8>,
}

impl Canvas {
    fn new(w: usize, h: usize, bg: [u8; 3]) -> Self {
        let mut rgb = Vec::with_capacity(w * h * 3);
        for _ in 0..w * h {
            rgb.extend_from_slice(&bg);
        }
        Self { w, h, rgb }
    }

    fn blit(&mut self, tile: &Tile, x0: usize, y0: usize) {
        let (tw, th) = (tile.width() as usize, tile.height() as usize);
        let px = tile.pixels();
        for y in 0..th {
            for x in 0..tw {
                let i = (y * tw + x) * 4;
                let a = u32::from(px[i + 3]);
                if a == 0 {
                    continue;
                }
                let (cx, cy) = (x0 + x, y0 + y);
                if cx >= self.w || cy >= self.h {
                    continue;
                }
                let o = (cy * self.w + cx) * 3;
                for c in 0..3 {
                    let src = u32::from(px[i + c]);
                    let dst = u32::from(self.rgb[o + c]);
                    self.rgb[o + c] = ((src * a + dst * (255 - a)) / 255) as u8;
                }
            }
        }
    }

    fn save(&self, path: &str) {
        let file = std::fs::File::create(path).expect("create png");
        let mut enc =
            png::Encoder::new(std::io::BufWriter::new(file), self.w as u32, self.h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .expect("png header")
            .write_image_data(&self.rgb)
            .expect("png data");
    }
}

fn montage(tiles: &[Tile], cols: usize, bg: [u8; 3]) -> Canvas {
    let pad = 12usize;
    let cell_w = tiles.iter().map(|t| t.width() as usize).max().unwrap_or(1) + pad;
    let cell_h = tiles.iter().map(|t| t.height() as usize).max().unwrap_or(1) + pad;
    let rows = tiles.len().div_ceil(cols);
    let mut canvas = Canvas::new(cols * cell_w + pad, rows * cell_h + pad, bg);
    for (i, t) in tiles.iter().enumerate() {
        let (r, c) = (i / cols, i % cols);
        canvas.blit(
            t,
            pad + c * cell_w + (cell_w - pad - t.width() as usize) / 2,
            pad + r * cell_h + (cell_h - pad - t.height() as usize) / 2,
        );
    }
    let _ = DOG_GLYPHS; // keep the import obviously roster-tied
    canvas
}
