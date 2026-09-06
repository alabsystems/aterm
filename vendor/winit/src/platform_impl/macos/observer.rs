// Modified by the aterm project in 2026; see the repository NOTICE.
// (`RunLoop::main`, `setup_control_flow_observers` and this file's internal
//  handlers take `aterm_objc::MainThread` instead of objc2's
//  `MainThreadMarker`, and the queued-closure block is `aterm_objc::RcBlock`
//  instead of a `block2::RcBlock`; no third-party Objective-C name remains
//  in the file.)
//! Utilities for working with `CFRunLoop`.
//!
//! See Apple's documentation on Run Loops for details:
//! <https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/Multithreading/RunLoopManagement/RunLoopManagement.html>
use std::cell::Cell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, UnwindSafe};
use std::ptr;
use std::rc::Weak;
use std::time::Instant;

use core_foundation::base::{CFIndex, CFOptionFlags, CFRelease, CFTypeRef};
use core_foundation::date::CFAbsoluteTimeGetCurrent;
use core_foundation::runloop::{
    kCFRunLoopAfterWaiting, kCFRunLoopBeforeWaiting, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
    kCFRunLoopExit, CFRunLoopActivity, CFRunLoopAddObserver, CFRunLoopAddTimer, CFRunLoopGetMain,
    CFRunLoopObserverCallBack, CFRunLoopObserverContext, CFRunLoopObserverCreate,
    CFRunLoopObserverRef, CFRunLoopRef, CFRunLoopTimerCreate, CFRunLoopTimerInvalidate,
    CFRunLoopTimerRef, CFRunLoopTimerSetNextFireDate, CFRunLoopWakeUp,
};
use tracing::error;

// LOCAL PATCH (aterm): `objc2_foundation::MainThreadMarker` became
// `aterm_objc::MainThread`. Every marker in this file is a WITNESS ONLY —
// `stop_app_on_panic`, `ApplicationDelegate::get` and `RunLoop::main` are all
// first-party, and `RunLoop::main` discards it — so nothing here was pinned by
// a framework binding and the file needed no `seam::marker` crossing at all.
// That is what makes this file, and `window_delegate.rs`, the two the
// substitution actually frees.
use aterm_objc::{MainThread, RcBlock};

use super::app_state::ApplicationDelegate;
use super::event_loop::{stop_app_on_panic, PanicInfo};
use super::ffi;

// LOCAL PATCH (aterm): the two observer callbacks are `extern "C-unwind"` and
// CONTAIN an NSException. Each runs the application's event handlers
// (`wakeup`/`cleared`), whose AppKit sends are `aterm_objc` sends and can
// raise; a raise unwinding into `stop_app_on_panic`'s `catch_unwind` used to
// abort with "Rust cannot catch foreign exceptions". The order is the measured
// one — `catch_unwind` OUTSIDE (a Rust panic still stops the app through
// `stop_app_on_panic`), the containment INSIDE — and `site` names the
// observer in the report.
unsafe fn control_flow_handler<F>(panic_info: *mut c_void, site: &'static str, f: F)
where
    F: FnOnce(Weak<PanicInfo>) + UnwindSafe,
{
    let info_from_raw = unsafe { Weak::from_raw(panic_info as *mut PanicInfo) };
    // Asserting unwind safety on this type should be fine because `PanicInfo` is
    // `RefUnwindSafe` and `Rc<T>` is `UnwindSafe` if `T` is `RefUnwindSafe`.
    let panic_info = AssertUnwindSafe(Weak::clone(&info_from_raw));
    // `from_raw` takes ownership of the data behind the pointer.
    // But if this scope takes ownership of the weak pointer, then
    // the weak pointer will get free'd at the end of the scope.
    // However we want to keep that weak reference around after the function.
    std::mem::forget(info_from_raw);

    let mtm = MainThread::new().unwrap();
    stop_app_on_panic(mtm, Weak::clone(&panic_info), move || {
        let _ = &panic_info;
        let _ = aterm_objc::contain(site, move || f(panic_info.0));
    });
}

/// The observer callback prototype, spelled `"C-unwind"`; converted to
/// `core_foundation`'s `extern "C"` alias at the one registration site.
type ObserverCallBack =
    extern "C-unwind" fn(CFRunLoopObserverRef, CFRunLoopActivity, *mut c_void);

// begin is queued with the highest priority to ensure it is processed before other observers
extern "C-unwind" fn control_flow_begin_handler(
    _: CFRunLoopObserverRef,
    activity: CFRunLoopActivity,
    panic_info: *mut c_void,
) {
    unsafe {
        control_flow_handler(panic_info, "CFRunLoop observer (AfterWaiting)", |panic_info| {
            #[allow(non_upper_case_globals)]
            match activity {
                kCFRunLoopAfterWaiting => {
                    // trace!("Triggered `CFRunLoopAfterWaiting`");
                    ApplicationDelegate::get(MainThread::new().unwrap()).wakeup(panic_info);
                    // trace!("Completed `CFRunLoopAfterWaiting`");
                },
                _ => unreachable!(),
            }
        });
    }
}

// end is queued with the lowest priority to ensure it is processed after other observers
// without that, LoopExiting would  get sent after AboutToWait
extern "C-unwind" fn control_flow_end_handler(
    _: CFRunLoopObserverRef,
    activity: CFRunLoopActivity,
    panic_info: *mut c_void,
) {
    unsafe {
        control_flow_handler(panic_info, "CFRunLoop observer (BeforeWaiting/Exit)", |panic_info| {
            #[allow(non_upper_case_globals)]
            match activity {
                kCFRunLoopBeforeWaiting => {
                    // trace!("Triggered `CFRunLoopBeforeWaiting`");
                    ApplicationDelegate::get(MainThread::new().unwrap()).cleared(panic_info);
                    // trace!("Completed `CFRunLoopBeforeWaiting`");
                },
                kCFRunLoopExit => (), // unimplemented!(), // not expected to ever happen
                _ => unreachable!(),
            }
        });
    }
}

#[derive(Debug)]
pub struct RunLoop(CFRunLoopRef);

impl Default for RunLoop {
    fn default() -> Self {
        Self(ptr::null_mut())
    }
}

impl RunLoop {
    pub fn main(mtm: MainThread) -> Self {
        // SAFETY: We have a main-thread witness here, which means we know we're on the main
        // thread, so
        // scheduling (and scheduling a non-`Send` block) to that thread is allowed.
        let _ = mtm;
        RunLoop(unsafe { CFRunLoopGetMain() })
    }

    pub fn wakeup(&self) {
        unsafe { CFRunLoopWakeUp(self.0) }
    }

    unsafe fn add_observer(
        &self,
        flags: CFOptionFlags,
        priority: CFIndex,
        handler: ObserverCallBack,
        context: *mut CFRunLoopObserverContext,
    ) {
        // LOCAL PATCH (aterm): `core_foundation` spells the callback
        // `extern "C"`; ours is `extern "C-unwind"`. The two share one calling
        // convention — the ABI string only tells rustc whether the callee may
        // unwind — and NOTHING unwinds out of these handlers: a Rust panic is
        // caught by `stop_app_on_panic`, an NSException by `contain`. So the
        // conversion is a re-spelling of the same address.
        // SAFETY: same machine ABI, same arity and argument types; the pointer
        // is a valid function of that shape.
        let handler: CFRunLoopObserverCallBack =
            unsafe { std::mem::transmute::<ObserverCallBack, CFRunLoopObserverCallBack>(handler) };
        let observer = unsafe {
            CFRunLoopObserverCreate(
                ptr::null_mut(),
                flags,
                ffi::TRUE, // Indicates we want this to run repeatedly
                priority,  // The lower the value, the sooner this will run
                handler,
                context,
            )
        };
        unsafe { CFRunLoopAddObserver(self.0, observer, kCFRunLoopCommonModes) };
    }

    /// Submit a closure to run on the main thread as the next step in the run loop, before other
    /// event sources are processed.
    ///
    /// This is used for running event handlers, as those are not allowed to run re-entrantly.
    ///
    /// # Implementation
    ///
    /// This queuing could be implemented in the following several ways with subtle differences in
    /// timing. This list is sorted in rough order in which they are run:
    ///
    /// 1. Using `CFRunLoopPerformBlock` or `-[NSRunLoop performBlock:]`.
    ///
    /// 2. Using `-[NSObject performSelectorOnMainThread:withObject:waitUntilDone:]` or wrapping the
    ///    event in `NSEvent` and posting that to `-[NSApplication postEvent:atStart:]` (both
    ///    creates a custom `CFRunLoopSource`, and signals that to wake up the main event loop).
    ///
    ///    a. `atStart = true`.
    ///
    ///    b. `atStart = false`.
    ///
    /// 3. `dispatch_async` or `dispatch_async_f`. Note that this may appear before 2b, it does not
    ///    respect the ordering that runloop events have.
    ///
    /// We choose the first one, both for ease-of-implementation, but mostly for consistency, as we
    /// want the event to be queued in a way that preserves the order the events originally arrived
    /// in.
    ///
    /// As an example, let's assume that we receive two events from the user, a mouse click which we
    /// handled by queuing it, and a window resize which we handled immediately. If we allowed
    /// AppKit to choose the ordering when queuing the mouse event, it might get put in the back of
    /// the queue, and the events would appear out of order to the user of Winit. So we must instead
    /// put the event at the very front of the queue, to be handled as soon as possible after
    /// handling whatever event it's currently handling.
    pub fn queue_closure(&self, closure: impl FnOnce() + 'static) {
        // LOCAL PATCH (aterm): the block parameter is a BARE BLOCK POINTER.
        //
        // `&Block<dyn Fn()>` and `*mut c_void` are the same argument to this C
        // function; what the binding bought was the TYPE, and `new0`'s `Encode`
        // bounds check the closure's arity and return type, which
        // `Block<dyn Fn()>` did not.
        //
        // `as_ptr`, not `as_block_ptr`: this is a C FUNCTION ARGUMENT, so no
        // Objective-C encoding is emitted for it by anyone and there is nothing
        // for the `"@?"` letter to be right about. `as_block_ptr` is for a
        // block argument of a method this tree DECLARES.
        extern "C" {
            fn CFRunLoopPerformBlock(rl: CFRunLoopRef, mode: CFTypeRef, block: *mut c_void);
        }

        // LOCAL PATCH (aterm): the block is `aterm_objc::RcBlock`, whose
        // `invoke` is the same guarded trampoline every declared method runs
        // under — `catch_unwind` OUTSIDE (a Rust panic aborts, NAMED, through
        // `abort_on_unwind("block invoke")`), the Objective-C `@try` INSIDE (an
        // `NSException` is contained and reported against `block invoke`).
        // This frame is what `handle_event` runs under for every event queued
        // while another is in flight, and what `handle_scale_factor_changed`
        // runs under; the third-party block it replaced had a `nounwind`
        // `extern "C"` invoke, and a raise below such a frame was MEASURED to
        // abort the process ("panic in a function that cannot unwind") even
        // with a containment outside it — or, with nothing outside, to
        // terminate through libc++abi's uncaught-exception handler. So the
        // queued road is now contained at the same depth as the observers'.
        //
        // Convert `FnOnce()` to a `Fn()` block.
        let closure = Cell::new(Some(closure));
        // SAFETY: the prototype `CFRunLoopPerformBlock` invokes is `void (^)(void)`,
        // which is exactly what `new0` over a `Fn() -> ()` builds; nothing
        // unwinds out of its `invoke`.
        let block = unsafe {
            RcBlock::new0(move || {
                if let Some(closure) = closure.take() {
                    closure()
                } else {
                    error!("tried to execute queued closure on main thread twice");
                }
            })
        }
        .expect("_Block_copy of the queued closure");

        // There are a few common modes (`kCFRunLoopCommonModes`) defined by Cocoa:
        // - `NSDefaultRunLoopMode`, alias of `kCFRunLoopDefaultMode`.
        // - `NSEventTrackingRunLoopMode`, used when mouse-dragging and live-resizing a window.
        // - `NSModalPanelRunLoopMode`, used when running a modal inside the Winit event loop.
        // - `NSConnectionReplyMode`: TODO.
        //
        // We only want to run event handlers in the default mode, as we support running a blocking
        // modal inside a Winit event handler (see [#1779]) which outrules the modal panel mode, and
        // resizing such panel window enters the event tracking run loop mode, so we can't directly
        // trigger events inside that mode either.
        //
        // Any events that are queued while running a modal or when live-resizing will instead wait,
        // and be delivered to the application afterwards.
        //
        // [#1779]: https://github.com/rust-windowing/winit/issues/1779
        let mode = unsafe { kCFRunLoopDefaultMode as CFTypeRef };

        // SAFETY: The runloop is valid, the mode is a `CFStringRef`, and the block is `'static`
        // (`CFRunLoopPerformBlock` copies it; `block`'s own reference is released on return).
        //
        // THE OWNERSHIP RULE, MEASURED. `block` is `_Block_release`d at the
        // end of this scope while the run loop invokes the closure later, which
        // is sound because `CFRunLoopPerformBlock` takes its OWN reference:
        // measured 1 -> 2 on the block header's refcount across the call, and
        // the capture's `-retainCount` back to 1 after the invoke.
        // `aterm-objc/tests/blocks.rs` asserts both halves. The ownership is
        // UNCHANGED from upstream, which dropped its `block2::RcBlock` here and
        // passed a borrow; the note exists because the borrow is now a raw
        // pointer.
        unsafe { CFRunLoopPerformBlock(self.0, mode, block.as_ptr()) }
    }
}

pub fn setup_control_flow_observers(mtm: MainThread, panic_info: Weak<PanicInfo>) {
    let run_loop = RunLoop::main(mtm);
    unsafe {
        let mut context = CFRunLoopObserverContext {
            info: Weak::into_raw(panic_info) as *mut _,
            version: 0,
            retain: None,
            release: None,
            copyDescription: None,
        };
        run_loop.add_observer(
            kCFRunLoopAfterWaiting,
            CFIndex::MIN,
            control_flow_begin_handler,
            &mut context as *mut _,
        );
        run_loop.add_observer(
            kCFRunLoopExit | kCFRunLoopBeforeWaiting,
            CFIndex::MAX,
            control_flow_end_handler,
            &mut context as *mut _,
        );
    }
}

#[derive(Debug)]
pub struct EventLoopWaker {
    timer: CFRunLoopTimerRef,

    /// An arbitrary instant in the past, that will trigger an immediate wake
    /// We save this as the `next_fire_date` for consistency so we can
    /// easily check if the next_fire_date needs updating.
    start_instant: Instant,

    /// This is what the `NextFireDate` has been set to.
    /// `None` corresponds to `waker.stop()` and `start_instant` is used
    /// for `waker.start()`
    next_fire_date: Option<Instant>,
}

impl Drop for EventLoopWaker {
    fn drop(&mut self) {
        unsafe {
            CFRunLoopTimerInvalidate(self.timer);
            CFRelease(self.timer as _);
        }
    }
}

impl EventLoopWaker {
    pub(crate) fn new() -> Self {
        extern "C" fn wakeup_main_loop(_timer: CFRunLoopTimerRef, _info: *mut c_void) {}
        unsafe {
            // Create a timer with a 0.1µs interval (1ns does not work) to mimic polling.
            // It is initially setup with a first fire time really far into the
            // future, but that gets changed to fire immediately in did_finish_launching
            let timer = CFRunLoopTimerCreate(
                ptr::null_mut(),
                f64::MAX,
                0.000_000_1,
                0,
                0,
                wakeup_main_loop,
                ptr::null_mut(),
            );
            CFRunLoopAddTimer(CFRunLoopGetMain(), timer, kCFRunLoopCommonModes);
            Self { timer, start_instant: Instant::now(), next_fire_date: None }
        }
    }

    pub fn stop(&mut self) {
        if self.next_fire_date.is_some() {
            self.next_fire_date = None;
            unsafe { CFRunLoopTimerSetNextFireDate(self.timer, f64::MAX) }
        }
    }

    pub fn start(&mut self) {
        if self.next_fire_date != Some(self.start_instant) {
            self.next_fire_date = Some(self.start_instant);
            unsafe { CFRunLoopTimerSetNextFireDate(self.timer, f64::MIN) }
        }
    }

    pub fn start_at(&mut self, instant: Option<Instant>) {
        let now = Instant::now();
        match instant {
            Some(instant) if now >= instant => {
                self.start();
            },
            Some(instant) => {
                if self.next_fire_date != Some(instant) {
                    self.next_fire_date = Some(instant);
                    unsafe {
                        let current = CFAbsoluteTimeGetCurrent();
                        let duration = instant - now;
                        let fsecs = duration.subsec_nanos() as f64 / 1_000_000_000.0
                            + duration.as_secs() as f64;
                        CFRunLoopTimerSetNextFireDate(self.timer, current + fsecs)
                    }
                }
            },
            None => {
                self.stop();
            },
        }
    }
}
