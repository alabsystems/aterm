// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `robi_gallery` — Robi-roster contact sheets for human / visual-LLM QA, the
//! robot twin of `dog_gallery`. Bakes every authored pose through the REAL bake
//! path ([`aterm_effects::robi_baker::bake_pose`]) on a light and a dark
//! ground, at reference and terminal-small art heights.
//!
//! ```text
//! cargo run -q -p aterm-effects --example robi_gallery -- [out_dir]
//! (default out_dir: target/robi_gallery)
//! ```

use aterm_effects::robi_baker::{RobiBaker, bake_pose};
use aterm_effects::robi_glyphs_gen::{ROBI_GLYPH_IDS, ROBI_GLYPHS};
use aterm_scene::Tile;

const DARK_BG: [u8; 3] = [0x1A, 0x1B, 0x26];
const LIGHT_BG: [u8; 3] = [0xFA, 0xFA, 0xF4];

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/robi_gallery".to_string());
    std::fs::create_dir_all(&dir).expect("create out dir");
    for (tag, bg) in [("light", LIGHT_BG), ("dark", DARK_BG)] {
        page_poses(&dir, tag, bg, 120, "poses");
        page_poses(&dir, tag, bg, 34, "poses_small");
    }
    println!("robi_gallery: wrote sheets to {dir}");
}

/// All poses at one art height (the ladder segment bakes at its own aspect).
fn page_poses(dir: &str, tag: &str, bg: [u8; 3], art_h: u32, name: &str) {
    let tiles: Vec<Tile> = ROBI_GLYPH_IDS
        .iter()
        .map(|&id| {
            let w = (art_h as f32 * RobiBaker::aspect(id)).round() as u32;
            bake_pose(id, w, art_h)
        })
        .collect();
    montage(&tiles, 6, bg).save(&format!("{dir}/{name}_{tag}.png"));
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
    let _ = ROBI_GLYPHS; // keep the import obviously roster-tied
    canvas
}
