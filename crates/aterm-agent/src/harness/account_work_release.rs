// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Counted release-only operations. There is deliberately no Scope conversion
//! or work/source admission. A caller must separately prove its cleanup role.

use super::*;

#[derive(Clone)]
pub(in crate::harness) struct ReleaseOperation(Arc<Operation>);
#[derive(Clone)]
pub(in crate::harness) struct ReleaseMonitor {
    identity: OperationIdentity,
    owner: Weak<Shared>,
    status: Arc<Status>,
}
impl ReleaseOperation {
    pub(in crate::harness) fn operation_identity(&self) -> OperationIdentity {
        OperationIdentity(Arc::downgrade(&self.0))
    }
    pub(in crate::harness) fn monitor(&self) -> ReleaseMonitor {
        ReleaseMonitor {
            identity: self.operation_identity(),
            owner: Arc::downgrade(&self.0.shared),
            status: self.0.status.clone(),
        }
    }
}
impl ReleaseMonitor {
    pub(in crate::harness) fn operation_identity(&self) -> &OperationIdentity {
        &self.identity
    }
    pub(in crate::harness) fn retired(&self) -> bool {
        self.status.retired.load(Ordering::Acquire)
    }
    pub(in crate::harness) fn owned_by(&self, lifetime: &Lifetime) -> bool {
        Weak::ptr_eq(&self.owner, &Arc::downgrade(&lifetime.0))
    }
}
impl Lifetime {
    /// Reserve Pending cleanup plus settlement accounting in this still-current
    /// epoch. The result conveys no native, provider or credential authority.
    pub(in crate::harness) fn reserve_release_operation(
        &self,
    ) -> Option<(ReleaseOperation, ReleaseTurn)> {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.sealed {
            return None;
        }
        state.operations = state.operations.checked_add(2)?;
        let operation = ReleaseOperation(Arc::new(Operation {
            shared: self.0.clone(),
            epoch: state.epoch.clone(),
            status: Arc::new(Status::default()),
            #[cfg(test)]
            dropping: None,
        }));
        let settlement = ReleaseTurn {
            shared: self.0.clone(),
            operation: Some(operation.operation_identity()),
        };
        Some((operation, settlement))
    }
}

#[cfg(test)]
#[path = "account_work_release_tests.rs"]
mod tests;
