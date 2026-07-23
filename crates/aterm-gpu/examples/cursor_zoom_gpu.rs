// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GPU-PATH screenshot of the cursor "zoom" comet — the REAL pixels the window
// shows, INCLUDING the GPU-only gaussian bloom (which the CPU/`aterm-render` demo
// cannot show). Renders the SAME diagonal sweep through `GpuRenderer` and reads the
// offscreen back to a PNG: the lumen comet with bloom ON (the default) vs OFF (the
// byte-parity base), plus a couple of extra styles. The comet geometry comes from
// the SHARED `aterm_render::comet_glow_quads`, so this preview matches the live
// cursor exactly.
//   cargo run -p aterm-gpu --example cursor_zoom_gpu -- <out_dir>

use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuRenderer, WindowGpu};
use aterm_render::{BeamClip, CometSample, GlowQuad, Theme, comet_glow_quads};

fn lerp_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let m = |sh: u32| {
        let ca = ((a >> sh) & 0xff) as f32;
        let cb = ((b >> sh) & 0xff) as f32;
        (ca + (cb - ca) * t + 0.5) as u32
    };
    (m(16) << 16) | (m(8) << 8) | m(0)
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let theme = Theme::default();
    let px = 28.0;
    let mut gpu = GpuRenderer::new(px, theme).expect("create GpuRenderer");
    let (name, backend) = gpu.adapter();
    eprintln!("GPU: {name} (backend {backend})");
    let (cw, ch) = gpu.cell_size();

    let (rows, cols) = (12usize, 46usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("$ aterm \u{2014} GPU bloom cursor zoom\r\n".as_bytes());

    // Bresenham diagonal origin→head (matches the CPU `cursor_glow_demo`).
    let (o_row, o_col, h_row, h_col) = (10i32, 3i32, 1i32, 34i32);
    let cells = {
        let (mut r0, mut c0) = (o_row, o_col);
        let (dr, dc) = ((h_row - r0).abs(), (h_col - c0).abs());
        let (sr, sc) = (
            if r0 < h_row { 1 } else { -1 },
            if c0 < h_col { 1 } else { -1 },
        );
        let mut err = dc - dr;
        let mut v = Vec::new();
        loop {
            v.push((r0, c0));
            if r0 == h_row && c0 == h_col {
                break;
            }
            let e2 = 2 * err;
            if e2 > -dr {
                err -= dr;
                c0 += sc;
            }
            if e2 < dc {
                err += dc;
                r0 += sr;
            }
        }
        v
    };
    let ncells = cells.len();
    let (gw, gh) = (cols * cw, rows * ch);
    let core_thick = (ch as f32 * 0.13).max(2.0);
    let straighten = (cw.max(ch) as f32) * 0.8;

    let cursor = theme.cursor & 0x00FF_FFFF;
    let accent = 0x007A_A2F7u32;
    #[allow(clippy::type_complexity)]
    let styles: [(&str, Box<dyn Fn(f32) -> u32>); 3] = [
        (
            "lumen",
            Box::new(move |pos: f32| lerp_rgb(accent, cursor, pos)),
        ),
        (
            "laser",
            Box::new(|pos: f32| lerp_rgb(0x0040_C0FF, 0x00FF_FFFF, pos * pos)),
        ),
        (
            "ember",
            Box::new(|pos: f32| lerp_rgb(0x00E0_4A00, 0x00FF_F0C0, pos)),
        ),
    ];

    let build_quads = |color_at: &dyn Fn(f32) -> u32| -> Vec<GlowQuad> {
        let run: Vec<CometSample> = cells
            .iter()
            .enumerate()
            .map(|(i, (rr, cc))| {
                let pos = (i as f32 + 1.0) / ncells as f32;
                CometSample {
                    x: (*cc as f32 + 0.5) * cw as f32,
                    y: (*rr as f32 + 0.5) * ch as f32,
                    cov: ((40.0 + 175.0 * pos) * 0.9) as u8,
                    pos,
                }
            })
            .collect();
        let mut q = Vec::new();
        // Identity geometry (no chrome band in the preview): box == the grid extents.
        comet_glow_quads(
            &mut q,
            BeamClip::grid(gw, gh, ch),
            &run,
            core_thick,
            straighten,
            color_at,
        );
        q
    };

    let mut win = WindowGpu::new();
    let mut shot =
        |gpu: &mut GpuRenderer, win: &mut WindowGpu, quads: Vec<GlowQuad>, label: &str| {
            let nq = quads.len();
            let mut input = term.cell_frame(rows, cols);
            input.cursor_glow_add = quads;
            let frame = gpu.render_input(win, &input, None);
            let p = format!("{dir}/gpu_zoom_{label}.png");
            std::fs::write(&p, frame.to_png()).expect("write png");
            eprintln!(
                "wrote {p}  ({}x{}, {nq} quads, bloom {})",
                frame.width,
                frame.height,
                if gpu.bloom_enabled() { "ON" } else { "OFF" }
            );
        };

    // Lumen: base (bloom OFF) then full GPU bloom (the default) — same input, so the
    // difference IS the bloom. Bloom ON first so the bloom target is built with the
    // offscreen; the OFF pass then reuses that offscreen with the bloom gated out.
    let lumen_quads = build_quads(styles[0].1.as_ref());
    gpu.set_bloom(true);
    shot(&mut gpu, &mut win, lumen_quads.clone(), "lumen_bloom");
    gpu.set_bloom(false);
    shot(&mut gpu, &mut win, lumen_quads, "lumen_base");

    // The other styles, with bloom on.
    gpu.set_bloom(true);
    for (sname, color_at) in styles.iter().skip(1) {
        let quads = build_quads(color_at.as_ref());
        shot(&mut gpu, &mut win, quads, &format!("{sname}_bloom"));
    }
}
