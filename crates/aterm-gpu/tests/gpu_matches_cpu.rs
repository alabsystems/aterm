// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Correctness gate for the GPU renderer: render the same terminal on the CPU
// (`aterm_render::Renderer`, whose own tests prove what it draws) and the GPU
// (`aterm_gpu::GpuRenderer`) at the same px/theme and check that the frames
// have identical dimensions and a per-channel delta within a small tolerance
// (only rounding differs), exactly where both paint hard coverage. Random
// mixed content is `parity_fuzz.rs`; these pin specific features.
//
// A host without a GPU could not run it (`common::gpu`).

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

/// A composed split row carries one DEC line-size span per pane, resolved per
/// cell: a double-width pane beside a single-width one (and the inverse on the
/// next row), with the clipped-away logical half of each double-width pane
/// never reappearing in the neighbour.
#[test]
fn gpu_matches_cpu_with_pane_local_mixed_dec_widths() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);

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
    let (mut cpu, mut gpu) = backends(px, theme);

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
    let (mut cpu, mut gpu) = backends(px, theme);

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
    let (mut cpu, mut gpu) = backends(px, theme);

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

    let (mut cpu, mut gpu) = backends(px_a, theme);

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
}

/// Escape-sequence fixtures render the same on both backends, at an odd
/// interior pad that flips the ░▒▓ dither's absolute-pixel phase in every
/// cell. Hard-coverage procedural cells (box drawing, blocks, shades, braille,
/// sextants, eighths, octants) match EXACTLY. Within the glyph tolerance:
/// anti-aliased procedural cells (arcs, diagonals, Powerline, wedges);
/// synthetic bold/italic, whose wide bold-italic W overflows into the block
/// cursor both backends paint last; emoji clusters (ZWJ family, skin tone,
/// keycap, flag, and an unligated ZWJ pair composited from two glyphs); a VS15
/// one-cell emoji beside an X; condensed symbol-tier arrows, on a primary
/// that maps none of them; every underline style with descender skip on and
/// off; combining marks stacked at a tight and a roomy line height; and a mark
/// on a space base, which the CPU drops, so the GPU mark loop must too.
#[test]
fn escape_fixtures_match_cpu() {
    const PAD: usize = 3;
    let theme = Theme::default();
    let underlines = "\x1b[?25l\x1b[4:3mgyqj\x1b[0m\r\n\x1b[4:3;58;2;255;80;80mwavy\x1b[0m\r\n\
                      \x1b[4:4mdotdotdo\x1b[0m\r\n\x1b[4:5mdashdash\x1b[0m";
    let stacked = "\x1b[?25l\x1b[2;1He\u{0301}\u{0301} e\u{0302}\u{0301} e\u{0323}\u{0323} e\u{0301} e\u{0323}";
    // (name, (rows, cols), max per-channel delta, line height, skip descenders, bytes)
    let fixtures = [
        (
            "orthogonal procedural cells",
            (5, 16),
            0,
            1.0,
            true,
            "\x1b[?25l\
\u{250C}\u{2500}\u{252C}\u{2500}\u{2510}\u{2554}\u{2550}\u{2566}\u{2550}\u{2557}\u{2501}\u{2513}\u{2517}\u{2503}\u{254B}\r\n\
\u{251C}\u{2500}\u{253C}\u{2500}\u{2524}\x1b[31m\u{2560}\u{2550}\u{256C}\u{2550}\u{2563}\x1b[0m\u{2580}\u{2584}\u{258C}\u{2590}\u{2588}\r\n\
\u{2514}\u{2500}\u{2534}\u{2500}\u{2518}\u{255A}\u{2550}\u{2569}\u{2550}\u{255D}\u{2591}\u{2592}\u{2593}\u{2847}\u{28FF}\r\n\
\u{2504}\u{2508}\u{254C}\u{2581}\u{2582}\u{258E}\u{1FB13}\u{1FB70}\u{1FB76}\u{1FB7C}\u{1FB80}\u{1FB81}\u{1FB82}\u{1FB87}\u{1FB8B}\r\n\
\u{1CD00}\u{1CD01}\u{1CD17}\u{1CD2E}\u{1CD45}\u{1CD5C}\u{1CD73}\u{1CD8A}\u{1CDA1}\u{1CDB8}\u{1CDCF}\u{1CDDD}\u{1CDE4}\u{1CDE5}",
        ),
        (
            "anti-aliased procedural cells",
            (2, 18),
            8,
            1.0,
            true,
            "\x1b[?25l\
\u{256D}\u{2500}\u{256E}\u{2570}\u{256F}\u{2571}\u{2572}\u{2573}\x1b[31m\u{E0B0}\u{E0B1}\u{E0B2}\u{E0B4}\u{E0B6}\u{E0B8}\u{E0BA}\u{E0BB}\u{E0BC}\u{E0BE}\x1b[0m\r\n\
\u{1FB3C}\u{1FB40}\u{1FB44}\u{1FB4B}\u{1FB52}\u{1FB56}\u{1FB5B}\u{1FB61}\u{1FB65}\u{1FB68}\u{1FB69}\u{1FB6A}\u{1FB6B}\u{1FB6C}\u{1FB6F}",
        ),
        (
            "synthetic bold/italic under the block cursor",
            (3, 8),
            8,
            1.0,
            true,
            "\x1b[1mBB\x1b[0m\r\n\x1b[3mII\x1b[0m\r\n\x1b[1;3mWW\x1b[0m",
        ),
        (
            "emoji clusters",
            (2, 16),
            8,
            1.0,
            true,
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{1F44D}\u{1F3FD} \u{31}\u{FE0F}\u{20E3} \
             \u{1F1FA}\u{1F1F8}\r\n\u{1F680}\u{200D}\u{1F355}",
        ),
        (
            "VS15 emoji beside an X",
            (1, 5),
            8,
            1.0,
            true,
            "\x1b[?25l\u{1F600}\u{FE0E}\x1b[2GX",
        ),
        (
            "underlines, descender skip on",
            (4, 10),
            8,
            1.0,
            true,
            underlines,
        ),
        (
            "underlines, descender skip off",
            (4, 10),
            8,
            1.0,
            false,
            underlines,
        ),
        (
            "stacked marks, tight line height",
            (2, 16),
            8,
            1.0,
            true,
            stacked,
        ),
        (
            "stacked marks, roomy line height",
            (2, 16),
            8,
            2.0,
            true,
            stacked,
        ),
        (
            "a mark on a space base",
            (1, 8),
            8,
            1.0,
            true,
            "\x1b[?25le\u{0301} \u{0301}b",
        ),
    ];
    // The condensed arrows' primary is the checked-in Cascadia Mono PL, which
    // maps none of them, so every one reaches the symbol tier.
    //
    // A PRIMARY face's arrows are not condensed, on purpose. The JetBrains Mono
    // fixture maps U+27F5..27F7 itself and draws them past its 600-unit advance
    // by design — U+27F5/27F6 span -40..640 and U+27F7 -80..680, as its U+2500
    // spans -20..620 — so a run of them, or one beside a box-drawing rule, joins
    // into one line; at the fixture's px that is U+27F5's 2 px past the cell on
    // the CPU path, and the GPU draws the same raster. Condensing would break
    // the joins the face was drawn for. A FALLBACK face is condensed because it
    // was never drawn for this grid's cell, and its spill is arbitrary rather
    // than designed.
    let symbol_primary = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../aterm-render/assets/bundled/CascadiaMonoPL-Regular.ttf"
    ));
    let symbol_fixtures = [(
        "condensed symbol-tier arrows",
        (1, 12),
        8,
        1.0,
        true,
        "\x1b[?25l\u{27F5}  \u{27F6}  \u{27F8}  \u{27F9}  ",
    )];
    for ((mut cpu, mut gpu), table) in [
        (backends(18.0, theme), &fixtures[..]),
        (
            common::backends_with_font(symbol_primary, 18.0, theme),
            &symbol_fixtures[..],
        ),
    ] {
        cpu.set_pad(PAD);
        gpu.set_pad(PAD);
        let mut win = aterm_gpu::WindowGpu::new();
        for &(name, dims, tol, line_height, skip, text) in table {
            cpu.set_line_height(line_height);
            gpu.set_line_height(line_height);
            cpu.set_underline_skip_descenders(skip);
            gpu.set_underline_skip_descenders(skip);
            let (c, g) = render_both(&mut cpu, &mut gpu, &mut win, dims, text.as_bytes());
            assert_eq!(
                (g.width, g.height),
                (c.width, c.height),
                "{name}: dimensions differ"
            );
            assert!(
                g.pixels.iter().any(|&p| dist(p, BG) > 24),
                "{name}: the GPU frame is blank"
            );
            let delta = max_channel_delta(&c, &g);
            assert!(
                delta <= tol,
                "{name}: GPU/CPU pixels diverge: max per-channel delta {delta} > {tol}"
            );
        }
    }
}

/// Parity in the `linear` text-blending mode as well as the default: the demo
/// grid within tolerance and procedural cells exactly. Also checks the two
/// modes differ on antialiased text, so the mode flag is live.
#[test]
fn linear_mode_matches_cpu_and_keeps_procedural_exact() {
    let theme = Theme::default();
    let px = 18.0;

    let (mut cpu, mut gpu) = backends(px, theme);

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

/// A SUBSCRIPT cell (`Script::Sub`, chrome-only: the Claude footer's facts)
/// draws its glyph smaller and lower — no glyph's ink, a parenthesis taller
/// than the letters included, above `SUB_CEILING` — the rule glyph beside it
/// full size; a nudged one draws the same glyph moved sideways by exactly its
/// nudge, a fraction of a pixel included (its ink's centre, weighted by
/// coverage); and the GPU, which takes the CPU's raster and placement,
/// matches it.
#[test]
fn sub_script_gpu_matches_cpu() {
    use aterm_core::terminal::Script;
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (rows, cols) = (3usize, 4usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("H\u{2500}(H\r\nH\u{2500}(H\r\n H".as_bytes());
    let mut input = term.cell_frame(rows, cols);
    for cell in &mut input.cells[0] {
        cell.script = Script::Sub { nudge: 0 };
    }
    // Row 2: the same sub H one cell right, nudged half a cell back left.
    let half = Script::NUDGES_PER_CELL / 2;
    input.cells[2][1].script = Script::Sub { nudge: -half };
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
    // The coverage-weighted centre column of `row`'s ink within its first
    // `span` cells: where a glyph stands, to a fraction of a pixel.
    let ink_centre = |f: &Frame, row: usize, span: usize| {
        let ground = f.pixels[row * ch * f.width];
        let (mut sum, mut weight) = (0.0f64, 0.0f64);
        for x in 0..span * cw {
            for y in row * ch..row * ch + ch {
                let w = f64::from(dist(f.pixels[y * f.width + x], ground));
                sum += w * (x as f64 + 0.5);
                weight += w;
            }
        }
        assert!(weight > 0.0, "the row has ink");
        sum / weight
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
        let (nudged, home) = (ink_centre(f, 2, 2), ink_centre(f, 0, 1));
        assert!(
            (nudged - (home + cw as f64 / 2.0)).abs() <= 0.25,
            "the nudged sub H stands half a cell left of its cell: {nudged} vs {home} + {}",
            cw as f64 / 2.0
        );
    }
}

/// Last-resort fontless symbols (`font_chain::Tier::Synthetic`) draw the same
/// pixels on both backends: the narrow `⏵⏵` family, ⭘ and the procedural `⎿`,
/// and the wide emoji-presentation symbols (⏩ ⏪ ⏫ ⚪ ⚫) rasterized as one
/// `2 * cell_w` bitmap across both cells. Built from bundled DejaVu Sans Mono
/// with runtime discovery off, so the synthesis is the only answer; each glyph
/// is asserted `FaceId::Procedural`.
#[test]
fn synthetic_symbol_cells_match_cpu_within_tolerance() {
    let theme = Theme::default();
    let px = 18.0;
    let ctx = aterm_gpu::GpuContext::new()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU available: {e}")));
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

    let narrow = [
        '\u{23F5}', '\u{23F4}', '\u{23F6}', '\u{23F7}', '\u{2B58}', '\u{23BF}',
    ];
    let wide = ['\u{23E9}', '\u{23EA}', '\u{23EB}', '\u{26AA}', '\u{26AB}'];
    for g in narrow.into_iter().chain(wide) {
        let key = cpu.glyph_key(g);
        assert_eq!(
            key.source,
            aterm_render::FaceId::Procedural,
            "{g:?} must reach the synthesis in a fontless renderer"
        );
        assert_eq!(
            key.cell_span == 2,
            wide.contains(&g),
            "{g:?} cell span {}",
            key.cell_span
        );
    }
    // Cursor hidden; each glyph, then one blank separator column.
    let line = |glyphs: &[char]| glyphs.iter().map(|g| format!("{g} ")).collect::<String>();
    let mut win = aterm_gpu::WindowGpu::new();
    let (cpu_frame, gpu_frame) = render_both(
        &mut cpu,
        &mut gpu,
        &mut win,
        (2, wide.len() * 3),
        format!("\x1b[?25l{}\r\n{}", line(&narrow), line(&wide)).as_bytes(),
    );
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dims"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "synthetic symbol cells diverge: max per-channel delta {delta} > 8"
    );
}

/// Combining diacritics (é = e + U+0301, …) are overlaid as extra mono-glyph
/// instances on the base cell, pulled by the GPU from the same atlas. Sparse
/// marks keep their column order across wide gaps and changing frames.
/// A host-built duplicate must use the accessor's chosen entry only once;
/// entries beyond the viewport or the materialized row have no base to paint.
#[test]
fn sparse_combining_columns_and_duplicates_match_cpu() {
    let (mut cpu, mut gpu) = backends(18.0, Theme::default());
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

/// A renderer reused across changing dimensions produces frames identical to a
/// fresh renderer: grow, shrink, grow and a repeated size, each compared pixel
/// for pixel, so stale resident resources are caught.
#[test]
fn reused_renderer_across_dims_matches_fresh_render() {
    let theme = Theme::default();
    let px = 16.0;
    let mut reused = common::gpu(px, theme);
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
        let mut fresh = common::gpu(px, theme);
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
/// `aterm_render::blend_rgb` over the pane's default background, brightest at
/// the head.
#[test]
fn cursor_trail_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
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

    // The comet is drawn, head exactly the trail colour over the pane default,
    // and head-brighter-than-tail, on BOTH.
    for (label, f) in [("cpu", &cpu_frame), ("gpu", &gpu_frame)] {
        let head = cell_pixels(f, cw, ch, 5, 6);
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

/// Word decorations match the CPU: the paw and the nova's ring stamped Over,
/// sparkles Add (some offset off their cell), and the eclipse veil Over text,
/// including on a DEC double-width row (both backends stretch the sprite) and
/// an additive sparkle over a selected cell (both freeze it with the same
/// predicate).
#[test]
fn word_decorations_gpu_match_cpu() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_render::{DecoBlend, DecoGlyph, WordDecoration};
    let (mut cpu, mut gpu) = backends(18.0, Theme::default());
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (3usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Row 0 DEC double-width, row 1 selected over cols 0..=3, cursor hidden.
    term.process(b"\x1b[?25lcat\r\nfuck here\r\nmore\x1b[1;1H\x1b#6");
    {
        let sel = term.text_selection_mut();
        sel.start_selection(1, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(1, 3, SelectionSide::Right);
        sel.complete_selection();
    }
    let deco =
        |row: u16, col: u16, dx: i8, dy: i8, glyph, blend, color: u32, alpha: u8| WordDecoration {
            row,
            col,
            dx,
            dy,
            glyph,
            blend,
            color,
            alpha,
        };
    let (over, add) = (DecoBlend::Over, DecoBlend::Add);
    let mut input = term.cell_frame(rows, cols);
    input.word_decorations = vec![
        deco(0, 0, 0, 0, DecoGlyph::Paw, over, 0x00F7_A8B8, 200),
        deco(0, 1, 0, 0, DecoGlyph::Star4, add, 0x00FF_D447, 230),
        deco(1, 0, 0, 0, DecoGlyph::Star4, add, 0x00FF_D447, 230),
        deco(1, 7, -1, 1, DecoGlyph::Dot, add, 0x007C_F0FF, 180),
        deco(2, 6, 1, -1, DecoGlyph::Star4, add, 0x00FF_D447, 230),
        deco(2, 9, 0, 0, DecoGlyph::RingArc, over, 0x00B0_90FF, 210),
        deco(2, 1, 0, 0, DecoGlyph::Shade, over, 0x0018_0C2A, 200),
    ];
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height)
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "decoration CPU/GPU pixels diverge: max per-channel delta {delta} > 8"
    );
    let plain = term.cell_frame(rows, cols);
    assert_ne!(
        gpu_frame.pixels,
        gpu.render_input(&mut win, &plain, None).pixels,
        "non-vacuity: the GPU drew no decoration"
    );
}

/// Minimum-contrast floor parity: floored cells (bright white on near-white)
/// and concealed SGR 8 text match the CPU with the floor off and on, and a
/// sparse cursor in a composed pane is floored against the pane's default,
/// not the frame scalar.
#[test]
fn minimum_contrast_floored_cells_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
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

    let gpu_off = gpu.render_input(&mut win, &input, None);
    assert!(
        max_channel_delta(&cpu.render_input(&input), &gpu_off) <= 8,
        "baseline diverges"
    );
    cpu.set_minimum_contrast(4.5);
    gpu.set_minimum_contrast(4.5);
    let gpu_on = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu.render_input(&input), &gpu_on);
    assert!(
        delta <= 8,
        "min-contrast CPU/GPU pixels diverge: max per-channel delta {delta} > 8"
    );
    assert_ne!(
        gpu_on.pixels, gpu_off.pixels,
        "non-vacuity: the floor must repaint the low-contrast cells"
    );

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
    let mut sparse_win = aterm_gpu::WindowGpu::new();
    let gpu_sparse = gpu.render_input(&mut sparse_win, &sparse_input, None);
    assert!(
        cell_pixels(&gpu_sparse, cw, ch, 0, 7)
            .iter()
            .all(|&p| dist(p, expected_cursor) <= 3),
        "GPU: a sparse cursor must be floored against its pane default"
    );
}

/// Background and cursor opacity parity at 0.5, including alpha (the CPU's
/// top-byte transmittance). Opacity 1.0 is identical to the defaults,
/// default-bg pixels carry alpha 128, SGR-coloured cells stay opaque, and the
/// translucent cursor blends instead of cutting out.
#[test]
fn background_and_cursor_opacity_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
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

/// Animated ink parity: (1) on the GPU, ink equals the same SGR truecolour
/// recolour (glyph, combining, wide lead, underline, strike, curl); (2) inked
/// frames match the CPU within tolerance; (3) cleared ink changes nothing. An
/// SGR 58 underline colour still wins over ink.
#[test]
fn ink_gpu_matches_cpu() {
    use aterm_core::render::InkCell;
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
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

    // (1) Exact substitution on the GPU: ink == SGR recolor, byte-for-byte
    // (the CPU twin is aterm-render's `ink.rs`).
    let cpu_ink = cpu.render_input(&inked);
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

    // (3) Cleared ink renders the plain frame on the GPU.
    let plain = term_a.cell_frame(rows, cols);
    let gpu_plain = gpu.render_input(&mut win, &plain, None);
    let mut cleared = term_a.cell_frame(rows, cols);
    cleared.ink = inked.ink.clone();
    cleared.clear_overlays();
    assert_eq!(
        gpu.render_input(&mut win, &cleared, None).pixels,
        gpu_plain.pixels,
        "GPU: cleared ink must render the bare frame"
    );
    // Non-vacuous: the ink really recoloured pixels vs the plain frame.
    assert_ne!(gpu_ink.pixels, gpu_plain.pixels);
}

/// Ink under selection and on a DEC double-width row stays CPU==GPU: ink first,
/// then the selection fg floor (an ink near the band colour, so the floor must
/// move it), and the inked glyph stretched over the doubled advance.
#[test]
fn ink_selection_and_decdwl_gpu_match_cpu() {
    use aterm_core::render::InkCell;
    use aterm_core::selection::{SelectionSide, SelectionType};
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
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

    // Non-vacuous: ink changed the frame vs no ink.
    let plain = term.cell_frame(rows, cols);
    assert_ne!(
        gpu_frame.pixels,
        gpu.render_input(&mut win, &plain, None).pixels
    );
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
    let (mut cpu, mut gpu) = backends(px, theme);
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

    // Non-vacuity: dropping the list hands the frame back to the scalar
    // authority, which here paints no band.
    let plain = term.cell_frame(rows, cols);
    assert_ne!(
        gpu_frame.pixels,
        gpu.render_input(&mut win, &plain, None).pixels
    );
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
/// GPU translated frames match within tolerance, the GPU's chrome rows are
/// identical to its frac-0 frame, and grid pixels moved both ways.
#[test]
fn sub_row_scroll_translate_gpu_matches_cpu_at_asymmetric_origin() {
    let theme = Theme::default();
    let px = 18.0;
    let (rows, cols) = (8usize, 20usize);

    let (mut cpu, mut gpu) = backends(px, theme);
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
    // The GPU's frac-0 baseline.
    let gpu_zero = {
        let mut t = scroll_seeded_term(rows, cols);
        gpu.render_input(&mut win, &with_partition(&mut t, 0), None)
    };
    let (w, h) = (gpu_zero.width, gpu_zero.height);

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
            ((gpu_f.width, gpu_f.height), (cpu_f.width, cpu_f.height)),
            ((w, h), (w, h)),
            "dims stable across frac and backend"
        );

        // (1) CPU == GPU for the TRANSLATED frame, within the standard tolerance.
        let delta = max_channel_delta(&cpu_f, &gpu_f);
        assert!(
            delta <= 8,
            "M1b frac={frac}: GPU/CPU translated frames diverge (max delta {delta} > 8)"
        );

        // (2) GPU chrome invariance (the CPU's is aterm-render's
        //     `scroll_frac_translate`): rows outside [y0, y1) equal the frac-0
        //     frame byte-for-byte (the shift touched only the grid band).
        for y in (0..y0).chain(y1..h) {
            assert_eq!(
                &gpu_f.pixels[y * w..y * w + w],
                &gpu_zero.pixels[y * w..y * w + w],
                "M1b frac={frac}: GPU chrome row {y} must be invariant"
            );
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

/// Incoming-row apron parity: on a positive-frac frame the exposed bottom
/// strip (the incoming row's top `frac` px, which aterm-render's
/// `incoming_row_apron` proves on the CPU) is byte-identical on the GPU, is not
/// the placeholder, and the GPU's chrome rows stay pinned to the frac-0 frame.
#[test]
fn incoming_row_apron_gpu_matches_cpu() {
    let theme = Theme::default();
    let (rows, cols) = (8usize, 20usize);
    let (mut cpu, mut gpu) = backends(18.0, theme);
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
    let gpu_flat = gpu.render_input(&mut win, &partitioned(&mut tg, 0), None);

    let w = cpu_glide.width;
    assert_eq!((gpu_glide.width, gpu_glide.height), (w, cpu_glide.height));
    let y1 = grid_top + rows * ch; // grid_bot_row == rows
    let band = |f: &Frame, y0: usize, k: usize| f.pixels[y0 * w..(y0 + k) * w].to_vec();
    let gpu_strip = band(&gpu_glide, y1 - n, n);
    assert_eq!(
        gpu_strip,
        band(&cpu_glide, y1 - n, n),
        "the apron strip is byte-identical CPU==GPU"
    );
    assert_ne!(
        gpu_strip,
        band(&gpu_flat, y1 - n, n),
        "GPU: placeholder gone"
    );
    assert!(
        gpu_strip.iter().any(|&p| dist(p, BG) > 0),
        "non-vacuity: the strip carries ink"
    );
    // Chrome invariance with the strip painted: rows outside the band equal
    // the frac-0 frame.
    let y0 = grid_top + ch; // grid_top_row == 1
    for y in (0..y0).chain(y1..gpu_glide.height) {
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

/// The overline's own colour channel on the GPU (aterm-render's
/// `overline_color` is the CPU oracle). Both resolve it through
/// `aterm_render::deco_inks`; a seam must hold one tone, exactly, across a row
/// whose cells have different foregrounds, and colour only its own rule.
#[test]
fn the_overline_colour_channel_paints_the_same_seam_on_both_backends() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (1usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Two runs of overlined text in two very different inks; the cursor is
    // hidden so no blink phase can move a pixel between the renders compared.
    term.process(b"\x1b[?25l\x1b[53m\x1b[38;2;80;80;80mdim\x1b[38;2;255;255;255mLIT");

    // The un-channelled frame: with no channel a rule follows each cell's fg.
    let plain = term.cell_frame(rows, cols);
    let gpu_plain = gpu.render_input(&mut win, &plain, None);

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

    let (f, plain_f) = (&gpu_frame, &gpu_plain);
    let t = band_depth(f, cw, ch, 0, want);
    assert!(
        (1..ch).contains(&t),
        "the seam must be a band inside the cell, not {t} of {ch} rows"
    );
    for col in 0..6 {
        let band = top_row(f, cw, col);
        assert!(
            band.iter().all(|px| *px == want),
            "column {col} breaks the seam's one tone: {band:?}"
        );
        assert_eq!(
            band_depth(f, cw, ch, col, want),
            t,
            "column {col} wears the seam to a different depth"
        );
    }
    // The channel colours ONE rule, not the text under it: everything below
    // the band is what it was before any seam colour was set.
    assert_eq!(
        &f.pixels[f.width * t..],
        &plain_f.pixels[plain_f.width * t..],
        "the channel must not reach any row but the overline's own"
    );
}

/// The RGB of every pixel outside the `grid_w`x`grid_h` grid at interior pad
/// `pad`, asserted identical across the backends.
fn padding_rgb(cpu: &Frame, gpu: &Frame, pad: usize, grid_w: usize, grid_h: usize) -> Vec<u32> {
    assert_eq!((gpu.width, gpu.height), (cpu.width, cpu.height));
    let mut out = Vec::new();
    for y in 0..cpu.height {
        for x in 0..cpu.width {
            if (pad..pad + grid_w).contains(&x) && (pad..pad + grid_h).contains(&y) {
                continue;
            }
            let i = y * cpu.width + x;
            let (c, g) = (cpu.pixels[i] & 0x00ff_ffff, gpu.pixels[i] & 0x00ff_ffff);
            assert_eq!(
                c, g,
                "padding pixel ({x},{y}) differs: cpu {c:06x} gpu {g:06x}"
            );
            out.push(c);
        }
    }
    assert!(!out.is_empty(), "the frame has padding");
    out
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
    let (mut cpu, mut gpu) = backends(18.0, theme);
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
    let padding = padding_rgb(&cpu_frame, &gpu_frame, P, cols * cw, rows * ch);
    for tone in [BAND, SEAM, FILL, TRACK] {
        assert!(padding.contains(&tone), "{tone:06x} is not painted");
    }
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
    let (mut cpu, mut gpu) = backends(18.0, theme);
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
    assert!(
        !padding_rgb(&cpu_frame, &gpu_frame, P, cols * cw, rows * ch).contains(&SEAM),
        "no gutter segment of the bleed's own seam"
    );
    let top = cpu.grid_top();
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
    let (mut cpu, mut gpu) = backends(18.0, theme);
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
    let (mut cpu, mut gpu) = backends(18.0, theme);
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
    let (mut cpu, mut gpu) = backends(18.0, theme);
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
