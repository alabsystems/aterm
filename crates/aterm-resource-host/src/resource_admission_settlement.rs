// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original root settlement, minted by the actual native owner after closure.

use super::*;

/// A live, non-cloneable owner result. There is no constructor from a binding,
/// PID, event, acknowledgement, journal record or observed process exit.
/// This result does not itself implement build-input release or transfer.
pub(super) struct ClosedRoot {
    binding: Binding,
    group: i32,
    leader_serial: u64,
    birth: u64,
}

impl ClosedRoot {
    pub(super) fn matches(&self, job: &Job) -> bool {
        self.binding == job.binding
            && self.group == job.child.cohort().group()
            && self.leader_serial == job.child.cohort().leader_serial()
            && self.birth == job.root_birth
    }

    pub(super) fn matches_original(
        &self,
        binding: &Binding,
        group: i32,
        leader_serial: u64,
        birth: u64,
    ) -> bool {
        self.binding == *binding
            && self.group == group
            && self.leader_serial == leader_serial
            && self.birth == birth
    }
}

/// Keep the one original result on its owning Job. Failed or repeated attempts
/// cannot manufacture another result or infer closure from durable bookkeeping.
pub(super) fn settle(job: &mut Job, fence: &Fence, journal: &journal::Journal) -> io::Result<()> {
    if job.closed.is_some()
        || !job.reaped
        || !job.members.is_empty()
        || !job.loans.is_empty()
        || !job.runtime_inputs.preparation_finished()
        || job
            .workload
            .as_ref()
            .is_some_and(|state| !state.settlement_ready())
    {
        return Err(refused(
            "original root has unsettled lifecycle ownership or already has a settlement result",
        ));
    }
    let group = job.child.cohort().group();
    let leader_serial = job.child.cohort().leader_serial();
    let birth = job.root_birth;
    match &mut job.custody {
        Custody::Fixed(custody) => custody.try_finish(fence)?,
        Custody::Cooperative(claim) => claim.finish_cohort()?,
    }
    if let Some(state) = &job.workload {
        state.primary.settle()?;
    }
    journal.settle(&job.binding)?;
    let closed = ClosedRoot {
        binding: job.binding.clone(),
        group,
        leader_serial,
        birth,
    };
    job.runtime_inputs.release(&closed)?;
    job.closed = Some(closed);
    Ok(())
}
