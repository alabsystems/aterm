// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PIPELINE TABLE — the one declaration of every render pipeline aterm
//! builds, read by BOTH backends.
//!
//! # Why this file exists
//!
//! `renderer.rs` used to spell each pipeline's entry points, blend factors,
//! write mask, colour-target role and vertex layout as a 40-line `wgpu`
//! descriptor literal at each of its seventeen `create_render_pipeline` call
//! sites, and `metal/` re-spelled a handful of them by hand in its tests. A
//! judge proved what that costs: the two Metal pipeline tests each papered over
//! the missing colour write mask IN A DIFFERENT AND MUTUALLY INCONSISTENT WAY
//! and so certified a state the renderer does not use —
//!
//! * the fire test built `(One, One, One, One)` with NO write mask, which is
//!   not `renderer.rs`'s fire-add state (that one is RGB-only);
//! * the EDR test substituted `(Zero, One)` on the alpha channel, which is not
//!   `renderer.rs`'s `alpha: add` either — it passed only because
//!   `fs_hdr_glow` happens to emit alpha `0.0`;
//! * both built on a `Bgra8*` attachment while the offscreen the renderer
//!   attaches is `Rgba8Unorm` / `Rgba8UnormSrgb`.
//!
//! Each of those is a hand-copied constant that drifted, and a hand-copied
//! constant is exactly what cannot be trusted to keep two backends in step. So
//! the state moved HERE, once, and both consumers read it:
//!
//! * `renderer.rs` builds its `wgpu` descriptors out of [`PIPELINES`]
//!   ([`PipelineSpec::wgpu_color_target`], [`PipelineSpec::wgpu_vertex_buffers`],
//!   [`PipelineSpec::wgpu_primitive`], `spec.vs` / `spec.fs`), so there is no
//!   longer a blend factor, a write mask or an entry-point string spelled at a
//!   construction site at all;
//! * `metal/pipelines.rs` builds real `MTLRenderPipelineState` objects out of
//!   the same rows, and `metal/shaders.rs` derives its MSL entry-point roster
//!   from them.
//!
//! The bar this is held to is `metal::pipelines::tests` plus
//! `renderer::pipeline_table_tests`: a construction site that re-inlines a
//! literal fails [`crate::renderer::pipeline_table_tests`]'s source scan, and a
//! table row Metal cannot express fails the Metal pipeline sweep.
//!
//! # Seventeen sites, eighteen pipelines
//!
//! `renderer.rs` has seventeen `create_render_pipeline` call sites but builds
//! EIGHTEEN distinct pipelines: `build_glow_boost_pipeline` is one site
//! parameterised twice, once for the EDR crown (`fs_hdr_glow`, `Rgba16Float`,
//! One/One) and once for its SDR twin (`fs_sdr_glow`, the live swapchain
//! format, `Screen`). The table has one row per PIPELINE, because the two rows
//! differ in every field that matters.
//!
//! # What the table deliberately does NOT hold
//!
//! Bind-group layouts, uniform buffers, samplers and pipeline layouts — the
//! per-backend resource OBJECTS. What it DOES hold since W2 is WHERE those
//! objects bind on the first-party Metal side: [`BindSpec`], one column per
//! row, replacing the prose-only maps that used to live in
//! `crate::metal::blit`'s header and `ffi::draw_and_read`'s doc. The MSL scan
//! in `crate::metal::shaders` guards the column against the `.metal` sources
//! in both directions.

/// Which shader module a pipeline's entry points live in. One variant per
/// WGSL `const &str` in `renderer.rs` and per `.metal` file beside
/// `crate::metal::shaders`; the two are twins, so this names both.
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
    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
    /// The library's short name — the `.metal` file stem, and the key
    /// `crate::metal::shaders::libraries` reports under.
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
    pub(crate) const ALL: [Self; 6] = [
        Self::Cell,
        Self::Blit,
        Self::HdrGlow,
        Self::Tray,
        Self::Bloom,
        Self::Shimmer,
    ];
}

/// A blend factor. Exactly the five `renderer.rs` uses — a closed set on
/// purpose, so a sixth cannot appear on one backend without a matching
/// `MTLBlendFactor` on the other (`crate::metal::ffi::BlendFactor` is the twin,
/// and `metal::pipelines` maps this enum onto it exhaustively).
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
    /// `wgpu::BlendFactor::OneMinusSrc` / `MTLBlendFactorOneMinusSourceColor` —
    /// the SCREEN operator's destination factor (`BoostComposite::Screen`). It
    /// is the one factor no Metal pipeline in this tree could express before
    /// the table existed, because `ffi::BlendFactor` did not declare it.
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

    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
    /// A short stable name, for the table listing the golden test pins.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Zero => "Zero",
            Self::One => "One",
            Self::SrcAlpha => "SrcAlpha",
            Self::OneMinusSrcAlpha => "OneMinusSrcAlpha",
            Self::OneMinusSrc => "OneMinusSrc",
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

    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
    /// A short stable name, for the golden listing.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Add => "Add",
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
    /// `src * src_a + dst * (1 - src_a)` — the STRAIGHT-alpha source-over RGB
    /// half of `wgpu::BlendState::ALPHA_BLENDING`.
    const STRAIGHT_OVER: Self = Self {
        src: Factor::SrcAlpha,
        dst: Factor::OneMinusSrcAlpha,
        op: BlendOp::Add,
    };
    /// `src + dst` — the premultiplied additive light shared by the aurora, the
    /// rain halos, the fire-add half, the sparkle-add and the EDR crown.
    const ADD: Self = Self {
        src: Factor::One,
        dst: Factor::One,
        op: BlendOp::Add,
    };
    /// `src + dst * (1 - src)` — SCREEN, `BoostComposite::Screen`.
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
    /// The alpha channel's equation. It still runs when the write mask is
    /// [`WriteMask::Color`] — the result is simply discarded — so it is stated
    /// rather than left implicit.
    pub(crate) alpha: BlendComponent,
}

impl Blend {
    /// `wgpu::BlendState::ALPHA_BLENDING`: straight-alpha source-over on RGB,
    /// premultiplied source-over on alpha.
    const ALPHA_BLENDING: Self = Self {
        color: BlendComponent::STRAIGHT_OVER,
        alpha: BlendComponent::OVER,
    };
    /// `wgpu::BlendState::REPLACE`. Kept as an explicit BLEND rather than
    /// `None` only where `renderer.rs` spells it that way; Metal expresses the
    /// identical fixed-function state as blending DISABLED, and
    /// [`Self::is_replace`] is what lets the Metal side make that swap
    /// deliberately instead of by accident.
    const REPLACE: Self = Self {
        color: BlendComponent::REPLACE,
        alpha: BlendComponent::REPLACE,
    };
    /// `One`/`One` on both halves.
    const ADDITIVE: Self = Self {
        color: BlendComponent::ADD,
        alpha: BlendComponent::ADD,
    };
    /// `BoostComposite::Screen` on both halves.
    const SCREEN: Self = Self {
        color: BlendComponent::SCREEN,
        alpha: BlendComponent::SCREEN,
    };
    /// The one-sided generalization the flat `GlowQuad` streams take:
    /// `out = src + dst*(1 - src_a)`, which IS `One`/`One` at `src_a == 0`.
    const GLOW_OVER: Self = Self {
        color: BlendComponent::OVER,
        alpha: BlendComponent::OVER,
    };

    /// Whether this state is fixed-function REPLACE — i.e. `src*1 + dst*0` on
    /// both halves, which is arithmetically identical to no blending at all.
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

/// Which channels a pipeline's colour attachment may write.
///
/// Metal's DEFAULT is ALL and `wgpu`'s is ALL, so a pipeline that MEANT
/// [`Self::Color`] and forgot to say so is silent on both backends until a
/// translucent window goes opaque. Ten of the eighteen rows below are
/// [`Self::Color`]; the field is not defaulted anywhere.
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

    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
    /// A short stable name, for the golden listing.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Color => "COLOR",
            Self::All => "ALL",
        }
    }
}

/// WHICH attachment a pipeline draws into — a role, not a format, because the
/// concrete format depends on the adapter (`srgb_offscreen`) and on the live
/// swapchain. [`TargetFormats::resolve`] turns a role into the format.
///
/// The sRGB/Unorm split is THE FORMAT LAW (see `crate::metal::shaders`): the
/// base OVER/REPLACE passes attach the sRGB-typed VIEW so fixed-function
/// blending composites in linear light, and the additive passes attach the
/// plain Unorm view of the SAME texture so `One`/`One` stays byte-exact against
/// the CPU `add_sat`. Getting that backwards is invisible to a pipeline build
/// and visible only as a gamma shift in the frame, which is precisely why the
/// choice is a table row rather than an argument at the call site.
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

/// The formats a construction site can offer a [`TargetRole`]. Built once from
/// the `GpuContext` (`GpuContext::pipeline_targets`) so a site cannot hand a
/// pipeline the wrong one of the offscreen's two views.
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
    /// If `role` is [`TargetRole::Present`] and this site has no destination
    /// format. That is a construction-time programming error (a pipeline built
    /// from the wrong seam), not a runtime condition.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Topology {
    /// `wgpu::PrimitiveTopology::TriangleList` — `wgpu`'s default.
    TriangleList,
    /// `wgpu::PrimitiveTopology::TriangleStrip` — the tray's 4-vertex quad.
    TriangleStrip,
}

impl Topology {
    /// The `wgpu` spelling.
    #[cfg(wgpu_arm)]
    pub(crate) const fn wgpu(self) -> wgpu::PrimitiveTopology {
        match self {
            Self::TriangleList => wgpu::PrimitiveTopology::TriangleList,
            Self::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
        }
    }

    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
    /// A short stable name, for the golden listing.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::TriangleList => "TriangleList",
            Self::TriangleStrip => "TriangleStrip",
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

/// A vertex attribute's element type. Exactly the five `renderer.rs` uses; the
/// Metal twin is `crate::metal::ffi::VertexFormat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttrFormat {
    /// `wgpu::VertexFormat::Uint16x4` / `MTLVertexFormatUShort4` — pixel rects,
    /// rain falloff, fire geometry.
    Uint16x4,
    /// `wgpu::VertexFormat::Unorm8x4` / `MTLVertexFormatUChar4Normalized` —
    /// every packed RGBA colour.
    Unorm8x4,
    /// `wgpu::VertexFormat::Uint8x4` / `MTLVertexFormatUChar4` — the fire `tsl`
    /// bytes, read as raw integers.
    Uint8x4,
    /// `wgpu::VertexFormat::Float32x4` / `MTLVertexFormatFloat4` — glyph rect
    /// and UV.
    Float32x4,
    /// `wgpu::VertexFormat::Uint32` / `MTLVertexFormatUInt` — the fire churn
    /// phase.
    Uint32,
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
        }
    }

    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
    /// A short stable name, for the golden listing.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Uint16x4 => "Uint16x4",
            Self::Unorm8x4 => "Unorm8x4",
            Self::Uint8x4 => "Uint8x4",
            Self::Float32x4 => "Float32x4",
            Self::Uint32 => "Uint32",
        }
    }
}

/// A per-INSTANCE vertex buffer layout: the instance struct's stride and its
/// attributes. Every aterm quad is 6 (or 4) vertices of one instance, so the
/// step mode is per-instance for all of them and is not a field.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VertexLayoutSpec {
    /// The instance struct's name, for diagnostics and the golden listing.
    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
    pub(crate) name: &'static str,
    /// `size_of` the instance struct. Pinned to the Rust type by a
    /// `const _: () = assert!(...)` beside each struct in `renderer.rs`.
    pub(crate) stride: u64,
    /// The attributes, in `location` order.
    pub(crate) attrs: &'static [VertexAttr],
}

/// Which instance stream a pipeline reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VertexLayout {
    /// No vertex buffer at all: the fullscreen-triangle and tray passes derive
    /// their positions from `[[vertex_id]]` (`buffers: &[]` in `wgpu`, and no
    /// `MTLVertexDescriptor` in Metal).
    None,
    /// `renderer.rs::BgInstance` — 12 bytes.
    Bg,
    /// `renderer.rs::GlyphInstance` — 40 bytes.
    Glyph,
    /// `renderer.rs::RainGlowInstance` — 20 bytes.
    RainGlow,
    /// `renderer.rs::FireInstance` — 24 bytes.
    Fire,
}

/// `BgInstance`: `[u16;4]` rect + `[u8;4]` colour.
pub(crate) const BG_LAYOUT: VertexLayoutSpec = VertexLayoutSpec {
    name: "BgInstance",
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

/// `GlyphInstance`: `f32x4` rect + `f32x4` uv + `[u8;4]` colour + `[u8;4]` aux.
pub(crate) const GLYPH_LAYOUT: VertexLayoutSpec = VertexLayoutSpec {
    name: "GlyphInstance",
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
    name: "RainGlowInstance",
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
    name: "FireInstance",
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

impl VertexLayout {
    /// The layout's stride + attributes, or `None` for a vertex-id-only pass.
    pub(crate) const fn spec(self) -> Option<VertexLayoutSpec> {
        match self {
            Self::None => None,
            Self::Bg => Some(BG_LAYOUT),
            Self::Glyph => Some(GLYPH_LAYOUT),
            Self::RainGlow => Some(RAIN_GLOW_LAYOUT),
            Self::Fire => Some(FIRE_LAYOUT),
        }
    }

    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
    /// A short stable name, for the golden listing.
    pub(crate) const fn name(self) -> &'static str {
        match self.spec() {
            None => "-",
            Some(s) => s.name,
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

/// The `wgpu` attribute array for [`BG_LAYOUT`], derived from it rather than
/// re-spelled — this is what `renderer.rs::BG_ATTRS` used to be.
#[cfg(wgpu_arm)]
static BG_ATTRS: [wgpu::VertexAttribute; 2] =
    [wgpu_attr(BG_LAYOUT.attrs[0]), wgpu_attr(BG_LAYOUT.attrs[1])];
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

/// The `wgpu` per-instance buffer layout for [`VertexLayout::Bg`].
#[cfg(wgpu_arm)]
static BG_BUFFERS: [wgpu::VertexBufferLayout<'static>; 1] = [wgpu::VertexBufferLayout {
    array_stride: BG_LAYOUT.stride,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &BG_ATTRS,
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

/// WHERE one row's resources bind on the first-party Metal side — the per-row
/// binding map that used to be prose in two places (`crate::metal::blit`'s
/// header table and `ffi::draw_and_read`'s hardcoded 0/0/2) and is now a
/// column both consumers read: `metal/blit.rs` and the encoder call sites
/// spell their `set*:atIndex:` indices FROM here, and the MSL scan in
/// `crate::metal::shaders` checks every `.metal` entry point against it in
/// BOTH directions (a declared-but-untabled binding and a tabled-but-
/// undeclared binding both fail, naming file and line).
///
/// The instance stream is NOT here: it always binds at the one deconflicted
/// vertex-buffer slot (`metal::ffi::INSTANCE_STREAM_SLOT`), reached only
/// through the vertex descriptor / `set_instance_stream`, never spelled by a
/// row.
///
/// # The wgpu cross-check, and exactly how far it reaches
///
/// The WGSL twins of these shaders declare the same resources as
/// `@group(g) @binding(b)`, and the seven `BindGroupLayout`s in `renderer.rs`
/// restate them per layout. Where the mapping is expressible it is guarded
/// (`renderer::tests::the_wgsl_binding_map_agrees_with_the_tables_bind_column`
/// scans the WGSL consts against this column under the law below); where it is
/// not, this doc says so rather than forcing it:
///
/// * **Uniform BUFFERS map 1:1 by number**: a `var<uniform>` at
///   `@binding(b)` keeps `b` as its Metal `[[buffer]]` index — cell and
///   hdr_glow at 0, blit/bloom/shimmer/tray at 2. Expressible, guarded.
/// * **Texture and sampler NUMBERS do not map 1:1.** WGSL binding numbers
///   share one per-group space while Metal keeps three per-stage, per-type
///   spaces, so the twins give textures and samplers per-type sequential
///   slots in `(group, binding)` order: the blit's sampler at
///   `@binding(1)` is `[[sampler(0)]]`. The guarded mapping is therefore
///   "b-th texture/sampler of the module -> slot b-th of its type", not "same
///   number".
/// * **The stage split is not expressible from the wgpu side at all.** WGSL
///   module-scope declarations and `BindGroupLayoutEntry::visibility` are
///   stage UPPER BOUNDS: the cell `Uniforms` layout is VERTEX_FRAGMENT-visible
///   for all twelve cell rows, but only `fs_glyph` declares the fragment-stage
///   bind in MSL, because an MSL entry point declares what it READS. So the
///   WGSL cross-check compares the per-LIBRARY union, and the per-entry-point
///   split is the MSL scan's job alone.
/// * **wgpu's own internal MSL slots are not claimed here.** On macOS wgpu
///   derives naga's `BindingMap` from the `BindGroupLayout` at pipeline build;
///   that map is invisible to this crate and free to differ. This column names
///   the FIRST-PARTY MSL slots only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BindSpec {
    /// The VERTEX-stage uniform block's `[[buffer(n)]]` index, or `None` for a
    /// `[[vertex_id]]`-only vertex function that reads no uniform (`vs_fs`,
    /// `vs_blit`). Cell and crown vertex functions sit at 0 — safe ONLY
    /// because the instance stream binds at `INSTANCE_STREAM_SLOT` — while
    /// `vs_tray` (no `[[stage_in]]`, so no collision to dodge) kept its WGSL
    /// binding and says 2.
    pub(crate) vertex_uniform: Option<u32>,
    /// The FRAGMENT-stage `[[texture(n)]]` slots, in slot order.
    pub(crate) fragment_textures: &'static [u32],
    /// The FRAGMENT-stage `[[sampler(n)]]` slots, in slot order.
    pub(crate) fragment_samplers: &'static [u32],
    /// The FRAGMENT-stage `[[buffer(n)]]` slots, in slot order. `fs_glyph`
    /// re-binds the SAME 16-byte `Uniforms` block the vertex stage reads
    /// (Metal buffer argument tables are per-stage, so one buffer costs two
    /// binds); the post passes read their own uniform at 2.
    pub(crate) fragment_buffers: &'static [u32],
}

impl BindSpec {
    /// The flat cell rows (bg / cursor / glow / rain / fire): `Uniforms` at
    /// vertex `[[buffer(0)]]`, nothing in the fragment stage.
    const CELL_FLAT: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[],
        fragment_samplers: &[],
        fragment_buffers: &[],
    };
    /// The atlas-sampling cell rows (colour glyph, deco pair, sprites):
    /// [`Self::CELL_FLAT`] plus the atlas at fragment texture/sampler 0.
    const CELL_ATLAS: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[0],
        fragment_samplers: &[0],
        fragment_buffers: &[],
    };
    /// `fs_glyph` alone: [`Self::CELL_ATLAS`] plus the `Uniforms` block
    /// re-bound to the fragment stage at `[[buffer(0)]]` (it reads
    /// `text_blend`, the corrected-alpha remap gate).
    const CELL_GLYPH_TEXT: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[0],
        fragment_samplers: &[0],
        fragment_buffers: &[0],
    };
    /// The `[[vertex_id]]` post passes (bloom / shimmer / blit): no vertex
    /// uniform; the staged frame at fragment texture/sampler 0 and the pass's
    /// own uniform at fragment `[[buffer(2)]]` (the WGSL `@binding(2)`, kept).
    const POST_FS: Self = Self {
        vertex_uniform: None,
        fragment_textures: &[0],
        fragment_samplers: &[0],
        fragment_buffers: &[2],
    };
    /// The crown pair: `HdrU` at vertex `[[buffer(0)]]` AND fragment
    /// `[[buffer(0)]]`; no texture (the crown is procedural light).
    const CROWN: Self = Self {
        vertex_uniform: Some(0),
        fragment_textures: &[],
        fragment_samplers: &[],
        fragment_buffers: &[0],
    };
    /// The tray: placement uniform at vertex `[[buffer(2)]]`, card texture and
    /// LINEAR sampler at fragment 0.
    const TRAY: Self = Self {
        vertex_uniform: Some(2),
        fragment_textures: &[0],
        fragment_samplers: &[0],
        fragment_buffers: &[],
    };

    /// One stable text form for the golden listing: `vu(n|-) ft(..|-)
    /// fsamp(..|-) fb(..|-)`.
    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
    pub(crate) fn listing(&self) -> String {
        fn slots(s: &[u32]) -> String {
            if s.is_empty() {
                "-".to_owned()
            } else {
                s.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
            }
        }
        let vu = match self.vertex_uniform {
            None => "-".to_owned(),
            Some(n) => n.to_string(),
        };
        format!(
            "vu({vu}) ft({}) fsamp({}) fb({})",
            slots(self.fragment_textures),
            slots(self.fragment_samplers),
            slots(self.fragment_buffers),
        )
    }
}

/// The LOAD-OP CONVENTION of the pass a row draws in — the second half of the
/// map's per-row PASS metadata (the first, the target role, is
/// [`PipelineSpec::target`]). DECLARATIVE ONLY, on purpose: W4 ports the
/// encode ladder and the pass coalescer, and that machinery will READ this
/// column; nothing consumes it yet beyond the golden listing, because
/// inventing pass-descriptor plumbing one wave early is exactly what W2's
/// brief forbids.
///
/// A row PINS a load op only when every production pass it draws in opens the
/// same way (site audit, renderer.rs 2026-08-31): the blit pass always Clears
/// (`:8512`, the swapchain/virtual attach), and the bloom composite
/// (`:9405`), shimmer (`:10026`) and both tray passes (`:11342`, `:11446`)
/// always Load — including through the in-place twins, which reuse the same
/// pass-opening functions (`encode_bloom_halo`, `encode_shimmer`). Everything
/// else is [`Self::Dynamic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PassLoad {
    /// The row's pass always opens with a Clear (the clear COLOUR stays the
    /// call site's: theme background for the blit).
    Clear,
    /// The row's pass always opens with Load.
    Load,
    /// Not this row's to pin: the twelve cell rows ride the coalesced frame
    /// plan's Clear-or-Load (pass 0 clears-or-loads per damage, later passes
    /// Load — `coalesce_frame_passes` decides); `glow_add` ADDITIONALLY opens
    /// the bloom-extract pass with Clear(TRANSPARENT) on bloom frames, which
    /// is precisely why it cannot pin either; the two crowns never open a
    /// pass at all (they draw inside the blit's).
    Dynamic,
}

impl PassLoad {
    /// A short stable name, for the golden listing.
    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Clear => "Clear",
            Self::Load => "Load",
            Self::Dynamic => "dyn",
        }
    }
}

/// One pipeline, completely. Every field is stated: nothing here is defaulted,
/// because every default on both backends is the permissive one.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PipelineSpec {
    /// The `wgpu` debug label, and the Metal pipeline's name in diagnostics.
    pub(crate) label: &'static str,
    /// Which shader module both entry points live in.
    pub(crate) library: ShaderLibrary,
    /// The vertex entry point, IDENTICAL in the WGSL and the MSL twin.
    pub(crate) vs: &'static str,
    /// The fragment entry point, IDENTICAL in the WGSL and the MSL twin.
    pub(crate) fs: &'static str,
    /// Which attachment this pipeline draws into.
    pub(crate) target: TargetRole,
    /// The blend state, or `None` for a pipeline `renderer.rs` builds with
    /// `blend: None` (the shimmer's REPLACE-by-omission).
    pub(crate) blend: Option<Blend>,
    /// Which channels may be written.
    pub(crate) write_mask: WriteMask,
    /// Which instance stream feeds the vertex stage.
    pub(crate) vertex: VertexLayout,
    /// The primitive topology.
    pub(crate) topology: Topology,
    /// Where the row's resources bind on the Metal side — see [`BindSpec`].
    pub(crate) binds: BindSpec,
    /// The load-op convention of the pass this row draws in — see [`PassLoad`].
    pub(crate) pass_load: PassLoad,
}

impl PipelineSpec {
    /// The `wgpu` colour-target state: this row's blend + write mask, over the
    /// format its [`TargetRole`] resolves to. The ONE place a `renderer.rs`
    /// construction site gets a `ColorTargetState` from.
    #[cfg(wgpu_arm)]
    pub(crate) fn wgpu_color_target(&self, targets: TargetFormats) -> wgpu::ColorTargetState {
        wgpu::ColorTargetState {
            format: targets.resolve(self.target),
            blend: self.blend.map(Blend::wgpu),
            write_mask: self.write_mask.wgpu(),
        }
    }

    /// The `wgpu` vertex-buffer list — `&[]` for a `[[vertex_id]]`-only pass.
    #[cfg(wgpu_arm)]
    pub(crate) fn wgpu_vertex_buffers(&self) -> &'static [wgpu::VertexBufferLayout<'static>] {
        match self.vertex {
            VertexLayout::None => &[],
            VertexLayout::Bg => &BG_BUFFERS,
            VertexLayout::Glyph => &GLYPH_BUFFERS,
            VertexLayout::RainGlow => &RAIN_GLOW_BUFFERS,
            VertexLayout::Fire => &FIRE_BUFFERS,
        }
    }

    /// The `wgpu` primitive state — the row's topology over `wgpu`'s defaults
    /// for everything else (no culling, CCW front face, fill mode).
    #[cfg(wgpu_arm)]
    pub(crate) fn wgpu_primitive(&self) -> wgpu::PrimitiveState {
        wgpu::PrimitiveState {
            topology: self.topology.wgpu(),
            ..Default::default()
        }
    }

    /// One line of the golden listing — every field that decides a pixel, in a
    /// stable textual form. See [`listing`].
    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
    pub(crate) fn listing(&self, name: &str) -> String {
        let blend = match self.blend {
            None => "none".to_owned(),
            Some(b) => format!(
                "rgb({} {} {}) a({} {} {})",
                b.color.src.name(),
                b.color.dst.name(),
                b.color.op.name(),
                b.alpha.src.name(),
                b.alpha.dst.name(),
                b.alpha.op.name(),
            ),
        };
        format!(
            "{name} lib={} vs={} fs={} target={:?} blend={blend} mask={} vertex={} topology={} binds={} load={}",
            self.library.name(),
            self.vs,
            self.fs,
            self.target,
            self.write_mask.name(),
            self.vertex.name(),
            self.topology.name(),
            self.binds.listing(),
            self.pass_load.name(),
        )
    }
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
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
        pass_load: PassLoad::Dynamic,
    },
    PipelineSpec {
        label: "aterm-gpu glyph pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_glyph",
        fs: "fs_glyph",
        target: TargetRole::OffscreenSrgb,
        // out = fg*cov + dst*(1-cov) in LINEAR light (sRGB target).
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Glyph,
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_GLYPH_TEXT,
        pass_load: PassLoad::Dynamic,
    },
    PipelineSpec {
        label: "aterm-gpu colour-glyph pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_glyph",
        fs: "fs_glyph_color",
        target: TargetRole::OffscreenSrgb,
        // out = rgb*a + dst*(1-a) in LINEAR light (sRGB target).
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Glyph,
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_ATLAS,
        pass_load: PassLoad::Dynamic,
    },
    // --- the nine demand-built EFFECT pipelines ----------------------------
    PipelineSpec {
        label: "aterm-gpu cursor blend pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_bg",
        fs: "fs_bg",
        target: TargetRole::OffscreenSrgb,
        blend: Some(Blend::ALPHA_BLENDING),
        write_mask: WriteMask::All,
        vertex: VertexLayout::Bg,
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
        pass_load: PassLoad::Dynamic,
    },
    PipelineSpec {
        label: "aterm-gpu glow additive pipeline",
        library: ShaderLibrary::Cell,
        vs: "vs_bg",
        // Glow is RAW (no sRGB decode): the Unorm view keeps One/One byte-exact
        // against the CPU `add_sat`.
        fs: "fs_glow",
        target: TargetRole::OffscreenUnorm,
        blend: Some(Blend::GLOW_OVER),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Bg,
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
        pass_load: PassLoad::Dynamic,
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
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
        pass_load: PassLoad::Dynamic,
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
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
        pass_load: PassLoad::Dynamic,
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
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
        pass_load: PassLoad::Dynamic,
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
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_FLAT,
        pass_load: PassLoad::Dynamic,
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
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_ATLAS,
        pass_load: PassLoad::Dynamic,
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
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_ATLAS,
        pass_load: PassLoad::Dynamic,
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
        topology: Topology::TriangleList,
        binds: BindSpec::CELL_ATLAS,
        pass_load: PassLoad::Dynamic,
    },
    // --- the two lazily-built post passes over the offscreen ---------------
    PipelineSpec {
        label: "aterm-gpu bloom composite pipeline",
        library: ShaderLibrary::Bloom,
        vs: "vs_fs",
        fs: "fs_bloom",
        target: TargetRole::OffscreenUnorm,
        // THE HALO IS INSIDE THE BUDGET: `Screen` spends the headroom the
        // destination has left, so the halo can brighten and never saturate.
        blend: Some(Blend::SCREEN),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::None,
        topology: Topology::TriangleList,
        binds: BindSpec::POST_FS,
        pass_load: PassLoad::Load,
    },
    PipelineSpec {
        label: "aterm-gpu shimmer pipeline",
        library: ShaderLibrary::Shimmer,
        vs: "vs_fs",
        fs: "fs_shimmer",
        target: TargetRole::OffscreenUnorm,
        // REPLACE by omission: the fragment IS the refracted frame.
        blend: None,
        // ALL, alpha included: the refraction displaces the WHOLE sample
        // (fs_shimmer returns the displaced rgba), so the translucent
        // present's alpha coheres with the rgb it publishes. Under Color the
        // alpha byte stayed the UNDISPLACED pixel's — rgb and opacity from
        // two different source pixels inside the haze band (2026-09-01
        // audit). Opaque presents are byte-identical either way (uniform
        // offscreen alpha inside the grid).
        write_mask: WriteMask::All,
        vertex: VertexLayout::None,
        topology: Topology::TriangleList,
        binds: BindSpec::POST_FS,
        pass_load: PassLoad::Load,
    },
    // --- the glow-boost pair: ONE construction site, TWO pipelines ---------
    PipelineSpec {
        label: "aterm-gpu hdr-glow pipeline",
        library: ShaderLibrary::HdrGlow,
        vs: "vs_hdr_glow",
        fs: "fs_hdr_glow",
        target: TargetRole::Edr,
        // EDR stays ADDITIVE: the f16 target's whole point is emission above
        // 1.0, so there is no `1 - dst` headroom to divide up.
        blend: Some(Blend::ADDITIVE),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Bg,
        topology: Topology::TriangleList,
        binds: BindSpec::CROWN,
        pass_load: PassLoad::Dynamic,
    },
    PipelineSpec {
        label: "aterm-gpu sdr-glow pipeline",
        library: ShaderLibrary::HdrGlow,
        vs: "vs_hdr_glow",
        fs: "fs_sdr_glow",
        target: TargetRole::Present,
        // THE CROWN IS INSIDE THE BUDGET — see `BoostComposite::Screen`.
        blend: Some(Blend::SCREEN),
        write_mask: WriteMask::Color,
        vertex: VertexLayout::Bg,
        topology: Topology::TriangleList,
        binds: BindSpec::CROWN,
        pass_load: PassLoad::Dynamic,
    },
    // --- the two swapchain passes ------------------------------------------
    PipelineSpec {
        label: "aterm-gpu blit pipeline",
        library: ShaderLibrary::Blit,
        vs: "vs_blit",
        fs: "fs_blit",
        target: TargetRole::Present,
        blend: Some(Blend::REPLACE),
        // The blit is what PUBLISHES the alpha the ten RGB-only effect
        // pipelines were careful not to disturb.
        write_mask: WriteMask::All,
        vertex: VertexLayout::None,
        topology: Topology::TriangleList,
        binds: BindSpec::POST_FS,
        pass_load: PassLoad::Clear,
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
        topology: Topology::TriangleStrip,
        binds: BindSpec::TRAY,
        pass_load: PassLoad::Load,
    },
];

/// How many rows [`PIPELINES`] has. A `const` so the array length is visible at
/// its definition, exactly like `EFFECT_PIPELINE_COUNT`.
pub(crate) const PIPELINE_COUNT: usize = 18;

/// A row of [`PIPELINES`], by name. The discriminants ARE the indices.
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
    /// Sparkle-word paw, W7 undercurl, glyph contrast-halo.
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
    HdrGlow = 14,
    /// The SDR aurora crown.
    SdrGlow = 15,
    /// The offscreen -> swapchain present blit.
    Blit = 16,
    /// The tray/overlay card blit.
    Tray = 17,
}

/// Every [`Pipeline`], in row order — the single enumeration a sweep walks.
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
];

impl Pipeline {
    /// This pipeline's row.
    ///
    /// [`PIPELINES`] is a `static`, not a `const`, so this hands back a
    /// reference to THE row rather than to a per-call-site copy of it — which
    /// is what lets a test assert that two lookups of the same slot are the
    /// same object, and what keeps eighteen `PipelineSpec`s out of every
    /// caller's rodata.
    pub(crate) fn spec(self) -> &'static PipelineSpec {
        &PIPELINES[self as usize]
    }

    /// The row's stable name — the enum variant, snake-cased. Used by the
    /// golden listing and by test failure messages.
    #[allow(
        dead_code,
        reason = "the table's stable NAMES exist for the golden listing and for \
                  test diagnostics; a non-test build has no reader for them"
    )]
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
        }
    }
}

/// Every DISTINCT entry point `library` must export, in first-use order.
///
/// This is what `crate::metal::shaders::libraries` hands the MSL compile test,
/// so the roster is DERIVED from the table rather than maintained beside it —
/// the exact structural hole that let `vs_fs` be renamed to `vs_fs_bloom` in
/// the MSL while `renderer.rs` went on asking for `vs_fs`, with a
/// self-consistent roster test that could not see it.
pub(crate) fn entry_points(library: ShaderLibrary) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for p in ALL_PIPELINES {
        let spec = p.spec();
        if spec.library != library {
            continue;
        }
        for e in [spec.vs, spec.fs] {
            if !out.contains(&e) {
                out.push(e);
            }
        }
    }
    out
}

/// The whole table as one stable text block, one line per row. The golden
/// listing `renderer::pipeline_table_tests::the_pipeline_table_is_what_it_says`
/// pins, so that changing any blend factor, write mask, entry point, target
/// role, vertex layout or topology is a REVIEWED diff instead of a silent one.
#[allow(
    dead_code,
    reason = "the golden listing and its checked-in twin exist for \
              `tests::the_pipeline_table_is_what_it_says`; a non-test build has \
              no reader for them"
)]
pub(crate) fn listing() -> String {
    let mut s = String::new();
    for p in ALL_PIPELINES {
        s.push_str(&p.spec().listing(p.name()));
        s.push('\n');
    }
    s
}

/// The checked-in golden of [`listing`] — `crates/aterm-gpu/pipeline-table.txt`.
///
/// It lives in its OWN FILE, beside the crate rather than inside this one, on
/// purpose: a blend factor or a write mask cannot then be edited in the same
/// hunk as the assertion that was supposed to notice. Comment lines (`#`) and
/// blank lines are stripped before comparison so the file can explain itself.
#[allow(
    dead_code,
    reason = "the golden listing and its checked-in twin exist for \
              `tests::the_pipeline_table_is_what_it_says`; a non-test build has \
              no reader for them"
)]
const GOLDEN: &str = include_str!("../pipeline-table.txt");

#[cfg(test)]
mod tests {
    use super::*;

    /// THE REVIEW GATE over the table's VALUES.
    ///
    /// Threading the table through both backends made drift BETWEEN them
    /// impossible — there is only one declaration left — but it cannot make a
    /// wrong declaration impossible. This is the guard for that half: every
    /// field of every row, as one text block, pinned against a checked-in
    /// golden. Changing a blend factor, a write mask, an entry point, a target
    /// role, a vertex layout or a topology is now a two-file diff a reviewer
    /// sees, instead of one character inside a 40-line descriptor.
    ///
    /// This is NOT the "hand-copied constant in two places" the table exists to
    /// abolish, and the difference is worth being precise about: a hand-copy is
    /// a SECOND PRODUCER OF BEHAVIOUR that can disagree with the first while
    /// both keep working. The golden produces no behaviour at all — nothing
    /// builds a pipeline from it — so it cannot disagree with the renderer; it
    /// can only refuse to be silent.
    #[test]
    fn the_pipeline_table_is_what_it_says() {
        let want: Vec<&str> = GOLDEN
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        let got = listing();
        let have: Vec<&str> = got.lines().collect();
        assert_eq!(
            have.len(),
            PIPELINE_COUNT,
            "the listing must have one line per row"
        );
        for (i, (w, h)) in want.iter().zip(have.iter()).enumerate() {
            assert_eq!(
                w,
                h,
                "pipeline-table.txt line {} disagrees with the table.\n\
                 If the change was intended, update crates/aterm-gpu/pipeline-table.txt.",
                i + 1
            );
        }
        assert_eq!(
            want.len(),
            have.len(),
            "pipeline-table.txt has {} rows, the table has {}",
            want.len(),
            have.len()
        );
    }

    /// The enum discriminants ARE the indices into [`PIPELINES`], and
    /// [`ALL_PIPELINES`] enumerates every one exactly once. Everything else in
    /// this module indexes through those two facts.
    #[test]
    fn the_row_index_is_the_enum_discriminant() {
        assert_eq!(PIPELINES.len(), PIPELINE_COUNT);
        assert_eq!(ALL_PIPELINES.len(), PIPELINE_COUNT);
        for (i, p) in ALL_PIPELINES.into_iter().enumerate() {
            assert_eq!(p as usize, i, "{} is at the wrong index", p.name());
            assert_eq!(
                std::ptr::from_ref(p.spec()),
                std::ptr::from_ref(&PIPELINES[i])
            );
        }
        let mut names: Vec<&str> = ALL_PIPELINES.into_iter().map(Pipeline::name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "two rows share a name");
    }

    /// The `wgpu` vertex-buffer layouts are DERIVED from the neutral ones, so
    /// this pins the derivation rather than the values: each layout must carry
    /// exactly its spec's stride, attribute count, formats and offsets, and
    /// must step per INSTANCE (every aterm quad is one instance).
    #[test]
    fn the_wgpu_vertex_layouts_are_the_tables_own() {
        for p in ALL_PIPELINES {
            let spec = p.spec();
            let buffers = spec.wgpu_vertex_buffers();
            match spec.vertex.spec() {
                None => assert!(
                    buffers.is_empty(),
                    "{} draws from [[vertex_id]] but declares a vertex buffer",
                    p.name()
                ),
                Some(v) => {
                    assert_eq!(buffers.len(), 1, "{}", p.name());
                    let b = &buffers[0];
                    assert_eq!(b.array_stride, v.stride, "{} stride", p.name());
                    assert_eq!(b.step_mode, wgpu::VertexStepMode::Instance, "{}", p.name());
                    assert_eq!(b.attributes.len(), v.attrs.len(), "{}", p.name());
                    for (a, n) in b.attributes.iter().zip(v.attrs) {
                        assert_eq!(a.shader_location, n.location, "{}", p.name());
                        assert_eq!(a.offset, n.offset, "{}", p.name());
                        assert_eq!(a.format, n.format.wgpu(), "{}", p.name());
                    }
                    // An attribute may not start past the end of its instance.
                    let last = v.attrs.last().expect("a layout has attributes");
                    assert!(last.offset < v.stride, "{} overruns its stride", p.name());
                }
            }
        }
    }

    /// A [`TargetRole::Present`] row needs the destination format and every
    /// other row must NOT — that is what keeps the offscreen's sRGB/Unorm
    /// choice out of the call sites' hands.
    #[test]
    fn target_roles_resolve_from_the_context_not_the_call_site() {
        let targets = TargetFormats {
            offscreen_srgb: wgpu::TextureFormat::Rgba8UnormSrgb,
            offscreen_unorm: wgpu::TextureFormat::Rgba8Unorm,
            present: None,
        };
        let mut present_rows = 0;
        for p in ALL_PIPELINES {
            let role = p.spec().target;
            if role == TargetRole::Present {
                present_rows += 1;
                assert_eq!(
                    targets
                        .with_present(wgpu::TextureFormat::Bgra8Unorm)
                        .resolve(role),
                    wgpu::TextureFormat::Bgra8Unorm,
                    "{}",
                    p.name()
                );
                continue;
            }
            let want = match role {
                TargetRole::OffscreenSrgb => wgpu::TextureFormat::Rgba8UnormSrgb,
                TargetRole::OffscreenUnorm => wgpu::TextureFormat::Rgba8Unorm,
                TargetRole::Edr => wgpu::TextureFormat::Rgba16Float,
                TargetRole::Present => unreachable!("handled above"),
            };
            assert_eq!(targets.resolve(role), want, "{}", p.name());
        }
        assert_eq!(
            present_rows, 3,
            "the swapchain rows are sdr_glow, blit and tray"
        );
    }

    /// Two rows naming the SAME entry point in the SAME library must declare
    /// the SAME bindings for that stage, because the MSL declares each
    /// function's argument table exactly once — a per-row disagreement could
    /// never be satisfied by any `.metal` source and would mean the column
    /// stopped describing the shaders. (The MSL scan in `metal::shaders`
    /// checks the column against the sources; this checks it against ITSELF,
    /// and runs on every platform that compiles the crate.)
    #[test]
    fn rows_sharing_an_entry_point_share_its_bindings() {
        for a in ALL_PIPELINES {
            for b in ALL_PIPELINES {
                let (sa, sb) = (a.spec(), b.spec());
                if sa.library != sb.library {
                    continue;
                }
                if sa.vs == sb.vs {
                    assert_eq!(
                        sa.binds.vertex_uniform,
                        sb.binds.vertex_uniform,
                        "{} and {} share vs `{}` but disagree on its vertex uniform slot",
                        a.name(),
                        b.name(),
                        sa.vs
                    );
                }
                if sa.fs == sb.fs {
                    assert_eq!(
                        (
                            sa.binds.fragment_textures,
                            sa.binds.fragment_samplers,
                            sa.binds.fragment_buffers
                        ),
                        (
                            sb.binds.fragment_textures,
                            sb.binds.fragment_samplers,
                            sb.binds.fragment_buffers
                        ),
                        "{} and {} share fs `{}` but disagree on its fragment bindings",
                        a.name(),
                        b.name(),
                        sa.fs
                    );
                }
            }
        }
        // Every fragment texture in this shader set is sampled (not read raw
        // via a slot with no sampler), so the two slot lists pair 1:1 — a row
        // that breaks the pairing is a new shader SHAPE and must be a reviewed
        // change here, not an accident.
        for p in ALL_PIPELINES {
            let b = p.spec().binds;
            assert_eq!(
                b.fragment_textures.len(),
                b.fragment_samplers.len(),
                "{}: every fragment texture slot pairs with a sampler slot",
                p.name()
            );
        }
    }

    /// The load-convention partition is structural, not editorial: a row pins
    /// its pass's load op IFF it is a `[[vertex_id]]` whole-pass operator
    /// (`VertexLayout::None` — the blit, the bloom composite, the shimmer,
    /// the tray), because those are the rows that OPEN a dedicated pass in
    /// the production graph. Every instanced row is a batch citizen of a
    /// shared pass (the coalesced frame plan, the bloom extract, the blit
    /// pass the crowns ride) and must stay [`PassLoad::Dynamic`] — pinning
    /// one would declare a convention no pass site owns, which is exactly the
    /// drift this column exists to make reviewable.
    #[test]
    fn only_whole_pass_rows_pin_a_load_op() {
        for p in ALL_PIPELINES {
            let spec = p.spec();
            let pinned = spec.pass_load != PassLoad::Dynamic;
            assert_eq!(
                pinned,
                spec.vertex == VertexLayout::None,
                "{}: load convention {:?} vs vertex layout {:?} — pinned iff \
                 the row is a whole-pass [[vertex_id]] operator",
                p.name(),
                spec.pass_load,
                spec.vertex
            );
        }
        // And the pinned values themselves are the audited site facts: the
        // blit CLEARS (it repaints the whole drawable), the three
        // compose-over passes LOAD (they refine a frame that already exists).
        assert_eq!(Pipeline::Blit.spec().pass_load, PassLoad::Clear);
        for p in [Pipeline::Bloom, Pipeline::Shimmer, Pipeline::Tray] {
            assert_eq!(p.spec().pass_load, PassLoad::Load, "{}", p.name());
        }
    }

    /// The derived entry-point roster must cover every row and nothing else,
    /// and must dedupe — `vs_bg` is shared by three cell pipelines and `vs_fs`
    /// is the name BOTH the bloom and the shimmer libraries export.
    #[test]
    fn the_entry_point_roster_covers_every_row() {
        let mut seen = 0;
        for lib in ShaderLibrary::ALL {
            let roster = entry_points(lib);
            let mut deduped = roster.clone();
            deduped.sort_unstable();
            let before = deduped.len();
            deduped.dedup();
            assert_eq!(
                deduped.len(),
                before,
                "{} roster repeats a name",
                lib.name()
            );
            for p in ALL_PIPELINES {
                let spec = p.spec();
                if spec.library != lib {
                    continue;
                }
                seen += 1;
                assert!(
                    roster.contains(&spec.vs),
                    "{} missing {}",
                    lib.name(),
                    spec.vs
                );
                assert!(
                    roster.contains(&spec.fs),
                    "{} missing {}",
                    lib.name(),
                    spec.fs
                );
            }
            for e in &roster {
                assert!(
                    ALL_PIPELINES.into_iter().any(
                        |p| p.spec().library == lib && (p.spec().vs == *e || p.spec().fs == *e)
                    ),
                    "{} roster lists `{e}`, which no row asks for",
                    lib.name()
                );
            }
        }
        assert_eq!(
            seen, PIPELINE_COUNT,
            "every row belongs to exactly one library"
        );
        // 26 entry-point slots over 24 DISTINCT names: `vs_fs` is exported by
        // two different libraries, so it is one name in each roster and two in
        // the union count.
        let total: usize = ShaderLibrary::ALL
            .into_iter()
            .map(|l| entry_points(l).len())
            .sum();
        assert_eq!(total, 26);
    }
}
