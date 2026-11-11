// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

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

/// A proxy's half of the loop: the queue carries the events, the waker only "something was posted"
/// (`common::poll::Waker`, which never blocks the sender).
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
        let Ok(mut queue) = self.queue.lock() else {
            return Err(EventLoopClosed(event));
        };
        queue.push_back(event);
        drop(queue);
        self.waker.wake();
        Ok(())
    }
}

/// The windowless twin of the X11/Wayland `ActiveEventLoop`.
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

    #[cfg(wayland_platform)]
    #[inline]
    pub(crate) fn set_exit_code(&self, code: i32) {
        self.exit.set(Some(code));
    }

    #[inline]
    pub(crate) fn exit_code(&self) -> Option<i32> {
        self.exit.get()
    }
}

pub struct EventLoop<T: 'static> {
    target: RootELW,
    queue: Arc<Mutex<VecDeque<T>>>,
    waker: Waker,
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

        let _ = poll::wait(&mut [poll::readable(self.waker.as_raw_fd())], deadline);
        self.waker.drain();

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
#[path = "headless_tests.rs"]
mod tests;
