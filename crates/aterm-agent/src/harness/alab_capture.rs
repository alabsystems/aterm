// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A retained source observation and its synchronous receipt admission.

use super::SourceSnapshot;
#[cfg(test)]
use crate::harness::account_sync_error::Retry;
use crate::harness::account_sync_error::SyncError;
use std::sync::Arc;

/// Implemented by the source host, which authenticates its retained observation
/// before running one local receipt save. The callback must not replay images,
/// reopen this source, take another account writer, or perform network work.
pub trait SourceGuard: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
    fn same_source(&self, other: &dyn SourceGuard) -> bool;
    /// # Errors
    /// The source is no longer admissible or the receipt cannot be saved.
    fn with_admitted(
        &self,
        save: Box<dyn FnOnce() -> Result<(), SyncError> + '_>,
    ) -> Result<(), SyncError>;
}

/// Runtime custody, never part of a serialized roster or receipt.
#[derive(Clone)]
pub struct ReceiptGuard(Arc<dyn SourceGuard>);

impl ReceiptGuard {
    #[must_use]
    pub fn new(guard: impl SourceGuard + 'static) -> Self {
        Self(Arc::new(guard))
    }

    /// Save exactly once under this source's current admission. A callback
    /// error is retained independently of the host's result. A later host error
    /// leaves a successfully saved receipt for ordinary recovery, never replay.
    ///
    /// # Errors
    /// Source admission or the save fails, or the host returns without saving.
    pub fn with_admitted<T>(
        &self,
        save: impl FnOnce() -> Result<T, SyncError>,
    ) -> Result<T, SyncError> {
        let mut saved = None;
        let admitted = self.0.with_admitted(Box::new(|| {
            let result = save();
            let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
            saved = Some(result);
            outcome
        }));
        match (saved, admitted) {
            (Some(Err(error)), _) | (_, Err(error)) => Err(error),
            (Some(Ok(result)), Ok(())) => Ok(result),
            (None, Ok(())) => Err("source admission returned without saving its receipt".into()),
        }
    }
}

impl PartialEq for ReceiptGuard {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.same_source(other.0.as_ref())
    }
}

impl std::fmt::Debug for ReceiptGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiptGuard").finish_non_exhaustive()
    }
}

/// One immutable decision input. Only a source host may supply its guard;
/// observation without a guard never authorizes a native receipt.
#[derive(Clone, Debug)]
pub struct SourceCapture {
    snapshot: SourceSnapshot,
    guard: Option<ReceiptGuard>,
}

impl SourceCapture {
    #[must_use]
    pub fn observed(snapshot: SourceSnapshot) -> Self {
        Self {
            snapshot,
            guard: None,
        }
    }

    #[must_use]
    pub fn guarded(snapshot: SourceSnapshot, guard: ReceiptGuard) -> Self {
        Self {
            snapshot,
            guard: Some(guard),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> &SourceSnapshot {
        &self.snapshot
    }

    /// # Errors
    /// The captured observation has no native receipt admission.
    pub fn native_guard(&self) -> Result<&ReceiptGuard, String> {
        self.guard
            .as_ref()
            .ok_or_else(|| "native account source has no receipt admission".into())
    }

    #[must_use]
    pub fn same_source(&self, other: &Self) -> bool {
        self.snapshot.same_observation(&other.snapshot) && self.guard == other.guard
    }
}

#[cfg(test)]
pub(super) struct TestGuard(pub SourceSnapshot);

#[cfg(test)]
impl SourceGuard for TestGuard {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn same_source(&self, other: &dyn SourceGuard) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self.0.same_observation(&other.0))
    }
    fn with_admitted(
        &self,
        save: Box<dyn FnOnce() -> Result<(), SyncError> + '_>,
    ) -> Result<(), SyncError> {
        if !self.0.same_observation(&super::snapshot()) {
            return Err(SyncError::retry(
                Retry::Changed,
                "test source changed before receipt admission",
            ));
        }
        save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Host(u8);
    impl SourceGuard for Host {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn same_source(&self, _: &dyn SourceGuard) -> bool {
            false
        }
        fn with_admitted(
            &self,
            save: Box<dyn FnOnce() -> Result<(), SyncError> + '_>,
        ) -> Result<(), SyncError> {
            match self.0 {
                0 => save(),
                1 => Ok(()),
                2 => {
                    let _ = save();
                    Ok(())
                }
                _ => {
                    save()?;
                    Err("host failed after save".into())
                }
            }
        }
    }

    #[test]
    fn guard_retains_the_full_callback_result_and_requires_a_call() {
        assert_eq!(ReceiptGuard::new(Host(0)).with_admitted(|| Ok(42)), Ok(42));
        assert!(ReceiptGuard::new(Host(1)).with_admitted(|| Ok(42)).is_err());
        assert_eq!(
            ReceiptGuard::new(Host(2)).with_admitted::<()>(|| Err("save failed".into())),
            Err("save failed".into())
        );
        let mut saved = false;
        let result = ReceiptGuard::new(Host(3)).with_admitted(|| {
            saved = true;
            Ok(42)
        });
        assert!(saved);
        assert_eq!(result, Err("host failed after save".into()));
    }
}
