// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// CPU==GPU parity with a ligating font: plain, under a selection, under a block
// cursor and under ink.

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_render::{LigatureMode, Renderer, TextShapingConfig, Theme};

use crate::common::{backends_with_font, max_channel_delta_frame as max_channel_delta};

fn ligature_test_font() -> std::path::PathBuf {
    if let Some(p) = std::env::var("ATERM_LIGATURE_TEST_FONT")
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
    {
        return p;
    }
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
    );
    let p = std::path::PathBuf::from(FIXTURE);
    assert!(p.exists(), "{FIXTURE}: missing (a committed fixture)");
    p
}

#[test]
fn ligature_font_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let font = ligature_test_font();

    let (mut cpu, mut gpu) = backends_with_font(&font, px, theme);
    let mut cpu_off = Renderer::from_system_with_family(font.to_str(), px, theme)
        .expect("the ligature fixture loads");
    cpu_off.set_text_shaping(TextShapingConfig {
        ligature_mode: LigatureMode::Disabled,
        ..Default::default()
    });

    let (rows, cols) = (1usize, 28usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25la => b != c == d -> e");

    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let cpu_off_frame = cpu_off.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ"
    );

    assert_ne!(
        cpu_frame.pixels, cpu_off_frame.pixels,
        "operators did not ligate — test would be vacuous (is this really a ligature font?)"
    );

    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("ligature GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU ligature pixels diverge: max per-channel delta {delta} > 8"
    );
}

#[test]
fn ligature_selection_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let font = ligature_test_font();

    let (mut cpu, mut gpu) = backends_with_font(&font, px, theme);

    let (rows, cols) = (1usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25la=>b");
    let sel = term.text_selection_mut();
    sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 1, SelectionSide::Right);
    sel.complete_selection();

    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_sel = cpu.render_input(&input);
    let gpu_sel = gpu.render_input(&mut win, &input, None);

    let mut no_sel_term = Terminal::new(rows as u16, cols as u16);
    no_sel_term.process(b"\x1b[?25la=>b");
    let cpu_no_sel = cpu.render_input(&no_sel_term.cell_frame(rows, cols));
    assert_ne!(
        cpu_sel.pixels, cpu_no_sel.pixels,
        "selecting half of '=>' must change the CPU render — selection break did not fire"
    );

    assert_eq!(
        (gpu_sel.width, gpu_sel.height),
        (cpu_sel.width, cpu_sel.height),
        "dimensions differ"
    );
    let delta = max_channel_delta(&cpu_sel, &gpu_sel);
    eprintln!("ligature+selection GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU diverge WITH a selection: max per-channel delta {delta} > 8"
    );
}

#[test]
fn cursor_cutout_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let font = ligature_test_font();

    let (mut cpu, mut gpu) = backends_with_font(&font, px, theme);
    // Block on lazy CJK fallback parsing so neither backend compares 日 against `.notdef`.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();

    let (rows, cols) = (1usize, 12usize);
    let scenario = |cursor: Option<usize>| -> aterm_render::RenderInput {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[2 q");
        term.process("a=>b \u{65E5}x".as_bytes());
        match cursor {
            Some(c) => term.process(format!("\x1b[1;{}H", c + 1).as_bytes()),
            None => term.process(b"\x1b[?25l"),
        }
        term.cell_frame(rows, cols)
    };

    let hidden_cpu = cpu.render_input(&scenario(None));
    for (label, col) in [("tail '>'", 2usize), ("lead '='", 1), ("wide 日 lead", 5)] {
        let input = scenario(Some(col));
        let cpu_frame = cpu.render_input(&input);
        let mut win = aterm_gpu::WindowGpu::new();
        let gpu_frame = gpu.render_input(&mut win, &input, None);
        assert_eq!(
            (gpu_frame.width, gpu_frame.height),
            (cpu_frame.width, cpu_frame.height),
            "dimensions differ ({label})"
        );
        assert_ne!(
            cpu_frame.pixels, hidden_cpu.pixels,
            "cursor on {label} changed nothing — scenario is vacuous"
        );
        let delta = max_channel_delta(&cpu_frame, &gpu_frame);
        eprintln!("cursor cut-out ({label}) GPU vs CPU max per-channel delta = {delta}");
        assert!(
            delta <= 8,
            "GPU/CPU diverge with a block cursor on {label}: max per-channel \
             delta {delta} > 8"
        );
    }
}

#[test]
fn ligature_ink_gpu_matches_cpu() {
    use aterm_core::render::InkCell;
    let theme = Theme::default();
    let px = 18.0;

    let font = ligature_test_font();

    let (mut cpu, mut gpu) = backends_with_font(&font, px, theme);

    let (rows, cols) = (1usize, 16usize);
    let ink: [u8; 3] = [0x7C, 0xC8, 0xFF];

    let mut term_a = Terminal::new(rows as u16, cols as u16);
    term_a.process(b"\x1b[?25la=>b");
    let mut inked = term_a.cell_frame(rows, cols);
    inked.ink = (0..4u16)
        .map(|col| InkCell {
            row: 0,
            col,
            color: ink,
        })
        .collect();

    let mut term_b = Terminal::new(rows as u16, cols as u16);
    term_b.process(b"\x1b[?25l\x1b[38;2;124;200;255ma=>b\x1b[39m");
    let recolored = term_b.cell_frame(rows, cols);

    let mut win = aterm_gpu::WindowGpu::new();
    let cpu_ink = cpu.render_input(&inked);
    let cpu_sgr = cpu.render_input(&recolored);
    assert_eq!(
        cpu_ink.pixels, cpu_sgr.pixels,
        "CPU: inking the ligature run must equal the SGR fg recolor byte-for-byte"
    );
    let gpu_ink = gpu.render_input(&mut win, &inked, None);
    let gpu_sgr = gpu.render_input(&mut win, &recolored, None);
    assert_eq!(
        gpu_ink.pixels, gpu_sgr.pixels,
        "GPU: inking the ligature run must equal the SGR fg recolor byte-for-byte"
    );

    let plain = term_a.cell_frame(rows, cols);
    assert_ne!(cpu_ink.pixels, cpu.render_input(&plain).pixels);

    let delta = max_channel_delta(&cpu_ink, &gpu_ink);
    eprintln!("ligature+ink GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU ligature-ink pixels diverge: max per-channel delta {delta} > 8"
    );
}
