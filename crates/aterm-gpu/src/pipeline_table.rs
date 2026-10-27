// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared render pipeline specification table ([`PIPELINES`]) for the Metal, GL and D3D11 arms.
//! The enums keep wgpu's names for the states they spell, the vocabulary the arms were held to
//! until GW.6 (docs/AUDIT-third-party-2026-10-15.md).
/// Shader module containing a pipeline's entry points.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ShaderLibrary {
    /// `shaders/cell.{metal,glsl,hlsl}`.
    Cell,
    /// `shaders/blit.{metal,glsl,hlsl}`.
    Blit,
    /// `shaders/hdr_glow.{metal,glsl,hlsl}`.
    HdrGlow,
    /// `shaders/tray.{metal,glsl,hlsl}`.
    Tray,
    /// `shaders/bloom.{metal,glsl,hlsl}`.
    Bloom,
    /// `shaders/shimmer.{metal,glsl,hlsl}`.
    Shimmer,
}

impl ShaderLibrary {
    /// Returns the `.metal` file stem for this shader library.
    #[inline]
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Cell => "cell",
            Self::Blit => "blit",
            Self::HdrGlow => "hdr_glow",
            Self::Tray => "tray",
            Self::Bloom => "bloom",
            Self::Shimmer => "shimmer",
        }
    }

    /// All shader libraries in declaration order.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) const ALL: [Self; 6] = [
        Self::Cell,
        Self::Blit,
        Self::HdrGlow,
        Self::Tray,
        Self::Bloom,
        Self::Shimmer,
    ];
}

/// Blend factor shared by every arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Factor {
    /// `wgpu::BlendFactor::Zero` / `MTLBlendFactorZero`.
    Zero,
    /// `wgpu::BlendFactor::One` / `MTLBlendFactorOne`.
    One,
    /// `wgpu::BlendFactor::SrcAlpha` / `MTLBlendFactorSourceAlpha`.
    SrcAlpha,
    /// `wgpu::BlendFactor::OneMinusSrcAlpha` / `MTLBlendFactorOneMinusSourceAlpha`.
    OneMinusSrcAlpha,
    /// `wgpu::BlendFactor::OneMinusSrc` / `MTLBlendFactorOneMinusSourceColor`.
    OneMinusSrc,
}

/// Blend operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlendOp {
    /// `src * src_factor + dst * dst_factor`.
    Add,
}

/// Blend equation for one channel group (RGB or alpha).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlendComponent {
    /// Source blend factor.
    pub(crate) src: Factor,
    /// Destination blend factor.
    pub(crate) dst: Factor,
    /// Blend operation.
    pub(crate) op: BlendOp,
}

impl BlendComponent {
    /// `src * 1 + dst * 0` — the REPLACE half.
    const REPLACE: Self = Self {
        src: Factor::One,
        dst: Factor::Zero,
        op: BlendOp::Add,
    };
    /// `src * 1 + dst * (1 - src_a)` — premultiplied source-over, `wgpu`'s
    /// `BlendComponent::OVER`.
    const OVER: Self = Self {
        src: Factor::One,
        dst: Factor::OneMinusSrcAlpha,
        op: BlendOp::Add,
    };
    /// `src * src_a + dst * (1 - src_a)` — the straight-alpha source-over RGB
    /// half of `wgpu::BlendState::ALPHA_BLENDING`.
    const STRAIGHT_OVER: Self = Self {
        src: Factor::SrcAlpha,
        dst: Factor::OneMinusSrcAlpha,
        op: BlendOp::Add,
    };
    /// `src + dst` — the premultiplied additive light shared by the rain halos,
    /// the fire-add half, the deco sparkle and the EDR crown.
    const ADD: Self = Self {
        src: Factor::One,
        dst: Factor::One,
        op: BlendOp::Add,
    };
    /// `src + dst * (1 - src)` — SCREEN, for the bloom composite and the SDR
    /// crown.
    const SCREEN: Self = Self {
        src: Factor::One,
        dst: Factor::OneMinusSrc,
        op: BlendOp::Add,
    };
}

/// Full color and alpha blend state for a pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Blend {
    /// RGB blend equation.
    pub(crate) color: BlendComponent,
    /// Alpha blend equation.
    pub(crate) alpha: BlendComponent,
}

impl Blend {
    /// `wgpu::BlendState::ALPHA_BLENDING`: straight-alpha source-over on RGB,
    /// premultiplied source-over on alpha.
    const ALPHA_BLENDING: Self = Self {
        color: BlendComponent::STRAIGHT_OVER,
        alpha: BlendComponent::OVER,
    };
    /// `wgpu::BlendState::REPLACE`, kept explicit where the renderer spells it.
    const REPLACE: Self = Self {
        color: BlendComponent::REPLACE,
        alpha: BlendComponent::REPLACE,
    };
    /// `One`/`One` on both halves.
    const ADDITIVE: Self = Self {
        color: BlendComponent::ADD,
        alpha: BlendComponent::ADD,
    };
    /// SCREEN on both halves: the bloom composite and the SDR crown.
    const SCREEN: Self = Self {
        color: BlendComponent::SCREEN,
        alpha: BlendComponent::SCREEN,
    };
    /// The one-sided generalization the flat `GlowQuad` streams take:
    /// `out = src + dst*(1 - src_a)`, which is `One`/`One` at `src_a == 0`.
    const GLOW_OVER: Self = Self {
        color: BlendComponent::OVER,
        alpha: BlendComponent::OVER,
    };

    /// Returns whether both channels perform fixed-function replacement (`src * 1 + dst * 0`).
    #[inline]
    #[cfg(any(target_os = "macos", gl_arm))]
    pub(crate) const fn is_replace(self) -> bool {
        matches!(
            (
                self.color.src,
                self.color.dst,
                self.color.op,
                self.alpha.src,
                self.alpha.dst,
                self.alpha.op,
            ),
            (
                Factor::One,
                Factor::Zero,
                BlendOp::Add,
                Factor::One,
                Factor::Zero,
                BlendOp::Add,
            )
        )
    }
}

/// Color attachment channel write mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteMask {
    /// RGB channels only (`wgpu::ColorWrites::COLOR`).
    Color,
    /// All RGBA channels (`wgpu::ColorWrites::ALL`).
    All,
}

/// Logical color attachment target role for a pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetRole {
    /// sRGB view of the offscreen target.
    OffscreenSrgb,
    /// Unorm view of the offscreen target.
    OffscreenUnorm,
    /// Live swapchain present target.
    Present,
    /// `Rgba16Float` EDR target.
    Edr,
}

/// Primitive topology.
#[cfg(any(gl_arm, d3d11_arm, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Topology {
    /// `wgpu::PrimitiveTopology::TriangleList`.
    TriangleList,
    /// `wgpu::PrimitiveTopology::TriangleStrip`.
    TriangleStrip,
}

/// Backend-neutral vertex attribute descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VertexAttr {
    /// Shader attribute location index.
    pub(crate) location: u32,
    /// Attribute element format.
    pub(crate) format: AttrFormat,
    /// Byte offset within the instance struct.
    pub(crate) offset: u64,
}

/// Vertex attribute element format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttrFormat {
    /// `wgpu::VertexFormat::Uint16x4` / `MTLVertexFormatUShort4`.
    Uint16x4,
    /// `wgpu::VertexFormat::Unorm8x4` / `MTLVertexFormatUChar4Normalized`.
    Unorm8x4,
    /// `wgpu::VertexFormat::Uint8x4` / `MTLVertexFormatUChar4`.
    Uint8x4,
    /// `wgpu::VertexFormat::Float32x4` / `MTLVertexFormatFloat4`.
    Float32x4,
    /// `wgpu::VertexFormat::Uint32` / `MTLVertexFormatUInt`.
    Uint32,
    /// `wgpu::VertexFormat::Sint16x2` / `MTLVertexFormatShort2`.
    Sint16x2,
    /// `wgpu::VertexFormat::Uint16x2` / `MTLVertexFormatUShort2`.
    Uint16x2,
}

/// Per-instance vertex buffer layout specification.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VertexLayoutSpec {
    /// Instance stride in bytes.
    pub(crate) stride: u64,
    /// Vertex attributes in location order.
    pub(crate) attrs: &'static [VertexAttr],
}

/// Instance stream layout kind for a pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VertexLayout {
    /// No vertex buffer (`[[vertex_id]]` only).
    None,
    /// `BgInstance` (12 bytes).
    Bg,
    /// `GlowInstance` (16 bytes, [`aterm_render::GlowQuad::color2`]).
    Glow,
    /// `GlyphInstance` (40 bytes).
    Glyph,
    /// `RainGlowInstance` (20 bytes).
    RainGlow,
    /// `FireInstance` (24 bytes).
    Fire,
    /// `FxInstance` (32 bytes).
    Fx,
}

/// Layout specification for `BgInstance`.
pub(crate) const BG_LAYOUT: VertexLayoutSpec = VertexLayoutSpec {
    stride: 12,
    attrs: &[
        VertexAttr {
            location: 0,
            format: AttrFormat::Uint16x4,
            offset: 0,
        },
        VertexAttr {
            location: 1,
            format: AttrFormat::Unorm8x4,
            offset: 8,
        },
    ],
};

/// Layout specification for `GlowInstance`.
pub(crate) const GLOW_LAYOUT: VertexLayoutSpec = VertexLayoutSpec {
    stride: 16,
    attrs: &[
        VertexAttr {
            location: 0,
            format: AttrFormat::Uint16x4,
            offset: 0,
        },
        VertexAttr {
            location: 1,
            format: AttrFormat::Uint8x4,
            offset: 8,
        },
        VertexAttr {
            location: 2,
            format: AttrFormat::Uint8x4,
            offset: 12,
        },
    ],
};

/// Layout specification for `GlyphInstance`.
pub(crate) const GLYPH_LAYOUT: VertexLayoutSpec = VertexLayoutSpec {
    stride: 40,
    attrs: &[
        VertexAttr {
            location: 0,
            format: AttrFormat::Float32x4,
            offset: 0,
        },
        VertexAttr {
            location: 1,
            format: AttrFormat::Float32x4,
            offset: 16,
        },
        VertexAttr {
            location: 2,
            format: AttrFormat::Unorm8x4,
            offset: 32,
        },
        VertexAttr {
            location: 3,
            format: AttrFormat::Unorm8x4,
            offset: 36,
        },
    ],
};

/// Layout specification for `RainGlowInstance`.
pub(crate) const RAIN_GLOW_LAYOUT: VertexLayoutSpec = VertexLayoutSpec {
    stride: 20,
    attrs: &[
        VertexAttr {
            location: 0,
            format: AttrFormat::Uint16x4,
            offset: 0,
        },
        VertexAttr {
            location: 1,
            format: AttrFormat::Unorm8x4,
            offset: 8,
        },
        VertexAttr {
            location: 2,
            format: AttrFormat::Uint16x4,
            offset: 12,
        },
    ],
};

/// Layout specification for `FireInstance`.
pub(crate) const FIRE_LAYOUT: VertexLayoutSpec = VertexLayoutSpec {
    stride: 24,
    attrs: &[
        VertexAttr {
            location: 0,
            format: AttrFormat::Uint16x4,
            offset: 0,
        },
        VertexAttr {
            location: 1,
            format: AttrFormat::Uint16x4,
            offset: 8,
        },
        VertexAttr {
            location: 2,
            format: AttrFormat::Uint32,
            offset: 16,
        },
        VertexAttr {
            location: 3,
            format: AttrFormat::Uint8x4,
            offset: 20,
        },
    ],
};

/// Layout specification for `FxInstance`.
pub(crate) const FX_LAYOUT: VertexLayoutSpec = VertexLayoutSpec {
    stride: 32,
    attrs: &[
        VertexAttr {
            location: 0,
            format: AttrFormat::Uint16x4,
            offset: 0,
        },
        VertexAttr {
            location: 1,
            format: AttrFormat::Sint16x2,
            offset: 8,
        },
        VertexAttr {
            location: 2,
            format: AttrFormat::Uint16x2,
            offset: 12,
        },
        VertexAttr {
            location: 3,
            format: AttrFormat::Uint16x4,
            offset: 16,
        },
        VertexAttr {
            location: 4,
            format: AttrFormat::Uint32,
            offset: 24,
        },
        VertexAttr {
            location: 5,
            format: AttrFormat::Uint32,
            offset: 28,
        },
    ],
};

impl VertexLayout {
    /// Returns the [`VertexLayoutSpec`] for this layout, or `None` for [`Self::None`].
    #[inline]
    pub(crate) const fn spec(self) -> Option<VertexLayoutSpec> {
        match self {
            Self::None => None,
            Self::Bg => Some(BG_LAYOUT),
            Self::Glow => Some(GLOW_LAYOUT),
            Self::Glyph => Some(GLYPH_LAYOUT),
            Self::RainGlow => Some(RAIN_GLOW_LAYOUT),
            Self::Fire => Some(FIRE_LAYOUT),
            Self::Fx => Some(FX_LAYOUT),
        }
    }
}

/// Resource binding slot specification for explicit-slot backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BindSpec {
    /// Vertex uniform buffer slot, or `None` for vertex-id-only passes.
    pub(crate) vertex_uniform: Option<u32>,
    /// Fragment texture slots in order.
    pub(crate) fragment_textures: &'static [u32],
    /// Fragment sampler slots in order.
    pub(crate) fragment_samplers: &'static [u32],
    /// Fragment uniform buffer slots in order.
    pub(crate) fragment_buffers: &'static [u32],
    /// Optional fragment storage buffer slot for the fx palette.
    pub(crate) fragment_palette: Option<u32>,
    /// The texture unit the vertex stage reads the bloom's item list from on
    /// GL, and its shader-resource slot on D3D11
    /// (`aterm_render::bloom::BloomItems`; Metal reads it at the instance
    /// stream's slot).
    pub(crate) vertex_items: Option<u32>,
}

impl BindSpec {
    /// The flat cell rows (bg / cursor / glow / rain / fire): `Uniforms` at
    /// vertex `[[buffer(0)]]`, nothing in the fragment stage.
    const CELL_FLAT: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[],
        fragment_samplers: &[],
        fragment_buffers: &[],
        fragment_palette: None,
        vertex_items: None,
    };
    /// The atlas-sampling cell rows (colour glyph, deco pair, sprites):
    /// [`Self::CELL_FLAT`] plus the atlas at fragment texture/sampler 0.
    const CELL_ATLAS: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[0],
        fragment_samplers: &[0],
        fragment_buffers: &[],
        fragment_palette: None,
        vertex_items: None,
    };
    /// `fs_glyph`: [`Self::CELL_ATLAS`] plus `Uniforms` re-bound at fragment
    /// `[[buffer(0)]]` (it reads `text_blend`).
    const CELL_GLYPH_TEXT: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[0],
        fragment_samplers: &[0],
        fragment_buffers: &[0],
        fragment_palette: None,
        vertex_items: None,
    };
    /// The `[[vertex_id]]` post passes (bloom, shimmer, blit): no vertex
    /// uniform; the frame at fragment texture/sampler 0 and the pass uniform at
    /// fragment `[[buffer(2)]]`.
    const POST_FS: Self = Self {
        vertex_uniform: None,
        fragment_textures: &[0],
        fragment_samplers: &[0],
        fragment_buffers: &[2],
        fragment_palette: None,
        vertex_items: None,
    };
    /// The crown pair: `HdrU` at vertex `[[buffer(0)]]` and fragment
    /// `[[buffer(0)]]`; no texture (the crown is procedural light).
    const CROWN: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[],
        fragment_samplers: &[],
        fragment_buffers: &[0],
        fragment_palette: None,
        vertex_items: None,
    };
    /// The fx rows: `Uniforms` at vertex `[[buffer(0)]]`; the R8 (Index8) and
    /// RGBA8 atlases at fragment textures 0 and 1, read by integer texel
    /// address (`read`, `texelFetch`, `textureLoad`), so no sampler; and the
    /// frame palette at slot 2 ([`Self::fragment_palette`]).
    const FX: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[0, 1],
        fragment_samplers: &[],
        fragment_buffers: &[],
        fragment_palette: Some(2),
        vertex_items: None,
    };
    /// The bloom extract over its item list: [`Self::CELL_FLAT`], the list at
    /// vertex texture unit 0 on GL (the vertex stage's t0 on D3D11).
    const GLOW_ITEMS: Self = Self {
        vertex_items: Some(0),
        ..Self::CELL_FLAT
    };
    /// The crown over the bloom's item list: [`Self::CROWN`], the list at
    /// vertex texture unit 0 on GL (the vertex stage's t0 on D3D11).
    const CROWN_ITEMS: Self = Self {
        vertex_items: Some(0),
        ..Self::CROWN
    };
    /// The tray: placement uniform at vertex `[[buffer(2)]]`, card texture and
    /// linear sampler at fragment 0.
    const TRAY: Self = Self {
        vertex_uniform: Some(2),
        fragment_textures: &[0],
        fragment_samplers: &[0],
        fragment_buffers: &[],
        fragment_palette: None,
        vertex_items: None,
    };
}

/// Complete specification of a render pipeline.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PipelineSpec {
    /// Pipeline debug label.
    pub(crate) label: &'static str,
    /// Shader module containing `vs` and `fs`.
    pub(crate) library: ShaderLibrary,
    /// Vertex entry point name.
    pub(crate) vs: &'static str,
    /// Fragment entry point name.
    pub(crate) fs: &'static str,
    /// Target attachment role.
    #[cfg_attr(
        any(gl_arm, d3d11_arm),
        expect(
            dead_code,
            reason = "GL programs and D3D11 pipelines are format-free; the Metal pipelines \
                      key on it"
        )
    )]
    pub(crate) target: TargetRole,
    /// Blend state, or `None` for unblended replacement.
    pub(crate) blend: Option<Blend>,
    /// Channel write mask.
    pub(crate) write_mask: WriteMask,
    /// Per-instance vertex layout.
    pub(crate) vertex: VertexLayout,
    /// Primitive topology.
    #[cfg(any(gl_arm, d3d11_arm, test))]
    pub(crate) topology: Topology,
    /// Explicit resource binding slots (see [`BindSpec`]).
    pub(crate) binds: BindSpec,
}

/// Table of all render pipelines in [`Pipeline`] discriminant order.
pub(crate) static PIPELINES: [PipelineSpec; PIPELINE_COUNT] = [
    PipelineSpec {
        label: "aterm-gpu bg pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_bg",
        fs: "fs_bg",
        target: TargetRole::OffscreenSrgb,
        blend: Some(Blend::REPLACE),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Bg,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
    },
    PipelineSpec {
        label: "aterm-gpu glyph pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_glyph",
        fs: "fs_glyph",
        target: TargetRole::OffscreenSrgb,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Glyph,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_GLYPH_TEXT,
    },
    PipelineSpec {
        label: "aterm-gpu colour-glyph pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_glyph",
        fs: "fs_glyph_color",
        target: TargetRole::OffscreenSrgb,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Glyph,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_ATLAS,
    },
    PipelineSpec {
        label: "aterm-gpu cursor blend pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_bg",
        fs: "fs_bg",
        target: TargetRole::OffscreenSrgb,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Bg,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
    },
    PipelineSpec {
        label: "aterm-gpu glow additive pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_glow",
        fs: "fs_glow",
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::GLOW_OVER),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Glow,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
    },
    PipelineSpec {
        label: "aterm-gpu rain halo pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_rain_glow",
        fs: "fs_rain_glow",
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::ADDITIVE),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::RainGlow,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
    },
    PipelineSpec {
        label: "aterm-gpu rain halo over pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_rain_glow",
        fs: "fs_rain_glow_over",
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::RainGlow,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
    },
    PipelineSpec {
        label: "aterm-gpu fire add pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_fire",
        fs: "fs_fire_add",
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::ADDITIVE),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Fire,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
    },
    PipelineSpec {
        label: "aterm-gpu fire over pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_fire",
        fs: "fs_fire_over",
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Fire,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
    },
    PipelineSpec {
        label: "aterm-gpu deco-over pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_glyph",
        fs: "fs_deco_over",
        target: TargetRole::OffscreenSrgb,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Glyph,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_ATLAS,
    },
    PipelineSpec {
        label: "aterm-gpu deco-add pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_glyph",
        fs: "fs_deco_add",
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::ADDITIVE),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Glyph,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_ATLAS,
    },
    PipelineSpec {
        label: "aterm-gpu scene-over pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_glyph",
        fs: "fs_sprite_over",
        target: TargetRole::OffscreenSrgb,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Glyph,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_ATLAS,
    },
    PipelineSpec {
        label: "aterm-gpu bloom composite pipeline",
        library: ShaderLibrary::Bloom,
        vs: "vs_fs",
        fs: "fs_bloom",
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::SCREEN),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::None,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::POST_FS,
    },
    PipelineSpec {
        label: "aterm-gpu shimmer pipeline",
        library: ShaderLibrary::Shimmer,
        vs: "vs_fs",
        fs: "fs_shimmer",
        target: TargetRole::OffscreenUnorm,
        blend: None,
        write_mask: WriteMask::All,
        vertex: VertexLayout::None,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::POST_FS,
    },
    PipelineSpec {
        label: "aterm-gpu hdr-glow pipeline",
        library: ShaderLibrary::HdrGlow,
        vs: "vs_hdr_glow",
        fs: "fs_hdr_glow",
        target: TargetRole::Edr,
        blend: Some(Blend::ADDITIVE),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Glow,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CROWN,
    },
    PipelineSpec {
        label: "aterm-gpu sdr-glow pipeline",
        library: ShaderLibrary::HdrGlow,
        vs: "vs_hdr_glow",
        fs: "fs_sdr_glow",
        target: TargetRole::Present,
        blend: Some(Blend::SCREEN),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Glow,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CROWN,
    },
    PipelineSpec {
        label: "aterm-gpu blit pipeline",
        library: ShaderLibrary::Blit,
        vs: "vs_blit",
        fs: "fs_blit",
        target: TargetRole::Present,
        blend: Some(Blend::REPLACE),
        write_mask: WriteMask::All,
        vertex: VertexLayout::None,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::POST_FS,
    },
    PipelineSpec {
        label: "aterm-gpu tray pipeline",
        library: ShaderLibrary::Tray,
        vs: "vs_tray",
        fs: "fs_tray",
        target: TargetRole::Present,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::None,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleStrip,
        binds: BindSpec::TRAY,
    },
    PipelineSpec {
        label: "aterm-gpu fx pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_fx",
        fs: "fs_fx",
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::GLOW_OVER),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Fx,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::FX,
    },
    PipelineSpec {
        label: "aterm-gpu fx linear pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_fx",
        fs: "fs_fx_linear",
        target: TargetRole::OffscreenSrgb,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Fx,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::FX,
    },
    PipelineSpec {
        label: "aterm-gpu fx store pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_fx",
        fs: "fs_fx_store",
        target: TargetRole::OffscreenSrgb,
        blend: None,
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Fx,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::FX,
    },
    PipelineSpec {
        label: "aterm-gpu glow items pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_glow_items",
        fs: "fs_glow",
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::GLOW_OVER),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::None,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::GLOW_ITEMS,
    },
    PipelineSpec {
        label: "aterm-gpu hdr-glow items pipeline",
        library: ShaderLibrary::HdrGlow,
        vs: "vs_hdr_glow_items",
        fs: "fs_hdr_glow",
        target: TargetRole::Edr,
        blend: Some(Blend::ADDITIVE),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::None,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CROWN_ITEMS,
    },
    PipelineSpec {
        label: "aterm-gpu sdr-glow items pipeline",
        library: ShaderLibrary::HdrGlow,
        vs: "vs_hdr_glow_items",
        fs: "fs_sdr_glow",
        target: TargetRole::Present,
        blend: Some(Blend::SCREEN),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::None,
        #[cfg(any(gl_arm, d3d11_arm, test))]
        topology: Topology::TriangleList,
        binds: BindSpec::CROWN_ITEMS,
    },
];

/// Total number of entries in [`PIPELINES`].
pub(crate) const PIPELINE_COUNT: usize = 24;

/// Identifier and index into [`PIPELINES`] for each render pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub(crate) enum Pipeline {
    /// Per-cell background fill.
    Bg = 0,
    /// Coverage-blended monochrome glyphs.
    Glyph = 1,
    /// Straight-RGBA color glyphs.
    ColorGlyph = 2,
    /// Translucent cursor fill.
    CursorBlend = 3,
    /// Additive glow pass.
    GlowAdd = 4,
    /// Additive rain bright-head halos.
    RainGlow = 5,
    /// Source-over radial rain veils.
    RainGlowOver = 6,
    /// Additive half of `FirePatch`.
    FireAdd = 7,
    /// Source-over half of `FirePatch`.
    FireOver = 8,
    /// Source-over decorations (paw, undercurl, contrast halo).
    DecoOver = 9,
    /// Additive sparkle decorations.
    DecoAdd = 10,
    /// Source-over sprites (wallpaper, rain, cats).
    SpriteOver = 11,
    /// Bounded bloom composite.
    Bloom = 12,
    /// Heat-haze shimmer displacement.
    Shimmer = 13,
    /// EDR aurora crown.
    #[cfg_attr(
        all(gl_arm, not(test)),
        expect(dead_code, reason = "no f16 destination exists on GL")
    )]
    HdrGlow = 14,
    /// SDR aurora crown.
    SdrGlow = 15,
    /// Offscreen-to-swapchain present blit.
    Blit = 16,
    /// Tray and overlay card blit.
    Tray = 17,
    /// Exact fx layers on the Unorm view.
    Fx = 18,
    /// Linear-light fx layers.
    FxLinear = 19,
    /// Exact fx layers whose every drawn pixel is opaque, stored on the sRGB view.
    FxStore = 20,
    /// [`Self::GlowAdd`] over the bloom's item list: the extract of a stream
    /// with ramps (`aterm_render::bloom::BloomItems`).
    GlowItems = 21,
    /// [`Self::HdrGlow`] over the bloom's item list.
    #[cfg_attr(
        all(gl_arm, not(test)),
        expect(dead_code, reason = "no f16 destination exists on GL")
    )]
    HdrGlowItems = 22,
    /// [`Self::SdrGlow`] over the bloom's item list.
    SdrGlowItems = 23,
}

/// All [`Pipeline`] variants in row order.
#[cfg(any(target_os = "macos", all(test, any(gl_arm, d3d11_arm))))]
pub(crate) const ALL_PIPELINES: [Pipeline; PIPELINE_COUNT] = [
    Pipeline::Bg,
    Pipeline::Glyph,
    Pipeline::ColorGlyph,
    Pipeline::CursorBlend,
    Pipeline::GlowAdd,
    Pipeline::RainGlow,
    Pipeline::RainGlowOver,
    Pipeline::FireAdd,
    Pipeline::FireOver,
    Pipeline::DecoOver,
    Pipeline::DecoAdd,
    Pipeline::SpriteOver,
    Pipeline::Bloom,
    Pipeline::Shimmer,
    Pipeline::HdrGlow,
    Pipeline::SdrGlow,
    Pipeline::Blit,
    Pipeline::Tray,
    Pipeline::Fx,
    Pipeline::FxLinear,
    Pipeline::FxStore,
    Pipeline::GlowItems,
    Pipeline::HdrGlowItems,
    Pipeline::SdrGlowItems,
];

impl Pipeline {
    /// Returns this pipeline's [`PipelineSpec`] in [`PIPELINES`].
    #[inline]
    pub(crate) const fn spec(self) -> &'static PipelineSpec {
        &PIPELINES[self as usize]
    }

    /// Returns the snake_case name of this pipeline.
    #[inline]
    #[cfg(test)]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Bg => "bg",
            Self::Glyph => "glyph",
            Self::ColorGlyph => "color_glyph",
            Self::CursorBlend => "cursor_blend",
            Self::GlowAdd => "glow_add",
            Self::RainGlow => "rain_glow",
            Self::RainGlowOver => "rain_glow_over",
            Self::FireAdd => "fire_add",
            Self::FireOver => "fire_over",
            Self::DecoOver => "deco_over",
            Self::DecoAdd => "deco_add",
            Self::SpriteOver => "sprite_over",
            Self::Bloom => "bloom",
            Self::Shimmer => "shimmer",
            Self::HdrGlow => "hdr_glow",
            Self::SdrGlow => "sdr_glow",
            Self::Blit => "blit",
            Self::Tray => "tray",
            Self::Fx => "fx",
            Self::FxLinear => "fx_linear",
            Self::FxStore => "fx_store",
            Self::GlowItems => "glow_items",
            Self::HdrGlowItems => "hdr_glow_items",
            Self::SdrGlowItems => "sdr_glow_items",
        }
    }
}

/// The framebuffer-fetch twin of a Unorm-law row (Metal on an Apple GPU): the row's vertex stage,
/// layout and binds, drawn into the sRGB view with no fixed-function blend under the row's write
/// mask, by a fragment (`*_fetch` in `cell.metal`, compiled under `ATERM_FETCH`) that applies the
/// row's blend in exact integers to the byte it reads back.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub(crate) enum FetchTwin {
    /// [`Pipeline::GlowAdd`]'s twin: `over_premul`.
    GlowAdd = 0,
    /// [`Pipeline::RainGlow`]'s twin: `add_sat`.
    RainGlow = 1,
    /// [`Pipeline::RainGlowOver`]'s twin: `over_rgb`.
    RainGlowOver = 2,
    /// [`Pipeline::FireAdd`]'s twin: `add_sat`.
    FireAdd = 3,
    /// [`Pipeline::FireOver`]'s twin: `over_rgb`.
    FireOver = 4,
    /// [`Pipeline::DecoAdd`]'s twin: `add_sat` of `premul_rgb`.
    DecoAdd = 5,
    /// [`Pipeline::Fx`]'s twin: `over_premul`.
    Fx = 6,
}

/// How many [`FetchTwin`]s exist.
#[cfg(target_os = "macos")]
pub(crate) const FETCH_TWIN_COUNT: usize = 7;

#[cfg(target_os = "macos")]
impl FetchTwin {
    /// Every twin, in discriminant order.
    #[cfg(test)]
    pub(crate) const ALL: [Self; FETCH_TWIN_COUNT] = [
        Self::GlowAdd,
        Self::RainGlow,
        Self::RainGlowOver,
        Self::FireAdd,
        Self::FireOver,
        Self::DecoAdd,
        Self::Fx,
    ];

    /// The twin of `row`, or `None` for a row that draws on the sRGB view or outside the frame
    /// plan.
    #[inline]
    pub(crate) const fn of(row: Pipeline) -> Option<Self> {
        Some(match row {
            Pipeline::GlowAdd => Self::GlowAdd,
            Pipeline::RainGlow => Self::RainGlow,
            Pipeline::RainGlowOver => Self::RainGlowOver,
            Pipeline::FireAdd => Self::FireAdd,
            Pipeline::FireOver => Self::FireOver,
            Pipeline::DecoAdd => Self::DecoAdd,
            Pipeline::Fx => Self::Fx,
            _ => return None,
        })
    }

    /// The row this twin stands in for.
    #[inline]
    pub(crate) const fn row(self) -> Pipeline {
        match self {
            Self::GlowAdd => Pipeline::GlowAdd,
            Self::RainGlow => Pipeline::RainGlow,
            Self::RainGlowOver => Pipeline::RainGlowOver,
            Self::FireAdd => Pipeline::FireAdd,
            Self::FireOver => Pipeline::FireOver,
            Self::DecoAdd => Pipeline::DecoAdd,
            Self::Fx => Pipeline::Fx,
        }
    }

    /// The twin's fragment entry point and label.
    #[inline]
    const fn fs_and_label(self) -> (&'static str, &'static str) {
        match self {
            Self::GlowAdd => ("fs_glow_fetch", "aterm-gpu glow fetch twin"),
            Self::RainGlow => ("fs_rain_glow_fetch", "aterm-gpu rain halo fetch twin"),
            Self::RainGlowOver => (
                "fs_rain_glow_over_fetch",
                "aterm-gpu rain halo over fetch twin",
            ),
            Self::FireAdd => ("fs_fire_add_fetch", "aterm-gpu fire add fetch twin"),
            Self::FireOver => ("fs_fire_over_fetch", "aterm-gpu fire over fetch twin"),
            Self::DecoAdd => ("fs_deco_add_fetch", "aterm-gpu deco-add fetch twin"),
            Self::Fx => ("fs_fx_fetch", "aterm-gpu fx fetch twin"),
        }
    }

    /// The twin's pipeline: its row's spec, with the twin's fragment, the sRGB view and no
    /// fixed-function blend.
    #[inline]
    pub(crate) const fn spec(self) -> PipelineSpec {
        let (fs, label) = self.fs_and_label();
        PipelineSpec {
            label,
            fs,
            target: TargetRole::OffscreenSrgb,
            blend: None,
            ..*self.row().spec()
        }
    }
}
