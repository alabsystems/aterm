// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual private input/cache lifetimes, independent of a native dispatcher.
//! This creates no source meaning, seed, SDK, execution, GNU or proof authority.

use super::{InputUse, Refused};
use crate::cargo_cache::SourceCacheGuard;
use std::{io, time::Instant};

const MAX_INPUTS: usize = 8;

/// Original guards, not input roles or content credentials. Neither serialization
/// nor an existing path constructs one of its actual private-input use guards.
#[must_use = "retain the actual input guards through native cohort settlement"]
pub struct PreparationInputs {
    inputs: Vec<InputUse>,
    cache: SourceCacheGuard,
    deadline: Instant,
}

impl PreparationInputs {
    /// Bind a bounded actual guard bundle to the caller's original endpoint.
    /// Input names/bytes are not examined or certified here.
    ///
    /// # Errors
    /// Empty/over-limit bundle, expired endpoint, or original guard drift. A
    /// refusal returns every original input; it has dispatched no target.
    pub fn new(
        inputs: Vec<InputUse>,
        cache: SourceCacheGuard,
        deadline: Instant,
    ) -> Result<Self, Refused<(Vec<InputUse>, SourceCacheGuard)>> {
        let bundle = Self {
            inputs,
            cache,
            deadline,
        };
        match bundle.check_current() {
            Ok(()) => Ok(bundle),
            Err(error) => Err(Refused::new(error, (bundle.inputs, bundle.cache))),
        }
    }

    /// Recheck the same original guards under the same finite endpoint.
    /// This does not inspect recursive content or establish execution authority.
    /// # Errors
    /// The bundle bound, deadline or any original lifecycle/cache guard refuses.
    pub fn check_current(&self) -> io::Result<()> {
        if self.inputs.is_empty() || self.inputs.len() > MAX_INPUTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "retained input bundle requires 1..=8 actual uses",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        for input in &self.inputs {
            input.check_current()?;
        }
        self.cache.check_current()?;
        if Instant::now() >= self.deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}
