// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The ACK attempt retains its real counted operation across uncertain Book
//! writes. Neither EOF nor a public Applied observation can create this role.
use super::*;
use crate::harness::codex_assignment::target_ack as signed;

pub(super) struct AckWork {
    pub(super) scope: crate::harness::account_work::Scope,
    observation: signed::Observation,
    pub(super) worker: AtomicBool,
    pub(super) attempted: AtomicBool,
    pub(super) publication_started: AtomicBool,
}
pub struct TargetAckCommand {
    state: Weak<Mutex<State>>,
    cell: Weak<Mutex<Cell>>,
    installed: Arc<Installed>,
    kernel: Arc<ReservationFamilyExclusive>,
    work: Arc<AckWork>,
}
impl std::fmt::Debug for TargetAckCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TargetAckCommand(redacted)")
    }
}
pub struct TargetApplied {
    pub(in crate::harness) receipt: Arc<signed::AppliedReceipt>,
}
impl std::fmt::Debug for TargetApplied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TargetApplied(redacted)")
    }
}
impl TargetApplied {
    pub fn operation(&self) -> &str {
        &self.receipt.observation().operation
    }
    pub fn boundary(&self) -> &str {
        &self.receipt.observation().boundary
    }
    pub fn assignment_generation(&self) -> u64 {
        self.receipt.observation().assignment_generation
    }
    pub fn credential_generation(&self) -> u64 {
        self.receipt.observation().credential_generation
    }
    pub fn digest(&self) -> &str {
        &self.receipt.observation().digest
    }
    pub fn account(&self) -> &str {
        &self.receipt.observation().account
    }
    pub fn user(&self) -> &str {
        &self.receipt.observation().user
    }
    pub fn auth(&self) -> AuthState {
        let observed = self.receipt.observation();
        AuthState {
            generation: observed.auth_generation,
            owner: observed.owner_generation,
            source: observed.source_generation,
            login: observed.login_generation,
        }
    }
}

pub enum AckPreparation {
    Applied(TargetApplied),
    Pending(Box<TargetAckCommand>),
}

pub(in crate::harness) struct TargetAckPermit<'a> {
    command: &'a TargetAckCommand,
}
impl TargetAckPermit<'_> {
    pub(in crate::harness) fn assignment(&self) -> &assignment::AssignmentCommit {
        &self.command.installed.assignment
    }
    pub(in crate::harness) fn observation(&self) -> &signed::Observation {
        &self.command.work.observation
    }
    pub(in crate::harness) fn expected(&self) -> &slot::SourceHandle {
        // The source is the actual installed immutable grant; the observation
        // may be a later same-grant revision, validated by the resource producer.
        &self.command.installed.source
    }
    pub(in crate::harness) fn gate(&self) -> &SourceGateIdentity {
        self.command.installed.resource.gate_identity()
    }
    /// Storage-only continuation of this exact live producer occurrence. It
    /// cannot authorize a fresh write body, source read or credential delivery.
    pub(in crate::harness) fn settle_started(&self) -> Result<(), String> {
        if !self
            .command
            .work
            .publication_started
            .load(Ordering::Acquire)
        {
            return Err("target ACK write never started".into());
        }
        let state = self
            .command
            .state
            .upgrade()
            .ok_or("target ACK registry retired")?;
        let cell = self
            .command
            .cell
            .upgrade()
            .ok_or("target ACK reservation retired")?;
        let state = lock(&state).map_err(|_| "target ACK registry busy")?;
        same_cell(&state, &cell, self.assignment().caller())
            .map_err(|_| "target ACK reservation changed")?;
        let held = lock(&cell).map_err(|_| "target ACK reservation busy")?;
        let observation = self.observation();
        let record = state
            .target_resources
            .get(&open_key(
                self.assignment().caller(),
                &observation.resource_operation,
            ))
            .ok_or("target ACK resource missing")?;
        if !matches!(&held.resource_phase, Phase::Acknowledging { operation } if operation == &observation.resource_operation)
            || record.context != observation.context
            || record.assignment != observation.operation
            || record.receipt.as_deref() != Some(observation.resource_receipt.as_str())
            || record
                .ack
                .as_ref()
                .is_none_or(|work| !Arc::ptr_eq(work, &self.command.work))
            || state
                .app_lifetime
                .as_ref()
                .is_none_or(|app| !self.command.work.scope.cancellation().owned_by(app))
        {
            return Err("target ACK continuation changed".into());
        }
        Ok(())
    }
    pub(in crate::harness) fn before_write(&self) -> Result<(), String> {
        self.current()?;
        self.command
            .work
            .publication_started
            .store(true, Ordering::Release);
        Ok(())
    }
    pub(in crate::harness) fn matches_home(&self, home: &std::path::Path) -> bool {
        self.command
            .installed
            .resource
            .gate_identity()
            .matches_home(home)
    }
    pub(in crate::harness) fn current(&self) -> Result<(), String> {
        self.command.kernel.current().map_err(str::to_owned)?;
        self.command
            .installed
            .resource
            .current()
            .map_err(str::to_owned)?;
        let process =
            Process::observe(self.assignment().caller().pid).ok_or("target ACK process retired")?;
        if self.command.work.scope.cancelled() {
            return Err("target ACK owner retired".into());
        }
        let state = self
            .command
            .state
            .upgrade()
            .ok_or("target ACK registry retired")?;
        let cell = self
            .command
            .cell
            .upgrade()
            .ok_or("target ACK reservation retired")?;
        let state = lock(&state).map_err(|_| "target ACK registry busy")?;
        same_cell(&state, &cell, self.assignment().caller())
            .map_err(|_| "target ACK reservation changed")?;
        let held = lock(&cell).map_err(|_| "target ACK reservation busy")?;
        let observation = self.observation();
        let record = state
            .target_resources
            .get(&open_key(
                self.assignment().caller(),
                &observation.resource_operation,
            ))
            .ok_or("target ACK resource missing")?;
        if !matches!(&held.phase, seal::Phase::Sealed { kernel, .. } if Arc::ptr_eq(kernel, &self.command.kernel))
            || !matches!(&held.resource_phase, Phase::Acknowledging { operation } if operation == &observation.resource_operation)
            || held
                .installed
                .as_ref()
                .is_none_or(|installed| !Arc::ptr_eq(installed, &self.command.installed))
            || record.process != process
            || record.revoked
            || record.refused
            || record.context != observation.context
            || record.receipt.as_deref() != Some(observation.resource_receipt.as_str())
            || record
                .ack
                .as_ref()
                .is_none_or(|work| !Arc::ptr_eq(work, &self.command.work))
            || !cohort_current(&state, self.assignment().caller(), &record.cohort)
            || state
                .app_lifetime
                .as_ref()
                .is_none_or(|app| !self.command.work.scope.cancellation().owned_by(app))
        {
            return Err("target ACK resource authority changed".into());
        }
        Ok(())
    }
}

fn observation(record: &Record) -> Result<signed::Observation, Error> {
    let report = &record.report;
    Ok(signed::Observation {
        operation: record.assignment.clone(),
        context: record.context.clone(),
        resource_operation: report.operation.clone(),
        resource_receipt: record.receipt.clone().ok_or(Error::Busy)?,
        member: report.member.clone(),
        registration: report.registration.clone(),
        boundary: report.boundary.clone(),
        assignment_generation: report.assignment_generation,
        credential_generation: report.credential_generation,
        digest: report.digest.clone(),
        account: report.account.clone(),
        user: report.user.clone(),
        auth_generation: report.auth.generation,
        owner_generation: report.auth.owner,
        source_generation: report.auth.source,
        login_generation: report.auth.login,
    })
}

impl Registry {
    /// Correlation-only replay of an actual native-issued Applied receipt. The
    /// front door authenticates the socket caller independently of foreground
    /// and ordinary App admission; an absent record mints no role.
    pub fn replay_target_ack(
        &self,
        caller: &slot::Caller,
        assignment: &str,
        context: &str,
        token: &str,
    ) -> Result<Option<TargetApplied>, Error> {
        if !hex(assignment, 32) || !hex(context, 64) || !hex(token, 64) {
            return Err(Error::Denied);
        }
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        let state = lock(&self.state)?;
        let Some(record) = state
            .target_resources
            .values()
            .find(|record| record.caller == *caller && record.receipt.as_deref() == Some(token))
        else {
            return Ok(None);
        };
        if record.process != process || record.assignment != assignment || record.context != context
        {
            return Err(Error::Stale);
        }
        Ok(record.applied.as_ref().map(|receipt| TargetApplied {
            receipt: receipt.clone(),
        }))
    }
    pub fn prepare_target_ack(
        &self,
        peer: &Peer,
        assignment: &str,
        context: &str,
        token: &str,
    ) -> Result<AckPreparation, Error> {
        if !hex(assignment, 32) || !hex(context, 64) || !hex(token, 64) {
            return Err(Error::Denied);
        }
        let current_peer = peer.current();
        let mut state = lock(&self.state)?;
        let key = state
            .target_resources
            .iter()
            .find(|(_, record)| {
                record.caller == peer.caller
                    && record.assignment == assignment
                    && record.context == context
                    && record.receipt.as_deref() == Some(token)
            })
            .map(|(key, _)| key.clone())
            .ok_or(Error::Stale)?;
        let record = state.target_resources.get(&key).ok_or(Error::Stale)?;
        // Exact durable replay is correlation even after explicit retirement.
        if let Some(applied) = &record.applied {
            return Ok(AckPreparation::Applied(TargetApplied {
                receipt: applied.clone(),
            }));
        }
        if !current_peer
            || record.process != peer.process
            || record.revoked
            || record.refused
            || !cohort_current(&state, &peer.caller, &record.cohort)
        {
            return Err(Error::Stale);
        }
        let cell = state
            .reservations
            .get(&actor_key(&peer.caller))
            .cloned()
            .ok_or(Error::Stale)?;
        let mut held = lock(&cell)?;
        let installed = held.installed.clone().ok_or(Error::Stale)?;
        if installed.context != context || installed.assignment.operation() != assignment {
            return Err(Error::Stale);
        }
        let kernel = match &held.phase {
            seal::Phase::Sealed { kernel, .. } => kernel.clone(),
            _ => return Err(Error::Stale),
        };
        let operation = record.report.operation.clone();
        let work = if let Some(work) = &record.ack {
            if !matches!(&held.resource_phase, Phase::Acknowledging { operation: current } if current == &operation)
            {
                return Err(Error::Stale);
            }
            work.clone()
        } else {
            if !matches!(&held.resource_phase, Phase::Ready { operation: current } if current == &operation)
            {
                return Err(Error::Stale);
            }
            let scope = state
                .app_lifetime
                .as_ref()
                .and_then(|app| app.enter())
                .ok_or(Error::Busy)?;
            let work = Arc::new(AckWork {
                scope,
                observation: observation(record)?,
                worker: false.into(),
                attempted: false.into(),
                publication_started: false.into(),
            });
            let record = state.target_resources.get_mut(&key).ok_or(Error::Stale)?;
            record.ack_retirement = Some(work.scope.cancellation());
            record.ack = Some(work.clone());
            held.resource_phase = Phase::Acknowledging { operation };
            work
        };
        Ok(AckPreparation::Pending(Box::new(TargetAckCommand {
            state: Arc::downgrade(&self.state),
            cell: Arc::downgrade(&cell),
            installed,
            kernel,
            work,
        })))
    }

    pub fn run_target_ack(
        &self,
        command: Box<TargetAckCommand>,
        env: &Env,
    ) -> Result<TargetApplied, Error> {
        if command
            .state
            .upgrade()
            .is_none_or(|state| !Arc::ptr_eq(&state, &self.state))
        {
            return Err(Error::Stale);
        }
        {
            let state = lock(&self.state)?;
            let record = state
                .target_resources
                .get(&open_key(
                    command.installed.assignment.caller(),
                    &command.work.observation.resource_operation,
                ))
                .ok_or(Error::Stale)?;
            if record
                .ack
                .as_ref()
                .is_none_or(|work| !Arc::ptr_eq(work, &command.work))
            {
                return Err(Error::Stale);
            }
            command
                .work
                .worker
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| Error::Busy)?;
            command.work.attempted.store(true, Ordering::Release);
        }
        struct Worker(Arc<AckWork>);
        impl Drop for Worker {
            fn drop(&mut self) {
                self.0.worker.store(false, Ordering::Release);
            }
        }
        let _worker = Worker(command.work.clone());
        let permit = TargetAckPermit { command: &command };
        let applied = match signed::acknowledge(env, &permit) {
            Ok(applied) => Arc::new(applied),
            Err(_) => {
                if !command.work.publication_started.load(Ordering::Acquire) {
                    let retired = {
                        let mut state = lock(&self.state)?;
                        let cell = command.cell.upgrade().ok_or(Error::Stale)?;
                        let mut held = lock(&cell)?;
                        let record = state
                            .target_resources
                            .get_mut(&open_key(
                                command.installed.assignment.caller(),
                                &command.work.observation.resource_operation,
                            ))
                            .ok_or(Error::Stale)?;
                        if record
                            .ack
                            .as_ref()
                            .is_none_or(|work| !Arc::ptr_eq(work, &command.work))
                        {
                            return Err(Error::Stale);
                        }
                        let retired = record.ack.take();
                        held.resource_phase = Phase::Ready {
                            operation: command.work.observation.resource_operation.clone(),
                        };
                        retired
                    };
                    drop(retired);
                }
                return Err(Error::Unavailable);
            }
        };
        if !applied.matches(&command.installed.assignment) {
            return Err(Error::Stale);
        }
        let retired = {
            let mut state = lock(&self.state)?;
            let cell = command.cell.upgrade().ok_or(Error::Stale)?;
            same_cell(&state, &cell, command.installed.assignment.caller())?;
            let mut held = lock(&cell)?;
            let record = state
                .target_resources
                .get_mut(&open_key(
                    command.installed.assignment.caller(),
                    &command.work.observation.resource_operation,
                ))
                .ok_or(Error::Stale)?;
            if record
                .ack
                .as_ref()
                .is_none_or(|work| !Arc::ptr_eq(work, &command.work))
            {
                return Err(Error::Stale);
            }
            record.applied = Some(applied.clone());
            let retired = record.ack.take();
            held.resource_phase = Phase::Applied {
                operation: command.work.observation.resource_operation.clone(),
            };
            retired
        };
        drop(retired);
        Ok(TargetApplied { receipt: applied })
    }
}
