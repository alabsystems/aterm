// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::common;

use crate::rain_common::RainScene;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_effects::matrix_rain::RainVisibility;
use aterm_gpu::GpuRenderer;
use aterm_render::{RenderInput, Theme};

const ROWS: usize = 8;
const COLS: usize = 24;

/// A fresh GpuRenderer at the suite's px/theme.
fn fresh_gpu() -> GpuRenderer {
    let mut g = common::gpu(18.0, Theme::default());
    g.debug_block_on_lazy_fallbacks();
    g
}

/// Render `input` on a brand-new GpuRenderer.
fn fresh_render(input: &RenderInput, blink: bool, override_: Option<CursorStyle>) -> Vec<u32> {
    let mut g = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();
    g.set_cursor_blink_phase(blink);
    g.set_cursor_style_override(override_);
    g.render_input(&mut win, input, None).pixels
}

#[test]
fn gpu_dirty_gate_byte_identical() {
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();

    let mut term = Terminal::new(ROWS as u16, COLS as u16);

    struct Step {
        desc: &'static str,
        act: Box<dyn Fn(&mut Terminal)>,
        blink: bool,
        override_: Option<CursorStyle>,
        expect_hit: Option<bool>,
    }

    fn step(
        desc: &'static str,
        act: impl Fn(&mut Terminal) + 'static,
        blink: bool,
        override_: Option<CursorStyle>,
        expect_hit: Option<bool>,
    ) -> Step {
        Step {
            desc,
            act: Box::new(act),
            blink,
            override_,
            expect_hit,
        }
    }

    let steps = vec![
        step("prompt", |t| t.process(b"$ "), true, None, Some(false)),
        step("idle (unchanged)", |_| {}, true, None, Some(true)),
        step("idle again", |_| {}, true, None, Some(true)),
        step("blink off", |_| {}, false, None, None),
        step("blink on", |_| {}, true, None, None),
        step("type 'l'", |t| t.process(b"l"), true, None, Some(false)),
        step("idle after type", |_| {}, true, None, Some(true)),
        step("type 's'", |t| t.process(b"s"), true, None, Some(false)),
        step(
            "run output",
            |t| {
                t.process(b"\r\n");
                for n in 0..20 {
                    t.process(format!("file{n} ").as_bytes());
                    t.process(b"\r\n");
                }
                t.process(b"done");
            },
            true,
            None,
            Some(false),
        ),
        step("idle after output", |_| {}, true, None, Some(true)),
        step(
            "focus lost",
            |_| {},
            true,
            Some(CursorStyle::HollowBlock),
            Some(false),
        ),
        step(
            "idle unfocused",
            |_| {},
            true,
            Some(CursorStyle::HollowBlock),
            Some(true),
        ),
        step("focus gained", |_| {}, true, None, Some(false)),
        step(
            "select",
            |t| {
                let sel = t.text_selection_mut();
                sel.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
                sel.update_selection(2, 4, SelectionSide::Right);
                sel.complete_selection();
            },
            true,
            None,
            Some(false),
        ),
        step("idle selected", |_| {}, true, None, Some(true)),
        step(
            "clear selection",
            |t| t.text_selection_mut().clear(),
            true,
            None,
            Some(false),
        ),
        step(
            "scroll back",
            |t| t.scroll_display(1),
            true,
            None,
            Some(false),
        ),
        step("idle scrolled", |_| {}, true, None, Some(true)),
        step(
            "scroll to bottom",
            |t| t.scroll_display(-1),
            true,
            None,
            Some(false),
        ),
        step(
            "clear + new screen",
            |t| t.process(b"\x1b[2J\x1b[Hready> "),
            true,
            None,
            Some(false),
        ),
        step("idle new screen", |_| {}, true, None, Some(true)),
    ];

    let mut hits_seen = 0u64;
    let mut gate_hit_frames = 0usize;

    for (i, s) in steps.iter().enumerate() {
        (s.act)(&mut term);
        gpu.set_cursor_blink_phase(s.blink);
        gpu.set_cursor_style_override(s.override_);

        let input = term.cell_frame(ROWS, COLS);

        let hits_before = gpu.gate_hits();
        let view = gpu.render_input_cached(&mut win, &input);

        let got: Vec<u32> = view.pixels().to_vec();
        let (gw, gh) = (view.width(), view.height());
        drop(view);
        let took_gate = gpu.gate_hits() > hits_before;

        assert_eq!(
            (gw, gh),
            (COLS * gpu.cell_size().0, ROWS * gpu.cell_size().1),
            "step {i} ({}): bad dims",
            s.desc
        );

        let oracle = fresh_render(&input, s.blink, s.override_);
        assert_eq!(
            got.len(),
            oracle.len(),
            "step {i} ({}): pixel count differs",
            s.desc
        );
        assert!(
            got == oracle,
            "step {i} ({}): gate {} pixels are NOT byte-identical to a fresh GPU render",
            s.desc,
            if took_gate { "HIT" } else { "miss" },
        );

        match s.expect_hit {
            Some(true) => {
                assert!(
                    took_gate,
                    "step {i} ({}): expected GATE-HIT but it missed",
                    s.desc
                );
                gate_hit_frames += 1;
            }
            Some(false) => {
                assert!(
                    !took_gate,
                    "step {i} ({}): expected a MISS but it took the gate",
                    s.desc
                );
            }
            None => {}
        }

        if took_gate {
            hits_seen += 1;
        }
        eprintln!(
            "step {i:2} {:<22} gate={} (hits={}, misses={})",
            s.desc,
            if took_gate { "HIT " } else { "miss" },
            gpu.gate_hits(),
            gpu.gate_misses(),
        );
    }

    assert!(
        hits_seen >= 1,
        "the dirty-gate never fired — optimisation not exercised"
    );
    assert_eq!(
        hits_seen as usize, gate_hit_frames,
        "every counted hit should correspond to an expected-hit frame",
    );
    assert_eq!(
        gpu.gate_hits(),
        hits_seen,
        "gate_hits counter disagrees with observed hits"
    );
    eprintln!(
        "dirty-gate: {hits_seen} gate-hits across {} frames",
        steps.len()
    );
}

#[test]
fn gpu_dirty_gate_one_cell_change_after_hit() {
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"ABC");
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    let in1 = term.cell_frame(ROWS, COLS);
    let _ = gpu.render_input_cached(&mut win, &in1);
    assert_eq!(gpu.gate_misses(), 1, "first frame must miss");

    let in2 = term.cell_frame(ROWS, COLS);
    let hits_before = gpu.gate_hits();
    let v2 = gpu.render_input_cached(&mut win, &in2);
    let got2 = v2.pixels().to_vec();
    drop(v2);
    assert!(
        gpu.gate_hits() > hits_before,
        "second (unchanged) frame must take the gate"
    );
    assert!(
        got2 == fresh_render(&in2, true, None),
        "gate-hit pixels diverge from fresh render"
    );

    term.process(b"\x08D"); // backspace over 'C', write 'D' → "ABD"
    let in3 = term.cell_frame(ROWS, COLS);
    let misses_before = gpu.gate_misses();
    let v3 = gpu.render_input_cached(&mut win, &in3);
    let got3 = v3.pixels().to_vec();
    drop(v3);
    assert!(
        gpu.gate_misses() > misses_before,
        "one-cell change must MISS the gate"
    );
    assert!(
        got3 == fresh_render(&in3, true, None),
        "post-change pixels diverge from fresh render"
    );
    assert!(
        got3 != got2,
        "one-cell change produced an identical framebuffer (suspicious)"
    );

    let in4 = term.cell_frame(ROWS, COLS);
    let hits_before = gpu.gate_hits();
    let v4 = gpu.render_input_cached(&mut win, &in4);
    let got4 = v4.pixels().to_vec();
    drop(v4);
    assert!(
        gpu.gate_hits() > hits_before,
        "idle after change must take the gate"
    );
    assert!(
        got4 == got3,
        "re-presented frame differs from the frame it cached"
    );
    assert!(
        got4 == fresh_render(&in4, true, None),
        "post-change gate-hit diverges from fresh render"
    );
}

#[test]
fn gpu_dirty_gate_tracks_cursor_effect_style_override_and_plain_clear() {
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[4 q"); // terminal owns steady underline
    let plain = term.cell_frame(ROWS, COLS);

    let _ = gpu.render_input_cached(&mut win, &plain);
    let hits_before = gpu.gate_hits();
    let _ = gpu.render_input_cached(&mut win, &plain);
    assert!(
        gpu.gate_hits() > hits_before,
        "settled plain frame must hit"
    );

    let mut effect = plain.clone();
    effect.cursor_effect_style_override = Some(CursorStyle::Bolt);
    let misses_before = gpu.gate_misses();
    let effect_pixels = gpu.render_input_cached(&mut win, &effect).pixels().to_vec();
    assert!(
        gpu.gate_misses() > misses_before,
        "stationary effect-shape transition must miss"
    );
    assert_eq!(effect_pixels, fresh_render(&effect, true, None));

    let hits_before = gpu.gate_hits();
    let _ = gpu.render_input_cached(&mut win, &effect);
    assert!(
        gpu.gate_hits() > hits_before,
        "settled effect frame must hit"
    );

    let mut cleared = effect.clone();
    cleared.clear_overlays();
    assert_eq!(cleared.cursor_effect_style_override, None);
    assert_eq!(cleared.cursor_style, CursorStyle::SteadyUnderline);
    let misses_before = gpu.gate_misses();
    let cleared_pixels = gpu
        .render_input_cached(&mut win, &cleared)
        .pixels()
        .to_vec();
    assert!(
        gpu.gate_misses() > misses_before,
        "plain clear after a settled effect must miss"
    );
    assert_eq!(cleared_pixels, fresh_render(&plain, true, None));
}

#[test]
fn gpu_dirty_gate_settled_and_drained_rain() {
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[?25l$ rain"); // hidden cursor: no blink in the mix
    let base = term.cell_frame(ROWS, COLS);
    let mut scene = RainScene::new(ROWS, COLS, gpu.cell_size(), &base);
    let fp_a = scene.drive_until_raining();

    let mut in_a = term.cell_frame(ROWS, COLS);
    scene.apply(&mut in_a);
    let _ = gpu.render_input_cached(&mut win, &in_a);
    assert_eq!(gpu.gate_misses(), 1, "first frame must miss");

    let fp_b = scene.emit();
    assert_eq!(fp_b, fp_a, "same-tick re-emission must be fp-stable");
    let mut in_b = term.cell_frame(ROWS, COLS);
    scene.apply(&mut in_b);
    assert!(
        !in_b.rain_quads.is_empty(),
        "settled rain is non-empty (non-vacuous)"
    );
    let hits_before = gpu.gate_hits();
    let view = gpu.render_input_cached(&mut win, &in_b);
    let got_b = view.pixels().to_vec();
    drop(view);
    assert!(
        gpu.gate_hits() > hits_before,
        "a SETTLED rain frame (equal quads, same atlas version) must take the gate"
    );
    assert!(
        got_b == fresh_render(&in_b, true, None),
        "settled-rain gate-hit pixels diverge from a fresh full GPU render"
    );

    scene.engine.set_visibility(RainVisibility::Hidden);
    let fp_c = scene.tick();
    assert_eq!(fp_c, 0, "a drained field must fingerprint 0");
    assert!(scene.quads.is_empty() && scene.add.is_empty());
    assert!(
        !scene.engine.is_active(),
        "a drained, sleeping field must report inactive (the host timer disarms)"
    );
    let mut in_c = term.cell_frame(ROWS, COLS);
    scene.apply(&mut in_c); // empty channels, atlas None (the host contract)
    assert!(in_c.rain_atlas.is_none());
    let misses_before = gpu.gate_misses();
    let view = gpu.render_input_cached(&mut win, &in_c);
    let got_c = view.pixels().to_vec();
    drop(view);
    assert!(
        gpu.gate_misses() > misses_before,
        "the rain vanishing is a real change and must MISS the gate"
    );
    assert!(
        got_c == fresh_render(&in_c, true, None),
        "the drained frame must fully erase the rain (no ghost pixels)"
    );
    assert!(
        got_c != got_b,
        "draining must visibly change the frame (non-vacuous)"
    );

    let fp_d = scene.tick();
    assert_eq!(
        fp_d, 0,
        "the drained fingerprint must stay byte-stable at 0"
    );
    let mut in_d = term.cell_frame(ROWS, COLS);
    scene.apply(&mut in_d);
    let hits_before = gpu.gate_hits();
    let view = gpu.render_input_cached(&mut win, &in_d);
    let got_d = view.pixels().to_vec();
    drop(view);
    assert!(
        gpu.gate_hits() > hits_before,
        "the drained steady state must take the dirty gate (idle rain is free)"
    );
    assert!(
        got_d == got_c,
        "drained gate-hit must re-present the prior frame verbatim"
    );
    assert!(
        got_d == fresh_render(&in_d, true, None),
        "drained gate-hit pixels diverge from a fresh full GPU render"
    );
}
