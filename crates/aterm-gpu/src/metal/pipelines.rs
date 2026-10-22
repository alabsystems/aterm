// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Builds `MTLRenderPipelineState`s from [`crate::pipeline_table`], the same
//! rows `renderer.rs` builds its wgpu pipelines from.
//!
//! The mappings off the table's neutral enums are total, so a variant added to
//! the table fails to compile here until Metal spells it. Metal expresses
//! REPLACE (`(One, Zero)` on both halves) as blending disabled, so
//! [`metal_blend`] maps a REPLACE row to `None`, decided by
//! [`crate::pipeline_table::Blend::is_replace`], not by name.

use super::ffi::{
    self, BlendFactor, BlendOperation, BlendState, ColorWriteMask, Device, Library, Obj,
    PixelFormat, RenderPipelineDescriptor, VertexDescriptor, VertexFormat,
};
#[cfg(test)]
use crate::pipeline_table::Topology;
use crate::pipeline_table::{
    AttrFormat, Blend, BlendOp, Factor, PipelineSpec, ShaderLibrary, TargetRole, VertexLayout,
    WriteMask,
};

/// The Metal spelling of one [`Factor`].
#[inline]
const fn metal_factor(f: Factor) -> BlendFactor {
    match f {
        Factor::Zero => BlendFactor::Zero,
        Factor::One => BlendFactor::One,
        Factor::SrcAlpha => BlendFactor::SourceAlpha,
        Factor::OneMinusSrcAlpha => BlendFactor::OneMinusSourceAlpha,
        Factor::OneMinusSrc => BlendFactor::OneMinusSourceColor,
    }
}

/// The Metal blend state for
/// [`RenderPipelineDescriptor::set_color_attachment`], or `None` for no
/// blending: a row with `blend: None` or a row whose blend is REPLACE
/// ([`Blend::is_replace`]).
#[inline]
pub(crate) const fn metal_blend(blend: Option<Blend>) -> Option<BlendState> {
    let Some(b) = blend else {
        return None;
    };
    if b.is_replace() {
        return None;
    }
    Some(BlendState {
        source_rgb: metal_factor(b.color.src),
        destination_rgb: metal_factor(b.color.dst),
        rgb_operation: metal_operation(b.color.op),
        source_alpha: metal_factor(b.alpha.src),
        destination_alpha: metal_factor(b.alpha.dst),
        alpha_operation: metal_operation(b.alpha.op),
    })
}

/// The Metal spelling of one [`BlendOp`].
#[inline]
const fn metal_operation(op: BlendOp) -> BlendOperation {
    match op {
        BlendOp::Add => BlendOperation::Add,
    }
}

/// The Metal spelling of one [`WriteMask`].
#[inline]
pub(crate) const fn metal_write_mask(m: WriteMask) -> ColorWriteMask {
    match m {
        WriteMask::Color => ColorWriteMask::COLOR,
        WriteMask::All => ColorWriteMask::ALL,
    }
}

/// The Metal spelling of one [`AttrFormat`].
#[inline]
const fn metal_attr_format(f: AttrFormat) -> VertexFormat {
    match f {
        AttrFormat::Uint16x4 => VertexFormat::UShort4,
        AttrFormat::Unorm8x4 => VertexFormat::UChar4Normalized,
        AttrFormat::Uint8x4 => VertexFormat::UChar4,
        AttrFormat::Float32x4 => VertexFormat::Float4,
        AttrFormat::Uint32 => VertexFormat::UInt,
        AttrFormat::Sint16x2 => VertexFormat::Short2,
        AttrFormat::Uint16x2 => VertexFormat::UShort2,
    }
}

/// The colour-attachment format for a row's [`TargetRole`]. The offscreen pair
/// is `format_plan`'s native choice (`Rgba8Unorm` with an `Rgba8UnormSrgb`
/// alias view), view-compatible in Metal as in wgpu. `present` is the live
/// swapchain format (`Bgra8Unorm` or `Rgba8Unorm` from `pick_surface_format`).
///
/// Hand-written because no wgpu type may enter this module; the Metal-vs-wgpu
/// byte differentials in `metal::tests` catch a role it resolves differently.
#[inline]
pub(crate) const fn metal_format(role: TargetRole, present: PixelFormat) -> PixelFormat {
    match role {
        TargetRole::OffscreenSrgb => PixelFormat::Rgba8UnormSrgb,
        TargetRole::OffscreenUnorm => PixelFormat::Rgba8Unorm,
        TargetRole::Edr => PixelFormat::Rgba16Float,
        TargetRole::Present => present,
    }
}

/// The `MTLVertexDescriptor` for a row's instance stream, or `None` for a
/// `[[vertex_id]]`-only pass. Attributes and stride come from the table's
/// [`crate::pipeline_table::VertexLayoutSpec`].
///
/// The stream lives at [`ffi::INSTANCE_STREAM_SLOT`], never 0: vertex functions
/// taking `[[stage_in]]` declare uniforms at `[[buffer(0)]]` in the same
/// argument table.
pub(crate) fn metal_vertex_descriptor(layout: VertexLayout) -> Option<VertexDescriptor> {
    let spec = layout.spec()?;
    let vd = VertexDescriptor::new()?;
    for a in spec.attrs {
        vd.attribute(
            a.location as usize,
            metal_attr_format(a.format),
            a.offset as usize,
            ffi::INSTANCE_STREAM_SLOT,
        );
    }
    vd.layout_per_instance(ffi::INSTANCE_STREAM_SLOT, spec.stride as usize);
    Some(vd)
}

/// The `MTLPrimitiveType` for a row's [`Topology`]. Topology is pipeline state
/// in wgpu but a `drawPrimitives:` argument in Metal; [`ffi::DrawCall`] takes
/// its `primitive` from here. The tray's 4-vertex strip drawn as a list would
/// cover half the card.
#[cfg(test)]
pub(crate) const fn metal_primitive_type(t: Topology) -> ffi::PrimitiveType {
    match t {
        Topology::TriangleList => ffi::PrimitiveType::Triangle,
        Topology::TriangleStrip => ffi::PrimitiveType::TriangleStrip,
    }
}

/// Build the real `MTLRenderPipelineState` for one table row.
///
/// `library` must be the compiled MSL for `spec.library`; `present` is the
/// swapchain format for a [`TargetRole::Present`] row and is ignored otherwise.
pub(crate) fn build(
    device: &Device,
    library: &Library,
    spec: &PipelineSpec,
    present: PixelFormat,
) -> Result<Obj, String> {
    let vs = library
        .function(spec.vs)
        .ok_or_else(|| format!("{} exports no `{}`", spec.label, spec.vs))?;
    let fs = library
        .function(spec.fs)
        .ok_or_else(|| format!("{} exports no `{}`", spec.label, spec.fs))?;

    let desc = RenderPipelineDescriptor::new()
        .ok_or_else(|| "MTLRenderPipelineDescriptor allocation failed".to_owned())?;
    desc.set_vertex_function(&vs);
    desc.set_fragment_function(&fs);
    if let Some(vd) = metal_vertex_descriptor(spec.vertex) {
        desc.set_vertex_descriptor(&vd);
    }
    desc.set_color_attachment(
        metal_format(spec.target, present),
        metal_write_mask(spec.write_mask),
        metal_blend(spec.blend),
    );
    device.new_render_pipeline(&desc)
}

/// One MSL library, compiled with the shipped [`ffi::CompileOptions`]
/// (`preserveInvariance`, MSL 2.3 pinned at the macOS 11 floor).
pub(crate) fn compile_library(device: &Device, library: ShaderLibrary) -> Result<Library, String> {
    let opts = ffi::CompileOptions::aterm_default()
        .ok_or_else(|| "MTLCompileOptions allocation failed".to_owned())?;
    device.new_library_with_options(super::shaders::source(library), &opts)
}

/// [`compile_library`] for the library one row names. Sweeps compile per
/// library and share the result.
#[cfg(test)]
pub(crate) fn compile(device: &Device, spec: &PipelineSpec) -> Result<Library, String> {
    compile_library(device, spec.library)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metal::test_support::device;
    use crate::pipeline_table::ALL_PIPELINES;
    use std::collections::HashMap;

    /// Every table row builds a real `MTLRenderPipelineState` with the
    /// renderer's own entry points, blend, write mask, formats and vertex
    /// layouts, for both swapchain formats `pick_surface_format` can choose.
    #[test]
    fn every_table_row_builds_a_metal_pipeline() {
        let dev = device();
        // Six libraries serve twenty rows: compile each once.
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
    }

    /// The FFI constants, pinned against the SDK headers
    /// (`MTLVertexDescriptor.h`, `MTLRenderCommandEncoder.h`,
    /// `MTLRenderPipeline.h`). A wrong value still builds a pipeline:
    /// `UChar4 == 3` in place of `UChar4Normalized == 9` turns `Unorm8x4`
    /// colours from 0..1 into 0..255, and a transposed blend factor swaps the
    /// blend equation.
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

    /// REPLACE, however the table spells it, reaches Metal as blending
    /// disabled, and nothing else does.
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
}
