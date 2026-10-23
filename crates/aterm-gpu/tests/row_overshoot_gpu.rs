// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use aterm_core::render::{
    FireHaloCell, FireMode, FirePatch, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot,
    FxSpace,
};
use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuRenderer, WindowGpu};
use aterm_render::{GlowQuad, RenderInput, Theme};

use crate::common;

const ROWS: usize = 8;
const COLS: usize = 22;
/// Row carrying accented capitals in the `"one"` class and edited by typing streams.
const HOT: usize = 3;
/// Frames per case after the warm frame.
const FRAMES: usize = 6;
const ACCENTS: &str = "\u{c9}\u{c1}\u{d1}\u{c5}\u{ca}\u{cd}";

fn gpu_px(px: f32) -> GpuRenderer {
    let mut g = common::gpu(px, Theme::default());
    g.debug_block_on_lazy_fallbacks();
    g.set_shimmer(false);
    g
}

fn line(class: &str, r: usize) -> String {
    let accented = match class {
        "accent" => true,
        "one" => r == HOT,
        "plain" => false,
        other => panic!("unknown class {other}"),
    };
    if accented {
        format!("{ACCENTS} {ACCENTS} r{r}")
    } else {
        format!("plain text row {r}")
    }
}

fn screen(class: &str, cursor: Option<(usize, usize)>) -> Terminal {
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    for r in 0..ROWS {
        t.process(format!("\x1b[{};1H{}", r + 1, line(class, r)).as_bytes());
    }
    match cursor {
        Some((r, c)) => t.process(format!("\x1b[{};{}H", r + 1, c + 1).as_bytes()),
        None => t.process(b"\x1b[?25l"),
    }
    t
}

/// Steady flame filling row `r`'s band with a contrast halo on its first twelve cells.
fn flames(g: Geo, r: usize, i: &mut RenderInput) {
    i.fire_patch = vec![FirePatch {
        row: r as u16,
        x: 0,
        y: (r * g.ch) as u16,
        w: (COLS * g.cw) as u16,
        h: g.ch as u16,
        base_y: ((r + 1) * g.ch) as u16,
        peak_h: (3 * g.ch) as u16,
        phase: 4096,
        temp: 240,
        strength: 255,
        lean: 0,
        cov_cap: 200,
        cell_h: g.ch as u16,
        mode: FireMode::Add,
    }];
    i.fire_halo = (0..12)
        .map(|col| FireHaloCell {
            row: r as u16,
            col,
            strength: 255,
        })
        .collect();
}

#[derive(Clone, Copy)]
struct Geo {
    cw: usize,
    ch: usize,
    pad: usize,
    top: usize,
}

fn frames(class: &str, stream: &str, g: Geo) -> Vec<RenderInput> {
    let snap = |t: &mut Terminal| t.cell_frame(ROWS, COLS);
    match stream {
        "cursor" => {
            let walk = [(HOT, 2), (HOT + 1, 3), (HOT - 1, 4), (HOT, 1), (HOT + 1, 0)];
            let mut t = screen(class, Some((HOT - 1, 0)));
            let mut out = vec![snap(&mut t)];
            for &(r, c) in walk.iter().cycle().take(FRAMES) {
                t.process(format!("\x1b[{};{}H", r + 1, c + 1).as_bytes());
                out.push(snap(&mut t));
            }
            out
        }
        "glow_under" | "glow_pair" | "fire_halo" | "cursor_parked" => {
            let parked = stream == "cursor_parked";
            let mut t = screen(class, parked.then_some((HOT, 3)));
            let quad_rows: &[usize] = match stream {
                "glow_pair" => &[0, HOT],
                "cursor_parked" => &[HOT + 2],
                _ => &[HOT],
            };
            (0..=FRAMES)
                .map(|f| {
                    let mut i = snap(&mut t);
                    let color = 0x0008_0402 * (1 + f as u32);
                    i.glow_under = quad_rows
                        .iter()
                        .map(|&r| GlowQuad {
                            row: r as u16,
                            x: g.pad as u16,
                            y: (g.top + r * g.ch) as u16,
                            w: (COLS * g.cw) as u16,
                            h: g.ch as u16,
                            color,
                            alpha: 0,
                            color2: color,
                            alpha2: 0,
                        })
                        .collect();
                    if stream == "fire_halo" {
                        flames(g, HOT + 1, &mut i);
                    }
                    i
                })
                .collect()
        }
        "fx_under" | "fx_glow" | "fx_over" => {
            let slot = match stream {
                "fx_under" => FxSlot::Under,
                "fx_glow" => FxSlot::Glow,
                _ => FxSlot::Over,
            };
            let mut t = screen(class, None);
            (0..=FRAMES)
                .map(|_| {
                    let mut i = snap(&mut t);
                    let rect = FxItem::Rect(FxRect {
                        x: 2,
                        y: (HOT * g.ch) as i32,
                        w: (12 * g.cw) as u16,
                        h: g.ch as u16,
                        ink: 0,
                    });
                    let palette = vec![FxInk::add(0x0030_2010), FxInk::add(0x0010_2030)];
                    i.fx_layers = vec![FxLayer::new(
                        u64::from(slot as u8) + 1,
                        Arc::new(FxContent::new(vec![rect], palette)),
                        slot,
                        FxSpace::Grid,
                        FxBlend::Exact,
                    )];
                    i
                })
                .collect()
        }
        "type_on" | "type_above" | "type_below" => {
            let row = match stream {
                "type_on" => HOT,
                "type_above" => HOT - 1,
                _ => HOT + 1,
            };
            let col = 8;
            let mut t = screen(class, Some((row, col)));
            let mut out = vec![snap(&mut t)];
            let glyphs: [&str; FRAMES] = if class == "plain" {
                ["H", "x", " ", "Q", "|", " "]
            } else {
                ["\u{c9}", "x", " ", "\u{c5}", "\u{cd}", " "]
            };
            for (f, s) in glyphs.iter().enumerate() {
                let c = col + f % 2;
                t.process(format!("\x1b[{};{}H{s}", row + 1, c + 1).as_bytes());
                out.push(snap(&mut t));
            }
            out
        }
        other => panic!("unknown stream {other}"),
    }
}

const STREAMS: [&str; 11] = [
    "cursor",
    "cursor_parked",
    "glow_under",
    "glow_pair",
    "fire_halo",
    "fx_under",
    "fx_glow",
    "fx_over",
    "type_on",
    "type_above",
    "type_below",
];

/// Every scissored present frame is byte-identical to a full repaint of the same input.
#[test]
fn scissored_rows_beside_overshooting_glyphs_are_byte_exact() {
    let mut failures: Vec<String> = Vec::new();
    let mut widened: Vec<u32> = Vec::new();
    let (mut frames_total, mut scissored) = (0u64, 0u64);
    for size in 11..=20 {
        let px = size as f32;
        let mut warm = gpu_px(px);
        let mut fresh = gpu_px(px);
        let cpu = common::cpu(px, Theme::default());
        let (cw, ch) = cpu.cell_size();
        let g = Geo {
            cw,
            ch,
            pad: cpu.pad(),
            top: cpu.grid_top(),
        };
        for class in ["accent", "one", "plain"] {
            for stream in STREAMS {
                let inputs = frames(class, stream, g);
                let mut win = WindowGpu::new();
                let (mut bad, mut bad_px) = (0usize, 0usize);
                for (f, input) in inputs.iter().enumerate() {
                    let before = warm.scissor_taken();
                    let shown = warm.present_input_readback(&mut win, input).pixels;
                    if f > 0 {
                        frames_total += 1;
                        scissored += warm.scissor_taken() - before;
                    }
                    let full = fresh
                        .render_input(&mut WindowGpu::new(), input, None)
                        .pixels;
                    let n = shown.iter().zip(&full).filter(|(a, b)| a != b).count();
                    if n > 0 {
                        bad += 1;
                        bad_px += n;
                    }
                }
                if bad > 0 {
                    failures.push(format!(
                        "px {px} {class}/{stream}: {bad} of {} frames differ ({bad_px} px)",
                        inputs.len()
                    ));
                }
            }
        }
        if warm.overshoot_rule_widened() {
            widened.push(size);
        }
    }
    eprintln!(
        "row-overshoot GPU sweep: {frames_total} frames, {scissored} scissored, {} failing \
         cases",
        failures.len()
    );
    assert!(
        scissored * 10 >= frames_total * 9,
        "only {scissored} of {frames_total} frames took the scissored path"
    );
    assert!(
        failures.is_empty(),
        "the scissored present differs from a full repaint:\n{}",
        failures.join("\n")
    );
    assert!(
        widened.is_empty(),
        "a raster widened the overshoot rule at px {widened:?}"
    );
}

/// Instances encoded per keystroke when typing `keys` after `prompt` on row [`HOT`].
fn typing_instances(g: &mut GpuRenderer, prompt: &str, keys: &[&str]) -> Vec<usize> {
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    for row in 0..ROWS {
        if row != HOT {
            t.process(format!("\x1b[{};1H{}", row + 1, line("plain", row)).as_bytes());
        }
    }
    t.process(format!("\x1b[{};1H{prompt}", HOT + 1).as_bytes());
    let mut win = WindowGpu::new();
    let _ = g.present_input_readback(&mut win, &t.cell_frame(ROWS, COLS));
    keys.iter()
        .map(|k| {
            t.process(k.as_bytes());
            let before = g.scissor_taken();
            let _ = g.present_input_readback(&mut win, &t.cell_frame(ROWS, COLS));
            assert_eq!(g.scissor_taken(), before + 1, "{prompt:?} typing {k:?}");
            g.last_instances()
        })
        .collect()
}

/// First px in `11..=20` where `glyph` on row 1 inks into row 0's band.
fn an_overshooting_px(glyph: &str) -> f32 {
    let inks_above = |px: f32| {
        let mut r = common::cpu(px, Theme::default());
        let (_, ch) = r.cell_size();
        let top = r.grid_top();
        let mut t = Terminal::new(3, COLS as u16);
        t.process(format!("\x1b[?25l\x1b[2;1H{glyph}").as_bytes());
        let lit = r.render_input(&t.cell_frame(3, COLS));
        let mut blank = Terminal::new(3, COLS as u16);
        blank.process(b"\x1b[?25l");
        let bare = r.render_input(&blank.cell_frame(3, COLS));
        let band0 = (top * lit.width)..((top + ch) * lit.width);
        lit.pixels[band0.clone()] != bare.pixels[band0]
    };
    (11..=20)
        .map(|px| px as f32)
        .find(|&px| inks_above(px))
        .unwrap_or_else(|| panic!("{glyph:?} inks above its cell at no px in 11..=20"))
}

/// Typing ASCII at symbol prompts (`❯`, `λ`, `→`) encodes only the prompt's row, while an overshooting `Å` prompt encodes the row above too.
#[test]
fn typing_at_a_symbol_prompt_encodes_only_its_row() {
    let keys = ["l", "s", " ", "-"];
    for size in 11..=20 {
        let mut g = gpu_px(size as f32);
        let dollar = typing_instances(&mut g, "$ ", &keys);
        for prompt in ["\u{276f} ", "\u{3bb} ", "\u{2192} "] {
            let symbol = typing_instances(&mut g, prompt, &keys);
            assert_eq!(symbol, dollar, "px {size}: typing at the {prompt:?} prompt");
        }
        assert!(!g.overshoot_rule_widened(), "px {size}");
    }
    let px = an_overshooting_px("\u{c5}");
    let mut g = gpu_px(px);
    let dollar = typing_instances(&mut g, "$ ", &keys);
    let accent = typing_instances(&mut g, "\u{c5} ", &keys);
    assert!(
        accent.iter().zip(&dollar).all(|(a, d)| a > d),
        "px {px}: an accented prompt encodes {accent:?}, a `$` prompt {dollar:?}"
    );
}

/// ASCII inking above its cell at line height 0.7 triggers one full re-encode and stays byte-identical to a full repaint.
#[test]
fn ascii_inking_above_its_cell_reencodes_in_full_once() {
    let mut caught = Vec::new();
    for size in 11..=20 {
        let mut warm = gpu_px(size as f32);
        let mut fresh = gpu_px(size as f32);
        warm.set_line_height(0.7);
        fresh.set_line_height(0.7);
        let mut t = Terminal::new(ROWS as u16, COLS as u16);
        t.process(b"\x1b[?25l");
        let mut win = WindowGpu::new();
        let _ = warm.present_input_readback(&mut win, &t.cell_frame(ROWS, COLS));
        let (full0, scissored0) = (warm.full_repaints(), warm.scissor_taken());
        let keys = ["(", "i", "(", "x", " "];
        for (f, key) in keys.iter().enumerate() {
            t.process(format!("\x1b[{};{}H{key}", HOT + 1, 5 + f).as_bytes());
            let input = t.cell_frame(ROWS, COLS);
            let shown = warm.present_input_readback(&mut win, &input).pixels;
            let full = fresh
                .render_input(&mut WindowGpu::new(), &input, None)
                .pixels;
            assert!(
                shown == full,
                "px {size}: frame {f} ({key:?}) differs from a full repaint"
            );
        }
        let caught_here = u64::from(warm.overshoot_rule_widened());
        assert_eq!(
            (
                warm.full_repaints() - full0,
                warm.scissor_taken() - scissored0
            ),
            (caught_here, keys.len() as u64),
            "px {size}"
        );
        if caught_here == 1 {
            caught.push(size);
        }
    }
    assert!(
        !caught.is_empty(),
        "no ASCII inked above its cell at any px"
    );
}
