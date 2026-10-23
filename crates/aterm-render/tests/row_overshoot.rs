// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Row-damage upward glyph overshoot differential tests.

use std::sync::Arc;

use aterm_core::render::{
    FireHaloCell, FireMode, FirePatch, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot,
    FxSpace, GlowQuad, RenderInput,
};
use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, Renderer, Theme, WindowCpu};

const ROWS: usize = 8;
const COLS: usize = 22;
/// The row the "one" class carries its accented capitals on, and the row the typing streams edit.
const HOT: usize = 3;
/// Frames per case after the warm frame.
const FRAMES: usize = 6;

fn renderer_px(px: f32) -> Renderer {
    Renderer::from_system(px, Theme::default())
        .map(|mut r| {
            r.debug_block_on_lazy_fallbacks();
            r
        })
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// Accented capitals.
const ACCENTS: &str = "\u{c9}\u{c1}\u{d1}\u{c5}\u{ca}\u{cd}";

/// Row `r`'s text for a glyph class ([`HOT`]).
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

/// A terminal showing `class`'s screen, the cursor parked at `(row, col)` and shown or hidden.
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

/// What a frame's quads and layers are placed with.
#[derive(Clone, Copy)]
struct Geo {
    cw: usize,
    ch: usize,
    pad: usize,
    top: usize,
}

/// A full-width `glow_under` quad over row `r` (window-absolute pixels).
fn under(g: Geo, r: usize, color: u32) -> GlowQuad {
    GlowQuad {
        row: r as u16,
        x: g.pad as u16,
        y: (g.top + r * g.ch) as u16,
        w: (COLS * g.cw) as u16,
        h: g.ch as u16,
        color,
        alpha: 0,
        color2: color,
        alpha2: 0,
    }
}

/// A steady flame filling row `r`'s band ([`Renderer::row_may_ink_above`]).
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

/// An fx layer over row [`HOT`]'s band in `slot`, with a fresh content `Arc` each call.
fn hot_layer(g: Geo, slot: FxSlot) -> FxLayer {
    let rect = FxItem::Rect(FxRect {
        x: 2,
        y: (HOT * g.ch) as i32,
        w: (12 * g.cw) as u16,
        h: g.ch as u16,
        ink: 0,
    });
    let palette = vec![FxInk::add(0x0030_2010), FxInk::add(0x0010_2030)];
    FxLayer::new(
        u64::from(slot as u8) + 1,
        Arc::new(FxContent::new(vec![rect], palette)),
        slot,
        FxSpace::Grid,
        FxBlend::Exact,
    )
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

/// The warm frame and [`FRAMES`] frames of `stream` over `class`'s screen.
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
        "cursor_parked" => {
            let mut t = screen(class, Some((HOT, 3)));
            (0..=FRAMES)
                .map(|f| {
                    let mut i = snap(&mut t);
                    i.glow_under = vec![under(g, HOT + 2, 0x0008_0402 * (1 + f as u32))];
                    i
                })
                .collect()
        }
        "glow_under" => {
            let mut t = screen(class, None);
            (0..=FRAMES)
                .map(|f| {
                    let mut i = snap(&mut t);
                    i.glow_under = vec![under(g, HOT, 0x0008_0402 * (1 + f as u32))];
                    i
                })
                .collect()
        }
        "glow_pair" => {
            let mut t = screen(class, None);
            (0..=FRAMES)
                .map(|f| {
                    let mut i = snap(&mut t);
                    let color = 0x0008_0402 * (1 + f as u32);
                    i.glow_under = vec![under(g, 0, color), under(g, HOT, color)];
                    i
                })
                .collect()
        }
        "fire_halo" => {
            let mut t = screen(class, None);
            (0..=FRAMES)
                .map(|f| {
                    let mut i = snap(&mut t);
                    i.glow_under = vec![under(g, HOT, 0x0008_0402 * (1 + f as u32))];
                    flames(g, HOT + 1, &mut i);
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
                    i.fx_layers = vec![hot_layer(g, slot)];
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

#[test]
fn row_damage_beside_overshooting_glyphs_is_byte_exact() {
    let classes = ["accent", "one", "plain"];
    let mut failures: Vec<String> = Vec::new();
    let mut widened: Vec<u32> = Vec::new();
    let (mut frames_total, mut rows_frames) = (0usize, 0usize);
    for px in 11..=20 {
        let size = px;
        let px = px as f32;
        let mut warm = renderer_px(px);
        let mut fresh = renderer_px(px);
        let (cw, ch) = warm.cell_size();
        let g = Geo {
            cw,
            ch,
            pad: warm.pad(),
            top: warm.grid_top(),
        };
        for class in classes {
            for stream in STREAMS {
                let inputs = frames(class, stream, g);
                let mut wc = WindowCpu::new();
                let (mut bad, mut bad_px) = (0usize, 0usize);
                for (f, input) in inputs.iter().enumerate() {
                    let cached = warm.render_input_cached(&mut wc, input).pixels().to_vec();
                    let full = fresh.render_input(input).pixels;
                    if f > 0 {
                        frames_total += 1;
                        if wc.last_damage() == DamageOutcome::Rows {
                            rows_frames += 1;
                        }
                    }
                    let n = cached.iter().zip(&full).filter(|(a, b)| a != b).count();
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
        "row-overshoot sweep: {frames_total} frames, {rows_frames} on the row path, {} \
         failing cases",
        failures.len()
    );
    assert!(
        rows_frames * 10 >= frames_total * 9,
        "only {rows_frames} of {frames_total} frames took the row path"
    );
    assert!(
        failures.is_empty(),
        "the row path differs from a full render:\n{}",
        failures.join("\n")
    );
    assert!(
        widened.is_empty(),
        "a raster widened the overshoot rule at px {widened:?}"
    );
}

#[test]
fn ascii_typing_repaints_only_its_row() {
    for px in 11..=20 {
        let mut r = renderer_px(px as f32);
        for row in [HOT - 1, HOT + 1, ROWS - 2] {
            let mut t = screen("one", Some((row, 4)));
            let mut wc = WindowCpu::new();
            let _ = r.render_input_cached(&mut wc, &t.cell_frame(ROWS, COLS));
            for s in ["a", "Z", " ", "|"] {
                t.process(s.as_bytes());
                let _ = r.render_input_cached(&mut wc, &t.cell_frame(ROWS, COLS));
                assert_eq!(wc.last_damage(), DamageOutcome::Rows, "px {px} row {row}");
                let marked: Vec<usize> = (0..ROWS).filter(|&i| wc.dirty_rows()[i]).collect();
                assert_eq!(marked, [row], "px {px}: typing {s:?} on row {row}");
            }
        }
    }
}

/// Whether `glyphs` on row 1 ink into row 0's band at `px` in this host's face.
fn inks_above(px: f32, glyphs: &str) -> bool {
    let mut r = renderer_px(px);
    let (_, ch) = r.cell_size();
    let top = r.grid_top();
    let mut t = Terminal::new(3, COLS as u16);
    t.process(format!("\x1b[?25l\x1b[2;1H{glyphs}").as_bytes());
    let lit = r.render_input(&t.cell_frame(3, COLS));
    let mut blank = Terminal::new(3, COLS as u16);
    blank.process(b"\x1b[?25l");
    let bare = r.render_input(&blank.cell_frame(3, COLS));
    let band0 = (top * lit.width)..((top + ch) * lit.width);
    lit.pixels[band0.clone()] != bare.pixels[band0]
}

/// The first px of the sweep at which `glyphs` ink above their cell in this host's face.
fn an_overshooting_px(glyphs: &str) -> f32 {
    (11..=20)
        .map(|px| px as f32)
        .find(|&px| inks_above(px, glyphs))
        .unwrap_or_else(|| panic!("{glyphs:?} ink above their cell at no px in 11..=20"))
}

#[test]
fn accented_capitals_ink_above_their_cell_at_some_px() {
    an_overshooting_px(ACCENTS);
}

/// The prompt glyphs a shell draws its prompt with.
const PROMPTS: [&str; 4] = ["$ ", "\u{276f} ", "\u{3bb} ", "\u{2192} "];

/// The rows [`HOT`]'s typing marked.
fn rows_typing_marks(r: &mut Renderer, prompt: &str, keys: &[&str]) -> Vec<Vec<usize>> {
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    for row in 0..ROWS {
        if row != HOT {
            t.process(format!("\x1b[{};1H{}", row + 1, line("plain", row)).as_bytes());
        }
    }
    t.process(format!("\x1b[{};1H{prompt}", HOT + 1).as_bytes());
    let mut wc = WindowCpu::new();
    let _ = r.render_input_cached(&mut wc, &t.cell_frame(ROWS, COLS));
    keys.iter()
        .map(|k| {
            t.process(k.as_bytes());
            let _ = r.render_input_cached(&mut wc, &t.cell_frame(ROWS, COLS));
            assert_eq!(
                wc.last_damage(),
                DamageOutcome::Rows,
                "{prompt:?} typing {k:?}"
            );
            (0..ROWS).filter(|&i| wc.dirty_rows()[i]).collect()
        })
        .collect()
}

#[test]
fn typing_at_a_symbol_prompt_repaints_only_its_row() {
    let keys = ["l", "s", " ", "-"];
    for px in 11..=20 {
        let mut r = renderer_px(px as f32);
        for prompt in PROMPTS {
            for (k, marked) in keys.iter().zip(rows_typing_marks(&mut r, prompt, &keys)) {
                assert_eq!(
                    marked,
                    [HOT],
                    "px {px}: typing {k:?} at the {prompt:?} prompt"
                );
            }
        }
        assert!(!r.overshoot_rule_widened(), "px {px}");
    }
    let px = an_overshooting_px("\u{c9}");
    let mut r = renderer_px(px);
    for marked in rows_typing_marks(&mut r, "\u{c9} ", &keys) {
        assert_eq!(
            marked,
            [HOT - 1, HOT],
            "px {px}: typing at an accented prompt"
        );
    }
    let mut r = renderer_px(px);
    let marked = rows_typing_marks(&mut r, "$ ", &["\u{d4}"]);
    assert_eq!(
        marked,
        [[HOT - 1, HOT]],
        "px {px}: typing an undrawn accented capital"
    );
}

#[test]
fn ascii_inking_above_its_cell_repaints_in_full_once() {
    let mut caught = Vec::new();
    for px in 11..=20 {
        let mut warm = renderer_px(px as f32);
        let mut fresh = renderer_px(px as f32);
        warm.set_line_height(0.7);
        fresh.set_line_height(0.7);
        let mut t = Terminal::new(ROWS as u16, COLS as u16);
        t.process(b"\x1b[?25l");
        let mut wc = WindowCpu::new();
        let _ = warm.render_input_cached(&mut wc, &t.cell_frame(ROWS, COLS));
        let mut outcomes = Vec::new();
        for (f, key) in ["(", "i", "(", "x", " "].iter().enumerate() {
            t.process(format!("\x1b[{};{}H{key}", HOT + 1, 5 + f).as_bytes());
            let input = t.cell_frame(ROWS, COLS);
            let cached = warm.render_input_cached(&mut wc, &input).pixels().to_vec();
            assert!(
                cached == fresh.render_input(&input).pixels,
                "px {px}: frame {f} ({key:?}) differs from a full render"
            );
            outcomes.push(wc.last_damage());
        }
        let fulls = outcomes
            .iter()
            .filter(|&&o| o == DamageOutcome::Full)
            .count();
        let rows = outcomes
            .iter()
            .filter(|&&o| o == DamageOutcome::Rows)
            .count();
        let caught_here = usize::from(warm.overshoot_rule_widened());
        assert_eq!(
            (fulls, rows),
            (caught_here, outcomes.len() - caught_here),
            "px {px}: {outcomes:?}"
        );
        if caught_here == 1 {
            caught.push(px);
        }
    }
    assert!(
        !caught.is_empty(),
        "no ASCII inked above its cell at any px"
    );
}

#[test]
fn each_catch_narrows_the_rule_a_step() {
    let mut r = renderer_px(13.0);
    let mut t = Terminal::new(3, COLS as u16);
    t.process("\x1b[?25l\x1b[1;1Hplain\x1b[2;1H\u{2500}\u{2500}\u{2500}".as_bytes());
    let input = t.cell_frame(3, COLS);
    let _ = r.render_input(&input);
    let (plain, rule) = (0, 1);
    let caught = |r: &mut Renderer| r.catch_unpredicted_overshoot(&input, plain, -1, 0);
    assert!(!r.row_may_ink_above(&input, plain) && !r.row_may_ink_above(&input, rule));
    assert!(caught(&mut r), "ASCII taken on trust");
    assert!(r.overshoot_rule_widened());
    assert!(!r.row_may_ink_above(&input, plain) && !r.row_may_ink_above(&input, rule));
    assert!(caught(&mut r), "a raster the per-cell keys do not name");
    assert!(r.row_may_ink_above(&input, plain) && r.row_may_ink_above(&input, rule));
    assert!(!caught(&mut r), "every glyph counts: nothing left to catch");
}
