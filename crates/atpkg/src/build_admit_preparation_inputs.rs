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

use super::{Held, Owner, PreparationInputs, RetainedAccepted, RetainedRefused};
use aterm_uds::spawnfd;
use std::time::Duration;

impl Owner {
    /// Retain the same fresh materialization producer and private cache before
    /// dispatch. Only this native job's successful completion and original
    /// cohort settlement can return that exact bundle through
    /// `Completion::try_into_retained`. No content, source or SDK is certified.
    ///
    /// # Errors
    /// Original guards/deadline or native pre-dispatch admission refuses. The
    /// original admission and attempt are returned; accepted failures stay owned.
    pub fn submit_materialization(
        &mut self,
        held: Held,
        command: &mut spawnfd::Command,
        grace: Duration,
        attempt: crate::private_input::MaterializationAttempt,
    ) -> Result<RetainedAccepted, RetainedRefused<crate::private_input::MaterializationAttempt>>
    {
        if let Err(error) = attempt.check_current() {
            return Err(RetainedRefused::new(error, held, attempt));
        }
        self.submit_returning(held, command, grace, attempt)
    }

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
