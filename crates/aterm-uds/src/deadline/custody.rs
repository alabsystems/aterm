// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual-child ownership and retained resources across bounded reap attempts.

use super::ExitWait;
use crate::spawnfd::Child;
use std::fmt;
use std::io;
use std::process::ExitStatus;
use std::time::Instant;

#[path = "custody_decision.rs"]
mod decision;

/// Observed completion of this custody's original child.
/// This value supplies no authority to reap, signal or release another child's resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildCompletion {
    /// No child was returned by the attempted spawn.
    NotStarted,
    /// No positive owning reap has been observed here.
    Pending,
    /// The actual child returned this status from its owning wait.
    /// This proves neither descendant retirement nor complete output collection.
    Reaped(ExitStatus),
}

/// The original child and resources that must remain owned until its positive reap.
///
/// Bounded observation never releases `T`. The caller may recover it after a positive
/// owning wait, or let custody keep it in the background until that wait succeeds.
/// Guards requiring stronger evidence, such as descendant retirement or an executable's
/// successful start witness, need their own stronger owner.
#[must_use = "pending custody retains its child and resources until a positive owning reap"]
pub struct ChildCustody<T: Send + 'static = ()> {
    child: Option<Box<Child>>,
    exit: Option<ExitWait>,
    retained: Option<T>,
    reaped: Option<ExitStatus>,
    started: bool,
}

impl<T: Send + 'static> ChildCustody<T> {
    pub(super) fn not_started(retained: T) -> Self {
        Self {
            child: None,
            exit: None,
            retained: Some(retained),
            reaped: None,
            started: false,
        }
    }

    pub(super) fn from_child(child: Child, retained: T) -> Self {
        // An already-reaped child's old pid is no authority to register a new watcher.
        // Its cached status will be established by the same owning poll as any other child.
        let exit = (!child.is_reaped()).then(|| ExitWait::new(&child));
        Self {
            child: Some(Box::new(child)),
            exit,
            retained: Some(retained),
            reaped: None,
            started: true,
        }
    }

    /// The completion observed by this custody, with no transferable release authority.
    #[must_use]
    pub const fn completion(&self) -> ChildCompletion {
        if !self.started {
            ChildCompletion::NotStarted
        } else if let Some(status) = self.reaped {
            ChildCompletion::Reaped(status)
        } else {
            ChildCompletion::Pending
        }
    }

    /// Poll this actual child until `by`, retaining all resources on every outcome.
    /// `None` means no child started or no owning reap was observed by the deadline.
    /// An already reaped child's positive status is cached for subsequent observations.
    ///
    /// # Errors
    /// The actual child's wait or foreground restoration failed. Its entire child handle,
    /// terminal custody and retained resources stay owned here for a later attempt.
    pub fn observe_until(&mut self, by: Instant) -> io::Result<Option<ExitStatus>> {
        loop {
            if let Some(status) = self.poll_with(Child::try_wait)? {
                return Ok(Some(status));
            }
            let left = by.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let Some(exit) = self.exit.as_mut() else {
                return Ok(None);
            };
            exit.wait(left);
        }
    }

    fn poll_with(
        &mut self,
        poll: impl FnOnce(&mut Child) -> io::Result<Option<ExitStatus>>,
    ) -> io::Result<Option<ExitStatus>> {
        if decision::may_release(self.reaped.is_some(), false, false) {
            return Ok(self.reaped);
        }
        let Some(child) = self.child.as_deref_mut() else {
            return Ok(None);
        };
        let observed = poll(child);
        if decision::may_release(false, observed.is_ok(), matches!(&observed, Ok(Some(_))))
            && let Ok(Some(status)) = &observed
        {
            self.reaped = Some(*status);
            self.exit = None;
        }
        observed
    }

    /// Recover the retained resources only if no child started or its positive reap is known.
    ///
    /// # Errors
    /// Returns the entire original custody unchanged when no positive reap is known.
    pub fn try_into_retained(mut self) -> Result<T, Box<Self>> {
        if !self.started || decision::may_release(self.reaped.is_some(), false, false) {
            Ok(self.retained.take().expect("custody retains its resources"))
        } else {
            Err(Box::new(self))
        }
    }

    /// Explicitly leave pending child observation and its retained resources to the reaper.
    /// This supplies no receipt of completed reaping or descendant retirement.
    pub fn reap_in_background(self) {
        drop(self);
    }

    fn take_reaper(&mut self) -> Option<impl FnMut() -> bool + Send + 'static + use<T>> {
        if !self.started || decision::may_release(self.reaped.is_some(), false, false) {
            return None;
        }
        let mut child = self.child.take()?;
        let retained = self.retained.take();
        Some(move || {
            let _keep = &retained;
            let observed = child.try_wait();
            decision::may_release(false, observed.is_ok(), matches!(observed, Ok(Some(_))))
        })
    }
}

impl<T: Send + 'static> fmt::Debug for ChildCustody<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChildCustody")
            .field("completion", &self.completion())
            .finish_non_exhaustive()
    }
}

impl<T: Send + 'static> Drop for ChildCustody<T> {
    fn drop(&mut self) {
        if let Some(reap) = self.take_reaper() {
            crate::reaper::hand_off(reap);
        }
    }
}

#[cfg(all(test, unix))]
#[path = "custody_tests.rs"]
mod tests;
