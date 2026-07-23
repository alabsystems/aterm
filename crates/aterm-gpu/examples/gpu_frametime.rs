// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Honest GPU frame-time: separates the on-screen render cost (render_no_readback
// — encode + GPU execute, what a window pays) from the offscreen verification
// cost (render — adds the synchronous texture→CPU readback). Reports ms/frame
// and the equivalent FPS for a couple of grid sizes.
//   cargo run -p aterm-gpu --release --example gpu_frametime

use std::time::Instant;

use aterm_core::terminal::Terminal;
use aterm_gpu::GpuRenderer;
use aterm_render::Theme;

fn fill(rows: u16, cols: u16) -> Terminal {
    let mut t = Terminal::new(rows, cols);
    // A busy, colourful screen (worst-ish case: every cell has a glyph + SGR).
    for r in 0..rows {
        let sgr = format!("\x1b[1;38;5;{}m", 16 + (r % 200));
        t.process(sgr.as_bytes());
        for c in 0..cols {
            let ch = char::from(b'!' + ((r as u8 + c as u8) % 90));
            t.process(&[ch as u8]);
        }
        t.process(b"\x1b[0m\r\n");
    }
    t
}

fn bench(label: &str, rows: u16, cols: u16, r: &mut GpuRenderer) {
    let mut term = fill(rows, cols);
    let (ru, cu) = (rows as usize, cols as usize);
    // A-3: the engine builds the snapshot once; the renderer consumes the value.
    // (The frame-time being measured is the GPU encode/execute, not the snapshot.)
    let input = term.cell_frame(ru, cu);
    let mut win = aterm_gpu::WindowGpu::new();
    // warm up (atlas, pipelines, first submit)
    for _ in 0..10 {
        r.render_no_readback(&mut win, &input);
    }
    let n = 200;
    let t0 = Instant::now();
    for _ in 0..n {
        r.render_no_readback(&mut win, &input);
    }
    let render_ms = t0.elapsed().as_secs_f64() * 1e3 / n as f64;

    let t1 = Instant::now();
    for _ in 0..n {
        let _ = r.render_input(&mut win, &input, None);
    }
    let readback_ms = t1.elapsed().as_secs_f64() * 1e3 / n as f64;

    println!(
        "{label:>9} {cols}x{rows}: render-only {render_ms:6.3} ms ({:.0} fps)  |  +readback {readback_ms:6.3} ms",
        1000.0 / render_ms
    );
}

fn main() {
    let mut r = match GpuRenderer::new(18.0, Theme::default()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("SKIP: {e}");
            return;
        }
    };
    let (name, backend) = r.adapter();
    println!("GPU: {name} ({backend})");
    bench("standard", 24, 80, &mut r);
    bench("large", 50, 200, &mut r);
    bench("huge", 100, 400, &mut r);
}
