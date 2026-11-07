// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scheduling fulfillment under the existing alignment lock. Nothing here admits a
//! credential, repository destination or Git effect; the native batch does that afresh.

use super::{AlignLock, Record, read_record, try_lock, write_record};
use aterm_converge::FileJournal;
use aterm_secrets::converge::{
    FIRST_RETRY_MS, LONGEST_RETRY_MS, RECHECK_MS, RefreshObservation, RefreshRequest, RefreshScope,
    RefreshVersion,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Only this subrecord is changed by a plain repository-only init.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshState {
    scope: Option<RefreshScope>,
    version: RefreshVersion,
    pending: Option<RefreshRequest>,
    running: Option<Attempt>,
    fulfilled: Option<Fulfilled>,
    retry: Option<Retry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Attempt {
    id: u64,
    input: u64,
    scope: RefreshScope,
    started_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Fulfilled {
    attempt: Attempt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Retry {
    at_ms: u64,
    delay_ms: u64,
}

fn remaining(now: u64, at: u64, delay: u64) -> u64 {
    // Epoch clocks are shared across processes/restarts. Rollback makes age
    // unknown and work due, rather than serializing process-local elapsed time.
    now.checked_sub(at)
        .map_or(0, |age| delay.saturating_sub(age))
}

impl RefreshState {
    fn observation(&self, now: u64) -> RefreshObservation {
        let mut version = self.version;
        if self.fulfilled.is_none_or(|done| {
            done.attempt.input != version.input || Some(done.attempt.scope) != self.scope
        }) {
            version.fulfilled = 0;
        }
        RefreshObservation {
            version,
            scope: self.scope,
            pending: self.pending,
            retry_ms: self
                .retry
                .map(|retry| remaining(now, retry.at_ms, retry.delay_ms.min(LONGEST_RETRY_MS))),
            recheck_ms: self
                .fulfilled
                .map(|done| remaining(now, done.attempt.started_ms, RECHECK_MS)),
        }
    }
}

/// Read scheduling facts without taking the effect lock or opening private inputs.
#[must_use]
pub fn observe(state: &Path, now_ms: u64) -> RefreshObservation {
    read_record(state)
        .and_then(|record| record.repositories)
        .unwrap_or_default()
        .observation(now_ms)
}

/// Per-owner observation memo of scheduling facts only. Unchanged metadata avoids
/// reparsing the generic pass record on every existing listener tick.
#[derive(Default)]
pub struct Observer {
    version: Option<Result<[u8; 32], std::io::ErrorKind>>,
    state: RefreshState,
}

impl Observer {
    pub fn read(
        &mut self,
        sys: &dyn crate::port::System,
        state: &Path,
        now_ms: u64,
    ) -> RefreshObservation {
        let version = sys
            .file_version(&state.join(FileJournal::DIR).join(super::RECORD))
            .map_err(|e| e.kind());
        if self.version.as_ref() != Some(&version)
            || version
                .as_ref()
                .is_err_and(|e| *e != std::io::ErrorKind::NotFound)
        {
            self.state = read_record(state)
                .and_then(|record| record.repositories)
                .unwrap_or_default();
            self.version = Some(version);
        }
        self.state.observation(now_ms)
    }
}

/// One admitted wave. Its identity cannot be used to authorize a later effect.
#[derive(Clone, Copy)]
pub struct Ticket(Attempt);

impl Ticket {
    pub fn scope(self) -> RefreshScope {
        self.0.scope
    }
}

/// The batch either owns a new wave, shares a completed wave, or owes a retry.
pub enum Admission {
    /// A fresh remote wave is required.
    Refresh(Ticket),
    /// Another owner already completed this same request.
    Fulfilled,
    /// A failed shared wave is still within its ordinary retry delay.
    Waiting,
}

/// The observation is sampled before acquiring the lock, so an explicit request
/// arriving during a wave retains its strictly-later-attempt boundary.
pub struct Access {
    pub observed: RefreshObservation,
    pub pass: Option<Arc<RepositoryPass>>,
}

#[derive(Default)]
pub struct Memory {
    record: Mutex<Option<Record>>,
    held: AtomicBool,
}

enum Storage {
    File { state: PathBuf, _held: AlignLock },
    Memory(Arc<Memory>),
}

/// A single owner of align.lock, shared through sync and its native repository
/// batch. A native batch inside sync never tries to acquire this lock again.
pub struct RepositoryPass {
    storage: Storage,
    record: Mutex<Record>,
}

fn empty_record() -> Record {
    Record {
        build: String::new(),
        spec: 0,
        builds: Vec::new(),
        attempted_ms: 0,
        converged_ms: None,
        routes: Vec::new(),
        rows: Vec::new(),
        base: None,
        repositories: None,
    }
}

impl RepositoryPass {
    /// Nonwaiting access to this account's existing alignment record/lock.
    /// # Errors
    /// The private record directory or lock cannot be admitted.
    pub fn open(state: &Path, now_ms: u64) -> Result<Access, String> {
        let observed = observe(state, now_ms);
        let pass = try_lock(state)?.map(|held| {
            Arc::new(Self {
                storage: Storage::File {
                    state: state.to_owned(),
                    _held: held,
                },
                record: Mutex::new(read_record(state).unwrap_or_else(empty_record)),
            })
        });
        Ok(Access { observed, pass })
    }

    /// An inert persistent record with an actual exclusive lease, used by System
    /// implementations whose effects and journal already live in memory.
    #[must_use]
    pub fn memory(memory: &Arc<Memory>, now_ms: u64) -> Access {
        let record = memory.record.lock().unwrap_or_else(PoisonError::into_inner);
        let observed = record
            .as_ref()
            .and_then(|r| r.repositories.as_ref())
            .cloned()
            .unwrap_or_default()
            .observation(now_ms);
        let pass = memory
            .held
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| {
                Arc::new(Self {
                    storage: Storage::Memory(Arc::clone(memory)),
                    record: Mutex::new(record.clone().unwrap_or_else(empty_record)),
                })
            });
        Access { observed, pass }
    }

    fn persist(&self, record: &Record) -> Result<(), String> {
        match &self.storage {
            Storage::File { state, .. } => write_record(&state.join(FileJournal::DIR), record),
            Storage::Memory(memory) => {
                *memory.record.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(record.clone());
                Ok(())
            }
        }
    }

    /// Read the outer pass record without granting authority over any inputs.
    #[must_use]
    pub fn record(&self) -> Record {
        self.record
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A generic pass preserves repository progress written by its native batch.
    /// # Errors
    /// The private record cannot be atomically written.
    pub fn record_pass(&self, mut newer: Record) -> Result<(), String> {
        let mut record = self.record.lock().unwrap_or_else(PoisonError::into_inner);
        newer.repositories = record.repositories.clone();
        self.persist(&newer)?;
        *record = newer;
        Ok(())
    }

    /// Admit scheduling work only after the caller validated its real captured
    /// account/source. Publishing this claim does not admit a Git effect.
    /// # Errors
    /// The request changed source, counters exhausted, or the record write failed.
    pub fn admit(
        &self,
        request: RefreshRequest,
        scope: RefreshScope,
        now: u64,
    ) -> Result<Admission, String> {
        if request.scope.is_some_and(|expected| expected != scope) {
            return Err("repository refresh inputs changed before the request was admitted".into());
        }
        let mut record = self.record.lock().unwrap_or_else(PoisonError::into_inner);
        let mut state = record.repositories.clone().unwrap_or_default();
        let changed = state.scope != Some(scope)
            || (request.changed && state.version.input <= request.base_input);
        if changed {
            state.version.input = state
                .version
                .input
                .max(request.base_input)
                .checked_add(1)
                .ok_or("repository input generation exhausted")?;
            state.scope = Some(scope);
            state.retry = None;
        }
        // A running claim left by a dead owner remains debt; no successful
        // fulfillment is invented when a replacement takes the same lock.
        if state.running.take().is_some() {
            let delay_ms = state.retry.map_or(FIRST_RETRY_MS, |retry| {
                retry.delay_ms.saturating_mul(2).min(LONGEST_RETRY_MS)
            });
            state.retry = Some(Retry {
                at_ms: now,
                delay_ms,
            });
        }
        let request = RefreshRequest {
            scope: Some(scope),
            ..request
        };
        let observation = state.observation(now);
        if observation.fulfills(request) {
            return Ok(Admission::Fulfilled);
        }
        let waiting = observation
            .retry_for(request)
            .is_some_and(|delay| delay > 0);
        state.pending = Some(
            match state.pending.filter(|pending| pending.scope == Some(scope)) {
                Some(older) => older.merge(request),
                None => request,
            },
        );
        if waiting {
            let mut candidate = record.clone();
            candidate.repositories = Some(state);
            self.persist(&candidate)?;
            *record = candidate;
            return Ok(Admission::Waiting);
        }
        state.version.attempt = state
            .version
            .attempt
            .max(
                state
                    .pending
                    .and_then(|pending| pending.after_attempt)
                    .unwrap_or(0),
            )
            .checked_add(1)
            .ok_or("repository refresh attempt sequence exhausted")?;
        let attempt = Attempt {
            id: state.version.attempt,
            input: state.version.input,
            scope,
            started_ms: now,
        };
        state.running = Some(attempt);
        let mut candidate = record.clone();
        candidate.repositories = Some(state);
        self.persist(&candidate)?;
        *record = candidate;
        Ok(Admission::Refresh(Ticket(attempt)))
    }

    /// A batch fulfills only after its complete local readback and full current
    /// source fence. Failure keeps the exact request and shared bounded retry.
    /// # Errors
    /// The admitted claim no longer matches, or its completion cannot be written.
    pub fn finish(&self, ticket: Ticket, complete: bool, now: u64) -> Result<(), String> {
        let mut record = self.record.lock().unwrap_or_else(PoisonError::into_inner);
        let mut candidate = record.clone();
        let state = candidate
            .repositories
            .as_mut()
            .ok_or("repository refresh claim is missing")?;
        if state.running != Some(ticket.0) || state.scope != Some(ticket.0.scope) {
            return Err("repository refresh claim changed before completion".into());
        }
        state.running = None;
        if complete {
            state.version.fulfilled = ticket.0.id;
            state.fulfilled = Some(Fulfilled { attempt: ticket.0 });
            state.pending = None;
            state.retry = None;
        } else {
            let delay_ms = state.retry.map_or(FIRST_RETRY_MS, |retry| {
                retry.delay_ms.saturating_mul(2).min(LONGEST_RETRY_MS)
            });
            state.retry = Some(Retry {
                at_ms: now,
                delay_ms,
            });
        }
        self.persist(&candidate)?;
        *record = candidate;
        Ok(())
    }
}

impl Drop for RepositoryPass {
    fn drop(&mut self) {
        if let Storage::Memory(memory) = &self.storage {
            memory.held.store(false, Ordering::Release);
        }
    }
}

/// Work and the already-owned pass supplied by an enclosing sync operation.
#[derive(Clone)]
pub struct Execution {
    pub request: aterm_secrets::converge::RepositoryRequest,
    pub pass: Arc<RepositoryPass>,
}

/// An ordinary caller captures its boundary before waiting for the lock; a
/// subscriber/node carries the boundary already captured by its owned request.
pub enum Job {
    Work(aterm_secrets::converge::RepositoryWork),
    Requested(aterm_secrets::converge::RepositoryRequest),
}

impl From<aterm_secrets::converge::RepositoryWork> for Job {
    fn from(work: aterm_secrets::converge::RepositoryWork) -> Self {
        Self::Work(work)
    }
}

impl From<aterm_secrets::converge::RepositoryRequest> for Job {
    fn from(request: aterm_secrets::converge::RepositoryRequest) -> Self {
        Self::Requested(request)
    }
}

impl Job {
    pub fn request(
        self,
        observed: RefreshObservation,
    ) -> aterm_secrets::converge::RepositoryRequest {
        use aterm_secrets::converge::{RepositoryRequest, RepositoryWork};
        match self {
            Self::Requested(request) => request,
            Self::Work(RepositoryWork::Repair) => RepositoryRequest::Repair,
            Self::Work(RepositoryWork::Refresh) => {
                RepositoryRequest::Refresh(RefreshRequest::after(None, observed.version))
            }
        }
    }
}
