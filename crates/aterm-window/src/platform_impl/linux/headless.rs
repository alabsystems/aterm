// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (The wake pipe and the
// deadline rounding moved to `common::poll`, which the X11 loop shares:
// docs/AUDIT-third-party-2026-10-15.md, LW.7.)

// A display-free event loop, selected explicitly with `with_headless`.
// It never connects to X11 or Wayland or silently substitutes for a failed display.
// A proxy wakes it across threads; ControlFlow governs polling and deadlines.
// Init emits NewEvents, Resumed, user events and AboutToWait; later turns omit
// Resumed, and exit emits LoopExiting. Window creation is an explicit error, and
// monitor enumeration is empty.

use std::cell::Cell;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::EventLoopError;
use crate::event::{Event, StartCause};
use crate::event_loop::{ActiveEventLoop as RootELW, ControlFlow, EventLoopClosed};
use crate::platform::pump_events::PumpStatus;

use super::common::poll::{self, Waker};
use super::MonitorHandle;

/// A proxy's half of the loop: the queue carries the events, the waker only "something
/// was posted" (`common::poll::Waker`, which never blocks the sender).
pub struct EventLoopProxy<T: 'static> {
    queue: Arc<Mutex<VecDeque<T>>>,
    waker: Waker,
}

impl<T: 'static> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        Self { queue: Arc::clone(&self.queue), waker: self.waker.clone() }
    }
}

impl<T: 'static> EventLoopProxy<T> {
    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        // A poisoned queue hands the event back rather than unwinding the sender: the
        // caller's `Err` arm already knows how to cope with a loop it cannot reach, and
        // a panic here would be a second failure on top of the first.
        let Ok(mut queue) = self.queue.lock() else {
            return Err(EventLoopClosed(event));
        };
        queue.push_back(event);
        drop(queue);
        self.waker.wake();
        Ok(())
    }
}

/// The windowless twin of the X11/Wayland `ActiveEventLoop`: the control-flow and exit
/// state a handler is allowed to set, and nothing that implies a display.
pub struct ActiveEventLoop {
    control_flow: Cell<ControlFlow>,
    exit: Cell<Option<i32>>,
}

impl ActiveEventLoop {
    fn new() -> Self {
        Self { control_flow: Cell::new(ControlFlow::default()), exit: Cell::new(None) }
    }

    #[inline]
    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        VecDeque::new()
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        None
    }

    #[inline]
    pub(crate) fn set_control_flow(&self, control_flow: ControlFlow) {
        self.control_flow.set(control_flow);
    }

    #[inline]
    pub(crate) fn control_flow(&self) -> ControlFlow {
        self.control_flow.get()
    }

    #[inline]
    pub(crate) fn exit(&self) {
        self.exit.set(Some(0));
    }

    #[inline]
    pub(crate) fn exiting(&self) -> bool {
        self.exit.get().is_some()
    }

    // Reached only through the dispatcher's, which the Wayland loop calls.
    #[cfg(wayland_platform)]
    #[inline]
    pub(crate) fn set_exit_code(&self, code: i32) {
        self.exit.set(Some(code));
    }

    #[inline]
    pub(crate) fn exit_code(&self) -> Option<i32> {
        self.exit.get()
    }

    /// There is no display, so there is no handle to hand out. `NotSupported` is the
    /// honest answer and the one a graphics backend can act on — it is the same answer
    /// it would get from a platform that genuinely cannot present.
    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Err(rwh_06::HandleError::NotSupported)
    }
}

pub struct EventLoop<T: 'static> {
    /// The root wrapper handed to the callback on every dispatch.
    target: RootELW,
    queue: Arc<Mutex<VecDeque<T>>>,
    waker: Waker,
    /// Whether the `StartCause::Init` iteration has already run. `pump_events` may be
    /// called many times for one loop, and only the first is `Init`.
    loop_running: bool,
    _marker: PhantomData<T>,
}

impl<T: 'static> EventLoop<T> {
    pub(crate) fn new() -> Result<Self, EventLoopError> {
        let waker = Waker::new().map_err(|_| {
            EventLoopError::Os(os_error!(super::OsError::Misc(
                "could not create the headless waker"
            )))
        })?;
        Ok(Self {
            target: RootELW {
                p: super::ActiveEventLoop::Headless(ActiveEventLoop::new()),
                _marker: PhantomData,
            },
            queue: Arc::new(Mutex::new(VecDeque::new())),
            waker,
            loop_running: false,
            _marker: PhantomData,
        })
    }

    fn state(&self) -> &ActiveEventLoop {
        match &self.target.p {
            super::ActiveEventLoop::Headless(state) => state,
            #[allow(unreachable_patterns)]
            _ => unreachable!("a headless EventLoop always holds a headless ActiveEventLoop"),
        }
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy { queue: Arc::clone(&self.queue), waker: self.waker.clone() }
    }

    pub fn window_target(&self) -> &RootELW {
        &self.target
    }

    pub fn run_on_demand<F>(&mut self, mut callback: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootELW),
    {
        loop {
            match self.pump_events(None, &mut callback) {
                PumpStatus::Exit(0) => break Ok(()),
                PumpStatus::Exit(code) => break Err(EventLoopError::ExitFailure(code)),
                PumpStatus::Continue => continue,
            }
        }
    }

    pub fn pump_events<F>(&mut self, timeout: Option<Duration>, mut callback: F) -> PumpStatus
    where
        F: FnMut(Event<T>, &RootELW),
    {
        if !self.loop_running {
            self.loop_running = true;
            self.single_iteration(&mut callback, StartCause::Init);
        }

        // The `Init` iteration is allowed to ask to exit, exactly as it is on the other
        // backends; polling after that request would run a turn the app already declined.
        if !self.state().exiting() {
            self.poll_events_with_timeout(timeout, &mut callback);
        }

        if let Some(code) = self.state().exit_code() {
            self.loop_running = false;
            callback(Event::LoopExiting, &self.target);
            PumpStatus::Exit(code)
        } else {
            PumpStatus::Continue
        }
    }

    fn has_pending(&self) -> bool {
        self.queue.lock().map(|q| !q.is_empty()).unwrap_or(false)
    }

    fn poll_events_with_timeout<F>(&mut self, timeout: Option<Duration>, callback: &mut F)
    where
        F: FnMut(Event<T>, &RootELW),
    {
        let start = Instant::now();

        // Work already queued must not be made to wait on a deadline: block for zero.
        let deadline = if self.has_pending() {
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

        // A failed wait is a spurious wake: the turn below runs, and the next one waits again.
        let _ = poll::wait(&mut [poll::readable(self.waker.as_raw_fd())], deadline);
        self.waker.drain();

        // The cause is read AFTER the wait, so a `WaitUntil` that actually elapsed is
        // reported as `ResumeTimeReached` and one cut short by a proxy wake is reported
        // as `WaitCancelled` — the distinction a handler uses to tell a due timer from
        // an early wake.
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
        F: FnMut(Event<T>, &RootELW),
    {
        callback(Event::NewEvents(cause), &self.target);

        if cause == StartCause::Init {
            callback(Event::Resumed, &self.target);
        }

        // Drain by repeated pop rather than by swapping the whole queue out: a handler is
        // allowed to post to its own proxy, and those events belong to THIS turn's drain
        // if they arrive before it finishes, exactly as on the other backends.
        loop {
            let next = match self.queue.lock() {
                Ok(mut queue) => queue.pop_front(),
                Err(_) => None,
            };
            let Some(event) = next else { break };
            callback(Event::UserEvent(event), &self.target);
        }

        callback(Event::AboutToWait, &self.target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Record events until `on_turn` exits. Timed pumps bound a lost wake by a
    /// one-minute hang detector without changing the handler's ControlFlow.
    fn drive<T: 'static>(
        mut loop_: EventLoop<T>,
        mut on_turn: impl FnMut(&RootELW, usize),
    ) -> Vec<String> {
        let mut log = Vec::new();
        let mut turns = 0usize;
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            let remaining = until.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "headless event-loop test hung");
            let status = loop_.pump_events(Some(remaining), |event, target| match &event {
                Event::NewEvents(cause) => log.push(format!("NewEvents({cause:?})")),
                Event::Resumed => log.push("Resumed".into()),
                Event::UserEvent(_) => log.push("UserEvent".into()),
                Event::AboutToWait => {
                    log.push("AboutToWait".into());
                    turns += 1;
                    on_turn(target, turns);
                },
                Event::LoopExiting => log.push("LoopExiting".into()),
                _ => {},
            });
            assert!(Instant::now() < until, "headless event-loop test hung");
            if let PumpStatus::Exit(code) = status {
                assert_eq!(code, 0);
                break;
            }
        }
        log
    }

    /// The order an `ApplicationHandler` is entitled to. `Resumed` must arrive exactly
    /// once, on the `Init` turn, and `LoopExiting` must be last — a handler that sets up
    /// on `Resumed` and tears down on `LoopExiting` is otherwise silently broken here
    /// while working on X11 and Wayland.
    #[test]
    fn the_first_turn_is_init_then_resumed_and_the_last_is_loop_exiting() {
        let loop_: EventLoop<()> = EventLoop::new().unwrap();
        let log = drive(loop_, |target, _| target.p.exit());
        assert_eq!(log.first().map(String::as_str), Some("NewEvents(Init)"));
        assert_eq!(log.get(1).map(String::as_str), Some("Resumed"));
        assert_eq!(log.last().map(String::as_str), Some("LoopExiting"));
        assert_eq!(
            log.iter().filter(|l| *l == "Resumed").count(),
            1,
            "Resumed is once, not per turn"
        );
    }

    /// A proxy is the ONLY way a windowless run learns anything from another thread, so
    /// an event posted before the loop starts must still be delivered — the queue, not
    /// the wake byte, is what carries the payload.
    #[test]
    fn an_event_posted_before_the_loop_runs_is_still_delivered() {
        let loop_: EventLoop<u32> = EventLoop::new().unwrap();
        loop_.create_proxy().send_event(7).unwrap();
        let log = drive(loop_, |target, _| target.p.exit());
        assert!(log.contains(&"UserEvent".to_string()), "a pre-posted event was dropped: {log:?}");
    }

    /// A worker released after Init delivers its event under ControlFlow::Wait.
    /// The handoff does not establish whether the kernel poll has blocked yet.
    #[test]
    fn an_event_posted_after_init_is_delivered_from_another_thread_under_wait() {
        let loop_: EventLoop<u32> = EventLoop::new().unwrap();
        let proxy = loop_.create_proxy();
        // The worker starts after the first turn, with the loop set to Wait.
        // Only its proxy event or the driver's hang detector ends the wait.
        loop_.window_target().p.set_control_flow(ControlFlow::Wait);
        let (start, ready) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            ready.recv_timeout(Duration::from_secs(60)).expect("the first turn");
            proxy.send_event(11).expect("live loop");
        });
        let log = drive(loop_, |target, turn| {
            if turn == 1 {
                start.send(()).expect("the proxy worker");
            } else {
                target.p.exit();
            }
        });
        worker.join().expect("the proxy worker");
        assert!(log.contains(&"UserEvent".to_string()), "a threaded wake never arrived: {log:?}");
    }

    /// An already due WaitUntil produces ResumeTimeReached on the next turn.
    #[test]
    fn a_due_wait_until_reports_resume_time_reached() {
        let loop_: EventLoop<()> = EventLoop::new().unwrap();
        let deadline = Instant::now();
        loop_.window_target().p.set_control_flow(ControlFlow::WaitUntil(deadline));
        let log = drive(loop_, |target, turn| {
            if turn >= 2 {
                target.p.exit();
            }
        });
        assert!(
            log.iter().any(|l| l.contains("ResumeTimeReached")),
            "an elapsed deadline must report ResumeTimeReached, got {log:?}"
        );
    }

    /// Exit requested during the very first turn must be honoured without running a
    /// second one, matching the other backends.
    #[test]
    fn an_exit_on_the_init_turn_runs_no_further_turn() {
        let loop_: EventLoop<()> = EventLoop::new().unwrap();
        let log = drive(loop_, |target, _| target.p.exit());
        assert_eq!(
            log.iter().filter(|l| l.starts_with("NewEvents")).count(),
            1,
            "a loop told to exit on Init ran another turn: {log:?}"
        );
    }
}
