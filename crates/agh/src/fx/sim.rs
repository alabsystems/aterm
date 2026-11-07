// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The simulated world behind `Fx::simulated`: a test's time, moved only by sleeps, and its
//! scripted children with the log of every child spec the doors were handed.

use std::collections::VecDeque;
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

#[cfg(unix)]
use super::child::ChildOut;
use super::child::{ChildSpec, ScriptedChild};

#[cfg(unix)]
type AfterGit = Box<dyn FnOnce(&mut crate::error::Result<ChildOut>) + Send>;

#[cfg(unix)]
pub(super) type NativePushObserver = Arc<dyn Fn(super::NativePushTiming) + Send + Sync>;

/// One exact real Git child around which a fixture changes its local world or
/// loses an acknowledgement. It is consumed before invoking either callback.
#[cfg(unix)]
pub(super) struct GitHook {
    pub(super) program: PathBuf,
    pub(super) args: Vec<OsString>,
    pub(super) before: Box<dyn FnOnce() + Send>,
    pub(super) after: AfterGit,
}

/// Simulated monotonic and wall clock advanced only by `sleep`.
pub(super) struct SimClock {
    start_unix_secs: u64,
    /// The time slept so far, and each sleep in order.
    state: Mutex<(Duration, Vec<Duration>)>,
    /// The children `Fx::child` answers from, in order.
    scripted: Mutex<VecDeque<ScriptedChild>>,
    /// Every spec `Fx::child` or `Fx::git` accepted (one `child::check` passed), in call order.
    run: Mutex<Vec<ChildSpec>>,
    #[cfg(unix)]
    git_hook: Mutex<Option<GitHook>>,
    #[cfg(unix)]
    native_push_observer: Mutex<Option<NativePushObserver>>,
}

impl SimClock {
    /// Both clocks at their start, no scripted child, an empty log.
    pub(super) const fn new(start_unix_secs: u64) -> SimClock {
        SimClock {
            start_unix_secs,
            state: Mutex::new((Duration::ZERO, Vec::new())),
            scripted: Mutex::new(VecDeque::new()),
            run: Mutex::new(Vec::new()),
            #[cfg(unix)]
            git_hook: Mutex::new(None),
            #[cfg(unix)]
            native_push_observer: Mutex::new(None),
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

    #[cfg(unix)]
    pub(super) fn set_git_hook(&self, hook: GitHook) {
        let mut slot = self.git_hook.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(slot.is_none(), "one pending Git interception");
        *slot = Some(hook);
    }

    #[cfg(unix)]
    pub(super) fn take_git_hook(&self, spec: &ChildSpec) -> Option<GitHook> {
        let mut slot = self.git_hook.lock().unwrap_or_else(PoisonError::into_inner);
        if slot
            .as_ref()
            .is_some_and(|h| h.program == spec.program && h.args == spec.args)
        {
            slot.take()
        } else {
            None
        }
    }

    #[cfg(unix)]
    pub(super) fn set_native_push_observer(&self, observer: NativePushObserver) {
        let mut slot = self
            .native_push_observer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        assert!(slot.is_none(), "one native push observer");
        *slot = Some(observer);
    }

    #[cfg(unix)]
    pub(super) fn native_push_observer(&self) -> Option<NativePushObserver> {
        self.native_push_observer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}
