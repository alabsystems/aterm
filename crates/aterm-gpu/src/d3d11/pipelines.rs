// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! D3D11 shaders, input layouts and fixed-function state built from
//! [`crate::pipeline_table`], the rows the wgpu, Metal and GL pipelines are
//! built from.
//!
//! Each row's states are wgpu-dx12's for the same row, so the arm's frames
//! are the oracle's: blending is enabled whenever the row has a blend (wgpu
//! enables it for REPLACE too, and it is arithmetically the same), a factor's
//! colour form becomes its alpha form in the alpha half (wgpu-hal's
//! `map_blend_factor`), the rasterizer culls nothing and always scissors (a
//! pass with no scissor scissors to its whole target, as DX12 does), and every
//! attribute steps once per instance under the semantic `LOC<location>`, the
//! name the HLSL twins (and naga's HLSL) give it.
//!
//! The mappings off the table's neutral enums are total, so a variant added to
//! the table fails to compile here until D3D11 spells it.

use std::ffi::c_void;

use aterm_win32::d3d11 as d3d;
use aterm_win32::dxgi;

use super::device::{Child, D3dDevice};
use super::shaders::{self, Stage};
use crate::pipeline_table::{
    AttrFormat, Blend, BlendOp, Factor, PipelineSpec, Topology, VertexLayoutSpec, WriteMask,
};

/// The D3D11 spelling of one [`Factor`] in the colour half (`alpha` false) or
/// the alpha half, which takes no colour factor.
#[inline]
const fn d3d_factor(f: Factor, alpha: bool) -> d3d::D3D11_BLEND {
    match f {
        Factor::Zero => d3d::D3D11_BLEND_ZERO,
        Factor::One => d3d::D3D11_BLEND_ONE,
        Factor::SrcAlpha => d3d::D3D11_BLEND_SRC_ALPHA,
        Factor::OneMinusSrcAlpha => d3d::D3D11_BLEND_INV_SRC_ALPHA,
        Factor::OneMinusSrc if alpha => d3d::D3D11_BLEND_INV_SRC_ALPHA,
        Factor::OneMinusSrc => d3d::D3D11_BLEND_INV_SRC_COLOR,
    }
}

/// The D3D11 spelling of one [`BlendOp`].
#[inline]
const fn d3d_op(op: BlendOp) -> d3d::D3D11_BLEND_OP {
    match op {
        BlendOp::Add => d3d::D3D11_BLEND_OP_ADD,
    }
}

/// The D3D11 primitive topology of a row's [`Topology`].
#[inline]
const fn d3d_topology(t: Topology) -> d3d::D3D_PRIMITIVE_TOPOLOGY {
    match t {
        Topology::TriangleList => d3d::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
        Topology::TriangleStrip => d3d::D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
    }
}

/// The DXGI format of one [`AttrFormat`] (wgpu-hal's `map_vertex_format`).
#[inline]
const fn d3d_attr(f: AttrFormat) -> dxgi::DXGI_FORMAT {
    match f {
        AttrFormat::Uint16x4 => dxgi::DXGI_FORMAT_R16G16B16A16_UINT,
        AttrFormat::Unorm8x4 => dxgi::DXGI_FORMAT_R8G8B8A8_UNORM,
        AttrFormat::Uint8x4 => dxgi::DXGI_FORMAT_R8G8B8A8_UINT,
        AttrFormat::Float32x4 => dxgi::DXGI_FORMAT_R32G32B32A32_FLOAT,
        AttrFormat::Uint32 => dxgi::DXGI_FORMAT_R32_UINT,
        AttrFormat::Sint16x2 => dxgi::DXGI_FORMAT_R16G16_SINT,
        AttrFormat::Uint16x2 => dxgi::DXGI_FORMAT_R16G16_UINT,
    }
}

/// The render-target blend a row sets (see the module header).
#[inline]
pub(crate) const fn d3d_blend(
    blend: Option<Blend>,
    mask: WriteMask,
) -> d3d::D3D11_RENDER_TARGET_BLEND_DESC {
    let write = match mask {
        WriteMask::Color => {
            d3d::D3D11_COLOR_WRITE_ENABLE_RED
                | d3d::D3D11_COLOR_WRITE_ENABLE_GREEN
                | d3d::D3D11_COLOR_WRITE_ENABLE_BLUE
        }
        WriteMask::All => d3d::D3D11_COLOR_WRITE_ENABLE_ALL,
    } as u8;
    match blend {
        None => d3d::D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: 0,
            SrcBlend: d3d::D3D11_BLEND_ONE,
            DestBlend: d3d::D3D11_BLEND_ZERO,
            BlendOp: d3d::D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: d3d::D3D11_BLEND_ONE,
            DestBlendAlpha: d3d::D3D11_BLEND_ZERO,
            BlendOpAlpha: d3d::D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: write,
        },
        Some(b) => d3d::D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: 1,
            SrcBlend: d3d_factor(b.color.src, false),
            DestBlend: d3d_factor(b.color.dst, false),
            BlendOp: d3d_op(b.color.op),
            SrcBlendAlpha: d3d_factor(b.alpha.src, true),
            DestBlendAlpha: d3d_factor(b.alpha.dst, true),
            BlendOpAlpha: d3d_op(b.alpha.op),
            RenderTargetWriteMask: write,
        },
    }
}

/// The semantic every instance attribute is declared under.
const LOC: &[u8] = b"LOC\0";

/// A row as D3D11 draws it: the two shaders, the instance layout and the
/// fixed-function states `set_pipeline` binds.
pub(crate) struct D3dPipeline {
    pub(crate) vs: Child,
    pub(crate) ps: Child,
    /// The instance stream's input layout, `None` for an `SV_VertexID`-only
    /// pass.
    pub(crate) input: Option<Child>,
    pub(crate) blend: Child,
    pub(crate) raster: Child,
    pub(crate) topology: d3d::D3D_PRIMITIVE_TOPOLOGY,
    /// The instance layout, `None` for an `SV_VertexID`-only pass.
    pub(crate) layout: Option<VertexLayoutSpec>,
    /// The table label, for messages.
    pub(crate) label: &'static str,
}

impl std::fmt::Debug for D3dPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("D3dPipeline")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl D3dDevice {
    /// Build the shaders and states for one table row.
    pub(crate) fn pipeline(&self, spec: &PipelineSpec) -> Result<D3dPipeline, String> {
        let vs_code = shaders::bytecode(spec.library, spec.vs, Stage::Vertex)?;
        let ps_code = shaders::bytecode(spec.library, spec.fs, Stage::Pixel)?;
        let shared = &self.0;
        let vt = shared.device_vt();
        let dev = shared.device_raw();
        let mut out = std::ptr::null_mut();
        // SAFETY: bytecode FXC compiled for vs_5_0; no class linkage; one owned
        // shader written on success.
        let hr = unsafe {
            (vt.CreateVertexShader)(
                dev,
                vs_code.as_ptr().cast(),
                vs_code.len(),
                std::ptr::null_mut(),
                &mut out,
            )
        };
        let vs = shared.created(spec.vs, hr, out)?;
        let mut out = std::ptr::null_mut();
        // SAFETY: as above, for ps_5_0.
        let hr = unsafe {
            (vt.CreatePixelShader)(
                dev,
                ps_code.as_ptr().cast(),
                ps_code.len(),
                std::ptr::null_mut(),
                &mut out,
            )
        };
        let ps = shared.created(spec.fs, hr, out)?;
        let layout = spec.vertex.spec();
        let input = match layout {
            None => None,
            Some(l) => {
                let elements: Vec<d3d::D3D11_INPUT_ELEMENT_DESC> = l
                    .attrs
                    .iter()
                    .map(|a| d3d::D3D11_INPUT_ELEMENT_DESC {
                        SemanticName: LOC.as_ptr(),
                        SemanticIndex: a.location,
                        Format: d3d_attr(a.format),
                        InputSlot: 0,
                        AlignedByteOffset: a.offset as u32,
                        InputSlotClass: d3d::D3D11_INPUT_PER_INSTANCE_DATA,
                        InstanceDataStepRate: 1,
                    })
                    .collect();
                let mut out = std::ptr::null_mut();
                // SAFETY: `elements` is live for the call with its length; the
                // semantic name is NUL-terminated and static; the vertex
                // shader's bytecode carries the signature it is matched to.
                let hr = unsafe {
                    (vt.CreateInputLayout)(
                        dev,
                        elements.as_ptr(),
                        elements.len() as u32,
                        vs_code.as_ptr().cast::<c_void>(),
                        vs_code.len(),
                        &mut out,
                    )
                };
                Some(shared.created(spec.label, hr, out)?)
            }
        };
        let mut desc = d3d::D3D11_BLEND_DESC::default();
        desc.RenderTarget[0] = d3d_blend(spec.blend, spec.write_mask);
        let mut out = std::ptr::null_mut();
        // SAFETY: a valid blend description; one owned state on success.
        let hr = unsafe { (vt.CreateBlendState)(dev, &desc, &mut out) };
        let blend = shared.created("CreateBlendState", hr, out)?;
        let raster_desc = d3d::D3D11_RASTERIZER_DESC {
            FillMode: d3d::D3D11_FILL_SOLID,
            CullMode: d3d::D3D11_CULL_NONE,
            FrontCounterClockwise: 0,
            DepthBias: 0,
            DepthBiasClamp: 0.0,
            SlopeScaledDepthBias: 0.0,
            DepthClipEnable: 1,
            ScissorEnable: 1,
            MultisampleEnable: 0,
            AntialiasedLineEnable: 0,
        };
        let mut out = std::ptr::null_mut();
        // SAFETY: a valid rasterizer description; one owned state on success.
        let hr = unsafe { (vt.CreateRasterizerState)(dev, &raster_desc, &mut out) };
        let raster = shared.created("CreateRasterizerState", hr, out)?;
        Ok(D3dPipeline {
            vs,
            ps,
            input,
            blend,
            raster,
            topology: d3d_topology(spec.topology),
            layout,
            label: spec.label,
        })
    }
}

#[cfg(test)]
mod tests {
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

    /// A broken twin is refused with the compiler's log, not drawn blank: the
    /// negative control for the compile check above.
    #[test]
    fn a_twin_that_does_not_compile_is_refused_with_the_compilers_log() {
        let _dev = super::super::test_device();
        let src = shaders::source(crate::pipeline_table::ShaderLibrary::Cell);
        let good = "return float4(s2l(i.color.xyz), i.color.w);";
        assert!(src.contains(good), "the control edits a line the twin has");
        let broken = src.replace(good, "return undeclared;");
        let err = shaders::compile_source(&broken, "cell.hlsl", "fs_bg", Stage::Pixel)
            .expect_err("must not compile");
        assert!(err.contains("does not compile"), "{err}");
        assert!(
            err.contains("undeclared"),
            "the compiler's log is carried: {err}"
        );
        shaders::compile_source(src, "cell.hlsl", "fs_bg", Stage::Pixel)
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
}
