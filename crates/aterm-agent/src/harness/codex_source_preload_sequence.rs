// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Semantic PRELOAD ordering is independent of transport request ordering.
//! This metadata never grants a read or proves that captured work has retired.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PreloadSequence {
    prefix: u64,
    counter: u64,
}
impl PreloadSequence {
    pub fn parse(word: &str) -> Option<Self> {
        if !hex(word, 32) {
            return None;
        }
        let sequence = Self {
            prefix: u64::from_str_radix(&word[..16], 16).ok()?,
            counter: u64::from_str_radix(&word[16..], 16).ok()?,
        };
        (sequence.counter > 0).then_some(sequence)
    }
    pub fn encoded(self) -> String {
        format!("{:016x}{:016x}", self.prefix, self.counter)
    }
}
impl std::fmt::Debug for PreloadSequence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreloadSequence(redacted)")
    }
}

/// Exact predecessor returned by a native PRELOAD step, not source authority.
#[derive(Clone, PartialEq, Eq)]
pub struct PreloadPredecessor {
    pub registration: String,
    pub credential_generation: u64,
    pub digest: String,
}
impl PreloadPredecessor {
    fn valid(&self) -> bool {
        hex(&self.registration, 32) && self.credential_generation > 0 && hex(&self.digest, 64)
    }
}
pub(super) enum ResampleAdmission {
    Read,
    Replay(Preload),
    ReadyCommitted,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Origin {
    Ordinary,
    Use {
        member: String,
        nonce: String,
        capability: String,
    },
    // Release correlation for an explicitly fenced, never-admitted attempt.
    // It can never match a source-reading admission.
    RetirementOnly,
    Target {
        assignment: String,
        context: String,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Attempt {
    pub(super) sequence: PreloadSequence,
    pub(super) operation: String,
    pub(super) origin: Origin,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Retirement {
    pub(super) operation: String,
    pub(super) sequence: PreloadSequence,
    pub(super) preload: String,
    pub(super) registration: Option<String>,
}

#[derive(Clone)]
struct WorkWitness {
    identity: crate::harness::account_work::OperationIdentity,
    cancellation: crate::harness::account_work::Cancellation,
}
#[derive(Clone)]
pub(super) struct Ledger {
    process: Process,
    attempt: Attempt,
    registration: Option<String>,
    revision: Option<(u64, String)>,
    ready: bool,
    ready_report: Option<(String, ReadyRole)>,
    initial_preload: Option<Preload>,
    pending_resample: Option<PreloadPredecessor>,
    resampled: Vec<(PreloadPredecessor, Preload)>,
    retirement: Option<Retirement>,
    closed: bool,
    witnesses: Vec<WorkWitness>,
    release: Option<Arc<crate::harness::account_work::ReleaseTurn>>,
}
impl Ledger {
    #[cfg(all(unix, any(test, debug_assertions)))]
    pub(super) fn proof_sequence(
        &self,
        operation: &str,
        registration: &str,
        role: Option<ReadyRole>,
    ) -> Option<String> {
        (self.attempt.operation == operation
            && self.registration.as_deref() == Some(registration)
            && self.ready
            && self.retirement.is_none()
            && self
                .ready_report
                .as_ref()
                .is_some_and(|(saved, report)| saved == registration && Some(*report) == role))
        .then(|| self.attempt.sequence.encoded())
    }
    #[cfg(all(unix, any(test, debug_assertions)))]
    pub(super) fn proof_closed(&self) -> bool {
        self.closed && self.release.is_none() && self.retired()
    }

    pub(super) fn rebound_completed_attempt(
        &self,
        process: &Process,
        operation: &str,
        sequence: PreloadSequence,
        registration: &str,
        role: ReadyRole,
    ) -> Result<Attempt, Error> {
        if &self.process != process
            || self.retirement.is_some()
            || self.closed
            || self.attempt.operation != operation
            || self.attempt.sequence != sequence
            || !self.completed(registration)
            || self.ready_report.as_ref() != Some(&(registration.into(), role))
        {
            return Err(Error::Stale);
        }
        Ok(self.attempt.clone())
    }
    /// The caller must already own the actual counted PRELOAD occurrence.
    pub(super) fn new(process: Process, attempt: Attempt) -> Self {
        Self {
            process,
            attempt,
            registration: None,
            revision: None,
            ready: false,
            ready_report: None,
            initial_preload: None,
            pending_resample: None,
            resampled: Vec::new(),
            retirement: None,
            closed: false,
            witnesses: Vec::new(),
            release: None,
        }
    }

    pub(super) fn check_attempt(&self, process: &Process, next: &Attempt) -> Result<(), Error> {
        if &self.process != process
            || self.retirement.is_some()
            || next.sequence.prefix != self.attempt.sequence.prefix
            || next.sequence.counter < self.attempt.sequence.counter
            || (next.sequence != self.attempt.sequence && next.operation == self.attempt.operation)
        {
            return Err(Error::Stale);
        }
        if next.sequence == self.attempt.sequence {
            return if next == &self.attempt {
                Ok(())
            } else {
                Err(Error::Stale)
            };
        }
        if !self.ready {
            return Err(Error::Busy);
        }
        Ok(())
    }

    /// Called in the same State transaction that retains real native custody.
    pub(super) fn accept(&mut self, process: &Process, next: Attempt) -> Result<(), Error> {
        self.check_attempt(process, &next)?;
        if next.sequence != self.attempt.sequence {
            self.attempt = next;
            self.registration = None;
            self.revision = None;
            self.ready = false;
            self.ready_report = None;
            self.initial_preload = None;
            self.pending_resample = None;
            self.resampled.clear();
        }
        Ok(())
    }

    pub(super) fn loaded(
        &mut self,
        attempt: &Attempt,
        registration: &str,
        generation: u64,
        digest: &str,
    ) -> Result<(), Error> {
        if self.retirement.is_some()
            || &self.attempt != attempt
            || !hex(registration, 32)
            || generation == 0
            || !hex(digest, 64)
        {
            return Err(Error::Stale);
        }
        if let Some((old_generation, old_digest)) = &self.revision
            && (!revision_descends(generation, digest, *old_generation, old_digest)
                || (generation == *old_generation
                    && self.registration.as_deref() != Some(registration))
                || (self.ready && self.registration.as_deref() != Some(registration)))
        {
            return Err(Error::Stale);
        }
        self.registration = Some(registration.into());
        self.revision = Some((generation, digest.into()));
        Ok(())
    }

    pub(super) fn initial_preload(&self, attempt: &Attempt) -> Option<Preload> {
        (&self.attempt == attempt && self.retirement.is_none())
            .then(|| self.initial_preload.clone())
            .flatten()
    }
    pub(super) fn record_initial(
        &mut self,
        attempt: &Attempt,
        preload: Preload,
    ) -> Result<(), Error> {
        if &self.attempt != attempt
            || self.retirement.is_some()
            || self.registration.as_deref() != Some(preload.registration.as_str())
        {
            return Err(Error::Stale);
        }
        if self.initial_preload.is_none() {
            self.initial_preload = Some(preload);
        }
        Ok(())
    }
    pub(super) fn resample_status(
        &self,
        attempt: &Attempt,
        previous: &PreloadPredecessor,
    ) -> Result<ResampleAdmission, Error> {
        if &self.attempt != attempt || self.retirement.is_some() || !previous.valid() {
            return Err(Error::Stale);
        }
        if let Some((_, result)) = self.resampled.iter().find(|(step, _)| step == previous) {
            return Ok(ResampleAdmission::Replay(result.clone()));
        }
        if self.registration.as_deref() != Some(previous.registration.as_str())
            || self.revision.as_ref().is_none_or(|(generation, digest)| {
                *generation != previous.credential_generation || digest != &previous.digest
            })
        {
            return Err(Error::Stale);
        }
        if self.ready {
            return Ok(ResampleAdmission::ReadyCommitted);
        }
        if self
            .pending_resample
            .as_ref()
            .is_some_and(|pending| pending != previous)
        {
            return Err(Error::Busy);
        }
        Ok(ResampleAdmission::Read)
    }
    pub(super) fn begin_resample(
        &mut self,
        attempt: &Attempt,
        previous: &PreloadPredecessor,
    ) -> Result<ResampleAdmission, Error> {
        let status = self.resample_status(attempt, previous)?;
        if matches!(&status, ResampleAdmission::Read) {
            self.pending_resample = Some(previous.clone());
        }
        Ok(status)
    }
    pub(super) fn finish_resample(
        &mut self,
        attempt: &Attempt,
        previous: &PreloadPredecessor,
        result: Preload,
    ) -> Result<(), Error> {
        if &self.attempt != attempt
            || self.retirement.is_some()
            || self.ready
            || self.pending_resample.as_ref() != Some(previous)
            || self.registration.as_deref() != Some(previous.registration.as_str())
            || result.registration == previous.registration
            || !hex(&result.registration, 32)
            || !revision_descends(
                result.source.credential_generation(),
                result.source.digest(),
                previous.credential_generation,
                &previous.digest,
            )
        {
            return Err(Error::Stale);
        }
        self.registration = Some(result.registration.clone());
        self.revision = Some((
            result.source.credential_generation(),
            result.source.digest().into(),
        ));
        self.ready_report = None;
        self.pending_resample = None;
        self.resampled.push((previous.clone(), result));
        // Eviction cannot authorize a reread: only the exact current native
        // predecessor is accepted. An evicted older step is permanently stale.
        if self.resampled.len() > 8 {
            self.resampled.remove(0);
        }
        Ok(())
    }

    pub(super) fn accept_ready(
        &mut self,
        attempt: &Attempt,
        registration: &str,
        role: ReadyRole,
    ) -> Result<(), Error> {
        if self.pending_resample.is_some()
            || self.retirement.is_some()
            || &self.attempt != attempt
            || self.registration.as_deref() != Some(registration)
            || self
                .ready_report
                .as_ref()
                .is_some_and(|(previous, reported)| previous != registration || *reported != role)
        {
            return Err(Error::Stale);
        }
        self.ready_report = Some((registration.into(), role));
        Ok(())
    }

    pub(super) fn recorded_ready(
        &self,
        attempt: &Attempt,
        registration: &str,
        role: ReadyRole,
    ) -> bool {
        self.ready
            && self.retirement.is_none()
            && &self.attempt == attempt
            && self
                .ready_report
                .as_ref()
                .is_some_and(|(previous, reported)| previous == registration && *reported == role)
    }

    pub(super) fn complete(&mut self, attempt: &Attempt, registration: &str) -> Result<(), Error> {
        if self.pending_resample.is_some()
            || self.retirement.is_some()
            || &self.attempt != attempt
            || self.registration.as_deref() != Some(registration)
        {
            return Err(Error::Stale);
        }
        self.ready = true;
        Ok(())
    }

    /// Accept a release-only fence, never a source/read permission. A higher
    /// unaccepted attempt is legitimate only with None (lost/unsent ACK), and
    /// does not erase the actual older worker or its counted custody.
    pub(super) fn fence(&mut self, process: &Process, body: Retirement) -> Result<(), Error> {
        if &self.process != process
            || !hex(&body.operation, 32)
            || !hex(&body.preload, 32)
            || body.registration.as_ref().is_some_and(|r| !hex(r, 32))
            || body.sequence.prefix != self.attempt.sequence.prefix
            || body.sequence.counter < self.attempt.sequence.counter
        {
            return Err(Error::Stale);
        }
        if let Some(current) = &self.retirement {
            return if current == &body {
                Ok(())
            } else {
                Err(Error::Stale)
            };
        }
        if body.sequence == self.attempt.sequence {
            if body.preload != self.attempt.operation
                || body
                    .registration
                    .as_ref()
                    .is_some_and(|r| self.registration.as_ref() != Some(r))
            {
                return Err(Error::Stale);
            }
        } else if body.registration.is_some() {
            return Err(Error::Stale);
        }
        self.retirement = Some(body);
        Ok(())
    }

    pub(super) fn exact_closed(&self, process: &Process, body: &Retirement) -> bool {
        &self.process == process && self.closed && self.retirement.as_ref() == Some(body)
    }
    pub(super) fn exact_fence(&self, process: &Process, body: &Retirement) -> bool {
        &self.process == process && self.retirement.as_ref() == Some(body)
    }

    /// Only the outer retirement transaction can establish actual last-use,
    /// worker/Scope retirement and target OPEN closure before calling this.
    pub(super) fn close(&mut self, process: &Process, body: &Retirement) -> Result<(), Error> {
        if &self.process != process
            || self.retirement.as_ref() != Some(body)
            || self
                .witnesses
                .iter()
                .any(|witness| !witness.cancellation.retired())
        {
            return Err(Error::Stale);
        }
        self.closed = true;
        Ok(())
    }
    pub(super) fn retain_retirement(
        &mut self,
        scope: &crate::harness::account_work::Scope,
    ) -> Result<(), Error> {
        let identity = scope.operation_identity();
        self.witnesses
            .retain(|witness| !witness.cancellation.retired());
        if self
            .witnesses
            .iter()
            .any(|witness| witness.identity == identity)
        {
            return Ok(());
        }
        if self.witnesses.len() >= MAX_MEMBERS {
            return Err(Error::Busy);
        }
        self.witnesses.push(WorkWitness {
            identity,
            cancellation: scope.cancellation(),
        });
        Ok(())
    }
    pub(super) fn retired(&self) -> bool {
        self.witnesses
            .iter()
            .all(|witness| witness.cancellation.retired())
    }
    pub(super) fn fenced(&self) -> bool {
        self.retirement.is_some()
    }
    pub(super) fn completed(&self, registration: &str) -> bool {
        self.ready
            && self.retirement.is_none()
            && self.registration.as_deref() == Some(registration)
    }
    pub(super) fn retain_release(
        &mut self,
        release: &Arc<crate::harness::account_work::ReleaseTurn>,
    ) -> Arc<crate::harness::account_work::ReleaseTurn> {
        self.release.get_or_insert_with(|| release.clone()).clone()
    }
    pub(super) fn take_release(
        &mut self,
    ) -> Option<Arc<crate::harness::account_work::ReleaseTurn>> {
        self.release.take()
    }
    pub(super) fn cancel_captured_work(&self) {
        for witness in &self.witnesses {
            witness.cancellation.cancel();
        }
    }
}

pub(super) fn check_current(
    state: &State,
    key: &str,
    process: &Process,
    attempt: &Attempt,
) -> Result<(), Error> {
    let ledger = state.preload_sequences.get(key).ok_or(Error::Stale)?;
    if &ledger.process != process || &ledger.attempt != attempt || ledger.retirement.is_some() {
        return Err(Error::Stale);
    }
    Ok(())
}

pub(super) fn accept(
    state: &mut State,
    key: &str,
    process: &Process,
    attempt: &Attempt,
    scope: &crate::harness::account_work::Scope,
) -> Result<(), Error> {
    if !hex(&attempt.operation, 32) {
        return Err(Error::Denied);
    }
    let mut ledger = if let Some(ledger) = state.preload_sequences.get(key) {
        let mut ledger = ledger.clone();
        ledger.accept(process, attempt.clone())?;
        ledger
    } else {
        capacity(state, key)?;
        // The enclosing admission transaction must first validate actual
        // Issued membership and retain counted native custody.
        Ledger::new(process.clone(), attempt.clone())
    };
    ledger.retain_retirement(scope)?;
    state.preload_sequences.insert(key.into(), ledger);
    Ok(())
}

pub(super) fn capacity(state: &State, key: &str) -> Result<(), Error> {
    if state.preload_sequences.contains_key(key) {
        return Ok(());
    }
    let actor = key
        .rsplit_once(':')
        .map(|(actor, _)| actor)
        .ok_or(Error::Denied)?;
    if state.preload_sequences.len() >= 4096
        || state
            .preload_sequences
            .keys()
            .filter(|key| key.starts_with(&format!("{actor}:")))
            .count()
            >= 256
    {
        return Err(Error::Busy);
    }
    Ok(())
}

// Each observes this process (`Process::observe`), which only Unix reads.
#[cfg(all(test, unix))]
#[path = "codex_source_preload_sequence_tests.rs"]
mod tests;
