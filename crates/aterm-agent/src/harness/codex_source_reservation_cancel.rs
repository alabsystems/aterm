// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit cancellation retains the freeze until a signed Book proves that
//! target effects did not start. Unknown storage results never reopen A.

use super::*;
use crate::harness::codex_assignment::prepare;

pub struct Cancelled {
    _receipt: Arc<prepare::cancel::Cancelled>,
}
impl std::fmt::Debug for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceCancellation(<private>)")
    }
}

pub enum CancellationProgress {
    Cancelled(Cancelled),
    Retiring,
    // No source permission. The controller must reconcile the actual effect.
    EffectsStarted,
}

struct Driver(Arc<AtomicBool>);
impl Driver {
    fn acquire(flag: Arc<AtomicBool>) -> Result<Self, Error> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        Ok(Self(flag))
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Reservation {
    /// Release-only cancellation. This mints neither a target role nor a new
    /// source grant, and does not depend on entering an unpaused App lifetime.
    pub fn cancel_before_effects(
        &self,
        env: &Env,
        peer: &Peer,
    ) -> Result<super::CancellationProgress, Error> {
        if !peer.current() {
            return Err(Error::Stale);
        }
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let (intent, origin, _driver, operation_scope) = {
            let state = lock(&state)?;
            let mut cell = lock(&self.cell)?;
            if cell.process != peer.process || cell.target.caller() != &peer.caller {
                return Err(Error::Stale);
            }
            if matches!(cell.phase, seal::Phase::Terminal) {
                drop(cell);
                drop(state);
                return self.finish_cancellation(peer);
            }
            same_cell(&state, &self.cell, &peer.caller)?;
            cell.scope()?;
            if cell.installed.is_some() || !cell.target_constructors.is_empty() {
                return Ok(CancellationProgress::EffectsStarted);
            }
            let driver = Driver::acquire(cell.target_driver.clone())?;
            match &cell.phase {
                seal::Phase::Sealing(_) if cell.phase.sealing_worker_active() => {
                    return Err(Error::Busy);
                }
                seal::Phase::Terminal => return Err(Error::Stale),
                seal::Phase::Cancelling(_) => (),
                seal::Phase::Draining | seal::Phase::Sealing(_) | seal::Phase::Sealed { .. } => {
                    let prior = std::mem::replace(&mut cell.phase, seal::Phase::Terminal);
                    cell.phase = seal::Phase::Cancelling(Box::new(prior));
                }
            }
            (
                cell.target.intent(),
                cell.origin.clone(),
                driver,
                cell.scope()?.clone(),
            )
        };
        let home = env.home.as_deref().ok_or(Error::Unavailable)?;
        if !origin.current() || !origin.gate().matches_home(home) {
            return Err(Error::Stale);
        }
        let namespace = origin
            .source()
            .namespaces()
            .ok_or(Error::Unavailable)?
            .book();
        let progress = prepare::cancel::before_effects(env, &intent, namespace, || {
            // Book -> State -> Cell; no physical/process probe in this callback.
            let state = lock(&state).map_err(|_| prepare::cancel::Failure::Busy)?;
            same_cell(&state, &self.cell, &peer.caller)
                .map_err(|_| prepare::cancel::Failure::Unavailable)?;
            let cell = lock(&self.cell).map_err(|_| prepare::cancel::Failure::Busy)?;
            if cell.process != peer.process
                || !cell.target.matches_intent(&intent)
                || !matches!(cell.phase, seal::Phase::Cancelling(_))
                || cell.installed.is_some()
                || !cell.target_constructors.is_empty()
                || cell.scope().is_err()
            {
                return Err("source cancellation changed".into());
            }
            Ok(())
        })
        .map_err(|error| match error {
            prepare::cancel::Failure::Busy => Error::Busy,
            prepare::cancel::Failure::Unavailable => Error::Unavailable,
        })?;
        match progress {
            prepare::cancel::Progress::EffectsStarted => {
                let state = lock(&state)?;
                same_cell(&state, &self.cell, &peer.caller)?;
                let mut cell = lock(&self.cell)?;
                // An already-earned exclusion may resume exact reconciliation.
                // An unexpected effect while Draining remains fenced/unknown.
                if matches!(&cell.phase, seal::Phase::Cancelling(prior)
                    if matches!(prior.as_ref(), seal::Phase::Sealed { .. }))
                    && let seal::Phase::Cancelling(prior) =
                        std::mem::replace(&mut cell.phase, seal::Phase::Terminal)
                {
                    cell.phase = *prior;
                }
                Ok(CancellationProgress::EffectsStarted)
            }
            prepare::cancel::Progress::Cancelled(receipt) => {
                let handoff = {
                    let state = lock(&state)?;
                    same_cell(&state, &self.cell, &peer.caller)?;
                    lock(&self.cell)?.phase.normalized_source().cloned()
                };
                if let Some(handoff) = handoff {
                    receipt
                        .with_current_source(env, &intent, namespace, |permit| {
                            {
                                let state =
                                    lock(&state).map_err(|_| prepare::cancel::Failure::Busy)?;
                                same_cell(&state, &self.cell, &peer.caller)
                                    .map_err(|_| prepare::cancel::Failure::Unavailable)?;
                                let cell =
                                    lock(&self.cell).map_err(|_| prepare::cancel::Failure::Busy)?;
                                if !matches!(&cell.phase, seal::Phase::Cancelling(_))
                                    || cell.phase.sealing_worker_active()
                                    || cell
                                        .phase
                                        .normalized_source()
                                        .is_none_or(|actual| !Arc::ptr_eq(actual, &handoff))
                                    || !handoff.matches(cell.origin.source())
                                {
                                    return Err(prepare::cancel::Failure::Unavailable);
                                }
                            }
                            // Book remains claimed; State is gone before restoring
                            // the same physical FD. Failed conversion stays fenced.
                            handoff.restore(permit).map_err(|error| match error {
                                Error::Busy => prepare::cancel::Failure::Busy,
                                _ => prepare::cancel::Failure::Unavailable,
                            })
                        })
                        .map_err(|error| match error {
                            prepare::cancel::Failure::Busy => Error::Busy,
                            prepare::cancel::Failure::Unavailable => Error::Unavailable,
                        })?;
                }
                let custody = {
                    let state = lock(&state)?;
                    same_cell(&state, &self.cell, &peer.caller)?;
                    let mut cell = lock(&self.cell)?;
                    if !matches!(cell.phase, seal::Phase::Cancelling(_))
                        || !cell.target.matches_intent(&intent)
                        || !receipt.matches(&peer.caller, intent.operation(), namespace)
                    {
                        return Err(Error::Stale);
                    }
                    cell.scope()?.cancellation().cancel();
                    cell.cancelled = Some(Arc::new(receipt));
                    cell.take_custody()
                };
                drop(custody);
                drop(operation_scope);
                self.finish_cancellation(peer)
            }
        }
    }

    fn finish_cancellation(&self, peer: &Peer) -> Result<super::CancellationProgress, Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let mut state = lock(&state)?;
        let cell = lock(&self.cell)?;
        if cell.process != peer.process
            || cell.target.caller() != &peer.caller
            || !matches!(cell.phase, seal::Phase::Terminal)
        {
            return Err(Error::Stale);
        }
        let receipt = cell.cancelled.as_ref().ok_or(Error::Stale)?;
        let namespace = cell
            .origin
            .source()
            .namespaces()
            .ok_or(Error::Unavailable)?;
        if !receipt.matches(&peer.caller, cell.target.operation(), namespace.book()) {
            return Err(Error::Stale);
        }
        let actor = actor_key(&peer.caller);
        let history = format!("{actor}:{}", cell.target.operation());
        let current = state
            .reservations
            .get(&actor)
            .is_some_and(|current| Arc::ptr_eq(current, &self.cell));
        let historical = state
            .retired_reservations
            .get(&history)
            .is_some_and(|old| Arc::ptr_eq(old, &self.cell));
        if !current && !historical {
            return Err(Error::Stale);
        }
        if !cell.custody_retired() {
            return Ok(CancellationProgress::Retiring);
        }
        if current {
            if state.retired_reservations.contains_key(&history)
                || state
                    .retired_reservations
                    .keys()
                    .filter(|key| key.starts_with(&format!("{actor}:")))
                    .count()
                    >= prepare::MAX_PREPARED
            {
                return Err(Error::Busy);
            }
            state
                .retired_reservations
                .insert(history, self.cell.clone());
            state.reservations.remove(&actor);
        }
        Ok(CancellationProgress::Cancelled(super::Cancelled {
            _receipt: receipt.clone(),
        }))
    }
}
