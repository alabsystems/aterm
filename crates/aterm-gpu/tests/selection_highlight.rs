// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU/GPU parity tests for selection background and foreground highlighting.

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_render::{Frame, SelectionClip, Theme};

use crate::common::{backends, bb, gg, max_channel_delta_frame as max_channel_delta, rr};

fn cell_pixels(f: &Frame, cw: usize, ch: usize, row: usize, col: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw * ch);
    for y in row * ch..(row * ch + ch).min(f.height) {
        for x in col * cw..(col * cw + cw).min(f.width) {
            out.push(f.pixels[y * f.width + x]);
        }
    }
    out
}

fn near(p: u32, c: u32, tol: i32) -> bool {
    (rr(p) - rr(c)).abs() <= tol && (gg(p) - gg(c)).abs() <= tol && (bb(p) - bb(c)).abs() <= tol
}

#[test]
fn selection_highlight_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let (mut cpu, mut gpu) = backends(px, theme);

    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"hello\r\nworld");
    let sel = term.text_selection_mut();
    sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 3, SelectionSide::Right);
    sel.complete_selection();

    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    input.selection_fg = aterm_core::render::COLOR_UNSET;
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("selection: GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU pixels diverge with selection: max per-channel delta {delta} > 8"
    );

    for (name, f) in [("cpu", &cpu_frame), ("gpu", &gpu_frame)] {
        let sel_px = cell_pixels(f, cw, ch, 0, 2); // selected 'l'
        let n_sel = sel_px
            .iter()
            .filter(|&&p| near(p, theme.selection, 8))
            .count();
        assert!(
            n_sel > sel_px.len() / 2,
            "{name}: selected cell (0,2) should be selection-coloured ({n_sel}/{})",
            sel_px.len()
        );

        let blank_px = cell_pixels(f, cw, ch, 0, 8); // blank, unselected
        let n_bg = blank_px.iter().filter(|&&p| near(p, theme.bg, 8)).count();
        assert!(
            n_bg == blank_px.len(),
            "{name}: blank cell (0,8) should stay theme bg ({n_bg}/{})",
            blank_px.len()
        );
        for (row, col) in [(0usize, 0usize), (0, 4), (1, 2), (0, 8)] {
            let px_cell = cell_pixels(f, cw, ch, row, col);
            let stray = px_cell
                .iter()
                .filter(|&&p| near(p, theme.selection, 8))
                .count();
            assert_eq!(
                stray, 0,
                "{name}: unselected cell ({row},{col}) shows selection colour"
            );
        }
    }
}

#[test]
fn sparse_tail_selection_and_deepest_image_cover_match_cpu_gpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"x");
    let sel = term.text_selection_mut();
    sel.start_selection(0, 4, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 6, SelectionSide::Right);
    sel.complete_selection();

    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false;
    assert_eq!(
        input.cells[0].len(),
        1,
        "fixture requires cols 4..=6 to be omitted sparse-tail cells"
    );
    let image = std::sync::Arc::new(aterm_core::grid::extra::ImageData {
        bytes: vec![240, 20, 80, 255],
        format: aterm_core::grid::extra::ImageFormat::RawRgba8 {
            width: 1,
            height: 1,
        },
        cols: 1,
        rows: 1,
        z_index: aterm_render::KITTY_IMAGE_BELOW_BG_Z_THRESHOLD - 1,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: aterm_core::grid::extra::ImageScaling::Fit,
        source_rect: None,
    });
    input.images[0].push((
        5,
        aterm_core::grid::extra::ImageRef {
            image,
            cell_row: 0,
            cell_col: 0,
            kitty: None,
        },
    ));

    let cpu_frame = cpu.render_input(&input);
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "sparse selection CPU/GPU output diverges by {delta} > 8"
    );

    for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        for col in 4..=6 {
            let pixels = cell_pixels(frame, cw, ch, 0, col);
            assert_eq!(
                pixels
                    .iter()
                    .filter(|&&pixel| near(pixel, theme.selection, 2))
                    .count(),
                pixels.len(),
                "{name}: selected implicit blank col {col} must be entirely selection-filled"
            );
        }
        assert_eq!(
            cell_pixels(frame, cw, ch, 0, 5)
                .iter()
                .filter(|&&pixel| near(pixel, 0x00f0_1450, 2))
                .count(),
            0,
            "{name}: selection must cover the deepest image in an omitted cell"
        );
        let untouched = cell_pixels(frame, cw, ch, 0, 7);
        assert_eq!(
            untouched
                .iter()
                .filter(|&&pixel| near(pixel, theme.bg, 2))
                .count(),
            untouched.len(),
            "{name}: unselected omitted cell remains the frame default"
        );
    }
}

#[test]
fn multiline_selection_clip_matches_cpu_and_never_tints_sibling_cells() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (3usize, 9usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let selection = term.text_selection_mut();
    selection.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(2, 7, SelectionSide::Right);
    selection.complete_selection();

    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false;
    input.selection_bg = 0x0021_4365;
    input.selection_clip = Some(SelectionClip::new(0, rows, 5, cols));
    let cpu_frame = cpu.render_input(&input);
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "clipped multiline selection CPU/GPU output diverges by {delta} > 8"
    );

    for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        for (row, col) in [(0, 5), (0, 8), (1, 5), (1, 8), (2, 5), (2, 7)] {
            let pixels = cell_pixels(frame, cw, ch, row, col);
            assert_eq!(
                pixels
                    .iter()
                    .filter(|&&pixel| near(pixel, input.selection_bg, 2))
                    .count(),
                pixels.len(),
                "{name}: selected focused-pane cell ({row},{col}) must be highlighted"
            );
        }
        for (row, col) in [(0, 4), (1, 0), (1, 4), (2, 4), (2, 8)] {
            let pixels = cell_pixels(frame, cw, ch, row, col);
            assert_eq!(
                pixels
                    .iter()
                    .filter(|&&pixel| near(pixel, theme.bg, 2))
                    .count(),
                pixels.len(),
                "{name}: sibling/divider cell ({row},{col}) must stay at frame background"
            );
        }
    }
}

#[test]
fn inactive_selection_bg_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let (mut cpu, mut gpu) = backends(px, theme);

    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b]17;rgb:60/40/20\x07");
    term.process(b"hello\r\nworld");
    let sel = term.text_selection_mut();
    sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 3, SelectionSide::Right);
    sel.complete_selection();

    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    input.default_bg = 0x00e0_d0c0;

    assert_eq!(input.selection_bg, 0x0060_4020);
    let inactive_bg =
        aterm_render::derive_inactive_selection_bg(input.selection_bg, input.default_bg);
    assert!(
        !near(inactive_bg, input.selection_bg, 8),
        "derived inactive bg must visibly differ from the active selection"
    );

    cpu.set_selection_inactive(true);
    gpu.set_selection_inactive(true);
    let cpu_inactive = cpu.render_input(&input);
    let gpu_inactive = gpu.render_input(&mut win, &input, None);
    let delta_inactive = max_channel_delta(&cpu_inactive, &gpu_inactive);
    assert!(
        delta_inactive <= 8,
        "inactive selection: GPU/CPU diverge, max per-channel delta {delta_inactive} > 8"
    );
    for (name, f) in [("cpu", &cpu_inactive), ("gpu", &gpu_inactive)] {
        let sel_px = cell_pixels(f, cw, ch, 0, 2); // selected 'l'
        let n_inactive = sel_px.iter().filter(|&&p| near(p, inactive_bg, 8)).count();
        let n_active = sel_px
            .iter()
            .filter(|&&p| near(p, input.selection_bg, 8))
            .count();
        assert!(
            n_inactive > sel_px.len() / 2,
            "{name}: unfocused selected cell should use the INACTIVE bg ({n_inactive}/{})",
            sel_px.len()
        );
        assert_eq!(
            n_active, 0,
            "{name}: unfocused selected cell must NOT show the active selection colour"
        );
    }

    cpu.set_selection_inactive(false);
    gpu.set_selection_inactive(false);
    let cpu_active = cpu.render_input(&input);
    let gpu_active = gpu.render_input(&mut win, &input, None);
    let delta_active = max_channel_delta(&cpu_active, &gpu_active);
    assert!(
        delta_active <= 8,
        "active selection: GPU/CPU diverge, max per-channel delta {delta_active} > 8"
    );
    for (name, f) in [("cpu", &cpu_active), ("gpu", &gpu_active)] {
        let sel_px = cell_pixels(f, cw, ch, 0, 2);
        let n_active = sel_px
            .iter()
            .filter(|&&p| near(p, input.selection_bg, 8))
            .count();
        assert!(
            n_active > sel_px.len() / 2,
            "{name}: focused selected cell should use the ACTIVE selection bg ({n_active}/{})",
            sel_px.len()
        );
    }

    let same = input.default_bg;
    assert_eq!(
        aterm_render::derive_inactive_selection_bg(same, same),
        same,
        "selection_bg == default_bg is a fixed point of derive_inactive_selection_bg"
    );
    let mut same_input = input;
    same_input.selection_bg = same;
    cpu.set_selection_inactive(true);
    gpu.set_selection_inactive(true);
    let cpu_same = cpu.render_input(&same_input);
    let gpu_same = gpu.render_input(&mut win, &same_input, None);
    let delta_same = max_channel_delta(&cpu_same, &gpu_same);
    assert!(
        delta_same <= 8,
        "selection_bg == default_bg inactive selection: GPU/CPU diverge, delta {delta_same} > 8"
    );
}

#[test]
fn selection_fg_override_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let sel_fg = 0x00ff_00ffu32;
    cpu.set_selection_fg(Some(sel_fg));
    gpu.set_selection_fg(Some(sel_fg));

    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"hello\r\nworld");
    let sel = term.text_selection_mut();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 4, SelectionSide::Right);
    sel.complete_selection();

    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    input.selection_fg = aterm_core::render::COLOR_UNSET;
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "selection_fg override: GPU/CPU diverge, max per-channel delta {delta} > 8"
    );
    let sel_px = cell_pixels(&cpu_frame, cw, ch, 0, 1); // selected 'e'
    let hits = sel_px.iter().filter(|&&p| near(p, sel_fg, 40)).count();
    assert!(
        hits > 0,
        "selected glyph should paint the selectionForeground override (hits={hits})"
    );

    term.process(b"\x1b]21;selection_foreground=\x1b\\");
    let dynamic_input = term.cell_frame(rows, cols);
    assert_eq!(
        dynamic_input.selection_fg,
        aterm_core::render::COLOR_DYNAMIC
    );
    let cpu_dynamic = cpu.render_input(&dynamic_input);
    let gpu_dynamic = gpu.render_input(&mut win, &dynamic_input, None);
    let dynamic_delta = max_channel_delta(&cpu_dynamic, &gpu_dynamic);
    assert!(
        dynamic_delta <= 8,
        "dynamic selection fg: GPU/CPU diverge, max per-channel delta {dynamic_delta} > 8"
    );
    let dynamic_px = cell_pixels(&cpu_dynamic, cw, ch, 0, 1);
    let stale_hits = dynamic_px.iter().filter(|&&p| near(p, sel_fg, 40)).count();
    assert_eq!(
        stale_hits, 0,
        "OSC 21 dynamic foreground must not reuse the static renderer override"
    );
    let automatic_fg = aterm_render::floor_selection_fg(theme.fg, theme.selection);
    let automatic_hits = dynamic_px
        .iter()
        .filter(|&&p| near(p, automatic_fg, 40))
        .count();
    assert!(
        automatic_hits > 0,
        "OSC 21 dynamic foreground must positively paint the automatic contrast colour"
    );
    assert_ne!(
        cpu_dynamic.pixels, cpu_frame.pixels,
        "explicit renderer foreground and engine-requested automatic contrast must be distinct"
    );
}
