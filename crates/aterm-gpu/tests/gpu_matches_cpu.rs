// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Correctness gate for the GPU renderer: render the SAME terminal on the CPU
// (`aterm_render::Renderer`, already verified) and on the GPU
// (`aterm_gpu::GpuRenderer`) at the same px/theme, and prove numerically that
// the GPU output matches. The GPU has no human to "see" it; this is its oracle.
//
// Checks:
//   1. identical frame dimensions,
//   2. per-channel pixel delta within a small tolerance across the whole frame
//      (geometry + coverage blend match; only round-vs-floor rounding differs),
//   3. the same SEMANTIC properties the CPU visual-regression test asserts hold
//      on the GPU frame (red cell is red, blue-bg cell is blue, CJK cell is
//      non-blank, blank cell is background).
//
// Gated: if there is no GPU or no system font, the test no-ops (returns).

use aterm_core::{grid::LineSize, render::LineSizeSpan, terminal::Terminal};
use aterm_render::{Frame, Renderer, Theme};

mod common;
use common::{backends, bb, gg, max_channel_delta_frame as max_channel_delta, rr};

const BG: u32 = 0x0011_1318; // Theme::default().bg

fn dist(a: u32, c: u32) -> i32 {
    (rr(a) - rr(c)).abs() + (gg(a) - gg(c)).abs() + (bb(a) - bb(c)).abs()
}

fn cell_pixels(f: &Frame, cw: usize, ch: usize, row: usize, col: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw * ch);
    for y in row * ch..(row * ch + ch).min(f.height) {
        for x in col * cw..(col * cw + cw).min(f.width) {
            out.push(f.pixels[y * f.width + x]);
        }
    }
    out
}

fn non_bg_count(px: &[u32]) -> usize {
    px.iter().filter(|&&p| dist(p, BG) > 24).count()
}

/// The visual-regression demo grid (same as aterm-render's visual_regression test).
fn demo_term() -> (Terminal, usize, usize) {
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(
        b"\x1b[31mRR\x1b[0m\r\n\
\x1b[44m  \x1b[0m\r\n\
\xe6\x97\xa5\xe6\x9c\xac\r\n\
\x1b[7mXX\x1b[0m\r\n\
ab\r\n",
    );
    (term, rows, cols)
}

#[test]
fn gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let mut win = aterm_gpu::WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    // 1. identical dimensions
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ"
    );
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cols * cw, rows * ch),
        "unexpected frame size"
    );

    // 2. near-identical pixels: geometry + blend match, so only rounding differs.
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU pixels diverge: max per-channel delta {delta} > 8"
    );

    // 3. semantic checks (same as aterm-render's visual_regression) on the GPU frame.
    // red 'R' cell (0,0)
    let red = cell_pixels(&gpu_frame, cw, ch, 0, 0)
        .iter()
        .any(|&p| rr(p) > 140 && gg(p) < 90 && bb(p) < 90);
    assert!(red, "GPU: expected red glyph pixels in cell (0,0)");

    // blue-bg space cell (1,0)
    let blue_px = cell_pixels(&gpu_frame, cw, ch, 1, 0);
    let blue = blue_px
        .iter()
        .filter(|&&p| bb(p) > 110 && rr(p) < 90)
        .count();
    assert!(
        blue > blue_px.len() / 2,
        "GPU: expected blue background in cell (1,0) ({}/{})",
        blue,
        blue_px.len()
    );

    // CJK 日 cell (2,0): non-blank via font fallback
    let cjk = non_bg_count(&cell_pixels(&gpu_frame, cw, ch, 2, 0));
    assert!(
        cjk > 12,
        "GPU: CJK cell (2,0) is blank ({cjk} non-bg pixels)"
    );

    // blank cell (5,8): stays background
    let blank_px = cell_pixels(&gpu_frame, cw, ch, 5, 8);
    let blank_non_bg = non_bg_count(&blank_px);
    assert!(
        blank_non_bg < blank_px.len() / 20,
        "GPU: blank cell (5,8) should be background ({blank_non_bg} non-bg)"
    );
}

/// Interior padding holds GPU/CPU parity: with the SAME `pad` set on both
/// renderers, the GPU and CPU frames have the same (grown) dimensions and the
/// grid lands on the same pixels within the antialiasing tolerance, and the
/// theme-bg border matches exactly. This locks the GPU `encode_frame` pad mirror
/// to the CPU `render_row` inset, so the application-present source and `image`
/// capture stay pixel-faithful at the app-owned boundary.
#[test]
fn gpu_matches_cpu_with_padding() {
    let theme = Theme::default();
    let px = 18.0;
    const P: usize = 10;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    gpu.set_pad(P);
    cpu.set_pad(P);

    let mut win = aterm_gpu::WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    // Same grown dimensions on both backends.
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "padded dimensions differ"
    );
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cols * cw + 2 * P, rows * ch + 2 * P),
        "unexpected padded frame size"
    );

    // Grid pixels match within the AA tolerance (same as the unpadded test).
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("padded GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "padded GPU/CPU pixels diverge: max per-channel delta {delta} > 8"
    );

    // The padding border is theme bg on BOTH (exact): top row + left column.
    for x in 0..gpu_frame.width {
        assert_eq!(
            gpu_frame.pixels[x], BG,
            "GPU top padding row not bg at x={x}"
        );
        assert_eq!(
            cpu_frame.pixels[x], BG,
            "CPU top padding row not bg at x={x}"
        );
    }
    for y in 0..gpu_frame.height {
        let i = y * gpu_frame.width;
        assert_eq!(
            gpu_frame.pixels[i], BG,
            "GPU left padding col not bg at y={y}"
        );
        assert_eq!(
            cpu_frame.pixels[i], BG,
            "CPU left padding col not bg at y={y}"
        );
    }
}

/// A composed split row carries one DEC line-size span per pane. Its geometry
/// must be resolved per cell: the left pane can be double-width while the right
/// remains single-width, and the next row can invert that arrangement. Cells in
/// the clipped-away logical half of each double-width pane must not reappear in
/// the neighbour.
#[test]
fn gpu_matches_cpu_with_pane_local_mixed_dec_widths() {
    let theme = Theme::default();
    let px = 18.0;
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (2usize, 17usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Row 0: visible red cell in the left pane, a magenta poison cell in
    // its clipped-away logical half, and a green cell in the right pane.
    // Row 1 reverses pane widths and puts the poison cell in the right pane's
    // clipped-away half. Cursor hidden so the fixture is pure pane geometry.
    term.process(
        b"\x1b[?25l\
\x1b[1;1H\x1b[48;2;190;20;20m \x1b[0m\
\x1b[1;7H\x1b[48;2;255;0;255m \x1b[0m\
\x1b[1;10H\x1b[48;2;20;180;40m \x1b[0m\
\x1b[2;1H\x1b[48;2;20;170;190m \x1b[0m\
\x1b[2;10H\x1b[48;2;30;60;210m \x1b[0m\
\x1b[2;14H\x1b[48;2;255;0;255m \x1b[0m",
    );
    let mut input = term.cell_frame(rows, cols);
    input.line_sizes.fill(LineSize::SingleWidth);
    input.line_size_spans.resize_with(rows, Vec::new);
    input.line_size_spans[0] = vec![
        LineSizeSpan::new(0, 8, LineSize::DoubleWidth),
        LineSizeSpan::new(9, 17, LineSize::SingleWidth),
    ];
    input.line_size_spans[1] = vec![
        LineSizeSpan::new(0, 8, LineSize::SingleWidth),
        LineSizeSpan::new(9, 17, LineSize::DoubleWidth),
    ];

    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "mixed-pane dimensions differ"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "mixed pane-local DEC geometry diverges: max delta {delta} > 8"
    );

    let mostly = |row: usize, col: usize, pred: fn(u32) -> bool| {
        let pixels = cell_pixels(&gpu_frame, cw, ch, row, col);
        pixels.iter().filter(|&&p| pred(p)).count() > pixels.len() * 3 / 4
    };
    let red = |p| rr(p) > 140 && gg(p) < 70 && bb(p) < 70;
    let green = |p| gg(p) > 130 && rr(p) < 80 && bb(p) < 90;
    let cyan = |p| bb(p) > 130 && gg(p) > 120 && rr(p) < 80;
    let blue = |p| bb(p) > 150 && rr(p) < 80 && gg(p) < 100;
    assert!(
        mostly(0, 0, red) && mostly(0, 1, red),
        "left double-width cell did not expand across two physical cells"
    );
    assert!(
        mostly(0, 9, green) && !mostly(0, 10, green),
        "right single-width pane inherited the left pane's DEC width"
    );
    assert!(
        mostly(1, 0, cyan) && !mostly(1, 1, cyan),
        "left single-width pane inherited the right pane's DEC width"
    );
    assert!(
        mostly(1, 9, blue) && mostly(1, 10, blue),
        "right double-width cell did not expand inside its own pane"
    );
    assert!(
        !gpu_frame
            .pixels
            .iter()
            .any(|&p| rr(p) > 230 && gg(p) < 30 && bb(p) > 230),
        "a clipped-away double-width source cell leaked into the composite"
    );
}

/// The three fixtures above install spans but only ever paint SGR-background
/// SPACES through them, and background rects come from the cell ADVANCE — so
/// they never observe a glyph's ENLARGEMENT (`Scale::xs`/`ys`/`anchor_y`). This
/// one puts a real glyph in EACH pane of a composed row and reproduces the
/// compositor's own frame shape: `line_size_spans` carries a span for the DEC
/// pane only, and `line_sizes[row]` is that pane's size (the row-level SUMMARY
/// `app_render` writes — NOT `SingleWidth`, which is what the other fixtures
/// fill and is the opposite of production). A column no run claims is
/// single-width by definition, so the innocent pane's glyph must stay 1× — on
/// BOTH backends.
///
/// Row 0 tests the width axis (a DECDWL pane beside an unclaimed one), row 1 the
/// height/anchor axis (DECDHL-bottom, whose `anchor_y` is a whole cell up).
#[test]
fn gpu_matches_cpu_for_glyph_enlargement_inside_its_own_dec_run() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (2usize, 17usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // One bright-red 'M' at the head of each pane on each row (cursor hidden —
    // this fixture is pure glyph geometry). 'M' fills its advance, so a 2× copy
    // visibly occupies the NEXT physical cell and a 1× copy does not. The
    // assertions count RED INK rather than "not theme bg": a written-but-blank
    // cell carries the terminal's own default background, not the theme's, so
    // "non-bg" would be true for most of this frame.
    term.process(b"\x1b[?25l\x1b[38;2;255;0;0m\x1b[1;1HM\x1b[1;10HM\x1b[2;1HM\x1b[2;10HM\x1b[0m");
    let mut input = term.cell_frame(rows, cols);
    input.line_size_spans.resize_with(rows, Vec::new);
    // Row 0: LEFT pane is DECDWL, right pane unclaimed (the compositor emits no
    // span for a single-width pane). Row 1: RIGHT pane is DECDHL-bottom, left
    // unclaimed. `line_sizes[r]` is the row-level SUMMARY the compositor writes.
    input.line_size_spans[0] = vec![LineSizeSpan::new(0, 8, LineSize::DoubleWidth)];
    input.line_size_spans[1] = vec![LineSizeSpan::new(9, 17, LineSize::DoubleHeightBottom)];
    input.line_sizes[0] = LineSize::DoubleWidth;
    input.line_sizes[1] = LineSize::DoubleHeightBottom;

    let (cw, ch) = cpu.cell_size();
    let cpu_frame = cpu.render_input(&input);
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "mixed-run glyph dimensions differ"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "per-run glyph enlargement diverges CPU/GPU: max delta {delta} > 8"
    );

    let ink = |frame: &Frame, row: usize, col: usize| {
        cell_pixels(frame, cw, ch, row, col)
            .iter()
            .filter(|&&p| rr(p) > 120 && gg(p) < 80 && bb(p) < 80)
            .count()
    };
    for (label, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        // NEGATIVE CONTROL: the fixture really does enlarge inside each DEC pane
        // — the second physical cell of each spanned pane carries the 2× glyph's
        // right half. Without this the assertions below would hold on a frame
        // that had lost DEC scaling altogether.
        assert!(
            ink(frame, 0, 1) > 4,
            "{label}: DECDWL pane's glyph did not spill into its own second cell"
        );
        assert!(
            ink(frame, 1, 10) > 4,
            "{label}: DECDHL pane's glyph did not spill into its own second cell"
        );
        // The claim under test: the pane with NO run of its own keeps a 1× glyph,
        // so the cell after its 'M' carries no ink. Taking the enlargement from
        // `line_sizes[r]` instead of the column's run puts the neighbouring
        // pane's 2× here (measured: it did, on the GPU only).
        assert_eq!(
            ink(frame, 0, 10),
            0,
            "{label}: unspanned pane inherited the row's DECDWL enlargement"
        );
        assert_eq!(
            ink(frame, 1, 1),
            0,
            "{label}: unspanned pane inherited the row's DECDHL enlargement"
        );
    }
}

/// An odd-width pane can expose only half of its final DEC double-width logical
/// cell. Curly underlines still paint that visible remainder, while the hard
/// pane clip keeps the wave out of the divider.
#[test]
fn gpu_matches_cpu_for_partial_double_width_pane_undercurl() {
    let theme = Theme::default();
    let px = 18.0;
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (1usize, 15usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Logical col 3 starts at physical col 6 in the double-width pane. The
    // pane ends at physical col 7, leaving exactly one base-cell of its 2×
    // undercurl visible.
    term.process(b"\x1b[?25l\x1b[1;4H\x1b[4:3;58;2;255;255;255m \x1b[0m");
    let mut input = term.cell_frame(rows, cols);
    input.line_sizes.fill(LineSize::SingleWidth);
    input.line_size_spans.resize_with(rows, Vec::new);
    input.line_size_spans[0] = vec![
        LineSizeSpan::new(0, 7, LineSize::DoubleWidth),
        LineSizeSpan::new(8, 15, LineSize::SingleWidth),
    ];

    let (cw, ch) = cpu.cell_size();
    let cpu_frame = cpu.render_input(&input);
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "partial pane undercurl GPU/CPU pixels diverge: max delta {delta} > 8"
    );
    for (label, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        assert!(
            non_bg_count(&cell_pixels(frame, cw, ch, 0, 6)) > 0,
            "{label} dropped the visible half-cell undercurl"
        );
        assert!(
            cell_pixels(frame, cw, ch, 0, 7)
                .iter()
                .all(|&p| dist(p, BG) <= 1),
            "{label} undercurl crossed into the divider"
        );
    }
}

/// Italic ink is allowed to overhang its home cell, but never its pane. Keep a
/// one-column divider between two single-width spans and prove the shared pane
/// clip removes the synthetic-italic `f` bearing on both backends.
#[test]
fn gpu_matches_cpu_and_clips_italic_ink_at_pane_edge() {
    let theme = Theme::default();
    let px = 18.0;
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (1usize, 17usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l\x1b[1;8H\x1b[3;97mf\x1b[0m");
    let mut unclipped = term.cell_frame(rows, cols);
    unclipped.line_sizes.fill(LineSize::SingleWidth);
    let mut clipped = unclipped.clone();
    clipped.line_size_spans.resize_with(rows, Vec::new);
    clipped.line_size_spans[0] = vec![
        LineSizeSpan::new(0, 8, LineSize::SingleWidth),
        LineSizeSpan::new(9, 17, LineSize::SingleWidth),
    ];

    let (cw, ch) = cpu.cell_size();
    let no_span = cpu.render_input(&unclipped);
    let cpu_frame = cpu.render_input(&clipped);
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_frame = gpu.render_input(&mut win, &clipped, None);
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "italic pane-edge dimensions differ"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "italic pane-edge GPU/CPU pixels diverge: max delta {delta} > 8"
    );

    let divider_x = 8 * cw;
    let divider = |frame: &Frame| {
        (0..ch)
            .flat_map(|y| {
                (divider_x..divider_x + cw).map(move |x| frame.pixels[y * frame.width + x])
            })
            .collect::<Vec<_>>()
    };
    let unclipped_divider = divider(&no_span);
    assert!(
        unclipped_divider.iter().any(|&p| dist(p, BG) > 24),
        "negative control: italic fixture has no natural pane-edge overhang"
    );
    assert!(
        divider(&cpu_frame).iter().all(|&p| dist(p, BG) <= 1),
        "CPU italic ink crossed the pane boundary"
    );
    assert!(
        divider(&gpu_frame).iter().all(|&p| dist(p, BG) <= 1),
        "GPU italic ink crossed the pane boundary"
    );
}

/// W12 mixed-DPI: two windows on different-scale displays are BOTH pixel-correct
/// simultaneously through ONE shared renderer pair. `activate_px` (the LIGHT
/// size switch) selects the drawing window's size WITHOUT tearing down the atlas,
/// so the two sizes coexist — and CPU/GPU stay byte-parity at EACH size. This
/// gates the whole point of W12: the non-frontmost, different-DPI window renders
/// at its OWN cell size, not whichever window last drew. We interleave the sizes
/// (A, B, A, B) to prove the switch is order-independent and never contaminates
/// one window's frame with the other's metrics.
#[test]
fn gpu_matches_cpu_mixed_dpi_activate_px() {
    let theme = Theme::default();
    let px_a = 18.0; // a 1× (laptop) window
    let px_b = 36.0; // a 2× (external Retina) window

    let Some((mut cpu, mut gpu)) = backends(px_a, theme) else {
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let mut win = aterm_gpu::WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let input = term.cell_frame(rows, cols);

    // The size-A and size-B cell metrics resolved purely (no mutation), and they
    // must genuinely differ (the mixed-DPI case is non-vacuous).
    let (cwa, cha, _) = cpu.cell_geometry(px_a);
    let (cwb, chb, _) = cpu.cell_geometry(px_b);
    assert!(
        cwb > cwa && chb > cha,
        "2× window must have a larger cell box"
    );

    // Draw window A (already active), then switch to B, back to A, to B — the
    // exact thrash two continuously-animating mixed-DPI windows produce.
    let mut check_parity = |cpu: &mut Renderer, gpu: &mut aterm_gpu::GpuRenderer, tag: &str| {
        let cpu_frame = cpu.render_input(&input);
        let gpu_frame = gpu.render_input(&mut win, &input, None);
        assert_eq!(
            (gpu_frame.width, gpu_frame.height),
            (cpu_frame.width, cpu_frame.height),
            "{tag}: dimensions differ"
        );
        let (cw, ch) = cpu.cell_size();
        assert_eq!(
            (gpu_frame.width, gpu_frame.height),
            (cols * cw, rows * ch),
            "{tag}: frame is not the ACTIVE size's grid ({cw}x{ch})"
        );
        let delta = max_channel_delta(&cpu_frame, &gpu_frame);
        assert!(
            delta <= 8,
            "{tag}: GPU/CPU pixels diverge at active size: max delta {delta} > 8"
        );
        (cpu_frame.width, cpu_frame.height)
    };

    let dims_a0 = check_parity(&mut cpu, &mut gpu, "A(initial)");
    assert_eq!(dims_a0, (cols * cwa, rows * cha), "A draws at size A");

    cpu.activate_px(px_b);
    gpu.activate_px(px_b);
    let dims_b = check_parity(&mut cpu, &mut gpu, "B(after switch up)");
    assert_eq!(dims_b, (cols * cwb, rows * chb), "B draws at size B");

    cpu.activate_px(px_a);
    gpu.activate_px(px_a);
    let dims_a1 = check_parity(&mut cpu, &mut gpu, "A(after switch back)");
    assert_eq!(
        dims_a1, dims_a0,
        "A back to size A, unchanged by B's frames"
    );

    cpu.activate_px(px_b);
    gpu.activate_px(px_b);
    let dims_b1 = check_parity(&mut cpu, &mut gpu, "B(second)");
    assert_eq!(dims_b1, dims_b, "B stable across interleave");

    // The activated A frame must be byte-identical to a FRESH CPU renderer taken
    // straight to A — proving the light switch changed only cache lifetimes.
    let Some(mut fresh_a) = Renderer::from_system(px_a, theme) else {
        return;
    };
    fresh_a.debug_block_on_lazy_fallbacks();
    cpu.activate_px(px_a);
    let activated_a = cpu.render_input(&input);
    let fresh_frame_a = fresh_a.render_input(&input);
    assert_eq!(
        activated_a.pixels, fresh_frame_a.pixels,
        "activate round-trip must render byte-identically to a fresh renderer at size A"
    );
}

/// GPU/CPU parity holds after DYNAMIC color changes (OSC 4 palette remap + OSC
/// 10/11 default fg/bg + OSC 17/19 selection bg/fg). The other parity tests render
/// a STATIC theme; this gates
/// the runtime-recolor path — a program changing the palette/defaults mid-session.
/// Color resolution happens once in aterm-core's `cell_frame`, so both renderers
/// consume the SAME recolored `RenderInput`; a divergence here would mean the GPU
/// blit a stale color (e.g. a glyph cached without its colour as part of the key).
/// The test also proves the OSC changes actually took effect, so it can't pass
/// vacuously by both backends rendering the original colours.
#[test]
fn gpu_matches_cpu_after_dynamic_osc_colors() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (3usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // OSC 4 palette SET is fail-closed by default (#7937); a themeable host opts in.
    term.set_allow_palette_reconfigure(true);
    // Recolor at runtime: OSC 4 remaps ANSI index 1 (red) -> pure green; OSC 10/11
    // set the default fg -> magenta and default bg -> navy. (ST = BEL.)
    term.process(b"\x1b]4;1;rgb:00/ff/00\x07");
    term.process(b"\x1b]10;rgb:ff/00/ff\x07");
    term.process(b"\x1b]11;rgb:00/00/80\x07");
    term.process(b"\x1b]17;rgb:12/34/56\x07");
    term.process(b"\x1b]19;rgb:fe/cd/32\x07");
    // 'A' uses SGR 31 (palette[1], now green); 'B' uses the recolored defaults.
    term.process(b"\x1b[31mA\x1b[0mB\r\nX");
    let selection = term.text_selection_mut();
    selection.start_selection(1, 0, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(1, 0, SelectionSide::Right);
    selection.complete_selection();

    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    // 1. identical dimensions + 2. pixel parity — the core gate.
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ after recolor"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("dynamic-color GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU diverge after OSC recolor: max per-channel delta {delta} > 8"
    );

    // 3. the OSC changes actually landed on the GPU frame (non-vacuous):
    // OSC 4 — cell (0,0) 'A' is now GREEN (red was remapped), not red.
    let a = cell_pixels(&gpu_frame, cw, ch, 0, 0);
    assert!(
        a.iter().any(|&p| gg(p) > 140 && rr(p) < 100 && bb(p) < 110),
        "OSC4 remap not applied: cell (0,0) has no green glyph pixels"
    );
    assert!(
        !a.iter().any(|&p| rr(p) > 140 && gg(p) < 90 && bb(p) < 90),
        "OSC4 remap not applied: cell (0,0) still shows red"
    );
    // The OSC recolor actually landed on the GPU frame (so the pass isn't vacuous),
    // and identically on the CPU (delta=1 above). NOTE: OSC 11's new default bg shows
    // on WRITTEN default cells; truly-empty cells + margins keep the renderer's
    // theme/clear bg — the same written-vs-clear split the GUI reconciles via
    // applied_terminal_config (the black-backed-text fix). So we assert the WRITTEN
    // cells, where the dynamic colors demonstrably resolve.
    let a = cell_pixels(&gpu_frame, cw, ch, 0, 0);
    // OSC 11 — the written cell's default background is now NAVY (#000080).
    let a_navy = a
        .iter()
        .filter(|&&p| rr(p) < 40 && gg(p) < 40 && (96..=160).contains(&bb(p)))
        .count();
    assert!(
        a_navy > a.len() / 2,
        "OSC11 default-bg not applied: 'A' cell bg not navy ({a_navy}/{})",
        a.len()
    );
    // OSC 10 — cell (0,1) 'B' glyph uses the new default fg (MAGENTA).
    let b = cell_pixels(&gpu_frame, cw, ch, 0, 1);
    assert!(
        b.iter().any(|&p| rr(p) > 120 && bb(p) > 120 && gg(p) < 110),
        "OSC10 default-fg not applied: no magenta glyph in cell (0,1)"
    );
    // OSC 17/19 — a stationary selection carries the live terminal-owned band
    // and ink colours into both renderer paths.
    let selected = cell_pixels(&gpu_frame, cw, ch, 1, 0);
    let live_selection_bg = selected
        .iter()
        .filter(|&&p| rr(p) == 0x12 && gg(p) == 0x34 && bb(p) == 0x56)
        .count();
    assert!(
        live_selection_bg > selected.len() / 3,
        "OSC17 selection bg did not reach GPU pixels ({live_selection_bg}/{})",
        selected.len()
    );
    assert!(
        selected
            .iter()
            .any(|&p| rr(p) > 220 && gg(p) > 160 && bb(p) < 100),
        "OSC19 selected-text foreground did not reach GPU pixels"
    );
}

/// GPU/CPU parity holds when a NAMED THEME is applied via the GUI's path —
/// `TerminalConfig::apply_config` with the scheme's `custom_palette` + default fg/bg,
/// not the OSC route the other dynamic test covers. Writes several distinct ANSI
/// colours so the pass can't be vacuous, and confirms the theme's colours land in
/// the GPU frame.
#[test]
fn gpu_matches_cpu_with_named_theme() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let mut win = aterm_gpu::WindowGpu::new();
    // Apply Dracula exactly as the GUI does: engine default fg/bg + the 16-slot palette.
    let scheme = aterm_types::scheme::builtin("Dracula").expect("Dracula builtin");
    let tc = aterm_core::config::TerminalConfig {
        default_foreground: scheme.foreground,
        default_background: scheme.background,
        custom_palette: Some(scheme.to_color_palette()),
        ..Default::default()
    };

    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.apply_config(&tc);
    term.process(b"\x1b[31mR\x1b[32mG\x1b[34mB\x1b[0m");

    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dims differ under a named theme"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("named-theme GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU diverge under a named theme: max per-channel delta {delta} > 8"
    );

    // The Dracula palette actually reached the GPU frame (non-vacuous): R = red
    // #ff5555, G = green #50fa7b, B = blue #bd93f9.
    let r = cell_pixels(&gpu_frame, cw, ch, 0, 0);
    assert!(
        r.iter().any(|&p| rr(p) > 200 && gg(p) < 120 && bb(p) < 120),
        "R not Dracula red"
    );
    let gc = cell_pixels(&gpu_frame, cw, ch, 0, 1);
    assert!(
        gc.iter()
            .any(|&p| gg(p) > 180 && rr(p) < 130 && bb(p) < 160),
        "G not Dracula green"
    );
    let bc = cell_pixels(&gpu_frame, cw, ch, 0, 2);
    assert!(
        bc.iter()
            .any(|&p| bb(p) > 180 && (120..220).contains(&rr(p)) && gg(p) < 180),
        "B not Dracula blue"
    );
}

/// EXACT parity on ORTHOGONAL procedural cells: axis-aligned box-drawing /
/// block / shade / braille / sextant / legacy-eighth glyphs are synthesized
/// as hard 0/255 coverage sized to the cell, so the CPU coverage blend and
/// the GPU alpha blend agree on EVERY pixel — max per-channel delta must be
/// 0, not merely within the antialiasing tolerance above. The frame holds
/// only orthogonal procedural glyphs and solid fills (cursor hidden via
/// DECTCEM), i.e. the whole frame is in the exactness domain. The
/// diagonal/curved families (arcs, diagonals, Powerline, wedges) are now
/// DELIBERATELY anti-aliased and live under the delta<=8 gate in
/// `aa_procedural_cells_match_cpu_within_tolerance` — that is the exactness
/// domain's honest boundary; never weaken THIS test back to a tolerance.
#[test]
fn procedural_cells_match_cpu_exactly() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (4usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Hide the cursor so no cell mixes in cursor styling; every pixel is then
    // a solid bg fill or hard procedural coverage. Row 2 paints a red double
    // junction run to exercise the fg tint path; the rest uses default fg.
    // Row 4 covers dashes, eighth blocks, and the legacy orthogonal
    // eighth-block ranges (U+1FB70–1FB8B).
    term.process(
        "\x1b[?25l\
\u{250C}\u{2500}\u{252C}\u{2500}\u{2510}\u{2554}\u{2550}\u{2566}\u{2550}\u{2557}\u{2501}\u{2513}\u{2517}\u{2503}\u{254B}\r\n\
\u{251C}\u{2500}\u{253C}\u{2500}\u{2524}\x1b[31m\u{2560}\u{2550}\u{256C}\u{2550}\u{2563}\x1b[0m\u{2580}\u{2584}\u{258C}\u{2590}\u{2588}\r\n\
\u{2514}\u{2500}\u{2534}\u{2500}\u{2518}\u{255A}\u{2550}\u{2569}\u{2550}\u{255D}\u{2591}\u{2592}\u{2593}\u{2847}\u{28FF}\r\n\
\u{2504}\u{2508}\u{254C}\u{2581}\u{2582}\u{258E}\u{1FB13}\u{1FB70}\u{1FB76}\u{1FB7C}\u{1FB80}\u{1FB81}\u{1FB82}\u{1FB87}\u{1FB8B}"
            .as_bytes(),
    );

    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ"
    );
    // Sanity that the fixture really drew glyphs (default fg pixels exist —
    // procedural coverage is hard 0/255, so stroke pixels are EXACTLY the
    // terminal's default foreground — and the red SGR run produced
    // red-dominant pixels). Guards a false pass on an all-background frame.
    let dfg = term.default_foreground();
    let dfg = (u32::from(dfg.r) << 16) | (u32::from(dfg.g) << 8) | u32::from(dfg.b);
    assert!(
        cpu_frame.pixels.contains(&dfg),
        "no default-fg glyph pixels"
    );
    assert!(
        cpu_frame
            .pixels
            .iter()
            .any(|&p| rr(p) > 100 && rr(p) > gg(p) && rr(p) > bb(p)),
        "no red glyph pixels from the SGR run"
    );

    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert_eq!(
        delta, 0,
        "orthogonal procedural cells must match EXACTLY between CPU and GPU"
    );
}

/// The ANTI-ALIASED procedural families (arcs U+256D–2570, diagonals
/// U+2571–2573, Powerline U+E0B0–E0BF, wedges/triangles U+1FB3C–1FB6F) carry
/// fractional coverage by design, so — like font glyphs — they live under
/// the standard <=8 LSB software-vs-hardware sRGB blend tolerance, NOT the
/// exact gate above. Their cell-EDGE texels are still hard 0/255 (the
/// seam-tiling law, proven in aterm-render's procedural_aa_edges), so cell
/// seams remain in the exactness domain. Every AA cell must actually draw.
#[test]
fn aa_procedural_cells_match_cpu_within_tolerance() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (2usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Row 1: arcs + diagonals + a red Powerline run (fg-tint path). Row 2:
    // wedges from each corner family + the quarter/three-quarter triangles.
    term.process(
        "\x1b[?25l\
\u{256D}\u{2500}\u{256E}\u{2570}\u{256F}\u{2571}\u{2572}\u{2573}\x1b[31m\u{E0B0}\u{E0B1}\u{E0B2}\u{E0B4}\u{E0B8}\u{E0BB}\u{E0BE}\x1b[0m\r\n\
\u{1FB3C}\u{1FB40}\u{1FB44}\u{1FB4B}\u{1FB52}\u{1FB56}\u{1FB5B}\u{1FB61}\u{1FB65}\u{1FB68}\u{1FB69}\u{1FB6A}\u{1FB6B}\u{1FB6C}\u{1FB6F}"
            .as_bytes(),
    );

    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ"
    );
    // Every AA cell actually drew ink on BOTH paths (non-vacuous).
    for r in 0..rows {
        for c in 0..15 {
            let cpu_ink = non_bg_count(&cell_pixels(&cpu_frame, cw, ch, r, c));
            let gpu_ink = non_bg_count(&cell_pixels(&gpu_frame, cw, ch, r, c));
            assert!(
                cpu_ink > 4 && gpu_ink > 4,
                "AA cell ({r},{c}) is blank (cpu {cpu_ink}, gpu {gpu_ink} non-bg)"
            );
        }
    }
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("AA procedural GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "AA procedural cells diverge: max per-channel delta {delta} > 8"
    );
}

/// SHADE-PHASE parity: ░▒▓ key their dither on the cell's ABSOLUTE pixel
/// parity, computed independently at the CPU blit and the GPU quad emission
/// (`shade_phase_key` at both sites plus the GPU atlas pass). An ODD interior
/// pad flips every cell's phase, so if either GPU site dropped or mis-computed
/// the fold, this frame diverges by a full fg/bg swing on the dither lattice
/// (or the quad misses its atlas slot and the cell goes blank). Shades stay
/// hard 0/255 at every phase, so the gate is EXACT (delta == 0).
#[test]
fn shade_phase_matches_cpu_exactly_with_odd_pad() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    gpu.set_pad(3);
    cpu.set_pad(3);

    let (rows, cols) = (3usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l░░░░░░░░░░░░\r\n▒▒▒▒▒▒▒▒▒▒▒▒\r\n▓▓▓▓▓▓▓▓▓▓▓▓".as_bytes());

    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ"
    );
    // Non-vacuous: the shade rows drew ink on the GPU (pad offsets the grid,
    // so probe via the padded frame directly at each row's first cell).
    for r in 0..rows {
        let mut ink = 0usize;
        for y in 3 + r * ch..3 + (r + 1) * ch {
            for x in 3..3 + cw {
                if dist(gpu_frame.pixels[y * gpu_frame.width + x], BG) > 24 {
                    ink += 1;
                }
            }
        }
        assert!(ink > 4, "shade row {r} is blank on the GPU ({ink} non-bg)");
    }
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert_eq!(
        delta, 0,
        "phase-keyed shades must match EXACTLY between CPU and GPU"
    );
}

/// W2 (`text_blending`): the parity net must hold in BOTH blend modes. Every
/// other test in this file runs the DEFAULT (`linear-corrected`); this one
/// flips both backends to `linear` and re-asserts (a) the demo grid within the
/// AA tolerance and (b) the procedural frame EXACTLY (delta == 0) — the
/// endpoint-exactness invariant made visible: hard 0/255 coverage must be
/// byte-identical regardless of mode. Also pins that the two modes genuinely
/// DIFFER on antialiased text (a non-vacuity control: if the mode flag were
/// dropped on either path, this test's premise — and the audit fix — would be
/// dead code).
#[test]
fn linear_mode_matches_cpu_and_keeps_procedural_exact() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let mut win = aterm_gpu::WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let input = term.cell_frame(rows, cols);

    // Default (corrected) CPU frame, kept for the non-vacuity control below.
    let corrected_frame = cpu.render_input(&input);

    cpu.set_text_blending(aterm_render::TextBlending::Linear);
    gpu.set_text_blending(aterm_render::TextBlending::Linear);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("linear-mode GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "linear-mode GPU/CPU pixels diverge: max per-channel delta {delta} > 8"
    );

    // Non-vacuity: the modes must actually differ on antialiased glyph texels
    // (identical output would mean the mode flag reaches neither backend).
    assert!(
        max_channel_delta(&cpu_frame, &corrected_frame) > 8,
        "linear vs linear-corrected must differ on AA texels"
    );

    // Procedural cells are hard 0/255 coverage — the endpoint domain — so the
    // mode must be invisible there and CPU/GPU stay EXACT, like the default-
    // mode `procedural_cells_match_cpu_exactly`.
    let (prows, pcols) = (2usize, 8usize);
    let mut pterm = Terminal::new(prows as u16, pcols as u16);
    pterm.process(
        "\x1b[?25l\u{250C}\u{2500}\u{252C}\u{2510}\u{2588}\u{2580}\u{2584}\u{258C}".as_bytes(),
    );
    let pinput = pterm.cell_frame(prows, pcols);
    let cpu_proc = cpu.render_input(&pinput);
    let gpu_proc = gpu.render_input(&mut win, &pinput, None);
    assert_eq!(
        max_channel_delta(&cpu_proc, &gpu_proc),
        0,
        "linear-mode procedural cells must match EXACTLY between CPU and GPU"
    );
}

/// Colour-emoji parity: the GPU must reproduce the CPU's RGBA emoji blit, not
/// drop it. Before the colour atlas existed the GPU skipped every `Rgba` glyph
/// and emoji rendered BLANK on the Metal path — a silent parity hole. This
/// renders a row of emoji on both paths and asserts (a) the GPU emoji cells are
/// substantially non-background (the glyph was actually drawn) and (b) the GPU
/// frame matches the CPU frame within the usual blend tolerance. Gated twice:
/// no GPU/font -> skip; no colour-emoji font on this host (the CPU cell comes
/// back blank) -> skip, since there's nothing to reproduce.
#[test]
fn colour_emoji_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (2usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // 🚀😀🎉🔥 — each a wide (2-cell) colour glyph from the sbix font.
    term.process("\u{1F680}\u{1F600}\u{1F389}\u{1F525}".as_bytes());

    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ"
    );

    // Gate on colour-emoji availability: if the CPU drew nothing in the first
    // emoji's lead cell, this host has no colour-emoji font — nothing to test.
    let cpu_lead = non_bg_count(&cell_pixels(&cpu_frame, cw, ch, 0, 0));
    if cpu_lead < 12 {
        eprintln!("SKIP: no colour-emoji font on this host (CPU emoji cell is blank)");
        return;
    }

    // (a) the GPU actually drew the emoji — every emoji lead cell is non-blank.
    for (i, col) in [0usize, 2, 4, 6].iter().enumerate() {
        let gpu_cell = non_bg_count(&cell_pixels(&gpu_frame, cw, ch, 0, *col));
        assert!(
            gpu_cell > 12,
            "GPU emoji #{i} (cell 0,{col}) is blank ({gpu_cell} non-bg pixels) — colour glyph dropped"
        );
    }

    // (b) GPU reproduces the CPU emoji within the blend tolerance. The colour
    // atlas holds the CPU's exact scaled pixels (1:1 NEAREST), so only the
    // edge alpha-blend rounding differs — the same <=8 LSB the mono path allows.
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("colour-emoji GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU emoji pixels diverge: max per-channel delta {delta} > 8"
    );

    // The emoji are genuinely colourful (not a mono fallback rendered identically
    // on both): the row has pixels from clearly different hues.
    let row0 = {
        let mut v = Vec::new();
        for col in 0..8 {
            v.extend(cell_pixels(&gpu_frame, cw, ch, 0, col));
        }
        v
    };
    let reddish = row0
        .iter()
        .any(|&p| rr(p) > 140 && rr(p) > gg(p) + 30 && rr(p) > bb(p) + 30);
    let other = row0.iter().any(|&p| gg(p) > 120 || bb(p) > 120);
    assert!(
        reddish && other,
        "GPU emoji row is not multi-coloured (reddish={reddish}, other={other})"
    );
}

/// VS16 emoji-presentation parity end-to-end: `❤️` (U+2764 + VS16) has a
/// MONOCHROME glyph in the text fonts, so without presentation handling it
/// would render as a grey heart. The core flags the VS16-widened cell
/// (`RenderCell::emoji_presentation`), and BOTH renderers must then prefer the
/// colour face — drawing a RED heart. This proves the GPU honours the flag
/// through `extract` -> `cell_key` -> colour atlas, matching the CPU. Gated on a
/// colour-emoji font (if the CPU heart isn't red, the host has none -> skip).
#[test]
fn vs16_emoji_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\u{2764}\u{FE0F}".as_bytes()); // ❤️

    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ"
    );

    // A RED-dominant pixel marks the colour heart (the mono glyph is drawn in
    // the light default fg, so it is never red-dominant).
    let red = |f: &Frame| {
        cell_pixels(f, cw, ch, 0, 0)
            .iter()
            .filter(|&&p| rr(p) > 120 && rr(p) > gg(p) + 40 && rr(p) > bb(p) + 40)
            .count()
    };
    if red(&cpu_frame) == 0 {
        eprintln!("SKIP: no colour ❤ on this host (CPU heart is not red)");
        return;
    }
    assert!(
        red(&gpu_frame) > 0,
        "GPU did not render the VS16 ❤️ in colour (emoji_presentation ignored)"
    );

    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("VS16 emoji GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU VS16 emoji pixels diverge: max per-channel delta {delta} > 8"
    );
}

/// VS15 text-presentation parity and containment end-to-end. The default-emoji
/// scalar 😀 is narrowed to one materialized cell and X is explicitly placed in
/// the immediately-adjacent column. Both backends must draw a non-vacuous text
/// glyph while leaving that X cell byte-identical to an independently-rendered
/// blank+X control. VS16 and CJK remain wide controls.
#[test]
fn vs15_text_emoji_is_one_cell_on_cpu_and_gpu_without_adjacent_overpaint() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let grin = '\u{1F600}';
    let default_key = cpu.glyph_key(grin);
    if default_key.source != aterm_render::FaceId::ColorEmoji
        || default_key.glyph_class != aterm_render::GlyphClass::Rgba
    {
        eprintln!("SKIP: no ordinary colour 😀 path on this host: {default_key:?}");
        return;
    }

    let (rows, cols) = (1usize, 5usize);
    let (cw, ch) = cpu.cell_size();
    let mut vs15_term = Terminal::new(rows as u16, cols as u16);
    vs15_term.process("\x1b[?25l😀\u{FE0E}\x1b[2GX".as_bytes());
    let vs15 = vs15_term.cell_frame(rows, cols);
    assert!(vs15.cells[0][0].text_presentation);
    assert!(!vs15.cells[0][0].emoji_presentation);
    assert_eq!(aterm_render::materialized_cell_span(&vs15.cells[0], 0), 1);
    assert_eq!(vs15.cells[0][1].ch, 'X');

    let mut win = aterm_gpu::WindowGpu::new();
    let cpu_vs15 = cpu.render_input(&vs15);
    let gpu_vs15 = gpu.render_input(&mut win, &vs15, None);
    let delta = max_channel_delta(&cpu_vs15, &gpu_vs15);
    assert!(delta <= 8, "GPU/CPU VS15 pixels diverge: {delta}");

    let mut control_term = Terminal::new(rows as u16, cols as u16);
    control_term.process(b"\x1b[?25l X");
    let control = control_term.cell_frame(rows, cols);
    let cpu_control = cpu.render_input(&control);
    let gpu_control = gpu.render_input(&mut win, &control, None);
    assert!(
        max_channel_delta(&cpu_control, &gpu_control) <= 8,
        "blank+X control lost CPU/GPU parity"
    );

    assert!(
        non_bg_count(&cell_pixels(&cpu_vs15, cw, ch, 0, 0)) > 12,
        "CPU VS15 glyph is vacuously blank"
    );
    assert!(
        non_bg_count(&cell_pixels(&gpu_vs15, cw, ch, 0, 0)) > 12,
        "GPU VS15 glyph is vacuously blank"
    );
    assert_eq!(
        cell_pixels(&cpu_vs15, cw, ch, 0, 1),
        cell_pixels(&cpu_control, cw, ch, 0, 1),
        "CPU VS15 glyph overpainted adjacent X"
    );
    assert_eq!(
        cell_pixels(&gpu_vs15, cw, ch, 0, 1),
        cell_pixels(&gpu_control, cw, ch, 0, 1),
        "GPU VS15 glyph/quad overpainted adjacent X"
    );

    // Negative controls against over-correcting all wide characters: VS16 stays
    // an emoji-presentation 2-cell unit and CJK stays a non-presentation 2-cell
    // unit. Their following X begins at column 2, not the adjacent column 1 above.
    let mut vs16_term = Terminal::new(rows as u16, cols as u16);
    vs16_term.process("\x1b[?25l\u{2764}\u{FE0F}X".as_bytes());
    let vs16 = vs16_term.cell_frame(rows, cols);
    assert!(vs16.cells[0][0].emoji_presentation);
    assert!(!vs16.cells[0][0].text_presentation);
    assert_eq!(aterm_render::materialized_cell_span(&vs16.cells[0], 0), 2);
    assert_eq!(vs16.cells[0][2].ch, 'X');

    let mut cjk_term = Terminal::new(rows as u16, cols as u16);
    cjk_term.process("\x1b[?25l中X".as_bytes());
    let cjk = cjk_term.cell_frame(rows, cols);
    assert!(!cjk.cells[0][0].emoji_presentation);
    assert!(!cjk.cells[0][0].text_presentation);
    assert_eq!(aterm_render::materialized_cell_span(&cjk.cells[0], 0), 2);
    assert_eq!(cjk.cells[0][2].ch, 'X');
}

/// Emoji grapheme-CLUSTER parity end-to-end: a ZWJ family (👨‍👩‍👧), a skin-tone
/// thumbs-up (👍🏽), and a keycap (1️⃣) are multi-codepoint clusters the renderer
/// SHAPES (rustybuzz) to a single colour glyph. Both paths must draw that glyph,
/// not just the base codepoint. Proves the GPU resolves cluster keys via the
/// shared `resolve_cell_key` and atlases them identically to the CPU. Gated on a
/// colour-emoji font (if the CPU family cell is blank, the host has none).
///
/// SCOPE (do not over-read a green result): this asserts CPU↔GPU PARITY (the two
/// backends draw the SAME pixels) plus non-blank coverage — NOT that the glyph is
/// the CORRECT multi-colour emoji. On Linux a ZWJ family/couple resolves correctly
/// in the renderer but the LIVE print path does not yet group multi-emoji ZWJ
/// sequences into one cell, so end-to-end it falls back to mono; that is a known,
/// documented engine gap (see aterm-render's `zwj_cluster_resolves_to_colour_in_renderer`),
/// not something this parity test certifies.
#[test]
fn cluster_emoji_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (2usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // family (0-1) sp(2) skin (3-4) sp(5) keycap (6) sp(7) flag (8-9)
    term.process(
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{1F44D}\u{1F3FD} \u{31}\u{FE0F}\u{20E3} \u{1F1FA}\u{1F1F8}".as_bytes(),
    );

    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ"
    );

    // Gate: the family cluster (lead col 0) must be a non-blank colour glyph on
    // the CPU; if not, this host lacks a colour-emoji font -> skip.
    let cpu_family = non_bg_count(&cell_pixels(&cpu_frame, cw, ch, 0, 0));
    if cpu_family < 12 {
        eprintln!("SKIP: no colour-emoji font on this host (CPU family cluster is blank)");
        return;
    }

    // The GPU drew each cluster (family col 0, skin col 3, keycap col 6,
    // regional-indicator flag col 8).
    for (label, col) in [("family", 0usize), ("skin", 3), ("keycap", 6), ("flag", 8)] {
        let gpu_cell = non_bg_count(&cell_pixels(&gpu_frame, cw, ch, 0, col));
        assert!(
            gpu_cell > 12,
            "GPU {label} cluster (cell 0,{col}) is blank ({gpu_cell} non-bg pixels) — cluster not shaped"
        );
    }

    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("cluster emoji GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU cluster emoji pixels diverge: max per-channel delta {delta} > 8"
    );
}

/// Line decorations (underline / strikethrough / double underline) are drawn as
/// hard-edged rects OVER the glyphs. Both paths use the same
/// `aterm_render::underline_rects` / `strike_overline_rects` geometry, so the
/// GPU must match the CPU within the glyph tolerance — AND the decorated frame
/// must differ from an undecorated one (proving the line is actually drawn, on
/// both paths).
#[test]
fn decorations_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (3usize, 8usize);
    let mut win = aterm_gpu::WindowGpu::new();
    let render = |cpu: &mut Renderer,
                  gpu: &mut aterm_gpu::GpuRenderer,
                  win: &mut aterm_gpu::WindowGpu,
                  bytes: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(bytes);
        let input = term.cell_frame(rows, cols);
        (
            cpu.render_input(&input),
            gpu.render_input(win, &input, None),
        )
    };

    // Underlined, strikethrough, double-underlined rows.
    let (cpu_deco, gpu_deco) = render(
        &mut cpu,
        &mut gpu,
        &mut win,
        b"\x1b[4mUU\x1b[0m\r\n\x1b[9mSS\x1b[0m\r\n\x1b[21mDD\x1b[0m",
    );
    // Same glyphs, no decorations.
    let (cpu_plain, _) = render(&mut cpu, &mut gpu, &mut win, b"UU\r\nSS\r\nDD");

    assert_eq!(
        (gpu_deco.width, gpu_deco.height),
        (cpu_deco.width, cpu_deco.height),
        "dimensions differ"
    );

    // GPU reproduces the CPU decorated frame (hard rects + glyph AA <= 8).
    let delta = max_channel_delta(&cpu_deco, &gpu_deco);
    eprintln!("decorations GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU decorated pixels diverge: max per-channel delta {delta} > 8"
    );

    // The decorations are actually drawn: the decorated frame differs from the
    // plain one, on BOTH paths (so neither silently skips the lines).
    assert!(
        cpu_deco.pixels != cpu_plain.pixels,
        "CPU decorated frame is identical to the undecorated one — no lines drawn"
    );
    assert!(
        gpu_deco.pixels != cpu_plain.pixels,
        "GPU decorated frame is identical to the undecorated one — no lines drawn"
    );
}

/// W7 decorations: the AA cosine undercurl (curly, SGR 4:3 — GPU quads over
/// the shared deco-atlas tile vs the CPU coverage blend), the absolute-x
/// phased dotted (4:4) / dashed (4:5) patterns across multi-cell runs, an SGR
/// 58 coloured undercurl, and descender ink-skip (DEFAULT ON, plus the
/// knob-off variant) must all stay CPU==GPU. Descender text (gyqj) forces the
/// skip to actually erase, so the parity check covers the kept-span geometry
/// on both paths — and flipping the knob must CHANGE the output identically on
/// both (proving the GPU honours the shared switch, not just the default).
#[test]
fn w7_underlines_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (4usize, 10usize);
    let mut win = aterm_gpu::WindowGpu::new();
    let mut render = |cpu: &mut Renderer, gpu: &mut aterm_gpu::GpuRenderer, bytes: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(bytes);
        let input = term.cell_frame(rows, cols);
        (
            cpu.render_input(&input),
            gpu.render_input(&mut win, &input, None),
        )
    };
    // Row 0: curly over descenders; row 1: coloured (SGR 58) curly; row 2:
    // dotted run; row 3: dashed run — multi-cell, so the absolute-x phase
    // crosses seams on both paths.
    let txt: &[u8] = b"\x1b[4:3mgyqj\x1b[0m\r\n\
          \x1b[4:3;58;2;255;80;80mwavy\x1b[0m\r\n\
          \x1b[4:4mdotdotdo\x1b[0m\r\n\
          \x1b[4:5mdashdash\x1b[0m";

    for skip in [true, false] {
        cpu.set_underline_skip_descenders(skip);
        gpu.set_underline_skip_descenders(skip);
        let (cpu_f, gpu_f) = render(&mut cpu, &mut gpu, txt);
        assert_eq!(
            (gpu_f.width, gpu_f.height),
            (cpu_f.width, cpu_f.height),
            "dimensions differ (skip={skip})"
        );
        let delta = max_channel_delta(&cpu_f, &gpu_f);
        eprintln!("W7 underlines GPU vs CPU max per-channel delta (skip={skip}) = {delta}");
        assert!(
            delta <= 8,
            "W7 GPU/CPU pixels diverge (skip={skip}): max per-channel delta {delta} > 8"
        );
    }

    // The knob is not decorative: skip on vs off must differ under descenders,
    // and identically so on both paths (each path already matched the other
    // above; assert the CPU actually changed).
    cpu.set_underline_skip_descenders(true);
    gpu.set_underline_skip_descenders(true);
    let (cpu_on, _) = render(&mut cpu, &mut gpu, txt);
    cpu.set_underline_skip_descenders(false);
    gpu.set_underline_skip_descenders(false);
    let (cpu_off, _) = render(&mut cpu, &mut gpu, txt);
    assert!(
        cpu_on.pixels != cpu_off.pixels,
        "descender ink-skip must change the rendered underline under gyqj"
    );

    // The undercurl actually draws: the curly row differs from plain text on
    // both paths (neither silently dropped the mask/quads).
    cpu.set_underline_skip_descenders(true);
    gpu.set_underline_skip_descenders(true);
    let (cpu_curl, gpu_curl) = render(&mut cpu, &mut gpu, b"\x1b[4:3mwavy\x1b[0m");
    let (cpu_plain, gpu_plain) = render(&mut cpu, &mut gpu, b"wavy");
    assert!(
        cpu_curl.pixels != cpu_plain.pixels,
        "CPU undercurl drew nothing"
    );
    assert!(
        gpu_curl.pixels != gpu_plain.pixels,
        "GPU undercurl drew nothing"
    );
}

/// Synthetic BOLD / ITALIC are baked into the cached glyph coverage, which the
/// GPU atlas pulls by `GlyphKey` (style included) — so the GPU reproduces them
/// with no shader change. Assert parity AND that styled text differs from plain
/// (the weight/slant is actually applied on both paths).
#[test]
fn bold_italic_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (3usize, 8usize);
    let mut win = aterm_gpu::WindowGpu::new();
    let render = |cpu: &mut Renderer,
                  gpu: &mut aterm_gpu::GpuRenderer,
                  win: &mut aterm_gpu::WindowGpu,
                  bytes: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(bytes);
        let input = term.cell_frame(rows, cols);
        (
            cpu.render_input(&input),
            gpu.render_input(win, &input, None),
        )
    };

    // Cursor visible: a wide synthetic-bold-italic glyph overflows into the next
    // cell, and the block cursor now composites the same on both paths (see
    // block_cursor_over_glyph_overflow_matches_cpu).
    let (cpu_styled, gpu_styled) = render(
        &mut cpu,
        &mut gpu,
        &mut win,
        b"\x1b[1mBB\x1b[0m\r\n\x1b[3mII\x1b[0m\r\n\x1b[1;3mWW\x1b[0m",
    );
    let (cpu_plain, _) = render(&mut cpu, &mut gpu, &mut win, b"BB\r\nII\r\nWW");

    assert_eq!(
        (gpu_styled.width, gpu_styled.height),
        (cpu_styled.width, cpu_styled.height),
        "dimensions differ"
    );
    let delta = max_channel_delta(&cpu_styled, &gpu_styled);
    eprintln!("bold/italic GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU bold-italic pixels diverge: max per-channel delta {delta} > 8"
    );

    assert!(
        cpu_styled.pixels != cpu_plain.pixels,
        "CPU bold/italic frame identical to plain — synthetic styling not applied"
    );
    assert!(
        gpu_styled.pixels != cpu_plain.pixels,
        "GPU bold/italic frame identical to plain — synthetic styling not applied"
    );
}

/// DECDWL double-width lines (`ESC # 6`) draw every cell twice as wide via 2×
/// NEAREST replication. The GPU's 2×-wide nearest-sampled quad must match the
/// CPU's 2× column replicate, AND the row must actually be wider than the same
/// text on a single-width line (the doubling is applied).
#[test]
fn decdwl_double_width_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (1usize, 16usize);
    let mut win = aterm_gpu::WindowGpu::new();
    let render = |cpu: &mut Renderer,
                  gpu: &mut aterm_gpu::GpuRenderer,
                  win: &mut aterm_gpu::WindowGpu,
                  bytes: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(bytes);
        let input = term.cell_frame(rows, cols);
        (
            cpu.render_input(&input),
            gpu.render_input(win, &input, None),
        )
    };
    let (cw, ch) = cpu.cell_size();

    // DECDWL line vs the same text single-width, cursor hidden.
    let (cpu_dw, gpu_dw) = render(&mut cpu, &mut gpu, &mut win, b"\x1b[?25l\x1b#6ABCD");
    let (cpu_sw, _) = render(&mut cpu, &mut gpu, &mut win, b"\x1b[?25lABCD");

    assert_eq!(
        (gpu_dw.width, gpu_dw.height),
        (cpu_dw.width, cpu_dw.height),
        "dims"
    );
    let delta = max_channel_delta(&cpu_dw, &gpu_dw);
    eprintln!("DECDWL GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU double-width pixels diverge: max per-channel delta {delta} > 8"
    );

    // The 'C' (col 2) sits at single-width col 2 but double-width col 2*2=4.
    // The double-width cell (0,4) is non-blank; the single-width frame's col 4
    // is already past "ABCD" (blank) — so the row is genuinely twice as wide.
    let dw_at4 = non_bg_count(&cell_pixels(&cpu_dw, cw, ch, 0, 4));
    let sw_at4 = non_bg_count(&cell_pixels(&cpu_sw, cw, ch, 0, 4));
    assert!(
        dw_at4 > 12 && sw_at4 < 12,
        "DECDWL not 2× wide (dw col4={dw_at4}, sw col4={sw_at4})"
    );
}

/// DECDHL double-height lines (`ESC # 3`/`# 4`): the same text on two rows forms
/// ONE 2×-both line — the top row shows the upper half of the doubled glyphs, the
/// bottom row the lower half (a dest-row clip of the 2× glyph). The GPU computes
/// the visible slice (rect + UV) via the shared `glyph_quad`, so its NEAREST
/// quad reproduces the CPU's 2× replicate + clip.
#[test]
fn decdhl_double_height_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (2usize, 16usize);
    let mut win = aterm_gpu::WindowGpu::new();
    let render = |cpu: &mut Renderer,
                  gpu: &mut aterm_gpu::GpuRenderer,
                  win: &mut aterm_gpu::WindowGpu,
                  bytes: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(bytes);
        let input = term.cell_frame(rows, cols);
        (
            cpu.render_input(&input),
            gpu.render_input(win, &input, None),
        )
    };

    // DECDHL top + bottom halves vs the same text plain (cursor hidden).
    let (cpu_dh, gpu_dh) = render(
        &mut cpu,
        &mut gpu,
        &mut win,
        b"\x1b[?25l\x1b#3BIG\r\n\x1b#4BIG",
    );
    let (cpu_plain, _) = render(&mut cpu, &mut gpu, &mut win, b"\x1b[?25lBIG\r\nBIG");

    assert_eq!(
        (gpu_dh.width, gpu_dh.height),
        (cpu_dh.width, cpu_dh.height),
        "dims"
    );
    let delta = max_channel_delta(&cpu_dh, &gpu_dh);
    eprintln!("DECDHL GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU double-height pixels diverge: max per-channel delta {delta} > 8"
    );
    // Double-height is genuinely different from the plain duplicated text.
    assert!(
        cpu_dh.pixels != cpu_plain.pixels,
        "DECDHL renders the same as plain text"
    );
}

/// Powerline separators (U+E0B0–E0BF) are synthesized procedurally but are
/// now an ANTI-ALIASED family (4× supersampled, perpendicular stroke widths),
/// so they hold the standard <=8 LSB blend tolerance — the same gate as font
/// glyphs — instead of the pre-AA delta==0 (their cell-edge texels are still
/// hard, which `procedural_aa_edges` proves). The glyphs must actually draw.
#[test]
fn powerline_cells_match_cpu_within_tolerance() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (1usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Cursor hidden so the whole frame is procedural coverage + solid fills.
    term.process(
        "\x1b[?25l\u{E0B0}\u{E0B2}\u{E0B4}\u{E0B6}\u{E0B8}\u{E0BA}\u{E0BC}\u{E0BE}".as_bytes(),
    );
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dims"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("Powerline GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "Powerline AA cells diverge: max per-channel delta {delta} > 8"
    );
    // Each separator actually drew ink (the solid triangle/cap cells especially).
    for col in [0usize, 2, 4, 6] {
        let n = non_bg_count(&cell_pixels(&cpu_frame, cw, ch, 0, col));
        assert!(
            n > 12,
            "Powerline cell (0,{col}) is blank ({n} non-bg) — not synthesized"
        );
    }
}

/// A glyph overflowing into the BLOCK-cursor cell must composite the same on
/// both paths: the CPU paints the block cursor LAST (over the overflow), and the
/// GPU now fills the block cursor AFTER the glyph passes too (was: cursor bg in
/// the bg pass, so overflow drew on top — a ~137-LSB divergence). A wide
/// synthetic bold-italic glyph sits immediately left of the cursor.
#[test]
fn block_cursor_over_glyph_overflow_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (1usize, 4usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Bold-italic W at col 0 (overflows right); the block cursor lands at col 1.
    term.process(b"\x1b[1;3mW");
    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dims"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("cursor-over-overflow GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "block cursor vs glyph overflow diverges: max per-channel delta {delta} > 8"
    );
}

/// Combining diacritics (é = e + U+0301, …) are overlaid as extra mono-glyph
/// instances on the base cell. The GPU pulls each mark from the same atlas, so
/// it must match the CPU — AND the accented frame must differ from the bare-base
/// one (the mark is actually drawn on both paths).
#[test]
fn combining_marks_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (1usize, 8usize);
    let mut win = aterm_gpu::WindowGpu::new();
    let render = |cpu: &mut Renderer,
                  gpu: &mut aterm_gpu::GpuRenderer,
                  win: &mut aterm_gpu::WindowGpu,
                  bytes: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(bytes);
        let input = term.cell_frame(rows, cols);
        (
            cpu.render_input(&input),
            gpu.render_input(win, &input, None),
        )
    };

    // é ñ å — base + combining mark.
    let (cpu_acc, gpu_acc) = render(
        &mut cpu,
        &mut gpu,
        &mut win,
        "\x1b[?25le\u{0301}n\u{0303}a\u{030A}".as_bytes(),
    );
    let (cpu_bare, _) = render(&mut cpu, &mut gpu, &mut win, b"\x1b[?25lena");

    assert_eq!(
        (gpu_acc.width, gpu_acc.height),
        (cpu_acc.width, cpu_acc.height),
        "dimensions differ"
    );
    let delta = max_channel_delta(&cpu_acc, &gpu_acc);
    eprintln!("combining marks GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU combining-mark pixels diverge: max per-channel delta {delta} > 8"
    );
    assert!(
        cpu_acc.pixels != cpu_bare.pixels,
        "CPU: combining marks not drawn (accented == bare)"
    );
    assert!(
        gpu_acc.pixels != cpu_bare.pixels,
        "GPU: combining marks not drawn (accented == bare)"
    );
}

/// W8 (g)/(h) CPU/GPU parity for the CONDENSED symbol-tier raster.
///
/// The bug this pins: U+27F5..U+27FC are one STIX Two Math design (advance
/// 1.612 em, ink 1.499 em) that neither SF Mono nor Arial Unicode carries, so
/// they land on the symbol fallback tier and used to paint ~2.9 CELLS wide
/// while occupying exactly ONE cell in the grid — burying the two columns to
/// their right and shearing any box-drawing table they appeared in. The fix
/// condenses the coverage at RASTER time, before the `GlyphKey` cache insert,
/// so the GPU inherits it for free: `Atlas::place` pulls the EXACT cached
/// bytes and `slot.xmin` from the CPU renderer and places them through the
/// SHARED `aterm_render::glyph_quad`. There is no GPU-side code in the fix —
/// THIS TEST is what pins that, and what stops a future divergence.
///
/// The spill law is stated DIFFERENTIALLY (against the same row drawn with
/// spaces) rather than against a background constant, because the frame the
/// compositor hands back carries the theme's own ground treatment and a
/// "blank cell" is not literally `Theme::bg`.
#[test]
fn condensed_symbol_fallback_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // The arrows come from a LAZILY parsed symbol face; block so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    // One arrow every third column, so the two columns to its right — exactly
    // the ones the pre-fix raster buried — are known-blank cells.
    let (rows, cols) = (1usize, 12usize);
    let mut win = aterm_gpu::WindowGpu::new();
    let mut render = |cpu: &mut Renderer, gpu: &mut aterm_gpu::GpuRenderer, bytes: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(bytes);
        let input = term.cell_frame(rows, cols);
        (
            cpu.render_input(&input),
            gpu.render_input(&mut win, &input, None),
        )
    };
    let (cpu_arrows, gpu_arrows) = render(
        &mut cpu,
        &mut gpu,
        "\x1b[?25l\u{27F5}  \u{27F6}  \u{27F8}  \u{27F9}  ".as_bytes(),
    );
    let (cpu_blank, gpu_blank) = render(&mut cpu, &mut gpu, b"\x1b[?25l            ");

    assert_eq!(
        (gpu_arrows.width, gpu_arrows.height),
        (cpu_arrows.width, cpu_arrows.height),
        "dimensions differ"
    );
    let delta = max_channel_delta(&cpu_arrows, &gpu_arrows);
    eprintln!("condensed symbol fallback GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU condensed-fallback pixels diverge: max per-channel delta {delta} > 8"
    );

    let (cw, ch) = cpu.cell_size();
    let mut drew = 0usize;
    for arrow in [0usize, 3, 6, 9] {
        for (who, ink_f, blank_f) in [
            ("CPU", &cpu_arrows, &cpu_blank),
            ("GPU", &gpu_arrows, &gpu_blank),
        ] {
            // Non-vacuity: the arrow cell itself must differ from blank, or
            // the spill law below is trivially satisfied.
            if cell_pixels(ink_f, cw, ch, 0, arrow) != cell_pixels(blank_f, cw, ch, 0, arrow) {
                drew += 1;
            }
            // The two columns to the right must be pixel-identical to the
            // same columns of the all-spaces frame: the arrow touched nothing.
            for spill in [arrow + 1, arrow + 2] {
                let got = cell_pixels(ink_f, cw, ch, 0, spill);
                let want = cell_pixels(blank_f, cw, ch, 0, spill);
                let differing = got.iter().zip(&want).filter(|(a, b)| a != b).count();
                assert_eq!(
                    differing,
                    0,
                    "{who}: the arrow at col {arrow} paints into col {spill} \
                     ({differing}/{} pixels differ from a blank cell)",
                    got.len()
                );
            }
        }
    }
    assert!(
        drew > 0,
        "non-vacuity: no arrow drew any ink, so the spill law is trivially satisfied"
    );
}

/// Every combining fixture above hangs its mark off a printable LETTER, so none
/// of them reaches the case where the two backends express drawability
/// differently: the CPU wraps the base glyph AND its marks in ONE guard
/// (`!wide && ch != ' ' && !ch.is_control()`), while the GPU has two independent
/// loops and only the base loop consults it. `add_combining_to_previous_cell`
/// attaches unconditionally to the previous cell, so a mark typed after a space
/// lands on a SPACE base — reachable, and the one shape that separates the loops.
///
/// The base 'e' carries the SAME U+0301 as the space, which is what makes the
/// divergence observable: the GPU's atlas prepass is itself gated on
/// `drawable`, so a mark that appears ONLY on a space base is silently dropped
/// downstream for want of an atlas slot (measured — that variant passes with or
/// without the guard). Put the mark on a drawable base too and its slot exists,
/// at which point the unguarded mark loop paints it on the space as well.
///
/// The oracle is the CPU: on a space base it draws nothing, so the GPU must draw
/// nothing either. (Whether dropping it is the RIGHT rendering is a separate
/// question about the CPU renderer; this gate only pins the two backends
/// together.)
#[test]
fn combining_mark_on_space_base_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (1usize, 8usize);
    let frame_for = |cpu: &mut Renderer,
                     gpu: &mut aterm_gpu::GpuRenderer,
                     win: &mut aterm_gpu::WindowGpu,
                     bytes: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(bytes);
        let input = term.cell_frame(rows, cols);
        let cpu_frame = cpu.render_input(&input);
        let gpu_frame = gpu.render_input(win, &input, None);
        (input, cpu_frame, gpu_frame)
    };

    let mut win = aterm_gpu::WindowGpu::new();
    // "e" + U+0301 (a drawable base, so the acute reaches the atlas), then a
    // SPACE + U+0301 — a mark does not advance the cursor, so that second acute
    // attaches to the space at col 1 and 'b' lands at col 2.
    let (marked, cpu_marked, gpu_marked) = frame_for(
        &mut cpu,
        &mut gpu,
        &mut win,
        "\x1b[?25le\u{0301} \u{0301}b".as_bytes(),
    );
    let (_, cpu_plain, _) = frame_for(
        &mut cpu,
        &mut gpu,
        &mut win,
        "\x1b[?25le\u{0301} b".as_bytes(),
    );

    // NON-VACUITY: the frame really does carry a combining mark on a ' ' base.
    // Without this the parity assertion below could hold because nothing was
    // ever attached.
    assert_eq!(
        marked.cells[0][1].ch, ' ',
        "fixture drifted: base cell is not a space"
    );
    assert!(
        marked.combining_at(0, 1).is_some(),
        "fixture drifted: no combining mark attached to the space cell"
    );
    assert!(
        marked.combining_at(0, 0).is_some(),
        "fixture drifted: the drawable base lost its mark, so the atlas may not hold the acute"
    );
    // The CPU oracle drops it: the marked frame equals the unmarked one.
    assert_eq!(
        cpu_marked.pixels, cpu_plain.pixels,
        "CPU: a mark on a space base is expected to be dropped by the pass-2 guard"
    );
    let delta = max_channel_delta(&cpu_marked, &gpu_marked);
    assert!(
        delta <= 8,
        "GPU drew a combining mark the CPU dropped: max per-channel delta {delta} > 8"
    );
}

/// OFFSCREEN-PERSISTENCE GATE: a renderer REUSED across CHANGING dimensions must
/// produce frames BYTE-IDENTICAL to a FRESH renderer rendering the same frame.
///
/// The persistent offscreen render target (texture + view + blit-source bind
/// group) and the gated screen-uniform write are reused across presents at one
/// dimension and rebuilt only on a resize. A stale-resource bug (e.g. a frame
/// reusing a previous, differently-sized texture/view, or skipping a needed
/// uniform rewrite) would silently corrupt a resized frame — yet might still pass
/// the CPU<=8-LSB parity bound. This drives a SINGLE reused renderer through a
/// grow -> shrink -> grow -> same dimension sweep and asserts each frame equals
/// EXACTLY (every pixel) what a renderer constructed fresh for that frame
/// produces, so any cross-dimension resource staleness is caught.
#[test]
fn reused_renderer_across_dims_matches_fresh_render() {
    let theme = Theme::default();
    let px = 16.0;
    let mut reused = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    // Deterministic pixels: block on the lazy fallback parses so the reused
    // renderer and every fresh oracle rasterize the CJK/emoji identically.
    reused.debug_block_on_lazy_fallbacks();
    let mut win_reused = aterm_gpu::WindowGpu::new();

    // Content per cell-size, deliberately glyph-rich (mono + CJK + colour emoji +
    // combining + decorations) so the full pass set runs. CJK/emoji are written as
    // literal `\u{..}` chars (a `&str` `\xNN` escape is ASCII-only).
    let content: &[u8] = "\u{1b}[31mRR\u{1b}[0m \u{1b}[44mbg\u{1b}[0m \u{65e5}\u{672c} \
\u{2764}\u{fe0f} e\u{0301} \u{1b}[4;9mUu\u{1b}[0m"
        .as_bytes();

    // grow -> shrink -> grow, then REPEAT a prior dimension (24x80 appears twice)
    // so we exercise: first-create, grow, shrink (smaller than resident), grow
    // again, and a same-as-an-earlier-frame dimension reusing a now-resident size.
    let dims: &[(usize, usize)] = &[(6, 16), (24, 80), (3, 8), (24, 80), (12, 40), (6, 16)];

    for (i, &(rows, cols)) in dims.iter().enumerate() {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(content);

        // A fresh renderer is the oracle: it has no resident resources, so its
        // frame is the canonical full render for these exact dimensions.
        let mut fresh = aterm_gpu::GpuRenderer::new(px, theme)
            .expect("fresh GPU renderer (device already proven above)");
        fresh.debug_block_on_lazy_fallbacks();
        let mut win_fresh = aterm_gpu::WindowGpu::new();

        let input = term.cell_frame(rows, cols);
        let reused_frame = reused.render_input(&mut win_reused, &input, None);
        let fresh_frame = fresh.render_input(&mut win_fresh, &input, None);

        assert_eq!(
            (reused_frame.width, reused_frame.height),
            (fresh_frame.width, fresh_frame.height),
            "frame {i} ({rows}x{cols}): reused-renderer dimensions diverge from fresh",
        );
        assert_eq!(
            reused_frame.pixels, fresh_frame.pixels,
            "frame {i} ({rows}x{cols}): reused renderer (resized) is NOT byte-identical to a fresh render \
             — stale offscreen/uniform resource",
        );
    }
}

/// CURSOR MOTION TRAIL parity (the "streaming trailer"): a frame carrying a
/// non-empty `cursor_trail` must render the same on the GPU as on the CPU. The
/// trail is drawn as OPAQUE quads pre-blended with the SHARED
/// `aterm_render::blend_rgb`, so even the strict bg-quad path matches; we also
/// assert the comet is actually painted — a visible green tint, brightest at the
/// head and dimmer at the tail.
#[test]
fn cursor_trail_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let mut win = aterm_gpu::WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    assert!(
        input.cells[5].get(6).is_none(),
        "regression requires a trail cell in an unmaterialized sparse tail"
    );
    // Deliberately diverge both from `theme.bg` and from the frame scalar:
    // sparse trail cells must blend over their owning pane's live
    // OSC-11/DECSCNM-resolved background. CPU/GPU parity alone would miss both
    // paths making the same stale-scalar mistake, so the exact oracle below is
    // required.
    input.default_bg = 0x0012_3456;
    let pane_default_bg = 0x0024_4668;
    input.default_bg_spans.resize_with(rows, Vec::new);
    input.default_bg_spans[5].push(aterm_render::DefaultBgSpan::new(0, cols, pane_default_bg));
    // A comet across the empty bottom row (row 5): faint tail → bright head.
    input.cursor_trail = vec![
        aterm_render_api::TrailCell {
            row: 5,
            col: 4,
            alpha: 60,
        },
        aterm_render_api::TrailCell {
            row: 5,
            col: 5,
            alpha: 130,
        },
        aterm_render_api::TrailCell {
            row: 5,
            col: 6,
            alpha: 200,
        },
    ];
    input.cursor_trail_color = 0x0050_FA7B; // Dracula green
    let expected_head = aterm_render::blend_rgb(pane_default_bg, input.cursor_trail_color, 200);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "trail: dimensions differ"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("trail GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "trail GPU/CPU pixels diverge: max per-channel delta {delta} > 8"
    );

    // The comet is actually drawn, green, and head-brighter-than-tail, on BOTH.
    for (label, f) in [("cpu", &cpu_frame), ("gpu", &gpu_frame)] {
        let head = cell_pixels(f, cw, ch, 5, 6);
        let greenish = head
            .iter()
            .filter(|&&p| gg(p) > 80 && gg(p) > rr(p) && gg(p) > bb(p))
            .count();
        assert!(
            greenish > head.len() / 2,
            "{label}: bright trail head (5,6) is not a green tint ({greenish}/{})",
            head.len()
        );
        assert!(
            head.iter()
                .all(|&pixel| pixel & 0x00ff_ffff == expected_head),
            "{label}: sparse trail head must blend over pane default \
             {pane_default_bg:#08x}, not frame scalar {:#08x}",
            input.default_bg,
        );
        let head_g = head.iter().map(|&p| gg(p)).max().unwrap();
        let tail = cell_pixels(f, cw, ch, 5, 4);
        let tail_g = tail.iter().map(|&p| gg(p)).max().unwrap();
        assert!(
            non_bg_count(&tail) > 0,
            "{label}: trail tail (5,4) not drawn"
        );
        assert!(
            tail_g < head_g,
            "{label}: trail tail should be dimmer than head ({tail_g} < {head_g})"
        );
    }

    // The trail is OPAQUE (pre-blended via the shared blend_rgb), so the swept
    // trail cells must be byte-EXACT between CPU and GPU, not merely within the
    // frame-wide tolerance above.
    for (r, c) in [(5, 4), (5, 5), (5, 6)] {
        assert_eq!(
            cell_pixels(&cpu_frame, cw, ch, r, c),
            cell_pixels(&gpu_frame, cw, ch, r, c),
            "trail cell ({r},{c}) must be byte-exact CPU==GPU"
        );
    }
}

/// Sparkle-word decorations (the cat-paw and the profanity sparkle) must render
/// byte-for-byte the same on the GPU as on the CPU: the deco sprite atlas is the
/// same `procedural::deco_coverage` mask, sampled 1:1 at the cell, then alpha-
/// blended (paw, Over) or premultiplied-additive (sparkle, Add) — the GPU twins
/// of the CPU `blend` / `add_sat`. Tolerance matches the glow path.
#[test]
fn word_decorations_gpu_match_cpu() {
    use aterm_render::{DecoBlend, DecoGlyph, WordDecoration};
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let mut win = aterm_gpu::WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    // A steady pink paw and a few additive sparkles on the empty bottom row.
    input.word_decorations = vec![
        WordDecoration {
            row: 5,
            col: 3,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::Paw,
            blend: DecoBlend::Over,
            color: 0x00F7_A8B8,
            alpha: 200,
        },
        WordDecoration {
            row: 5,
            col: 6,
            dx: 1,
            dy: -1,
            glyph: DecoGlyph::Star4,
            blend: DecoBlend::Add,
            color: 0x00FF_D447,
            alpha: 230,
        },
        WordDecoration {
            row: 5,
            col: 7,
            dx: -1,
            dy: 1,
            glyph: DecoGlyph::Dot,
            blend: DecoBlend::Add,
            color: 0x007C_F0FF,
            alpha: 180,
        },
        // The Singularity nova's per-cell Over darkening ring (Sparkle Words
        // v2 §6.1): a bright-tinted RingArc so the Over stamp visibly moves
        // pixels on the dark theme — same mask on both backends, same
        // ALPHA_BLENDING regime as the paw.
        WordDecoration {
            row: 5,
            col: 9,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::RingArc,
            blend: DecoBlend::Over,
            color: 0x00B0_90FF,
            alpha: 210,
        },
        // The SUPER NOVA's light-bg eclipse veil (Sparkle Words v3 §3.3): a
        // full-cell Shade square stamped Over the "ab" text row, pinning the
        // dark-veil-over-glyph-AA regime at the suite-wide <=8 bar.
        WordDecoration {
            row: 4,
            col: 0,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::Shade,
            blend: DecoBlend::Over,
            color: 0x0018_0C2A,
            alpha: 200,
        },
    ];
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "decorations: dimensions differ"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("word-decorations GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "decoration GPU/CPU pixels diverge: max per-channel delta {delta} > 8"
    );

    // The paw cell is actually painted (pink-ish, not background) on BOTH paths.
    for (label, f) in [("cpu", &cpu_frame), ("gpu", &gpu_frame)] {
        let paw = cell_pixels(f, cw, ch, 5, 3);
        assert!(
            non_bg_count(&paw) > 0,
            "{label}: the cat-paw must be drawn at (5,3)"
        );
        // The Singularity RingArc is painted too (the atlas grew a 7th column
        // and both backends rasterize the same annulus mask).
        let ring = cell_pixels(f, cw, ch, 5, 9);
        assert!(
            non_bg_count(&ring) > 0,
            "{label}: the Singularity RingArc must be drawn at (5,9)"
        );
    }

    // An empty list is byte-identical to the no-decoration frame on BOTH.
    let plain = term.cell_frame(rows, cols);
    let cpu_plain = cpu.render_input(&plain);
    let gpu_plain = gpu.render_input(&mut win, &plain, None);
    assert!(max_channel_delta(&cpu_plain, &gpu_plain) <= 8);

    // The Shade veil visibly moves pixels over the 'a' text cell on BOTH
    // paths. (Compared against the undecorated frame, not the background
    // distance heuristic: the eclipse tint is dark by design, so over a dark
    // theme its bg-region pixels stay near-background.)
    for (label, dec, plain_f) in [
        ("cpu", &cpu_frame, &cpu_plain),
        ("gpu", &gpu_frame, &gpu_plain),
    ] {
        assert_ne!(
            cell_pixels(dec, cw, ch, 4, 0),
            cell_pixels(plain_f, cw, ch, 4, 0),
            "{label}: the Shade eclipse veil must be drawn over the text at (4,0)"
        );
    }
}

/// Decorations on a DEC double-width row (#0) and an additive sparkle over a
/// selected cell (#9) must also stay CPU==GPU: both sides NEAREST-stretch the
/// base-width sprite over the doubled advance, and both freeze the Add sparkle
/// over selected cells with the identical predicate.
#[test]
fn word_decorations_wide_row_and_selection_gpu_match_cpu() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_render::{DecoBlend, DecoGlyph, WordDecoration};
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (3usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Row 0 = DEC double-width; rows 1-2 normal. Hide the cursor for determinism.
    term.process(b"\x1b[?25lcat\r\nfuck here\r\nmore");
    // Make row 0 double-width.
    term.process(b"\x1b[1;1H\x1b#6");
    // Select row 1 cols 0..=3 (covers the additive sparkle below).
    {
        let sel = term.text_selection_mut();
        sel.start_selection(1, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(1, 3, SelectionSide::Right);
        sel.complete_selection();
    }
    let mut input = term.cell_frame(rows, cols);
    input.word_decorations = vec![
        // On the double-width row → exercises the base-width NEAREST stretch.
        WordDecoration {
            row: 0,
            col: 0,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::Paw,
            blend: DecoBlend::Over,
            color: 0x00F7_A8B8,
            alpha: 200,
        },
        WordDecoration {
            row: 0,
            col: 1,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::Star4,
            blend: DecoBlend::Add,
            color: 0x00FF_D447,
            alpha: 230,
        },
        // Additive sparkle over a SELECTED cell (row 1 col 0) → must be frozen on both.
        WordDecoration {
            row: 1,
            col: 0,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::Star4,
            blend: DecoBlend::Add,
            color: 0x00FF_D447,
            alpha: 230,
        },
        // Additive sparkle over an UNSELECTED cell (row 2) → drawn on both.
        WordDecoration {
            row: 2,
            col: 0,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::Dot,
            blend: DecoBlend::Add,
            color: 0x007C_F0FF,
            alpha: 200,
        },
    ];
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height)
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("wide-row+selection deco GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "wide-row/selection deco CPU/GPU diverge: delta {delta} > 8"
    );
}

/// Per-cell minimum-contrast floor (xterm's `minimumContrastRatio`) parity:
/// with the SAME `set_minimum_contrast` on both renderers, a frame CONTAINING
/// floored cells (bright-white truecolor text on a near-white truecolor bg)
/// stays CPU==GPU; the floor really fired (the frame differs from the
/// disabled baseline and the cell carries the shared floored colour); and a
/// concealed SGR 8 cell (resolved fg == bg upstream) stays hidden on both.
#[test]
fn minimum_contrast_floored_cells_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Hide the cursor for determinism. Row 0: bright-white on near-white
    // (~1.2:1 — the classic agent-CLI-on-a-light-theme failure). Row 1: SGR 8
    // concealed text on the same bg (resolves fg = bg; must stay hidden).
    term.process(
        b"\x1b[?25l\x1b[48;2;230;230;230m\x1b[38;2;255;255;255mXY\x1b[0m\r\n\
\x1b[48;2;230;230;230m\x1b[8mhid\x1b[0m",
    );
    let input = term.cell_frame(rows, cols);
    let (cw, ch) = cpu.cell_size();
    const CELL_BG: u32 = 0x00e6_e6e6;

    // Disabled (the default): baseline parity, output untouched by the feature.
    let cpu_off = cpu.render_input(&input);
    let gpu_off = gpu.render_input(&mut win, &input, None);
    assert!(
        max_channel_delta(&cpu_off, &gpu_off) <= 8,
        "baseline diverges"
    );

    // Enable the SAME floor on both paths.
    cpu.set_minimum_contrast(4.5);
    gpu.set_minimum_contrast(4.5);
    let cpu_on = cpu.render_input(&input);
    let gpu_on = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_on, &gpu_on);
    eprintln!("min-contrast GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "min-contrast CPU/GPU pixels diverge: max per-channel delta {delta} > 8"
    );

    // The floor really fired: the frame changed, and the 'X' cell carries the
    // SHARED floored colour on both paths (within the antialiasing tolerance).
    assert_ne!(
        cpu_on.pixels, cpu_off.pixels,
        "enabling the floor must repaint the low-contrast cells"
    );
    let expected = aterm_render::floor_min_contrast_fg(0x00ff_ffff, CELL_BG, 4.5);
    for (name, frame) in [("CPU", &cpu_on), ("GPU", &gpu_on)] {
        assert!(
            cell_pixels(frame, cw, ch, 0, 0)
                .iter()
                .any(|&p| dist(p, expected) <= 96),
            "{name}: the 'X' cell must contain floored-fg glyph pixels"
        );
        // Concealed cell (1,0): nothing but its own bg — the floor must not
        // reveal SGR 8 text on either path.
        assert!(
            cell_pixels(frame, cw, ch, 1, 0)
                .iter()
                .all(|&p| dist(p, CELL_BG) <= 24),
            "{name}: SGR 8 concealed cell must stay hidden under the floor"
        );
    }

    // Sparse cursor fallback in a composed pane: the cursor cell is not
    // materialized, so its contrast operand must be the owning pane's default,
    // not the unrelated frame scalar used for padding/divider gaps.
    let mut sparse = Terminal::new(rows as u16, cols as u16);
    sparse.process(b"\x1b[2 q\x1b[1;8H");
    let mut sparse_input = sparse.cell_frame(rows, cols);
    assert!(
        sparse_input.cells[0].get(7).is_none(),
        "fixture requires an unmaterialized cursor cell"
    );
    let pane_bg = 0x0024_4668;
    sparse_input.default_bg = 0x0012_3456;
    sparse_input.default_bg_spans = vec![
        vec![aterm_render::DefaultBgSpan::new(4, cols, pane_bg)],
        Vec::new(),
    ];
    sparse_input.cursor_color = pane_bg;
    let expected_cursor = aterm_render::floor_cursor_fill(pane_bg, pane_bg, 4.5);
    let cpu_sparse = cpu.render_input(&sparse_input);
    let mut sparse_win = aterm_gpu::WindowGpu::new();
    let gpu_sparse = gpu.render_input(&mut sparse_win, &sparse_input, None);
    for (name, frame) in [("CPU", &cpu_sparse), ("GPU", &gpu_sparse)] {
        assert!(
            cell_pixels(frame, cw, ch, 0, 7)
                .iter()
                .all(|&p| dist(p, expected_cursor) <= 3),
            "{name}: sparse cursor must be floored against its pane default"
        );
    }
}

/// Background + cursor OPACITY parity (Ghostty-style translucency): with the
/// SAME `set_background_opacity(0.5)` + `set_cursor_opacity(0.5)` on both
/// renderers, the frames agree per channel INCLUDING the alpha channel (the
/// CPU encodes it as a top-byte transmittance; `read_back` folds the GPU
/// texture alpha into the same encoding). Also gates the semantics: opacity
/// 1.0 stays byte-identical to the defaults, default-bg pixels carry alpha
/// 128 on both backends, an SGR-colored bg cell stays opaque, and the
/// translucent cursor blends over its cell instead of cutting out.
#[test]
fn background_and_cursor_opacity_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (2usize, 8usize);
    // Frame A: block cursor parked on a BLANK default cell (col 5). Used for
    // the baseline + the opacity-1.0 byte-identity gate. (An OPAQUE block
    // cursor on an overflowing glyph has a pre-existing CPU/GPU AA divergence
    // in the neighbour-cell overflow — unrelated to opacity — so the baseline
    // avoids it; the translucent path below has no cut-out and is compared ON
    // a glyph.)
    let input_at = |cursor_seq: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        // Seed the ENGINE default bg to the theme bg (the production contract)
        // so WRITTEN default-bg cells resolve to the colour the renderers
        // clear with — making them "default background" for the opacity rule.
        term.set_default_background(aterm_core::terminal::Rgb {
            r: 0x11,
            g: 0x13,
            b: 0x18,
        });
        // Col 0: SGR truecolor bg (must stay opaque). Cols 1-2: text on the
        // default bg.
        term.process(b"\x1b[48;2;0;0;128m \x1b[0mAB");
        term.process(cursor_seq);
        let blank = term.implicit_blank_render_cell();
        let mut input = term.cell_frame(rows, cols);
        for row in &mut input.cells {
            row.resize(cols, blank);
        }
        input
    };
    let mut input_blank = input_at(b"\x1b[1;6H");
    // Frame B: the SAME grid with the cursor ON the 'A' glyph — the
    // translucent-cursor-over-text case (glyph shows through on both paths).
    let mut input_glyph = input_at(b"\x1b[1;2H");
    // A composed pane's OSC-11 provenance differs from the frame scalar used
    // for padding. The cell grid still resolves default cells to `BG`, so the
    // opacity classifier must consult the pane span; comparing to the scalar
    // would incorrectly make every default cell opaque on both backends.
    for input in [&mut input_blank, &mut input_glyph] {
        input.default_bg = 0x0022_3344;
        input.default_bg_spans = vec![vec![aterm_render::DefaultBgSpan::new(0, cols, BG)]; rows];
    }
    let (cw, ch) = cpu.cell_size();

    // Baseline (defaults): parity holds and the 1.0 setters are byte-identical.
    let cpu_off = cpu.render_input(&input_blank);
    let gpu_off = gpu.render_input(&mut win, &input_blank, None);
    assert!(
        max_channel_delta(&cpu_off, &gpu_off) <= 8,
        "baseline diverges"
    );
    cpu.set_background_opacity(1.0);
    cpu.set_cursor_opacity(1.0);
    gpu.set_background_opacity(1.0);
    gpu.set_cursor_opacity(1.0);
    assert_eq!(
        cpu.render_input(&input_blank).pixels,
        cpu_off.pixels,
        "CPU: opacity 1.0 must be byte-identical to the default"
    );
    assert_eq!(
        gpu.render_input(&mut win, &input_blank, None).pixels,
        gpu_off.pixels,
        "GPU: opacity 1.0 must be byte-identical to the default"
    );

    // Both opacities active, cursor over a glyph: per-channel parity INCLUDING
    // the alpha channel (top byte = transmittance on both paths).
    cpu.set_background_opacity(0.5);
    cpu.set_cursor_opacity(0.5);
    gpu.set_background_opacity(0.5);
    gpu.set_cursor_opacity(0.5);
    let cpu_on = cpu.render_input(&input_glyph);
    let gpu_on = gpu.render_input(&mut win, &input_glyph, None);
    let mut delta = max_channel_delta(&cpu_on, &gpu_on);
    for (&pa, &pb) in cpu_on.pixels.iter().zip(gpu_on.pixels.iter()) {
        delta = delta.max(((pa >> 24) as i32 - (pb >> 24) as i32).abs());
    }
    eprintln!("opacity GPU vs CPU max per-channel (rgba) delta = {delta}");
    assert!(
        delta <= 8,
        "opacity CPU/GPU pixels diverge: max per-channel delta {delta} > 8"
    );

    // Semantics on BOTH frames: a blank default-bg cell carries alpha 128
    // (transmittance 127) with unchanged RGB; the SGR-bg cell stays opaque;
    // the translucent cursor cell never shows the raw opaque cursor fill.
    for (name, frame) in [("CPU", &cpu_on), ("GPU", &gpu_on)] {
        assert!(
            cell_pixels(frame, cw, ch, 1, 6)
                .iter()
                .all(|&p| ((p >> 24) as i32 - 127).abs() <= 1 && dist(p & 0x00ff_ffff, BG) <= 3),
            "{name}: blank default-bg cell must carry ~alpha 128 with the bg RGB"
        );
        // Every pixel opaque (the neighbour glyph's AA overflow may tint a few
        // edge pixels' RGB, but never their alpha); the body is the SGR navy.
        let sgr = cell_pixels(frame, cw, ch, 0, 0);
        assert!(
            sgr.iter().all(|&p| p >> 24 == 0),
            "{name}: SGR-colored bg cell must stay opaque"
        );
        let navy = sgr
            .iter()
            .filter(|&&p| dist(p & 0x00ff_ffff, 0x0000_0080) <= 3)
            .count();
        assert!(
            navy > sgr.len() / 2,
            "{name}: SGR-colored bg cell body must keep its SGR colour"
        );
        assert!(
            cell_pixels(frame, cw, ch, 0, 1)
                .iter()
                .all(|&p| dist(p & 0x00ff_ffff, theme.cursor & 0x00ff_ffff) > 3),
            "{name}: no pixel of the translucent cursor cell may be the raw cursor fill"
        );
    }
}

/// Animated ink (`RenderInput.ink`, Sparkle Words v2) parity: ink rides the
/// text path — both backends substitute the SAME host-resolved bytes at their
/// per-instance colour points — so an ink-only diff adds ZERO CPU/GPU delta.
/// Asserted three ways: (1) within EACH backend, inking a cell is byte-
/// identical to recolouring the same text via SGR truecolor fg (the exact-
/// substitution pin, covering glyph + combining + wide-CJK lead + underline-
/// follow + strike); (2) the inked frames stay within the suite's glyph-AA
/// delta bar CPU vs GPU; (3) empty ink is byte-identical to the plain frame on
/// both. An SGR 58 explicit underline colour still wins over ink on both.
#[test]
fn ink_gpu_matches_cpu() {
    use aterm_core::render::InkCell;
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (3usize, 16usize);
    let ink: [u8; 3] = [0x7C, 0xC8, 0xFF];

    // Row 0: combining é + underlined x + struck s + CURLY-underlined w; row 1:
    // wide 猫; row 2: an SGR 58 underlined space (ink must NOT reach that
    // underline).
    //
    // The curly cell (col 6) is the AA-undercurl path, which is a SEPARATE
    // render pass from the solid decorations of col 2 — CPU pass 3b and the
    // GPU's `curl` stream. Straight underline alone left that pass unpinned,
    // and it drifted: the CPU curl kept using the cell's own fg after ink /
    // char_fg substitution landed in every other deco site.
    let mut term_a = Terminal::new(rows as u16, cols as u16);
    term_a.process(
        "\x1b[?25le\u{0301} \x1b[4mx\x1b[24m \x1b[9ms\x1b[29m \x1b[4:3mw\x1b[24m\r\n猫\r\n\
\x1b[4m\x1b[58;2;10;20;30m \x1b[0m"
            .as_bytes(),
    );
    let mut inked = term_a.cell_frame(rows, cols);
    inked.ink = vec![
        InkCell {
            row: 0,
            col: 0,
            color: ink,
        },
        InkCell {
            row: 0,
            col: 2,
            color: ink,
        },
        InkCell {
            row: 0,
            col: 4,
            color: ink,
        },
        InkCell {
            row: 0,
            col: 6,
            color: ink,
        }, // curly-underlined w: the curl follows the ink too
        InkCell {
            row: 1,
            col: 0,
            color: ink,
        }, // 猫's LEAD cell
        InkCell {
            row: 2,
            col: 0,
            color: ink,
        }, // SGR 58 wins here
    ];

    // The same text recoloured via SGR truecolor fg (rows 0-1); row 2's SGR 58
    // underline keeps its explicit colour, so it is NOT recoloured.
    let mut term_b = Terminal::new(rows as u16, cols as u16);
    term_b.process(
        "\x1b[?25l\x1b[38;2;124;200;255me\u{0301}\x1b[39m \x1b[38;2;124;200;255m\x1b[4mx\x1b[24m\
\x1b[39m \x1b[38;2;124;200;255m\x1b[9ms\x1b[29m\x1b[39m \x1b[38;2;124;200;255m\x1b[4:3mw\x1b[24m\
\x1b[39m\r\n\x1b[38;2;124;200;255m猫\x1b[39m\r\n\
\x1b[4m\x1b[58;2;10;20;30m \x1b[0m"
            .as_bytes(),
    );
    let recolored = term_b.cell_frame(rows, cols);

    // (1) Exact substitution WITHIN each backend: ink == SGR recolor, byte-for-byte.
    let cpu_ink = cpu.render_input(&inked);
    let cpu_sgr = cpu.render_input(&recolored);
    assert_eq!(
        cpu_ink.pixels, cpu_sgr.pixels,
        "CPU: ink must be byte-identical to the SGR fg recolor"
    );
    let gpu_ink = gpu.render_input(&mut win, &inked, None);
    let gpu_sgr = gpu.render_input(&mut win, &recolored, None);
    assert_eq!(
        gpu_ink.pixels, gpu_sgr.pixels,
        "GPU: ink must be byte-identical to the SGR fg recolor (zero extra delta \
         on an ink-only diff)"
    );

    // (2) Whole-frame parity within the suite's existing glyph-AA budget.
    let delta = max_channel_delta(&cpu_ink, &gpu_ink);
    eprintln!("ink GPU vs CPU max per-channel delta = {delta}");
    assert!(delta <= 8, "inked frame CPU/GPU diverge: delta {delta} > 8");

    // (3) Empty ink stays byte-identical to the plain frame on BOTH backends.
    let plain = term_a.cell_frame(rows, cols);
    let cpu_plain = cpu.render_input(&plain);
    let gpu_plain = gpu.render_input(&mut win, &plain, None);
    let mut cleared = term_a.cell_frame(rows, cols);
    cleared.ink = inked.ink.clone();
    cleared.clear_overlays();
    assert_eq!(
        cpu.render_input(&cleared).pixels,
        cpu_plain.pixels,
        "CPU: cleared ink must render the bare frame"
    );
    assert_eq!(
        gpu.render_input(&mut win, &cleared, None).pixels,
        gpu_plain.pixels,
        "GPU: cleared ink must render the bare frame"
    );
    // Non-vacuous: the ink really recoloured pixels vs the plain frame.
    assert_ne!(cpu_ink.pixels, cpu_plain.pixels);
}

/// Ink under selection and on a DEC double-width row stays CPU==GPU: both
/// backends apply ink FIRST, then the selection fg floor (selection legibility
/// wins over ink) with the identical `floor_selection_fg` bytes, and both
/// NEAREST-stretch the inked glyph over the doubled advance.
#[test]
fn ink_selection_and_decdwl_gpu_match_cpu() {
    use aterm_core::render::InkCell;
    use aterm_core::selection::{SelectionSide, SelectionType};
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (2usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Row 0 = DEC double-width "cat"; row 1 = "fuck here", cols 0..=3 selected.
    term.process(b"\x1b[?25lcat\r\nfuck here");
    term.process(b"\x1b[1;1H\x1b#6");
    {
        let sel = term.text_selection_mut();
        sel.start_selection(1, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(1, 3, SelectionSide::Right);
        sel.complete_selection();
    }
    let mut input = term.cell_frame(rows, cols);
    // Ink chosen near the selection band colour so the selection floor MUST
    // move it — pinning the ink→selection-floor order on both backends.
    let ink: [u8; 3] = [0x33, 0x41, 0x5E];
    input.ink = vec![
        InkCell {
            row: 0,
            col: 0,
            color: [0xF7, 0xA8, 0xB8],
        }, // DECDWL row
        InkCell {
            row: 1,
            col: 0,
            color: ink,
        }, // selected
        InkCell {
            row: 1,
            col: 1,
            color: ink,
        }, // selected
        InkCell {
            row: 1,
            col: 5,
            color: [0xF7, 0xA8, 0xB8],
        }, // unselected
    ];

    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height)
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("ink selection+DECDWL GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "ink selection/DECDWL CPU/GPU diverge: delta {delta} > 8"
    );

    // The selection floor really fired on the ink: the selected inked cell
    // carries the floored colour (not the raw ink) on BOTH backends.
    let (cw, ch) = cpu.cell_size();
    let expected = aterm_render::floor_selection_fg(
        aterm_render::rgb_to_u32(ink),
        cpu.effective_selection_bg(),
    );
    assert_ne!(expected, aterm_render::rgb_to_u32(ink));
    for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        assert!(
            cell_pixels(frame, cw, ch, 1, 0)
                .iter()
                .any(|&p| dist(p, expected) <= 24),
            "{name}: the selected inked cell must carry the selection-floored ink"
        );
    }
    // Non-vacuous: ink changed the frame vs no ink.
    let plain = term.cell_frame(rows, cols);
    assert_ne!(cpu_frame.pixels, cpu.render_input(&plain).pixels);
}

/// TWO panes selected on the SAME rows, each with its OWN live OSC 17/19 colour
/// and its own focus — the composed-split shape `push_pane_selection` produces.
///
/// This is the highest-risk spot for the ±8 gate: the band colour is no longer
/// one hoisted scalar but a per-entry resolution, and doing that resolution
/// twice (once in `aterm-render`, once in `aterm-gpu`) is exactly how the two
/// faces start drifting. Both go through `RenderInput::selection_hit` and
/// `Renderer::selection_palette`, so this pins that they still derive one pixel.
#[test]
fn per_pane_selection_colours_gpu_match_cpu() {
    use aterm_core::render::{PaneSelection, SelectionClip};
    use aterm_core::selection::{SelectionSide, SelectionType, TextSelection};

    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let mut win = aterm_gpu::WindowGpu::new();

    let (rows, cols) = (2usize, 17usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Two panes of 8 columns with a 1-cell divider at column 8.
    term.process(b"\x1b[?25lleft one|right on\r\nleft two|right tw");
    let mut input = term.cell_frame(rows, cols);

    let pane = |lo: u16, hi: u16, clip: SelectionClip, bg: u32, fg: u32, inactive: bool| {
        let mut selection = TextSelection::new();
        selection.start_selection(0, lo, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(1, hi, SelectionSide::Right);
        selection.complete_selection();
        PaneSelection {
            selection,
            clip,
            bg,
            fg,
            inactive,
        }
    };
    input.selections = vec![
        // Focused pane: a live OSC 17 band with an explicit OSC 19 ink.
        pane(
            0,
            7,
            SelectionClip::new(0, 2, 0, 8),
            0x0021_4365,
            0x00fe_dcba,
            false,
        ),
        // Unfocused pane: no live colour at all, so it takes the theme policy —
        // and, being unfocused, its INACTIVE derivation.
        pane(
            9,
            16,
            SelectionClip::new(0, 2, 9, 17),
            aterm_core::render::COLOR_UNSET,
            aterm_core::render::COLOR_UNSET,
            true,
        ),
    ];

    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height)
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("per-pane selection GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "per-pane selection CPU/GPU diverge: delta {delta} > 8"
    );

    // Non-vacuous, and the two bands really are DIFFERENT colours on both faces:
    // the focused pane wears its live OSC 17, the unfocused one the derived
    // inactive theme band. A regression that hoists one scalar again would make
    // these equal — and would still pass the delta gate above.
    let (cw, ch) = cpu.cell_size();
    let live = 0x0021_4365;
    let dim = aterm_render::derive_inactive_selection_bg(theme.selection, theme.bg);
    assert_ne!(live, dim, "the fixture's two bands must differ");
    for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        assert!(
            cell_pixels(frame, cw, ch, 0, 7)
                .iter()
                .any(|&p| dist(p, live) <= 24),
            "{name}: the focused pane's band is its own live OSC 17 colour"
        );
        assert!(
            cell_pixels(frame, cw, ch, 0, 16)
                .iter()
                .any(|&p| dist(p, dim) <= 24),
            "{name}: the unfocused pane's band is the derived inactive colour"
        );
        assert!(
            cell_pixels(frame, cw, ch, 0, 8)
                .iter()
                .all(|&p| dist(p, live) > 24 && dist(p, dim) > 24),
            "{name}: the divider between them takes neither band"
        );
    }

    // …and dropping the list hands the frame back to the scalar authority, which
    // here paints nothing — proof the list is what produced those bands.
    let plain = term.cell_frame(rows, cols);
    assert_ne!(cpu_frame.pixels, cpu.render_input(&plain).pixels);
}

/// A scrollback-seeded grid with distinct per-row colour/text so any stray
/// vertical shift is detectable, scrolled into history (a real smooth-scroll
/// frame). Mirrors aterm-render's `scroll_frac_translate` seed.
fn scroll_seeded_term(rows: usize, cols: usize) -> Terminal {
    let mut t = Terminal::new(rows as u16, cols as u16);
    for i in 0..40 {
        let line = if i % 2 == 0 {
            format!("\x1b[3{}mrow {:02} abcdef\x1b[0m\r\n", (i % 6) + 1, i)
        } else {
            format!("\x1b[1mROW {i:02} XYZWUV\x1b[0m\r\n")
        };
        t.process(line.as_bytes());
    }
    t.scroll_display(5); // show scrollback
    t
}

/// M1b SUB-ROW SCROLL PARITY: the GPU grid-band pixel shift matches the CPU
/// present translate for a FRACTIONAL-scroll frame at an asymmetric grid
/// origin (`head != 0`, `pad_top != pad`). With a chrome partition (row 0 =
/// tab strip, last row = bottom chrome) and a nonzero `scroll_frac_px`, (1) the
/// CPU and GPU translated frames agree within the standard parity tolerance,
/// (2) chrome rows (outside `[grid_top_row, grid_bot_row)`) are byte-identical
/// to the frac-0 frame on BOTH backends (chrome invariance), and (3) some grid
/// pixels genuinely moved (non-vacuity). This is the GPU oracle for the
/// offscreen band shift `shift_offscreen_band`.
#[test]
fn sub_row_scroll_translate_gpu_matches_cpu_at_asymmetric_origin() {
    let theme = Theme::default();
    let px = 18.0;
    let (rows, cols) = (8usize, 20usize);

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    const PAD: usize = 12;
    const PAD_TOP: usize = 3;
    const HEAD: usize = 17;
    cpu.set_pad(PAD);
    cpu.set_pad_top(PAD_TOP);
    cpu.set_head(HEAD);
    gpu.set_pad(PAD);
    gpu.set_pad_top(PAD_TOP);
    gpu.set_head(HEAD);
    let (_cw, ch) = cpu.cell_size();
    let grid_top = cpu.grid_top();
    assert_eq!(grid_top, PAD_TOP + HEAD);
    assert_ne!(
        grid_top, PAD,
        "regression must exercise the asymmetric origin"
    );
    // The chrome partition: row 0 = strip, last row = bottom chrome; middle = terminal grid.
    let with_partition = |t: &mut Terminal, frac: i32| {
        let mut input = t.cell_frame(rows, cols);
        input.grid_top_row = 1;
        input.grid_bot_row = rows - 1;
        input.scroll_frac_px = frac;
        input
    };
    // The grid band in device px (renderer row→px mapping) for the chrome check.
    let y0 = grid_top + ch; // grid_top_row = 1
    let y1 = grid_top + (rows - 1) * ch; // grid_bot_row = rows-1

    let mut win = aterm_gpu::WindowGpu::new();
    // Frac-0 baselines on each backend.
    let cpu_zero = {
        let mut t = scroll_seeded_term(rows, cols);
        cpu.render_input(&with_partition(&mut t, 0))
    };
    let gpu_zero = {
        let mut t = scroll_seeded_term(rows, cols);
        gpu.render_input(&mut win, &with_partition(&mut t, 0), None)
    };
    let (w, h) = (cpu_zero.width, cpu_zero.height);
    assert_eq!(
        (gpu_zero.width, gpu_zero.height),
        (w, h),
        "dims match at frac 0"
    );

    let mut any_moved = false;
    let mut any_down = false;
    // BIDIRECTIONAL sweep: positive fracs shift the grid band UP (glide), NEGATIVE
    // fracs shift it DOWN (the elastic-overscroll bounce). The GPU offscreen band
    // shift must match the CPU present translate for BOTH signs.
    for frac in [
        -(ch as i32 - 1),
        -((ch as i32) / 2),
        -3,
        -1,
        1i32,
        3,
        (ch as i32) / 2,
        ch as i32 - 1,
    ] {
        let mut tc = scroll_seeded_term(rows, cols);
        let cpu_f = cpu.render_input(&with_partition(&mut tc, frac));
        let mut tg = scroll_seeded_term(rows, cols);
        let gpu_f = gpu.render_input(&mut win, &with_partition(&mut tg, frac), None);
        assert_eq!(
            (gpu_f.width, gpu_f.height),
            (w, h),
            "dims stable across frac"
        );

        // (1) CPU == GPU for the TRANSLATED frame, within the standard tolerance.
        let delta = max_channel_delta(&cpu_f, &gpu_f);
        assert!(
            delta <= 8,
            "M1b frac={frac}: GPU/CPU translated frames diverge (max delta {delta} > 8)"
        );

        // (2) Chrome invariance on BOTH backends: rows outside [y0, y1) equal the
        //     frac-0 frame byte-for-byte (the shift touched only the grid band).
        for y in 0..h {
            if y < y0 || y >= y1 {
                assert_eq!(
                    &gpu_f.pixels[y * w..y * w + w],
                    &gpu_zero.pixels[y * w..y * w + w],
                    "M1b frac={frac}: GPU chrome row {y} must be invariant"
                );
                assert_eq!(
                    &cpu_f.pixels[y * w..y * w + w],
                    &cpu_zero.pixels[y * w..y * w + w],
                    "M1b frac={frac}: CPU chrome row {y} must be invariant"
                );
            }
        }
        // (3) Non-vacuity: the grid band genuinely shifted on the GPU.
        if gpu_f.pixels != gpu_zero.pixels {
            any_moved = true;
            if frac < 0 {
                any_down = true;
            }
        }
    }
    assert!(
        any_moved,
        "non-vacuity: some fractional shift must move GPU grid-band pixels"
    );
    assert!(
        any_down,
        "non-vacuity: a NEGATIVE frac (overscroll bounce) must move GPU pixels DOWN"
    );
}
