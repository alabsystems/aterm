// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Builds `MTLRenderPipelineState`s from [`crate::pipeline_table`], the same
//! rows `renderer.rs` builds its wgpu pipelines from.
//!
//! [`metal_blend`], [`metal_write_mask`], [`metal_format`] and
//! [`metal_vertex_descriptor`] are total mappings off the neutral enums, so a
//! factor added on the wgpu side fails to compile here.
//! [`tests::every_table_row_builds_a_metal_pipeline`] builds every row on the
//! real MSL functions with the renderer's own state.
//!
//! # REPLACE
//!
//! `renderer.rs` spells the bg and blit pipelines `wgpu::BlendState::REPLACE`
//! (`(One, Zero)` on both halves). Metal expresses the same state as blending
//! disabled, so [`metal_blend`] maps a REPLACE row to `None`, decided by
//! [`crate::pipeline_table::Blend::is_replace`], not by name.
//!
//! The armed macOS renderer mints its pipeline states through [`build`].

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

/// The Metal spelling of one [`Factor`]. Total, so the table's closed factor
/// set is enforced at compile time on this side too.
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
///
/// Operations are mapped explicitly (today only `Add`), so a new operation
/// fails this exhaustive mapping until Metal supports it.
pub(crate) fn metal_blend(blend: Option<Blend>) -> Option<BlendState> {
    let b = blend?;
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
const fn metal_operation(op: BlendOp) -> BlendOperation {
    match op {
        BlendOp::Add => BlendOperation::Add,
    }
}

/// The Metal spelling of one [`WriteMask`].
pub(crate) const fn metal_write_mask(m: WriteMask) -> ColorWriteMask {
    match m {
        WriteMask::Color => ColorWriteMask::COLOR,
        WriteMask::All => ColorWriteMask::ALL,
    }
}

/// The Metal spelling of one [`AttrFormat`].
const fn metal_attr_format(f: AttrFormat) -> VertexFormat {
    match f {
        AttrFormat::Uint16x4 => VertexFormat::UShort4,
        AttrFormat::Unorm8x4 => VertexFormat::UChar4Normalized,
        AttrFormat::Uint8x4 => VertexFormat::UChar4,
        AttrFormat::Float32x4 => VertexFormat::Float4,
        AttrFormat::Uint32 => VertexFormat::UInt,
    }
}

/// The colour-attachment format for a row's [`TargetRole`]. The offscreen pair
/// is `format_plan`'s native choice (`Rgba8Unorm` with an `Rgba8UnormSrgb`
/// alias view), view-compatible in Metal as in wgpu. `present` is the live
/// swapchain format (`Bgra8Unorm` or `Rgba8Unorm` from `pick_surface_format`).
///
/// Hand-written because no wgpu type may enter this module;
/// `format_plan::tests::the_metal_format_axis_equals_the_wgpu_resolve_for_every_role`
/// checks it against the wgpu resolve for every role and present format.
pub(crate) fn metal_format(role: TargetRole, present: PixelFormat) -> PixelFormat {
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
    use crate::pipeline_table::{ALL_PIPELINES, Pipeline};
    use std::collections::HashMap;

    /// Every table row builds a real `MTLRenderPipelineState` with the
    /// renderer's own entry points, blend, write mask, formats and vertex
    /// layouts, for both swapchain formats `pick_surface_format` can choose.
    #[test]
    fn every_table_row_builds_a_metal_pipeline() {
        let Some(dev) = device() else { return };
        // Six libraries serve eighteen rows: compile each once.
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

    /// No MSL source binds `[[buffer(n)]]` at [`ffi::INSTANCE_STREAM_SLOT`]:
    /// stage-in attributes and constant buffers share one argument table, so a
    /// binding there would collide with the stream.
    ///
    /// The scan does not parse stages, so it also checks fragment bindings
    /// (stricter, not wrong). `parity_kernel.metal` is excluded: compute kernels
    /// use the compute argument table.
    #[test]
    fn no_msl_buffer_binding_collides_with_the_instance_stream_slot() {
        use crate::metal::shaders;
        let sources: Vec<(&'static str, &'static str)> = ShaderLibrary::ALL
            .into_iter()
            .map(|l| (l.name(), shaders::source(l)))
            .chain([("state_probe", shaders::STATE_PROBE)])
            .collect();
        let mut found = 0usize;
        for (name, src) in sources {
            for (i, line) in src.lines().enumerate() {
                let mut rest = line;
                while let Some(pos) = rest.find("buffer(") {
                    rest = &rest[pos + "buffer(".len()..];
                    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    let n: usize = digits.parse().unwrap_or_else(|_| {
                        panic!("{name}.metal:{}: unparsable buffer index", i + 1)
                    });
                    found += 1;
                    assert_ne!(
                        n,
                        ffi::INSTANCE_STREAM_SLOT,
                        "{name}.metal:{}: `[[buffer({n})]]` collides with the instance \
                         stream slot — this is the binding that made a built pipeline \
                         draw nothing",
                        i + 1
                    );
                    assert!(
                        n < 8,
                        "{name}.metal:{}: `[[buffer({n})]]` is outside the small uniform \
                         range every aterm shader uses; if this is deliberate, revisit \
                         INSTANCE_STREAM_SLOT's headroom first",
                        i + 1
                    );
                }
            }
        }
        // A scan that finds nothing is a scan that went blind (the probe alone
        // declares two).
        assert!(
            found >= 10,
            "only {found} `buffer(n)` bindings found across seven sources — the \
             declaration style changed and this guard stopped seeing them"
        );
        // Count down from the top of Metal's 31-entry table (wgpu-hal's
        // principle; its own slot is 15). This pin is the contract with the MSL
        // scan above: change one, change both.
        assert_eq!(ffi::INSTANCE_STREAM_SLOT, 30);
    }

    /// The `MTLVertexDescriptor.h` value for one [`AttrFormat`], spelled here
    /// independently of [`metal_attr_format`] so the tests do not check the
    /// mapping against itself.
    const fn pinned_vertex_format(f: AttrFormat) -> (usize, &'static str) {
        match f {
            AttrFormat::Uint8x4 => (3, "MTLVertexFormatUChar4"),
            AttrFormat::Unorm8x4 => (9, "MTLVertexFormatUChar4Normalized"),
            AttrFormat::Uint16x4 => (15, "MTLVertexFormatUShort4"),
            AttrFormat::Float32x4 => (31, "MTLVertexFormatFloat4"),
            AttrFormat::Uint32 => (36, "MTLVertexFormatUInt"),
        }
    }

    /// Vertex-format constants pinned against `MTLVertexDescriptor.h`.
    /// `UChar4 == 3` and `UChar4Normalized == 9` both build, but the first turns
    /// `Unorm8x4` colours from 0..1 into 0..255. The GPU half is
    /// `metal::tests::the_bg_row_draws_its_instance_stream_at_the_deconflicted_slot`.
    #[test]
    fn every_attr_format_maps_to_the_pinned_metal_constant() {
        let all = [
            AttrFormat::Uint8x4,
            AttrFormat::Unorm8x4,
            AttrFormat::Uint16x4,
            AttrFormat::Float32x4,
            AttrFormat::Uint32,
        ];
        let mut seen: Vec<usize> = Vec::new();
        for f in all {
            let (raw, name) = pinned_vertex_format(f);
            let got = metal_attr_format(f) as usize;
            assert_eq!(got, raw, "{} must map to {name} ({raw})", f.name());
            assert!(!seen.contains(&got), "{} shares a constant", f.name());
            seen.push(got);
        }
    }

    /// Every table layout's `MTLVertexDescriptor`, read back through separate
    /// getters and checked field by field against the spec: raw format, offset,
    /// buffer index, stride, per-instance stepping, and slot 0 left empty. No
    /// GPU needed; the drawn half is the bg-row readback test in `metal::tests`.
    #[test]
    fn the_vertex_descriptor_restates_every_table_layout_at_the_stream_slot() {
        const STEP_PER_INSTANCE: usize = 2;
        let mut verified = 0usize;
        for row in ALL_PIPELINES {
            let spec = row.spec();
            let Some(v) = spec.vertex.spec() else {
                assert!(
                    metal_vertex_descriptor(spec.vertex).is_none(),
                    "{} is [[vertex_id]]-only and must have no descriptor",
                    row.name()
                );
                continue;
            };
            let vd = metal_vertex_descriptor(spec.vertex)
                .unwrap_or_else(|| panic!("{} descriptor allocation failed", row.name()));
            for a in v.attrs {
                let (format, offset, buffer) = vd.attribute_raw(a.location as usize);
                let (want_format, format_name) = pinned_vertex_format(a.format);
                assert_eq!(
                    format,
                    want_format,
                    "{} attribute {}: format must be {format_name} — compared \
                     against the pinned constant, NOT metal_attr_format, so a \
                     wrong mapping cannot launder itself through the readback",
                    row.name(),
                    a.location
                );
                assert_eq!(
                    offset,
                    a.offset as usize,
                    "{} attribute {}: offset — zeroed offsets built and drew garbage",
                    row.name(),
                    a.location
                );
                assert_eq!(
                    buffer,
                    ffi::INSTANCE_STREAM_SLOT,
                    "{} attribute {}: stream slot",
                    row.name(),
                    a.location
                );
            }
            let (stride, step, rate) = vd.layout_raw(ffi::INSTANCE_STREAM_SLOT);
            assert_eq!(
                stride,
                v.stride as usize,
                "{} stride — a doubled stride built and lost every second instance",
                row.name()
            );
            assert_eq!(
                step,
                STEP_PER_INSTANCE,
                "{} must step per instance",
                row.name()
            );
            assert_eq!(rate, 1, "{} step rate", row.name());
            // Slot 0 belongs to the MSL uniform blocks; a layout there is the
            // P1 collision come back.
            let (s0, _, _) = vd.layout_raw(0);
            assert_eq!(s0, 0, "{} lays out a stream at slot 0", row.name());
            verified += 1;
        }
        // 14 of 18 rows carry a stream; a sweep that verified fewer went blind.
        assert_eq!(verified, 14, "expected 14 instanced rows");
    }

    /// Topology constants pinned against `MTLRenderCommandEncoder.h`
    /// (`Triangle == 3`, `TriangleStrip == 4`), with the table's one strip row
    /// named. The drawn half is
    /// `metal::tests::the_tray_strip_covers_the_whole_card`.
    #[test]
    fn every_table_topology_maps_to_the_pinned_primitive_type() {
        assert_eq!(
            metal_primitive_type(Topology::TriangleList) as usize,
            3,
            "MTLPrimitiveTypeTriangle"
        );
        assert_eq!(
            metal_primitive_type(Topology::TriangleStrip) as usize,
            4,
            "MTLPrimitiveTypeTriangleStrip"
        );
        let strips: Vec<&str> = ALL_PIPELINES
            .into_iter()
            .filter(|r| r.spec().topology == Topology::TriangleStrip)
            .map(Pipeline::name)
            .collect();
        assert_eq!(strips, ["tray"], "the strip rows are exactly the tray");
    }

    /// The table's blend factors map to distinct `MTLBlendFactor` values; a
    /// transposed pair would swap blend equations while still building.
    #[test]
    fn every_table_factor_maps_to_a_distinct_metal_constant() {
        let all = [
            Factor::Zero,
            Factor::One,
            Factor::SrcAlpha,
            Factor::OneMinusSrcAlpha,
            Factor::OneMinusSrc,
        ];
        let mut seen: Vec<usize> = Vec::new();
        for f in all {
            let raw = metal_factor(f) as usize;
            assert!(
                !seen.contains(&raw),
                "{} maps to MTLBlendFactor {raw}, already taken",
                f.name()
            );
            seen.push(raw);
        }
        // Pinned against the headers: the screen factor `OneMinusSourceColor`
        // is 3, between `One == 1` and `SourceAlpha == 4`.
        assert_eq!(metal_factor(Factor::Zero) as usize, 0);
        assert_eq!(metal_factor(Factor::One) as usize, 1);
        assert_eq!(metal_factor(Factor::OneMinusSrc) as usize, 3);
        assert_eq!(metal_factor(Factor::SrcAlpha) as usize, 4);
        assert_eq!(metal_factor(Factor::OneMinusSrcAlpha) as usize, 5);
    }

    /// REPLACE — however the table spells it — must reach Metal as blending
    /// DISABLED, and nothing else may.
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
        // The three rows that reach Metal with blending off, named so a fourth
        // (or a lost one) is a diff a reviewer sees.
        let off: Vec<&str> = ALL_PIPELINES
            .into_iter()
            .filter(|r| metal_blend(r.spec().blend).is_none())
            .map(Pipeline::name)
            .collect();
        assert_eq!(off, vec!["bg", "shimmer", "blit"]);
    }

    /// The RGB-only rows, named: both backends default to all channels, so a
    /// row that lost `WriteMask::Color` would otherwise be silent until a
    /// translucent window goes opaque. Both crowns (one call site) are
    /// RGB-only; the shimmer writes all channels because it displaces the whole
    /// rgba sample.
    #[test]
    fn nine_rows_are_rgb_only_and_they_are_these() {
        let rgb_only: Vec<&str> = ALL_PIPELINES
            .into_iter()
            .filter(|r| metal_write_mask(r.spec().write_mask) == ColorWriteMask::COLOR)
            .map(Pipeline::name)
            .collect();
        assert_eq!(
            rgb_only,
            vec![
                "glow_add",
                "rain_glow",
                "rain_glow_over",
                "fire_add",
                "fire_over",
                "deco_add",
                "bloom",
                "hdr_glow",
                "sdr_glow",
            ]
        );
    }
}
