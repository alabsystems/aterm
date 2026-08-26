// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Visual demo of the PHASER trail — [`aterm_effects::cursor_glow`]'s fat
//! spectrum band plus the [`aterm_effects::cursor_phaser`] emitter cursor —
//! driven like the live host through the review scenarios: a hot typing run
//! (the three-letter band), a thinking pause (the band must FADE, never park),
//! the pause-then-one-key repro ("it stays back at the last time I typed"),
//! and an Enter (the old line's band snuffs). Composites to PNGs.
//!
//!   cargo run -p aterm-effects --example phaser_trail_demo -- <out_dir>

use std::time::Duration;

use aterm_time::Instant;

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::cursor_phaser::{CursorPhaser, PhaserConfig};

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
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img).unwrap();
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
    let cfg = GlowConfig {
        enabled: true,
        style: GlowStyle::Phaser,
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
        ribbon_tall: false,
        dark_theme: true,
        // The documented default dark palette — a COHERENT pair, never 0/0
        // (`fg == bg` reads as a conceal-shaped theme and suppresses the tint).
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
    };
    let em_cfg = PhaserConfig {
        enabled: true,
        intensity: 1.0,
    };

    let typed = "# the words stay readable in the beam";
    let row = 3u16;
    let ctx = [
        (2usize, "context line above the beam"),
        (row as usize, typed),
        (4usize, "and the line underneath it"),
    ];

    let mut glow = CursorGlow::default();
    let mut em = CursorPhaser::default();
    let mut quads = Vec::new();
    let t0 = Instant::now();
    let mut t = t0;
    let shoot = |glow: &mut CursorGlow,
                 em: &mut CursorPhaser,
                 quads: &mut Vec<aterm_render::GlowQuad>,
                 cur: (u16, u16),
                 t: Instant,
                 energy: f32,
                 name: &str| {
        glow.tick(Some(cur), t, &cfg, geom, quads);
        let f = em.tick(
            Some(cur),
            t,
            glow.beam_hue(),
            energy,
            true,
            geom,
            &em_cfg,
            quads,
        );
        composite(&format!("{dir}/{name}.png"), quads, f.fill, cur, &ctx, w, h);
    };

    // 1) Hot typing run at 45 ms/key: the band must be ONE bar over the last
    //    three letters, with the letters readable through it.
    let mut col = 2u16;
    glow.tick(Some((row, col)), t, &cfg, geom, &mut quads);
    for i in 0..24u16 {
        t += Duration::from_millis(45);
        col = 3 + i;
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((row, col)), t, &cfg, geom, &mut quads);
        let _ = i;
    }
    shoot(
        &mut glow,
        &mut em,
        &mut quads,
        (row, col),
        t,
        1.0,
        "typing_hot",
    );

    // 2) The thinking pause: 1.5 s after the last key the band must be gone
    //    (or clearly dying), never parked at the last word.
    t += Duration::from_millis(1500);
    shoot(
        &mut glow,
        &mut em,
        &mut quads,
        (row, col),
        t,
        0.1,
        "pause_1500ms",
    );

    // 3) The stays-back repro: one key after a 2.5 s pause used to inherit a
    //    multi-second life (5 s chain window) and PARK. Now it's a new burst:
    //    a crisp short blip.
    t += Duration::from_millis(1000);
    col += 1;
    glow.note_synthetic_typed(t, 1);
    glow.tick(Some((row, col)), t, &cfg, geom, &mut quads);
    shoot(
        &mut glow,
        &mut em,
        &mut quads,
        (row, col),
        t,
        0.2,
        "lone_key_0ms",
    );
    t += Duration::from_millis(900);
    shoot(
        &mut glow,
        &mut em,
        &mut quads,
        (row, col),
        t,
        0.05,
        "lone_key_900ms",
    );

    // 4) Retype a short burst then ENTER: the old line's band snuffs within a
    //    beat; nothing sweeps back across the line above.
    for _ in 0..6u16 {
        t += Duration::from_millis(45);
        col += 1;
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((row, col)), t, &cfg, geom, &mut quads);
    }
    t += Duration::from_millis(60);
    let landing = (row + 1, 0u16);
    glow.note_synthetic_move(t);
    glow.tick(Some(landing), t, &cfg, geom, &mut quads);
    shoot(&mut glow, &mut em, &mut quads, landing, t, 0.6, "enter_0ms");
    t += Duration::from_millis(300);
    shoot(
        &mut glow,
        &mut em,
        &mut quads,
        landing,
        t,
        0.3,
        "enter_300ms",
    );
}
