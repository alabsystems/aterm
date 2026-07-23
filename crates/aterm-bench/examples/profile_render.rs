// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Profiling harness: drive `Renderer::render_input` (the CPU rasterize hot path) on a
// busy grid in a tight loop so a sampling profiler can attribute time within the
// renderer. Two DIFFERENT grids are alternated so every frame is a full-damage repaint
// (rendering the same input twice would hit the damage cache and measure nothing).
//   cargo build --profile profiling -p aterm-bench --example profile_render
//   ./target/profiling/examples/profile_render 6000   # arg = frames
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

fn busy_term(rows: usize, cols: usize, shift: usize) -> Terminal {
    let mut t = Terminal::new(rows as u16, cols as u16);
    let alpha = b"abcdefghijklmnopqrstuvwxyz0123456789 ";
    let mut line = Vec::with_capacity(cols + 16);
    for r in 0..rows {
        line.clear();
        line.extend_from_slice(b"\x1b[3");
        line.push(b'1' + ((r + shift) % 6) as u8);
        line.push(b'm');
        for c in 0..cols {
            line.push(alpha[(r + c + shift) % alpha.len()]);
        }
        line.extend_from_slice(b"\x1b[0m\r\n");
        t.process(&line);
    }
    t
}

fn main() {
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font; render profile not run");
        return;
    };
    let (rows, cols) = (50usize, 200usize);
    // Two grids with different content + colours → alternating them forces a full-damage
    // repaint every frame (the worst-case rasterize: a vim/tmux full-screen redraw).
    let mut ta = busy_term(rows, cols, 0);
    let mut tb = busy_term(rows, cols, 7);
    let ia = ta.cell_frame(rows, cols);
    let ib = tb.cell_frame(rows, cols);

    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(6000);
    let mut sink = 0usize;
    for i in 0..iters {
        let f = r.render_input(if i % 2 == 0 { &ia } else { &ib });
        sink = sink.wrapping_add(f.pixels.len());
    }
    std::hint::black_box(sink);
    eprintln!("rendered {iters} full-damage frames of {rows}x{cols}");
}
