// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Display-free `CFRunLoop` event loop for `--headless` on macOS (never initializes
//! `NSApplication` or connects to WindowServer).

use std::cell::Cell;
use std::marker::PhantomData;
use std::os::raw::c_void;
use std::ptr;
use std::time::{Duration, Instant};

use aterm_objc::cf::{
    kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFIndex, CFRelease, CFRunLoopAddSource,
    CFRunLoopGetMain, CFRunLoopRunInMode, CFRunLoopSourceContext, CFRunLoopSourceCreate,
    CFRunLoopSourceInvalidate, CFRunLoopSourceRef,
};
use aterm_objc::{autoreleasepool, MainThread};

use super::event_loop::{ActiveEventLoop, EventLoopProxy};
use super::wake_queue;
use crate::error::EventLoopError;
use crate::event::{Event, StartCause};
use crate::event_loop::{ActiveEventLoop as RootWindowTarget, ControlFlow};
use crate::platform::pump_events::PumpStatus;

/// Indefinite wait duration for `CFRunLoopRunInMode` on `ControlFlow::Wait`.
const WAIT_FOREVER_SECS: f64 = 1.0e10;

/// Control-flow and exit state of a headless macOS event loop.
#[derive(Debug)]
pub(super) struct LoopState {
    control_flow: Cell<ControlFlow>,
    exit: Cell<Option<i32>>,
    drained: wake_queue::Drained,
}

impl LoopState {
    pub(super) fn new(drained: wake_queue::Drained) -> Self {
        Self { control_flow: Cell::new(ControlFlow::default()), exit: Cell::new(None), drained }
    }

    pub(super) fn user_events_drained(&self) -> bool {
        self.drained.emptied()
    }

    pub(super) fn set_control_flow(&self, control_flow: ControlFlow) {
        self.control_flow.set(control_flow);
    }

    pub(super) const fn control_flow(&self) -> ControlFlow {
        self.control_flow.get()
    }

    pub(super) fn exit(&self) {
        self.exit.set(Some(0));
    }

    pub(super) const fn exiting(&self) -> bool {
        self.exit.get().is_some()
    }

    const fn exit_code(&self) -> Option<i32> {
        self.exit.get()
    }
}

/// Shared wake source registered in the main run loop's common modes.
struct WakeSource {
    source: CFRunLoopSourceRef,
    _identity: Box<u8>,
}

impl WakeSource {
    fn new() -> Self {
        const extern "C" fn perform(_: *mut c_void) {}
        let identity = Box::new(0u8);
        let info = (&*identity as *const u8).cast_mut().cast::<c_void>();
        // SAFETY: version-0 context whose `info` is only compared/hashed; released in `Drop`, after
        // the source is invalidated.
        unsafe {
            let mut context = CFRunLoopSourceContext {
                version: 0,
                info,
                retain: None,
                release: None,
                copyDescription: None,
                equal: None,
                hash: None,
                schedule: None,
                cancel: None,
                perform,
            };
            let source = CFRunLoopSourceCreate(ptr::null(), CFIndex::MAX - 1, &mut context);
            CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
            Self { source, _identity: identity }
        }
    }
}

impl Drop for WakeSource {
    fn drop(&mut self) {
        // SAFETY: invalidates and releases the +1 source created in `new`; invalidating removes it
        // from every mode, so a proxy outliving the loop signals a source nothing runs.
        unsafe {
            CFRunLoopSourceInvalidate(self.source);
            CFRelease(self.source as _);
        }
    }
}

pub struct EventLoop<T: 'static> {
    sender: wake_queue::Sender<T>,
    receiver: wake_queue::Receiver<T>,
    wake: WakeSource,
    window_target: RootWindowTarget,
    /// Whether the `StartCause::Init` iteration has run.
    loop_running: bool,
}

impl<T: 'static> EventLoop<T> {
    pub(super) fn new() -> Result<Self, EventLoopError> {
        let mtm =
            MainThread::new().expect("on macOS, `EventLoop` must be created on the main thread!");
        let (sender, receiver) = wake_queue::channel();
        let drained = receiver.drained();
        Ok(Self {
            sender,
            receiver,
            wake: WakeSource::new(),
            window_target: RootWindowTarget {
                p: ActiveEventLoop::new_headless(mtm, drained),
                _marker: PhantomData,
            },
            loop_running: false,
        })
    }

    const fn state(&self) -> &LoopState {
        self.window_target.p.headless_state().expect("a headless EventLoop holds a headless target")
    }

    pub(super) const fn window_target(&self) -> &RootWindowTarget {
        &self.window_target
    }

    pub(super) fn create_proxy(&self) -> EventLoopProxy<T> {
        // SAFETY: `self.wake.source` is live on the main run loop; the proxy retains it.
        unsafe { EventLoopProxy::sharing(self.sender.clone(), self.wake.source) }
    }

    pub(super) fn run_on_demand<F>(&mut self, mut callback: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        loop {
            match self.pump_events(None, &mut callback) {
                PumpStatus::Exit(0) => break Ok(()),
                PumpStatus::Exit(code) => break Err(EventLoopError::ExitFailure(code)),
                PumpStatus::Continue => continue,
            }
        }
    }

    pub(super) fn pump_events<F>(
        &mut self,
        timeout: Option<Duration>,
        mut callback: F,
    ) -> PumpStatus
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        if !self.loop_running {
            self.loop_running = true;
            self.single_iteration(&mut callback, StartCause::Init);
        }

        if !self.state().exiting() {
            self.wait_and_iterate(timeout, &mut callback);
        }

        if let Some(code) = self.state().exit_code() {
            self.loop_running = false;
            autoreleasepool(|_| callback(Event::LoopExiting, &self.window_target));
            PumpStatus::Exit(code)
        } else {
            PumpStatus::Continue
        }
    }

    fn has_pending(&self) -> bool {
        self.receiver.has_pending()
    }

    fn wait_and_iterate<F>(&mut self, timeout: Option<Duration>, callback: &mut F)
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        let start = Instant::now();

        let wait = if self.has_pending() {
            Some(Duration::ZERO)
        } else {
            let by_control_flow = match self.state().control_flow() {
                ControlFlow::Wait => None,
                ControlFlow::Poll => Some(Duration::ZERO),
                ControlFlow::WaitUntil(resume) => Some(resume.saturating_duration_since(start)),
            };
            match (by_control_flow, timeout) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        };

        let seconds = wait.map_or(WAIT_FOREVER_SECS, |d| d.as_secs_f64());
        // SAFETY: runs the main run loop's default mode on the main thread (`new` asserted it;
        // `EventLoop` is `!Send`).
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 1);
        }

        let cause = match self.state().control_flow() {
            ControlFlow::Poll => StartCause::Poll,
            ControlFlow::Wait => StartCause::WaitCancelled { start, requested_resume: None },
            ControlFlow::WaitUntil(resume) => {
                if Instant::now() < resume {
                    StartCause::WaitCancelled { start, requested_resume: Some(resume) }
                } else {
                    StartCause::ResumeTimeReached { start, requested_resume: resume }
                }
            },
        };

        self.single_iteration(callback, cause);
    }

    fn single_iteration<F>(&mut self, callback: &mut F, cause: StartCause)
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        autoreleasepool(|_| {
            callback(Event::NewEvents(cause), &self.window_target);

            if cause == StartCause::Init {
                callback(Event::Resumed, &self.window_target);
            }

            while let Some(event) = self.receiver.try_next() {
                callback(Event::UserEvent(event), &self.window_target);
            }

            callback(Event::AboutToWait, &self.window_target);
        });
    }
}
