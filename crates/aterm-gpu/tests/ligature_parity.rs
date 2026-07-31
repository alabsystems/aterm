// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// CPU==GPU parity with an ACTIVELY-LIGATING font. The other parity tests use the
// host system font (which may not ligate the demo text), so this one points BOTH
// renderers at the bundled JetBrains Mono via $ATERM_FONT and renders a row full
// of programming operators ("a => b != c == d -> e <= f"). It asserts:
//   1. the CPU frame actually ligated (it differs from the same renderer with
//      ligatures forced off — so the test is non-vacuous), and
//   2. the GPU frame matches the CPU frame within the usual <=8 LSB blend
//      tolerance — i.e. the shared shaping plan keys + places the IDENTICAL
//      ligature glyph on both paths.
// Its own test BINARY (separate process) so the $ATERM_FONT env set here never
// races the other parity SUITES; within this binary the set is hoisted behind a
// OnceLock (see ligature_test_font) so the parallel #[test] threads never race
// it either. Gated: no GPU / font -> skip cleanly.

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_render::{LigatureMode, Renderer, TextShapingConfig, Theme};

mod common;
use common::{backends, max_channel_delta_frame as max_channel_delta};

// Layout-independent ligature font discovery, and the SINGLE point where
// $ATERM_FONT is exported to both renderers. Order: (a) $ATERM_FONT if already
// set and readable; (b) the committed fixture in the sibling aterm-render crate
// (present in both canonical and vendored layouts).
//
// Every test in this binary wants the SAME font, but libtest runs the #[test]
// fns on PARALLEL threads — a per-test set_var would race a sibling test's
// renderer construction (C-side getenv/setenv under concurrent mutation is
// dangling-pointer UB). So the mutation is hoisted here: `get_or_init` parks
// every caller until the closure returns, so the ONE write is complete before
// any renderer in this process is built — the same guarantee glow_parity.rs
// gets from its Once. Returns the resolved path; None -> the caller SKIPs.
fn ligature_test_font() -> Option<&'static std::path::Path> {
    static FONT: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        let found = std::env::var("ATERM_FONT")
            .ok()
            .map(std::path::PathBuf::from)
            .filter(|p| p.exists())
            .or_else(|| {
                // aterm-gpu manifest is crates/aterm-gpu; the fixture is a sibling crate over.
                const FIXTURE: &str = concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
                );
                let p = std::path::PathBuf::from(FIXTURE);
                p.exists().then_some(p)
            })?;
        // Set exactly once per process (OnceLock init), before any renderer is
        // constructed — every concurrent caller is parked in get_or_init until this
        // write completes, so no getenv can observe it mid-mutation — and routed
        // through the workspace's one lock-scoped env helper.
        aterm_log::env::set("ATERM_FONT", &found);
        Some(found)
    })
    .as_deref()
}

#[test]
fn ligature_font_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    // Points BOTH renderers at the ligature font: resolves AND exports $ATERM_FONT,
    // once per process (see ligature_test_font).
    if ligature_test_font().is_none() {
        eprintln!("SKIP: no ligature test font (set ATERM_FONT or add the repo fixture)");
        return;
    }

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    // A CPU renderer with ligatures FORCED OFF, to prove the ligated frame is not
    // vacuously equal (the font really ligates the operators).
    let Some(mut cpu_off) = Renderer::from_system(px, theme) else {
        return;
    };
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

    // Non-vacuous: with this font the operators actually ligate (ligated != off).
    assert_ne!(
        cpu_frame.pixels, cpu_off_frame.pixels,
        "operators did not ligate — test would be vacuous (is this really a ligature font?)"
    );

    // The core gate: GPU reproduces the CPU ligature frame within the blend
    // tolerance, because both keyed + placed the IDENTICAL `mono_gid` glyph.
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("ligature GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU ligature pixels diverge: max per-channel delta {delta} > 8"
    );
}

/// CPU==GPU parity WITH a selection active over part of a ligature. The shared
/// `ligature_break_cols_into` now breaks runs on selection columns, so a ligature must
/// not span the selection-highlight boundary on EITHER path. This drives "a=>b"
/// with col 1 (the '=' of the arrow) selected and asserts both that the CPU frame
/// changed vs no-selection (non-vacuous: the break actually fired) and that the
/// GPU frame still matches the CPU frame within the blend tolerance — i.e. both
/// paths consumed the IDENTICAL break set + plan with the selection active.
#[test]
fn ligature_selection_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    // Resolves AND exports $ATERM_FONT, once per process (see ligature_test_font).
    if ligature_test_font().is_none() {
        eprintln!("SKIP: no ligature test font (set ATERM_FONT or add the repo fixture)");
        return;
    }

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (1usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25la=>b");
    // Select exactly col 1 (the '=' half of the '=>' arrow): Left start + Right end.
    let sel = term.text_selection_mut();
    sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 1, SelectionSide::Right);
    sel.complete_selection();

    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_sel = cpu.render_input(&input);
    let gpu_sel = gpu.render_input(&mut win, &input, None);

    // Non-vacuous: the same text with NO selection differs (the selection break
    // fired and changed the CPU ink/bg), so this is a real selection scenario.
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

/// W4 (cursor ink integrity) CPU==GPU parity: a BLOCK cursor over ligature and
/// wide-glyph geometry. The CPU side of these frames is PROVEN correct by
/// aterm-render/tests/cursor_ink.rs (partition/no-bleed: the complement of the
/// cursor rect is byte-identical to the no-cursor frame); this test binds the
/// GPU's quad slicing (`glyph_quad` x-clip + widened `cursor_block` fill +
/// ligature-run cut-out sources) to those frames pixel-for-pixel:
///   * cursor on the '>' of '=>' (the covering-glyph column — pre-W4 the GPU
///     moved the WHOLE arrow quad into the bg-coloured cursor stream, erasing
///     the '=' cell's ink),
///   * cursor on the '=' lead (pre-W4: empty placeholder = no cut-out at all),
///   * cursor on a wide 日 lead (pre-W4: single-cell fill + full-glyph re-blit
///     erased the ideograph's right half).
///
/// Non-vacuous: each cursor frame must differ from the cursor-hidden frame.
#[test]
fn cursor_cutout_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    // Resolves AND exports $ATERM_FONT, once per process (see ligature_test_font).
    if ligature_test_font().is_none() {
        eprintln!("SKIP: no ligature test font (set ATERM_FONT or add the repo fixture)");
        return;
    }

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (1usize, 12usize);
    // '=>' at cols 1-2; wide 日 lead+continuation at cols 5-6 (same row the CPU
    // no-bleed sweep proves). DECSCUSR 2 = steady block, cursor VISIBLE.
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
        // Non-vacuous: the cursor really drew something.
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

/// Animated ink over an ACTIVELY-LIGATED run stays CPU==GPU: the run's single
/// shaped glyph takes the ink colour of its owning column (both backends read
/// the SAME per-column plan + the same ink slice), so inking every column of
/// the run is byte-identical to recolouring the run via SGR truecolor fg on
/// EACH backend — and the two backends agree within the suite's delta bar.
#[test]
fn ligature_ink_gpu_matches_cpu() {
    use aterm_core::render::InkCell;
    let theme = Theme::default();
    let px = 18.0;

    // Resolves AND exports $ATERM_FONT, once per process (see ligature_test_font).
    if ligature_test_font().is_none() {
        eprintln!("SKIP: no ligature test font (set ATERM_FONT or add the repo fixture)");
        return;
    }

    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };

    let (rows, cols) = (1usize, 16usize);
    let ink: [u8; 3] = [0x7C, 0xC8, 0xFF];

    // "a=>b": ink cols 0..=3 (the whole run, so the owning column is covered
    // regardless of which cell the plan hands the shaped glyph).
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

    // The same run recoloured via SGR truecolor fg — no ink.
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

    // Non-vacuous: ink actually recoloured the ligated glyph.
    let plain = term_a.cell_frame(rows, cols);
    assert_ne!(cpu_ink.pixels, cpu.render_input(&plain).pixels);

    let delta = max_channel_delta(&cpu_ink, &gpu_ink);
    eprintln!("ligature+ink GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU ligature-ink pixels diverge: max per-channel delta {delta} > 8"
    );
}
