// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The simulated world behind `Fx::simulated`: a test's time, moved only by sleeps, and its
//! scripted children with the log of every child spec the doors were handed.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use super::child::{ChildSpec, ScriptedChild};

/// Simulated monotonic and wall clock advanced only by `sleep`.
pub(super) struct SimClock {
    start_unix_secs: u64,
    /// The time slept so far, and each sleep in order.
    state: Mutex<(Duration, Vec<Duration>)>,
    /// The children `Fx::child` answers from, in order.
    scripted: Mutex<VecDeque<ScriptedChild>>,
    /// Every spec `Fx::child` or `Fx::git` accepted (one `child::check` passed), in call order.
    run: Mutex<Vec<ChildSpec>>,
}

impl SimClock {
    /// Both clocks at their start, no scripted child, an empty log.
    pub(super) const fn new(start_unix_secs: u64) -> SimClock {
        SimClock {
            start_unix_secs,
            state: Mutex::new((Duration::ZERO, Vec::new())),
            scripted: Mutex::new(VecDeque::new()),
            run: Mutex::new(Vec::new()),
        }
    }

    /// The time slept so far.
    pub(super) fn mono(&self) -> Duration {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0
    }

    /// `start_unix_secs` plus the whole seconds slept.
    pub(super) fn wall_secs(&self) -> u64 {
        self.start_unix_secs.saturating_add(self.mono().as_secs())
    }

    /// Move both clocks by `d`, taking no real time.
    pub(super) fn sleep(&self, d: Duration) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 = state.0.saturating_add(d);
        state.1.push(d);
    }

    /// Every sleep so far, in order.
    pub(super) fn slept(&self) -> Vec<Duration> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .clone()
    }

    /// Queue `children` after any already scripted.
    pub(super) fn script(&self, children: Vec<ScriptedChild>) {
        self.scripted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(children);
    }

    /// The next scripted child, if any.
    pub(super) fn next_child(&self) -> Option<ScriptedChild> {
        self.scripted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
    }

    /// Log `spec` as run.
    pub(super) fn log(&self, spec: &ChildSpec) {
        self.run
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(spec.clone());
    }

    /// Every spec logged so far, in call order.
    pub(super) fn children_run(&self) -> Vec<ChildSpec> {
        self.run
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}
