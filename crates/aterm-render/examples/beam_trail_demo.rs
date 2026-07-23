// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pixel review for the BEAM trail — the space scene: a steady photon TUBE
//! (white-hot core, indigo nebula sleeve) behind the cursor, weightless
//! twinkling STARDUST hanging in the wake, the full-vector rod a jump lays,
//! and the signature POWER-DOWN (the tube dims and thins toward a hairline in
//! one motion). Also frames the thin-BAR anchor: with `head_dx` at the bar's
//! x the streak noses into the bar instead of overshooting it.
//!
//! ```text
//! cargo run -p aterm-render --example beam_trail_demo -- /tmp/aterm-beam-trail
//! ```

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_effects::cursor_glow::{BEAM_DEFAULT_COLOR, CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_render::{Frame, Renderer, Theme};

const ROWS: usize = 9;
const COLS: usize = 68;

fn cfg() -> GlowConfig {
    GlowConfig {
        enabled: true,
        style: GlowStyle::Beam,
        color: BEAM_DEFAULT_COLOR,
        accent: BEAM_DEFAULT_COLOR,
        duration: Duration::from_millis(260),
        length: 24,
        intensity: 0.9,
        // The beam ships bloom-free from the host: no crown, no ring.
        radius: 0.0,
        ring: false,
        beam: true,
        head_dx: 0.5,
        pack: None,
        dark_theme: true,
    }
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/aterm-beam-trail".to_owned());
    let dir = Path::new(&dir);
    fs::create_dir_all(dir).expect("create output directory");

    let theme = Theme::default();
    let mut renderer = Renderer::from_system(22.0, theme).expect("system monospace font");
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

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[2J\x1b[H");
    term.process(b"aterm  the beam trail: a photon tube through space\r\n");
    term.process(b"------------------------------------------------------------\r\n");
    term.process(b"white-hot core, indigo nebula sleeve, weightless stardust\r\n");
    term.process(b"a jump lays one solid rod; the light POWERS DOWN, thinning\r\n");
    term.process(b"\x1b[6;5Hthe quick brown fox jumps over the lazy dog\x1b[6;5H");

    let c = cfg();
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let mut now = Instant::now();
    let row = 5u16;
    let mut col = 4u16;
    glow.tick(Some((row, col)), now, &c, geom, &mut quads);
    let mut frames = Vec::new();
    let shoot = |renderer: &mut Renderer,
                 quads: &Vec<aterm_render::GlowQuad>,
                 term: &mut Terminal,
                 cursor: (u16, u16),
                 name: &str,
                 frames: &mut Vec<Frame>| {
        term.process(format!("\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1).as_bytes());
        let mut input = term.cell_frame(ROWS, COLS);
        input.cursor_glow_add = quads.clone();
        let frame = renderer.render_input(&input);
        let path = dir.join(format!("beam_trail_{name}.png"));
        fs::write(&path, frame.to_png()).expect("write trail review PNG");
        println!("wrote {}", path.display());
        frames.push(frame);
    };

    // A hot typing run: 30 keys at ~48ms cadence along row 5, with 16ms frame
    // ticks between advances — the tube chained behind the cursor + stardust.
    for key in 1..=30u16 {
        for _ in 0..3 {
            now += Duration::from_millis(16);
            glow.tick(Some((row, col)), now, &c, geom, &mut quads);
        }
        col += 1;
        glow.tick(Some((row, col)), now, &c, geom, &mut quads);
        if key == 15 {
            shoot(
                &mut renderer,
                &quads,
                &mut term,
                (row, col),
                "mid_run",
                &mut frames,
            );
        }
        if key == 30 {
            shoot(
                &mut renderer,
                &quads,
                &mut term,
                (row, col),
                "run_end",
                &mut frames,
            );
        }
    }

    // POWER-DOWN: the tube dims and thins toward a hairline; stardust lingers
    // a beat longer, then space goes dark.
    let stop = now;
    let phases: [(u64, &str); 4] = [
        (150, "powerdown_150ms"),
        (300, "powerdown_300ms"),
        (600, "stardust_600ms"),
        (1600, "gone_1600ms"),
    ];
    let mut t = stop;
    for (ms, name) in phases {
        let target = stop + Duration::from_millis(ms);
        while t < target {
            t = (t + Duration::from_millis(16)).min(target);
            let mut q = Vec::new();
            glow.tick(Some((row, col)), t, &c, geom, &mut q);
            quads = q;
        }
        shoot(
            &mut renderer,
            &quads,
            &mut term,
            (row, col),
            name,
            &mut frames,
        );
    }

    // A JUMP: one solid full-vector rod across the leap, then ITS power-down.
    let mut glow = CursorGlow::default();
    let mut now = t;
    glow.tick(Some((2, 60)), now, &c, geom, &mut quads);
    now += Duration::from_millis(40);
    glow.tick(Some((7, 8)), now, &c, geom, &mut quads);
    shoot(
        &mut renderer,
        &quads,
        &mut term,
        (7, 8),
        "jump_rod",
        &mut frames,
    );
    let stop = now;
    for (ms, name) in [
        (140u64, "jump_powerdown_140ms"),
        (230, "jump_thinning_230ms"),
    ] {
        let target = stop + Duration::from_millis(ms);
        let mut t = now;
        while t < target {
            t = (t + Duration::from_millis(16)).min(target);
            let mut q = Vec::new();
            glow.tick(Some((7, 8)), t, &c, geom, &mut q);
            quads = q;
        }
        now = t;
        shoot(&mut renderer, &quads, &mut term, (7, 8), name, &mut frames);
    }

    // The thin-BAR anchor: same typing run, but the live head bridges to the
    // bar's own x (`head_dx` 0.08) and the renderer draws the bar shape — the
    // streak noses INTO the bar (validating the light-leaves-the-cursor fix).
    renderer.set_cursor_style_override(Some(CursorStyle::SteadyBar));
    let mut bar_cfg = cfg();
    bar_cfg.head_dx = 0.08;
    let mut glow = CursorGlow::default();
    let mut now = now + Duration::from_millis(50);
    let mut col = 10u16;
    glow.tick(Some((3, col)), now, &bar_cfg, geom, &mut quads);
    for _ in 1..=8u16 {
        for _ in 0..3 {
            now += Duration::from_millis(16);
            glow.tick(Some((3, col)), now, &bar_cfg, geom, &mut quads);
        }
        col += 1;
        glow.tick(Some((3, col)), now, &bar_cfg, geom, &mut quads);
    }
    shoot(
        &mut renderer,
        &quads,
        &mut term,
        (3, col),
        "bar_anchor",
        &mut frames,
    );

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
    let path = dir.join("beam_trail_sheet.png");
    fs::write(&path, sheet.to_png()).expect("write contact sheet");
    println!("wrote {}", path.display());
}
