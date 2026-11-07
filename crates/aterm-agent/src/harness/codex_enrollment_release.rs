// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pre-seal counted cancellation cleanup. No Scope or publication authority.
use super::*;
use crate::harness::{
    account_work, cli::Env, codex_native_retirement, wake_deadline::WakeDeadline,
};
use std::sync::{Arc, Mutex};

pub struct RetirementWork {
    env: Env,
    observation: codex_native_retirement::RetirementObservation,
    operation: account_work::ReleaseOperation,
    settlement: account_work::ReleaseTurn,
    control: RetirementControl,
    deadline: WakeDeadline,
}
#[derive(Clone)]
pub struct RetirementControl {
    request: Request,
    monitor: account_work::ReleaseMonitor,
    recovery: Arc<Mutex<Option<RecoveryOwnership>>>,
}
pub(in crate::harness) struct ReleaseAdmission<'a> {
    request: &'a Request,
    operation: &'a account_work::ReleaseOperation,
    recovery: &'a Mutex<Option<RecoveryOwnership>>,
}
impl ReleaseAdmission<'_> {
    pub(in crate::harness) fn native_recovery(
        &self,
        receipt: &RecoveryOwnership,
    ) -> Result<(), String> {
        if receipt.native.as_ref().is_none_or(|native| {
            native.kind != RecoveryKind::NativeRetirement || &native.request != self.request
        }) || !receipt.belongs_to(&self.operation.operation_identity())
        {
            return Err("native cleanup has no matching release occurrence".into());
        }
        let mut owned = self
            .recovery
            .lock()
            .map_err(|_| "native cleanup owner unavailable")?;
        if owned.is_some() {
            return Err("native cleanup already owns a claim".into());
        }
        *owned = Some(receipt.clone());
        Ok(())
    }
}
/// Pure exact canonical observation precedes counting. Historical credential
/// authentication occurs only when the returned counted work runs.
pub fn reserve_cancelled_retirement(
    env: &Env,
    request: &Request,
    lifetime: &account_work::Lifetime,
) -> Result<(RetirementWork, RetirementControl), String> {
    let observation = codex_native_retirement::observe(env, request)?;
    let (operation, settlement) = lifetime
        .reserve_release_operation()
        .ok_or("native cleanup admission sealed")?;
    let control = RetirementControl {
        request: request.clone(),
        monitor: operation.monitor(),
        recovery: Arc::new(Mutex::new(None)),
    };
    Ok((
        RetirementWork {
            env: env.clone(),
            observation,
            operation,
            settlement,
            control: control.clone(),
            deadline: WakeDeadline::after(super::super::login_drive::HANG),
        },
        control,
    ))
}
impl RetirementControl {
    pub fn work_retired(&self) -> bool {
        self.monitor.retired()
    }
    pub fn owned_by(&self, lifetime: &account_work::Lifetime) -> bool {
        self.monitor.owned_by(lifetime)
    }
    pub fn retired(&self) -> Option<Retired> {
        let owner = self.recovery.lock().ok()?.as_ref()?.clone();
        if !self.monitor.retired() || !owner.belongs_to(self.monitor.operation_identity()) {
            return None;
        }
        Some(Retired {
            request: self.request.clone(),
            kind: RetiredKind::Recovery(Box::new(owner)),
        })
    }
}
impl RetirementWork {
    pub fn run(self) -> Result<Status, String> {
        let Self {
            env,
            observation,
            operation,
            settlement,
            control,
            deadline,
        } = self;
        let result = (|| {
            let (candidate, fresh) =
                codex_native_retirement::authenticate_observation(&env, &observation)?;
            let store = env
                .accounts_store_for(accounts::Cli::Codex)
                .ok_or("native cleanup Store unavailable")?;
            let resources = super::super::codex_grant_owner::admit_native_retirement_released(
                &env, &store, candidate, &fresh, &operation, deadline,
            )?;
            let admission = ReleaseAdmission {
                request: &control.request,
                operation: &operation,
                recovery: &control.recovery,
            };
            codex_native_retirement::claim_released(&env, &store, resources, &admission)
        })();
        drop(operation);
        let result = match result {
            Ok(()) => control
                .retired()
                .ok_or_else(|| "native cleanup resources remain live".into())
                .and_then(|retired| retired.finish(&env)),
            Err(error) => Err(error),
        };
        drop(settlement);
        result
    }
}
