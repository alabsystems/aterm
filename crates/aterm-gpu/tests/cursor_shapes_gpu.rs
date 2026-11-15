// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cursor-shape parity tests between CPU and GPU renderers.

use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_render::{Frame, RenderInput, Renderer, Theme};

use crate::common::{backends, bb, gg, max_channel_delta_frame as max_channel_delta, rr};

const CURSOR: u32 = 0x0050_FA7B; // Theme::default().cursor
const PX: f32 = 18.0;

fn near_cursor(p: u32) -> bool {
    near_color(p, CURSOR)
}

fn near_color(p: u32, color: u32) -> bool {
    (rr(p) - rr(color)).abs() <= 2
        && (gg(p) - gg(color)).abs() <= 2
        && (bb(p) - bb(color)).abs() <= 2
}

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

fn renderers() -> (Renderer, aterm_gpu::GpuRenderer) {
    backends(PX, Theme::default())
}

fn term_with(bytes: &[u8]) -> Terminal {
    let mut t = Terminal::new(2, 4);
    t.process(b"a\x1b[1;1H");
    t.process(bytes);
    t
}

fn parity(
    cpu: &mut Renderer,
    gpu: &mut aterm_gpu::GpuRenderer,
    win: &mut aterm_gpu::WindowGpu,
    term: &mut Terminal,
    label: &str,
) -> Frame {
    let input = term.cell_frame(2, 4);
    parity_input(cpu, gpu, win, &input, label)
}

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
    assert_eq!(
        cursor_positions(&cpu_frame),
        cursor_positions(&gpu_frame),
        "{label}: cursor-coloured pixel positions differ between CPU and GPU"
    );
    gpu_frame
}

struct Shape {
    label: &'static str,
    /// DECSCUSR bytes fed through the terminal (the terminal-owned styles).
    decscusr: &'static [u8],
    /// The frontend style override (HollowBlock, Bolt), never set through DECSCUSR.
    style_override: Option<CursorStyle>,
    /// The exact cursor-coloured pixel count for a `(cw, ch)` cell, or `None` for the block,
    /// whose fill has a glyph cut-out and only has to cover well over half the cell.
    count: fn(usize, usize) -> Option<usize>,
    /// Every cursor-coloured pixel `(x, y)` lies where this says, for `(cw, ch)`.
    inside: fn(usize, usize, usize, usize) -> bool,
}

fn cursor_rects(style: CursorStyle, cw: usize, ch: usize) -> Vec<[usize; 4]> {
    let mut rects = Vec::new();
    aterm_render::for_each_cursor_rect(style, 0, 0, cw, ch, |rect| rects.push(rect));
    rects
}

fn bolt_rects(cw: usize, ch: usize) -> Vec<[usize; 4]> {
    cursor_rects(CursorStyle::Bolt, cw, ch)
}

const SHAPES: &[Shape] = &[
    Shape {
        label: "block",
        decscusr: b"\x1b[2 q",
        style_override: None,
        count: |_, _| None,
        inside: |x, y, cw, ch| x < cw && y < ch,
    },
    Shape {
        label: "underline",
        decscusr: b"\x1b[4 q",
        style_override: None,
        count: |cw, ch| Some(cw * (ch / 8).max(2)),
        inside: |x, y, cw, ch| x < cw && y >= ch - (ch / 8).max(2) && y < ch,
    },
    Shape {
        label: "bar",
        decscusr: b"\x1b[6 q",
        style_override: None,
        count: |cw, ch| Some((cw / 8).max(2) * ch),
        inside: |x, y, cw, ch| x < (cw / 8).max(2) && y < ch,
    },
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
    let (mut cpu, mut gpu) = renderers();
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
    let (mut cpu, mut gpu) = renderers();
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

    let rects = cursor_rects(CursorStyle::SteadyBar, cw, ch);
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
    let (mut cpu, mut gpu) = renderers();
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let mut term = Terminal::new(2, 4);
    term.process(b"\x1b[4 q"); // terminal owns steady underline
    let mut input = term.cell_frame(2, 4);
    input.cursor_effect_style_override = Some(CursorStyle::Bolt);

    let effect = parity_input(&mut cpu, &mut gpu, &mut win, &input, "effect-bolt");
    let bolt_area: usize = bolt_rects(cw, ch).iter().map(|&[_, _, w, h]| w * h).sum();
    assert_eq!(cursor_positions(&effect).len(), bolt_area);
    assert_eq!(input.cursor_style, CursorStyle::SteadyUnderline);

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
    let (mut cpu, mut gpu) = renderers();
    let mut win = aterm_gpu::WindowGpu::new();

    let mut term = term_with(b"\x1b[1 q");
    cpu.set_cursor_blink_phase(false);
    gpu.set_cursor_blink_phase(false);
    let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, "blink-off");
    assert!(
        cursor_positions(&f).is_empty(),
        "blink phase off -> no cursor pixels"
    );

    cpu.set_cursor_blink_phase(true);
    gpu.set_cursor_blink_phase(true);
    let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, "blink-on");
    assert!(
        !cursor_positions(&f).is_empty(),
        "blink phase on -> cursor drawn"
    );

    let mut term = term_with(b"\x1b[?25l");
    let f = parity(&mut cpu, &mut gpu, &mut win, &mut term, "hidden");
    assert!(
        cursor_positions(&f).is_empty(),
        "DECTCEM off -> no cursor pixels"
    );
}

/// The rainbow block's keyline as the window draws it: the GPU's cursor stream lays the same
/// one-pixel ring just inside the block as the CPU renderer, in the colour both work from the
/// fill they draw (after the minimum-contrast floor) and the cell's ground
/// (`aterm_render::block_keyline_colour`), over the fill and its letter; a bar and the host's
/// hollow outline wear none. The control is the same frame with no tint: its edge is the fill.
#[test]
fn gpu_block_keyline_matches_cpu_and_follows_the_drawn_fill() {
    const FILL: u32 = 0x0002_8402;
    const TINT: u32 = 0x0013_1B13;
    let (mut cpu, mut gpu) = renderers();
    let (cw, ch) = cpu.cell_size();
    let edge = |f: &Frame| -> Vec<u32> {
        let mut px = Vec::new();
        for x in 0..cw {
            px.push(f.pixels[x]);
            px.push(f.pixels[(ch - 1) * f.width + x]);
        }
        for y in 1..ch - 1 {
            px.push(f.pixels[y * f.width]);
            px.push(f.pixels[y * f.width + cw - 1]);
        }
        px
    };
    for min_contrast in [1.0f32, 7.0] {
        cpu.set_minimum_contrast(min_contrast);
        gpu.set_minimum_contrast(min_contrast);
        let label = format!("keyline at minimum contrast {min_contrast}");
        let mut win = aterm_gpu::WindowGpu::new();
        let mut term = term_with(b"\x1b[2 q");
        let mut input = term.cell_frame(2, 4);
        // The fill the renderers draw: floored against the caret cell's ground at their
        // effective floor (the knob, under the vibrancy rule).
        let ground = aterm_render::rgb_to_u32(input.cells[0][0].bg);
        let drawn = aterm_render::floor_cursor_fill(FILL, ground, cpu.effective_minimum_contrast());
        assert_eq!(
            min_contrast > 1.0,
            drawn != FILL,
            "fixture: the floor moves the fill"
        );
        let line = aterm_render::block_keyline_colour(drawn, TINT, ground);
        assert_ne!(line, drawn, "fixture: the keyline is not the fill");
        input.cursor_fill_override = Some(FILL);
        input.cursor_keyline_tint = Some(TINT);
        let cpu_frame = cpu.render_input(&input);
        let gpu_frame = gpu.render_input(&mut win, &input, None);
        let delta = max_channel_delta(&cpu_frame, &gpu_frame);
        assert!(delta <= 8, "{label}: GPU/CPU pixels diverge by {delta}");
        for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
            let ring = edge(frame);
            assert!(
                ring.iter().all(|&p| near_color(p, line)),
                "{label}: {name} draws the block's edge off the keyline {line:06x}: {:06x?}",
                ring.iter()
                    .filter(|&&p| !near_color(p, line))
                    .take(4)
                    .collect::<Vec<_>>()
            );
            assert!(
                near_color(frame.pixels[frame.width + 1], drawn),
                "{label}: {name} keeps the fill {drawn:06x} inside the keyline"
            );
        }
        // The control: no tint, and the block's edge is the fill it draws.
        input.cursor_keyline_tint = None;
        let plain = gpu.render_input(&mut win, &input, None);
        assert!(
            edge(&plain).iter().all(|&p| near_color(p, drawn)),
            "{label}: control, the GPU block with no tint draws its edge in the fill"
        );
        input.cursor_keyline_tint = Some(TINT);
        // A bar and the host's hollow outline wear none.
        for (style, decscusr) in [
            (None, &b"\x1b[6 q"[..]),
            (Some(CursorStyle::HollowBlock), b""),
        ] {
            cpu.set_cursor_style_override(style);
            gpu.set_cursor_style_override(style);
            let mut term = term_with(decscusr);
            let mut thin = term.cell_frame(2, 4);
            thin.cursor_fill_override = Some(FILL);
            thin.cursor_keyline_tint = Some(TINT);
            for (name, frame) in [
                ("CPU", cpu.render_input(&thin)),
                ("GPU", gpu.render_input(&mut win, &thin, None)),
            ] {
                assert!(
                    !frame.pixels.iter().any(|&p| near_color(p, line)),
                    "{label}: {name} {style:?} {decscusr:?} wears the block's keyline"
                );
            }
            cpu.set_cursor_style_override(None);
            gpu.set_cursor_style_override(None);
        }
    }
}
