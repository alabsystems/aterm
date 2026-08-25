// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-process host for the opt-in operator observer (`$ATERM_OPERATOR=1`).
//!
//! The observer is deliberately narrower than the operator policy/runtime: it owns
//! no control token and has no input-writing capability. It snapshots the local
//! [`SessionStore`](crate::session_store::SessionStore), waits on the existing
//! in-process subscriber registry between bounded roster reconciliations, and emits
//! deterministic attention candidates through [`EventSink`]. The portable operator
//! crate supplies the durable sink; keeping that dependency behind one trait prevents
//! GUI session/terminal types from leaking into the durable state machine.
//!
//! Nothing in this module runs on the winit thread. Terminal reads use `try_lock`,
//! candidate delivery happens only after every GUI lock has been dropped, and
//! shutdown is an explicit bounded join from `main_entry` after the event loop exits.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TrySendError;
use std::sync::{Arc, Condvar, Mutex, OnceLock, TryLockError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_agent::operator::{
    AckOutcome, AttentionCondition, Claim, ClaimToken, DurableQueue, EnqueueOutcome,
    EventGeneration, EventId, EventSnapshot, FaultLatchOutcome, FinalActionPermit,
    FleetFaultReason, FleetGateStatus, NewEvent, OperatorError, QueueConfig, Resolution,
};
use sha2::{Digest as _, Sha256};

use crate::session_store::{SessionHandle, SessionState, Store};
use crate::subscribe::{SubscriberSet, Subscribers, Subscription};

/// Maximum delay before a newly-created/removed session is reconciled even when no
/// watched terminal produces output. Output wakes usually make the loop run sooner.
const RECONCILE_INTERVAL: Duration = Duration::from_millis(250);
/// A changed surface must remain stable for this long before a prompt-shaped screen
/// becomes a busy-to-attention transition candidate.
const ATTENTION_SETTLE: Duration = Duration::from_millis(750);
/// Evidence is deliberately a bounded tail: agent prompts live at the bottom and an
/// untrusted terminal must not make one queued event retain an arbitrary screen.
const EVIDENCE_ROWS: usize = 24;
const EVIDENCE_BYTES: usize = 16 * 1024;
/// A failed downstream sink cannot turn the resident observer into an unbounded queue.
const PENDING_CAPACITY: usize = 256;
const FLUSH_BUDGET: usize = 16;
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const ESCALATION_NOTICE_HISTORY: usize = 512;
/// The normal, unnamed aterm profile has one durable operator namespace per
/// user state root. `ATERM_STATE_HOME` already provides install/deployment
/// isolation; an explicitly named profile splits that namespace further.
const DEFAULT_OPERATOR_PROFILE: &str = "default";
const OPERATOR_PROFILE_ENV: &str = "ATERM_OPERATOR_PROFILE";
/// The opt-in switch. The embedded operator is DEFAULT-OFF.
///
/// Default-on would have bought a user exactly nothing: the managed SID
/// allowlist is empty on a new profile, so an operator nobody has opted into
/// observes no session, hashes no screen and takes no terminal lock — while
/// still costing every aterm process a resident thread, a durable WAL under the
/// user's state root, and a place in the self-update path. The RFC approves an
/// EXPERIMENTAL implementation whose §9 acceptance is not discharged
/// (`docs/OPERATOR-EMBEDDED.md`, "Shape and status"). Zero benefit against that
/// risk is not a defensible default, so enabling it is one deliberate act:
/// `ATERM_OPERATOR=1`, then `aterm fleet manage <sid>`.
const OPERATOR_OPT_IN_ENV: &str = "ATERM_OPERATOR";

/// The kill switch, retained and authoritative: it wins over the opt-in, so a
/// profile or launcher that exports `ATERM_OPERATOR=1` can still be overridden
/// per-process without editing it. Spelled exactly like its shipped sibling
/// `$ATERM_NO_CONTROL_SOCK`.
const OPERATOR_KILL_ENV: &str = "ATERM_NO_OPERATOR";

/// Fixed retry cadence for leadership standby: a sibling aterm can hold the
/// kernel lock indefinitely and this process must be ready to take over.
const LEADERSHIP_RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// First backoff after a NON-contention durable-state open failure.
const OPEN_RETRY_BASE: Duration = Duration::from_secs(1);
/// Ceiling for that backoff.
const OPEN_RETRY_CAP: Duration = Duration::from_secs(30);
/// Consecutive non-contention open failures after which this process stops
/// trying and the embedded operator is simply off. Cheaper than an unbounded
/// retry on a filesystem that will never satisfy it, and strictly safer than a
/// fleet fault: dormancy revokes nothing, blocks no update, and needs no human
/// clear protocol to leave — the next launch tries again from zero.
const OPEN_FAILURE_BUDGET: u32 = 3;

/// Stable generation of the exact surface/lifecycle observation behind a candidate.
/// `alternate_screen` is part of the key because the two grids have independent
/// `content_seq` counters. `lifecycle_epoch` distinguishes an exit/removal from the
/// final screen generation it follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Generation {
    pub(crate) lifecycle_epoch: u64,
    pub(crate) alternate_screen: bool,
    pub(crate) content_seq: u64,
    pub(crate) fingerprint: [u8; 32],
}

impl Generation {
    fn initial() -> Self {
        Self {
            lifecycle_epoch: 0,
            alternate_screen: false,
            content_seq: 0,
            fingerprint: [0; 32],
        }
    }

    fn next_lifecycle(self) -> Self {
        Self {
            lifecycle_epoch: self.lifecycle_epoch.saturating_add(1),
            ..self
        }
    }
}

impl From<Generation> for EventGeneration {
    fn from(generation: Generation) -> Self {
        Self::new(
            generation.lifecycle_epoch,
            generation.alternate_screen,
            generation.content_seq,
            generation.fingerprint,
        )
    }
}

/// Deterministic transition classes produced by this host. They are candidates, not
/// decisions: in particular an approval-looking screen never authorizes a keypress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CandidateKind {
    /// Fresh full-snapshot baseline after this process obtains durable
    /// leadership. It closes the handoff race without claiming that a quiet
    /// prompt proves a completed turn.
    LeadershipBaseline,
    SessionMissing,
    SessionExited,
    ApprovalPrompt,
    BusyBecameAttention,
}

impl CandidateKind {
    fn priority(self) -> u8 {
        match self {
            Self::LeadershipBaseline => 0,
            Self::BusyBecameAttention => 1,
            Self::ApprovalPrompt => 2,
            Self::SessionExited => 3,
            Self::SessionMissing => 4,
        }
    }
}

/// GUI-neutral payload handed to the portable durable queue adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) sid: String,
    pub(crate) local_id: u64,
    pub(crate) generation: Generation,
    pub(crate) kind: CandidateKind,
    pub(crate) screen_tail: String,
}

pub(crate) enum ObservationAccess {
    /// Test/embedding sinks which deliberately receive every local session.
    All,
    /// Production leadership plus the exact durable allowlist (possibly empty).
    Managed(HashSet<String>),
    /// Another live process owns this profile's kernel lock.
    Standby,
    /// Durable state could not be read safely; observation stays disabled.
    Failed(String),
    /// The durable state never opened, repeatedly. The embedded operator is off
    /// for the life of this process: the observer thread ends here rather than
    /// retrying a filesystem that has already answered.
    Disabled(String),
}

/// The only assumption this host makes about the portable operator API.
///
/// The durable adapter should map [`Candidate`] to its public event DTO and perform a
/// bounded enqueue. A durable sink may synchronously fsync its WAL here because this
/// is the dedicated observer thread, never the UI thread; it must not run a model or
/// wait for an actuator. `Err` retains the candidate for bounded retry.
pub(crate) trait EventSink: Send + Sync + 'static {
    fn enqueue(&self, candidate: Candidate) -> Result<(), String>;

    /// Admit one complete observation transaction. Production returns an RAII
    /// activity token which stays live from roster admission through terminal
    /// reads, classification, local coalescing, fault publication, and durable
    /// flush. The default keeps lightweight test/embedding sinks unchanged.
    fn begin_observation_cycle(&self) -> Result<Option<Box<dyn ObservationCycleGuard>>, String> {
        Ok(None)
    }

    /// Return the durable roster this observer is allowed to inspect. `All` is
    /// reserved for test/embedding sinks that intentionally observe the whole
    /// local store. Production returns `Managed`, including an empty set, so
    /// default-on with an empty allowlist takes no terminal locks and computes no
    /// screen hashes; standby/failure grants no observation access.
    fn observation_access(&self) -> ObservationAccess {
        ObservationAccess::All
    }

    /// Give a durable sink a cheap opportunity to acquire leadership after a
    /// seamless-handoff predecessor exits. The default sink has no maintenance.
    fn maintenance(&self) {}

    /// Latch a process-visible fleet fault when a bounded observer queue cannot
    /// retain another non-coalescable event. The default test/embedding sink has
    /// no resident status surface.
    fn fault(&self, _reason: FleetFaultReason) {}

    /// Last-resort independent marker publication for a worker which may be
    /// wedged holding its normal durable-state mutex. Production overrides this;
    /// lightweight embedding sinks have no durable namespace to mark.
    fn fault_marker_without_live(&self, _reason: FleetFaultReason) -> Result<(), String> {
        Ok(())
    }

    #[cfg(test)]
    fn pending_capacity_for_test(&self) -> usize {
        PENDING_CAPACITY
    }

    /// Deterministic race seam after classification and before local publication.
    #[cfg(test)]
    fn after_classify_for_test(&self, _candidate_count: usize) {}

    /// Deterministic shutdown seam after one bounded flush batch.
    #[cfg(test)]
    fn after_flush_batch_for_test(&self, _pending: usize) {}
}

pub(crate) trait ObservationCycleGuard: Send {}

struct QueueSlot {
    queue: Option<DurableQueue>,
    last_error: Option<String>,
    /// When the next open attempt is admitted. `None` with a `last_error` means
    /// this process will not try again — see `dormant`.
    retry_after: Option<Instant>,
    /// Consecutive open failures that were NOT leadership contention.
    open_failures: u32,
    /// The durable state failed to open `OPEN_FAILURE_BUDGET` times in a row.
    /// The embedded operator is then OFF for the life of this process: no
    /// observer thread, no authority, no further filesystem attempts — the same
    /// place `$ATERM_NO_OPERATOR` lands, reached automatically.
    dormant: bool,
}

impl QueueSlot {
    /// Record one failed open and decide when (or whether) to try again.
    ///
    /// Leadership contention is not a failure of the state itself: a sibling
    /// aterm may hold the kernel lock for hours, and standby must keep retrying
    /// at a fixed interval forever. Every other open failure — permission,
    /// ENOSPC, an unresolvable state root, a filesystem that cannot represent a
    /// private directory (SMB/NFS, exFAT, a bind mount), an unrepairable WAL —
    /// backs off and, after a small budget, disables the operator cleanly.
    ///
    /// It deliberately does NOT latch a fleet fault. Durable state that never
    /// opened has nothing in doubt: no claim was minted, no action is in flight,
    /// no record is torn. Faulting on it would spend a process-wide authority
    /// revocation on an ordinary `chmod` — and, before this fix, that latch also
    /// vetoed every self-update seam for the life of the process.
    fn record_open_failure(&mut self, message: String, contended: bool, now: Instant) {
        self.last_error = Some(message);
        if contended {
            self.retry_after = Some(now + LEADERSHIP_RETRY_INTERVAL);
            return;
        }
        self.open_failures = self.open_failures.saturating_add(1);
        if self.open_failures >= OPEN_FAILURE_BUDGET {
            self.dormant = true;
            self.retry_after = None;
            return;
        }
        let backoff = OPEN_RETRY_BASE
            .saturating_mul(1 << (self.open_failures - 1))
            .min(OPEN_RETRY_CAP);
        self.retry_after = Some(now + backoff);
    }

    /// The message every caller sees once the operator has gone dormant. It
    /// names the subsystem, so nothing that fails afterwards is anonymous.
    fn disabled_message(&self) -> String {
        let detail = self
            .last_error
            .as_deref()
            .unwrap_or("durable state is unavailable");
        format!("embedded operator disabled for this process: {detail}")
    }
}

/// Process-local serialization point for every transition that can revoke the
/// resident operator's authority to start or finish PTY egress.
///
/// Durable fleet faults live in [`DurableQueue`] as well; this small host gate
/// closes the final in-process race with unmanagement and normal shutdown while
/// the control listener is still reachable.
#[derive(Default)]
struct HostActuationGate {
    fleet_fault: Option<FleetFaultReason>,
    shutting_down: bool,
    update_quiesce: Option<u64>,
    next_update_quiesce: u64,
    active_actions: usize,
    active_observer_mutations: usize,
}

/// Counts one proposal transaction from its first durable validation through
/// its durable result/in-doubt record. Update quiesce refuses while any such
/// transaction exists, closing the post-egress/pre-result replacement race.
pub(crate) struct ActionActivity {
    control: ControlHandle,
}

impl Drop for ActionActivity {
    fn drop(&mut self) {
        let mut gate = self
            .control
            .shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        gate.active_actions = gate.active_actions.saturating_sub(1);
        drop(gate);
        self.control.notify();
    }
}

struct ObserverMutationActivity {
    control: ControlHandle,
}

impl ObservationCycleGuard for ObserverMutationActivity {}

impl Drop for ObserverMutationActivity {
    fn drop(&mut self) {
        let mut gate = self
            .control
            .shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        gate.active_observer_mutations = gate.active_observer_mutations.saturating_sub(1);
        drop(gate);
        self.control.notify();
    }
}

/// Reversible authority fence held while a process replacement reaches its
/// final admission/exec seam. Dropping it after any rejected/failed update
/// restores normal service; a successful replacement never returns to drop it.
pub(crate) struct UpdateQuiesce {
    control: ControlHandle,
    id: u64,
}

impl UpdateQuiesce {
    /// Serialize the actual Commit/exec/spawn syscall with late fleet faults,
    /// shutdown, and every operator mutation. The closure must be the final
    /// replacement operation; success is expected not to return.
    ///
    /// Serializing with a late fault is not the same as being vetoed by one:
    /// the latch either completes before this permit (durably, under this gate)
    /// or waits behind the replacement. Only shutdown and a stale token refuse.
    pub(crate) fn with_commit_permit<T>(&self, commit: impl FnOnce() -> T) -> Result<T, String> {
        let gate = match self.control.shared.fleet_fault.try_lock() {
            Ok(gate) => gate,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err("operator authority transition is busy".to_string());
            }
        };
        if gate.shutting_down {
            return Err("operator host is shutting down".to_string());
        }
        if gate.update_quiesce != Some(self.id) {
            return Err("operator update quiesce is stale".to_string());
        }
        if let Some(reason) = gate.fleet_fault {
            // A latched fault is STRICTLY MORE fenced than this quiesce: a
            // faulted operator can no longer claim, mutate the queue, or
            // actuate, and its durable marker is written under this very gate
            // before the latch becomes visible, so the successor process reopens
            // fail-closed with the fault intact. Refusing replacement here does
            // not protect anything — it converts a subsystem fault into a
            // permanent veto over EVERY self-update seam (cold exec, Windows
            // spawn-and-exit, seamless Commit) for the life of the process, with
            // no message naming the operator as the cause. Replacement proceeds;
            // the fault outlives it durably.
            aterm_log::warn!(
                "replacing this process while the operator fleet is faulted ({}); \
                 the successor reopens fail-closed with the durable fault intact",
                reason.as_str()
            );
        }
        let result = commit();
        drop(gate);
        Ok(result)
    }
}

impl Drop for UpdateQuiesce {
    fn drop(&mut self) {
        let mut gate = self
            .control
            .shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if gate.update_quiesce == Some(self.id) {
            gate.update_quiesce = None;
        }
        drop(gate);
        self.control.notify();
    }
}

struct OperatorShared {
    fleet_id: String,
    /// Production resolves this lazily from `fleet_id`. Tests may pin a private
    /// directory so leadership/takeover can be exercised without mutating process
    /// environment or the user's real state root.
    state_directory: Option<PathBuf>,
    slot: Mutex<QueueSlot>,
    /// Once queue leadership has opened, retain a lock-independent clone for
    /// timeout/panic marker publication. `OnceLock::get` never waits behind the
    /// queue's live-state mutex or the lazy-open slot.
    marker_queue: OnceLock<DurableQueue>,
    /// Per-process manage occurrence, paired with the durable run epoch and the
    /// observer's local lifecycle counter in every EventGeneration.
    manage_occurrences: Mutex<HashMap<String, u64>>,
    notify_tx: std::sync::mpsc::SyncSender<crate::notify::NotifyMsg>,
    /// OS delivery is a separate bounded worker shared with terminal OSC
    /// notifications. Preserve one coalesced operator alert when that queue is
    /// temporarily full and retry it on the 250 ms maintenance wake rather than
    /// silently dropping the only human escalation.
    pending_notice: Mutex<Option<crate::notify::NotifyMsg>>,
    /// A bounded queue overflow means the observer can no longer promise
    /// lossless delivery. Keep that fact loud and sticky until process restart;
    /// it is distinct from transient leadership standby.
    fleet_fault: Mutex<HostActuationGate>,
    wake_serial: Mutex<u64>,
    changed: Condvar,
    claim_waiter_active: AtomicBool,
    /// OS notifications are not durable protocol state, but two in-process paths
    /// can discover the same cap-converted escalation (maintenance and `next`).
    /// Suppress duplicate presentation within this process while retaining the
    /// durable event itself as the source of truth across restart.
    surfaced_escalations: Mutex<SurfacedEscalations>,
}

#[derive(Default)]
struct SurfacedEscalations {
    keys: HashSet<(u64, u32, bool)>,
    order: VecDeque<(u64, u32, bool)>,
}

impl SurfacedEscalations {
    fn insert(&mut self, key: (u64, u32, bool)) -> bool {
        if self.keys.contains(&key) {
            return false;
        }
        if self.order.len() >= ESCALATION_NOTICE_HISTORY
            && let Some(oldest) = self.order.pop_front()
        {
            self.keys.remove(&oldest);
        }
        self.order.push_back(key);
        self.keys.insert(key)
    }
}

struct ClaimWaiterGuard<'a>(&'a AtomicBool);

impl Drop for ClaimWaiterGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Read/claim side of the embedded operator. It carries no raw terminal writer;
/// the guarded actuator in `control` remains the only place that can turn a
/// validated proposal into input.
#[derive(Clone)]
pub(crate) struct ControlHandle {
    shared: Arc<OperatorShared>,
}

impl ControlHandle {
    pub(crate) fn new(
        fleet_id: String,
        notify_tx: std::sync::mpsc::SyncSender<crate::notify::NotifyMsg>,
    ) -> Self {
        Self::new_with_directory(fleet_id, None, notify_tx)
    }

    fn new_with_directory(
        fleet_id: String,
        state_directory: Option<PathBuf>,
        notify_tx: std::sync::mpsc::SyncSender<crate::notify::NotifyMsg>,
    ) -> Self {
        Self {
            shared: Arc::new(OperatorShared {
                fleet_id,
                state_directory,
                slot: Mutex::new(QueueSlot {
                    queue: None,
                    last_error: None,
                    retry_after: Some(Instant::now()),
                    open_failures: 0,
                    dormant: false,
                }),
                marker_queue: OnceLock::new(),
                manage_occurrences: Mutex::new(HashMap::new()),
                notify_tx,
                pending_notice: Mutex::new(None),
                fleet_fault: Mutex::new(HostActuationGate::default()),
                wake_serial: Mutex::new(0),
                changed: Condvar::new(),
                claim_waiter_active: AtomicBool::new(false),
                surfaced_escalations: Mutex::new(SurfacedEscalations::default()),
            }),
        }
    }

    /// Open lazily and retry lock contention. During seamless handoff the incoming
    /// process starts while the outgoing process still owns the OS lock; retaining a
    /// standby handle here lets the successor activate without a second daemon or a
    /// process restart once that lock is released.
    pub(crate) fn queue(&self) -> Result<DurableQueue, String> {
        let mut slot = self
            .shared
            .slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(queue) = &slot.queue {
            return Ok(queue.clone());
        }
        if slot.dormant {
            return Err(slot.disabled_message());
        }
        let now = Instant::now();
        if let Some(message) = &slot.last_error {
            match slot.retry_after {
                Some(retry_after) if now >= retry_after => {}
                Some(_) | None => return Err(message.clone()),
            }
        }
        let directory = if let Some(directory) = &self.shared.state_directory {
            directory.clone()
        } else {
            match aterm_agent::operator::fleet_state_dir(&self.shared.fleet_id) {
                Ok(directory) => directory,
                Err(error) => {
                    // The state root could not even be resolved or created. That
                    // is an environment verdict about a directory, not a
                    // durability verdict about operator state: back off, and go
                    // dormant rather than faulted if it keeps failing.
                    let message = error.to_string();
                    slot.record_open_failure(message.clone(), false, Instant::now());
                    let dormant = slot.dormant;
                    drop(slot);
                    if dormant {
                        Self::warn_dormant(&message);
                    }
                    return Err(message);
                }
            }
        };
        match DurableQueue::open_next_epoch(directory, QueueConfig::default()) {
            Ok((queue, report)) => {
                if report.repaired_partial_final_frame {
                    aterm_log::warn!(
                        "operator {} repaired an incomplete final WAL frame",
                        self.shared.fleet_id
                    );
                }
                slot.last_error = None;
                slot.retry_after = None;
                slot.open_failures = 0;
                let _ = self.shared.marker_queue.set(queue.clone());
                slot.queue = Some(queue.clone());
                drop(slot);
                self.notify();
                Ok(queue)
            }
            Err(error) => {
                let message = error.to_string();
                let contended = matches!(error, OperatorError::LockContended(_));
                slot.record_open_failure(message.clone(), contended, now);
                let dormant = slot.dormant;
                drop(slot);
                if dormant {
                    Self::warn_dormant(&message);
                }
                Err(message)
            }
        }
    }

    /// Announce the one transition into dormancy. Said once, in the user's
    /// language of consequence: the terminal is fine, this subsystem is not.
    fn warn_dormant(message: &str) {
        aterm_log::warn!(
            "embedded operator disabled for this process after {OPEN_FAILURE_BUDGET} failed \
             attempts to open its durable state ({message}). The terminal, its sessions and \
             self-update are unaffected; a later launch retries from zero."
        );
    }

    pub(crate) fn notify(&self) {
        let mut serial = self
            .shared
            .wake_serial
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *serial = serial.wrapping_add(1);
        self.shared.changed.notify_all();
    }

    fn fleet_fault(&self) -> Option<FleetFaultReason> {
        self.shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fleet_fault
    }

    fn update_quiesced(&self) -> bool {
        self.shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .update_quiesce
            .is_some()
    }

    /// Refuse any operation that could create a new claim, management baseline,
    /// or actuator intent after the observer has lost its delivery guarantee.
    /// Resolution/cleanup commands deliberately do not call this gate.
    pub(crate) fn ensure_accepting_new_work(&self) -> Result<(), String> {
        let gate = self
            .shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if gate.shutting_down {
            return Err("operator host is shutting down".to_string());
        }
        if gate.update_quiesce.is_some() {
            return Err("operator is quiesced for process replacement".to_string());
        }
        match gate.fleet_fault.as_ref() {
            Some(reason) => Err(format!("operator fleet faulted: {}", reason.as_str())),
            None => Ok(()),
        }
    }

    /// Fence normal shutdown against the still-live control listener.
    ///
    /// The flag is process-local because shutdown cannot be resumed after this
    /// process exits. Taking the same mutex as final egress guarantees that, on
    /// return, no new guarded paste/Enter can start and any egress that won the
    /// race has already completed its one bounded nonblocking syscall.
    pub(crate) fn begin_shutdown(&self) {
        let mut gate = self
            .shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        gate.shutting_down = true;
        drop(gate);
        self.notify();
    }

    /// Enter a reversible process-replacement fence without waiting behind an
    /// in-flight operator mutation. The caller rejects/retries the update on
    /// `Busy`; once returned, all subsequently-starting observation, queue
    /// mutation, proposal, and PTY egress paths refuse until the token drops.
    pub(crate) fn try_begin_update_quiesce(&self) -> Result<UpdateQuiesce, String> {
        let mut gate = match self.shared.fleet_fault.try_lock() {
            Ok(gate) => gate,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err("operator mutation is busy; retry update".to_string());
            }
        };
        if gate.shutting_down {
            return Err("operator host is shutting down".to_string());
        }
        if gate.update_quiesce.is_some() {
            return Err("operator update quiesce is already active".to_string());
        }
        if gate.active_actions != 0 || gate.active_observer_mutations != 0 {
            return Err("operator transaction is in flight; retry update".to_string());
        }
        let id = gate
            .next_update_quiesce
            .checked_add(1)
            .ok_or_else(|| "operator update quiesce identity exhausted".to_string())?;
        gate.next_update_quiesce = id;
        gate.update_quiesce = Some(id);
        drop(gate);
        self.notify();
        Ok(UpdateQuiesce {
            control: self.clone(),
            id,
        })
    }

    /// Register one complete proposal transaction against update replacement.
    pub(crate) fn begin_action_activity(&self) -> Result<ActionActivity, String> {
        let mut gate = self.try_actuation_guard()?;
        gate.active_actions = gate
            .active_actions
            .checked_add(1)
            .ok_or_else(|| "operator action activity count exhausted".to_string())?;
        drop(gate);
        Ok(ActionActivity {
            control: self.clone(),
        })
    }

    /// Admit a complete observer transaction. Ordinary shutdown deliberately
    /// remains admissible: after the actuator/control fence closes, the worker
    /// owes one final snapshot and a complete drain. Update replacement and an
    /// already-durable fleet fault still refuse the transaction.
    fn begin_observer_mutation(&self) -> Result<ObserverMutationActivity, String> {
        let mut gate = self
            .shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if gate.update_quiesce.is_some() {
            return Err("operator is quiesced for process replacement".to_string());
        }
        if let Some(reason) = gate.fleet_fault.as_ref() {
            return Err(format!("operator fleet faulted: {}", reason.as_str()));
        }
        gate.active_observer_mutations = gate
            .active_observer_mutations
            .checked_add(1)
            .ok_or_else(|| "operator observer activity count exhausted".to_string())?;
        drop(gate);
        Ok(ObserverMutationActivity {
            control: self.clone(),
        })
    }

    /// Hold the host authority gate across one bounded final PTY mutation.
    ///
    /// `latch_fleet_fault`, manage, and unmanage all serialize on `fleet_fault`.
    /// Under that same guard this method rechecks the durable allowlist and the
    /// exact token/hash ActionInFlight record immediately before invoking the
    /// caller's nonblocking terminal+sink critical section. Whichever operation
    /// wins the gate linearizes first: an invalidation that wins makes this return
    /// without calling `actuate`; an actuation that wins completes its bounded
    /// syscall before the invalidation becomes visible.
    pub(crate) fn with_actuation_permit<T>(
        &self,
        queue: &DurableQueue,
        event_id: EventId,
        token: &ClaimToken,
        sid: &str,
        action_hash: &str,
        actuate: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let gate = self.try_actuation_guard()?;
        match queue
            .try_validate_action_permit(event_id, token, sid, "turn", action_hash)
            .map_err(|error| error.to_string())?
        {
            FinalActionPermit::Granted => {}
            FinalActionPermit::Busy => {
                return Err("operator durable authority transition is busy".to_string());
            }
            FinalActionPermit::Revoked => {
                return Err("operator durable action authority was revoked".to_string());
            }
        }
        let result = actuate();
        drop(gate);
        Ok(result)
    }

    /// Hold the fault latch stable across one queue mutation. A concurrent
    /// observer fault either wins before this guard (the mutation refuses) or
    /// waits until it completes; it can never interleave between the check and a
    /// newly-minted claim/manage record.
    fn accepting_guard(&self) -> Result<std::sync::MutexGuard<'_, HostActuationGate>, String> {
        let guard = self
            .shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.shutting_down {
            return Err("operator host is shutting down".to_string());
        }
        if guard.update_quiesce.is_some() {
            return Err("operator is quiesced for process replacement".to_string());
        }
        if let Some(reason) = guard.fleet_fault.as_ref() {
            return Err(format!("operator fleet faulted: {}", reason.as_str()));
        }
        Ok(guard)
    }

    /// Serialize cleanup/control mutations with process replacement while still
    /// permitting them during an ordinary fleet fault or normal shutdown.
    fn mutation_guard(&self) -> Result<std::sync::MutexGuard<'_, HostActuationGate>, String> {
        let guard = self
            .shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.update_quiesce.is_some() {
            return Err("operator is quiesced for process replacement".to_string());
        }
        Ok(guard)
    }

    /// Nonparking final-egress twin of [`Self::accepting_guard`]. A concurrent
    /// durable fault append may include an arbitrarily slow filesystem sync;
    /// actuator calls must return a zero-byte refusal instead of waiting behind
    /// that sync and violating their foreground bound.
    fn try_actuation_guard(&self) -> Result<std::sync::MutexGuard<'_, HostActuationGate>, String> {
        let guard = match self.shared.fleet_fault.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err("operator authority transition is busy".to_string());
            }
        };
        if guard.shutting_down {
            return Err("operator host is shutting down".to_string());
        }
        if guard.update_quiesce.is_some() {
            return Err("operator is quiesced for process replacement".to_string());
        }
        if let Some(reason) = guard.fleet_fault.as_ref() {
            return Err(format!("operator fleet faulted: {}", reason.as_str()));
        }
        Ok(guard)
    }

    /// Record the first loss-of-delivery guarantee and surface it once. Later
    /// retries preserve the original diagnostic and cannot notification-spam.
    fn latch_fleet_fault(&self, reason: FleetFaultReason) {
        let mut fault = self
            .shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if fault.fleet_fault.is_some() {
            return;
        }
        // Serialize the durable latch with the host's final-write permit. The
        // portable queue writes its independent marker before the WAL record;
        // if either operation fails, this process remains faulted and the queue
        // handle is itself poisoned by the core.
        let effective_reason = match self.queue() {
            Ok(queue) => match queue.latch_fault(reason) {
                Ok(
                    FaultLatchOutcome::Latched(fault) | FaultLatchOutcome::AlreadyLatched(fault),
                ) => fault.reason,
                Err(error) => {
                    aterm_log::warn!(
                        "operator durable fleet-fault latch failed ({}): {error}",
                        reason.as_str()
                    );
                    reason
                }
            },
            Err(error) => {
                aterm_log::warn!(
                    "operator durable fleet-fault latch unavailable ({}): {error}",
                    reason.as_str()
                );
                reason
            }
        };
        fault.fleet_fault = Some(effective_reason);
        drop(fault);
        self.surface_notice(
            u64::MAX,
            "The aterm operator stopped accepting events and actions after a fleet fault.",
        );
        self.notify();
    }

    #[cfg(test)]
    pub(crate) fn inject_fleet_fault_for_test(&self, reason: FleetFaultReason) {
        self.shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fleet_fault = Some(reason);
    }

    fn surface_notice(&self, local_id: u64, body: &str) {
        let message = crate::notify::NotifyMsg {
            session: local_id,
            title: Some("aterm operator".to_string()),
            body: body.to_string(),
        };
        match self.shared.notify_tx.try_send(message) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(message)) => {
                let mut pending = self
                    .shared
                    .pending_notice
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if pending.is_none() {
                    *pending = Some(message);
                }
            }
        }
    }

    fn flush_pending_notice(&self) {
        let message = self
            .shared
            .pending_notice
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(message) = message else {
            return;
        };
        if let Err(TrySendError::Full(message)) = self.shared.notify_tx.try_send(message) {
            let mut pending = self
                .shared
                .pending_notice
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if pending.is_none() {
                *pending = Some(message);
            }
        }
    }

    fn event_generation(
        &self,
        queue: &DurableQueue,
        sid: &str,
        generation: EventGeneration,
    ) -> Result<EventGeneration, String> {
        let occurrence = *self
            .shared
            .manage_occurrences
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(sid)
            .unwrap_or(&0);
        let epoch = queue.durable_epoch().map_err(|error| error.to_string())?;
        compose_generation(generation, epoch, occurrence)
    }

    fn surface_attention(
        &self,
        local_id: u64,
        condition: AttentionCondition,
        outcome: EnqueueOutcome,
    ) {
        let newly_attention_worthy = matches!(
            outcome,
            EnqueueOutcome::Enqueued(_)
                | EnqueueOutcome::Coalesced {
                    strengthened: true,
                    ..
                }
        );
        if !newly_attention_worthy {
            return;
        }
        let body = match condition {
            AttentionCondition::ApprovalRequired => {
                "A managed session is waiting for human approval."
            }
            AttentionCondition::SessionExited => "A managed session exited.",
            AttentionCondition::Escalation => "A managed session requires human attention.",
            AttentionCondition::Changed
            | AttentionCondition::Ready
            | AttentionCondition::SuspectedStuck => return,
        };
        self.surface_notice(local_id, body);
    }

    /// Block without polling the control socket. A one-second ceiling on each
    /// condition wait lets expired claims be reclaimed even when no new screen
    /// transition signals the queue.
    pub(crate) fn wait_claim(
        &self,
        store: &Store,
        timeout: Duration,
    ) -> Result<Option<Claim>, String> {
        if self
            .shared
            .claim_waiter_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("another operator next call is already waiting".to_string());
        }
        let _waiter = ClaimWaiterGuard(&self.shared.claim_waiter_active);
        // This call occupies one ordinary control-plane worker. Keep the only
        // permitted waiter bounded so observation never starves the remaining
        // control verbs even if an Owner client supplies an extreme duration.
        let timeout = timeout.min(Duration::from_secs(30));
        let deadline = Instant::now() + timeout;
        loop {
            let fault_guard = self.accepting_guard()?;
            let claim = self.queue()?.claim().map_err(|error| error.to_string())?;
            drop(fault_guard);
            if let Some(claim) = claim {
                if claim.event.escalated || claim.event.condition == AttentionCondition::Escalation
                {
                    self.surface_escalation_once(store, &claim.event);
                }
                return Ok(Some(claim));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let wait_for = deadline
                .saturating_duration_since(now)
                .min(Duration::from_secs(1));
            let serial = self
                .shared
                .wake_serial
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = self
                .shared
                .changed
                .wait_timeout(serial, wait_for)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn surface_escalation_once(&self, store: &Store, event: &EventSnapshot) {
        let terminal_in_doubt = matches!(
            &event.status,
            aterm_agent::operator::EventStatus::InDoubt { .. }
        );
        let mut surfaced = self
            .shared
            .surfaced_escalations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !surfaced.insert((event.id.get(), event.redelivery_count, terminal_in_doubt)) {
            return;
        }
        drop(surfaced);
        let local_id = store
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .by_sid(&aterm_session::SessionId::new(&event.sid))
            .map_or(u64::MAX, |session| session.local_id);
        self.surface_notice(
            local_id,
            "A managed session requires human attention after an operator claim expired.",
        );
    }

    /// Owner-only text command adapter used by the control socket. Keeping queue
    /// mutation here gives both the CLI and the guarded actuator one API surface.
    pub(crate) fn command(&self, store: &Store, rest: &str) -> String {
        match self.try_command(store, rest) {
            Ok(reply) => reply,
            Err(error) => format!("ERR {error}\n"),
        }
    }

    fn try_command(&self, store: &Store, rest: &str) -> Result<String, String> {
        let mut words = rest.split_whitespace();
        let Some(command) = words.next() else {
            return Err(operator_usage().to_string());
        };
        match command {
            "status" if words.next().is_none() => self.command_status(),
            "inspect" => {
                let event_id = parse_event_id(words.next(), "usage: inspect <event>")?;
                if words.next().is_some() {
                    return Err("usage: inspect <event>".to_string());
                }
                self.command_inspect(event_id)
            }
            "manage" => {
                let sid = one_argument(&mut words, "manage <sid>")?;
                self.command_manage(store, sid)
            }
            "unmanage" => {
                let sid = one_argument(&mut words, "unmanage <sid>")?;
                // Cleanup remains allowed while faulted, but it shares the final
                // actuation gate so it cannot revoke authority between the
                // actuator's exact state check and bounded PTY write.
                let _actuation_gate = self.mutation_guard()?;
                let _management = self
                    .shared
                    .manage_occurrences
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let changed = self
                    .queue()?
                    .unmanage_sid(sid)
                    .map_err(|error| error.to_string())?;
                self.notify();
                Ok(format!("OK managed=false changed={}\n", u8::from(changed)))
            }
            "next" => {
                let timeout = parse_next_timeout(words)?;
                match self.wait_claim(store, timeout)? {
                    Some(claim) => Ok(format_claim(self, store, &claim)),
                    None => Ok("OK timeout\n".to_string()),
                }
            }
            "extend" => self.command_extend(words),
            "ack" => self.command_ack(words),
            "reconcile" => self.command_reconcile(words),
            "clear-fault" => {
                if words.next() != Some("confirm=human") || words.next().is_some() {
                    return Err("usage: clear-fault confirm=human".to_string());
                }
                self.command_clear_fault(store)
            }
            _ => Err(operator_usage().to_string()),
        }
    }

    fn command_status(&self) -> Result<String, String> {
        // A status call may lazily open the durable queue. Serialize that
        // possible mutation with update quiesce; whichever starts second
        // receives a fixed refusal/state instead of racing exec.
        let host_gate = self
            .shared
            .fleet_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let profile = self
            .shared
            .fleet_id
            .strip_prefix("profile-")
            .unwrap_or(&self.shared.fleet_id);
        if host_gate.shutting_down {
            return Ok(format!(
                "OK {{\"schema\":1,\"fleet_id\":\"{}\",\"profile\":\"{}\",\"scope\":\"process-local\",\"actuator_mode\":\"interactive-owner\",\"stability\":\"experimental\",\"state\":\"stopping\"}}\n",
                json_escape(&self.shared.fleet_id),
                json_escape(profile),
            ));
        }
        if host_gate.update_quiesce.is_some() {
            return Ok(format!(
                "OK {{\"schema\":1,\"fleet_id\":\"{}\",\"profile\":\"{}\",\"scope\":\"process-local\",\"actuator_mode\":\"interactive-owner\",\"stability\":\"experimental\",\"state\":\"quiesced-for-update\"}}\n",
                json_escape(&self.shared.fleet_id),
                json_escape(profile),
            ));
        }
        let process_fault = host_gate.fleet_fault;
        let queue = match self.queue() {
            Ok(queue) => queue,
            Err(error) => {
                if let Some(fault) = process_fault {
                    return Ok(format!(
                        "OK {{\"schema\":1,\"fleet_id\":\"{}\",\"profile\":\"{}\",\"scope\":\"process-local\",\"actuator_mode\":\"interactive-owner\",\"stability\":\"experimental\",\"state\":\"faulted\",\"error\":\"{}\",\"managed_sids\":[],\"pending_baseline_sids\":[],\"in_doubt_event_ids\":[]}}\n",
                        json_escape(&self.shared.fleet_id),
                        json_escape(profile),
                        fault.as_str(),
                    ));
                }
                let state = if self
                    .shared
                    .slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .retry_after
                    .is_some()
                {
                    "standby"
                } else {
                    "failed"
                };
                return Ok(format!(
                    "OK {{\"schema\":1,\"fleet_id\":\"{}\",\"profile\":\"{}\",\"scope\":\"process-local\",\"actuator_mode\":\"interactive-owner\",\"stability\":\"experimental\",\"state\":\"{state}\",\"error\":\"{}\"}}\n",
                    json_escape(&self.shared.fleet_id),
                    json_escape(profile),
                    json_escape(&error),
                ));
            }
        };
        let managed = queue.managed_sids().map_err(|error| error.to_string())?;
        let in_doubt_ids = queue
            .unresolved_snapshots()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| {
                matches!(
                    event.status,
                    aterm_agent::operator::EventStatus::InDoubt { .. }
                )
            })
            .map(|event| event.id)
            .collect::<Vec<_>>();
        let managed_json = json_sid_list(&managed);
        let in_doubt_json = json_event_id_list(&in_doubt_ids);
        match queue.fleet_gate().map_err(|error| error.to_string())? {
            FleetGateStatus::Faulted(fault) => {
                return Ok(format!(
                    "OK {{\"schema\":1,\"fleet_id\":\"{}\",\"profile\":\"{}\",\"scope\":\"process-local\",\"actuator_mode\":\"interactive-owner\",\"stability\":\"experimental\",\"state\":\"faulted\",\"error\":\"{}\",\"managed_sids\":[{managed_json}],\"pending_baseline_sids\":[],\"in_doubt_event_ids\":[{in_doubt_json}]}}\n",
                    json_escape(&self.shared.fleet_id),
                    json_escape(profile),
                    fault.reason.as_str(),
                ));
            }
            FleetGateStatus::RebaselineRequired {
                fault,
                pending_sids,
            } => {
                let pending_json = json_sid_list(&pending_sids);
                return Ok(format!(
                    "OK {{\"schema\":1,\"fleet_id\":\"{}\",\"profile\":\"{}\",\"scope\":\"process-local\",\"actuator_mode\":\"interactive-owner\",\"stability\":\"experimental\",\"state\":\"rebaseline-required\",\"error\":\"{}\",\"managed_sids\":[{managed_json}],\"pending_baseline_sids\":[{pending_json}],\"in_doubt_event_ids\":[{in_doubt_json}]}}\n",
                    json_escape(&self.shared.fleet_id),
                    json_escape(profile),
                    fault.reason.as_str(),
                ));
            }
            FleetGateStatus::Healthy => {}
        }
        if let Some(fault) = process_fault {
            return Ok(format!(
                "OK {{\"schema\":1,\"fleet_id\":\"{}\",\"profile\":\"{}\",\"scope\":\"process-local\",\"actuator_mode\":\"interactive-owner\",\"stability\":\"experimental\",\"state\":\"faulted\",\"error\":\"{}\",\"managed_sids\":[{managed_json}],\"pending_baseline_sids\":[],\"in_doubt_event_ids\":[{in_doubt_json}]}}\n",
                json_escape(&self.shared.fleet_id),
                json_escape(profile),
                fault.as_str(),
            ));
        }
        let epoch = queue.durable_epoch().map_err(|error| error.to_string())?;
        let queued = queue.queued_len().map_err(|error| error.to_string())?;
        let unresolved = queue.unresolved_len().map_err(|error| error.to_string())?;
        let recovery = queue.recovery_report();
        Ok(format!(
            "OK {{\"schema\":1,\"fleet_id\":\"{}\",\"profile\":\"{}\",\"scope\":\"process-local\",\"actuator_mode\":\"interactive-owner\",\"stability\":\"experimental\",\"state\":\"active\",\"durable_epoch\":{epoch},\"recovery\":{{\"records_replayed\":{},\"repaired_partial_final_frame\":{}}},\"managed_sids\":[{managed_json}],\"queued\":{queued},\"unresolved\":{unresolved},\"in_doubt_event_ids\":[{in_doubt_json}]}}\n",
            json_escape(&self.shared.fleet_id),
            json_escape(profile),
            recovery.records_replayed,
            recovery.repaired_partial_final_frame,
        ))
    }

    fn command_manage(&self, store: &Store, sid: &str) -> Result<String, String> {
        let fault_guard = self.accepting_guard()?;
        let queue = self.queue()?;
        if queue.is_managed(sid).map_err(|error| error.to_string())? {
            return Ok("OK managed=true changed=0\n".to_string());
        }
        // Snapshot before changing the durable allowlist. Approval and exit are
        // safety-bearing facts, so the same atomic record that grants management
        // must retain them; other pre-existing text enters as neutral Changed.
        let observed = current_snapshot(store, sid)
            .ok_or_else(|| "session is absent or its terminal is busy; retry manage".to_string())?;
        let mut occurrences = self
            .shared
            .manage_occurrences
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_occurrence = *occurrences.get(sid).unwrap_or(&0);
        let occurrence = previous_occurrence
            .checked_add(1)
            .ok_or_else(|| "operator manage occurrence overflow".to_string())?;
        let epoch = queue.durable_epoch().map_err(|error| error.to_string())?;
        let generation = compose_generation(observed.generation, epoch, occurrence)?;
        let condition = manage_baseline_condition(observed.state, &observed.evidence);
        let event_id = queue
            .manage_with_baseline(NewEvent::new(sid, generation, condition, observed.evidence))
            .map_err(|error| error.to_string())?;
        let Some(event_id) = event_id else {
            return Ok("OK managed=true changed=0\n".to_string());
        };
        occurrences.insert(sid.to_string(), occurrence);
        self.surface_attention(
            observed.local_id,
            condition,
            EnqueueOutcome::Enqueued(event_id),
        );
        self.notify();
        drop(fault_guard);
        Ok("OK managed=true changed=1\n".to_string())
    }

    fn command_extend<'a>(
        &self,
        mut words: impl Iterator<Item = &'a str>,
    ) -> Result<String, String> {
        let _mutation_gate = self.mutation_guard()?;
        let event_id = parse_event_id(words.next(), "extend <event> <claim-token> [ms=<n>]")?;
        let token = parse_claim_token(words.next(), "extend <event> <claim-token> [ms=<n>]")?;
        let additional_ms = match words.next() {
            None => 120_000,
            Some(value) => value
                .strip_prefix("ms=")
                .ok_or_else(|| "usage: extend <event> <claim-token> [ms=<n>]".to_string())?
                .parse::<u64>()
                .map_err(|_| "usage: extend <event> <claim-token> [ms=<n>]".to_string())?,
        };
        if additional_ms == 0 || words.next().is_some() {
            return Err("usage: extend <event> <claim-token> [ms=<n>]".to_string());
        }
        let outcome = self
            .queue()?
            .extend(event_id, &token, Duration::from_millis(additional_ms))
            .map_err(|error| error.to_string())?;
        Ok(format!(
            "OK expires_at_ms={} cumulative_extension_ms={}\n",
            outcome.expires_at_ms, outcome.cumulative_extension_ms
        ))
    }

    fn command_ack<'a>(&self, mut words: impl Iterator<Item = &'a str>) -> Result<String, String> {
        let _mutation_gate = self.mutation_guard()?;
        const USAGE: &str = "usage: ack <event> <claim-token> <no-action|pause|escalate>";
        let event_id = parse_event_id(words.next(), USAGE)?;
        let token = parse_claim_token(words.next(), USAGE)?;
        let resolution = match words.next() {
            Some("no-action") => Resolution::NoAction,
            Some("pause") => Resolution::Paused,
            Some("escalate") => Resolution::Escalated,
            _ => return Err(USAGE.to_string()),
        };
        if words.next().is_some() {
            return Err(USAGE.to_string());
        }
        let outcome = self
            .queue()?
            .ack(event_id, &token, resolution)
            .map_err(|error| error.to_string())?;
        self.notify();
        let outcome = match outcome {
            AckOutcome::Resolved => "resolved",
            AckOutcome::AlreadyResolved => "already-resolved",
        };
        Ok(format!("OK {outcome}\n"))
    }

    fn command_inspect(&self, event_id: EventId) -> Result<String, String> {
        let _mutation_gate = self.mutation_guard()?;
        let event = self
            .queue()?
            .snapshot(event_id)
            .map_err(|error| error.to_string())?;
        Ok(format_event_inspection(&event))
    }

    fn command_reconcile<'a>(
        &self,
        mut words: impl Iterator<Item = &'a str>,
    ) -> Result<String, String> {
        let _mutation_gate = self.mutation_guard()?;
        const USAGE: &str =
            "usage: reconcile <event> <claim-token> <acted|no-action|pause|escalate> confirm=human";
        let event_id = parse_event_id(words.next(), USAGE)?;
        let token = parse_claim_token(words.next(), USAGE)?;
        let resolution = match words.next() {
            Some("acted") => Resolution::Acted,
            Some("no-action") => Resolution::NoAction,
            Some("pause") => Resolution::Paused,
            Some("escalate") => Resolution::Escalated,
            _ => return Err(USAGE.to_string()),
        };
        if words.next() != Some("confirm=human") || words.next().is_some() {
            return Err(USAGE.to_string());
        }
        let outcome = self
            .queue()?
            .reconcile_in_doubt(
                event_id,
                &token,
                resolution,
                "human reconciliation via Owner-authenticated fleet command",
            )
            .map_err(|error| error.to_string())?;
        self.notify();
        let outcome = match outcome {
            AckOutcome::Resolved => "resolved",
            AckOutcome::AlreadyResolved => "already-resolved",
        };
        Ok(format!("OK {outcome}\n"))
    }

    /// Run the explicit human recovery protocol under the same host gate as
    /// unmanagement and final PTY egress. Old work is revoked durably first;
    /// every still-live managed SID receives only a neutral Changed baseline.
    /// Missing, exited, or temporarily busy sessions remain named in the reply
    /// and must be retried or explicitly unmanaged before completion.
    fn command_clear_fault(&self, store: &Store) -> Result<String, String> {
        let mut host_gate = self.mutation_guard()?;
        if host_gate.shutting_down {
            return Err("operator host is shutting down".to_string());
        }
        let queue = self.queue()?;
        let pending = queue
            .begin_fault_clear()
            .map_err(|error| error.to_string())?;
        let fault = queue
            .fleet_fault()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "fault-clear lost its durable fault identity".to_string())?;
        host_gate.fleet_fault = Some(fault.reason);

        let mut baselined = 0_usize;
        for sid in pending {
            let Some(observed) = current_snapshot(store, &sid) else {
                continue;
            };
            if observed.state != SessionState::Alive {
                continue;
            }
            let generation = self.event_generation(&queue, &sid, observed.generation)?;
            match queue.enqueue_rebaseline(NewEvent::new(
                sid,
                generation,
                AttentionCondition::Changed,
                observed.evidence,
            )) {
                Ok(_) => baselined += 1,
                // An in-doubt event may temporarily consume the final queue
                // slot. Its human reconciliation frees capacity; a subsequent
                // clear-fault call resumes the idempotent pending roster.
                Err(OperatorError::QueueFull { .. }) => break,
                Err(error) => return Err(error.to_string()),
            }
        }

        let (pending_sids, fault_reason) =
            match queue.fleet_gate().map_err(|error| error.to_string())? {
                FleetGateStatus::RebaselineRequired {
                    fault,
                    pending_sids,
                } => (pending_sids, fault.reason),
                FleetGateStatus::Faulted(fault) => {
                    return Err(format!(
                        "fault clear returned to the faulted state ({})",
                        fault.reason.as_str()
                    ));
                }
                FleetGateStatus::Healthy => {
                    return Err("fault clear became healthy before explicit completion".to_string());
                }
            };
        host_gate.fleet_fault = Some(fault_reason);
        let in_doubt_ids = queue
            .unresolved_snapshots()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| {
                matches!(
                    event.status,
                    aterm_agent::operator::EventStatus::InDoubt { .. }
                )
            })
            .map(|event| event.id)
            .collect::<Vec<_>>();

        let cleared = pending_sids.is_empty() && in_doubt_ids.is_empty();
        if cleared {
            queue
                .complete_fault_clear()
                .map_err(|error| error.to_string())?;
            host_gate.fleet_fault = None;
        }
        drop(host_gate);
        self.notify();
        Ok(format!(
            "OK {{\"schema\":1,\"cleared\":{cleared},\"baselined\":{baselined},\"pending_baseline_sids\":[{}],\"in_doubt_event_ids\":[{}]}}\n",
            json_sid_list(&pending_sids),
            json_event_id_list(&in_doubt_ids),
        ))
    }

    /// Snapshot the current terminal with the queue's durable process epoch.
    pub(crate) fn current_snapshot(
        &self,
        store: &Store,
        sid: &str,
    ) -> Result<CurrentSnapshot, String> {
        let mut snapshot = current_snapshot(store, sid)
            .ok_or_else(|| "target session is absent or its terminal is busy".to_string())?;
        let queue = self.queue()?;
        snapshot.generation = self.event_generation(&queue, sid, snapshot.generation)?;
        Ok(snapshot)
    }
}

fn operator_usage() -> &'static str {
    "usage: operator <status|inspect|manage|unmanage|next|extend|ack|reconcile|clear-fault>"
}

fn one_argument<'a>(
    words: &mut impl Iterator<Item = &'a str>,
    usage: &str,
) -> Result<&'a str, String> {
    match (words.next(), words.next()) {
        (Some(value), None) => Ok(value),
        _ => Err(format!("usage: {usage}")),
    }
}

fn parse_next_timeout<'a>(mut words: impl Iterator<Item = &'a str>) -> Result<Duration, String> {
    let timeout_ms = match words.next() {
        None => 30_000,
        Some(value) => value
            .strip_prefix("timeout=")
            .ok_or_else(|| "usage: next [timeout=<ms>]".to_string())?
            .parse::<u64>()
            .map_err(|_| "usage: next [timeout=<ms>]".to_string())?,
    };
    if words.next().is_some() {
        return Err("usage: next [timeout=<ms>]".to_string());
    }
    Ok(Duration::from_millis(timeout_ms.min(30_000)))
}

fn parse_event_id(value: Option<&str>, usage: &str) -> Result<EventId, String> {
    value
        .ok_or_else(|| usage.to_string())?
        .parse::<EventId>()
        .map_err(|_| usage.to_string())
}

fn parse_claim_token(value: Option<&str>, usage: &str) -> Result<ClaimToken, String> {
    ClaimToken::from_wire(value.ok_or_else(|| usage.to_string())?).map_err(|_| usage.to_string())
}

fn compose_generation(
    mut generation: EventGeneration,
    durable_epoch: u64,
    manage_occurrence: u64,
) -> Result<EventGeneration, String> {
    // A single u64 carries three independently advancing identities. Explicit
    // bounds make the packing injective and fail closed instead of wrapping into
    // a previously resolved event generation.
    let run = u32::try_from(durable_epoch)
        .map_err(|_| "operator durable epoch exhausted generation space".to_string())?;
    let occurrence = u16::try_from(manage_occurrence)
        .map_err(|_| "operator manage occurrence exhausted generation space".to_string())?;
    let lifecycle = u16::try_from(generation.lifecycle_epoch)
        .map_err(|_| "operator lifecycle exhausted generation space".to_string())?;
    generation.lifecycle_epoch =
        (u64::from(run) << 32) | (u64::from(occurrence) << 16) | u64::from(lifecycle);
    Ok(generation)
}

fn condition_name(condition: AttentionCondition) -> &'static str {
    match condition {
        AttentionCondition::Changed => "changed",
        AttentionCondition::Ready => "ready",
        AttentionCondition::SuspectedStuck => "suspected-stuck",
        AttentionCondition::ApprovalRequired => "approval-required",
        AttentionCondition::SessionExited => "session-exited",
        AttentionCondition::Escalation => "escalation",
    }
}

fn resolution_name(resolution: Resolution) -> &'static str {
    match resolution {
        Resolution::Acted => "acted",
        Resolution::NoAction => "no-action",
        Resolution::Paused => "pause",
        Resolution::Escalated => "escalate",
    }
}

fn json_sid_list(sids: &[String]) -> String {
    sids.iter()
        .map(|sid| format!("\"{}\"", json_escape(sid)))
        .collect::<Vec<_>>()
        .join(",")
}

fn json_event_id_list(event_ids: &[EventId]) -> String {
    event_ids
        .iter()
        .map(|event_id| event_id.get().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn format_event_inspection(event: &EventSnapshot) -> String {
    use aterm_agent::operator::EventStatus;

    let (status, token, reason, resolution) = match &event.status {
        EventStatus::Queued => ("queued", None, None, None),
        EventStatus::Delivered { token, .. } => ("delivered", Some(token.expose()), None, None),
        EventStatus::ActionInFlight { token, .. } => {
            ("action-in-flight", Some(token.expose()), None, None)
        }
        EventStatus::Resolved {
            token, resolution, ..
        } => (
            "resolved",
            Some(token.expose()),
            None,
            Some(resolution_name(*resolution)),
        ),
        EventStatus::ResolvedUnclaimed {
            resolution, reason, ..
        } => (
            "resolved",
            None,
            Some(reason.as_str()),
            Some(resolution_name(*resolution)),
        ),
        EventStatus::InDoubt { token, reason, .. } => (
            "in-doubt",
            token.as_ref().map(ClaimToken::expose),
            Some(reason.as_str()),
            None,
        ),
    };
    let token = token.map_or_else(
        || "null".to_string(),
        |token| format!("\"{}\"", json_escape(token)),
    );
    let reason = reason.map_or_else(
        || "null".to_string(),
        |reason| format!("\"{}\"", json_escape(reason)),
    );
    let resolution = resolution.map_or_else(
        || "null".to_string(),
        |resolution| format!("\"{}\"", resolution),
    );
    format!(
        "OK {{\"schema\":1,\"event_id\":{},\"sid\":\"{}\",\"condition\":\"{}\",\"status\":\"{status}\",\"claim_token\":{token},\"resolution\":{resolution},\"reason\":{reason},\"redelivery_count\":{},\"escalated\":{}}}\n",
        event.id.get(),
        json_escape(&event.sid),
        condition_name(event.condition),
        event.redelivery_count,
        event.escalated,
    )
}

fn format_claim(control: &ControlHandle, store: &Store, claim: &Claim) -> String {
    let EventSnapshot {
        id,
        sid,
        generation,
        condition,
        redelivery_count,
        escalated,
        ..
    } = &claim.event;
    // Evidence is never durable operator state. Re-read it from the live terminal
    // and disclose it only when it is still bound to this exact event generation.
    let live = control.current_snapshot(store, sid).ok();
    let (evidence_json, stale) = match live {
        Some(snapshot) if snapshot.generation == *generation => {
            (format!("\"{}\"", json_escape(&snapshot.evidence)), false)
        }
        _ => ("null".to_string(), true),
    };
    format!(
        "OK {{\"schema\":1,\"event\":{{\"event_id\":{},\"claim_token\":\"{}\",\"expires_at_ms\":{},\"sid\":\"{}\",\"condition\":\"{}\",\"generation\":{{\"lifecycle_epoch\":{},\"alternate_screen\":{},\"content_seq\":{},\"fingerprint\":\"{}\"}},\"evidence\":{},\"stale\":{},\"redelivery_count\":{},\"escalated\":{}}}}}\n",
        id.get(),
        claim.token.expose(),
        claim.expires_at_ms,
        json_escape(sid),
        condition_name(*condition),
        generation.lifecycle_epoch,
        generation.alternate_screen,
        generation.content_seq,
        hex_bytes(&generation.fingerprint),
        evidence_json,
        stale,
        redelivery_count,
        escalated,
    )
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn json_escape(value: &str) -> String {
    crate::control::json_escape(value)
}

struct DurableSink {
    control: ControlHandle,
    store: Store,
}

impl DurableSink {
    fn condition(kind: CandidateKind) -> AttentionCondition {
        match kind {
            CandidateKind::LeadershipBaseline => AttentionCondition::Changed,
            CandidateKind::SessionMissing | CandidateKind::SessionExited => {
                AttentionCondition::SessionExited
            }
            CandidateKind::ApprovalPrompt => AttentionCondition::ApprovalRequired,
            CandidateKind::BusyBecameAttention => AttentionCondition::Ready,
        }
    }

    fn maintenance_queue_result<T>(
        &self,
        operation: &str,
        result: Result<T, OperatorError>,
    ) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                aterm_log::warn!("operator maintenance {operation} failed: {error}");
                self.control
                    .latch_fleet_fault(FleetFaultReason::DurableStateUnavailable);
                None
            }
        }
    }
}

impl EventSink for DurableSink {
    fn begin_observation_cycle(&self) -> Result<Option<Box<dyn ObservationCycleGuard>>, String> {
        self.control
            .begin_observer_mutation()
            .map(|activity| Some(Box::new(activity) as Box<dyn ObservationCycleGuard>))
    }

    fn enqueue(&self, candidate: Candidate) -> Result<(), String> {
        let _mutation = self.control.begin_observer_mutation()?;
        if let Some(error) = self.control.fleet_fault() {
            return Err(error.as_str().to_string());
        }
        let queue = match self.control.queue() {
            Ok(queue) => queue,
            Err(error) => {
                self.control
                    .latch_fleet_fault(FleetFaultReason::DurableStateUnavailable);
                return Err(error);
            }
        };
        let generation =
            match self
                .control
                .event_generation(&queue, &candidate.sid, candidate.generation.into())
            {
                Ok(generation) => generation,
                Err(error) => {
                    self.control
                        .latch_fleet_fault(FleetFaultReason::DurableStateUnavailable);
                    return Err(error);
                }
            };
        let condition = Self::condition(candidate.kind);
        let local_id = candidate.local_id;
        let outcome = match queue.enqueue(NewEvent::new(
            candidate.sid,
            generation,
            condition,
            candidate.screen_tail,
        )) {
            Ok(outcome) => outcome,
            Err(error @ OperatorError::QueueFull { .. }) => {
                let message = error.to_string();
                self.control
                    .latch_fleet_fault(FleetFaultReason::ObserverOverflow);
                return Err(message);
            }
            Err(error) => {
                let message = error.to_string();
                self.control
                    .latch_fleet_fault(FleetFaultReason::DurableStateUnavailable);
                return Err(message);
            }
        };
        if !matches!(outcome, EnqueueOutcome::Unmanaged) {
            self.control.surface_attention(local_id, condition, outcome);
            self.control.notify();
        }
        Ok(())
    }

    fn observation_access(&self) -> ObservationAccess {
        if self.control.update_quiesced() || self.control.fleet_fault().is_some() {
            return ObservationAccess::Standby;
        }
        let queue = match self.control.queue() {
            Ok(queue) => queue,
            Err(error) => {
                // NEVER a fleet fault. State that never opened has nothing in
                // doubt, and a fault here would be reachable — on a profile with
                // an empty allowlist, within one 250 ms reconcile of launch — from
                // an ordinary unwritable or non-POSIX state root, taking every
                // self-update seam with it. Retry quietly, then go dormant.
                // See `QueueSlot::record_open_failure`.
                let dormant = self
                    .control
                    .shared
                    .slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .dormant;
                return if dormant {
                    ObservationAccess::Disabled(error)
                } else {
                    ObservationAccess::Standby
                };
            }
        };
        match queue.fleet_gate() {
            Ok(FleetGateStatus::Healthy) => {}
            Ok(FleetGateStatus::Faulted(_) | FleetGateStatus::RebaselineRequired { .. }) => {
                // Standby is a RESET signal to observer_loop: discard the
                // classifier and every process-local pending candidate. A stale
                // pre-fault Ready must never flush after human rebaseline.
                return ObservationAccess::Standby;
            }
            Err(error) => {
                let message = format!("operator durable fleet gate read failed: {error}");
                self.control
                    .latch_fleet_fault(FleetFaultReason::DurableStateUnavailable);
                return ObservationAccess::Failed(message);
            }
        }
        match queue.managed_sids() {
            Ok(managed) => ObservationAccess::Managed(managed.into_iter().collect()),
            Err(error) => {
                let message = format!("operator durable roster read failed: {error}");
                self.control
                    .latch_fleet_fault(FleetFaultReason::DurableStateUnavailable);
                ObservationAccess::Failed(message)
            }
        }
    }

    fn maintenance(&self) {
        self.control.flush_pending_notice();
        let Ok(_mutation) = self.control.begin_observer_mutation() else {
            return;
        };
        let Ok(queue) = self.control.queue() else {
            return;
        };
        let Some(outcomes) =
            self.maintenance_queue_result("claim reclaim", queue.reclaim_expired())
        else {
            return;
        };
        if outcomes.is_empty() {
            return;
        }
        for outcome in outcomes.iter().filter(|outcome| outcome.escalated) {
            let Some(event) = self
                .maintenance_queue_result("escalation snapshot", queue.snapshot(outcome.event_id))
            else {
                return;
            };
            self.control.surface_escalation_once(&self.store, &event);
        }
        // Requeued work and escalation transitions must wake a parked `next`.
        self.control.notify();
    }

    fn fault(&self, reason: FleetFaultReason) {
        self.control.latch_fleet_fault(reason);
    }

    fn fault_marker_without_live(&self, reason: FleetFaultReason) -> Result<(), String> {
        let queue = self
            .control
            .shared
            .marker_queue
            .get()
            .ok_or_else(|| "operator durable queue never opened".to_string())?;
        queue
            .latch_fault_marker_without_live(reason)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// Best-effort process-local wrapper around the independent, synchronized fault
/// marker. The production implementation never takes `DurableQueue.live`; the
/// unwind catch keeps a pathological embedding sink from turning cleanup into a
/// second panic.
fn publish_fault_marker_without_live(sink: &dyn EventSink, reason: FleetFaultReason) {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sink.fault_marker_without_live(reason)
    })) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            aterm_log::warn!("operator independent fleet-fault marker failed: {error}");
        }
        Err(_) => {
            aterm_log::warn!("operator independent fleet-fault marker panicked");
        }
    }
}

/// Process-lifetime observer handle. Production calls [`Self::shutdown_and_join`]
/// explicitly after `run_app`; `Drop` is only the unwind/early-return backstop.
pub(crate) struct Runtime {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    fault_sink: Option<Arc<dyn EventSink>>,
    shutdown_timeout: Duration,
}

impl Runtime {
    pub(crate) fn shutdown_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.thread().unpark();
            let deadline = Instant::now() + self.shutdown_timeout;
            while !join.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if !join.is_finished() {
                // The worker did not establish that its local queue drained.
                // Publish a synchronous durable stop before detaching it, so a
                // successor cannot treat potentially-lost observations as healthy.
                if let Some(sink) = self.fault_sink.as_ref() {
                    publish_fault_marker_without_live(
                        sink.as_ref(),
                        FleetFaultReason::DurableStateUnavailable,
                    );
                }
                aterm_log::warn!(
                    "operator observer did not stop within two seconds; fleet marked faulted"
                );
            } else if join.join().is_err() {
                if let Some(sink) = self.fault_sink.as_ref() {
                    publish_fault_marker_without_live(
                        sink.as_ref(),
                        FleetFaultReason::ObserverPanicked,
                    );
                }
                aterm_log::warn!("operator observer thread panicked during shutdown");
            }
        }
        self.fault_sink = None;
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shutdown_and_join();
    }
}

/// Start the default-on, read-only local observer.
///
/// Thread creation is the only fallible startup step. A caller treats failure as a
/// safely-disabled observer and keeps the terminal running normally.
pub(crate) fn start(
    store: Store,
    subscribers: Subscribers,
    sink: Arc<dyn EventSink>,
) -> Result<Runtime, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker_sink = Arc::clone(&sink);
    let join = std::thread::Builder::new()
        .name("aterm-operator-observer".to_string())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                observer_loop(&store, &subscribers, worker_sink.as_ref(), &worker_stop);
            }));
            if result.is_err() {
                // Observation is advisory and read-only. A failed worker disables
                // itself; it must never take the terminal process down with it.
                // Its delivery guarantee is no longer trustworthy, so new claims
                // and actions must fail closed rather than silently running blind.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker_sink.fault(FleetFaultReason::ObserverPanicked);
                }));
                publish_fault_marker_without_live(
                    worker_sink.as_ref(),
                    FleetFaultReason::ObserverPanicked,
                );
                aterm_log::warn!("operator observer disabled after an internal panic");
            }
        })
        .map_err(|error| format!("could not start operator observer: {error}"))?;
    Ok(Runtime {
        stop,
        join: Some(join),
        fault_sink: Some(sink),
        shutdown_timeout: SHUTDOWN_JOIN_TIMEOUT,
    })
}

/// Whether an operator env switch is engaged.
///
/// The predicate is deliberately the SAME one `$ATERM_NO_CONTROL_SOCK` uses
/// (`aterm_types::control_socket::socket_directive`): set-and-not-`0` engages,
/// unset/empty/`0` does not. One spelling for every resident-subsystem env var
/// is the whole point — a user who learned one has learned all of them, and
/// `ATERM_NO_OPERATOR=0` cannot accidentally mean "off".
fn switch_engaged(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty() && value != "0")
}

fn environment_switch(name: &str) -> bool {
    switch_engaged(
        std::env::var_os(name)
            .map(|value| value.to_string_lossy().into_owned())
            .as_deref(),
    )
}

/// Whether this process starts the embedded operator at all.
///
/// Default-OFF: `$ATERM_OPERATOR` opts in, `$ATERM_NO_OPERATOR` overrides it.
/// Without the opt-in there is no observer thread, no durable state directory,
/// and every operator verb answers `ERR operator unavailable` off the
/// already-shipped `None` path.
pub(crate) fn enabled_by_environment() -> bool {
    environment_switch(OPERATOR_OPT_IN_ENV) && !environment_switch(OPERATOR_KILL_ENV)
}

/// Resolve the durable fleet namespace for this aterm profile.
///
/// This identity deliberately contains no PID, root SID, nonce, or other launch
/// material: a cold restart must reopen the same WAL and explicit allowlist. The
/// OS lock in [`DurableQueue`] is the authority boundary when two processes use
/// the same profile. `ATERM_STATE_HOME` separates installations/deployments; the
/// optional `ATERM_OPERATOR_PROFILE` names independent profiles within that root.
fn default_fleet_id() -> Result<String, String> {
    let profile = match std::env::var_os(OPERATOR_PROFILE_ENV) {
        Some(value) => value
            .into_string()
            .map_err(|_| format!("{OPERATOR_PROFILE_ENV} must be valid UTF-8"))?,
        None => DEFAULT_OPERATOR_PROFILE.to_string(),
    };
    fleet_id_for_profile(&profile)
}

fn fleet_id_for_profile(profile: &str) -> Result<String, String> {
    if profile.is_empty()
        || profile.len() > 112
        || profile == "."
        || profile == ".."
        || !profile
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!(
            "{OPERATOR_PROFILE_ENV} must be 1..=112 ASCII letters, digits, '.', '-', or '_'"
        ));
    }
    Ok(format!("profile-{profile}"))
}

/// Start the production default-on observer and its durable per-profile queue.
/// Queue leadership may initially be in standby when another live aterm uses the
/// same profile; the observer's maintenance tick retries without blocking the UI
/// thread. A standby has no queue or actuator authority.
pub(crate) fn start_default(
    store: Store,
    subscribers: Subscribers,
    notify_tx: std::sync::mpsc::SyncSender<crate::notify::NotifyMsg>,
) -> Result<(Runtime, ControlHandle), String> {
    let fleet_id = default_fleet_id()?;
    let control = ControlHandle::new(fleet_id, notify_tx);
    let sink: Arc<dyn EventSink> = Arc::new(DurableSink {
        control: control.clone(),
        store: store.clone(),
    });
    let runtime = start(store, subscribers, sink)?;
    Ok((runtime, control))
}

fn observer_loop(
    store: &Store,
    subscribers: &Subscribers,
    sink: &dyn EventSink,
    stop: &AtomicBool,
) {
    let mut classifier = Classifier::default();
    #[cfg(test)]
    let pending_capacity = sink.pending_capacity_for_test();
    #[cfg(not(test))]
    let pending_capacity = PENDING_CAPACITY;
    let mut queue = CoalescingQueue::with_capacity(pending_capacity);
    let mut watched = Vec::new();
    let mut subscription = None;
    let mut last_sink_warning = None;

    loop {
        // A stop which arrives during this transaction requests a subsequent
        // post-fence cycle. Only a cycle admitted with stop already visible is
        // the one final shutdown snapshot.
        let final_cycle = stop.load(Ordering::Acquire);
        // Admission precedes every source-of-truth read. `UpdateQuiesce` can win
        // before this point or reject while the token is live, never splice
        // itself between classify and durable publish.
        let cycle_activity = match sink.begin_observation_cycle() {
            Ok(activity) => activity,
            Err(_) => {
                // A successful cycle never leaves local work behind. Preserve
                // fail-closed behavior if that invariant is ever violated.
                if !queue.is_empty() {
                    sink.fault(FleetFaultReason::DurableStateUnavailable);
                }
                if stop.load(Ordering::Acquire) {
                    return;
                }
                classifier = Classifier::default();
                queue = CoalescingQueue::with_capacity(pending_capacity);
                subscription = None;
                watched.clear();
                last_sink_warning = None;
                std::thread::park_timeout(RECONCILE_INTERVAL);
                continue;
            }
        };

        // Catch each transaction while its RAII activity is still live. This is
        // load-bearing: panic-fault publication must precede the decrement which
        // could otherwise admit process replacement into an unaudited gap.
        let cycle = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_observation_cycle(
                store,
                subscribers,
                sink,
                final_cycle,
                &mut classifier,
                &mut queue,
                &mut watched,
                &mut subscription,
                &mut last_sink_warning,
            )
        }));
        let outcome = match cycle {
            Ok(outcome) => outcome,
            Err(_) => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    sink.fault(FleetFaultReason::ObserverPanicked);
                }));
                publish_fault_marker_without_live(sink, FleetFaultReason::ObserverPanicked);
                drop(cycle_activity);
                aterm_log::warn!("operator observer disabled after an internal panic");
                return;
            }
        };
        drop(cycle_activity);

        match outcome {
            ObserverCycleOutcome::Stop => return,
            ObserverCycleOutcome::Reset => {
                classifier = Classifier::default();
                queue = CoalescingQueue::with_capacity(pending_capacity);
                subscription = None;
                watched.clear();
                last_sink_warning = None;
            }
            ObserverCycleOutcome::Wait => {}
        }

        if stop.load(Ordering::Acquire) {
            continue;
        }
        if let Some(wait) = &subscription {
            let _ = wait.wait(RECONCILE_INTERVAL);
        } else {
            std::thread::park_timeout(RECONCILE_INTERVAL);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObserverCycleOutcome {
    Wait,
    Reset,
    Stop,
}

#[allow(clippy::too_many_arguments)]
fn run_observation_cycle(
    store: &Store,
    subscribers: &Subscribers,
    sink: &dyn EventSink,
    final_cycle: bool,
    classifier: &mut Classifier,
    queue: &mut CoalescingQueue,
    watched: &mut Vec<u64>,
    subscription: &mut Option<Subscription>,
    last_sink_warning: &mut Option<Instant>,
) -> ObserverCycleOutcome {
    sink.maintenance();
    let (handles, managed_baseline) = match sink.observation_access() {
        ObservationAccess::All => (
            store
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .snapshot(),
            false,
        ),
        ObservationAccess::Managed(managed) => {
            // Load the durable roster before touching the Store, then clone only
            // allowlisted handles. An empty default roster takes no Store or
            // Terminal lock and computes no screen hash.
            let handles = if managed.is_empty() {
                Vec::new()
            } else {
                let guard = store
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let mut handles = managed
                    .iter()
                    .filter_map(|sid| guard.by_sid(&aterm_session::SessionId::new(sid)).cloned())
                    .collect::<Vec<_>>();
                handles.sort_by_key(|handle| handle.local_id);
                handles
            };
            (handles, true)
        }
        ObservationAccess::Standby => {
            // Durable non-authority/fault/rebaseline is already the persistent
            // explanation for any discarded classifier state.
            return if final_cycle {
                ObserverCycleOutcome::Stop
            } else {
                ObserverCycleOutcome::Reset
            };
        }
        ObservationAccess::Disabled(error) => {
            aterm_log::warn!("{error}; observation ends for this process");
            return ObserverCycleOutcome::Stop;
        }
        ObservationAccess::Failed(error) => {
            let should_warn =
                last_sink_warning.is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(30));
            if should_warn {
                aterm_log::warn!(
                    "operator durable roster unavailable; observation disabled: {error}"
                );
                *last_sink_warning = Some(Instant::now());
            }
            return if final_cycle {
                ObserverCycleOutcome::Stop
            } else {
                ObserverCycleOutcome::Reset
            };
        }
    };

    let mut local_ids: Vec<u64> = handles.iter().map(|handle| handle.local_id).collect();
    local_ids.sort_unstable();
    local_ids.dedup();
    if local_ids != *watched {
        // A Subscription owns its registry entries. Replacing it drops the old
        // registration before the next wait; no control-plane socket lane exists.
        *subscription =
            (!local_ids.is_empty()).then(|| SubscriberSet::register(subscribers, &local_ids));
        *watched = local_ids;
    }

    let observed: Vec<ObservedSession> = handles.iter().map(observe_handle).collect();
    let candidates = if managed_baseline {
        classifier.observe_managed(observed, Instant::now())
    } else {
        classifier.observe(observed, Instant::now())
    };
    #[cfg(test)]
    sink.after_classify_for_test(candidates.len());
    for candidate in candidates {
        if !queue.push(candidate) {
            // Publish the durable stop before returning to the outer scope that
            // drops the observation activity token.
            sink.fault(FleetFaultReason::ObserverOverflow);
            aterm_log::warn!("operator observer queue saturated; fleet marked faulted");
            return if final_cycle {
                ObserverCycleOutcome::Stop
            } else {
                ObserverCycleOutcome::Reset
            };
        }
    }

    // Empty the entire bounded local queue before releasing cycle admission.
    // FLUSH_BUDGET is a batch boundary, not a loss window: shutdown or update can
    // observe either every candidate durable, or a durable fleet fault.
    while !queue.is_empty() {
        if let Err(error) = queue.flush(sink, FLUSH_BUDGET) {
            let should_warn =
                last_sink_warning.is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(30));
            if should_warn {
                aterm_log::warn!("operator event sink unavailable; fleet faulted: {error}");
                *last_sink_warning = Some(Instant::now());
            }
            sink.fault(FleetFaultReason::DurableStateUnavailable);
            return if final_cycle {
                ObserverCycleOutcome::Stop
            } else {
                ObserverCycleOutcome::Reset
            };
        }
        #[cfg(test)]
        sink.after_flush_batch_for_test(queue.pending.len());
    }
    *last_sink_warning = None;

    if final_cycle {
        ObserverCycleOutcome::Stop
    } else {
        ObserverCycleOutcome::Wait
    }
}

/// Immutable observation cloned out of GUI-owned handles. Tests drive this shape
/// directly, keeping transition logic independent of a PTY or event loop.
#[derive(Clone, Debug)]
struct ObservedSession {
    sid: String,
    local_id: u64,
    state: SessionState,
    surface: Option<Surface>,
}

#[derive(Clone, Debug)]
struct Surface {
    generation: Generation,
    screen_tail: String,
}

/// Exact current screen identity used by the actuator's final TOCTOU check.
pub(crate) struct CurrentSnapshot {
    pub(crate) generation: EventGeneration,
    pub(crate) evidence: String,
    pub(crate) state: SessionState,
    pub(crate) local_id: u64,
}

pub(crate) fn current_snapshot(store: &Store, sid: &str) -> Option<CurrentSnapshot> {
    let handle = {
        let guard = store
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.by_sid(&aterm_session::SessionId::new(sid))?.clone()
    };
    let observed = observe_handle(&handle);
    let surface = observed.surface?;
    Some(CurrentSnapshot {
        generation: surface.generation.into(),
        evidence: surface.screen_tail,
        state: observed.state,
        local_id: observed.local_id,
    })
}

fn observe_handle(handle: &SessionHandle) -> ObservedSession {
    let guard = match handle.term.try_lock() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    };
    let surface = guard.map(|terminal| {
        let screen_tail = terminal_evidence(&terminal);
        let fingerprint: [u8; 32] = Sha256::digest(screen_tail.as_bytes()).into();
        Surface {
            generation: Generation {
                lifecycle_epoch: 0,
                alternate_screen: terminal.is_alternate_screen(),
                content_seq: terminal.content_seq(),
                fingerprint,
            },
            screen_tail,
        }
    });
    ObservedSession {
        sid: handle.sid.as_str().to_string(),
        local_id: handle.local_id,
        state: handle.state,
        surface,
    }
}

/// The single bounded terminal evidence projection used by both the resident
/// observer and the final actuator fence. The actuator calls this while holding
/// the terminal lock through its conditional PTY write, so these two producers
/// cannot drift on row/byte bounds or fingerprint input.
pub(crate) fn terminal_evidence(terminal: &aterm_core::terminal::Terminal) -> String {
    let rows = terminal.rows() as usize;
    let first = rows.saturating_sub(EVIDENCE_ROWS);
    let mut screen_tail = String::new();
    for row in first..rows {
        if !screen_tail.is_empty() {
            screen_tail.push('\n');
        }
        screen_tail.push_str(&crate::control::visible_row(terminal, row));
    }
    trim_to_tail_bytes(&mut screen_tail, EVIDENCE_BYTES);
    screen_tail
}

fn trim_to_tail_bytes(text: &mut String, max: usize) {
    if text.len() <= max {
        return;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.drain(..start);
}

#[derive(Debug)]
struct Track {
    local_id: u64,
    state: SessionState,
    generation: Generation,
    has_surface: bool,
    screen_tail: String,
    last_change: Instant,
    busy_seen: bool,
    approval_emitted: Option<Generation>,
    attention_emitted: Option<Generation>,
}

impl Track {
    fn new(observed: &ObservedSession, now: Instant) -> Self {
        let surface = observed.surface.as_ref();
        Self {
            local_id: observed.local_id,
            state: observed.state,
            generation: surface.map_or_else(Generation::initial, |s| s.generation),
            has_surface: surface.is_some(),
            screen_tail: surface.map_or_else(String::new, |s| s.screen_tail.clone()),
            last_change: now,
            busy_seen: false,
            approval_emitted: None,
            attention_emitted: None,
        }
    }

    fn candidate(&self, sid: &str, kind: CandidateKind) -> Candidate {
        Candidate {
            sid: sid.to_string(),
            local_id: self.local_id,
            generation: self.generation,
            kind,
            screen_tail: self.screen_tail.clone(),
        }
    }
}

#[derive(Default)]
struct Classifier {
    sessions: HashMap<String, Track>,
}

impl Classifier {
    fn observe(&mut self, observed: Vec<ObservedSession>, now: Instant) -> Vec<Candidate> {
        self.observe_inner(observed, now, false)
    }

    /// Production leadership starts with a complete managed-roster baseline.
    /// Unlike an ordinary ready transition, that baseline is `Changed`: it
    /// closes the old-owner/new-owner scan gap but grants no actuator inference.
    fn observe_managed(&mut self, observed: Vec<ObservedSession>, now: Instant) -> Vec<Candidate> {
        self.observe_inner(observed, now, true)
    }

    fn observe_inner(
        &mut self,
        observed: Vec<ObservedSession>,
        now: Instant,
        baseline_new_sessions: bool,
    ) -> Vec<Candidate> {
        let mut candidates = Vec::new();
        let mut seen = HashSet::with_capacity(observed.len());
        for observed in observed {
            seen.insert(observed.sid.clone());
            if let Some(track) = self.sessions.get_mut(&observed.sid) {
                Self::update_track(
                    track,
                    &observed,
                    now,
                    baseline_new_sessions,
                    &mut candidates,
                );
            } else {
                let mut track = Track::new(&observed, now);
                if observed.state == SessionState::Exited {
                    track.generation = track.generation.next_lifecycle();
                    candidates.push(track.candidate(&observed.sid, CandidateKind::SessionExited));
                } else if track.has_surface && looks_like_approval(&track.screen_tail) {
                    track.approval_emitted = Some(track.generation);
                    candidates.push(track.candidate(&observed.sid, CandidateKind::ApprovalPrompt));
                } else if baseline_new_sessions && track.has_surface {
                    candidates
                        .push(track.candidate(&observed.sid, CandidateKind::LeadershipBaseline));
                }
                self.sessions.insert(observed.sid, track);
            }
        }

        let missing: Vec<String> = self
            .sessions
            .keys()
            .filter(|sid| !seen.contains(*sid))
            .cloned()
            .collect();
        for sid in missing {
            if let Some(mut track) = self.sessions.remove(&sid) {
                track.generation = track.generation.next_lifecycle();
                candidates.push(track.candidate(&sid, CandidateKind::SessionMissing));
            }
        }
        candidates
    }

    fn update_track(
        track: &mut Track,
        observed: &ObservedSession,
        now: Instant,
        baseline_new_sessions: bool,
        candidates: &mut Vec<Candidate>,
    ) {
        track.local_id = observed.local_id;
        if observed.state == SessionState::Exited && track.state != SessionState::Exited {
            track.state = observed.state;
            track.generation = track.generation.next_lifecycle();
            track.busy_seen = false;
            candidates.push(track.candidate(&observed.sid, CandidateKind::SessionExited));
            return;
        }
        track.state = observed.state;
        if observed.state == SessionState::Exited {
            return;
        }

        let Some(surface) = observed.surface.as_ref() else {
            return;
        };
        let first_surface = !track.has_surface;
        if first_surface || surface.generation != track.generation {
            // A first readable snapshot is a baseline, not proof of preceding work.
            // Every later grid-generation change proves display activity.
            track.busy_seen |= track.has_surface;
            track.generation = Generation {
                lifecycle_epoch: track.generation.lifecycle_epoch,
                ..surface.generation
            };
            track.has_surface = true;
            track.screen_tail.clone_from(&surface.screen_tail);
            track.last_change = now;
        }

        if looks_like_approval(&track.screen_tail)
            && track.approval_emitted != Some(track.generation)
        {
            track.approval_emitted = Some(track.generation);
            track.busy_seen = false;
            candidates.push(track.candidate(&observed.sid, CandidateKind::ApprovalPrompt));
            return;
        }

        if first_surface && baseline_new_sessions {
            candidates.push(track.candidate(&observed.sid, CandidateKind::LeadershipBaseline));
            return;
        }

        if track.busy_seen
            && now.saturating_duration_since(track.last_change) >= ATTENTION_SETTLE
            && looks_like_ready_prompt(&track.screen_tail)
            && track.attention_emitted != Some(track.generation)
        {
            track.attention_emitted = Some(track.generation);
            track.busy_seen = false;
            candidates.push(track.candidate(&observed.sid, CandidateKind::BusyBecameAttention));
        }
    }
}

/// Conservative, deterministic presentation heuristic. Both an approval/request
/// phrase and an actionable choice affordance must be present; prose merely discussing
/// approval is not an event.
pub(crate) fn looks_like_approval(screen: &str) -> bool {
    let lower = screen.to_lowercase();
    let request = [
        "do you want to proceed",
        "would you like to proceed",
        "would you like to run",
        "allow this command",
        "allow this action",
        "permission required",
        "requires approval",
        "approve this command",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let choice = [
        "[y/n]",
        "(y/n)",
        "yes, proceed",
        "yes, allow",
        "allow once",
        "always allow",
        "don't ask again",
        "deny",
        "esc to cancel",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    request && choice
}

fn manage_baseline_condition(state: SessionState, evidence: &str) -> AttentionCondition {
    if state == SessionState::Exited {
        AttentionCondition::SessionExited
    } else if looks_like_approval(evidence) {
        AttentionCondition::ApprovalRequired
    } else {
        AttentionCondition::Changed
    }
}

/// Prompt-shape heuristic for a completed coding-agent turn. It intentionally knows
/// only the distinctive Claude/Codex composer glyphs and rejects screens still carrying
/// a live interrupt affordance. This raises a candidate; it never drives the session.
fn looks_like_ready_prompt(screen: &str) -> bool {
    let lower = screen.to_lowercase();
    if [
        "esc to interrupt",
        "ctrl-c to interrupt",
        "ctrl+c to interrupt",
        "press esc to interrupt",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return false;
    }
    screen.lines().rev().take(8).any(|line| {
        let line = line.trim().trim_start_matches(['│', '┃', '┆', '┊', ' ']);
        line.starts_with('❯') || line.starts_with('›') || line.starts_with('»')
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct EventKey {
    sid: String,
    generation: Generation,
}

impl From<&Candidate> for EventKey {
    fn from(candidate: &Candidate) -> Self {
        Self {
            sid: candidate.sid.clone(),
            generation: candidate.generation,
        }
    }
}

struct CoalescingQueue {
    pending: HashMap<EventKey, Candidate>,
    order: VecDeque<EventKey>,
    capacity: usize,
}

impl Default for CoalescingQueue {
    fn default() -> Self {
        Self::with_capacity(PENDING_CAPACITY)
    }
}

impl CoalescingQueue {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            pending: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Coalesce one SID/generation to its strongest candidate. At capacity, a
    /// new distinct event is refused rather than evicting any already-observed
    /// event. `observer_loop` turns that refusal into a sticky fleet fault; losing
    /// either the resident or incoming event silently would violate delivery.
    fn push(&mut self, candidate: Candidate) -> bool {
        let key = EventKey::from(&candidate);
        if let Some(current) = self.pending.get_mut(&key) {
            if candidate.kind.priority() > current.kind.priority() {
                *current = candidate;
            }
            return true;
        }
        if self.pending.len() >= self.capacity {
            return false;
        }
        self.order.push_back(key.clone());
        self.pending.insert(key, candidate);
        true
    }

    fn flush(&mut self, sink: &dyn EventSink, budget: usize) -> Result<(), String> {
        for _ in 0..budget {
            let Some(key) = self.order.front().cloned() else {
                return Ok(());
            };
            let Some(candidate) = self.pending.get(&key).cloned() else {
                self.order.pop_front();
                continue;
            };
            sink.enqueue(candidate)?;
            self.order.pop_front();
            self.pending.remove(&key);
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_agent::operator::EventStatus;
    use aterm_session::sink::SinkWriter;
    use aterm_session::{EdgeTable, LaunchNonce, SessionId};
    use aterm_spec::derive::{operator_resync_cursor_model, operator_wal_actuator_model};
    use aterm_spec::verify;
    use std::collections::BTreeMap;

    fn registered_operator_session(sid: &str, local_id: u64) -> SessionHandle {
        let sid = SessionId::new(sid);
        let nonce = LaunchNonce::generate();
        let ctx = Arc::new(crate::SessionCtx {
            sink: Arc::new(SinkWriter::new(-1)),
            edges: Mutex::new(EdgeTable::new()),
            self_id: sid.clone(),
            nonce,
            turn_lease: Mutex::new(None),
            cast: Arc::new(Mutex::new(crate::cast::CastRecorder::new(80, 24))),
            temporal: Arc::new(Mutex::new(crate::temporal::TemporalRecorder::new())),
            byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
            turns: Arc::new(Mutex::new(crate::turn_ledger::TurnLedger::default())),
            meta: Mutex::new(crate::session_timeline::SessionMeta::default()),
            app_kitty: Mutex::new(crate::app_kitty::AppKittySlot::default()),
            timeline: Arc::new(Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
        });
        SessionHandle {
            sid,
            nonce,
            local_id,
            parent: None,
            state: SessionState::Alive,
            title: format!("operator-{local_id}"),
            term: Arc::new(Mutex::new(aterm_core::terminal::Terminal::new(24, 80))),
            master: -1,
            ctx,
        }
    }

    #[test]
    fn fleet_identity_is_stable_and_profile_scoped() {
        assert_eq!(
            fleet_id_for_profile(DEFAULT_OPERATOR_PROFILE).unwrap(),
            "profile-default"
        );
        assert_eq!(fleet_id_for_profile("work").unwrap(), "profile-work");
        assert_ne!(
            fleet_id_for_profile("work").unwrap(),
            fleet_id_for_profile("personal").unwrap()
        );
        for invalid in ["", ".", "..", "has/slash", "has space", "line\nbreak"] {
            assert!(
                fleet_id_for_profile(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn the_operator_switches_read_exactly_like_their_control_socket_sibling() {
        // Unset / empty / "0" leave a switch disengaged; anything else engages
        // it. This is `socket_directive`'s `no_control_sock` rule verbatim; the
        // pin exists so these env vars cannot drift apart and leave a user who
        // typed `ATERM_NO_OPERATOR=0` silently unobserved.
        for disengaged in [None, Some(""), Some("0")] {
            assert!(
                !switch_engaged(disengaged),
                "{disengaged:?} must not engage a switch"
            );
            assert_eq!(
                switch_engaged(disengaged),
                matches!(
                    aterm_types::control_socket::socket_directive(None, disengaged),
                    aterm_types::control_socket::SocketDirective::Disabled
                ),
                "the switches must agree on {disengaged:?}"
            );
        }
        for engages in ["1", "true", "off", "yes", "anything"] {
            assert!(switch_engaged(Some(engages)), "{engages} must engage");
            assert!(matches!(
                aterm_types::control_socket::socket_directive(None, Some(engages)),
                aterm_types::control_socket::SocketDirective::Disabled
            ));
        }
    }

    #[test]
    fn durable_generation_keeps_run_manage_and_lifecycle_axes_distinct() {
        let surface = EventGeneration::new(0, false, 7, [9; 32]);
        let exited = EventGeneration::new(1, false, 7, [9; 32]);
        let first_manage = compose_generation(surface, 4, 1).unwrap();
        assert_ne!(
            first_manage,
            compose_generation(surface, 5, 1).unwrap(),
            "a new durable owner run cannot coalesce with its predecessor"
        );
        assert_ne!(
            first_manage,
            compose_generation(surface, 4, 2).unwrap(),
            "unmanage then re-manage of an unchanged screen is a new occurrence"
        );
        assert_ne!(
            first_manage,
            compose_generation(exited, 4, 1).unwrap(),
            "exit/removal lifecycle edges survive durable-epoch composition"
        );
        assert!(compose_generation(surface, u64::from(u32::MAX) + 1, 1).is_err());
        assert!(compose_generation(surface, 4, u64::from(u16::MAX) + 1).is_err());
    }

    #[test]
    fn durable_sink_surfaces_only_new_human_attention_without_terminal_text() {
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let control = ControlHandle::new("test-fleet".to_string(), tx);
        let event_id = EventId::from_wire(1).unwrap();

        control.surface_attention(
            7,
            AttentionCondition::Ready,
            EnqueueOutcome::Enqueued(event_id),
        );
        control.surface_attention(
            7,
            AttentionCondition::ApprovalRequired,
            EnqueueOutcome::Unmanaged,
        );
        control.surface_attention(
            7,
            AttentionCondition::ApprovalRequired,
            EnqueueOutcome::Coalesced {
                event_id,
                strengthened: false,
            },
        );
        assert!(
            rx.try_recv().is_err(),
            "ordinary/unmanaged/duplicate events stay quiet"
        );

        control.surface_attention(
            7,
            AttentionCondition::ApprovalRequired,
            EnqueueOutcome::Enqueued(event_id),
        );
        let approval = rx.try_recv().unwrap();
        assert_eq!(approval.session, 7);
        assert_eq!(approval.title.as_deref(), Some("aterm operator"));
        assert_eq!(
            approval.body,
            "A managed session is waiting for human approval."
        );

        control.surface_attention(
            8,
            AttentionCondition::SessionExited,
            EnqueueOutcome::Coalesced {
                event_id,
                strengthened: true,
            },
        );
        let exited = rx.try_recv().unwrap();
        assert_eq!(exited.session, 8);
        assert_eq!(exited.title.as_deref(), Some("aterm operator"));
        assert_eq!(exited.body, "A managed session exited.");
        assert!(rx.try_recv().is_err());
    }

    fn observed(sid: &str, seq: u64, screen: &str) -> ObservedSession {
        observed_on(sid, true, seq, screen)
    }

    fn observed_on(sid: &str, alternate_screen: bool, seq: u64, screen: &str) -> ObservedSession {
        ObservedSession {
            sid: sid.to_string(),
            local_id: 7,
            state: SessionState::Alive,
            surface: Some(Surface {
                generation: Generation {
                    lifecycle_epoch: 0,
                    alternate_screen,
                    content_seq: seq,
                    fingerprint: Sha256::digest(screen.as_bytes()).into(),
                },
                screen_tail: screen.to_string(),
            }),
        }
    }

    #[test]
    fn operator_embedded_snapshot_resync_matches_model() {
        type State = BTreeMap<&'static str, i64>;

        let validate = |before: &State, after: &State, action: &str, label: &str| {
            let model = operator_resync_cursor_model();
            let (accepted, diagnostics) = verify::validate_transition_tiered(
                &model,
                &[("Buggy", 0)],
                before,
                after,
                Some(action),
                label,
            );
            assert!(
                accepted,
                "{label} must be admitted as {action}\n{diagnostics}"
            );
        };

        let model = operator_resync_cursor_model();
        let initial = model.init_state();
        let now = Instant::now();
        let mut classifier = Classifier::default();

        // A first in-process Store snapshot establishes the baseline cursor. It
        // is already represented by the model's caught-up initial state.
        assert!(
            classifier
                .observe(vec![observed_on("s-a", false, 10, "old main grid")], now,)
                .is_empty()
        );
        let baseline = classifier.sessions.get("s-a").unwrap();
        assert_eq!(baseline.screen_tail, "old main grid");
        assert!(!baseline.generation.alternate_screen);

        // A same-grid generation update replaces both the concrete cursor and
        // the complete evidence snapshot in one Classifier transition.
        assert!(
            classifier
                .observe(
                    vec![observed_on("s-a", false, 11, "fresh main grid")],
                    now + Duration::from_millis(10),
                )
                .is_empty()
        );
        let mut advanced = initial.clone();
        advanced.insert("source_seq", 1);
        advanced.insert("cursor_seq", 1);
        validate(
            &initial,
            &advanced,
            "ObserveAdvance",
            "embedded operator full-snapshot advance",
        );
        let current = classifier.sessions.get("s-a").unwrap();
        assert_eq!(current.generation.content_seq, 11);
        assert_eq!(current.screen_tail, "fresh main grid");

        assert!(
            classifier
                .observe(
                    vec![observed_on("s-a", false, 11, "fresh main grid")],
                    now + Duration::from_millis(20),
                )
                .is_empty()
        );
        validate(
            &advanced,
            &advanced,
            "ParkCurrent",
            "embedded operator caught-up park",
        );

        // The alternate-grid identity changes even when its independent content
        // sequence happens to equal the last main-grid value. Classifier installs
        // the newly read Store snapshot; it never carries old main-grid evidence
        // across that reset.
        let new_grid = "fresh alternate grid\n❯ ";
        assert!(
            classifier
                .observe(
                    vec![observed_on("s-a", true, 11, new_grid)],
                    now + Duration::from_millis(30),
                )
                .is_empty()
        );
        let mut reset = advanced.clone();
        reset.insert("source_epoch", 2);
        reset.insert("source_seq", 0);
        reset.insert("cursor_epoch", 2);
        reset.insert("cursor_seq", 0);
        reset.insert("snapshot_epoch", 2);
        validate(
            &advanced,
            &reset,
            "ResetAndResnapshot",
            "embedded operator alternate-grid reset",
        );
        let current = classifier.sessions.get("s-a").unwrap();
        assert!(current.generation.alternate_screen);
        assert_eq!(current.screen_tail, new_grid);
        let expected_fingerprint: [u8; 32] = Sha256::digest(new_grid.as_bytes()).into();
        assert_eq!(current.generation.fingerprint, expected_fingerprint);

        // Once the new full snapshot settles, the actual candidate is built only
        // from that generation and its fresh evidence.
        let candidates = classifier.observe(
            vec![observed_on("s-a", true, 11, new_grid)],
            now + Duration::from_millis(30) + ATTENTION_SETTLE,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].kind, CandidateKind::BusyBecameAttention);
        assert!(candidates[0].generation.alternate_screen);
        assert_eq!(candidates[0].screen_tail, new_grid);

        // NEGATIVE CONTROL: the historical defect advances the source identity
        // while retaining the prior cursor/snapshot. Healthy Next rejects it.
        let mut forged = advanced.clone();
        forged.insert("source_epoch", 2);
        forged.insert("source_seq", 0);
        forged.insert("silent_loss", 1);
        let (accepted, diagnostics) = verify::validate_transition_tiered(
            &model,
            &[("Buggy", 0)],
            &advanced,
            &forged,
            Some("ResetWithoutResnapshot"),
            "embedded operator stale-snapshot negative control",
        );
        assert!(
            !accepted,
            "stale-snapshot negative control was accepted; binding is vacuous\n{diagnostics}"
        );
    }

    #[test]
    fn approval_requires_request_and_choice() {
        assert!(looks_like_approval(
            "Would you like to run the following command?\n1. Yes, proceed\n2. No"
        ));
        assert!(looks_like_approval(
            "Do you want to proceed?\n❯ 1. Yes, allow once\n  2. Deny"
        ));
        assert!(!looks_like_approval(
            "The design requires approval before the next release."
        ));
        assert!(!looks_like_approval(
            "Do you want to proceed with the explanation?"
        ));
        let prompt = "Do you want to proceed?\n1. Yes, allow once\n2. Deny";
        assert_eq!(
            manage_baseline_condition(SessionState::Alive, prompt),
            AttentionCondition::ApprovalRequired
        );
        assert_eq!(
            manage_baseline_condition(SessionState::Exited, prompt),
            AttentionCondition::SessionExited,
            "exit must dominate an approval-looking final screen"
        );
        assert_eq!(
            manage_baseline_condition(SessionState::Alive, "ordinary pre-existing output"),
            AttentionCondition::Changed
        );
    }

    #[test]
    fn ready_prompt_rejects_a_still_busy_composer() {
        assert!(looks_like_ready_prompt("answer complete\n❯ "));
        assert!(looks_like_ready_prompt("done\n│ › ask a follow-up"));
        assert!(!looks_like_ready_prompt("Thinking… esc to interrupt\n› "));
        assert!(!looks_like_ready_prompt("ordinary shell output\n$ "));
    }

    #[test]
    fn initial_approval_is_emitted_once_per_generation() {
        let now = Instant::now();
        let mut classifier = Classifier::default();
        let prompt = "Do you want to proceed?\n1. Yes, allow once\n2. Deny";
        let first = classifier.observe(vec![observed("s-a", 10, prompt)], now);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, CandidateKind::ApprovalPrompt);
        assert!(
            classifier
                .observe(
                    vec![observed("s-a", 10, prompt)],
                    now + Duration::from_secs(1)
                )
                .is_empty()
        );
        let next = classifier.observe(
            vec![observed("s-a", 11, prompt)],
            now + Duration::from_secs(2),
        );
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].generation.content_seq, 11);
    }

    #[test]
    fn busy_to_attention_requires_change_settle_and_emits_once() {
        let now = Instant::now();
        let mut classifier = Classifier::default();
        assert!(
            classifier
                .observe(vec![observed("s-a", 1, "❯ ")], now)
                .is_empty(),
            "an initial prompt is a baseline, not a completed turn"
        );
        assert!(
            classifier
                .observe(
                    vec![observed("s-a", 2, "result\n❯ ")],
                    now + Duration::from_millis(10),
                )
                .is_empty(),
            "the changed generation has not settled"
        );
        assert!(
            classifier
                .observe(
                    vec![observed("s-a", 2, "result\n❯ ")],
                    now + Duration::from_millis(700),
                )
                .is_empty()
        );
        let attention = classifier.observe(
            vec![observed("s-a", 2, "result\n❯ ")],
            now + Duration::from_millis(800),
        );
        assert_eq!(attention.len(), 1);
        assert_eq!(attention[0].kind, CandidateKind::BusyBecameAttention);
        assert!(
            classifier
                .observe(
                    vec![observed("s-a", 2, "result\n❯ ")],
                    now + Duration::from_secs(2)
                )
                .is_empty()
        );
    }

    #[test]
    fn leadership_baseline_closes_the_handoff_scan_gap_without_claiming_ready() {
        let now = Instant::now();
        let mut outgoing = Classifier::default();
        assert!(
            outgoing
                .observe(vec![observed("s-a", 8, "working")], now)
                .is_empty()
        );

        // The target can finish after the old observer's final scan and before
        // the successor acquires the profile lock. A fresh ordinary classifier
        // would silently treat this ready surface as its initial baseline.
        let ready = observed("s-a", 9, "result\n❯ ");
        let mut successor = Classifier::default();
        let baseline = successor.observe_managed(vec![ready], now + Duration::from_millis(1));
        assert_eq!(baseline.len(), 1);
        assert_eq!(baseline[0].kind, CandidateKind::LeadershipBaseline);
        assert_eq!(
            DurableSink::condition(baseline[0].kind),
            AttentionCondition::Changed,
            "takeover must surface ambiguity without authorizing a ready turn"
        );
        assert!(
            successor
                .observe_managed(
                    vec![observed("s-a", 9, "result\n❯ ")],
                    now + Duration::from_millis(2)
                )
                .is_empty(),
            "the leadership baseline is one event, not a polling stream"
        );
    }

    #[test]
    fn exit_then_removal_are_distinct_single_edges() {
        let now = Instant::now();
        let mut classifier = Classifier::default();
        assert!(
            classifier
                .observe(vec![observed("s-a", 3, "working")], now)
                .is_empty()
        );
        let mut exited = observed("s-a", 3, "done");
        exited.state = SessionState::Exited;
        let exit = classifier.observe(vec![exited.clone()], now + Duration::from_millis(1));
        assert_eq!(exit.len(), 1);
        assert_eq!(exit[0].kind, CandidateKind::SessionExited);
        assert!(
            classifier
                .observe(vec![exited], now + Duration::from_millis(2))
                .is_empty()
        );
        let missing = classifier.observe(Vec::new(), now + Duration::from_millis(3));
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].kind, CandidateKind::SessionMissing);
        assert!(
            classifier
                .observe(Vec::new(), now + Duration::from_millis(4))
                .is_empty()
        );
        assert!(missing[0].generation > exit[0].generation);
    }

    #[test]
    fn queue_coalesces_one_generation_to_the_strongest_candidate() {
        let generation = Generation {
            lifecycle_epoch: 0,
            alternate_screen: false,
            content_seq: 9,
            fingerprint: [9; 32],
        };
        let event = |kind| Candidate {
            sid: "s-a".to_string(),
            local_id: 1,
            generation,
            kind,
            screen_tail: String::new(),
        };
        let mut queue = CoalescingQueue::default();
        assert!(queue.push(event(CandidateKind::BusyBecameAttention)));
        assert!(queue.push(event(CandidateKind::ApprovalPrompt)));
        assert_eq!(queue.pending.len(), 1);
        assert_eq!(
            queue.pending.values().next().map(|event| event.kind),
            Some(CandidateKind::ApprovalPrompt)
        );
    }

    #[test]
    fn pending_overflow_never_evicts_a_distinct_observed_event() {
        let candidate = |index: usize, kind| Candidate {
            sid: format!("s-{index}"),
            local_id: index as u64,
            generation: Generation {
                lifecycle_epoch: 0,
                alternate_screen: false,
                content_seq: index as u64,
                fingerprint: [index as u8; 32],
            },
            kind,
            screen_tail: String::new(),
        };

        let mut queue = CoalescingQueue::default();
        for index in 0..PENDING_CAPACITY {
            assert!(queue.push(candidate(index, CandidateKind::ApprovalPrompt)));
        }
        assert!(
            !queue.push(candidate(PENDING_CAPACITY, CandidateKind::SessionMissing)),
            "a stronger incoming event must fault, not silently evict an older event"
        );
        assert_eq!(queue.pending.len(), PENDING_CAPACITY);
        assert!(
            queue
                .pending
                .values()
                .all(|event| event.kind == CandidateKind::ApprovalPrompt)
        );
        assert!(!queue.pending.contains_key(&EventKey::from(&candidate(
            PENDING_CAPACITY,
            CandidateKind::SessionMissing,
        ))));

        let mut all_strong = CoalescingQueue::default();
        for index in 0..PENDING_CAPACITY {
            assert!(all_strong.push(candidate(index, CandidateKind::SessionMissing)));
        }
        assert!(
            !all_strong.push(candidate(PENDING_CAPACITY, CandidateKind::SessionMissing)),
            "every distinct overflow must be reported to observer_loop, which latches a fleet fault"
        );
        assert_eq!(all_strong.pending.len(), PENDING_CAPACITY);
        assert!(
            all_strong
                .pending
                .values()
                .all(|event| event.kind == CandidateKind::SessionMissing)
        );
    }

    fn operator_test_directory(label: &str) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "aterm-operator-host-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ))
    }

    fn operator_test_queue(
        directory: &std::path::Path,
        capacity: usize,
        redelivery_cap: u32,
    ) -> DurableQueue {
        let config = QueueConfig {
            capacity,
            visibility_timeout: Duration::from_millis(1),
            max_cumulative_extension: Duration::from_millis(10),
            redelivery_cap,
            max_wal_bytes: 2 * 1024 * 1024,
        };
        DurableQueue::open(directory, 1, config).unwrap()
    }

    #[test]
    fn classify_to_overflow_fault_stays_inside_update_exclusion() {
        struct OverflowBarrierSink {
            control: ControlHandle,
            managed: HashSet<String>,
            classified_tx: std::sync::mpsc::SyncSender<usize>,
            release_rx: Mutex<std::sync::mpsc::Receiver<()>>,
            fault_tx: std::sync::mpsc::SyncSender<(FleetFaultReason, bool)>,
        }

        impl EventSink for OverflowBarrierSink {
            fn enqueue(&self, _candidate: Candidate) -> Result<(), String> {
                Ok(())
            }

            fn begin_observation_cycle(
                &self,
            ) -> Result<Option<Box<dyn ObservationCycleGuard>>, String> {
                self.control
                    .begin_observer_mutation()
                    .map(|activity| Some(Box::new(activity) as Box<dyn ObservationCycleGuard>))
            }

            fn observation_access(&self) -> ObservationAccess {
                ObservationAccess::Managed(self.managed.clone())
            }

            fn fault(&self, reason: FleetFaultReason) {
                let active = self
                    .control
                    .shared
                    .fleet_fault
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .active_observer_mutations
                    != 0;
                self.control.latch_fleet_fault(reason);
                let _ = self.fault_tx.try_send((reason, active));
            }

            fn pending_capacity_for_test(&self) -> usize {
                1
            }

            fn after_classify_for_test(&self, candidate_count: usize) {
                self.classified_tx.send(candidate_count).unwrap();
                self.release_rx
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .recv_timeout(Duration::from_secs(2))
                    .expect("release classify barrier");
            }
        }

        let directory = operator_test_directory("cycle-overflow-fence");
        let _ = std::fs::remove_dir_all(&directory);
        let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
        let control = ControlHandle::new_with_directory(
            "test-cycle-overflow-fence".to_string(),
            Some(directory.clone()),
            notify_tx,
        );
        let queue = control.queue().unwrap();
        let epoch = queue.durable_epoch().unwrap();
        let store = crate::session_store::new_store();
        let managed = ["s-overflow-a", "s-overflow-b"]
            .into_iter()
            .map(str::to_string)
            .collect::<HashSet<_>>();
        {
            let mut guard = store.write().unwrap();
            guard.register(registered_operator_session("s-overflow-a", 1));
            guard.register(registered_operator_session("s-overflow-b", 2));
        }
        let (classified_tx, classified_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let (fault_tx, fault_rx) = std::sync::mpsc::sync_channel(1);
        let sink = Arc::new(OverflowBarrierSink {
            control: control.clone(),
            managed,
            classified_tx,
            release_rx: Mutex::new(release_rx),
            fault_tx,
        });
        let mut runtime = start(
            store,
            crate::subscribe::new_registry(),
            Arc::clone(&sink) as Arc<dyn EventSink>,
        )
        .unwrap();

        assert_eq!(
            classified_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            2,
            "the barrier must sit behind a non-empty classified batch"
        );
        let refused = match control.try_begin_update_quiesce() {
            Ok(_) => panic!("update quiesce entered during a classified observer cycle"),
            Err(error) => error,
        };
        assert!(refused.contains("transaction is in flight"), "{refused}");
        release_tx.send(()).unwrap();
        let (reason, activity_was_live) = fault_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(reason, FleetFaultReason::ObserverOverflow);
        assert!(
            activity_was_live,
            "overflow publication ran after cycle admission was dropped"
        );
        assert_eq!(
            queue.fleet_fault().unwrap().map(|fault| fault.reason),
            Some(FleetFaultReason::ObserverOverflow)
        );

        control.begin_shutdown();
        runtime.shutdown_and_join();
        drop(runtime);
        drop(sink);
        drop(control);
        drop(queue);
        let reopened = DurableQueue::open(&directory, epoch + 1, QueueConfig::default()).unwrap();
        assert_eq!(
            reopened.fleet_fault().unwrap().map(|fault| fault.reason),
            Some(FleetFaultReason::ObserverOverflow),
            "the fault seen under cycle admission must survive cold reopen"
        );
        drop(reopened);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn shutdown_after_first_batch_drains_more_than_flush_budget_before_reopen() {
        struct ShutdownBarrierSink {
            inner: DurableSink,
            classified: std::sync::atomic::AtomicUsize,
            classified_cycles: std::sync::atomic::AtomicUsize,
            blocked_once: AtomicBool,
            batch_tx: std::sync::mpsc::SyncSender<usize>,
            release_rx: Mutex<std::sync::mpsc::Receiver<()>>,
        }

        impl EventSink for ShutdownBarrierSink {
            fn enqueue(&self, candidate: Candidate) -> Result<(), String> {
                self.inner.enqueue(candidate)
            }

            fn begin_observation_cycle(
                &self,
            ) -> Result<Option<Box<dyn ObservationCycleGuard>>, String> {
                self.inner.begin_observation_cycle()
            }

            fn observation_access(&self) -> ObservationAccess {
                self.inner.observation_access()
            }

            fn maintenance(&self) {
                self.inner.maintenance();
            }

            fn fault(&self, reason: FleetFaultReason) {
                self.inner.fault(reason);
            }

            fn after_classify_for_test(&self, candidate_count: usize) {
                self.classified.store(candidate_count, Ordering::Release);
                self.classified_cycles.fetch_add(1, Ordering::AcqRel);
            }

            fn after_flush_batch_for_test(&self, pending: usize) {
                if !self.blocked_once.swap(true, Ordering::AcqRel) {
                    self.batch_tx.send(pending).unwrap();
                    self.release_rx
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .recv_timeout(Duration::from_secs(2))
                        .expect("release shutdown barrier");
                }
            }
        }

        const EVENT_COUNT: usize = FLUSH_BUDGET + 4;
        let directory = operator_test_directory("shutdown-full-drain");
        let _ = std::fs::remove_dir_all(&directory);
        let config = QueueConfig {
            capacity: 64,
            visibility_timeout: Duration::from_millis(10),
            max_cumulative_extension: Duration::from_secs(1),
            redelivery_cap: 3,
            max_wal_bytes: 2 * 1024 * 1024,
        };
        let queue = DurableQueue::open(&directory, 1, config.clone()).unwrap();
        let store = crate::session_store::new_store();
        {
            let mut guard = store.write().unwrap();
            for index in 0..EVENT_COUNT {
                let sid = format!("s-shutdown-{index:02}");
                queue.manage_sid(&sid).unwrap();
                guard.register(registered_operator_session(&sid, index as u64));
            }
        }
        let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(64);
        let control = ControlHandle::new("test-shutdown-full-drain".to_string(), notify_tx);
        {
            let mut slot = control.shared.slot.lock().unwrap();
            slot.queue = Some(queue.clone());
            slot.retry_after = None;
        }
        let (batch_tx, batch_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let sink = Arc::new(ShutdownBarrierSink {
            inner: DurableSink {
                control: control.clone(),
                store: store.clone(),
            },
            classified: std::sync::atomic::AtomicUsize::new(0),
            classified_cycles: std::sync::atomic::AtomicUsize::new(0),
            blocked_once: AtomicBool::new(false),
            batch_tx,
            release_rx: Mutex::new(release_rx),
        });
        let mut runtime = start(
            store,
            crate::subscribe::new_registry(),
            Arc::clone(&sink) as Arc<dyn EventSink>,
        )
        .unwrap();

        assert_eq!(
            batch_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            EVENT_COUNT - FLUSH_BUDGET,
            "the stop race must occur with a real >16 local backlog"
        );
        assert_eq!(
            sink.classified.load(Ordering::Acquire),
            EVENT_COUNT,
            "the first cycle must classify every managed session"
        );
        control.begin_shutdown();
        runtime.stop.store(true, Ordering::Release);
        release_tx.send(()).unwrap();
        runtime.shutdown_and_join();
        assert_eq!(
            sink.classified_cycles.load(Ordering::Acquire),
            2,
            "a stop arriving mid-cycle owes exactly one post-fence final scan"
        );

        let snapshots = queue.snapshots().unwrap();
        let fault = queue.fleet_fault().unwrap();
        assert!(
            snapshots.len() == EVENT_COUNT || fault.is_some(),
            "shutdown returned with {} of {EVENT_COUNT} events and no durable fault",
            snapshots.len()
        );
        assert_eq!(
            snapshots.len(),
            EVENT_COUNT,
            "healthy test sink must fully drain"
        );

        drop(runtime);
        drop(sink);
        drop(control);
        drop(queue);
        let reopened = DurableQueue::open(&directory, 2, config).unwrap();
        let reopened_snapshots = reopened.snapshots().unwrap();
        let reopened_fault = reopened.fleet_fault().unwrap();
        assert!(
            reopened_snapshots.len() == EVENT_COUNT || reopened_fault.is_some(),
            "cold reopen found {} of {EVENT_COUNT} events and no durable fault",
            reopened_snapshots.len()
        );
        assert_eq!(reopened_snapshots.len(), EVENT_COUNT);
        assert!(reopened_fault.is_none());
        drop(reopened);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn shutdown_timeout_uses_live_lock_independent_marker_and_returns() {
        struct WedgedCycleSink {
            queue: DurableQueue,
            managed: HashSet<String>,
            reached_tx: std::sync::mpsc::SyncSender<usize>,
            release_rx: Mutex<std::sync::mpsc::Receiver<()>>,
            blocked_once: AtomicBool,
        }

        impl EventSink for WedgedCycleSink {
            fn enqueue(&self, _candidate: Candidate) -> Result<(), String> {
                Ok(())
            }

            fn observation_access(&self) -> ObservationAccess {
                ObservationAccess::Managed(self.managed.clone())
            }

            fn fault_marker_without_live(&self, reason: FleetFaultReason) -> Result<(), String> {
                self.queue
                    .latch_fault_marker_without_live(reason)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }

            fn after_classify_for_test(&self, candidate_count: usize) {
                if !self.blocked_once.swap(true, Ordering::AcqRel) {
                    self.reached_tx.send(candidate_count).unwrap();
                    self.release_rx
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .recv()
                        .expect("release wedged observer");
                }
            }
        }

        let directory = operator_test_directory("shutdown-timeout-marker");
        let _ = std::fs::remove_dir_all(&directory);
        let config = QueueConfig::default();
        let queue = DurableQueue::open(&directory, 1, config.clone()).unwrap();
        let store = crate::session_store::new_store();
        store
            .write()
            .unwrap()
            .register(registered_operator_session("s-timeout", 1));
        let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let sink = Arc::new(WedgedCycleSink {
            queue: queue.clone(),
            managed: ["s-timeout".to_string()].into_iter().collect(),
            reached_tx,
            release_rx: Mutex::new(release_rx),
            blocked_once: AtomicBool::new(false),
        });
        let mut runtime = start(
            store,
            crate::subscribe::new_registry(),
            Arc::clone(&sink) as Arc<dyn EventSink>,
        )
        .unwrap();
        runtime.shutdown_timeout = Duration::from_millis(30);
        assert_eq!(
            reached_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            1,
            "timeout seam must hold a real classified candidate"
        );

        let started = Instant::now();
        runtime.shutdown_and_join();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "timeout fallback waited behind the wedged observer"
        );
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Arc::strong_count(&sink) != 1 {
            assert!(Instant::now() < deadline, "detached observer did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
        drop(runtime);
        drop(sink);
        drop(queue);

        // Core's `operator_fault_marker_bypasses_held_live_mutex` regression
        // proves the same primitive does not acquire `DurableQueue.live`; this
        // host test proves the timeout path uses that primitive and returns.
        let reopened = DurableQueue::open(&directory, 2, config).unwrap();
        assert_eq!(
            reopened.fleet_fault().unwrap().map(|fault| fault.reason),
            Some(FleetFaultReason::DurableStateUnavailable)
        );
        drop(reopened);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn human_fault_clear_names_pending_sids_and_requires_explicit_unmanage() {
        let directory = operator_test_directory("fault-clear-command");
        let _ = std::fs::remove_dir_all(&directory);
        let queue = operator_test_queue(&directory, 8, 3);
        queue.manage_sid("s-stale").unwrap();
        queue
            .latch_fault(FleetFaultReason::ObserverOverflow)
            .unwrap();

        let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
        let control = ControlHandle::new("test-fault-clear".to_string(), notify_tx);
        {
            let mut slot = control.shared.slot.lock().unwrap();
            slot.queue = Some(queue.clone());
            slot.retry_after = None;
        }
        let store = crate::session_store::new_store();

        assert!(
            control
                .command(&store, "clear-fault")
                .contains("usage: clear-fault confirm=human")
        );
        let first = control.command(&store, "clear-fault confirm=human");
        assert!(first.contains("\"cleared\":false"), "{first}");
        assert!(
            first.contains("\"pending_baseline_sids\":[\"s-stale\"]"),
            "{first}"
        );
        let status = control.command(&store, "status");
        assert!(
            status.contains("\"state\":\"rebaseline-required\""),
            "{status}"
        );
        assert!(
            status.contains("\"managed_sids\":[\"s-stale\"]"),
            "{status}"
        );
        assert!(
            status.contains("\"pending_baseline_sids\":[\"s-stale\"]"),
            "{status}"
        );

        assert_eq!(
            control.command(&store, "unmanage s-stale"),
            "OK managed=false changed=1\n"
        );
        let completed = control.command(&store, "clear-fault confirm=human");
        assert!(completed.contains("\"cleared\":true"), "{completed}");
        assert!(matches!(
            queue.fleet_gate().unwrap(),
            FleetGateStatus::Healthy
        ));
        let status = control.command(&store, "status");
        assert!(status.contains("\"state\":\"active\""), "{status}");
        assert!(status.contains("\"managed_sids\":[]"), "{status}");

        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn stable_profile_has_one_leader_and_reopens_its_explicit_allowlist() {
        let directory = operator_test_directory("profile-leadership");
        let _ = std::fs::remove_dir_all(&directory);
        let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);

        let leader = ControlHandle::new_with_directory(
            "profile-default".to_string(),
            Some(directory.clone()),
            notify_tx.clone(),
        );
        let leader_queue = leader.queue().unwrap();
        assert!(
            leader_queue.managed_sids().unwrap().is_empty(),
            "a stable namespace must still start with an explicit empty allowlist"
        );
        let leader_sink = DurableSink {
            control: leader.clone(),
            store: crate::session_store::new_store(),
        };
        assert!(matches!(
            leader_sink.observation_access(),
            ObservationAccess::Managed(ref managed) if managed.is_empty()
        ));
        leader_queue.manage_sid("s-explicit").unwrap();

        let standby = ControlHandle::new_with_directory(
            "profile-default".to_string(),
            Some(directory.clone()),
            notify_tx,
        );
        let standby_error = match standby.queue() {
            Ok(_) => panic!("a second live process acquired shared authority"),
            Err(error) => error,
        };
        assert!(standby_error.contains("WAL lock"), "{standby_error}");
        let standby_sink = DurableSink {
            control: standby.clone(),
            store: crate::session_store::new_store(),
        };
        assert!(matches!(
            standby_sink.observation_access(),
            ObservationAccess::Standby
        ));
        let standby_status = standby.command_status().unwrap();
        assert!(
            standby_status.contains("\"state\":\"standby\""),
            "a second live process must expose standby, never shared authority"
        );
        assert!(standby_status.contains("\"profile\":\"default\""));
        assert!(standby_status.contains("\"scope\":\"process-local\""));
        assert!(standby_status.contains("\"actuator_mode\":\"interactive-owner\""));
        // RFC-operator-2026-08-15 §10.4: the experimental label ships in every
        // status reply until every §9 gate passes. It is machine-readable on
        // purpose — a client that never reads prose still sees it.
        assert!(
            standby_status.contains("\"stability\":\"experimental\""),
            "every status reply must carry the experimental label: {standby_status}"
        );

        drop(leader_sink);
        drop(leader_queue);
        drop(leader);
        standby.shared.slot.lock().unwrap().retry_after = Some(Instant::now());
        let successor_queue = standby.queue().unwrap();
        assert_eq!(
            successor_queue.managed_sids().unwrap(),
            vec!["s-explicit".to_string()],
            "cold/takeover open must recover the durable allowlist"
        );
        assert_eq!(successor_queue.durable_epoch().unwrap(), 2);

        drop(successor_queue);
        drop(standby_sink);
        drop(standby);
        let _ = std::fs::remove_dir_all(directory);
    }

    /// REGRESSION — the whole reported chain, end to end.
    ///
    /// The observer reaches `observation_access` every `RECONCILE_INTERVAL`
    /// whatever the allowlist holds, so a durable-state OPEN failure is
    /// reachable on a profile with zero managed sessions within 250 ms of
    /// launch. It used to latch `DurableStateUnavailable`, and a latched fault
    /// used to be refused a commit permit — so an ordinary unwritable or
    /// non-POSIX state root (SMB/NFS, exFAT, a Docker bind mount, a `chmod`ped
    /// directory) permanently vetoed cold exec, Windows spawn-and-exit AND
    /// seamless Commit: a terminal that could never update itself again, on
    /// every launch, with nothing naming the operator as the cause.
    ///
    /// Now: standby, then a clean self-disable, and every replacement seam is
    /// left strictly alone.
    #[test]
    fn an_unopenable_state_root_disables_the_operator_without_touching_self_update() {
        let directory = operator_test_directory("roster-open-failure");
        let _ = std::fs::remove_file(&directory);
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::write(&directory, b"not a directory").unwrap();
        let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
        let control = ControlHandle::new_with_directory(
            "profile-default".to_string(),
            Some(directory.clone()),
            notify_tx,
        );
        let store = crate::session_store::new_store();
        let sink = DurableSink {
            control: control.clone(),
            store: store.clone(),
        };

        // Every attempt inside the budget is silent standby, and none of them
        // latches anything. (The clock nudge only skips the retry backoff a
        // real 250 ms observer would have waited out.)
        for attempt in 1..OPEN_FAILURE_BUDGET {
            assert!(
                matches!(sink.observation_access(), ObservationAccess::Standby),
                "attempt {attempt} must be standby, not a fault"
            );
            assert!(
                control.fleet_fault().is_none(),
                "attempt {attempt} latched a fleet fault on an open failure"
            );
            control.shared.slot.lock().unwrap().retry_after = Some(Instant::now());
        }

        // The budget runs out: the operator turns itself OFF. Not faulted —
        // dormancy revokes nothing, needs no `clear-fault confirm=human` (which
        // would itself need the queue that cannot open), and the next launch
        // starts over.
        let access = sink.observation_access();
        assert!(
            matches!(access, ObservationAccess::Disabled(_)),
            "budget exhausted must disable"
        );
        assert!(
            control.fleet_fault().is_none(),
            "dormancy must not be a fleet fault"
        );
        let Err(disabled) = control.queue() else {
            panic!("a dormant operator must not open a queue");
        };
        assert!(
            disabled.contains("embedded operator disabled for this process"),
            "{disabled}"
        );

        // The observer thread ends here instead of retrying a filesystem that
        // has already answered.
        let mut classifier = Classifier::default();
        let mut pending = CoalescingQueue::with_capacity(4);
        let mut watched = Vec::new();
        let mut subscription = None;
        let mut last_sink_warning = None;
        assert_eq!(
            run_observation_cycle(
                &store,
                &crate::subscribe::new_registry(),
                &sink,
                false,
                &mut classifier,
                &mut pending,
                &mut watched,
                &mut subscription,
                &mut last_sink_warning,
            ),
            ObserverCycleOutcome::Stop
        );

        // THE POINT: this terminal can still replace itself. All three seams
        // take their permit through `with_commit_permit`.
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let quiesce = control.try_begin_update_quiesce().unwrap();
        assert_eq!(
            quiesce
                .with_commit_permit(|| calls.fetch_add(1, Ordering::SeqCst))
                .unwrap(),
            0
        );
        drop(quiesce);

        // And a fault that IS real (durable state opened, then failed a
        // durability or integrity check) fences operator authority without
        // vetoing replacement either.
        control.latch_fleet_fault(FleetFaultReason::ActuatorIntegrity);
        assert!(control.ensure_accepting_new_work().is_err());
        let quiesce = control.try_begin_update_quiesce().unwrap();
        assert_eq!(
            quiesce
                .with_commit_permit(|| calls.fetch_add(1, Ordering::SeqCst))
                .unwrap(),
            1
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        drop(quiesce);

        drop(sink);
        drop(control);
        let _ = std::fs::remove_file(directory);
    }

    fn enqueue_operator_test_event(queue: &DurableQueue, sid: &str, seq: u64) -> EventId {
        let evidence = format!("ready event {seq}");
        let fingerprint: [u8; 32] = Sha256::digest(evidence.as_bytes()).into();
        let EnqueueOutcome::Enqueued(event_id) = queue
            .enqueue(NewEvent::new(
                sid,
                EventGeneration::new(1, false, seq, fingerprint),
                AttentionCondition::Ready,
                evidence,
            ))
            .unwrap()
        else {
            panic!("fresh test event must enqueue");
        };
        event_id
    }

    #[test]
    fn durable_maintenance_surfaces_cap_conversion_and_escalation_expiry_once() {
        let directory = operator_test_directory("maintenance");
        let _ = std::fs::remove_dir_all(&directory);
        let queue = operator_test_queue(&directory, 4, 1);
        queue.manage_sid("s-a").unwrap();
        let event_id = enqueue_operator_test_event(&queue, "s-a", 1);
        let first = queue.claim_at(0).unwrap().unwrap();

        let (notify_tx, notify_rx) = std::sync::mpsc::sync_channel(8);
        let control = ControlHandle::new("test-maintenance".to_string(), notify_tx);
        {
            let mut slot = control.shared.slot.lock().unwrap();
            slot.queue = Some(queue.clone());
            slot.retry_after = None;
        }
        let sink = DurableSink {
            control: control.clone(),
            store: crate::session_store::new_store(),
        };

        assert!(first.expires_at_ms <= 1);
        // The queue's authority clock counts MONOTONIC milliseconds from its
        // own origin, so "expired" here means "at least 2 ms of real time have
        // passed since the queue was built". A machine fast enough to reach
        // this line inside one millisecond reclaims nothing and waits out the
        // recv below for a notice that was never owed — a race, not a defect.
        // Cross the boundary explicitly rather than betting on being slow.
        std::thread::sleep(Duration::from_millis(5));
        sink.maintenance();
        let first_notice = notify_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(first_notice.session, u64::MAX);
        assert!(first_notice.body.contains("human attention"));
        let escalated = queue.snapshot(event_id).unwrap();
        assert!(escalated.escalated);
        assert_eq!(escalated.condition, AttentionCondition::Escalation);
        assert!(matches!(escalated.status, EventStatus::Queued));

        let final_claim = queue.claim_at(0).unwrap().unwrap();
        assert_eq!(final_claim.event.id, event_id);
        sink.maintenance();
        let second_notice = notify_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(second_notice.session, u64::MAX);
        assert!(matches!(
            queue.snapshot(event_id).unwrap().status,
            EventStatus::InDoubt { .. }
        ));

        sink.maintenance();
        assert!(
            notify_rx.try_recv().is_err(),
            "each expiry transition surfaces once"
        );

        drop(sink);
        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn next_surfaces_a_cap_converted_escalation_before_returning_it() {
        let directory = operator_test_directory("next-escalation");
        let _ = std::fs::remove_dir_all(&directory);
        let queue = operator_test_queue(&directory, 4, 1);
        queue.manage_sid("s-a").unwrap();
        let event_id = enqueue_operator_test_event(&queue, "s-a", 1);
        let first = queue.claim_at(0).unwrap().unwrap();
        assert_eq!(first.event.id, event_id);

        let (notify_tx, notify_rx) = std::sync::mpsc::sync_channel(8);
        let control = ControlHandle::new("test-next-escalation".to_string(), notify_tx);
        {
            let mut slot = control.shared.slot.lock().unwrap();
            slot.queue = Some(queue.clone());
            slot.retry_after = None;
        }
        std::thread::sleep(Duration::from_millis(5));
        let store = crate::session_store::new_store();
        let escalation = control
            .wait_claim(&store, Duration::ZERO)
            .unwrap()
            .expect("expired cap conversion must be claimable");
        assert_eq!(escalation.event.id, event_id);
        assert!(escalation.event.escalated);
        assert_eq!(escalation.event.condition, AttentionCondition::Escalation);
        let notice = notify_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("next must surface the escalation before returning it");
        assert_eq!(notice.session, u64::MAX);
        assert!(notice.body.contains("human attention"));

        control.surface_escalation_once(&store, &escalation.event);
        assert!(
            notify_rx.try_recv().is_err(),
            "maintenance/next discovery of one escalation must not duplicate it"
        );

        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn durable_queue_full_latches_one_loud_fleet_fault() {
        let directory = operator_test_directory("queue-full");
        let _ = std::fs::remove_dir_all(&directory);
        let queue = operator_test_queue(&directory, 1, 3);
        queue.manage_sid("s-a").unwrap();
        enqueue_operator_test_event(&queue, "s-a", 1);

        let (notify_tx, notify_rx) = std::sync::mpsc::sync_channel(8);
        let control = ControlHandle::new("test-full".to_string(), notify_tx);
        {
            let mut slot = control.shared.slot.lock().unwrap();
            slot.queue = Some(queue.clone());
            slot.retry_after = None;
        }
        let sink = DurableSink {
            control: control.clone(),
            store: crate::session_store::new_store(),
        };
        let candidate = Candidate {
            sid: "s-a".to_string(),
            local_id: 7,
            generation: Generation {
                lifecycle_epoch: 0,
                alternate_screen: false,
                content_seq: 2,
                fingerprint: Sha256::digest(b"second ready event").into(),
            },
            kind: CandidateKind::BusyBecameAttention,
            screen_tail: "second ready event".to_string(),
        };

        let error = sink.enqueue(candidate.clone()).unwrap_err();
        assert!(error.contains("queue is full"));
        let notice = notify_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(
            notice.session,
            u64::MAX,
            "fleet faults bypass focus suppression"
        );
        assert!(notice.body.contains("stopped accepting events"));
        let status = control.command_status().unwrap();
        assert!(status.contains("\"state\":\"faulted\""), "{status}");
        assert!(status.contains("observer-overflow"), "{status}");
        assert!(!status.contains("second ready event"), "{status}");

        assert!(sink.enqueue(candidate).is_err());
        assert!(
            notify_rx.try_recv().is_err(),
            "a sticky fault must notify only on its first transition"
        );

        drop(sink);
        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn operator_notice_retries_after_shared_delivery_queue_backpressure() {
        let (notify_tx, notify_rx) = std::sync::mpsc::sync_channel(1);
        assert!(
            notify_tx
                .try_send(crate::notify::NotifyMsg {
                    session: 1,
                    title: None,
                    body: "occupied".to_string(),
                })
                .is_ok()
        );
        let control = ControlHandle::new("test-notice-retry".to_string(), notify_tx);
        control.surface_notice(7, "A managed session requires human attention.");
        assert!(
            control.shared.pending_notice.lock().unwrap().is_some(),
            "a full shared notification queue must not silently drop the alert"
        );

        assert_eq!(notify_rx.recv().unwrap().body, "occupied");
        control.flush_pending_notice();
        let retried = notify_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(retried.session, 7);
        assert_eq!(retried.title.as_deref(), Some("aterm operator"));
        assert!(retried.body.contains("human attention"));
        assert!(control.shared.pending_notice.lock().unwrap().is_none());
    }

    #[test]
    fn fleet_fault_blocks_new_claims_and_manage_but_allows_cleanup() {
        let directory = operator_test_directory("fault-gates");
        let _ = std::fs::remove_dir_all(&directory);
        let config = QueueConfig {
            capacity: 4,
            visibility_timeout: Duration::from_secs(120),
            max_cumulative_extension: Duration::from_secs(600),
            redelivery_cap: 3,
            max_wal_bytes: 2 * 1024 * 1024,
        };
        let queue = DurableQueue::open(&directory, 1, config).unwrap();
        queue.manage_sid("s-a").unwrap();
        queue.manage_sid("s-b").unwrap();
        let first_id = enqueue_operator_test_event(&queue, "s-a", 1);
        let second_id = enqueue_operator_test_event(&queue, "s-b", 2);
        let first = queue.claim().unwrap().unwrap();
        assert_eq!(first.event.id, first_id);

        let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
        let control = ControlHandle::new("test-fault-gates".to_string(), notify_tx);
        {
            let mut slot = control.shared.slot.lock().unwrap();
            slot.queue = Some(queue.clone());
            slot.retry_after = None;
        }
        let store = crate::session_store::new_store();
        let sink = DurableSink {
            control: control.clone(),
            store: store.clone(),
        };
        queue
            .latch_fault(FleetFaultReason::ObserverOverflow)
            .unwrap();
        assert!(control.fleet_fault().is_none());
        assert!(matches!(
            sink.observation_access(),
            ObservationAccess::Standby
        ));
        control.latch_fleet_fault(FleetFaultReason::ObserverOverflow);

        let next = control.command(&store, "next timeout=0");
        assert!(next.contains("fleet faulted"), "{next}");
        assert!(matches!(
            queue.snapshot(second_id).unwrap().status,
            EventStatus::Queued
        ));
        let manage = control.command(&store, "manage s-new");
        assert!(manage.contains("fleet faulted"), "{manage}");
        assert!(!queue.is_managed("s-new").unwrap());

        let ack = control.command(
            &store,
            &format!("ack {} {} pause", first_id.get(), first.token.expose()),
        );
        assert_eq!(ack, "OK resolved\n");
        let unmanage = control.command(&store, "unmanage s-b");
        assert!(unmanage.starts_with("OK managed=false"), "{unmanage}");
        let status = control.command(&store, "status");
        assert!(status.contains("\"state\":\"faulted\""), "{status}");

        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn final_actuation_permit_rechecks_fault_management_and_exact_intent() {
        let assert_authority_denial_model = || {
            let model = operator_wal_actuator_model();
            let initial = model.init_state();
            let mut intent = initial.clone();
            intent.insert("phase", 1);
            intent.insert("intent_durable", 1);
            let (accepted, diagnostics) = verify::validate_transition_tiered(
                &model,
                &[("Buggy", 0)],
                &initial,
                &intent,
                Some("PersistIntent"),
                "host permit durable intent",
            );
            assert!(accepted, "PersistIntent rejected\n{diagnostics}");

            let mut invalid = intent.clone();
            invalid.insert("authority_valid", 0);
            invalid.insert("authority_invalidated", 1);
            let (accepted, diagnostics) = verify::validate_transition_tiered(
                &model,
                &[("Buggy", 0)],
                &intent,
                &invalid,
                Some("InvalidateAuthority"),
                "host final authority invalidation",
            );
            assert!(accepted, "InvalidateAuthority rejected\n{diagnostics}");

            let mut rejected = invalid.clone();
            rejected.insert("phase", 3);
            rejected.insert("in_doubt", 1);
            let (accepted, diagnostics) = verify::validate_transition_tiered(
                &model,
                &[("Buggy", 0)],
                &invalid,
                &rejected,
                Some("RejectInvalidAuthority"),
                "host zero-byte authority rejection",
            );
            assert!(accepted, "RejectInvalidAuthority rejected\n{diagnostics}");

            // Non-vacuous negative control: the same invalidated state may not
            // take the model's real paste transition.
            let mut forged_write = invalid.clone();
            forged_write.insert("phase", 2);
            forged_write.insert("mutations", 1);
            forged_write.insert("input_epoch", 1);
            forged_write.insert("expected_epoch", 1);
            let (accepted, diagnostics) = verify::validate_transition_tiered(
                &model,
                &[("Buggy", 0)],
                &invalid,
                &forged_write,
                Some("MutateOnce"),
                "host invalid-authority egress negative control",
            );
            assert!(
                !accepted,
                "invalid authority admitted a forged write\n{diagnostics}"
            );
        };

        fn prepared(
            label: &str,
        ) -> (
            std::path::PathBuf,
            DurableQueue,
            ControlHandle,
            EventId,
            ClaimToken,
            String,
        ) {
            let directory = operator_test_directory(label);
            let _ = std::fs::remove_dir_all(&directory);
            let queue = operator_test_queue(&directory, 4, 3);
            queue.manage_sid("s-a").unwrap();
            let event_id = enqueue_operator_test_event(&queue, "s-a", 1);
            let claim = queue.claim_at(0).unwrap().unwrap();
            let action_hash = "c".repeat(64);
            queue
                .begin_action_at(event_id, &claim.token, "turn", &action_hash, 0)
                .unwrap();
            let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
            let control = ControlHandle::new(format!("test-{label}"), notify_tx);
            {
                let mut slot = control.shared.slot.lock().unwrap();
                slot.queue = Some(queue.clone());
                slot.retry_after = None;
            }
            (
                directory,
                queue,
                control,
                event_id,
                claim.token,
                action_hash,
            )
        }

        let (directory, queue, control, event_id, token, action_hash) = prepared("permit-unmanage");
        // This stands at the exact race seam: earlier validation succeeded, then
        // unmanage wins the shared gate before final egress asks for its permit.
        assert!(control.ensure_accepting_new_work().is_ok());
        let store = crate::session_store::new_store();
        assert!(
            control
                .command(&store, "unmanage s-a")
                .starts_with("OK managed=false")
        );
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let denied =
            control.with_actuation_permit(&queue, event_id, &token, "s-a", &action_hash, || {
                calls.fetch_add(1, Ordering::SeqCst)
            });
        assert!(denied.unwrap_err().contains("authority was revoked"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_authority_denial_model();
        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);

        let (directory, queue, control, event_id, token, action_hash) = prepared("permit-fault");
        assert!(control.ensure_accepting_new_work().is_ok());
        control.latch_fleet_fault(FleetFaultReason::ActuatorIntegrity);
        assert_eq!(
            queue.fleet_fault().unwrap().map(|fault| fault.reason),
            Some(FleetFaultReason::ActuatorIntegrity),
            "host fault did not reach the durable fleet gate"
        );
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let denied =
            control.with_actuation_permit(&queue, event_id, &token, "s-a", &action_hash, || {
                calls.fetch_add(1, Ordering::SeqCst)
            });
        assert!(denied.unwrap_err().contains("fleet faulted"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);

        let (directory, queue, control, event_id, token, action_hash) = prepared("permit-shutdown");
        assert!(control.ensure_accepting_new_work().is_ok());
        control.begin_shutdown();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let denied =
            control.with_actuation_permit(&queue, event_id, &token, "s-a", &action_hash, || {
                calls.fetch_add(1, Ordering::SeqCst)
            });
        assert!(denied.unwrap_err().contains("shutting down"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(control.ensure_accepting_new_work().is_err());
        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);

        let (directory, queue, control, event_id, token, action_hash) =
            prepared("permit-contended");
        let gate = control.shared.fleet_fault.lock().unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let started = Instant::now();
        let denied =
            control.with_actuation_permit(&queue, event_id, &token, "s-a", &action_hash, || {
                calls.fetch_add(1, Ordering::SeqCst)
            });
        assert!(denied.unwrap_err().contains("transition is busy"));
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(gate);
        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn update_quiesce_is_reversible_and_a_late_fault_fences_authority_not_replacement() {
        fn prepared(
            label: &str,
        ) -> (
            std::path::PathBuf,
            DurableQueue,
            ControlHandle,
            EventId,
            ClaimToken,
            String,
        ) {
            let directory = operator_test_directory(label);
            let _ = std::fs::remove_dir_all(&directory);
            let queue = operator_test_queue(&directory, 4, 3);
            queue.manage_sid("s-a").unwrap();
            let event_id = enqueue_operator_test_event(&queue, "s-a", 1);
            let claim = queue.claim_at(0).unwrap().unwrap();
            let action_hash = "d".repeat(64);
            queue
                .begin_action_at(event_id, &claim.token, "turn", &action_hash, 0)
                .unwrap();
            let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
            let control = ControlHandle::new(format!("test-{label}"), notify_tx);
            {
                let mut slot = control.shared.slot.lock().unwrap();
                slot.queue = Some(queue.clone());
                slot.retry_after = None;
            }
            (
                directory,
                queue,
                control,
                event_id,
                claim.token,
                action_hash,
            )
        }

        let (directory, queue, control, event_id, token, action_hash) =
            prepared("quiesce-rollback");
        let action_activity = control.begin_action_activity().unwrap();
        assert!(
            control
                .try_begin_update_quiesce()
                .err()
                .unwrap()
                .contains("transaction is in flight")
        );
        drop(action_activity);
        let quiesce = control.try_begin_update_quiesce().unwrap();
        assert!(control.ensure_accepting_new_work().is_err());
        assert!(
            control
                .command_status()
                .unwrap()
                .contains("quiesced-for-update")
        );
        let calls = std::sync::atomic::AtomicUsize::new(0);
        assert!(
            control
                .with_actuation_permit(&queue, event_id, &token, "s-a", &action_hash, || calls
                    .fetch_add(1, Ordering::SeqCst),)
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let store = crate::session_store::new_store();
        assert!(control.command(&store, "unmanage s-a").contains("quiesced"));
        assert!(queue.is_managed("s-a").unwrap());

        // A failed replacement returns through Drop and re-enables the exact
        // same process without clearing durable/operator state.
        assert_eq!(quiesce.with_commit_permit(|| 7).unwrap(), 7);
        assert!(control.ensure_accepting_new_work().is_err());
        drop(quiesce);
        assert!(control.ensure_accepting_new_work().is_ok());
        assert!(
            control
                .with_actuation_permit(&queue, event_id, &token, "s-a", &action_hash, || calls
                    .fetch_add(1, Ordering::SeqCst),)
                .is_ok()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);

        // A fault latched while the replacement token is live fences operator
        // authority — and lets the replacement through. The fault is durable
        // before it is visible here, so the successor reopens fail-closed; a
        // refusal would only have cost the user their update.
        let (directory, queue, control, _event_id, _token, _action_hash) =
            prepared("quiesce-late-fault");
        let quiesce = control.try_begin_update_quiesce().unwrap();
        control.latch_fleet_fault(FleetFaultReason::ActuatorIntegrity);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        assert_eq!(
            quiesce
                .with_commit_permit(|| calls.fetch_add(1, Ordering::SeqCst))
                .unwrap(),
            0
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(quiesce);
        assert!(control.ensure_accepting_new_work().is_err());
        assert_eq!(
            queue.fleet_fault().unwrap().map(|fault| fault.reason),
            Some(FleetFaultReason::ActuatorIntegrity)
        );
        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn in_doubt_is_inspectable_and_requires_explicit_human_reconciliation() {
        let directory = operator_test_directory("human-reconcile");
        let _ = std::fs::remove_dir_all(&directory);
        let queue = operator_test_queue(&directory, 4, 3);
        queue.manage_sid("s-a").unwrap();
        let event_id = enqueue_operator_test_event(&queue, "s-a", 1);
        let claim = queue.claim_at(0).unwrap().unwrap();
        let action_hash = "a".repeat(64);
        queue
            .begin_action_at(event_id, &claim.token, "turn", &action_hash, 0)
            .unwrap();
        queue
            .mark_action_in_doubt_at(event_id, &claim.token, "ambiguous submit", 1)
            .unwrap();

        let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
        let control = ControlHandle::new("test-human-reconcile".to_string(), notify_tx);
        {
            let mut slot = control.shared.slot.lock().unwrap();
            slot.queue = Some(queue.clone());
            slot.retry_after = None;
        }
        let store = crate::session_store::new_store();
        let status = control.command(&store, "status");
        assert!(
            status.contains(&format!("\"in_doubt_event_ids\":[{}]", event_id.get())),
            "{status}"
        );
        let inspection = control.command(&store, &format!("inspect {}", event_id.get()));
        assert!(
            inspection.contains("\"status\":\"in-doubt\""),
            "{inspection}"
        );
        assert!(inspection.contains(claim.token.expose()), "{inspection}");
        assert!(inspection.contains("ambiguous submit"), "{inspection}");

        let missing_confirmation = control.command(
            &store,
            &format!(
                "reconcile {} {} acted",
                event_id.get(),
                claim.token.expose()
            ),
        );
        assert!(
            missing_confirmation.starts_with("ERR usage:"),
            "{missing_confirmation}"
        );
        assert!(matches!(
            queue.snapshot(event_id).unwrap().status,
            EventStatus::InDoubt { .. }
        ));

        let reconciled = control.command(
            &store,
            &format!(
                "reconcile {} {} acted confirm=human",
                event_id.get(),
                claim.token.expose()
            ),
        );
        assert_eq!(reconciled, "OK resolved\n");
        assert!(matches!(
            queue.snapshot(event_id).unwrap().status,
            EventStatus::Resolved {
                resolution: Resolution::Acted,
                reconciliation_note: Some(_),
                ..
            }
        ));

        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn maintenance_queue_error_latches_fleet_fault() {
        let directory = operator_test_directory("maintenance-fault");
        let _ = std::fs::remove_dir_all(&directory);
        let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
        let control = ControlHandle::new_with_directory(
            "test-maintenance-error".to_string(),
            Some(directory.clone()),
            notify_tx,
        );
        let sink = DurableSink {
            control: control.clone(),
            store: crate::session_store::new_store(),
        };
        let result = sink.maintenance_queue_result::<()>(
            "injected operation",
            Err(OperatorError::InvalidInput("injected failure".to_string())),
        );
        assert!(result.is_none());
        let fault = control.ensure_accepting_new_work().unwrap_err();
        assert!(fault.contains("durable-state-unavailable"), "{fault}");
        assert!(!fault.contains("injected failure"), "{fault}");
        drop(sink);
        drop(control);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn observer_panic_latches_fault_before_cycle_activity_drops() {
        struct PanicSink {
            control: ControlHandle,
            queue: DurableQueue,
            fault: Arc<Mutex<Option<(FleetFaultReason, bool)>>>,
        }

        impl EventSink for PanicSink {
            fn enqueue(&self, _candidate: Candidate) -> Result<(), String> {
                Ok(())
            }

            fn begin_observation_cycle(
                &self,
            ) -> Result<Option<Box<dyn ObservationCycleGuard>>, String> {
                self.control
                    .begin_observer_mutation()
                    .map(|activity| Some(Box::new(activity) as Box<dyn ObservationCycleGuard>))
            }

            fn maintenance(&self) {
                panic!("injected observer panic");
            }

            fn fault(&self, reason: FleetFaultReason) {
                let active = self
                    .control
                    .shared
                    .fleet_fault
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .active_observer_mutations
                    != 0;
                self.control.latch_fleet_fault(reason);
                *self.fault.lock().unwrap() = Some((reason, active));
            }

            fn fault_marker_without_live(&self, reason: FleetFaultReason) -> Result<(), String> {
                self.queue
                    .latch_fault_marker_without_live(reason)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
        }

        let directory = operator_test_directory("panic-cycle-fence");
        let _ = std::fs::remove_dir_all(&directory);
        let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
        let control = ControlHandle::new_with_directory(
            "test-panic-cycle-fence".to_string(),
            Some(directory.clone()),
            notify_tx,
        );
        let queue = control.queue().unwrap();
        let fault = Arc::new(Mutex::new(None));
        let sink: Arc<dyn EventSink> = Arc::new(PanicSink {
            control: control.clone(),
            queue: queue.clone(),
            fault: Arc::clone(&fault),
        });
        let mut runtime = start(
            crate::session_store::new_store(),
            crate::subscribe::new_registry(),
            Arc::clone(&sink),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if fault.lock().unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "panic fault was not latched");
            std::thread::sleep(Duration::from_millis(2));
        }
        runtime.shutdown_and_join();
        assert_eq!(
            *fault.lock().unwrap(),
            Some((FleetFaultReason::ObserverPanicked, true)),
            "panic fault publication must precede cycle-activity decrement"
        );
        assert_eq!(
            queue.fleet_fault().unwrap().map(|fault| fault.reason),
            Some(FleetFaultReason::ObserverPanicked)
        );
        drop(runtime);
        drop(sink);
        drop(control);
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }
}
