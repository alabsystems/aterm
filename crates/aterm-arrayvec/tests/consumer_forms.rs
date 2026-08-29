// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Every invocation form the six third-party consumers of `arrayvec` write,
//! reproduced verbatim against this crate.
//!
//! # Why this file is the deliverable and not a formality
//!
//! Five of the six consumers cannot be compiled on the macOS box this crate was
//! written on. `naga` resolves here with `default,msl-out,wgsl-in`, so its
//! entire SPIR-V writer — where a third of the `ArrayVec` uses live — is
//! `cfg`'d off; `wgpu-hal` resolves with `metal,portable-atomic`, and the Metal
//! backend contains no `ArrayVec` at all, so `dx12`, `gles` and `vulkan` are
//! dark; `tiny-skia` only enters the graph on Linux, under `sctk-adwaita`; and
//! `vte` only through dev edges. A macOS `cargo check -p aterm-gui` therefore
//! exercises `Hash`, `Extend`, by-value `IntoIterator` and `into_inner` — and
//! misses `drain`, by-`&`/`&mut` `IntoIterator`, `new_const` and `clone_from`
//! entirely.
//!
//! So the shapes are brought here instead. Each block below is a transcription
//! of real consumer source, commented with the crate, file and line it came
//! from, with the consumers' own types replaced by local stand-ins of the same
//! shape (`Arc<TextureView>` becomes `Arc<StandIn>`, `PCWSTR` becomes a
//! `*const u16`, and so on) because it is the *form* — the turbofish, the const
//! block, the derive list, the loop over a reference — that decides whether the
//! surface accepts it. Compiling this file on any cell is the proof.
//!
//! It is a compile-time proof first; the `#[test]` functions additionally run
//! the forms so that a shape which compiles but computes the wrong thing is
//! caught too. Behaviour against the real upstream crate is checked separately
//! and more aggressively by
//! `crates/aterm-alloc/tests/arrayvec_differential.rs`.

// The consumers' shapes are reproduced whole, including fields and variants
// this file never reads. `dead_code` fires on exactly those and says nothing
// about whether the surface compiles, which is the question being asked.
#![allow(dead_code)]

// ── naga 29.0.3 ─────────────────────────────────────────────────────────────

mod naga {
    // naga-29.0.3/src/proc/constant_evaluator.rs:14
    use arrayvec::ArrayVec;

    /// Stand-in for `naga::VectorSize`, whose `MAX` appears inside const
    /// blocks as an `ArrayVec` capacity argument.
    pub struct VectorSize;
    impl VectorSize {
        pub const MAX: usize = 4;
    }

    /// Stand-ins for `naga::Handle<Expression>` and `spirv::{BuiltIn, Decoration}`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Handle(pub u32);
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Decoration(pub u32);
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct BuiltIn(pub u32);
    pub type Word = u32;

    // naga-29.0.3/src/back/spv/helpers.rs:199 — ArrayVec as an enum-variant payload
    #[derive(Debug)]
    pub enum Decorated {
        BuiltIn(BuiltIn, ArrayVec<Decoration, 2>),
        Location(Word),
    }

    // naga-29.0.3/src/proc/constant_evaluator.rs:285 — variant payload whose
    // capacity is a const block
    #[derive(Debug)]
    pub enum LiteralVector {
        F64(ArrayVec<f64, { VectorSize::MAX }>),
        U32(ArrayVec<u32, { VectorSize::MAX }>),
    }

    impl LiteralVector {
        // naga-29.0.3/src/proc/constant_evaluator.rs — `LiteralVector::len`
        pub fn len(&self) -> usize {
            match self {
                Self::F64(v) => v.len(),
                Self::U32(v) => v.len(),
            }
        }
    }

    // naga-29.0.3/src/proc/constant_evaluator.rs:105 and :113 — a turbofish over
    // a const-generic PARAMETER, then `.into_inner().unwrap()`.
    pub fn compose_exact<const N: usize>(src: &[f64]) -> [f64; N] {
        let mut arr = ArrayVec::<_, N>::new();
        for v in src {
            arr.push(*v);
        }
        // naga-29.0.3/src/proc/constant_evaluator.rs:113
        arr.into_inner().unwrap()
    }

    // naga-29.0.3/src/proc/constant_evaluator.rs:122 — ArrayVec of ArrayVec,
    // inner capacity a const block, outer a const-generic parameter.
    pub fn compose_matrix<const N: usize>(cols: &[&[f64]]) -> usize {
        let mut columns = ArrayVec::<ArrayVec<_, { VectorSize::MAX }>, N>::new();
        for col in cols {
            let mut inner = ArrayVec::<f64, { VectorSize::MAX }>::new();
            for v in *col {
                inner.push(*v);
            }
            columns.push(inner);
        }
        columns.len()
    }

    // naga-29.0.3/src/proc/constant_evaluator.rs:3195 — ArrayVec in RETURN
    // position, and :3201 — `Extend` from a borrowed-and-cloned iterator.
    pub fn flatten_matrix(columns: &[Handle]) -> ArrayVec<Handle, 16> {
        let mut flattened = ArrayVec::new();
        flattened.extend(columns.iter().cloned());
        flattened
    }

    // naga-29.0.3/src/back/spv/writer.rs:872 — annotated local + `ArrayVec::new()`
    // naga-29.0.3/src/back/spv/writer.rs:1115 — `.collect::<ArrayVec<_, 4>>()`
    pub fn column_ids(words: &[Word]) -> (ArrayVec<Word, 4>, ArrayVec<Word, 4>) {
        let mut column_ids: ArrayVec<Word, 4> = ArrayVec::new();
        for w in words {
            column_ids.push(*w);
        }
        let collected = words.iter().copied().collect::<ArrayVec<_, 4>>();
        (column_ids, collected)
    }

    // naga-29.0.3/src/proc/constant_evaluator.rs:175 —
    // `new_components.into_iter().collect()`
    pub fn recollect(new_components: ArrayVec<Handle, 4>) -> ArrayVec<Handle, 4> {
        new_components.into_iter().collect()
    }

    // naga-29.0.3/src/proc/constant_evaluator.rs:347 — `for l in &$components`
    // naga-29.0.3/src/back/spv/block.rs:652   — `for index in &column_indices`
    pub fn sum_by_ref(components: &ArrayVec<f64, 4>, column_indices: &ArrayVec<Word, 4>) -> f64 {
        let mut total = 0.0;
        for l in components {
            total += *l;
        }
        for index in column_indices {
            total += f64::from(*index);
        }
        total
    }

    // naga-29.0.3/src/proc/constant_evaluator.rs:378 — `for e in $v` (by value)
    // naga-29.0.3/src/back/spv/writer.rs:2898    — `for other in others`
    pub fn sum_by_value(v: ArrayVec<f64, 4>, others: ArrayVec<Word, 4>) -> f64 {
        let mut total = 0.0;
        for e in v {
            total += e;
        }
        for other in others {
            total += f64::from(other);
        }
        total
    }
}

#[test]
fn naga_forms() {
    use naga::*;
    let _ = Decorated::BuiltIn(BuiltIn(1), {
        let mut d = arrayvec::ArrayVec::new();
        d.push(Decoration(7));
        d
    });

    let mut lit = arrayvec::ArrayVec::<f64, { VectorSize::MAX }>::new();
    lit.push(1.0);
    lit.push(2.0);
    assert_eq!(LiteralVector::F64(lit).len(), 2);

    assert_eq!(compose_exact::<3>(&[1.0, 2.0, 3.0]), [1.0, 2.0, 3.0]);
    assert_eq!(compose_matrix::<2>(&[&[1.0, 2.0], &[3.0, 4.0]]), 2);
    assert_eq!(flatten_matrix(&[Handle(1), Handle(2)]).len(), 2);

    let (a, b) = column_ids(&[1, 2, 3]);
    assert_eq!(a.as_slice(), b.as_slice());

    let mut src: arrayvec::ArrayVec<Handle, 4> = arrayvec::ArrayVec::new();
    src.push(Handle(9));
    assert_eq!(recollect(src).len(), 1);

    let mut comps: arrayvec::ArrayVec<f64, 4> = arrayvec::ArrayVec::new();
    comps.push(1.5);
    let mut idxs: arrayvec::ArrayVec<u32, 4> = arrayvec::ArrayVec::new();
    idxs.push(2);
    assert_eq!(sum_by_ref(&comps, &idxs), 3.5);
    assert_eq!(sum_by_value(comps, idxs), 3.5);
}

// ── wgpu 29.0.3 ─────────────────────────────────────────────────────────────

mod wgpu {
    use arrayvec::ArrayVec;

    /// Stand-ins for `wgc::MAX_BIND_GROUPS` / `wgc::MAX_VERTEX_BUFFERS`.
    pub const MAX_BIND_GROUPS: usize = 4;
    pub const MAX_VERTEX_BUFFERS: usize = 8;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct BindGroupLayoutId(pub u32);
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct VertexBufferLayout(pub u32);

    // wgpu-29.0.3/src/backend/wgpu_core.rs:1296-1308 — the `// Guards following
    // ArrayVec` collect. The capacity is the LIMIT; a truncating `collect` here
    // would silently build a pipeline layout missing its last bind group.
    pub fn bind_group_layouts(ids: &[u32]) -> ArrayVec<BindGroupLayoutId, { MAX_BIND_GROUPS }> {
        ids.iter()
            .map(|id| BindGroupLayoutId(*id))
            .collect::<ArrayVec<_, { MAX_BIND_GROUPS }>>()
    }

    // wgpu-29.0.3/src/backend/wgpu_core.rs:1340-1345 — annotated binding whose
    // capacity is a const block, initialized by `.collect()`.
    pub fn vertex_buffers(desc: &[u32]) -> ArrayVec<VertexBufferLayout, { MAX_VERTEX_BUFFERS }> {
        let vertex_buffers: ArrayVec<_, { MAX_VERTEX_BUFFERS }> =
            desc.iter().map(|v| VertexBufferLayout(*v)).collect();
        vertex_buffers
    }
}

#[test]
fn wgpu_forms() {
    assert_eq!(wgpu::bind_group_layouts(&[1, 2, 3, 4]).len(), 4);
    assert_eq!(wgpu::vertex_buffers(&[1, 2]).len(), 2);
}

// ── wgpu-core 29.0.3 ────────────────────────────────────────────────────────

mod wgpu_core {
    use arrayvec::ArrayVec;
    use std::sync::Arc;

    pub const MAX_COLOR_ATTACHMENTS: usize = 8;
    pub const MAX_MIP_LEVELS: u32 = 16;
    pub const MAX_BIND_GROUPS: usize = 4;
    pub const MAX_TOTAL_ATTACHMENTS: usize = MAX_COLOR_ATTACHMENTS * 2 + 1;

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct StandIn(pub u32);
    pub type TextureView = StandIn;
    pub type EntryMap = StandIn;
    pub type ShaderModule = StandIn;
    pub type TextureBarrier = StandIn;

    // wgpu-core-29.0.3/src/resource.rs:1922 — FULLY-QUALIFIED path in field
    // position, holding `Arc`s (so `Drop` matters).
    #[derive(Debug)]
    pub struct ExternalTexture {
        pub(crate) planes: arrayvec::ArrayVec<Arc<TextureView>, 3>,
    }

    // wgpu-core-29.0.3/src/device/mod.rs:50-55 — `#[derive(Hash)]` reaches
    // ArrayVec here, on all four cells. Without `impl Hash for ArrayVec` this
    // single derive is a hard compile error everywhere.
    #[derive(Clone, Debug, Hash, PartialEq)]
    pub struct AttachmentData<T> {
        pub colors: ArrayVec<Option<T>, { MAX_COLOR_ATTACHMENTS }>,
        pub resolves: ArrayVec<T, { MAX_COLOR_ATTACHMENTS }>,
        pub depth_stencil: Option<T>,
    }
    // wgpu-core-29.0.3/src/device/mod.rs — `impl<T: PartialEq> Eq for AttachmentData<T>`
    impl<T: PartialEq> Eq for AttachmentData<T> {}

    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct TextureLayerInitTracker(pub u32);

    // wgpu-core-29.0.3/src/init_tracker/texture.rs:50 — capacity is a const
    // block containing a CAST.
    #[derive(Debug)]
    pub struct TextureInitTracker {
        pub mips: ArrayVec<TextureLayerInitTracker, { MAX_MIP_LEVELS as usize }>,
    }

    // wgpu-core-29.0.3/src/validation.rs:910 — ArrayVec inside a Box, inside an
    // enum variant.
    #[derive(Debug)]
    pub enum PipelineLayoutSource {
        Provided(u32),
        Derived(Box<ArrayVec<EntryMap, { MAX_BIND_GROUPS }>>),
    }

    // wgpu-core-29.0.3/src/command/render.rs:935 — a generic `type` alias
    pub type AttachmentDataVec<T> = ArrayVec<T, MAX_TOTAL_ATTACHMENTS>;

    // wgpu-core-29.0.3/src/track/texture.rs — `#[derive(Default)]` + iteration
    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct ComplexTextureState {
        pub mips: ArrayVec<u32, { MAX_MIP_LEVELS as usize }>,
    }

    // wgpu-core-29.0.3/src/device/resource.rs:3787 and :3793 — `pop()`, then
    // `into_iter()` THROUGH A BOX.
    //
    // The `Box` is the form under test, not an accident: wgpu-core's
    // `PipelineLayoutSource::Derived` stores the ArrayVec boxed
    // (validation.rs:910) and moves out of it with `.into_iter()`, which only
    // works because `Box` can be moved out of. `clippy::boxed_local` is right
    // in general and wrong here — taking the `ArrayVec` by value would stop
    // reproducing the consumer's shape.
    #[allow(clippy::boxed_local)]
    pub fn finish_derived(
        mut derived: Box<ArrayVec<EntryMap, { MAX_BIND_GROUPS }>>,
    ) -> Vec<EntryMap> {
        derived.pop();
        derived.into_iter().collect()
    }

    // wgpu-core-29.0.3/src/device/resource.rs:3989 — annotated local, const-block capacity
    pub fn color_target_states(n: usize) -> ArrayVec<u32, { MAX_COLOR_ATTACHMENTS }> {
        let cts: ArrayVec<_, { MAX_COLOR_ATTACHMENTS }> = (0..n as u32).collect();
        cts
    }

    // wgpu-core-29.0.3/src/device/resource.rs:4738 — `Extend` from an `Option`
    pub fn shader_modules(
        vertex: ShaderModule,
        fragment: Option<ShaderModule>,
    ) -> ArrayVec<ShaderModule, 2> {
        let mut shader_modules: ArrayVec<ShaderModule, 2> = ArrayVec::new();
        shader_modules.push(vertex);
        shader_modules.extend(fragment);
        shader_modules
    }

    // wgpu-core-29.0.3/src/command/transfer.rs:1453 and :1462 — `.collect()`
    // then `Extend` from a mapped `Option`. A truncating `extend` here drops
    // the destination barrier of a texture-to-texture copy.
    pub fn transfer_barriers(
        src_pending: &[u32],
        dst_pending: Option<u32>,
    ) -> ArrayVec<TextureBarrier, 2> {
        let mut barriers: ArrayVec<TextureBarrier, 2> =
            src_pending.iter().map(|p| StandIn(*p)).collect();
        barriers.extend(dst_pending.map(StandIn));
        barriers
    }

    // wgpu-core-29.0.3/src/command/render.rs:1532 — by-value iteration over a
    // struct field that owns `Arc`s. Every early return out of a loop like this
    // is where a leaking `IntoIter` would strand a texture for the process
    // lifetime.
    pub struct RenderPassInfo {
        pub render_attachments: AttachmentDataVec<Arc<TextureView>>,
    }
    impl RenderPassInfo {
        pub fn finish(self) -> usize {
            let mut seen = 0;
            for ra in self.render_attachments {
                seen += Arc::strong_count(&ra);
            }
            seen
        }
    }

    // wgpu-core-29.0.3/src/track/texture.rs:108 — `get_unchecked_mut` over a
    // RANGE, through `DerefMut`, then iterated by `&mut`.
    pub fn bump_mips(complex: &mut ComplexTextureState, mips: std::ops::Range<usize>) {
        for mip in unsafe { complex.mips.get_unchecked_mut(mips) } {
            *mip += 1;
        }
    }

    // wgpu-core-29.0.3/src/track/texture.rs:1383 — the SHARED half of the pair
    // above, `get_unchecked` with a `usize` index, through `Deref`. It is a
    // different `SliceIndex` impl and a different inherent method from the one
    // on the previous line, and it is the only place either shape appears
    // outside `track/texture.rs`: re-derive with
    // `grep -rn 'get_unchecked' <registry>/{naga,wgpu,wgpu-core,wgpu-hal}-29.0.3
    //  <registry>/{tiny-skia-0.11.4,vte-0.15.0} --include='*.rs'`.
    pub fn read_mip(complex: &ComplexTextureState, mip_id: usize) -> u32 {
        // SAFETY: the caller has already bounds-checked `mip_id`, exactly as
        // wgpu-core does one line above its own call
        // (`strict_assert!((mip_id as usize) < current_complex.mips.len());`).
        *unsafe { complex.mips.get_unchecked(mip_id) }
    }

    // wgpu-core-29.0.3/src/device/resource.rs:3991 — `is_empty()`
    // wgpu-core-29.0.3/src/command/render.rs:1852 — `as_slice()`
    pub fn describe(cts: &ArrayVec<u32, { MAX_COLOR_ATTACHMENTS }>) -> (bool, &[u32]) {
        (cts.is_empty(), cts.as_slice())
    }
}

#[test]
fn wgpu_core_forms() {
    use std::sync::Arc;
    use wgpu_core::*;

    let mut planes = arrayvec::ArrayVec::new();
    planes.push(Arc::new(StandIn(1)));
    let ext = ExternalTexture { planes };
    assert_eq!(ext.planes.len(), 1);

    // The Hash derive, exercised as a HashMap key — the shape wgpu-core uses.
    let mut colors = arrayvec::ArrayVec::new();
    colors.push(Some(StandIn(1)));
    let key = AttachmentData {
        colors,
        resolves: arrayvec::ArrayVec::new(),
        depth_stencil: None,
    };
    let mut map = std::collections::HashMap::new();
    map.insert(key.clone(), 1u32);
    assert_eq!(map.get(&key), Some(&1));

    let tracker = TextureInitTracker {
        mips: (0..3).map(TextureLayerInitTracker).collect(),
    };
    assert_eq!(tracker.mips.len(), 3);

    let derived = Box::new((0..3).map(StandIn).collect::<arrayvec::ArrayVec<_, 4>>());
    let layout = PipelineLayoutSource::Derived(derived);
    let PipelineLayoutSource::Derived(derived) = layout else {
        unreachable!()
    };
    assert_eq!(finish_derived(derived).len(), 2);

    assert_eq!(color_target_states(3).len(), 3);
    assert_eq!(shader_modules(StandIn(1), Some(StandIn(2))).len(), 2);
    assert_eq!(shader_modules(StandIn(1), None).len(), 1);
    assert_eq!(transfer_barriers(&[1], Some(2)).len(), 2);

    let arc = Arc::new(StandIn(4));
    let info = RenderPassInfo {
        render_attachments: {
            let mut v: AttachmentDataVec<Arc<StandIn>> = arrayvec::ArrayVec::new();
            v.push(Arc::clone(&arc));
            v
        },
    };
    assert_eq!(info.finish(), 2);
    // …and the by-value loop dropped its `Arc`, so the outer one is alone again.
    assert_eq!(Arc::strong_count(&arc), 1);

    let mut complex = ComplexTextureState::default();
    complex.mips.extend([1, 2, 3]);
    bump_mips(&mut complex, 0..2);
    assert_eq!(complex.mips.as_slice(), &[2, 3, 3]);
    assert_eq!(read_mip(&complex, 1), 3);

    let cts = color_target_states(2);
    assert_eq!(describe(&cts), (false, &[0u32, 1][..]));
}

// ── wgpu-hal 29.0.3 ─────────────────────────────────────────────────────────

mod wgpu_hal {
    use arrayvec::ArrayVec;

    pub const MAX_COLOR_ATTACHMENTS: usize = 8;
    pub const MAX_IMMEDIATES_COMMANDS: usize = 4;

    /// Stand-in for windows-rs `PCWSTR`.
    pub type Pcwstr = *const u16;

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct ProgramStage(pub u32);
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct ImmediateDesc(pub u32);
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct ViewInstanceLocation(pub u32);
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct TextureView(pub u32);
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct VertexAttribute(pub u32);
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct ColorTargetState(pub u32);

    // wgpu-hal-29.0.3/src/dx12/shader_compilation.rs:382 — `new_const()`, the
    // only site in the graph, and :388 — `Extend` from an ARRAY LITERAL. A
    // truncating extend here silently drops `-no-warnings`, strictness and
    // `-HV 2018` from every HLSL compile.
    pub fn compile_args(source_name: Option<Pcwstr>, extra: [Pcwstr; 3]) -> ArrayVec<Pcwstr, 13> {
        let mut compile_args = arrayvec::ArrayVec::<Pcwstr, 13>::new_const();
        if let Some(name) = source_name {
            compile_args.push(name);
        }
        compile_args.extend(extra);
        compile_args
    }

    // wgpu-hal-29.0.3/src/dx12/device.rs:1974 — turbofish with a literal capacity
    // wgpu-hal-29.0.3/src/dx12/device.rs:1990 — `as_slice()`
    pub fn view_instancing(n: u32) -> Vec<ViewInstanceLocation> {
        let mut view_instancing = ArrayVec::<ViewInstanceLocation, 32>::new();
        for i in 0..n {
            view_instancing.push(ViewInstanceLocation(i));
        }
        view_instancing.as_slice().to_vec()
    }

    // wgpu-hal-29.0.3/src/gles/mod.rs:714-716 — `#[derive(Hash)]` on a
    // HashMap key whose field is an ArrayVec.
    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub struct ProgramCacheKey {
        pub stages: ArrayVec<ProgramStage, 3>,
    }

    // wgpu-hal-29.0.3/src/gles/mod.rs:676
    #[derive(Clone, Debug, Default)]
    pub struct PipelineInner {
        pub immediates_descs: ArrayVec<ImmediateDesc, MAX_IMMEDIATES_COMMANDS>,
    }

    // wgpu-hal-29.0.3/src/gles/mod.rs:824 — `type` alias, capacity is a const
    // block containing ARITHMETIC.
    pub type InvalidatedAttachments = ArrayVec<u32, { MAX_COLOR_ATTACHMENTS + 2 }>;

    // wgpu-hal-29.0.3/src/vulkan/mod.rs:463 — `Default` AND `Hash` in one derive
    #[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
    pub struct RenderPassKey {
        pub colors: ArrayVec<TextureView, { MAX_COLOR_ATTACHMENTS }>,
        pub resolves: ArrayVec<TextureView, { MAX_COLOR_ATTACHMENTS }>,
    }

    // wgpu-hal-29.0.3/src/vulkan/mod.rs:913
    #[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
    pub struct FramebufferKey {
        pub attachments: ArrayVec<TextureView, { MAX_COLOR_ATTACHMENTS }>,
    }

    // wgpu-hal-29.0.3/src/vulkan/command.rs:788, :795, :796 — `ArrayVec::default()`
    pub fn empty_render_pass_key() -> RenderPassKey {
        RenderPassKey {
            colors: ArrayVec::default(),
            resolves: ArrayVec::default(),
        }
    }

    // wgpu-hal-29.0.3/src/gles/command.rs:51-66 — `Default::default()` in a
    // struct literal, and the five-ArrayVec `State`.
    pub struct State {
        pub resolve_attachments: ArrayVec<(u32, TextureView), { MAX_COLOR_ATTACHMENTS }>,
        pub invalidate_attachments: InvalidatedAttachments,
        pub color_targets: ArrayVec<ColorTargetState, { MAX_COLOR_ATTACHMENTS }>,
        pub vertex_attributes: ArrayVec<VertexAttribute, 16>,
        pub immediates_descs: ArrayVec<ImmediateDesc, MAX_IMMEDIATES_COMMANDS>,
    }

    impl State {
        pub fn new() -> Self {
            Self {
                resolve_attachments: Default::default(),
                invalidate_attachments: Default::default(),
                color_targets: Default::default(),
                vertex_attributes: Default::default(),
                immediates_descs: Default::default(),
            }
        }

        // wgpu-hal-29.0.3/src/gles/command.rs:511, :512, :709, :718, :724, :1002
        pub fn begin_pass(&mut self) {
            self.resolve_attachments.clear();
            self.invalidate_attachments.clear();
            self.color_targets.clear();
            self.vertex_attributes.clear();
        }

        // wgpu-hal-29.0.3/src/gles/command.rs:238 — `clone_from`
        pub fn bind_pipeline(&mut self, inner: &PipelineInner) {
            self.immediates_descs.clone_from(&inner.immediates_descs);
        }

        // wgpu-hal-29.0.3/src/gles/command.rs:698 — `drain(..)`, the ONLY place
        // `resolve_attachments` is emptied at end-of-pass. A `drain` that
        // yielded without removing would re-emit the previous pass's MSAA
        // resolves against stale views on every later pass.
        pub fn end_pass(&mut self) -> Vec<(u32, TextureView)> {
            let mut done = Vec::new();
            for (attachment, dst) in self.resolve_attachments.drain(..) {
                done.push((attachment, dst));
            }
            done
        }

        // wgpu-hal-29.0.3/src/gles/command.rs:719 and :887 — `for x in &field`
        pub fn vertex_attribute_sum(&self) -> u32 {
            let mut total = 0;
            for vat in &self.vertex_attributes {
                total += vat.0;
            }
            total
        }

        // wgpu-hal-29.0.3/src/gles/command.rs:982 — slice `PartialEq` through
        // `Index<RangeFull>`
        pub fn color_targets_differ(&self, pipeline: &Self) -> bool {
            self.color_targets[..] != pipeline.color_targets[..]
        }
    }

    // wgpu-hal-29.0.3/src/gles/device.rs:329 — `for &(a, b) in &av` (a DESTRUCTURING
    // by-reference pattern, which is why the by-ref `IntoIterator` impl has to exist)
    pub fn shader_stages(shaders: &ArrayVec<(u32, ProgramStage), 3>) -> u32 {
        let mut total = 0;
        for &(naga_stage, stage) in shaders {
            total += naga_stage + stage.0;
        }
        total
    }

    // wgpu-hal-29.0.3/src/gles/device.rs:446 — by-value loop over owned handles
    pub fn delete_shaders(shaders_to_delete: ArrayVec<u32, 3>) -> u32 {
        let mut deleted = 0;
        for shader in shaders_to_delete {
            deleted += shader;
        }
        deleted
    }

    // wgpu-hal-29.0.3/src/gles/device.rs:401, :409-412 — `last_mut()` through
    // `DerefMut`, inside a BLOCK EXPRESSION whose value is the `&mut` it
    // returns, and that borrow then outlives the block and is stored in a
    // struct. It is the graph's only `last_mut` (the `deref_sites` module below
    // covers the shared `last()`), and the position is the reason it is
    // reproduced whole rather than as a bare method call: `push` then
    // `last_mut()` in one block means the borrow checker has to see the mutable
    // borrow of `immediates_items` begin AFTER the `push` ends, which it can do
    // only because `last_mut` goes through `DerefMut` on the ArrayVec itself
    // rather than through a method that also holds something else.
    pub struct CompilationContext<'a> {
        pub immediates_items: &'a mut Vec<u32>,
    }
    pub fn compile_stages(stages: usize) -> usize {
        let mut immediates_items = ArrayVec::<Vec<u32>, 3>::new();
        let mut total = 0;
        for stage in 0..stages {
            let pc_item = {
                immediates_items.push(Vec::new());
                immediates_items.last_mut().unwrap()
            };
            let context = CompilationContext {
                immediates_items: pc_item,
            };
            context.immediates_items.push(stage as u32);
            total += 1;
        }
        assert_eq!(immediates_items.len(), total);
        immediates_items.iter().map(Vec::len).sum()
    }

    // wgpu-hal-29.0.3/src/gles/device.rs:489 — `.into_iter().enumerate()`
    pub fn immediates(immediates_items: ArrayVec<u32, 4>) -> Vec<(usize, u32)> {
        let mut out = Vec::new();
        for (stage_idx, stage_items) in immediates_items.into_iter().enumerate() {
            out.push((stage_idx, stage_items));
        }
        out
    }

    // wgpu-hal-29.0.3/src/vulkan/instance.rs:803 — a DEFERRED-INIT binding
    // (declared without a value, assigned in a branch).
    pub fn validation_features(enable: bool) -> usize {
        let mut validation_feature_list: ArrayVec<u32, 3>;
        if enable {
            validation_feature_list = ArrayVec::new();
            validation_feature_list.push(1);
        } else {
            validation_feature_list = ArrayVec::new();
        }
        validation_feature_list.len()
    }

    // wgpu-hal-29.0.3/src/vulkan/device.rs:300 — `.collect::<ArrayVec<_, 8>>()`
    // wgpu-hal-29.0.3/src/vulkan/device.rs:99  — `.iter()`
    pub fn queue_families(ids: &[u32]) -> u32 {
        let families = ids.iter().copied().collect::<ArrayVec<_, 8>>();
        families.iter().sum()
    }
}

#[test]
fn wgpu_hal_forms() {
    use wgpu_hal::*;

    let a: u16 = 1;
    let b: u16 = 2;
    let args = compile_args(Some(&a), [&b, &b, &b]);
    assert_eq!(args.len(), 4);

    assert_eq!(view_instancing(3).len(), 3);

    let key = ProgramCacheKey {
        stages: (0..3).map(ProgramStage).collect(),
    };
    let mut cache = std::collections::HashMap::new();
    cache.insert(key.clone(), 1u32);
    assert_eq!(cache.get(&key), Some(&1));

    // RenderPassKey / FramebufferKey: Default + Hash + Eq, as HashMap keys.
    assert_eq!(empty_render_pass_key(), RenderPassKey::default());
    let mut fbs = std::collections::HashMap::new();
    fbs.insert(FramebufferKey::default(), 0u32);
    assert!(fbs.contains_key(&FramebufferKey::default()));

    let mut state = State::new();
    state.begin_pass();
    state.resolve_attachments.push((0, TextureView(7)));
    state.resolve_attachments.push((1, TextureView(8)));
    state.vertex_attributes.push(VertexAttribute(3));
    state.vertex_attributes.push(VertexAttribute(4));
    assert_eq!(state.vertex_attribute_sum(), 7);

    let mut inner = PipelineInner::default();
    inner.immediates_descs.push(ImmediateDesc(11));
    state.bind_pipeline(&inner);
    assert_eq!(state.immediates_descs.as_slice(), &[ImmediateDesc(11)]);

    let drained = state.end_pass();
    assert_eq!(drained, vec![(0, TextureView(7)), (1, TextureView(8))]);
    assert!(
        state.resolve_attachments.is_empty(),
        "drain(..) must actually empty the ArrayVec"
    );

    let other = State::new();
    assert!(!state.color_targets_differ(&other));

    let shaders: arrayvec::ArrayVec<(u32, ProgramStage), 3> =
        [(1, ProgramStage(2)), (3, ProgramStage(4))]
            .into_iter()
            .collect();
    assert_eq!(shader_stages(&shaders), 10);

    assert_eq!(delete_shaders([1, 2, 3].into_iter().collect()), 6);
    assert_eq!(compile_stages(3), 3);
    assert_eq!(
        immediates([5, 6].into_iter().collect()),
        vec![(0, 5), (1, 6)]
    );
    assert_eq!(validation_features(true), 1);
    assert_eq!(validation_features(false), 0);
    assert_eq!(queue_families(&[1, 2, 3]), 6);
}

// ── tiny-skia 0.11.4 (Linux only, via sctk-adwaita ← winit) ─────────────────

mod tiny_skia {
    use arrayvec::ArrayVec;

    pub const MAX_VERBS: usize = 4;
    pub const MAX_STAGES: usize = 8;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct PathEdge(pub u32);

    // tiny-skia-0.11.4/src/edge_clipper.rs:37 — a public `type` alias
    pub type ClippedEdges = ArrayVec<PathEdge, MAX_VERBS>;

    // tiny-skia-0.11.4/src/edge_clipper.rs:68, :108, :233 — `is_empty()`
    pub fn any(edges: &ClippedEdges) -> bool {
        !edges.is_empty()
    }

    // tiny-skia-0.11.4/src/edge_builder.rs:104 — by-value loop
    pub fn push_edges(edges: ClippedEdges) -> u32 {
        let mut total = 0;
        for edge in edges {
            total += edge.0;
        }
        total
    }

    // tiny-skia-0.11.4/src/pipeline/mod.rs:394 — `.collect()` into an annotated
    // local whose capacity is a plain const
    pub fn functions(stages: &[u32]) -> ArrayVec<u32, MAX_STAGES> {
        let functions: ArrayVec<_, MAX_STAGES> = stages.iter().map(|s| s * 2).collect();
        functions
    }

    // tiny-skia-0.11.4/src/pipeline/mod.rs:407 and :440 — `for x in &mut av`.
    // THE ONLY by-`&mut` iteration sites in the whole graph, and they WRITE
    // through every element they visit: a walk bounded by `N` instead of `len`
    // would store into uninitialized slots.
    pub fn patch_tail(tail_functions: &mut ArrayVec<u32, MAX_STAGES>) {
        for fun in tail_functions {
            *fun += 1;
        }
    }

    // tiny-skia-0.11.4/src/pipeline/mod.rs:379 — `is_empty()`
    // tiny-skia-0.11.4/src/pipeline/mod.rs:406, :439 — `.clone()`
    // tiny-skia-0.11.4/src/pipeline/mod.rs:498, :499, :513, :514 — `as_slice()`
    // tiny-skia-0.11.4/src/pipeline/mod.rs:387, :395, :432 — `.iter()`
    pub fn run(functions: &ArrayVec<u32, MAX_STAGES>) -> (bool, Vec<u32>, u32) {
        let mut tail = functions.clone();
        patch_tail(&mut tail);
        (
            functions.is_empty(),
            tail.as_slice().to_vec(),
            functions.iter().sum(),
        )
    }
}

#[test]
fn tiny_skia_forms() {
    use tiny_skia::*;
    let edges: ClippedEdges = [PathEdge(1), PathEdge(2)].into_iter().collect();
    assert!(any(&edges));
    assert_eq!(push_edges(edges), 3);

    let functions = functions(&[1, 2, 3]);
    assert_eq!(run(&functions), (false, vec![3, 5, 7], 12));
}

// ── vte 0.15.0 (dev edges only, and it names nothing) ───────────────────────

mod vte {
    use arrayvec::ArrayVec;

    pub const OSC_RAW_BUF_SIZE: usize = 1024;

    // vte-0.15.0/src/lib.rs:62. Reproduced for completeness and to record the
    // finding: this field — and the `use arrayvec::ArrayVec;` at lib.rs:36 —
    // sit under `#[cfg(not(feature = "std"))]`, and vte resolves in aterm's
    // graph with `std` ON. So vte compiles with ZERO `ArrayVec` uses and
    // imposes no API requirement at all. It is still a REAL consumer for the
    // purposes of the patch (`aterm-bench` pulls it directly and through
    // `alacritty_terminal 0.26.0`, so `[patch.crates-io]` reaches it and
    // `cargo test -p aterm-bench` compiles it) — which is why a
    // `cargo tree -e normal` census, which hides dev edges, is not sufficient
    // evidence for a patch change.
    pub struct Parser {
        pub osc_raw: ArrayVec<u8, OSC_RAW_BUF_SIZE>,
    }
}

#[test]
fn vte_forms() {
    let mut p = vte::Parser {
        osc_raw: arrayvec::ArrayVec::new(),
    };
    p.osc_raw.push(b'x');
    assert_eq!(p.osc_raw.as_slice(), b"x");
}

// ── Deref coercion sites ────────────────────────────────────────────────────

/// Every `&ArrayVec<T, N>` that the consumers hand to something expecting
/// `&[T]`, plus the slice inherent methods they reach through `Deref`. None of
/// these compiles without `Deref<Target = [T]>`, and none of them is visible in
/// a grep for `ArrayVec`.
mod deref_sites {
    use arrayvec::ArrayVec;
    use std::borrow::Cow;

    // wgpu-core: `Cow::Borrowed(&temp_layouts)`
    pub fn cow_borrowed(temp_layouts: &ArrayVec<u32, 4>) -> Cow<'_, [u32]> {
        Cow::Borrowed(temp_layouts)
    }

    // wgpu-hal gles: `gl.draw_buffers(&indices)`
    pub fn draw_buffers(indices: &ArrayVec<u32, 8>) -> usize {
        fn gl_draw_buffers(bufs: &[u32]) -> usize {
            bufs.len()
        }
        gl_draw_buffers(indices)
    }

    // wgpu-hal vulkan: `.clear_values(&vk_clear_values)` / `.attachments(attachment_views)`
    pub fn vk_builder(
        vk_clear_values: &ArrayVec<u32, 8>,
        attachment_views: &ArrayVec<u32, 8>,
    ) -> usize {
        fn clear_values(v: &[u32]) -> usize {
            v.len()
        }
        clear_values(vk_clear_values) + clear_values(attachment_views)
    }

    // wgpu-core: `transition_textures(&barriers)`
    pub fn transition_textures(barriers: &ArrayVec<u32, 2>) -> usize {
        fn inner(b: &[u32]) -> usize {
            b.len()
        }
        inner(barriers)
    }

    // wgpu-hal dx12: `Some(&compile_args)`
    pub fn some_slice(compile_args: &ArrayVec<u32, 13>) -> Option<&[u32]> {
        Some(compile_args)
    }

    /// The slice inherent methods and index forms the consumers reach through
    /// `Deref`/`DerefMut`: `Index` (usize / `Range` / `RangeFrom` / `RangeFull`),
    /// `get`, `get_mut`, `first`, `last`, `sort`, `contains`.
    ///
    /// Three more `Deref`-reached methods are pinned in their real consumer
    /// context rather than here, because the CONTEXT is what is being tested:
    /// `get_unchecked_mut` and `get_unchecked` in `wgpu_core`
    /// (`track/texture.rs:108` / `:1383`, an `unsafe` block and two different
    /// `SliceIndex` impls) and `last_mut` in `wgpu_hal`
    /// (`gles/device.rs:409-412`, a block expression whose value is the `&mut`
    /// it returns).
    pub fn slice_surface(av: &mut ArrayVec<u32, 8>) -> bool {
        av.sort();
        let _ = av[0];
        let _ = &av[1..2];
        let _ = &av[1..];
        let _ = &av[..];
        if let Some(v) = av.get_mut(0) {
            *v += 0;
        }
        av.get(1).is_some()
            && av.first() == Some(&av[0])
            && av.last().is_some()
            && av.contains(&av[0])
            && av[..] == *av.as_slice()
    }
}

#[test]
fn deref_sites_forms() {
    let mut av: arrayvec::ArrayVec<u32, 8> = [3, 1, 2].into_iter().collect();
    assert!(deref_sites::slice_surface(&mut av));
    assert_eq!(av.as_slice(), &[1, 2, 3]);

    let small: arrayvec::ArrayVec<u32, 4> = [1, 2].into_iter().collect();
    assert_eq!(deref_sites::cow_borrowed(&small).len(), 2);
    assert_eq!(deref_sites::draw_buffers(&av), 3);
    assert_eq!(deref_sites::vk_builder(&av, &av), 6);

    let barriers: arrayvec::ArrayVec<u32, 2> = [9].into_iter().collect();
    assert_eq!(deref_sites::transition_textures(&barriers), 1);

    let args: arrayvec::ArrayVec<u32, 13> = [1, 2, 3].into_iter().collect();
    assert_eq!(deref_sites::some_slice(&args), Some(&[1u32, 2, 3][..]));
}

// ── The two exported iterator types, by name ────────────────────────────────

/// Upstream exports `arrayvec::IntoIter` and `arrayvec::Drain` as nameable
/// types. Nothing in aterm's graph writes either name today, but a consumer
/// storing an iterator in a struct would, so the names have to resolve and
/// carry their iterator impls.
#[test]
fn iterator_types_are_nameable() {
    let av: arrayvec::ArrayVec<u32, 4> = [1, 2, 3].into_iter().collect();
    let it: arrayvec::IntoIter<u32, 4> = av.into_iter();
    assert_eq!(it.len(), 3);
    assert_eq!(it.as_slice(), &[1, 2, 3]);

    let mut av: arrayvec::ArrayVec<u32, 4> = [1, 2, 3].into_iter().collect();
    let d: arrayvec::Drain<'_, u32, 4> = av.drain(1..);
    assert_eq!(d.len(), 2);
    drop(d);
    assert_eq!(av.as_slice(), &[1]);

    // `CapacityError`, upstream's third public export.
    let e: arrayvec::CapacityError<u32> =
        arrayvec::ArrayVec::<u32, 0>::new().try_push(1).unwrap_err();
    assert_eq!(e.element(), 1);
}
