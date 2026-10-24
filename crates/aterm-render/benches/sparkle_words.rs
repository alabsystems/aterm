// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frame-cost benchmarks for a sprite layer under every row and the ink merge-walk.

#[allow(dead_code, unused_imports)]
#[path = "../tests/ink.rs"]
mod ink;
#[allow(dead_code, unused_imports)]
#[path = "../tests/common/sprites.rs"]
mod sprites;

fn main() {
    if cfg!(debug_assertions) {
        panic!("the bounds are release-build numbers: run it with `targo --unverified bench`");
    }
    bench_render_row_under_sprites();
    bench_ink_apply();
}

fn bench_render_row_under_sprites() {
    use std::time::Instant;
    let mut rend = aterm_render::Renderer::from_system(18.0, aterm_render::Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (40usize, 120usize);
    let mut term = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let line = "the quick brown fox jumps over the lazy dog 0123456789 ".repeat(3);
    for r in 0..rows {
        term.process(format!("\x1b[{};1H{}", r + 1, &line[..cols]).as_bytes());
    }

    let base_input = term.cell_frame(rows, cols);
    let mut sprite_input = base_input.clone();
    let art = sprites::patterned(80, ch as u16);
    let row_sprites: Vec<aterm_render::sprites::FreeSprite> = (0..rows)
        .map(|r| sprites::under((4 * cw) as i32, (r * ch) as i32, art.clone()))
        .collect();
    sprites::put(&mut sprite_input, &row_sprites);
    // The rain's kind of row layer (no knock-out), as the old per-row quads were.
    for l in &mut sprite_input.fx_layers {
        l.knock = false;
    }

    for _ in 0..4 {
        let _ = rend.render_input(&base_input);
        let _ = rend.render_input(&sprite_input);
    }
    let iters = 60usize;
    let (mut t_base, mut t_sprite) = (Vec::with_capacity(iters), Vec::with_capacity(iters));
    for _ in 0..iters {
        let s = Instant::now();
        let _ = rend.render_input(&base_input);
        t_base.push(s.elapsed());
        let s = Instant::now();
        let _ = rend.render_input(&sprite_input);
        t_sprite.push(s.elapsed());
    }
    t_base.sort();
    t_sprite.sort();
    let (mb, ms) = (t_base[iters / 2], t_sprite[iters / 2]);
    let per_row_ns = (ms.as_nanos() as i128 - mb.as_nanos() as i128) as f64 / rows as f64;
    println!(
        "bench_render_row_under_sprites: baseline full-frame median {mb:?} \
         ({:.1} us/row), with a sprite under EVERY row median {ms:?} ({:.1} us/row) \
         — sprite cost {:.2} us/row (120x40, {}x{} px cells, an 80 px sprite layer a row)",
        mb.as_nanos() as f64 / rows as f64 / 1000.0,
        ms.as_nanos() as f64 / rows as f64 / 1000.0,
        per_row_ns / 1000.0,
        cw,
        ch
    );
    assert!(
        per_row_ns < 10_000.0,
        "§7.4 gate: sprite row cost {per_row_ns:.0} ns/row >= 10 us/row"
    );
}

fn bench_ink_apply() {
    use std::time::Instant;
    let mut rend = ink::renderer();
    let (rows, cols) = (40usize, 120usize);
    let mut term = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let line = "the quick brown fox jumps over the lazy dog 0123456789 ".repeat(3);
    for r in 0..rows {
        term.process(format!("\x1b[{};1H{}", r + 1, &line[..cols]).as_bytes());
    }

    let base_input = term.cell_frame(rows, cols);
    let mut inked_input = base_input.clone();
    let mut ink = Vec::with_capacity(512);
    'fill: for r in 0..rows as u16 {
        for c in 0..13u16 {
            if ink.len() == 512 {
                break 'fill;
            }
            let col = c * 9 + (r % 3); // staggered word-ish spread, still sorted
            ink.push(aterm_core::render::InkCell {
                row: r,
                col,
                color: [0x7C, 0xC8, 0xFF],
            });
        }
    }
    assert_eq!(ink.len(), 512, "the MAX_INK_CELLS worst case");
    inked_input.ink = ink;

    for _ in 0..4 {
        let _ = rend.render_input(&base_input);
        let _ = rend.render_input(&inked_input);
    }
    let iters = 60usize;
    let (mut t_base, mut t_ink) = (Vec::with_capacity(iters), Vec::with_capacity(iters));
    for _ in 0..iters {
        let s = Instant::now();
        let _ = rend.render_input(&base_input);
        t_base.push(s.elapsed());
        let s = Instant::now();
        let _ = rend.render_input(&inked_input);
        t_ink.push(s.elapsed());
    }
    t_base.sort();
    t_ink.sort();
    let (mb, mi) = (t_base[iters / 2], t_ink[iters / 2]);
    let overhead_us = (mi.as_nanos() as i128 - mb.as_nanos() as i128) as f64 / 1000.0;
    println!(
        "bench_ink_apply: no-ink full-frame median {mb:?}, 512-InkCell merge-walk \
         median {mi:?} — overhead {overhead_us:.1} us/frame ({:.1} ns/inked cell; \
         120x40 grid)",
        overhead_us * 1000.0 / 512.0
    );
    assert!(
        overhead_us < 500.0,
        "§7.4 gate: 512-cell ink apply overhead {overhead_us:.0} us >= 500 us/frame"
    );
}
