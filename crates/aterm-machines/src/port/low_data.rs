// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::low_data_mode`] on the live system: whether the network this Mac uses now
//! is in Low Data Mode, as Network.framework's path monitor reads it (`nw_path_is_constrained`
//! of the path its first update hands over). The framework is opened at the look (`dlopen`), so
//! a run that never asks loads nothing more.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::mpsc;
use std::time::Duration;

/// How long the look waits for the monitor's first path, which it hands over at once on a Mac
/// with a network.
pub(super) const PATIENCE: Duration = Duration::from_secs(2);

/// Network.framework's binary.
const NETWORK: &CStr = c"/System/Library/Frameworks/Network.framework/Network";

/// `<dlfcn.h>`'s `RTLD_LAZY` on Darwin.
const RTLD_LAZY: c_int = 0x1;

/// `nw_path_status_satisfied`: the path can carry traffic.
const SATISFIED: c_int = 1;

// SAFETY: libSystem's `dlopen`, with `<dlfcn.h>`'s prototype; aterm's `libc` declares
// `dlsym` but not it.
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
}

/// The Network.framework functions the look calls, with `<Network/path_monitor.h>`'s and
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

impl Network {
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
            let found = unsafe { libc::dlsym(handle, name.as_ptr()) };
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

/// Whether the current network path is constrained (Low Data Mode), waiting at most
/// `patience` for the monitor's first update; `None` when no path that carries traffic was
/// read.
pub(super) fn read(patience: Duration) -> Option<bool> {
    let network = Network::open()?;
    let queue = aterm_objc::SerialQueue::new(c"aterm.machines.low-data")?;
    let (tx, rx) = mpsc::sync_channel::<(c_int, bool)>(1);
    let (status, constrained) = (network.status, network.constrained);
    let update = move |path: *mut c_void| {
        // SAFETY: `path` is the live `nw_path_t` the monitor hands its handler, only read.
        let read = unsafe { (status(path), constrained(path)) };
        let _ = tx.try_send(read);
    };
    // SAFETY: the monitor calls its update handler as `void (^)(nw_path_t)`, a block of one
    // object pointer returning nothing, which is this prototype.
    let handler = unsafe { aterm_objc::RcBlock::new1(update) }?;
    // SAFETY: a +1 monitor from `nw_path_monitor_create`, given this live queue and block
    // (each retained by the monitor, `nw_path_monitor_set_update_handler` copying the block)
    // before it starts, cancelled and released exactly once below; the queue and the block
    // outlive both calls.
    let first = unsafe {
        let monitor = (network.create)();
        if monitor.is_null() {
            return None;
        }
        (network.set_queue)(monitor, queue.as_id().as_ptr());
        (network.set_update_handler)(monitor, handler.as_ptr());
        (network.start)(monitor);
        let first = rx.recv_timeout(patience).ok();
        (network.cancel)(monitor);
        (network.release)(monitor);
        first
    };
    first
        .filter(|(status, _)| *status == SATISFIED)
        .map(|(_, constrained)| constrained)
}

#[cfg(test)]
mod tests {
    /// The framework opens and every function the look calls resolves; the look itself reads
    /// this Mac's network settings, which a test leaves alone.
    #[test]
    fn the_framework_opens_and_every_function_resolves() {
        assert!(super::Network::open().is_some());
    }
}
