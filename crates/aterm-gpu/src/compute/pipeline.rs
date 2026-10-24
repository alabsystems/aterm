// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kernel's generated prelude, its library and specialised pipelines, and each pipeline's qualification (design §12.3, §12.5).

use std::cell::Cell;
use std::rc::Rc;

use aterm_effects::int_bag_mlp::{
    self, B_MAX, Batch, C_MAX, Digest, Dims, E_MAX, H_MAX, LAYOUT, MAX_ROW_IDS, ParamsView,
};

use super::device::{ComputeDevice, Domain};
use super::job::{Job, Params};
use crate::metal::ffi::{self, CompileOptions, FunctionConstants, Obj};

/// The kernel's MSL source, before its prelude.
pub const KERNEL: &str = include_str!("../../shaders/int_bag_mlp.metal");

/// The test-only kernel that echoes every header word by its generated name.
#[cfg(test)]
pub(super) const ECHO: &str = include_str!("../../shaders/int_bag_mlp_echo.metal");

/// The kernel's entry point.
const ENTRY: &str = "k_int_bag_mlp";

/// The domain under which [`identity`] digests a pipeline.
pub const PIPELINE_DOMAIN: &[u8] = b"aterm-gpu-int-bag-mlp-pipeline-v1\0";

/// The smallest run salt: above every row index, since ROWS is at most 2²⁰.
pub const RUN_SALT_MIN: u32 = 1 << 21;

// The function-constant indices the kernel declares.
const FC_EMBED: usize = 0;
const FC_HIDDEN: usize = 1;
const FC_CLASSES: usize = 2;
const FC_MIN_IDS: usize = 3;
const FC_RUN_SALT: usize = 4;

/// One `constant constexpr uint NAME = index;` line per `int_bag_mlp::LAYOUT` field, job header first.
pub fn prelude() -> String {
    LAYOUT
        .fields()
        .map(|(name, index)| format!("constant constexpr uint {name} = {index};\n"))
        .collect()
}

/// The canonical MSL a pipeline is compiled from: the prelude, then [`KERNEL`].
pub fn source() -> String {
    format!("{}{KERNEL}", prelude())
}

/// The values a pipeline is specialised to: E, H, C and MIN_IDS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shape {
    /// E.
    pub embed: u32,
    /// H.
    pub hidden: u32,
    /// C.
    pub classes: u32,
    /// MIN_IDS.
    pub min_ids: u32,
}

impl Shape {
    /// The shape of `params`.
    pub fn of(params: &Params) -> Shape {
        let d = params.dims();
        Shape {
            embed: d.embed,
            hidden: d.hidden,
            classes: d.classes,
            min_ids: params.min_ids(),
        }
    }

    /// Whether the kernel's thread-private arrays (`int S[32]`, `int h[128]`) hold it.
    const fn fits(self) -> bool {
        self.embed >= 1
            && self.embed <= E_MAX
            && self.hidden >= 1
            && self.hidden <= H_MAX
            && self.classes >= 1
            && self.classes <= C_MAX
            && self.min_ids >= 1
    }
}

/// A pipeline's identity: `Digest::of(PIPELINE_DOMAIN, [the canonical source, E, H, C, MIN_IDS])`, each number a little-endian u32, no salt in it.
pub fn identity(shape: Shape) -> Digest {
    Digest::of(
        PIPELINE_DOMAIN,
        &[
            source().as_bytes(),
            &shape.embed.to_le_bytes(),
            &shape.hidden.to_le_bytes(),
            &shape.classes.to_le_bytes(),
            &shape.min_ids.to_le_bytes(),
        ],
    )
}

/// The first output word where a qualification job and the CPU reference differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mismatch {
    /// The qualification batch.
    pub batch: usize,
    /// The word within its output.
    pub word: usize,
    /// The GPU's word.
    pub gpu: u32,
    /// The reference's word.
    pub cpu: u32,
}

/// Why a library or pipeline was not made, or a pipeline is unusable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipelineError {
    /// The MSL did not compile.
    Compile(String),
    /// The specialised entry point was not made.
    Function(String),
    /// The pipeline state was not made, or reads no thread width.
    Build(String),
    /// Metal made no function-constant values.
    NoConstants,
    /// The shape lies outside the kernel's maxima.
    BadShape(Shape),
    /// A run salt below [`RUN_SALT_MIN`], which a row index could reach.
    SaltTooSmall {
        /// The salt.
        salt: u32,
    },
    /// Qualification found a word that differs from the CPU reference: the pipeline is refused for good.
    Unqualified(Mismatch),
    /// Qualification could not run to its comparison.
    Qualification(String),
}

/// A compiled MSL library holding the kernel.
#[derive(Debug)]
pub struct Library {
    lib: ffi::Library,
    domain: Rc<Domain>,
    salt: Option<u64>,
}

impl Library {
    /// Compile the canonical source.
    pub fn compile(dev: &ComputeDevice) -> Result<Library, PipelineError> {
        Self::compile_text(dev, &source(), None)
    }

    /// Compile the canonical source with a trailing comment naming `salt`, which misses Metal's library cache.
    pub fn compile_salted(dev: &ComputeDevice, salt: u64) -> Result<Library, PipelineError> {
        let text = format!("{}\n// aterm compute run salt {salt}\n", source());
        Self::compile_text(dev, &text, Some(salt))
    }

    /// Compile `text` as it stands, which a test uses for a deliberately wrong kernel.
    #[cfg(test)]
    pub(super) fn compile_source(
        dev: &ComputeDevice,
        text: &str,
    ) -> Result<Library, PipelineError> {
        Self::compile_text(dev, text, None)
    }

    fn compile_text(
        dev: &ComputeDevice,
        text: &str,
        salt: Option<u64>,
    ) -> Result<Library, PipelineError> {
        let opts = CompileOptions::aterm_default()
            .ok_or_else(|| PipelineError::Compile("MTLCompileOptions allocation failed".into()))?;
        let lib = dev
            .domain
            .device()
            .new_library_with_options(text, &opts)
            .map_err(PipelineError::Compile)?;
        Ok(Library {
            lib,
            domain: Rc::clone(&dev.domain),
            salt,
        })
    }

    /// The comment salt, if any.
    #[inline]
    pub const fn salt(&self) -> Option<u64> {
        self.salt
    }
}

/// Where a pipeline stands with qualification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    Pending,
    Qualified,
    Refused(Mismatch),
}

/// A compute pipeline specialised to one [`Shape`], usable for jobs once qualified.
#[derive(Debug)]
pub struct Pipeline {
    pso: Obj,
    domain: Rc<Domain>,
    shape: Shape,
    library_salt: Option<u64>,
    run_salt: Option<u32>,
    max_total_threads: u32,
    execution_width: u32,
    standing: Cell<Standing>,
}

impl Pipeline {
    /// Specialise `lib`'s kernel to `shape`.
    pub fn new(lib: &Library, shape: Shape) -> Result<Pipeline, PipelineError> {
        Self::specialise(lib, shape, None)
    }

    /// [`Self::new`] with the run salt `run_salt` (at least [`RUN_SALT_MIN`]) as one more function constant, which misses Metal's function cache.
    pub fn new_salted(
        lib: &Library,
        shape: Shape,
        run_salt: u32,
    ) -> Result<Pipeline, PipelineError> {
        if run_salt < RUN_SALT_MIN {
            return Err(PipelineError::SaltTooSmall { salt: run_salt });
        }
        Self::specialise(lib, shape, Some(run_salt))
    }

    /// Compile the canonical library, specialise it to `params`' shape and qualify it.
    pub fn build(dev: &ComputeDevice, params: &Params) -> Result<Pipeline, PipelineError> {
        let pipeline = Pipeline::new(&Library::compile(dev)?, Shape::of(params))?;
        pipeline.qualify()?;
        Ok(pipeline)
    }

    fn specialise(
        lib: &Library,
        shape: Shape,
        run_salt: Option<u32>,
    ) -> Result<Pipeline, PipelineError> {
        if !shape.fits() {
            return Err(PipelineError::BadShape(shape));
        }
        let fc = FunctionConstants::new().ok_or(PipelineError::NoConstants)?;
        fc.set_u32(shape.embed, FC_EMBED);
        fc.set_u32(shape.hidden, FC_HIDDEN);
        fc.set_u32(shape.classes, FC_CLASSES);
        fc.set_u32(shape.min_ids, FC_MIN_IDS);
        if let Some(salt) = run_salt {
            fc.set_u32(salt, FC_RUN_SALT);
        }
        let f = lib
            .lib
            .specialized_function(ENTRY, &fc)
            .map_err(PipelineError::Function)?;
        let pso = lib
            .domain
            .device()
            .new_compute_pipeline(&f)
            .map_err(PipelineError::Build)?;
        let to_u32 = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let max_total_threads = to_u32(ffi::max_total_threads_per_threadgroup(&pso));
        let execution_width = to_u32(ffi::thread_execution_width(&pso));
        if max_total_threads == 0 || execution_width == 0 {
            return Err(PipelineError::Build(format!(
                "a pipeline with {max_total_threads} threads per threadgroup and execution width {execution_width}"
            )));
        }
        Ok(Pipeline {
            pso,
            domain: Rc::clone(&lib.domain),
            shape,
            library_salt: lib.salt,
            run_salt,
            max_total_threads,
            execution_width,
            standing: Cell::new(Standing::Pending),
        })
    }

    /// Run the qualification set, the extreme blob's batches then the interior blob's, and compare every output word with the CPU reference; a mismatch refuses the pipeline for good.
    pub fn qualify(&self) -> Result<(), PipelineError> {
        self.qualify_against(None)
    }

    /// [`Self::qualify`] with one reference word flipped, which a correct pipeline must refuse.
    #[cfg(test)]
    pub(super) fn qualify_against_corrupted(&self, word: usize) -> Result<(), PipelineError> {
        self.qualify_against(Some(word))
    }

    fn qualify_against(&self, corrupt: Option<usize>) -> Result<(), PipelineError> {
        match self.standing.get() {
            Standing::Qualified => return Ok(()),
            Standing::Refused(m) => return Err(PipelineError::Unqualified(m)),
            Standing::Pending => {}
        }
        let failed = |what: &str, e: &dyn std::fmt::Debug| {
            PipelineError::Qualification(format!("{what}: {e:?}"))
        };
        let dev = ComputeDevice {
            domain: Rc::clone(&self.domain),
        };
        let mut batch_no = 0;
        for (blob, batches) in qualification_set(self.shape)? {
            let params = Params::new(&dev, &blob).map_err(|e| failed("params", &e))?;
            let view = params.view();
            for batch in &batches {
                let mut want = vec![0u32; batch.out_words()];
                int_bag_mlp::forward(&view, batch, &mut want)
                    .map_err(|e| failed("reference", &e))?;
                if batch_no == 0
                    && let Some(w) = corrupt.and_then(|at| want.get_mut(at))
                {
                    *w ^= 1;
                }
                let mut job = Job::new(&dev, &params);
                job.load(batch).map_err(|e| failed("load", &e))?;
                let done = job
                    .submit_qualifying(self)
                    .map_err(|(_, e)| failed("submit", &e))?
                    .wait()
                    .map_err(|e| failed("job", &e))?;
                let got = done.out();
                if got.len() != want.len() {
                    return Err(failed("out length", &(got.len(), want.len())));
                }
                if let Some(word) = got.iter().zip(&want).position(|(g, w)| g != w) {
                    let m = Mismatch {
                        batch: batch_no,
                        word,
                        gpu: got[word],
                        cpu: want[word],
                    };
                    self.standing.set(Standing::Refused(m));
                    return Err(PipelineError::Unqualified(m));
                }
                batch_no += 1;
            }
        }
        self.standing.set(Standing::Qualified);
        Ok(())
    }

    /// Whether qualification passed.
    #[inline]
    pub fn is_qualified(&self) -> bool {
        self.standing.get() == Standing::Qualified
    }

    /// The shape it is specialised to.
    #[inline]
    pub const fn shape(&self) -> Shape {
        self.shape
    }

    /// Its identity, which no salt enters.
    pub fn identity(&self) -> Digest {
        identity(self.shape)
    }

    /// The library's comment salt, if any.
    #[inline]
    pub const fn library_salt(&self) -> Option<u64> {
        self.library_salt
    }

    /// The run salt, if any.
    #[inline]
    pub const fn run_salt(&self) -> Option<u32> {
        self.run_salt
    }

    /// The pipeline state's `maxTotalThreadsPerThreadgroup`.
    #[inline]
    pub const fn max_total_threads_per_threadgroup(&self) -> u32 {
        self.max_total_threads
    }

    /// The pipeline state's `threadExecutionWidth`.
    #[inline]
    pub const fn thread_execution_width(&self) -> u32 {
        self.execution_width
    }

    /// The threads per threadgroup a dispatch uses: the execution width, capped at the pipeline's maximum.
    #[inline]
    pub const fn threadgroup_width(&self) -> u32 {
        if self.execution_width < self.max_total_threads {
            self.execution_width
        } else {
            self.max_total_threads
        }
    }

    /// The pipeline state.
    #[inline]
    pub(super) const fn pso(&self) -> &Obj {
        &self.pso
    }

    /// The domain it was built in.
    #[inline]
    pub(super) const fn domain(&self) -> &Rc<Domain> {
        &self.domain
    }
}

/// The bucket count of the interior qualification blob.
const INTERIOR_BUCKETS: u32 = 256;

/// A qualification blob and the batches packed against it.
pub(super) type Qualifier = (Vec<u8>, Vec<Batch>);

/// The qualification set at `shape`, each blob with the batches packed against it: the extreme blob at B = 65,536 (both clamps, the row cap, ties, MIN_IDS's edge, no ids), then the interior blob at B = 256, whose units reach steps 4–6 unclamped under the identity table.
pub(super) fn qualification_set(shape: Shape) -> Result<Vec<Qualifier>, PipelineError> {
    let failed = |what: &str, e: &dyn std::fmt::Debug| {
        PipelineError::Qualification(format!("{what}: {e:?}"))
    };
    let at = |buckets: u32| Dims {
        buckets,
        embed: shape.embed,
        hidden: shape.hidden,
        classes: shape.classes,
    };
    let extreme = int_bag_mlp::extreme_params_for(at(B_MAX), shape.min_ids);
    let interior = int_bag_mlp::interior_params_for(at(INTERIOR_BUCKETS), shape.min_ids);
    let extreme_batches = {
        let p = int_bag_mlp::check(&extreme).map_err(|e| failed("extreme params", &e))?;
        qualification_batches(&p).map_err(|e| failed("extreme batch", &e))?
    };
    let interior_batch = {
        let p = int_bag_mlp::check(&interior).map_err(|e| failed("interior params", &e))?;
        let (offsets, ids) = int_bag_mlp::interior_rows(p.dims(), shape.min_ids);
        Batch::pack(&p, offsets, ids).map_err(|e| failed("interior batch", &e))?
    };
    Ok(vec![
        (extreme, extreme_batches),
        (interior, vec![interior_batch]),
    ])
}

/// The extreme blob's qualification batches for `p`: the extreme rows (three at `MAX_ROW_IDS`, step 3's bound with either sign, tied argmaxes), rows either side of MIN_IDS, and an all-abstain batch with no ids.
fn qualification_batches(p: &ParamsView<'_>) -> Result<Vec<Batch>, int_bag_mlp::BatchError> {
    let (mut offsets, mut ids) = int_bag_mlp::extreme_rows();
    let top = (p.dims().buckets - 1) as u16;
    let edge = p.min_ids().min(MAX_ROW_IDS) as usize;
    for row in [
        vec![0u16; edge - 1],
        vec![top; edge],
        (0..edge).map(|i| (i % 7) as u16).collect(),
    ] {
        ids.extend_from_slice(&row);
        offsets.push(ids.len() as u32);
    }
    Ok(vec![
        Batch::pack(p, offsets, ids)?,
        Batch::pack(p, vec![0, 0, 0, 0], Vec::new())?,
    ])
}
