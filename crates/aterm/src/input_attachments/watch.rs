// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Device lifecycle is a hint to the existing full keyboard census, never its authority.

use std::cell::Cell;
use std::ffi::{CStr, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{Thread, ThreadId};
use std::time::Instant;

type Ref = *const c_void;
type Port = *mut c_void;
type RunLoop = *mut c_void;
type Source = *mut c_void;

#[repr(C)]
struct SourceContext {
    version: isize,
    info: *mut c_void,
    retain: Option<extern "C" fn(Ref) -> Ref>,
    release: Option<extern "C" fn(Ref)>,
    copy_description: Option<extern "C" fn(Ref) -> Ref>,
    equal: Option<extern "C" fn(Ref, Ref) -> u8>,
    hash: Option<extern "C" fn(Ref) -> usize>,
    schedule: Option<extern "C" fn(*mut c_void, RunLoop, Ref)>,
    cancel: Option<extern "C" fn(*mut c_void, RunLoop, Ref)>,
    perform: extern "C" fn(*mut c_void),
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IONotificationPortCreate(main_port: u32) -> Port;
    fn IONotificationPortDestroy(port: Port);
    fn IONotificationPortGetRunLoopSource(port: Port) -> Source;
    fn IOServiceMatching(name: *const std::ffi::c_char) -> *mut c_void;
    fn IOServiceAddMatchingNotification(
        port: Port,
        kind: *const std::ffi::c_char,
        matching: Ref,
        callback: unsafe extern "C" fn(*mut c_void, u32),
        context: *mut c_void,
        iterator: *mut u32,
    ) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: Ref;
    fn CFRunLoopGetCurrent() -> RunLoop;
    fn CFRunLoopSourceCreate(allocator: Ref, order: isize, context: *mut SourceContext) -> Source;
    fn CFRunLoopAddSource(run_loop: RunLoop, source: Source, mode: Ref);
    fn CFRunLoopRemoveSource(run_loop: RunLoop, source: Source, mode: Ref);
    fn CFRunLoopSourceSignal(source: Source);
    fn CFRunLoopSourceInvalidate(source: Source);
    fn CFRunLoopWakeUp(run_loop: RunLoop);
    fn CFRunLoopRunInMode(mode: Ref, seconds: f64, return_after_source: u8) -> i32;
    fn CFRetain(value: Ref) -> Ref;
    fn CFRelease(value: Ref);
}

#[derive(Default)]
pub(super) struct Control {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    pending: bool,
    owner: Option<Wake>,
}

struct Wake {
    thread: Thread,
    native: Option<NativeWake>,
}

struct NativeWake {
    run_loop: RunLoop,
    source: Source,
}

// SAFETY: these are retained references used across threads only by the documented
// thread-safe signal/wake/release calls. The worker alone runs and removes the source.
unsafe impl Send for NativeWake {}

impl NativeWake {
    fn signal(&self) {
        // SAFETY: this owns retained live references, serialized against removal by Control.
        unsafe {
            CFRunLoopSourceSignal(self.source);
            CFRunLoopWakeUp(self.run_loop);
        }
    }
}

impl Drop for NativeWake {
    fn drop(&mut self) {
        // SAFETY: Control's registration retained each reference once for this handle.
        unsafe {
            CFRelease(self.source.cast_const());
            CFRelease(self.run_loop.cast_const());
        }
    }
}

impl Control {
    pub(super) fn wake(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.pending = true;
        if let Some(owner) = &state.owner {
            owner.thread.unpark();
            if let Some(native) = &owner.native {
                native.signal();
            }
        }
    }

    fn take_pending(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut state.pending)
    }

    fn register(&self, native: Option<&Native>, stopped: &AtomicBool) {
        let native = native.map(|native| {
            // SAFETY: the current worker owns these live references until it unregisters;
            // this extra ownership lets Supervisor safely signal them from another thread.
            unsafe {
                CFRetain(native.run_loop.cast_const());
                CFRetain(native.source.cast_const());
            }
            NativeWake {
                run_loop: native.run_loop,
                source: native.source,
            }
        });
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.owner = Some(Wake {
            thread: std::thread::current(),
            native,
        });
        // A stop before setup and a wake before the first wait both remain pending.
        if state.pending || stopped.load(Ordering::Acquire) {
            let owner = state.owner.as_ref().expect("registered worker");
            owner.thread.unpark();
            if let Some(native) = &owner.native {
                native.signal();
            }
        }
    }

    fn unregister(&self, worker: ThreadId) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state
            .owner
            .as_ref()
            .is_some_and(|owner| owner.thread.id() == worker)
        {
            state.owner = None;
        }
    }
}

/// Owned by the already-existing attachment worker, including every callback's context.
pub(super) struct Watch {
    control: Arc<Control>,
    native: Option<Native>,
    worker: ThreadId,
    #[cfg(test)]
    before_wait: Option<Box<dyn FnMut()>>,
}

impl Watch {
    pub(super) fn new(control: Arc<Control>, stopped: &AtomicBool) -> Self {
        Self::with_native(control, stopped, Native::new(true))
    }

    fn with_native(control: Arc<Control>, stopped: &AtomicBool, native: Option<Native>) -> Self {
        control.register(native.as_ref(), stopped);
        Self {
            control,
            native,
            worker: std::thread::current().id(),
            #[cfg(test)]
            before_wait: None,
        }
    }

    #[cfg(test)]
    pub(super) fn private(
        control: Arc<Control>,
        stopped: &AtomicBool,
        before_wait: impl FnMut() + 'static,
    ) -> Self {
        // The actual run-loop wait/source, with no subscription to this Mac's devices.
        let native = Native::new(false).expect("private native wake source");
        let mut watch = Self::with_native(control, stopped, Some(native));
        watch.before_wait = Some(Box::new(before_wait));
        watch
    }

    /// True only for a consumed hint; stop and fallback expiry return false.
    pub(super) fn wait_until(&mut self, stopped: &AtomicBool, deadline: Instant) -> bool {
        loop {
            if stopped.load(Ordering::Acquire) {
                return false;
            }
            if Instant::now() >= deadline {
                return false;
            }
            let pending = self.control.take_pending();
            let Some(native) = &mut self.native else {
                // Notification setup is optional. Preserve the full original fallback;
                // Control also unparks this owner when it is stopped.
                return pending || self.park_until(stopped, deadline);
            };
            if pending | native.context.changed.replace(false) | native.drain_notifications() {
                return true;
            }
            #[cfg(test)]
            if let Some(before_wait) = &mut self.before_wait {
                before_wait();
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            // SAFETY: both sources belong to this thread's run loop. A signalled source
            // remains pending before this call, closing the sample-to-wait window.
            let result =
                unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, left.as_secs_f64(), 1) };
            if result == 3 {
                return false;
            }
            if !matches!(result, 2..=4) {
                // A failed/finished native source must not create a hot loop or slide
                // the absolute fallback. Stop still unparks the owning thread.
                return self.park_until(stopped, deadline);
            }
        }
    }

    fn park_until(&self, stopped: &AtomicBool, deadline: Instant) -> bool {
        loop {
            if stopped.load(Ordering::Acquire) {
                return false;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            if self.control.take_pending() {
                return true;
            }
            // The pending predicate plus Thread's retained unpark token closes the
            // check-to-park gap. Spurious returns keep this same absolute deadline.
            std::thread::park_timeout(left);
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // Serialize against a supervisor's last signal before Native removes any source.
        self.control.unregister(self.worker);
    }
}

#[derive(Default)]
struct Context {
    changed: Cell<bool>,
}

extern "C" fn signalled(_context: *mut c_void) {}

unsafe extern "C" fn matched(context: *mut c_void, iterator: u32) {
    if drain(iterator) {
        // SAFETY: the same worker owns the registration and context through deregistration.
        unsafe { &*context.cast::<Context>() }.changed.set(true);
    }
}

fn drain(iterator: u32) -> bool {
    let mut changed = false;
    loop {
        // SAFETY: callers retain this notification iterator through every drain.
        let object = unsafe { IOIteratorNext(iterator) };
        if object == 0 {
            return changed;
        }
        changed = true;
        // SAFETY: every nonzero IOIteratorNext result transfers one owned reference.
        let _ = unsafe { IOObjectRelease(object) };
    }
}

struct Native {
    run_loop: RunLoop,
    source: Source,
    port: Port,
    notifications: Vec<u32>,
    context: Box<Context>,
}

impl Native {
    fn new(devices: bool) -> Option<Self> {
        let context = Box::<Context>::default();
        let mut definition = SourceContext {
            version: 0,
            info: ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: signalled,
        };
        // SAFETY: CoreFoundation copies this context; its callback needs no refcon.
        let source = unsafe { CFRunLoopSourceCreate(ptr::null(), 0, &raw mut definition) };
        if source.is_null() {
            return None;
        }
        // SAFETY: this worker's run loop remains live for Native's entire lifetime.
        let run_loop = unsafe { CFRunLoopGetCurrent() };
        // SAFETY: source is owned, and this is its owning thread's run loop.
        unsafe { CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode) };
        let mut native = Self {
            run_loop,
            source,
            port: ptr::null_mut(),
            notifications: Vec::new(),
            context,
        };
        if devices && !native.register_devices() {
            native.remove_notifications();
        }
        Some(native)
    }

    fn register_devices(&mut self) -> bool {
        // Registry lifecycle notifications open no HID device and subscribe to no input
        // reports/events. Kernel and DriverKit service families are both wake hints; the
        // full Passive census still decides keyboard conformance and mapping readiness.
        // SAFETY: zero requests the default main port; the returned port is ours.
        self.port = unsafe { IONotificationPortCreate(0) };
        if self.port.is_null() {
            return false;
        }
        // SAFETY: the live port owns this borrowed source through port destruction.
        let source = unsafe { IONotificationPortGetRunLoopSource(self.port) };
        if source.is_null() {
            return false;
        }
        // SAFETY: the borrowed source and this worker's run loop remain live together.
        unsafe { CFRunLoopAddSource(self.run_loop, source, kCFRunLoopDefaultMode) };
        for class in [
            c"IOHIDEventService",
            c"AppleUserHIDEventService",
            c"AppleHIDKeyboardEventDriverV2",
        ] {
            for kind in [c"IOServiceFirstMatch", c"IOServiceTerminate"] {
                // An unavailable family does not discard other valid wake sources.
                // The unchanged fallback covers every unregistered service as well.
                let _ = self.register(class, kind);
            }
        }
        !self.notifications.is_empty()
    }

    fn register(&mut self, class: &CStr, kind: &CStr) -> bool {
        // SAFETY: both names are static NUL-terminated strings; matching is transferred
        // to IOServiceAddMatchingNotification even if registration returns an error.
        let matching = unsafe { IOServiceMatching(class.as_ptr()) };
        if matching.is_null() {
            return false;
        }
        let mut iterator = 0;
        // SAFETY: self owns the port and stable context until unregistering every iterator.
        let result = unsafe {
            IOServiceAddMatchingNotification(
                self.port,
                kind.as_ptr(),
                matching.cast_const(),
                matched,
                (&raw mut *self.context).cast(),
                &raw mut iterator,
            )
        };
        if result != 0 || iterator == 0 {
            if iterator != 0 {
                // SAFETY: even an unsuccessful call may have transferred an iterator.
                let _ = unsafe { IOObjectRelease(iterator) };
            }
            return false;
        }
        self.notifications.push(iterator);
        // Emptying the initial iterator arms it BEFORE the first full owner census.
        let _ = drain(iterator);
        true
    }

    fn drain_notifications(&self) -> bool {
        self.notifications.iter().fold(false, |changed, &iterator| {
            // Drain every subscription, coalescing overlapping service-class hints.
            drain(iterator) | changed
        })
    }

    fn remove_notifications(&mut self) {
        for iterator in self.notifications.drain(..) {
            // SAFETY: each notification iterator is owned once and releases registration.
            let _ = unsafe { IOObjectRelease(iterator) };
        }
        if !self.port.is_null() {
            // SAFETY: this thread is not dispatching callbacks during teardown. The port
            // owns its source; remove it before destroying the port and then the context.
            unsafe {
                let source = IONotificationPortGetRunLoopSource(self.port);
                if !source.is_null() {
                    CFRunLoopRemoveSource(self.run_loop, source, kCFRunLoopDefaultMode);
                }
                IONotificationPortDestroy(self.port);
            }
            self.port = ptr::null_mut();
        }
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        self.remove_notifications();
        // SAFETY: Control was unregistered first; no remote caller can signal now, and
        // this worker alone invokes callbacks. Remove/invalidate before freeing context.
        unsafe {
            CFRunLoopRemoveSource(self.run_loop, self.source, kCFRunLoopDefaultMode);
            CFRunLoopSourceInvalidate(self.source);
            CFRelease(self.source.cast_const());
        }
    }
}
