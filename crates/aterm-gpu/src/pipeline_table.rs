// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pipeline table: the single declaration of every render pipeline, read
//! by both backends.
//!
//! Each row holds entry points, blend, write mask, colour-target role, vertex
//! layout and topology. The wgpu backend builds every pipeline through
//! [`build_wgpu`], this module's one constructor, whose descriptor parts are
//! private here, and `metal/pipelines.rs` builds `MTLRenderPipelineState`s from
//! the same rows. No other construction site can spell this state, so the
//! backends cannot drift: the workspace `clippy.toml` refuses
//! `wgpu::Device::create_render_pipeline` outside [`build_wgpu`],
//! `metal::pipelines::tests` fails a row Metal cannot express, and the
//! Metal-vs-wgpu byte differentials in `metal::tests` fail a row the two
//! backends draw differently.
//!
//! There is one row per pipeline. `build_glow_boost_pipeline` is one
//! construction site for two rows (the HDR crown, `fs_hdr_glow` on
//! `Rgba16Float` One/One, and the SDR crown, `fs_sdr_glow` on the swapchain
//! format with `Screen`).
//!
//! Not held here: bind-group layouts, buffers, samplers and pipeline layouts
//! (per-backend objects). Where those bind on the explicit-slot backends is the
//! [`BindSpec`] column.

/// Which shader module a pipeline's entry points live in: one WGSL const in
/// `renderer.rs` and its `.metal` twin beside `crate::metal::shaders`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ShaderLibrary {
    /// `renderer.rs::SHADER` / `shaders/cell.metal`.
    Cell,
    /// `renderer.rs::BLIT_SHADER` / `shaders/blit.metal`.
    Blit,
    /// `renderer.rs::HDR_GLOW_SHADER` / `shaders/hdr_glow.metal`.
    HdrGlow,
    /// `renderer.rs::TRAY_SHADER` / `shaders/tray.metal`.
    Tray,
    /// `renderer.rs::BLOOM_SHADER` / `shaders/bloom.metal`.
    Bloom,
    /// `renderer.rs::SHIMMER_SHADER` / `shaders/shimmer.metal`.
    Shimmer,
}

impl ShaderLibrary {
    /// The library's short name — the `.metal` file stem.
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

    /// Every library, in declaration order.
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

/// A blend factor: a closed set mapped exhaustively onto
/// `crate::metal::ffi::BlendFactor` by `metal::pipelines`, so a new factor
/// cannot land on one backend only.
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
    /// `wgpu::BlendFactor::OneMinusSrc` / `MTLBlendFactorOneMinusSourceColor`,
    /// the screen operator's destination factor.
    OneMinusSrc,
}

impl Factor {
    /// The `wgpu` spelling.
    #[cfg(wgpu_arm)]
    pub(crate) const fn wgpu(self) -> wgpu::BlendFactor {
        match self {
            Self::Zero => wgpu::BlendFactor::Zero,
            Self::One => wgpu::BlendFactor::One,
            Self::SrcAlpha => wgpu::BlendFactor::SrcAlpha,
            Self::OneMinusSrcAlpha => wgpu::BlendFactor::OneMinusSrcAlpha,
            Self::OneMinusSrc => wgpu::BlendFactor::OneMinusSrc,
        }
    }
}

/// The blend equation. Every aterm pipeline adds; `Subtract`/`Min`/`Max` are
/// deliberately absent so a new one has to be declared on both backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlendOp {
    /// `src * src_factor + dst * dst_factor`.
    Add,
}

impl BlendOp {
    /// The `wgpu` spelling.
    #[cfg(wgpu_arm)]
    pub(crate) const fn wgpu(self) -> wgpu::BlendOperation {
        match self {
            Self::Add => wgpu::BlendOperation::Add,
        }
    }
}

/// One channel group's blend equation — the RGB half or the alpha half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlendComponent {
    /// Factor applied to the fragment's own output.
    pub(crate) src: Factor,
    /// Factor applied to what the attachment already holds.
    pub(crate) dst: Factor,
    /// How the two products combine.
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

    /// The `wgpu` spelling.
    #[cfg(wgpu_arm)]
    pub(crate) const fn wgpu(self) -> wgpu::BlendComponent {
        wgpu::BlendComponent {
            src_factor: self.src.wgpu(),
            dst_factor: self.dst.wgpu(),
            operation: self.op.wgpu(),
        }
    }
}

/// A pipeline's full blend state, RGB half and alpha half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Blend {
    /// The RGB channels' equation.
    pub(crate) color: BlendComponent,
    /// The alpha channel's equation, stated even under [`WriteMask::Color`],
    /// where its result is discarded.
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
    /// Metal expresses it as blending disabled; [`Self::is_replace`] makes that
    /// swap deliberate.
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

    /// Whether this state is fixed-function REPLACE — i.e. `src*1 + dst*0` on
    /// both halves, which is arithmetically identical to no blending at all.
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

    /// The `wgpu` spelling.
    #[cfg(wgpu_arm)]
    pub(crate) const fn wgpu(self) -> wgpu::BlendState {
        wgpu::BlendState {
            color: self.color.wgpu(),
            alpha: self.alpha.wgpu(),
        }
    }
}

/// Which channels a pipeline's colour attachment may write. Both backends
/// default to all channels, so a forgotten [`Self::Color`] would turn a
/// translucent window opaque; the field has no default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteMask {
    /// `wgpu::ColorWrites::COLOR` — RGB only, alpha left exactly as loaded.
    Color,
    /// `wgpu::ColorWrites::ALL`.
    All,
}

impl WriteMask {
    /// The `wgpu` spelling.
    #[cfg(wgpu_arm)]
    pub(crate) const fn wgpu(self) -> wgpu::ColorWrites {
        match self {
            Self::Color => wgpu::ColorWrites::COLOR,
            Self::All => wgpu::ColorWrites::ALL,
        }
    }
}

/// Which attachment a pipeline draws into: a role, since the format depends on
/// the adapter (`srgb_offscreen`) and the live swapchain.
/// [`TargetFormats::resolve`] maps a role to its format.
///
/// Base OVER/REPLACE passes attach the sRGB view so blending runs in linear
/// light; additive passes attach the Unorm view of the same texture so One/One
/// stays byte-exact with the CPU `add_sat`. A wrong choice shows only as a
/// gamma shift, which is why it is a table column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetRole {
    /// The sRGB-typed view of the offscreen — `GpuContext::offscreen_srgb_view_format`.
    OffscreenSrgb,
    /// The plain-Unorm view of the offscreen — `GpuContext::offscreen_format`.
    OffscreenUnorm,
    /// The destination the call site was handed: the live swapchain format for
    /// the blit / tray / SDR crown.
    Present,
    /// Pinned `Rgba16Float` — the EDR crown, the only format
    /// `format_plan::hdr_present_plan` enables that pass for.
    Edr,
}

/// The formats a construction site can offer a [`TargetRole`], built once from
/// `GpuContext::pipeline_targets`.
#[derive(Clone, Copy, Debug)]
#[cfg(wgpu_arm)]
pub(crate) struct TargetFormats {
    /// `GpuContext::offscreen_srgb_view_format()`.
    pub(crate) offscreen_srgb: wgpu::TextureFormat,
    /// `GpuContext::offscreen_format()`.
    pub(crate) offscreen_unorm: wgpu::TextureFormat,
    /// The destination format this build is for, when the site has one (the
    /// swapchain). `None` at the sites that only ever build offscreen passes.
    pub(crate) present: Option<wgpu::TextureFormat>,
}

#[cfg(wgpu_arm)]
impl TargetFormats {
    /// The same formats, now also able to satisfy [`TargetRole::Present`].
    pub(crate) const fn with_present(self, format: wgpu::TextureFormat) -> Self {
        Self {
            present: Some(format),
            ..self
        }
    }

    /// The concrete colour-attachment format for `role`.
    ///
    /// # Panics
    /// If `role` is [`TargetRole::Present`] and no destination format was
    /// supplied (a construction-time programming error).
    pub(crate) fn resolve(self, role: TargetRole) -> wgpu::TextureFormat {
        match role {
            TargetRole::OffscreenSrgb => self.offscreen_srgb,
            TargetRole::OffscreenUnorm => self.offscreen_unorm,
            TargetRole::Edr => wgpu::TextureFormat::Rgba16Float,
            TargetRole::Present => self
                .present
                .expect("a TargetRole::Present pipeline needs the destination format"),
        }
    }
}

/// The primitive topology. Every aterm draw is a triangle list except the
/// four-vertex tray strip.
#[cfg(any(wgpu_arm, gl_arm, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Topology {
    /// `wgpu::PrimitiveTopology::TriangleList` — `wgpu`'s default.
    TriangleList,
    /// `wgpu::PrimitiveTopology::TriangleStrip` — the tray's 4-vertex quad.
    TriangleStrip,
}

#[cfg(any(wgpu_arm, gl_arm, test))]
impl Topology {
    /// The `wgpu` spelling.
    #[cfg(wgpu_arm)]
    pub(crate) const fn wgpu(self) -> wgpu::PrimitiveTopology {
        match self {
            Self::TriangleList => wgpu::PrimitiveTopology::TriangleList,
            Self::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
        }
    }
}

/// One vertex attribute, backend-neutral.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VertexAttr {
    /// `@location(n)` in WGSL, `[[attribute(n)]]` in MSL.
    pub(crate) location: u32,
    /// The element type.
    pub(crate) format: AttrFormat,
    /// Byte offset within the instance struct.
    pub(crate) offset: u64,
}

/// A vertex attribute's element type. Exactly the seven `renderer.rs` uses;
/// the Metal twin is `crate::metal::ffi::VertexFormat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttrFormat {
    /// `wgpu::VertexFormat::Uint16x4` / `MTLVertexFormatUShort4` — pixel rects,
    /// rain falloff, fire geometry.
    Uint16x4,
    /// `wgpu::VertexFormat::Unorm8x4` / `MTLVertexFormatUChar4Normalized` —
    /// every packed RGBA colour.
    Unorm8x4,
    /// `wgpu::VertexFormat::Uint8x4` / `MTLVertexFormatUChar4`: the fire `tsl`
    /// bytes and the glow colour pair, read as raw integers for the integer
    /// per-column ramp.
    Uint8x4,
    /// `wgpu::VertexFormat::Float32x4` / `MTLVertexFormatFloat4` — glyph rect
    /// and UV.
    Float32x4,
    /// `wgpu::VertexFormat::Uint32` / `MTLVertexFormatUInt` — the fire churn
    /// phase, and the fx palette base and flags.
    Uint32,
    /// `wgpu::VertexFormat::Sint16x2` / `MTLVertexFormatShort2` — the fx
    /// item origin (`FxInstance::origin`, stored mod 2¹⁶).
    Sint16x2,
    /// `wgpu::VertexFormat::Uint16x2` / `MTLVertexFormatUShort2` — the fx
    /// sprite's destination size.
    Uint16x2,
}

impl AttrFormat {
    /// The `wgpu` spelling.
    #[cfg(wgpu_arm)]
    pub(crate) const fn wgpu(self) -> wgpu::VertexFormat {
        match self {
            Self::Uint16x4 => wgpu::VertexFormat::Uint16x4,
            Self::Unorm8x4 => wgpu::VertexFormat::Unorm8x4,
            Self::Uint8x4 => wgpu::VertexFormat::Uint8x4,
            Self::Float32x4 => wgpu::VertexFormat::Float32x4,
            Self::Uint32 => wgpu::VertexFormat::Uint32,
            Self::Sint16x2 => wgpu::VertexFormat::Sint16x2,
            Self::Uint16x2 => wgpu::VertexFormat::Uint16x2,
        }
    }
}

/// A per-instance vertex buffer layout: stride and attributes. Every quad is
/// one instance, so the step mode is always per-instance.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VertexLayoutSpec {
    /// `size_of` the instance struct, pinned to the Rust type by a `const`
    /// assert in `renderer.rs`.
    pub(crate) stride: u64,
    /// The attributes, in `location` order.
    pub(crate) attrs: &'static [VertexAttr],
}

/// Which instance stream a pipeline reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VertexLayout {
    /// No vertex buffer: the fullscreen-triangle and tray passes derive
    /// positions from `[[vertex_id]]`.
    None,
    /// `renderer.rs::BgInstance` — 12 bytes.
    Bg,
    /// `renderer.rs::GlowInstance`, 16 bytes: the rect plus a left/right colour
    /// pair ([`aterm_render::GlowQuad::color2`]). Used by the glow pipeline and
    /// both crowns.
    Glow,
    /// `renderer.rs::GlyphInstance` — 40 bytes.
    Glyph,
    /// `renderer.rs::RainGlowInstance` — 20 bytes.
    RainGlow,
    /// `renderer.rs::FireInstance` — 24 bytes.
    Fire,
    /// `fx_gpu::FxInstance` — 32 bytes: one band slice of one fx item.
    Fx,
}

/// `BgInstance`: `[u16;4]` rect + `[u8;4]` colour.
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

/// `GlowInstance`: `[u16;4]` rect, `[u8;4]` left colour, `[u8;4]` right colour.
/// Both colours are `Uint8x4` because the fragment applies
/// `aterm_render::glow_lerp`'s integer law per column and divides by 255
/// itself, byte-exact with the CPU.
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

/// `GlyphInstance`: `f32x4` rect + `f32x4` uv + `[u8;4]` colour + `[u8;4]` aux.
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

/// `RainGlowInstance`: the bg pair plus the elliptical falloff basis.
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

/// `FireInstance`: rect + geometry + churn phase + the four packed field bytes.
/// The widest layout: four attributes over three distinct integer formats.
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

/// `FxInstance`: the slice rect, the item origin (mod 2¹⁶), the sprite's
/// destination size, the atlas region, the absolute palette base and the
/// flags (kind, flip, texel kind, Index8 reach). Every field is an integer the
/// fragment reads flat: pixel law F has no interpolant.
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
    /// The layout's stride + attributes, or `None` for a vertex-id-only pass.
    #[cfg(any(target_os = "macos", gl_arm))]
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

/// The `wgpu` spelling of one [`VertexAttr`].
#[cfg(wgpu_arm)]
const fn wgpu_attr(a: VertexAttr) -> wgpu::VertexAttribute {
    wgpu::VertexAttribute {
        format: a.format.wgpu(),
        offset: a.offset,
        shader_location: a.location,
    }
}

/// The wgpu attribute array for [`BG_LAYOUT`], derived from it.
#[cfg(wgpu_arm)]
static BG_ATTRS: [wgpu::VertexAttribute; 2] =
    [wgpu_attr(BG_LAYOUT.attrs[0]), wgpu_attr(BG_LAYOUT.attrs[1])];
/// The `wgpu` attribute array for [`GLOW_LAYOUT`].
#[cfg(wgpu_arm)]
static GLOW_ATTRS: [wgpu::VertexAttribute; 3] = [
    wgpu_attr(GLOW_LAYOUT.attrs[0]),
    wgpu_attr(GLOW_LAYOUT.attrs[1]),
    wgpu_attr(GLOW_LAYOUT.attrs[2]),
];
/// The `wgpu` attribute array for [`GLYPH_LAYOUT`].
#[cfg(wgpu_arm)]
static GLYPH_ATTRS: [wgpu::VertexAttribute; 4] = [
    wgpu_attr(GLYPH_LAYOUT.attrs[0]),
    wgpu_attr(GLYPH_LAYOUT.attrs[1]),
    wgpu_attr(GLYPH_LAYOUT.attrs[2]),
    wgpu_attr(GLYPH_LAYOUT.attrs[3]),
];
/// The `wgpu` attribute array for [`RAIN_GLOW_LAYOUT`].
#[cfg(wgpu_arm)]
static RAIN_GLOW_ATTRS: [wgpu::VertexAttribute; 3] = [
    wgpu_attr(RAIN_GLOW_LAYOUT.attrs[0]),
    wgpu_attr(RAIN_GLOW_LAYOUT.attrs[1]),
    wgpu_attr(RAIN_GLOW_LAYOUT.attrs[2]),
];
/// The `wgpu` attribute array for [`FIRE_LAYOUT`].
#[cfg(wgpu_arm)]
static FIRE_ATTRS: [wgpu::VertexAttribute; 4] = [
    wgpu_attr(FIRE_LAYOUT.attrs[0]),
    wgpu_attr(FIRE_LAYOUT.attrs[1]),
    wgpu_attr(FIRE_LAYOUT.attrs[2]),
    wgpu_attr(FIRE_LAYOUT.attrs[3]),
];

/// The `wgpu` attribute array for [`FX_LAYOUT`].
#[cfg(wgpu_arm)]
static FX_ATTRS: [wgpu::VertexAttribute; 6] = [
    wgpu_attr(FX_LAYOUT.attrs[0]),
    wgpu_attr(FX_LAYOUT.attrs[1]),
    wgpu_attr(FX_LAYOUT.attrs[2]),
    wgpu_attr(FX_LAYOUT.attrs[3]),
    wgpu_attr(FX_LAYOUT.attrs[4]),
    wgpu_attr(FX_LAYOUT.attrs[5]),
];

/// The `wgpu` per-instance buffer layout for [`VertexLayout::Bg`].
#[cfg(wgpu_arm)]
static BG_BUFFERS: [wgpu::VertexBufferLayout<'static>; 1] = [wgpu::VertexBufferLayout {
    array_stride: BG_LAYOUT.stride,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &BG_ATTRS,
}];
/// The `wgpu` per-instance buffer layout for [`VertexLayout::Glow`].
#[cfg(wgpu_arm)]
static GLOW_BUFFERS: [wgpu::VertexBufferLayout<'static>; 1] = [wgpu::VertexBufferLayout {
    array_stride: GLOW_LAYOUT.stride,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &GLOW_ATTRS,
}];
/// The `wgpu` per-instance buffer layout for [`VertexLayout::Glyph`].
#[cfg(wgpu_arm)]
static GLYPH_BUFFERS: [wgpu::VertexBufferLayout<'static>; 1] = [wgpu::VertexBufferLayout {
    array_stride: GLYPH_LAYOUT.stride,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &GLYPH_ATTRS,
}];
/// The `wgpu` per-instance buffer layout for [`VertexLayout::RainGlow`].
#[cfg(wgpu_arm)]
static RAIN_GLOW_BUFFERS: [wgpu::VertexBufferLayout<'static>; 1] = [wgpu::VertexBufferLayout {
    array_stride: RAIN_GLOW_LAYOUT.stride,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &RAIN_GLOW_ATTRS,
}];
/// The `wgpu` per-instance buffer layout for [`VertexLayout::Fire`].
#[cfg(wgpu_arm)]
static FIRE_BUFFERS: [wgpu::VertexBufferLayout<'static>; 1] = [wgpu::VertexBufferLayout {
    array_stride: FIRE_LAYOUT.stride,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &FIRE_ATTRS,
}];

/// The `wgpu` per-instance buffer layout for [`VertexLayout::Fx`].
#[cfg(wgpu_arm)]
static FX_BUFFERS: [wgpu::VertexBufferLayout<'static>; 1] = [wgpu::VertexBufferLayout {
    array_stride: FX_LAYOUT.stride,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &FX_ATTRS,
}];

/// Where one row's resources bind on the explicit-slot backends (Metal today;
/// D3D11's per-stage registers take the same numbers, and GL's units and block
/// bindings the buffer and texture ones). `metal/blit.rs` and the encoder call
/// sites take their `set*:atIndex:` indices from here.
///
/// The instance stream is not here; it always binds at
/// `metal::ffi::INSTANCE_STREAM_SLOT`.
///
/// Relation to the WGSL twins:
///
/// * Uniform buffers keep their binding number as the `[[buffer]]` index (cell
///   and hdr_glow at 0; blit, bloom, shimmer, tray at 2).
/// * Textures and samplers take per-type sequential slots in `(group, binding)`
///   order (the blit sampler at `@binding(1)` is `[[sampler(0)]]`).
/// * wgpu's own internal MSL slots are not described here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", gl_arm))]
pub(crate) struct BindSpec {
    /// The vertex-stage uniform `[[buffer(n)]]` index, or `None` for a
    /// `[[vertex_id]]`-only function (`vs_fs`, `vs_blit`). Cell and crown
    /// vertex functions use 0 (safe because the stream binds at
    /// `INSTANCE_STREAM_SLOT`); `vs_tray` uses 2.
    pub(crate) vertex_uniform: Option<u32>,
    /// The fragment-stage `[[texture(n)]]` slots, in slot order.
    pub(crate) fragment_textures: &'static [u32],
    /// The fragment-stage `[[sampler(n)]]` slots, in slot order.
    pub(crate) fragment_samplers: &'static [u32],
    /// Fragment-stage `[[buffer(n)]]` slots in order. `fs_glyph` re-binds the
    /// vertex stage's `Uniforms` (argument tables are per stage); post passes
    /// read their uniform at 2.
    pub(crate) fragment_buffers: &'static [u32],
    /// The fx frame palette's slot, or `None`: Metal binds the per-frame
    /// palette stream as fragment `[[buffer(n)]]` (`device const uint*`); GL,
    /// whose GLSL ES 3.00 has no storage buffer, binds the frame slot's
    /// palette texture as sampler `aterm_t<n>` on texture unit `n`.
    pub(crate) fragment_palette: Option<u32>,
}

#[cfg(any(target_os = "macos", gl_arm))]
impl BindSpec {
    /// The flat cell rows (bg / cursor / glow / rain / fire): `Uniforms` at
    /// vertex `[[buffer(0)]]`, nothing in the fragment stage.
    const CELL_FLAT: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[],
        fragment_samplers: &[],
        fragment_buffers: &[],
        fragment_palette: None,
    };
    /// The atlas-sampling cell rows (colour glyph, deco pair, sprites):
    /// [`Self::CELL_FLAT`] plus the atlas at fragment texture/sampler 0.
    const CELL_ATLAS: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[0],
        fragment_samplers: &[0],
        fragment_buffers: &[],
        fragment_palette: None,
    };
    /// `fs_glyph`: [`Self::CELL_ATLAS`] plus `Uniforms` re-bound at fragment
    /// `[[buffer(0)]]` (it reads `text_blend`).
    const CELL_GLYPH_TEXT: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[0],
        fragment_samplers: &[0],
        fragment_buffers: &[0],
        fragment_palette: None,
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
    };
    /// The crown pair: `HdrU` at vertex `[[buffer(0)]]` and fragment
    /// `[[buffer(0)]]`; no texture (the crown is procedural light).
    const CROWN: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[],
        fragment_samplers: &[],
        fragment_buffers: &[0],
        fragment_palette: None,
    };
    /// The fx rows: `Uniforms` at vertex `[[buffer(0)]]`; the R8 (Index8) and
    /// RGBA8 atlases at fragment textures 0 and 1, read by integer texel
    /// address (`read`, `texelFetch`, `textureLoad`), so no sampler; and the
    /// frame palette at slot 2 ([`Self::fragment_palette`]). GL pairs each
    /// texture unit with the nearest sampler object regardless, so a texture
    /// is complete without mipmaps.
    const FX: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[0, 1],
        fragment_samplers: &[],
        fragment_buffers: &[],
        fragment_palette: Some(2),
    };
    /// The tray: placement uniform at vertex `[[buffer(2)]]`, card texture and
    /// linear sampler at fragment 0.
    const TRAY: Self = Self {
        vertex_uniform: Some(2),
        fragment_textures: &[0],
        fragment_samplers: &[0],
        fragment_buffers: &[],
        fragment_palette: None,
    };
}

/// One pipeline, completely. Every field is stated: nothing here is defaulted,
/// because every default on both backends is the permissive one.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PipelineSpec {
    /// The `wgpu` debug label, and the Metal pipeline's name in diagnostics.
    pub(crate) label: &'static str,
    /// Which shader module both entry points live in.
    pub(crate) library: ShaderLibrary,
    /// The vertex entry point, identical in the WGSL and the MSL twin.
    pub(crate) vs: &'static str,
    /// The fragment entry point, identical in the WGSL and the MSL twin.
    pub(crate) fs: &'static str,
    /// Which attachment this pipeline draws into.
    #[cfg_attr(
        all(gl_arm, not(wgpu_arm)),
        expect(
            dead_code,
            reason = "GL programs are format-free; the wgpu and Metal pipelines key on it"
        )
    )]
    pub(crate) target: TargetRole,
    /// The blend state, or `None` for a pipeline `renderer.rs` builds with
    /// `blend: None` (the shimmer's REPLACE-by-omission).
    pub(crate) blend: Option<Blend>,
    /// Which channels may be written.
    pub(crate) write_mask: WriteMask,
    /// Which instance stream feeds the vertex stage.
    pub(crate) vertex: VertexLayout,
    /// The primitive topology. wgpu builds the pipeline with it; Metal names the
    /// primitive at each draw, so only the tests check this column there.
    #[cfg(any(wgpu_arm, gl_arm, test))]
    pub(crate) topology: Topology,
    /// Where the row's resources bind on the Metal side — see [`BindSpec`].
    #[cfg(any(target_os = "macos", gl_arm))]
    pub(crate) binds: BindSpec,
}

impl PipelineSpec {
    /// The wgpu colour-target state: blend and write mask over the format the
    /// row's [`TargetRole`] resolves to.
    #[cfg(wgpu_arm)]
    fn wgpu_color_target(&self, targets: TargetFormats) -> wgpu::ColorTargetState {
        wgpu::ColorTargetState {
            format: targets.resolve(self.target),
            blend: self.blend.map(Blend::wgpu),
            write_mask: self.write_mask.wgpu(),
        }
    }

    /// The `wgpu` vertex-buffer list — `&[]` for a `[[vertex_id]]`-only pass.
    #[cfg(wgpu_arm)]
    fn wgpu_vertex_buffers(&self) -> &'static [wgpu::VertexBufferLayout<'static>] {
        match self.vertex {
            VertexLayout::None => &[],
            VertexLayout::Bg => &BG_BUFFERS,
            VertexLayout::Glow => &GLOW_BUFFERS,
            VertexLayout::Glyph => &GLYPH_BUFFERS,
            VertexLayout::RainGlow => &RAIN_GLOW_BUFFERS,
            VertexLayout::Fire => &FIRE_BUFFERS,
            VertexLayout::Fx => &FX_BUFFERS,
        }
    }

    /// The `wgpu` primitive state — the row's topology over `wgpu`'s defaults
    /// for everything else (no culling, CCW front face, fill mode).
    #[cfg(wgpu_arm)]
    fn wgpu_primitive(&self) -> wgpu::PrimitiveState {
        wgpu::PrimitiveState {
            topology: self.topology.wgpu(),
            ..Default::default()
        }
    }
}

/// THE ONE `wgpu` RENDER PIPELINE CONSTRUCTOR: build `spec`'s pipeline entirely
/// from its pipeline-table row. Entry points, blend, write mask, vertex format
/// and topology all come from `spec`, which the Metal backend also consumes; the
/// caller supplies only the per-backend plumbing (the compiled module and the
/// layout), and `targets` only resolves the row's [`TargetRole`] to a format.
///
/// The descriptor's parts are private to this module, and the workspace
/// `clippy.toml` refuses `wgpu::Device::create_render_pipeline` everywhere but
/// here, so a second spelling of a blend factor or write mask beside the table
/// does not lint.
#[cfg(wgpu_arm)]
pub(crate) fn build_wgpu(
    spec: &PipelineSpec,
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    targets: TargetFormats,
) -> wgpu::RenderPipeline {
    #[expect(
        clippy::disallowed_methods,
        reason = "the pipeline table's own constructor: every row is built here"
    )]
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(spec.label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(spec.vs),
            compilation_options: Default::default(),
            buffers: spec.wgpu_vertex_buffers(),
        },
        primitive: spec.wgpu_primitive(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(spec.fs),
            compilation_options: Default::default(),
            targets: &[Some(spec.wgpu_color_target(targets))],
        }),
        multiview_mask: None,
        cache: None,
    });
    pipeline
}

/// Every render pipeline aterm builds, in [`Pipeline`] order.
///
/// Index it through [`Pipeline`] rather than by number:
/// `PIPELINES[Pipeline::FireAdd as usize]`, or [`Pipeline::spec`].
pub(crate) static PIPELINES: [PipelineSpec; PIPELINE_COUNT] = [
    // --- the three pipelines every frame draws -----------------------------
    PipelineSpec {
        label: "aterm-gpu bg pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_bg",
        fs: "fs_bg",
        target: TargetRole::OffscreenSrgb,
        blend: Some(Blend::REPLACE),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Bg,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::CELL_FLAT,
    },
    PipelineSpec {
        label: "aterm-gpu glyph pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_glyph",
        fs: "fs_glyph",
        target: TargetRole::OffscreenSrgb,
        // out = fg*cov + dst*(1-cov) in linear light (sRGB target).
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Glyph,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::CELL_GLYPH_TEXT,
    },
    PipelineSpec {
        label: "aterm-gpu colour-glyph pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_glyph",
        fs: "fs_glyph_color",
        target: TargetRole::OffscreenSrgb,
        // out = rgb*a + dst*(1-a) in linear light (sRGB target).
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Glyph,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::CELL_ATLAS,
    },
    // --- the nine demand-built effect pipelines ----------------------------
    PipelineSpec {
        label: "aterm-gpu cursor blend pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_bg",
        fs: "fs_bg",
        target: TargetRole::OffscreenSrgb,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Bg,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::CELL_FLAT,
    },
    PipelineSpec {
        label: "aterm-gpu glow additive pipeline",
        library: ShaderLibrary::Cell,
        // `vs_glow` carries the instance's colour pair and rect span flat to
        // the fragment, which ramps them per column (`GlowInstance`).
        vs: "vs_glow",
        // Glow is raw (no sRGB decode): the Unorm view keeps One/One byte-exact
        // against the CPU `add_sat`.
        fs: "fs_glow",
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::GLOW_OVER),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Glow,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
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
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
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
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
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
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
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
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
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
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
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
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
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
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::CELL_ATLAS,
    },
    // --- the two lazily-built post passes over the offscreen ---------------
    PipelineSpec {
        label: "aterm-gpu bloom composite pipeline",
        library: ShaderLibrary::Bloom,
        vs: "vs_fs",
        fs: "fs_bloom",
        target: TargetRole::OffscreenUnorm,
        // `Screen` spends only the destination's remaining headroom.
        blend: Some(Blend::SCREEN),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::None,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::POST_FS,
    },
    PipelineSpec {
        label: "aterm-gpu shimmer pipeline",
        library: ShaderLibrary::Shimmer,
        vs: "vs_fs",
        fs: "fs_shimmer",
        target: TargetRole::OffscreenUnorm,
        // REPLACE by omission: the fragment is the refracted frame.
        blend: None,
        // All channels: the refraction displaces the whole sample, so alpha
        // stays consistent with the rgb on a translucent present.
        write_mask: WriteMask::All,
        vertex: VertexLayout::None,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::POST_FS,
    },
    // --- the glow-boost pair: one construction site, two pipelines ---------
    PipelineSpec {
        label: "aterm-gpu hdr-glow pipeline",
        library: ShaderLibrary::HdrGlow,
        vs: "vs_hdr_glow",
        fs: "fs_hdr_glow",
        target: TargetRole::Edr,
        // EDR stays additive: the f16 target's whole point is emission above
        // 1.0, so there is no `1 - dst` headroom to divide up.
        blend: Some(Blend::ADDITIVE),
        write_mask: WriteMask::Color,
        // The crown reads the same `GlowInstance` stream the bloom extract
        // does, so a gradient quad ramps in the crown as it does on the glass.
        vertex: VertexLayout::Glow,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::CROWN,
    },
    PipelineSpec {
        label: "aterm-gpu sdr-glow pipeline",
        library: ShaderLibrary::HdrGlow,
        vs: "vs_hdr_glow",
        fs: "fs_sdr_glow",
        target: TargetRole::Present,
        // `Screen`: the crown brightens without saturating.
        blend: Some(Blend::SCREEN),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Glow,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::CROWN,
    },
    // --- the two swapchain passes ------------------------------------------
    PipelineSpec {
        label: "aterm-gpu blit pipeline",
        library: ShaderLibrary::Blit,
        vs: "vs_blit",
        fs: "fs_blit",
        target: TargetRole::Present,
        blend: Some(Blend::REPLACE),
        // The blit publishes the alpha the RGB-only effect rows leave
        // untouched.
        write_mask: WriteMask::All,
        vertex: VertexLayout::None,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::POST_FS,
    },
    PipelineSpec {
        label: "aterm-gpu tray pipeline",
        library: ShaderLibrary::Tray,
        vs: "vs_tray",
        fs: "fs_tray",
        target: TargetRole::Present,
        // Straight-alpha src-over == CPU `composite_tray`.
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::None,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleStrip,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::TRAY,
    },
    // --- the fx engine's two rows (design §4.5-§4.6) -----------------------
    PipelineSpec {
        label: "aterm-gpu fx pipeline",
        library: ShaderLibrary::Cell,
        // Pixel law F over flat integer varyings: the fragment addresses its
        // texel and its ramp column from the absolute pixel and the item's
        // unclamped origin, never a sampler or a float UV.
        vs: "vs_fx",
        fs: "fs_fx",
        // `fs_glow`'s state: `(c, a) / 255` raw into `src + dst·(1 − a)` on
        // the Unorm view rounds once on store, byte-exact with the CPU
        // `over_premul` (`add_sat` at `a = 0`).
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::GLOW_OVER),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Fx,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::FX,
    },
    PipelineSpec {
        label: "aterm-gpu fx linear pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_fx",
        // A Linear layer's straight texels under their tint, decoded and
        // blended in linear light: the scene-over row's target and blend
        // (FreeSprite's law and tolerance).
        fs: "fs_fx_linear",
        target: TargetRole::OffscreenSrgb,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Fx,
        #[cfg(any(wgpu_arm, gl_arm, test))]
        topology: Topology::TriangleList,
        #[cfg(any(target_os = "macos", gl_arm))]
        binds: BindSpec::FX,
    },
];

/// How many rows [`PIPELINES`] has. A `const` so the array length is visible at
/// its definition, exactly like `EFFECT_PIPELINE_COUNT`.
pub(crate) const PIPELINE_COUNT: usize = 20;

/// A row of [`PIPELINES`], by name. The discriminants are the indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub(crate) enum Pipeline {
    /// Per-cell background fill.
    Bg = 0,
    /// Coverage-blended mono glyphs.
    Glyph = 1,
    /// Straight-RGBA colour emoji.
    ColorGlyph = 2,
    /// Translucent cursor fill.
    CursorBlend = 3,
    /// LUMEN cursor aurora / supernova / EMBERFORGE under-glyph light.
    GlowAdd = 4,
    /// PHOSPHOR rain bright-head halos.
    RainGlow = 5,
    /// `HaloMode::Over` radial veils.
    RainGlowOver = 6,
    /// EMBERFORGE FirePatch, One/One half.
    FireAdd = 7,
    /// EMBERFORGE FirePatch, source-over half.
    FireOver = 8,
    /// Sparkle-word paw, undercurl, glyph contrast-halo.
    DecoOver = 9,
    /// Sparkle-word sparkle.
    DecoAdd = 10,
    /// Wallpaper, rain, cats, free sprites.
    SpriteOver = 11,
    /// The bounded bloom composite.
    Bloom = 12,
    /// The EMBERFORGE heat-haze displacement.
    Shimmer = 13,
    /// The EDR aurora crown.
    #[cfg_attr(
        all(gl_arm, not(wgpu_arm), not(test)),
        expect(dead_code, reason = "no f16 destination exists on GL")
    )]
    HdrGlow = 14,
    /// The SDR aurora crown.
    SdrGlow = 15,
    /// The offscreen -> swapchain present blit.
    Blit = 16,
    /// The tray/overlay card blit.
    Tray = 17,
    /// The fx engine's Exact layers (pixel law F on the Unorm view).
    Fx = 18,
    /// The fx engine's Linear layers (straight texels, linear-light blend).
    FxLinear = 19,
}

/// Every [`Pipeline`], in row order — the single enumeration a sweep walks.
#[cfg(all(test, any(target_os = "macos", gl_arm)))]
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
];

impl Pipeline {
    /// This pipeline's row. [`PIPELINES`] is a `static`, so every lookup returns
    /// the same object.
    pub(crate) fn spec(self) -> &'static PipelineSpec {
        &PIPELINES[self as usize]
    }

    /// The row's stable name — the enum variant, snake-cased, for test
    /// failure messages.
    #[cfg(all(test, any(target_os = "macos", gl_arm)))]
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
        }
    }
}
