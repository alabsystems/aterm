// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A later ordinary admission in the same actual App namespace. This weak
//! locator cannot retain or revive the original operation or bypass a pause.
use super::*;

#[derive(Clone)]
pub(in crate::harness) struct MaintenanceEpoch(Weak<Epoch>);
impl PartialEq for MaintenanceEpoch {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for MaintenanceEpoch {}

pub(in crate::harness) struct MaintenanceOwner {
    original: Cancellation,
}
impl MaintenanceOwner {
    pub(in crate::harness) fn admission_epoch(&self) -> Option<MaintenanceEpoch> {
        if !self.original.retired() {
            return None;
        }
        let shared = self.original.owner.upgrade()?;
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.sealed || state.epoch.cancelled.load(Ordering::Acquire) {
            return None;
        }
        Some(MaintenanceEpoch(Arc::downgrade(&state.epoch)))
    }
    pub(in crate::harness) fn enter(&self) -> Option<Scope> {
        if !self.original.retired() {
            return None;
        }
        // Reuse the original App's actual Shared, never a fresh Lifetime or a
        // release-only turn. counted_enter takes the admission-state mutex,
        // so pause/seal wins before any maintenance credential read.
        let shared = self.original.owner.upgrade()?;
        super::counted_enter(&shared)
    }
}
impl Scope {
    pub(in crate::harness) fn maintenance_epoch(&self) -> MaintenanceEpoch {
        MaintenanceEpoch(Arc::downgrade(&self.0.epoch))
    }
    pub(in crate::harness) fn maintenance_owner(&self) -> MaintenanceOwner {
        MaintenanceOwner {
            original: self.cancellation(),
        }
    }
}

/// Weak admission into this actual App only. It cannot revive an operation or
/// admit through pause/seal; callers still need separate native authority.
#[derive(Clone)]
pub struct AdmissionOwner(Weak<Shared>);
impl AdmissionOwner {
    pub fn enter(&self) -> Option<Scope> {
        super::counted_enter(&self.0.upgrade()?)
    }
}
impl Lifetime {
    pub fn admission_owner(&self) -> AdmissionOwner {
        AdmissionOwner(Arc::downgrade(&self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_uses_a_new_operation_only_after_retirement_and_real_app_resume() {
        let app = Lifetime::default();
        let foreign = Lifetime::default();
        let original = app.enter().unwrap();
        let original_id = original.operation_identity();
        let original_status = original.cancellation();
        let owner = original.maintenance_owner();
        assert!(owner.enter().is_none());
        original_status.cancel();
        assert!(owner.enter().is_none());
        drop(original);
        assert!(original_status.retired());
        let pause = app.pause().unwrap();
        assert!(owner.enter().is_none());
        let drained = pause.try_wait().unwrap();
        let foreign_scope = foreign.enter().unwrap();
        assert!(
            owner.enter().is_none(),
            "a foreign open App cannot supply admission"
        );
        drop(foreign_scope);
        assert!(foreign.pause().unwrap().try_wait().is_ok());
        assert!(
            owner.enter().is_none(),
            "sealed admission must remain closed"
        );
        drained.resume().unwrap();
        let maintenance = owner.enter().unwrap();
        assert_ne!(maintenance.operation_identity(), original_id);
        assert!(maintenance.cancellation().owned_by(&app));
        assert!(!maintenance.cancellation().owned_by(&foreign));
        assert!(original_status.cancelled() && original_status.retired());
        let pause = app.pause().unwrap();
        let pause = match pause.try_wait() {
            Err(pause) => pause,
            Ok(_) => panic!("new maintenance must be actually counted"),
        };
        drop(maintenance);
        assert!(pause.try_wait().is_ok(), "the new operation really retired");
        assert!(owner.enter().is_none());
    }

    #[test]
    fn weak_discovery_admission_is_counted_and_never_borrows_a_foreign_app() {
        let app = Lifetime::default();
        let foreign = Lifetime::default();
        let admission = app.admission_owner();
        let original = admission.enter().unwrap();
        assert!(original.cancellation().owned_by(&app));
        assert!(!original.cancellation().owned_by(&foreign));
        let original_id = original.operation_identity();
        let pause = app.pause().unwrap();
        assert!(admission.enter().is_none());
        let pause = match pause.try_wait() {
            Err(pause) => pause,
            Ok(_) => panic!("discovery is counted before credential work"),
        };
        let foreign_scope = foreign.enter().unwrap();
        assert!(admission.enter().is_none());
        drop(foreign_scope);
        drop(original);
        let drained = pause.try_wait().unwrap();
        assert!(admission.enter().is_none());
        drained.resume().unwrap();
        let next = admission.enter().unwrap();
        assert_ne!(next.operation_identity(), original_id);
        drop(next);
        drop(app);
        assert!(
            admission.enter().is_none(),
            "foreign App cannot revive a dead owner"
        );
    }

    #[test]
    fn weak_maintenance_locator_does_not_keep_an_app_or_original_scope_alive() {
        let app = Lifetime::default();
        let original = app.enter().unwrap();
        let status = original.cancellation();
        let owner = original.maintenance_owner();
        drop(original);
        assert!(status.retired());
        drop(app);
        assert!(owner.enter().is_none());
    }
}
