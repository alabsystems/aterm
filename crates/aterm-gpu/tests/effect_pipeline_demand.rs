// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE DEMAND-DRIVEN EFFECT PIPELINES, END TO END ON A REAL DEVICE.
//
// `renderer.rs` already unit-tests `frame_effect_pipelines` — the pure predicate
// that says WHICH pipelines one frame binds. What no test covered is that
// `encode_frame` actually consults it, that a frame carrying no effect stream
// therefore compiles nothing on real hardware, and what the first frame that
// DOES carry one pays. This binary answers all three against a live adapter, and
// prints the per-slot compile cost so the launch budget has a measured number
// instead of a remembered one.
//
// Gated: no GPU / no system font -> the test no-ops (returns).

use aterm_core::terminal::Terminal;
use aterm_render::{GlowQuad, Theme};

mod common;
use common::backends;

/// The one glow quad a cursor effect publishes, in cell (1,1).
fn glow_quad(cw: usize, ch: usize) -> GlowQuad {
    GlowQuad {
        row: 1,
        x: cw as u16,
        y: ch as u16,
        w: cw as u16,
        h: ch as u16,
        color: 0x0020_2020,

        // ADDITIVE light (see `GlowQuad::alpha`).
        alpha: 0,
    }
}

/// A frame with the shipped Windows defaults — no cursor trail, no glow, no
/// rain, no sparkle words, no sprites — must leave EVERY effect pipeline
/// unbuilt, and the first frame that publishes one glow quad must build exactly
/// `glow_add` and nothing else.
///
/// This is the standing proof of the demand gate at the seam that matters. A
/// launch compiling the nine effect pipelines eagerly cost 136.13 ms of every
/// window open on this machine, all of it on time-to-first-present, for pixels
/// the shipped config never draws.
#[test]
fn an_effect_free_frame_compiles_nothing_and_one_glow_quad_compiles_only_glow_add() {
    let theme = Theme::default();
    let Some((cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (4usize, 16usize);
    let (cw, ch) = cpu.cell_size();
    let mut input = Terminal::new(rows as u16, cols as u16).cell_frame(rows, cols);

    // FRAME 1 — the shipped default: a plain grid with an opaque cursor.
    let _ = gpu.render_input(&mut win, &input, None);
    let resident = gpu.effect_pipelines_resident();
    let (builds, build_ns) = gpu.effect_pipeline_build_cost();
    assert_eq!(
        resident,
        [false; aterm_gpu::EFFECT_PIPELINE_COUNT],
        "an effect-free frame compiled {:?}",
        aterm_gpu::EFFECT_PIPELINE_NAMES
            .iter()
            .zip(resident)
            .filter_map(|(name, built)| built.then_some(*name))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        (builds, build_ns),
        (0, 0),
        "an effect-free frame must spend no time in the pipeline builder"
    );

    // FRAME 2 — one glow quad, exactly what a live cursor effect publishes.
    input.cursor_glow_add.push(glow_quad(cw, ch));
    let _ = gpu.render_input(&mut win, &input, None);
    let resident = gpu.effect_pipelines_resident();
    let (builds, build_ns) = gpu.effect_pipeline_build_cost();
    let mut want = [false; aterm_gpu::EFFECT_PIPELINE_COUNT];
    want[aterm_gpu::EffectPipeline::GlowAdd as usize] = true;
    assert_eq!(
        resident,
        want,
        "one glow quad must demand exactly `glow_add`; it demanded {:?}",
        aterm_gpu::EFFECT_PIPELINE_NAMES
            .iter()
            .zip(resident)
            .filter_map(|(name, built)| built.then_some(*name))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        builds, 1,
        "the demand pass must build each slot exactly once"
    );
    // THE MEASURED NUMBER this bundle is about: what a launch pays the first
    // time a frame carries a glow stream. On an effects-off config no frame
    // should carry one at all, so a launch should pay none of it.
    eprintln!(
        "MEASURED glow_add demand build = {:.2} ms",
        build_ns as f64 / 1.0e6
    );

    // FRAME 3 — the same glow stream again: idempotent, no second compile.
    let _ = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        gpu.effect_pipeline_build_cost().0,
        1,
        "a resident pipeline must not be rebuilt on the next frame"
    );
}

/// What the four expensive slots cost, so the warm-up seam's existence is
/// justified by a measurement rather than by memory. `warm_effect_pipelines`
/// builds every slot off the frame path; this prints the total and the count.
///
/// Deliberately NOT an assertion on the milliseconds — that would be a
/// machine-speed test. The assertion is that the warm-up leaves the whole set
/// resident, which is the property the config-apply seam relies on.
#[test]
fn the_warm_up_seam_builds_every_slot_off_the_frame_path() {
    let theme = Theme::default();
    let Some((_, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    assert_eq!(
        gpu.effect_pipeline_build_cost().0,
        0,
        "construction must build no effect pipeline"
    );
    gpu.warm_effect_pipelines();
    let (builds, build_ns) = gpu.effect_pipeline_build_cost();
    assert_eq!(
        gpu.effect_pipelines_resident(),
        [true; aterm_gpu::EFFECT_PIPELINE_COUNT],
        "the warm-up must leave every effect pipeline resident"
    );
    assert_eq!(builds as usize, aterm_gpu::EFFECT_PIPELINE_COUNT);
    eprintln!(
        "MEASURED warm_effect_pipelines: {builds} pipelines in {:.2} ms \
         (this is what an effects-off launch no longer pays)",
        build_ns as f64 / 1.0e6
    );
    gpu.warm_effect_pipelines();
    assert_eq!(
        gpu.effect_pipeline_build_cost().0,
        builds,
        "the warm-up must be idempotent"
    );
}
