// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Demand-driven GPU effect pipeline compilation tests.

use aterm_core::terminal::Terminal;
use aterm_render::{GlowQuad, Theme};

use crate::common::backends;

fn glow_quad(cw: usize, ch: usize) -> GlowQuad {
    GlowQuad {
        row: 1,
        x: cw as u16,
        y: ch as u16,
        w: cw as u16,
        h: ch as u16,
        color: 0x0020_2020,
        alpha: 0,
        color2: 0x0020_2020,
        alpha2: 0,
    }
}

#[test]
fn an_effect_free_frame_compiles_nothing_and_one_glow_quad_compiles_only_glow_add() {
    let theme = Theme::default();
    let (cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (4usize, 16usize);
    let (cw, ch) = cpu.cell_size();
    let mut input = Terminal::new(rows as u16, cols as u16).cell_frame(rows, cols);

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
    eprintln!(
        "MEASURED glow_add demand build = {:.2} ms",
        build_ns as f64 / 1.0e6
    );

    let _ = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        gpu.effect_pipeline_build_cost().0,
        1,
        "a resident pipeline must not be rebuilt on the next frame"
    );
}

#[test]
fn the_warm_up_seam_builds_every_slot_off_the_frame_path() {
    let theme = Theme::default();
    let (_, mut gpu) = backends(18.0, theme);
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

#[test]
fn an_fx_frame_demands_exactly_its_rows() {
    use std::sync::Arc;

    use aterm_core::render::{
        FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace, FxSprite,
        FxTexels, PxRect,
    };
    use aterm_gpu::EffectPipeline;

    let rect = FxItem::Rect(FxRect {
        x: 4,
        y: 4,
        w: 40,
        h: 12,
        ink: 0,
    });
    let sprite = FxItem::Sprite(FxSprite {
        x: 4,
        y: 4,
        dw: 40,
        dh: 12,
        flip_x: false,
        art: Arc::new(
            FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![200, 90, 40, 160]).expect("sized"),
        ),
        ink: 0,
        clip: PxRect::EVERYWHERE,
    });
    let layer = |item: &FxItem, slot, blend, bloom| {
        let mut l = FxLayer::new(
            1,
            Arc::new(FxContent::new(
                vec![item.clone()],
                vec![FxInk::add(0x0030_2010); 2],
            )),
            slot,
            FxSpace::Window,
            blend,
        );
        l.bloom = bloom;
        l
    };
    let cases = [
        (
            "exact",
            layer(&rect, FxSlot::Over, FxBlend::Exact, false),
            &[EffectPipeline::Fx][..],
        ),
        (
            "linear",
            layer(&sprite, FxSlot::Under, FxBlend::Linear, false),
            &[EffectPipeline::FxLinear][..],
        ),
        (
            "bloom",
            layer(&rect, FxSlot::Glow, FxBlend::Exact, true),
            &[EffectPipeline::Fx, EffectPipeline::GlowAdd][..],
        ),
    ];
    let (rows, cols) = (4usize, 16usize);
    for (name, l, rows_wanted) in cases {
        let (_, mut gpu) = backends(18.0, Theme::default());
        let mut win = aterm_gpu::WindowGpu::new();
        let mut input = Terminal::new(rows as u16, cols as u16).cell_frame(rows, cols);
        input.fx_layers = vec![l];
        let _ = gpu.render_input(&mut win, &input, None);
        assert!(gpu.frame_shape().fx_instances > 0, "{name}: the layer drew");
        let mut want = [false; aterm_gpu::EFFECT_PIPELINE_COUNT];
        for &p in rows_wanted {
            want[p as usize] = true;
        }
        assert_eq!(
            gpu.effect_pipelines_resident(),
            want,
            "{name}: the frame demanded {:?}",
            aterm_gpu::EFFECT_PIPELINE_NAMES
                .iter()
                .zip(gpu.effect_pipelines_resident())
                .filter_map(|(n, built)| built.then_some(*n))
                .collect::<Vec<_>>()
        );
    }
}
