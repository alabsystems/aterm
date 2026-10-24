// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The differential law of lane B7 (`docs/DESIGN-card-engine-2026-10-19.md`
//! §6): every companion sprite the old path could draw, drawn as a sprite
//! layer ([`aterm_render::sprites`]), is the old stamp byte for byte, the
//! knock-out included: on random pages, at 1:1 and integer scales, mirrored,
//! cut by a peek's window or a pane's box, under and over the text, on the
//! full path and the damaged one. The old path is the
//! [`aterm_render::legacy_sprite_oracle`]. At a non-integer scale the two
//! part on purpose: the layer maps a texel by the floor of its scale (Law F,
//! the same on the CPU, Metal and wgpu) where the old stamp sampled texel
//! centres.

use std::sync::Arc;

use aterm_core::render::{FxArt, PxRect, RenderInput};
use aterm_core::terminal::Terminal;
use aterm_render::legacy_sprite_oracle::{LegacySprite, LegacySprites};
use aterm_render::sprites::{FreeSprite, FreeZ, SpriteLayers};
use aterm_render::{Renderer, Theme, WindowCpu};

use crate::sprites::{SALT, art_of};

const ROWS: usize = 5;
const COLS: usize = 18;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo) as u64) as i64
    }

    fn chance(&mut self, n: u64, d: u64) -> bool {
        self.next() % d < n
    }
}

fn renderer() -> Renderer {
    let mut r = Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    r.debug_block_on_lazy_fallbacks();
    r
}

fn page(rng: &mut Rng) -> Terminal {
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    t.process(b"\x1b[?25l");
    for r in 0..ROWS {
        if rng.chance(3, 4) {
            let words = ["the kitty", "naps on", "a warm row", "of text", "█▌ cat"];
            let w = words[rng.range(0, words.len() as i64) as usize];
            t.process(format!("\x1b[{};{}H{w}", r + 1, rng.range(1, 6)).as_bytes());
        }
    }
    t
}

fn random_art(rng: &mut Rng) -> Arc<FxArt> {
    let (w, h) = (rng.range(2, 14) as u16, rng.range(2, 16) as u16);
    let seed = rng.next();
    art_of(w, h, |x, y| {
        let v = seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(u64::from(x) * 0x1_0001 + u64::from(y) * 0x10_0010_0001);
        let v = v ^ (v >> 29);
        let a = match v % 5 {
            0 => 0,
            1 | 2 => 255,
            _ => (v >> 8) as u8,
        };
        [(v >> 16) as u8, (v >> 24) as u8, (v >> 32) as u8, a]
    })
}

/// A sprite the old path could draw: whole, or cut on texel lines.
fn random_sprite(rng: &mut Rng, cw: usize, ch: usize) -> FreeSprite {
    loop {
        let art = random_art(rng);
        let (kx, ky) = if rng.chance(1, 2) {
            (1, 1)
        } else {
            (rng.range(1, 4) as u16, rng.range(1, 4) as u16)
        };
        let (w, h) = (art.w() * kx, art.h() * ky);
        let x = rng.range(-i64::from(w), (COLS * cw) as i64) as i32;
        let y = rng.range(-i64::from(h), (ROWS * ch) as i64) as i32;
        let mut s = FreeSprite {
            w,
            h,
            tint: (rng.next() as u32) & 0x00FF_FFFF | 0x0040_4040,
            alpha: rng.range(1, 256) as u8,
            flip_x: rng.chance(1, 3),
            z: if rng.chance(1, 2) {
                FreeZ::UnderText
            } else {
                FreeZ::OverText
            },
            ..FreeSprite::whole(x, y, art)
        };
        match rng.range(0, 3) {
            // A peek's window: the rows it has risen into view.
            0 => {
                let shown = rng.range(1, i64::from(s.art.h()) + 1) as i32 * i32::from(ky);
                s.clip = PxRect::new(i32::MIN, y + i32::from(h) - shown, i32::MAX, i32::MAX);
            }
            // A pane's box, on texel lines.
            1 => {
                let tx = |n: i64| x + n as i32 * i32::from(kx);
                let ty = |n: i64| y + n as i32 * i32::from(ky);
                let (aw, ah) = (i64::from(s.art.w()), i64::from(s.art.h()));
                let (c0, r0) = (rng.range(0, aw), rng.range(0, ah));
                s.clip = PxRect::new(
                    tx(c0),
                    ty(r0),
                    tx(rng.range(c0 + 1, aw + 1)),
                    ty(rng.range(r0 + 1, ah + 1)),
                );
            }
            _ => {}
        }
        if !s.is_blank() && LegacySprite::of(&s).is_some() {
            return s;
        }
    }
}

fn old_frame(rend: &mut Renderer, t: &mut Terminal, sprites: &[FreeSprite]) -> Vec<u32> {
    let old = sprites
        .iter()
        .map(|s| LegacySprite::of(s).expect("drawable by the old path"))
        .collect();
    rend.set_legacy_sprites(Some(LegacySprites { sprites: old }));
    let px = rend.render_input(&t.cell_frame(ROWS, COLS)).pixels.clone();
    rend.set_legacy_sprites(None);
    px
}

fn new_input(t: &mut Terminal, pubr: &mut SpriteLayers, sprites: &[FreeSprite]) -> RenderInput {
    let mut input = t.cell_frame(ROWS, COLS);
    pubr.publish(sprites, &mut input.fx_layers);
    input
}

#[test]
fn every_old_sprite_is_its_layer_byte_for_byte() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut rng = Rng(0x0B7D_1FF0_5EED_0001);
    let (mut cut, mut scaled, mut flipped, mut knocked) = (0, 0, 0, 0);
    for case in 0..160 {
        let mut t = page(&mut rng);
        let n = rng.range(1, 5) as usize;
        let sprites: Vec<FreeSprite> = (0..n).map(|_| random_sprite(&mut rng, cw, ch)).collect();
        let mut pubr = SpriteLayers::new(SALT);
        let input = new_input(&mut t, &mut pubr, &sprites);
        let got = rend.render_input(&input).pixels.clone();
        let want = old_frame(&mut rend, &mut t, &sprites);
        assert!(got == want, "case {case}: {sprites:?}");

        // Negative control: the same layers without their knock-out part from
        // the old path wherever an under-text sprite lies on a letter.
        let mut bare = input.clone();
        for l in &mut bare.fx_layers {
            l.knock = false;
        }
        if rend.render_input(&bare).pixels != want {
            knocked += 1;
        }
        cut += sprites
            .iter()
            .filter(|s| s.clip != PxRect::EVERYWHERE)
            .count();
        scaled += sprites
            .iter()
            .filter(|s| s.w != s.art.w() || s.h != s.art.h())
            .count();
        flipped += sprites.iter().filter(|s| s.flip_x).count();
    }
    assert!(
        cut > 40 && scaled > 40 && flipped > 30,
        "the sweep reaches cut ({cut}), scaled ({scaled}) and mirrored ({flipped}) sprites"
    );
    assert!(
        knocked > 10,
        "CONTROL: the knock-out separates the paths in {knocked} cases"
    );
}

#[test]
fn the_damaged_path_draws_moving_layers_as_the_old_full_frame() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut rng = Rng(0x0B7D_1FF0_5EED_0002);
    let mut t = page(&mut rng);
    let mut pubr = SpriteLayers::new(SALT);
    let mut wc = WindowCpu::new();
    let mut sprites: Vec<FreeSprite> = (0..3).map(|_| random_sprite(&mut rng, cw, ch)).collect();
    let (mut mirrored, mut retinted, mut rescaled) = (0, 0, 0);
    for step in 0..80 {
        // Move, re-cut, re-dress, turn, re-tint, rescale or replace one sprite
        // a step, as a companion does between frames; the others hold. A
        // sprite that keeps its art but not its whole look (a pet turning
        // round) must not draw the content its old look left behind.
        let i = rng.range(0, sprites.len() as i64) as usize;
        match rng.range(0, 6) {
            0 => {
                let (dx, dy) = (rng.range(-9, 10) as i32, rng.range(-9, 10) as i32);
                let s = &mut sprites[i];
                s.x += dx;
                s.y += dy;
                s.clip = s.clip.translate(dx, dy);
            }
            1 => sprites[i].alpha = rng.range(1, 256) as u8,
            2 => {
                // Only where the old stamp could draw it: whole across.
                let turned = FreeSprite {
                    flip_x: !sprites[i].flip_x,
                    ..sprites[i].clone()
                };
                if LegacySprite::of(&turned).is_some() {
                    sprites[i] = turned;
                    mirrored += 1;
                }
            }
            3 => {
                sprites[i].tint = (rng.next() as u32) & 0x00FF_FFFF | 0x0040_4040;
                retinted += 1;
            }
            4 => {
                let s = &mut sprites[i];
                let k = if s.w == s.art.w() { 2 } else { 1 };
                s.w = s.art.w() * k;
                s.h = s.art.h() * k;
                s.clip = PxRect::EVERYWHERE;
                rescaled += 1;
            }
            _ => sprites[i] = random_sprite(&mut rng, cw, ch),
        }
        if step % 9 == 4 {
            t.process(format!("\x1b[{};2Hedit {step}", step % ROWS + 1).as_bytes());
        }
        let input = new_input(&mut t, &mut pubr, &sprites);
        let cached = rend.render_input_cached(&mut wc, &input).pixels().to_vec();
        let want = old_frame(&mut rend, &mut t, &sprites);
        assert!(cached == want, "step {step}: {sprites:?}");
    }
    assert!(
        mirrored > 5 && retinted > 5 && rescaled > 5,
        "the walk turns ({mirrored}), re-tints ({retinted}) and rescales ({rescaled}) kept arts"
    );
}

#[test]
fn a_non_integer_scale_is_where_the_floor_map_parts_from_the_old_centre_sample() {
    let mut rend = renderer();
    rend.set_knock_out(false);
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    t.process(b"\x1b[?25l");
    let (aw, ah, dw, dh) = (7u16, 9u16, 10u16, 13u16);
    let art = art_of(aw, ah, |x, y| [(x * 30) as u8, (y * 25) as u8, 90, 255]);
    let s = FreeSprite {
        w: dw,
        h: dh,
        ..FreeSprite::whole(5, 4, art.clone())
    };
    let mut pubr = SpriteLayers::new(SALT);
    let f = rend.render_input(&new_input(&mut t, &mut pubr, std::slice::from_ref(&s)));
    let (pad, top) = (rend.pad(), rend.grid_top());
    for dy in 0..usize::from(dh) {
        for dx in 0..usize::from(dw) {
            let (sx, sy) = (
                dx * usize::from(aw) / usize::from(dw),
                dy * usize::from(ah) / usize::from(dh),
            );
            let i = (sy * usize::from(aw) + sx) * 4;
            let d = art.data();
            let want = (u32::from(d[i]) << 16) | (u32::from(d[i + 1]) << 8) | u32::from(d[i + 2]);
            assert_eq!(
                f.pixels[(top + 4 + dy) * f.width + pad + 5 + dx] & 0x00FF_FFFF,
                want,
                "texel floor(d·a/D) at ({dx},{dy})"
            );
        }
    }
    let old = old_frame(&mut rend, &mut t, std::slice::from_ref(&s));
    assert!(
        f.pixels != old,
        "at 7→10 × 9→13 the floor map and the old centre sample pick different texels"
    );
}
