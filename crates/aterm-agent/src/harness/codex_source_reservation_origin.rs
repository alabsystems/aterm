// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Retained source metadata for reservation phases after member retirement.
//! This snapshot supplies neither a census nor kernel exclusion authority.

use super::*;

#[derive(Clone)]
pub(super) struct Origin {
    caller: slot::Caller,
    process: Process,
    version: u64,
    registration: String,
    connection: Connection,
    source: Arc<slot::SourceHandle>,
    gate: SourceGateIdentity,
    role: Option<ReadyRole>,
}

impl Origin {
    pub(super) fn caller(&self) -> &slot::Caller {
        &self.caller
    }
    pub(super) fn source(&self) -> &Arc<slot::SourceHandle> {
        &self.source
    }
    pub(super) fn gate(&self) -> &SourceGateIdentity {
        &self.gate
    }

    pub(super) fn capture(member: &Member) -> Self {
        Self {
            caller: member.caller.clone(),
            process: member.process.clone(),
            version: member.version,
            registration: member.registration.clone(),
            connection: member.connection.clone(),
            source: member.source.clone(),
            gate: member.readiness.identity.clone(),
            role: member.readiness.role,
        }
    }

    /// Physical probes run only after the registry's state guard is released.
    pub(super) fn current(&self) -> bool {
        Process::observe(self.caller.pid).as_ref() == Some(&self.process) && self.gate.current()
    }

    pub(super) fn matches_member(&self, member: &Member) -> bool {
        self.caller == member.caller
            && self.process == member.process
            && self.version == member.version
            && self.registration == member.registration
            && self.connection.same(&member.connection)
            && member.connection.is_live()
            && Arc::ptr_eq(&self.source, &member.source)
            && self.gate == member.readiness.identity
            && self.role == member.readiness.role
            && member.readiness.manager()
            && !member.unknown
    }

    /// Credential C may advance, but the immutable grant/assignment may not.
    pub(super) fn same_source(&self, other: &Self) -> bool {
        self.caller == other.caller
            && self.process == other.process
            && self.gate == other.gate
            && self.source.binding() == other.source.binding()
            && self.source.source_authority() == other.source.source_authority()
            && match (self.source.namespaces(), other.source.namespaces()) {
                (Some(a), Some(b)) => a.shares_storage(b),
                (None, None) => true,
                _ => false,
            }
    }

    pub(super) fn matches_authority(&self, source: &slot::SourceHandle) -> bool {
        self.source.binding() == source.binding()
            && self.source.source_authority() == source.source_authority()
    }
}
