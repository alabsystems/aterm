// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-and-run coverage for `ArrayVec` consumer forms used by `naga` and `wgpu`.
#![allow(dead_code)]

mod naga {
    use arrayvec::ArrayVec;

    /// Stand-in for `naga::VectorSize`.
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

    #[derive(Debug)]
    pub enum Decorated {
        BuiltIn(BuiltIn, ArrayVec<Decoration, 2>),
        Location(Word),
    }

    #[derive(Debug)]
    pub enum LiteralVector {
        F64(ArrayVec<f64, { VectorSize::MAX }>),
        U32(ArrayVec<u32, { VectorSize::MAX }>),
    }

    impl LiteralVector {
        pub fn len(&self) -> usize {
            match self {
                Self::F64(v) => v.len(),
                Self::U32(v) => v.len(),
            }
        }
    }

    pub fn compose_exact<const N: usize>(src: &[f64]) -> [f64; N] {
        let mut arr = ArrayVec::<_, N>::new();
        for v in src {
            arr.push(*v);
        }
        arr.into_inner().unwrap()
    }

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

    pub fn flatten_matrix(columns: &[Handle]) -> ArrayVec<Handle, 16> {
        let mut flattened = ArrayVec::new();
        flattened.extend(columns.iter().cloned());
        flattened
    }

    pub fn column_ids(words: &[Word]) -> (ArrayVec<Word, 4>, ArrayVec<Word, 4>) {
        let mut column_ids: ArrayVec<Word, 4> = ArrayVec::new();
        for w in words {
            column_ids.push(*w);
        }
        let collected = words.iter().copied().collect::<ArrayVec<_, 4>>();
        (column_ids, collected)
    }

    pub fn recollect(new_components: ArrayVec<Handle, 4>) -> ArrayVec<Handle, 4> {
        new_components.into_iter().collect()
    }

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

mod wgpu {
    use arrayvec::ArrayVec;

    /// Stand-ins for `wgc::MAX_BIND_GROUPS` / `wgc::MAX_VERTEX_BUFFERS`.
    pub const MAX_BIND_GROUPS: usize = 4;
    pub const MAX_VERTEX_BUFFERS: usize = 8;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct BindGroupLayoutId(pub u32);
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct VertexBufferLayout(pub u32);

    pub fn bind_group_layouts(ids: &[u32]) -> ArrayVec<BindGroupLayoutId, { MAX_BIND_GROUPS }> {
        ids.iter()
            .map(|id| BindGroupLayoutId(*id))
            .collect::<ArrayVec<_, { MAX_BIND_GROUPS }>>()
    }

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

    #[derive(Debug)]
    pub struct ExternalTexture {
        pub(crate) planes: arrayvec::ArrayVec<Arc<TextureView>, 3>,
    }

    #[derive(Clone, Debug, Hash, PartialEq)]
    pub struct AttachmentData<T> {
        pub colors: ArrayVec<Option<T>, { MAX_COLOR_ATTACHMENTS }>,
        pub resolves: ArrayVec<T, { MAX_COLOR_ATTACHMENTS }>,
        pub depth_stencil: Option<T>,
    }
    impl<T: PartialEq> Eq for AttachmentData<T> {}

    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct TextureLayerInitTracker(pub u32);

    #[derive(Debug)]
    pub struct TextureInitTracker {
        pub mips: ArrayVec<TextureLayerInitTracker, { MAX_MIP_LEVELS as usize }>,
    }

    #[derive(Debug)]
    pub enum PipelineLayoutSource {
        Provided(u32),
        Derived(Box<ArrayVec<EntryMap, { MAX_BIND_GROUPS }>>),
    }

    pub type AttachmentDataVec<T> = ArrayVec<T, MAX_TOTAL_ATTACHMENTS>;

    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct ComplexTextureState {
        pub mips: ArrayVec<u32, { MAX_MIP_LEVELS as usize }>,
    }

    #[allow(clippy::boxed_local)]
    pub fn finish_derived(
        mut derived: Box<ArrayVec<EntryMap, { MAX_BIND_GROUPS }>>,
    ) -> Vec<EntryMap> {
        derived.pop();
        derived.into_iter().collect()
    }

    pub fn color_target_states(n: usize) -> ArrayVec<u32, { MAX_COLOR_ATTACHMENTS }> {
        let cts: ArrayVec<_, { MAX_COLOR_ATTACHMENTS }> = (0..n as u32).collect();
        cts
    }

    pub fn shader_modules(
        vertex: ShaderModule,
        fragment: Option<ShaderModule>,
    ) -> ArrayVec<ShaderModule, 2> {
        let mut shader_modules: ArrayVec<ShaderModule, 2> = ArrayVec::new();
        shader_modules.push(vertex);
        shader_modules.extend(fragment);
        shader_modules
    }

    pub fn transfer_barriers(
        src_pending: &[u32],
        dst_pending: Option<u32>,
    ) -> ArrayVec<TextureBarrier, 2> {
        let mut barriers: ArrayVec<TextureBarrier, 2> =
            src_pending.iter().map(|p| StandIn(*p)).collect();
        barriers.extend(dst_pending.map(StandIn));
        barriers
    }

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

    pub fn bump_mips(complex: &mut ComplexTextureState, mips: std::ops::Range<usize>) {
        for mip in unsafe { complex.mips.get_unchecked_mut(mips) } {
            *mip += 1;
        }
    }

    pub fn read_mip(complex: &ComplexTextureState, mip_id: usize) -> u32 {
        // SAFETY: the caller has bounds-checked `mip_id`, as wgpu-core does one line above its own
        // call.
        *unsafe { complex.mips.get_unchecked(mip_id) }
    }

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
    assert_eq!(Arc::strong_count(&arc), 1);

    let mut complex = ComplexTextureState::default();
    complex.mips.extend([1, 2, 3]);
    bump_mips(&mut complex, 0..2);
    assert_eq!(complex.mips.as_slice(), &[2, 3, 3]);
    assert_eq!(read_mip(&complex, 1), 3);

    let cts = color_target_states(2);
    assert_eq!(describe(&cts), (false, &[0u32, 1][..]));
}

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

    pub fn compile_args(source_name: Option<Pcwstr>, extra: [Pcwstr; 3]) -> ArrayVec<Pcwstr, 13> {
        let mut compile_args = arrayvec::ArrayVec::<Pcwstr, 13>::new_const();
        if let Some(name) = source_name {
            compile_args.push(name);
        }
        compile_args.extend(extra);
        compile_args
    }

    pub fn view_instancing(n: u32) -> Vec<ViewInstanceLocation> {
        let mut view_instancing = ArrayVec::<ViewInstanceLocation, 32>::new();
        for i in 0..n {
            view_instancing.push(ViewInstanceLocation(i));
        }
        view_instancing.as_slice().to_vec()
    }

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub struct ProgramCacheKey {
        pub stages: ArrayVec<ProgramStage, 3>,
    }

    #[derive(Clone, Debug, Default)]
    pub struct PipelineInner {
        pub immediates_descs: ArrayVec<ImmediateDesc, MAX_IMMEDIATES_COMMANDS>,
    }

    pub type InvalidatedAttachments = ArrayVec<u32, { MAX_COLOR_ATTACHMENTS + 2 }>;

    #[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
    pub struct RenderPassKey {
        pub colors: ArrayVec<TextureView, { MAX_COLOR_ATTACHMENTS }>,
        pub resolves: ArrayVec<TextureView, { MAX_COLOR_ATTACHMENTS }>,
    }

    #[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
    pub struct FramebufferKey {
        pub attachments: ArrayVec<TextureView, { MAX_COLOR_ATTACHMENTS }>,
    }

    pub fn empty_render_pass_key() -> RenderPassKey {
        RenderPassKey {
            colors: ArrayVec::default(),
            resolves: ArrayVec::default(),
        }
    }

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

        pub fn begin_pass(&mut self) {
            self.resolve_attachments.clear();
            self.invalidate_attachments.clear();
            self.color_targets.clear();
            self.vertex_attributes.clear();
        }

        pub fn bind_pipeline(&mut self, inner: &PipelineInner) {
            self.immediates_descs.clone_from(&inner.immediates_descs);
        }

        pub fn end_pass(&mut self) -> Vec<(u32, TextureView)> {
            let mut done = Vec::new();
            for (attachment, dst) in self.resolve_attachments.drain(..) {
                done.push((attachment, dst));
            }
            done
        }

        pub fn vertex_attribute_sum(&self) -> u32 {
            let mut total = 0;
            for vat in &self.vertex_attributes {
                total += vat.0;
            }
            total
        }

        pub fn color_targets_differ(&self, pipeline: &Self) -> bool {
            self.color_targets[..] != pipeline.color_targets[..]
        }
    }

    pub fn shader_stages(shaders: &ArrayVec<(u32, ProgramStage), 3>) -> u32 {
        let mut total = 0;
        for &(naga_stage, stage) in shaders {
            total += naga_stage + stage.0;
        }
        total
    }

    pub fn delete_shaders(shaders_to_delete: ArrayVec<u32, 3>) -> u32 {
        let mut deleted = 0;
        for shader in shaders_to_delete {
            deleted += shader;
        }
        deleted
    }

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

    pub fn immediates(immediates_items: ArrayVec<u32, 4>) -> Vec<(usize, u32)> {
        let mut out = Vec::new();
        for (stage_idx, stage_items) in immediates_items.into_iter().enumerate() {
            out.push((stage_idx, stage_items));
        }
        out
    }

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

/// Slice coercion and `Deref` slice method coverage for `&ArrayVec<T, N>`.
mod deref_sites {
    use arrayvec::ArrayVec;
    use std::borrow::Cow;

    pub fn cow_borrowed(temp_layouts: &ArrayVec<u32, 4>) -> Cow<'_, [u32]> {
        Cow::Borrowed(temp_layouts)
    }

    pub fn draw_buffers(indices: &ArrayVec<u32, 8>) -> usize {
        fn gl_draw_buffers(bufs: &[u32]) -> usize {
            bufs.len()
        }
        gl_draw_buffers(indices)
    }

    pub fn vk_builder(
        vk_clear_values: &ArrayVec<u32, 8>,
        attachment_views: &ArrayVec<u32, 8>,
    ) -> usize {
        fn clear_values(v: &[u32]) -> usize {
            v.len()
        }
        clear_values(vk_clear_values) + clear_values(attachment_views)
    }

    pub fn transition_textures(barriers: &ArrayVec<u32, 2>) -> usize {
        fn inner(b: &[u32]) -> usize {
            b.len()
        }
        inner(barriers)
    }

    pub fn some_slice(compile_args: &ArrayVec<u32, 13>) -> Option<&[u32]> {
        Some(compile_args)
    }

    /// The slice inherent methods and index forms the consumers reach through `Deref`/`DerefMut`.
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

    let e: arrayvec::CapacityError<u32> =
        arrayvec::ArrayVec::<u32, 0>::new().try_push(1).unwrap_err();
    assert_eq!(e.element(), 1);
}
