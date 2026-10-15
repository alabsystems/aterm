// Modified by the aterm project in 2026; see the repository NOTICE.

// A display-free event loop for `--headless` runs on macOS: the twin of
// `platform_impl/linux/headless.rs`, selected the same way (only by an explicit
// `EventLoopBuilderExtHeadless::with_headless`, never by probing).
//
// WHY. The AppKit loop is built on `+[NSApplication sharedApplication]`, and
// `-[NSApplication init]` opens the process's WindowServer connection, which
// makes WindowServer run a synchronous TCC preflight of the binary on its main
// thread (aterm's AGENTS.md: from a binary in a huge directory that
// preflight outran WindowServer's watchdog). A process with no window has no
// reason to be a WindowServer client, so this loop never creates the
// application object.
//
// WHAT. The MAIN thread's CFRunLoop, run in its default mode: one wake source
// that every proxy signals (`EventLoopProxy::sharing` says why it is shared),
// `ControlFlow` honoured including `WaitUntil`, and the `NewEvents -> Resumed ->
// UserEvent -> AboutToWait -> LoopExiting` order of every other backend. The
// run loop rather than `poll(2)` because the main thread's run loop also drains
// the main dispatch queue, which `dispatch_sync` callers on other threads wait
// on. CoreFoundation only.
//
// WHAT IT REFUSES. Windows (`window.rs` answers an error), monitors, the system
// theme, a display handle, custom cursors, hiding the application: each would
// reach AppKit or CoreGraphics' display services, i.e. open the connection.

use std::cell::Cell;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::os::raw::c_void;
use std::ptr;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_objc::{autoreleasepool, MainThread};
use core_foundation::base::{CFIndex, CFRelease};
use core_foundation::runloop::{
    kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetMain,
    CFRunLoopRunInMode, CFRunLoopSourceContext, CFRunLoopSourceCreate, CFRunLoopSourceInvalidate,
    CFRunLoopSourceRef,
};

use super::event_loop::{ActiveEventLoop, EventLoopProxy};
use crate::error::EventLoopError;
use crate::event::{Event, StartCause};
use crate::event_loop::{ActiveEventLoop as RootWindowTarget, ControlFlow};
use crate::platform::pump_events::PumpStatus;

/// One `ControlFlow::Wait` turn: effectively forever. `CFRunLoopRunInMode` has
/// no "no deadline" spelling; a handled source (a proxy wake) returns it.
const WAIT_FOREVER_SECS: f64 = 1.0e10;

/// The control-flow and exit state of a headless loop, held by the macOS
/// `ActiveEventLoop` in place of the AppKit backend's application delegate.
#[derive(Debug)]
pub(super) struct LoopState {
    control_flow: Cell<ControlFlow>,
    exit: Cell<Option<i32>>,
    /// Kept here instead of asking AppKit; starts at AppKit's default (`YES`).
    window_tabbing: Cell<bool>,
}

impl LoopState {
    pub(super) fn new() -> Self {
        Self {
            control_flow: Cell::new(ControlFlow::default()),
            exit: Cell::new(None),
            window_tabbing: Cell::new(true),
        }
    }

    pub(super) fn set_control_flow(&self, control_flow: ControlFlow) {
        self.control_flow.set(control_flow);
    }

    pub(super) fn control_flow(&self) -> ControlFlow {
        self.control_flow.get()
    }

    pub(super) fn exit(&self) {
        self.exit.set(Some(0));
    }

    pub(super) fn clear_exit(&self) {
        self.exit.set(None);
    }

    pub(super) fn exiting(&self) -> bool {
        self.exit.get().is_some()
    }

    fn exit_code(&self) -> Option<i32> {
        self.exit.get()
    }

    pub(super) fn set_allows_automatic_window_tabbing(&self, enabled: bool) {
        self.window_tabbing.set(enabled);
    }

    pub(super) fn allows_automatic_window_tabbing(&self) -> bool {
        self.window_tabbing.get()
    }
}

/// The loop's ONE wake source, in the main run loop's common modes. Every proxy
/// signals it, so an event always has a source the run loop really holds and a
/// signal landing between runs is still pending at the next; it also keeps the
/// mode non-empty (an empty mode returns at once: a spin). Its `info` is a heap
/// address owned here, which makes it unequal to every other source (see
/// `EventLoopProxy::sharing`).
struct WakeSource {
    source: CFRunLoopSourceRef,
    _identity: Box<u8>,
}

impl WakeSource {
    fn new() -> Self {
        extern "C" fn perform(_: *const c_void) {}
        let identity = Box::new(0u8);
        let info = (&*identity as *const u8).cast_mut().cast::<c_void>();
        // SAFETY: a version-0 context whose `info` CoreFoundation only compares
        // and hashes (no callback dereferences it); the +1 from `Create` is
        // released in `Drop`, after the source is invalidated.
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
            let source = CFRunLoopSourceCreate(ptr::null_mut(), CFIndex::MAX - 1, &mut context);
            CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
            Self { source, _identity: identity }
        }
    }
}

impl Drop for WakeSource {
    fn drop(&mut self) {
        // SAFETY: the +1 source from `new`; invalidating removes it from every
        // mode, so a proxy outliving the loop (whose sends now fail) signals
        // a source nothing runs.
        unsafe {
            CFRunLoopSourceInvalidate(self.source);
            CFRelease(self.source as _);
        }
    }
}

pub struct EventLoop<T: 'static> {
    sender: mpsc::Sender<T>,
    receiver: mpsc::Receiver<T>,
    /// Events `has_pending` took off the channel (a `Receiver` cannot peek).
    pending: VecDeque<T>,
    wake: WakeSource,
    window_target: RootWindowTarget,
    /// Whether the `StartCause::Init` iteration has run.
    loop_running: bool,
}

impl<T: 'static> EventLoop<T> {
    pub(super) fn new() -> Result<Self, EventLoopError> {
        // The loop runs the MAIN run loop, on the thread that calls `run`.
        let mtm = MainThread::new()
            .expect("on macOS, `EventLoop` must be created on the main thread!");
        let (sender, receiver) = mpsc::channel();
        Ok(Self {
            sender,
            receiver,
            pending: VecDeque::new(),
            wake: WakeSource::new(),
            window_target: RootWindowTarget {
                p: ActiveEventLoop::new_headless(mtm),
                _marker: PhantomData,
            },
            loop_running: false,
        })
    }

    fn state(&self) -> &LoopState {
        self.window_target.p.headless_state().expect("a headless EventLoop holds a headless target")
    }

    pub(super) fn window_target(&self) -> &RootWindowTarget {
        &self.window_target
    }

    pub(super) fn create_proxy(&self) -> EventLoopProxy<T> {
        // SAFETY: `self.wake.source` is live and in the main run loop for as
        // long as this loop exists; the proxy holds its own retain on it.
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

    pub(super) fn pump_events<F>(&mut self, timeout: Option<Duration>, mut callback: F) -> PumpStatus
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        if !self.loop_running {
            self.loop_running = true;
            self.single_iteration(&mut callback, StartCause::Init);
        }

        // An exit asked on the `Init` turn runs no further turn.
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

    /// Move everything already sent onto `pending`; is anything waiting?
    fn has_pending(&mut self) -> bool {
        self.pending.extend(self.receiver.try_iter());
        !self.pending.is_empty()
    }

    fn wait_and_iterate<F>(&mut self, timeout: Option<Duration>, callback: &mut F)
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        let start = Instant::now();

        // Queued work never waits on a deadline: one pass of zero seconds.
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
        // SAFETY: the main thread (`new` asserted it; `EventLoop` is `!Send`), so
        // this runs the MAIN run loop's default mode, which holds `self.wake`.
        // `returnAfterSourceHandled`: a proxy wake or a main-queue block ends
        // the wait, as does the deadline.
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 1);
        }

        // Read AFTER the wait: an elapsed `WaitUntil` is `ResumeTimeReached`,
        // one cut short by a wake `WaitCancelled`.
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
        // One pool per turn, as `-[NSApplication run]` gives every event.
        autoreleasepool(|_| {
            callback(Event::NewEvents(cause), &self.window_target);

            if cause == StartCause::Init {
                callback(Event::Resumed, &self.window_target);
            }

            // Pop one at a time: events a handler posts to itself during this
            // drain belong to it, as on the other backends.
            loop {
                let next = match self.pending.pop_front() {
                    Some(event) => Some(event),
                    None => self.receiver.try_recv().ok(),
                };
                let Some(event) = next else { break };
                callback(Event::UserEvent(event), &self.window_target);
            }

            callback(Event::AboutToWait, &self.window_target);
        });
    }
}
