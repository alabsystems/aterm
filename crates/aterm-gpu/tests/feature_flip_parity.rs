// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// A LIVE `GpuRenderer::set_text_shaping()` flip must reach GPU pixels AND keep
// CPU==GPU parity. `ligature_parity.rs` builds the renderers fresh and only flips
// the CPU side, so nothing exercised the new GPU config passthrough
// (`GpuRenderer::set_text_shaping` -> `invalidate_atlas`) on a live renderer. These
// tests drive the same renderer through a config change and re-render.
//
// Gated: no GPU / font -> skip cleanly.

use aterm_core::terminal::Terminal;
use aterm_render::{LigatureMode, TextShapingConfig, Theme};
use aterm_types::text_shaping::{FontFeature, FontFeatureSet};

mod common;
use common::{backends_with_font, max_channel_delta_frame as max_channel_delta};

// Layout-independent ligature font discovery (mirrors ligature_parity.rs): the
// bundled JetBrains Mono ligates `=>` and carries a `zero` (slashed-zero) feature.
// The dedicated $ATERM_LIGATURE_TEST_FONT overrides it. The path is handed to both
// renderers as their FAMILY (`backends_with_font`), so nothing here writes the
// environment. Returns (path, is_fixture): is_fixture is true iff discovery fell
// through to the bundled fixture, whose `zero` feature the tests may rely on.
fn ligature_test_font() -> Option<(std::path::PathBuf, bool)> {
    if let Some(p) = std::env::var("ATERM_LIGATURE_TEST_FONT")
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
    {
        return Some((p, false));
    }
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
    );
    let p = std::path::PathBuf::from(FIXTURE);
    p.exists().then_some((p, true))
}

/// A live ligature on->off flip via `set_text_shaping` reaches GPU pixels and keeps
/// CPU==GPU. This is the same `set_text_shaping` entrypoint the GUI config wiring
/// calls, exercised on a LIVE GpuRenderer (atlas already resident from the first
/// frame) — the path the prior tests never covered.
#[test]
fn live_ligature_flip_changes_gpu_pixels_and_keeps_parity() {
    let theme = Theme::default();
    let px = 18.0;

    // Builds BOTH renderers on the ligature font (see ligature_test_font).
    let Some((font, _)) = ligature_test_font() else {
        eprintln!(
            "SKIP: no ligature test font (set ATERM_LIGATURE_TEST_FONT or add the repo fixture)"
        );
        return;
    };

    let Some((mut cpu, mut gpu)) = backends_with_font(&font, px, theme) else {
        return;
    };

    let (rows, cols) = (1usize, 28usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25la => b != c == d -> e");
    let input = term.cell_frame(rows, cols);

    // First frame: ligatures ON (default). Resident atlas is now warm on the GPU.
    let mut win_on = aterm_gpu::WindowGpu::new();
    let gpu_on = gpu.render_input(&mut win_on, &input, None);
    let cpu_on = cpu.render_input(&input);
    assert_eq!(
        (gpu_on.width, gpu_on.height),
        (cpu_on.width, cpu_on.height),
        "dimensions differ (ligatures on)"
    );
    assert!(
        max_channel_delta(&cpu_on, &gpu_on) <= 8,
        "CPU/GPU diverge with ligatures on"
    );

    // LIVE flip both renderers to ligatures OFF (the new GpuRenderer passthrough).
    let off = TextShapingConfig {
        ligature_mode: LigatureMode::Disabled,
        ..Default::default()
    };
    gpu.set_text_shaping(off.clone());
    cpu.set_text_shaping(off);

    let mut win_off = aterm_gpu::WindowGpu::new();
    let gpu_off = gpu.render_input(&mut win_off, &input, None);
    let cpu_off = cpu.render_input(&input);

    // The live GPU flip actually reached pixels (the operators no longer ligate).
    assert_ne!(
        gpu_on.pixels, gpu_off.pixels,
        "a live GpuRenderer::set_text_shaping flip did not change GPU pixels"
    );
    // And CPU==GPU still holds AFTER the live flip (both re-shaped per-cell).
    let delta = max_channel_delta(&cpu_off, &gpu_off);
    eprintln!("post-flip GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "CPU/GPU diverge after the live shaping flip: max per-channel delta {delta} > 8"
    );
}

/// A live `font_features` flip (`zero` = slashed zero) reaches GPU pixels and keeps
/// CPU==GPU. This proves the headline `font_features` knob is not a no-op on the GPU
/// backend. Non-vacuity is enforced for the bundled JetBrains Mono fixture; if a
/// host points $ATERM_LIGATURE_TEST_FONT at a font WITHOUT a `zero` feature it
/// SKIPs (logged) rather than failing, so the suite stays portable.
#[test]
fn live_font_feature_flip_reaches_gpu_pixels() {
    let theme = Theme::default();
    let px = 18.0;

    // is_fixture: whether the bundled fixture (with its `zero` feature) is in use
    // (see ligature_test_font).
    let Some((font, is_fixture)) = ligature_test_font() else {
        eprintln!("SKIP: no ligature test font");
        return;
    };

    let Some((mut cpu, mut gpu)) = backends_with_font(&font, px, theme) else {
        return;
    };

    // A row of zeros: the `zero` feature slashes each one (a 1:1, cadence-preserving
    // substitution that survives the planner's monospace guard).
    let (rows, cols) = (1usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l00000000");
    let input = term.cell_frame(rows, cols);

    let mut win_base = aterm_gpu::WindowGpu::new();
    let gpu_base = gpu.render_input(&mut win_base, &input, None);

    // LIVE flip to font_features = [zero=1] on BOTH renderers.
    let zero = TextShapingConfig {
        font_features: vec![FontFeatureSet {
            font_id: 0,
            features: vec![FontFeature::new(*b"zero", 1)],
        }],
        ..Default::default()
    };
    gpu.set_text_shaping(zero.clone());
    cpu.set_text_shaping(zero);

    let mut win_feat = aterm_gpu::WindowGpu::new();
    let gpu_feat = gpu.render_input(&mut win_feat, &input, None);
    let cpu_feat = cpu.render_input(&input);

    if gpu_base.pixels == gpu_feat.pixels {
        if is_fixture {
            panic!("the bundled fixture's `zero` feature did not reach GPU pixels");
        }
        eprintln!("SKIP: the resolved ligature font has no observable `zero` feature; parity-only");
    }

    // Parity holds under the live feature config (the core invariant either way).
    assert_eq!(
        (gpu_feat.width, gpu_feat.height),
        (cpu_feat.width, cpu_feat.height),
        "dimensions differ"
    );
    let delta = max_channel_delta(&cpu_feat, &gpu_feat);
    eprintln!("font-feature GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "CPU/GPU diverge under a live font_features flip: max per-channel delta {delta} > 8"
    );
}
