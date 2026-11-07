// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Release-only native recovery. This role cannot lend a response or become a
//! browser, publication turn, SigninTurn, or credential materialization proof.

use super::*;
use crate::harness::{account_work, codex_native_retirement, wake_deadline::WakeDeadline};

pub(in crate::harness) struct NativeRetirementResources {
    candidate: codex_native_retirement::NativeRetirementCandidate,
    occurrence: String,
    legacy: Held,
    kernel: super::retirement_gate::RetirementTurn,
    custody: RetirementCustody,
    deadline: WakeDeadline,
}
enum RetirementCustody {
    Scoped {
        scope: account_work::Scope,
        release: account_work::ReleaseTurn,
    },
    Released(account_work::ReleaseOperation),
}
impl RetirementCustody {
    fn identity(&self) -> account_work::OperationIdentity {
        match self {
            Self::Scoped { scope, .. } => scope.operation_identity(),
            Self::Released(operation) => operation.operation_identity(),
        }
    }
}
impl std::fmt::Debug for NativeRetirementResources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeRetirementResources(release only, redacted)")
    }
}
impl NativeRetirementResources {
    pub(in crate::harness) fn acquire(
        store: &accounts::Store,
        candidate: codex_native_retirement::NativeRetirementCandidate,
        identity: &crate::harness::codex_grant_owner::RetirementIdentity,
        scope: &account_work::Scope,
        release: account_work::ReleaseTurn,
        deadline: WakeDeadline,
    ) -> Result<Self, String> {
        if release.operation_identity() != Some(&scope.operation_identity()) {
            return Err("native retirement has no matching operation".into());
        }
        Self::acquire_owned(
            store,
            candidate,
            identity,
            RetirementCustody::Scoped {
                scope: scope.clone(),
                release,
            },
            deadline,
        )
    }
    pub(in crate::harness) fn acquire_released(
        store: &accounts::Store,
        candidate: codex_native_retirement::NativeRetirementCandidate,
        identity: &crate::harness::codex_grant_owner::RetirementIdentity,
        operation: &account_work::ReleaseOperation,
        deadline: WakeDeadline,
    ) -> Result<Self, String> {
        Self::acquire_owned(
            store,
            candidate,
            identity,
            RetirementCustody::Released(operation.clone()),
            deadline,
        )
    }
    fn acquire_owned(
        store: &accounts::Store,
        candidate: codex_native_retirement::NativeRetirementCandidate,
        identity: &crate::harness::codex_grant_owner::RetirementIdentity,
        custody: RetirementCustody,
        deadline: WakeDeadline,
    ) -> Result<Self, String> {
        candidate.check_store(store)?;
        if identity.operation() != candidate.request().id {
            return Err("native retirement occurrence changed".into());
        }
        let legacy = loop {
            if deadline.passed() {
                return Err("native retirement deadline passed".into());
            }
            candidate.check_store(store)?;
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
        // A pre-seal release turn is counted cleanup, not cancelled work. Only
        // this distinct release role can wait without a normal Scope admission.
        let kernel = super::retirement_gate::RetirementTurn::acquire_within(
            candidate.source_home(),
            identity.resources(),
            deadline,
            || {
                candidate.check_store(store)?;
                legacy.current()
            },
        )?;
        let occurrence = aterm_uds::rand::hex_token::<16>()
            .map_err(|_| "native retirement occurrence unavailable")?;
        let resources = Self {
            candidate,
            occurrence,
            legacy,
            kernel,
            custody,
            deadline,
        };
        resources.check_store(store)?;
        Ok(resources)
    }
    pub(in crate::harness) fn candidate(
        &self,
    ) -> &codex_native_retirement::NativeRetirementCandidate {
        &self.candidate
    }
    pub(in crate::harness) fn occurrence(&self) -> &str {
        &self.occurrence
    }
    pub(in crate::harness) fn operation_identity(&self) -> account_work::OperationIdentity {
        self.custody.identity()
    }
    pub(in crate::harness) fn current(&self) -> Result<(), String> {
        if self.deadline.passed() {
            return Err("native retirement deadline passed".into());
        }
        self.legacy.current()?;
        self.kernel.current()?;
        Ok(())
    }
    pub(in crate::harness) fn check_store(&self, store: &accounts::Store) -> Result<(), String> {
        self.candidate.check_store(store)?;
        self.current()
    }
    pub(in crate::harness) fn release_after_retirement(self) -> Option<account_work::ReleaseTurn> {
        let Self {
            candidate: _,
            occurrence: _,
            legacy,
            kernel,
            custody,
            deadline: _,
        } = self;
        drop(kernel);
        drop(legacy);
        match custody {
            RetirementCustody::Scoped { scope, release } => {
                drop(scope);
                Some(release)
            }
            RetirementCustody::Released(operation) => {
                drop(operation);
                None
            }
        }
    }
}
