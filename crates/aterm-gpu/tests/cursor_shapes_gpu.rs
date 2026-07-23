// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Cursor-shape gate for the GPU renderer: for every DECSCUSR shape (block /
// underline / bar), the frontend HollowBlock override, the off blink phase,
// and a DECTCEM-hidden cursor, the GPU frame must (a) show the same exact
// pixel pattern the CPU asserts (strip-only / outline-only / nothing) and
// (b) match the CPU frame within the usual small per-channel tolerance.
// Styles are driven end-to-end by feeding DECSCUSR bytes through a Terminal.
//
// Gated: if there is no GPU or no system font, the tests no-op (return).

use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_render::{Frame, Renderer, Theme};

const CURSOR: u32 = 0x0050_FA7B; // Theme::default().cursor
const PX: f32 = 18.0;

fn rr(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
fn gg(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
fn bb(p: u32) -> i32 {
    (p & 0xff) as i32
}

/// Max per-channel absolute difference between two same-sized frames.
fn max_channel_delta(a: &Frame, b: &Frame) -> i32 {
    let mut m = 0;
    for (&pa, &pb) in a.pixels.iter().zip(b.pixels.iter()) {
        m = m.max((rr(pa) - rr(pb)).abs());
        m = m.max((gg(pa) - gg(pb)).abs());
        m = m.max((bb(pa) - bb(pb)).abs());
    }
    m
}

/// Per-channel closeness to a packed `0x00RRGGBB` colour. Tolerance is 2 LSB, not 1:
/// at a GLYPH-EDGE pixel the GPU's fixed-function linear blend can round 1 LSB below
/// the CPU's f64 sRGB blend, and that single LSB is enough to push one edge pixel
/// across a hard ±1 band — making the exact `cursor_positions` set differ by one
/// element on some GPUs (reproduced on NVIDIA Blackwell/Vulkan). This is the accepted
/// sub-perceptual cross-backend AA rounding (the whole-frame parity gate is `delta<=8`
/// everywhere), NOT a rendering error. ±2 admits only that edge LSB: bg (0x111318) and
/// fg (0xD0D0D0) sit ~200 LSB from the cursor green, so the flat-fill shape-count
/// assertions (underline/bar/hollow) still count exactly the same pixels.
fn near_cursor(p: u32) -> bool {
    (rr(p) - rr(CURSOR)).abs() <= 2
        && (gg(p) - gg(CURSOR)).abs() <= 2
        && (bb(p) - bb(CURSOR)).abs() <= 2
}

/// All (x, y) positions whose pixel is the cursor colour (within 1 LSB).
fn cursor_positions(f: &Frame) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for y in 0..f.height {
        for x in 0..f.width {
            if near_cursor(f.pixels[y * f.width + x]) {
                out.push((x, y));
            }
        }
    }
    out
}

/// Both renderers at the same px/theme, or `None` to skip (no GPU / no font).
fn renderers() -> Option<(Renderer, aterm_gpu::GpuRenderer)> {
    let theme = Theme::default();
    let gpu = match aterm_gpu::GpuRenderer::new(PX, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return None;
        }
    };
    let Some(cpu) = Renderer::from_system(PX, theme) else {
        eprintln!("SKIP: no system monospace font");
        return None;
    };
    Some((cpu, gpu))
}

/// A 2x4 terminal (text "a", cursor back over it) with `bytes` processed —
/// the glyph-under-cursor case, the harshest for shape parity.
fn term_with(bytes: &[u8]) -> Terminal {
    let mut t = Terminal::new(2, 4);
    t.process(b"a\x1b[1;1H");
    t.process(bytes);
    t
}

/// Render `term` on both paths and assert pixel parity (1-LSB fills, the
/// glyph blend within the usual gpu_matches_cpu tolerance). Returns the GPU
/// frame for the pattern assertions.
fn parity(
    cpu: &mut Renderer,
    gpu: &mut aterm_gpu::GpuRenderer,
    win: &mut aterm_gpu::WindowGpu,
    term: &mut Terminal,
    label: &str,
) -> Frame {
    // A-3: the engine builds the snapshot; both renderers consume the same value.
    let input = term.cell_frame(2, 4);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(win, &input, None);
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "{label}: dimensions differ"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("{label}: GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "{label}: GPU/CPU pixels diverge (delta {delta} > 8)"
    );
    // The cursor-coloured pattern itself must agree EXACTLY (fills are flat
    // colour on both paths; 1 LSB covers Rgba8 round-tripping).
    assert_eq!(
        cursor_positions(&cpu_frame),
        cursor_positions(&gpu_frame),
        "{label}: cursor-coloured pixel positions differ between CPU and GPU"
    );
    gpu_frame
}

#[test]
fn gpu_block_cursor_matches_cpu() {
    let Some((mut cpu, mut gpu)) = renderers() else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let mut term = term_with(b"\x1b[2 q"); // steady block
    let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, "block");
    let pos = cursor_positions(&f);
    // The block fill covers the cell except the glyph cut-out; well over half
    // the cell stays cursor-coloured and nothing outside the cell does.
    assert!(
        pos.len() > cw * ch / 2,
        "block: too few cursor pixels ({})",
        pos.len()
    );
    assert!(
        pos.iter().all(|&(x, y)| x < cw && y < ch),
        "block: cursor pixels outside cell"
    );
}

#[test]
fn gpu_underline_cursor_matches_cpu() {
    let Some((mut cpu, mut gpu)) = renderers() else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let mut term = term_with(b"\x1b[4 q"); // steady underline
    let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, "underline");
    let t = (ch / 8).max(2);
    let pos = cursor_positions(&f);
    assert_eq!(
        pos.len(),
        cw * t,
        "underline: should fill exactly the bottom strip"
    );
    assert!(
        pos.iter().all(|&(x, y)| x < cw && y >= ch - t && y < ch),
        "underline: cursor pixels outside the bottom strip"
    );
}

#[test]
fn gpu_bar_cursor_matches_cpu() {
    let Some((mut cpu, mut gpu)) = renderers() else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let mut term = term_with(b"\x1b[6 q"); // steady bar
    let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, "bar");
    let t = (cw / 8).max(2);
    let pos = cursor_positions(&f);
    // The bar strip may cross the glyph's left edge: every strip pixel is
    // cursor-coloured and no cursor colour leaks outside the strip.
    assert_eq!(pos.len(), t * ch, "bar: should fill exactly the left strip");
    assert!(
        pos.iter().all(|&(x, y)| x < t && y < ch),
        "bar: cursor pixels outside the left strip"
    );
}

#[test]
fn gpu_hollow_block_matches_cpu() {
    let Some((mut cpu, mut gpu)) = renderers() else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    cpu.set_cursor_style_override(Some(CursorStyle::HollowBlock));
    gpu.set_cursor_style_override(Some(CursorStyle::HollowBlock));
    let mut term = term_with(b"");
    let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, "hollow");
    let t = (ch / 16).max(1);
    let border = 2 * cw * t + 2 * t * (ch - 2 * t);
    let pos = cursor_positions(&f);
    assert_eq!(
        pos.len(),
        border,
        "hollow: should paint exactly the outline"
    );
    let (mx, my) = (cw / 2, ch / 2);
    assert!(
        !near_cursor(f.pixels[my * f.width + mx]),
        "hollow: center must stay unfilled"
    );
}

#[test]
fn gpu_blink_phase_and_hidden_suppress_cursor() {
    let Some((mut cpu, mut gpu)) = renderers() else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();

    // Blinking block, phase off: no cursor pixels on either path.
    let mut term = term_with(b"\x1b[1 q");
    cpu.set_cursor_blink_phase(false);
    gpu.set_cursor_blink_phase(false);
    let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, "blink-off");
    assert!(
        cursor_positions(&f).is_empty(),
        "blink phase off -> no cursor pixels"
    );

    // Phase back on: the cursor returns.
    cpu.set_cursor_blink_phase(true);
    gpu.set_cursor_blink_phase(true);
    let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, "blink-on");
    assert!(
        !cursor_positions(&f).is_empty(),
        "blink phase on -> cursor drawn"
    );

    // DECTCEM hidden: no cursor pixels regardless of style or phase.
    let mut term = term_with(b"\x1b[?25l");
    let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, "hidden");
    assert!(
        cursor_positions(&f).is_empty(),
        "DECTCEM off -> no cursor pixels"
    );
}

#[test]
fn gpu_bolt_matches_cpu() {
    let Some((mut cpu, mut gpu)) = renderers() else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    // Bolt is the frontend's laser-lightning override (not DECSCUSR); like the
    // hollow block it arrives via the style override on both paths.
    cpu.set_cursor_style_override(Some(CursorStyle::Bolt));
    gpu.set_cursor_style_override(Some(CursorStyle::Bolt));
    let mut term = term_with(b"");
    let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, "bolt");
    // The painted pattern is exactly the shared per-row strip union — over a
    // glyph ('a' under the cursor: strips paint over it, no cut-out).
    let rects = aterm_render::cursor_rects(CursorStyle::Bolt, 0, 0, cw, ch);
    let area: usize = rects.iter().map(|&[_, _, w, h]| w * h).sum();
    let pos = cursor_positions(&f);
    assert_eq!(pos.len(), area, "bolt: paint exactly the strip union");
    for &(x, y) in &pos {
        let [sx, _, sw, _] = rects[y];
        assert!(
            x >= sx && x < sx + sw,
            "bolt: pixel ({x},{y}) outside its row strip"
        );
    }
}
