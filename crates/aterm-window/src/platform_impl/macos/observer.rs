// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/observer.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Utilities for working with `CFRunLoop`.
//!
//! LOCAL PATCH (aterm): the run-loop declarations are `aterm_objc::cf`'s (not the `core-foundation`
//! crate's), the handlers take `aterm_objc::MainThread`, and the queued-closure block is
//! `aterm_objc::RcBlock` (not `block2`'s).
use std::cell::Cell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, UnwindSafe};
use std::ptr;
use std::rc::Weak;
use std::time::Instant;

use aterm_objc::cf::{
    kCFRunLoopAfterWaiting, kCFRunLoopBeforeWaiting, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
    kCFRunLoopExit, CFIndex, CFOptionFlags, CFRunLoopActivity, CFRunLoopAddObserver,
    CFRunLoopGetMain, CFRunLoopObserverCallBack, CFRunLoopObserverContext, CFRunLoopObserverCreate,
    CFRunLoopObserverRef, CFRunLoopRef, CFRunLoopWakeUp, CFTypeRef,
};
use tracing::error;

use aterm_objc::{MainThread, RcBlock, WakeTimer};

use super::app_state::ApplicationDelegate;
use super::event_loop::{stop_app_on_panic, PanicInfo};
use super::ffi;

// LOCAL PATCH (aterm): the observer callbacks are `extern "C-unwind"` and contain an NSException
// from the handlers' AppKit sends inside `stop_app_on_panic`'s `catch_unwind`, where it used to
// abort as a foreign exception.
unsafe fn control_flow_handler<F>(panic_info: *mut c_void, site: &'static str, f: F)
where
    F: FnOnce(Weak<PanicInfo>) + UnwindSafe,
{
    let info_from_raw = unsafe { Weak::from_raw(panic_info as *mut PanicInfo) };
    let panic_info = AssertUnwindSafe(Weak::clone(&info_from_raw));
    std::mem::forget(info_from_raw);

    let mtm = MainThread::new().unwrap();
    stop_app_on_panic(mtm, Weak::clone(&panic_info), move || {
        let _ = &panic_info;
        let _ = aterm_objc::contain(site, move || f(panic_info.0));
    });
}

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
                    ApplicationDelegate::get(MainThread::new().unwrap()).wakeup(panic_info);
                },
                _ => unreachable!(),
            }
        });
    }
}

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
                    ApplicationDelegate::get(MainThread::new().unwrap()).cleared(panic_info);
                },
                kCFRunLoopExit => (),
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
        // SAFETY: We have a main-thread witness here, which means we know we're on the main thread, so scheduling (and scheduling a non-`Send` block) to that thread is allowed.
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
        handler: CFRunLoopObserverCallBack,
        context: *mut CFRunLoopObserverContext,
    ) {
        let observer = unsafe {
            CFRunLoopObserverCreate(ptr::null(), flags, ffi::TRUE, priority, handler, context)
        };
        unsafe { CFRunLoopAddObserver(self.0, observer, kCFRunLoopCommonModes) };
    }

    /// Submit a closure to run on the main thread as the next step in the run loop, before other event sources are processed.
    pub fn queue_closure(&self, closure: impl FnOnce() + 'static) {
        extern "C" {
            fn CFRunLoopPerformBlock(rl: CFRunLoopRef, mode: CFTypeRef, block: *mut c_void);
        }

        // LOCAL PATCH (aterm): `RcBlock`'s invoke runs under the declared-method guards (a panic
        // aborts named, an NSException is contained); upstream's block had a `nounwind` invoke,
        // below which a raise aborted the process.
        let closure = Cell::new(Some(closure));
        // SAFETY: the prototype `CFRunLoopPerformBlock` invokes is `void (^)(void)`, which is exactly what `new0` over a `Fn() -> ()` builds; nothing unwinds out of its `invoke`.
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

        let mode = unsafe { kCFRunLoopDefaultMode as CFTypeRef };

        // SAFETY: The runloop is valid, the mode is a `CFStringRef`, and the block is `'static` (`CFRunLoopPerformBlock` copies it; `block`'s own reference is released on return).
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

// LOCAL PATCH (aterm): wraps `aterm_objc::WakeTimer`, not a `CFRunLoopTimer` repeating every 0.1
// µs, which CoreFoundation advanced one interval at a time under the run-loop lock after the
// process had been stopped.
#[derive(Debug)]
pub struct EventLoopWaker {
    timer: WakeTimer,
}

impl EventLoopWaker {
    pub(crate) fn new(mtm: MainThread) -> Self {
        Self { timer: WakeTimer::main(mtm) }
    }

    pub fn stop(&mut self) {
        self.timer.stop();
    }

    pub fn start(&mut self) {
        self.timer.start();
    }

    pub fn start_at(&mut self, instant: Option<Instant>) {
        self.timer.start_at(Instant::now(), instant);
    }
}
