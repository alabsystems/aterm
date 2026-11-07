// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only funded execution scope observations. No decoded object here can
//! acquire, adopt, launch, signal, reap or release any resource.
use crate::{
    CohortClosureRef,
    cohort::{Lifetime, RecordedCohort, Scope},
};
use aterm_uds::process::{self, Ancestry, Identity};

/// Whether an actual stable parent path overlaps a retained funded scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FundedMembership {
    /// At least one original live holder, root or group covers the caller.
    Inside,
    /// Every represented scope is current and distinct from the caller.
    Outside,
    /// An unbound birth, unknown scope or changed native observation remains.
    Unknown,
}

/// Original checkpoint-backed claim scope. This is observational data, never
/// a capability or a proof of closure. Unarmed and uncertain debt stays visible.
#[derive(Clone, Debug)]
pub struct FundedScope {
    pub(crate) claim: CohortClosureRef,
    pub(crate) scope: Scope,
    pub(crate) holder: Option<Identity>,
    pub(crate) groups: Vec<RecordedCohort>,
    pub(crate) unresolved: bool,
}
impl FundedScope {
    /// Original epoch and guard, for comparison with an actually held claim.
    /// Its decoded spelling never authorizes excluding another reservation.
    #[must_use]
    pub fn claim(&self) -> &CohortClosureRef {
        &self.claim
    }

    /// Compare native identities/groups with the complete actual caller path.
    /// The caller must reread the ancestry after checking the complete inventory.
    #[must_use]
    pub fn membership(&self, ancestry: &Ancestry) -> FundedMembership {
        use FundedMembership::{Inside, Outside, Unknown};
        match self.scope.ended_or_unknown() {
            Some(Lifetime::Drained) => return Outside,
            Some(_) => return Unknown,
            None => {}
        }
        let mut unknown = self.unresolved;
        if let Some(holder) = self.holder {
            if process::read_identity(holder.pid) != Some(holder) {
                unknown = true;
            } else if ancestry.nodes().iter().any(|node| node.identity == holder) {
                return Inside;
            }
        }
        for group in &self.groups {
            match group.membership(ancestry) {
                Inside => return Inside,
                Unknown => unknown = true,
                Outside => {}
            }
        }
        if unknown { Unknown } else { Outside }
    }
}
