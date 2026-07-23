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
use aterm_render::{Frame, Renderer, Theme};

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

    // Gate: no GPU or no font -> skip cleanly.
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
    let input = term.cell_frame(rows, cols);
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
fn inactive_selection_bg_gpu_matches_cpu() {
    // When the pane is UNFOCUSED, selected cells must paint with the (derived or
    // explicit) INACTIVE selection bg instead of the active `Theme::selection`, on
    // BOTH the CPU and GPU paths, byte-equal within the parity tolerance. Mirrors
    // `selection_highlight_gpu_matches_cpu`, but toggling the focus flag.
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

    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"hello\r\nworld");
    let sel = term.text_selection_mut();
    sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 3, SelectionSide::Right);
    sel.complete_selection();

    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let input = term.cell_frame(rows, cols);

    // The derived inactive bg the renderer will paint (single source of truth).
    let inactive_bg = aterm_render::derive_inactive_selection_bg(theme.selection, theme.bg);
    // It must DIFFER from the active selection — otherwise this test proves nothing.
    assert!(
        !near(inactive_bg, theme.selection, 8),
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
            .filter(|&&p| near(p, theme.selection, 8))
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
            .filter(|&&p| near(p, theme.selection, 8))
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
    let input = term.cell_frame(rows, cols);
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
}
