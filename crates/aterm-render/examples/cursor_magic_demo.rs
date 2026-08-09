// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pixel review for the live cursor engines.
//!
//! Unlike `cursor_glow_demo`, this drives `CursorGlow` itself through a hot
//! typing run, then renders three animation phases with the shipping CPU
//! compositor. Water and rainbow kitty get identity-layout panels; Fire and every
//! non-fire style also get a HEAD-BAND panel (pad 8 + head 48, row-0 run +
//! upward jump) so top-edge freedom is pinned by PNGs for each style. The
//! output is suitable for visual review and design diffs.
//!
//! ```text
//! cargo run -p aterm-render --example cursor_magic_demo -- /tmp/aterm-magic
//! ```

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::pipeline::EffectsPipeline;
use aterm_render::{Frame, RenderInput, Renderer, Theme};

const ROWS: usize = 9;
const COLS: usize = 68;

fn config(style: GlowStyle, theme: Theme) -> GlowConfig {
    GlowConfig {
        enabled: true,
        style,
        color: theme.cursor & 0x00ff_ffff,
        accent: 0x0048_c9ff,
        duration: Duration::from_millis(560),
        length: 48,
        intensity: 0.92,
        radius: 0.34,
        ring: false,
        dark_theme: true,
        // This helper already takes a `Theme` and derives `color` from it.
        theme_fg: theme.fg & 0x00ff_ffff,
        theme_bg: theme.bg & 0x00ff_ffff,
        beam: false,
        head_dx: 0.5,
        pack: None,
        wake_persist_s: aterm_effects::cursor_glow::RAINBOW_WAKE_PERSIST,
    }
}

fn terminal_at(head_col: u16) -> Terminal {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[2J\x1b[H");
    term.process(b"aterm  live output stays crisp while cursor magic moves\r\n");
    term.process(b"------------------------------------------------------------\r\n");
    term.process(b"fluid momentum:  build  test  inspect  refine\r\n");
    term.process(b"real terminal text remains readable beneath every effect\r\n");
    term.process(b"the cursor leads; the wake follows; idle returns to zero\r\n");
    term.process(format!("\x1b[4;{}H", head_col + 1).as_bytes());
    term
}

fn hot_run(style: GlowStyle, theme: Theme, cw: usize, ch: usize) -> (CursorGlow, Instant, u16) {
    let cfg = config(style, theme);
    // Identity layout: origin 0 + win == grid extents (no chrome head band).
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
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let mut now = Instant::now();
    let row = 3;
    glow.tick(Some((row, 8)), now, &cfg, geom, &mut quads);
    for col in 9..=54 {
        now += Duration::from_millis(22);
        glow.tick(Some((row, col)), now, &cfg, geom, &mut quads);
    }
    (glow, now, 54)
}

fn render_phases(
    renderer: &mut Renderer,
    theme: Theme,
    style: GlowStyle,
    name: &str,
    dir: &Path,
) -> Vec<Frame> {
    let (cw, ch) = renderer.cell_size();
    // Identity layout: origin 0 + win == grid extents (no chrome head band).
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
    let cfg = config(style, theme);
    let (mut glow, now, head_col) = hot_run(style, theme, cw, ch);
    let mut term = terminal_at(head_col);
    let mut phases = Vec::new();
    for (index, dt_ms) in [0_u64, 72, 144].into_iter().enumerate() {
        let mut quads = Vec::new();
        glow.tick(
            Some((3, head_col)),
            now + Duration::from_millis(dt_ms),
            &cfg,
            geom,
            &mut quads,
        );
        let mut input = term.cell_frame(ROWS, COLS);
        input.cursor_glow_add = quads;
        let frame = renderer.render_input(&input);
        let path = dir.join(format!("cursor_{name}_{index}.png"));
        fs::write(&path, frame.to_png()).expect("write cursor review PNG");
        println!("wrote {}", path.display());
        phases.push(frame);
    }
    phases
}

fn contact_sheet(frames: &[Frame], path: &Path) {
    let width = frames.iter().map(|f| f.width).sum();
    let height = frames.iter().map(|f| f.height).max().unwrap_or(0);
    let mut sheet = Frame {
        width,
        height,
        pixels: vec![0x0011_1318; width * height],
    };
    let mut ox = 0;
    for frame in frames {
        for y in 0..frame.height {
            let src = y * frame.width;
            let dst = y * width + ox;
            sheet.pixels[dst..dst + frame.width]
                .copy_from_slice(&frame.pixels[src..src + frame.width]);
        }
        ox += frame.width;
    }
    fs::write(path, sheet.to_png()).expect("write cursor contact sheet");
    println!("wrote {}", path.display());
}

fn render_literal_rain(renderer: &mut Renderer, theme: Theme, dir: &Path) {
    let (rows, cols) = (24_usize, 80_usize);
    let (cw, ch) = renderer.cell_size();
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[2J\x1b[H");
    term.process(b"OpenAI Codex v0.144.0\r\nClaude Code v2.1.206\r\n");
    term.process(b"REAL output material: Cc0{}[]<>?/\\+-=_ | build.test()\r\n");
    term.process(b"Only empty cells receive rain; nearby text keeps breathing room.\r\n");
    term.process(b"\x1b[24;1H> private composer draft\x1b[24;25H");

    let mut effects = EffectsPipeline::new();
    effects.set_matrix_rain(
        30,
        12,
        7,
        7,
        Some(108),
        Some(132),
        "matrix",
        None,
        133,
        30,
        false,
        true,
        true,
        true,
        0x00c0_ffee,
        theme.bg,
        theme.fg,
    );
    effects.set_matrix_rain_enabled(true);

    let mut input = RenderInput::default();
    // Drive enough real engine ticks to finish the atlas and establish a
    // representative deterministic field. No sleeps or wall-clock sampling.
    for _ in 0..18 {
        effects.advance(34.0);
        term.cell_frame_into(&mut input, rows, cols);
        effects.apply(&mut term, &mut input, cw, ch);
    }
    let frame = renderer.render_input(&input);
    let path = dir.join("matrix_literal_output.png");
    fs::write(&path, frame.to_png()).expect("write literal-rain review PNG");
    println!("wrote {}", path.display());
}

/// FIRE review panel — the owner's iteration loop for flame legibility and the
/// organic root. Renders a HOT top-row blaze over dense text with a real chrome
/// head band (pad 8 + head 48), copying every fire stream (field patches,
/// under-ink body, radial halos, charred ink, aurora quads) exactly as the GUI
/// host does — so what these PNGs show is what the terminal shows.
fn render_fire_band(renderer: &mut Renderer, theme: Theme, dir: &Path) -> Vec<Frame> {
    const PAD: usize = 8;
    const HEAD: usize = 48;
    renderer.set_pad(PAD);
    renderer.set_head(HEAD);
    let (cw, ch) = renderer.cell_size();
    let geom = Geom {
        cw,
        ch,
        rows: ROWS,
        cols: COLS,
        origin_x: PAD as u16,
        origin_y: (PAD + HEAD) as u16,
        win_w: (COLS * cw + 2 * PAD) as u16,
        win_h: (ROWS * ch + 2 * PAD + HEAD) as u16,
        head: HEAD as u16,
    };
    let cfg = config(GlowStyle::Fire, theme);
    // Dense text on the TOP row (the owner's screenshot case), cursor at its end.
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[2J\x1b[H");
    term.process(b"fadsfadsfadsfadsfadsfadsfadsfadsfadsfadsfadsfadsfads");
    // Hot run ALONG row 0 so the blaze towers over the glyphs.
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let mut now = Instant::now();
    glow.tick(Some((0, 8)), now, &cfg, geom, &mut quads);
    for col in 9..=52 {
        now += Duration::from_millis(22);
        glow.tick(Some((0, col)), now, &cfg, geom, &mut quads);
    }
    let mut phases = Vec::new();
    for (index, dt_ms) in [0_u64, 72, 144].into_iter().enumerate() {
        let mut quads = Vec::new();
        glow.tick(
            Some((0, 52)),
            now + Duration::from_millis(dt_ms),
            &cfg,
            geom,
            &mut quads,
        );
        let mut input = term.cell_frame(ROWS, COLS);
        input.cursor_glow_add = quads;
        input.glow_halo = glow.halos().to_vec();
        input.fire_patch = glow.patches().to_vec();
        input.glow_under = glow.under_quads().to_vec();
        input.char_fg = glow.charred().to_vec();
        input.fire_halo = glow.halo_cells().to_vec();
        let frame = renderer.render_input(&input);
        let path = dir.join(format!("cursor_fire_{index}.png"));
        fs::write(&path, frame.to_png()).expect("write fire review PNG");
        println!("wrote {}", path.display());
        phases.push(frame);
    }
    renderer.set_pad(0);
    renderer.set_head(0);
    phases
}

/// HEAD-BAND review panel for EVERY non-fire style — the parameterized twin of
/// [`render_fire_band`]: real chrome head band (pad 8 + head 48), dense text on
/// the top row, a hot run ALONG row 0 (so upward-flying pixels — rainbow kitty stars,
/// water spray, comet debris — cross into the chrome band and clamp at the
/// effects-box top, not the grid top), then a drop to a lower row and a >=2-cell
/// upward JUMP landing back on row 0 (arming the jump beam / splash ring /
/// meteor against the band). The captured phases re-tick AFTER the jump, so the
/// jump streak materializes in flight. Copies every window-space stream exactly
/// as the GUI host does unconditionally for every style — streams a style does
/// not emit stay empty, and `charred` stays empty under the no-recolor law —
/// so what these PNGs show is what the terminal shows.
fn render_band(
    renderer: &mut Renderer,
    theme: Theme,
    style: GlowStyle,
    name: &str,
    dir: &Path,
) -> Vec<Frame> {
    const PAD: usize = 8;
    const HEAD: usize = 48;
    renderer.set_pad(PAD);
    renderer.set_head(HEAD);
    let (cw, ch) = renderer.cell_size();
    let geom = Geom {
        cw,
        ch,
        rows: ROWS,
        cols: COLS,
        origin_x: PAD as u16,
        origin_y: (PAD + HEAD) as u16,
        win_w: (COLS * cw + 2 * PAD) as u16,
        win_h: (ROWS * ch + 2 * PAD + HEAD) as u16,
        head: HEAD as u16,
    };
    let cfg = config(style, theme);
    // Dense text on the TOP row, so the band effects play over real glyphs.
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[2J\x1b[H");
    term.process(b"fadsfadsfadsfadsfadsfadsfadsfadsfadsfadsfadsfadsfads");
    // Hot run ALONG row 0 so the wake hugs the head band.
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let mut now = Instant::now();
    glow.tick(Some((0, 8)), now, &cfg, geom, &mut quads);
    for col in 9..=44 {
        now += Duration::from_millis(22);
        glow.tick(Some((0, col)), now, &cfg, geom, &mut quads);
    }
    // Establish the cursor on a lower row mid-run, then JUMP >=2 cells back UP
    // onto row 0 — the splash-ring / jump-beam arming move (dist >= 2), landing
    // against the head band.
    now += Duration::from_millis(22);
    glow.tick(Some((4, 48)), now, &cfg, geom, &mut quads);
    now += Duration::from_millis(22);
    glow.tick(Some((0, 52)), now, &cfg, geom, &mut quads);
    let mut phases = Vec::new();
    for (index, dt_ms) in [0_u64, 72, 144].into_iter().enumerate() {
        let mut quads = Vec::new();
        glow.tick(
            Some((0, 52)),
            now + Duration::from_millis(dt_ms),
            &cfg,
            geom,
            &mut quads,
        );
        let mut input = term.cell_frame(ROWS, COLS);
        input.cursor_glow_add = quads;
        input.glow_halo = glow.halos().to_vec();
        input.fire_patch = glow.patches().to_vec();
        input.glow_under = glow.under_quads().to_vec();
        input.char_fg = glow.charred().to_vec();
        input.fire_halo = glow.halo_cells().to_vec();
        let frame = renderer.render_input(&input);
        let path = dir.join(format!("cursor_{name}_band_{index}.png"));
        fs::write(&path, frame.to_png()).expect("write band review PNG");
        println!("wrote {}", path.display());
        phases.push(frame);
    }
    renderer.set_pad(0);
    renderer.set_head(0);
    phases
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/aterm-magic".to_owned());
    let dir = Path::new(&dir);
    fs::create_dir_all(dir).expect("create output directory");

    let theme = Theme::default();
    let mut renderer = Renderer::from_system(22.0, theme).expect("system monospace font");
    let water = render_phases(&mut renderer, theme, GlowStyle::Water, "water", dir);
    contact_sheet(&water, &dir.join("cursor_water_contact.png"));

    // The artifact names stay `nyan` on purpose: they are the demo's on-disk
    // output filenames, and existing capture scripts glob for them.
    let rainbow_kitty = render_phases(&mut renderer, theme, GlowStyle::RainbowKitty, "nyan", dir);
    contact_sheet(&rainbow_kitty, &dir.join("cursor_nyan_contact.png"));

    let fire = render_fire_band(&mut renderer, theme, dir);
    contact_sheet(&fire, &dir.join("cursor_fire_contact.png"));

    // Head-band panels for every non-fire style: row-0 run + upward jump
    // against a real chrome band, one contact sheet per style.
    for (style, name) in [
        (GlowStyle::RainbowKitty, "nyan"),
        (GlowStyle::Water, "water"),
        (GlowStyle::Comet, "comet"),
        (GlowStyle::Lumen, "lumen"),
        (GlowStyle::Phaser, "phaser"),
        (GlowStyle::Laser, "laser"),
        (GlowStyle::Beam, "beam"),
        (GlowStyle::Sparkle, "sparkle"),
    ] {
        let band = render_band(&mut renderer, theme, style, name, dir);
        contact_sheet(&band, &dir.join(format!("cursor_{name}_band_contact.png")));
    }

    render_literal_rain(&mut renderer, theme, dir);
}
