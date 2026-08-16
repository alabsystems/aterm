// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `pet_gallery` — full-body pet contact sheets, the roster-as-one-animal twin
//! of `kitty_gallery` (which covers the head/cat-art roster).
//!
//! Bakes every authored pose in [`PET_GLYPH_IDS`] through the REAL bake path
//! ([`PetBakeKey::bake`] — the same fill resolver and face LOD
//! `PetBaker::tile` uses) at the SHIP size (the ~1.7-row body on a 20 px
//! cell) and at 2×, side by side, on a light AND a dark ground. This is the
//! sheet a "does the cat look right?" review actually needs: every pose, the
//! size users see, both grounds, one page.
//!
//! ```text
//! cargo run -q -p aterm-effects --example pet_gallery -- [out_dir]
//! (default out_dir: target/pet_gallery)
//! ```

use std::path::Path;

use aterm_effects::cat_baker::CatColorKey;
use aterm_effects::pet_baker::{PetBakeKey, PetBaker};
use aterm_effects::pet_glyphs_gen::PET_GLYPH_IDS;
use aterm_scene::Tile;

/// Tokyo-Night-ish dark ground / warm paper light ground (kitty_gallery's).
const DARK_BG: [u8; 3] = [0x1A, 0x1B, 0x26];
const LIGHT_BG: [u8; 3] = [0xFA, 0xFA, 0xF4];
const DARK_FG: [u8; 3] = [0xC0, 0xCA, 0xF5];
const LIGHT_FG: [u8; 3] = [0x24, 0x29, 0x2F];

/// Ship art height: the resident pet's ~1.70 rows on a 20 px cell.
const SHIP_H: u32 = 34;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/pet_gallery".into());
    std::fs::create_dir_all(&out).expect("create out dir");

    for &(tag, bg, fg) in &[("light", LIGHT_BG, LIGHT_FG), ("dark", DARK_BG, DARK_FG)] {
        let dark = tag == "dark";
        let items: Vec<Cell> = PET_GLYPH_IDS
            .iter()
            .map(|&pose| {
                let ship = bake(pose, dark, SHIP_H);
                let big = bake(pose, dark, SHIP_H * 2);
                Cell {
                    tile: hstack(&[&ship, &big], bg),
                    label: format!("{pose:?}"),
                }
            })
            .collect();
        sheet(
            &format!("PET POSES  ({tag})  {SHIP_H}/{} PX  ONE ANIMAL", SHIP_H * 2),
            &items,
            5,
            bg,
            fg,
        )
        .save(&format!("{out}/poses_{tag}.png"));
    }
    println!("pet gallery written to {out}/ (poses_light.png, poses_dark.png)");
}

/// Bake one pose through the real path at art height `h`, width from the
/// pose's own aspect, neutral context, the gallery's reference coat/iris.
fn bake(pose: aterm_effects::pet_glyphs_gen::PetGlyphId, dark: bool, h: u32) -> Tile {
    let w = ((h as f32) * PetBaker::aspect(pose)).round().max(1.0) as u32;
    let key = PetBakeKey {
        pose,
        coat: 8,
        iris: 4,
        colors: CatColorKey {
            accent: 12, // neutral
            background: if dark { 0 } else { 3 },
        },
        w: w.min(u32::from(u16::MAX)) as u16,
        h: h.min(u32::from(u16::MAX)) as u16,
    };
    key.bake()
}

// ─── canvas (the kitty_gallery montage kit, minus the patch strip the pet
//     tiles never carry) ────────────────────────────────────────────────────

struct Canvas {
    w: usize,
    h: usize,
    rgb: Vec<u8>,
}

struct Cell {
    tile: Tile,
    label: String,
}

impl Canvas {
    fn new(w: usize, h: usize, bg: [u8; 3]) -> Self {
        let mut rgb = vec![0u8; w * h * 3];
        for px in rgb.as_chunks_mut::<3>().0 {
            px.copy_from_slice(&bg);
        }
        Self { w, h, rgb }
    }

    fn put(&mut self, x: usize, y: usize, c: [u8; 3]) {
        if x < self.w && y < self.h {
            let i = (y * self.w + x) * 3;
            self.rgb[i..i + 3].copy_from_slice(&c);
        }
    }

    fn blit(&mut self, tile: &Tile, x0: usize, y0: usize) {
        let (tw, th) = (tile.width() as usize, tile.height() as usize);
        let src = tile.pixels();
        for ty in 0..th {
            for tx in 0..tw {
                let s = (ty * tw + tx) * 4;
                let a = src[s + 3] as f32 / 255.0;
                if a <= 0.0 {
                    continue;
                }
                let (dx, dy) = (x0 + tx, y0 + ty);
                if dx >= self.w || dy >= self.h {
                    continue;
                }
                let di = (dy * self.w + dx) * 3;
                for c in 0..3 {
                    let fg = src[s + c] as f32;
                    let bgc = self.rgb[di + c] as f32;
                    self.rgb[di + c] = (fg * a + bgc * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }

    fn text(&mut self, x0: usize, y0: usize, s: &str, scale: usize, c: [u8; 3]) {
        let mut cx = x0;
        for ch in s.chars() {
            let g = font5x7(ch);
            for (row, line) in g.iter().enumerate() {
                for (col, cell) in line.bytes().enumerate() {
                    if cell == b'#' {
                        for sy in 0..scale {
                            for sx in 0..scale {
                                self.put(cx + col * scale + sx, y0 + row * scale + sy, c);
                            }
                        }
                    }
                }
            }
            cx += 6 * scale;
        }
    }

    fn save(&self, path: &str) {
        write_png(Path::new(path), self.w as u32, self.h as u32, &self.rgb).expect("write png");
    }
}

fn sheet(title: &str, items: &[Cell], cols: usize, bg: [u8; 3], fg: [u8; 3]) -> Canvas {
    const MARGIN: usize = 10;
    const LABEL_H: usize = 12;
    const TITLE_H: usize = 26;
    let cell_w = items
        .iter()
        .map(|it| it.tile.width() as usize)
        .max()
        .unwrap_or(1)
        + 2 * MARGIN;
    let cell_h = items
        .iter()
        .map(|it| it.tile.height() as usize)
        .max()
        .unwrap_or(1)
        + LABEL_H
        + 2 * MARGIN;
    let rows = items.len().div_ceil(cols);
    let w = cols * cell_w;
    let h = TITLE_H + rows * cell_h;
    let mut cv = Canvas::new(w, h, bg);
    cv.text(MARGIN, 8, title, 2, fg);
    for (i, it) in items.iter().enumerate() {
        let (r, c) = (i / cols, i % cols);
        let cx = c * cell_w;
        let cy = TITLE_H + r * cell_h;
        let art_w = it.tile.width() as usize;
        let art_h = it.tile.height() as usize;
        let bx = cx + (cell_w.saturating_sub(art_w)) / 2;
        let by = cy + MARGIN;
        cv.blit(&it.tile, bx, by);
        cv.text(cx + MARGIN, cy + MARGIN + art_h + 2, &it.label, 1, fg);
    }
    cv
}

/// Composite tiles horizontally, bottom-aligned, over an opaque ground.
fn hstack(tiles: &[&Tile], bg: [u8; 3]) -> Tile {
    const GAP: u32 = 8;
    let total_w: u32 =
        tiles.iter().map(|t| t.width()).sum::<u32>() + GAP * (tiles.len() as u32).saturating_sub(1);
    let max_h = tiles.iter().map(|t| t.height()).max().unwrap_or(1);
    let mut out = Tile::new(total_w, max_h);
    {
        let px = out.pixels_mut();
        for p in px.as_chunks_mut::<4>().0 {
            p.copy_from_slice(&[bg[0], bg[1], bg[2], 255]);
        }
    }
    let mut x = 0u32;
    for t in tiles {
        let y = max_h - t.height();
        blit_tile_onto(&mut out, t, x, y);
        x += t.width() + GAP;
    }
    out
}

fn blit_tile_onto(dst: &mut Tile, src: &Tile, x0: u32, y0: u32) {
    let (sw, sh) = (src.width(), src.height());
    let (dw, dh) = (dst.width(), dst.height());
    let sp = src.pixels().to_vec();
    let dp = dst.pixels_mut();
    for ty in 0..sh {
        for tx in 0..sw {
            let s = ((ty * sw + tx) * 4) as usize;
            let a = sp[s + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            let (dx, dy) = (x0 + tx, y0 + ty);
            if dx >= dw || dy >= dh {
                continue;
            }
            let di = ((dy * dw + dx) * 4) as usize;
            for c in 0..3 {
                let fg = sp[s + c] as f32;
                let bgc = dp[di + c] as f32;
                dp[di + c] = (fg * a + bgc * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
            }
            dp[di + 3] = 255;
        }
    }
}

fn write_png(path: &Path, w: u32, h: u32, rgb: &[u8]) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?
        .write_image_data(rgb)
        .map_err(std::io::Error::other)
}

/// Compact 5×7 bitmap font — the subset the pose labels need.
fn font5x7(c: char) -> [&'static str; 7] {
    match c.to_ascii_uppercase() {
        'A' => [
            ".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
        'B' => [
            "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####.",
        ],
        'C' => [
            ".###.", "#...#", "#....", "#....", "#....", "#...#", ".###.",
        ],
        'D' => [
            "####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####.",
        ],
        'E' => [
            "#####", "#....", "#....", "####.", "#....", "#....", "#####",
        ],
        'F' => [
            "#####", "#....", "#....", "####.", "#....", "#....", "#....",
        ],
        'G' => [
            ".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".###.",
        ],
        'H' => [
            "#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
        'I' => [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####",
        ],
        'K' => [
            "#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#",
        ],
        'L' => [
            "#....", "#....", "#....", "#....", "#....", "#....", "#####",
        ],
        'M' => [
            "#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#",
        ],
        'N' => [
            "#...#", "##..#", "#.#.#", "#.#.#", "#..##", "#...#", "#...#",
        ],
        'O' => [
            ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
        'P' => [
            "####.", "#...#", "#...#", "####.", "#....", "#....", "#....",
        ],
        'R' => [
            "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
        ],
        'S' => [
            ".####", "#....", "#....", ".###.", "....#", "....#", "####.",
        ],
        'T' => [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#..",
        ],
        'U' => [
            "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
        'V' => [
            "#...#", "#...#", "#...#", "#...#", ".#.#.", ".#.#.", "..#..",
        ],
        'W' => [
            "#...#", "#...#", "#...#", "#.#.#", "#.#.#", "##.##", "#...#",
        ],
        'Y' => [
            "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..",
        ],
        'Z' => [
            "#####", "....#", "...#.", "..#..", ".#...", "#....", "#####",
        ],
        '0' => [
            ".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###.",
        ],
        '1' => [
            "..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###.",
        ],
        '2' => [
            ".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####",
        ],
        '3' => [
            "#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###.",
        ],
        '4' => [
            "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.",
        ],
        '6' => [
            ".###.", "#....", "#....", "####.", "#...#", "#...#", ".###.",
        ],
        '7' => [
            "#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#...",
        ],
        '8' => [
            ".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###.",
        ],
        '/' => [
            "....#", "....#", "...#.", "..#..", ".#...", "#....", "#....",
        ],
        _ => [
            ".....", ".....", ".....", ".....", ".....", ".....", ".....",
        ],
    }
}
