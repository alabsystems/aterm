// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lumen wake: the deterministic half of the release performance gates, and
//! the shipping config both halves drive. The timing half is a bench,
//! `benches/cursor_bench.rs`, which mounts this file for [`config`].

use std::time::Duration;

use aterm_effects::cursor_glow::{GlowConfig, GlowStyle};

/// The shipping default config in `style`.
pub(crate) fn config(style: GlowStyle) -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        // Shipping default: the tall body; explicit underline is exercised by
        // the workload matrix benchmark.
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        // The documented default dark palette — a COHERENT pair, never 0/0
        // (`fg == bg` reads as a conceal-shaped theme and suppresses the tint).
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style,
        color: 0x00d0_d0d0,
        accent: 0x0048_c9ff,
        duration: Duration::from_millis(650),
        length: usize::MAX,
        intensity: 1.0,
        audible: true,
        radius: 0.4,
        ring: true,
        beam: false,
        head_dx: 0.5,
    }
}

/// The HOT ribbon gate the frozen-clock fixture above cannot reach: `saturated`
/// re-ticks one Instant, so `dt == 0` never runs the momentum integrator,
/// `rainbow.disp` stays 0, and only the cheaper resting path is measured. This
/// 125-key/s saturation stress warms the clock 8 ms per event and keeps typing
/// through the measured window, pinning the capped tall-ribbon worst-case cost;
/// it cannot observe geometry growth after the deliberate budget edge. It is
/// deliberately harsher than a claim about ordinary human cadence.
///
/// The deterministic half: checks the amount of render work, not host timing;
/// the release-only timing probe (`benches/cursor_bench.rs`) is supplementary.
#[test]
fn cursor_rainbow_hot_ribbon_work_stays_below_its_normalized_share() {
    use aterm_effects::cursor_glow::{CursorGlow, Geom};
    use std::time::Instant;
    let config = config(GlowStyle::RainbowKitty);
    let geometry = Geom {
        cw: 18,
        ch: 40,
        rows: 26,
        cols: 190,
        origin_x: 0,
        origin_y: 0,
        win_w: 3420,
        win_h: 1040,
        head: 0,
    };
    let row = 12u16;
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let mut now = Instant::now();
    let mut col = 0u16;
    let mut dir = 1i32;
    glow.tick(Some((row, col)), now, &config, geometry, &mut quads);
    for _ in 0..320 {
        now += Duration::from_millis(8);
        if (dir > 0 && col as usize + 2 >= geometry.cols) || (dir < 0 && col == 0) {
            dir = -dir;
        }
        col = (col as i32 + dir) as u16;
        glow.note_synthetic_typed(now, 1);
        glow.tick(Some((row, col)), now, &config, geometry, &mut quads);
    }
    let under = glow.under_quads().len();
    let total = under + quads.len();
    println!(
        "hot rainbow normalized work: {under} under + {} over",
        quads.len()
    );
    assert!(
        under > 2_000,
        "fixture must reach the active tall ribbon ({under} quads)"
    );
    // The ribbon's declared share of `MAX_QUADS` (5/8), restated because a
    // `tests/` target cannot see `RAINBOW_RIBBON_QUAD_BUDGET`. At retina
    // metrics a full-width tall line costs ~77 quads per cell, so a smaller
    // share would shed the tail of long lines. The ribbon stays inside its
    // share; the frame-wide headroom assert below bounds the companions.
    assert!(
        under * 8 <= 16_384 * 5,
        "default tall ribbon spent more than its five-eighths normalized share: {under}"
    );
    assert!(
        total < 16_384,
        "active rim/ribbon frame must retain global quad headroom: {under} + {}",
        quads.len()
    );
}
