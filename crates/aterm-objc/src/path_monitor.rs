// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A long-lived watch on the path this Mac's traffic takes ([`PathMonitor`]), over
//! Network.framework's path monitor (`nw_path_monitor_t`): every update the monitor hands its
//! handler reaches a sink as a [`PathReading`] (`nw_path_get_status` and
//! `nw_path_is_constrained` of that path), on a private [`SerialQueue`].
//!
//! The monitor's first update is the path in use when it starts, and each later one a change
//! of it. Network.framework is not linked: its functions are opened at the first start
//! (`dlopen`, never closed) into one table, so a process that never starts a monitor loads
//! nothing more and the crate keeps its zero dependencies.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;

use crate::{RcBlock, SerialQueue};

/// Network.framework's binary.
const NETWORK: &CStr = c"/System/Library/Frameworks/Network.framework/Network";

/// `<dlfcn.h>`'s `RTLD_LAZY` on Darwin.
const RTLD_LAZY: c_int = 0x1;

// SAFETY: libSystem's `dlopen` and `dlsym`, with `<dlfcn.h>`'s prototypes.
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// The Network.framework functions a monitor calls, with `<Network/path_monitor.h>`'s and
/// `<Network/path.h>`'s prototypes (every `nw_*_t` is an object pointer).
struct Network {
    create: unsafe extern "C" fn() -> *mut c_void,
    set_queue: unsafe extern "C" fn(*mut c_void, *mut c_void),
    set_update_handler: unsafe extern "C" fn(*mut c_void, *mut c_void),
    start: unsafe extern "C" fn(*mut c_void),
    cancel: unsafe extern "C" fn(*mut c_void),
    status: unsafe extern "C" fn(*mut c_void) -> c_int,
    constrained: unsafe extern "C" fn(*mut c_void) -> bool,
    release: unsafe extern "C" fn(*mut c_void),
}

/// The table, opened at the first [`Network::table`] and kept for the process's life (`None`
/// when the framework or one of its functions cannot be found).
static TABLE: OnceLock<Option<Network>> = OnceLock::new();

impl Network {
    /// The process's one table. Its initializer is [`Network::open`], which never comes back
    /// here.
    fn table() -> Option<&'static Self> {
        TABLE.get_or_init(Self::open).as_ref()
    }

    /// The framework's functions, or `None` when it or one of them cannot be found.
    fn open() -> Option<Self> {
        // SAFETY: a NUL-terminated path; `dlopen` of a system framework already in the shared
        // cache maps nothing new and the handle is kept for the process's life (never closed).
        let handle = unsafe { dlopen(NETWORK.as_ptr(), RTLD_LAZY) };
        if handle.is_null() {
            return None;
        }
        let symbol = |name: &CStr| {
            // SAFETY: a live handle and a NUL-terminated name.
            let found = unsafe { dlsym(handle, name.as_ptr()) };
            (!found.is_null()).then_some(found)
        };
        // SAFETY: each symbol is the framework's exported function of that name, whose C
        // prototype is the field's type; a function pointer and a data pointer share their
        // width on every Mac.
        unsafe {
            Some(Self {
                create: std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> *mut c_void>(
                    symbol(c"nw_path_monitor_create")?,
                ),
                set_queue: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*mut c_void, *mut c_void),
                >(symbol(c"nw_path_monitor_set_queue")?),
                set_update_handler: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*mut c_void, *mut c_void),
                >(symbol(
                    c"nw_path_monitor_set_update_handler",
                )?),
                start: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void)>(
                    symbol(c"nw_path_monitor_start")?,
                ),
                cancel: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void)>(
                    symbol(c"nw_path_monitor_cancel")?,
                ),
                status: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*mut c_void) -> c_int,
                >(symbol(c"nw_path_get_status")?),
                constrained: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*mut c_void) -> bool,
                >(symbol(c"nw_path_is_constrained")?),
                release: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void)>(
                    symbol(c"nw_release")?,
                ),
            })
        }
    }
}

/// A path's `nw_path_status_t` (`<Network/path.h>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStatus {
    /// `nw_path_status_invalid` (0): the path is not valid. Every value the header does not
    /// name reads as this too.
    Invalid,
    /// `nw_path_status_satisfied` (1): the path can carry traffic.
    Satisfied,
    /// `nw_path_status_unsatisfied` (2): the path cannot carry traffic.
    Unsatisfied,
    /// `nw_path_status_satisfiable` (3): the path cannot carry traffic now, but a connection
    /// attempt could make it (a VPN or a cellular link not yet brought up).
    Satisfiable,
}

impl PathStatus {
    /// The status `nw_path_get_status` returned as `raw`.
    #[must_use]
    pub const fn from_raw(raw: c_int) -> Self {
        match raw {
            1 => Self::Satisfied,
            2 => Self::Unsatisfied,
            3 => Self::Satisfiable,
            _ => Self::Invalid,
        }
    }
}

/// What one update of the monitor said about the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathReading {
    /// Whether the path can carry traffic (`nw_path_get_status`).
    pub status: PathStatus,
    /// Whether the path uses an interface in Low Data Mode (`nw_path_is_constrained`).
    pub constrained: bool,
}

/// A started Network.framework path monitor, which hands every update to its sink until it is
/// dropped.
///
/// Dropping it cancels the monitor and releases it, once. The cancel is asynchronous: an
/// update already submitted to the monitor's queue may still reach the sink after the drop
/// returns, which is why the sink is `'static` and must tolerate that one late call.
pub struct PathMonitor {
    /// The +1 `nw_path_monitor_t` `nw_path_monitor_create` returned, cancelled and released
    /// exactly once, in `Drop`.
    raw: NonNull<c_void>,
    /// The table its functions come from.
    network: &'static Network,
    /// The update handler, a block of `void (^)(nw_path_t)` (the monitor holds its own copy).
    handler: RcBlock,
    /// The queue the handler runs on (the monitor holds its own reference).
    queue: SerialQueue,
}

// SAFETY: the monitor is a Network.framework object, reference counted by the framework and
// bound to no thread: its own queue only runs the handler, and `start` makes every other call
// (create, set the queue and the handler, start) from the caller's thread. `Drop` is the only
// code that reaches it from this holder, cancelling and releasing it from whichever thread
// drops it. The handler's closure captures only the sink, which is `Send + Sync`, and the
// `'static` table of function pointers, so the block's last release (and the closure's drop)
// may run on any thread; the queue is `Send`.
unsafe impl Send for PathMonitor {}
// SAFETY: `&PathMonitor` reaches neither the monitor nor the block (`Drop` takes `&mut`), so
// sharing a reference shares nothing.
unsafe impl Sync for PathMonitor {}

impl std::fmt::Debug for PathMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PathMonitor({:p})", self.raw.as_ptr())
    }
}

impl PathMonitor {
    /// Starts a monitor whose updates reach `sink` on a new serial queue named `label` (the
    /// name a crash report and `sample` show), or `None` when Network.framework, the queue,
    /// the block or the monitor could not be had.
    ///
    /// The first update is the path in use now; on a Mac with a network the monitor hands it
    /// over at once. Each later update is a change of that path.
    #[must_use]
    pub fn start(label: &CStr, sink: impl Fn(PathReading) + Send + Sync + 'static) -> Option<Self> {
        let network = Network::table()?;
        let queue = SerialQueue::new(label)?;
        let update = move |path: *mut c_void| {
            // SAFETY: `path` is the live `nw_path_t` the monitor hands its handler, valid only
            // for this call and only read inside it.
            let (raw, constrained) =
                unsafe { ((network.status)(path), (network.constrained)(path)) };
            sink(PathReading {
                status: PathStatus::from_raw(raw),
                constrained,
            });
        };
        // SAFETY: the monitor calls its update handler as `void (^)(nw_path_t)`, a block of one
        // object pointer returning nothing, which is this prototype.
        let handler = unsafe { RcBlock::new1(update) }?;
        // SAFETY: `nw_path_monitor_create` takes nothing and returns a +1 monitor or null.
        let raw = NonNull::new(unsafe { (network.create)() })?;
        let monitor = Self {
            raw,
            network,
            handler,
            queue,
        };
        // SAFETY: a +1 monitor from `nw_path_monitor_create`, given this live queue and block
        // (each retained by the monitor, `nw_path_monitor_set_update_handler` copying the
        // block) before it starts, and cancelled and released exactly once, in `Drop`; this
        // holder keeps the queue and the block alive until `Drop` has made both calls.
        unsafe {
            (network.set_queue)(raw.as_ptr(), monitor.queue.as_id().as_ptr());
            (network.set_update_handler)(raw.as_ptr(), monitor.handler.as_ptr());
            (network.start)(raw.as_ptr());
        }
        Some(monitor)
    }
}

impl Drop for PathMonitor {
    fn drop(&mut self) {
        // SAFETY: this holder owns the one +1 reference `nw_path_monitor_create` returned,
        // cancelled and then released exactly once here; the queue and the block are fields
        // dropped only after this body, and the monitor keeps its own references to both for
        // whatever update its asynchronous cancel still lets through.
        unsafe {
            (self.network.cancel)(self.raw.as_ptr());
            (self.network.release)(self.raw.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PathStatus;

    /// The framework opens and every function a monitor calls resolves; a started monitor
    /// reads this Mac's network path, which a test leaves alone.
    #[test]
    fn the_framework_opens_and_every_function_resolves() {
        assert!(super::Network::open().is_some());
    }

    /// Each `nw_path_status_t` the header names reads as its own status, and every other
    /// value as invalid.
    #[test]
    fn each_raw_status_reads_as_the_one_the_header_names() {
        let table = [
            (0, PathStatus::Invalid),
            (1, PathStatus::Satisfied),
            (2, PathStatus::Unsatisfied),
            (3, PathStatus::Satisfiable),
            (4, PathStatus::Invalid),
            (-1, PathStatus::Invalid),
            (i32::MAX, PathStatus::Invalid),
            (i32::MIN, PathStatus::Invalid),
        ];
        for (raw, status) in table {
            assert_eq!(PathStatus::from_raw(raw), status, "raw {raw}");
        }
    }
}
