// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Correctness gate for the GPU renderer: render the same terminal on the CPU
// (`aterm_render::Renderer`) and the GPU (`aterm_gpu::GpuRenderer`) at the
// same px/theme and check that the frames match:
//   1. identical frame dimensions,
//   2. per-channel delta within a small tolerance (only rounding differs),
//   3. the semantic properties of the CPU visual-regression test (red cell is
//      red, blue-bg cell is blue, CJK cell non-blank, blank cell background).
//
// Skips when there is no GPU or no system font.

use crate::common;
use aterm_core::{grid::LineSize, render::LineSizeSpan, terminal::Terminal};
use aterm_render::{Frame, Renderer, Theme};

use crate::common::{backends, bb, gg, max_channel_delta_frame as max_channel_delta, rr};

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

/// Feed `bytes` to a fresh `rows`×`cols` terminal and render that one frame on
/// both backends (the GPU through `win`): `(cpu_frame, gpu_frame)`.
fn render_both(
    cpu: &mut Renderer,
    gpu: &mut aterm_gpu::GpuRenderer,
    win: &mut aterm_gpu::WindowGpu,
    (rows, cols): (usize, usize),
    bytes: &[u8],
) -> (Frame, Frame) {
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(bytes);
    let input = term.cell_frame(rows, cols);
    (
        cpu.render_input(&input),
        gpu.render_input(win, &input, None),
    )
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

/// `backends` returns a font-settled pair: neither renderer holds an unstarted
/// lazy fallback chain. Otherwise the CJK row can race (one backend's chain
/// landed, the other's not). Checked white-box: `Renderer::ensure_fallback`
/// takes its candidate-path list when it starts the parse, so an empty list
/// means the chain is no longer pending, on any machine load.
#[test]
fn backends_hand_back_a_font_settled_pair() {
    // Non-vacuity: a raw renderer on THIS host really does start with candidates
    // to take, so the emptiness asserted below has teeth here.
    let Some(raw) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    if raw.debug_fallback_candidate_paths().is_empty() {
        eprintln!("SKIP: no lazy fallback candidates on this host");
        return;
    }
    let Some((cpu, gpu)) = backends(18.0, Theme::default()) else {
        return;
    };
    assert!(
        cpu.debug_fallback_candidate_paths().is_empty(),
        "the CPU renderer still holds an unstarted fallback chain: its first \
         renders will draw `.notdef` where the chain would draw a glyph, and a \
         parity comparison that straddles the arrival reads it as a divergence"
    );
    assert!(
        gpu.debug_fallback_candidate_paths().is_empty(),
        "the GPU renderer still holds an unstarted fallback chain (the pair has \
         two chains, and each races its own parse)"
    );
}

/// Interior padding keeps GPU/CPU parity: with the same `pad`, frames have the
/// same grown dimensions, the grid lands on the same pixels within tolerance,
/// and the theme-bg border matches exactly.
#[test]
fn gpu_matches_cpu_with_padding() {
    let theme = Theme::default();
    let px = 18.0;
    const P: usize = 10;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
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

/// A composed split row carries one DEC line-size span per pane, resolved per
/// cell: a double-width pane beside a single-width one (and the inverse on the
/// next row), with the clipped-away logical half of each double-width pane
/// never reappearing in the neighbour.
#[test]
fn gpu_matches_cpu_with_pane_local_mixed_dec_widths() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (2usize, 17usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Row 0: red cell in the left pane, a magenta poison cell in its
    // clipped-away half, green in the right pane. Row 1 reverses the widths.
    // Cursor hidden.
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

/// Glyph enlargement per pane: a real glyph in each pane of a composed row,
/// with a span only for the DEC pane and `line_sizes[row]` set to that pane's
/// size (as the compositor writes it). The unclaimed pane's glyph must stay 1×
/// on both backends. Row 0 tests width (DECDWL), row 1 height and anchor
/// (DECDHL bottom).
#[test]
fn gpu_matches_cpu_for_glyph_enlargement_inside_its_own_dec_run() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (2usize, 17usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // One bright-red 'M' at the head of each pane on each row (cursor hidden).
    // A 2× 'M' occupies the next physical cell, a 1× does not. Assertions
    // count red ink, since written blank cells carry the terminal default bg.
    term.process(b"\x1b[?25l\x1b[38;2;255;0;0m\x1b[1;1HM\x1b[1;10HM\x1b[2;1HM\x1b[2;10HM\x1b[0m");
    let mut input = term.cell_frame(rows, cols);
    input.line_size_spans.resize_with(rows, Vec::new);
    // Row 0: left pane DECDWL, right pane unclaimed. Row 1: right pane
    // DECDHL-bottom, left unclaimed. `line_sizes[r]` is the row summary.
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
        // Negative control: the second physical cell of each spanned pane
        // carries the 2× glyph's right half.
        assert!(
            ink(frame, 0, 1) > 4,
            "{label}: DECDWL pane's glyph did not spill into its own second cell"
        );
        assert!(
            ink(frame, 1, 10) > 4,
            "{label}: DECDHL pane's glyph did not spill into its own second cell"
        );
        // The unclaimed pane keeps a 1× glyph, so the cell after its 'M' has
        // no ink.
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

/// An odd-width pane can expose only half of its last DEC double-width cell;
/// the curly underline paints that visible part and stays out of the divider.
#[test]
fn gpu_matches_cpu_for_partial_double_width_pane_undercurl() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (1usize, 15usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Logical col 3 starts at physical col 6; the pane ends at physical col 7,
    // leaving one base cell of the 2× undercurl visible.
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

/// Italic ink may overhang its cell but not its pane: the pane clip removes
/// the synthetic-italic `f` bearing at a one-column divider on both backends.
#[test]
fn gpu_matches_cpu_and_clips_italic_ink_at_pane_edge() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

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

/// Mixed DPI: two sizes through one renderer pair via `activate_px`, which
/// switches size without dropping the atlas. CPU/GPU parity holds at each size,
/// interleaved A, B, A, B, so the switch is order-independent and never mixes
/// one size's metrics into the other's frame.
#[test]
fn gpu_matches_cpu_mixed_dpi_activate_px() {
    let theme = Theme::default();
    let px_a = 18.0; // a 1× (laptop) window
    let px_b = 36.0; // a 2× (external Retina) window

    let Some((mut cpu, mut gpu)) = backends(px_a, theme) else {
        return;
    };

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

/// Parity after dynamic colour changes (OSC 4 palette, OSC 10/11 default fg/bg,
/// OSC 17/19 selection). Both renderers consume the same recoloured
/// `RenderInput`, so a divergence means a stale colour (e.g. a glyph cached
/// without its colour in the key). Also checks the changes took effect.
#[test]
fn gpu_matches_cpu_after_dynamic_osc_colors() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

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
    // The recolour landed on the GPU frame. OSC 11's new default bg shows on
    // written default cells; empty cells and margins keep the renderer's clear
    // colour, so only written cells are checked.
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

/// Parity with a named theme applied through `TerminalConfig::apply_config`
/// (the GUI path), with several ANSI colours written and checked in the GPU
/// frame.
#[test]
fn gpu_matches_cpu_with_named_theme() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

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

/// Exact parity (delta 0) on orthogonal procedural cells (box drawing, blocks,
/// shades, braille, sextants, eighths, octants): hard 0/255 coverage, so both
/// blends agree on every pixel. Cursor hidden, so the whole frame is in the
/// exactness domain. Anti-aliased procedural families are tested with the
/// tolerance in `aa_procedural_cells_match_cpu_within_tolerance`; keep this
/// test exact.
#[test]
fn procedural_cells_match_cpu_exactly() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (5usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Cursor hidden. Row 2 paints a red double-junction run (fg tint path);
    // row 4 dashes, eighth blocks and U+1FB70–1FB8B; row 5 the octants
    // U+1CD00–1CDE5.
    term.process(
        "\x1b[?25l\
\u{250C}\u{2500}\u{252C}\u{2500}\u{2510}\u{2554}\u{2550}\u{2566}\u{2550}\u{2557}\u{2501}\u{2513}\u{2517}\u{2503}\u{254B}\r\n\
\u{251C}\u{2500}\u{253C}\u{2500}\u{2524}\x1b[31m\u{2560}\u{2550}\u{256C}\u{2550}\u{2563}\x1b[0m\u{2580}\u{2584}\u{258C}\u{2590}\u{2588}\r\n\
\u{2514}\u{2500}\u{2534}\u{2500}\u{2518}\u{255A}\u{2550}\u{2569}\u{2550}\u{255D}\u{2591}\u{2592}\u{2593}\u{2847}\u{28FF}\r\n\
\u{2504}\u{2508}\u{254C}\u{2581}\u{2582}\u{258E}\u{1FB13}\u{1FB70}\u{1FB76}\u{1FB7C}\u{1FB80}\u{1FB81}\u{1FB82}\u{1FB87}\u{1FB8B}\r\n\
\u{1CD00}\u{1CD01}\u{1CD17}\u{1CD2E}\u{1CD45}\u{1CD5C}\u{1CD73}\u{1CD8A}\u{1CDA1}\u{1CDB8}\u{1CDCF}\u{1CDDD}\u{1CDE4}\u{1CDE5}"
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
    // The fixture drew glyphs: default-fg stroke pixels exist and the red run
    // produced red-dominant pixels.
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

/// The anti-aliased procedural families (arcs, diagonals, Powerline,
/// wedges/triangles) match within the ≤8 LSB tolerance; their cell-edge texels
/// are hard, so seams stay exact. Every AA cell must draw.
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

/// Shade-phase parity: ░▒▓ dither on absolute pixel parity, computed at the CPU
/// blit, the GPU quad emission and the GPU atlas pass. An odd pad flips every
/// cell's phase, so a missing fold shows as a full fg/bg swing. Exact
/// (delta 0).
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

/// Parity in the `linear` text-blending mode as well as the default: the demo
/// grid within tolerance and procedural cells exactly. Also checks the two
/// modes differ on antialiased text, so the mode flag is live.
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

    // Default (corrected) CPU frame for the control below, settled because the
    // fallback chain can land on any render (see `settled_cpu`).
    let corrected_frame = common::settled_cpu(&mut cpu, &input);

    cpu.set_text_blending(aterm_render::TextBlending::Linear);
    gpu.set_text_blending(aterm_render::TextBlending::Linear);
    let cpu_frame = common::settled_cpu(&mut cpu, &input);
    let gpu_frame = common::settled_gpu(&mut gpu, &mut win, &input);

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

    // Procedural cells are hard coverage, so the mode is invisible there and
    // parity is exact.
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

/// Colour emoji: the GPU reproduces the CPU's RGBA emoji blit. Emoji cells are
/// drawn and match within tolerance. Skips without a GPU, font, or colour-emoji
/// font.
#[test]
fn colour_emoji_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

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

    // The colour atlas holds the CPU's exact pixels, so only edge blend
    // rounding differs (≤8 LSB).
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

/// VS16 parity: `❤️` (U+2764 + VS16) has a mono glyph in text fonts, so both
/// renderers must honour `RenderCell::emoji_presentation` and draw a red heart
/// from the colour face. Skips without a colour-emoji font.
#[test]
fn vs16_emoji_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

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

/// VS15 text presentation: 😀 narrowed to one cell with X in the next column.
/// Both backends draw a text glyph and leave the X cell identical to a
/// blank+X control. VS16 and CJK stay wide.
#[test]
fn vs15_text_emoji_is_one_cell_on_cpu_and_gpu_without_adjacent_overpaint() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

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

    // Controls: VS16 and CJK remain two-cell units, so their X starts at col 2.
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
/// SHAPES (rustybuzz) to a single colour glyph, and 🚀 ZWJ 🍕 one no face ligates,
/// which it composites from its two glyphs. Both paths must draw that glyph or
/// composite, not just the base codepoint. Proves the GPU resolves cluster keys
/// via the shared `resolve_cell_key` and atlases them identically to the CPU.
/// Gated on a colour-emoji font (if the CPU family cell is blank, the host has
/// none).
///
/// Checks CPU/GPU parity and non-blank coverage only, not that the glyph is the
/// correct emoji.
#[test]
fn cluster_emoji_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (2usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // family (0-1) sp(2) skin (3-4) sp(5) keycap (6) sp(7) flag (8-9);
    // row 1: the unligated rocket-pizza sequence (0-1)
    term.process(
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{1F44D}\u{1F3FD} \u{31}\u{FE0F}\u{20E3} \u{1F1FA}\u{1F1F8}\r\n\u{1F680}\u{200D}\u{1F355}".as_bytes(),
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
    // regional-indicator flag col 8, and both cells of the composite).
    for (label, row, col) in [
        ("family", 0usize, 0usize),
        ("skin", 0, 3),
        ("keycap", 0, 6),
        ("flag", 0, 8),
        ("composite rocket", 1, 0),
        ("composite pizza", 1, 1),
    ] {
        let gpu_cell = non_bg_count(&cell_pixels(&gpu_frame, cw, ch, row, col));
        assert!(
            gpu_cell > 12,
            "GPU {label} cluster (cell {row},{col}) is blank ({gpu_cell} non-bg pixels) — cluster not shaped"
        );
    }

    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("cluster emoji GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU cluster emoji pixels diverge: max per-channel delta {delta} > 8"
    );
}

/// Line decorations (underline, strikethrough, double underline) use the
/// shared rect helpers, so the GPU matches the CPU within tolerance and the
/// decorated frame differs from an undecorated one on both paths.
#[test]
fn decorations_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let dims = (3usize, 8usize);
    let mut win = aterm_gpu::WindowGpu::new();

    // Underlined, strikethrough, double-underlined rows.
    let (cpu_deco, gpu_deco) = render_both(
        &mut cpu,
        &mut gpu,
        &mut win,
        dims,
        b"\x1b[4mUU\x1b[0m\r\n\x1b[9mSS\x1b[0m\r\n\x1b[21mDD\x1b[0m",
    );
    // Same glyphs, no decorations.
    let (cpu_plain, _) = render_both(&mut cpu, &mut gpu, &mut win, dims, b"UU\r\nSS\r\nDD");

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

/// Decorations stay CPU==GPU: the AA undercurl (SGR 4:3), dotted (4:4) and
/// dashed (4:5) runs phased on absolute x, an SGR 58 coloured undercurl, and
/// descender ink-skip on and off. Turning the skip off changes the output
/// identically on both.
#[test]
fn underlines_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let dims = (4usize, 10usize);
    let mut win = aterm_gpu::WindowGpu::new();
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
        let (cpu_f, gpu_f) = render_both(&mut cpu, &mut gpu, &mut win, dims, txt);
        assert_eq!(
            (gpu_f.width, gpu_f.height),
            (cpu_f.width, cpu_f.height),
            "dimensions differ (skip={skip})"
        );
        let delta = max_channel_delta(&cpu_f, &gpu_f);
        eprintln!("underlines GPU vs CPU max per-channel delta (skip={skip}) = {delta}");
        assert!(
            delta <= 8,
            "underline GPU/CPU pixels diverge (skip={skip}): max per-channel delta {delta} > 8"
        );
    }

    // Skip on vs off must differ under descenders.
    cpu.set_underline_skip_descenders(true);
    gpu.set_underline_skip_descenders(true);
    let (cpu_on, _) = render_both(&mut cpu, &mut gpu, &mut win, dims, txt);
    cpu.set_underline_skip_descenders(false);
    gpu.set_underline_skip_descenders(false);
    let (cpu_off, _) = render_both(&mut cpu, &mut gpu, &mut win, dims, txt);
    assert!(
        cpu_on.pixels != cpu_off.pixels,
        "descender ink-skip must change the rendered underline under gyqj"
    );

    // The undercurl actually draws: the curly row differs from plain text on
    // both paths (neither silently dropped the mask/quads).
    cpu.set_underline_skip_descenders(true);
    gpu.set_underline_skip_descenders(true);
    let (cpu_curl, gpu_curl) =
        render_both(&mut cpu, &mut gpu, &mut win, dims, b"\x1b[4:3mwavy\x1b[0m");
    let (cpu_plain, gpu_plain) = render_both(&mut cpu, &mut gpu, &mut win, dims, b"wavy");
    assert!(
        cpu_curl.pixels != cpu_plain.pixels,
        "CPU undercurl drew nothing"
    );
    assert!(
        gpu_curl.pixels != gpu_plain.pixels,
        "GPU undercurl drew nothing"
    );
}

/// Synthetic bold/italic are baked into cached coverage keyed by style, so the
/// GPU matches, and styled text differs from plain on both paths.
#[test]
fn bold_italic_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let dims = (3usize, 8usize);
    let mut win = aterm_gpu::WindowGpu::new();

    // Cursor visible: the wide synthetic-bold-italic 'W' at (2,1) overflows into
    // the BLOCK-cursor cell (2,2). The CPU paints the block cursor LAST (over the
    // overflow) and the GPU fills it AFTER the glyph passes too (was: cursor bg
    // in the bg pass, so overflow drew on top — a ~137-LSB divergence).
    let (cpu_styled, gpu_styled) = render_both(
        &mut cpu,
        &mut gpu,
        &mut win,
        dims,
        b"\x1b[1mBB\x1b[0m\r\n\x1b[3mII\x1b[0m\r\n\x1b[1;3mWW\x1b[0m",
    );
    let (cpu_plain, _) = render_both(&mut cpu, &mut gpu, &mut win, dims, b"BB\r\nII\r\nWW");

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

/// A SUBSCRIPT cell (`Script::Sub`, chrome-only: the Claude footer's facts)
/// draws its glyph smaller and lower — no glyph's ink, a parenthesis taller
/// than the letters included, above `SUB_CEILING` — the rule glyph beside it
/// full size; and the GPU, which takes the CPU's raster and placement,
/// matches it.
#[test]
fn sub_script_gpu_matches_cpu() {
    use aterm_core::terminal::Script;
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let (rows, cols) = (2usize, 4usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("H\u{2500}(H\r\nH\u{2500}(H".as_bytes());
    let mut input = term.cell_frame(rows, cols);
    for cell in &mut input.cells[0] {
        cell.script = Script::Sub;
    }
    let mut win = aterm_gpu::WindowGpu::new();
    let (cf, gf) = (
        cpu.render_input(&input),
        gpu.render_input(&mut win, &input, None),
    );
    assert_eq!((gf.width, gf.height), (cf.width, cf.height));
    let delta = max_channel_delta(&cf, &gf);
    assert!(delta <= 8, "GPU/CPU sub-script pixels diverge: {delta} > 8");
    let (cw, ch) = cpu.cell_size();
    let ink_rows = |f: &Frame, row: usize, col: usize| {
        let px = cell_pixels(f, cw, ch, row, col);
        // The cell's ground is its corner pixel (the frame's palette, not
        // the theme's).
        let ground = px[0];
        let inked = |p: &u32| dist(*p, ground) > 24;
        let rows: Vec<usize> = (0..ch)
            .filter(|y| px[y * cw..(y + 1) * cw].iter().any(inked))
            .collect();
        (
            rows[0],
            *rows.last().unwrap(),
            px.iter().filter(|p| inked(p)).count(),
        )
    };
    for f in [&cf, &gf] {
        let (sub_top, _, sub_ink) = ink_rows(f, 0, 0);
        let (top, _, ink) = ink_rows(f, 1, 0);
        assert!(
            sub_top > top && sub_ink < ink,
            "the sub H is smaller and lower: top {sub_top} vs {top}, ink {sub_ink} vs {ink}"
        );
        assert_eq!(
            cell_pixels(f, cw, ch, 0, 1),
            cell_pixels(f, cw, ch, 1, 1),
            "the rule glyph keeps its full size"
        );
        let ceiling = (ch as f32 * aterm_render::SUB_CEILING).ceil() as usize;
        for col in [0, 2, 3] {
            let (top, _, _) = ink_rows(f, 0, col);
            assert!(
                top >= ceiling,
                "col {col}: ink from row {top}, ceiling {ceiling}"
            );
        }
    }
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

    let dims = (1usize, 16usize);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();

    // DECDWL line vs the same text single-width, cursor hidden.
    let (cpu_dw, gpu_dw) = render_both(&mut cpu, &mut gpu, &mut win, dims, b"\x1b[?25l\x1b#6ABCD");
    let (cpu_sw, _) = render_both(&mut cpu, &mut gpu, &mut win, dims, b"\x1b[?25lABCD");

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

    // 'C' sits at col 2 single-width but col 4 double-width; the single-width
    // frame's col 4 is blank.
    let dw_at4 = non_bg_count(&cell_pixels(&cpu_dw, cw, ch, 0, 4));
    let sw_at4 = non_bg_count(&cell_pixels(&cpu_sw, cw, ch, 0, 4));
    assert!(
        dw_at4 > 12 && sw_at4 < 12,
        "DECDWL not 2× wide (dw col4={dw_at4}, sw col4={sw_at4})"
    );
}

/// DECDHL lines: the same text on two rows forms one 2× line (top and bottom
/// halves). The GPU's visible slice via the shared `glyph_quad` matches the
/// CPU's replicate and clip.
#[test]
fn decdhl_double_height_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let dims = (2usize, 16usize);
    let mut win = aterm_gpu::WindowGpu::new();

    // DECDHL top + bottom halves vs the same text plain (cursor hidden).
    let (cpu_dh, gpu_dh) = render_both(
        &mut cpu,
        &mut gpu,
        &mut win,
        dims,
        b"\x1b[?25l\x1b#3BIG\r\n\x1b#4BIG",
    );
    let (cpu_plain, _) = render_both(&mut cpu, &mut gpu, &mut win, dims, b"\x1b[?25lBIG\r\nBIG");

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

/// Powerline separators (U+E0B0–E0BF) are anti-aliased procedural glyphs, so
/// they match within ≤8 LSB, and they must draw.
#[test]
fn powerline_cells_match_cpu_within_tolerance() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

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

/// Last-resort fontless symbols (`font_chain::Tier::Synthetic`): `⏵⏵` and its
/// siblings, ⭘, and the procedural `⎿` draw the same pixels on both backends,
/// whatever face the host resolves, and every cell has ink.
#[test]
fn synthetic_symbol_cells_match_cpu_within_tolerance() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let glyphs = [
        '\u{23F5}', '\u{23F4}', '\u{23F6}', '\u{23F7}', '\u{2B58}', '\u{23BF}',
    ];
    // Cursor hidden; each glyph, then one blank column.
    let line: String = glyphs.iter().map(|g| format!("{g} ")).collect();
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (cpu_frame, gpu_frame) = render_both(
        &mut cpu,
        &mut gpu,
        &mut win,
        (1, glyphs.len() * 2),
        format!("\x1b[?25l{line}").as_bytes(),
    );

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dims"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("synthetic symbol GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "synthetic symbol cells diverge: max per-channel delta {delta} > 8"
    );
    for (i, g) in glyphs.iter().enumerate() {
        let n = non_bg_count(&cell_pixels(&gpu_frame, cw, ch, 0, i * 2));
        assert!(n > 4, "{g:?} drew no ink on the GPU ({n} non-bg)");
    }
}

/// The wide branch of the fontless synthesis: emoji-presentation symbols
/// (⏩ ⏪ ⏫ ⚪ ⚫) with no colour face rasterize one `2 * cell_w` bitmap across
/// both cells. Built from bundled DejaVu Sans Mono with runtime discovery off,
/// so the synthesis is the only answer; each glyph is asserted
/// `FaceId::Procedural`.
#[test]
fn wide_synthetic_symbol_cells_match_cpu_within_tolerance() {
    let theme = Theme::default();
    let px = 18.0;
    let ctx = match aterm_gpu::GpuContext::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: no GPU available: {e}");
            return;
        }
    };
    // aterm-gpu builds aterm-render without `embedded-font`, so read the
    // bundled asset directly (the same bytes `embedded_font()` would return).
    let face: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    // Runtime discovery off on both, so no system face answers first.
    let fontless = || {
        let mut r = Renderer::from_bytes(face, px, theme).expect("bundled face parses");
        r.set_runtime_font_discovery(false);
        r
    };
    let mut gpu =
        aterm_gpu::GpuRenderer::from_parts(ctx, fontless(), None, theme).expect("GPU renderer");
    let mut cpu = fontless();

    let glyphs = ['\u{23E9}', '\u{23EA}', '\u{23EB}', '\u{26AA}', '\u{26AB}'];
    for &g in &glyphs {
        let key = cpu.glyph_key(g);
        assert_eq!(
            (key.source, key.cell_span),
            (aterm_render::FaceId::Procedural, 2),
            "{g:?} must reach the two-cell synthesis in a colour-face-free renderer"
        );
    }
    // Cursor hidden; each wide glyph takes two columns, then one blank
    // separator column.
    let line: String = glyphs.iter().map(|g| format!("{g} ")).collect();
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (cpu_frame, gpu_frame) = render_both(
        &mut cpu,
        &mut gpu,
        &mut win,
        (1, glyphs.len() * 3),
        format!("\x1b[?25l{line}").as_bytes(),
    );

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dims"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("wide synthetic symbol GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "wide synthetic symbol cells diverge: max per-channel delta {delta} > 8"
    );
    for (i, g) in glyphs.iter().enumerate() {
        // Centred across BOTH cells: ink in each half, on both backends.
        for half in 0..2 {
            let col = i * 3 + half;
            for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
                let n = non_bg_count(&cell_pixels(frame, cw, ch, 0, col));
                assert!(
                    n > 4,
                    "{name}: {g:?} drew no ink in its cell {half} ({n} non-bg)"
                );
            }
        }
    }
}

/// Combining diacritics (é = e + U+0301, …) are overlaid as extra mono-glyph
/// instances on the base cell, pulled by the GPU from the same atlas. Sparse
/// marks keep their column order across wide gaps and changing frames.
/// A host-built duplicate must use the accessor's chosen entry only once;
/// entries beyond the viewport or the materialized row have no base to paint.
#[test]
fn sparse_combining_columns_and_duplicates_match_cpu() {
    let Some((mut cpu, mut gpu)) = backends(18.0, Theme::default()) else {
        return;
    };
    let (rows, cols) = (2usize, 96usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25le\u{0301}\x1b[1;49Hn\u{0303}\x1b[1;95Ha\u{030a}".as_bytes());
    let mut input = term.cell_frame(rows, cols);
    assert_eq!(
        input.combining[0]
            .iter()
            .map(|(c, _)| *c)
            .collect::<Vec<_>>(),
        [0, 48, 94]
    );
    input.combining[0].insert(2, (48, vec!['\u{0323}'].into_boxed_slice()));
    input.combining[0].push((cols, vec!['\u{0301}'].into_boxed_slice()));
    assert!(input.cells[1].get(24).is_none());
    input.combining[1].push((24, vec!['\u{0301}'].into_boxed_slice()));
    let mut bare = input.clone();
    for row in &mut bare.combining {
        row.clear();
    }
    let cpu_bare = cpu.render_input(&bare);
    let mut win = aterm_gpu::WindowGpu::new();
    for phase in ["sparse duplicates", "middle marks removed"] {
        let cpu_frame = cpu.render_input(&input);
        let gpu_frame = gpu.render_input(&mut win, &input, None);
        let delta = max_channel_delta(&cpu_frame, &gpu_frame);
        assert!(delta <= 8, "{phase}: sparse mark parity delta {delta} > 8");
        assert_ne!(
            cpu_frame.pixels, cpu_bare.pixels,
            "{phase}: no visible marks"
        );
        input.combining[0].retain(|(col, _)| *col != 48);
    }
}

/// Parity for the condensed symbol-tier raster: U+27F5..U+27FC fall to the
/// symbol fallback and are condensed at raster time to one cell. The GPU
/// inherits it through the shared cache and `glyph_quad`; this test keeps the
/// backends together. Spill is checked against the same row drawn with
/// spaces, since a blank cell is not literally `Theme::bg`.
#[test]
fn condensed_symbol_fallback_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    // One arrow every third column, so the two columns to its right — exactly
    // the ones the pre-fix raster buried — are known-blank cells.
    let dims = (1usize, 12usize);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cpu_arrows, gpu_arrows) = render_both(
        &mut cpu,
        &mut gpu,
        &mut win,
        dims,
        "\x1b[?25l\u{27F5}  \u{27F6}  \u{27F8}  \u{27F9}  ".as_bytes(),
    );
    let (cpu_blank, gpu_blank) =
        render_both(&mut cpu, &mut gpu, &mut win, dims, b"\x1b[?25l            ");

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

/// A combining mark on a space base: the CPU guards base and marks together
/// (`!wide && ch != ' ' && !ch.is_control()`), so it draws nothing there, and
/// the GPU must match. The base 'e' carries the same U+0301, so the mark has an
/// atlas slot and an unguarded GPU mark loop would paint it on the space.
#[test]
fn combining_mark_on_space_base_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

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
    // "e" + U+0301, then a space + U+0301: the mark does not advance, so it
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

    // The frame really carries a mark on a space base.
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

/// Stacked marks: a second mark on the same side is lifted (or dropped) clear
/// of the first by `aterm_render::MarkStack` as far as the cell allows, and the
/// GPU quad lands on the same rows. Assertions are about rows of ink. Two
/// rigs: default line height (tight, must stay in the band) and doubled
/// (separate bands). Text sits on row 1 so row 0 catches an unbounded climb.
/// Both painters place by the mark's ink box from the shared memo
/// (`Renderer::glyph_ink_box_cached`).
#[test]
fn stacked_combining_marks_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (2usize, 4usize);
    let mut win = aterm_gpu::WindowGpu::new();
    // Frame rows on which two frames differ anywhere: against the bare base,
    // the rows a cell's marks ink.
    let differing_rows = |a: &Frame, b: &Frame| -> Vec<usize> {
        (0..a.height)
            .filter(|&y| {
                a.pixels[y * a.width..(y + 1) * a.width] != b.pixels[y * b.width..(y + 1) * b.width]
            })
            .collect()
    };

    for (rig, line_height) in [("tight", 1.0f32), ("roomy", 2.0f32)] {
        cpu.set_line_height(line_height);
        gpu.set_line_height(line_height);
        let mut frame_for = |cpu: &mut Renderer, gpu: &mut aterm_gpu::GpuRenderer, text: &str| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(format!("\x1b[?25l\x1b[2;1H{text}").as_bytes());
            let input = term.cell_frame(rows, cols);
            let cpu_frame = cpu.render_input(&input);
            let gpu_frame = gpu.render_input(&mut win, &input, None);
            (input, cpu_frame, gpu_frame)
        };
        let (_, cpu_bare, gpu_bare) = frame_for(&mut cpu, &mut gpu, "e");
        let (_, cpu_one, gpu_one) = frame_for(&mut cpu, &mut gpu, "e\u{0301}");
        let (two, cpu_two, gpu_two) = frame_for(&mut cpu, &mut gpu, "e\u{0301}\u{0301}");
        let (_, cpu_pair, gpu_pair) = frame_for(&mut cpu, &mut gpu, "e\u{0302}\u{0301}");
        let (_, cpu_dot1, gpu_dot1) = frame_for(&mut cpu, &mut gpu, "e\u{0323}");
        let (_, cpu_dot2, gpu_dot2) = frame_for(&mut cpu, &mut gpu, "e\u{0323}\u{0323}");

        // NON-VACUITY: the frame lane hands both marks to the renderers.
        assert_eq!(
            two.combining_at(1, 0).map(<[char]>::len),
            Some(2),
            "{rig}: fixture drifted, the two-acute cell does not carry two marks"
        );

        // Parity on every fixture within the combining tolerance.
        for (name, c, g) in [
            ("bare", &cpu_bare, &gpu_bare),
            ("one acute", &cpu_one, &gpu_one),
            ("two acutes", &cpu_two, &gpu_two),
            ("circumflex + acute", &cpu_pair, &gpu_pair),
            ("one dot below", &cpu_dot1, &gpu_dot1),
            ("two dots below", &cpu_dot2, &gpu_dot2),
        ] {
            assert_eq!(
                (g.width, g.height),
                (c.width, c.height),
                "{rig} {name}: dims"
            );
            let delta = max_channel_delta(c, g);
            eprintln!("stacked marks [{rig} {name}] GPU vs CPU max per-channel delta = {delta}");
            assert!(
                delta <= 8,
                "{rig} {name}: GPU/CPU stacked-mark pixels diverge: max per-channel delta {delta} > 8"
            );
        }

        let (_, ch) = cpu.cell_size();
        let band_top = cpu.grid_top() + ch;
        let band_bottom = band_top + ch;
        // (a) Two acutes grow upward: ink starts higher and covers more rows
        // than one, and row 0 stays identical to the bare frame.
        for (name, one, two, bare) in [
            ("CPU", &cpu_one, &cpu_two, &cpu_bare),
            ("GPU", &gpu_one, &gpu_two, &gpu_bare),
        ] {
            let r1 = differing_rows(one, bare);
            let r2 = differing_rows(two, bare);
            eprintln!("{rig} {name}: one acute rows {r1:?}, two acutes rows {r2:?}");
            assert!(
                !r1.is_empty(),
                "{rig} {name}: non-vacuity, a single acute draws ink"
            );
            assert!(
                r2[0] < r1[0],
                "{rig} {name}: two acutes must start above one: {r2:?} vs {r1:?}"
            );
            assert!(
                r2.len() > r1.len(),
                "{rig} {name}: two acutes must occupy more rows than one: {r2:?} vs {r1:?}"
            );
            assert!(
                r2.iter().all(|&y| y >= band_top),
                "{rig} {name}: the stack climbed above the cell band (top {band_top}): {r2:?}"
            );
            if rig == "roomy" {
                // A whole lift: one ink-free row between the two acutes, and the
                // first acute's own rows untouched by the second.
                assert!(
                    r2.windows(2).any(|w| w[1] > w[0] + 1),
                    "{rig} {name}: the acutes must be separated by an ink-free row: {r2:?}"
                );
                for &y in &r1 {
                    assert_eq!(
                        one.pixels[y * one.width..(y + 1) * one.width],
                        two.pixels[y * two.width..(y + 1) * two.width],
                        "{rig} {name}: row {y}: the first acute moved when a second was added"
                    );
                }
            }
        }
        // (b) Circumflex + acute: the acute is lifted off the circumflex.
        for (name, pair, one, bare) in [
            ("CPU", &cpu_pair, &cpu_one, &cpu_bare),
            ("GPU", &gpu_pair, &gpu_one, &gpu_bare),
        ] {
            let r = differing_rows(pair, bare);
            let r1 = differing_rows(one, bare);
            eprintln!("{rig} {name}: circumflex + acute rows {r:?}");
            assert!(
                r.len() > r1.len() && r[0] < r1[0],
                "{rig} {name}: circumflex + acute must rise above one acute: {r:?} vs {r1:?}"
            );
            if rig == "roomy" {
                assert!(
                    r.windows(2).any(|w| w[1] > w[0] + 1),
                    "{rig} {name}: acute and circumflex must be separated by an ink-free row: {r:?}"
                );
            }
        }
        // (c) Two dots below: a second row under the first where the descent
        // has room (the tight rig has none); ink stays in the band.
        for (name, one, two, bare) in [
            ("CPU", &cpu_dot1, &cpu_dot2, &cpu_bare),
            ("GPU", &gpu_dot1, &gpu_dot2, &gpu_bare),
        ] {
            let r1 = differing_rows(one, bare);
            let r2 = differing_rows(two, bare);
            eprintln!("{rig} {name}: one dot below rows {r1:?}, two dots below rows {r2:?}");
            assert!(
                !r1.is_empty(),
                "{rig} {name}: non-vacuity, a single dot below draws ink"
            );
            assert!(
                r2.iter().all(|&y| y < band_bottom),
                "{rig} {name}: the stack dropped below the cell band (bottom {band_bottom}): {r2:?}"
            );
            // A below stack never moves its first mark and never loses ink.
            assert!(
                r2[0] == r1[0] && r2.last() >= r1.last(),
                "{rig} {name}: the first dot moved or the pair lost rows: {r2:?} vs {r1:?}"
            );
            if rig == "roomy" {
                assert!(
                    r2.last() > r1.last() && r2.len() > r1.len(),
                    "{rig} {name}: two dots below must end below one: {r2:?} vs {r1:?}"
                );
            }
        }
    }
}

/// A renderer reused across changing dimensions produces frames identical to a
/// fresh renderer: grow, shrink, grow and a repeated size, each compared pixel
/// for pixel, so stale resident resources are caught.
#[test]
fn reused_renderer_across_dims_matches_fresh_render() {
    let theme = Theme::default();
    let px = 16.0;
    let Some(mut reused) = common::gpu_or_skip(px, theme) else {
        return;
    };
    // Deterministic pixels: block on the lazy fallback parses so the reused
    // renderer and every fresh oracle rasterize the CJK/emoji identically.
    reused.debug_block_on_lazy_fallbacks();
    let mut win_reused = aterm_gpu::WindowGpu::new();

    // Glyph-rich content (mono, CJK, emoji, combining, decorations), so every
    // pass runs.
    let content: &[u8] = "\u{1b}[31mRR\u{1b}[0m \u{1b}[44mbg\u{1b}[0m \u{65e5}\u{672c} \
\u{2764}\u{fe0f} e\u{0301} \u{1b}[4;9mUu\u{1b}[0m"
        .as_bytes();

    // Grow, shrink, grow, and repeat an earlier dimension.
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

/// Cursor trail parity: trail quads are opaque, pre-blended with
/// `aterm_render::blend_rgb`, and the comet visibly tints green, brightest at
/// the head.
#[test]
fn cursor_trail_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    assert!(
        input.cells[5].get(6).is_none(),
        "regression requires a trail cell in an unmaterialized sparse tail"
    );
    // Differ from both `theme.bg` and the frame scalar: trail cells must blend
    // over their pane's live background, checked against an exact oracle.
    input.default_bg = 0x0012_3456;
    let pane_default_bg = 0x0024_4668;
    input.default_bg_spans.resize_with(rows, Vec::new);
    input.default_bg_spans[5].push(aterm_render::DefaultBgSpan::new(0, cols, pane_default_bg));
    // A comet across the empty bottom row (row 5): faint tail → bright head.
    input.cursor_trail = vec![
        aterm_render::TrailCell {
            row: 5,
            col: 4,
            alpha: 60,
        },
        aterm_render::TrailCell {
            row: 5,
            col: 5,
            alpha: 130,
        },
        aterm_render::TrailCell {
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

    // The trail is opaque, so swept cells are byte-exact.
    for (r, c) in [(5, 4), (5, 5), (5, 6)] {
        assert_eq!(
            cell_pixels(&cpu_frame, cw, ch, r, c),
            cell_pixels(&gpu_frame, cw, ch, r, c),
            "trail cell ({r},{c}) must be byte-exact CPU==GPU"
        );
    }
}

/// Sparkle-word decorations (paw Over, sparkle Add) match the CPU: the same
/// `procedural::deco_coverage` mask sampled 1:1 and blended.
#[test]
fn word_decorations_gpu_match_cpu() {
    use aterm_render::{DecoBlend, DecoGlyph, WordDecoration};
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
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
        // The nova's Over darkening ring: a bright RingArc so the Over stamp
        // visibly moves pixels on the dark theme.
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
        // The light-bg eclipse veil: a full-cell Shade square Over the text row.
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

    // The veil changes pixels over the 'a' cell on both paths (compared with
    // the undecorated frame, since the tint is dark).
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

/// Decorations on a DEC double-width row and an additive sparkle over a
/// selected cell stay CPU==GPU (both stretch the sprite and freeze Add over
/// selection with the same predicate).
#[test]
fn word_decorations_wide_row_and_selection_gpu_match_cpu() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_render::{DecoBlend, DecoGlyph, WordDecoration};
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
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

/// Minimum-contrast floor parity: floored cells (bright white on near-white)
/// match, the floor really fired, and concealed SGR 8 text stays hidden.
#[test]
fn minimum_contrast_floored_cells_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Cursor hidden. Row 0: bright white on near-white (~1.2:1). Row 1: SGR 8
    // concealed text on the same bg.
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

    // Sparse cursor in a composed pane: the contrast operand is the pane's
    // default, not the frame scalar.
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

/// Background and cursor opacity parity at 0.5, including alpha (the CPU's
/// top-byte transmittance). Opacity 1.0 is identical to the defaults,
/// default-bg pixels carry alpha 128, SGR-coloured cells stay opaque, and the
/// translucent cursor blends instead of cutting out.
#[test]
fn background_and_cursor_opacity_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (2usize, 8usize);
    // Frame A: block cursor on a blank default cell, for the baseline and the
    // opacity-1.0 identity check (avoids opaque-cursor overflow on a glyph).
    let input_at = |cursor_seq: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        // Seed the engine default bg to the theme bg, so written default cells
        // count as default background.
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
    // The pane's OSC-11 provenance differs from the frame scalar; the opacity
    // classifier must consult the pane span.
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

    // On both frames: a blank default cell has alpha 128 with unchanged RGB,
    // the SGR-bg cell is opaque, and the translucent cursor never shows the raw
    // fill.
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

    // Additive glow over a translucent ground preserves the ground's
    // transmittance on both backends.
    let mut input_glow = input_at(b"\x1b[1;6H");
    input_glow.default_bg = 0x0022_3344;
    input_glow.default_bg_spans = vec![vec![aterm_render::DefaultBgSpan::new(0, cols, BG)]; rows];
    input_glow.glow_under = vec![aterm_render::GlowQuad {
        row: 1,
        x: (cw / 2) as u16,
        y: (ch + ch / 2) as u16,
        w: (cw * 3) as u16,
        h: 4,
        color: 0x0018_0c04,
        alpha: 0,
        color2: 0x0018_0c04,
        alpha2: 0,
    }];
    input_glow.glow_halo = vec![aterm_render::RainHalo {
        row: 1,
        x: (cw * 4) as u16,
        y: (ch + 2) as u16,
        w: (cw * 2) as u16,
        h: (ch / 2) as u16,
        color: 0x0010_1418,
        cx: (cw * 5) as u16,
        cy: (ch + ch / 4) as u16,
        rx: cw as u16,
        ry: (ch / 3) as u16,
        ..Default::default()
    }];
    let cpu_glow = cpu.render_input(&input_glow);
    let gpu_glow = gpu.render_input(&mut win, &input_glow, None);
    let mut gdelta = max_channel_delta(&cpu_glow, &gpu_glow);
    for (&pa, &pb) in cpu_glow.pixels.iter().zip(gpu_glow.pixels.iter()) {
        gdelta = gdelta.max(((pa >> 24) as i32 - (pb >> 24) as i32).abs());
    }
    assert!(
        gdelta <= 8,
        "glow-over-translucent CPU/GPU pixels diverge (alpha included): {gdelta} > 8"
    );
    let (qx, qy) = (cw / 2 + 2, ch + ch / 2 + 1);
    let p = cpu_glow.pixels[qy * cpu_glow.width + qx];
    assert!(
        ((p >> 24) as i32 - 127).abs() <= 1,
        "CPU: a glow-lit default-bg pixel keeps the ground's transmittance, got {:#010x}",
        p
    );
}

/// Animated ink parity: both backends substitute the same bytes at their colour
/// points. (1) Within each backend, ink equals the same SGR truecolour
/// recolour (glyph, combining, wide lead, underline, strike); (2) inked frames
/// match within tolerance; (3) empty ink changes nothing. An SGR 58 underline
/// colour still wins over ink.
#[test]
fn ink_gpu_matches_cpu() {
    use aterm_core::render::InkCell;
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (3usize, 16usize);
    let ink: [u8; 3] = [0x7C, 0xC8, 0xFF];

    // Row 0: combining é, underlined x, struck s, curly-underlined w; row 1:
    // wide 猫; row 2: an SGR 58 underlined space (ink must not reach it). The
    // curly cell exercises the separate undercurl pass.
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

/// Ink under selection and on a DEC double-width row stays CPU==GPU: ink first,
/// then the selection fg floor, and the inked glyph stretched over the doubled
/// advance.
#[test]
fn ink_selection_and_decdwl_gpu_match_cpu() {
    use aterm_core::render::InkCell;
    use aterm_core::selection::{SelectionSide, SelectionType};
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
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

/// Two panes selected on the same rows, each with its own OSC 17/19 colours and
/// focus. Both backends resolve bands through `RenderInput::selection_hit` and
/// `Renderer::selection_palette`, and must match.
#[test]
fn per_pane_selection_colours_gpu_match_cpu() {
    use aterm_core::render::{PaneSelection, SelectionClip};
    use aterm_core::selection::{SelectionSide, SelectionType, TextSelection};

    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
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

    // The two bands differ on both backends: focused pane wears its OSC 17,
    // unfocused the inactive band.
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

/// A scrollback-seeded grid with distinct per-row colour and text, scrolled
/// into history (mirrors aterm-render's `scroll_frac_translate` seed).
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

/// Sub-row scroll parity at an asymmetric grid origin with chrome rows: CPU and
/// GPU translated frames match within tolerance, chrome rows are identical to
/// the frac-0 frame on both backends, and grid pixels moved.
#[test]
fn sub_row_scroll_translate_gpu_matches_cpu_at_asymmetric_origin() {
    let theme = Theme::default();
    let px = 18.0;
    let (rows, cols) = (8usize, 20usize);

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
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
    // Positive fracs shift the band up (glide), negative down (bounce).
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

/// Incoming-row apron parity: on a positive-frac frame, the exposed bottom
/// strip is the incoming row's top `frac` px. (1) GPU strip byte-exact with the
/// CPU strip; (2) each strip matches its backend's whole-row oracle at offset
/// `d - 1`; (3) the strip differs from the placeholder; (4) chrome invariance
/// holds.
#[test]
fn incoming_row_apron_gpu_matches_cpu() {
    let theme = Theme::default();
    let (rows, cols) = (8usize, 20usize);
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let (_cw, ch) = cpu.cell_size();
    let grid_top = cpu.grid_top();
    let frac = (ch / 2).max(1) as i32;
    let n = frac as usize;
    let partitioned = |t: &mut Terminal, frac: i32| {
        let mut input = t.cell_frame(rows, cols);
        input.grid_top_row = 1;
        input.grid_bot_row = rows;
        input.scroll_frac_px = frac;
        input
    };
    let mut win = aterm_gpu::WindowGpu::new();

    // The glide frame at the fixture's offset (`scroll_seeded_term` scrolls 5).
    let mut tg = scroll_seeded_term(rows, cols);
    let glide_in = partitioned(&mut tg, frac);
    assert!(
        glide_in.apron_row.present,
        "scrolled back: the apron is present"
    );
    let cpu_glide = cpu.render_input(&glide_in);
    let gpu_glide = gpu.render_input(&mut win, &glide_in, None);
    // The placeholder's source: the same offset, whole-row.
    let flat_in = partitioned(&mut tg, 0);
    let cpu_flat = cpu.render_input(&flat_in);
    let gpu_flat = gpu.render_input(&mut win, &flat_in, None);
    // The oracle: one offset shallower, whole-row — its bottom grid row IS the apron.
    let mut to = scroll_seeded_term(rows, cols);
    to.scroll_display(-1);
    let oracle_in = partitioned(&mut to, 0);
    assert_eq!(oracle_in.display_offset + 1, glide_in.display_offset);
    let cpu_oracle = cpu.render_input(&oracle_in);
    let gpu_oracle = gpu.render_input(&mut win, &oracle_in, None);

    let w = cpu_glide.width;
    assert_eq!((gpu_glide.width, gpu_glide.height), (w, cpu_glide.height));
    let y1 = grid_top + rows * ch; // grid_bot_row == rows
    let band = |f: &Frame, y0: usize, k: usize| f.pixels[y0 * w..(y0 + k) * w].to_vec();
    let cpu_strip = band(&cpu_glide, y1 - n, n);
    let gpu_strip = band(&gpu_glide, y1 - n, n);
    // (1) Byte-exact across backends: the strip is the CPU's raster on both.
    assert_eq!(
        gpu_strip, cpu_strip,
        "the apron strip is byte-identical CPU==GPU"
    );
    // (2) Each backend's strip is its own oracle's bottom-row top `n` px.
    assert_eq!(
        cpu_strip,
        band(&cpu_oracle, y1 - ch, n),
        "CPU strip == the incoming row's top {n} px"
    );
    let gpu_or = band(&gpu_oracle, y1 - ch, n);
    let delta = gpu_strip
        .iter()
        .zip(&gpu_or)
        .map(|(&a, &b)| {
            (rr(a) - rr(b))
                .abs()
                .max((gg(a) - gg(b)).abs())
                .max((bb(a) - bb(b)).abs())
        })
        .max()
        .unwrap_or(0);
    assert!(
        delta <= 8,
        "GPU strip vs the GPU's own landed row: max channel delta {delta} > 8"
    );
    // (3) The strip is not the placeholder (the band's own bottom px).
    assert_ne!(
        cpu_strip,
        band(&cpu_flat, y1 - n, n),
        "CPU: placeholder gone"
    );
    assert_ne!(
        gpu_strip,
        band(&gpu_flat, y1 - n, n),
        "GPU: placeholder gone"
    );
    assert!(
        cpu_strip.iter().any(|&p| dist(p, BG) > 0),
        "non-vacuity: the strip carries ink"
    );
    // (4) Chrome invariance with the strip painted: rows outside the band equal
    //     the frac-0 frame on BOTH backends.
    let y0 = grid_top + ch; // grid_top_row == 1
    for y in (0..y0).chain(y1..cpu_glide.height) {
        assert_eq!(
            &cpu_glide.pixels[y * w..y * w + w],
            &cpu_flat.pixels[y * w..y * w + w],
            "CPU chrome row {y} pinned"
        );
        assert_eq!(
            &gpu_glide.pixels[y * w..y * w + w],
            &gpu_flat.pixels[y * w..y * w + w],
            "GPU chrome row {y} pinned"
        );
    }
}

/// One cell's TOP pixel row — where `overline_rect` anchors its band.
fn top_row(f: &Frame, cw: usize, col: usize) -> Vec<u32> {
    let x0 = col * cw;
    f.pixels[x0..(x0 + cw).min(f.width)].to_vec()
}

/// How deep `ink` runs down from a cell's top edge, measured from the frame
/// since overline thickness varies by face and size.
fn band_depth(f: &Frame, cw: usize, ch: usize, col: usize, ink: u32) -> usize {
    let x0 = col * cw;
    let x1 = (x0 + cw).min(f.width);
    (0..ch.min(f.height))
        .take_while(|&y| {
            f.pixels[y * f.width + x0..y * f.width + x1]
                .iter()
                .all(|px| *px == ink)
        })
        .count()
}

/// The overline's own colour channel on both backends. `overline_color` has no
/// escape sequence, so this differential is its only oracle. Both resolve it
/// through `aterm_render::deco_inks`; a seam must hold one tone across a row
/// whose cells have different foregrounds.
#[test]
fn the_overline_colour_channel_paints_the_same_seam_on_both_backends() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (1usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Two runs of overlined text in two very different inks; the cursor is
    // hidden so no blink phase can move a pixel between the renders compared.
    term.process(b"\x1b[?25l\x1b[53m\x1b[38;2;80;80;80mdim\x1b[38;2;255;255;255mLIT");

    // The un-channelled frame is the negative control on BOTH sides: with no
    // channel a rule follows each cell's own fg, so the two runs' bands differ.
    let plain = term.cell_frame(rows, cols);
    let cpu_plain = cpu.render_input(&plain);
    let gpu_plain = gpu.render_input(&mut win, &plain, None);
    for (label, f) in [("cpu", &cpu_plain), ("gpu", &gpu_plain)] {
        assert_ne!(
            top_row(f, cw, 0),
            top_row(f, cw, 3),
            "{label}: without the channel a rule takes as many tones as the row has inks"
        );
    }

    let seam: [u8; 3] = [0x40, 0x44, 0x4A];
    let want = aterm_render::rgb_to_u32(seam);
    let mut input = term.cell_frame(rows, cols);
    for row in &mut input.cells {
        for cell in row.iter_mut() {
            cell.overline_color = Some(seam);
        }
    }
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "overline colour: dimensions differ"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("overline-colour GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "overline-colour GPU/CPU pixels diverge: max per-channel delta {delta} > 8"
    );

    for (label, f, plain_f) in [
        ("cpu", &cpu_frame, &cpu_plain),
        ("gpu", &gpu_frame, &gpu_plain),
    ] {
        // Non-vacuity per backend: the channel moved THAT backend's pixels,
        // rather than the two agreeing by both ignoring it.
        assert_ne!(
            f.pixels, plain_f.pixels,
            "{label}: an overline colour that changes nothing is not a channel"
        );
        let t = band_depth(f, cw, ch, 0, want);
        assert!(
            (1..ch).contains(&t),
            "{label}: the seam must be a band inside the cell, not {t} of {ch} rows"
        );
        for col in 0..6 {
            let band = top_row(f, cw, col);
            assert!(
                band.iter().all(|px| *px == want),
                "{label}: column {col} breaks the seam's one tone: {band:?}"
            );
            assert_eq!(
                band_depth(f, cw, ch, col, want),
                t,
                "{label}: column {col} wears the seam to a different depth"
            );
        }
        // The channel colours ONE rule, not the text under it: everything below
        // the band is what it was before any seam colour was set.
        assert_eq!(
            &f.pixels[f.width * t..],
            &plain_f.pixels[plain_f.width * t..],
            "{label}: the channel must not reach any row but the overline's own"
        );
    }
}

/// Chrome bleed with per-row tones: two chrome rows over a padded frame, the
/// second metered (`row_edges`), with the seam. Every padding pixel matches
/// exactly across backends; the grid stays within tolerance.
#[test]
fn gpu_matches_cpu_with_a_metered_chrome_bleed() {
    let theme = Theme::default();
    const P: usize = 10;
    const BAND: u32 = 0x0030_3135;
    const SEAM: u32 = 0x0060_6164;
    const FILL: u32 = 0x0050_FA7B;
    const TRACK: u32 = 0x0044_4750;
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    gpu.set_pad(P);
    cpu.set_pad(P);
    let bleed = aterm_render::ChromeBleed {
        rows: 2,
        first: 0,
        color: BAND,
        seam: Some(SEAM),
        top_extends_cells: false,
        row_edges: [
            Some(aterm_render::ChromeRowEdges {
                row: 1,
                left: FILL,
                right: TRACK,
            }),
            None,
            None,
        ],
    };
    cpu.set_chrome_bleed(Some(bleed));
    gpu.set_chrome_bleed(Some(bleed));
    let mut win = aterm_gpu::WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let (w, h) = (cpu_frame.width, cpu_frame.height);
    assert_eq!((gpu_frame.width, gpu_frame.height), (w, h));
    let grid =
        |x: usize, y: usize| (P..P + cols * cw).contains(&x) && (P..P + rows * ch).contains(&y);
    let (mut padding, mut tones) = (0usize, [false; 4]);
    for y in 0..h {
        for x in 0..w {
            if grid(x, y) {
                continue;
            }
            let (c, g) = (
                cpu_frame.pixels[y * w + x] & 0x00ff_ffff,
                gpu_frame.pixels[y * w + x] & 0x00ff_ffff,
            );
            assert_eq!(
                c, g,
                "padding pixel ({x},{y}) differs: cpu {c:06x} gpu {g:06x}"
            );
            for (i, tone) in [BAND, SEAM, FILL, TRACK].into_iter().enumerate() {
                tones[i] |= c == tone;
            }
            padding += 1;
        }
    }
    assert!(padding > 0);
    assert_eq!(
        tones, [true; 4],
        "the band, seam, fill and track all painted"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "grid pixels diverge: max per-channel delta {delta} > 8"
    );
}

/// A SEAMED CHROME BLEED WHOSE LAST ROW CARRIES AN EDGE-TO-EDGE RAIL, ON BOTH
/// BACKENDS: the rail covers the gutters (`chrome_rail_covers_gutters`), so
/// neither backend draws the bleed's own seam segment there (a second line
/// two pixels above the rail). Every padding pixel is exactly equal across the
/// arms, the rail's colour is in the gutters' lowest pixel row, and the pixel
/// row above it is the band's ground - no seam colour in the gutters at all.
#[test]
fn gpu_matches_cpu_with_a_seamed_bleed_under_an_edge_to_edge_rail() {
    let theme = Theme::default();
    const P: usize = 10;
    const BAND: u32 = 0x0030_3135;
    const SEAM: u32 = 0x0060_6164;
    const RAIL: u32 = 0x00F1_FA8C;
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    gpu.set_pad(P);
    cpu.set_pad(P);
    let bleed = aterm_render::ChromeBleed {
        rows: 1,
        first: 0,
        color: BAND,
        seam: Some(SEAM),
        top_extends_cells: false,
        row_edges: [None, None, None],
    };
    cpu.set_chrome_bleed(Some(bleed));
    gpu.set_chrome_bleed(Some(bleed));
    let mut win = aterm_gpu::WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    let w = cols * cw + 2 * P;
    let ground: std::sync::Arc<[u32]> = (0..w).map(|_| BAND).collect();
    let rail: std::sync::Arc<[u32]> = (0..w).map(|_| RAIL).collect();
    input.chrome_rasters.push(aterm_render::ChromeRaster {
        row: 0,
        ground,
        rail,
        rail_h: 1,
        clear_rail: true,
        own: Vec::new(),
        split: None,
        rings: Vec::new(),
        icons: Vec::new(),
    });
    assert!(aterm_render::chrome_rail_covers_gutters(
        input.chrome_raster(0)
    ));
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let (w, h) = (cpu_frame.width, cpu_frame.height);
    assert_eq!((gpu_frame.width, gpu_frame.height), (w, h));
    let top = cpu.grid_top();
    let grid =
        |x: usize, y: usize| (P..P + cols * cw).contains(&x) && (P..P + rows * ch).contains(&y);
    let mut seam_seen = false;
    for y in 0..h {
        for x in 0..w {
            if grid(x, y) {
                continue;
            }
            let (c, g) = (
                cpu_frame.pixels[y * w + x] & 0x00ff_ffff,
                gpu_frame.pixels[y * w + x] & 0x00ff_ffff,
            );
            assert_eq!(
                c, g,
                "padding pixel ({x},{y}) differs: cpu {c:06x} gpu {g:06x}"
            );
            seam_seen |= c == SEAM;
        }
    }
    assert!(!seam_seen, "no gutter segment of the bleed's own seam");
    for x in [0, P - 1, w - P, w - 1] {
        let at = |y: usize| gpu_frame.pixels[y * w + x] & 0x00ff_ffff;
        assert_eq!(at(top + ch - 1), RAIL, "gutter x={x}: the rail");
        assert_eq!(at(top + ch - 2), BAND, "gutter x={x}: no second line");
    }
}

/// THE PIXEL-RESOLUTION CHROME ROW ON BOTH BACKENDS (design ruling 242):
/// one chrome row carrying a `ChromeRaster` — a per-pixel ground over its
/// band and gutters, a rail in its lowest pixels, a cell left to its own
/// background, and an ink split through one glyph — paints the SAME ground
/// and rail pixels on the GPU as on the CPU (both place the runs of the one
/// shared builder, `aterm_render::visit_chrome_raster_runs`), and its glyphs stay
/// within the usual glyph tolerance, the split one included.
#[test]
fn gpu_matches_cpu_with_a_chrome_raster() {
    let theme = Theme::default();
    const P: usize = 10;
    const FILL: u32 = 0x0050_FA7B;
    const TRACK: u32 = 0x0044_4750;
    const RAIL: u32 = 0x00F1_FA8C;
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    gpu.set_pad(P);
    cpu.set_pad(P);
    let mut win = aterm_gpu::WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    let w = cols * cw + 2 * P;
    let edge = P + 3 * cw + cw / 3;
    // A smooth gradient to the edge, one AA pixel, the track after.
    let ground: std::sync::Arc<[u32]> = (0..w)
        .map(|x| {
            if x < edge {
                let g = 0x80 + u32::try_from(x * 0x7f / edge).unwrap();
                (0x50 << 16) | (g << 8) | 0x7b
            } else if x == edge {
                0x004A_A066
            } else {
                TRACK
            }
        })
        .collect();
    let rail: std::sync::Arc<[u32]> = (0..w)
        .map(|x| {
            if x < edge {
                RAIL
            } else {
                aterm_render::ChromeRaster::KEEP
            }
        })
        .collect();
    input.chrome_rasters.push(aterm_render::ChromeRaster {
        row: 0,
        ground,
        rail,
        rail_h: 3,
        clear_rail: false,
        own: vec![(6, 8)],
        split: Some(aterm_render::InkSplit {
            col: 3,
            x: u32::try_from(edge).unwrap(),
            ink: 0,
            bg: FILL,
        }),
        rings: Vec::new(),
        icons: Vec::new(),
    });
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let h = cpu_frame.height;
    assert_eq!((gpu_frame.width, gpu_frame.height), (w, h));
    let top = cpu.grid_top();
    // The band's top pixel row and its rail rows carry no glyph ink: there the
    // raster's pixels must be byte-identical across the backends.
    let mut seen = [false; 3];
    for y in [top, top + ch - 3, top + ch - 2, top + ch - 1] {
        for x in 0..w {
            let (c, g) = (
                cpu_frame.pixels[y * w + x] & 0x00ff_ffff,
                gpu_frame.pixels[y * w + x] & 0x00ff_ffff,
            );
            assert_eq!(
                c, g,
                "raster pixel ({x},{y}) differs: cpu {c:06x} gpu {g:06x}"
            );
            for (i, tone) in [TRACK, RAIL, 0x004A_A066].into_iter().enumerate() {
                seen[i] |= c == tone;
            }
        }
    }
    assert_eq!(
        seen, [true; 3],
        "the track, the rail and the AA pixel painted"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "glyph pixels diverge: max per-channel delta {delta} > 8"
    );
}

/// A chrome row that lifts its words clear of its rail
/// (`aterm_render::chrome_fit` / `chrome_lifted`): rail rows and the clear row
/// above match exactly and carry only the rail; lifted glyphs within
/// tolerance.
#[test]
fn gpu_matches_cpu_with_a_clear_rail_row() {
    let theme = Theme::default();
    const P: usize = 10;
    const RAIL: u32 = 0x00F2_FD4A;
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    gpu.set_pad(P);
    cpu.set_pad(P);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (4usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"gypq (jgy)'\r\nabc");
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    for cell in &mut input.cells[0] {
        cell.fg = [0xd0, 0xd0, 0xd0];
        cell.bg = [0x30, 0x31, 0x35];
    }
    let w = cols * cw + 2 * P;
    let lit = w * 3 / 5;
    input.chrome_rasters.push(aterm_render::ChromeRaster {
        row: 0,
        ground: std::sync::Arc::from(Vec::new()),
        rail: (0..w)
            .map(|x| {
                if x < lit {
                    RAIL
                } else {
                    aterm_render::ChromeRaster::KEEP
                }
            })
            .collect(),
        rail_h: 3,
        clear_rail: true,
        own: Vec::new(),
        split: None,
        rings: Vec::new(),
        icons: Vec::new(),
    });
    let (lift, rail) =
        aterm_render::chrome_fit(&input.chrome_rasters[0], cpu.chrome_room_for(&input));
    assert!(lift > 0 && rail > 0, "the face leaves room to lift into");
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!((gpu_frame.width, gpu_frame.height), (w, cpu_frame.height));
    let top = cpu.grid_top();
    for y in top + ch - rail - 1..top + ch {
        for x in 0..w {
            let (c, g) = (
                cpu_frame.pixels[y * w + x] & 0x00ff_ffff,
                gpu_frame.pixels[y * w + x] & 0x00ff_ffff,
            );
            assert_eq!(c, g, "clear-rail row ({x},{y}): cpu {c:06x} gpu {g:06x}");
            if y >= top + ch - rail && x < lit {
                assert_eq!(c, RAIL, "({x},{y}): the rail, with no glyph ink on it");
            }
        }
    }
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "glyph pixels diverge: max per-channel delta {delta} > 8"
    );
}

/// The band's drawn icons (every icon, regular and bold, spilling into blank
/// neighbours, and side by side) and an outlined capsule over a gradient. Icons
/// come through `Renderer::chrome_icon_key` and match within tolerance; the
/// ring from `aterm_render::chrome_ring_runs` matches exactly.
#[test]
fn gpu_matches_cpu_with_band_icons_and_an_outlined_capsule() {
    let theme = Theme::default();
    const P: usize = 10;
    const RING: u32 = 0x00BD_93F9;
    const INNER: u32 = 0x0030_3135;
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    gpu.set_pad(P);
    cpu.set_pad(P);
    let mut win = aterm_gpu::WindowGpu::new();
    let icons = aterm_render::BandIcon::ALL;
    // ` G ` per icon (the band's layout): each icon at column 3k + 1.
    let spaced = 3 * icons.len();
    let cols = spaced + 10;
    let rows = 4usize;
    let mut term = Terminal::new(rows as u16, cols as u16);
    let line: String = icons.iter().flat_map(|i| [' ', i.ch(), ' ']).collect();
    let tight: String = icons.iter().map(|i| i.ch()).collect();
    term.process(line.as_bytes());
    term.process(b"\r\n");
    term.process(line.as_bytes());
    term.process(b"\r\n");
    term.process(tight.as_bytes());
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    for (r, row) in input.cells.iter_mut().take(3).enumerate() {
        for cell in row.iter_mut() {
            cell.fg = [0xf1, 0xfa, 0x8c];
            cell.bg = [0x30, 0x31, 0x35];
            cell.bold = r == 1;
        }
    }
    let w = cols * cw + 2 * P;
    let ground: std::sync::Arc<[u32]> = (0..w)
        .map(|x| {
            let g = 0x60 + u32::try_from(x * 0x9f / w).unwrap();
            (0x50 << 16) | (g << 8) | 0x7b
        })
        .collect();
    let with_icons = |row: u16, ring: bool| aterm_render::ChromeRaster {
        row,
        ground: if ring {
            std::sync::Arc::clone(&ground)
        } else {
            std::sync::Arc::from(Vec::new())
        },
        rail: std::sync::Arc::from(Vec::new()),
        rail_h: 0,
        clear_rail: false,
        own: Vec::new(),
        split: None,
        rings: if ring {
            vec![aterm_render::ChromeRing {
                start: u16::try_from(spaced + 1).unwrap(),
                end: u16::try_from(spaced + 8).unwrap(),
                ring: RING,
                inner: INNER,
                // As on a band's last row: the ring draws the seam across
                // its end cells (ruling 254).
                seam: Some(0x00A0_A2A8),
            }]
        } else {
            Vec::new()
        },
        icons: icons
            .iter()
            .enumerate()
            .map(|(c, &icon)| aterm_render::ChromeIcon {
                col: u16::try_from(if row == 2 { c } else { 3 * c + 1 }).unwrap(),
                icon,
            })
            .collect(),
    };
    input.chrome_rasters.push(with_icons(0, true));
    input.chrome_rasters.push(with_icons(1, false));
    input.chrome_rasters.push(with_icons(2, false));
    // The capsule's floor is its cells' underline in the ring's colour
    // (ruling 254), as the band stamps it.
    let floor_cols = aterm_render::chrome_ring_floor_cols(&input.chrome_rasters[0].rings[0]);
    let blank = aterm_core::terminal::RenderCell {
        bg: [0x30, 0x31, 0x35],
        ..aterm_core::terminal::RenderCell::default()
    };
    input.cells[0].resize(cols, blank);
    for cell in &mut input.cells[0][floor_cols.clone()] {
        cell.underline = aterm_core::terminal::UnderlineStyle::Single;
        cell.underline_color = Some([0xBD, 0x93, 0xF9]);
    }
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!((gpu_frame.width, gpu_frame.height), (w, cpu_frame.height));
    for (c, &icon) in icons.iter().enumerate() {
        assert_eq!(
            input.cells[0][3 * c + 1].ch,
            icon.ch(),
            "the cell keeps its character"
        );
        assert_eq!(
            input.cells[2][c].ch,
            icon.ch(),
            "the cell keeps its character"
        );
        // Spilled between blanks, kept to its cell beside another icon.
        let key = |r: usize, c: usize| {
            cpu.chrome_icon_key(input.chrome_raster(r), &input.cells[r], c)
                .expect("an icon cell")
        };
        assert_ne!(
            key(0, 3 * c + 1).ch_or_id & aterm_render::CHROME_ICON_SPILL_BIT,
            0
        );
        if c > 0 && c + 1 < icons.len() {
            assert_eq!(key(2, c).ch_or_id & aterm_render::CHROME_ICON_SPILL_BIT, 0);
        }
    }
    let top = cpu.grid_top();
    // The ring's pixels, from the one builder, are the same on both.
    let floor = cpu.chrome_ring_floor();
    let runs = aterm_render::chrome_ring_runs(&input.chrome_rasters[0], w, P, cw, ch, floor);
    assert!(!runs.is_empty(), "the ring is drawn");
    let (lx0, lx1) = (P + floor_cols.start * cw, P + floor_cols.end * cw);
    let mut ring_px = 0usize;
    for (x0, x1, ya, yb, color) in runs {
        for y in ya..yb {
            for x in x0..x1 {
                // The floor's underline, on both, over the builder's inside.
                let color = if (floor.0..floor.1).contains(&y) && (lx0..lx1).contains(&x) {
                    RING
                } else {
                    color
                };
                let (c, g) = (
                    cpu_frame.pixels[(top + y) * w + x] & 0x00ff_ffff,
                    gpu_frame.pixels[(top + y) * w + x] & 0x00ff_ffff,
                );
                assert_eq!(c, color, "cpu ring ({x},{y})");
                assert_eq!(g, color, "gpu ring ({x},{y})");
                ring_px += usize::from(color == RING);
            }
        }
    }
    assert!(ring_px > 0, "the ring's own colour is painted");
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "icon pixels diverge: max per-channel delta {delta} > 8"
    );
}
