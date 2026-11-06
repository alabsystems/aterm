// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright 2026 Andrew Yates

//! Join local GNU tokens to worker-owned global CPU leases.

use std::io;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{ExternalDebt, Lease, LegacyFence, Owner, Pool, invalid};

/// A local jobserver with one already-owned implicit global credit.
///
/// The caller must borrow an inherited implicit credit for synchronous nested
/// work, rather than reacquiring one while its parent waits. This constructor
/// deliberately cannot allocate an implicit credit or invent descendant custody.
#[derive(Clone)]
pub struct WorkerClient {
    local: jobserver::Client,
    pool: Pool,
    shared: Option<Arc<ExternalDebt>>,
    implicit: Arc<Lease>,
    limit: Arc<dyn Fn(u32) -> u32 + Send + Sync>,
    owner: Owner,
    fence: Option<Arc<LegacyFence>>,
}

impl WorkerClient {
    /// Bind an existing local jobserver and implicit global lease to a live
    /// observation callback. A zero observation pauses extra admission.
    /// The callback must be bounded/nonblocking (read a cached observation).
    pub fn new(
        local: jobserver::Client,
        pool: Pool,
        implicit: Arc<Lease>,
        limit: Arc<dyn Fn(u32) -> u32 + Send + Sync>,
        owner: Owner,
    ) -> io::Result<Self> {
        // A named FIFO provides independent nonblocking descriptions on
        // macOS. Never mutate an inherited anonymous pipe's blocking mode.
        // Dropping a probe immediately returns any token it acquired.
        drop(local.try_acquire()?);
        if owner.pid == 0 || !pool.contains(&implicit)? {
            return Err(invalid("worker implicit credit does not belong to its CPU pool"));
        }
        Ok(Self { local, pool, shared: None, implicit, limit, owner, fence: None })
    }

    /// Join local tokens to the activated native/external transaction. Retained
    /// external debt remains unavailable after its owner's descriptors close.
    /// The implicit lease must belong to this exact checked native backing.
    pub fn new_shared(
        local: jobserver::Client,
        debt: Arc<ExternalDebt>,
        implicit: Arc<Lease>,
        limit: Arc<dyn Fn(u32) -> u32 + Send + Sync>,
        owner: Owner,
    ) -> io::Result<Self> {
        let mut client = Self::new(local, debt.native_pool()?, implicit, limit, owner)?;
        client.shared = Some(debt);
        Ok(client)
    }

    /// Retain the native epoch's legacy exclusion in every issued credit.
    /// Production native-epoch callers must supply their validated fence.
    pub fn with_fence(mut self, fence: Arc<LegacyFence>) -> Self {
        self.fence = Some(fence);
        self
    }

    /// The actual implicit credit. A checked child launcher can clone this
    /// lease and inherit its descriptor; a path/PID is not a substitute.
    pub fn implicit(&self) -> &Arc<Lease> {
        &self.implicit
    }

    /// The epoch fence retained by this client's implicit worker.
    pub fn fence(&self) -> Option<&Arc<LegacyFence>> {
        self.fence.as_ref()
    }

    /// Acquire both credits, or neither, without waiting on either semaphore.
    /// A global refusal immediately returns the local token, preventing the
    /// hold-and-wait deadlock between independent pools and nested roots.
    pub fn try_acquire(&self) -> io::Result<Option<Credit>> {
        let Some(local) = self.local.try_acquire()? else { return Ok(None) };
        let cpu = match &self.shared {
            Some(debt) => debt.try_acquire_observed(self.owner, |held| (self.limit)(held))?,
            None => self.pool.try_acquire_observed(self.owner, |held| (self.limit)(held))?,
        };
        let Some(cpu) = cpu else {
            return Ok(None);
        };
        Ok(Some(Credit {
            _local: local,
            cpu: Arc::new(cpu),
            ticket: Arc::new(AtomicU8::new(OFFERED)),
            fence: self.fence.clone(),
        }))
    }

    /// Handle requests for runnable workers. Do not use this to prefetch CPU
    /// leases for queued work which is not yet eligible to execute.
    pub fn into_helper_thread(
        self,
        mut callback: impl FnMut(io::Result<Credit>) + Send + 'static,
    ) -> io::Result<WorkerHelper> {
        let state = Arc::new((Mutex::new(Requests::default()), Condvar::new()));
        let worker_state = Arc::clone(&state);
        let thread =
            thread::Builder::new().name("trust-cpu-admission".into()).spawn(move || {
                loop {
                    let (lock, wake) = &*worker_state;
                    let mut pending =
                        lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    while pending.count == 0 && !pending.stop {
                        pending =
                            wake.wait(pending).unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    if pending.stop {
                        return;
                    }
                    drop(pending);
                    let result = match self.try_acquire() {
                        Ok(None) => {
                            let pending =
                                lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            if pending.stop {
                                return;
                            }
                            // Bounded polling also observes cross-process releases
                            // and new cached telemetry without a GUI dependency.
                            drop(wake.wait_timeout(pending, Duration::from_millis(10)));
                            continue;
                        }
                        Ok(Some(credit)) => Ok(credit),
                        Err(error) => Err(error),
                    };
                    let mut pending =
                        lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if pending.stop {
                        return;
                    }
                    pending.count -= 1;
                    if let Ok(credit) = &result {
                        pending.tickets.retain(|ticket| ticket.strong_count() != 0);
                        pending.tickets.push(Arc::downgrade(&credit.ticket));
                    }
                    drop(pending);
                    callback(result);
                }
            })?;
        Ok(WorkerHelper { state, thread: Some(thread) })
    }
}

/// One runnable worker's inseparable local token and global CPU lease.
/// Dropping the local token never unlocks a descendant's duplicated CPU lease.
#[derive(Debug)]
pub struct Credit {
    // Keep the local token alive until this entire value is dropped.
    _local: jobserver::Acquired,
    cpu: Arc<Lease>,
    ticket: Arc<AtomicU8>,
    fence: Option<Arc<LegacyFence>>,
}

const OFFERED: u8 = 0;
const COMMITTED: u8 = 1;
const CANCELLED: u8 = 2;

impl Credit {
    /// Whether this credit was already committed to a prior worker dispatch.
    pub fn is_committed(&self) -> bool {
        self.ticket.load(Ordering::Acquire) == COMMITTED
    }

    /// Commit this offered credit once, immediately before dispatching work.
    /// A helper cancellation that wins this race refuses the dispatch. A
    /// committed worker retains its backing until it and its children finish.
    pub fn commit(&mut self) -> bool {
        self.ticket
            .compare_exchange(OFFERED, COMMITTED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// The worker's actual CPU backing for a checked child handoff.
    pub fn cpu(&self) -> &Arc<Lease> {
        &self.cpu
    }

    /// The actual epoch fence carried by this credit, if one was supplied.
    pub fn fence(&self) -> Option<&Arc<LegacyFence>> {
        self.fence.as_ref()
    }
}

#[derive(Default)]
struct Requests {
    count: usize,
    stop: bool,
    tickets: Vec<Weak<AtomicU8>>,
}

/// Cancellable admission helper; active credits live independently of it.
pub struct WorkerHelper {
    state: Arc<(Mutex<Requests>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl WorkerHelper {
    /// Request one credit for work that is ready to run.
    pub fn request_token(&self) {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.stop {
            return;
        }
        state.count = state.count.saturating_add(1);
        wake.notify_one();
    }

    /// Stop pending admission and revoke offered, uncommitted dispatches.
    /// Already committed work is not revoked and keeps its leases alive.
    pub fn cancel(&self) {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.stop = true;
        for ticket in state.tickets.iter().filter_map(Weak::upgrade) {
            let _ =
                ticket.compare_exchange(OFFERED, CANCELLED, Ordering::AcqRel, Ordering::Acquire);
        }
        wake.notify_one();
    }
}

impl Drop for WorkerHelper {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests;
