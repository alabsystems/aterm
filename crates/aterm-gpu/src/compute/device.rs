// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The compute device: its own queue, loss latch, byte limits and live-byte ledger (design §12.5).

use std::cell::Cell;
#[cfg(debug_assertions)]
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aterm_effects::int_bag_mlp::{DOMAIN_BYTES_MAX, JOB_BYTES_MAX, PARAMS_BYTES_MAX};

use super::job::HANG_DETECTOR;
use crate::metal::ffi::{self, Device, MachTimebase, Obj};
use crate::metal::loss::LossLatch;
use crate::metal::resources::SharedBuffer;

/// Why [`ComputeDevice::open`] refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The device listing names no GPU this process may compute on.
    NoDevice,
    /// The GPU's shared storage is not the memory the CPU writes.
    NotUnified {
        /// The GPU's name.
        name: String,
    },
    /// Metal made no command queue.
    NoQueue,
}

/// Which byte limit a refusal names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    /// One params blob: [`PARAMS_BYTES_MAX`] or `maxBufferLength`, whichever is smaller.
    Params,
    /// One job's four buffers, summed: [`JOB_BYTES_MAX`] or `maxBufferLength`, whichever is smaller.
    Job,
    /// Every live buffer of the domain: [`DOMAIN_BYTES_MAX`] or `recommendedMaxWorkingSetSize`, whichever is smaller.
    Domain,
}

/// A refusal before any allocation: `bytes` would exceed `limit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLarge {
    /// The limit.
    pub what: What,
    /// The bytes asked for (for [`What::Domain`], the live bytes they would make).
    pub bytes: u64,
    /// The limit in force.
    pub limit: u64,
}

/// The byte limits in force, each the smaller of its declared budget and the device's limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The device's `maxBufferLength`, or a smaller injected one.
    pub max_buffer_length: u64,
    /// The device's `recommendedMaxWorkingSetSize`, or a smaller injected one.
    pub working_set: u64,
    /// One params blob.
    pub params: u64,
    /// One job's four buffers, summed.
    pub job: u64,
    /// Every live buffer of the domain.
    pub domain: u64,
}

/// What the domain leaked on purpose after hung jobs, all of it still counted live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Leaks {
    /// Hung jobs.
    pub jobs: u64,
    /// Their four buffers' bytes.
    pub bytes: u64,
    /// Their `Arc<Params>` clones.
    pub params_clones: u64,
}

/// A fault the waiter injects for one job serial, after the job's real terminal status (debug builds only).
#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The completed handler withholds its reading, so the waiter's own detector expires; the job is then declared hung once the withheld reading arrives.
    Hang,
    /// The job reads as lost.
    Lost,
    /// The job reads as a retryable failure.
    Retryable,
}

/// Every live buffer of one domain, counted by byte, and what it leaked.
#[derive(Debug, Default)]
pub(super) struct Ledger {
    live: AtomicU64,
    leaked_jobs: AtomicU64,
    leaked_bytes: AtomicU64,
    leaked_params: AtomicU64,
    #[cfg(any(test, debug_assertions))]
    created: AtomicU64,
    #[cfg(any(test, debug_assertions))]
    drained_drops: AtomicU64,
}

impl Ledger {
    /// Count `bytes` more live unless that makes more than `limit`.
    fn reserve(&self, bytes: u64, limit: u64) -> Result<(), TooLarge> {
        self.live
            .try_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                live.checked_add(bytes).filter(|&n| n <= limit)
            })
            .map(drop)
            .map_err(|live| TooLarge {
                what: What::Domain,
                bytes: live.saturating_add(bytes),
                limit,
            })
    }

    fn release(&self, bytes: u64) {
        self.live.fetch_sub(bytes, Ordering::AcqRel);
    }

    /// The bytes counted live now.
    #[inline]
    pub(super) fn live(&self) -> u64 {
        self.live.load(Ordering::Acquire)
    }

    /// Record one hung job's leak: its buffers' `bytes` and its params clone stay counted.
    pub(super) fn leak(&self, bytes: u64) {
        self.leaked_jobs.fetch_add(1, Ordering::Relaxed);
        self.leaked_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.leaked_params.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a dropped in-flight job that waited for its terminal status.
    pub(super) fn drained_drop(&self) {
        #[cfg(any(test, debug_assertions))]
        self.drained_drops.fetch_add(1, Ordering::Relaxed);
    }
}

/// A shared-storage buffer counted live in its domain's ledger until it drops.
#[derive(Debug)]
pub(super) struct Counted {
    buf: SharedBuffer,
    bytes: u64,
    ledger: Arc<Ledger>,
}

impl Counted {
    /// The buffer.
    #[inline]
    pub(super) const fn obj(&self) -> &Obj {
        self.buf.obj()
    }

    /// Its length in bytes, at least 4.
    #[inline]
    pub(super) const fn len(&self) -> u64 {
        self.bytes
    }

    /// The ledger it is counted in, which names its domain.
    #[inline]
    pub(super) const fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.ledger.release(self.bytes);
    }
}

/// Why the domain made no buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AllocError {
    /// The domain's limit.
    TooLarge(TooLarge),
    /// Metal returned no buffer.
    Failed {
        /// The bytes asked for.
        bytes: u64,
    },
}

/// The device limits read at open.
#[derive(Clone, Copy, Debug)]
struct DeviceLimits {
    max_buffer_length: u64,
    working_set: u64,
}

/// One compute loss domain: a device, its own queue and latch, its limits and its ledger.
#[derive(Debug)]
pub(super) struct Domain {
    dev: Device,
    queue: Obj,
    latch: LossLatch,
    ledger: Arc<Ledger>,
    device_limits: Cell<DeviceLimits>,
    timebase: Option<MachTimebase>,
    serial: Cell<u64>,
    #[cfg(debug_assertions)]
    faults: RefCell<Vec<(u64, Fault)>>,
    #[cfg(debug_assertions)]
    hang_detector: Cell<Duration>,
}

impl Domain {
    /// `mach_timebase_info`, read at open.
    #[inline]
    pub(super) const fn timebase(&self) -> Option<MachTimebase> {
        self.timebase
    }

    /// The command queue every job of the domain commits on.
    #[inline]
    pub(super) const fn queue(&self) -> &Obj {
        &self.queue
    }

    /// The domain's loss latch, bound to no `EncodeSession`.
    #[inline]
    pub(super) const fn latch(&self) -> &LossLatch {
        &self.latch
    }

    /// The domain's ledger.
    #[inline]
    pub(super) const fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    /// The device.
    #[inline]
    pub(super) const fn device(&self) -> &Device {
        &self.dev
    }

    /// The limits in force.
    pub(super) fn limits(&self) -> Limits {
        let d = self.device_limits.get();
        Limits {
            max_buffer_length: d.max_buffer_length,
            working_set: d.working_set,
            params: PARAMS_BYTES_MAX.min(d.max_buffer_length),
            job: JOB_BYTES_MAX.min(d.max_buffer_length),
            domain: DOMAIN_BYTES_MAX.min(d.working_set),
        }
    }

    /// How long a wait gives each of the domain's jobs: [`HANG_DETECTOR`], or a shorter one a debug build's test set.
    #[inline]
    pub(super) fn hang_detector(&self) -> Duration {
        #[cfg(debug_assertions)]
        {
            self.hang_detector.get()
        }
        #[cfg(not(debug_assertions))]
        {
            HANG_DETECTOR
        }
    }

    /// The first loss recorded, if the domain is latched.
    #[inline]
    pub(super) fn lost(&self) -> Option<String> {
        self.latch.reason().map(str::to_owned)
    }

    /// A shared-storage buffer of `bytes` bytes (at least 4), counted live, or a refusal before allocation.
    pub(super) fn allocate(&self, bytes: u64) -> Result<Counted, AllocError> {
        let bytes = bytes.max(4);
        self.ledger
            .reserve(bytes, self.limits().domain)
            .map_err(AllocError::TooLarge)?;
        let Some(obj) = usize::try_from(bytes)
            .ok()
            .and_then(|len| self.dev.new_buffer(len))
        else {
            self.ledger.release(bytes);
            return Err(AllocError::Failed { bytes });
        };
        #[cfg(any(test, debug_assertions))]
        self.ledger.created.fetch_add(1, Ordering::Relaxed);
        Ok(Counted {
            buf: SharedBuffer::new(obj),
            bytes,
            ledger: Arc::clone(&self.ledger),
        })
    }

    /// The serial the next committed job carries, and the fault armed for it.
    pub(super) fn mint_serial(&self) -> (u64, Option<InjectedFault>) {
        let serial = self.serial.get();
        self.serial.set(serial + 1);
        #[cfg(debug_assertions)]
        {
            let mut faults = self.faults.borrow_mut();
            let fault = faults
                .iter()
                .position(|&(at, _)| at == serial)
                .map(|i| faults.swap_remove(i).1);
            (serial, fault)
        }
        #[cfg(not(debug_assertions))]
        (serial, None)
    }
}

/// The fault a job carries: [`Fault`] in a debug build, nothing in a release one.
#[cfg(debug_assertions)]
pub(super) type InjectedFault = Fault;
/// The fault a job carries: [`Fault`] in a debug build, nothing in a release one.
#[cfg(not(debug_assertions))]
pub(super) type InjectedFault = std::convert::Infallible;

/// A Metal GPU opened for compute jobs, with its own queue and loss latch; one thread owns it.
#[derive(Debug)]
pub struct ComputeDevice {
    pub(super) domain: Rc<Domain>,
}

impl ComputeDevice {
    /// Open the GPU the device listing names, never asking WindowServer, refusing one without unified memory.
    pub fn open() -> Result<ComputeDevice, OpenError> {
        let dev = Device::listed(false).ok_or(OpenError::NoDevice)?;
        if !dev.has_unified_memory() {
            return Err(OpenError::NotUnified { name: dev.name() });
        }
        let queue = dev.new_command_queue().ok_or(OpenError::NoQueue)?;
        let device_limits = DeviceLimits {
            max_buffer_length: dev.max_buffer_length() as u64,
            working_set: dev.recommended_max_working_set_size(),
        };
        Ok(ComputeDevice {
            domain: Rc::new(Domain {
                dev,
                queue,
                latch: LossLatch::new(),
                ledger: Arc::new(Ledger::default()),
                device_limits: Cell::new(device_limits),
                timebase: ffi::mach_timebase(),
                serial: Cell::new(0),
                #[cfg(debug_assertions)]
                faults: RefCell::new(Vec::new()),
                #[cfg(debug_assertions)]
                hang_detector: Cell::new(HANG_DETECTOR),
            }),
        })
    }

    /// The GPU's name.
    pub fn name(&self) -> String {
        self.domain.dev.name()
    }

    /// The GPU's `registryID`.
    pub fn registry_id(&self) -> u64 {
        self.domain.dev.registry_id()
    }

    /// The byte limits in force.
    pub fn limits(&self) -> Limits {
        self.domain.limits()
    }

    /// The bytes of every live buffer of the domain, leaked ones included.
    pub fn live_bytes(&self) -> u64 {
        self.domain.ledger.live()
    }

    /// What hung jobs leaked.
    pub fn leaks(&self) -> Leaks {
        let l = &self.domain.ledger;
        Leaks {
            jobs: l.leaked_jobs.load(Ordering::Relaxed),
            bytes: l.leaked_bytes.load(Ordering::Relaxed),
            params_clones: l.leaked_params.load(Ordering::Relaxed),
        }
    }

    /// Metal's `currentAllocatedSize` for this process's device.
    pub fn current_allocated_size(&self) -> u64 {
        self.domain.dev.current_allocated_size() as u64
    }

    /// The first loss the domain latched, or `None`.
    pub fn loss_reason(&self) -> Option<String> {
        self.domain.lost()
    }

    /// The serial the next committed job carries.
    pub fn next_serial(&self) -> u64 {
        self.domain.serial.get()
    }

    /// Lower the device's limits as seen by this domain, to drive a refusal no real limit reaches here (debug builds only).
    #[cfg(any(test, debug_assertions))]
    pub fn inject_device_limits(&self, max_buffer_length: Option<u64>, working_set: Option<u64>) {
        let mut d = self.domain.device_limits.get();
        if let Some(n) = max_buffer_length {
            d.max_buffer_length = d.max_buffer_length.min(n);
        }
        if let Some(n) = working_set {
            d.working_set = d.working_set.min(n);
        }
        self.domain.device_limits.set(d);
    }

    /// How many buffers the domain has created (debug builds only).
    #[cfg(any(test, debug_assertions))]
    pub fn buffers_created(&self) -> u64 {
        self.domain.ledger.created.load(Ordering::Relaxed)
    }

    /// How many in-flight jobs were dropped after their wait reached a terminal status (debug builds only).
    #[cfg(any(test, debug_assertions))]
    pub fn drained_drops(&self) -> u64 {
        self.domain.ledger.drained_drops.load(Ordering::Relaxed)
    }

    /// Arm `fault` for the job committed with `serial` (debug builds only).
    #[cfg(debug_assertions)]
    pub fn arm_fault(&self, fault: Fault, serial: u64) {
        self.domain.faults.borrow_mut().push((serial, fault));
    }

    /// Shorten the hang detector this domain's waits give each job, never past [`HANG_DETECTOR`] (debug builds only).
    #[cfg(debug_assertions)]
    pub fn shorten_hang_detector(&self, detector: Duration) {
        self.domain.hang_detector.set(detector.min(HANG_DETECTOR));
    }
}

/// Metal's `currentAllocatedSize`, read through a fresh device listing, so it shows what the process-wide `MTLDevice` still holds after every [`ComputeDevice`] is gone.
pub fn current_allocated_size_fresh() -> Option<u64> {
    Device::listed(false).map(|d| d.current_allocated_size() as u64)
}
