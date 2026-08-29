// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Visual demo of the FULL fire stack — [`aterm_effects::cursor_glow`]'s fire
//! comet/curtain/embers PLUS the [`aterm_effects::cursor_fireball`] cursor —
//! driven exactly like the live host (synthetic typing at human cadence, then
//! an Enter), composited over font8x8 glyph rows to PNGs. The offline twin of
//! a focused live window: no OS focus needed, full control of the timeline.
//!
//!   cargo run -p aterm-effects --example fire_stack_demo -- <out_dir>

use std::time::Duration;

use aterm_time::Instant;

use aterm_effects::cursor_fireball::{CursorFireball, FireballConfig};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};

const CW: usize = 14;
const CH: usize = 28;
const COLS: usize = 40;
const ROWS: usize = 8;
const BG: [u8; 3] = [0x10, 0x10, 0x16];
const FG: [u8; 3] = [0xC8, 0xC8, 0xD2];

fn draw_glyph(img: &mut [u8], w: usize, cell_x: usize, cell_y: usize, c: char, fg: [u8; 3]) {
    let i = c as usize;
    let bm: [u8; 8] = if i < 128 {
        font8x8::legacy::BASIC_LEGACY[i]
    } else {
        [0; 8]
    };
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

fn write_png(path: &str, img: &[u8], w: usize, h: usize) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = aterm_png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(aterm_png::ColorType::Rgb);
    enc.set_depth(aterm_png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(img).unwrap();
}

#[allow(clippy::too_many_arguments)]
fn composite(
    path: &str,
    quads: &[aterm_render::GlowQuad],
    fill: Option<u32>,
    cursor: (u16, u16),
    rows_text: &[(usize, &str)],
    w: usize,
    h: usize,
) {
    let mut img = vec![0u8; w * h * 3];
    for px in img.as_chunks_mut::<3>().0 {
        px.copy_from_slice(&BG);
    }
    for &(row, line) in rows_text {
        for (i, c) in line.chars().enumerate().take(COLS) {
            draw_glyph(&mut img, w, i * CW, row * CH, c, FG);
        }
    }
    if let Some(fill) = fill {
        let rgb = [(fill >> 16) as u8, (fill >> 8) as u8, fill as u8];
        let (cx, cy) = (cursor.1 as usize * CW, cursor.0 as usize * CH);
        for py in cy..cy + CH {
            for px in cx..cx + CW {
                let d = (py * w + px) * 3;
                img[d..d + 3].copy_from_slice(&rgb);
            }
        }
    }
    for q in quads {
        for py in q.y as usize..(q.y as usize + q.h as usize).min(h) {
            for px in q.x as usize..(q.x as usize + q.w as usize).min(w) {
                let d = (py * w + px) * 3;
                img[d] = img[d].saturating_add((q.color >> 16) as u8);
                img[d + 1] = img[d + 1].saturating_add((q.color >> 8) as u8);
                img[d + 2] = img[d + 2].saturating_add(q.color as u8);
            }
        }
    }
    write_png(path, &img, w, h);
    println!("wrote {path} ({} quads)", quads.len());
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
    // Mirror the host's resolved fire config (defaults + the live-review 2s trail).
    let cfg = GlowConfig {
        ribbon_tall: false,
        enabled: true,
        style: GlowStyle::Fire,
        color: 0x0050_FA7B,
        accent: 0x0078_FFB9,
        duration: Duration::from_millis(2000),
        length: 24,
        intensity: 0.7,
        radius: 0.6,
        ring: true,
        beam: true,
        head_dx: 0.5,
        pack: None,
        wake_persist_s: aterm_effects::cursor_glow::RAINBOW_WAKE_PERSIST,
        dark_theme: true,
        // The documented default dark palette — a COHERENT pair, never 0/0
        // (`fg == bg` reads as a conceal-shaped theme and suppresses the tint).
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
    };
    let fb_cfg = FireballConfig {
        enabled: true,
        intensity: 1.0,
    };

    let typed = "# the words stay readable in the burn";
    let row = 3u16;
    let ctx_above = (2usize, "context line above the fire");
    let ctx_below = (4usize, "and the line underneath it");

    let mut glow = CursorGlow::default();
    let mut ball = CursorFireball::default();
    let mut quads = Vec::new();
    let t0 = Instant::now();
    let mut t = t0;

    // Type across the row at 45 ms/key (hot cadence).
    let mut col = 2u16;
    glow.tick(Some((row, col)), t, &cfg, geom, &mut quads);
    let mut shots = 0;
    for i in 0..typed.len() as u16 {
        t += Duration::from_millis(45);
        col = 3 + i;
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((row, col)), t, &cfg, geom, &mut quads);
        // Mid-typing captures at a third and at the end of the line.
        if i == typed.len() as u16 / 2 || i == typed.len() as u16 - 1 {
            let f = ball.tick(Some((row, col)), t, glow.blaze(), geom, &fb_cfg, &mut quads);
            composite(
                &format!("{dir}/stack_typing_{shots}.png"),
                &quads,
                f.fill,
                (row, col),
                &[ctx_above, (row as usize, typed), ctx_below],
                w,
                h,
            );
            shots += 1;
        }
    }

    // ENTER: down one row, back to column 0. Capture the snuff sequence.
    t += Duration::from_millis(60);
    let landing = (row + 1, 0u16);
    for (label, wait_ms) in [
        ("enter_0ms", 0u64),
        ("enter_120ms", 120),
        ("enter_400ms", 400),
    ] {
        t += Duration::from_millis(wait_ms);
        if wait_ms == 0 {
            glow.note_synthetic_move(t);
        }
        glow.tick(Some(landing), t, &cfg, geom, &mut quads);
        let f = ball.tick(Some(landing), t, glow.blaze(), geom, &fb_cfg, &mut quads);
        composite(
            &format!("{dir}/stack_{label}.png"),
            &quads,
            f.fill,
            landing,
            &[ctx_above, (row as usize, typed), ctx_below],
            w,
            h,
        );
    }
}
