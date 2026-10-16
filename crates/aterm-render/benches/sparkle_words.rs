// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sparkle Words v2.1's frame-cost gates (design §7.4, §14 P5): the pass-1c
//! row cost of a sprite under every row, and the ink merge-walk's overhead at
//! the 512-cell cap, each against a baseline of the same frame. Their numbers
//! land in PROOF_CARRYING_PERFORMANCE.md ("Sparkle Words v2.1").
//!
//! ```sh
//! targo --unverified bench -p aterm-render --bench sparkle_words
//! ```

// The test modules, mounted for their fixtures; their laws are not compiled.
#[allow(dead_code, unused_imports)]
#[path = "../tests/cat_sprites.rs"]
mod cat_sprites;
#[allow(dead_code, unused_imports)]
#[path = "../tests/ink.rs"]
mod ink;

fn main() {
    if cfg!(debug_assertions) {
        panic!("the bounds are release-build numbers: run it with `targo --unverified bench`");
    }
    bench_render_row_under_sprites();
    bench_ink_apply();
}

/// §7.4/§14 P5 perf gate — `bench_render_row_under_sprites`: the pass-1c row
/// cost. A 120×40 text frame with ONE cat-scale sprite quad (80 px × cell)
/// under EVERY row is rendered against a no-sprite baseline of the same frame;
/// both full-frame medians are reported plus the per-row pass-1c delta. Runs
/// alternate so drift hits both sides. The measured numbers land in
/// PROOF_CARRYING_PERFORMANCE.md ("Sparkle Words v2.1").
fn bench_render_row_under_sprites() {
    use std::time::Instant;
    let mut rend = cat_sprites::renderer();
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
    sprite_input.cat_atlas = Some(std::sync::Arc::new(cat_sprites::patterned_atlas(
        128, ch as u32, 1,
    )));
    // One cat-scale quad (≈ a settled HeadPeek body slice: 80 px wide, one full
    // row band) under EVERY row — 40 pass-1c stamps per frame.
    sprite_input.cat_quads = (0..rows as u16)
        .map(|r| {
            cat_sprites::quad(
                r,
                [(4 * cw) as u16, r * ch as u16, 80, ch as u16],
                [0, 0, 80, ch as u16],
            )
        })
        .collect();

    // Warm both paths.
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
         — pass-1c cost {:.2} us/row (120x40, {}x{} px cells, 80 px quad/row)",
        mb.as_nanos() as f64 / rows as f64 / 1000.0,
        ms.as_nanos() as f64 / rows as f64 / 1000.0,
        per_row_ns / 1000.0,
        cw,
        ch
    );
    assert!(
        per_row_ns < 10_000.0,
        "§7.4 gate: pass-1c row cost {per_row_ns:.0} ns/row >= 10 us/row"
    );
}

/// §7.4/§14 P5 perf gate — `bench_ink_apply`: the §4.1 merge-walk overhead of
/// a FULL 512-InkCell load (the MAX_INK_CELLS cap) in a full-frame render vs a
/// no-ink baseline of the identical frame; both medians are reported. The
/// merge-walk is O(1)/cell — one pointer compare per column — so the overhead
/// must stay in the noise class, not the per-cell-binary-search class. Runs
/// alternate so drift hits both sides. The measured numbers land in
/// PROOF_CARRYING_PERFORMANCE.md ("Sparkle Words v2.1").
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
    // Exactly 512 InkCells (the cap), sorted (row, col), spread over every row
    // — 12–13 word-shaped cells per row, the §7.3 worst-case ink screen.
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

    // Warm both paths.
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
