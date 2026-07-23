// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// PHOSPHOR digital-rain visual capture. Not a parity gate — an #[ignore]d
// harness that drives the REAL `MatrixRain` engine (aterm-effects) over a
// realistic agent-CLI screen snapshot and renders it through the REAL CPU
// rasterizer (`aterm_render::Renderer`), then dumps the framebuffer to a PNG.
// This is the "run it and screenshot the rain" path: rain is deliberately
// excluded from the control-socket capture surface and drains when a window is
// unfocused, so a headless render of the exact host channels
// (`rain_quads`/`rain_atlas`/`rain_add`) is the reliable way to SEE it.
//
// Two host layouts — Claude Code and Codex — prove the design's premise: rain
// fills the empty field while EVERY line of the agent's UI stays legible. That
// holds for two reasons, both exercised here against the REAL engine:
//   * the two-tier mask keeps rain out of occupied cells AND out of the bottom
//     input pane (the host feeds `hidden_band` = the last K damaged rows;
//     reproduced here so the composer stays rain-free exactly as in production);
//   * the contrast invariant (design §6) caps rain alpha (`RAIN_ALPHA_CAP=135`)
//     so rain is provably dimmer than the dimmest text — inter-word rain in the
//     transcript can never outshine a word.
//
//   PHOSPHOR_SCENE=claude PHOSPHOR_RAIN_PNG=/path/out.png \
//     cargo test -p aterm-gpu --test rain_screenshot -- --ignored --nocapture
//   PHOSPHOR_SCENE=codex  PHOSPHOR_RAIN_PNG=/path/out.png  ...

use aterm_core::terminal::{RenderCell, Terminal};
use aterm_effects::matrix_rain::{EffectGeom, MatrixRain, RainConfig, RainTickInput};
use aterm_render::{Renderer, Theme};

/// Dilate the occupancy scan by one column around every text run: mark the
/// empty default-bg cell immediately flanking a glyph as ineligible (sentinel
/// bg) so a rain glyph never lands directly beside a word. Computed from the
/// original glyph mask (single-pass, non-cascading).
fn dilate_text_gutter(cells: &mut [Vec<RenderCell>], default_bg: [u8; 3]) {
    for row in cells.iter_mut() {
        let glyph: Vec<bool> = row
            .iter()
            .map(|c| c.ch != ' ' || c.bg != default_bg)
            .collect();
        let n = row.len();
        for i in 0..n {
            if !glyph[i] {
                continue;
            }
            for &j in &[i.wrapping_sub(1), i + 1] {
                if j < n && !glyph[j] && row[j].ch == ' ' && row[j].bg == default_bg {
                    row[j].bg = [1, 1, 1]; // != default_bg ⇒ rescan skips it
                }
            }
        }
    }
}

/// Deterministic replay seed (shared with the parity fixtures so a failure here
/// reproduces the same field).
const RAIN_SEED: u64 = 0xA7E2_11D3;
/// CALM engine tick period (12 Hz weather gate) — one engine tick per loop.
const CALM_TICK_MS: u64 = 83;
/// Host `hidden_band` height: the bottom input pane the design masks (Claude's
/// inline box / Codex's composer) — `HIDDEN_CURSOR_BAND_ROWS`.
const INPUT_PANE_ROWS: usize = 5;

// ---- ANSI compositing helpers ------------------------------------------------

fn cup(s: &mut String, row: usize, col: usize) {
    s.push_str(&format!("\x1b[{row};{col}H"));
}
fn fg(s: &mut String, c: (u8, u8, u8)) {
    s.push_str(&format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2));
}
fn bg(s: &mut String, c: (u8, u8, u8)) {
    s.push_str(&format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2));
}
fn hrule(s: &mut String, n: usize) {
    for _ in 0..n {
        s.push('\u{2500}');
    }
}
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

const WHITE: (u8, u8, u8) = (228, 228, 232);
const DIM: (u8, u8, u8) = (120, 126, 142);
const GREEN: (u8, u8, u8) = (80, 250, 123);
const CYAN: (u8, u8, u8) = (86, 190, 202);

// ---- Claude Code layout ------------------------------------------------------

fn compose_claude_code(rows: usize, cols: usize) -> String {
    let mut s = String::new();
    s.push_str("\x1b[?25l\x1b[2J");
    let (bf, bt) = (3usize, cols - 3);

    cup(&mut s, 2, 3);
    fg(&mut s, (200, 120, 220));
    s.push('\u{273b}'); // ✻
    fg(&mut s, (190, 190, 200));
    s.push_str(" Welcome to Claude Code");
    fg(&mut s, DIM);
    s.push_str("  \u{2014}  aterm \u{00b7} PHOSPHOR digital-rain");

    cup(&mut s, 4, 3);
    fg(&mut s, WHITE);
    s.push_str("> make the matrix rain fucking awesome");

    cup(&mut s, 6, 3);
    fg(&mut s, GREEN);
    s.push('\u{25cf}'); // ●
    fg(&mut s, (214, 214, 220));
    s.push_str(" Driving the real MatrixRain engine headless and rendering");
    cup(&mut s, 7, 5);
    fg(&mut s, (214, 214, 220));
    s.push_str("it through the CPU rasterizer \u{2014} the same quads the GUI feeds.");

    cup(&mut s, 9, 3);
    fg(&mut s, GREEN);
    s.push('\u{25cf}');
    fg(&mut s, CYAN);
    s.push_str(" Bash");
    fg(&mut s, DIM);
    s.push_str("(cargo test --test rain_screenshot -- --ignored)");
    cup(&mut s, 10, 5);
    fg(&mut s, DIM);
    s.push_str("\u{23bf}  wrote phosphor-rain.png \u{2014} the frame you're looking at");

    // Rounded prompt box pinned to the bottom pane (the masked band).
    let top = rows - 4;
    let inner = bt.saturating_sub(bf).saturating_sub(1);
    cup(&mut s, top, bf);
    fg(&mut s, (70, 120, 90));
    s.push('\u{256d}');
    hrule(&mut s, inner);
    s.push('\u{256e}');
    cup(&mut s, top + 1, bf);
    s.push('\u{2502}');
    cup(&mut s, top + 1, bf + 2);
    fg(&mut s, WHITE);
    s.push_str("> ");
    fg(&mut s, GREEN);
    s.push('\u{2588}'); // caret
    cup(&mut s, top + 1, bt);
    fg(&mut s, (70, 120, 90));
    s.push('\u{2502}');
    cup(&mut s, top + 2, bf);
    s.push('\u{2570}');
    hrule(&mut s, inner);
    s.push('\u{256f}');
    cup(&mut s, top + 3, bf + 1);
    fg(&mut s, DIM);
    s.push_str("? for shortcuts");
    s.push_str(RESET);
    s
}

// ---- Codex CLI layout (codex-rs tui, current borderless composer) ------------

fn compose_codex(rows: usize, cols: usize) -> String {
    let mut s = String::new();
    s.push_str("\x1b[?25l\x1b[2J");

    // Startup header card: rounded dim border, NO interior fill — real codex
    // draws it as a dim outline on the default bg, so aterm's rain streams
    // honestly behind the empty interior (masked only from the border + text,
    // each of which gets a 1-cell gutter).
    let (cl, cr) = (3usize, 3 + 1 + 56 + 1);
    let inner = cr - cl - 1;
    cup(&mut s, 2, cl);
    fg(&mut s, DIM);
    s.push('\u{256d}');
    hrule(&mut s, inner);
    s.push('\u{256e}');
    for row in 3..=6 {
        cup(&mut s, row, cl);
        fg(&mut s, DIM);
        s.push('\u{2502}');
        cup(&mut s, row, cr);
        s.push('\u{2502}');
    }
    cup(&mut s, 7, cl);
    fg(&mut s, DIM);
    s.push('\u{2570}');
    hrule(&mut s, inner);
    s.push('\u{256f}');
    // Title / model / directory, on the black interior.
    cup(&mut s, 3, cl + 2);
    fg(&mut s, DIM);
    s.push_str("\u{003e}_ ");
    s.push_str(BOLD);
    fg(&mut s, (222, 222, 228));
    s.push_str("OpenAI Codex");
    s.push_str("\x1b[22m");
    fg(&mut s, DIM);
    s.push_str(" (v0.20.0)");
    cup(&mut s, 5, cl + 2);
    fg(&mut s, DIM);
    s.push_str("model:     ");
    fg(&mut s, (214, 214, 220));
    s.push_str("gpt-5.1-codex");
    fg(&mut s, DIM);
    s.push_str("   ");
    fg(&mut s, CYAN);
    s.push_str("/model");
    fg(&mut s, DIM);
    s.push_str(" to change");
    cup(&mut s, 6, cl + 2);
    fg(&mut s, DIM);
    s.push_str("directory: ");
    fg(&mut s, (214, 214, 220));
    s.push_str("~/aterm");
    s.push_str(RESET);

    // User message: full-width lighter tint band with a blank tinted line above
    // and below (codex `UserHistoryCell`), '›' bold in the default fg.
    let ubg = (30, 31, 35);
    for r in 8..=10 {
        cup(&mut s, r, 1);
        bg(&mut s, ubg);
        for _ in 0..cols {
            s.push(' ');
        }
    }
    cup(&mut s, 9, 3);
    bg(&mut s, ubg);
    s.push_str(BOLD);
    fg(&mut s, (170, 174, 186));
    s.push_str("\u{203a} "); // ›
    s.push_str("\x1b[22m");
    fg(&mut s, WHITE);
    s.push_str("make the matrix rain fucking awesome");
    s.push_str(RESET);

    // Assistant answer: '•' dim bullet, default-fg body.
    cup(&mut s, 12, 3);
    fg(&mut s, DIM);
    s.push_str("\u{2022} "); // •
    fg(&mut s, (214, 214, 220));
    s.push_str("I'll drive the real rain engine headless and render every");
    cup(&mut s, 13, 5);
    fg(&mut s, (214, 214, 220));
    s.push_str("host cell so the field stays legible under the cascade.");

    // Explored: dim bullet, BOLD bright label, cyan action titles, default paths.
    cup(&mut s, 15, 3);
    fg(&mut s, DIM);
    s.push_str("\u{2022} ");
    s.push_str(BOLD);
    fg(&mut s, (222, 222, 228));
    s.push_str("Explored");
    s.push_str("\x1b[22m");
    cup(&mut s, 16, 5);
    fg(&mut s, DIM);
    s.push_str("\u{2514} ");
    fg(&mut s, CYAN);
    s.push_str("List");
    fg(&mut s, (206, 210, 218));
    s.push_str(" crates/aterm-effects/src/matrix_rain");
    cup(&mut s, 17, 7);
    fg(&mut s, CYAN);
    s.push_str("Read");
    fg(&mut s, (206, 210, 218));
    s.push_str(" field.rs");

    // Exec: '•' green+bold, 'Ran' bold default, command near-white.
    cup(&mut s, 19, 3);
    fg(&mut s, GREEN);
    s.push_str(BOLD);
    s.push_str("\u{2022} ");
    fg(&mut s, (226, 226, 230));
    s.push_str("Ran");
    s.push_str("\x1b[22m");
    fg(&mut s, (198, 204, 214));
    s.push_str(" cargo test --test rain_screenshot -- --ignored");
    cup(&mut s, 20, 5);
    fg(&mut s, DIM);
    s.push_str("\u{2514} wrote phosphor-rain.png (");
    fg(&mut s, (150, 220, 130));
    s.push_str("529 quads");
    fg(&mut s, DIM);
    s.push_str(", 30 heads)");

    // Turn separator (full-width dim rule with a Worked-for label).
    cup(&mut s, 22, 3);
    fg(&mut s, DIM);
    s.push_str("\u{2500} Worked for 1m 12s ");
    hrule(&mut s, cols.saturating_sub(24));

    cup(&mut s, 24, 3);
    fg(&mut s, DIM);
    s.push_str("\u{2022} ");
    fg(&mut s, (214, 214, 220));
    s.push_str("Here's the capture \u{2014} pure-black field, bright-head halos,");
    cup(&mut s, 25, 5);
    fg(&mut s, (214, 214, 220));
    s.push_str("every line readable.");

    // Bottom pane (masked band): status + borderless composer + footer.
    let base = rows - 4;
    cup(&mut s, base, 3);
    fg(&mut s, (214, 214, 220)); // shimmering '•' → static bright approximation
    s.push_str("\u{2022} ");
    s.push_str(BOLD);
    fg(&mut s, (222, 222, 228));
    s.push_str("Working");
    s.push_str("\x1b[22m");
    fg(&mut s, DIM);
    s.push_str(" (12s \u{00b7} esc to interrupt)");
    cup(&mut s, base + 1, 3);
    s.push_str(BOLD);
    fg(&mut s, WHITE);
    s.push_str("\u{203a} "); // › — default fg, not cyan
    s.push_str("\x1b[22m");
    fg(&mut s, DIM);
    s.push_str("Ask Codex to do anything");
    cup(&mut s, base + 2, 3);
    fg(&mut s, DIM);
    s.push_str("  ? for shortcuts");
    cup(&mut s, base + 2, cols - 18);
    s.push_str("100% context left");
    s.push_str(RESET);
    s
}

// ---- capture core ------------------------------------------------------------

/// Drive the real engine to its most striking frame within a tick window and
/// PNG it. Returns `(quads, halos)` of the chosen frame.
fn capture(scene: &str, out_path: &str) -> (usize, usize) {
    // Pure-black backdrop, unified across the canvas clear, absent cells, and
    // present default-bg cells (the terminal's own DEFAULT_BACKGROUND is
    // 0x000000). Matching `theme.bg` to it removes the render's tonal seam and
    // gives rain the canonical Matrix black; the green ramp still reads at full
    // contrast against black.
    let theme = Theme {
        bg: 0x0000_0000,
        ..Theme::default()
    };
    let mut cpu = Renderer::from_system(20.0, theme).expect("system monospace font");

    let (rows, cols) = (46usize, 132usize);
    let content = match scene {
        "codex" => compose_codex(rows, cols),
        _ => compose_claude_code(rows, cols),
    };
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(content.as_bytes());
    let base_input = term.cell_frame(rows, cols);

    // Real engine, max density, deterministic seed. Punch knobs (adversarial
    // impact audit): white-hot heads at the §6 cap over a dimmer body maximize
    // head/trail contrast, and a full-length trail runs the 16-level ramp deep
    // into near-black — the iconic cascade, still provably legible (head 135 ==
    // RAIN_ALPHA_CAP stays under text; tails are the dim ramp end).
    let cfg = RainConfig {
        enabled: true,
        density: 12,
        trail: 10,
        alpha_override: Some(96),
        head_alpha_override: Some(135),
        seed: RAIN_SEED,
        default_bg: base_input.default_bg,
        theme_fg: 0x00D0_D0D0,
        ..RainConfig::default()
    };
    let mut engine = MatrixRain::new(cfg);

    // Give every text run a 1-cell horizontal quiet gutter so no rain glyph
    // abuts a word edge (the "aterm|1" token-merge the legibility audit
    // flagged). Applied ONLY to this occupancy-scan copy — the sweep renders
    // fresh cells, so the gutter shows as clean black, never a sentinel.
    let dbg = [
        (base_input.default_bg >> 16) as u8,
        (base_input.default_bg >> 8) as u8,
        base_input.default_bg as u8,
    ];
    let mut scan_cells = base_input.cells.clone();
    dilate_text_gutter(&mut scan_cells, dbg);
    engine.rescan_from_cells(
        &scan_cells,
        &base_input.line_sizes,
        &base_input.images,
        rows,
        cols,
        base_input.default_bg,
        1,
    );
    let (cw, ch) = cpu.cell_size();
    let geom = EffectGeom {
        cell_w: cw as u16,
        cell_h: ch as u16,
        rows: rows as u16,
        cols: cols as u16,
    };

    // The host masks the bottom input pane: hidden cursor (DECTCEM off) → the
    // last K damaged rows. Feeding it keeps rain out of the composer, exactly
    // as production does — the difference between a faithful capture and a
    // flooded input box.
    let band: Vec<u16> = ((rows - INPUT_PANE_ROWS) as u16..rows as u16).collect();
    let tick_input = RainTickInput {
        cursor: None,
        hidden_band: &band,
        sel: None,
        display_offset: 0,
        is_alt_screen: false,
    };

    let mut quads = Vec::new();
    let mut add = Vec::new();

    // Warm-up: bake the 64-tile atlas (>=8 bake ticks) and mature the field.
    for _ in 0..60 {
        engine.note_keystroke();
        engine.advance_ms(CALM_TICK_MS);
        engine.emit(geom, &tick_input, &mut quads, &mut add);
    }
    assert!(
        engine.rain_atlas().is_some(),
        "rain glyph atlas never baked — the capture would be blank"
    );

    // Rows carrying UI text — used to steer bright HEADS out of text rows so
    // the punch lands in the open field, never crowding a sentence (audit).
    let text_rows: Vec<bool> = (0..rows)
        .map(|r| {
            base_input
                .cells
                .get(r)
                .is_some_and(|row| row.iter().any(|c| c.ch != ' ' || c.bg != dbg))
        })
        .collect();

    // Sweep a tick window; keep the frame with the most visual punch (bright
    // heads dominate the score, then coverage), minus a penalty for heads that
    // land in text rows. Deterministic.
    let mut best_score = i64::MIN;
    let mut best: Option<(Vec<u32>, usize, usize, usize, usize)> = None;
    for _ in 0..400 {
        engine.note_keystroke();
        engine.advance_ms(CALM_TICK_MS);
        engine.emit(geom, &tick_input, &mut quads, &mut add);
        quads.sort_by_key(|q| q.row); // row-sorted arrival (CSR bucketing)
        let (nq, nh) = (quads.len(), add.len());
        // Bright heads dominate the punch; coverage weighted up (nq*3) so the
        // sparser Claude mask still fills; penalize any bright head landing in
        // a text row so heads scatter across the OPEN field (legibility audit).
        let heads_in_text = add
            .iter()
            .filter(|h| (h.row as usize) < rows && text_rows[h.row as usize])
            .count();
        let score = (nh * 40 + nq * 3) as i64 - (heads_in_text as i64) * 80;
        if score > best_score {
            best_score = score;
            let mut input = term.cell_frame(rows, cols);
            input.rain_quads.clone_from(&quads);
            input.rain_add.clone_from(&add);
            input.rain_atlas = if quads.is_empty() {
                None
            } else {
                engine.rain_atlas()
            };
            let frame = cpu.render_input(&input);
            best = Some((frame.pixels, frame.width, frame.height, nq, nh));
        }
    }

    let (pixels, width, height, nq, nh) = best.expect("no rain frame produced across the sweep");

    let mut rgb = Vec::with_capacity(pixels.len() * 3);
    for &p in &pixels {
        rgb.push((p >> 16) as u8);
        rgb.push((p >> 8) as u8);
        rgb.push(p as u8);
    }
    let file = std::fs::File::create(out_path).expect("create png");
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("png header")
        .write_image_data(&rgb)
        .expect("png data");

    eprintln!(
        "[{scene}] wrote {out_path} ({width}x{height}) \u{2014} {nq} glyph quads, {nh} bright-head halos"
    );
    (nq, nh)
}

/// Render one PHOSPHOR frame to a PNG. Ignored by default (visual, needs a
/// system font); run with `--ignored`. `PHOSPHOR_SCENE` selects the host layout
/// (`claude` | `codex`), `PHOSPHOR_RAIN_PNG` the output path.
#[test]
#[ignore = "visual capture; run explicitly with --ignored"]
fn screenshot_the_rain() {
    if Renderer::from_system(20.0, Theme::default()).is_none() {
        eprintln!("SKIP: no system monospace font");
        return;
    }
    let scene = std::env::var("PHOSPHOR_SCENE").unwrap_or_else(|_| "claude".to_string());
    // Default under the temp dir so a bare `--ignored` run never litters the
    // repo; override with PHOSPHOR_RAIN_PNG to place it anywhere.
    let path = std::env::var("PHOSPHOR_RAIN_PNG").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join(format!("phosphor-rain-{scene}.png"))
            .to_string_lossy()
            .into_owned()
    });
    let (quads, _halos) = capture(&scene, &path);
    assert!(quads > 0, "no rain quads emitted — nothing to screenshot");
}
