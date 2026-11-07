// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Distinct publication-only recovery resources. No browser, callback, code
//! exchange, implicit retirement or legacy SigninTurn is constructed here.

use super::*;
use crate::harness::{account_work, codex_native_row, wake_deadline::WakeDeadline};

pub(in crate::harness) struct NativeRecoveryResources {
    candidate: codex_native_row::NativeRecoveryCandidate,
    occurrence: String,
    legacy: Held,
    publication: PublicationTurn,
    scope: account_work::Scope,
    deadline: WakeDeadline,
}
impl std::fmt::Debug for NativeRecoveryResources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeRecoveryResources(no publication claim, redacted)")
    }
}
impl NativeRecoveryResources {
    pub(in crate::harness) fn acquire(
        store: &accounts::Store,
        candidate: codex_native_row::NativeRecoveryCandidate,
        identity: &crate::harness::codex_grant_owner::PublicationIdentity,
        scope: &account_work::Scope,
        deadline: WakeDeadline,
    ) -> Result<Self, String> {
        candidate.check_store(store)?;
        if identity.operation() != candidate.request().id {
            return Err("native recovery publication occurrence changed".into());
        }
        // Shared compatibility is a separate role; it is never a SigninTurn.
        // No source/owner/Auth/account claim is held while this gate is busy.
        let legacy = loop {
            if scope.cancelled() || deadline.passed() {
                return Err("native recovery publication stopped".into());
            }
            if let Some(held) =
                Held::open(&store.shared_dir(), accounts::SIGNIN_LOCK_FILE)?.try_shared()?
            {
                break held;
            }
            std::thread::sleep(
                deadline
                    .remaining()
                    .min(std::time::Duration::from_millis(20)),
            );
        };
        // The actual global S-exclusive is acquired before Eshared and sorted
        // authenticated principal/RT resources. Every partial is dropped on Busy.
        let publication = PublicationTurn::acquire_within(
            candidate.source_home(),
            identity.resources(),
            scope,
            deadline,
            || {
                candidate.check_store(store)?;
                legacy.current()
            },
        )?;
        let occurrence = aterm_uds::rand::hex_token::<16>()
            .map_err(|_| "native recovery occurrence unavailable")?;
        let turn = Self {
            candidate,
            occurrence,
            legacy,
            publication,
            scope: scope.clone(),
            deadline,
        };
        turn.check_store(store)?;
        Ok(turn)
    }

    #[cfg(unix)]
    pub(in crate::harness) fn scope(&self) -> &account_work::Scope {
        &self.scope
    }
    #[cfg(unix)]
    pub(in crate::harness) fn deadline(&self) -> WakeDeadline {
        self.deadline
    }

    pub(in crate::harness) fn candidate(&self) -> &codex_native_row::NativeRecoveryCandidate {
        &self.candidate
    }

    pub(in crate::harness) fn occurrence(&self) -> &str {
        &self.occurrence
    }

    pub(in crate::harness) fn operation_identity(&self) -> account_work::OperationIdentity {
        self.scope.operation_identity()
    }

    /// Checks physical resources only. The held CurrentNativeRow separately
    /// checks the exact canonical recovery claim before credential or row writes.
    pub(in crate::harness) fn check_store(&self, store: &accounts::Store) -> Result<(), String> {
        self.candidate.check_store(store)?;
        self.current()
    }

    pub(in crate::harness) fn current(&self) -> Result<(), String> {
        if self.scope.cancelled() || self.deadline.passed() {
            return Err("native recovery publication stopped".into());
        }
        self.legacy.current()?;
        self.publication._source.current().map_err(str::to_owned)?;
        self.publication.exchange.current()?;
        for resource in &self.publication.resources {
            resource.current()?;
        }
        Ok(())
    }
}

pub(in crate::harness) struct NativeRecoveryTurn {
    resources: NativeRecoveryResources,
    admission: codex_native_row::NativeRecoveryAdmission,
}
impl std::fmt::Debug for NativeRecoveryTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeRecoveryTurn(publication only, redacted)")
    }
}
impl NativeRecoveryTurn {
    pub(in crate::harness) fn from_admission(
        resources: NativeRecoveryResources,
        admission: codex_native_row::NativeRecoveryAdmission,
    ) -> Self {
        Self {
            resources,
            admission,
        }
    }
    pub(in crate::harness) fn admission(&self) -> &codex_native_row::NativeRecoveryAdmission {
        &self.admission
    }
    #[cfg(unix)]
    pub(in crate::harness) fn scope(&self) -> &account_work::Scope {
        self.resources.scope()
    }
    #[cfg(unix)]
    pub(in crate::harness) fn deadline(&self) -> WakeDeadline {
        self.resources.deadline()
    }
    pub(in crate::harness) fn check_store(&self, store: &accounts::Store) -> Result<(), String> {
        self.admission.check_store(store)?;
        let candidate = self.resources.candidate();
        if candidate.request() != self.admission.request()
            || candidate.reference() != self.admission.reference()
            || candidate.staging() != self.admission.staging()
            || self.admission.ownership().nonce() != Some(self.resources.occurrence())
            || !self
                .admission
                .ownership()
                .belongs_to(&self.resources.operation_identity())
        {
            return Err("native recovery resources changed".into());
        }
        self.resources.check_store(store)
    }
}
