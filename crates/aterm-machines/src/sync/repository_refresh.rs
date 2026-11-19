// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scheduling fulfillment under the existing alignment lock. Nothing here admits a
//! credential, repository destination or Git effect; the native batch does that afresh.
//!
//! The record also holds the pass's own Git writes to the checkouts the repository input
//! observer watches ([`CheckoutWrites`]), as digests of what the observer reads, so the observer
//! takes none of them for new work.

use super::{AlignLock, Record, StarterWatch, read_record, try_lock, wait_lock, write_record};
use aterm_converge::FileJournal;
use aterm_secrets::converge::{
    FIRST_RETRY_MS, LONGEST_RETRY_MS, RECHECK_MS, RefreshObservation, RefreshRequest, RefreshScope,
    RefreshVersion,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

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
    /// The passes' own Git writes to the checkouts ([`CheckoutWrites`]).
    #[serde(default)]
    own: CheckoutWrites,
    /// Empty-origin observations, bound to their account/source and exact local storage.
    #[serde(default, skip_serializing_if = "EmptyCheckouts::is_empty")]
    empty: EmptyCheckouts,
}

/// Durable empty-origin observations. The keys contain only digests; consuming one still
/// requires fresh account/source admission and the exact checkout's physical and ref identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyCheckouts {
    scope: Option<RefreshScope>,
    subjects: std::collections::BTreeMap<CheckoutDigest, CheckoutDigest>,
}

impl EmptyCheckouts {
    fn is_empty(&self) -> bool {
        self.subjects.is_empty()
    }

    pub(crate) fn has_checkout(&self, scope: RefreshScope, checkout: CheckoutDigest) -> bool {
        self.scope == Some(scope) && self.subjects.contains_key(&checkout)
    }

    pub(crate) fn contains(
        &self,
        scope: RefreshScope,
        checkout: CheckoutDigest,
        subject: CheckoutDigest,
    ) -> bool {
        self.scope == Some(scope) && self.subjects.get(&checkout) == Some(&subject)
    }
}

/// Read empty-origin observations from the account's admitted private alignment record.
/// Missing or unreadable evidence proves nothing and requires a new remote observation.
#[must_use]
pub fn empty_checkouts(state: &Path) -> EmptyCheckouts {
    read_record(state)
        .and_then(|record| record.repositories)
        .map_or_else(EmptyCheckouts::default, |record| record.empty)
}

/// Encode one readable receipt. Optional empty-origin observations may be evicted to keep
/// the record within its reader's budget; scheduling epochs, rows and own writes are retained.
/// The file and in-memory backends keep the same reduced evidence.
pub(super) fn encode_record(record: &mut Record) -> Result<String, String> {
    let encode = |record: &Record| aterm_json::to_string(record).map_err(|why| why.to_string());
    let mut text = encode(record)?;
    if text.len().saturating_add(1) > super::RECORD_BYTES
        && let Some(state) = record
            .repositories
            .as_mut()
            .filter(|state| !state.empty.is_empty())
    {
        let scope = state.empty.scope;
        let subjects: Vec<_> = std::mem::take(&mut state.empty.subjects)
            .into_iter()
            .collect();
        let keep = |record: &mut Record, count: usize| {
            record
                .repositories
                .as_mut()
                .expect("the record retains its repository state")
                .empty = if count == 0 {
                EmptyCheckouts::default()
            } else {
                EmptyCheckouts {
                    scope,
                    subjects: subjects.iter().take(count).copied().collect(),
                }
            };
        };
        let (mut lower, mut upper) = (0, subjects.len() - 1);
        while lower < upper {
            let middle = lower + (upper - lower).div_ceil(2);
            keep(record, middle);
            if encode(record)?.len().saturating_add(1) <= super::RECORD_BYTES {
                lower = middle;
            } else {
                upper = middle - 1;
            }
        }
        keep(record, lower);
        text = encode(record)?;
    }
    if text.len().saturating_add(1) > super::RECORD_BYTES {
        return Err(format!(
            "converge record exceeds its read budget of {} bytes",
            super::RECORD_BYTES
        ));
    }
    text.push('\n');
    Ok(text)
}

/// A digest of one checkout's Git metadata as the repository input observer reads it
/// ([`crate::init_github::RepositoryCheckoutMetadata`]), or of the folder that names a checkout
/// ([`Self::folder`]): never a path, and nothing read from inside a file.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CheckoutDigest([u8; 32]);

impl CheckoutDigest {
    /// A domain-separated SHA-256 its maker computed.
    #[must_use]
    pub const fn new(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// Which checkout `folder` is, as a digest of its path: a write names its checkout by this.
    #[must_use]
    pub fn folder(folder: &Path) -> Self {
        let mut digest = aterm_digest::Sha256::new();
        digest.update(b"aterm checkout folder/1\0");
        digest.update(folder.as_os_str().as_encoded_bytes());
        Self(digest.finalize())
    }
}

impl std::fmt::Debug for CheckoutDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CheckoutDigest(..)")
    }
}

/// Lowercase hexadecimal digits, by value.
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Kept as 64 lowercase hexadecimal digits.
impl Serialize for CheckoutDigest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let text: String = self
            .0
            .iter()
            .flat_map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0x0f)]])
            .map(char::from)
            .collect();
        serializer.serialize_str(&text)
    }
}

impl<'de> Deserialize<'de> for CheckoutDigest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let digit = |byte: u8| {
            HEX.iter()
                .position(|hex| *hex == byte)
                .and_then(|value| u8::try_from(value).ok())
        };
        let mut digest = [0_u8; 32];
        if text.len() != 2 * digest.len() {
            return Err(serde::de::Error::custom(
                "a checkout digest is 64 hexadecimal digits",
            ));
        }
        for (slot, pair) in digest.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
            let (Some(high), Some(low)) = (digit(pair[0]), digit(pair[1])) else {
                return Err(serde::de::Error::custom(
                    "a checkout digest is lowercase hexadecimal digits",
                ));
            };
            *slot = (high << 4) | low;
        }
        Ok(Self(digest))
    }
}

/// The process a pass runs in, as the kernel names it while it lives: its pid and the second it
/// started ([`crate::port::System::process_started`]), so a pid used again names no other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Holder {
    pub pid: u32,
    pub started_s: u64,
}

/// One of a pass's own Git effects on a checkout: the checkout as the input observer reads it
/// just before the effect began, and once it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckoutWrite {
    /// The pass that made it ([`CheckoutWrites::pass`]).
    pass: u64,
    /// Which checkout ([`CheckoutDigest::folder`]).
    checkout: CheckoutDigest,
    before: CheckoutDigest,
    /// `None` while the effect runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after: Option<CheckoutDigest>,
    /// The effect made the checkout or a required dependency of it (a clone, an unfinished
    /// checkout's first commit), so what moved includes what makes a checkout.
    #[serde(default)]
    made: bool,
}

/// The most writes [`CheckoutWrites`] keeps of one checkout: twice the effects one pass makes on
/// it at most (a fast-forward, its dependencies' repair, the tuning; a clone with its own), so an
/// observer that last looked before two passes ran back to back follows all of them.
const WRITES_KEPT: usize = 8;

/// The most writes [`CheckoutWrites`] keeps in all, the oldest finished ones let go first.
const ALL_WRITES_KEPT: usize = 1024;

/// The passes' own Git writes to the checkouts the repository input observer watches, which it
/// takes for no new work where they account for what it reads ([`Self::account`]). A
/// checkout's writes are kept in order while each begins where the one before it ended; a write
/// that begins elsewhere (something else moved the checkout between them) lets the earlier ones
/// go, since no observer could follow them to it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckoutWrites {
    /// The number of the pass that recorded last, each pass that records taking the next.
    pass: u64,
    /// That pass's process, where it could name it: an effect it began runs while it lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    holder: Option<Holder>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    writes: Vec<CheckoutWrite>,
}

/// How a pass's own writes account for a checkout's move ([`CheckoutWrites::account`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accounted {
    /// Its finished writes take the checkout from where the observer last read it to where it
    /// reads it now, exactly: `made` where one of them made the checkout or a dependency of it.
    Own { made: bool },
    /// They take it to an effect of a pass that still runs: what the observer reads now is that
    /// effect's work under way, which it judges once the effect ended.
    Running,
    /// They do not: something else moved it.
    Not,
}

impl CheckoutWrites {
    /// How these writes take the checkout `checkout` from `from` to `to`: a run of its finished
    /// writes, each beginning where the one before it ended, from `from` to `to` is
    /// [`Accounted::Own`]; one from `from` that reaches an effect begun by the pass that recorded
    /// last, where `running` says that pass still runs ([`Self::running`]), is
    /// [`Accounted::Running`]; anything else is [`Accounted::Not`].
    #[must_use]
    pub fn account(
        &self,
        checkout: CheckoutDigest,
        from: CheckoutDigest,
        to: CheckoutDigest,
        running: bool,
    ) -> Accounted {
        let chain: Vec<&CheckoutWrite> = self
            .writes
            .iter()
            .filter(|write| write.checkout == checkout)
            .collect();
        let mut accounted = Accounted::Not;
        for (start, first) in chain.iter().enumerate() {
            if first.before != from {
                continue;
            }
            let (mut at, mut made) = (from, false);
            for write in chain.iter().skip(start) {
                if write.before != at {
                    break;
                }
                let Some(after) = write.after else {
                    if running && write.pass == self.pass {
                        accounted = Accounted::Running;
                    }
                    break;
                };
                made |= write.made;
                at = after;
                if at == to {
                    return Accounted::Own { made };
                }
            }
        }
        accounted
    }

    /// Whether the pass that recorded last still runs an effect it began: its process, as the
    /// kernel names it now, is the one it recorded.
    #[must_use]
    pub fn running(&self, sys: &dyn crate::port::System) -> bool {
        self.writes
            .iter()
            .any(|write| write.after.is_none() && write.pass == self.pass)
            && self
                .holder
                .is_some_and(|holder| sys.process_started(holder.pid) == Some(holder.started_s))
    }
}

/// The passes' own Git writes to the checkouts as the record under `state` holds them: none
/// where it holds none or cannot be read.
#[must_use]
pub fn checkout_writes(state: &Path) -> CheckoutWrites {
    read_record(state)
        .and_then(|record| record.repositories)
        .map(|repositories| repositories.own)
        .unwrap_or_default()
}

/// An effect [`RepositoryPass::begin_write`] recorded as begun, for
/// [`RepositoryPass::end_write`].
#[derive(Clone, Copy, Debug)]
pub struct OpenWrite {
    pass: u64,
    checkout: CheckoutDigest,
    before: CheckoutDigest,
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

/// Whether another owner's refresh wave is under way, read without the lease: the shared
/// record holds a refresh claim (its `running` attempt, set between an admission and its
/// finish) while this process holds no repository lease of its own ([`RepositoryPass::open`]),
/// so the claim is not this process's. A claim a dead owner left reads as running until the
/// next admission clears it. Nothing here takes or tries `align.lock`.
#[must_use]
pub fn running(state: &Path) -> bool {
    OPEN_HERE.load(Ordering::Acquire) == 0
        && read_record(state)
            .and_then(|record| record.repositories)
            .is_some_and(|repositories| repositories.running.is_some())
}

/// How many file leases ([`Storage::File`]) this process holds now: a refresh claim in the
/// record while one is held here is this process's own ([`running`]).
static OPEN_HERE: AtomicUsize = AtomicUsize::new(0);

/// Per-owner observation memo of scheduling facts only. Unchanged metadata avoids
/// reparsing the generic pass record on every existing listener tick.
#[derive(Default)]
pub struct Observer {
    version: Option<Result<[u8; 32], std::io::ErrorKind>>,
    state: RefreshState,
}

/// The shared receipt [`Observer::read`] reads under the account's state folder `state`.
#[must_use]
pub fn record_path(state: &Path) -> PathBuf {
    state.join(FileJournal::DIR).join(super::RECORD)
}

impl Observer {
    pub fn read(
        &mut self,
        sys: &dyn crate::port::System,
        state: &Path,
        now_ms: u64,
    ) -> RefreshObservation {
        let version = sys.file_version(&record_path(state)).map_err(|e| e.kind());
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

/// The observation is sampled before acquiring the lock, or waiting for it, so an explicit
/// request arriving during a wave retains its strictly-later-attempt boundary.
pub struct Access {
    pub observed: RefreshObservation,
    pub pass: Option<Arc<RepositoryPass>>,
}

#[derive(Default)]
pub struct Memory {
    record: Mutex<Option<Record>>,
    held: AtomicBool,
}

impl Memory {
    /// Empty-origin evidence kept by this persistent in-memory account record.
    #[must_use]
    pub fn empty_checkouts(&self) -> EmptyCheckouts {
        self.record
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|record| record.repositories.as_ref())
            .map_or_else(EmptyCheckouts::default, |record| record.empty.clone())
    }

    /// [`running`] for an in-memory record: its refresh claim, read without the lease.
    #[must_use]
    pub fn running(&self) -> bool {
        self.record
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|record| record.repositories.as_ref())
            .is_some_and(|repositories| repositories.running.is_some())
    }
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
    /// This pass's number among those that recorded their own checkout writes, from its first
    /// ([`Self::begin_write`]).
    writes: OnceLock<u64>,
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
        Ok(Self::holding(state, observed, try_lock(state)?))
    }

    /// Access to this account's alignment record/lock for a pass a starter started, waiting
    /// for the lock while another pass holds it ([`wait_lock`], its starter watched by
    /// `watch`): no pass where the starter ended first.
    /// # Errors
    /// The private record directory or lock cannot be admitted, or the wait failed.
    pub fn wait(
        state: &Path,
        now_ms: u64,
        watch: impl FnOnce(Box<dyn FnOnce() + Send>) -> StarterWatch,
    ) -> Result<Access, String> {
        let observed = observe(state, now_ms);
        Ok(Self::holding(state, observed, wait_lock(state, watch)?))
    }

    /// The pass over `state`'s record where `held` is its lock.
    fn holding(state: &Path, observed: RefreshObservation, held: Option<AlignLock>) -> Access {
        let pass = held.map(|held| {
            OPEN_HERE.fetch_add(1, Ordering::AcqRel);
            Arc::new(Self {
                storage: Storage::File {
                    state: state.to_owned(),
                    _held: held,
                },
                record: Mutex::new(read_record(state).unwrap_or_else(empty_record)),
                writes: OnceLock::new(),
            })
        });
        Access { observed, pass }
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
                    writes: OnceLock::new(),
                })
            });
        Access { observed, pass }
    }

    fn persist(&self, record: &mut Record) -> Result<(), String> {
        match &self.storage {
            Storage::File { state, .. } => write_record(&state.join(FileJournal::DIR), record),
            Storage::Memory(memory) => {
                drop(encode_record(record)?);
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
        self.persist(&mut newer)?;
        *record = newer;
        Ok(())
    }

    /// Replace the admitted source's empty-origin evidence under this pass's alignment lease.
    /// Unchanged evidence writes nothing. A fresh remote observation may clear old evidence
    /// before it starts; a failed observation then cannot revive an older empty result.
    pub(crate) fn record_empty(
        &self,
        scope: RefreshScope,
        subjects: Vec<(CheckoutDigest, CheckoutDigest)>,
    ) -> Result<(), String> {
        let empty = if subjects.is_empty() {
            EmptyCheckouts::default()
        } else {
            EmptyCheckouts {
                scope: Some(scope),
                subjects: subjects.into_iter().collect(),
            }
        };
        let mut record = self.record.lock().unwrap_or_else(PoisonError::into_inner);
        let mut state = record.repositories.clone().unwrap_or_default();
        if state.empty == empty {
            return Ok(());
        }
        state.empty = empty;
        let mut candidate = record.clone();
        candidate.repositories = Some(state);
        self.persist(&mut candidate)?;
        *record = candidate;
        Ok(())
    }

    /// Admit scheduling work only after the caller validated its real captured
    /// account/source, `scope`, which is what is admitted: a request that named other inputs
    /// was made before they moved, and what it asked for is a refresh of the inputs there
    /// are. Refused instead, it ended the pass failed and was asked again with the same
    /// stale scope, pass after pass (0.200.0, each time a vault version rewrote the
    /// catalog under a pass). Publishing this claim does not admit a Git effect.
    /// # Errors
    /// Counters exhausted, or the record write failed.
    pub fn admit(
        &self,
        request: RefreshRequest,
        scope: RefreshScope,
        now: u64,
    ) -> Result<Admission, String> {
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
            if record.repositories.as_ref() != Some(&state) {
                let mut candidate = record.clone();
                candidate.repositories = Some(state);
                self.persist(&mut candidate)?;
                *record = candidate;
            }
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
        self.persist(&mut candidate)?;
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
        self.persist(&mut candidate)?;
        *record = candidate;
        Ok(())
    }

    /// Record that one of this pass's own Git effects begins on the checkout `checkout`, which
    /// the input observer reads as `before` now: while it runs, a look that reads the checkout
    /// moved from `before` waits for its end ([`Accounted::Running`]), as long as `holder`, this
    /// pass's process, lives. This pass's first takes the next pass number and lets go an effect
    /// a pass before it left begun: that pass no longer holds the lock, so it ended.
    /// # Errors
    /// The pass numbers are exhausted, or the record cannot be written.
    pub fn begin_write(
        &self,
        holder: Option<Holder>,
        checkout: CheckoutDigest,
        before: CheckoutDigest,
    ) -> Result<OpenWrite, String> {
        let mut record = self.record.lock().unwrap_or_else(PoisonError::into_inner);
        let mut candidate = record.clone();
        let own = &mut candidate
            .repositories
            .get_or_insert_with(RefreshState::default)
            .own;
        let first = self.writes.get().is_none();
        let pass = match self.writes.get() {
            Some(pass) => *pass,
            None => {
                let pass = own
                    .pass
                    .checked_add(1)
                    .ok_or("checkout write pass numbers exhausted")?;
                own.writes.retain(|write| write.after.is_some());
                own.pass = pass;
                own.holder = holder;
                pass
            }
        };
        own.writes.push(CheckoutWrite {
            pass,
            checkout,
            before,
            after: None,
            made: false,
        });
        self.persist(&mut candidate)?;
        *record = candidate;
        if first {
            let _ = self.writes.set(pass);
        }
        Ok(OpenWrite {
            pass,
            checkout,
            before,
        })
    }

    /// End the effect [`Self::begin_write`] began. `after` is the checkout as the observer reads
    /// it once the effect ended, and whether the effect made it or a required dependency of it,
    /// where what moved is the effect's own; `None` where it is not (another made what the effect
    /// would have), or where the effect ended no other way (its thread unwound). A write that
    /// moved nothing is not kept.
    /// # Errors
    /// The record cannot be written.
    pub fn end_write(
        &self,
        open: OpenWrite,
        after: Option<(CheckoutDigest, bool)>,
    ) -> Result<(), String> {
        let mut record = self.record.lock().unwrap_or_else(PoisonError::into_inner);
        let mut candidate = record.clone();
        let own = &mut candidate
            .repositories
            .get_or_insert_with(RefreshState::default)
            .own;
        let Some(at) = own.writes.iter().position(|write| {
            write.after.is_none()
                && write.pass == open.pass
                && write.checkout == open.checkout
                && write.before == open.before
        }) else {
            return Ok(());
        };
        own.writes.remove(at);
        if let Some((after, made)) = after.filter(|(after, _)| *after != open.before) {
            let continues = own
                .writes
                .iter()
                .rev()
                .find(|write| write.checkout == open.checkout && write.after.is_some())
                .is_none_or(|last| last.after == Some(open.before));
            if !continues {
                own.writes
                    .retain(|write| write.checkout != open.checkout || write.after.is_none());
            }
            own.writes.push(CheckoutWrite {
                pass: open.pass,
                checkout: open.checkout,
                before: open.before,
                after: Some(after),
                made,
            });
            let kept = |writes: &[CheckoutWrite]| {
                writes
                    .iter()
                    .filter(|write| write.checkout == open.checkout)
                    .count()
            };
            while kept(&own.writes) > WRITES_KEPT {
                let Some(oldest) = own
                    .writes
                    .iter()
                    .position(|write| write.checkout == open.checkout && write.after.is_some())
                else {
                    break;
                };
                own.writes.remove(oldest);
            }
            while own.writes.len() > ALL_WRITES_KEPT {
                let Some(oldest) = own.writes.iter().position(|write| write.after.is_some()) else {
                    break;
                };
                own.writes.remove(oldest);
            }
        }
        self.persist(&mut candidate)?;
        *record = candidate;
        Ok(())
    }
}

impl Drop for RepositoryPass {
    fn drop(&mut self) {
        match &self.storage {
            Storage::Memory(memory) => memory.held.store(false, Ordering::Release),
            Storage::File { .. } => {
                OPEN_HERE.fetch_sub(1, Ordering::AcqRel);
            }
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

#[cfg(test)]
#[path = "repository_refresh_tests.rs"]
mod tests;
