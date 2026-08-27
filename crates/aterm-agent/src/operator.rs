// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable state machine for an embedded fleet operator.
//!
//! The queue deliberately keeps the authority-bearing transition and its WAL
//! write under one mutex. Every mutating operation validates its compare-and-set
//! precondition, appends and synchronizes one checksummed record, and only then
//! changes memory. A failed or uncertain write poisons the live handle; reopening
//! replays the durable answer instead of guessing whether an operation landed.
//!
//! Invalid/stale claim attempts are deliberately not one-record-per-request audit
//! events: the authenticated requester controls that input, so such a WAL would be
//! an unbounded durable-storage amplification primitive. The durable event state
//! and successful reclaim/CAS transition remain authoritative; a bounded external
//! security log may record rejected requests. Adding an in-WAL rejection audit
//! requires a separately specified, coalesced quota and is intentionally deferred.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aterm_digest::Sha256;

/// Default claim visibility window.
pub const DEFAULT_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum cumulative extension of one claim.
pub const DEFAULT_MAX_CUMULATIVE_EXTENSION: Duration = Duration::from_secs(10 * 60);
/// Number of expired deliveries after which an event is converted to escalation.
pub const DEFAULT_REDELIVERY_CAP: u32 = 3;
/// Default number of unresolved events admitted at once.
pub const DEFAULT_QUEUE_CAPACITY: usize = 1024;
/// Fail-closed WAL ceiling between atomic durable checkpoints.
pub const DEFAULT_MAX_WAL_BYTES: u64 = 64 * 1024 * 1024;

const WAL_MAGIC: [u8; 4] = *b"AOPW";
const WAL_SCHEMA: u16 = 1;
const MAX_WAL_RECORD_KIND: u16 = 20;
const WAL_HEADER_LEN: usize = 32;
const WAL_CHECKSUM_LEN: usize = 32;
const WAL_FRAME_OVERHEAD: usize = WAL_HEADER_LEN + WAL_CHECKSUM_LEN;
const MAX_WAL_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_SID_BYTES: usize = 256;
const MAX_EVIDENCE_BYTES: usize = 16 * 1024;
const MAX_REASON_BYTES: usize = 4096;
const MAX_ACTION_CLASS_BYTES: usize = 128;
const MAX_ACTION_RESULT_BYTES: usize = 4096;
const CLAIM_TOKEN_BYTES: usize = 32;
const CLAIM_TOKEN_HEX_LEN: usize = CLAIM_TOKEN_BYTES * 2;
const ACTION_HASH_BYTES: usize = 32;
const ACTION_HASH_HEX_LEN: usize = ACTION_HASH_BYTES * 2;
const WAL_FILE_NAME: &str = "operator.wal";
const LOCK_FILE_NAME: &str = "operator.lock";
const CHECKPOINT_MAGIC: [u8; 4] = *b"AOPC";
const LEGACY_CHECKPOINT_SCHEMA: u16 = 1;
const CHECKPOINT_SCHEMA: u16 = 2;
const CHECKPOINT_HEADER_LEN: usize = 32;
const CHECKPOINT_CHECKSUM_LEN: usize = 32;
const CHECKPOINT_PREFIX: &str = "operator.checkpoint.";
const CHECKPOINT_PENDING_NAME: &str = "operator.checkpoint.pending";
const FAULT_MARKER_NAME: &str = "operator.fault";
const FAULT_MARKER_MAGIC: [u8; 4] = *b"AOPF";
const FAULT_MARKER_SCHEMA: u16 = 1;
const FAULT_MARKER_LEN: usize = 8;
const MAX_CHECKPOINT_BYTES: usize = 32 * 1024 * 1024;
const RESOLVED_RETENTION: usize = 256;
const MAX_MANAGED_SIDS: usize = 4096;

/// Stable identifier assigned to an attention event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventId(u64);

impl EventId {
    /// Reconstruct a checked identifier received over authenticated IPC.
    pub fn from_wire(value: u64) -> Result<Self, OperatorError> {
        if value == 0 {
            return Err(OperatorError::InvalidInput(
                "event id zero is reserved".into(),
            ));
        }
        Ok(Self(value))
    }

    /// Numeric identifier for protocol or persistence adapters.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::str::FromStr for EventId {
    type Err = OperatorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parsed = value.parse::<u64>().map_err(|error| {
            OperatorError::InvalidInput(format!("event id is not an unsigned integer: {error}"))
        })?;
        Self::from_wire(parsed)
    }
}

/// Opaque, CSPRNG-minted identity for one delivery claim.
///
/// `Debug` intentionally redacts the value. IPC adapters may deliberately use
/// [`ClaimToken::expose`] when putting it on their authenticated wire.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ClaimToken(String);

impl ClaimToken {
    fn mint() -> Result<Self, OperatorError> {
        aterm_uds::rand::hex_token::<CLAIM_TOKEN_BYTES>()
            .map(Self)
            .map_err(OperatorError::Entropy)
    }

    fn from_wal(value: String) -> Result<Self, String> {
        if value.len() != CLAIM_TOKEN_HEX_LEN
            || !value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("claim token is not the canonical 32-byte lowercase-hex shape".into());
        }
        if value.as_bytes().iter().any(u8::is_ascii_uppercase) {
            return Err("claim token contains uppercase hex".into());
        }
        Ok(Self(value))
    }

    /// Reveal the token for an authenticated IPC request.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Parse the canonical lowercase-hex form received over authenticated IPC.
    pub fn from_wire(value: &str) -> Result<Self, OperatorError> {
        Self::from_wal(value.to_string()).map_err(OperatorError::InvalidInput)
    }
}

impl std::str::FromStr for ClaimToken {
    type Err = OperatorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_wire(value)
    }
}

impl fmt::Debug for ClaimToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ClaimToken(<redacted>)")
    }
}

/// Severity used by strongest-unresolved-condition coalescing.
///
/// Declaration order is the strength order. Coalescing can only move upward.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttentionCondition {
    /// A generic transition worth classification.
    Changed,
    /// The target appears ready for another turn.
    Ready,
    /// The target exceeded an expectation-specific progress deadline.
    SuspectedStuck,
    /// The target is waiting for human approval.
    ApprovalRequired,
    /// The target process or session exited.
    SessionExited,
    /// Autonomy stopped and a human must reconcile the event.
    Escalation,
}

impl AttentionCondition {
    const fn to_tag(self) -> u8 {
        match self {
            Self::Changed => 0,
            Self::Ready => 1,
            Self::SuspectedStuck => 2,
            Self::ApprovalRequired => 3,
            Self::SessionExited => 4,
            Self::Escalation => 5,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, String> {
        match tag {
            0 => Ok(Self::Changed),
            1 => Ok(Self::Ready),
            2 => Ok(Self::SuspectedStuck),
            3 => Ok(Self::ApprovalRequired),
            4 => Ok(Self::SessionExited),
            5 => Ok(Self::Escalation),
            _ => Err(format!("unknown attention-condition tag {tag}")),
        }
    }

    const fn is_safe_manage_baseline(self) -> bool {
        matches!(
            self,
            Self::Changed | Self::ApprovalRequired | Self::SessionExited
        )
    }
}

/// Durable terminal resolution supplied by a claimant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Resolution {
    Acted,
    NoAction,
    Paused,
    Escalated,
}

impl Resolution {
    const fn to_tag(self) -> u8 {
        match self {
            Self::Acted => 0,
            Self::NoAction => 1,
            Self::Paused => 2,
            Self::Escalated => 3,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, String> {
        match tag {
            0 => Ok(Self::Acted),
            1 => Ok(Self::NoAction),
            2 => Ok(Self::Paused),
            3 => Ok(Self::Escalated),
            _ => Err(format!("unknown resolution tag {tag}")),
        }
    }
}

/// Exact generation identity of the observed screen/lifecycle state.
///
/// Sequence numbers are meaningful only with their lifecycle and grid identity.
/// The fingerprint binds the generation to the bounded evidence snapshot that an
/// actuator must re-read and compare immediately before acting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventGeneration {
    pub lifecycle_epoch: u64,
    pub alternate_screen: bool,
    pub content_seq: u64,
    pub fingerprint: [u8; 32],
}

impl EventGeneration {
    /// Construct an exact generation identity.
    #[must_use]
    pub const fn new(
        lifecycle_epoch: u64,
        alternate_screen: bool,
        content_seq: u64,
        fingerprint: [u8; 32],
    ) -> Self {
        Self {
            lifecycle_epoch,
            alternate_screen,
            content_seq,
            fingerprint,
        }
    }
}

/// Input for a newly observed per-session transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewEvent {
    pub sid: String,
    pub generation: EventGeneration,
    pub condition: AttentionCondition,
    /// Transient generation-bound evidence used only to validate the fingerprint.
    /// It is never copied into an event snapshot, checkpoint, or WAL record.
    pub evidence: String,
}

impl NewEvent {
    /// Construct an event with a supplied generation-bound evidence reference.
    #[must_use]
    pub fn new(
        sid: impl Into<String>,
        generation: EventGeneration,
        condition: AttentionCondition,
        evidence: impl Into<String>,
    ) -> Self {
        Self {
            sid: sid.into(),
            generation,
            condition,
            evidence: evidence.into(),
        }
    }

    fn validate(&self) -> Result<(), OperatorError> {
        validate_text("sid", &self.sid, MAX_SID_BYTES, false)?;
        validate_evidence("evidence", &self.evidence, MAX_EVIDENCE_BYTES, true)?;
        let actual: [u8; 32] = Sha256::digest(self.evidence.as_bytes());
        if actual != self.generation.fingerprint {
            return Err(OperatorError::InvalidInput(
                "event generation fingerprint does not match evidence SHA-256".into(),
            ));
        }
        Ok(())
    }
}

/// Public lifecycle view of an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventStatus {
    Queued,
    Delivered {
        token: ClaimToken,
        claim_epoch: u64,
        claimed_at_ms: u64,
        expires_at_ms: u64,
        cumulative_extension_ms: u64,
    },
    /// A durable action intent exists. The caller may execute it once, but no
    /// retry/recovery path may execute it again. An unmatched recovered intent
    /// is projected to [`EventStatus::InDoubt`].
    ActionInFlight {
        token: ClaimToken,
        claim_epoch: u64,
        action_class: String,
        action_hash: String,
        intent_at_ms: u64,
    },
    Resolved {
        token: ClaimToken,
        claim_epoch: u64,
        resolution: Resolution,
        resolved_at_ms: u64,
        /// Present only when a human explicitly reconciled an in-doubt action.
        /// In particular, `Acted` here is an audit assertion by that human, not
        /// permission for an autonomous retry.
        reconciliation_note: Option<String>,
    },
    /// A queued/delivered event revoked before any actuator intent existed.
    /// It has no claimant token because no ambiguous side effect needs human
    /// reconciliation.
    ResolvedUnclaimed {
        resolution: Resolution,
        resolved_at_ms: u64,
        reason: String,
    },
    InDoubt {
        token: Option<ClaimToken>,
        reason: String,
        at_ms: u64,
    },
}

/// Immutable event projection safe to hand to an embedded UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventSnapshot {
    pub id: EventId,
    pub sid: String,
    pub generation: EventGeneration,
    pub condition: AttentionCondition,
    pub redelivery_count: u32,
    pub escalated: bool,
    pub status: EventStatus,
}

/// Value returned when the fair queue grants a delivery claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub event: EventSnapshot,
    pub token: ClaimToken,
    /// Opaque milliseconds on this queue handle's process-local authority clock.
    /// It is comparable to other timestamps in the same durable run only.
    pub expires_at_ms: u64,
}

/// Result of inserting or coalescing an observed transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// The observation was outside the explicit managed-session allowlist.
    Unmanaged,
    Enqueued(EventId),
    Coalesced {
        event_id: EventId,
        strengthened: bool,
    },
}

/// Successful acknowledgement result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckOutcome {
    Resolved,
    AlreadyResolved,
}

/// Successful extension result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtensionOutcome {
    pub expires_at_ms: u64,
    pub cumulative_extension_ms: u64,
}

/// One expired-claim transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpiryOutcome {
    pub event_id: EventId,
    /// True when this expiry converted the event to escalation or made an
    /// already-escalated delivery in-doubt.
    pub escalated: bool,
}

/// Redacted, bounded reason for a durable fleet-wide safety stop.
///
/// No terminal text, path, model output, or arbitrary error string can enter the
/// WAL/checkpoint through this type. Detailed diagnostics remain process-local.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetFaultReason {
    ObserverOverflow,
    ObserverPanicked,
    DurableStateUnavailable,
    ActuatorIntegrity,
    /// A present but torn/unknown fail-closed marker recovered after a crash.
    DurabilityUncertain,
}

impl FleetFaultReason {
    const fn to_tag(self) -> u8 {
        match self {
            Self::ObserverOverflow => 1,
            Self::ObserverPanicked => 2,
            Self::DurableStateUnavailable => 3,
            Self::ActuatorIntegrity => 4,
            Self::DurabilityUncertain => 255,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, String> {
        match tag {
            1 => Ok(Self::ObserverOverflow),
            2 => Ok(Self::ObserverPanicked),
            3 => Ok(Self::DurableStateUnavailable),
            4 => Ok(Self::ActuatorIntegrity),
            255 => Ok(Self::DurabilityUncertain),
            _ => Err(format!("unknown fleet-fault reason tag {tag}")),
        }
    }

    /// Stable safe token for authenticated status/audit output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ObserverOverflow => "observer-overflow",
            Self::ObserverPanicked => "observer-panicked",
            Self::DurableStateUnavailable => "durable-state-unavailable",
            Self::ActuatorIntegrity => "actuator-integrity",
            Self::DurabilityUncertain => "durability-uncertain",
        }
    }
}

/// Immutable durable fault identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetFault {
    pub reason: FleetFaultReason,
    pub fault_epoch: u64,
    pub latched_at_ms: u64,
}

/// Durable fleet gate. Only `Healthy` permits claims, normal observation enqueue,
/// management additions, or action intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FleetGateStatus {
    Healthy,
    Faulted(FleetFault),
    RebaselineRequired {
        fault: FleetFault,
        pending_sids: Vec<String>,
    },
}

/// Idempotent result of requesting a fleet fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultLatchOutcome {
    Latched(FleetFault),
    AlreadyLatched(FleetFault),
}

/// Nonparking verdict for the host's last check immediately before terminal
/// egress. `Busy` and `Revoked` both require a zero-byte refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalActionPermit {
    Granted,
    Busy,
    Revoked,
}

/// Durable queue policy.
#[derive(Clone, Debug)]
pub struct QueueConfig {
    pub capacity: usize,
    pub visibility_timeout: Duration,
    pub max_cumulative_extension: Duration,
    pub redelivery_cap: u32,
    pub max_wal_bytes: u64,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_QUEUE_CAPACITY,
            visibility_timeout: DEFAULT_VISIBILITY_TIMEOUT,
            max_cumulative_extension: DEFAULT_MAX_CUMULATIVE_EXTENSION,
            redelivery_cap: DEFAULT_REDELIVERY_CAP,
            max_wal_bytes: DEFAULT_MAX_WAL_BYTES,
        }
    }
}

impl QueueConfig {
    fn validate(&self) -> Result<(), OperatorError> {
        if self.capacity == 0 {
            return Err(OperatorError::InvalidInput(
                "queue capacity must be greater than zero".into(),
            ));
        }
        if self.redelivery_cap == 0 {
            return Err(OperatorError::InvalidInput(
                "redelivery cap must be greater than zero".into(),
            ));
        }
        duration_ms(self.visibility_timeout, "visibility timeout")?;
        duration_ms(
            self.max_cumulative_extension,
            "maximum cumulative extension",
        )?;
        // Epoch payload = u64 epoch + u32 redelivery cap and is the first frame
        // every usable queue must be able to commit.
        let smallest_frame = (WAL_FRAME_OVERHEAD + 12) as u64;
        if self.max_wal_bytes < smallest_frame {
            return Err(OperatorError::InvalidInput(format!(
                "WAL ceiling must be at least {smallest_frame} bytes"
            )));
        }
        Ok(())
    }
}

/// What recovery observed before admitting new mutations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    pub records_replayed: u64,
    pub repaired_partial_final_frame: bool,
    pub durable_epoch: u64,
}

/// Fail-closed operator state or persistence error.
#[derive(Debug)]
pub enum OperatorError {
    Io(io::Error),
    Entropy(io::Error),
    LockContended(PathBuf),
    InvalidInput(String),
    CorruptWal { offset: u64, reason: String },
    UnsupportedSchema { offset: u64, schema: u16 },
    CorruptCheckpoint { path: PathBuf, reason: String },
    UnsupportedCheckpointSchema { path: PathBuf, schema: u16 },
    EpochRegression { requested: u64, durable: u64 },
    WalFull { limit: u64 },
    WalPoisoned,
    StatePoisoned,
    EventNotFound(EventId),
    UnmanagedSid(String),
    QueueFull { capacity: usize },
    FleetFaulted(FleetFaultReason),
    RebaselineRequired { remaining: usize },
    FleetNotFaulted,
    RebaselineSidNotPending(String),
    StaleClaim(EventId),
    ClaimExpired(EventId),
    ExtensionLimit(EventId),
    ResolutionConflict(EventId),
    EventInDoubt(EventId),
    TokenlessInDoubt(EventId),
    ActionInFlight(EventId),
    ActionMismatch(EventId),
    AlreadyResolved(EventId),
    InvariantViolation(String),
}

impl fmt::Display for OperatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "operator I/O: {error}"),
            Self::Entropy(error) => write!(formatter, "cannot mint claim token: {error}"),
            Self::LockContended(path) => write!(
                formatter,
                "another operator holds the WAL lock at {}",
                path.display()
            ),
            Self::InvalidInput(reason) => write!(formatter, "invalid operator input: {reason}"),
            Self::CorruptWal { offset, reason } => {
                write!(formatter, "operator WAL corrupt at byte {offset}: {reason}")
            }
            Self::UnsupportedSchema { offset, schema } => write!(
                formatter,
                "operator WAL schema {schema} at byte {offset} is unsupported"
            ),
            Self::CorruptCheckpoint { path, reason } => write!(
                formatter,
                "operator checkpoint {} is corrupt: {reason}",
                path.display()
            ),
            Self::UnsupportedCheckpointSchema { path, schema } => write!(
                formatter,
                "operator checkpoint {} has unsupported schema {schema}",
                path.display()
            ),
            Self::EpochRegression { requested, durable } => write!(
                formatter,
                "run epoch {requested} does not advance durable epoch {durable}"
            ),
            Self::WalFull { limit } => {
                write!(formatter, "operator WAL reached its {limit}-byte ceiling")
            }
            Self::WalPoisoned => formatter.write_str(
                "operator WAL outcome is uncertain; drop this handle and reopen to reconcile",
            ),
            Self::StatePoisoned => formatter.write_str("operator state mutex is poisoned"),
            Self::EventNotFound(id) => write!(formatter, "event {} does not exist", id.get()),
            Self::UnmanagedSid(sid) => {
                write!(formatter, "session {sid:?} is not in the managed allowlist")
            }
            Self::QueueFull { capacity } => {
                write!(
                    formatter,
                    "operator queue is full ({capacity} unresolved events)"
                )
            }
            Self::FleetFaulted(reason) => write!(
                formatter,
                "operator fleet is durably faulted ({})",
                reason.as_str()
            ),
            Self::RebaselineRequired { remaining } => write!(
                formatter,
                "operator fleet requires durable fresh baselines for {remaining} managed session(s)"
            ),
            Self::FleetNotFaulted => {
                formatter.write_str("operator fleet has no durable fault to clear")
            }
            Self::RebaselineSidNotPending(sid) => write!(
                formatter,
                "session {sid:?} does not require a fault-clear baseline"
            ),
            Self::StaleClaim(id) => write!(formatter, "claim for event {} is stale", id.get()),
            Self::ClaimExpired(id) => write!(formatter, "claim for event {} expired", id.get()),
            Self::ExtensionLimit(id) => write!(
                formatter,
                "claim for event {} reached its cumulative extension limit",
                id.get()
            ),
            Self::ResolutionConflict(id) => write!(
                formatter,
                "event {} received conflicting resolutions and is in-doubt",
                id.get()
            ),
            Self::EventInDoubt(id) => write!(formatter, "event {} is in-doubt", id.get()),
            Self::TokenlessInDoubt(id) => write!(
                formatter,
                "event {} is in-doubt without a claimant token and requires out-of-band recovery",
                id.get()
            ),
            Self::ActionInFlight(id) => {
                write!(
                    formatter,
                    "event {} already has a durable action intent",
                    id.get()
                )
            }
            Self::ActionMismatch(id) => write!(
                formatter,
                "action result does not match the durable intent for event {}",
                id.get()
            ),
            Self::AlreadyResolved(id) => {
                write!(formatter, "event {} is already resolved", id.get())
            }
            Self::InvariantViolation(reason) => {
                write!(formatter, "operator state invariant violated: {reason}")
            }
        }
    }
}

impl std::error::Error for OperatorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) | Self::Entropy(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for OperatorError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Locate aterm's durable per-user state root.
///
/// `ATERM_STATE_HOME` is a test/deployment override. The returned directory is
/// the aterm root itself; callers append `operator/<fleet-id>`.
pub fn default_state_root() -> Result<PathBuf, OperatorError> {
    if let Some(root) = std::env::var_os("ATERM_STATE_HOME") {
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return Err(OperatorError::InvalidInput(
                "ATERM_STATE_HOME must be absolute".into(),
            ));
        }
        return Ok(root);
    }

    #[cfg(target_os = "macos")]
    {
        home_dir()
            .map(|home| home.join("Library/Application Support/aterm"))
            .ok_or_else(|| {
                OperatorError::InvalidInput("HOME is unavailable or not absolute".into())
            })
    }

    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|root| root.is_absolute())
            .map(|root| root.join("aterm"))
            .ok_or_else(|| {
                OperatorError::InvalidInput("LOCALAPPDATA is unavailable or not absolute".into())
            })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                return Err(OperatorError::InvalidInput(
                    "XDG_STATE_HOME must be absolute".into(),
                ));
            }
            return Ok(root.join("aterm"));
        }
        home_dir()
            .map(|home| home.join(".local/state/aterm"))
            .ok_or_else(|| {
                OperatorError::InvalidInput("HOME is unavailable or not absolute".into())
            })
    }

    #[cfg(not(any(unix, windows)))]
    {
        Err(OperatorError::InvalidInput(
            "this platform has no durable state-directory convention".into(),
        ))
    }
}

/// Resolve and create the private directory for one fleet.
///
/// SCOPE DISCIPLINE: the operator owns `<root>/operator/**` and nothing above
/// it. `<root>` is aterm's SHARED per-user state root — the recovery journal,
/// and anything else this app keeps per user, live there too. An opt-in
/// subsystem does not get to rewrite the mode of, or veto its own startup on,
/// state it merely shares: this creates the root when it is missing and
/// otherwise touches nothing about it beyond "it is a directory". That is the
/// pre-existing precedent in `native_document_journal`, which `create_dir_all`s
/// the shared root and hardens only its own `drafts/` subdirectory.
pub fn fleet_state_dir(fleet_id: &str) -> Result<PathBuf, OperatorError> {
    fleet_state_dir_in(&default_state_root()?, fleet_id)
}

/// [`fleet_state_dir`] against an explicit root, so the scope discipline above
/// is testable without mutating this process's environment.
pub fn fleet_state_dir_in(root: &Path, fleet_id: &str) -> Result<PathBuf, OperatorError> {
    validate_fleet_id(fleet_id)?;
    ensure_shared_root(root)?;
    let operator_root = root.join("operator");
    // The anchor of this creation is the shared root, so it is NOT policed for
    // link-likeness; `operator/` and below are ours and are policed in full.
    ensure_private_dir_below_shared_root(&operator_root)?;
    let path = operator_root.join(fleet_id);
    ensure_private_dir(&path)?;
    Ok(path)
}

/// Admit aterm's shared state root without taking ownership of it.
///
/// Deliberately weaker than [`ensure_private_dir`]: no `chmod`, no uid/mode
/// verification, and `metadata` FOLLOWS a link, because a user who points their
/// aterm state root at another volume has not thereby created an operator
/// security boundary — the operator's own directory, one level down, is opened
/// `O_NOFOLLOW` and mode-verified, and that is where every token, claim, and WAL
/// byte lives. Creating a missing root uses the process umask, exactly as the
/// recovery journal already does.
fn ensure_shared_root(root: &Path) -> Result<(), OperatorError> {
    if !root.is_absolute() {
        return Err(OperatorError::InvalidInput(format!(
            "aterm state root {} must be absolute",
            root.display()
        )));
    }
    match fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(OperatorError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a directory", root.display()),
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(root)?;
            Ok(())
        }
        Err(error) => Err(OperatorError::Io(error)),
    }
}

#[cfg(unix)]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[cfg(windows)]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn validate_fleet_id(fleet_id: &str) -> Result<(), OperatorError> {
    validate_text("fleet id", fleet_id, 128, false)?;
    if fleet_id == "."
        || fleet_id == ".."
        || !fleet_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(OperatorError::InvalidInput(
            "fleet id must contain only ASCII letters, digits, '.', '-', or '_'".into(),
        ));
    }
    Ok(())
}

fn validate_text(
    label: &str,
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<(), OperatorError> {
    if (!allow_empty && value.is_empty()) || value.len() > max_bytes {
        return Err(OperatorError::InvalidInput(format!(
            "{label} must be {} and at most {max_bytes} bytes",
            if allow_empty { "valid" } else { "non-empty" }
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(OperatorError::InvalidInput(format!(
            "{label} contains a control character"
        )));
    }
    Ok(())
}

fn validate_evidence(
    label: &str,
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<(), OperatorError> {
    if (!allow_empty && value.is_empty()) || value.len() > max_bytes {
        return Err(OperatorError::InvalidInput(format!(
            "{label} must be {} and at most {max_bytes} bytes",
            if allow_empty { "valid" } else { "non-empty" }
        )));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err(OperatorError::InvalidInput(format!(
            "{label} contains an unsupported control character"
        )));
    }
    Ok(())
}

fn duration_ms(duration: Duration, label: &str) -> Result<u64, OperatorError> {
    let millis = u64::try_from(duration.as_millis()).map_err(|_| {
        OperatorError::InvalidInput(format!("{label} does not fit in milliseconds"))
    })?;
    if millis == 0 {
        return Err(OperatorError::InvalidInput(format!(
            "{label} must be at least one millisecond"
        )));
    }
    Ok(millis)
}

trait TickSource: Send + Sync {
    fn sample_ms(&self) -> Result<u64, OperatorError>;
}

struct InstantSource {
    origin: Instant,
}

impl TickSource for InstantSource {
    fn sample_ms(&self) -> Result<u64, OperatorError> {
        u64::try_from(self.origin.elapsed().as_millis()).map_err(|_| {
            OperatorError::InvalidInput("monotonic process clock exhausted u64 milliseconds".into())
        })
    }
}

/// Process-local authority clock.
///
/// The underlying `Instant` cannot move backwards when the civil clock is
/// corrected. The atomic high-water mark also makes that property explicit for
/// injected/test sources: a faulty rollback can delay neither an expiry nor a
/// previously observed forward jump. Rust deliberately does not promise whether
/// `Instant` includes time spent suspended on every target, so a process that is
/// asleep may grant extra real-world visibility time. Restart/takeover is safe:
/// the durable epoch transition revokes every claim from the previous process.
struct AuthorityClock {
    source: Arc<dyn TickSource>,
    high_water_ms: AtomicU64,
}

impl AuthorityClock {
    fn process_local() -> Self {
        Self {
            source: Arc::new(InstantSource {
                origin: Instant::now(),
            }),
            high_water_ms: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    fn injected(source: Arc<dyn TickSource>) -> Self {
        Self {
            source,
            high_water_ms: AtomicU64::new(0),
        }
    }

    fn now_ms(&self) -> Result<u64, OperatorError> {
        let sampled = self.source.sample_ms()?;
        Ok(self
            .high_water_ms
            .fetch_max(sampled, Ordering::AcqRel)
            .max(sampled))
    }
}

fn metadata_is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    false
}

fn harden_new_directory(path: &Path) -> Result<(), OperatorError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Whether the closest EXISTING ancestor of a path we are creating is ours to
/// police. It is not, when that ancestor is aterm's shared state root: see
/// [`fleet_state_dir`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum AnchorPolicy {
    /// The anchor is inside the operator's own subtree: reject a link-like one.
    Police,
    /// The anchor belongs to the app, not the operator: admit any directory,
    /// link or not, exactly as the lexical ancestors above it are admitted.
    Admit,
}

/// Create missing descendants one component at a time. The closest existing
/// caller-selected ancestor is admitted only if it is a real directory (see
/// [`AnchorPolicy`]); every descendant is then created and rechecked without
/// following a final symlink.
///
/// We intentionally do not reject symlinks above that existing anchor: macOS
/// exposes `/var` and `/tmp` as system aliases. Rejecting every lexical ancestor
/// makes ordinary `temp_dir` and state paths unusable without adding security at
/// the user-controlled boundary.
fn prepare_directory_tree(path: &Path, policy: AnchorPolicy) -> Result<(), OperatorError> {
    if !path.is_absolute() {
        return Err(OperatorError::InvalidInput(format!(
            "operator state directory {} must be absolute",
            path.display()
        )));
    }
    let mut anchor = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(&anchor) {
            Ok(metadata) => {
                let admissible = if metadata_is_link_like(&metadata) {
                    // Not an operator boundary (see `AnchorPolicy::Admit`):
                    // follow it and require only that it lands on a directory,
                    // exactly as every lexical ancestor above it is treated.
                    matches!(policy, AnchorPolicy::Admit)
                        && fs::metadata(&anchor).is_ok_and(|target| target.is_dir())
                } else {
                    metadata.file_type().is_dir()
                };
                if !admissible {
                    return Err(OperatorError::Io(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("{} is not a real directory", anchor.display()),
                    )));
                }
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = anchor.file_name().ok_or_else(|| {
                    OperatorError::InvalidInput("operator state path has no existing root".into())
                })?;
                missing.push(name.to_os_string());
                anchor = anchor
                    .parent()
                    .ok_or_else(|| {
                        OperatorError::InvalidInput("operator state path has no parent".into())
                    })?
                    .to_path_buf();
            }
            Err(error) => return Err(OperatorError::Io(error)),
        }
    }
    for name in missing.into_iter().rev() {
        anchor.push(name);
        fs::create_dir(&anchor)?;
        harden_new_directory(&anchor)?;
        if let Some(parent) = anchor.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), OperatorError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), OperatorError> {
    // `std` has no portable way to open a Windows directory for
    // `FlushFileBuffers`. Checkpoints therefore use create-new final names (no
    // replace-over-existing dependency), while file contents themselves are
    // synchronized. Directory-entry durability remains platform/filesystem
    // dependent and is not presented as an ACL or flush guarantee.
    Ok(())
}

#[cfg(unix)]
fn ensure_private_dir_with_anchor(path: &Path, policy: AnchorPolicy) -> Result<(), OperatorError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    prepare_directory_tree(path, policy)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: getuid takes no arguments and cannot invalidate memory.
    let our_uid = unsafe { libc::getuid() };
    if !metadata.file_type().is_dir()
        || metadata.uid() != our_uid
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(OperatorError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} must be a real uid-{our_uid} directory with mode 0700",
                path.display()
            ),
        )));
    }
    sync_directory(path)?;
    if let Some(parent) = path.parent() {
        // Best-effort: every component this call CREATED already had its parent
        // synchronized in `prepare_directory_tree`, so the only thing left here
        // is a re-flush of a directory that may not be ours (the shared state
        // root) on a filesystem that may not flush directories at all. A
        // subsystem must not fail to start over a redundant fsync.
        let _ = sync_directory(parent);
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_private_dir_with_anchor(path: &Path, policy: AnchorPolicy) -> Result<(), OperatorError> {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    prepare_directory_tree(path, policy)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(OperatorError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a real directory", path.display()),
        )));
    }
    sync_directory(path)?;
    if let Some(parent) = path.parent() {
        // Best-effort: every component this call CREATED already had its parent
        // synchronized in `prepare_directory_tree`, so the only thing left here
        // is a re-flush of a directory that may not be ours (the shared state
        // root) on a filesystem that may not flush directories at all. A
        // subsystem must not fail to start over a redundant fsync.
        let _ = sync_directory(parent);
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_dir_with_anchor(path: &Path, policy: AnchorPolicy) -> Result<(), OperatorError> {
    prepare_directory_tree(path, policy)?;
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(OperatorError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a real directory", path.display()),
        )));
    }
    sync_directory(path)?;
    if let Some(parent) = path.parent() {
        // Best-effort: every component this call CREATED already had its parent
        // synchronized in `prepare_directory_tree`, so the only thing left here
        // is a re-flush of a directory that may not be ours (the shared state
        // root) on a filesystem that may not flush directories at all. A
        // subsystem must not fail to start over a redundant fsync.
        let _ = sync_directory(parent);
    }
    Ok(())
}

/// The operator's own directory: created, hardened to 0700, and verified.
fn ensure_private_dir(path: &Path) -> Result<(), OperatorError> {
    ensure_private_dir_with_anchor(path, AnchorPolicy::Police)
}

/// The operator's top directory, whose anchor is the shared state root.
fn ensure_private_dir_below_shared_root(path: &Path) -> Result<(), OperatorError> {
    ensure_private_dir_with_anchor(path, AnchorPolicy::Admit)
}

fn private_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options
}

fn open_private_regular_file(path: &Path) -> Result<File, OperatorError> {
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    #[cfg(windows)]
    use std::os::windows::fs::MetadataExt as _;

    let existed = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata_is_link_like(&metadata) {
                return Err(OperatorError::Io(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{} is a symlink or reparse point", path.display()),
                )));
            }
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(OperatorError::Io(error)),
    };
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(OperatorError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is a symlink", path.display()),
        )));
    }
    let file = private_open_options().open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(OperatorError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a regular file", path.display()),
        )));
    }
    #[cfg(unix)]
    {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let metadata = file.metadata()?;
        // SAFETY: getuid takes no arguments and cannot invalidate memory.
        let our_uid = unsafe { libc::getuid() };
        if metadata.uid() != our_uid || metadata.mode() & 0o777 != 0o600 {
            return Err(OperatorError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} must be a uid-{our_uid} regular file with mode 0600",
                    path.display()
                ),
            )));
        }
    }
    #[cfg(windows)]
    if metadata.file_attributes() & 0x400 != 0 {
        return Err(OperatorError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is a reparse point", path.display()),
        )));
    }
    if !existed {
        file.sync_all()?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(file)
}

fn fault_marker_bytes(reason: FleetFaultReason) -> [u8; FAULT_MARKER_LEN] {
    let mut bytes = [0_u8; FAULT_MARKER_LEN];
    bytes[0..4].copy_from_slice(&FAULT_MARKER_MAGIC);
    bytes[4..6].copy_from_slice(&FAULT_MARKER_SCHEMA.to_le_bytes());
    bytes[6] = reason.to_tag();
    bytes
}

/// Read the independent fail-closed marker without ever creating it. Any
/// present but unparseable marker is itself durable evidence of uncertainty.
fn read_fault_marker(directory: &Path) -> Result<Option<FleetFaultReason>, OperatorError> {
    let path = directory.join(FAULT_MARKER_NAME);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(OperatorError::Io(error)),
    };
    if metadata_is_link_like(&metadata) || !metadata.file_type().is_file() {
        return Err(OperatorError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a real regular file", path.display()),
        )));
    }
    if metadata.len() != FAULT_MARKER_LEN as u64 {
        return Ok(Some(FleetFaultReason::DurabilityUncertain));
    }
    let mut file = open_private_regular_file(&path)?;
    let mut bytes = [0_u8; FAULT_MARKER_LEN];
    file.seek(SeekFrom::Start(0))?;
    if file.read_exact(&mut bytes).is_err()
        || bytes[0..4] != FAULT_MARKER_MAGIC
        || u16::from_le_bytes([bytes[4], bytes[5]]) != FAULT_MARKER_SCHEMA
        || bytes[7] != 0
    {
        return Ok(Some(FleetFaultReason::DurabilityUncertain));
    }
    Ok(Some(
        FleetFaultReason::from_tag(bytes[6]).unwrap_or(FleetFaultReason::DurabilityUncertain),
    ))
}

/// Ensure a synchronized marker exists before attempting the WAL transition.
/// The first marker wins; a malformed predecessor is normalized to the generic
/// fail-closed reason rather than overwritten with a less conservative cause.
fn ensure_fault_marker(
    directory: &Path,
    requested: FleetFaultReason,
) -> Result<FleetFaultReason, OperatorError> {
    let reason = read_fault_marker(directory)?.unwrap_or(requested);
    let path = directory.join(FAULT_MARKER_NAME);
    let mut file = open_private_regular_file(&path)?;
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(&fault_marker_bytes(reason))?;
    file.sync_all()?;
    sync_directory(directory)?;
    Ok(reason)
}

fn remove_fault_marker(directory: &Path) -> Result<(), OperatorError> {
    let path = directory.join(FAULT_MARKER_NAME);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata_is_link_like(&metadata) || !metadata.file_type().is_file() {
                return Err(OperatorError::Io(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{} is not a real regular file", path.display()),
                )));
            }
            fs::remove_file(path)?;
            sync_directory(directory)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(OperatorError::Io(error)),
    }
    Ok(())
}

struct ProcessLock {
    file: File,
}

impl ProcessLock {
    fn acquire(path: &Path) -> Result<Self, OperatorError> {
        let file = open_private_regular_file(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file }),
            Err(fs::TryLockError::WouldBlock) => {
                Err(OperatorError::LockContended(path.to_path_buf()))
            }
            Err(fs::TryLockError::Error(error)) => Err(OperatorError::Io(error)),
        }
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        // Release the lock itself; do not settle for closing a descriptor that
        // names it. The lock lives on the open file description, and any child
        // forked anywhere in this process while it was held owns a duplicate of
        // that description until it reaches `exec` — a window a loaded machine
        // stretches into milliseconds, and aterm forks for every session it
        // spawns. Closing alone inside that window leaves the lock standing in a
        // process that never opened the queue, so the next opener — this process
        // reopening the directory cold, or the next process to try — is refused
        // as `LockContended` by a holder no diagnostic can name. Unlocking is the
        // release no fork window can outlive: it makes "the last handle dropped"
        // mean "the next opener may have it".
        //
        // It reaches exactly the paths that RUN DESTRUCTORS, which is not all of
        // them. A process that leaves through `libc::_exit` runs none: the
        // seamless handoff's point of no return (`commit_and_exit` in
        // crates/aterm-gui/src/seamless.rs) is such an exit, so a successor
        // taking leadership after handoff still meets whatever the predecessor's
        // fork window left standing. Closing that gap means unlocking BEFORE the
        // exit, which no `Drop` can do for it.
        //
        // The workspace's other advisory-lock guards do NOT yet release this way:
        // `atpkg::lock::StoreLock` (crates/atpkg/src/lock.rs),
        // `aterm_update_core::FileLock` (crates/aterm-update-core/src/sys.rs) and
        // `atpkg-keys`' roster claim (crates/atpkg-keys/src/provision.rs) each
        // still rely on the close, and each carries the same fork window. They
        // are named here so the difference is deliberate and findable rather than
        // an accident of which one was measured first.
        let _ = self.file.unlock();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompletedAction {
    class: String,
    hash: String,
    result: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StoredStatus {
    Queued,
    Delivered {
        token: ClaimToken,
        claim_epoch: u64,
        claimed_at_ms: u64,
        expires_at_ms: u64,
        cumulative_extension_ms: u64,
    },
    ActionInFlight {
        token: ClaimToken,
        claim_epoch: u64,
        action_class: String,
        action_hash: String,
        intent_at_ms: u64,
    },
    Resolved {
        token: ClaimToken,
        claim_epoch: u64,
        resolution: Resolution,
        resolved_at_ms: u64,
        action: Option<CompletedAction>,
        reconciliation_note: Option<String>,
    },
    ResolvedUnclaimed {
        resolution: Resolution,
        resolved_at_ms: u64,
        reason: String,
    },
    InDoubt {
        token: Option<ClaimToken>,
        reason: String,
        at_ms: u64,
    },
}

impl StoredStatus {
    fn unresolved(&self) -> bool {
        !matches!(self, Self::Resolved { .. } | Self::ResolvedUnclaimed { .. })
    }

    fn snapshot(&self) -> EventStatus {
        match self {
            Self::Queued => EventStatus::Queued,
            Self::Delivered {
                token,
                claim_epoch,
                claimed_at_ms,
                expires_at_ms,
                cumulative_extension_ms,
            } => EventStatus::Delivered {
                token: token.clone(),
                claim_epoch: *claim_epoch,
                claimed_at_ms: *claimed_at_ms,
                expires_at_ms: *expires_at_ms,
                cumulative_extension_ms: *cumulative_extension_ms,
            },
            Self::ActionInFlight {
                token,
                claim_epoch,
                action_class,
                action_hash,
                intent_at_ms,
            } => EventStatus::ActionInFlight {
                token: token.clone(),
                claim_epoch: *claim_epoch,
                action_class: action_class.clone(),
                action_hash: action_hash.clone(),
                intent_at_ms: *intent_at_ms,
            },
            Self::Resolved {
                token,
                claim_epoch,
                resolution,
                resolved_at_ms,
                reconciliation_note,
                ..
            } => EventStatus::Resolved {
                token: token.clone(),
                claim_epoch: *claim_epoch,
                resolution: *resolution,
                resolved_at_ms: *resolved_at_ms,
                reconciliation_note: reconciliation_note.clone(),
            },
            Self::ResolvedUnclaimed {
                resolution,
                resolved_at_ms,
                reason,
            } => EventStatus::ResolvedUnclaimed {
                resolution: *resolution,
                resolved_at_ms: *resolved_at_ms,
                reason: reason.clone(),
            },
            Self::InDoubt {
                token,
                reason,
                at_ms,
            } => EventStatus::InDoubt {
                token: token.clone(),
                reason: reason.clone(),
                at_ms: *at_ms,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredEvent {
    id: EventId,
    sid: String,
    generation: EventGeneration,
    condition: AttentionCondition,
    redelivery_count: u32,
    escalated: bool,
    status: StoredStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StoredFleetGate {
    Healthy,
    Faulted(FleetFault),
    RebaselineRequired {
        fault: FleetFault,
        pending_sids: BTreeSet<String>,
    },
}

impl StoredFleetGate {
    fn snapshot(&self) -> FleetGateStatus {
        match self {
            Self::Healthy => FleetGateStatus::Healthy,
            Self::Faulted(fault) => FleetGateStatus::Faulted(*fault),
            Self::RebaselineRequired {
                fault,
                pending_sids,
            } => FleetGateStatus::RebaselineRequired {
                fault: *fault,
                pending_sids: pending_sids.iter().cloned().collect(),
            },
        }
    }

    fn require_healthy(&self) -> Result<(), OperatorError> {
        match self {
            Self::Healthy => Ok(()),
            Self::Faulted(fault) => Err(OperatorError::FleetFaulted(fault.reason)),
            Self::RebaselineRequired { pending_sids, .. } => {
                Err(OperatorError::RebaselineRequired {
                    remaining: pending_sids.len(),
                })
            }
        }
    }
}

impl StoredEvent {
    fn snapshot(&self) -> EventSnapshot {
        EventSnapshot {
            id: self.id,
            sid: self.sid.clone(),
            generation: self.generation,
            condition: self.condition,
            redelivery_count: self.redelivery_count,
            escalated: self.escalated,
            status: self.status.snapshot(),
        }
    }
}

#[derive(Clone, Debug)]
struct QueueState {
    durable_epoch: u64,
    next_event_id: u64,
    next_record_sequence: u64,
    managed: BTreeSet<String>,
    events: BTreeMap<EventId, StoredEvent>,
    generations: BTreeMap<(String, EventGeneration), EventId>,
    ready_by_sid: BTreeMap<String, VecDeque<EventId>>,
    fair_sids: VecDeque<String>,
    fleet_gate: StoredFleetGate,
}

impl Default for QueueState {
    fn default() -> Self {
        Self {
            durable_epoch: 0,
            next_event_id: 1,
            next_record_sequence: 1,
            managed: BTreeSet::new(),
            events: BTreeMap::new(),
            generations: BTreeMap::new(),
            ready_by_sid: BTreeMap::new(),
            fair_sids: VecDeque::new(),
            fleet_gate: StoredFleetGate::Healthy,
        }
    }
}

impl QueueState {
    fn unresolved_len(&self) -> usize {
        self.events
            .values()
            .filter(|event| event.status.unresolved())
            .count()
    }

    fn add_ready(&mut self, sid: &str, event_id: EventId) -> Result<(), String> {
        if !self.managed.contains(sid) {
            return Err(format!(
                "queued event {} has unmanaged sid {sid:?}",
                event_id.get()
            ));
        }
        let queue = self.ready_by_sid.entry(sid.to_string()).or_default();
        if queue.is_empty() {
            if self.fair_sids.iter().any(|queued| queued == sid) {
                return Err(format!("sid {sid:?} already exists in fair queue"));
            }
            self.fair_sids.push_back(sid.to_string());
        }
        if queue.contains(&event_id) {
            return Err(format!("event {} is already ready", event_id.get()));
        }
        queue.push_back(event_id);
        Ok(())
    }

    fn apply_initial_event(&mut self, event: &StoredEvent) -> Result<(), String> {
        if event.id.get() != self.next_event_id {
            return Err(format!(
                "enqueue id {} does not equal next id {}",
                event.id.get(),
                self.next_event_id
            ));
        }
        if !self.managed.contains(&event.sid) {
            return Err(format!("enqueue sid {:?} is unmanaged", event.sid));
        }
        if !matches!(event.status, StoredStatus::Queued)
            || event.redelivery_count != 0
            || event.escalated
        {
            return Err("new event has a non-initial lifecycle".into());
        }
        let key = (event.sid.clone(), event.generation);
        if self.generations.contains_key(&key) || self.events.contains_key(&event.id) {
            return Err("enqueue duplicates an event identity".into());
        }
        self.next_event_id = self
            .next_event_id
            .checked_add(1)
            .ok_or_else(|| "event id space exhausted".to_string())?;
        self.generations.insert(key, event.id);
        self.events.insert(event.id, event.clone());
        self.add_ready(&event.sid, event.id)
    }

    fn remove_ready(&mut self, sid: &str, event_id: EventId) -> Result<(), String> {
        let Some(queue) = self.ready_by_sid.get_mut(sid) else {
            return Err(format!("sid {sid:?} has no ready queue"));
        };
        let Some(position) = queue.iter().position(|queued| *queued == event_id) else {
            return Err(format!("event {} is not ready", event_id.get()));
        };
        queue.remove(position);
        if queue.is_empty() {
            self.ready_by_sid.remove(sid);
            let Some(position) = self.fair_sids.iter().position(|queued| queued == sid) else {
                return Err(format!("sid {sid:?} is absent from fair queue"));
            };
            self.fair_sids.remove(position);
        }
        Ok(())
    }

    fn next_ready(&self) -> Option<EventId> {
        let sid = self.fair_sids.front()?;
        self.ready_by_sid.get(sid)?.front().copied()
    }

    fn claim_ready(&mut self, event_id: EventId) -> Result<(), String> {
        let sid = self
            .fair_sids
            .pop_front()
            .ok_or_else(|| "fair queue is empty".to_string())?;
        let queue = self
            .ready_by_sid
            .get_mut(&sid)
            .ok_or_else(|| format!("fair sid {sid:?} has no ready queue"))?;
        let actual = queue
            .pop_front()
            .ok_or_else(|| format!("fair sid {sid:?} has an empty ready queue"))?;
        if actual != event_id {
            return Err(format!(
                "fair claim expected event {}, record names {}",
                actual.get(),
                event_id.get()
            ));
        }
        if queue.is_empty() {
            self.ready_by_sid.remove(&sid);
        } else {
            self.fair_sids.push_back(sid);
        }
        Ok(())
    }

    /// Apply a durable leadership epoch transition.
    ///
    /// This transition is intentionally part of `Epoch` replay rather than an
    /// in-memory recovery projection. A new leader atomically revokes every old
    /// delivered claim and turns every unmatched old action intent into InDoubt.
    /// Consequently a later reconciliation record replays from exactly the same
    /// predecessor state that the live process observed.
    fn advance_epoch(&mut self, epoch: u64, redelivery_cap: u32) -> Result<(), String> {
        if epoch == 0 || epoch <= self.durable_epoch {
            return Err(format!(
                "epoch {epoch} does not advance durable epoch {}",
                self.durable_epoch
            ));
        }
        if redelivery_cap == 0 {
            return Err("epoch carries a zero redelivery cap".into());
        }
        self.durable_epoch = epoch;

        let stale: Vec<EventId> = self
            .events
            .iter()
            .filter_map(|(id, event)| match &event.status {
                StoredStatus::Delivered { claim_epoch, .. }
                | StoredStatus::ActionInFlight { claim_epoch, .. }
                    if *claim_epoch < epoch =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect();
        for event_id in stale {
            let (sid, status, escalated, redelivery_count) = {
                let event = self
                    .events
                    .get(&event_id)
                    .ok_or_else(|| format!("event {} vanished", event_id.get()))?;
                (
                    event.sid.clone(),
                    event.status.clone(),
                    event.escalated,
                    event.redelivery_count,
                )
            };
            match status {
                StoredStatus::Delivered {
                    token,
                    claimed_at_ms,
                    ..
                } if escalated => {
                    let event = self
                        .events
                        .get_mut(&event_id)
                        .ok_or_else(|| format!("event {} vanished", event_id.get()))?;
                    event.status = StoredStatus::InDoubt {
                        token: Some(token),
                        reason: "escalation delivery lost during leader takeover".into(),
                        at_ms: claimed_at_ms,
                    };
                }
                StoredStatus::Delivered { .. } => {
                    let next_count = redelivery_count.saturating_add(1);
                    let event = self
                        .events
                        .get_mut(&event_id)
                        .ok_or_else(|| format!("event {} vanished", event_id.get()))?;
                    event.redelivery_count = next_count;
                    if next_count >= redelivery_cap {
                        event.escalated = true;
                        event.condition = AttentionCondition::Escalation;
                    }
                    event.status = StoredStatus::Queued;
                    self.add_ready(&sid, event_id)?;
                }
                StoredStatus::ActionInFlight {
                    token,
                    action_class,
                    action_hash,
                    intent_at_ms,
                    ..
                } => {
                    let event = self
                        .events
                        .get_mut(&event_id)
                        .ok_or_else(|| format!("event {} vanished", event_id.get()))?;
                    event.status = StoredStatus::InDoubt {
                        token: Some(token),
                        reason: format!(
                            "recovered unmatched action intent class={action_class} hash={action_hash}"
                        ),
                        at_ms: intent_at_ms,
                    };
                }
                _ => return Err("stale-claim scan selected an incompatible event".into()),
            }
        }
        Ok(())
    }

    fn latch_fleet_fault(&mut self, fault: FleetFault) -> Result<(), String> {
        if fault.fault_epoch != self.durable_epoch || fault.fault_epoch == 0 {
            return Err(format!(
                "fault epoch {} does not equal durable epoch {}",
                fault.fault_epoch, self.durable_epoch
            ));
        }
        if matches!(self.fleet_gate, StoredFleetGate::Faulted(_)) {
            return Err("fleet fault latch is a no-op".into());
        }
        self.fleet_gate = StoredFleetGate::Faulted(fault);
        Ok(())
    }

    fn begin_fault_clear(&mut self, at_ms: u64) -> Result<(), String> {
        let StoredFleetGate::Faulted(fault) = self.fleet_gate.clone() else {
            return Err("fleet is not in the faulted state".into());
        };
        let active = self
            .events
            .iter()
            .filter_map(|(id, event)| {
                matches!(
                    event.status,
                    StoredStatus::Queued
                        | StoredStatus::Delivered { .. }
                        | StoredStatus::ActionInFlight { .. }
                )
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        for event_id in active {
            let (sid, status) = {
                let event = self
                    .events
                    .get(&event_id)
                    .ok_or_else(|| format!("event {} vanished", event_id.get()))?;
                (event.sid.clone(), event.status.clone())
            };
            if matches!(status, StoredStatus::Queued) {
                self.remove_ready(&sid, event_id)?;
            }
            let event = self
                .events
                .get_mut(&event_id)
                .ok_or_else(|| format!("event {} vanished", event_id.get()))?;
            event.status = match status {
                StoredStatus::Queued | StoredStatus::Delivered { .. } => {
                    StoredStatus::ResolvedUnclaimed {
                        resolution: Resolution::Paused,
                        resolved_at_ms: at_ms,
                        reason: "fleet fault clear revoked pre-baseline work".into(),
                    }
                }
                StoredStatus::ActionInFlight { token, .. } => StoredStatus::InDoubt {
                    token: Some(token),
                    reason: "fleet fault clear found an action intent in flight".into(),
                    at_ms,
                },
                StoredStatus::Resolved { .. }
                | StoredStatus::ResolvedUnclaimed { .. }
                | StoredStatus::InDoubt { .. } => {
                    return Err("fault clear selected a terminal event".into());
                }
            };
        }
        if !self.ready_by_sid.is_empty() || !self.fair_sids.is_empty() {
            return Err("fault clear left a nonempty ready queue".into());
        }
        // Rebaseline is an explicit new identity boundary. Historical events
        // remain for bounded audit, but their exact-generation idempotency keys
        // cannot suppress the fresh baseline records required for this clear.
        self.generations.clear();
        self.fleet_gate = StoredFleetGate::RebaselineRequired {
            fault,
            pending_sids: self.managed.clone(),
        };
        Ok(())
    }

    fn apply_rebaseline(&mut self, event: &StoredEvent) -> Result<(), String> {
        let StoredFleetGate::RebaselineRequired { pending_sids, .. } = &self.fleet_gate else {
            return Err("fleet is not waiting for fault-clear baselines".into());
        };
        if !pending_sids.contains(&event.sid) {
            return Err(format!("sid {:?} has no pending baseline", event.sid));
        }
        if event.id.get() != self.next_event_id
            || event.condition != AttentionCondition::Changed
            || !matches!(event.status, StoredStatus::Queued)
            || event.redelivery_count != 0
            || event.escalated
        {
            return Err("fault-clear baseline has a non-initial lifecycle".into());
        }
        if !self.managed.contains(&event.sid) {
            return Err(format!("baseline sid {:?} is unmanaged", event.sid));
        }
        let key = (event.sid.clone(), event.generation);
        if self.generations.contains_key(&key) || self.events.contains_key(&event.id) {
            return Err("fault-clear baseline duplicates an event identity".into());
        }
        self.next_event_id = self
            .next_event_id
            .checked_add(1)
            .ok_or_else(|| "event id space exhausted".to_string())?;
        self.generations.insert(key, event.id);
        self.events.insert(event.id, event.clone());
        self.add_ready(&event.sid, event.id)?;
        let StoredFleetGate::RebaselineRequired { pending_sids, .. } = &mut self.fleet_gate else {
            return Err("fleet baseline gate changed during one transition".into());
        };
        if !pending_sids.remove(&event.sid) {
            return Err("fault-clear baseline did not consume its sid".into());
        }
        Ok(())
    }

    fn complete_fault_clear(&mut self) -> Result<(), String> {
        let StoredFleetGate::RebaselineRequired { pending_sids, .. } = &self.fleet_gate else {
            return Err("fleet is not waiting for fault-clear baselines".into());
        };
        if !pending_sids.is_empty() {
            return Err(format!(
                "fault clear still needs {} baseline(s)",
                pending_sids.len()
            ));
        }
        if self
            .events
            .values()
            .any(|event| matches!(event.status, StoredStatus::InDoubt { .. }))
        {
            return Err("fault clear has unresolved in-doubt action state".into());
        }
        self.fleet_gate = StoredFleetGate::Healthy;
        Ok(())
    }

    fn validate(&self) -> Result<(), String> {
        if self.managed.len() > MAX_MANAGED_SIDS {
            return Err(format!(
                "managed-session count {} exceeds hard bound {MAX_MANAGED_SIDS}",
                self.managed.len()
            ));
        }
        let max_id = self.events.keys().next_back().map_or(0, |id| id.get());
        if self.next_event_id == 0 || self.next_event_id <= max_id {
            return Err("next event id does not lead every stored event".into());
        }
        if self.next_record_sequence == 0 {
            return Err("next WAL record sequence is zero".into());
        }
        match &self.fleet_gate {
            StoredFleetGate::Healthy => {}
            StoredFleetGate::Faulted(fault) | StoredFleetGate::RebaselineRequired { fault, .. }
                if fault.fault_epoch == 0 || fault.fault_epoch > self.durable_epoch =>
            {
                return Err(format!(
                    "fleet fault epoch {} is outside durable epoch {}",
                    fault.fault_epoch, self.durable_epoch
                ));
            }
            StoredFleetGate::Faulted(_) => {}
            StoredFleetGate::RebaselineRequired { pending_sids, .. } => {
                if !pending_sids.is_subset(&self.managed) {
                    return Err("fault-clear baseline roster exceeds managed allowlist".into());
                }
                for event in self.events.values().filter(|event| {
                    matches!(
                        event.status,
                        StoredStatus::Queued
                            | StoredStatus::Delivered { .. }
                            | StoredStatus::ActionInFlight { .. }
                    )
                }) {
                    if !matches!(event.status, StoredStatus::Queued)
                        || event.condition != AttentionCondition::Changed
                        || pending_sids.contains(&event.sid)
                    {
                        return Err(format!(
                            "event {} is not a completed fault-clear baseline",
                            event.id.get()
                        ));
                    }
                }
            }
        }
        for ((sid, generation), id) in &self.generations {
            let indexed = self
                .events
                .get(id)
                .ok_or_else(|| format!("generation index points at missing event {}", id.get()))?;
            if indexed.sid != *sid || indexed.generation != *generation {
                return Err(format!(
                    "generation index key differs from event {}",
                    id.get()
                ));
            }
        }
        for (id, event) in &self.events {
            if event.id != *id {
                return Err(format!("event-map key {} differs from payload", id.get()));
            }
            if matches!(
                event.status,
                StoredStatus::Queued
                    | StoredStatus::Delivered { .. }
                    | StoredStatus::ActionInFlight { .. }
            ) && self.generations.get(&(event.sid.clone(), event.generation)) != Some(id)
            {
                return Err(format!(
                    "unresolved event {} is absent from generation index",
                    id.get()
                ));
            }
            if event.escalated
                && (event.condition != AttentionCondition::Escalation
                    || event.redelivery_count == 0)
            {
                return Err(format!("event {} has malformed escalation state", id.get()));
            }
            if matches!(
                event.status,
                StoredStatus::Queued
                    | StoredStatus::Delivered { .. }
                    | StoredStatus::ActionInFlight { .. }
            ) && !self.managed.contains(&event.sid)
            {
                return Err(format!("active event {} has unmanaged sid", id.get()));
            }
            match &event.status {
                StoredStatus::Delivered { claim_epoch, .. }
                | StoredStatus::ActionInFlight { claim_epoch, .. }
                    if *claim_epoch != self.durable_epoch =>
                {
                    return Err(format!(
                        "active event {} belongs to claim epoch {claim_epoch}, current epoch is {}",
                        id.get(),
                        self.durable_epoch
                    ));
                }
                _ => {}
            }
        }

        let mut ready_seen = BTreeSet::new();
        let mut fair_seen = BTreeSet::new();
        for sid in &self.fair_sids {
            if !fair_seen.insert(sid) {
                return Err(format!("sid {sid:?} occurs twice in fair queue"));
            }
            let queue = self
                .ready_by_sid
                .get(sid)
                .ok_or_else(|| format!("fair sid {sid:?} has no event queue"))?;
            if queue.is_empty() {
                return Err(format!("fair sid {sid:?} has an empty event queue"));
            }
        }
        if fair_seen.len() != self.ready_by_sid.len() {
            return Err("fair queue and per-sid queues differ".into());
        }
        for (sid, queue) in &self.ready_by_sid {
            if !fair_seen.contains(sid) {
                return Err(format!("ready sid {sid:?} is absent from fair queue"));
            }
            for event_id in queue {
                if !ready_seen.insert(*event_id) {
                    return Err(format!(
                        "event {} occurs twice in ready queues",
                        event_id.get()
                    ));
                }
                let event = self
                    .events
                    .get(event_id)
                    .ok_or_else(|| format!("ready event {} does not exist", event_id.get()))?;
                if event.sid != *sid || !matches!(event.status, StoredStatus::Queued) {
                    return Err(format!(
                        "ready event {} has incompatible state",
                        event_id.get()
                    ));
                }
            }
        }
        let queued: BTreeSet<EventId> = self
            .events
            .iter()
            .filter_map(|(id, event)| matches!(event.status, StoredStatus::Queued).then_some(*id))
            .collect();
        if queued != ready_seen {
            return Err("ready queues do not contain exactly the queued events".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum WalRecord {
    Epoch {
        epoch: u64,
        redelivery_cap: u32,
    },
    Manage {
        sid: String,
    },
    Unmanage {
        sid: String,
        at_ms: u64,
    },
    Enqueue {
        event: StoredEvent,
    },
    Coalesce {
        event_id: EventId,
        condition: AttentionCondition,
    },
    Claim {
        event_id: EventId,
        token: ClaimToken,
        claim_epoch: u64,
        claimed_at_ms: u64,
        expires_at_ms: u64,
    },
    Extend {
        event_id: EventId,
        token: ClaimToken,
        expires_at_ms: u64,
        cumulative_extension_ms: u64,
    },
    Expire {
        event_id: EventId,
        token: ClaimToken,
        at_ms: u64,
        redelivery_count: u32,
        became_escalated: bool,
    },
    Resolve {
        event_id: EventId,
        token: ClaimToken,
        resolution: Resolution,
        resolved_at_ms: u64,
    },
    Conflict {
        event_id: EventId,
        token: ClaimToken,
        reason: String,
        at_ms: u64,
    },
    BeginAction {
        event_id: EventId,
        token: ClaimToken,
        action_class: String,
        action_hash: String,
        intent_at_ms: u64,
    },
    FinishAction {
        event_id: EventId,
        token: ClaimToken,
        action_hash: String,
        result: String,
        resolution: Resolution,
        resolved_at_ms: u64,
    },
    MarkActionInDoubt {
        event_id: EventId,
        token: ClaimToken,
        reason: String,
        at_ms: u64,
    },
    ReconcileInDoubt {
        event_id: EventId,
        token: ClaimToken,
        resolution: Resolution,
        note: String,
        resolved_at_ms: u64,
    },
    InvalidateDelivered {
        event_id: EventId,
        token: ClaimToken,
        claim_epoch: u64,
        condition: AttentionCondition,
    },
    LatchFleetFault {
        fault: FleetFault,
    },
    BeginFaultClear {
        at_ms: u64,
    },
    Rebaseline {
        event: StoredEvent,
    },
    CompleteFaultClear {
        at_ms: u64,
    },
    ManageWithBaseline {
        event: StoredEvent,
    },
}

impl WalRecord {
    const fn kind(&self) -> u16 {
        match self {
            Self::Epoch { .. } => 1,
            Self::Manage { .. } => 2,
            Self::Unmanage { .. } => 3,
            Self::Enqueue { .. } => 4,
            Self::Coalesce { .. } => 5,
            Self::Claim { .. } => 6,
            Self::Extend { .. } => 7,
            Self::Expire { .. } => 8,
            Self::Resolve { .. } => 9,
            Self::Conflict { .. } => 10,
            Self::BeginAction { .. } => 11,
            Self::FinishAction { .. } => 12,
            Self::MarkActionInDoubt { .. } => 13,
            Self::ReconcileInDoubt { .. } => 14,
            Self::InvalidateDelivered { .. } => 15,
            Self::LatchFleetFault { .. } => 16,
            Self::BeginFaultClear { .. } => 17,
            Self::Rebaseline { .. } => 18,
            Self::CompleteFaultClear { .. } => 19,
            Self::ManageWithBaseline { .. } => 20,
        }
    }

    fn header_epoch(&self, current_epoch: u64) -> u64 {
        match self {
            Self::Epoch { epoch, .. } => *epoch,
            _ => current_epoch,
        }
    }

    fn apply(&self, state: &mut QueueState) -> Result<(), String> {
        match self {
            Self::Epoch {
                epoch,
                redelivery_cap,
            } => {
                state.advance_epoch(*epoch, *redelivery_cap)?;
            }
            Self::Manage { sid } => {
                state
                    .fleet_gate
                    .require_healthy()
                    .map_err(|error| error.to_string())?;
                if state.managed.len() >= MAX_MANAGED_SIDS {
                    return Err(format!(
                        "managed-session count reached hard bound {MAX_MANAGED_SIDS}"
                    ));
                }
                if !state.managed.insert(sid.clone()) {
                    return Err(format!("sid {sid:?} is already managed"));
                }
            }
            Self::Unmanage { sid, at_ms } => {
                if !state.managed.remove(sid) {
                    return Err(format!("sid {sid:?} is not managed"));
                }
                let affected: Vec<EventId> = state
                    .events
                    .iter()
                    .filter_map(|(id, event)| {
                        (event.sid == *sid
                            && matches!(
                                event.status,
                                StoredStatus::Queued
                                    | StoredStatus::Delivered { .. }
                                    | StoredStatus::ActionInFlight { .. }
                            ))
                        .then_some(*id)
                    })
                    .collect();
                for event_id in affected {
                    let queued = matches!(
                        state.events.get(&event_id).map(|event| &event.status),
                        Some(StoredStatus::Queued)
                    );
                    if queued {
                        state.remove_ready(sid, event_id)?;
                    }
                    let event = state
                        .events
                        .get_mut(&event_id)
                        .ok_or_else(|| format!("event {} vanished", event_id.get()))?;
                    event.status = match &event.status {
                        StoredStatus::Queued | StoredStatus::Delivered { .. } => {
                            StoredStatus::ResolvedUnclaimed {
                                resolution: Resolution::Paused,
                                resolved_at_ms: *at_ms,
                                reason: "session removed from managed allowlist before actuation"
                                    .into(),
                            }
                        }
                        StoredStatus::ActionInFlight { token, .. } => StoredStatus::InDoubt {
                            token: Some(token.clone()),
                            reason: "session removed with an action in flight".into(),
                            at_ms: *at_ms,
                        },
                        _ => return Err("unmanage selected a terminal event".into()),
                    };
                }
                // Retire exact-generation idempotency keys at the management
                // boundary. Historical events remain available for bounded
                // audit/idempotent acknowledgement, but re-managing the same
                // unchanged screen may create a fresh baseline occurrence.
                state
                    .generations
                    .retain(|(indexed_sid, _), _| indexed_sid != sid);
                if let StoredFleetGate::RebaselineRequired { pending_sids, .. } =
                    &mut state.fleet_gate
                {
                    pending_sids.remove(sid);
                }
            }
            Self::Enqueue { event } => {
                state
                    .fleet_gate
                    .require_healthy()
                    .map_err(|error| error.to_string())?;
                state.apply_initial_event(event)?;
            }
            Self::Coalesce {
                event_id,
                condition,
            } => {
                state
                    .fleet_gate
                    .require_healthy()
                    .map_err(|error| error.to_string())?;
                let event = state
                    .events
                    .get_mut(event_id)
                    .ok_or_else(|| format!("event {} does not exist", event_id.get()))?;
                if !matches!(event.status, StoredStatus::Queued) {
                    return Err(format!("event {} is not coalescible", event_id.get()));
                }
                if *condition < event.condition {
                    return Err(format!("event {} condition regressed", event_id.get()));
                }
                if *condition == event.condition {
                    return Err(format!("event {} coalesce is a no-op", event_id.get()));
                }
                event.condition = *condition;
            }
            Self::InvalidateDelivered {
                event_id,
                token,
                claim_epoch,
                condition,
            } => {
                state
                    .fleet_gate
                    .require_healthy()
                    .map_err(|error| error.to_string())?;
                let event = state
                    .events
                    .get(event_id)
                    .ok_or_else(|| format!("event {} does not exist", event_id.get()))?;
                let StoredStatus::Delivered {
                    token: current,
                    claim_epoch: current_epoch,
                    ..
                } = &event.status
                else {
                    return Err(format!("event {} is not delivered", event_id.get()));
                };
                if !tokens_equal(current, token)
                    || *current_epoch != *claim_epoch
                    || *claim_epoch != state.durable_epoch
                {
                    return Err(format!(
                        "event {} delivered-invalidation CAS is stale",
                        event_id.get()
                    ));
                }
                if *condition <= event.condition {
                    return Err(format!(
                        "event {} delivered invalidation is regressive or a no-op",
                        event_id.get()
                    ));
                }
                let sid = event.sid.clone();
                let event = state
                    .events
                    .get_mut(event_id)
                    .ok_or_else(|| format!("event {} vanished", event_id.get()))?;
                event.condition = *condition;
                event.status = StoredStatus::Queued;
                state.add_ready(&sid, *event_id)?;
            }
            Self::Claim {
                event_id,
                token,
                claim_epoch,
                claimed_at_ms,
                expires_at_ms,
            } => {
                state
                    .fleet_gate
                    .require_healthy()
                    .map_err(|error| error.to_string())?;
                if *claim_epoch != state.durable_epoch {
                    return Err(format!(
                        "claim epoch {claim_epoch} does not equal durable epoch {}",
                        state.durable_epoch
                    ));
                }
                if *expires_at_ms <= *claimed_at_ms {
                    return Err("claim expiry does not follow claim time".into());
                }
                let event = state
                    .events
                    .get(event_id)
                    .ok_or_else(|| format!("event {} does not exist", event_id.get()))?;
                if !matches!(event.status, StoredStatus::Queued) {
                    return Err(format!("event {} is not queued", event_id.get()));
                }
                state.claim_ready(*event_id)?;
                let event = state
                    .events
                    .get_mut(event_id)
                    .ok_or_else(|| format!("event {} vanished", event_id.get()))?;
                event.status = StoredStatus::Delivered {
                    token: token.clone(),
                    claim_epoch: *claim_epoch,
                    claimed_at_ms: *claimed_at_ms,
                    expires_at_ms: *expires_at_ms,
                    cumulative_extension_ms: 0,
                };
            }
            Self::Extend {
                event_id,
                token,
                expires_at_ms,
                cumulative_extension_ms,
            } => {
                let event = state
                    .events
                    .get_mut(event_id)
                    .ok_or_else(|| format!("event {} does not exist", event_id.get()))?;
                let StoredStatus::Delivered {
                    token: current,
                    claim_epoch,
                    expires_at_ms: current_expiry,
                    cumulative_extension_ms: current_extension,
                    ..
                } = &mut event.status
                else {
                    return Err(format!("event {} is not delivered", event_id.get()));
                };
                if !tokens_equal(current, token)
                    || *claim_epoch != state.durable_epoch
                    || *expires_at_ms <= *current_expiry
                    || *cumulative_extension_ms <= *current_extension
                    || expires_at_ms - *current_expiry
                        != cumulative_extension_ms - *current_extension
                {
                    return Err(format!("event {} extension CAS is invalid", event_id.get()));
                }
                *current_expiry = *expires_at_ms;
                *current_extension = *cumulative_extension_ms;
            }
            Self::Expire {
                event_id,
                token,
                at_ms,
                redelivery_count,
                became_escalated,
            } => {
                let event = state
                    .events
                    .get(event_id)
                    .ok_or_else(|| format!("event {} does not exist", event_id.get()))?;
                let StoredStatus::Delivered {
                    token: current,
                    claim_epoch,
                    expires_at_ms,
                    ..
                } = &event.status
                else {
                    return Err(format!("event {} is not delivered", event_id.get()));
                };
                if !tokens_equal(current, token)
                    || *claim_epoch != state.durable_epoch
                    || *at_ms < *expires_at_ms
                {
                    return Err(format!("event {} expiry CAS is invalid", event_id.get()));
                }
                let sid = event.sid.clone();
                let already_escalated = event.escalated;
                let old_count = event.redelivery_count;
                if already_escalated {
                    if *redelivery_count != old_count || !*became_escalated {
                        return Err("escalated expiry record has invalid counters".into());
                    }
                    let event = state
                        .events
                        .get_mut(event_id)
                        .ok_or_else(|| format!("event {} vanished", event_id.get()))?;
                    event.status = StoredStatus::InDoubt {
                        token: Some(token.clone()),
                        reason: "escalation delivery expired".into(),
                        at_ms: *at_ms,
                    };
                } else {
                    if *redelivery_count != old_count.saturating_add(1) {
                        return Err("expiry did not increment redelivery count once".into());
                    }
                    let event = state
                        .events
                        .get_mut(event_id)
                        .ok_or_else(|| format!("event {} vanished", event_id.get()))?;
                    event.redelivery_count = *redelivery_count;
                    if *became_escalated {
                        event.escalated = true;
                        event.condition = AttentionCondition::Escalation;
                    }
                    event.status = StoredStatus::Queued;
                    state.add_ready(&sid, *event_id)?;
                }
            }
            Self::Resolve {
                event_id,
                token,
                resolution,
                resolved_at_ms,
            } => {
                let event = state
                    .events
                    .get_mut(event_id)
                    .ok_or_else(|| format!("event {} does not exist", event_id.get()))?;
                let StoredStatus::Delivered {
                    token: current,
                    claim_epoch,
                    expires_at_ms,
                    ..
                } = &event.status
                else {
                    return Err(format!("event {} is not delivered", event_id.get()));
                };
                if !tokens_equal(current, token)
                    || *claim_epoch != state.durable_epoch
                    || *resolved_at_ms >= *expires_at_ms
                {
                    return Err(format!(
                        "event {} resolution CAS is invalid",
                        event_id.get()
                    ));
                }
                event.status = StoredStatus::Resolved {
                    token: token.clone(),
                    claim_epoch: *claim_epoch,
                    resolution: *resolution,
                    resolved_at_ms: *resolved_at_ms,
                    action: None,
                    reconciliation_note: None,
                };
            }
            Self::Conflict {
                event_id,
                token,
                reason,
                at_ms,
            } => {
                let event = state
                    .events
                    .get_mut(event_id)
                    .ok_or_else(|| format!("event {} does not exist", event_id.get()))?;
                let StoredStatus::Resolved {
                    token: current,
                    claim_epoch,
                    ..
                } = &event.status
                else {
                    return Err(format!("event {} is not resolved", event_id.get()));
                };
                if !tokens_equal(current, token) || *claim_epoch != state.durable_epoch {
                    return Err(format!("event {} conflict token is stale", event_id.get()));
                }
                event.status = StoredStatus::InDoubt {
                    token: Some(token.clone()),
                    reason: reason.clone(),
                    at_ms: *at_ms,
                };
            }
            Self::BeginAction {
                event_id,
                token,
                action_class,
                action_hash,
                intent_at_ms,
            } => {
                state
                    .fleet_gate
                    .require_healthy()
                    .map_err(|error| error.to_string())?;
                let event = state
                    .events
                    .get_mut(event_id)
                    .ok_or_else(|| format!("event {} does not exist", event_id.get()))?;
                let StoredStatus::Delivered {
                    token: current,
                    claim_epoch,
                    expires_at_ms,
                    ..
                } = &event.status
                else {
                    return Err(format!("event {} is not delivered", event_id.get()));
                };
                if !tokens_equal(current, token)
                    || *claim_epoch != state.durable_epoch
                    || *intent_at_ms >= *expires_at_ms
                {
                    return Err(format!(
                        "event {} action-intent CAS is invalid",
                        event_id.get()
                    ));
                }
                event.status = StoredStatus::ActionInFlight {
                    token: token.clone(),
                    claim_epoch: *claim_epoch,
                    action_class: action_class.clone(),
                    action_hash: action_hash.clone(),
                    intent_at_ms: *intent_at_ms,
                };
            }
            Self::FinishAction {
                event_id,
                token,
                action_hash,
                result,
                resolution,
                resolved_at_ms,
            } => {
                let event = state
                    .events
                    .get_mut(event_id)
                    .ok_or_else(|| format!("event {} does not exist", event_id.get()))?;
                let StoredStatus::ActionInFlight {
                    token: current,
                    claim_epoch,
                    action_class,
                    action_hash: current_hash,
                    ..
                } = &event.status
                else {
                    return Err(format!("event {} has no action intent", event_id.get()));
                };
                if !tokens_equal(current, token)
                    || *claim_epoch != state.durable_epoch
                    || current_hash != action_hash
                {
                    return Err(format!(
                        "event {} action-result CAS is invalid",
                        event_id.get()
                    ));
                }
                let completed = CompletedAction {
                    class: action_class.clone(),
                    hash: action_hash.clone(),
                    result: result.clone(),
                };
                event.status = StoredStatus::Resolved {
                    token: token.clone(),
                    claim_epoch: *claim_epoch,
                    resolution: *resolution,
                    resolved_at_ms: *resolved_at_ms,
                    action: Some(completed),
                    reconciliation_note: None,
                };
            }
            Self::MarkActionInDoubt {
                event_id,
                token,
                reason,
                at_ms,
            } => {
                let event = state
                    .events
                    .get_mut(event_id)
                    .ok_or_else(|| format!("event {} does not exist", event_id.get()))?;
                let StoredStatus::ActionInFlight {
                    token: current,
                    claim_epoch,
                    ..
                } = &event.status
                else {
                    return Err(format!("event {} has no action intent", event_id.get()));
                };
                if !tokens_equal(current, token) || *claim_epoch != state.durable_epoch {
                    return Err(format!("event {} action token is stale", event_id.get()));
                }
                event.status = StoredStatus::InDoubt {
                    token: Some(token.clone()),
                    reason: reason.clone(),
                    at_ms: *at_ms,
                };
            }
            Self::ReconcileInDoubt {
                event_id,
                token,
                resolution,
                note,
                resolved_at_ms,
            } => {
                let claim_epoch = state.durable_epoch;
                let event = state
                    .events
                    .get_mut(event_id)
                    .ok_or_else(|| format!("event {} does not exist", event_id.get()))?;
                let StoredStatus::InDoubt {
                    token: Some(current),
                    ..
                } = &event.status
                else {
                    return Err(format!(
                        "event {} is not token-scoped in-doubt",
                        event_id.get()
                    ));
                };
                if !tokens_equal(current, token) {
                    return Err(format!(
                        "event {} reconciliation token is stale",
                        event_id.get()
                    ));
                }
                event.status = StoredStatus::Resolved {
                    token: token.clone(),
                    claim_epoch,
                    resolution: *resolution,
                    resolved_at_ms: *resolved_at_ms,
                    action: None,
                    reconciliation_note: Some(note.clone()),
                };
            }
            Self::LatchFleetFault { fault } => state.latch_fleet_fault(*fault)?,
            Self::BeginFaultClear { at_ms } => state.begin_fault_clear(*at_ms)?,
            Self::Rebaseline { event } => state.apply_rebaseline(event)?,
            Self::CompleteFaultClear { at_ms: _ } => state.complete_fault_clear()?,
            Self::ManageWithBaseline { event } => {
                state
                    .fleet_gate
                    .require_healthy()
                    .map_err(|error| error.to_string())?;
                if !event.condition.is_safe_manage_baseline() {
                    return Err("manage baseline condition is not an initial safe condition".into());
                }
                if state.managed.len() >= MAX_MANAGED_SIDS {
                    return Err(format!(
                        "managed-session count reached hard bound {MAX_MANAGED_SIDS}"
                    ));
                }
                if !state.managed.insert(event.sid.clone()) {
                    return Err(format!("sid {:?} is already managed", event.sid));
                }
                state.apply_initial_event(event)?;
            }
        }
        Ok(())
    }
}

fn tokens_equal(left: &ClaimToken, right: &ClaimToken) -> bool {
    left.0
        .as_bytes()
        .iter()
        .zip(right.0.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
        && left.0.len() == right.0.len()
}

fn validate_action_class(value: &str) -> Result<(), OperatorError> {
    validate_text("action class", value, MAX_ACTION_CLASS_BYTES, false)?;
    if value != "turn" {
        return Err(OperatorError::InvalidInput(
            "the operator actuator supports only the bounded 'turn' action class".into(),
        ));
    }
    Ok(())
}

fn validate_action_hash(value: &str) -> Result<(), OperatorError> {
    if value.len() != ACTION_HASH_HEX_LEN
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(OperatorError::InvalidInput(
            "action hash must be a lowercase 32-byte SHA-256 hex digest".into(),
        ));
    }
    Ok(())
}

#[derive(Default)]
struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn byte(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, value: &str) -> Result<(), OperatorError> {
        let length = u32::try_from(value.len())
            .map_err(|_| OperatorError::InvalidInput("WAL string is too long".into()))?;
        self.u32(length);
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }

    fn token(&mut self, token: &ClaimToken) -> Result<(), OperatorError> {
        self.string(token.expose())
    }

    fn generation(&mut self, generation: EventGeneration) {
        self.u64(generation.lifecycle_epoch);
        self.byte(u8::from(generation.alternate_screen));
        self.u64(generation.content_seq);
        self.bytes.extend_from_slice(&generation.fingerprint);
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or_else(|| "WAL field offset overflow".to_string())?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or_else(|| "WAL payload ends inside a field".to_string())?;
        self.cursor = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn boolean(&mut self) -> Result<bool, String> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(format!("invalid boolean byte {value}")),
        }
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| "invalid u32 field".to_string())?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let bytes: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| "invalid u64 field".to_string())?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn string(&mut self, max: usize, allow_empty: bool, label: &str) -> Result<String, String> {
        let length = usize::try_from(self.u32()?)
            .map_err(|_| format!("{label} length does not fit usize"))?;
        if length > max || (!allow_empty && length == 0) {
            return Err(format!("{label} length {length} is outside its bound"));
        }
        let value = std::str::from_utf8(self.take(length)?)
            .map_err(|_| format!("{label} is not UTF-8"))?
            .to_string();
        Ok(value)
    }

    fn token(&mut self) -> Result<ClaimToken, String> {
        let value = self.string(CLAIM_TOKEN_HEX_LEN, false, "claim token")?;
        ClaimToken::from_wal(value)
    }

    fn event_id(&mut self) -> Result<EventId, String> {
        let value = self.u64()?;
        if value == 0 {
            return Err("event id zero is reserved".into());
        }
        Ok(EventId(value))
    }

    fn generation(&mut self) -> Result<EventGeneration, String> {
        let lifecycle_epoch = self.u64()?;
        let alternate_screen = self.boolean()?;
        let content_seq = self.u64()?;
        let fingerprint: [u8; 32] = self
            .take(32)?
            .try_into()
            .map_err(|_| "invalid generation fingerprint".to_string())?;
        Ok(EventGeneration {
            lifecycle_epoch,
            alternate_screen,
            content_seq,
            fingerprint,
        })
    }

    fn finish(self) -> Result<(), String> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(format!(
                "WAL payload has {} trailing bytes",
                self.bytes.len() - self.cursor
            ))
        }
    }
}

fn encode_record(record: &WalRecord) -> Result<Vec<u8>, OperatorError> {
    let mut encoder = Encoder::default();
    match record {
        WalRecord::Epoch {
            epoch,
            redelivery_cap,
        } => {
            encoder.u64(*epoch);
            encoder.u32(*redelivery_cap);
        }
        WalRecord::Manage { sid } => encoder.string(sid)?,
        WalRecord::Unmanage { sid, at_ms } => {
            encoder.string(sid)?;
            encoder.u64(*at_ms);
        }
        WalRecord::Enqueue { event } => {
            encoder.u64(event.id.get());
            encoder.string(&event.sid)?;
            encoder.generation(event.generation);
            encoder.byte(event.condition.to_tag());
        }
        WalRecord::Coalesce {
            event_id,
            condition,
        } => {
            encoder.u64(event_id.get());
            encoder.byte(condition.to_tag());
        }
        WalRecord::Claim {
            event_id,
            token,
            claim_epoch,
            claimed_at_ms,
            expires_at_ms,
        } => {
            encoder.u64(event_id.get());
            encoder.token(token)?;
            encoder.u64(*claim_epoch);
            encoder.u64(*claimed_at_ms);
            encoder.u64(*expires_at_ms);
        }
        WalRecord::Extend {
            event_id,
            token,
            expires_at_ms,
            cumulative_extension_ms,
        } => {
            encoder.u64(event_id.get());
            encoder.token(token)?;
            encoder.u64(*expires_at_ms);
            encoder.u64(*cumulative_extension_ms);
        }
        WalRecord::Expire {
            event_id,
            token,
            at_ms,
            redelivery_count,
            became_escalated,
        } => {
            encoder.u64(event_id.get());
            encoder.token(token)?;
            encoder.u64(*at_ms);
            encoder.u32(*redelivery_count);
            encoder.byte(u8::from(*became_escalated));
        }
        WalRecord::Resolve {
            event_id,
            token,
            resolution,
            resolved_at_ms,
        } => {
            encoder.u64(event_id.get());
            encoder.token(token)?;
            encoder.byte(resolution.to_tag());
            encoder.u64(*resolved_at_ms);
        }
        WalRecord::Conflict {
            event_id,
            token,
            reason,
            at_ms,
        } => {
            encoder.u64(event_id.get());
            encoder.token(token)?;
            encoder.string(reason)?;
            encoder.u64(*at_ms);
        }
        WalRecord::BeginAction {
            event_id,
            token,
            action_class,
            action_hash,
            intent_at_ms,
        } => {
            encoder.u64(event_id.get());
            encoder.token(token)?;
            encoder.string(action_class)?;
            encoder.string(action_hash)?;
            encoder.u64(*intent_at_ms);
        }
        WalRecord::FinishAction {
            event_id,
            token,
            action_hash,
            result,
            resolution,
            resolved_at_ms,
        } => {
            encoder.u64(event_id.get());
            encoder.token(token)?;
            encoder.string(action_hash)?;
            encoder.string(result)?;
            encoder.byte(resolution.to_tag());
            encoder.u64(*resolved_at_ms);
        }
        WalRecord::MarkActionInDoubt {
            event_id,
            token,
            reason,
            at_ms,
        } => {
            encoder.u64(event_id.get());
            encoder.token(token)?;
            encoder.string(reason)?;
            encoder.u64(*at_ms);
        }
        WalRecord::ReconcileInDoubt {
            event_id,
            token,
            resolution,
            note,
            resolved_at_ms,
        } => {
            encoder.u64(event_id.get());
            encoder.token(token)?;
            encoder.byte(resolution.to_tag());
            encoder.string(note)?;
            encoder.u64(*resolved_at_ms);
        }
        WalRecord::InvalidateDelivered {
            event_id,
            token,
            claim_epoch,
            condition,
        } => {
            encoder.u64(event_id.get());
            encoder.token(token)?;
            encoder.u64(*claim_epoch);
            encoder.byte(condition.to_tag());
        }
        WalRecord::LatchFleetFault { fault } => {
            encoder.byte(fault.reason.to_tag());
            encoder.u64(fault.fault_epoch);
            encoder.u64(fault.latched_at_ms);
        }
        WalRecord::BeginFaultClear { at_ms } | WalRecord::CompleteFaultClear { at_ms } => {
            encoder.u64(*at_ms)
        }
        WalRecord::Rebaseline { event } => {
            encoder.u64(event.id.get());
            encoder.string(&event.sid)?;
            encoder.generation(event.generation);
            encoder.byte(event.condition.to_tag());
        }
        WalRecord::ManageWithBaseline { event } => {
            encoder.u64(event.id.get());
            encoder.string(&event.sid)?;
            encoder.generation(event.generation);
            encoder.byte(event.condition.to_tag());
        }
    }
    if encoder.bytes.len() > MAX_WAL_PAYLOAD_BYTES {
        return Err(OperatorError::InvalidInput(format!(
            "WAL payload exceeds {MAX_WAL_PAYLOAD_BYTES} bytes"
        )));
    }
    Ok(encoder.bytes)
}

fn decode_record(kind: u16, payload: &[u8]) -> Result<WalRecord, String> {
    let mut decoder = Decoder::new(payload);
    let record = match kind {
        1 => WalRecord::Epoch {
            epoch: decoder.u64()?,
            redelivery_cap: decoder.u32()?,
        },
        2 => {
            let sid = decoder.string(MAX_SID_BYTES, false, "sid")?;
            validate_text("sid", &sid, MAX_SID_BYTES, false).map_err(|error| error.to_string())?;
            WalRecord::Manage { sid }
        }
        3 => {
            let sid = decoder.string(MAX_SID_BYTES, false, "sid")?;
            validate_text("sid", &sid, MAX_SID_BYTES, false).map_err(|error| error.to_string())?;
            WalRecord::Unmanage {
                sid,
                at_ms: decoder.u64()?,
            }
        }
        4 => {
            let id = decoder.event_id()?;
            let sid = decoder.string(MAX_SID_BYTES, false, "sid")?;
            let generation = decoder.generation()?;
            let condition = AttentionCondition::from_tag(decoder.byte()?)?;
            validate_text("sid", &sid, MAX_SID_BYTES, false).map_err(|error| error.to_string())?;
            WalRecord::Enqueue {
                event: StoredEvent {
                    id,
                    sid,
                    generation,
                    condition,
                    redelivery_count: 0,
                    escalated: false,
                    status: StoredStatus::Queued,
                },
            }
        }
        5 => {
            let event_id = decoder.event_id()?;
            let condition = AttentionCondition::from_tag(decoder.byte()?)?;
            WalRecord::Coalesce {
                event_id,
                condition,
            }
        }
        6 => WalRecord::Claim {
            event_id: decoder.event_id()?,
            token: decoder.token()?,
            claim_epoch: decoder.u64()?,
            claimed_at_ms: decoder.u64()?,
            expires_at_ms: decoder.u64()?,
        },
        7 => WalRecord::Extend {
            event_id: decoder.event_id()?,
            token: decoder.token()?,
            expires_at_ms: decoder.u64()?,
            cumulative_extension_ms: decoder.u64()?,
        },
        8 => WalRecord::Expire {
            event_id: decoder.event_id()?,
            token: decoder.token()?,
            at_ms: decoder.u64()?,
            redelivery_count: decoder.u32()?,
            became_escalated: decoder.boolean()?,
        },
        9 => WalRecord::Resolve {
            event_id: decoder.event_id()?,
            token: decoder.token()?,
            resolution: Resolution::from_tag(decoder.byte()?)?,
            resolved_at_ms: decoder.u64()?,
        },
        10 => {
            let event_id = decoder.event_id()?;
            let token = decoder.token()?;
            let reason = decoder.string(MAX_REASON_BYTES, false, "in-doubt reason")?;
            validate_evidence("in-doubt reason", &reason, MAX_REASON_BYTES, false)
                .map_err(|error| error.to_string())?;
            WalRecord::Conflict {
                event_id,
                token,
                reason,
                at_ms: decoder.u64()?,
            }
        }
        11 => {
            let event_id = decoder.event_id()?;
            let token = decoder.token()?;
            let action_class = decoder.string(MAX_ACTION_CLASS_BYTES, false, "action class")?;
            let action_hash = decoder.string(ACTION_HASH_HEX_LEN, false, "action hash")?;
            validate_action_class(&action_class).map_err(|error| error.to_string())?;
            validate_action_hash(&action_hash).map_err(|error| error.to_string())?;
            WalRecord::BeginAction {
                event_id,
                token,
                action_class,
                action_hash,
                intent_at_ms: decoder.u64()?,
            }
        }
        12 => {
            let event_id = decoder.event_id()?;
            let token = decoder.token()?;
            let action_hash = decoder.string(ACTION_HASH_HEX_LEN, false, "action hash")?;
            let result = decoder.string(MAX_ACTION_RESULT_BYTES, true, "action result")?;
            validate_action_hash(&action_hash).map_err(|error| error.to_string())?;
            validate_evidence("action result", &result, MAX_ACTION_RESULT_BYTES, true)
                .map_err(|error| error.to_string())?;
            WalRecord::FinishAction {
                event_id,
                token,
                action_hash,
                result,
                resolution: Resolution::from_tag(decoder.byte()?)?,
                resolved_at_ms: decoder.u64()?,
            }
        }
        13 => {
            let event_id = decoder.event_id()?;
            let token = decoder.token()?;
            let reason = decoder.string(MAX_REASON_BYTES, false, "in-doubt reason")?;
            validate_evidence("in-doubt reason", &reason, MAX_REASON_BYTES, false)
                .map_err(|error| error.to_string())?;
            WalRecord::MarkActionInDoubt {
                event_id,
                token,
                reason,
                at_ms: decoder.u64()?,
            }
        }
        14 => {
            let event_id = decoder.event_id()?;
            let token = decoder.token()?;
            let resolution = Resolution::from_tag(decoder.byte()?)?;
            let note = decoder.string(MAX_REASON_BYTES, false, "human reconciliation note")?;
            validate_evidence("human reconciliation note", &note, MAX_REASON_BYTES, false)
                .map_err(|error| error.to_string())?;
            WalRecord::ReconcileInDoubt {
                event_id,
                token,
                resolution,
                note,
                resolved_at_ms: decoder.u64()?,
            }
        }
        15 => {
            let event_id = decoder.event_id()?;
            let token = decoder.token()?;
            let claim_epoch = decoder.u64()?;
            let condition = AttentionCondition::from_tag(decoder.byte()?)?;
            WalRecord::InvalidateDelivered {
                event_id,
                token,
                claim_epoch,
                condition,
            }
        }
        16 => WalRecord::LatchFleetFault {
            fault: FleetFault {
                reason: FleetFaultReason::from_tag(decoder.byte()?)?,
                fault_epoch: decoder.u64()?,
                latched_at_ms: decoder.u64()?,
            },
        },
        17 => WalRecord::BeginFaultClear {
            at_ms: decoder.u64()?,
        },
        18 => {
            let id = decoder.event_id()?;
            let sid = decoder.string(MAX_SID_BYTES, false, "sid")?;
            let generation = decoder.generation()?;
            let condition = AttentionCondition::from_tag(decoder.byte()?)?;
            validate_text("sid", &sid, MAX_SID_BYTES, false).map_err(|error| error.to_string())?;
            WalRecord::Rebaseline {
                event: StoredEvent {
                    id,
                    sid,
                    generation,
                    condition,
                    redelivery_count: 0,
                    escalated: false,
                    status: StoredStatus::Queued,
                },
            }
        }
        19 => WalRecord::CompleteFaultClear {
            at_ms: decoder.u64()?,
        },
        20 => {
            let id = decoder.event_id()?;
            let sid = decoder.string(MAX_SID_BYTES, false, "sid")?;
            let generation = decoder.generation()?;
            let condition = AttentionCondition::from_tag(decoder.byte()?)?;
            validate_text("sid", &sid, MAX_SID_BYTES, false).map_err(|error| error.to_string())?;
            WalRecord::ManageWithBaseline {
                event: StoredEvent {
                    id,
                    sid,
                    generation,
                    condition,
                    redelivery_count: 0,
                    escalated: false,
                    status: StoredStatus::Queued,
                },
            }
        }
        _ => return Err(format!("unknown WAL record kind {kind}")),
    };
    decoder.finish()?;
    Ok(record)
}

fn encode_checkpoint_status(
    encoder: &mut Encoder,
    status: &StoredStatus,
) -> Result<(), OperatorError> {
    match status {
        StoredStatus::Queued => encoder.byte(0),
        StoredStatus::Delivered {
            token,
            claim_epoch,
            claimed_at_ms,
            expires_at_ms,
            cumulative_extension_ms,
        } => {
            encoder.byte(1);
            encoder.token(token)?;
            encoder.u64(*claim_epoch);
            encoder.u64(*claimed_at_ms);
            encoder.u64(*expires_at_ms);
            encoder.u64(*cumulative_extension_ms);
        }
        StoredStatus::ActionInFlight {
            token,
            claim_epoch,
            action_class,
            action_hash,
            intent_at_ms,
        } => {
            encoder.byte(2);
            encoder.token(token)?;
            encoder.u64(*claim_epoch);
            encoder.string(action_class)?;
            encoder.string(action_hash)?;
            encoder.u64(*intent_at_ms);
        }
        StoredStatus::Resolved {
            token,
            claim_epoch,
            resolution,
            resolved_at_ms,
            action,
            reconciliation_note,
        } => {
            encoder.byte(3);
            encoder.token(token)?;
            encoder.u64(*claim_epoch);
            encoder.byte(resolution.to_tag());
            encoder.u64(*resolved_at_ms);
            encoder.byte(u8::from(action.is_some()));
            if let Some(action) = action {
                encoder.string(&action.class)?;
                encoder.string(&action.hash)?;
                encoder.string(&action.result)?;
            }
            encoder.byte(u8::from(reconciliation_note.is_some()));
            if let Some(note) = reconciliation_note {
                encoder.string(note)?;
            }
        }
        StoredStatus::ResolvedUnclaimed {
            resolution,
            resolved_at_ms,
            reason,
        } => {
            encoder.byte(4);
            encoder.byte(resolution.to_tag());
            encoder.u64(*resolved_at_ms);
            encoder.string(reason)?;
        }
        StoredStatus::InDoubt {
            token,
            reason,
            at_ms,
        } => {
            encoder.byte(5);
            encoder.byte(u8::from(token.is_some()));
            if let Some(token) = token {
                encoder.token(token)?;
            }
            encoder.string(reason)?;
            encoder.u64(*at_ms);
        }
    }
    Ok(())
}

fn decode_checkpoint_status(decoder: &mut Decoder<'_>) -> Result<StoredStatus, String> {
    match decoder.byte()? {
        0 => Ok(StoredStatus::Queued),
        1 => Ok(StoredStatus::Delivered {
            token: decoder.token()?,
            claim_epoch: decoder.u64()?,
            claimed_at_ms: decoder.u64()?,
            expires_at_ms: decoder.u64()?,
            cumulative_extension_ms: decoder.u64()?,
        }),
        2 => {
            let token = decoder.token()?;
            let claim_epoch = decoder.u64()?;
            let action_class = decoder.string(MAX_ACTION_CLASS_BYTES, false, "action class")?;
            let action_hash = decoder.string(ACTION_HASH_HEX_LEN, false, "action hash")?;
            validate_action_class(&action_class).map_err(|error| error.to_string())?;
            validate_action_hash(&action_hash).map_err(|error| error.to_string())?;
            Ok(StoredStatus::ActionInFlight {
                token,
                claim_epoch,
                action_class,
                action_hash,
                intent_at_ms: decoder.u64()?,
            })
        }
        3 => {
            let token = decoder.token()?;
            let claim_epoch = decoder.u64()?;
            let resolution = Resolution::from_tag(decoder.byte()?)?;
            let resolved_at_ms = decoder.u64()?;
            let action = if decoder.boolean()? {
                let class = decoder.string(MAX_ACTION_CLASS_BYTES, false, "action class")?;
                let hash = decoder.string(ACTION_HASH_HEX_LEN, false, "action hash")?;
                let result = decoder.string(MAX_ACTION_RESULT_BYTES, true, "action result")?;
                validate_action_class(&class).map_err(|error| error.to_string())?;
                validate_action_hash(&hash).map_err(|error| error.to_string())?;
                validate_evidence("action result", &result, MAX_ACTION_RESULT_BYTES, true)
                    .map_err(|error| error.to_string())?;
                Some(CompletedAction {
                    class,
                    hash,
                    result,
                })
            } else {
                None
            };
            let reconciliation_note = if decoder.boolean()? {
                let note = decoder.string(MAX_REASON_BYTES, false, "reconciliation note")?;
                validate_evidence("reconciliation note", &note, MAX_REASON_BYTES, false)
                    .map_err(|error| error.to_string())?;
                Some(note)
            } else {
                None
            };
            Ok(StoredStatus::Resolved {
                token,
                claim_epoch,
                resolution,
                resolved_at_ms,
                action,
                reconciliation_note,
            })
        }
        4 => {
            let resolution = Resolution::from_tag(decoder.byte()?)?;
            let resolved_at_ms = decoder.u64()?;
            let reason = decoder.string(MAX_REASON_BYTES, false, "resolution reason")?;
            validate_evidence("resolution reason", &reason, MAX_REASON_BYTES, false)
                .map_err(|error| error.to_string())?;
            Ok(StoredStatus::ResolvedUnclaimed {
                resolution,
                resolved_at_ms,
                reason,
            })
        }
        5 => {
            let token = decoder.boolean()?.then(|| decoder.token()).transpose()?;
            let reason = decoder.string(MAX_REASON_BYTES, false, "in-doubt reason")?;
            validate_evidence("in-doubt reason", &reason, MAX_REASON_BYTES, false)
                .map_err(|error| error.to_string())?;
            Ok(StoredStatus::InDoubt {
                token,
                reason,
                at_ms: decoder.u64()?,
            })
        }
        tag => Err(format!("unknown checkpoint status tag {tag}")),
    }
}

fn encode_checkpoint_fault(encoder: &mut Encoder, fault: FleetFault) {
    encoder.byte(fault.reason.to_tag());
    encoder.u64(fault.fault_epoch);
    encoder.u64(fault.latched_at_ms);
}

fn decode_checkpoint_fault(decoder: &mut Decoder<'_>) -> Result<FleetFault, String> {
    Ok(FleetFault {
        reason: FleetFaultReason::from_tag(decoder.byte()?)?,
        fault_epoch: decoder.u64()?,
        latched_at_ms: decoder.u64()?,
    })
}

fn encode_checkpoint_fleet_gate(
    encoder: &mut Encoder,
    fleet_gate: &StoredFleetGate,
) -> Result<(), OperatorError> {
    match fleet_gate {
        StoredFleetGate::Healthy => encoder.byte(0),
        StoredFleetGate::Faulted(fault) => {
            encoder.byte(1);
            encode_checkpoint_fault(encoder, *fault);
        }
        StoredFleetGate::RebaselineRequired {
            fault,
            pending_sids,
        } => {
            encoder.byte(2);
            encode_checkpoint_fault(encoder, *fault);
            encoder.u32(u32::try_from(pending_sids.len()).map_err(|_| {
                OperatorError::InvariantViolation(
                    "fault-clear pending-session count does not fit u32".into(),
                )
            })?);
            for sid in pending_sids {
                encoder.string(sid)?;
            }
        }
    }
    Ok(())
}

fn decode_checkpoint_fleet_gate(
    decoder: &mut Decoder<'_>,
    managed: &BTreeSet<String>,
) -> Result<StoredFleetGate, String> {
    match decoder.byte()? {
        0 => Ok(StoredFleetGate::Healthy),
        1 => Ok(StoredFleetGate::Faulted(decode_checkpoint_fault(decoder)?)),
        2 => {
            let fault = decode_checkpoint_fault(decoder)?;
            let pending_count = usize::try_from(decoder.u32()?)
                .map_err(|_| "fault-clear pending-session count does not fit usize".to_string())?;
            if pending_count > MAX_MANAGED_SIDS || pending_count > managed.len() {
                return Err(format!(
                    "fault-clear pending-session count {pending_count} exceeds its bound"
                ));
            }
            let mut pending_sids = BTreeSet::new();
            for _ in 0..pending_count {
                let sid = decoder.string(MAX_SID_BYTES, false, "fault-clear pending sid")?;
                validate_text("sid", &sid, MAX_SID_BYTES, false)
                    .map_err(|error| error.to_string())?;
                if !managed.contains(&sid) {
                    return Err(format!("fault-clear pending sid {sid:?} is unmanaged"));
                }
                if !pending_sids.insert(sid) {
                    return Err("checkpoint repeats a fault-clear pending sid".into());
                }
            }
            Ok(StoredFleetGate::RebaselineRequired {
                fault,
                pending_sids,
            })
        }
        tag => Err(format!("unknown checkpoint fleet-gate tag {tag}")),
    }
}

fn encode_checkpoint_payload(state: &QueueState) -> Result<Vec<u8>, OperatorError> {
    let mut encoder = Encoder::default();
    encoder.u64(state.durable_epoch);
    encoder.u64(state.next_event_id);
    encoder.u64(state.next_record_sequence);
    encoder.u32(u32::try_from(state.managed.len()).map_err(|_| {
        OperatorError::InvariantViolation("managed-session count does not fit u32".into())
    })?);
    for sid in &state.managed {
        encoder.string(sid)?;
    }
    encoder.u32(u32::try_from(state.events.len()).map_err(|_| {
        OperatorError::InvariantViolation("checkpoint event count does not fit u32".into())
    })?);
    for event in state.events.values() {
        encoder.u64(event.id.get());
        encoder.string(&event.sid)?;
        encoder.generation(event.generation);
        encoder.byte(event.condition.to_tag());
        encoder.u32(event.redelivery_count);
        encoder.byte(u8::from(event.escalated));
        encode_checkpoint_status(&mut encoder, &event.status)?;
    }
    encoder.u32(u32::try_from(state.fair_sids.len()).map_err(|_| {
        OperatorError::InvariantViolation("fair-session count does not fit u32".into())
    })?);
    for sid in &state.fair_sids {
        encoder.string(sid)?;
        let ready = state.ready_by_sid.get(sid).ok_or_else(|| {
            OperatorError::InvariantViolation(format!("fair sid {sid:?} has no ready queue"))
        })?;
        encoder.u32(u32::try_from(ready.len()).map_err(|_| {
            OperatorError::InvariantViolation("ready-event count does not fit u32".into())
        })?);
        for event_id in ready {
            encoder.u64(event_id.get());
        }
    }
    encode_checkpoint_fleet_gate(&mut encoder, &state.fleet_gate)?;
    if encoder.bytes.len() > MAX_CHECKPOINT_BYTES {
        return Err(OperatorError::WalFull {
            limit: MAX_CHECKPOINT_BYTES as u64,
        });
    }
    Ok(encoder.bytes)
}

fn decode_checkpoint_payload(
    payload: &[u8],
    config: &QueueConfig,
    schema: u16,
) -> Result<QueueState, String> {
    let mut decoder = Decoder::new(payload);
    let durable_epoch = decoder.u64()?;
    let next_event_id = decoder.u64()?;
    let next_record_sequence = decoder.u64()?;
    let managed_count = usize::try_from(decoder.u32()?)
        .map_err(|_| "managed-session count does not fit usize".to_string())?;
    if managed_count > MAX_MANAGED_SIDS {
        return Err(format!(
            "managed-session count {managed_count} exceeds its bound"
        ));
    }
    let mut managed = BTreeSet::new();
    for _ in 0..managed_count {
        let sid = decoder.string(MAX_SID_BYTES, false, "sid")?;
        validate_text("sid", &sid, MAX_SID_BYTES, false).map_err(|error| error.to_string())?;
        if !managed.insert(sid) {
            return Err("checkpoint repeats a managed sid".into());
        }
    }
    let event_count = usize::try_from(decoder.u32()?)
        .map_err(|_| "event count does not fit usize".to_string())?;
    let max_events = config
        .capacity
        .checked_add(RESOLVED_RETENTION)
        .ok_or_else(|| "checkpoint event bound overflow".to_string())?;
    if event_count > max_events {
        return Err(format!(
            "event count {event_count} exceeds checkpoint bound {max_events}"
        ));
    }
    let mut events = BTreeMap::new();
    let mut generations = BTreeMap::new();
    for _ in 0..event_count {
        let id = decoder.event_id()?;
        let sid = decoder.string(MAX_SID_BYTES, false, "sid")?;
        validate_text("sid", &sid, MAX_SID_BYTES, false).map_err(|error| error.to_string())?;
        let generation = decoder.generation()?;
        let condition = AttentionCondition::from_tag(decoder.byte()?)?;
        let redelivery_count = decoder.u32()?;
        let escalated = decoder.boolean()?;
        let status = decode_checkpoint_status(&mut decoder)?;
        let event = StoredEvent {
            id,
            sid: sid.clone(),
            generation,
            condition,
            redelivery_count,
            escalated,
            status,
        };
        if events.insert(id, event).is_some() {
            return Err(format!("checkpoint repeats event {}", id.get()));
        }
        generations.insert((sid, generation), id);
    }
    let fair_count = usize::try_from(decoder.u32()?)
        .map_err(|_| "fair-session count does not fit usize".to_string())?;
    if fair_count > managed_count {
        return Err("fair-session count exceeds managed-session count".into());
    }
    let mut ready_by_sid = BTreeMap::new();
    let mut fair_sids = VecDeque::new();
    for _ in 0..fair_count {
        let sid = decoder.string(MAX_SID_BYTES, false, "fair sid")?;
        let ready_count = usize::try_from(decoder.u32()?)
            .map_err(|_| "ready-event count does not fit usize".to_string())?;
        if ready_count == 0 || ready_count > config.capacity {
            return Err("ready-event count is outside its bound".into());
        }
        let mut ready = VecDeque::new();
        for _ in 0..ready_count {
            ready.push_back(decoder.event_id()?);
        }
        if ready_by_sid.insert(sid.clone(), ready).is_some() {
            return Err("checkpoint repeats a fair sid".into());
        }
        fair_sids.push_back(sid);
    }
    let fleet_gate = if schema == LEGACY_CHECKPOINT_SCHEMA {
        StoredFleetGate::Healthy
    } else {
        decode_checkpoint_fleet_gate(&mut decoder, &managed)?
    };
    decoder.finish()?;
    let state = QueueState {
        durable_epoch,
        next_event_id,
        next_record_sequence,
        managed,
        events,
        generations,
        ready_by_sid,
        fair_sids,
        fleet_gate,
    };
    state.validate()?;
    Ok(state)
}

fn checkpoint_projection(state: &QueueState) -> Result<QueueState, OperatorError> {
    let retained_terminal: BTreeSet<EventId> = state
        .events
        .iter()
        .rev()
        .filter_map(|(id, event)| (!event.status.unresolved()).then_some(*id))
        .take(RESOLVED_RETENTION)
        .collect();
    let mut projected = state.clone();
    projected
        .events
        .retain(|id, event| event.status.unresolved() || retained_terminal.contains(id));
    projected
        .generations
        .retain(|_, id| projected.events.contains_key(id));
    projected
        .validate()
        .map_err(OperatorError::InvariantViolation)?;
    Ok(projected)
}

fn encode_checkpoint(state: &QueueState) -> Result<Vec<u8>, OperatorError> {
    let payload = encode_checkpoint_payload(state)?;
    let payload_len = u64::try_from(payload.len())
        .map_err(|_| OperatorError::InvariantViolation("checkpoint size overflow".into()))?;
    let mut header = [0_u8; CHECKPOINT_HEADER_LEN];
    header[0..4].copy_from_slice(&CHECKPOINT_MAGIC);
    header[4..6].copy_from_slice(&CHECKPOINT_SCHEMA.to_le_bytes());
    header[6..8].copy_from_slice(&0_u16.to_le_bytes());
    header[8..16].copy_from_slice(&payload_len.to_le_bytes());
    header[16..24].copy_from_slice(&state.next_record_sequence.to_le_bytes());
    header[24..32].copy_from_slice(&state.durable_epoch.to_le_bytes());
    let mut digest = Sha256::new();
    digest.update(header);
    digest.update(&payload);
    let checksum = digest.finalize();
    let mut bytes =
        Vec::with_capacity(CHECKPOINT_HEADER_LEN + payload.len() + CHECKPOINT_CHECKSUM_LEN);
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(&checksum);
    Ok(bytes)
}

fn corrupt_checkpoint(path: &Path, reason: impl Into<String>) -> OperatorError {
    OperatorError::CorruptCheckpoint {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

fn decode_checkpoint(
    path: &Path,
    expected_sequence: u64,
    config: &QueueConfig,
) -> Result<QueueState, OperatorError> {
    let mut file = open_private_regular_file(path)?;
    let file_len = usize::try_from(file.metadata()?.len())
        .map_err(|_| corrupt_checkpoint(path, "file length does not fit usize"))?;
    let maximum = CHECKPOINT_HEADER_LEN
        .checked_add(MAX_CHECKPOINT_BYTES)
        .and_then(|value| value.checked_add(CHECKPOINT_CHECKSUM_LEN))
        .ok_or_else(|| corrupt_checkpoint(path, "maximum file length overflow"))?;
    if file_len < CHECKPOINT_HEADER_LEN + CHECKPOINT_CHECKSUM_LEN || file_len > maximum {
        return Err(corrupt_checkpoint(
            path,
            format!("file length {file_len} is outside its bound"),
        ));
    }
    let mut bytes = vec![0_u8; file_len];
    file.read_exact(&mut bytes)?;
    let header: &[u8; CHECKPOINT_HEADER_LEN] = bytes[..CHECKPOINT_HEADER_LEN]
        .try_into()
        .map_err(|_| corrupt_checkpoint(path, "header has the wrong length"))?;
    if header[0..4] != CHECKPOINT_MAGIC {
        return Err(corrupt_checkpoint(path, "bad checkpoint magic"));
    }
    let schema = u16::from_le_bytes([header[4], header[5]]);
    if schema != LEGACY_CHECKPOINT_SCHEMA && schema != CHECKPOINT_SCHEMA {
        return Err(OperatorError::UnsupportedCheckpointSchema {
            path: path.to_path_buf(),
            schema,
        });
    }
    if header[6..8] != [0, 0] {
        return Err(corrupt_checkpoint(path, "nonzero reserved header bits"));
    }
    let payload_len = usize::try_from(u64::from_le_bytes(
        header[8..16]
            .try_into()
            .map_err(|_| corrupt_checkpoint(path, "invalid payload length"))?,
    ))
    .map_err(|_| corrupt_checkpoint(path, "payload length does not fit usize"))?;
    if payload_len > MAX_CHECKPOINT_BYTES
        || file_len != CHECKPOINT_HEADER_LEN + payload_len + CHECKPOINT_CHECKSUM_LEN
    {
        return Err(corrupt_checkpoint(
            path,
            "payload length does not match file length",
        ));
    }
    let sequence = u64::from_le_bytes(
        header[16..24]
            .try_into()
            .map_err(|_| corrupt_checkpoint(path, "invalid sequence"))?,
    );
    if sequence != expected_sequence || sequence == 0 {
        return Err(corrupt_checkpoint(
            path,
            format!("header sequence {sequence} does not match filename {expected_sequence}"),
        ));
    }
    let payload = &bytes[CHECKPOINT_HEADER_LEN..CHECKPOINT_HEADER_LEN + payload_len];
    let checksum = &bytes[CHECKPOINT_HEADER_LEN + payload_len..];
    if !checksum_matches(header, payload, checksum) {
        return Err(corrupt_checkpoint(path, "checksum mismatch"));
    }
    let state = decode_checkpoint_payload(payload, config, schema)
        .map_err(|reason| corrupt_checkpoint(path, reason))?;
    let epoch = u64::from_le_bytes(
        header[24..32]
            .try_into()
            .map_err(|_| corrupt_checkpoint(path, "invalid epoch"))?,
    );
    if state.next_record_sequence != sequence || state.durable_epoch != epoch {
        return Err(corrupt_checkpoint(
            path,
            "header cursor/epoch differs from checkpoint state",
        ));
    }
    Ok(state)
}

fn checkpoint_sequence_from_name(path: &Path) -> Result<Option<u64>, OperatorError> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if name == CHECKPOINT_PENDING_NAME {
        return Ok(None);
    }
    let Some(suffix) = name.strip_prefix(CHECKPOINT_PREFIX) else {
        return Ok(None);
    };
    let sequence = suffix
        .parse::<u64>()
        .map_err(|_| corrupt_checkpoint(path, "checkpoint filename has a nonnumeric sequence"))?;
    if sequence == 0 {
        return Err(corrupt_checkpoint(
            path,
            "checkpoint sequence zero is reserved",
        ));
    }
    Ok(Some(sequence))
}

fn recover_checkpoint(
    directory: &Path,
    config: &QueueConfig,
) -> Result<(QueueState, Vec<PathBuf>), OperatorError> {
    let mut candidates = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if let Some(sequence) = checkpoint_sequence_from_name(&path)? {
            candidates.push((sequence, path));
        }
    }
    candidates.sort_by_key(|(sequence, _)| *sequence);
    let mut recovered = None;
    let mut paths = Vec::with_capacity(candidates.len());
    for (sequence, path) in candidates {
        // Every finalized checkpoint is authoritative evidence. A corrupt older
        // file is not silently ignored: that would turn storage corruption into
        // an apparent clean crash recovery.
        let state = decode_checkpoint(&path, sequence, config)?;
        recovered = Some(state);
        paths.push(path);
    }
    Ok((recovered.unwrap_or_default(), paths))
}

#[derive(Clone, Copy)]
struct WalHeader {
    kind: u16,
    payload_len: usize,
    sequence: u64,
    epoch: u64,
}

fn encode_frame(
    record: &WalRecord,
    sequence: u64,
    current_epoch: u64,
) -> Result<Vec<u8>, OperatorError> {
    let payload = encode_record(record)?;
    let payload_len = u32::try_from(payload.len())
        .map_err(|_| OperatorError::InvalidInput("WAL payload length overflow".into()))?;
    let mut header = [0_u8; WAL_HEADER_LEN];
    header[0..4].copy_from_slice(&WAL_MAGIC);
    header[4..6].copy_from_slice(&WAL_SCHEMA.to_le_bytes());
    header[6..8].copy_from_slice(&record.kind().to_le_bytes());
    header[8..12].copy_from_slice(&payload_len.to_le_bytes());
    header[12..16].copy_from_slice(&0_u32.to_le_bytes());
    header[16..24].copy_from_slice(&sequence.to_le_bytes());
    header[24..32].copy_from_slice(&record.header_epoch(current_epoch).to_le_bytes());
    let mut digest = Sha256::new();
    digest.update(header);
    digest.update(&payload);
    let checksum = digest.finalize();
    let mut frame = Vec::with_capacity(WAL_FRAME_OVERHEAD + payload.len());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&payload);
    frame.extend_from_slice(&checksum);
    Ok(frame)
}

fn decode_header(header: &[u8; WAL_HEADER_LEN], offset: u64) -> Result<WalHeader, OperatorError> {
    if header[0..4] != WAL_MAGIC {
        return Err(OperatorError::CorruptWal {
            offset,
            reason: "bad frame magic".into(),
        });
    }
    let schema = u16::from_le_bytes([header[4], header[5]]);
    if schema != WAL_SCHEMA {
        return Err(OperatorError::UnsupportedSchema { offset, schema });
    }
    let kind = u16::from_le_bytes([header[6], header[7]]);
    if !(1..=MAX_WAL_RECORD_KIND).contains(&kind) {
        return Err(OperatorError::CorruptWal {
            offset,
            reason: format!("unknown record kind {kind}"),
        });
    }
    let payload_len = usize::try_from(u32::from_le_bytes([
        header[8], header[9], header[10], header[11],
    ]))
    .map_err(|_| OperatorError::CorruptWal {
        offset,
        reason: "payload length does not fit usize".into(),
    })?;
    if payload_len > MAX_WAL_PAYLOAD_BYTES {
        return Err(OperatorError::CorruptWal {
            offset,
            reason: format!("payload length {payload_len} exceeds the hard frame bound"),
        });
    }
    if header[12..16] != [0, 0, 0, 0] {
        return Err(OperatorError::CorruptWal {
            offset,
            reason: "nonzero reserved header bits".into(),
        });
    }
    let sequence =
        u64::from_le_bytes(
            header[16..24]
                .try_into()
                .map_err(|_| OperatorError::CorruptWal {
                    offset,
                    reason: "invalid sequence field".into(),
                })?,
        );
    let epoch =
        u64::from_le_bytes(
            header[24..32]
                .try_into()
                .map_err(|_| OperatorError::CorruptWal {
                    offset,
                    reason: "invalid epoch field".into(),
                })?,
        );
    Ok(WalHeader {
        kind,
        payload_len,
        sequence,
        epoch,
    })
}

fn partial_header_plausible(bytes: &[u8], expected_sequence: u64) -> bool {
    let magic_len = bytes.len().min(WAL_MAGIC.len());
    if bytes[..magic_len] != WAL_MAGIC[..magic_len] {
        return false;
    }
    if bytes.len() >= 6 && u16::from_le_bytes([bytes[4], bytes[5]]) != WAL_SCHEMA {
        return false;
    }
    if bytes.len() >= 8 {
        let kind = u16::from_le_bytes([bytes[6], bytes[7]]);
        if !(1..=MAX_WAL_RECORD_KIND).contains(&kind) {
            return false;
        }
    }
    if bytes.len() >= 12 {
        let payload_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        if payload_len > MAX_WAL_PAYLOAD_BYTES {
            return false;
        }
    }
    if bytes.len() >= 16 && bytes[12..16] != [0, 0, 0, 0] {
        return false;
    }
    if bytes.len() >= 24 {
        let sequence = u64::from_le_bytes(bytes[16..24].try_into().unwrap_or([0; 8]));
        if sequence != expected_sequence {
            return false;
        }
    }
    true
}

fn checksum_matches(header: &[u8], payload: &[u8], actual: &[u8]) -> bool {
    let mut digest = Sha256::new();
    digest.update(header);
    digest.update(payload);
    let expected = digest.finalize();
    expected
        .iter()
        .zip(actual)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
        && actual.len() == WAL_CHECKSUM_LEN
}

fn recover_wal(
    file: &mut File,
    mut state: QueueState,
) -> Result<(QueueState, RecoveryReport, u64), OperatorError> {
    let file_len = file.metadata()?.len();
    file.seek(SeekFrom::Start(0))?;
    let mut offset = 0_u64;
    let mut records_replayed = 0_u64;
    let mut repaired = false;
    let checkpoint_sequence = state.next_record_sequence;
    let had_checkpoint = state.next_record_sequence != 1
        || state.durable_epoch != 0
        || !state.events.is_empty()
        || !state.managed.is_empty();
    let mut previous_wal_sequence = None;

    while offset < file_len {
        let remaining = file_len - offset;
        if remaining < WAL_HEADER_LEN as u64 {
            let length = usize::try_from(remaining).map_err(|_| OperatorError::CorruptWal {
                offset,
                reason: "partial header length does not fit usize".into(),
            })?;
            let mut partial = vec![0_u8; length];
            file.read_exact(&mut partial)?;
            let expected = previous_wal_sequence
                .and_then(|sequence: u64| sequence.checked_add(1))
                .unwrap_or(checkpoint_sequence);
            if !partial_header_plausible(&partial, expected) {
                return Err(OperatorError::CorruptWal {
                    offset,
                    reason: "invalid bytes after the final complete frame".into(),
                });
            }
            repaired = true;
            break;
        }

        let mut header_bytes = [0_u8; WAL_HEADER_LEN];
        file.read_exact(&mut header_bytes)?;
        let header = decode_header(&header_bytes, offset)?;
        let expected_wal_sequence =
            previous_wal_sequence.and_then(|sequence: u64| sequence.checked_add(1));
        if let Some(expected) = expected_wal_sequence
            && header.sequence != expected
        {
            return Err(OperatorError::CorruptWal {
                offset,
                reason: format!(
                    "record sequence {} does not equal expected {}",
                    header.sequence, expected
                ),
            });
        }
        if previous_wal_sequence.is_none()
            && ((!had_checkpoint && header.sequence != 1)
                || (had_checkpoint && header.sequence > checkpoint_sequence))
        {
            return Err(OperatorError::CorruptWal {
                offset,
                reason: format!(
                    "first WAL sequence {} cannot follow checkpoint cursor {checkpoint_sequence}",
                    header.sequence
                ),
            });
        }
        let frame_len = WAL_FRAME_OVERHEAD
            .checked_add(header.payload_len)
            .ok_or_else(|| OperatorError::CorruptWal {
                offset,
                reason: "frame length overflow".into(),
            })?;
        if remaining < frame_len as u64 {
            repaired = true;
            break;
        }
        let mut payload = vec![0_u8; header.payload_len];
        file.read_exact(&mut payload)?;
        let mut checksum = [0_u8; WAL_CHECKSUM_LEN];
        file.read_exact(&mut checksum)?;
        if !checksum_matches(&header_bytes, &payload, &checksum) {
            return Err(OperatorError::CorruptWal {
                offset,
                reason: "frame checksum mismatch".into(),
            });
        }
        let record = decode_record(header.kind, &payload)
            .map_err(|reason| OperatorError::CorruptWal { offset, reason })?;
        if header.sequence < state.next_record_sequence {
            // Crash between publishing a checkpoint and truncating its old WAL:
            // verify the covered frames but do not apply them twice.
            if header.epoch > state.durable_epoch {
                return Err(OperatorError::CorruptWal {
                    offset,
                    reason: "checkpoint-covered WAL frame has a future epoch".into(),
                });
            }
            if let WalRecord::Epoch { epoch, .. } = &record
                && *epoch != header.epoch
            {
                return Err(OperatorError::CorruptWal {
                    offset,
                    reason: "checkpoint-covered epoch frame disagrees with its header".into(),
                });
            }
        } else {
            if header.sequence != state.next_record_sequence {
                return Err(OperatorError::CorruptWal {
                    offset,
                    reason: format!(
                        "record sequence {} skips checkpoint/replay cursor {}",
                        header.sequence, state.next_record_sequence
                    ),
                });
            }
            let expected_epoch = record.header_epoch(state.durable_epoch);
            if header.epoch != expected_epoch || (record.kind() != 1 && state.durable_epoch == 0) {
                return Err(OperatorError::CorruptWal {
                    offset,
                    reason: format!(
                        "record epoch {} does not match expected {expected_epoch}",
                        header.epoch
                    ),
                });
            }
            record
                .apply(&mut state)
                .map_err(|reason| OperatorError::CorruptWal { offset, reason })?;
            state.next_record_sequence =
                state.next_record_sequence.checked_add(1).ok_or_else(|| {
                    OperatorError::CorruptWal {
                        offset,
                        reason: "record sequence exhausted".into(),
                    }
                })?;
            state
                .validate()
                .map_err(|reason| OperatorError::CorruptWal { offset, reason })?;
        }
        offset = offset
            .checked_add(frame_len as u64)
            .ok_or_else(|| OperatorError::CorruptWal {
                offset,
                reason: "WAL offset overflow".into(),
            })?;
        records_replayed =
            records_replayed
                .checked_add(1)
                .ok_or_else(|| OperatorError::CorruptWal {
                    offset,
                    reason: "record count overflow".into(),
                })?;
        previous_wal_sequence = Some(header.sequence);
    }

    if repaired {
        file.set_len(offset)?;
        file.sync_all()?;
    }
    file.seek(SeekFrom::Start(offset))?;
    state
        .validate()
        .map_err(OperatorError::InvariantViolation)?;
    let report = RecoveryReport {
        records_replayed,
        repaired_partial_final_frame: repaired,
        durable_epoch: state.durable_epoch,
    };
    Ok((state, report, offset))
}

struct LiveState {
    wal: File,
    wal_len: u64,
    state: QueueState,
    poisoned: bool,
    directory: PathBuf,
    checkpoint_paths: Vec<PathBuf>,
}

fn remove_pending_checkpoint(path: &Path) -> Result<(), OperatorError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata_is_link_like(&metadata) || !metadata.file_type().is_file() {
                return Err(corrupt_checkpoint(
                    path,
                    "pending checkpoint is not a real regular file",
                ));
            }
            fs::remove_file(path)?;
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(OperatorError::Io(error)),
    }
    Ok(())
}

fn create_private_new_file(path: &Path) -> Result<File, OperatorError> {
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let metadata = file.metadata()?;
        // SAFETY: getuid has no arguments and does not access Rust memory.
        let our_uid = unsafe { libc::getuid() };
        if !metadata.file_type().is_file()
            || metadata.uid() != our_uid
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(OperatorError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} must be a uid-{our_uid} regular file with mode 0600",
                    path.display()
                ),
            )));
        }
    }
    Ok(file)
}

fn compact_live(live: &mut LiveState) -> Result<(), OperatorError> {
    let projected = checkpoint_projection(&live.state)?;
    let bytes = encode_checkpoint(&projected)?;
    let pending_path = live.directory.join(CHECKPOINT_PENDING_NAME);
    remove_pending_checkpoint(&pending_path)?;
    let mut pending = create_private_new_file(&pending_path)?;
    pending.write_all(&bytes)?;
    pending.sync_all()?;

    let final_path = live.directory.join(format!(
        "{CHECKPOINT_PREFIX}{}",
        projected.next_record_sequence
    ));
    if fs::symlink_metadata(&final_path).is_ok() {
        return Err(corrupt_checkpoint(
            &final_path,
            "checkpoint cursor already exists",
        ));
    }
    fs::rename(&pending_path, &final_path)?;
    sync_directory(&live.directory)?;

    // The checkpoint is now authoritative. If a crash happens before or during
    // truncation, recovery verifies and skips any covered WAL prefix by sequence.
    live.wal.set_len(0)?;
    live.wal.sync_all()?;
    live.wal.seek(SeekFrom::Start(0))?;
    live.wal_len = 0;
    live.state = projected;

    let old_paths = std::mem::take(&mut live.checkpoint_paths);
    live.checkpoint_paths.push(final_path.clone());
    for old_path in old_paths {
        if old_path != final_path {
            fs::remove_file(old_path)?;
        }
    }
    sync_directory(&live.directory)?;
    Ok(())
}

fn commit_record(
    live: &mut LiveState,
    config: &QueueConfig,
    record: &WalRecord,
) -> Result<(), OperatorError> {
    if live.poisoned {
        return Err(OperatorError::WalPoisoned);
    }
    let mut next = live.state.clone();
    record
        .apply(&mut next)
        .map_err(OperatorError::InvariantViolation)?;
    next.next_record_sequence = next
        .next_record_sequence
        .checked_add(1)
        .ok_or_else(|| OperatorError::InvariantViolation("record sequence exhausted".into()))?;
    next.validate().map_err(OperatorError::InvariantViolation)?;
    let frame = encode_frame(
        record,
        live.state.next_record_sequence,
        live.state.durable_epoch,
    )?;
    let frame_len = u64::try_from(frame.len())
        .map_err(|_| OperatorError::InvalidInput("WAL frame length overflow".into()))?;
    let mut new_len = live
        .wal_len
        .checked_add(frame_len)
        .ok_or(OperatorError::WalFull {
            limit: config.max_wal_bytes,
        })?;
    let compact_at = config.max_wal_bytes / 2;
    let terminal_count = live
        .state
        .events
        .values()
        .filter(|event| !event.status.unresolved())
        .count();
    if live.wal_len > 0
        && (new_len > config.max_wal_bytes
            || live.wal_len >= compact_at
            || terminal_count >= RESOLVED_RETENTION.saturating_mul(2))
    {
        if let Err(error) = compact_live(live) {
            live.poisoned = true;
            return Err(error);
        }
        // `compact_live` may prune terminal history and its generation-index
        // entries. Rebase the pending transition on that durable projection;
        // using the pre-compaction clone here would resurrect pruned history in
        // memory and make live state diverge from reopen.
        next = live.state.clone();
        record
            .apply(&mut next)
            .map_err(OperatorError::InvariantViolation)?;
        next.next_record_sequence = next
            .next_record_sequence
            .checked_add(1)
            .ok_or_else(|| OperatorError::InvariantViolation("record sequence exhausted".into()))?;
        next.validate().map_err(OperatorError::InvariantViolation)?;
        new_len = frame_len;
    }
    if new_len > config.max_wal_bytes {
        return Err(OperatorError::WalFull {
            limit: config.max_wal_bytes,
        });
    }
    let write_result = (|| -> io::Result<()> {
        live.wal.seek(SeekFrom::Start(live.wal_len))?;
        live.wal.write_all(&frame)?;
        live.wal.sync_data()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        live.poisoned = true;
        return Err(OperatorError::Io(error));
    }
    live.wal_len = new_len;
    live.state = next;
    Ok(())
}

struct SharedQueue {
    config: QueueConfig,
    clock: Arc<AuthorityClock>,
    live: Mutex<LiveState>,
    _lock: ProcessLock,
    directory: PathBuf,
    recovery: RecoveryReport,
}

/// Cloneable, thread-safe handle to one process-locked durable operator queue.
#[derive(Clone)]
pub struct DurableQueue {
    shared: Arc<SharedQueue>,
}

/// Descriptive alias used by hosts that expose the queue as their operator.
pub type OperatorQueue = DurableQueue;

impl DurableQueue {
    /// Open a queue and fence its writes with `run_epoch`.
    pub fn open(
        directory: impl AsRef<Path>,
        run_epoch: u64,
        config: QueueConfig,
    ) -> Result<Self, OperatorError> {
        Self::open_with_report(directory, run_epoch, config).map(|(queue, _)| queue)
    }

    /// Open a queue and return the recovery facts observed before the new epoch.
    pub fn open_with_report(
        directory: impl AsRef<Path>,
        run_epoch: u64,
        config: QueueConfig,
    ) -> Result<(Self, RecoveryReport), OperatorError> {
        Self::open_internal(directory.as_ref(), Some(run_epoch), config)
    }

    /// Open a queue while atomically choosing the next durable run epoch.
    pub fn open_next_epoch(
        directory: impl AsRef<Path>,
        config: QueueConfig,
    ) -> Result<(Self, RecoveryReport), OperatorError> {
        Self::open_internal(directory.as_ref(), None, config)
    }

    /// Open the conventional private state directory for `fleet_id`.
    pub fn open_fleet(
        fleet_id: &str,
        run_epoch: u64,
        config: QueueConfig,
    ) -> Result<Self, OperatorError> {
        Self::open(fleet_state_dir(fleet_id)?, run_epoch, config)
    }

    fn open_internal(
        directory: &Path,
        requested_epoch: Option<u64>,
        config: QueueConfig,
    ) -> Result<(Self, RecoveryReport), OperatorError> {
        Self::open_internal_with_clock(
            directory,
            requested_epoch,
            config,
            Arc::new(AuthorityClock::process_local()),
        )
    }

    fn open_internal_with_clock(
        directory: &Path,
        requested_epoch: Option<u64>,
        config: QueueConfig,
        clock: Arc<AuthorityClock>,
    ) -> Result<(Self, RecoveryReport), OperatorError> {
        config.validate()?;
        if requested_epoch == Some(0) {
            return Err(OperatorError::InvalidInput(
                "run epoch zero is reserved".into(),
            ));
        }
        ensure_private_dir(directory)?;
        let lock_path = directory.join(LOCK_FILE_NAME);
        let process_lock = ProcessLock::acquire(&lock_path)?;
        let wal_path = directory.join(WAL_FILE_NAME);
        let mut wal = open_private_regular_file(&wal_path)?;
        let (checkpoint_state, checkpoint_paths) = recover_checkpoint(directory, &config)?;
        let (state, mut recovery, wal_len) = recover_wal(&mut wal, checkpoint_state)?;
        let epoch = match requested_epoch {
            Some(requested) if requested <= state.durable_epoch => {
                return Err(OperatorError::EpochRegression {
                    requested,
                    durable: state.durable_epoch,
                });
            }
            Some(requested) => requested,
            None => state
                .durable_epoch
                .checked_add(1)
                .ok_or_else(|| OperatorError::InvalidInput("run epoch exhausted".into()))?,
        };
        let mut live = LiveState {
            wal,
            wal_len,
            state,
            poisoned: false,
            directory: directory.to_path_buf(),
            checkpoint_paths,
        };
        if live.wal_len > config.max_wal_bytes
            || (live.wal_len > 0 && live.wal_len >= config.max_wal_bytes / 2)
        {
            compact_live(&mut live)?;
        }
        if epoch > live.state.durable_epoch {
            commit_record(
                &mut live,
                &config,
                &WalRecord::Epoch {
                    epoch,
                    redelivery_cap: config.redelivery_cap,
                },
            )?;
        }
        match live.state.fleet_gate.clone() {
            StoredFleetGate::Healthy => {
                if let Some(marker_reason) = read_fault_marker(directory)? {
                    let reason = ensure_fault_marker(directory, marker_reason)?;
                    let fault = FleetFault {
                        reason,
                        fault_epoch: live.state.durable_epoch,
                        latched_at_ms: clock.now_ms()?,
                    };
                    commit_record(&mut live, &config, &WalRecord::LatchFleetFault { fault })?;
                }
            }
            StoredFleetGate::Faulted(fault) | StoredFleetGate::RebaselineRequired { fault, .. } => {
                // A crash may occur after a checkpoint/WAL transition but
                // before the directory entry for a repaired marker is durable.
                // Recreate it before this handle can be returned.
                let _ = ensure_fault_marker(directory, fault.reason)?;
            }
        }
        let _ = reclaim_locked(&mut live, &config, clock.now_ms()?)?;
        recovery.durable_epoch = live.state.durable_epoch;
        let queue = Self {
            shared: Arc::new(SharedQueue {
                config,
                clock,
                live: Mutex::new(live),
                _lock: process_lock,
                directory: directory.to_path_buf(),
                recovery,
            }),
        };
        Ok((queue, recovery))
    }

    fn lock(&self) -> Result<MutexGuard<'_, LiveState>, OperatorError> {
        let guard = self
            .shared
            .live
            .lock()
            .map_err(|_| OperatorError::StatePoisoned)?;
        if guard.poisoned {
            Err(OperatorError::WalPoisoned)
        } else {
            Ok(guard)
        }
    }

    /// Directory containing the private lock and WAL files.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.shared.directory
    }

    /// Recovery facts from this handle's open operation.
    #[must_use]
    pub fn recovery_report(&self) -> RecoveryReport {
        self.shared.recovery
    }

    /// Synchronously write and sync the independent fail-closed marker without
    /// acquiring the queue's live-state mutex.
    ///
    /// This is the shutdown/last-resort path for a worker that may itself be
    /// wedged while holding that mutex. It may block on filesystem I/O, but it
    /// never waits for `LiveState`; the next open converts the marker into the
    /// ordinary durable fleet-fault transition before returning a usable handle.
    pub fn latch_fault_marker_without_live(
        &self,
        reason: FleetFaultReason,
    ) -> Result<FleetFaultReason, OperatorError> {
        ensure_fault_marker(&self.shared.directory, reason)
    }

    /// Current durable fencing epoch.
    pub fn durable_epoch(&self) -> Result<u64, OperatorError> {
        Ok(self.lock()?.state.durable_epoch)
    }

    /// Snapshot the durable fleet-wide safety gate.
    pub fn fleet_gate(&self) -> Result<FleetGateStatus, OperatorError> {
        Ok(self.lock()?.state.fleet_gate.snapshot())
    }

    /// Return the fault identity while faulted or awaiting fresh baselines.
    pub fn fleet_fault(&self) -> Result<Option<FleetFault>, OperatorError> {
        Ok(match &self.lock()?.state.fleet_gate {
            StoredFleetGate::Healthy => None,
            StoredFleetGate::Faulted(fault) | StoredFleetGate::RebaselineRequired { fault, .. } => {
                Some(*fault)
            }
        })
    }

    /// Atomically and without parking validate the exact durable action intent
    /// used by the host's final egress gate.
    ///
    /// The host additionally holds its actuator gate across this check and the
    /// bounded sink write, serializing host fault/unmanage operations with the
    /// actual terminal mutation. Queue contention is a safe `Busy` refusal;
    /// this method never waits behind observer WAL I/O.
    pub fn try_validate_action_permit(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        sid: &str,
        action_class: &str,
        action_hash: &str,
    ) -> Result<FinalActionPermit, OperatorError> {
        validate_text("sid", sid, MAX_SID_BYTES, false)?;
        validate_action_class(action_class)?;
        validate_action_hash(action_hash)?;
        let live = match self.shared.live.try_lock() {
            Ok(live) => live,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(FinalActionPermit::Busy),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(OperatorError::StatePoisoned);
            }
        };
        if live.poisoned
            || !matches!(live.state.fleet_gate, StoredFleetGate::Healthy)
            || !live.state.managed.contains(sid)
        {
            return Ok(FinalActionPermit::Revoked);
        }
        let Some(event) = live.state.events.get(&event_id) else {
            return Ok(FinalActionPermit::Revoked);
        };
        let StoredStatus::ActionInFlight {
            token: current,
            claim_epoch,
            action_class: current_class,
            action_hash: current_hash,
            ..
        } = &event.status
        else {
            return Ok(FinalActionPermit::Revoked);
        };
        if event.sid == sid
            && *claim_epoch == live.state.durable_epoch
            && tokens_equal(current, token)
            && current_class == action_class
            && current_hash == action_hash
        {
            Ok(FinalActionPermit::Granted)
        } else {
            Ok(FinalActionPermit::Revoked)
        }
    }

    /// Durably stop all new observation, claim, management-addition, and action
    /// intent transitions. The fixed reason set prevents terminal/model text
    /// from becoming durable fault metadata.
    pub fn latch_fault(
        &self,
        reason: FleetFaultReason,
    ) -> Result<FaultLatchOutcome, OperatorError> {
        let at_ms = match self.shared.clock.now_ms() {
            Ok(at_ms) => at_ms,
            Err(error) => {
                self.latch_fault_marker_without_live(reason)?;
                return Err(error);
            }
        };
        self.latch_fault_at(reason, at_ms)
    }

    /// Deterministic-clock form of [`Self::latch_fault`].
    pub fn latch_fault_at(
        &self,
        reason: FleetFaultReason,
        at_ms: u64,
    ) -> Result<FaultLatchOutcome, OperatorError> {
        let mut live = match self.shared.live.lock() {
            Ok(live) => live,
            Err(_) => {
                // The independent marker does not depend on the poisoned state
                // projection. Persist the caller's fixed, redacted reason before
                // reporting that the in-memory state cannot be trusted.
                self.latch_fault_marker_without_live(reason)?;
                return Err(OperatorError::StatePoisoned);
            }
        };
        if live.poisoned {
            let marker_reason = match &live.state.fleet_gate {
                StoredFleetGate::Healthy => reason,
                StoredFleetGate::Faulted(fault)
                | StoredFleetGate::RebaselineRequired { fault, .. } => fault.reason,
            };
            // `commit_record` cannot be used once the WAL outcome is uncertain,
            // but the separately synchronized marker remains available. Never
            // return WalPoisoned until that independent fail-closed fact exists.
            self.latch_fault_marker_without_live(marker_reason)?;
            return Err(OperatorError::WalPoisoned);
        }
        if let StoredFleetGate::Faulted(fault) = &live.state.fleet_gate {
            let fault = *fault;
            if let Err(error) = ensure_fault_marker(&live.directory, fault.reason) {
                live.poisoned = true;
                return Err(error);
            }
            return Ok(FaultLatchOutcome::AlreadyLatched(fault));
        }
        let reason = match ensure_fault_marker(&live.directory, reason) {
            Ok(reason) => reason,
            Err(error) => {
                live.poisoned = true;
                return Err(error);
            }
        };
        let fault = FleetFault {
            reason,
            fault_epoch: live.state.durable_epoch,
            latched_at_ms: at_ms,
        };
        if let Err(error) = commit_record(
            &mut live,
            &self.shared.config,
            &WalRecord::LatchFleetFault { fault },
        ) {
            // The independent marker may be the only durable fact after a
            // short/failed WAL append. This handle must not continue from its
            // still-healthy in-memory projection.
            live.poisoned = true;
            return Err(error);
        }
        Ok(FaultLatchOutcome::Latched(fault))
    }

    /// Begin the explicit human fault-clear protocol and return the sorted
    /// managed-session roster that still needs a fresh Changed baseline.
    /// Repeating the call while reconciliation is underway is idempotent.
    pub fn begin_fault_clear(&self) -> Result<Vec<String>, OperatorError> {
        self.begin_fault_clear_at(self.shared.clock.now_ms()?)
    }

    /// Deterministic-clock form of [`Self::begin_fault_clear`].
    pub fn begin_fault_clear_at(&self, at_ms: u64) -> Result<Vec<String>, OperatorError> {
        let mut live = self.lock()?;
        match &live.state.fleet_gate {
            StoredFleetGate::Healthy => return Err(OperatorError::FleetNotFaulted),
            StoredFleetGate::RebaselineRequired { pending_sids, .. } => {
                return Ok(pending_sids.iter().cloned().collect());
            }
            StoredFleetGate::Faulted(_) => {}
        }
        commit_record(
            &mut live,
            &self.shared.config,
            &WalRecord::BeginFaultClear { at_ms },
        )?;
        let StoredFleetGate::RebaselineRequired { pending_sids, .. } = &live.state.fleet_gate
        else {
            return Err(OperatorError::InvariantViolation(
                "fault clear did not enter rebaseline state".into(),
            ));
        };
        Ok(pending_sids.iter().cloned().collect())
    }

    /// Durably install one fresh, exact Changed baseline during an explicit
    /// human fault clear. Normal observation enqueue remains blocked until all
    /// roster entries are reconciled and the clear is completed.
    pub fn enqueue_rebaseline(&self, event: NewEvent) -> Result<EventId, OperatorError> {
        event.validate()?;
        if event.condition != AttentionCondition::Changed {
            return Err(OperatorError::InvalidInput(
                "fault-clear baseline condition must be Changed".into(),
            ));
        }
        let mut live = self.lock()?;
        let StoredFleetGate::RebaselineRequired { pending_sids, .. } = &live.state.fleet_gate
        else {
            return match &live.state.fleet_gate {
                StoredFleetGate::Healthy => Err(OperatorError::FleetNotFaulted),
                StoredFleetGate::Faulted(fault) => Err(OperatorError::FleetFaulted(fault.reason)),
                StoredFleetGate::RebaselineRequired { .. } => unreachable!(),
            };
        };
        if !pending_sids.contains(&event.sid) {
            return Err(OperatorError::RebaselineSidNotPending(event.sid));
        }
        if live.state.unresolved_len() >= self.shared.config.capacity {
            return Err(OperatorError::QueueFull {
                capacity: self.shared.config.capacity,
            });
        }
        let id = EventId(live.state.next_event_id);
        let stored = StoredEvent {
            id,
            sid: event.sid,
            generation: event.generation,
            condition: event.condition,
            redelivery_count: 0,
            escalated: false,
            status: StoredStatus::Queued,
        };
        commit_record(
            &mut live,
            &self.shared.config,
            &WalRecord::Rebaseline { event: stored },
        )?;
        Ok(id)
    }

    /// Complete a human-confirmed fault clear after every managed SID has a
    /// fresh baseline (or was explicitly unmanaged) and no action is in-doubt.
    pub fn complete_fault_clear(&self) -> Result<(), OperatorError> {
        self.complete_fault_clear_at(self.shared.clock.now_ms()?)
    }

    /// Deterministic-clock form of [`Self::complete_fault_clear`].
    pub fn complete_fault_clear_at(&self, at_ms: u64) -> Result<(), OperatorError> {
        let mut live = self.lock()?;
        match &live.state.fleet_gate {
            StoredFleetGate::Healthy => return Err(OperatorError::FleetNotFaulted),
            StoredFleetGate::Faulted(fault) => {
                return Err(OperatorError::FleetFaulted(fault.reason));
            }
            StoredFleetGate::RebaselineRequired { pending_sids, .. }
                if !pending_sids.is_empty() =>
            {
                return Err(OperatorError::RebaselineRequired {
                    remaining: pending_sids.len(),
                });
            }
            StoredFleetGate::RebaselineRequired { .. } => {}
        }
        if let Some(event) = live
            .state
            .events
            .values()
            .find(|event| matches!(event.status, StoredStatus::InDoubt { .. }))
        {
            return Err(OperatorError::EventInDoubt(event.id));
        }
        if let Err(error) = commit_record(
            &mut live,
            &self.shared.config,
            &WalRecord::CompleteFaultClear { at_ms },
        ) {
            live.poisoned = true;
            return Err(error);
        }
        if let Err(error) = remove_fault_marker(&live.directory) {
            // The complete record is durable, but a retained/uncertain marker
            // makes reopen latch a new fault. Keep this live handle from acting
            // until that idempotent, explicit recovery path has run.
            live.poisoned = true;
            return Err(error);
        }
        Ok(())
    }

    /// Persistently admit one session together with its first exact safe
    /// baseline as one WAL transition.
    ///
    /// `Ok(None)` means the SID was already managed and no new baseline was
    /// written. On every successful new admission, the authorization and its
    /// exact generation either both replay or neither does; an ambiguous WAL
    /// outcome poisons the handle rather than attempting a compensating unmanage.
    pub fn manage_with_baseline(&self, event: NewEvent) -> Result<Option<EventId>, OperatorError> {
        event.validate()?;
        if !event.condition.is_safe_manage_baseline() {
            return Err(OperatorError::InvalidInput(
                "manage baseline condition must be Changed, ApprovalRequired, or SessionExited"
                    .into(),
            ));
        }
        let mut live = self.lock()?;
        live.state.fleet_gate.require_healthy()?;
        if live.state.managed.contains(&event.sid) {
            return Ok(None);
        }
        if live.state.managed.len() >= MAX_MANAGED_SIDS {
            return Err(OperatorError::InvalidInput(format!(
                "managed-session allowlist reached its {MAX_MANAGED_SIDS}-entry bound"
            )));
        }
        if live.state.unresolved_len() >= self.shared.config.capacity {
            return Err(OperatorError::QueueFull {
                capacity: self.shared.config.capacity,
            });
        }
        let id = EventId(live.state.next_event_id);
        let stored = StoredEvent {
            id,
            sid: event.sid,
            generation: event.generation,
            condition: event.condition,
            redelivery_count: 0,
            escalated: false,
            status: StoredStatus::Queued,
        };
        commit_record(
            &mut live,
            &self.shared.config,
            &WalRecord::ManageWithBaseline { event: stored },
        )?;
        Ok(Some(id))
    }

    /// Persistently admit one session to the operator's privacy/actuation boundary.
    pub fn manage_sid(&self, sid: &str) -> Result<bool, OperatorError> {
        validate_text("sid", sid, MAX_SID_BYTES, false)?;
        let mut live = self.lock()?;
        live.state.fleet_gate.require_healthy()?;
        if live.state.managed.contains(sid) {
            return Ok(false);
        }
        if live.state.managed.len() >= MAX_MANAGED_SIDS {
            return Err(OperatorError::InvalidInput(format!(
                "managed-session allowlist reached its {MAX_MANAGED_SIDS}-entry bound"
            )));
        }
        commit_record(
            &mut live,
            &self.shared.config,
            &WalRecord::Manage {
                sid: sid.to_string(),
            },
        )?;
        Ok(true)
    }

    /// Persistently remove a managed session and revoke all unresolved claims.
    pub fn unmanage_sid(&self, sid: &str) -> Result<bool, OperatorError> {
        self.unmanage_sid_at(sid, self.shared.clock.now_ms()?)
    }

    /// Deterministic-clock form of [`Self::unmanage_sid`].
    pub fn unmanage_sid_at(&self, sid: &str, at_ms: u64) -> Result<bool, OperatorError> {
        validate_text("sid", sid, MAX_SID_BYTES, false)?;
        let mut live = self.lock()?;
        if !live.state.managed.contains(sid) {
            return Ok(false);
        }
        commit_record(
            &mut live,
            &self.shared.config,
            &WalRecord::Unmanage {
                sid: sid.to_string(),
                at_ms,
            },
        )?;
        Ok(true)
    }

    /// Whether `sid` is inside the explicit managed-session allowlist.
    pub fn is_managed(&self, sid: &str) -> Result<bool, OperatorError> {
        Ok(self.lock()?.state.managed.contains(sid))
    }

    /// Sorted snapshot of the durable managed-session allowlist.
    pub fn managed_sids(&self) -> Result<Vec<String>, OperatorError> {
        Ok(self.lock()?.state.managed.iter().cloned().collect())
    }

    /// Insert or strongest-condition-coalesce one exact observed generation.
    pub fn enqueue(&self, event: NewEvent) -> Result<EnqueueOutcome, OperatorError> {
        event.validate()?;
        let mut live = self.lock()?;
        live.state.fleet_gate.require_healthy()?;
        if !live.state.managed.contains(&event.sid) {
            return Ok(EnqueueOutcome::Unmanaged);
        }
        let key = (event.sid.clone(), event.generation);
        if let Some(event_id) = live.state.generations.get(&key).copied() {
            let (existing_condition, status) = {
                let existing = live.state.events.get(&event_id).ok_or_else(|| {
                    OperatorError::InvariantViolation("generation index is stale".into())
                })?;
                (existing.condition, existing.status.clone())
            };
            let strengthened = event.condition > existing_condition;
            let condition = existing_condition.max(event.condition);
            let changed = condition != existing_condition;
            let mut invalidated = false;
            if changed {
                let record = match status {
                    StoredStatus::Queued => Some(WalRecord::Coalesce {
                        event_id,
                        condition,
                    }),
                    StoredStatus::Delivered {
                        token, claim_epoch, ..
                    } => {
                        invalidated = true;
                        Some(WalRecord::InvalidateDelivered {
                            event_id,
                            token,
                            claim_epoch,
                            condition,
                        })
                    }
                    StoredStatus::ActionInFlight { .. }
                    | StoredStatus::Resolved { .. }
                    | StoredStatus::ResolvedUnclaimed { .. }
                    | StoredStatus::InDoubt { .. } => None,
                };
                if let Some(record) = record {
                    commit_record(&mut live, &self.shared.config, &record)?;
                }
            }
            return Ok(EnqueueOutcome::Coalesced {
                event_id,
                strengthened: strengthened
                    && (invalidated
                        || matches!(
                            live.state
                                .events
                                .get(&event_id)
                                .map(|stored| &stored.status),
                            Some(StoredStatus::Queued)
                        )),
            });
        }
        if live.state.unresolved_len() >= self.shared.config.capacity {
            return Err(OperatorError::QueueFull {
                capacity: self.shared.config.capacity,
            });
        }
        let id = EventId(live.state.next_event_id);
        let stored = StoredEvent {
            id,
            sid: event.sid,
            generation: event.generation,
            condition: event.condition,
            redelivery_count: 0,
            escalated: false,
            status: StoredStatus::Queued,
        };
        commit_record(
            &mut live,
            &self.shared.config,
            &WalRecord::Enqueue { event: stored },
        )?;
        Ok(EnqueueOutcome::Enqueued(id))
    }

    /// Fairly claim the next event using the process-local monotonic authority clock.
    pub fn claim(&self) -> Result<Option<Claim>, OperatorError> {
        self.claim_at(self.shared.clock.now_ms()?)
    }

    /// Deterministic-clock form of [`Self::claim`]. Expired deliveries are first
    /// reclaimed under the same mutex, then per-session round-robin fairness is used.
    pub fn claim_at(&self, now_ms: u64) -> Result<Option<Claim>, OperatorError> {
        let mut live = self.lock()?;
        live.state.fleet_gate.require_healthy()?;
        let _ = reclaim_locked(&mut live, &self.shared.config, now_ms)?;
        let Some(event_id) = live.state.next_ready() else {
            return Ok(None);
        };
        let token = ClaimToken::mint()?;
        let visibility_ms =
            duration_ms(self.shared.config.visibility_timeout, "visibility timeout")?;
        let expires_at_ms = now_ms
            .checked_add(visibility_ms)
            .ok_or_else(|| OperatorError::InvalidInput("claim expiry timestamp overflow".into()))?;
        let record = WalRecord::Claim {
            event_id,
            token: token.clone(),
            claim_epoch: live.state.durable_epoch,
            claimed_at_ms: now_ms,
            expires_at_ms,
        };
        commit_record(&mut live, &self.shared.config, &record)?;
        let event = live
            .state
            .events
            .get(&event_id)
            .ok_or_else(|| OperatorError::InvariantViolation("claimed event vanished".into()))?
            .snapshot();
        Ok(Some(Claim {
            event,
            token,
            expires_at_ms,
        }))
    }

    /// Extend a live claim, bounded by the configured cumulative cap.
    pub fn extend(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        additional: Duration,
    ) -> Result<ExtensionOutcome, OperatorError> {
        self.extend_at(event_id, token, additional, self.shared.clock.now_ms()?)
    }

    /// Deterministic-clock form of [`Self::extend`].
    pub fn extend_at(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        additional: Duration,
        now_ms: u64,
    ) -> Result<ExtensionOutcome, OperatorError> {
        let additional_ms = duration_ms(additional, "claim extension")?;
        let max_ms = duration_ms(
            self.shared.config.max_cumulative_extension,
            "maximum cumulative extension",
        )?;
        let mut live = self.lock()?;
        let current_epoch = live.state.durable_epoch;
        let event = live
            .state
            .events
            .get(&event_id)
            .ok_or(OperatorError::EventNotFound(event_id))?;
        let StoredStatus::Delivered {
            token: current,
            claim_epoch,
            expires_at_ms,
            cumulative_extension_ms,
            ..
        } = &event.status
        else {
            return Err(status_claim_error(event_id, &event.status));
        };
        if *claim_epoch != current_epoch || !tokens_equal(current, token) {
            return Err(OperatorError::StaleClaim(event_id));
        }
        if now_ms >= *expires_at_ms {
            return Err(OperatorError::ClaimExpired(event_id));
        }
        let new_cumulative = cumulative_extension_ms
            .checked_add(additional_ms)
            .ok_or(OperatorError::ExtensionLimit(event_id))?;
        if new_cumulative > max_ms {
            return Err(OperatorError::ExtensionLimit(event_id));
        }
        let new_expiry = expires_at_ms.checked_add(additional_ms).ok_or_else(|| {
            OperatorError::InvalidInput("extended expiry timestamp overflow".into())
        })?;
        let record = WalRecord::Extend {
            event_id,
            token: token.clone(),
            expires_at_ms: new_expiry,
            cumulative_extension_ms: new_cumulative,
        };
        commit_record(&mut live, &self.shared.config, &record)?;
        Ok(ExtensionOutcome {
            expires_at_ms: new_expiry,
            cumulative_extension_ms: new_cumulative,
        })
    }

    /// Reclaim every claim whose visibility deadline has elapsed.
    pub fn reclaim_expired(&self) -> Result<Vec<ExpiryOutcome>, OperatorError> {
        self.reclaim_expired_at(self.shared.clock.now_ms()?)
    }

    /// Deterministic-clock form of [`Self::reclaim_expired`].
    pub fn reclaim_expired_at(&self, now_ms: u64) -> Result<Vec<ExpiryOutcome>, OperatorError> {
        let mut live = self.lock()?;
        reclaim_locked(&mut live, &self.shared.config, now_ms)
    }

    /// Compare-and-set acknowledgement using the monotonic authority clock.
    pub fn ack(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        resolution: Resolution,
    ) -> Result<AckOutcome, OperatorError> {
        self.ack_at(event_id, token, resolution, self.shared.clock.now_ms()?)
    }

    /// Deterministic-clock form of [`Self::ack`].
    pub fn ack_at(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        resolution: Resolution,
        now_ms: u64,
    ) -> Result<AckOutcome, OperatorError> {
        if resolution == Resolution::Acted {
            return Err(OperatorError::InvalidInput(
                "acted resolution requires a durable action intent/result transaction".into(),
            ));
        }
        let mut live = self.lock()?;
        let current_epoch = live.state.durable_epoch;
        let status = live
            .state
            .events
            .get(&event_id)
            .ok_or(OperatorError::EventNotFound(event_id))?
            .status
            .clone();
        match status {
            StoredStatus::Delivered {
                token: current,
                claim_epoch,
                expires_at_ms,
                ..
            } => {
                if claim_epoch != current_epoch || !tokens_equal(&current, token) {
                    return Err(OperatorError::StaleClaim(event_id));
                }
                if now_ms >= expires_at_ms {
                    return Err(OperatorError::ClaimExpired(event_id));
                }
                commit_record(
                    &mut live,
                    &self.shared.config,
                    &WalRecord::Resolve {
                        event_id,
                        token: token.clone(),
                        resolution,
                        resolved_at_ms: now_ms,
                    },
                )?;
                Ok(AckOutcome::Resolved)
            }
            StoredStatus::Resolved {
                token: current,
                claim_epoch,
                resolution: durable,
                ..
            } => {
                if !tokens_equal(&current, token) {
                    return Err(OperatorError::StaleClaim(event_id));
                }
                if durable == resolution {
                    return Ok(AckOutcome::AlreadyResolved);
                }
                if claim_epoch != current_epoch {
                    return Err(OperatorError::StaleClaim(event_id));
                }
                commit_record(
                    &mut live,
                    &self.shared.config,
                    &WalRecord::Conflict {
                        event_id,
                        token: token.clone(),
                        reason: format!(
                            "conflicting acknowledgements: durable={durable:?} incoming={resolution:?}"
                        ),
                        at_ms: now_ms,
                    },
                )?;
                Err(OperatorError::ResolutionConflict(event_id))
            }
            StoredStatus::ActionInFlight { token: current, .. } => {
                if tokens_equal(&current, token) {
                    Err(OperatorError::ActionInFlight(event_id))
                } else {
                    Err(OperatorError::StaleClaim(event_id))
                }
            }
            StoredStatus::InDoubt { .. } => Err(OperatorError::EventInDoubt(event_id)),
            StoredStatus::ResolvedUnclaimed { .. } => Err(OperatorError::AlreadyResolved(event_id)),
            StoredStatus::Queued => Err(OperatorError::StaleClaim(event_id)),
        }
    }

    /// Durably write an action intent before the caller performs any side effect.
    pub fn begin_action(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        action_class: &str,
        action_hash: &str,
    ) -> Result<(), OperatorError> {
        self.begin_action_at(
            event_id,
            token,
            action_class,
            action_hash,
            self.shared.clock.now_ms()?,
        )
    }

    /// Deterministic-clock form of [`Self::begin_action`].
    pub fn begin_action_at(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        action_class: &str,
        action_hash: &str,
        now_ms: u64,
    ) -> Result<(), OperatorError> {
        validate_action_class(action_class)?;
        validate_action_hash(action_hash)?;
        let mut live = self.lock()?;
        live.state.fleet_gate.require_healthy()?;
        let current_epoch = live.state.durable_epoch;
        let event = live
            .state
            .events
            .get(&event_id)
            .ok_or(OperatorError::EventNotFound(event_id))?;
        if !live.state.managed.contains(&event.sid) {
            return Err(OperatorError::UnmanagedSid(event.sid.clone()));
        }
        let StoredStatus::Delivered {
            token: current,
            claim_epoch,
            expires_at_ms,
            ..
        } = &event.status
        else {
            return Err(status_claim_error(event_id, &event.status));
        };
        if *claim_epoch != current_epoch || !tokens_equal(current, token) {
            return Err(OperatorError::StaleClaim(event_id));
        }
        if now_ms >= *expires_at_ms {
            return Err(OperatorError::ClaimExpired(event_id));
        }
        commit_record(
            &mut live,
            &self.shared.config,
            &WalRecord::BeginAction {
                event_id,
                token: token.clone(),
                action_class: action_class.to_string(),
                action_hash: action_hash.to_string(),
                intent_at_ms: now_ms,
            },
        )
    }

    /// Durably stop an in-flight action when submission may have happened but
    /// its result cannot be verified. The action is never made claimable again.
    pub fn mark_action_in_doubt(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        reason: &str,
    ) -> Result<(), OperatorError> {
        self.mark_action_in_doubt_at(event_id, token, reason, self.shared.clock.now_ms()?)
    }

    /// Deterministic-clock form of [`Self::mark_action_in_doubt`].
    pub fn mark_action_in_doubt_at(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        reason: &str,
        now_ms: u64,
    ) -> Result<(), OperatorError> {
        validate_evidence("in-doubt reason", reason, MAX_REASON_BYTES, false)?;
        let mut live = self.lock()?;
        let current_epoch = live.state.durable_epoch;
        let status = live
            .state
            .events
            .get(&event_id)
            .ok_or(OperatorError::EventNotFound(event_id))?
            .status
            .clone();
        match status {
            StoredStatus::ActionInFlight {
                token: current,
                claim_epoch,
                ..
            } => {
                if claim_epoch != current_epoch || !tokens_equal(&current, token) {
                    return Err(OperatorError::StaleClaim(event_id));
                }
                commit_record(
                    &mut live,
                    &self.shared.config,
                    &WalRecord::MarkActionInDoubt {
                        event_id,
                        token: token.clone(),
                        reason: reason.to_string(),
                        at_ms: now_ms,
                    },
                )
            }
            StoredStatus::InDoubt {
                token: Some(current),
                ..
            } if tokens_equal(&current, token) => Ok(()),
            StoredStatus::InDoubt { .. } => Err(OperatorError::EventInDoubt(event_id)),
            StoredStatus::Resolved { .. } => Err(OperatorError::AlreadyResolved(event_id)),
            StoredStatus::ResolvedUnclaimed { .. } => Err(OperatorError::AlreadyResolved(event_id)),
            StoredStatus::Delivered { .. } => Err(OperatorError::ActionMismatch(event_id)),
            StoredStatus::Queued => Err(OperatorError::StaleClaim(event_id)),
        }
    }

    /// Human-only reconciliation of a token-scoped in-doubt event.
    ///
    /// This is deliberately separate from [`Self::ack`] and the autonomous
    /// actuator transaction. A human may choose any terminal resolution after
    /// inspecting external evidence; choosing [`Resolution::Acted`] records the
    /// human's assertion that the ambiguous submission did occur. It never
    /// authorizes the operator to replay that action. `note` is mandatory and is
    /// persisted with the resolution as audit evidence.
    pub fn reconcile_in_doubt(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        resolution: Resolution,
        note: &str,
    ) -> Result<AckOutcome, OperatorError> {
        self.reconcile_in_doubt_at(
            event_id,
            token,
            resolution,
            note,
            self.shared.clock.now_ms()?,
        )
    }

    /// Deterministic-clock form of [`Self::reconcile_in_doubt`].
    pub fn reconcile_in_doubt_at(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        resolution: Resolution,
        note: &str,
        now_ms: u64,
    ) -> Result<AckOutcome, OperatorError> {
        validate_evidence("human reconciliation note", note, MAX_REASON_BYTES, false)?;
        let mut live = self.lock()?;
        let status = live
            .state
            .events
            .get(&event_id)
            .ok_or(OperatorError::EventNotFound(event_id))?
            .status
            .clone();
        match status {
            StoredStatus::InDoubt {
                token: Some(current),
                ..
            } => {
                if !tokens_equal(&current, token) {
                    return Err(OperatorError::StaleClaim(event_id));
                }
                commit_record(
                    &mut live,
                    &self.shared.config,
                    &WalRecord::ReconcileInDoubt {
                        event_id,
                        token: token.clone(),
                        resolution,
                        note: note.to_string(),
                        resolved_at_ms: now_ms,
                    },
                )?;
                Ok(AckOutcome::Resolved)
            }
            StoredStatus::InDoubt { token: None, .. } => {
                Err(OperatorError::TokenlessInDoubt(event_id))
            }
            StoredStatus::Resolved {
                token: current,
                resolution: durable_resolution,
                reconciliation_note: Some(durable_note),
                ..
            } => {
                if !tokens_equal(&current, token) {
                    return Err(OperatorError::StaleClaim(event_id));
                }
                if durable_resolution == resolution && durable_note == note {
                    Ok(AckOutcome::AlreadyResolved)
                } else {
                    Err(OperatorError::ResolutionConflict(event_id))
                }
            }
            StoredStatus::Resolved { token: current, .. } => {
                if tokens_equal(&current, token) {
                    Err(OperatorError::ResolutionConflict(event_id))
                } else {
                    Err(OperatorError::StaleClaim(event_id))
                }
            }
            StoredStatus::ResolvedUnclaimed { .. } => Err(OperatorError::AlreadyResolved(event_id)),
            StoredStatus::ActionInFlight { .. } => Err(OperatorError::ActionInFlight(event_id)),
            StoredStatus::Delivered { .. } => Err(OperatorError::ActionMismatch(event_id)),
            StoredStatus::Queued => Err(OperatorError::StaleClaim(event_id)),
        }
    }

    /// Durably record an action result, then resolve its event.
    pub fn finish_action(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        action_hash: &str,
        result: &str,
        resolution: Resolution,
    ) -> Result<AckOutcome, OperatorError> {
        self.finish_action_at(
            event_id,
            token,
            action_hash,
            result,
            resolution,
            self.shared.clock.now_ms()?,
        )
    }

    /// Deterministic-clock form of [`Self::finish_action`].
    pub fn finish_action_at(
        &self,
        event_id: EventId,
        token: &ClaimToken,
        action_hash: &str,
        result: &str,
        resolution: Resolution,
        now_ms: u64,
    ) -> Result<AckOutcome, OperatorError> {
        validate_action_hash(action_hash)?;
        validate_evidence("action result", result, MAX_ACTION_RESULT_BYTES, true)?;
        let mut live = self.lock()?;
        let current_epoch = live.state.durable_epoch;
        let status = live
            .state
            .events
            .get(&event_id)
            .ok_or(OperatorError::EventNotFound(event_id))?
            .status
            .clone();
        match status {
            StoredStatus::ActionInFlight {
                token: current,
                claim_epoch,
                action_hash: durable_hash,
                ..
            } => {
                if claim_epoch != current_epoch || !tokens_equal(&current, token) {
                    return Err(OperatorError::StaleClaim(event_id));
                }
                if durable_hash != action_hash {
                    return Err(OperatorError::ActionMismatch(event_id));
                }
                commit_record(
                    &mut live,
                    &self.shared.config,
                    &WalRecord::FinishAction {
                        event_id,
                        token: token.clone(),
                        action_hash: action_hash.to_string(),
                        result: result.to_string(),
                        resolution,
                        resolved_at_ms: now_ms,
                    },
                )?;
                Ok(AckOutcome::Resolved)
            }
            StoredStatus::Resolved {
                token: current,
                resolution: durable_resolution,
                action: Some(action),
                ..
            } if tokens_equal(&current, token)
                && action.hash == action_hash
                && action.result == result
                && durable_resolution == resolution =>
            {
                Ok(AckOutcome::AlreadyResolved)
            }
            StoredStatus::Resolved {
                token: current,
                claim_epoch,
                ..
            } => {
                if claim_epoch != current_epoch || !tokens_equal(&current, token) {
                    return Err(OperatorError::StaleClaim(event_id));
                }
                commit_record(
                    &mut live,
                    &self.shared.config,
                    &WalRecord::Conflict {
                        event_id,
                        token: token.clone(),
                        reason: "conflicting action results".into(),
                        at_ms: now_ms,
                    },
                )?;
                Err(OperatorError::ResolutionConflict(event_id))
            }
            StoredStatus::InDoubt { .. } => Err(OperatorError::EventInDoubt(event_id)),
            StoredStatus::ResolvedUnclaimed { .. } => Err(OperatorError::AlreadyResolved(event_id)),
            StoredStatus::Delivered { .. } => Err(OperatorError::ActionMismatch(event_id)),
            StoredStatus::Queued => Err(OperatorError::StaleClaim(event_id)),
        }
    }

    /// Snapshot one event by durable identifier.
    pub fn status(&self, event_id: EventId) -> Result<EventSnapshot, OperatorError> {
        self.snapshot(event_id)
    }

    /// Snapshot one event by durable identifier.
    pub fn snapshot(&self, event_id: EventId) -> Result<EventSnapshot, OperatorError> {
        self.lock()?
            .state
            .events
            .get(&event_id)
            .map(StoredEvent::snapshot)
            .ok_or(OperatorError::EventNotFound(event_id))
    }

    /// Stable event-id-ordered snapshot of all retained history.
    pub fn snapshots(&self) -> Result<Vec<EventSnapshot>, OperatorError> {
        Ok(self
            .lock()?
            .state
            .events
            .values()
            .map(StoredEvent::snapshot)
            .collect())
    }

    /// Stable event-id-ordered snapshot of unresolved events only.
    pub fn unresolved_snapshots(&self) -> Result<Vec<EventSnapshot>, OperatorError> {
        Ok(self
            .lock()?
            .state
            .events
            .values()
            .filter(|event| event.status.unresolved())
            .map(StoredEvent::snapshot)
            .collect())
    }

    /// Number of unresolved events consuming queue capacity.
    pub fn unresolved_len(&self) -> Result<usize, OperatorError> {
        Ok(self.lock()?.state.unresolved_len())
    }

    /// Number of currently queued, claimable events.
    pub fn queued_len(&self) -> Result<usize, OperatorError> {
        Ok(self
            .lock()?
            .state
            .events
            .values()
            .filter(|event| matches!(event.status, StoredStatus::Queued))
            .count())
    }

    /// Whether no unresolved event remains.
    pub fn is_empty(&self) -> Result<bool, OperatorError> {
        Ok(self.unresolved_len()? == 0)
    }
}

fn status_claim_error(event_id: EventId, status: &StoredStatus) -> OperatorError {
    match status {
        StoredStatus::Resolved { .. } => OperatorError::AlreadyResolved(event_id),
        StoredStatus::ResolvedUnclaimed { .. } => OperatorError::AlreadyResolved(event_id),
        StoredStatus::InDoubt { .. } => OperatorError::EventInDoubt(event_id),
        StoredStatus::ActionInFlight { .. } => OperatorError::ActionInFlight(event_id),
        StoredStatus::Queued | StoredStatus::Delivered { .. } => {
            OperatorError::StaleClaim(event_id)
        }
    }
}

fn reclaim_locked(
    live: &mut LiveState,
    config: &QueueConfig,
    now_ms: u64,
) -> Result<Vec<ExpiryOutcome>, OperatorError> {
    let mut expired: Vec<(u64, EventId, ClaimToken, u32, bool)> = live
        .state
        .events
        .iter()
        .filter_map(|(event_id, event)| {
            let StoredStatus::Delivered {
                token,
                expires_at_ms,
                ..
            } = &event.status
            else {
                return None;
            };
            (*expires_at_ms <= now_ms).then(|| {
                let new_count = if event.escalated {
                    event.redelivery_count
                } else {
                    event.redelivery_count.saturating_add(1)
                };
                let escalated = event.escalated || new_count >= config.redelivery_cap;
                (
                    *expires_at_ms,
                    *event_id,
                    token.clone(),
                    new_count,
                    escalated,
                )
            })
        })
        .collect();
    expired.sort_by_key(|(expires_at, event_id, ..)| (*expires_at, *event_id));
    let mut outcomes = Vec::with_capacity(expired.len());
    for (_, event_id, token, redelivery_count, escalated) in expired {
        commit_record(
            live,
            config,
            &WalRecord::Expire {
                event_id,
                token,
                at_ms: now_ms,
                redelivery_count,
                became_escalated: escalated,
            },
        )?;
        outcomes.push(ExpiryOutcome {
            event_id,
            escalated,
        });
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aterm-operator-{label}-{}-{nonce}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn generation(value: u8, evidence: &str) -> EventGeneration {
        EventGeneration::new(
            u64::from(value),
            value % 2 == 1,
            u64::from(value) * 10,
            Sha256::digest(evidence.as_bytes()),
        )
    }

    fn event(sid: &str, value: u8, condition: AttentionCondition) -> NewEvent {
        let evidence = format!("screen {value}\nready\tmarker");
        NewEvent::new(sid, generation(value, &evidence), condition, evidence)
    }

    fn fast_config() -> QueueConfig {
        QueueConfig {
            capacity: 16,
            visibility_timeout: Duration::from_millis(100),
            max_cumulative_extension: Duration::from_millis(600),
            redelivery_cap: 3,
            max_wal_bytes: 2 * 1024 * 1024,
        }
    }

    fn enqueued_id(outcome: EnqueueOutcome) -> EventId {
        match outcome {
            EnqueueOutcome::Enqueued(id) => id,
            other => panic!("expected enqueue, got {other:?}"),
        }
    }

    struct ManualTicks(AtomicU64);

    impl ManualTicks {
        fn new(value: u64) -> Self {
            Self(AtomicU64::new(value))
        }

        fn set(&self, value: u64) {
            self.0.store(value, Ordering::Release);
        }
    }

    impl TickSource for ManualTicks {
        fn sample_ms(&self) -> Result<u64, OperatorError> {
            Ok(self.0.load(Ordering::Acquire))
        }
    }

    fn checkpoint_files(directory: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| checkpoint_sequence_from_name(path).unwrap().is_some())
            .collect();
        paths.sort();
        paths
    }

    fn legacy_checkpoint_bytes(state: &QueueState) -> Vec<u8> {
        assert!(matches!(state.fleet_gate, StoredFleetGate::Healthy));
        let mut payload = encode_checkpoint_payload(state).unwrap();
        assert_eq!(
            payload.pop(),
            Some(0),
            "v2 Healthy gate is one trailing byte"
        );
        let payload_len = u64::try_from(payload.len()).unwrap();
        let mut header = [0_u8; CHECKPOINT_HEADER_LEN];
        header[0..4].copy_from_slice(&CHECKPOINT_MAGIC);
        header[4..6].copy_from_slice(&LEGACY_CHECKPOINT_SCHEMA.to_le_bytes());
        header[8..16].copy_from_slice(&payload_len.to_le_bytes());
        header[16..24].copy_from_slice(&state.next_record_sequence.to_le_bytes());
        header[24..32].copy_from_slice(&state.durable_epoch.to_le_bytes());
        let mut digest = Sha256::new();
        digest.update(header);
        digest.update(&payload);
        let checksum = digest.finalize();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&checksum);
        bytes
    }

    #[test]
    fn operator_production_clock_clamps_rollback_and_honors_forward_progress() {
        let directory = TestDir::new("monotonic-clock");
        let ticks = Arc::new(ManualTicks::new(100));
        let clock = Arc::new(AuthorityClock::injected(ticks.clone()));
        let (queue, _) =
            DurableQueue::open_internal_with_clock(&directory.0, Some(1), fast_config(), clock)
                .unwrap();
        queue.manage_sid("a").unwrap();
        let id = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let claim = queue.claim().unwrap().unwrap();
        assert_eq!(claim.expires_at_ms, 200);

        ticks.set(50);
        let extended = queue
            .extend(id, &claim.token, Duration::from_millis(10))
            .unwrap();
        assert_eq!(extended.expires_at_ms, 210);
        assert!(queue.reclaim_expired().unwrap().is_empty());

        ticks.set(209);
        assert!(queue.reclaim_expired().unwrap().is_empty());
        ticks.set(210);
        assert_eq!(queue.reclaim_expired().unwrap()[0].event_id, id);
        ticks.set(1);
        assert_eq!(
            queue.shared.clock.now_ms().unwrap(),
            210,
            "a rollback cannot revive authority after a forward observation"
        );
    }

    #[test]
    fn operator_checkpoint_rollover_reopens_and_bounds_terminal_history() {
        let directory = TestDir::new("checkpoint-rollover");
        let config = fast_config();
        let queue = DurableQueue::open(&directory.0, 1, config.clone()).unwrap();
        queue.manage_sid("a").unwrap();
        for value in 1..=512_u64 {
            let evidence = format!("resolved generation {value}");
            let generation =
                EventGeneration::new(value, false, value, Sha256::digest(evidence.as_bytes()));
            let id = enqueued_id(
                queue
                    .enqueue(NewEvent::new(
                        "a",
                        generation,
                        AttentionCondition::Ready,
                        evidence,
                    ))
                    .unwrap(),
            );
            let claim = queue.claim_at(value * 10).unwrap().unwrap();
            assert_eq!(claim.event.id, id);
            queue
                .ack_at(id, &claim.token, Resolution::NoAction, value * 10 + 1)
                .unwrap();
        }

        // This non-event commit crosses the terminal-history threshold. The
        // live state must stay on the pruned checkpoint projection rather than
        // resurrecting the pre-compaction `next` clone.
        assert!(queue.manage_sid("b").unwrap());
        let live_snapshots = queue.snapshots().unwrap();
        assert_eq!(live_snapshots.len(), RESOLVED_RETENTION);
        let checkpoint_after_trigger = checkpoint_files(&directory.0);
        assert_eq!(checkpoint_after_trigger.len(), 1);

        // The immediately following small append must not recompact because
        // pruning really took effect in memory.
        assert!(queue.manage_sid("c").unwrap());
        assert_eq!(checkpoint_files(&directory.0), checkpoint_after_trigger);
        assert_eq!(queue.snapshots().unwrap(), live_snapshots);
        let next_event_id = queue
            .shared
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .state
            .next_event_id;
        drop(queue);

        let reopened = DurableQueue::open(&directory.0, 2, config).unwrap();
        assert_eq!(
            reopened.managed_sids().unwrap(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(reopened.snapshots().unwrap(), live_snapshots);
        assert_eq!(
            reopened
                .shared
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .state
                .next_event_id,
            next_event_id
        );
    }

    #[test]
    fn operator_checkpoint_crash_windows_are_replay_safe_and_corruption_fails_closed() {
        let directory = TestDir::new("checkpoint-crash");
        let config = fast_config();
        let queue = DurableQueue::open(&directory.0, 1, config.clone()).unwrap();
        queue.manage_sid("a").unwrap();
        let id = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let old_wal = fs::read(directory.0.join(WAL_FILE_NAME)).unwrap();
        {
            let mut live = queue
                .shared
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            compact_live(&mut live).unwrap();
        }
        drop(queue);

        // Published checkpoint + pre-truncation WAL: covered frames are verified
        // and skipped, not applied twice.
        fs::write(directory.0.join(WAL_FILE_NAME), old_wal).unwrap();
        let reopened = DurableQueue::open(&directory.0, 2, config.clone()).unwrap();
        assert_eq!(reopened.status(id).unwrap().id, id);
        drop(reopened);

        // A pre-publish temporary is never authoritative and may be abandoned.
        fs::write(
            directory.0.join(CHECKPOINT_PENDING_NAME),
            b"torn pending checkpoint",
        )
        .unwrap();
        let reopened = DurableQueue::open(&directory.0, 3, config.clone()).unwrap();
        assert_eq!(reopened.status(id).unwrap().id, id);
        drop(reopened);

        let partial_final = directory
            .0
            .join(format!("{CHECKPOINT_PREFIX}{}", u64::MAX - 1));
        fs::write(&partial_final, CHECKPOINT_MAGIC).unwrap();
        assert!(matches!(
            DurableQueue::open(&directory.0, 4, config.clone()),
            Err(OperatorError::CorruptCheckpoint { .. })
        ));
        fs::remove_file(partial_final).unwrap();

        let checkpoint = checkpoint_files(&directory.0).pop().unwrap();
        let mut bytes = fs::read(&checkpoint).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x80;
        fs::write(&checkpoint, bytes).unwrap();
        assert!(matches!(
            DurableQueue::open(&directory.0, 4, config),
            Err(OperatorError::CorruptCheckpoint { .. })
        ));
    }

    #[test]
    fn operator_checkpoint_preserves_allowlist_fairness_and_unresolved_states() {
        let directory = TestDir::new("checkpoint-state");
        let config = fast_config();
        let queue = DurableQueue::open(&directory.0, 1, config.clone()).unwrap();
        queue.manage_sid("a").unwrap();
        queue.manage_sid("b").unwrap();

        let ambiguous = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let claim = queue.claim_at(1).unwrap().unwrap();
        let hash = "ab".repeat(32);
        queue
            .begin_action_at(ambiguous, &claim.token, "turn", &hash, 2)
            .unwrap();
        queue
            .mark_action_in_doubt_at(ambiguous, &claim.token, "submission uncertain", 3)
            .unwrap();
        let a_ready = enqueued_id(
            queue
                .enqueue(event("a", 2, AttentionCondition::Ready))
                .unwrap(),
        );
        let b_ready = enqueued_id(
            queue
                .enqueue(event("b", 3, AttentionCondition::Ready))
                .unwrap(),
        );
        {
            let mut live = queue
                .shared
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            compact_live(&mut live).unwrap();
        }
        let (checkpoint, _) = recover_checkpoint(&directory.0, &config).unwrap();
        assert_eq!(checkpoint.managed, BTreeSet::from(["a".into(), "b".into()]));
        assert_eq!(
            checkpoint.fair_sids,
            VecDeque::from(["a".into(), "b".into()])
        );
        assert_eq!(checkpoint.ready_by_sid["a"], VecDeque::from([a_ready]));
        assert_eq!(checkpoint.ready_by_sid["b"], VecDeque::from([b_ready]));
        assert!(matches!(
            checkpoint.events[&ambiguous].status,
            StoredStatus::InDoubt { .. }
        ));
    }

    #[test]
    fn operator_unmanage_retires_generation_idempotency_for_fresh_baseline() {
        let directory = TestDir::new("fresh-remanage");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        let first = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let duplicate = queue
            .enqueue(event("a", 1, AttentionCondition::Ready))
            .unwrap();
        assert!(matches!(
            duplicate,
            EnqueueOutcome::Coalesced { event_id, .. } if event_id == first
        ));
        queue.unmanage_sid_at("a", 1).unwrap();
        queue.manage_sid("a").unwrap();
        let second = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        assert!(second > first);
    }

    #[test]
    fn operator_durable_fault_gates_work_and_clear_requires_fresh_baselines() {
        let directory = TestDir::new("fleet-fault-clear");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        queue.manage_sid("b").unwrap();
        let action_id = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let delivered_id = enqueued_id(
            queue
                .enqueue(event("a", 2, AttentionCondition::Ready))
                .unwrap(),
        );
        let action_claim = queue.claim_at(1).unwrap().unwrap();
        assert_eq!(action_claim.event.id, action_id);
        let hash = "ab".repeat(32);
        queue
            .begin_action_at(action_id, &action_claim.token, "turn", &hash, 2)
            .unwrap();
        let delivered_claim = queue.claim_at(3).unwrap().unwrap();
        assert_eq!(delivered_claim.event.id, delivered_id);

        let latched = queue
            .latch_fault_at(FleetFaultReason::ObserverOverflow, 4)
            .unwrap();
        let fault = match latched {
            FaultLatchOutcome::Latched(fault) => fault,
            other => panic!("expected new fault, got {other:?}"),
        };
        assert_eq!(fault.reason, FleetFaultReason::ObserverOverflow);
        assert!(directory.0.join(FAULT_MARKER_NAME).is_file());
        assert!(matches!(
            queue.manage_sid("c"),
            Err(OperatorError::FleetFaulted(
                FleetFaultReason::ObserverOverflow
            ))
        ));
        assert!(matches!(
            queue.enqueue(event("a", 3, AttentionCondition::Ready)),
            Err(OperatorError::FleetFaulted(
                FleetFaultReason::ObserverOverflow
            ))
        ));
        assert!(matches!(
            queue.claim_at(5),
            Err(OperatorError::FleetFaulted(
                FleetFaultReason::ObserverOverflow
            ))
        ));
        assert!(matches!(
            queue.begin_action_at(delivered_id, &delivered_claim.token, "turn", &hash, 5),
            Err(OperatorError::FleetFaulted(
                FleetFaultReason::ObserverOverflow
            ))
        ));
        assert_eq!(
            queue
                .latch_fault_at(FleetFaultReason::ActuatorIntegrity, 6)
                .unwrap(),
            FaultLatchOutcome::AlreadyLatched(fault)
        );

        assert_eq!(
            queue.begin_fault_clear_at(7).unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(matches!(
            queue.status(action_id).unwrap().status,
            EventStatus::InDoubt { .. }
        ));
        assert!(matches!(
            queue.status(delivered_id).unwrap().status,
            EventStatus::ResolvedUnclaimed {
                resolution: Resolution::Paused,
                ..
            }
        ));
        let baseline = queue
            .enqueue_rebaseline(event("a", 9, AttentionCondition::Changed))
            .unwrap();
        assert!(queue.unmanage_sid_at("b", 8).unwrap());
        assert!(matches!(
            queue.complete_fault_clear_at(9),
            Err(OperatorError::EventInDoubt(found)) if found == action_id
        ));
        queue
            .reconcile_in_doubt_at(
                action_id,
                &action_claim.token,
                Resolution::NoAction,
                "owner verified no terminal submission landed",
                10,
            )
            .unwrap();
        queue.complete_fault_clear_at(11).unwrap();
        assert_eq!(queue.fleet_gate().unwrap(), FleetGateStatus::Healthy);
        assert!(!directory.0.join(FAULT_MARKER_NAME).exists());
        assert_eq!(queue.claim_at(12).unwrap().unwrap().event.id, baseline);
        drop(queue);

        let reopened = DurableQueue::open(&directory.0, 2, fast_config()).unwrap();
        assert_eq!(reopened.fleet_gate().unwrap(), FleetGateStatus::Healthy);
    }

    #[test]
    fn operator_final_action_permit_is_exact_and_never_parks() {
        let directory = TestDir::new("final-action-permit");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        queue.manage_sid("b").unwrap();
        let id = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let claim = queue.claim_at(1).unwrap().unwrap();
        let hash = "ab".repeat(32);
        queue
            .begin_action_at(id, &claim.token, "turn", &hash, 2)
            .unwrap();
        assert_eq!(
            queue
                .try_validate_action_permit(id, &claim.token, "a", "turn", &hash)
                .unwrap(),
            FinalActionPermit::Granted
        );
        assert_eq!(
            queue
                .try_validate_action_permit(id, &claim.token, "b", "turn", &hash)
                .unwrap(),
            FinalActionPermit::Revoked
        );
        assert_eq!(
            queue
                .try_validate_action_permit(id, &claim.token, "a", "turn", &"cd".repeat(32))
                .unwrap(),
            FinalActionPermit::Revoked
        );
        {
            let _held = queue
                .shared
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(
                queue
                    .try_validate_action_permit(id, &claim.token, "a", "turn", &hash)
                    .unwrap(),
                FinalActionPermit::Busy
            );
        }
        queue
            .latch_fault_at(FleetFaultReason::ObserverOverflow, 3)
            .unwrap();
        assert_eq!(
            queue
                .try_validate_action_permit(id, &claim.token, "a", "turn", &hash)
                .unwrap(),
            FinalActionPermit::Revoked
        );
    }

    #[test]
    fn operator_fault_marker_survives_a_wal_latch_failure() {
        let directory = TestDir::new("fault-marker-wal-failure");
        let mut tiny = fast_config();
        tiny.max_wal_bytes = WAL_FRAME_OVERHEAD as u64 + 12;
        let queue = DurableQueue::open(&directory.0, 1, tiny).unwrap();
        assert!(matches!(
            queue.latch_fault_at(FleetFaultReason::ObserverPanicked, 1),
            Err(OperatorError::WalFull { .. })
        ));
        assert!(directory.0.join(FAULT_MARKER_NAME).is_file());
        assert!(matches!(
            queue.fleet_gate(),
            Err(OperatorError::WalPoisoned)
        ));
        drop(queue);

        let reopened = DurableQueue::open(&directory.0, 2, fast_config()).unwrap();
        assert!(matches!(
            reopened.fleet_gate().unwrap(),
            FleetGateStatus::Faulted(FleetFault {
                reason: FleetFaultReason::ObserverPanicked,
                ..
            })
        ));
    }

    #[test]
    fn operator_poisoned_enqueue_still_latches_independent_fault_marker() {
        let directory = TestDir::new("enqueue-poison-fault-marker");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        {
            let mut live = queue
                .shared
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // An ordinary enqueue now reaches the real WAL write path, where a
            // read-only descriptor produces an I/O error and poisons the handle.
            live.wal = File::open(directory.0.join(WAL_FILE_NAME)).unwrap();
        }
        assert!(matches!(
            queue.enqueue(event("a", 1, AttentionCondition::Ready)),
            Err(OperatorError::Io(_))
        ));
        assert!(matches!(
            queue.latch_fault_at(FleetFaultReason::DurableStateUnavailable, 2),
            Err(OperatorError::WalPoisoned)
        ));
        assert_eq!(
            read_fault_marker(&directory.0).unwrap(),
            Some(FleetFaultReason::DurableStateUnavailable)
        );
        drop(queue);

        let reopened = DurableQueue::open(&directory.0, 2, fast_config()).unwrap();
        assert!(matches!(
            reopened.fleet_gate().unwrap(),
            FleetGateStatus::Faulted(FleetFault {
                reason: FleetFaultReason::DurableStateUnavailable,
                ..
            })
        ));
    }

    #[test]
    fn operator_fault_marker_bypasses_held_live_mutex() {
        let directory = TestDir::new("fault-marker-with-held-live");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        let held = queue
            .shared
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let writer_queue = queue.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let writer = std::thread::spawn(move || {
            let result =
                writer_queue.latch_fault_marker_without_live(FleetFaultReason::ObserverPanicked);
            let _ = tx.send(result);
        });
        let result = rx.recv_timeout(Duration::from_secs(2));
        drop(held);
        writer.join().unwrap();
        assert_eq!(
            result
                .expect("fault marker waited for the held live-state mutex")
                .unwrap(),
            FleetFaultReason::ObserverPanicked
        );
        drop(queue);

        let reopened = DurableQueue::open(&directory.0, 2, fast_config()).unwrap();
        assert!(matches!(
            reopened.fleet_gate().unwrap(),
            FleetGateStatus::Faulted(FleetFault {
                reason: FleetFaultReason::ObserverPanicked,
                ..
            })
        ));
    }

    #[test]
    fn operator_corrupt_or_preexisting_fault_marker_is_never_narrowed() {
        let directory = TestDir::new("fault-marker-corrupt");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        fs::write(directory.0.join(FAULT_MARKER_NAME), b"torn").unwrap();
        let outcome = queue
            .latch_fault_at(FleetFaultReason::ActuatorIntegrity, 1)
            .unwrap();
        assert!(matches!(
            outcome,
            FaultLatchOutcome::Latched(FleetFault {
                reason: FleetFaultReason::DurabilityUncertain,
                ..
            })
        ));
        assert_eq!(
            read_fault_marker(&directory.0).unwrap(),
            Some(FleetFaultReason::DurabilityUncertain)
        );
        drop(queue);

        let reopened = DurableQueue::open(&directory.0, 2, fast_config()).unwrap();
        assert!(matches!(
            reopened.fleet_gate().unwrap(),
            FleetGateStatus::Faulted(FleetFault {
                reason: FleetFaultReason::DurabilityUncertain,
                ..
            })
        ));

        let older = TestDir::new("fault-marker-first-wins");
        let queue = DurableQueue::open(&older.0, 1, fast_config()).unwrap();
        ensure_fault_marker(&older.0, FleetFaultReason::ObserverOverflow).unwrap();
        let outcome = queue
            .latch_fault_at(FleetFaultReason::ActuatorIntegrity, 2)
            .unwrap();
        assert!(matches!(
            outcome,
            FaultLatchOutcome::Latched(FleetFault {
                reason: FleetFaultReason::ObserverOverflow,
                ..
            })
        ));
    }

    #[test]
    fn operator_checkpoint_v2_and_legacy_v1_marker_recovery_preserve_faults() {
        let current = TestDir::new("fault-checkpoint-v2");
        let config = fast_config();
        let queue = DurableQueue::open(&current.0, 1, config.clone()).unwrap();
        queue
            .latch_fault_at(FleetFaultReason::ObserverOverflow, 1)
            .unwrap();
        {
            let mut live = queue
                .shared
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            compact_live(&mut live).unwrap();
        }
        drop(queue);
        let reopened = DurableQueue::open(&current.0, 2, config.clone()).unwrap();
        assert!(matches!(
            reopened.fleet_gate().unwrap(),
            FleetGateStatus::Faulted(_)
        ));
        drop(reopened);

        let legacy = TestDir::new("fault-checkpoint-v1-marker");
        let queue = DurableQueue::open(&legacy.0, 1, config.clone()).unwrap();
        let state = queue
            .shared
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .state
            .clone();
        let sequence = state.next_record_sequence;
        let bytes = legacy_checkpoint_bytes(&state);
        drop(queue);
        fs::write(
            legacy.0.join(format!("{CHECKPOINT_PREFIX}{sequence}")),
            bytes,
        )
        .unwrap();
        fs::write(legacy.0.join(WAL_FILE_NAME), []).unwrap();
        ensure_fault_marker(&legacy.0, FleetFaultReason::ObserverPanicked).unwrap();
        let reopened = DurableQueue::open(&legacy.0, 2, config).unwrap();
        assert!(matches!(
            reopened.fleet_gate().unwrap(),
            FleetGateStatus::Faulted(FleetFault {
                reason: FleetFaultReason::ObserverPanicked,
                ..
            })
        ));
    }

    #[test]
    fn operator_complete_record_before_marker_removal_reopens_blocked() {
        let directory = TestDir::new("fault-complete-crash");
        let config = fast_config();
        let queue = DurableQueue::open(&directory.0, 1, config.clone()).unwrap();
        queue
            .latch_fault_at(FleetFaultReason::ActuatorIntegrity, 1)
            .unwrap();
        assert!(queue.begin_fault_clear_at(2).unwrap().is_empty());
        {
            let mut live = queue
                .shared
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            commit_record(
                &mut live,
                &config,
                &WalRecord::CompleteFaultClear { at_ms: 3 },
            )
            .unwrap();
        }
        assert!(directory.0.join(FAULT_MARKER_NAME).is_file());
        drop(queue);

        let reopened = DurableQueue::open(&directory.0, 2, config).unwrap();
        assert!(matches!(
            reopened.fleet_gate().unwrap(),
            FleetGateStatus::Faulted(FleetFault {
                reason: FleetFaultReason::ActuatorIntegrity,
                ..
            })
        ));
        assert!(reopened.begin_fault_clear_at(4).unwrap().is_empty());
        reopened.complete_fault_clear_at(5).unwrap();
        assert_eq!(reopened.fleet_gate().unwrap(), FleetGateStatus::Healthy);
    }

    #[test]
    fn operator_wal_kind_bound_covers_fault_protocol_and_rejects_unknown() {
        let fault = WalRecord::LatchFleetFault {
            fault: FleetFault {
                reason: FleetFaultReason::ObserverOverflow,
                fault_epoch: 1,
                latched_at_ms: 1,
            },
        };
        let frame = encode_frame(&fault, 1, 1).unwrap();
        let header: &[u8; WAL_HEADER_LEN] = frame[..WAL_HEADER_LEN].try_into().unwrap();
        assert_eq!(decode_header(header, 0).unwrap().kind, 16);

        let mut unknown = *header;
        unknown[6..8].copy_from_slice(&(MAX_WAL_RECORD_KIND + 1).to_le_bytes());
        assert!(matches!(
            decode_header(&unknown, 0),
            Err(OperatorError::CorruptWal { reason, .. })
                if reason.contains("unknown record kind")
        ));
        assert!(!partial_header_plausible(&unknown[..8], 1));
    }

    #[test]
    fn operator_defaults_pin_delivery_policy() {
        assert_eq!(DEFAULT_VISIBILITY_TIMEOUT, Duration::from_secs(120));
        assert_eq!(DEFAULT_MAX_CUMULATIVE_EXTENSION, Duration::from_secs(600));
        assert_eq!(DEFAULT_REDELIVERY_CAP, 3);
        assert_eq!(QueueConfig::default().capacity, 1024);
    }

    #[test]
    fn operator_manage_with_baseline_is_atomic_across_failure_and_reopen() {
        let failed = TestDir::new("manage-baseline-definite-failure");
        let mut capacity_one = fast_config();
        capacity_one.capacity = 1;
        let queue = DurableQueue::open(&failed.0, 1, capacity_one.clone()).unwrap();
        let baseline_evidence = "MANAGE-BASELINE-RAW-SCREEN-CANARY";
        assert!(
            queue
                .manage_with_baseline(NewEvent::new(
                    "a",
                    generation(1, baseline_evidence),
                    AttentionCondition::Changed,
                    baseline_evidence,
                ))
                .unwrap()
                .is_some()
        );
        let wal = fs::read(failed.0.join(WAL_FILE_NAME)).unwrap();
        assert!(
            !wal.windows(baseline_evidence.len())
                .any(|window| window == baseline_evidence.as_bytes())
        );
        assert!(matches!(
            queue.manage_with_baseline(event("b", 2, AttentionCondition::Changed)),
            Err(OperatorError::QueueFull { capacity: 1 })
        ));
        drop(queue);

        let reopened = DurableQueue::open(&failed.0, 2, capacity_one).unwrap();
        assert!(!reopened.is_managed("b").unwrap());
        assert!(
            reopened
                .unresolved_snapshots()
                .unwrap()
                .iter()
                .all(|event| event.sid != "b")
        );

        // Model the ambiguous full-frame side of an I/O failure: the frame is
        // durable but the caller did not receive success and the live handle is
        // poisoned. Recovery may retain the admission, but never without its
        // neutral baseline because both facts are one checksummed record.
        let ambiguous = TestDir::new("manage-baseline-ambiguous-frame");
        let queue = DurableQueue::open(&ambiguous.0, 1, fast_config()).unwrap();
        let new = event("b", 3, AttentionCondition::Changed);
        new.validate().unwrap();
        let event_id;
        {
            let mut live = queue
                .shared
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            event_id = EventId(live.state.next_event_id);
            let record = WalRecord::ManageWithBaseline {
                event: StoredEvent {
                    id: event_id,
                    sid: new.sid,
                    generation: new.generation,
                    condition: new.condition,
                    redelivery_count: 0,
                    escalated: false,
                    status: StoredStatus::Queued,
                },
            };
            let frame = encode_frame(
                &record,
                live.state.next_record_sequence,
                live.state.durable_epoch,
            )
            .unwrap();
            let offset = live.wal_len;
            live.wal.seek(SeekFrom::Start(offset)).unwrap();
            live.wal.write_all(&frame).unwrap();
            live.wal.sync_data().unwrap();
            live.poisoned = true;
        }
        drop(queue);

        let reopened = DurableQueue::open(&ambiguous.0, 2, fast_config()).unwrap();
        assert!(reopened.is_managed("b").unwrap());
        let baseline = reopened.status(event_id).unwrap();
        assert_eq!(baseline.sid, "b");
        assert_eq!(baseline.condition, AttentionCondition::Changed);
        assert!(matches!(baseline.status, EventStatus::Queued));
    }

    #[test]
    fn operator_manage_baseline_keeps_approval_and_exit_through_immediate_ack() {
        let directory = TestDir::new("manage-baseline-safety-conditions");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();

        let approval_id = queue
            .manage_with_baseline(event("approval", 1, AttentionCondition::ApprovalRequired))
            .unwrap()
            .unwrap();
        let approval = queue.claim_at(10).unwrap().unwrap();
        assert_eq!(approval.event.id, approval_id);
        assert_eq!(
            approval.event.condition,
            AttentionCondition::ApprovalRequired
        );
        queue
            .ack_at(approval_id, &approval.token, Resolution::NoAction, 11)
            .unwrap();
        assert_eq!(
            queue.status(approval_id).unwrap().condition,
            AttentionCondition::ApprovalRequired
        );

        let exit_id = queue
            .manage_with_baseline(event("exited", 2, AttentionCondition::SessionExited))
            .unwrap()
            .unwrap();
        let exited = queue.claim_at(12).unwrap().unwrap();
        assert_eq!(exited.event.id, exit_id);
        assert_eq!(exited.event.condition, AttentionCondition::SessionExited);
        queue
            .ack_at(exit_id, &exited.token, Resolution::NoAction, 13)
            .unwrap();
        assert_eq!(
            queue.status(exit_id).unwrap().condition,
            AttentionCondition::SessionExited
        );

        assert!(matches!(
            queue.manage_with_baseline(event("ready", 3, AttentionCondition::Ready)),
            Err(OperatorError::InvalidInput(reason))
                if reason.contains("Changed, ApprovalRequired, or SessionExited")
        ));
        assert!(!queue.is_managed("ready").unwrap());
    }

    #[test]
    fn operator_allowlist_is_empty_durable_and_revokes_claims() {
        let directory = TestDir::new("allowlist");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        assert!(queue.managed_sids().unwrap().is_empty());
        assert_eq!(
            queue
                .enqueue(event("sid-a", 1, AttentionCondition::Ready))
                .unwrap(),
            EnqueueOutcome::Unmanaged
        );
        assert!(queue.manage_sid("sid-a").unwrap());
        assert!(!queue.manage_sid("sid-a").unwrap());
        let id = enqueued_id(
            queue
                .enqueue(event("sid-a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let claim = queue.claim_at(10).unwrap().unwrap();
        assert!(queue.unmanage_sid_at("sid-a", 11).unwrap());
        assert!(matches!(
            queue.status(id).unwrap().status,
            EventStatus::ResolvedUnclaimed {
                resolution: Resolution::Paused,
                ..
            }
        ));
        assert!(matches!(
            queue.ack_at(id, &claim.token, Resolution::NoAction, 12),
            Err(OperatorError::AlreadyResolved(found)) if found == id
        ));
        drop(queue);

        let reopened = DurableQueue::open(&directory.0, 2, fast_config()).unwrap();
        assert!(!reopened.is_managed("sid-a").unwrap());
        assert!(matches!(
            reopened.status(id).unwrap().status,
            EventStatus::ResolvedUnclaimed {
                resolution: Resolution::Paused,
                ..
            }
        ));
    }

    #[test]
    fn operator_enqueue_coalesces_strongest_condition_and_bounds_capacity() {
        let directory = TestDir::new("coalesce");
        let mut config = fast_config();
        config.capacity = 2;
        let queue = DurableQueue::open(&directory.0, 1, config).unwrap();
        queue.manage_sid("a").unwrap();
        let first = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Changed))
                .unwrap(),
        );
        assert_eq!(
            queue
                .enqueue(event("a", 1, AttentionCondition::ApprovalRequired))
                .unwrap(),
            EnqueueOutcome::Coalesced {
                event_id: first,
                strengthened: true,
            }
        );
        assert_eq!(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
            EnqueueOutcome::Coalesced {
                event_id: first,
                strengthened: false,
            }
        );
        assert_eq!(
            queue.status(first).unwrap().condition,
            AttentionCondition::ApprovalRequired
        );
        let second = enqueued_id(
            queue
                .enqueue(event("a", 2, AttentionCondition::Ready))
                .unwrap(),
        );
        assert!(matches!(
            queue.enqueue(event("a", 3, AttentionCondition::Ready)),
            Err(OperatorError::QueueFull { capacity: 2 })
        ));
        let one = queue.claim_at(0).unwrap().unwrap();
        assert_eq!(one.event.id, first);
        assert_eq!(
            queue
                .ack_at(first, &one.token, Resolution::NoAction, 1)
                .unwrap(),
            AckOutcome::Resolved
        );
        let third = enqueued_id(
            queue
                .enqueue(event("a", 3, AttentionCondition::Ready))
                .unwrap(),
        );
        assert!(third > second);
    }

    #[test]
    fn operator_strengthening_a_delivered_event_revokes_its_claim() {
        let directory = TestDir::new("coalesce-delivered");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        let id = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let old = queue.claim_at(10).unwrap().unwrap();
        assert_eq!(old.event.condition, AttentionCondition::Ready);

        assert_eq!(
            queue
                .enqueue(event("a", 1, AttentionCondition::ApprovalRequired))
                .unwrap(),
            EnqueueOutcome::Coalesced {
                event_id: id,
                strengthened: true,
            }
        );
        let invalidated = queue.status(id).unwrap();
        assert_eq!(invalidated.condition, AttentionCondition::ApprovalRequired);
        assert!(matches!(invalidated.status, EventStatus::Queued));
        assert!(matches!(
            queue.begin_action_at(id, &old.token, "turn", &"ab".repeat(32), 11),
            Err(OperatorError::StaleClaim(found)) if found == id
        ));

        let fresh = queue.claim_at(12).unwrap().unwrap();
        assert_ne!(old.token, fresh.token);
        assert_eq!(fresh.event.condition, AttentionCondition::ApprovalRequired);
    }

    #[test]
    fn operator_event_evidence_is_validated_but_never_persisted() {
        let directory = TestDir::new("privacy");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        let canary = "CANARY-RAW-SCREEN-SECRET-4d92f5c1";
        let generation = EventGeneration::new(1, false, 9, Sha256::digest(canary.as_bytes()));
        let id = enqueued_id(
            queue
                .enqueue(NewEvent::new(
                    "a",
                    generation,
                    AttentionCondition::Ready,
                    canary,
                ))
                .unwrap(),
        );
        assert_eq!(queue.status(id).unwrap().generation, generation);
        let wal = fs::read(directory.0.join(WAL_FILE_NAME)).unwrap();
        assert!(
            !wal.windows(canary.len())
                .any(|window| window == canary.as_bytes()),
            "raw screen evidence must never enter the operator WAL"
        );

        let mismatched = NewEvent::new(
            "a",
            EventGeneration::new(1, false, 10, [0; 32]),
            AttentionCondition::Ready,
            canary,
        );
        assert!(matches!(
            queue.enqueue(mismatched),
            Err(OperatorError::InvalidInput(reason))
                if reason.contains("fingerprint does not match")
        ));
    }

    #[test]
    fn operator_claim_is_per_sid_fair_and_extension_is_capped() {
        let directory = TestDir::new("fair");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("noisy").unwrap();
        queue.manage_sid("quiet").unwrap();
        let noisy_one = enqueued_id(
            queue
                .enqueue(event("noisy", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let noisy_two = enqueued_id(
            queue
                .enqueue(event("noisy", 2, AttentionCondition::Ready))
                .unwrap(),
        );
        let quiet = enqueued_id(
            queue
                .enqueue(event("quiet", 3, AttentionCondition::Ready))
                .unwrap(),
        );
        let first = queue.claim_at(1_000).unwrap().unwrap();
        assert_eq!(first.event.id, noisy_one);
        let extension = queue
            .extend_at(noisy_one, &first.token, Duration::from_millis(600), 1_001)
            .unwrap();
        assert_eq!(extension.expires_at_ms, 1_700);
        assert_eq!(extension.cumulative_extension_ms, 600);
        assert!(matches!(
            queue.extend_at(
                noisy_one,
                &first.token,
                Duration::from_millis(1),
                1_002
            ),
            Err(OperatorError::ExtensionLimit(found)) if found == noisy_one
        ));
        let second = queue.claim_at(1_000).unwrap().unwrap();
        assert_eq!(second.event.id, quiet, "quiet sid gets the next fair turn");
        let third = queue.claim_at(1_000).unwrap().unwrap();
        assert_eq!(third.event.id, noisy_two);
    }

    #[test]
    fn operator_stale_ack_cannot_resolve_a_redelivery() {
        let directory = TestDir::new("stale");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        let id = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let old = queue.claim_at(0).unwrap().unwrap();
        let expired = queue.reclaim_expired_at(100).unwrap();
        assert_eq!(
            expired,
            vec![ExpiryOutcome {
                event_id: id,
                escalated: false
            }]
        );
        let fresh = queue.claim_at(100).unwrap().unwrap();
        assert_ne!(old.token, fresh.token);
        assert!(matches!(
            queue.ack_at(id, &old.token, Resolution::NoAction, 101),
            Err(OperatorError::StaleClaim(found)) if found == id
        ));
        assert_eq!(
            queue
                .ack_at(id, &fresh.token, Resolution::NoAction, 101)
                .unwrap(),
            AckOutcome::Resolved
        );
    }

    #[test]
    fn operator_ack_is_idempotent_and_conflict_becomes_in_doubt() {
        let directory = TestDir::new("ack");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        let id = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let claim = queue.claim_at(0).unwrap().unwrap();
        assert_eq!(
            queue
                .ack_at(id, &claim.token, Resolution::NoAction, 1)
                .unwrap(),
            AckOutcome::Resolved
        );
        assert_eq!(
            queue
                .ack_at(id, &claim.token, Resolution::NoAction, 2)
                .unwrap(),
            AckOutcome::AlreadyResolved
        );
        assert!(matches!(
            queue.ack_at(id, &claim.token, Resolution::Paused, 3),
            Err(OperatorError::ResolutionConflict(found)) if found == id
        ));
        assert!(matches!(
            queue.status(id).unwrap().status,
            EventStatus::InDoubt { .. }
        ));
    }

    #[test]
    fn operator_redelivery_cap_converts_once_then_escalation_expiry_is_in_doubt() {
        let directory = TestDir::new("escalate");
        let mut config = fast_config();
        config.visibility_timeout = Duration::from_millis(10);
        let queue = DurableQueue::open(&directory.0, 1, config).unwrap();
        queue.manage_sid("a").unwrap();
        let id = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::SuspectedStuck))
                .unwrap(),
        );
        for delivery in 0..3_u64 {
            let start = delivery * 10;
            let claim = queue.claim_at(start).unwrap().unwrap();
            assert_eq!(claim.event.id, id);
            let outcomes = queue.reclaim_expired_at(start + 10).unwrap();
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].escalated, delivery == 2);
        }
        let snapshot = queue.status(id).unwrap();
        assert_eq!(snapshot.redelivery_count, 3);
        assert!(snapshot.escalated);
        assert_eq!(snapshot.condition, AttentionCondition::Escalation);
        assert!(matches!(snapshot.status, EventStatus::Queued));
        let escalation = queue.claim_at(30).unwrap().unwrap();
        assert!(queue.reclaim_expired_at(40).unwrap()[0].escalated);
        assert!(matches!(
            queue.status(id).unwrap().status,
            EventStatus::InDoubt { token: Some(token), .. } if token == escalation.token
        ));
    }

    #[test]
    fn operator_action_intent_result_is_durable_and_orphan_never_requeues() {
        let completed_dir = TestDir::new("action-complete");
        let queue = DurableQueue::open(&completed_dir.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        let id = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let claim = queue.claim_at(0).unwrap().unwrap();
        let hash = "ab".repeat(32);
        queue
            .begin_action_at(id, &claim.token, "turn", &hash, 1)
            .unwrap();
        assert!(matches!(
            queue.begin_action_at(id, &claim.token, "turn", &hash, 2),
            Err(OperatorError::ActionInFlight(found)) if found == id
        ));
        assert_eq!(
            queue
                .finish_action_at(
                    id,
                    &claim.token,
                    &hash,
                    "settled\noutput",
                    Resolution::Acted,
                    3
                )
                .unwrap(),
            AckOutcome::Resolved
        );
        assert_eq!(
            queue
                .finish_action_at(
                    id,
                    &claim.token,
                    &hash,
                    "settled\noutput",
                    Resolution::Acted,
                    4
                )
                .unwrap(),
            AckOutcome::AlreadyResolved
        );
        drop(queue);
        let reopened = DurableQueue::open(&completed_dir.0, 2, fast_config()).unwrap();
        assert!(matches!(
            reopened.status(id).unwrap().status,
            EventStatus::Resolved { .. }
        ));
        drop(reopened);

        let orphan_dir = TestDir::new("action-orphan");
        let orphan = DurableQueue::open(&orphan_dir.0, 1, fast_config()).unwrap();
        orphan.manage_sid("a").unwrap();
        let orphan_id = enqueued_id(
            orphan
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let orphan_claim = orphan.claim_at(0).unwrap().unwrap();
        orphan
            .begin_action_at(orphan_id, &orphan_claim.token, "turn", &hash, 1)
            .unwrap();
        drop(orphan);
        let recovered = DurableQueue::open(&orphan_dir.0, 2, fast_config()).unwrap();
        assert!(matches!(
            recovered.status(orphan_id).unwrap().status,
            EventStatus::InDoubt { .. }
        ));
        assert!(recovered.claim_at(1_000).unwrap().is_none());
        assert!(matches!(
            recovered.reconcile_in_doubt_at(
                orphan_id,
                &orphan_claim.token,
                Resolution::Acted,
                "",
                2
            ),
            Err(OperatorError::InvalidInput(_))
        ));
        let wrong = ClaimToken::from_wire(&"00".repeat(32)).unwrap();
        assert!(matches!(
            recovered.reconcile_in_doubt_at(
                orphan_id,
                &wrong,
                Resolution::Acted,
                "human checked shell history",
                2
            ),
            Err(OperatorError::StaleClaim(found)) if found == orphan_id
        ));
        assert_eq!(
            recovered
                .reconcile_in_doubt_at(
                    orphan_id,
                    &orphan_claim.token,
                    Resolution::Acted,
                    "human checked shell history",
                    2
                )
                .unwrap(),
            AckOutcome::Resolved
        );
        assert!(matches!(
            recovered.status(orphan_id).unwrap().status,
            EventStatus::Resolved {
                resolution: Resolution::Acted,
                reconciliation_note: Some(ref note),
                ..
            } if note == "human checked shell history"
        ));
        assert_eq!(
            recovered
                .reconcile_in_doubt_at(
                    orphan_id,
                    &orphan_claim.token,
                    Resolution::Acted,
                    "human checked shell history",
                    3
                )
                .unwrap(),
            AckOutcome::AlreadyResolved
        );
        drop(recovered);

        // The epoch record's orphan conversion is itself replayable: the later
        // reconciliation must not make the next open report a corrupt WAL.
        let reconciled = DurableQueue::open(&orphan_dir.0, 3, fast_config()).unwrap();
        assert!(matches!(
            reconciled.status(orphan_id).unwrap().status,
            EventStatus::Resolved {
                resolution: Resolution::Acted,
                reconciliation_note: Some(ref note),
                ..
            } if note == "human checked shell history"
        ));
    }

    #[test]
    fn operator_takeover_requeues_claims_to_cap_then_fences_escalation() {
        let directory = TestDir::new("takeover-claims");
        let mut config = fast_config();
        config.redelivery_cap = 2;
        config.visibility_timeout = Duration::from_secs(60);
        let now = 10_000;

        let first = DurableQueue::open(&directory.0, 1, config.clone()).unwrap();
        first.manage_sid("a").unwrap();
        let id = enqueued_id(
            first
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let claim_one = first.claim_at(now).unwrap().unwrap();
        assert!(matches!(
            claim_one.event.status,
            EventStatus::Delivered { claim_epoch: 1, .. }
        ));
        drop(first);

        let second = DurableQueue::open(&directory.0, 2, config.clone()).unwrap();
        let after_one = second.status(id).unwrap();
        assert_eq!(after_one.redelivery_count, 1);
        assert!(!after_one.escalated);
        assert!(matches!(after_one.status, EventStatus::Queued));
        assert!(matches!(
            second.ack_at(id, &claim_one.token, Resolution::NoAction, now + 1),
            Err(OperatorError::StaleClaim(found)) if found == id
        ));
        let claim_two = second.claim_at(now + 1).unwrap().unwrap();
        assert!(matches!(
            claim_two.event.status,
            EventStatus::Delivered { claim_epoch: 2, .. }
        ));
        drop(second);

        let third = DurableQueue::open(&directory.0, 3, config.clone()).unwrap();
        let at_cap = third.status(id).unwrap();
        assert_eq!(at_cap.redelivery_count, 2);
        assert!(at_cap.escalated);
        assert_eq!(at_cap.condition, AttentionCondition::Escalation);
        assert!(matches!(at_cap.status, EventStatus::Queued));
        assert!(matches!(
            third.extend_at(id, &claim_two.token, Duration::from_millis(1), now + 2),
            Err(OperatorError::StaleClaim(found)) if found == id
        ));
        let escalation = third.claim_at(now + 2).unwrap().unwrap();
        drop(third);

        let fourth = DurableQueue::open(&directory.0, 4, config).unwrap();
        assert!(matches!(
            fourth.status(id).unwrap().status,
            EventStatus::InDoubt { token: Some(ref token), .. }
                if token == &escalation.token
        ));
        assert!(fourth.claim_at(now + 3).unwrap().is_none());
    }

    #[test]
    fn operator_old_resolved_claim_cannot_regress_after_takeover() {
        let directory = TestDir::new("resolved-epoch");
        let first = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        first.manage_sid("a").unwrap();
        let id = enqueued_id(
            first
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        let claim = first.claim_at(10).unwrap().unwrap();
        first
            .ack_at(id, &claim.token, Resolution::NoAction, 11)
            .unwrap();
        drop(first);

        let second = DurableQueue::open(&directory.0, 2, fast_config()).unwrap();
        assert_eq!(
            second
                .ack_at(id, &claim.token, Resolution::NoAction, 12)
                .unwrap(),
            AckOutcome::AlreadyResolved
        );
        assert!(matches!(
            second.ack_at(id, &claim.token, Resolution::Paused, 13),
            Err(OperatorError::StaleClaim(found)) if found == id
        ));
        assert!(matches!(
            second.status(id).unwrap().status,
            EventStatus::Resolved {
                resolution: Resolution::NoAction,
                ..
            }
        ));
    }

    #[test]
    fn operator_unmanage_cycles_release_capacity_without_tokenless_doubt() {
        let directory = TestDir::new("unmanage-capacity");
        let mut config = fast_config();
        config.capacity = 1;
        let queue = DurableQueue::open(&directory.0, 1, config).unwrap();
        for value in 1..=8_u8 {
            assert!(queue.manage_sid("a").unwrap());
            let id = enqueued_id(
                queue
                    .enqueue(event("a", value, AttentionCondition::Ready))
                    .unwrap(),
            );
            assert!(queue.unmanage_sid_at("a", u64::from(value)).unwrap());
            assert!(matches!(
                queue.status(id).unwrap().status,
                EventStatus::ResolvedUnclaimed {
                    resolution: Resolution::Paused,
                    ..
                }
            ));
            assert_eq!(queue.unresolved_len().unwrap(), 0);
        }
    }

    #[test]
    fn operator_poison_refuses_reads_and_noop_paths() {
        let directory = TestDir::new("poison");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        let id = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        queue
            .shared
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .poisoned = true;

        assert!(matches!(queue.status(id), Err(OperatorError::WalPoisoned)));
        assert!(matches!(
            queue.is_managed("a"),
            Err(OperatorError::WalPoisoned)
        ));
        assert!(matches!(
            queue.manage_sid("a"),
            Err(OperatorError::WalPoisoned)
        ));
        assert!(matches!(
            queue.enqueue(event("unmanaged", 2, AttentionCondition::Ready)),
            Err(OperatorError::WalPoisoned)
        ));
        assert!(matches!(
            queue.claim_at(10),
            Err(OperatorError::WalPoisoned)
        ));
    }

    #[test]
    fn operator_wal_recovers_and_repairs_only_a_partial_final_frame() {
        let directory = TestDir::new("repair");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        let id = enqueued_id(
            queue
                .enqueue(event("a", 1, AttentionCondition::Ready))
                .unwrap(),
        );
        drop(queue);
        let wal_path = directory.0.join(WAL_FILE_NAME);
        let good_len = fs::metadata(&wal_path).unwrap().len();
        let mut file = OpenOptions::new().append(true).open(&wal_path).unwrap();
        file.write_all(&WAL_MAGIC[..3]).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let (reopened, report) =
            DurableQueue::open_with_report(&directory.0, 2, fast_config()).unwrap();
        assert!(report.repaired_partial_final_frame);
        assert_eq!(reopened.status(id).unwrap().id, id);
        assert!(fs::metadata(&wal_path).unwrap().len() > good_len);
        drop(reopened);

        let mut bytes = fs::read(&wal_path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x80;
        fs::write(&wal_path, &bytes).unwrap();
        assert!(matches!(
            DurableQueue::open(&directory.0, 3, fast_config()),
            Err(OperatorError::CorruptWal { .. })
        ));
    }

    #[test]
    fn operator_process_lock_and_epoch_fence_fail_closed() {
        let directory = TestDir::new("lock");
        let first = DurableQueue::open(&directory.0, 5, fast_config()).unwrap();
        assert!(matches!(
            DurableQueue::open(&directory.0, 6, fast_config()),
            Err(OperatorError::LockContended(_))
        ));
        drop(first);
        assert!(matches!(
            DurableQueue::open(&directory.0, 4, fast_config()),
            Err(OperatorError::EpochRegression {
                requested: 4,
                durable: 5
            })
        ));
        let (next, report) = DurableQueue::open_next_epoch(&directory.0, fast_config()).unwrap();
        assert_eq!(report.durable_epoch, 6);
        assert_eq!(next.durable_epoch().unwrap(), 6);
    }

    /// A descriptor duplicate is what every `fork` hands out: a child spawned
    /// anywhere in this process while the queue was open carries a copy of the
    /// lock's open file description until it reaches `exec`, and the OS lock
    /// lives on that description rather than on either descriptor. `try_clone`
    /// is that duplicate exactly. Dropping the last queue handle must therefore
    /// end the lock, not merely close one name for it — otherwise the next opener
    /// of the same directory is refused by a holder that never opened it, which is
    /// how a shell spawn in one thread breaks an unrelated reopen in another.
    #[test]
    fn a_dropped_queue_releases_its_lock_against_an_inherited_descriptor() {
        let directory = TestDir::new("lock-fork-window");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        let inherited = queue.shared._lock.file.try_clone().unwrap();
        drop(queue);
        let reopened = DurableQueue::open(&directory.0, 2, fast_config());
        drop(inherited);
        assert!(
            reopened.is_ok(),
            "an inherited descriptor must not keep a released lock alive: {:?}",
            reopened.err()
        );
    }

    #[test]
    fn operator_cloned_handles_serialize_concurrent_enqueues() {
        let directory = TestDir::new("threads");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        queue.manage_sid("a").unwrap();
        let mut joins = Vec::new();
        for value in 1..=12_u8 {
            let queue = queue.clone();
            joins.push(std::thread::spawn(move || {
                enqueued_id(
                    queue
                        .enqueue(event("a", value, AttentionCondition::Ready))
                        .unwrap(),
                )
            }));
        }
        let mut ids: Vec<EventId> = joins.into_iter().map(|join| join.join().unwrap()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 12);
        assert_eq!(queue.unresolved_len().unwrap(), 12);
    }

    #[cfg(unix)]
    #[test]
    fn operator_private_directory_and_files_are_hardened() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = TestDir::new("modes");
        let queue = DurableQueue::open(&directory.0, 1, fast_config()).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&directory.0), 0o700);
        assert_eq!(mode(&directory.0.join(WAL_FILE_NAME)), 0o600);
        assert_eq!(mode(&directory.0.join(LOCK_FILE_NAME)), 0o600);
        queue
            .latch_fault_at(FleetFaultReason::ObserverOverflow, 1)
            .unwrap();
        assert_eq!(mode(&directory.0.join(FAULT_MARKER_NAME)), 0o600);
        assert!(queue.begin_fault_clear_at(2).unwrap().is_empty());
        queue.complete_fault_clear_at(3).unwrap();
        assert!(!directory.0.join(FAULT_MARKER_NAME).exists());
        drop(queue);

        let target = TestDir::new("symlink-target");
        fs::create_dir_all(&target.0).unwrap();
        let link = directory.0.with_extension("link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&target.0, &link).unwrap();
        assert!(DurableQueue::open(&link, 1, fast_config()).is_err());
        fs::remove_file(link).unwrap();
    }

    /// REGRESSION: the operator must own `<root>/operator/**` and nothing above
    /// it. aterm's shared per-user state root also holds the recovery journal;
    /// an opt-in subsystem that silently rewrote its mode would be changing the
    /// permissions of state belonging to users who never enabled it.
    #[cfg(unix)]
    #[test]
    fn operator_state_scope_stops_at_its_own_subtree() {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = |path: &Path| fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777;

        let home = TestDir::new("shared-root");
        let root = home.0.join("aterm");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

        let path = fleet_state_dir_in(&root, "profile-default").unwrap();
        assert_eq!(path, root.join("operator").join("profile-default"));
        assert_eq!(
            mode(&root),
            0o755,
            "the operator rewrote the mode of aterm's SHARED state root"
        );
        assert_eq!(mode(&root.join("operator")), 0o700);
        assert_eq!(mode(&path), 0o700);

        // Idempotent, and still hands-off on the second resolution.
        assert_eq!(fleet_state_dir_in(&root, "profile-default").unwrap(), path);
        assert_eq!(mode(&root), 0o755);

        // A shared root the user redirected elsewhere (a synced folder, another
        // volume) is the app's business, not the operator's: it is followed,
        // while the operator's own directory below it stays a real 0700 one.
        let elsewhere = TestDir::new("shared-root-target");
        fs::create_dir_all(&elsewhere.0).unwrap();
        let linked_root = home.0.join("linked");
        std::os::unix::fs::symlink(&elsewhere.0, &linked_root).unwrap();
        let linked = fleet_state_dir_in(&linked_root, "profile-default").unwrap();
        assert_eq!(mode(&elsewhere.0.join("operator")), 0o700);
        assert_eq!(mode(&linked), 0o700);
        assert!(
            !fs::symlink_metadata(elsewhere.0.join("operator"))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // ... but the operator's OWN components are still policed in full.
        let hostile = home.0.join("hostile");
        fs::create_dir_all(&hostile).unwrap();
        std::os::unix::fs::symlink(&elsewhere.0, hostile.join("operator")).unwrap();
        assert!(fleet_state_dir_in(&hostile, "profile-default").is_err());
    }

    /// REGRESSION: a state root this process cannot write reports a plain
    /// error. It must never panic, and it must not leave a trace on the shared
    /// root. The embedded host turns this error into standby, never a fleet
    /// fault (`crates/aterm-gui/src/operator_host.rs`).
    #[cfg(unix)]
    #[test]
    fn operator_state_dir_on_an_unusable_root_errors_without_touching_it() {
        use std::os::unix::fs::PermissionsExt as _;

        // SAFETY: getuid takes no arguments and cannot invalidate memory.
        if unsafe { libc::getuid() } == 0 {
            return; // root ignores the mode bits this case depends on
        }
        let home = TestDir::new("unwritable-root");
        let root = home.0.join("aterm");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();

        let error = fleet_state_dir_in(&root, "profile-default").unwrap_err();
        assert!(
            matches!(&error, OperatorError::Io(io) if io.kind() == io::ErrorKind::PermissionDenied),
            "{error}"
        );
        assert_eq!(
            fs::symlink_metadata(&root).unwrap().permissions().mode() & 0o777,
            0o555
        );
        assert!(!root.join("operator").exists());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
