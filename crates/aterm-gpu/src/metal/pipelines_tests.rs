// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::metal::test_support::device;
use crate::pipeline_table::ALL_PIPELINES;
use std::collections::HashMap;

#[test]
fn every_table_row_builds_a_metal_pipeline() {
    let dev = device();
    let libs: HashMap<ShaderLibrary, Library> = ShaderLibrary::ALL
        .into_iter()
        .map(|lib| {
            let compiled = compile_library(&dev, lib)
                .unwrap_or_else(|e| panic!("{}.metal failed to compile:\n{e}", lib.name()));
            (lib, compiled)
        })
        .collect();
    for present in [PixelFormat::Bgra8Unorm, PixelFormat::Rgba8Unorm] {
        for row in ALL_PIPELINES {
            let spec = row.spec();
            let lib = &libs[&spec.library];
            build(&dev, lib, spec, present).unwrap_or_else(|e| {
                panic!(
                    "`{}` ({}) failed to build on a {present:?} swapchain: {e}",
                    row.name(),
                    spec.label
                )
            });
        }
    }
    if super::super::fetch::capable(&dev) {
        build(
            &dev,
            &libs[&ShaderLibrary::TrayPrims],
            &crate::pipeline_table::TRAY_PRIMS,
            PixelFormat::Bgra8Unorm,
        )
        .unwrap_or_else(|e| panic!("the tray prims pipeline failed to build: {e}"));
        build(
            &dev,
            &libs[&ShaderLibrary::TrayPrims],
            &crate::pipeline_table::TRAY_LAYER,
            PixelFormat::Bgra8Unorm,
        )
        .unwrap_or_else(|e| panic!("the tray layer pipeline failed to build: {e}"));
        for twin in crate::pipeline_table::FetchTwin::ALL {
            build(
                &dev,
                &libs[&ShaderLibrary::Cell],
                &twin.spec(),
                PixelFormat::Bgra8Unorm,
            )
            .unwrap_or_else(|e| panic!("{twin:?}'s fetch twin failed to build: {e}"));
        }
    }
}

#[test]
fn metal_constants_match_the_sdk_headers() {
    for (f, raw, name) in [
        (AttrFormat::Uint8x4, 3, "MTLVertexFormatUChar4"),
        (AttrFormat::Unorm8x4, 9, "MTLVertexFormatUChar4Normalized"),
        (AttrFormat::Uint16x4, 15, "MTLVertexFormatUShort4"),
        (AttrFormat::Float32x4, 31, "MTLVertexFormatFloat4"),
        (AttrFormat::Uint32, 36, "MTLVertexFormatUInt"),
        (AttrFormat::Uint16x2, 13, "MTLVertexFormatUShort2"),
        (AttrFormat::Sint16x2, 16, "MTLVertexFormatShort2"),
    ] {
        assert_eq!(metal_attr_format(f) as usize, raw, "{name}");
    }
    for (t, raw, name) in [
        (Topology::TriangleList, 3, "MTLPrimitiveTypeTriangle"),
        (Topology::TriangleStrip, 4, "MTLPrimitiveTypeTriangleStrip"),
    ] {
        assert_eq!(metal_primitive_type(t) as usize, raw, "{name}");
    }
    for (f, raw, name) in [
        (Factor::Zero, 0, "MTLBlendFactorZero"),
        (Factor::One, 1, "MTLBlendFactorOne"),
        (Factor::OneMinusSrc, 3, "MTLBlendFactorOneMinusSourceColor"),
        (Factor::SrcAlpha, 4, "MTLBlendFactorSourceAlpha"),
        (
            Factor::OneMinusSrcAlpha,
            5,
            "MTLBlendFactorOneMinusSourceAlpha",
        ),
    ] {
        assert_eq!(metal_factor(f) as usize, raw, "{name}");
    }
}

#[test]
fn only_replace_rows_disable_blending() {
    for row in ALL_PIPELINES {
        let spec = row.spec();
        let disabled = metal_blend(spec.blend).is_none();
        let is_replace = spec.blend.is_none_or(Blend::is_replace);
        assert_eq!(
            disabled,
            is_replace,
            "`{}` blending-disabled={disabled} but is_replace={is_replace}",
            row.name()
        );
    }
}
