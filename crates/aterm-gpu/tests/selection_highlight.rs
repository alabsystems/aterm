// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Selection-highlight gate for the GPU renderer: with an active text selection,
// the GPU must paint selected cells with `Theme::selection` as their background
// (glyph foreground unchanged), leave unselected cells alone, and stay
// pixel-equal to the CPU renderer within the same per-channel tolerance the
// `gpu_matches_cpu` test uses.
//
// Gated: if there is no GPU or no system font, the test no-ops (returns).

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_render::{Frame, Renderer, SelectionClip, Theme};

mod common;
use common::{backends, bb, gg, max_channel_delta_frame as max_channel_delta, rr};

fn cell_pixels(f: &Frame, cw: usize, ch: usize, row: usize, col: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw * ch);
    for y in row * ch..(row * ch + ch).min(f.height) {
        for x in col * cw..(col * cw + cw).min(f.width) {
            out.push(f.pixels[y * f.width + x]);
        }
    }
    out
}

/// Per-channel closeness to a packed `0x00RRGGBB` colour.
fn near(p: u32, c: u32, tol: i32) -> bool {
    (rr(p) - rr(c)).abs() <= tol && (gg(p) - gg(c)).abs() <= tol && (bb(p) - bb(c)).abs() <= tol
}

#[test]
fn selection_highlight_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"hello\r\nworld");
    // Select row 0, cols 1..=3 ("ell"). Cursor sits at (1,5) — away from every
    // cell this test inspects.
    let sel = term.text_selection_mut();
    sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 3, SelectionSide::Right);
    sel.complete_selection();

    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    // This phase exercises the raw renderer setting: an unresolved producer
    // delegates selected-text policy to the configured renderer.
    input.selection_fg = aterm_core::render::COLOR_UNSET;
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    // (iii) frames match within the gpu_matches_cpu tolerance, selection active.
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
        // (i) a selected cell's dominant background is ~theme.selection.
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

        // (ii) unselected cells keep the theme/default background.
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
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(gpu) => gpu,
        Err(error) => {
            eprintln!("SKIP: no GPU/font available: {error}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
    });
    input.images[0].push((
        5,
        aterm_core::grid::extra::ImageRef {
            image,
            cell_row: 0,
            cell_col: 0,
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
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(gpu) => gpu,
        Err(error) => {
            eprintln!("SKIP: no GPU/font available: {error}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
    // When the pane is UNFOCUSED, selected cells must paint with the (derived or
    // explicit) INACTIVE selection bg instead of the active `Theme::selection`, on
    // BOTH the CPU and GPU paths, byte-equal within the parity tolerance. Mirrors
    // `selection_highlight_gpu_matches_cpu`, but toggling the focus flag.
    let theme = Theme::default();
    let px = 18.0;

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

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

    // The derived inactive bg follows terminal-owned OSC 17 and the frame's
    // live default background, not the renderer's stale static theme.
    assert_eq!(input.selection_bg, 0x0060_4020);
    let inactive_bg =
        aterm_render::derive_inactive_selection_bg(input.selection_bg, input.default_bg);
    // It must DIFFER from the active selection — otherwise this test proves nothing.
    assert!(
        !near(inactive_bg, input.selection_bg, 8),
        "derived inactive bg must visibly differ from the active selection"
    );

    // (A) UNFOCUSED: both paths paint the inactive bg in selected cells, and match.
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

    // (B) FOCUSED again: both paths revert to the ACTIVE selection bg, and match.
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
}

#[test]
fn selection_fg_override_gpu_matches_cpu() {
    // With an explicit selectionForeground override, the GPU and CPU must paint
    // selected glyphs in that colour identically (parity), instead of the WCAG
    // contrast-floor default.
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // Distinctive override unlike the default fg/bg/selection, on BOTH paths.
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
    // This first phase intentionally exercises the renderer-owned host
    // override. A terminal snapshot with no live selection foreground now
    // carries COLOR_DYNAMIC, which correctly requests automatic contrast and
    // suppresses stale host state; COLOR_UNSET is the explicit delegation
    // sentinel for raw/non-terminal producers.
    input.selection_fg = aterm_core::render::COLOR_UNSET;
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "selection_fg override: GPU/CPU diverge, max per-channel delta {delta} > 8"
    );
    // The override colour must actually paint in a selected glyph's pixels (it is
    // unlike fg/bg/selection, so any near-hit proves the override took effect).
    let sel_px = cell_pixels(&cpu_frame, cw, ch, 0, 1); // selected 'e'
    let hits = sel_px.iter().filter(|&&p| near(p, sel_fg, 40)).count();
    assert!(
        hits > 0,
        "selected glyph should paint the selectionForeground override (hits={hits})"
    );

    // An engine-owned dynamic value is different from an unresolved producer:
    // OSC 21 `key=` must suppress the stale static renderer override and select
    // automatic contrast on both backends.
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
