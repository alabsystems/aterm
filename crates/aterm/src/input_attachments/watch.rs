// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Device lifecycle and a change of the console's user are hints to the existing full keyboard
//! census, never its authority.

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

/// `SCDynamicStoreContext` (`SCDynamicStore.h`).
#[repr(C)]
struct StoreContext {
    version: isize,
    info: *mut c_void,
    retain: Option<extern "C" fn(Ref) -> Ref>,
    release: Option<extern "C" fn(Ref)>,
    copy_description: Option<extern "C" fn(Ref) -> Ref>,
}

// The signatures are those of the macOS SDK's `SCDynamicStore.h` and `SCDynamicStoreKey.h`:
// a `Boolean` is a `u8`, a `CFIndex` an `isize`, and every reference a pointer.
#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCDynamicStoreCreate(
        allocator: Ref,
        name: Ref,
        callout: unsafe extern "C" fn(Ref, Ref, *mut c_void),
        context: *mut StoreContext,
    ) -> Ref;
    fn SCDynamicStoreKeyCreateConsoleUser(allocator: Ref) -> Ref;
    fn SCDynamicStoreSetNotificationKeys(store: Ref, keys: Ref, patterns: Ref) -> u8;
    fn SCDynamicStoreCreateRunLoopSource(allocator: Ref, store: Ref, order: isize) -> Source;
}

/// `CFArrayCallBacks`, only ever named by address.
#[repr(C)]
struct ArrayCallBacks {
    _opaque: [u8; 0],
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: Ref;
    static kCFTypeArrayCallBacks: ArrayCallBacks;
    fn CFArrayCreate(
        allocator: Ref,
        values: *const Ref,
        count: isize,
        callbacks: *const ArrayCallBacks,
    ) -> Ref;
    fn CFStringCreateWithCString(
        allocator: Ref,
        string: *const std::ffi::c_char,
        encoding: u32,
    ) -> Ref;
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

/// What ended a wait before its deadline: one bit for each kind of change it can hear.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Causes(u8);

impl Causes {
    /// A keyboard service arrived or left: a lifecycle notice on this worker's run loop.
    pub(super) const DEVICE: Self = Self(1);
    /// The login at the console changed.
    pub(super) const CONSOLE: Self = Self(1 << 1);
    /// A setup record was published or removed.
    pub(super) const RECORDS: Self = Self(1 << 2);
    /// One of this process's windows gained focus.
    pub(super) const FOCUS: Self = Self(1 << 3);
    /// This process asked for work a round does.
    pub(super) const REQUEST: Self = Self(1 << 4);
    /// [`Control::wake`], which names no change.
    pub(super) const WAKE: Self = Self(1 << 5);

    const NAMES: [(Self, &'static str); 6] = [
        (Self::DEVICE, "device"),
        (Self::CONSOLE, "console"),
        (Self::RECORDS, "records"),
        (Self::FOCUS, "focus"),
        (Self::REQUEST, "request"),
        (Self::WAKE, "wake"),
    ];

    pub(super) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every cause of `other` is among these.
    pub(super) const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any cause of `other` is among these.
    pub(super) const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub(super) const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Debug for Causes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return f.write_str("none");
        }
        let mut first = true;
        for (cause, name) in Self::NAMES {
            if self.contains(cause) {
                if !first {
                    f.write_str("+")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

/// Which of a watch's own hint sources tell it what they watch ([`Watch::armed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Armed {
    /// Every keyboard lifecycle notification registered.
    pub(super) devices: bool,
    /// The dynamic store tells a change of the console's user.
    pub(super) console: bool,
}

/// How a wait ended ([`Watch::wait`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Woke {
    Stopped,
    Deadline,
    Hint(Causes),
}

#[derive(Default)]
pub(super) struct Control {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    pending: Causes,
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
        self.hint(Causes::WAKE);
    }

    /// Keep `cause` for the worker's next wait and end the wait it is in.
    pub(super) fn hint(&self, cause: Causes) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.pending = state.pending.union(cause);
        if let Some(owner) = &state.owner {
            owner.thread.unpark();
            if let Some(native) = &owner.native {
                native.signal();
            }
        }
    }

    fn take_pending(&self) -> Causes {
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
        if !state.pending.is_empty() || stopped.load(Ordering::Acquire) {
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
    /// This worker's run loop once ended in a way that is neither a source, a stop nor its
    /// time running out, so its sources may no longer be heard.
    failed: bool,
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
            failed: false,
            #[cfg(test)]
            before_wait: None,
        }
    }

    /// Which of this watch's own sources a wait hears: none without a native source, and none
    /// once the run loop failed.
    pub(super) fn armed(&self) -> Armed {
        let native = self.native.as_ref().filter(|_| !self.failed);
        Armed {
            devices: native.is_some_and(|native| native.notifications.len() == DEVICE_NOTICES),
            console: native.is_some_and(|native| !native.console.is_null()),
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

    /// Wait for a hint, for `deadline` or for stop, whichever is first. A stop or a passed
    /// deadline is told before a hint that is also there.
    pub(super) fn wait(&mut self, stopped: &AtomicBool, deadline: Instant) -> Woke {
        loop {
            if stopped.load(Ordering::Acquire) {
                return Woke::Stopped;
            }
            if Instant::now() >= deadline {
                return Woke::Deadline;
            }
            let mut causes = self.control.take_pending();
            let Some(native) = &mut self.native else {
                // Notification setup is optional. Without it the wait is the whole deadline,
                // and `armed` says so: the keeper then keeps its timer.
                // Control also unparks this owner when it is stopped.
                if !causes.is_empty() {
                    return Woke::Hint(causes);
                }
                return self.park_until(stopped, deadline);
            };
            if native.context.changed.replace(false) | native.drain_notifications() {
                causes = causes.union(Causes::DEVICE);
            }
            if native.context.console.replace(false) {
                causes = causes.union(Causes::CONSOLE);
            }
            if !causes.is_empty() {
                return Woke::Hint(causes);
            }
            #[cfg(test)]
            if let Some(before_wait) = &mut self.before_wait {
                before_wait();
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Woke::Deadline;
            };
            // SAFETY: both sources belong to this thread's run loop. A signalled source
            // remains pending before this call, closing the sample-to-wait window.
            let result =
                unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, left.as_secs_f64(), 1) };
            if result == 3 {
                return Woke::Deadline;
            }
            if !matches!(result, 2..=4) {
                // A failed/finished native source must not create a hot loop or slide
                // the absolute deadline. Stop still unparks the owning thread.
                self.failed = true;
                return self.park_until(stopped, deadline);
            }
        }
    }

    fn park_until(&self, stopped: &AtomicBool, deadline: Instant) -> Woke {
        loop {
            if stopped.load(Ordering::Acquire) {
                return Woke::Stopped;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Woke::Deadline;
            };
            let causes = self.control.take_pending();
            if !causes.is_empty() {
                return Woke::Hint(causes);
            }
            // The pending predicate plus Thread's retained unpark token closes the
            // check-to-park gap. Spurious returns keep this same absolute deadline.
            std::thread::park_timeout(left);
        }
    }
}

#[cfg(test)]
impl Watch {
    /// Call the dynamic store's callout as its run-loop source calls it on this worker's thread:
    /// the store, the changed keys, then the context this watch registered.
    pub(super) fn console_notice(&self) {
        let native = self
            .native
            .as_ref()
            .expect("a private watch has a native source");
        let context: *const Context = &raw const *native.context;
        // SAFETY: the context is this watch's own and live; the callout reads nothing else.
        unsafe { console_changed(ptr::null(), ptr::null(), context.cast_mut().cast()) };
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
    /// A keyboard service arrived or left.
    changed: Cell<bool>,
    /// The dynamic store told a change of the console's user.
    console: Cell<bool>,
}

extern "C" fn signalled(_context: *mut c_void) {}

/// The dynamic store's callout for the console user's key: `SCDynamicStoreCallBack`.
unsafe extern "C" fn console_changed(_store: Ref, _changed_keys: Ref, info: *mut c_void) {
    // SAFETY: `info` is the context this worker registered and owns through deregistration.
    unsafe { &*info.cast::<Context>() }.console.set(true);
}

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

/// The service classes whose arrival and departure are keyboard lifecycle notices.
const DEVICE_CLASSES: [&CStr; 3] = [
    c"IOHIDEventService",
    c"AppleUserHIDEventService",
    c"AppleHIDKeyboardEventDriverV2",
];
const DEVICE_KINDS: [&CStr; 2] = [c"IOServiceFirstMatch", c"IOServiceTerminate"];
/// One notification for each class and kind.
const DEVICE_NOTICES: usize = DEVICE_CLASSES.len() * DEVICE_KINDS.len();

struct Native {
    run_loop: RunLoop,
    source: Source,
    port: Port,
    notifications: Vec<u32>,
    /// The dynamic store session and its run-loop source that tell a change of the console's
    /// user: both null until [`Self::register_console`] armed them.
    store: Ref,
    console: Source,
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
            store: ptr::null(),
            console: ptr::null_mut(),
            context,
        };
        if devices {
            if !native.register_devices() {
                native.remove_notifications();
            }
            let _ = native.register_console();
        }
        Some(native)
    }

    /// Have the dynamic store tell this worker's run loop when the console's user changes: a
    /// login, a logout, a switch of user. Whether it armed; a session that did not is let go.
    fn register_console(&mut self) -> bool {
        let mut context = StoreContext {
            version: 0,
            info: (&raw mut *self.context).cast(),
            retain: None,
            release: None,
            copy_description: None,
        };
        // SAFETY: the name is a static NUL-terminated string in UTF-8 (0x0800_0100), and both
        // owned references made here are released below. The store copies `context`, whose
        // `info` this worker keeps alive until `remove_console` released the store.
        let store = unsafe {
            let name = CFStringCreateWithCString(
                ptr::null(),
                c"aterm-input-attachments".as_ptr(),
                0x0800_0100,
            );
            if name.is_null() {
                return false;
            }
            let store = SCDynamicStoreCreate(ptr::null(), name, console_changed, &raw mut context);
            CFRelease(name);
            store
        };
        if store.is_null() {
            return false;
        }
        self.store = store;
        // SAFETY: `store` is live and owned; the key and the array are owned and released here,
        // the array retaining its one value through `kCFTypeArrayCallBacks`. The source is added
        // to this worker's own run loop, which outlives it.
        let armed = unsafe {
            let key = SCDynamicStoreKeyCreateConsoleUser(ptr::null());
            let keys = if key.is_null() {
                ptr::null()
            } else {
                CFArrayCreate(
                    ptr::null(),
                    &raw const key,
                    1,
                    &raw const kCFTypeArrayCallBacks,
                )
            };
            let watched =
                !keys.is_null() && SCDynamicStoreSetNotificationKeys(store, keys, ptr::null()) != 0;
            if !keys.is_null() {
                CFRelease(keys);
            }
            if !key.is_null() {
                CFRelease(key);
            }
            if watched {
                self.console = SCDynamicStoreCreateRunLoopSource(ptr::null(), store, 0);
            }
            if !self.console.is_null() {
                CFRunLoopAddSource(self.run_loop, self.console, kCFRunLoopDefaultMode);
            }
            !self.console.is_null()
        };
        if !armed {
            self.remove_console();
        }
        armed
    }

    fn remove_console(&mut self) {
        // SAFETY: this thread is not dispatching the store's callout during teardown. Each
        // reference is owned once; the source leaves this worker's run loop before its store
        // is released, and both before the context the callout reads.
        unsafe {
            if !self.console.is_null() {
                CFRunLoopRemoveSource(self.run_loop, self.console, kCFRunLoopDefaultMode);
                CFRunLoopSourceInvalidate(self.console);
                CFRelease(self.console.cast_const());
                self.console = ptr::null_mut();
            }
            if !self.store.is_null() {
                CFRelease(self.store);
                self.store = ptr::null();
            }
        }
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
        for class in DEVICE_CLASSES {
            for kind in DEVICE_KINDS {
                // An unavailable family does not discard other valid wake sources. While any
                // is missing `armed` says so, and the keeper's timer covers every
                // unregistered service as well.
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
        self.remove_console();
        // SAFETY: Control was unregistered first; no remote caller can signal now, and
        // this worker alone invokes callbacks. Remove/invalidate before freeing context.
        unsafe {
            CFRunLoopRemoveSource(self.run_loop, self.source, kCFRunLoopDefaultMode);
            CFRunLoopSourceInvalidate(self.source);
            CFRelease(self.source.cast_const());
        }
    }
}
