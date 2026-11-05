// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded host observations. Decoding data never creates native control authority.

use crate::cache_work::{Availability, Collection, Denominators, Summary, UnavailableReason, Work};
use aterm_json::{Value, json};
use std::{collections::BTreeSet, io};

/// This observation schema. It describes an explicit native session, not a
/// qualified computer-wide allocation authority or proof result.
pub const SCHEMA: u64 = 3;
/// Maximum aggregate native-root and transport-epoch rows in one frame.
pub const MAX_JOBS: usize = 256;
/// Reject oversized input before JSON allocation.
pub const MAX_FRAME_BYTES: usize = 128 * 1024;
/// Maximum completed-root cache-work records across one observation frame.
pub const MAX_CACHE_WORK: usize = 16;

/// One immutable completed-root diagnostic projection. Decoding this data
/// grants no execution, allocation, artifact acceptance or settlement authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootCacheWork {
    /// Actual root slot within the enclosing original owner epoch.
    pub root: u32,
    /// Validated exact-run report summary, including explicit legacy absence.
    pub summary: Option<Summary>,
    /// Closed collection failure, never private diagnostic prose or an invented zero.
    pub unavailable: Option<UnavailableReason>,
}

impl RootCacheWork {
    /// Project a checked collection without retaining its full unit report.
    #[must_use]
    pub fn from_collection(root: u32, collection: &Collection) -> Self {
        match collection {
            Collection::Report(report) => Self {
                root,
                summary: Some(report.summary()),
                unavailable: None,
            },
            Collection::Unavailable(reason) => Self {
                root,
                summary: None,
                unavailable: Some(*reason),
            },
        }
    }

    fn valid(&self) -> bool {
        match (&self.summary, self.unavailable) {
            (Some(summary), None) => summary.validate().is_ok(),
            (None, Some(_)) => true,
            _ => false,
        }
    }

    fn value(&self) -> Value {
        json!({"root":self.root,
            "summary":self.summary.as_ref().map_or(Value::Null, summary_value),
            "unavailable":self.unavailable.map_or(Value::Null, |reason| json!(unavailable_name(reason)))})
    }

    fn parse(value: &Value) -> io::Result<Self> {
        if !fields(value, &["root", "summary", "unavailable"]) {
            return Err(bad());
        }
        let record = Self {
            root: value["root"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(bad)?,
            summary: if value["summary"].is_null() {
                None
            } else {
                Some(parse_summary(&value["summary"])?)
            },
            unavailable: if value["unavailable"].is_null() {
                None
            } else {
                Some(
                    parse_unavailable(value["unavailable"].as_str().ok_or_else(bad)?)
                        .ok_or_else(bad)?,
                )
            },
        };
        if !record.valid() {
            return Err(bad());
        }
        Ok(record)
    }
}

fn unavailable_name(reason: UnavailableReason) -> &'static str {
    match reason {
        UnavailableReason::NotRecorded => "not-recorded",
        UnavailableReason::UnsupportedDriver => "unsupported-driver",
        UnavailableReason::MissingSession => "missing-session",
        UnavailableReason::AmbiguousSession => "ambiguous-session",
        UnavailableReason::QueryFailed => "query-failed",
        UnavailableReason::InvalidReport => "invalid-report",
        UnavailableReason::PersistenceFailed => "persistence-failed",
    }
}

fn parse_unavailable(value: &str) -> Option<UnavailableReason> {
    Some(match value {
        "not-recorded" => UnavailableReason::NotRecorded,
        "unsupported-driver" => UnavailableReason::UnsupportedDriver,
        "missing-session" => UnavailableReason::MissingSession,
        "ambiguous-session" => UnavailableReason::AmbiguousSession,
        "query-failed" => UnavailableReason::QueryFailed,
        "invalid-report" => UnavailableReason::InvalidReport,
        "persistence-failed" => UnavailableReason::PersistenceFailed,
        _ => return None,
    })
}

fn summary_value(summary: &Summary) -> Value {
    let d = summary.denominators;
    json!({"run_id":summary.run_id,"scope":summary.scope(),
        "availability":match summary.availability { Availability::Complete => "complete", Availability::UnavailableLegacyLog => "unavailable-legacy-log" },
        "denominators":{"registered_units":d.registered_units,"run_script_units":d.run_script_units,
            "observed_units":d.observed_units,"units_without_attempts":d.units_without_attempts,
            "attempts_started":d.attempts_started,"attempts_finished":d.attempts_finished,
            "captured_attempts":d.captured_attempts,"uncacheable_attempts":d.uncacheable_attempts},
        "totals":summary.totals.map_or(Value::Null, |w| json!({"entries":w.entries,"input_bytes":w.input_bytes,
            "bytes_hashed":w.bytes_hashed,"metadata_reads":w.metadata_reads,"resolution_steps":w.resolution_steps,
            "link_reads":w.link_reads,"observations":w.observations,"revalidations":w.revalidations}))})
}

fn parse_summary(value: &Value) -> io::Result<Summary> {
    if !fields(
        value,
        &["run_id", "scope", "availability", "denominators", "totals"],
    ) || value["scope"].as_str() != Some(crate::cache_work::SCOPE)
        || !fields(
            &value["denominators"],
            &[
                "registered_units",
                "run_script_units",
                "observed_units",
                "units_without_attempts",
                "attempts_started",
                "attempts_finished",
                "captured_attempts",
                "uncacheable_attempts",
            ],
        )
    {
        return Err(bad());
    }
    let d = &value["denominators"];
    let number = |key: &str| d[key].as_u64().ok_or_else(bad);
    let totals = if value["totals"].is_null() {
        None
    } else {
        let w = &value["totals"];
        if !fields(
            w,
            &[
                "entries",
                "input_bytes",
                "bytes_hashed",
                "metadata_reads",
                "resolution_steps",
                "link_reads",
                "observations",
                "revalidations",
            ],
        ) {
            return Err(bad());
        }
        let number = |key: &str| w[key].as_u64().ok_or_else(bad);
        Some(Work {
            entries: number("entries")?,
            input_bytes: number("input_bytes")?,
            bytes_hashed: number("bytes_hashed")?,
            metadata_reads: number("metadata_reads")?,
            resolution_steps: number("resolution_steps")?,
            link_reads: number("link_reads")?,
            observations: number("observations")?,
            revalidations: number("revalidations")?,
        })
    };
    let summary = Summary {
        run_id: value["run_id"].as_str().ok_or_else(bad)?.to_owned(),
        availability: match value["availability"].as_str() {
            Some("complete") => Availability::Complete,
            Some("unavailable-legacy-log") => Availability::UnavailableLegacyLog,
            _ => return Err(bad()),
        },
        denominators: Denominators {
            registered_units: number("registered_units")?,
            run_script_units: number("run_script_units")?,
            observed_units: number("observed_units")?,
            units_without_attempts: number("units_without_attempts")?,
            attempts_started: number("attempts_started")?,
            attempts_finished: number("attempts_finished")?,
            captured_attempts: number("captured_attempts")?,
            uncacheable_attempts: number("uncacheable_attempts")?,
        },
        totals,
    };
    summary.validate()?;
    Ok(summary)
}

/// Service incarnation data for fencing, never an authentication credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation(pub [u8; 16]);

/// An attempt's public reference. Only its actual owner can resolve it to control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attempt {
    /// Exact session incarnation.
    pub generation: Generation,
    /// Nonzero, never reused within that incarnation.
    pub sequence: u64,
}

/// Native supervision phase; none establishes CPU activity or program correctness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Stopped native birth awaits owner acknowledgement.
    Starting,
    /// No new native observation; the program may be CPU-active.
    Waiting,
    /// An actual stop was acknowledged.
    Stopped,
    /// The owner recognized cancellation.
    Cancelling,
    /// Actual exit is observed, with the original leader retained for control.
    ExitPinned,
    /// An owner error remains while the child is still owned.
    Faulted,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Waiting => "waiting",
            Self::Stopped => "stopped",
            Self::Cancelling => "cancelling",
            Self::ExitPinned => "exit-pinned",
            Self::Faulted => "faulted",
        }
    }
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "starting" => Self::Starting,
            "waiting" => Self::Waiting,
            "stopped" => Self::Stopped,
            "cancelling" => Self::Cancelling,
            "exit-pinned" => Self::ExitPinned,
            "faulted" => Self::Faulted,
            _ => return None,
        })
    }
}

/// Closed I/O classification. Arbitrary diagnostic text, paths and environment
/// payloads are deliberately absent from the public observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// An actual permission refusal.
    Permission,
    /// Invalid input or unsupported protocol/operation.
    Invalid,
    /// A disconnected or broken local channel.
    Disconnected,
    /// An explicit timeout.
    TimedOut,
    /// The requested attempt or object was not found.
    NotFound,
    /// Another error; no invented detailed cause.
    Other,
}

impl FailureKind {
    fn name(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Invalid => "invalid",
            Self::Disconnected => "disconnected",
            Self::TimedOut => "timed-out",
            Self::NotFound => "not-found",
            Self::Other => "other",
        }
    }
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "permission" => Self::Permission,
            "invalid" => Self::Invalid,
            "disconnected" => Self::Disconnected,
            "timed-out" => Self::TimedOut,
            "not-found" => Self::NotFound,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

/// Typed error projection, not a signal-acceptance or proof receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Closed classification; diagnostic prose is never serialized.
    pub kind: FailureKind,
    /// Original numeric OS error when supplied; absent stays unknown.
    pub os_error: Option<i32>,
}

impl Failure {
    /// Project only stable categories and optional errno, never an error's payload.
    #[must_use]
    pub fn of(error: &io::Error) -> Self {
        use io::ErrorKind as K;
        Self {
            kind: match error.kind() {
                K::PermissionDenied => FailureKind::Permission,
                K::InvalidInput | K::InvalidData | K::Unsupported => FailureKind::Invalid,
                K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe | K::NotConnected => {
                    FailureKind::Disconnected
                }
                K::TimedOut => FailureKind::TimedOut,
                K::NotFound => FailureKind::NotFound,
                _ => FailureKind::Other,
            },
            os_error: error.raw_os_error(),
        }
    }
}

/// One owner-reported native root. Serialized references supply no control capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// Nonzero session-local attempt, not a PID.
    pub attempt: u64,
    /// Actual supervision phase, not CPU-use inference.
    pub phase: Phase,
    /// Full admitted peak budget, not physical residency or arena address space.
    pub reserved_bytes: u64,
    /// Closed original failure projection, if any.
    pub failure: Option<Failure>,
}

/// One private transport epoch's observed custody, never a release capability.
/// Build outcomes remain independent of backing settlement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Epoch {
    /// Nonzero session-local epoch sequence, separate from native job attempts.
    pub sequence: u64,
    /// Actual retained backing units supplied by the owner.
    pub backing: u32,
    /// Separately backed implicit root units selected before publication.
    pub roots: u32,
    /// Full original admitted memory peak, not current physical residency.
    pub reserved_bytes: u64,
    /// The matching helper opening and original endpoint were validated.
    pub opened: bool,
    /// The private helper pipe supplied a matching retirement observation.
    pub retired: bool,
    /// The actual owned helper was reaped; this alone proves no cohort drain.
    pub helper_exited: bool,
    /// The original control output ended without undecoded trailing bytes.
    pub control_eof: bool,
    /// The original endpoint was durably removed, or unlaunched storage stayed absent.
    pub endpoint_cleaned: bool,
    /// Individual backing settlements that returned success.
    pub settled_units: u32,
    /// Consumed backing capabilities with an uncertain settlement result.
    pub uncertain_units: u32,
    /// Original backing capabilities still owned and unattempted.
    pub remaining_units: u32,
    /// Validated root exit observations, regardless of build success.
    pub completed_roots: u32,
    /// Completed roots whose actual exit reported failure.
    pub failed_roots: u32,
    /// A protocol fault prevents successful resource completion.
    pub faulted: bool,
    /// The actual child remains owned after unacknowledged durable binding.
    pub unbound: bool,
    /// An original controlled no-child receipt has not yet been transferred.
    pub not_spawned: bool,
    /// The original no-child receipt was transferred into claim cancellation custody.
    pub cancellation_started: bool,
    /// Closed owner error projection, independent of build and protocol outcomes.
    pub failure: Option<Failure>,
    /// Bounded historical completed-root input-work observations, never capacity.
    pub cache_work: Vec<RootCacheWork>,
}

impl Epoch {
    fn valid(&self) -> bool {
        let mut roots = BTreeSet::new();
        self.sequence != 0
            && self.cache_work.len() <= MAX_CACHE_WORK
            && self.cache_work.iter().all(|record| {
                record.root < self.roots && roots.insert(record.root) && record.valid()
            })
            && (self.cache_work.is_empty()
                || (self.remaining_units == 0
                    && self.uncertain_units == 0
                    && self.settled_units == self.backing
                    && self.completed_roots >= self.cache_work.len() as u32
                    && !self.faulted))
            && (1..=256).contains(&self.backing)
            && (1..=64).contains(&self.roots)
            && self.roots <= self.backing
            && self
                .settled_units
                .checked_add(self.uncertain_units)
                .and_then(|n| n.checked_add(self.remaining_units))
                == Some(self.backing)
            && self.completed_roots <= self.roots
            && self.failed_roots <= self.completed_roots
            && (!self.retired || self.opened)
            && (!self.control_eof || self.retired)
            && (self.completed_roots == 0 || self.opened)
            && (!self.unbound
                || !(self.opened
                    || self.not_spawned
                    || self.cancellation_started
                    || self.endpoint_cleaned))
            && (!self.not_spawned
                || !(self.opened
                    || self.helper_exited
                    || self.unbound
                    || self.cancellation_started
                    || self.endpoint_cleaned))
            && (!self.cancellation_started
                || !(self.opened || self.helper_exited || self.unbound || self.not_spawned))
            && (!self.endpoint_cleaned
                || self.cancellation_started
                || (self.retired && self.control_eof && self.helper_exited))
            && ((self.settled_units == 0 && self.uncertain_units == 0) || self.endpoint_cleaned)
    }

    fn value(&self) -> Value {
        json!({"sequence":self.sequence,"backing":self.backing,"roots":self.roots,"reserved_bytes":self.reserved_bytes,
            "opened":self.opened,"retired":self.retired,"helper_exited":self.helper_exited,
            "control_eof":self.control_eof,"endpoint_cleaned":self.endpoint_cleaned,
            "settled_units":self.settled_units,"uncertain_units":self.uncertain_units,
            "remaining_units":self.remaining_units,"completed_roots":self.completed_roots,
            "failed_roots":self.failed_roots,"faulted":self.faulted,"unbound":self.unbound,
            "not_spawned":self.not_spawned,"cancellation_started":self.cancellation_started,
            "failure":failure_value(self.failure),"cache_work":self.cache_work.iter().map(RootCacheWork::value).collect::<Vec<_>>()})
    }

    fn parse(row: &Value) -> io::Result<Self> {
        if !fields(
            row,
            &[
                "sequence",
                "backing",
                "roots",
                "reserved_bytes",
                "opened",
                "retired",
                "helper_exited",
                "control_eof",
                "endpoint_cleaned",
                "settled_units",
                "uncertain_units",
                "remaining_units",
                "completed_roots",
                "failed_roots",
                "faulted",
                "unbound",
                "not_spawned",
                "cancellation_started",
                "failure",
                "cache_work",
            ],
        ) {
            return Err(bad());
        }
        let number = |key: &str| {
            row[key]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(bad)
        };
        let boolean = |key: &str| row[key].as_bool().ok_or_else(bad);
        Ok(Self {
            sequence: row["sequence"].as_u64().ok_or_else(bad)?,
            backing: number("backing")?,
            roots: number("roots")?,
            reserved_bytes: row["reserved_bytes"].as_u64().ok_or_else(bad)?,
            opened: boolean("opened")?,
            retired: boolean("retired")?,
            helper_exited: boolean("helper_exited")?,
            control_eof: boolean("control_eof")?,
            endpoint_cleaned: boolean("endpoint_cleaned")?,
            settled_units: number("settled_units")?,
            uncertain_units: number("uncertain_units")?,
            remaining_units: number("remaining_units")?,
            completed_roots: number("completed_roots")?,
            failed_roots: number("failed_roots")?,
            faulted: boolean("faulted")?,
            unbound: boolean("unbound")?,
            not_spawned: boolean("not_spawned")?,
            cancellation_started: boolean("cancellation_started")?,
            failure: parse_failure(&row["failure"])?,
            cache_work: row["cache_work"]
                .as_array()
                .filter(|rows| rows.len() <= MAX_CACHE_WORK)
                .ok_or_else(bad)?
                .iter()
                .map(RootCacheWork::parse)
                .collect::<io::Result<Vec<_>>>()?,
        })
    }
}

/// Version-three explicit native-session observation. Its existence neither installs
/// a machine singleton nor qualifies cross-account enforcement or native containment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Owner session incarnation, never a credential.
    pub generation: Generation,
    /// Monotonic publication cursor in this incarnation.
    pub revision: u64,
    /// The owner stopped publishing; retained rows remain its last observation.
    pub closed: bool,
    /// Original native root observations, sharing MAX_JOBS with epoch rows.
    pub jobs: Vec<Job>,
    /// Private transport epochs; their observations confer no control authority.
    pub epochs: Vec<Epoch>,
}

fn bad() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid native host observation",
    )
}

fn fields(value: &Value, names: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == names.len() && names.iter().all(|name| object.contains_key(*name))
    })
}

fn failure_value(failure: Option<Failure>) -> Value {
    failure.map_or(
        Value::Null,
        |failure| json!({"kind":failure.kind.name(),"os_error":failure.os_error}),
    )
}

fn parse_failure(value: &Value) -> io::Result<Option<Failure>> {
    if value.is_null() {
        return Ok(None);
    }
    if !fields(value, &["kind", "os_error"]) {
        return Err(bad());
    }
    Ok(Some(Failure {
        kind: value["kind"]
            .as_str()
            .and_then(FailureKind::parse)
            .ok_or_else(bad)?,
        os_error: if value["os_error"].is_null() {
            None
        } else {
            Some(
                value["os_error"]
                    .as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .ok_or_else(bad)?,
            )
        },
    }))
}

impl Snapshot {
    fn validate(&self) -> io::Result<()> {
        if self
            .jobs
            .len()
            .checked_add(self.epochs.len())
            .is_none_or(|n| n > MAX_JOBS)
        {
            return Err(bad());
        }
        if self
            .epochs
            .iter()
            .try_fold(0usize, |sum, epoch| sum.checked_add(epoch.cache_work.len()))
            .is_none_or(|sum| sum > MAX_CACHE_WORK)
        {
            return Err(bad());
        }
        let mut seen = BTreeSet::new();
        if self.jobs.iter().any(|job| {
            job.attempt == 0
                || !seen.insert(job.attempt)
                || (job.phase == Phase::Faulted) != job.failure.is_some()
        }) {
            return Err(bad());
        }
        let mut seen = BTreeSet::new();
        if self
            .epochs
            .iter()
            .any(|epoch| !epoch.valid() || !seen.insert(epoch.sequence))
        {
            return Err(bad());
        }
        let mut runs = BTreeSet::new();
        if self
            .epochs
            .iter()
            .flat_map(|epoch| &epoch.cache_work)
            .filter_map(|record| record.summary.as_ref())
            .any(|summary| !runs.insert(summary.run_id.as_str()))
        {
            return Err(bad());
        }
        Ok(())
    }
    /// Encode only the closed projection, with explicit session scope.
    /// # Errors
    /// Invalid/oversized data or serialization failure.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let jobs: Vec<_> = self
            .jobs
            .iter()
            .map(|job| {
                json!({"attempt": job.attempt,
                    "phase": job.phase.name(), "reserved_bytes": job.reserved_bytes,
                    "failure": failure_value(job.failure)
                })
            })
            .collect();
        let epochs: Vec<_> = self.epochs.iter().map(Epoch::value).collect();
        let bytes = aterm_json::to_vec(&json!({"schema":SCHEMA,"scope":"explicit-native-session",
            "generation":self.generation.0.to_vec(),"revision":self.revision,"closed":self.closed,"jobs":jobs,"epochs":epochs})).map_err(|_| bad())?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(bad());
        }
        Ok(bytes)
    }
    /// Decode bounded observation data. Unknown versions, scope, fields, phases,
    /// duplicate identities/fields, inconsistent custody and malformed errors refuse.
    /// Schema three uses the same compact canonical representation as its encoder.
    /// # Errors
    /// Oversized, unsupported or malformed input. No native authority is created.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(bad());
        }
        let value: Value = aterm_json::from_slice(bytes).map_err(|_| bad())?;
        // Reject duplicate fields lost by Value's object map. All maintained
        // observation producers use this same canonical first-party encoder.
        if aterm_json::to_vec(&value).map_err(|_| bad())? != bytes {
            return Err(bad());
        }
        if !fields(
            &value,
            &[
                "schema",
                "scope",
                "generation",
                "revision",
                "closed",
                "jobs",
                "epochs",
            ],
        ) || value["schema"].as_u64() != Some(SCHEMA)
            || value["scope"].as_str() != Some("explicit-native-session")
        {
            return Err(bad());
        }
        let generation = value["generation"]
            .as_array()
            .filter(|v| v.len() == 16)
            .ok_or_else(bad)?;
        let mut raw = [0; 16];
        for (out, value) in raw.iter_mut().zip(generation) {
            *out = value
                .as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .ok_or_else(bad)?;
        }
        let rows = value["jobs"]
            .as_array()
            .filter(|v| v.len() <= MAX_JOBS)
            .ok_or_else(bad)?;
        let epochs = value["epochs"]
            .as_array()
            .filter(|epochs| {
                epochs
                    .len()
                    .checked_add(rows.len())
                    .is_some_and(|n| n <= MAX_JOBS)
            })
            .ok_or_else(bad)?;
        let epochs = epochs
            .iter()
            .map(Epoch::parse)
            .collect::<io::Result<Vec<_>>>()?;
        let mut jobs = Vec::with_capacity(rows.len());
        for row in rows {
            if !fields(row, &["attempt", "phase", "reserved_bytes", "failure"]) {
                return Err(bad());
            }
            let failure = parse_failure(&row["failure"])?;
            jobs.push(Job {
                attempt: row["attempt"].as_u64().ok_or_else(bad)?,
                phase: row["phase"]
                    .as_str()
                    .and_then(Phase::parse)
                    .ok_or_else(bad)?,
                reserved_bytes: row["reserved_bytes"].as_u64().ok_or_else(bad)?,
                failure,
            });
        }
        let snapshot = Self {
            generation: Generation(raw),
            revision: value["revision"].as_u64().ok_or_else(bad)?,
            closed: value["closed"].as_bool().ok_or_else(bad)?,
            jobs,
            epochs,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch() -> Epoch {
        Epoch {
            sequence: 1,
            backing: 2,
            roots: 1,
            reserved_bytes: 42,
            opened: true,
            retired: false,
            helper_exited: false,
            control_eof: false,
            endpoint_cleaned: false,
            settled_units: 0,
            uncertain_units: 0,
            remaining_units: 2,
            completed_roots: 1,
            failed_roots: 1,
            faulted: false,
            unbound: false,
            not_spawned: false,
            cancellation_started: false,
            failure: None,
            cache_work: Vec::new(),
        }
    }

    fn sample() -> Snapshot {
        Snapshot {
            generation: Generation([7; 16]),
            revision: 5,
            closed: false,
            jobs: vec![Job {
                attempt: 1,
                phase: Phase::Faulted,
                reserved_bytes: 42,
                failure: Some(Failure::of(&io::Error::other(
                    "private environment SECRET payload",
                ))),
            }],
            epochs: vec![epoch()],
        }
    }

    fn measured(run: &str) -> Summary {
        Summary {
            run_id: run.into(),
            availability: Availability::Complete,
            denominators: Denominators {
                registered_units: 4,
                run_script_units: 1,
                observed_units: 1,
                units_without_attempts: 3,
                attempts_started: 2,
                attempts_finished: 2,
                captured_attempts: 1,
                uncacheable_attempts: 1,
            },
            totals: Some(Work {
                entries: 2,
                input_bytes: 9,
                bytes_hashed: 6,
                metadata_reads: 12,
                resolution_steps: 10,
                link_reads: 0,
                observations: 15,
                revalidations: 7,
            }),
        }
    }

    fn settled_epoch(sequence: u64, roots: u32) -> Epoch {
        Epoch {
            sequence,
            roots,
            backing: roots,
            settled_units: roots,
            remaining_units: 0,
            completed_roots: roots,
            retired: true,
            helper_exited: true,
            control_eof: true,
            endpoint_cleaned: true,
            ..epoch()
        }
    }

    #[test]
    fn cache_work_roundtrips_exact_run_and_distinguishes_legacy_and_collection_failure() {
        let run = "20261105T103732783Z-1ab3ba0487a929a0";
        let mut legacy = measured("20261105T103732784Z-1ab3ba0487a929a0");
        legacy.availability = Availability::UnavailableLegacyLog;
        legacy.denominators.observed_units = 0;
        legacy.denominators.units_without_attempts = 4;
        legacy.denominators.attempts_started = 0;
        legacy.denominators.attempts_finished = 0;
        legacy.denominators.captured_attempts = 0;
        legacy.denominators.uncacheable_attempts = 0;
        legacy.totals = None;
        let mut snapshot = sample();
        let mut epoch = settled_epoch(1, 3);
        epoch.cache_work = vec![
            RootCacheWork {
                root: 0,
                summary: Some(measured(run)),
                unavailable: None,
            },
            RootCacheWork {
                root: 1,
                summary: Some(legacy),
                unavailable: None,
            },
            RootCacheWork {
                root: 2,
                summary: None,
                unavailable: Some(UnavailableReason::QueryFailed),
            },
        ];
        snapshot.epochs = vec![epoch];
        let bytes = snapshot.encode().unwrap();
        assert_eq!(Snapshot::decode(&bytes).unwrap(), snapshot);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(run));
        assert!(text.contains(crate::cache_work::SCOPE));
        assert!(text.contains("\"totals\":null"));
        assert!(
            !text.contains("\"units\":"),
            "full reports do not enter the monitor"
        );
        assert!(!text.contains("SECRET"));
    }

    #[test]
    fn cache_work_refuses_unknown_scope_duplicate_bindings_and_invalid_nullable_shapes() {
        let mut snapshot = sample();
        let mut epoch = settled_epoch(1, 2);
        epoch.cache_work.push(RootCacheWork {
            root: 0,
            summary: Some(measured("20261105T103732783Z-1ab3ba0487a929a0")),
            unavailable: None,
        });
        snapshot.epochs = vec![epoch];
        let mut malformed = snapshot.clone();
        malformed.epochs[0].cache_work[0].unavailable = Some(UnavailableReason::QueryFailed);
        assert!(malformed.encode().is_err());
        let mut malformed = snapshot.clone();
        let duplicate = malformed.epochs[0].cache_work[0].clone();
        malformed.epochs[0].cache_work.push(duplicate);
        assert!(malformed.encode().is_err());
        let mut malformed = snapshot.clone();
        let mut record = malformed.epochs[0].cache_work[0].clone();
        record.root = 1;
        malformed.epochs[0].cache_work.push(record);
        assert!(
            malformed.encode().is_err(),
            "one run cannot represent two roots"
        );
        let mut value: Value = aterm_json::from_slice(&snapshot.encode().unwrap()).unwrap();
        let epoch = &mut rows_mut(&mut value, "epochs")[0];
        let record = &mut rows_mut(epoch, "cache_work")[0];
        record
            .as_object_mut()
            .unwrap()
            .get_mut("summary")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("scope".into(), json!("all-compiler-costs"));
        refuses(&value);
    }

    #[test]
    fn cache_work_uses_one_global_budget_and_requires_original_settled_observations() {
        let mut snapshot = sample();
        snapshot.epochs[0].cache_work.push(RootCacheWork {
            root: 0,
            summary: None,
            unavailable: Some(UnavailableReason::MissingSession),
        });
        assert!(
            snapshot.encode().is_err(),
            "running custody cannot carry a completed-root report"
        );
        snapshot.jobs.clear();
        snapshot.epochs = (1..=9)
            .map(|sequence| {
                let mut epoch = settled_epoch(sequence, 2);
                epoch.cache_work = (0..2)
                    .map(|root| RootCacheWork {
                        root,
                        summary: None,
                        unavailable: Some(UnavailableReason::NotRecorded),
                    })
                    .collect();
                epoch
            })
            .collect();
        assert!(
            snapshot.encode().is_err(),
            "separate epochs cannot exceed the aggregate summary budget"
        );
    }
    #[test]
    fn bounded_snapshot_roundtrips_without_diagnostic_payloads_or_claiming_machine_scope() {
        let snapshot = sample();
        let bytes = snapshot.encode().unwrap();
        assert_eq!(Snapshot::decode(&bytes).unwrap(), snapshot);
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("SECRET"));
        assert!(text.contains("explicit-native-session"));
    }
    #[test]
    fn unknown_scope_version_fields_phases_and_duplicate_attempts_refuse() {
        let baseline: Value = aterm_json::from_slice(&sample().encode().unwrap()).unwrap();
        for (path, value) in [
            ("schema", json!(1)),
            ("schema", json!(SCHEMA + 1)),
            ("scope", json!("computer")),
            ("extra", json!(true)),
            ("generation", json!([0])),
        ] {
            let mut bad = baseline.clone();
            bad.as_object_mut().unwrap().insert(path.to_owned(), value);
            assert!(Snapshot::decode(&aterm_json::to_vec(&bad).unwrap()).is_err());
        }
        let mut bad = sample();
        bad.jobs.push(bad.jobs[0].clone());
        assert!(bad.encode().is_err());
        let mut unknown_phase = baseline.clone();
        rows_mut(&mut unknown_phase, "jobs")[0]
            .as_object_mut()
            .unwrap()
            .insert("phase".into(), json!("running"));
        refuses(&unknown_phase);
        let bytes = aterm_json::to_string(&baseline)
            .unwrap()
            .replace("\"os_error\":null", "\"extra\":null");
        assert!(Snapshot::decode(bytes.as_bytes()).is_err());
        let mut inconsistent = sample();
        inconsistent.jobs[0].phase = Phase::Waiting;
        assert!(inconsistent.encode().is_err());
        assert!(Snapshot::decode(&vec![0; MAX_FRAME_BYTES + 1]).is_err());
    }

    #[test]
    fn failed_build_retirement_and_uncertain_settlement_remain_separate() {
        let mut snapshot = sample();
        let row = &mut snapshot.epochs[0];
        row.retired = true;
        row.control_eof = true;
        row.helper_exited = true;
        let observed = Snapshot::decode(&snapshot.encode().unwrap()).unwrap();
        assert_eq!(
            observed.epochs[0].remaining_units, 2,
            "retirement is not settlement"
        );
        assert_eq!(observed.epochs[0].failed_roots, 1);

        let row = &mut snapshot.epochs[0];
        row.endpoint_cleaned = true;
        row.settled_units = 1;
        row.uncertain_units = 1;
        row.remaining_units = 0;
        row.failure = Some(Failure::of(&io::Error::other(
            "private settlement detail SECRET",
        )));
        let observed = Snapshot::decode(&snapshot.encode().unwrap()).unwrap();
        assert_eq!(observed.epochs[0].settled_units, 1);
        assert_eq!(observed.epochs[0].uncertain_units, 1);
        assert_eq!(
            observed.epochs[0].failed_roots, 1,
            "cleanup cannot turn a failed root green"
        );
        assert!(
            !observed.epochs[0].faulted,
            "settlement error is not a protocol fault"
        );
        assert!(
            !String::from_utf8(snapshot.encode().unwrap())
                .unwrap()
                .contains("SECRET")
        );
    }

    #[test]
    fn genuine_unpublished_cancellation_needs_no_fabricated_retirement() {
        let mut snapshot = sample();
        let row = &mut snapshot.epochs[0];
        row.opened = false;
        row.completed_roots = 0;
        row.failed_roots = 0;
        row.not_spawned = true;
        row.failure = Some(Failure::of(&io::Error::from(io::ErrorKind::NotFound)));
        assert_eq!(
            Snapshot::decode(&snapshot.encode().unwrap()).unwrap(),
            snapshot
        );
        let row = &mut snapshot.epochs[0];
        row.not_spawned = false;
        row.cancellation_started = true;
        row.endpoint_cleaned = true;
        row.settled_units = 2;
        row.remaining_units = 0;
        let observed = Snapshot::decode(&snapshot.encode().unwrap()).unwrap();
        assert!(observed.epochs[0].cancellation_started);
        assert!(!observed.epochs[0].retired);
        assert!(!observed.epochs[0].helper_exited);
        assert_eq!(observed.epochs[0].settled_units, 2);
    }

    fn value() -> Value {
        aterm_json::from_slice(&sample().encode().unwrap()).unwrap()
    }
    fn refuses(value: &Value) {
        assert!(Snapshot::decode(&aterm_json::to_vec(value).unwrap()).is_err());
    }
    fn rows_mut<'a>(value: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
        value
            .as_object_mut()
            .unwrap()
            .get_mut(key)
            .unwrap()
            .as_array_mut()
            .unwrap()
    }
    fn set_epoch(value: &mut Value, key: &str, field: Value) {
        rows_mut(value, "epochs")[0]
            .as_object_mut()
            .unwrap()
            .insert(key.into(), field);
    }

    #[test]
    fn decoder_refuses_release_without_actual_storage_closure() {
        let mut value = value();
        set_epoch(&mut value, "settled_units", json!(1));
        set_epoch(&mut value, "remaining_units", json!(1));
        refuses(&value);
        set_epoch(&mut value, "endpoint_cleaned", json!(true));
        refuses(&value);
        set_epoch(&mut value, "retired", json!(true));
        set_epoch(&mut value, "control_eof", json!(true));
        set_epoch(&mut value, "helper_exited", json!(true));
        assert!(Snapshot::decode(&aterm_json::to_vec(&value).unwrap()).is_ok());
    }

    #[test]
    fn decoder_refuses_false_eof_and_conflicting_launch_custody() {
        let mut value = value();
        set_epoch(&mut value, "control_eof", json!(true));
        refuses(&value);
        set_epoch(&mut value, "control_eof", json!(false));
        set_epoch(&mut value, "cancellation_started", json!(true));
        refuses(&value);
        set_epoch(&mut value, "cancellation_started", json!(false));
        set_epoch(&mut value, "opened", json!(false));
        set_epoch(&mut value, "completed_roots", json!(0));
        set_epoch(&mut value, "failed_roots", json!(0));
        set_epoch(&mut value, "not_spawned", json!(true));
        set_epoch(&mut value, "unbound", json!(true));
        refuses(&value);
    }

    #[test]
    fn epoch_counts_and_private_field_injection_refuse() {
        let mut value = value();
        set_epoch(&mut value, "failed_roots", json!(2));
        refuses(&value);
        set_epoch(&mut value, "failed_roots", json!(1));
        set_epoch(&mut value, "remaining_units", json!(u32::MAX));
        set_epoch(&mut value, "settled_units", json!(1));
        refuses(&value);
        let mut value = self::value();
        set_epoch(&mut value, "backing", json!(u64::MAX));
        refuses(&value);
        let mut value = self::value();
        set_epoch(&mut value, "fifo", json!("/private/SECRET"));
        refuses(&value);
        let mut value = self::value();
        let duplicate = value["epochs"][0].clone();
        rows_mut(&mut value, "epochs").push(duplicate);
        refuses(&value);
        let duplicate = String::from_utf8(sample().encode().unwrap())
            .unwrap()
            .replace("\"sequence\":1", "\"sequence\":1,\"sequence\":1");
        assert!(Snapshot::decode(duplicate.as_bytes()).is_err());
    }

    #[test]
    fn jobs_and_epochs_share_one_row_limit_but_have_separate_identity_spaces() {
        let mut snapshot = sample();
        snapshot.jobs = (1..MAX_JOBS as u64)
            .map(|attempt| Job {
                attempt,
                phase: Phase::Waiting,
                reserved_bytes: 0,
                failure: None,
            })
            .collect();
        let bytes = snapshot.encode().unwrap();
        assert_eq!(
            Snapshot::decode(&bytes).unwrap(),
            snapshot,
            "native attempt one and epoch one are distinct"
        );
        snapshot.jobs.push(Job {
            attempt: MAX_JOBS as u64,
            phase: Phase::Waiting,
            reserved_bytes: 0,
            failure: None,
        });
        assert!(snapshot.encode().is_err());
        let mut value: Value = aterm_json::from_slice(&bytes).unwrap();
        rows_mut(&mut value, "jobs")
            .push(json!({"attempt":256,"phase":"waiting","reserved_bytes":0,"failure":null}));
        refuses(&value);
    }
}
