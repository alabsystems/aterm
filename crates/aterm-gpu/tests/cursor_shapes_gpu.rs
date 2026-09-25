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
use aterm_render::{Frame, RenderInput, Renderer, Theme};

mod common;
use common::{backends, bb, gg, max_channel_delta_frame as max_channel_delta, rr};

const CURSOR: u32 = 0x0050_FA7B; // Theme::default().cursor
const PX: f32 = 18.0;

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
    near_color(p, CURSOR)
}

fn near_color(p: u32, color: u32) -> bool {
    (rr(p) - rr(color)).abs() <= 2
        && (gg(p) - gg(color)).abs() <= 2
        && (bb(p) - bb(color)).abs() <= 2
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
    backends(PX, Theme::default())
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
    parity_input(cpu, gpu, win, &input, label)
}

/// Render an already-composed input on both backends and assert the same parity
/// contract as [`parity`].
fn parity_input(
    cpu: &mut Renderer,
    gpu: &mut aterm_gpu::GpuRenderer,
    win: &mut aterm_gpu::WindowGpu,
    input: &RenderInput,
    label: &str,
) -> Frame {
    let cpu_frame = cpu.render_input(input);
    let gpu_frame = gpu.render_input(win, input, None);
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

/// One cursor shape: how it is selected, and the exact pixel pattern both
/// backends must paint for it over the glyph-under-cursor 2x4 terminal.
struct Shape {
    label: &'static str,
    /// DECSCUSR bytes fed through the terminal (the terminal-owned styles).
    decscusr: &'static [u8],
    /// The frontend style override (HollowBlock, Bolt): these arrive through
    /// the override on both paths, never through DECSCUSR.
    style_override: Option<CursorStyle>,
    /// The exact number of cursor-coloured pixels for a `(cw, ch)` cell, or
    /// `None` for the block, whose fill has a glyph cut-out and only has to
    /// cover well over half the cell.
    count: fn(usize, usize) -> Option<usize>,
    /// Every cursor-coloured pixel `(x, y)` lies where this says, for `(cw, ch)`.
    inside: fn(usize, usize, usize, usize) -> bool,
}

/// The bolt's per-row strip union (the frontend's laser-lightning override).
fn bolt_rects(cw: usize, ch: usize) -> Vec<[usize; 4]> {
    aterm_render::cursor_rects(CursorStyle::Bolt, 0, 0, cw, ch)
}

const SHAPES: &[Shape] = &[
    // Steady block: the fill covers the cell except the glyph cut-out, and
    // nothing outside the cell is cursor-coloured.
    Shape {
        label: "block",
        decscusr: b"\x1b[2 q",
        style_override: None,
        count: |_, _| None,
        inside: |x, y, cw, ch| x < cw && y < ch,
    },
    // Steady underline: exactly the bottom strip, `max(ch / 8, 2)` tall.
    Shape {
        label: "underline",
        decscusr: b"\x1b[4 q",
        style_override: None,
        count: |cw, ch| Some(cw * (ch / 8).max(2)),
        inside: |x, y, cw, ch| x < cw && y >= ch - (ch / 8).max(2) && y < ch,
    },
    // Steady bar: exactly the left strip, `max(cw / 8, 2)` wide. The strip may
    // cross the glyph's left edge: every strip pixel is cursor-coloured and no
    // cursor colour leaks outside it.
    Shape {
        label: "bar",
        decscusr: b"\x1b[6 q",
        style_override: None,
        count: |cw, ch| Some((cw / 8).max(2) * ch),
        inside: |x, y, cw, ch| x < (cw / 8).max(2) && y < ch,
    },
    // The unfocused HollowBlock: exactly the `max(ch / 16, 1)`-thick outline,
    // so the centre stays unfilled.
    Shape {
        label: "hollow",
        decscusr: b"",
        style_override: Some(CursorStyle::HollowBlock),
        count: |cw, ch| {
            let t = (ch / 16).max(1);
            Some(2 * cw * t + 2 * t * (ch - 2 * t))
        },
        inside: |x, y, cw, ch| {
            let t = (ch / 16).max(1);
            x < cw && y < ch && (x < t || x >= cw - t || y < t || y >= ch - t)
        },
    },
    // Bolt: exactly the shared per-row strip union, painted over the glyph
    // ('a' under the cursor: strips paint over it, no cut-out).
    Shape {
        label: "bolt",
        decscusr: b"",
        style_override: Some(CursorStyle::Bolt),
        count: |cw, ch| Some(bolt_rects(cw, ch).iter().map(|&[_, _, w, h]| w * h).sum()),
        inside: |x, y, cw, ch| {
            let [sx, _, sw, _] = bolt_rects(cw, ch)[y];
            x >= sx && x < sx + sw
        },
    },
];

#[test]
fn every_cursor_shape_paints_its_exact_pattern_and_matches_cpu() {
    let Some((mut cpu, mut gpu)) = renderers() else {
        return;
    };
    let (cw, ch) = cpu.cell_size();
    for shape in SHAPES {
        let label = shape.label;
        cpu.set_cursor_style_override(shape.style_override);
        gpu.set_cursor_style_override(shape.style_override);
        let mut win = aterm_gpu::WindowGpu::new();
        let mut term = term_with(shape.decscusr);
        let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, label);
        let pos = cursor_positions(&f);
        assert!(!pos.is_empty(), "{label}: no cursor pixels at all");
        match (shape.count)(cw, ch) {
            Some(n) => assert_eq!(pos.len(), n, "{label}: wrong cursor pixel count"),
            None => assert!(
                pos.len() > cw * ch / 2,
                "{label}: too few cursor pixels ({})",
                pos.len()
            ),
        }
        for &(x, y) in &pos {
            assert!(
                (shape.inside)(x, y, cw, ch),
                "{label}: cursor pixel ({x},{y}) outside its shape"
            );
        }
    }
}

#[test]
fn gpu_steady_bar_fill_override_matches_cpu_without_theme_flash() {
    let Some((mut cpu, mut gpu)) = renderers() else {
        return;
    };
    const OVERRIDE: u32 = 0x00FE_017F;
    let (cw, ch) = cpu.cell_size();
    let mut term = term_with(b"\x1b[6 q"); // steady bar over a real glyph
    let mut input = term.cell_frame(2, 4);
    input.cursor_fill_override = Some(OVERRIDE);

    let cpu_frame = cpu.render_input(&input);
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "steady-bar override CPU/GPU output diverges by {delta} > 8"
    );

    let rects = aterm_render::cursor_rects(CursorStyle::SteadyBar, 0, 0, cw, ch);
    let area: usize = rects.iter().map(|&[_, _, w, h]| w * h).sum();
    for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        assert_eq!(
            frame
                .pixels
                .iter()
                .filter(|&&pixel| near_color(pixel, OVERRIDE))
                .count(),
            area,
            "{name}: every steady-bar pixel must use the host override"
        );
        assert!(
            cursor_positions(frame).is_empty(),
            "{name}: theme cursor colour must not flash through the override"
        );
        for &[x, y, w, h] in &rects {
            for py in y..y + h {
                for px in x..x + w {
                    assert!(
                        near_color(frame.pixels[py * frame.width + px], OVERRIDE),
                        "{name}: steady-bar override missing at ({px},{py})"
                    );
                }
            }
        }
    }
}

#[test]
fn gpu_effect_shape_precedence_and_clear_overlays_match_cpu() {
    let Some((mut cpu, mut gpu)) = renderers() else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let mut term = Terminal::new(2, 4);
    term.process(b"\x1b[4 q"); // terminal owns steady underline
    let mut input = term.cell_frame(2, 4);
    input.cursor_effect_style_override = Some(CursorStyle::Bolt);

    let effect = parity_input(&mut cpu, &mut gpu, &mut win, &input, "effect-bolt");
    let bolt_area: usize = aterm_render::cursor_rects(CursorStyle::Bolt, 0, 0, cw, ch)
        .iter()
        .map(|&[_, _, w, h]| w * h)
        .sum();
    assert_eq!(cursor_positions(&effect).len(), bolt_area);
    assert_eq!(input.cursor_style, CursorStyle::SteadyUnderline);

    // The external/backend override remains highest on both implementations.
    cpu.set_cursor_style_override(Some(CursorStyle::HollowBlock));
    gpu.set_cursor_style_override(Some(CursorStyle::HollowBlock));
    let unfocused = parity_input(
        &mut cpu,
        &mut gpu,
        &mut win,
        &input,
        "backend-hollow-over-effect-bolt",
    );
    let t = (ch / 16).max(1);
    let hollow_area = 2 * cw * t + 2 * t * (ch - 2 * t);
    assert_eq!(cursor_positions(&unfocused).len(), hollow_area);

    // Plain capture strips the host/effect shape, exposing terminal DECSCUSR.
    cpu.set_cursor_style_override(None);
    gpu.set_cursor_style_override(None);
    input.clear_overlays();
    let plain = parity_input(&mut cpu, &mut gpu, &mut win, &input, "plain-underline");
    let underline_h = (ch / 8).max(2);
    let pos = cursor_positions(&plain);
    assert_eq!(pos.len(), cw * underline_h);
    assert!(
        pos.iter()
            .all(|&(x, y)| x < cw && y >= ch - underline_h && y < ch)
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
