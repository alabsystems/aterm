// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Registry-owned exclusion. Neither a dropped handle nor an operation name
//! releases this custody. This factory does not authorize a destination source.

use super::*;
use crate::harness::codex_source_gate::ReservationFamilyExclusive;

pub(super) enum Phase {
    Draining,
    Sealing(Arc<Attempt>),
    Sealed {
        attempt: Arc<Attempt>,
        kernel: Arc<ReservationFamilyExclusive>,
    },
}
impl Phase {
    pub(super) fn admits(&self) -> bool {
        matches!(self, Self::Draining)
    }
    pub(super) fn allows_member(&self, state: &State, caller: &slot::Caller, member: &str) -> bool {
        let attempt = match self {
            Self::Draining => return true,
            Self::Sealing(attempt) | Self::Sealed { attempt, .. } => attempt,
        };
        let Some(expected) = attempt.independent.get(member) else {
            return false;
        };
        member_key(caller, member)
            .ok()
            .and_then(|key| state.members.get(&key))
            .is_some_and(|actual| same_source_scope(expected, &actual.source))
    }
    fn matches(&self, expected: &Arc<Attempt>) -> bool {
        match self {
            Self::Draining => false,
            Self::Sealing(actual)
            | Self::Sealed {
                attempt: actual, ..
            } => Arc::ptr_eq(actual, expected),
        }
    }
}
pub(super) struct Attempt {
    worker: AtomicBool,
    independent: BTreeMap<String, Arc<slot::SourceHandle>>,
}
struct Worker(Arc<Attempt>);
impl Worker {
    fn acquire(attempt: Arc<Attempt>) -> Result<Self, Error> {
        attempt
            .worker
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        Ok(Self(attempt))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.0.worker.store(false, Ordering::Release);
    }
}

/// Only the actual phase-owning registry can construct this kernel-lock input.
/// Weak back references avoid a Cell -> guard -> Cell ownership cycle.
pub(in crate::harness) struct SealingSource {
    state: Weak<Mutex<State>>,
    cell: Weak<Mutex<Cell>>,
    attempt: Weak<Attempt>,
    origin: origin::Origin,
}
impl SealingSource {
    pub(in crate::harness) fn gate(&self) -> &SourceGateIdentity {
        self.origin.gate()
    }
    pub(in crate::harness) fn caller(&self) -> &slot::Caller {
        self.origin.caller()
    }
    pub(in crate::harness) fn source(&self) -> &slot::SourceHandle {
        self.origin.source()
    }
    pub(in crate::harness) fn current(&self) -> bool {
        if !self.origin.current() {
            return false;
        }
        let (Some(state), Some(cell), Some(attempt)) = (
            self.state.upgrade(),
            self.cell.upgrade(),
            self.attempt.upgrade(),
        ) else {
            return false;
        };
        let Ok(state) = lock(&state) else {
            return false;
        };
        if same_cell(&state, &cell, self.origin.caller()).is_err() {
            return false;
        }
        lock(&cell).is_ok_and(|cell| cell.phase.matches(&attempt))
    }
}

/// A weak handle to custody retained by the native registry, not transferable
/// credential authority and not permission to install or initialize target B.
pub struct SourceSeal {
    kernel: Weak<ReservationFamilyExclusive>,
}
impl SourceSeal {
    pub fn current(&self) -> bool {
        self.kernel
            .upgrade()
            .is_some_and(|guard| guard.current().is_ok())
    }
}
impl std::fmt::Debug for SourceSeal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceSeal(redacted)")
    }
}

#[derive(Clone, Copy)]
enum UsePhase {
    Pending,
    Active,
    Ending,
}
struct CapturedUse {
    key: String,
    counter: u64,
    phase: UsePhase,
    source: Arc<slot::SourceHandle>,
    gate: SourceGateIdentity,
}
struct Snapshot {
    origin: origin::Origin,
    issued: BTreeMap<String, String>,
    loaded: Vec<(String, LoadedMember)>,
    retired: Vec<String>,
    uses: Vec<CapturedUse>,
}
impl Snapshot {
    fn capture(state: &State, cell: &Cell) -> Result<Self, Error> {
        let actor = state
            .actors
            .get(&actor_key(cell.target.caller()))
            .ok_or(Error::Stale)?;
        if actor.process != cell.process
            || actor.issued.keys().any(|id| !cell.issued.contains_key(id))
        {
            return Err(Error::Stale);
        }
        let source_namespace = cell
            .origin
            .source()
            .namespaces()
            .ok_or(Error::Unavailable)?;
        let prefix = format!("{}:", actor_key(cell.target.caller()));
        if state.preloads.keys().any(|key| key.starts_with(&prefix)) {
            return Err(Error::Busy);
        }
        let mut result = Self {
            origin: cell.origin.clone(),
            issued: cell.issued.clone(),
            loaded: Vec::new(),
            retired: Vec::new(),
            uses: Vec::new(),
        };
        for (id, operation) in &cell.issued {
            let Some(issued) = actor.issued.get(id) else {
                result.retired.push(id.clone());
                continue;
            };
            if issued.operation != *operation {
                return Err(Error::Stale);
            }
            let key = member_key(cell.target.caller(), id)?;
            // Unrelated-role constructor evidence will join through its own
            // retained source allocation proof. A role enum alone is not that proof.
            let member = state.members.get(&key).ok_or(Error::Busy)?;
            let loaded = LoadedMember::capture(member)?;
            let namespace = loaded.source.namespaces().ok_or(Error::Unavailable)?;
            if loaded.source.binding() == cell.origin.source().binding()
                || namespace.shares_storage(source_namespace)
            {
                if !cell.origin.matches_authority(&loaded.source) {
                    return Err(Error::Stale);
                }
                let quiet = cell.quiescent.get(id).ok_or(Error::Busy)?;
                if quiet.registration != loaded.registration
                    || quiet.generation != loaded.source.credential_generation()
                    || quiet.digest != loaded.source.digest()
                    || loaded.role != Some(quiet.role)
                    || has_active_uses(state, &key)
                {
                    return Err(Error::Busy);
                }
            }
            result.loaded.push((id.clone(), loaded));
        }
        // A departed/rebound member can still own old A work. Its current row
        // cannot erase the source authority retained by Pending/Active custody.
        for (key, uses) in &state.uses {
            if !key.starts_with(&format!("{}:", actor_key(cell.target.caller()))) {
                continue;
            }
            for (counter, item) in &uses.pending {
                result.capture_use(
                    source_namespace,
                    key,
                    *counter,
                    UsePhase::Pending,
                    &item.source,
                    &item.gate,
                )?;
            }
            for (counter, item) in &uses.active {
                result.capture_use(
                    source_namespace,
                    key,
                    *counter,
                    UsePhase::Active,
                    &item.source,
                    &item.gate,
                )?;
            }
            for (counter, item) in &uses.ending {
                if !item.settled() {
                    result.capture_use(
                        source_namespace,
                        key,
                        *counter,
                        UsePhase::Ending,
                        &item.source,
                        &item.gate,
                    )?;
                }
            }
        }
        Ok(result)
    }
    fn capture_use(
        &mut self,
        namespace: &slot::SourceNamespaces,
        key: &str,
        counter: u64,
        phase: UsePhase,
        source: &Arc<slot::SourceHandle>,
        gate: &SourceGateIdentity,
    ) -> Result<(), Error> {
        if source.binding() == self.origin.source().binding()
            || source
                .namespaces()
                .ok_or(Error::Unavailable)?
                .shares_storage(namespace)
        {
            return Err(Error::Busy);
        }
        self.uses.push(CapturedUse {
            key: key.into(),
            counter,
            phase,
            source: source.clone(),
            gate: gate.clone(),
        });
        Ok(())
    }
    fn probe(&self) -> Result<(), Error> {
        if !self.origin.current() || self.loaded.iter().any(|(_, member)| !member.observed()) {
            return Err(Error::Stale);
        }
        for (source, gate) in std::iter::once((self.origin.source().as_ref(), self.origin.gate()))
            .chain(
                self.loaded
                    .iter()
                    .map(|(_, loaded)| (loaded.source.as_ref(), &loaded.gate)),
            )
            .chain(
                self.uses
                    .iter()
                    .map(|item| (item.source.as_ref(), &item.gate)),
            )
        {
            slot::authenticate_captured_namespaces(self.origin.caller(), source, gate)
                .map_err(|_| Error::Stale)?;
        }
        Ok(())
    }
    fn matches(&self, state: &State, cell: &Cell) -> bool {
        if cell.issued != self.issued
            || state
                .preloads
                .keys()
                .any(|key| key.starts_with(&format!("{}:", actor_key(cell.target.caller()))))
        {
            return false;
        }
        let Some(actor) = state.actors.get(&actor_key(cell.target.caller())) else {
            return false;
        };
        if actor.process != cell.process
            || actor.issued.keys().any(|id| !cell.issued.contains_key(id))
            || self.retired.iter().any(|id| actor.issued.contains_key(id))
        {
            return false;
        }
        if self.loaded.iter().any(|(id, loaded)| {
            member_key(cell.target.caller(), id)
                .ok()
                .and_then(|key| state.members.get(&key))
                .is_none_or(|member| !loaded.matches(member))
        }) {
            return false;
        }
        let prefix = format!("{}:", actor_key(cell.target.caller()));
        let total = state
            .uses
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(_, uses)| {
                uses.active.len()
                    + uses.pending.len()
                    + uses
                        .ending
                        .values()
                        .filter(|ending| !ending.settled())
                        .count()
            })
            .sum::<usize>();
        total == self.uses.len()
            && self.uses.iter().all(|item| {
                let Some(uses) = state.uses.get(&item.key) else {
                    return false;
                };
                match item.phase {
                    UsePhase::Pending => uses.pending.get(&item.counter).is_some_and(|current| {
                        Arc::ptr_eq(&item.source, &current.source) && item.gate == current.gate
                    }),
                    UsePhase::Active => uses.active.get(&item.counter).is_some_and(|current| {
                        Arc::ptr_eq(&item.source, &current.source) && item.gate == current.gate
                    }),
                    UsePhase::Ending => uses.ending.get(&item.counter).is_some_and(|current| {
                        !current.settled()
                            && Arc::ptr_eq(&item.source, &current.source)
                            && item.gate == current.gate
                    }),
                }
            })
    }
}

impl Reservation {
    /// No account/owner claims survive this operation. The kernel exclusion
    /// remains registry-owned even if delivery or its handle is lost.
    pub fn seal(&self, env: &Env, peer: &Peer) -> Result<SourceSeal, Error> {
        if !peer.current() {
            return Err(Error::Stale);
        }
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let (snapshot, attempt, existing) = {
            let state = lock(&state)?;
            same_cell(&state, &self.cell, &peer.caller)?;
            let cell = lock(&self.cell)?;
            if cell.process != peer.process || cell.target.caller() != &peer.caller {
                return Err(Error::Stale);
            }
            if !matches!(cell.phase, Phase::Sealed { .. }) {
                reservation_scope(&state, &cell._scope)?;
            }
            let snapshot = Snapshot::capture(&state, &cell)?;
            let source_namespace = snapshot
                .origin
                .source()
                .namespaces()
                .ok_or(Error::Unavailable)?;
            let independent = snapshot
                .loaded
                .iter()
                .filter(|(_, loaded)| {
                    loaded.source.binding() != snapshot.origin.source().binding()
                        && loaded
                            .source
                            .namespaces()
                            .is_some_and(|namespace| !namespace.shares_storage(source_namespace))
                })
                .map(|(id, loaded)| (id.clone(), loaded.source.clone()))
                .collect();
            let existing = match &cell.phase {
                Phase::Sealed { kernel, .. } => Some(kernel.clone()),
                _ => None,
            };
            let attempt = match &cell.phase {
                Phase::Draining => Arc::new(Attempt {
                    worker: false.into(),
                    independent,
                }),
                Phase::Sealing(attempt) | Phase::Sealed { attempt, .. } => attempt.clone(),
            };
            (snapshot, attempt, existing)
        };
        if let Some(kernel) = existing {
            kernel.current().map_err(|_| Error::Stale)?;
            return Ok(SourceSeal {
                kernel: Arc::downgrade(&kernel),
            });
        }
        let _worker = Worker::acquire(attempt.clone())?;
        if !snapshot
            .origin
            .gate()
            .matches_home(env.home.as_deref().ok_or(Error::Unavailable)?)
        {
            return Err(Error::Stale);
        }
        slot::authenticate_source_scope(env, &peer.caller, snapshot.origin.source())
            .map_err(|_| Error::Stale)?;
        snapshot.probe()?;
        {
            let state = lock(&state)?;
            same_cell(&state, &self.cell, &peer.caller)?;
            let mut cell = lock(&self.cell)?;
            if !snapshot.matches(&state, &cell) {
                return Err(Error::Busy);
            }
            match &cell.phase {
                Phase::Draining => cell.phase = Phase::Sealing(attempt.clone()),
                phase if phase.matches(&attempt) => (),
                _ => return Err(Error::Stale),
            }
        }
        let source = SealingSource {
            state: Arc::downgrade(&state),
            cell: Arc::downgrade(&self.cell),
            attempt: Arc::downgrade(&attempt),
            origin: snapshot.origin.clone(),
        };
        let kernel = Arc::new(
            ReservationFamilyExclusive::acquire(source).map_err(|error| {
                if error.contains("busy") {
                    Error::Busy
                } else {
                    Error::Unavailable
                }
            })?,
        );
        snapshot.probe()?;
        {
            let state = lock(&state)?;
            same_cell(&state, &self.cell, &peer.caller)?;
            let mut cell = lock(&self.cell)?;
            if !snapshot.matches(&state, &cell) || !cell.phase.matches(&attempt) {
                return Err(Error::Stale);
            }
            cell.phase = Phase::Sealed {
                attempt,
                kernel: kernel.clone(),
            };
        }
        Ok(SourceSeal {
            kernel: Arc::downgrade(&kernel),
        })
    }
}

#[cfg(all(test, unix))]
pub(super) fn kernel_fixture(reservation: &Reservation) -> SourceSeal {
    // Explicit synthetic registry injection: tests only kernel retention and
    // phase admission. It does not qualify MAC scope, SDK image or a census.
    let state = reservation.state.upgrade().unwrap();
    let attempt = Arc::new(Attempt {
        worker: false.into(),
        independent: BTreeMap::new(),
    });
    let origin = {
        let mut cell = reservation.cell.lock().unwrap();
        cell.phase = Phase::Sealing(attempt.clone());
        cell.origin.clone()
    };
    let source = SealingSource {
        state: Arc::downgrade(&state),
        cell: Arc::downgrade(&reservation.cell),
        attempt: Arc::downgrade(&attempt),
        origin,
    };
    let kernel = Arc::new(ReservationFamilyExclusive::acquire(source).unwrap());
    reservation.cell.lock().unwrap().phase = Phase::Sealed {
        attempt,
        kernel: kernel.clone(),
    };
    SourceSeal {
        kernel: Arc::downgrade(&kernel),
    }
}

#[cfg(all(test, unix))]
pub(super) fn metadata_census_fixture(reservation: &Reservation) -> Result<(), Error> {
    // Only census arithmetic over explicitly injected synthetic sources.
    let state = reservation.state.upgrade().ok_or(Error::Stale)?;
    let state = lock(&state)?;
    let cell = lock(&reservation.cell)?;
    Snapshot::capture(&state, &cell).map(|_| ())
}

#[cfg(all(test, unix))]
pub(super) fn independent_phase_fixture(
    reservation: &Reservation,
    member: &str,
    source: Arc<slot::SourceHandle>,
) {
    // Tests only the phase-before-allocation boundary, not independence proof.
    reservation.cell.lock().unwrap().phase = Phase::Sealing(Arc::new(Attempt {
        worker: false.into(),
        independent: BTreeMap::from([(member.into(), source)]),
    }));
}
