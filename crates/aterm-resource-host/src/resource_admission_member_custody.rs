// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Borrow the actual original claim and child. A member never owns a second
//! memory grant, and cleanup never reconstructs control from an observation.
use super::*;
use aterm_capacity::cohort::launch::{CohortChild, LaunchError};
use aterm_capacity::retained_cohort::CancelError;

pub(super) struct Borrowed<'a> {
    pub job: &'a mut Job,
    pub fence: &'a Fence,
    pub debt: &'a ExternalDebt,
    pub peer: Peer,
    pub proof: Option<&'a crate::resource_producer::TransportProof>,
}
impl member::MemberCustody for Borrowed<'_> {
    fn launch(&mut self, command: &mut spawnfd::Command) -> Result<CohortChild, LaunchError> {
        self.validate().map_err(LaunchError::Refused)?;
        match &mut self.job.custody {
            Custody::Fixed(custody) => {
                custody.launch_subgroup(self.fence, &mut self.job.child, command)
            }
            Custody::Cooperative(claim) => claim.launch_subgroup(&mut self.job.child, command),
        }
    }
    fn bind(&mut self, child: spawnfd::Spawned) -> Result<CohortChild, LaunchError> {
        // A dead caller/root forbids GO, but must not discard an actual held
        // child whose durable BIND needs retrying for cancellation and drain.
        match &mut self.job.custody {
            Custody::Fixed(custody) => custody.bind_subgroup(self.fence, child),
            Custody::Cooperative(claim) => {
                if let Err(error) = self.fence.check_claim(claim) {
                    return Err(LaunchError::Unbound {
                        spawned: Box::new(child),
                        error,
                    });
                }
                claim.bind_subgroup(child)
            }
        }
    }
    fn cancel_no_child(&mut self, proof: spawnfd::NotSpawned) -> Result<(), CancelError> {
        match &mut self.job.custody {
            Custody::Fixed(custody) => custody.cancel_subgroup(self.fence, proof),
            Custody::Cooperative(claim) => {
                if let Err(error) = self.fence.check_claim(claim) {
                    return Err(CancelError::Refused {
                        error,
                        proof: Box::new(proof),
                    });
                }
                // Once accepted the claim retains the original receipt before
                // its first persistence attempt, including on an I/O failure.
                claim.cancel_subgroup(proof).map_err(CancelError::Retained)
            }
        }
    }
    fn retire(&mut self, child: &CohortChild) -> io::Result<()> {
        match &mut self.job.custody {
            Custody::Fixed(custody) => custody.retire_subgroup(self.fence, child),
            Custody::Cooperative(claim) => {
                self.fence.check_claim(claim)?;
                claim.retire_subgroup(child)
            }
        }
    }
    fn validate(&mut self) -> io::Result<()> {
        if let Some(proof) = self.proof {
            proof.validate()?;
        }
        self.fence.check_current()?;
        if !self.job.launched
            || self.job.reaped
            || self.job.settled
            || self.job.cleanup_at.is_some()
            || self.job.child.child_mut().exited()?
            || !members::covered(self.job, self.peer)
            || !members::trusted_caller(self.job, self.peer)
        {
            return Err(refused(
                "original member caller/root is no longer executable",
            ));
        }
        job::inspection(self.fence, self.debt, self.job)?;
        Ok(())
    }
}
