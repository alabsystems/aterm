// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::Geom;
use aterm_effects::nova::MAX_NOVA_QUADS;
use aterm_effects::output_streak::{MAX_COMETS, OutputStreak, StreakConfig, TAIL_MAX};
use aterm_render::{GlowQuad, Renderer, Theme, premul_rgb};
use aterm_time::Instant;
use std::time::Duration;

fn geom(cw: usize, ch: usize, cols: usize, rows: usize) -> Geom {
    Geom {
        cw,
        ch,
        rows,
        cols,
        origin_x: 0,
        origin_y: 0,
        win_w: (cols * cw) as u16,
        win_h: (rows * ch) as u16,
        head: 0,
    }
}

fn cfg() -> StreakConfig {
    StreakConfig {
        enabled: true,
        intensity: 1.0,
        tail: TAIL_MAX,
        max_streaks: MAX_COMETS as u8,
        idle_secs: 10.0,
        dark_theme: true,
        theme_bg: 0x001A_1B26,
        theme_fg: 0x00C0_CAF5,
        theme_cursor: 0x0050_FA7B,
        sound: false,
    }
}

fn drive(seed: u64, g: Geom, frames: u64, out: &mut Vec<GlowQuad>) -> usize {
    let mut e = OutputStreak::new(seed);
    let c = cfg();
    let t0 = Instant::now();
    let mut peak = 0usize;
    for i in 0..frames {
        let now = t0 + Duration::from_millis(i * 8);
        let row = (i % g.rows as u64) as u16;
        e.note_output(i + 1, &[(row, 0, (g.cols - 1) as u16)], now, false);
        out.clear();
        e.tick(now, g, &c, out);
        peak = peak.max(out.len());
    }
    peak
}

#[test]
fn the_streak_share_is_tail_bounded_not_column_bounded() {
    for cols in [80usize, 120, 200, 400] {
        let rows = 50usize;
        let g = geom(8, 17, cols, rows);
        let mut out = Vec::new();
        let peak = drive(7, g, 960, &mut out);
        let structural = cols + MAX_COMETS * (usize::from(TAIL_MAX) + 2);
        assert!(
            peak <= structural,
            "cols={cols}: peak {peak} must sit under the ribbon+heads bound {structural}"
        );
        assert!(
            peak >= cols,
            "cols={cols}: peak {peak} — the flood ribbon never opened, the bound is vacuous"
        );
    }
}

#[test]
fn the_over_budget_channel_is_drawn_in_full_not_truncated() {
    let mut rend = Renderer::from_bytes(
        include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf"),
        18.0,
        Theme::default(),
    )
    .expect("the bundled font");
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (24usize, 120usize);
    let g = geom(cw, ch, cols, rows);

    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lcompiling aterm-effects v0.69.0");

    let mut channel: Vec<GlowQuad> = (0..MAX_NOVA_QUADS)
        .map(|_| GlowQuad {
            row: 0,
            x: 0,
            y: 0,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(0x0040_4040, 8),
            alpha: 0,
            color2: premul_rgb(0x0040_4040, 8),
            alpha2: 0,
        })
        .collect();
    assert_eq!(
        channel.len(),
        MAX_NOVA_QUADS,
        "the funded share is at its cap"
    );

    let mut pane = Vec::new();
    for p in 0..4u64 {
        drive(0xA11CE5 ^ p, g, 600, &mut pane);
        channel.extend_from_slice(&pane);
    }
    assert!(
        channel.len() > MAX_NOVA_QUADS,
        "premise: {} quads is over the backstop",
        channel.len()
    );

    let mut full = term.cell_frame(rows, cols);
    full.nova_add.clone_from(&channel);
    let full_px = rend.render_input(&full).pixels.clone();

    let mut clipped = term.cell_frame(rows, cols);
    clipped.nova_add = channel[..MAX_NOVA_QUADS].to_vec();
    let clipped_px = rend.render_input(&clipped).pixels.clone();

    assert_ne!(
        full_px, clipped_px,
        "the quads past MAX_NOVA_QUADS painted nothing — the draw side truncates after all"
    );
}
