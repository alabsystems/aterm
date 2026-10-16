// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `harness_card_preview`: a contact sheet of the harness card
//! (`aterm_effects::harness_card`, `docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §3) for visual iteration — a picture to look at, not a check.
//!
//! Seven panels, one above the next: dark settled, the burst, a blink with
//! the head passing, Land settled, light settled, the light burst, and a
//! light Land tail flick. Each is the emitter's real quads composited with
//! the renderer's own gradient law and blend functions
//! (`aterm_render::glow_lerp_rgb`, `glow_lerp`, `add_sat`, `over_premul`) over
//! stand-in text bars where the card's three rows sit, plus the line under
//! the card one block per letter in its ink, written at 2× to
//! `<out_dir>/harness_card.png`.
//!
//! ```text
//! targo --unverified run -p aterm-effects --example harness_card_preview -- [out_dir]
//! (default out_dir: target/harness_card_preview)
//! ```

use std::path::Path;
use std::process::ExitCode;

use aterm_effects::harness_card::{CardPalette, CardScene, EPISODE_MS, HarnessCard};
use aterm_time::Instant;
use std::time::Duration;

/// One panel's size in pixels, before the 2× scale.
const PANEL_W: usize = 620;
const PANEL_H: usize = 100;

/// The cell the panels are laid out on.
const CW: usize = 9;
const CH: usize = 18;

/// The grid's origin inside a panel.
const ORIGIN: usize = 8;

/// The output scale: each pixel drawn as a `SCALE × SCALE` block.
const SCALE: usize = 2;

fn main() -> ExitCode {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/harness_card_preview".to_string());
    let out = Path::new(&out);
    if let Err(e) = std::fs::create_dir_all(out) {
        eprintln!("harness_card_preview: {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    let path = out.join("harness_card.png");
    match write_png(&path, &sheet()) {
        Ok(()) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("harness_card_preview: {}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}

/// A launch card at a 9 × 18 cell at the panel's top-left: text to column
/// 31, the trail to the kitty at column 40, the box 62 columns wide, four
/// boxed rows (the card's three and the line's).
fn base() -> CardScene {
    CardScene {
        origin_x: ORIGIN as i32,
        origin_y: ORIGIN as i32,
        cw: CW as u16,
        ch: CH as u16,
        row_base: 0,
        pad_left: 10,
        top: 0,
        rows: 4,
        cols: 62,
        clip: (0, 0, PANEL_W as i32, PANEL_H as i32),
        trail: Some((32, 40)),
        kitty_col: Some(40),
        palette: CardPalette::Cruise,
        dark: true,
        serious: false,
        coat: 0x00E8_A060,
        eye: 0x0040_C080,
    }
}

/// The panels, `0x00RRGGBB` pixels, [`PANEL_W`] wide and stacked.
fn sheet() -> Vec<u32> {
    let dark = base();
    let land = CardScene {
        palette: CardPalette::Land,
        ..dark
    };
    let light = CardScene {
        dark: false,
        ..dark
    };
    let light_land = CardScene {
        dark: false,
        ..land
    };
    let panels = [
        (dark, EPISODE_MS + 1),
        (dark, 150),
        (dark, 1300),
        (land, EPISODE_MS + 1),
        (light, EPISODE_MS + 1),
        (light, 200),
        (light_land, 2950),
    ];
    let mut img = vec![0u32; PANEL_W * PANEL_H * panels.len()];
    for (i, (scene, at)) in panels.iter().enumerate() {
        let panel = &mut img[i * PANEL_W * PANEL_H..(i + 1) * PANEL_W * PANEL_H];
        draw_panel(panel, scene, *at);
    }
    img
}

/// One panel: the ground, stand-in text bars, the card `at` ms into an
/// episode, and the line under the card.
fn draw_panel(img: &mut [u32], s: &CardScene, at: u64) {
    let bg = if s.dark { 0x001A_1B26 } else { 0x00FA_FAF8 };
    let fg = if s.dark { 0x00C0_C4D0 } else { 0x0030_3030 };
    img.fill(bg);
    // Stand-in text: dashed bars where the card's three rows sit.
    for r in 0..3 {
        for x in (ORIGIN + CW * 11)..(ORIGIN + CW * 31) {
            for y in (ORIGIN + r * CH + 5)..(ORIGIN + r * CH + 13) {
                if (x / 3) % 3 != 0 {
                    img[y * PANEL_W + x] = fg;
                }
            }
        }
    }
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    let now = t0 + Duration::from_millis(at);
    let (mut under, mut over) = (Vec::new(), Vec::new());
    card.tick(now, s, true, &mut under, &mut over);
    for q in under.iter().chain(&over) {
        for dy in 0..usize::from(q.h) {
            for dx in 0..usize::from(q.w) {
                let (x, y) = (usize::from(q.x) + dx, usize::from(q.y) + dy);
                if x >= PANEL_W || y >= PANEL_H {
                    continue;
                }
                // The renderers' own gradient law and blends.
                let qw = u32::from(q.w);
                let c = aterm_render::glow_lerp_rgb(q.color, q.color2, dx as u32, qw);
                let a =
                    aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), dx as u32, qw)
                        as u8;
                let px = &mut img[y * PANEL_W + x];
                *px = if a == 0 {
                    aterm_render::add_sat(*px, c)
                } else {
                    aterm_render::over_premul(*px, c, a)
                };
            }
        }
    }
    // The line, one block per letter in its ink.
    for c in card.line_ink(now, 3, 11, 50, s.dark, true) {
        let ink =
            (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2]);
        let x0 = ORIGIN + usize::from(c.col) * CW;
        for x in x0 + 1..x0 + CW - 1 {
            for y in (ORIGIN + 3 * CH + 5)..(ORIGIN + 3 * CH + 13) {
                img[y * PANEL_W + x] = ink;
            }
        }
    }
}

/// Write `img` (stacked panels) at [`SCALE`]× as an RGB PNG.
fn write_png(path: &Path, img: &[u32]) -> std::io::Result<()> {
    let (w, h) = (PANEL_W * SCALE, img.len() / PANEL_W * SCALE);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let p = img[(y / SCALE) * PANEL_W + x / SCALE];
            rgb.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    let to_u32 = |n: usize| u32::try_from(n).map_err(std::io::Error::other);
    let file = std::fs::File::create(path)?;
    let mut enc = aterm_png::Encoder::new(std::io::BufWriter::new(file), to_u32(w)?, to_u32(h)?);
    enc.set_color(aterm_png::ColorType::Rgb);
    enc.set_depth(aterm_png::BitDepth::Eight);
    enc.write_header()?
        .write_image_data(&rgb)
        .map_err(std::io::Error::other)
}
