// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Builds `MTLRenderPipelineState`s from [`crate::pipeline_table`].

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

/// The Metal blend state for `blend`, or `None` when blending is disabled or [`Blend::is_replace`].
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

/// The colour-attachment format for `role`, using `present` for [`TargetRole::Present`].
#[inline]
pub(crate) const fn metal_format(role: TargetRole, present: PixelFormat) -> PixelFormat {
    match role {
        TargetRole::OffscreenSrgb => PixelFormat::Rgba8UnormSrgb,
        TargetRole::OffscreenUnorm => PixelFormat::Rgba8Unorm,
        TargetRole::Edr => PixelFormat::Rgba16Float,
        TargetRole::Present => present,
    }
}

/// The `MTLVertexDescriptor` for `layout` at [`ffi::INSTANCE_STREAM_SLOT`], or `None` for a `[[vertex_id]]`-only pass.
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

/// The `MTLPrimitiveType` for `t`.
#[cfg(test)]
pub(crate) const fn metal_primitive_type(t: Topology) -> ffi::PrimitiveType {
    match t {
        Topology::TriangleList => ffi::PrimitiveType::Triangle,
        Topology::TriangleStrip => ffi::PrimitiveType::TriangleStrip,
    }
}

/// Builds the `MTLRenderPipelineState` for `spec`.
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

/// Compiles `library` for `device` ([`super::shaders::source_for`]) with [`ffi::CompileOptions::aterm_default`], or [`ffi::CompileOptions::aterm_precise`] for [`ShaderLibrary::TrayPrims`], whose floats must land the CPU raster's: [`compile_library_with`] over the device's own compiler.
pub(crate) fn compile_library(device: &Device, library: ShaderLibrary) -> Result<Library, String> {
    compile_library_with(device, library, |src| {
        let opts = if matches!(library, ShaderLibrary::TrayPrims) {
            ffi::CompileOptions::aterm_precise()
        } else {
            ffi::CompileOptions::aterm_default()
        }
        .ok_or_else(|| "MTLCompileOptions allocation failed".to_owned())?;
        device.new_library_with_options(src, &opts)
    })
}

/// Compiles `library`'s source for `device` by `compile`. Where the device has framebuffer fetch and `cell.metal` does not compile with its fetch twins, the twins alone are lost: it compiles again without them ([`super::shaders::source`]), so the device reads as [`crate::FetchProbe::Unsupported`] and its Unorm-law light keeps its own passes, rather than the Metal arm failing to mint.
pub(crate) fn compile_library_with(
    device: &Device,
    library: ShaderLibrary,
    compile: impl Fn(&str) -> Result<Library, String>,
) -> Result<Library, String> {
    let fetch = super::fetch::capable(device);
    match compile(&super::shaders::source_for(library, fetch)) {
        Err(why) if fetch && matches!(library, ShaderLibrary::Cell) => {
            crate::stderr_line!(
                "aterm-gpu: cell.metal did not compile with its framebuffer-fetch twins \
                 ({why}); Unorm-law light keeps its own passes"
            );
            compile(super::shaders::source(library))
        }
        compiled => compiled,
    }
}

/// Compiles the shader library named by `spec`.
#[cfg(test)]
pub(crate) fn compile(device: &Device, spec: &PipelineSpec) -> Result<Library, String> {
    compile_library(device, spec.library)
}

#[cfg(test)]
#[path = "pipelines_tests.rs"]
mod tests;
