// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::Terminal;
use aterm_render::{LigatureMode, TextShapingConfig, Theme};
use aterm_types::text_shaping::{FontFeature, FontFeatureSet};

use crate::common::{backends_with_font, max_channel_delta_frame as max_channel_delta};

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

    let off = TextShapingConfig {
        ligature_mode: LigatureMode::Disabled,
        ..Default::default()
    };
    gpu.set_text_shaping(off.clone());
    cpu.set_text_shaping(off);

    let mut win_off = aterm_gpu::WindowGpu::new();
    let gpu_off = gpu.render_input(&mut win_off, &input, None);
    let cpu_off = cpu.render_input(&input);

    assert_ne!(
        gpu_on.pixels, gpu_off.pixels,
        "a live GpuRenderer::set_text_shaping flip did not change GPU pixels"
    );
    let delta = max_channel_delta(&cpu_off, &gpu_off);
    eprintln!("post-flip GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "CPU/GPU diverge after the live shaping flip: max per-channel delta {delta} > 8"
    );
}

fn font_feature_flip_frames() -> (
    aterm_render::Frame,
    aterm_render::Frame,
    aterm_render::Frame,
    bool,
) {
    let theme = Theme::default();
    let px = 18.0;

    let (font, is_fixture) = ligature_test_font();

    let (mut cpu, mut gpu) = backends_with_font(&font, px, theme);

    let (rows, cols) = (1usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l00000000");
    let input = term.cell_frame(rows, cols);

    let mut win_base = aterm_gpu::WindowGpu::new();
    let gpu_base = gpu.render_input(&mut win_base, &input, None);

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
    (gpu_base, gpu_feat, cpu_feat, is_fixture)
}

#[test]
fn live_font_feature_flip_keeps_parity() {
    let (_, gpu_feat, cpu_feat, _) = font_feature_flip_frames();
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

#[test]
fn live_font_feature_flip_reaches_gpu_pixels() {
    let (gpu_base, gpu_feat, _, is_fixture) = font_feature_flip_frames();
    if gpu_base.pixels == gpu_feat.pixels {
        assert!(
            !is_fixture,
            "the bundled fixture's `zero` feature did not reach GPU pixels"
        );
        aterm_spec::host::could_not_run(
            "the $ATERM_LIGATURE_TEST_FONT face has no observable `zero` feature",
        );
    }
}
