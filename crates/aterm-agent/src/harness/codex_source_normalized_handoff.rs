// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native phase custody for a normalized source. No lifetime retirement or
//! source permission is inferred from a quiescence report or lock identifier.
use super::*;

pub(super) fn same_materialization(a: &slot::SourceHandle, b: &slot::SourceHandle) -> bool {
    a.binding() == b.binding() && a.slot() == b.slot()
        && a.assignment_generation() == b.assignment_generation()
        && a.source_authority() == b.source_authority()
        && a.account() == b.account() && a.user() == b.user()
        // Actual claimed physical identities join path aliases; spelling a
        // different path alone can neither split nor join a source family.
        && a.namespaces().zip(b.namespaces()).is_some_and(|(a, b)| a == b)
}

pub(in crate::harness) struct FamilyHandoff {
    normal: Arc<NormalizedTarget>,
    source: Arc<slot::SourceHandle>,
}
impl FamilyHandoff {
    pub(in crate::harness) fn resource(
        &self,
    ) -> &crate::harness::codex_source_gate::TargetFamilyLease {
        &self.normal.installed.resource
    }
    pub(in crate::harness) fn restore(
        &self,
        permit: &crate::harness::codex_assignment::prepare::cancel::CancelledSource<'_>,
    ) -> Result<(), Error> {
        if !self.normal.active.load(Ordering::Acquire) {
            return Err(Error::Stale);
        }
        self.normal
            .installed
            .resource
            .restore_after_cancel(permit, &self.source)
            .map_err(|error| {
                if error.contains("busy") {
                    Error::Busy
                } else {
                    Error::Unavailable
                }
            })
    }
    pub(in crate::harness) fn matches(&self, source: &slot::SourceHandle) -> bool {
        self.normal.active.load(Ordering::Acquire)
            && same_materialization(&self.source, source)
            && same_materialization(&self.normal.installed.source, source)
    }
}

/// Called under Registry State. The subsequent sealing snapshot authenticates
/// the actual current Book/Slot and all native capture retirement outside State.
pub(in crate::harness) fn capture_family(
    state: &State,
    caller: &slot::Caller,
    source: &Arc<slot::SourceHandle>,
    gate: &SourceGateIdentity,
) -> Result<Option<Arc<FamilyHandoff>>, Error> {
    let mut result = None;
    for normal in state.normalized_targets.values().filter(|normal| {
        normal.installed.assignment.caller() == caller
            && normal.active.load(Ordering::Acquire)
            && same_materialization(&normal.installed.source, source)
    }) {
        if result.is_some()
            || !normal.retirement.retired()
            || !normal.applied.matches(&normal.installed.assignment)
            || normal.installed.resource.gate_identity() != gate
        {
            return Err(Error::Stale);
        }
        result = Some(Arc::new(FamilyHandoff {
            normal: normal.clone(),
            source: source.clone(),
        }));
    }
    Ok(result)
}

pub(in crate::harness) fn source_revoked(
    state: &State,
    caller: &slot::Caller,
    member: &str,
) -> bool {
    let Some(member) = member_key(caller, member)
        .ok()
        .and_then(|key| state.members.get(&key))
    else {
        return false;
    };
    source_is_revoked(state, caller, &member.source)
}

pub(in crate::harness) fn source_is_revoked(
    state: &State,
    caller: &slot::Caller,
    source: &slot::SourceHandle,
) -> bool {
    state.normalized_targets.values().any(|normal| {
        normal.installed.assignment.caller() == caller
            && !normal.active.load(Ordering::Acquire)
            && same_materialization(&normal.installed.source, source)
    })
}

/// This is a retained native revocation, not a disappearing member or an enum
/// claiming independence. A fresh successor Book must still authenticate it.
pub(in crate::harness) struct RevokedMember {
    normal: Arc<NormalizedTarget>,
    successor: Arc<AppliedReceipt>,
    source: Arc<slot::SourceHandle>,
    registration: String,
    version: u64,
}
impl RevokedMember {
    pub(in crate::harness) fn capture(
        state: &State,
        caller: &slot::Caller,
        member: &Member,
    ) -> Result<Option<Self>, Error> {
        for normal in state.normalized_targets.values() {
            if normal.installed.assignment.caller() != caller
                || normal.active.load(Ordering::Acquire)
                || !same_materialization(&normal.installed.source, &member.source)
            {
                continue;
            }
            let successor = lock(&normal.revoked_by)?.clone().ok_or(Error::Stale)?;
            return Ok(Some(Self {
                normal: normal.clone(),
                successor,
                source: member.source.clone(),
                registration: member.registration.clone(),
                version: member.version,
            }));
        }
        Ok(None)
    }
    pub(in crate::harness) fn probe(&self, current: &slot::SourceHandle) -> Result<(), Error> {
        if self.normal.active.load(Ordering::Acquire) || !self.successor.matches_source(current) {
            return Err(Error::Stale);
        }
        self.successor
            .with_current_book(|namespace| {
                if current
                    .namespaces()
                    .is_none_or(|scope| scope.book() != namespace)
                    || self
                        .source
                        .namespaces()
                        .is_none_or(|scope| scope.book() != namespace)
                {
                    return Err("revoked source Book changed".into());
                }
                Ok(())
            })
            .map_err(|_| Error::Stale)
    }
    pub(in crate::harness) fn matches(&self, member: &Member) -> bool {
        !self.normal.active.load(Ordering::Acquire)
            && Arc::ptr_eq(&self.source, &member.source)
            && self.registration == member.registration
            && self.version == member.version
            && lock(&self.normal.revoked_by).is_ok_and(|current| {
                current
                    .as_ref()
                    .is_some_and(|receipt| Arc::ptr_eq(receipt, &self.successor))
            })
    }
}

/// Actual Applied successor and native source phase, under the already claimed
/// successor Book. Callers release State before touching the physical family.
pub(in crate::harness) fn revoke_before_handoff(
    state: &State,
    predecessor: Option<&Arc<FamilyHandoff>>,
    successor: &Arc<AppliedReceipt>,
    namespace: &assignment::BookNamespace,
) -> Result<(), Error> {
    if let Some(predecessor) = predecessor {
        let normal = &predecessor.normal;
        if normal.installed.assignment.caller() != successor.caller()
            || normal
                .installed
                .source
                .namespaces()
                .is_none_or(|scope| scope.book() != namespace)
        {
            return Err(Error::Stale);
        }
        let mut revoked = lock(&normal.revoked_by)?;
        normal.active.store(false, Ordering::Release);
        *revoked = Some(successor.clone());
    }
    for normal in state.normalized_targets.values().filter(|normal| {
        normal.installed.assignment.caller() == successor.caller()
            && !normal.active.load(Ordering::Acquire)
            && normal
                .installed
                .source
                .namespaces()
                .is_some_and(|scope| scope.book() == namespace)
    }) {
        *lock(&normal.revoked_by)? = Some(successor.clone());
    }
    Ok(())
}
