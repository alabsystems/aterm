// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::pipeline_table::{ALL_PIPELINES, BlendComponent};

/// Every row's two entry points compile from the twin and its states
/// build on the device: the arm can draw every row the table lists.
#[test]
fn every_table_row_compiles_from_its_twin_and_builds() {
    let dev = super::super::test_device();
    let mut built = Vec::new();
    for p in ALL_PIPELINES {
        let spec = p.spec();
        let pipe = dev
            .pipeline(spec)
            .unwrap_or_else(|e| panic!("row {}: {e}", p.name()));
        assert_eq!(
            pipe.input.is_some(),
            spec.vertex != crate::pipeline_table::VertexLayout::None,
            "{}",
            p.name()
        );
        built.push(pipe);
    }
    assert_eq!(built.len(), crate::pipeline_table::PIPELINE_COUNT);
}

/// Every row's two entry points also compile with the flags a shipped build passes (FXC's
/// optimizer on, which this build's own compiles skip), and the row's pipeline builds from
/// that bytecode.
/// NEGATIVE CONTROL: some row's release bytecode differs from the same compile with the
/// optimizer skipped, asserted below, so the release flags run the optimizer in every build
/// (`RELEASE_FLAGS` gaining `SKIP_OPTIMIZATION` fails it).
#[test]
fn every_table_row_compiles_and_builds_with_the_release_flags() {
    let dev = super::super::test_device();
    let unoptimized = shaders::RELEASE_FLAGS | d3d::D3DCOMPILE_SKIP_OPTIMIZATION;
    let mut optimized = false;
    for p in ALL_PIPELINES {
        let spec = p.spec();
        let compile = |entry, stage, flags| {
            shaders::compile_source(
                shaders::source(spec.library),
                shaders::file_name(spec.library),
                entry,
                stage,
                flags,
            )
            .unwrap_or_else(|e| panic!("row {}: {e}", p.name()))
        };
        let vs = compile(spec.vs, Stage::Vertex, shaders::RELEASE_FLAGS);
        let ps = compile(spec.fs, Stage::Pixel, shaders::RELEASE_FLAGS);
        optimized |= vs != compile(spec.vs, Stage::Vertex, unoptimized)
            || ps != compile(spec.fs, Stage::Pixel, unoptimized);
        dev.pipeline_from(spec, &vs, &ps)
            .unwrap_or_else(|e| panic!("row {} from release bytecode: {e}", p.name()));
    }
    assert!(optimized, "the release flags changed no row's bytecode");
}

/// A broken twin is refused with the compiler's log, not drawn blank: the
/// negative control for the compile check above.
#[test]
fn a_twin_that_does_not_compile_is_refused_with_the_compilers_log() {
    let _dev = super::super::test_device();
    let src = shaders::source(crate::pipeline_table::ShaderLibrary::Cell);
    let good = "return float4(s2l(i.color.xyz), i.color.w);";
    assert!(src.contains(good), "the control edits a line the twin has");
    let broken = src.replace(good, "return undeclared;");
    let err = shaders::compile_source(&broken, "cell.hlsl", "fs_bg", Stage::Pixel, shaders::FLAGS)
        .expect_err("must not compile");
    assert!(err.contains("does not compile"), "{err}");
    assert!(
        err.contains("undeclared"),
        "the compiler's log is carried: {err}"
    );
    shaders::compile_source(src, "cell.hlsl", "fs_bg", Stage::Pixel, shaders::FLAGS)
        .expect("the unedited twin compiles");
}

/// The alpha half takes the alpha form of a colour factor (D3D11 refuses
/// a colour factor there): the screen operator's `OneMinusSrc`.
#[test]
fn the_alpha_half_takes_the_alpha_form_of_a_colour_factor() {
    let screen = Blend {
        color: BlendComponent {
            src: Factor::One,
            dst: Factor::OneMinusSrc,
            op: BlendOp::Add,
        },
        alpha: BlendComponent {
            src: Factor::One,
            dst: Factor::OneMinusSrc,
            op: BlendOp::Add,
        },
    };
    let d = d3d_blend(Some(screen), WriteMask::Color);
    assert_eq!(d.DestBlend, d3d::D3D11_BLEND_INV_SRC_COLOR);
    assert_eq!(d.DestBlendAlpha, d3d::D3D11_BLEND_INV_SRC_ALPHA);
    assert_eq!(d.RenderTargetWriteMask, 7);
    assert_eq!(d.BlendEnable, 1);
    assert_eq!(d3d_blend(None, WriteMask::All).BlendEnable, 0);
}
