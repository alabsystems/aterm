// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GPU DIRTY-GATE gate: exercises `GpuRenderer::render_input_cached` — the
// per-frame PRESENTATION hot path that, on an UNCHANGED frame, re-presents the
// previous frame's already-read-back pixels with ZERO GPU work (no encode, no
// submit, no `device.poll`, no readback). The hard contract is BYTE-IDENTITY:
// the pixels handed back on a gate-hit must equal EXACTLY what a fresh full GPU
// render of that same input produces, so the optimisation is invisible.
//
// The rest of the GPU suite only ever drives the owned-`Frame` `render`/
// `render_input` path; this is the only test that drives `render_input_cached`
// on the GPU, so it is also the only thing that can catch a stale gate cache.
//
// What it checks, on ONE reused GpuRenderer at FIXED dims across a multi-frame
// sequence (prompt, typing, blink toggles, cursor move, selection, scroll, full
// repaints):
//   (a) EVERY gate-hit frame's borrowed pixels are byte-identical to a FRESH
//       full GPU render of that same input on a SEPARATE GpuRenderer (proves the
//       cache is not stale — a hit returns the true current frame's pixels);
//   (b) genuinely-unchanged frames actually TAKE the gate (gate-hit counter
//       advances), so the optimisation is exercised, not silently dead;
//   (c) a gate-hit immediately followed by a one-cell change repaints correctly
//       (the next frame misses and its pixels match a fresh render).
//
// Gated: if there is no GPU or no system font, the test no-ops (returns).

// PHOSPHOR rain fixture (a real MatrixRain drive) for the settled/drained
// rain gate tests below; each consuming binary uses its own subset.
#[allow(dead_code)]
mod rain_common;

use std::time::Instant;

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_effects::matrix_rain::RainVisibility;
use aterm_gpu::GpuRenderer;
use aterm_render::{RenderInput, Theme};
use rain_common::RainScene;

const ROWS: usize = 8;
const COLS: usize = 24;

/// Build a FRESH GpuRenderer (or skip-marker) at the suite's standard px/theme.
fn fresh_gpu() -> Option<GpuRenderer> {
    match GpuRenderer::new(18.0, Theme::default()) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            None
        }
    }
}

/// Render `input` from scratch on a brand-new GpuRenderer (full encode +
/// readback), with the given cursor blink/override state, and return the owned
/// pixels. This is the ground-truth "what the GPU draws for this input" with NO
/// cache in play — the oracle the gate-hit borrow must match byte-for-byte.
fn fresh_render(input: &RenderInput, blink: bool, override_: Option<CursorStyle>) -> Vec<u32> {
    let mut g = fresh_gpu().expect("GPU was available a moment ago");
    let mut win = aterm_gpu::WindowGpu::new();
    g.set_cursor_blink_phase(blink);
    g.set_cursor_style_override(override_);
    g.render_input(&mut win, input, None).pixels
}

#[test]
fn gpu_dirty_gate_byte_identical() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();

    // A scripted sequence of (label, mutate-the-terminal closure, blink phase,
    // style override). Each step extracts a RenderInput at FIXED dims and feeds
    // it through the ONE reused renderer's `render_input_cached`. Steps that do
    // not change anything render-relevant should take the gate.
    let mut term = Terminal::new(ROWS as u16, COLS as u16);

    // Each step: (description, expect_gate_hit_or_none).
    //   - `Some(true)`  => this frame MUST take the gate (genuinely unchanged),
    //   - `Some(false)` => this frame MUST miss (something changed),
    //   - `None`        => don't assert hit/miss (e.g. the very first frame, or
    //                      where exactness of the predicate isn't the point).
    // The closure mutates `term` and/or returns blink/override for the frame.

    struct Step {
        desc: &'static str,
        // Mutate the terminal (process bytes, move cursor, select, scroll…).
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
        // 1. First paint: a shell prompt. First frame always MISSES (no cache).
        step("prompt", |t| t.process(b"$ "), true, None, Some(false)),
        // 2. Re-present the SAME frame, same blink — must GATE-HIT.
        step("idle (unchanged)", |_| {}, true, None, Some(true)),
        // 3. Idle again — still a hit.
        step("idle again", |_| {}, true, None, Some(true)),
        // 4. Blink OFF (cursor at end of "$ ", default block → blinking? the
        //    terminal's DECSCUSR default decides; either way a phase change that
        //    changes shown-ness must MISS, and an unchanged-shown-ness one HITs.
        //    We don't assert here — just exercise the blink path.
        step("blink off", |_| {}, false, None, None),
        // 5. Blink back ON — exercise again, no assertion.
        step("blink on", |_| {}, true, None, None),
        // 6. Type a character — content changes, MUST miss.
        step("type 'l'", |t| t.process(b"l"), true, None, Some(false)),
        // 7. Idle after typing — MUST hit.
        step("idle after type", |_| {}, true, None, Some(true)),
        // 8. Type more.
        step("type 's'", |t| t.process(b"s"), true, None, Some(false)),
        // 9. Newline + lots of output: enough lines to push content OFF the top
        //    into scrollback (so a later `scroll_display` genuinely changes the
        //    display offset). Content changes ⇒ miss.
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
        // 10. Idle — hit.
        step("idle after output", |_| {}, true, None, Some(true)),
        // 11. Style override (unfocused HollowBlock) — cursor style changes, miss.
        step(
            "focus lost",
            |_| {},
            true,
            Some(CursorStyle::HollowBlock),
            Some(false),
        ),
        // 12. Idle unfocused — hit.
        step(
            "idle unfocused",
            |_| {},
            true,
            Some(CursorStyle::HollowBlock),
            Some(true),
        ),
        // 13. Focus regained — override cleared, miss.
        step("focus gained", |_| {}, true, None, Some(false)),
        // 14. Start a selection — selection changes, miss.
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
        // 15. Idle with selection — hit.
        step("idle selected", |_| {}, true, None, Some(true)),
        // 16. Clear selection — miss.
        step(
            "clear selection",
            |t| t.text_selection_mut().clear(),
            true,
            None,
            Some(false),
        ),
        // 17. Scroll back into history — display_offset changes, miss.
        step(
            "scroll back",
            |t| t.scroll_display(1),
            true,
            None,
            Some(false),
        ),
        // 18. Idle scrolled — hit.
        step("idle scrolled", |_| {}, true, None, Some(true)),
        // 19. Scroll to bottom — miss.
        step(
            "scroll to bottom",
            |t| t.scroll_display(-1),
            true,
            None,
            Some(false),
        ),
        // 20. Full repaint of a new screen — miss.
        step(
            "clear + new screen",
            |t| t.process(b"\x1b[2J\x1b[Hready> "),
            true,
            None,
            Some(false),
        ),
        // 21. Idle on new screen — hit.
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

        // Snapshot the borrowed pixels now (the borrow ends when `gpu` is next
        // mutated/read, and `fresh_render` builds a separate renderer anyway).
        let got: Vec<u32> = view.pixels().to_vec();
        let (gw, gh) = (view.width(), view.height());
        drop(view);
        let took_gate = gpu.gate_hits() > hits_before;

        // Dimensions are fixed for the whole sequence.
        assert_eq!(
            (gw, gh),
            (COLS * gpu.cell_size().0, ROWS * gpu.cell_size().1),
            "step {i} ({}): bad dims",
            s.desc
        );

        // (a) BYTE-IDENTITY: whether this frame hit or missed, the pixels handed
        // back MUST equal a fresh full GPU render of the SAME input + cursor
        // state. On a hit this proves the cache is not stale; on a miss it proves
        // the gate's stored frame is the real render.
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

        // (b) the gate must fire exactly where the test says it should.
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

    // The optimisation must actually be EXERCISED: we asserted several
    // `Some(true)` frames above, so the gate genuinely fired on real frames.
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

/// A gate-hit followed by a ONE-CELL change must repaint correctly: the changed
/// frame misses and its pixels match a fresh render of the changed input
/// (i.e. the cache does not "stick" on the stale prior frame). This isolates the
/// hit→change boundary that a naive gate could get wrong.
#[test]
fn gpu_dirty_gate_one_cell_change_after_hit() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"ABC");
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    // Frame 1: first paint (miss, fills the cache).
    let in1 = term.cell_frame(ROWS, COLS);
    let _ = gpu.render_input_cached(&mut win, &in1);
    assert_eq!(gpu.gate_misses(), 1, "first frame must miss");

    // Frame 2: identical input → GATE-HIT.
    let in2 = term.cell_frame(ROWS, COLS);
    let hits_before = gpu.gate_hits();
    let v2 = gpu.render_input_cached(&mut win, &in2);
    let got2 = v2.pixels().to_vec();
    drop(v2);
    assert!(
        gpu.gate_hits() > hits_before,
        "second (unchanged) frame must take the gate"
    );
    // Hit pixels equal a fresh render of the unchanged input.
    assert!(
        got2 == fresh_render(&in2, true, None),
        "gate-hit pixels diverge from fresh render"
    );

    // Frame 3: change ONE cell ('C' → 'D'). Must MISS and match a fresh render
    // of the CHANGED input — not the stale "ABC" frame.
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
    // And the changed frame is genuinely different from the prior cached frame.
    assert!(
        got3 != got2,
        "one-cell change produced an identical framebuffer (suspicious)"
    );

    // Frame 4: idle on the changed screen → GATE-HIT again, byte-identical.
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

/// PHOSPHOR rain through the dirty gate (design §10): a SETTLED (fp-stable)
/// rain frame must take the gate, and a DRAINED field must settle back to
/// gate-hits — both byte-identical to a fresh full render. Sequence on one
/// reused renderer:
///   A. a genuine raining frame (real `MatrixRain` emission) — MISS, primes;
///   B. the SAME engine tick re-emitted (fp-stable, non-empty quads, same
///      atlas version) — must GATE-HIT and equal a fresh render byte-for-byte
///      (settled rain costs zero GPU work);
///   C. the engine drains (hidden pane): empty channels — a real change, MISS,
///      and the vacated rain pixels must be fully erased (== fresh render);
///   D. drained again (fp stable at 0, `is_active()` false — the host timer
///      disarms) — GATE-HIT, byte-identical.
#[test]
fn gpu_dirty_gate_settled_and_drained_rain() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[?25l$ rain"); // hidden cursor: no blink in the mix
    let base = term.cell_frame(ROWS, COLS);
    let mut scene = RainScene::new(ROWS, COLS, gpu.cell_size(), &base);
    let fp_a = scene.drive_until_raining();

    // A: raining — first frame, MISS, fills the gate cache.
    let mut in_a = term.cell_frame(ROWS, COLS);
    scene.apply(&mut in_a);
    let _ = gpu.render_input_cached(&mut win, &in_a);
    assert_eq!(gpu.gate_misses(), 1, "first frame must miss");

    // B: settled — the same engine tick re-emits the identical field.
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

    // C: drain — a hidden pane empties the field within one tick.
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

    // D: drained steady state — fp stays 0, the gate HITS, bytes identical.
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

/// Diagnostic (run with `--ignored --nocapture`): measure the per-frame cost of
/// an IDLE/blink frame BEFORE the gate (the full `encode + readback` path) vs.
/// AFTER (a gate-hit through `render_input_cached`). Not an assertion — prints
/// the two costs so the win is quantified. Skips cleanly without a GPU.
#[test]
#[ignore = "diagnostic benchmark; run with --ignored --nocapture"]
fn gpu_dirty_gate_idle_cost() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ idle frame cost benchmark");
    gpu.set_cursor_blink_phase(true);
    let input = term.cell_frame(ROWS, COLS);

    // Prime the gate cache.
    let _ = gpu.render_input_cached(&mut win, &input);

    const N: u32 = 200;

    // BEFORE: every idle frame did a full GPU encode + blocking readback.
    let t = Instant::now();
    for _ in 0..N {
        let _ = gpu.render_input(&mut win, &input, None); // owned Frame: full encode + readback
    }
    let before_us = t.elapsed().as_secs_f64() * 1e6 / f64::from(N);

    // AFTER: an unchanged idle frame takes the gate — zero GPU work.
    let hits0 = gpu.gate_hits();
    let t = Instant::now();
    for _ in 0..N {
        let v = gpu.render_input_cached(&mut win, &input);
        std::hint::black_box(v.pixels().len());
    }
    let after_us = t.elapsed().as_secs_f64() * 1e6 / f64::from(N);
    assert_eq!(
        gpu.gate_hits() - hits0,
        u64::from(N),
        "all idle frames should hit the gate"
    );

    eprintln!(
        "idle frame cost: BEFORE (encode+readback) = {before_us:.1} us/frame, \
         AFTER (gate-hit) = {after_us:.3} us/frame, speedup = {:.0}x",
        before_us / after_us.max(0.0001),
    );
}
