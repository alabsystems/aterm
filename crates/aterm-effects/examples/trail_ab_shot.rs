// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A/B SHOT — drive the LIVE cursor-trail animator through a fixed script and
//! composite its quads to PNGs, so two builds' trails can be compared pixel for
//! pixel without opening a window.
//!
//! This exists because the question "does this still look like it used to?"
//! had no honest answer before it. A headless `image` capture does not tick the
//! trail at all (it refills the render input from the grid), so the only way to
//! see the effect was to look at a running window — which cannot be diffed, and
//! cannot be compared against a release from two months ago at all.
//!
//! It is how `classic` was verified: run this in a worktree at tag `v0.28` and
//! again on main with `classic`, and the three PNGs come out byte-identical.
//!
//!   cargo run -p aterm-effects --example trail_ab_shot -- <out_dir> <style>
//!
//! The script is a fast typing run, a 300 ms decay, then a screen-crossing
//! jump — the three moments the trail's character actually lives in. Input
//! hints are supplied where the modern engine's license seam needs them, so a
//! style that consults the seam and one that does not are both driven fairly.

use std::time::Duration;

use aterm_time::Instant;

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle, RAINBOW_WAKE_PERSIST};

const CW: usize = 14;
const CH: usize = 28;
const COLS: usize = 60;
const ROWS: usize = 6;
const BG: [u8; 3] = [0x10, 0x10, 0x16];

fn brighten(c: u32) -> u32 {
    let m = |sh: u32| ((((c >> sh) & 0xff) as f32) * 1.5).min(255.0) as u32;
    (m(16) << 16) | (m(8) << 8) | m(0)
}

fn composite_all(
    path: &str,
    quads: &[aterm_render::GlowQuad],
    halos: &[aterm_render::RainHalo],
    w: usize,
    h: usize,
) {
    let mut img = vec![0u8; w * h * 3];
    for px in img.as_chunks_mut::<3>().0 {
        px.copy_from_slice(&BG);
    }
    for q in quads {
        for py in q.y as usize..(q.y as usize + q.h as usize).min(h) {
            for px in q.x as usize..(q.x as usize + q.w as usize).min(w) {
                let d = (py * w + px) * 3;
                // out = color + dst*(255-alpha)/255  (alpha==0 ⇒ pure additive)
                let a = u32::from(q.alpha);
                for k in 0..3 {
                    let src = (q.color >> (16 - 8 * k)) & 0xff;
                    let dst = u32::from(img[d + k]);
                    img[d + k] = (src + dst * (255 - a) / 255).min(255) as u8;
                }
            }
        }
    }
    // RainHalo: integer elliptical falloff, weight = ((256 - nsq)^2 >> 8).
    for q in halos {
        for py in q.y as usize..(q.y as usize + q.h as usize).min(h) {
            for px in q.x as usize..(q.x as usize + q.w as usize).min(w) {
                let dx = px as i64 - i64::from(q.cx);
                let dy = py as i64 - i64::from(q.cy);
                let rx = i64::from(q.rx).max(1);
                let ry = i64::from(q.ry).max(1);
                let nsq = (dx * dx * 256) / (rx * rx) + (dy * dy * 256) / (ry * ry);
                if nsq >= 256 {
                    continue;
                }
                let t = 256 - nsq;
                let weight = ((t * t) >> 8).clamp(0, 256);
                let d = (py * w + px) * 3;
                for k in 0..3 {
                    let src = ((q.color >> (16 - 8 * k)) & 0xff) as i64 * weight / 256;
                    img[d + k] = img[d + k].saturating_add(src as u8);
                }
            }
        }
    }
    let file = std::fs::File::create(path).unwrap();
    let mut enc = aterm_png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(aterm_png::ColorType::Rgb);
    enc.set_depth(aterm_png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img).unwrap();
    println!(
        "wrote {path} ({} quads, {} halos)",
        quads.len(),
        halos.len()
    );
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let raw = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "lumen".to_string());
    std::fs::create_dir_all(&dir).expect("create the output directory");
    let style = GlowStyle::parse(&raw);
    let (w, h) = (COLS * CW, ROWS * CH);
    let color = 0x0050_FA7Bu32;
    let cfg = GlowConfig {
        enabled: true,
        style,
        color,
        accent: brighten(color),
        duration: Duration::from_millis(260),
        length: 24,
        intensity: 0.7,
        radius: 0.6,
        ring: true,
        dark_theme: true,
        theme_fg: 0x00C8_C8D2,
        theme_bg: 0x0010_1016,
        beam: aterm_effects::cursor_glow::style_has_beam(&raw),
        head_dx: 0.5,
        pack: None,
        wake_persist_s: RAINBOW_WAKE_PERSIST,
        ribbon_tall: false,
        classic_mono: GlowStyle::style_names_classic_mono(&raw),
    };
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
    let t0 = Instant::now();
    let mut g = CursorGlow::default();
    let mut out = Vec::new();
    let mut ms = 0u64;

    for _ in 0..3 {
        g.tick(
            Some((2, 5)),
            t0 + Duration::from_millis(ms),
            &cfg,
            geom,
            &mut out,
        );
        ms += 17;
    }
    for k in 1..=25u16 {
        for f in 0..4 {
            let cur = (2u16, 5 + k - u16::from(f == 0));
            if f == 0 {
                g.note_typed(t0 + Duration::from_millis(ms));
            }
            g.tick(
                Some(cur),
                t0 + Duration::from_millis(ms),
                &cfg,
                geom,
                &mut out,
            );
            ms += 17;
        }
    }
    composite_all(&format!("{dir}/{raw}_typing.png"), &out, g.halos(), w, h);

    for _ in 0..18 {
        g.tick(
            Some((2, 30)),
            t0 + Duration::from_millis(ms),
            &cfg,
            geom,
            &mut out,
        );
        ms += 17;
    }
    composite_all(&format!("{dir}/{raw}_decay.png"), &out, g.halos(), w, h);

    for i in 0..4 {
        let cur = if i == 0 { (2u16, 30u16) } else { (4, 48) };
        if i == 1 {
            let t = t0 + Duration::from_millis(ms);
            g.note_user_gesture(t);
            g.note_navigation(t);
            g.note_motion(t);
        }
        g.tick(
            Some(cur),
            t0 + Duration::from_millis(ms),
            &cfg,
            geom,
            &mut out,
        );
        ms += 17;
    }
    eprintln!(
        "jump: {} quads, admission {:?}",
        out.len(),
        g.admission_tally()
    );
    composite_all(&format!("{dir}/{raw}_jump.png"), &out, g.halos(), w, h);
}
