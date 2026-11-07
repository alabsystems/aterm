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
mod tests {
    use super::*;

    #[test]
    fn paused_release_lifetime_is_distinct_counted_and_sealed_closed() {
        let lifetime = Lifetime::default();
        let pause = lifetime.pause().unwrap();
        assert!(
            lifetime.enter().is_none(),
            "ordinary Scope admission remains closed"
        );
        let (operation, settlement) = lifetime.reserve_release_operation().unwrap();
        let monitor = operation.monitor();
        let actual_resources = operation.clone();
        assert!(monitor.owned_by(&lifetime));
        assert!(!monitor.owned_by(&Lifetime::default()));
        assert_eq!(
            settlement.operation_identity(),
            Some(monitor.operation_identity())
        );
        drop(operation);
        assert!(!monitor.retired(), "actual release resources remain live");
        drop(actual_resources);
        assert!(
            monitor.retired(),
            "weak monitor must not retain the release operation"
        );
        let pause = match pause.try_wait() {
            Err(pause) => pause,
            Ok(_) => panic!("settlement must remain counted"),
        };
        drop(settlement);
        let drained = pause
            .try_wait()
            .unwrap_or_else(|_| panic!("real release+settlement drop must drain"));
        assert!(
            lifetime.reserve_release_operation().is_none(),
            "no reservation after seal"
        );
        drained
            .resume()
            .unwrap_or_else(|_| panic!("matching epoch must resume"));
        let work = lifetime.enter().unwrap();
        assert!(
            !monitor.operation_identity().matches_scope(&work),
            "release identity is not a fresh work Scope"
        );
    }
}
