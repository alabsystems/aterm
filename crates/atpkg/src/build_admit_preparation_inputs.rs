// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual input-lifetime resources installed before an independently admitted job.
//! This is no Bootstrap endpoint, canonical seed/source validation, SDK selector,
//! original GNU handoff, authenticated client or successful availability receipt.
//! Those producers must supply their own actual capabilities before that route
//! can be connected. Existing raw Darwin/full Bootstrap refusal remains necessary.
//! This is process-local retention: SIGKILL cannot run a quarantine destructor,
//! and host death releases the Cargo cache's shared kernel lock. Durable lifecycle
//! data is never adopted as a live owner or reclaim permission. A full resident
//! protocol still needs host-failure quarantine covering every resource provider.

use super::{Held, Owner, RetainedAccepted, RetainedRefused};
use crate::{
    cargo_cache::SourceCacheGuard,
    private_input::{InputUse, Refused},
};
use aterm_uds::spawnfd;
use std::{
    io,
    time::{Duration, Instant},
};

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

impl Owner {
    /// Install actual input guards before the native target can be born.
    /// The caller still supplies independently admitted `Held`. Publication,
    /// launch, wait or client loss does not release accepted resources before
    /// the original owner establishes positive native cohort settlement.
    ///
    /// This method supplies lifetime retention only. It does not certify source,
    /// seed or SDK meaning, transfer GNU credits, or expose full preparation.
    /// # Errors
    /// Original input/deadline or native pre-dispatch admission refuses. Every
    /// original resource is returned; accepted publication failures remain owned.
    pub fn submit_retained_inputs(
        &mut self,
        held: Held,
        command: &mut spawnfd::Command,
        grace: Duration,
        inputs: PreparationInputs,
    ) -> Result<RetainedAccepted, RetainedRefused<PreparationInputs>> {
        if let Err(error) = inputs.check_current() {
            return Err(RetainedRefused::new(error, held, inputs));
        }
        self.submit_retained(held, command, grace, inputs)
    }
}
