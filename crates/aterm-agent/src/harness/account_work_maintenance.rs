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
#[path = "account_work_maintenance_tests.rs"]
mod tests;
