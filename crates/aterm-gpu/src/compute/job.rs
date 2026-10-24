// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sealed shared parameters and the job typestate: exclusive buffers, completion, timing and failure (design §12.4–12.5).

use std::fmt;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use aterm_effects::int_bag_mlp::{self, Batch, Dims, JOB_WORDS, ParamsError, ParamsView};

#[cfg(debug_assertions)]
use super::device::Fault;
use super::device::{
    AllocError, ComputeDevice, Counted, Domain, InjectedFault, Ledger, Limits, TooLarge, What,
};
use super::pipeline::{Pipeline, Shape};
use crate::metal::ffi::{self, AutoreleasePool, CbEvent, ComputeEncoder, Id, MachTimebase, Obj};
use crate::metal::loss::{self, CbOutcome};

const _: () = assert!(
    cfg!(target_endian = "little"),
    "the job and params layouts are little-endian"
);

/// How long a wait gives a committed job to reach a terminal status before it is declared hung.
pub const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// What `load` fills `out` with, so a word the kernel failed to write cannot pass for a result (no class word is this large).
const OUT_POISON: u32 = 0xa5a5_a5a5;

/// The `mach_absolute_time` tick counter every [`JobTimes`] stamp is read on.
#[inline]
pub fn ticks() -> u64 {
    ffi::mach_absolute_time()
}

/// `mach_timebase_info`: ticks × `numer` / `denom` is nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timebase {
    /// The numerator.
    pub numer: u32,
    /// The denominator, never 0.
    pub denom: u32,
}

impl Timebase {
    /// The host's timebase, or `None` when `mach_timebase_info` fails.
    pub fn host() -> Option<Timebase> {
        ffi::mach_timebase().map(Timebase::from)
    }

    /// `ticks` in nanoseconds, saturating.
    pub fn ns(self, ticks: u64) -> u64 {
        let ns = u128::from(ticks) * u128::from(self.numer) / u128::from(self.denom);
        u64::try_from(ns).unwrap_or(u64::MAX)
    }
}

impl From<MachTimebase> for Timebase {
    fn from(t: MachTimebase) -> Timebase {
        Timebase {
            numer: t.numer,
            denom: t.denom,
        }
    }
}

/// One completed job's stamps, each on [`ticks`] except Metal's two GPU times (seconds on the same base).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JobTimes {
    /// The domain's timebase, read at open.
    pub timebase: Option<Timebase>,
    /// Before the command buffer was made.
    pub before_encode: u64,
    /// After `commit` returned.
    pub after_commit: u64,
    /// The scheduled handler, if it ran before the completed one was seen.
    pub scheduled: Option<u64>,
    /// `GPUStartTime`, in seconds.
    pub gpu_start: f64,
    /// `GPUEndTime`, in seconds.
    pub gpu_end: f64,
    /// The completed handler.
    pub handler: u64,
    /// The waiter's return.
    pub woke: u64,
}

/// Why [`Params::new`] refused a blob, before any buffer exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamsRefusal {
    /// The blob exceeds the params limit, or the domain's.
    TooLarge(TooLarge),
    /// The params checker refused it.
    Check(ParamsError),
    /// The domain is latched lost.
    DomainLost(String),
    /// Metal made no buffer.
    AllocFailed {
        /// The bytes asked for.
        bytes: u64,
    },
}

/// A checked params blob, written once into a shared buffer in [`Params::new`] and never written again.
#[derive(Debug)]
pub struct Params {
    buf: Counted,
    len: usize,
    dims: Dims,
    min_ids: u32,
}

impl Params {
    /// Check `bytes` with the profile's params checker and the limits, then copy them once into a new shared buffer.
    pub fn new(dev: &ComputeDevice, bytes: &[u8]) -> Result<Arc<Params>, ParamsRefusal> {
        let d = &dev.domain;
        if let Some(reason) = d.lost() {
            return Err(ParamsRefusal::DomainLost(reason));
        }
        let (len, limit) = (bytes.len() as u64, d.limits().params);
        if len > limit {
            return Err(ParamsRefusal::TooLarge(TooLarge {
                what: What::Params,
                bytes: len,
                limit,
            }));
        }
        let view = int_bag_mlp::check(bytes).map_err(ParamsRefusal::Check)?;
        let buf = d.allocate(len).map_err(|e| match e {
            AllocError::TooLarge(t) => ParamsRefusal::TooLarge(t),
            AllocError::Failed { bytes } => ParamsRefusal::AllocFailed { bytes },
        })?;
        // SAFETY: `buf` was just made at `len.max(4)` bytes of shared storage, no command buffer references it, and nothing else holds it.
        let Some(dst) = (unsafe { view_mut::<u8>(&buf, bytes.len()) }) else {
            return Err(ParamsRefusal::AllocFailed { bytes: buf.len() });
        };
        dst.copy_from_slice(bytes);
        Ok(Arc::new(Params {
            buf,
            len: bytes.len(),
            dims: view.dims(),
            min_ids: view.min_ids(),
        }))
    }

    /// The blob, read from the shared buffer the GPU reads.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the buffer holds `len` bytes written once in `new`; no method writes it and the kernel binds it read-only, so it only changes when it drops, after this borrow.
        unsafe { view::<u8>(&self.buf, self.len) }
    }

    /// The blob's checked view, which the CPU reference reads.
    pub fn view(&self) -> ParamsView<'_> {
        int_bag_mlp::check(self.bytes()).expect("sealed params bytes stay the checked blob")
    }

    /// B, E, H and C.
    #[inline]
    pub const fn dims(&self) -> Dims {
        self.dims
    }

    /// The fewest ids a row needs not to abstain.
    #[inline]
    pub const fn min_ids(&self) -> u32 {
        self.min_ids
    }

    /// The shared buffer.
    #[inline]
    pub(super) const fn obj(&self) -> &Obj {
        self.buf.obj()
    }

    /// The ledger its buffer is counted in.
    #[inline]
    pub(super) const fn ledger(&self) -> &Arc<Ledger> {
        self.buf.ledger()
    }
}

/// `contents` as a `*mut T`, or `None` when it is null or not aligned to `T`, so a view of a buffer Metal maps nowhere or misaligned is refused rather than undefined.
#[inline]
pub(super) fn typed<T>(contents: *mut u8) -> Option<*mut T> {
    let p = contents.cast::<T>();
    (!p.is_null() && p.is_aligned()).then_some(p)
}

/// The first `count` `T`s of `buf`'s shared storage, or nothing when Metal maps none or maps it misaligned for `T`.
///
/// # Safety
/// `T` is `u8`, `u16` or `u32`; `buf` holds at least `count` of them; no GPU work writes it and no `&mut` view of it lives while the slice does.
unsafe fn view<T: Copy>(buf: &Counted, count: usize) -> &[T] {
    let Some(p) = typed::<T>(ffi::buffer_contents(buf.obj())) else {
        return &[];
    };
    // SAFETY: shared storage is CPU-visible at `contents` for the buffer's life, which `buf`'s borrow bounds; `p` is non-null and aligned to `T`, which `typed` checked (Metal returns shared contents at least 256-byte aligned, measured, which is at least `align_of::<T>()` for u8, u16 and u32); the caller pins the length, the element types (valid for every bit pattern) and the absence of writers.
    unsafe { std::slice::from_raw_parts(p, count) }
}

/// The first `count` `T`s of `buf`'s shared storage, mutably, or `None` when Metal maps none or maps it misaligned for `T`.
///
/// # Safety
/// `T` is `u8`, `u16` or `u32`; `buf` holds at least `count` of them; no GPU work reads or writes it and no other view of it lives while the slice does.
#[expect(
    clippy::mut_from_ref,
    reason = "the buffer is the GPU's memory, not the handle's: the caller pins exclusivity"
)]
unsafe fn view_mut<T: Copy>(buf: &Counted, count: usize) -> Option<&mut [T]> {
    let p = typed::<T>(ffi::buffer_contents(buf.obj()))?;
    // SAFETY: as `view` (non-null, aligned to `T`, CPU-visible for the buffer's life, the length and element types the caller's), plus the caller's exclusivity: nothing else reads or writes these bytes while the slice lives.
    Some(unsafe { std::slice::from_raw_parts_mut(p, count) })
}

mod sealed {
    pub trait Sealed {}
}

/// A job's state: [`Prepared`], [`InFlight`] or [`Completed`].
pub trait JobState: sealed::Sealed {
    /// Whether the GPU may still read or write the job's buffers.
    #[doc(hidden)]
    const IN_FLIGHT: bool;
    /// The state's name, for `Debug`.
    #[doc(hidden)]
    const NAME: &'static str;
}

/// The CPU owns the buffers: it may load a batch, and nothing can read `out`.
#[derive(Debug)]
pub enum Prepared {}
/// The GPU owns the buffers: there is no accessor, only `wait`.
#[derive(Debug)]
pub enum InFlight {}
/// The GPU finished: `out` and the inputs are readable.
#[derive(Debug)]
pub enum Completed {}

impl sealed::Sealed for Prepared {}
impl sealed::Sealed for InFlight {}
impl sealed::Sealed for Completed {}

impl JobState for Prepared {
    const IN_FLIGHT: bool = false;
    const NAME: &'static str = "Prepared";
}
impl JobState for InFlight {
    const IN_FLIGHT: bool = true;
    const NAME: &'static str = "InFlight";
}
impl JobState for Completed {
    const IN_FLIGHT: bool = false;
    const NAME: &'static str = "Completed";
}

/// The binding of each job buffer, in [`Slots`] order: `job`, `offsets`, `ids`, `out`.
const BINDINGS: [usize; 4] = [0, 2, 3, 4];
/// The binding of `params`.
const PARAMS_BINDING: usize = 1;

/// A job's four buffers by role, each made on the first load that needs it larger.
type Slots = [Option<Counted>; 4];

/// The batch a job holds.
#[derive(Clone, Copy, Debug)]
struct Loaded {
    rows: u32,
    total_ids: u32,
    stride: u32,
}

/// The completed handler's reading.
#[derive(Clone, Copy, Debug)]
struct Terminal {
    status: usize,
    code: Option<isize>,
    gpu_start: f64,
    gpu_end: f64,
    at: u64,
}

/// What the handlers saw.
#[derive(Debug, Default)]
struct Observed {
    scheduled: Option<u64>,
    done: Option<Terminal>,
    /// Debug builds: an injected hang has the completed handler store its reading in `withheld`, where no waiter sees it.
    #[cfg(debug_assertions)]
    withhold: bool,
    #[cfg(debug_assertions)]
    withheld: Option<Terminal>,
}

/// The one thing a handler captures: a mutex and condvar the waiter blocks on.
#[derive(Debug, Default)]
struct Completion {
    state: Mutex<Observed>,
    cv: Condvar,
}

impl Completion {
    fn lock(&self) -> MutexGuard<'_, Observed> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock and notify once on the submitting thread, so the lazily boxed pthread state is allocated here and not in a handler.
    fn prime(&self) {
        drop(self.lock());
        self.cv.notify_all();
    }

    fn on_scheduled(&self) {
        let at = ffi::mach_absolute_time();
        self.lock().scheduled = Some(at);
    }

    fn on_completed(&self, cb: Id) {
        let status = loss::command_buffer_status(cb);
        let code = if status == loss::STATUS_ERROR {
            loss::command_buffer_error_code(cb)
        } else {
            None
        };
        let (gpu_start, gpu_end) = ffi::gpu_times(cb);
        let at = ffi::mach_absolute_time();
        let reading = Some(Terminal {
            status,
            code,
            gpu_start,
            gpu_end,
            at,
        });
        {
            let mut o = self.lock();
            #[cfg(debug_assertions)]
            if o.withhold {
                o.withheld = reading;
            } else {
                o.done = reading;
            }
            #[cfg(not(debug_assertions))]
            {
                o.done = reading;
            }
        }
        self.cv.notify_all();
    }

    /// Block until the completed handler ran or `timeout` passed.
    fn wait(&self, timeout: Duration) -> (Option<u64>, Option<Terminal>) {
        let (g, _) = self
            .cv
            .wait_timeout_while(self.lock(), timeout, |o| o.done.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        (g.scheduled, g.done)
    }

    /// Have the completed handler withhold its reading from [`Self::wait`], before commit.
    #[cfg(debug_assertions)]
    fn withhold(&self) {
        self.lock().withhold = true;
    }

    /// Block until the completed handler stored a withheld reading or `timeout` passed.
    #[cfg(debug_assertions)]
    fn wait_withheld(&self, timeout: Duration) {
        let _ = self
            .cv
            .wait_timeout_while(self.lock(), timeout, |o| o.withheld.is_none())
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// Hang `f` on `cb`'s `event`; Metal runs handlers on its own thread, hence `Send + Sync + 'static`.
fn on_event<F: Fn(Id) + Send + Sync + 'static>(cb: &Obj, event: CbEvent, f: F) -> bool {
    // SAFETY: `MTLCommandBufferHandler` is `void (^)(id<MTLCommandBuffer>)`, exactly the `(Id) -> ()` prototype `new1` builds; `f` may run on any thread, which its bounds admit, and aterm-objc aborts if it unwinds.
    let Some(block) = (unsafe { aterm_objc::RcBlock::new1(f) }) else {
        return false;
    };
    // SAFETY: `cb` is live and every caller commits it only after this returns; the block has the handler prototype.
    unsafe { ffi::add_handler(cb, event, &block) };
    true
}

/// A committed command buffer and what its waiter needs.
#[derive(Debug)]
struct Flight {
    /// The +1 handle, held until the job settles.
    _cb: Obj,
    completion: Arc<Completion>,
    serial: u64,
    fault: Option<InjectedFault>,
    before_encode: u64,
    after_commit: u64,
}

/// What a completed job keeps.
#[derive(Clone, Copy, Debug)]
struct Done {
    serial: u64,
    times: JobTimes,
}

/// Everything a job owns, boxed once; every transition moves the box to the next state's shell.
#[derive(Debug)]
struct Payload {
    domain: Rc<Domain>,
    params: Arc<Params>,
    slots: Slots,
    loaded: Option<Loaded>,
    flight: Option<Flight>,
    done: Option<Done>,
}

/// Why [`Job::load`] refused a batch, before any copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadRefusal {
    /// The domain is latched lost.
    DomainLost(String),
    /// The job's params belong to another device.
    OtherDomain,
    /// The batch was packed against params of other dimensions.
    DimsMismatch {
        /// The batch's.
        batch: Dims,
        /// The job's params'.
        params: Dims,
    },
    /// The job's buffers, or the domain's live bytes, would exceed their limit.
    TooLarge(TooLarge),
    /// Metal made no buffer.
    AllocFailed {
        /// The bytes asked for.
        bytes: u64,
    },
}

/// Why [`Job::submit`] gave the job back unsubmitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitRefusal {
    /// The domain is latched lost.
    DomainLost(String),
    /// The pipeline belongs to another device.
    OtherDomain,
    /// The pipeline has not passed qualification, or failed it.
    Unqualified,
    /// The params' dimensions or MIN_IDS differ from the pipeline's.
    DimsMismatch {
        /// The pipeline's.
        pipeline: Shape,
        /// The params'.
        params: Shape,
    },
    /// No batch is loaded.
    NotLoaded,
    /// Metal made no command buffer.
    NoCommandBuffer,
    /// Metal made no compute encoder.
    NoEncoder,
    /// A handler block could not be made.
    NoHandler,
}

/// How a submitted job failed; its buffers are released, except a hung job's, which are leaked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobFailure {
    /// A timeout or out-of-memory status: the domain is not latched.
    Retryable {
        /// The `MTLCommandBufferError` code.
        code: isize,
        /// Its name.
        name: &'static str,
    },
    /// This job's loss, as its classification reads; the domain is latched with its first loss's, so every later submit refuses.
    Lost(String),
    /// No terminal status within [`HANG_DETECTOR`]: the buffers and the params clone are leaked and the domain latched.
    Hung,
}

/// A job's buffers and its parameters in state `S`, owned by one thread.
///
/// ```no_run
/// use aterm_effects::tone_int;
/// use aterm_gpu::compute::{ComputeDevice, Job, Params, Pipeline};
///
/// let dev = ComputeDevice::open().expect("a unified-memory GPU");
/// let params = Params::new(&dev, tone_int::v1().expect("tone-int v1")).expect("sealed params");
/// let pipeline = Pipeline::build(&dev, &params).expect("a qualified pipeline");
/// let batch = tone_int::pack(&params.view(), &[tone_int::B1_TEXT]).expect("a batch");
/// let mut job = Job::new(&dev, &params);
/// job.load(&batch).expect("loaded");
/// let done = job.submit(&pipeline).expect("submitted").wait().expect("completed");
/// let class = done.out()[batch.out_stride() as usize - 1];
/// assert!(class <= params.dims().classes);
/// let mut again = done.recycle();
/// again.load(&batch).expect("reloaded");
/// ```
///
/// ```compile_fail,E0599
/// # use aterm_gpu::compute::{ComputeDevice, Job, Params};
/// # let dev = ComputeDevice::open().unwrap();
/// # let params = Params::new(&dev, aterm_effects::tone_int::v1().unwrap()).unwrap();
/// // `out` on a prepared job.
/// let job = Job::new(&dev, &params);
/// let _ = job.out();
/// ```
///
/// ```compile_fail,E0599
/// # use aterm_gpu::compute::{ComputeDevice, Job, Params, Pipeline};
/// # let dev = ComputeDevice::open().unwrap();
/// # let params = Params::new(&dev, aterm_effects::tone_int::v1().unwrap()).unwrap();
/// # let pipeline = Pipeline::build(&dev, &params).unwrap();
/// # let batch = aterm_effects::tone_int::pack(&params.view(), &["ok"]).unwrap();
/// # let mut job = Job::new(&dev, &params);
/// # job.load(&batch).unwrap();
/// // `out` on a job in flight.
/// let flight = job.submit(&pipeline).unwrap();
/// let _ = flight.out();
/// ```
///
/// ```compile_fail,E0599
/// # use aterm_gpu::compute::{ComputeDevice, Job, Params, Pipeline};
/// # let dev = ComputeDevice::open().unwrap();
/// # let params = Params::new(&dev, aterm_effects::tone_int::v1().unwrap()).unwrap();
/// # let pipeline = Pipeline::build(&dev, &params).unwrap();
/// # let batch = aterm_effects::tone_int::pack(&params.view(), &["ok"]).unwrap();
/// # let mut job = Job::new(&dev, &params);
/// # job.load(&batch).unwrap();
/// // `load` on a job in flight.
/// let mut flight = job.submit(&pipeline).unwrap();
/// flight.load(&batch).unwrap();
/// ```
///
/// ```compile_fail,E0382
/// # use aterm_gpu::compute::{ComputeDevice, Job, Params, Pipeline};
/// # let dev = ComputeDevice::open().unwrap();
/// # let params = Params::new(&dev, aterm_effects::tone_int::v1().unwrap()).unwrap();
/// # let pipeline = Pipeline::build(&dev, &params).unwrap();
/// # let batch = aterm_effects::tone_int::pack(&params.view(), &["ok"]).unwrap();
/// // The job after `submit` moved it.
/// let mut job = Job::new(&dev, &params);
/// job.load(&batch).unwrap();
/// let _flight = job.submit(&pipeline).unwrap();
/// job.load(&batch).unwrap();
/// ```
///
/// ```compile_fail,E0599
/// # use aterm_gpu::compute::{ComputeDevice, Params};
/// # let dev = ComputeDevice::open().unwrap();
/// // A write to sealed params.
/// let mut params = Params::new(&dev, aterm_effects::tone_int::v1().unwrap()).unwrap();
/// std::sync::Arc::get_mut(&mut params).unwrap().bytes_mut()[0] = 0;
/// ```
///
/// ```compile_fail,E0594
/// # use aterm_gpu::compute::{ComputeDevice, Params};
/// # let dev = ComputeDevice::open().unwrap();
/// // A write through the bytes sealed params read back.
/// let params = Params::new(&dev, aterm_effects::tone_int::v1().unwrap()).unwrap();
/// params.bytes()[0] = 0;
/// ```
///
/// ```compile_fail,E0616
/// # use aterm_gpu::compute::{ComputeDevice, Params};
/// # let dev = ComputeDevice::open().unwrap();
/// // A write to a field of sealed params held uniquely.
/// let mut params = Params::new(&dev, aterm_effects::tone_int::v1().unwrap()).unwrap();
/// std::sync::Arc::get_mut(&mut params).unwrap().len = 0;
/// ```
///
/// ```compile_fail,E0599
/// # use aterm_gpu::compute::{ComputeDevice, Job, Params, Pipeline};
/// # let dev = ComputeDevice::open().unwrap();
/// # let params = Params::new(&dev, aterm_effects::tone_int::v1().unwrap()).unwrap();
/// # let pipeline = Pipeline::build(&dev, &params).unwrap();
/// # let batch = aterm_effects::tone_int::pack(&params.view(), &["ok"]).unwrap();
/// # let mut job = Job::new(&dev, &params);
/// # job.load(&batch).unwrap();
/// // `load` on a completed job that was not recycled.
/// let mut done = job.submit(&pipeline).unwrap().wait().unwrap();
/// done.load(&batch).unwrap();
/// ```
///
/// ```compile_fail,E0277
/// # use aterm_gpu::compute::{ComputeDevice, Job, Params};
/// # let dev = ComputeDevice::open().unwrap();
/// # let params = Params::new(&dev, aterm_effects::tone_int::v1().unwrap()).unwrap();
/// // A job sent to another thread.
/// let job = Job::new(&dev, &params);
/// std::thread::spawn(move || drop(job));
/// ```
pub struct Job<S: JobState> {
    payload: ManuallyDrop<Box<Payload>>,
    _state: PhantomData<S>,
    _not_send: PhantomData<*const ()>,
}

impl<S: JobState> Job<S> {
    fn from_payload(payload: Box<Payload>) -> Job<S> {
        Job {
            payload: ManuallyDrop::new(payload),
            _state: PhantomData,
            _not_send: PhantomData,
        }
    }

    fn into_payload(self) -> Box<Payload> {
        let mut shell = ManuallyDrop::new(self);
        // SAFETY: `shell` is never dropped, and its payload is taken here once and never read again, so every field moves out exactly once.
        unsafe { ManuallyDrop::take(&mut shell.payload) }
    }

    /// The parameters the job reads.
    #[inline]
    pub fn params(&self) -> &Arc<Params> {
        &self.payload.params
    }
}

impl<S: JobState> fmt::Debug for Job<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job")
            .field("state", &S::NAME)
            .field("rows", &self.payload.loaded.map(|l| l.rows))
            .field("serial", &self.payload.flight.as_ref().map(|f| f.serial))
            .finish_non_exhaustive()
    }
}

impl<S: JobState> Drop for Job<S> {
    fn drop(&mut self) {
        // SAFETY: `drop` runs once, and nothing reads the payload after this take.
        let payload = unsafe { ManuallyDrop::take(&mut self.payload) };
        if S::IN_FLIGHT {
            let ledger = Arc::clone(payload.domain.ledger());
            if !matches!(settle(payload), Err(JobFailure::Hung)) {
                ledger.drained_drop();
            }
        }
    }
}

impl Job<Prepared> {
    /// A job on `dev` reading `params`, with no batch and no buffers yet.
    pub fn new(dev: &ComputeDevice, params: &Arc<Params>) -> Job<Prepared> {
        Job::from_payload(Box::new(Payload {
            domain: Rc::clone(&dev.domain),
            params: Arc::clone(params),
            slots: [None, None, None, None],
            loaded: None,
            flight: None,
            done: None,
        }))
    }

    /// Copy `batch` once into the job's shared buffers, making a buffer anew only where the one it has is too small, or larger than `batch` needs while keeping it would put the job over its limit or the domain over its own.
    pub fn load(&mut self, batch: &Batch) -> Result<(), LoadRefusal> {
        let p = &mut *self.payload;
        if let Some(reason) = p.domain.lost() {
            return Err(LoadRefusal::DomainLost(reason));
        }
        if !Arc::ptr_eq(p.params.ledger(), p.domain.ledger()) {
            return Err(LoadRefusal::OtherDomain);
        }
        if batch.dims() != p.params.dims {
            return Err(LoadRefusal::DimsMismatch {
                batch: batch.dims(),
                params: p.params.dims,
            });
        }
        let need = batch.bytes();
        let limits = p.domain.limits();
        if need.total() > limits.job {
            return Err(LoadRefusal::TooLarge(TooLarge {
                what: What::Job,
                bytes: need.total(),
                limit: limits.job,
            }));
        }
        let sizes = [need.job, need.offsets, need.ids, need.out];
        let (anew, after) = remade(
            p.slots.each_ref().map(|s| s.as_ref().map(Counted::len)),
            sizes,
            p.domain.ledger().live(),
            limits,
        );
        if after > limits.domain {
            return Err(LoadRefusal::TooLarge(TooLarge {
                what: What::Domain,
                bytes: after,
                limit: limits.domain,
            }));
        }
        p.loaded = None;
        // Release every buffer remade before allocating any, so the live bytes never pass `after`.
        for (slot, anew) in p.slots.iter_mut().zip(anew) {
            if anew {
                *slot = None;
            }
        }
        for ((slot, bytes), anew) in p.slots.iter_mut().zip(sizes).zip(anew) {
            if !anew {
                continue;
            }
            *slot = Some(p.domain.allocate(bytes).map_err(|e| match e {
                AllocError::TooLarge(t) => LoadRefusal::TooLarge(t),
                AllocError::Failed { bytes } => LoadRefusal::AllocFailed { bytes },
            })?);
        }
        let [Some(job), Some(offsets), Some(ids), Some(out)] = &p.slots else {
            return Err(LoadRefusal::AllocFailed { bytes: 0 });
        };
        let unmapped = |b: &Counted| LoadRefusal::AllocFailed { bytes: b.len() };
        // SAFETY (all four): each buffer was made at least as large as `batch.bytes()` says its contents are; a prepared job's buffers are referenced by no command buffer still running (fresh, recycled from a completed one, or refused before commit), and these four views are of distinct buffers and end with this call.
        unsafe {
            view_mut::<u32>(job, JOB_WORDS)
                .ok_or_else(|| unmapped(job))?
                .copy_from_slice(&batch.header());
            view_mut::<u32>(offsets, batch.offsets().len())
                .ok_or_else(|| unmapped(offsets))?
                .copy_from_slice(batch.offsets());
            view_mut::<u16>(ids, batch.ids().len())
                .ok_or_else(|| unmapped(ids))?
                .copy_from_slice(batch.ids());
            view_mut::<u32>(out, batch.out_words())
                .ok_or_else(|| unmapped(out))?
                .fill(OUT_POISON);
        }
        p.loaded = Some(Loaded {
            rows: batch.rows(),
            total_ids: batch.total_ids(),
            stride: batch.out_stride(),
        });
        Ok(())
    }

    /// Encode the loaded batch on `pipeline` and commit it, or give the job back with the reason.
    pub fn submit(
        self,
        pipeline: &Pipeline,
    ) -> Result<Job<InFlight>, (Job<Prepared>, SubmitRefusal)> {
        self.submit_with(pipeline, true)
    }

    /// [`Self::submit`] for qualification, which runs before the pipeline is qualified.
    pub(super) fn submit_qualifying(
        self,
        pipeline: &Pipeline,
    ) -> Result<Job<InFlight>, (Job<Prepared>, SubmitRefusal)> {
        self.submit_with(pipeline, false)
    }

    fn refusal(&self, pipeline: &Pipeline, need_qualified: bool) -> Option<SubmitRefusal> {
        let p = &*self.payload;
        if let Some(reason) = p.domain.lost() {
            return Some(SubmitRefusal::DomainLost(reason));
        }
        if !Rc::ptr_eq(pipeline.domain(), &p.domain) {
            return Some(SubmitRefusal::OtherDomain);
        }
        if need_qualified && !pipeline.is_qualified() {
            return Some(SubmitRefusal::Unqualified);
        }
        let params = Shape::of(&p.params);
        if params != pipeline.shape() {
            return Some(SubmitRefusal::DimsMismatch {
                pipeline: pipeline.shape(),
                params,
            });
        }
        p.loaded.is_none().then_some(SubmitRefusal::NotLoaded)
    }

    fn submit_with(
        self,
        pipeline: &Pipeline,
        need_qualified: bool,
    ) -> Result<Job<InFlight>, (Job<Prepared>, SubmitRefusal)> {
        if let Some(r) = self.refusal(pipeline, need_qualified) {
            return Err((self, r));
        }
        let mut payload = self.into_payload();
        match commit(&payload, pipeline) {
            Ok(flight) => {
                payload.flight = Some(flight);
                Ok(Job::from_payload(payload))
            }
            Err(r) => Err((Job::from_payload(payload), r)),
        }
    }
}

/// Which of a job's four slots a load makes anew for buffers of `need` bytes, given the lengths it `held`, and the domain's live bytes once it has: each slot missing or too small, then, while the four would sum over the job limit or the live bytes exceed the domain's, the largest kept slot larger than its need, which is remade at its need.
fn remade(held: [Option<u64>; 4], need: [u64; 4], live: u64, limits: Limits) -> ([bool; 4], u64) {
    let mut anew: [bool; 4] = std::array::from_fn(|i| held[i].is_none_or(|h| h < need[i]));
    loop {
        let (mut sum, mut freed, mut grow) = (0u64, 0u64, 0u64);
        for i in 0..4 {
            if anew[i] {
                (sum, freed, grow) = (sum + need[i], freed + held[i].unwrap_or(0), grow + need[i]);
            } else {
                sum += held[i].unwrap_or(0);
            }
        }
        let after = live.saturating_sub(freed).saturating_add(grow);
        let over = sum > limits.job || after > limits.domain;
        let shrink = (0..4)
            .filter(|&i| !anew[i] && held[i].is_some_and(|h| h > need[i]))
            .max_by_key(|&i| held[i]);
        match shrink {
            Some(i) if over => anew[i] = true,
            _ => return (anew, after),
        }
    }
}

/// Encode `p`'s loaded batch on `pipeline`, register both handlers and commit.
fn commit(p: &Payload, pipeline: &Pipeline) -> Result<Flight, SubmitRefusal> {
    let (Some(loaded), [Some(job), Some(offsets), Some(ids), Some(out)]) = (p.loaded, &p.slots)
    else {
        return Err(SubmitRefusal::NotLoaded);
    };
    let before_encode = ffi::mach_absolute_time();
    let _pool = AutoreleasePool::new();
    let cb = ffi::command_buffer(p.domain.queue()).ok_or(SubmitRefusal::NoCommandBuffer)?;
    // SAFETY: `cb` was just made, so it is live, uncommitted and has no encoder.
    let enc = unsafe { ComputeEncoder::open(cb.id()) }.ok_or(SubmitRefusal::NoEncoder)?;
    enc.set_pipeline(pipeline.pso());
    for (buf, binding) in [job, offsets, ids, out].into_iter().zip(BINDINGS) {
        enc.set_buffer(buf.obj(), binding);
    }
    enc.set_buffer(p.params.obj(), PARAMS_BINDING);
    let width = pipeline.threadgroup_width() as usize;
    enc.dispatch_threadgroups(
        ffi::MtlSize {
            width: (loaded.rows as usize).div_ceil(width),
            height: 1,
            depth: 1,
        },
        ffi::MtlSize {
            width,
            height: 1,
            depth: 1,
        },
    );
    enc.end();
    let completion = Arc::new(Completion::default());
    completion.prime();
    let scheduled = Arc::clone(&completion);
    let completed = Arc::clone(&completion);
    if !on_event(&cb, CbEvent::Scheduled, move |_| scheduled.on_scheduled())
        || !on_event(&cb, CbEvent::Completed, move |done| {
            completed.on_completed(done);
        })
    {
        return Err(SubmitRefusal::NoHandler);
    }
    let (serial, fault) = p.domain.mint_serial();
    #[cfg(debug_assertions)]
    if fault == Some(Fault::Hang) {
        completion.withhold();
    }
    ffi::commit(&cb);
    Ok(Flight {
        _cb: cb,
        completion,
        serial,
        fault,
        before_encode,
        after_commit: ffi::mach_absolute_time(),
    })
}

/// Wait for `p`'s flight, classify its terminal status, feed the latch, and release or (when hung) leak its buffers.
fn settle(mut p: Box<Payload>) -> Result<Box<Payload>, JobFailure> {
    let Some(flight) = p.flight.take() else {
        return Err(JobFailure::Lost(
            "a job in flight held no command buffer".to_owned(),
        ));
    };
    let (scheduled, terminal) = flight.completion.wait(p.domain.hang_detector());
    let woke = ffi::mach_absolute_time();
    let Some(t) = terminal else {
        #[cfg(debug_assertions)]
        if flight.fault == Some(Fault::Hang) {
            flight.completion.wait_withheld(HANG_DETECTOR);
        }
        return Err(hang(p, flight));
    };
    let outcome = injected(flight.fault, terminal_outcome(t.status, t.code));
    p.domain.latch().record(&outcome);
    match outcome {
        CbOutcome::Completed => {
            p.done = Some(Done {
                serial: flight.serial,
                times: JobTimes {
                    timebase: p.domain.timebase().map(Timebase::from),
                    before_encode: flight.before_encode,
                    after_commit: flight.after_commit,
                    scheduled,
                    gpu_start: t.gpu_start,
                    gpu_end: t.gpu_end,
                    handler: t.at,
                    woke,
                },
            });
            Ok(p)
        }
        CbOutcome::Retryable { code, name } => Err(JobFailure::Retryable { code, name }),
        CbOutcome::Lost { .. } => Err(JobFailure::Lost(format!("{outcome:?}"))),
        CbOutcome::Unfinished { .. } => {
            unreachable!("terminal_outcome reads an unfinished status as Lost")
        }
    }
}

/// What a completed handler's `status` and error `code` mean for a job: [`loss::classify`]'s outcome, except that a status not yet terminal there is a loss, a defect.
pub(super) const fn terminal_outcome(status: usize, code: Option<isize>) -> CbOutcome {
    match loss::classify(status, code) {
        CbOutcome::Unfinished { .. } => CbOutcome::Lost {
            code: None,
            name: "compute job unfinished at its completed handler",
        },
        o => o,
    }
}

/// The observation an injected fault puts in place of `real`, after the real terminal status.
#[cfg(debug_assertions)]
fn injected(fault: Option<InjectedFault>, real: CbOutcome) -> CbOutcome {
    match fault {
        Some(Fault::Lost) => CbOutcome::Lost {
            code: None,
            name: "injected compute loss",
        },
        Some(Fault::Retryable) => CbOutcome::Retryable {
            code: loss::ERROR_TIMEOUT,
            name: "injected compute timeout",
        },
        Some(Fault::Hang) | None => real,
    }
}

/// The observation an injected fault puts in place of `real`: a release build injects none.
#[cfg(not(debug_assertions))]
fn injected(fault: Option<InjectedFault>, real: CbOutcome) -> CbOutcome {
    match fault {
        Some(never) => match never {},
        None => real,
    }
}

/// A job with no terminal status: latch the domain, drop the command buffer handle and the completion, and leak the buffers and the params clone, counted.
fn hang(p: Box<Payload>, flight: Flight) -> JobFailure {
    let Payload {
        domain,
        params,
        slots,
        ..
    } = *p;
    domain.latch().record(&CbOutcome::Lost {
        code: None,
        name: "compute job hung",
    });
    let bytes = slots.iter().flatten().map(Counted::len).sum();
    domain.ledger().leak(bytes);
    std::mem::forget(slots);
    std::mem::forget(params);
    drop(flight);
    JobFailure::Hung
}

impl Job<InFlight> {
    /// Block until the job reaches a terminal status or [`HANG_DETECTOR`] expires.
    pub fn wait(self) -> Result<Job<Completed>, JobFailure> {
        settle(self.into_payload()).map(Job::from_payload)
    }

    /// The serial the job was committed with.
    #[inline]
    pub fn serial(&self) -> u64 {
        self.payload.flight.as_ref().map_or(0, |f| f.serial)
    }
}

impl Job<Completed> {
    fn loaded(&self) -> Loaded {
        self.payload.loaded.unwrap_or(Loaded {
            rows: 0,
            total_ids: 0,
            stride: 0,
        })
    }

    fn slot(&self, i: usize) -> Option<&Counted> {
        self.payload.slots[i].as_ref()
    }

    /// Every row's out words, `L[0..C]` as `i32` bits then the class, read in place from the buffer the GPU wrote.
    pub fn out(&self) -> &[u32] {
        let l = self.loaded();
        let Some(buf) = self.slot(3) else { return &[] };
        // SAFETY: `out` holds ROWS × OUT_STRIDE words (`load` made it so); the job's command buffer completed, so every GPU write landed and no GPU work references it, and a completed job has no `&mut` method.
        unsafe { view::<u32>(buf, l.rows as usize * l.stride as usize) }
    }

    /// The `job` header as the GPU read it.
    pub fn header(&self) -> &[u32] {
        let Some(buf) = self.slot(0) else { return &[] };
        // SAFETY: as `out`, for the header's `JOB_WORDS` words.
        unsafe { view::<u32>(buf, JOB_WORDS) }
    }

    /// The CSR row starts as the GPU read them.
    pub fn offsets(&self) -> &[u32] {
        let l = self.loaded();
        let Some(buf) = self.slot(1) else { return &[] };
        // SAFETY: as `out`, for ROWS + 1 words.
        unsafe { view::<u32>(buf, l.rows as usize + 1) }
    }

    /// The ids as the GPU read them.
    pub fn ids(&self) -> &[u16] {
        let l = self.loaded();
        let Some(buf) = self.slot(2) else { return &[] };
        // SAFETY: as `out`, for TOTAL_IDS halfwords.
        unsafe { view::<u16>(buf, l.total_ids as usize) }
    }

    /// ROWS.
    #[inline]
    pub fn rows(&self) -> u32 {
        self.loaded().rows
    }

    /// The serial the job was committed with.
    #[inline]
    pub fn serial(&self) -> u64 {
        self.payload.done.map_or(0, |d| d.serial)
    }

    /// The job's stamps.
    #[inline]
    pub fn times(&self) -> Option<JobTimes> {
        self.payload.done.map(|d| d.times)
    }

    /// Give the buffers back to the CPU for another batch within their capacity.
    pub fn recycle(self) -> Job<Prepared> {
        let mut p = self.into_payload();
        p.loaded = None;
        p.done = None;
        Job::from_payload(p)
    }
}
