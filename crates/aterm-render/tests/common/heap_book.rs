// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Platform-independent accounting for the Windows heap profiler. Addresses are opaque identities;
//! this book never dereferences them or performs the allocator operation it observes.

use std::cell::Cell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard, OnceLock};

pub(crate) const MIN_TRACK: usize = 512;
pub(crate) const FRAMES: usize = 16;

thread_local! {
    static BUSY: Cell<usize> = const { Cell::new(0) };
}

/// Suppress attribution of allocations made by the attribution machinery itself. The guard and
/// references to it stay on their originating thread, whose TLS suppression depth they control.
#[must_use = "attribution suppression ends when the guard drops"]
pub(crate) struct Busy {
    _thread: PhantomData<Rc<()>>,
}

impl Busy {
    pub(crate) fn enter() -> Option<Self> {
        match BUSY.try_with(|depth| {
            if depth.get() != 0 {
                return false;
            }
            depth.set(1);
            true
        }) {
            Ok(true) => Some(Self {
                _thread: PhantomData,
            }),
            _ => None,
        }
    }

    pub(crate) fn force() -> Self {
        let _ = BUSY.try_with(|depth| depth.set(depth.get().saturating_add(1)));
        Self {
            _thread: PhantomData,
        }
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        let _ = BUSY.try_with(|depth| {
            let current = depth.get();
            // Saturation stays suppressed instead of overflowing back into recursive attribution.
            if current != usize::MAX {
                depth.set(current.saturating_sub(1));
            }
        });
    }
}

pub(crate) struct Stack {
    pub(crate) key: u64,
    pub(crate) frames: [usize; FRAMES],
    pub(crate) depth: usize,
}

#[derive(Default)]
pub(crate) struct Site {
    pub(crate) frames: [usize; FRAMES],
    pub(crate) depth: usize,
    pub(crate) live: i64,
    pub(crate) live_count: i64,
    pub(crate) total: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) size: usize,
    pub(crate) site: u32,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) live: HashMap<usize, Record>,
    by_stack: HashMap<u64, u32>,
    pub(crate) sites: Vec<Site>,
    pending: usize,
}

impl State {
    pub(crate) fn allocation_count(&self) -> usize {
        self.live.len().saturating_add(self.pending)
    }

    fn retire(&mut self, record: Record) {
        if let Some(site) = self.sites.get_mut(record.site as usize) {
            site.live = site.live.wrapping_sub(record.size as i64);
            site.live_count = site.live_count.wrapping_sub(1);
        }
    }
}

pub(crate) struct Book {
    pub(crate) live_bytes: AtomicI64,
    pub(crate) peak_bytes: AtomicI64,
    pub(crate) live_count: AtomicI64,
    pub(crate) total_allocs: AtomicU64,
    state: OnceLock<Mutex<State>>,
}

impl Book {
    pub(crate) const fn new() -> Self {
        Self {
            live_bytes: AtomicI64::new(0),
            peak_bytes: AtomicI64::new(0),
            live_count: AtomicI64::new(0),
            total_allocs: AtomicU64::new(0),
            state: OnceLock::new(),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .get_or_init(|| Mutex::new(State::default()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Count a successful allocation, including a successful realloc's replacement block.
    pub(crate) fn allocated(&self, ptr: *mut u8, size: usize) {
        if ptr.is_null() {
            return;
        }
        let live = self
            .live_bytes
            .fetch_add(size as i64, Relaxed)
            .wrapping_add(size as i64);
        self.live_count.fetch_add(1, Relaxed);
        self.total_allocs.fetch_add(1, Relaxed);
        self.peak_bytes.fetch_max(live, Relaxed);
    }

    /// Attach a captured stack while recursion is suppressed by the caller's guard.
    pub(crate) fn attribute(&self, ptr: *mut u8, size: usize, stack: Stack, _busy: &Busy) {
        if ptr.is_null() || size < MIN_TRACK {
            return;
        }
        let mut state = self.lock();
        let index = match state.by_stack.get(&stack.key) {
            Some(index) => *index,
            None => {
                state.sites.push(Site {
                    frames: stack.frames,
                    depth: stack.depth,
                    ..Site::default()
                });
                let index = u32::try_from(state.sites.len() - 1).unwrap_or(u32::MAX);
                state.by_stack.insert(stack.key, index);
                index
            }
        };
        if let Some(site) = state.sites.get_mut(index as usize) {
            site.live = site.live.wrapping_add(size as i64);
            site.live_count = site.live_count.wrapping_add(1);
            site.total = site.total.wrapping_add(size as u64);
        }
        state
            .live
            .insert(ptr as usize, Record { size, site: index });
    }

    /// Ordinary deallocation is booked before the native allocator can release the address.
    pub(crate) fn deallocated(&self, ptr: *mut u8, size: usize) {
        if ptr.is_null() {
            return;
        }
        self.live_bytes.fetch_sub(size as i64, Relaxed);
        self.live_count.fetch_sub(1, Relaxed);
        if size >= MIN_TRACK
            && let Some(_busy) = Busy::enter()
        {
            let mut state = self.lock();
            if let Some(record) = state.live.remove(&(ptr as usize)) {
                state.retire(record);
            }
        }
    }

    /// Observe one native realloc. The caller books the non-null replacement as an allocation.
    /// The native callback runs with neither the attribution mutex nor a recursion guard held.
    /// Failure leaves bytes, count, peak and stack attribution unchanged; unwinding restores the
    /// detached attribution too (the real global allocator's native callback never unwinds).
    pub(crate) fn reallocated(
        &self,
        ptr: *mut u8,
        size: usize,
        native: impl FnOnce() -> *mut u8,
    ) -> *mut u8 {
        let previous = if size >= MIN_TRACK
            && let Some(_busy) = Busy::enter()
        {
            let mut state = self.lock();
            let previous = state.live.remove(&(ptr as usize));
            if previous.is_some() {
                state.pending = state.pending.wrapping_add(1);
            }
            previous
        } else {
            None
        };
        let mut pending = Pending {
            book: self,
            ptr: ptr as usize,
            previous,
        };
        let next = native();
        if !next.is_null() {
            self.live_bytes.fetch_sub(size as i64, Relaxed);
            self.live_count.fetch_sub(1, Relaxed);
            if let Some(record) = pending.previous.take() {
                let _busy = Busy::force();
                // Retire the captured owner, never whatever may now occupy the old address.
                let mut state = self.lock();
                state.pending = state.pending.wrapping_sub(1);
                state.retire(record);
            }
        }
        next
    }
}

struct Pending<'a> {
    book: &'a Book,
    ptr: usize,
    previous: Option<Record>,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if let Some(record) = self.previous.take() {
            let _busy = Busy::force();
            let mut state = self.book.lock();
            state.pending = state.pending.wrapping_sub(1);
            state.live.insert(self.ptr, record);
        }
    }
}
