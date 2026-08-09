// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pixel review for the LIGHTNING TRAIL: a hot typing run with the `laser`
//! style leaves a CHARGED trail — full beam power at the freshly typed head,
//! discharging cell by cell to a dim flickering residual that lingers (and
//! crackles) behind the cursor before draining to nothing.
//!
//! ```text
//! cargo run -p aterm-render --example laser_trail_demo -- /tmp/aterm-laser-trail
//! ```

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle, LASER_DEFAULT_COLOR};
use aterm_render::{Frame, Renderer, Theme};

const ROWS: usize = 9;
const COLS: usize = 68;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/aterm-laser-trail".to_owned());
    let dir = Path::new(&dir);
    fs::create_dir_all(dir).expect("create output directory");

    let theme = Theme::default();
    let mut renderer = Renderer::from_system(22.0, theme).expect("system monospace font");
    // The laser cursor IS the lightning: the frontend forces the bolt shape
    // (and paints it in the beam's hue via the fill override below).
    renderer.set_cursor_style_override(Some(CursorStyle::Bolt));
    let (cw, ch) = renderer.cell_size();
    let geom = Geom {
        cw,
        ch,
        rows: ROWS,
        cols: COLS,
        origin_x: 0,
        origin_y: 0,
        win_w: (COLS * cw) as u16,
        win_h: (ROWS * ch) as u16,
        head: 0,
    };
    let cfg = GlowConfig {
        enabled: true,
        style: GlowStyle::Laser,
        color: LASER_DEFAULT_COLOR,
        accent: LASER_DEFAULT_COLOR,
        // A long configured fade — the charged trail honours it (the old
        // generic typing wake capped out at ~0.25s regardless).
        duration: Duration::from_millis(900),
        length: 18,
        intensity: 0.9,
        radius: 0.6,
        ring: true,
        beam: true,
        head_dx: 0.5,
        pack: None,
        wake_persist_s: aterm_effects::cursor_glow::RAINBOW_WAKE_PERSIST,
        dark_theme: true,
        // `theme` is in scope here, so render the TRUTH rather than a
        // stand-in: these demos exist to be looked at.
        theme_fg: theme.fg & 0x00ff_ffff,
        theme_bg: theme.bg & 0x00ff_ffff,
    };

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[2J\x1b[H");
    term.process(b"aterm  the lightning charged trail\r\n");
    term.process(b"------------------------------------------------------------\r\n");
    term.process(b"a hot typing run: bright at the head, residual charge\r\n");
    term.process(b"lingering and crackling down the trail, then draining:\r\n");
    term.process(b"\x1b[6;5Hthe quick brown fox jumps over the lazy dog\x1b[6;5H");

    // A hot run: 30 keys at ~48ms cadence along row 5 (0-based), driven like
    // the real app with intermediate 16ms frame ticks between key advances.
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let mut now = Instant::now();
    let row = 5u16;
    let mut col = 4u16;
    glow.tick(Some((row, col)), now, &cfg, geom, &mut quads);
    let mut frames = Vec::new();
    let mut shoot = |quads: &Vec<aterm_render::GlowQuad>,
                     term: &mut Terminal,
                     cursor: (u16, u16),
                     name: &str,
                     frames: &mut Vec<Frame>| {
        // Park the terminal cursor where the glow head is so the bolt rides it.
        term.process(format!("\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1).as_bytes());
        let mut input = term.cell_frame(ROWS, COLS);
        input.cursor_glow_add = quads.clone();
        input.cursor_fill_override = Some(LASER_DEFAULT_COLOR);
        let frame = renderer.render_input(&input);
        let path = dir.join(format!("laser_trail_{name}.png"));
        fs::write(&path, frame.to_png()).expect("write trail review PNG");
        println!("wrote {}", path.display());
        frames.push(frame);
    };
    for key in 1..=30u16 {
        for _ in 0..3 {
            now += Duration::from_millis(16);
            glow.tick(Some((row, col)), now, &cfg, geom, &mut quads);
        }
        col += 1;
        glow.tick(Some((row, col)), now, &cfg, geom, &mut quads);
        if key == 15 {
            shoot(&quads, &mut term, (row, col), "mid_run", &mut frames);
        }
        if key == 30 {
            shoot(&quads, &mut term, (row, col), "run_end", &mut frames);
        }
    }

    // After the run stops: the charged trail lingers as a dim residual,
    // then drains to exactly nothing.
    let stop = now;
    let phases: [(u64, &str); 4] = [
        (250, "residual_250ms"),
        (500, "residual_500ms"),
        (900, "draining_900ms"),
        (1600, "gone_1600ms"),
    ];
    let mut t = stop;
    for (ms, name) in phases {
        let target = stop + Duration::from_millis(ms);
        while t < target {
            t = (t + Duration::from_millis(16)).min(target);
            let mut q = Vec::new();
            glow.tick(Some((row, col)), t, &cfg, geom, &mut q);
            quads = q;
        }
        shoot(&quads, &mut term, (row, col), name, &mut frames);
    }

    // Vertical contact sheet: the whole arc top to bottom.
    let width = frames.iter().map(|f| f.width).max().unwrap_or(0);
    let height: usize = frames.iter().map(|f| f.height).sum();
    let mut sheet = Frame {
        width,
        height,
        pixels: vec![0x0011_1318; width * height],
    };
    let mut oy = 0;
    for frame in &frames {
        for y in 0..frame.height {
            let src = y * frame.width;
            let dst = (oy + y) * width;
            sheet.pixels[dst..dst + frame.width]
                .copy_from_slice(&frame.pixels[src..src + frame.width]);
        }
        oy += frame.height;
    }
    let path = dir.join("laser_trail_arc.png");
    fs::write(&path, sheet.to_png()).expect("write trail contact sheet");
    println!("wrote {}", path.display());
}
