// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `system-state-gallery`'s contact sheets: one PNG per look and size, every fixture's frame
//! scaled to a tile, its name and each check's verdict lettered under it (green pass, red fail).

use std::path::Path;

use crate::trail_contrast::{Image, Rgb};

/// The widest a tile is drawn, in sheet pixels: a frame is shrunk by the smallest whole factor
/// that fits it.
const TILE_MAX_W: usize = 460;
/// The sheet's width, in sheet pixels, which decides how many tiles a row holds.
const SHEET_W: usize = 2900;
const GAP: usize = 16;
/// The bitmap font's scale: a 5x7 glyph drawn 2x is 10x14 pixels in a 12-pixel advance.
const TEXT_SCALE: usize = 2;
const TITLE_SCALE: usize = 3;
const LINE_H: usize = 7 * TEXT_SCALE + 6;

const SHEET_BG: Rgb = [0x26, 0x27, 0x2b];
const INK: Rgb = [0xf2, 0xf2, 0xf2];
const QUIET: Rgb = [0xa8, 0xab, 0xb3];
const PASS: Rgb = [0x5c, 0xd6, 0x7a];
const FAIL: Rgb = [0xff, 0x6b, 0x6b];

/// One tile: a frame, its caption, and its verdicts as `(word, passed)`.
pub struct Tile {
    pub frame: Image,
    pub caption: String,
    pub verdicts: Vec<(String, bool)>,
}

/// An RGB canvas.
struct Canvas {
    w: usize,
    h: usize,
    px: Vec<Rgb>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Canvas {
        Canvas {
            w,
            h,
            px: vec![SHEET_BG; w * h],
        }
    }

    fn put(&mut self, x: usize, y: usize, c: Rgb) {
        if x < self.w && y < self.h {
            self.px[y * self.w + x] = c;
        }
    }

    /// `text` in the 5x7 font at `scale`, its top left at `(x, y)`; the x after it.
    fn text(&mut self, x: usize, y: usize, text: &str, scale: usize, c: Rgb) -> usize {
        let mut cx = x;
        for ch in text.chars() {
            for (row, bits) in glyph(ch).iter().enumerate() {
                for col in 0..5 {
                    if bits & (0b10000 >> col) != 0 {
                        for sy in 0..scale {
                            for sx in 0..scale {
                                self.put(cx + col * scale + sx, y + row * scale + sy, c);
                            }
                        }
                    }
                }
            }
            cx += 6 * scale;
        }
        cx
    }

    /// `frame` shrunk by `factor` (each sheet pixel the mean of its `factor`x`factor` block),
    /// its top left at `(x, y)`.
    fn blit(&mut self, frame: &Image, factor: usize, x: usize, y: usize) {
        let (w, h) = (frame.width / factor, frame.height / factor);
        let n = u32::try_from(factor * factor).unwrap_or(1);
        for ty in 0..h {
            for tx in 0..w {
                let mut sum = [0u32; 3];
                for sy in 0..factor {
                    for sx in 0..factor {
                        let p = frame.at(tx * factor + sx, ty * factor + sy);
                        for (s, v) in sum.iter_mut().zip(p) {
                            *s += u32::from(v);
                        }
                    }
                }
                let mean = sum.map(|s| u8::try_from((s + n / 2) / n).unwrap_or(u8::MAX));
                self.put(x + tx, y + ty, mean);
            }
        }
    }

    fn png(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let bytes: Vec<u8> = self.px.iter().flatten().copied().collect();
        let (w, h) = (
            u32::try_from(self.w).map_err(|e| e.to_string())?,
            u32::try_from(self.h).map_err(|e| e.to_string())?,
        );
        let mut enc = aterm_png::Encoder::new(&mut out, w, h);
        enc.set_color(aterm_png::ColorType::Rgb);
        enc.set_depth(aterm_png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&bytes).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
        Ok(out)
    }
}

/// The smallest whole factor that shrinks a frame `width` wide to a tile.
#[must_use]
pub fn shrink(width: usize) -> usize {
    width.div_ceil(TILE_MAX_W).max(1)
}

/// The width of `text` in the 5x7 font at `scale`.
const fn text_w(chars: usize, scale: usize) -> usize {
    chars * 6 * scale
}

/// A tile's verdicts as lines no wider than `width`: each line the indices of its words.
fn verdict_lines(verdicts: &[(String, bool)], width: usize) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut used = 0;
    for (i, (word, _)) in verdicts.iter().enumerate() {
        let w = text_w(word.chars().count(), TEXT_SCALE);
        match lines.last_mut() {
            Some(line) if used + text_w(1, TEXT_SCALE) + w <= width => {
                line.push(i);
                used += text_w(1, TEXT_SCALE) + w;
            }
            _ => {
                lines.push(vec![i]);
                used = w;
            }
        }
    }
    lines
}

/// Lay out `tiles` under `title` and `note` and write the sheet to `path`.
/// # Errors
/// No tiles, or the PNG could not be encoded or written.
pub fn write(path: &Path, title: &str, note: &str, tiles: &[Tile]) -> Result<(), String> {
    let png = sheet_png(title, note, tiles, false)?;
    crate::util::write_out(path, &png).map_err(|e| format!("{}: {e}", path.display()))
}

/// Native-pixel tiles, encoded for a caller that retains its own output directory.
/// # Errors
/// No tiles, a canvas above 16 million pixels, or PNG encoding fails.
pub(crate) fn native_png(title: &str, note: &str, tiles: &[Tile]) -> Result<Vec<u8>, String> {
    sheet_png(title, note, tiles, true)
}

fn sheet_png(title: &str, note: &str, tiles: &[Tile], native: bool) -> Result<Vec<u8>, String> {
    if native
        && (tiles.len() > 8
            || title.len() > 120
            || note.len() > 120
            || tiles.iter().any(|t| {
                t.frame.width > 4096
                    || t.frame.height > 1024
                    || t.caption.len() > 120
                    || !t.verdicts.is_empty()
            }))
    {
        return Err("native sheet exceeds its frame, caption or page bounds".into());
    }
    let factor = |width| if native { 1 } else { shrink(width) };
    let cell_w = tiles
        .iter()
        .map(|t| {
            let width = t.frame.width / factor(t.frame.width);
            if native {
                width.max(text_w(t.caption.chars().count(), TEXT_SCALE))
            } else {
                width
            }
        })
        .max()
        .ok_or("no frames for this sheet")?;
    let frame_h = tiles
        .iter()
        .map(|t| t.frame.height / factor(t.frame.width))
        .max()
        .unwrap_or(0);
    let verdict_rows = tiles
        .iter()
        .map(|t| verdict_lines(&t.verdicts, cell_w).len())
        .max()
        .unwrap_or(0);
    let cols = ((SHEET_W - GAP) / (cell_w + GAP)).max(1).min(tiles.len());
    let rows = tiles.len().div_ceil(cols);
    let cell_h = frame_h + 4 + (1 + verdict_rows) * LINE_H;
    let head = GAP + 7 * TITLE_SCALE + 8 + LINE_H + GAP;
    // Wide enough for the tiles, the title and the note.
    let width = (GAP + cols * (cell_w + GAP))
        .max(2 * GAP + text_w(title.chars().count(), TITLE_SCALE))
        .max(2 * GAP + text_w(note.chars().count(), TEXT_SCALE));
    let height = head + rows * (cell_h + GAP);
    if native
        && width
            .checked_mul(height)
            .is_none_or(|n| n > 16 * 1024 * 1024)
    {
        return Err("native sheet exceeds 16 million pixels".into());
    }
    let mut canvas = Canvas::new(width, height);
    canvas.text(GAP, GAP, title, TITLE_SCALE, INK);
    canvas.text(GAP, GAP + 7 * TITLE_SCALE + 8, note, TEXT_SCALE, QUIET);
    for (i, tile) in tiles.iter().enumerate() {
        let x = GAP + (i % cols) * (cell_w + GAP);
        let y = head + (i / cols) * (cell_h + GAP);
        canvas.blit(&tile.frame, factor(tile.frame.width), x, y);
        let below = y + frame_h + 4;
        canvas.text(x, below, &tile.caption, TEXT_SCALE, INK);
        for (row, line) in verdict_lines(&tile.verdicts, cell_w).iter().enumerate() {
            let mut cx = x;
            for &word in line {
                let (text, passed) = &tile.verdicts[word];
                let colour = if *passed { PASS } else { FAIL };
                cx = canvas.text(cx, below + (1 + row) * LINE_H, text, TEXT_SCALE, colour);
                cx += text_w(1, TEXT_SCALE);
            }
        }
    }
    canvas.png()
}

/// A character's 5x7 rows, the high bit leftmost; lower case is drawn as upper case, and a
/// character the font lacks as a space.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10101, 0b10011, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
        ],
        'X' | '×' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        '_' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111,
        ],
        '+' => [
            0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000,
        ],
        '=' => [
            0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000,
        ],
        '/' => [
            0b00001, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b10000,
        ],
        ':' => [
            0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
        ',' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b00100, 0b01000,
        ],
        '·' => [
            0b00000, 0b00000, 0b00000, 0b00100, 0b00000, 0b00000, 0b00000,
        ],
        '(' => [
            0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010,
        ],
        ')' => [
            0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000,
        ],
        '%' => [
            0b11001, 0b11010, 0b00010, 0b00100, 0b01000, 0b01011, 0b10011,
        ],
        _ => [0; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_sheet_keeps_a_one_pixel_slit() {
        let mut pixels = vec![[255, 255, 255]; 900 * 2];
        pixels[600] = [0, 0, 0];
        pixels[900 + 600] = [0, 0, 0];
        let frame = Image::from_pixels(900, 2, pixels);
        let crop = frame
            .crop(crate::trail_contrast::PxBox {
                x0: 100,
                y0: 0,
                x1: 800,
                y1: 2,
            })
            .unwrap();
        let tiles = [Tile {
            frame: crop,
            caption: "frame 1".into(),
            verdicts: vec![],
        }];
        let bytes = native_png("native", "no visual verdict", &tiles).unwrap();
        let sheet = Image::decode(Path::new("sheet.png"), &bytes).unwrap();
        let head = GAP + 7 * TITLE_SCALE + 8 + LINE_H + GAP;
        assert_eq!(sheet.at(GAP + 500, head), [0, 0, 0]);
        assert_eq!(sheet.at(GAP + 500, head + 1), [0, 0, 0]);
        assert_eq!(sheet.at(GAP + 499, head), [255, 255, 255]);
        assert_eq!(sheet.at(GAP + 501, head), [255, 255, 255]);
    }

    #[test]
    fn a_sheet_shrinks_frames_to_tiles_and_letters_them() {
        let frame = |w, h, c| Image::from_pixels(w, h, vec![c; w * h]);
        let tiles = vec![
            Tile {
                frame: frame(900, 644, [10, 20, 30]),
                caption: "running".into(),
                verdicts: vec![("text 4.6".into(), true), ("card 1.1".into(), false)],
            },
            Tile {
                frame: frame(900, 644, [200, 200, 200]),
                caption: "set-up".into(),
                verdicts: vec![],
            },
        ];
        assert_eq!(shrink(900), 2);
        assert_eq!(shrink(1800), 4);
        assert_eq!(shrink(368), 1);
        let dir = aterm_tempfile::Builder::new()
            .prefix("sheet")
            .suffix(".noindex")
            .tempdir()
            .expect("a scratch folder");
        let path = dir.path().join("sheet.png");
        write(&path, "system state", "settled", &tiles).unwrap();
        let sheet = Image::read(&path).unwrap();
        assert_eq!(
            sheet.width,
            GAP + 2 * (450 + GAP),
            "two 450-pixel tiles in a row"
        );
        // The first tile's mean colour lands at its top left, under the head.
        let head = GAP + 7 * TITLE_SCALE + 8 + LINE_H + GAP;
        assert_eq!(sheet.at(GAP, head), [10, 20, 30]);
        assert_eq!(sheet.at(GAP + 450 + GAP, head), [200, 200, 200]);
        // A failed verdict is lettered in red somewhere under the first tile.
        let under = head + 322 + 4 + LINE_H;
        let red = (under..under + 14)
            .flat_map(|y| (GAP..GAP + 450).map(move |x| (x, y)))
            .any(|(x, y)| sheet.at(x, y) == FAIL);
        assert!(red);
        assert!(write(&path, "empty", "", &[]).is_err());
        // One narrow tile: the sheet widens for its title, and the verdicts wrap to the tile.
        let words: Vec<(String, bool)> = [
            "text 6.05",
            "glyph 6.14",
            "card 1.15",
            "clip 0",
            "rhythm 76",
        ]
        .iter()
        .map(|w| ((*w).to_string(), true))
        .collect();
        let lines = verdict_lines(&words, 346);
        assert!(lines.len() > 1, "{lines:?}");
        for line in &lines {
            let chars: usize = line.iter().map(|&i| words[i].0.len() + 1).sum::<usize>() - 1;
            assert!(text_w(chars, TEXT_SCALE) <= 346, "{line:?}");
        }
        assert_eq!(
            lines.concat(),
            (0..words.len()).collect::<Vec<_>>(),
            "every word, in order"
        );
        let narrow = [Tile {
            frame: frame(346, 644, [10, 20, 30]),
            caption: "running".into(),
            verdicts: words,
        }];
        let title = "system state · hc-dark · compact 46 columns at 1x";
        write(&path, title, "settled", &narrow).unwrap();
        let sheet = Image::read(&path).unwrap();
        assert_eq!(
            sheet.width,
            2 * GAP + text_w(title.chars().count(), TITLE_SCALE)
        );
        assert_eq!(
            sheet.height,
            head + 644 + 4 + (1 + lines.len()) * LINE_H + GAP,
            "a line per verdict row under the caption"
        );
    }
}
