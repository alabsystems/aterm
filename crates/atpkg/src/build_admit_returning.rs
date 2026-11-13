// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native completion may return the SAME original materialization resources.
//! Serialized observations, a successful different job, or edited public result
//! fields cannot create this handoff. Content/source/seed validation is separate.

use super::{Completion, JobOutcome};
use std::{any::Any, fmt};

#[derive(Clone, Copy)]
pub(super) enum ReturnMode {
    DropAfterSettlement,
    ReturnAfterSuccess,
}

pub(super) struct Retained {
    mode: ReturnMode,
    resources: Box<dyn Any + Send>,
}

impl Retained {
    pub(super) fn new<T: Send + 'static>(mode: ReturnMode, resources: T) -> Self {
        Self {
            mode,
            resources: Box::new(resources),
        }
    }

    pub(super) fn complete(self, original: Option<JobOutcome>) -> Option<Returned> {
        match self.mode {
            ReturnMode::DropAfterSettlement => None,
            ReturnMode::ReturnAfterSuccess => Some(Returned {
                process: std::process::id(),
                original,
                resources: Some(self.resources),
            }),
        }
    }
}

pub(super) struct Returned {
    pub(super) process: u32,
    original: Option<JobOutcome>,
    resources: Option<Box<dyn Any + Send>>,
}

impl fmt::Debug for Returned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Returned")
            .field("process", &self.process)
            .field("original", &self.original)
            .finish_non_exhaustive()
    }
}

impl Drop for Returned {
    fn drop(&mut self) {
        if self.process != std::process::id() {
            // A fork copy must not run arbitrary resource destructors such as
            // LOCK_UN on the parent's shared open-file description. Its copied
            // descriptors close only at process exit; it cannot recover custody.
            std::mem::forget(self.resources.take());
        }
    }
}

impl Completion {
    /// Recover the original resource bundle from a returning submission, only
    /// after that actual job succeeded and its original native cohort drained.
    /// The caller must still perform its source/seed/content postchecks before
    /// advancing a materialization producer. This grants no SDK or proof grade.
    ///
    /// # Errors
    /// Returns this same completion, still holding its resources, if the original
    /// native result failed, the submission did not request a return, this is a
    /// foreign process copy, or `T` is not the original bundle's type. Public
    /// result fields are observations and changing them cannot authorize a
    /// return. Original-process drop quarantines a retained private-input
    /// producer through its normal destructor; foreign drop retains the copy.
    pub fn try_into_retained<T: Send + 'static>(mut self) -> Result<T, Self> {
        let Some(returned) = &self.returned else {
            return Err(self);
        };
        if returned.process != std::process::id()
            || !returned.original.is_some_and(JobOutcome::succeeded)
            || !returned
                .resources
                .as_ref()
                .is_some_and(|resources| resources.is::<T>())
        {
            return Err(self);
        }
        let mut returned = self
            .returned
            .take()
            .expect("same checked native completion");
        Ok(*returned
            .resources
            .take()
            .expect("same checked original resources")
            .downcast::<T>()
            .expect("same checked resource type"))
    }
}
