// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Historical target routing is normalized only by the actual durable ACK
//! receipt. This retains B custody, never the old A exclusive or its Scope.
use super::*;
#[path = "codex_source_normalized_handoff.rs"]
pub(in crate::harness) mod handoff;
#[path = "codex_source_target_normalized_member.rs"]
mod member;
use crate::harness::codex_assignment::target_ack::AppliedReceipt;
pub use member::NormalizedMemberRequest;
pub(in crate::harness) use member::NormalizedRead;
pub(in crate::harness::codex_source_registry) use member::{note_owned_preload, selected_owned};

struct RetirementCustody {
    original: crate::harness::account_work::Cancellation,
    release: std::sync::Mutex<Option<crate::harness::account_work::ReleaseTurn>>,
}
impl RetirementCustody {
    fn new(scope: &crate::harness::account_work::Scope) -> Result<Self, Error> {
        Ok(Self {
            original: scope.cancellation(),
            release: std::sync::Mutex::new(Some(scope.release_turn().ok_or(Error::Unavailable)?)),
        })
    }
    fn release(&self) {
        let release = self
            .release
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        drop(release);
    }
}

pub(in crate::harness) struct NormalizationRetirement {
    custody: RetirementCustody,
    cell: Weak<Mutex<Cell>>,
    namespace_source: Arc<slot::SourceHandle>,
    gate: crate::harness::codex_source_gate::SourceGateIdentity,
    finalized: std::sync::atomic::AtomicBool,
    departed: std::sync::atomic::AtomicBool,
}
impl NormalizationRetirement {
    pub(super) fn retired(&self) -> bool {
        self.finalized.load(std::sync::atomic::Ordering::Acquire)
    }
    // Called only outside State after exact kernel departure was established.
    // This drops cleanup counting, never fabricates a successful normalization.
    pub(super) fn abandon_departed(&self) {
        self.departed
            .store(true, std::sync::atomic::Ordering::Release);
        self.custody.release();
    }
    fn can_release_unused(&self) -> bool {
        self.retired()
            || (self.departed.load(std::sync::atomic::Ordering::Acquire)
                && self.custody.original.retired())
    }
}

pub(in crate::harness) struct NormalizedTarget {
    retirement: Arc<NormalizationRetirement>,
    installed: Arc<Installed>,
    applied: Arc<AppliedReceipt>,
    active: std::sync::atomic::AtomicBool,
    revoked_by: Mutex<Option<Arc<AppliedReceipt>>>,
}
impl NormalizedTarget {
    pub(in crate::harness::codex_source_registry) fn installed(&self) -> &Arc<Installed> {
        &self.installed
    }
    pub(in crate::harness::codex_source_registry) fn current(&self) -> Result<(), Error> {
        if !self.active.load(Ordering::Acquire) || !self.applied.matches(&self.installed.assignment)
        {
            return Err(Error::Stale);
        }
        self.installed.resource.current().map_err(|_| Error::Stale)
    }
    pub(in crate::harness::codex_source_registry) fn source_lease(&self) -> Arc<SourceWorkLease> {
        self.installed.resource.retained_source_lease()
    }
}

pub(in crate::harness::codex_source_registry) fn lookup(
    state: &State,
    caller: &slot::Caller,
    installed: &Arc<Installed>,
) -> Option<Arc<NormalizedTarget>> {
    state
        .normalized_targets
        .get(&open_key(caller, installed.assignment.operation()))
        .filter(|normal| {
            normal.active.load(Ordering::Acquire)
                && Arc::ptr_eq(&normal.installed, installed)
                && normal.applied.matches(&installed.assignment)
        })
        .cloned()
}
pub(in crate::harness::codex_source_registry) fn allows_member(
    state: &State,
    caller: &slot::Caller,
    member: &str,
) -> bool {
    let Some(issued) = state
        .actors
        .get(&actor_key(caller))
        .and_then(|actor| actor.issued.get(member))
    else {
        return false;
    };
    let Some(installed) = issued.target.as_ref().and_then(Weak::upgrade) else {
        return false;
    };
    lookup(state, caller, &installed).is_some()
        && member_key(caller, member)
            .ok()
            .and_then(|key| state.members.get(&key))
            .is_some_and(|member| same_source_scope(&member.source, &installed.source))
}

/// Metadata-only last-member join. Returned B claims retire outside State.
/// Native scopes that borrowed them keep their own actual strong custody.
pub(in crate::harness) fn take_unused(state: &mut State) -> Vec<Arc<NormalizedTarget>> {
    let keys: Vec<_> = state
        .normalized_targets
        .iter()
        .filter(|(_, normal)| {
            normal.retirement.can_release_unused()
                && !state.members.values().any(|member| {
                    member.caller == *normal.installed.assignment.caller()
                        && handoff::same_materialization(&member.source, &normal.installed.source)
                })
                && state
                    .actors
                    .get(&actor_key(normal.installed.assignment.caller()))
                    .is_none_or(|actor| {
                        !actor.issued.values().any(|issued| {
                            issued.target.as_ref().is_some_and(|target| {
                                target.ptr_eq(&Arc::downgrade(&normal.installed))
                            })
                        })
                    })
        })
        .map(|(key, _)| key.clone())
        .collect();
    keys.into_iter()
        .filter_map(|key| state.normalized_targets.remove(&key))
        .collect()
}

impl Registry {
    /// Cleanup and routing transfer after an actual signed ACK. No constructor,
    /// provider, Slot write or new work is admitted here. Original counted
    /// custody is retained even if ordinary App admission has closed.
    pub fn normalize_target(&self, env: &Env, applied: &TargetApplied) -> Result<(), Error> {
        let prior = {
            let state = lock(&self.state)?;
            resources::normalization_progress(&state, applied.receipt.caller(), &applied.receipt)
        };
        if let Some(prior) = prior {
            return self.finish_normalization(env, &applied.receipt, &prior);
        }
        let observation = applied.receipt.observation();
        let (cell, installed, process, driver, scope, kernel, predecessor) = {
            let state = lock(&self.state)?;
            let (caller, cell) = state
                .reservations
                .iter()
                .find_map(|(_, cell)| {
                    let held = lock(cell).ok()?;
                    held.installed
                        .as_ref()
                        .filter(|installed| applied.receipt.matches(&installed.assignment))
                        .map(|installed| (installed.assignment.caller().clone(), cell.clone()))
                })
                .ok_or(Error::Stale)?;
            let held = lock(&cell)?;
            let installed = held.installed.clone().ok_or(Error::Stale)?;
            if !matches!(&held.resource_phase, resources::Phase::Applied { operation } if operation == &observation.resource_operation)
                || !resources::authenticates_applied(&state, &caller, &applied.receipt)
                || !resources::ready_to_release(&state, &caller, &applied.receipt)
                || !held.target_constructors.is_empty()
                || held
                    .target_retiring
                    .iter()
                    .any(|witness| !witness.retired())
            {
                return Err(Error::Stale);
            }
            let scope = held.scope()?.clone();
            if state
                .app_lifetime
                .as_ref()
                .is_none_or(|app| !scope.cancellation().owned_by(app))
            {
                return Err(Error::Denied);
            }
            (
                cell.clone(),
                installed,
                held.process.clone(),
                held.target_driver.clone(),
                scope,
                match &held.phase {
                    seal::Phase::Sealed { kernel, .. } => kernel.clone(),
                    _ => return Err(Error::Stale),
                },
                held.phase.normalized_source().cloned(),
            )
        };
        installed.source.namespaces().ok_or(Error::Unavailable)?;
        let retirement = Arc::new(NormalizationRetirement {
            custody: RetirementCustody::new(&scope)?,
            cell: Arc::downgrade(&cell),
            namespace_source: Arc::clone(&installed.source),
            gate: installed.resource.gate_identity().clone(),
            finalized: std::sync::atomic::AtomicBool::new(false),
            departed: std::sync::atomic::AtomicBool::new(false),
        });
        let _driver = Driver::acquire(driver)?;
        let caller = installed.assignment.caller();
        if Process::observe(caller.pid).as_ref() != Some(&process)
            || !env
                .home
                .as_deref()
                .is_some_and(|home| installed.resource.gate_identity().matches_home(home))
        {
            return Err(Error::Stale);
        }
        // Metadata authentication reuses an actual live Book claim; this does
        // not adopt raw files or require a new owner/provider operation.
        slot::authenticate_captured_namespaces(
            caller,
            &installed.source,
            installed.resource.gate_identity(),
        )
        .map_err(|_| Error::Unavailable)?;
        let retired = applied.receipt.with_current(&installed.assignment, |namespace| {
            if installed.source.namespaces().is_none_or(|captured| captured.book() != namespace || !captured.current()) {
                return Err("normalized target namespace changed".into());
            }
            {
                let state = lock(&self.state).map_err(|_| "target registry busy")?;
                same_cell(&state, &cell, caller).map_err(|_| "target reservation changed")?;
                let held = lock(&cell).map_err(|_| "target reservation busy")?;
                if held.installed.as_ref().is_none_or(|current| !Arc::ptr_eq(current, &installed))
                    || !matches!(&held.phase, seal::Phase::Sealed { kernel: actual, .. } if Arc::ptr_eq(actual, &kernel))
                    || !resources::authenticates_applied(&state, caller, &applied.receipt)
                    || !resources::ready_to_release(&state, caller, &applied.receipt)
                { return Err("target normalization changed".into()); }
                handoff::revoke_before_handoff(&state, predecessor.as_ref(), &applied.receipt, namespace)
                    .map_err(|_| "source revocation busy")?;
            }
            // The real Applied Book stays claimed, but State/Cell are gone
            // before any kernel operation. Old source grants are already closed.
            installed.resource.normalize_after_applied(&applied.receipt, &installed.assignment)
                .map_err(str::to_owned)?;
            kernel.release_source_after_applied(&applied.receipt, &installed.assignment)
                .map_err(str::to_owned)?;
            let mut state = lock(&self.state).map_err(|_| "target registry busy")?;
            same_cell(&state, &cell, caller).map_err(|_| "target reservation changed")?;
            let mut held = lock(&cell).map_err(|_| "target reservation busy")?;
            if held.installed.as_ref().is_none_or(|current| !Arc::ptr_eq(current, &installed))
                || !matches!(&held.resource_phase, resources::Phase::Applied { operation } if operation == &observation.resource_operation)
                || !resources::authenticates_applied(&state, caller, &applied.receipt)
                || !resources::ready_to_release(&state, caller, &applied.receipt) {
                return Err("target normalization changed".into());
            }
            let key = open_key(caller, installed.assignment.operation());
            if state.normalized_targets.len() >= MAX_ACTORS * MAX_MEMBERS && !state.normalized_targets.contains_key(&key) {
                return Err("target normalization capacity unavailable".into());
            }
            resources::record_normalization(&mut state, caller, &applied.receipt, retirement.clone())?;
            state.normalized_targets.insert(key, Arc::new(NormalizedTarget { retirement: retirement.clone(), installed: installed.clone(), applied: applied.receipt.clone(), active: true.into(), revoked_by: Mutex::new(None) }));
            Ok(held.take_custody())
        }).map_err(|_| Error::Unavailable)?;
        // External Reservation Arcs now retain only terminal metadata. B owns
        // its independent actual share; no A exclusion survives in this record.
        drop(retired);
        drop(kernel);
        drop(predecessor);
        drop(scope);
        self.finish_normalization(env, &applied.receipt, &retirement)
    }

    fn finish_normalization(
        &self,
        env: &Env,
        applied: &Arc<AppliedReceipt>,
        proof: &Arc<NormalizationRetirement>,
    ) -> Result<(), Error> {
        if proof.retired() {
            return Ok(());
        }
        if !proof.custody.original.retired() {
            return Err(Error::Busy);
        }
        // A local counted turn protects this I/O even if exact-death cleanup
        // concurrently abandons the retained retry turn.
        let app = lock(&self.state)?
            .app_lifetime
            .clone()
            .ok_or(Error::Denied)?;
        let _turn = app.release_turn().ok_or(Error::Unavailable)?;
        if !env
            .home
            .as_deref()
            .is_some_and(|home| proof.gate.matches_home(home))
        {
            return Err(Error::Stale);
        }
        applied
            .with_current_book(|namespace| {
                if proof
                    .namespace_source
                    .namespaces()
                    .is_none_or(|captured| captured.book() != namespace)
                {
                    return Err("normalization Book namespace changed".into());
                }
                let mut state = lock(&self.state).map_err(|_| "normalization registry busy")?;
                let actual = resources::normalization_progress(&state, applied.caller(), applied)
                    .ok_or("normalization receipt missing")?;
                if !Arc::ptr_eq(&actual, proof) {
                    return Err("normalization receipt changed".into());
                }
                let cell = proof.cell.upgrade().ok_or("normalization cell missing")?;
                let held = lock(&cell).map_err(|_| "normalization cell busy")?;
                if !matches!(held.phase, seal::Phase::Terminal)
                    || !held.custody_retired()
                    || held.target.operation() != applied.observation().operation
                    || !matches!(&held.resource_phase, resources::Phase::Applied { operation }
                    if operation == &applied.observation().resource_operation)
                {
                    return Err("normalization original custody pending".into());
                }
                let actor = actor_key(applied.caller());
                let history = format!("{actor}:{}", applied.observation().operation);
                let current = state
                    .reservations
                    .get(&actor)
                    .is_some_and(|actual| Arc::ptr_eq(actual, &cell));
                let historical = state
                    .retired_reservations
                    .get(&history)
                    .is_some_and(|actual| Arc::ptr_eq(actual, &cell));
                if !current && !historical {
                    return Err("normalization cell changed".into());
                }
                if current {
                    if state.retired_reservations.contains_key(&history)
                        || state
                            .retired_reservations
                            .keys()
                            .filter(|key| key.starts_with(&format!("{actor}:")))
                            .count()
                            >= assignment::prepare::MAX_PREPARED
                    {
                        return Err("normalization history capacity unavailable".into());
                    }
                    state.retired_reservations.insert(history, cell.clone());
                    state.reservations.remove(&actor);
                }
                Ok(())
            })
            .map_err(|_| Error::Unavailable)?;
        proof
            .finalized
            .store(true, std::sync::atomic::Ordering::Release);
        // Never drop counted turns while Book/Registry/Cell locks are held.
        proof.custody.release();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_cleanup_counts_without_pinning_original_operation() {
        // Actual App operations and drain protocol, without any synthetic
        // Applied receipt, owner, Slot, or target admission authority.
        let app = crate::harness::account_work::Lifetime::default();
        let scope = app.enter().unwrap();
        let capture = scope.clone();
        let custody = RetirementCustody::new(&scope).unwrap();
        let pause = app.pause().unwrap();
        drop(scope);
        assert!(!custody.original.retired());
        let pause = pause
            .try_wait()
            .expect_err("actual original capture remains");
        drop(capture);
        assert!(
            custody.original.retired(),
            "cleanup must not self-pin original"
        );
        let pause = pause.try_wait().expect_err("retry custody remains counted");
        // Kernel-departure abandonment uses this same release path; it grants
        // no normalization/finalization observation.
        custody.release();
        custody.release();
        pause.try_wait().unwrap().resume().unwrap();
    }
}
