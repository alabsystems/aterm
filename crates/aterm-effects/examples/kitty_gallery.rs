// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `kitty_gallery` — cat-art v4 authored-character contact sheets (design §6).
//!
//! The v2/v3 procedural gallery is retired with the procedural cats. This tool
//! bakes every roster glyph through the REAL v4 bake path
//! ([`aterm_effects::cat_baker::bake_variant_with`] — the same fill resolver,
//! catch-light patch strip and ramps `CatBaker::get_v4` uses) and composites
//! labelled montages for human / visual-LLM QA against the reference sheets.
//! Pages (each on a light AND a dark ground, so the dark-bg outline lift is
//! judged):
//!
//!   * `heads_{light,dark}.png` — all 25 [`HEADS`] at the reference art height.
//!   * `heads_small_{light,dark}.png` — the same 25 at 16 px and 26 px art
//!     height side by side (the terminal-legibility check: eyes read small).
//!   * `specials_accessories_{light,dark}.png` — the 8 authored specials, then
//!     each of the 3 overlay accessories bare AND seated on a plain head.
//!   * `coats_{light,dark}.png` — one recolorable silhouette (S100) swept
//!     across the 16-stop `COAT_RAMP`, then across the reachable `EYE_RAMP`
//!     iris stops (the genome recolor space on one glyph).
//!   * `context_{light,dark}.png` — the hero head + cursor companion across
//!     all 12 local-text hue families and the neutral context key.
//!
//! Glyph proportions come from the asset viewbox aspect (the const drawlists
//! are normalized to a unit square; the viewbox carries the true aspect), so a
//! wide stretch-cat is not squished to a head's box.
//!
//! ```text
//! cargo run -q -p aterm-effects --example kitty_gallery -- [out_dir]
//! (default out_dir: target/kitty_gallery)
//! ```

use std::path::Path;

use aterm_effects::cat_baker::{
    COAT_RAMP, CatColorKey, EYE_RAMP, EyesFrame, PATCH_STRIP, ResolvedFills, bake_variant_with,
};
use aterm_effects::cat_glyphs_gen::{CatGlyphId, GLYPHS, HEADS};
use aterm_scene::Tile;

// ─────────────────────────── grounds & ink ───────────────────────────

/// Tokyo-Night-ish dark ground (the `capture_kitties.sh` dark theme bg).
const DARK_BG: [u8; 3] = [0x1A, 0x1B, 0x26];
/// Warm paper light ground (the light theme bg).
const LIGHT_BG: [u8; 3] = [0xFA, 0xFA, 0xF4];
/// Pale ink on the dark ground.
const DARK_FG: [u8; 3] = [0xC0, 0xCA, 0xF5];
/// Near-black ink on the light ground.
const LIGHT_FG: [u8; 3] = [0x24, 0x29, 0x2F];

/// The eight authored whole-cat specials, in roster order.
const SPECIALS: [CatGlyphId; 8] = [
    CatGlyphId::SpecFluffy,
    CatGlyphId::SpecManeki,
    CatGlyphId::SpecSleeping,
    CatGlyphId::SpecStretch,
    CatGlyphId::SpecTabbybell,
    CatGlyphId::SpecTuxedo,
    CatGlyphId::SpecWitch,
    CatGlyphId::SpecYarn,
];

/// The three overlay accessories.
const ACCESSORIES: [CatGlyphId; 3] = [
    CatGlyphId::AccBow,
    CatGlyphId::AccCrown,
    CatGlyphId::AccBell,
];

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/kitty_gallery".into());
    std::fs::create_dir_all(&out).expect("create out dir");

    for &(tag, bg, fg) in &[("light", LIGHT_BG, LIGHT_FG), ("dark", DARK_BG, DARK_FG)] {
        let dark = tag == "dark";
        page_heads(&out, tag, bg, fg, dark);
        page_heads_small(&out, tag, bg, fg, dark);
        page_specials_accessories(&out, tag, bg, fg, dark);
        page_coats(&out, tag, bg, fg, dark);
        page_context(&out, tag, bg, fg, dark);
    }
    println!(
        "gallery written to {out}/ (10 sheets: heads/heads_small/specials_accessories/coats/context × light/dark)"
    );
}

// ─────────────────────────── pages ───────────────────────────

/// All 25 heads at the reference art height, 5 across.
fn page_heads(dir: &str, tag: &str, bg: [u8; 3], fg: [u8; 3], dark: bool) {
    const H: u32 = 120;
    let items: Vec<Cell> = HEADS
        .iter()
        .map(|&id| Cell {
            tile: bake(id, None, 8, 4, dark, H),
            label: short(id),
        })
        .collect();
    sheet(
        &format!("HEADS  ({tag})  25 CAT CHARACTERS  coat=8 iris=4"),
        &items,
        5,
        bg,
        fg,
    )
    .save(&format!("{dir}/heads_{tag}.png"));
}

/// The legibility check: every head at 16 px and 26 px art height, paired.
fn page_heads_small(dir: &str, tag: &str, bg: [u8; 3], fg: [u8; 3], dark: bool) {
    let items: Vec<Cell> = HEADS
        .iter()
        .map(|&id| {
            let small = bake(id, None, 8, 4, dark, 16);
            let big = bake(id, None, 8, 4, dark, 26);
            Cell {
                tile: hstack(&[&small, &big], bg, dark),
                label: short(id),
            }
        })
        .collect();
    sheet(&format!("TINY HEADS  {tag}  16/26 PX"), &items, 5, bg, fg)
        .save(&format!("{dir}/heads_small_{tag}.png"));
}

/// The 8 specials, then each accessory bare and seated on a plain head.
fn page_specials_accessories(dir: &str, tag: &str, bg: [u8; 3], fg: [u8; 3], dark: bool) {
    const H: u32 = 120;
    let mut items: Vec<Cell> = SPECIALS
        .iter()
        .map(|&id| Cell {
            tile: bake(id, None, 8, 4, dark, H),
            label: short(id),
        })
        .collect();
    for &acc in &ACCESSORIES {
        items.push(Cell {
            tile: bake(acc, None, 8, 4, dark, H),
            label: short(acc),
        });
        // Seated on a plain head (S100), the emit-path overlay.
        items.push(Cell {
            tile: bake(CatGlyphId::S100, Some(acc), 8, 4, dark, H),
            label: format!("{}+S100", short(acc)),
        });
    }
    sheet(
        &format!("SPECIALS + ACCESSORIES  ({tag})  overlay seated on S100"),
        &items,
        4,
        bg,
        fg,
    )
    .save(&format!("{dir}/specials_accessories_{tag}.png"));
}

/// One recolorable silhouette (S100) across the coat ramp, then the iris ramp.
fn page_coats(dir: &str, tag: &str, bg: [u8; 3], fg: [u8; 3], dark: bool) {
    const H: u32 = 96;
    let head = CatGlyphId::S100;
    let mut items: Vec<Cell> = (0..COAT_RAMP.len() as u8)
        .map(|c| Cell {
            tile: bake(head, None, c, 4, dark, H),
            label: format!("COAT {c:02}"),
        })
        .collect();
    // The genome iris field is 3 bits → the first 8 EYE_RAMP stops are reachable.
    for i in 0..8u8 {
        items.push(Cell {
            tile: bake(head, None, 8, i, dark, H),
            label: format!("IRIS {i}"),
        });
    }
    let _ = EYE_RAMP.len();
    sheet(
        &format!("COATS + IRISES  ({tag})  ·  S100"),
        &items,
        4,
        bg,
        fg,
    )
    .save(&format!("{dir}/coats_{tag}.png"));
}

/// Hero art across every bounded local-text hue family plus neutral. This is
/// the visual counterpart to the CatColorKey contrast/quantization tests.
fn page_context(dir: &str, tag: &str, bg: [u8; 3], fg: [u8; 3], dark: bool) {
    const H: u32 = 64;
    let items: Vec<Cell> = (0..=12u8)
        .map(|accent| {
            let colors = CatColorKey {
                accent,
                background: if dark { 0 } else { 3 },
            };
            let head = bake_context(CatGlyphId::S103, 8, 4, colors, H);
            let cursor = bake_context(CatGlyphId::SpecStretch, 8, 4, colors, H);
            Cell {
                tile: hstack(&[&head, &cursor], bg, dark),
                label: if accent == 12 {
                    "NEUTRAL".to_string()
                } else {
                    format!("HUE {accent:02}")
                },
            }
        })
        .collect();
    sheet(
        &format!("CONTEXT PALETTE  ({tag})  ·  12 HUES + NEUTRAL"),
        &items,
        4,
        bg,
        fg,
    )
    .save(&format!("{dir}/context_{tag}.png"));
}

// ─────────────────────────── bake helpers ───────────────────────────

/// Bake one glyph through the real v4 path at art height `h`, its width taken
/// from the asset viewbox aspect (unit-square drawlist → true proportions).
fn bake(id: CatGlyphId, acc: Option<CatGlyphId>, coat: u8, iris: u8, dark: bool, h: u32) -> Tile {
    let aspect = viewbox_aspect(GLYPHS[id as usize].id);
    let w = ((h as f32) * aspect).round().max(1.0) as u32;
    let fills = ResolvedFills::from_indices(coat, iris, dark);
    bake_variant_with(id, acc, &fills, w, h, EyesFrame::Open)
}

fn bake_context(id: CatGlyphId, coat: u8, iris: u8, colors: CatColorKey, h: u32) -> Tile {
    let aspect = viewbox_aspect(GLYPHS[id as usize].id);
    let w = ((h as f32) * aspect).round().max(1.0) as u32;
    let fills = ResolvedFills::from_context(coat, iris, colors);
    bake_variant_with(id, None, &fills, w, h, EyesFrame::Open)
}

/// The `[w, h]` aspect (w/h) of a glyph's authored asset viewbox; `1.0` if absent.
fn viewbox_aspect(id: &str) -> f32 {
    let path = format!("{}/art/glyphs/{id}.toml", env!("CARGO_MANIFEST_DIR"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 1.0;
    };
    let Ok(doc) = text.parse::<aterm_toml::Value>() else {
        return 1.0;
    };
    let Some(vb) = doc.get("viewbox").and_then(|v| v.as_array()) else {
        return 1.0;
    };
    if vb.len() != 2 {
        return 1.0;
    }
    let n = |v: &aterm_toml::Value| {
        v.as_integer()
            .map(|i| i as f32)
            .or_else(|| v.as_float().map(|f| f as f32))
    };
    match (n(&vb[0]), n(&vb[1])) {
        (Some(w), Some(h)) if h > 0.0 => w / h,
        _ => 1.0,
    }
}

/// The debug name of a glyph id (e.g. `S100`, `SpecWitch`, `AccBow`) — the label.
fn short(id: CatGlyphId) -> String {
    format!("{id:?}")
}

// ─────────────────────────── canvas ───────────────────────────

struct Canvas {
    w: usize,
    h: usize,
    rgb: Vec<u8>,
}

/// One montage cell: a baked tile and its caption.
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

    /// Composite a baked tile's ART region (excluding the `PATCH_STRIP`
    /// catch-light column) straight-alpha over the canvas at `(x, y)`.
    fn blit(&mut self, tile: &Tile, x0: usize, y0: usize) {
        let art_w = tile.width().saturating_sub(u32::from(PATCH_STRIP)) as usize;
        let th = tile.height() as usize;
        let tw = tile.width() as usize;
        let src = tile.pixels();
        for ty in 0..th {
            for tx in 0..art_w {
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

    /// Draw a string with the built-in 5×7 font at integer `scale`.
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
            cx += 6 * scale; // 5 px glyph + 1 px gap
        }
    }

    fn save(&self, path: &str) {
        write_png(Path::new(path), self.w as u32, self.h as u32, &self.rgb).expect("write png");
    }
}

/// Lay `items` out in a `cols`-wide grid under a title, each cell sized to the
/// widest tile, captioned beneath.
fn sheet(title: &str, items: &[Cell], cols: usize, bg: [u8; 3], fg: [u8; 3]) -> Canvas {
    const MARGIN: usize = 10;
    const LABEL_H: usize = 12;
    const TITLE_H: usize = 26;
    let cell_w = items
        .iter()
        .map(|it| it.tile.width().saturating_sub(u32::from(PATCH_STRIP)) as usize)
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
        let art_w = it.tile.width().saturating_sub(u32::from(PATCH_STRIP)) as usize;
        let art_h = it.tile.height() as usize;
        let bx = cx + (cell_w.saturating_sub(art_w)) / 2;
        let by = cy + MARGIN;
        cv.blit(&it.tile, bx, by);
        cv.text(cx + MARGIN, cy + MARGIN + art_h + 2, &it.label, 1, fg);
    }
    let _ = bg;
    cv
}

/// Composite several tiles horizontally (baseline-bottom aligned) onto a fresh
/// opaque tile over `bg`, returning it as a single tile for a montage cell.
fn hstack(tiles: &[&Tile], bg: [u8; 3], _dark: bool) -> Tile {
    const GAP: u32 = 6;
    let art_ws: Vec<u32> = tiles
        .iter()
        .map(|t| t.width().saturating_sub(u32::from(PATCH_STRIP)))
        .collect();
    let total_w: u32 = art_ws.iter().sum::<u32>() + GAP * (tiles.len() as u32).saturating_sub(1);
    let max_h = tiles.iter().map(|t| t.height()).max().unwrap_or(1);
    let mut out = Tile::new(total_w + u32::from(PATCH_STRIP), max_h);
    // Prefill opaque bg so the paired glyphs share a ground inside the cell.
    {
        let px = out.pixels_mut();
        for p in px.as_chunks_mut::<4>().0 {
            p.copy_from_slice(&[bg[0], bg[1], bg[2], 255]);
        }
    }
    let mut x = 0u32;
    for (t, &aw) in tiles.iter().zip(&art_ws) {
        let y = max_h - t.height(); // bottom-align
        blit_tile_onto(&mut out, t, x, y);
        x += aw + GAP;
    }
    out
}

/// Straight-alpha composite tile `src`'s art region onto opaque tile `dst`.
fn blit_tile_onto(dst: &mut Tile, src: &Tile, x0: u32, y0: u32) {
    let art_w = src.width().saturating_sub(u32::from(PATCH_STRIP));
    let (sw, sh) = (src.width(), src.height());
    let (dw, dh) = (dst.width(), dst.height());
    let sp = src.pixels().to_vec();
    let dp = dst.pixels_mut();
    for ty in 0..sh {
        for tx in 0..art_w {
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

// ─────────────────────────── PNG + font ───────────────────────────

fn write_png(path: &Path, w: u32, h: u32, rgb: &[u8]) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = aterm_png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(aterm_png::ColorType::Rgb);
    enc.set_depth(aterm_png::BitDepth::Eight);
    enc.write_header()?
        .write_image_data(rgb)
        .map_err(std::io::Error::other)
}

/// A compact 5×7 bitmap font (uppercase, digits, a few punctuation) for cell
/// captions — self-contained so the gallery needs no font asset.
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
        'J' => [
            "..###", "...#.", "...#.", "...#.", "#..#.", "#..#.", ".##..",
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
        'Q' => [
            ".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#",
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
        'X' => [
            "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#",
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
        '5' => [
            "#####", "#....", "####.", "....#", "....#", "#...#", ".###.",
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
        '9' => [
            ".###.", "#...#", "#...#", ".####", "....#", "....#", ".###.",
        ],
        '-' => [
            ".....", ".....", ".....", "#####", ".....", ".....", ".....",
        ],
        '_' => [
            ".....", ".....", ".....", ".....", ".....", ".....", "#####",
        ],
        '+' => [
            ".....", "..#..", "..#..", "#####", "..#..", "..#..", ".....",
        ],
        '/' => [
            "....#", "....#", "...#.", "..#..", ".#...", "#....", "#....",
        ],
        '#' => [
            ".#.#.", ".#.#.", "#####", ".#.#.", "#####", ".#.#.", ".#.#.",
        ],
        ':' => [
            ".....", "..#..", "..#..", ".....", "..#..", "..#..", ".....",
        ],
        '.' => [
            ".....", ".....", ".....", ".....", ".....", ".##..", ".##..",
        ],
        '·' => [
            ".....", ".....", ".....", "..#..", ".....", ".....", ".....",
        ],
        _ => [
            ".....", ".....", ".....", ".....", ".....", ".....", ".....",
        ],
    }
}
