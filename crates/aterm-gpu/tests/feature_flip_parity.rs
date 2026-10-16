// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// A live `GpuRenderer::set_text_shaping()` flip (which invalidates the atlas) reaches GPU pixels
// and keeps CPU==GPU parity on the same renderer.
//
// No GPU: skip.

use aterm_core::terminal::Terminal;
use aterm_render::{LigatureMode, TextShapingConfig, Theme};
use aterm_types::text_shaping::{FontFeature, FontFeatureSet};

use crate::common::{backends_with_font, max_channel_delta_frame as max_channel_delta};

/// The ligature test font (as in ligature_parity.rs), handed to both renderers as their family:
/// the committed JetBrains Mono fixture, which ligates `=>` and has a `zero` (slashed-zero)
/// feature, unless $ATERM_LIGATURE_TEST_FONT overrides it. Returns (path, is_fixture), where
/// is_fixture means the `zero` feature can be relied on.
fn ligature_test_font() -> (std::path::PathBuf, bool) {
    if let Some(p) = std::env::var("ATERM_LIGATURE_TEST_FONT")
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
    {
        return (p, false);
    }
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
    );
    let p = std::path::PathBuf::from(FIXTURE);
    assert!(p.exists(), "{FIXTURE}: missing (a committed fixture)");
    (p, true)
}

/// A live ligature on->off flip via `set_text_shaping` (the entry point the
/// GUI config wiring calls) on a renderer with a resident atlas reaches GPU
/// pixels and keeps CPU==GPU.
#[test]
fn live_ligature_flip_changes_gpu_pixels_and_keeps_parity() {
    let theme = Theme::default();
    let px = 18.0;

    let (font, _) = ligature_test_font();

    let (mut cpu, mut gpu) = backends_with_font(&font, px, theme);

    let (rows, cols) = (1usize, 28usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25la => b != c == d -> e");
    let input = term.cell_frame(rows, cols);

    // First frame: ligatures on (default), leaving the GPU's resident atlas warm.
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

    // Live flip both renderers to ligatures off.
    let off = TextShapingConfig {
        ligature_mode: LigatureMode::Disabled,
        ..Default::default()
    };
    gpu.set_text_shaping(off.clone());
    cpu.set_text_shaping(off);

    let mut win_off = aterm_gpu::WindowGpu::new();
    let gpu_off = gpu.render_input(&mut win_off, &input, None);
    let cpu_off = cpu.render_input(&input);

    // The live GPU flip reached pixels (the operators no longer ligate).
    assert_ne!(
        gpu_on.pixels, gpu_off.pixels,
        "a live GpuRenderer::set_text_shaping flip did not change GPU pixels"
    );
    // CPU==GPU still holds after the live flip (both re-shaped per cell).
    let delta = max_channel_delta(&cpu_off, &gpu_off);
    eprintln!("post-flip GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "CPU/GPU diverge after the live shaping flip: max per-channel delta {delta} > 8"
    );
}

/// A live `font_features` flip (`zero` = slashed zero) reaches GPU pixels and
/// keeps CPU==GPU. Non-vacuity is enforced for the bundled fixture; a font
/// from $ATERM_LIGATURE_TEST_FONT without `zero` skips (logged).
#[test]
fn live_font_feature_flip_reaches_gpu_pixels() {
    let theme = Theme::default();
    let px = 18.0;

    let (font, is_fixture) = ligature_test_font();

    let (mut cpu, mut gpu) = backends_with_font(&font, px, theme);

    // A row of zeros: the `zero` feature slashes each one (a 1:1, cadence-preserving
    // substitution that survives the planner's monospace guard).
    let (rows, cols) = (1usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l00000000");
    let input = term.cell_frame(rows, cols);

    let mut win_base = aterm_gpu::WindowGpu::new();
    let gpu_base = gpu.render_input(&mut win_base, &input, None);

    // Live flip to font_features = [zero=1] on both renderers.
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

    // Parity holds under the live feature config either way.
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
