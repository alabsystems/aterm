// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded host observations. Decoding data never creates native control authority.

use aterm_json::{Value, json};
use std::{collections::BTreeSet, io};

/// This observation schema. It describes an explicit native session, not a
/// qualified computer-wide allocation authority or proof result.
pub const SCHEMA: u64 = 1;
/// Maximum live native roots represented by one frame.
pub const MAX_JOBS: usize = 256;
/// Reject oversized input before JSON allocation.
pub const MAX_FRAME_BYTES: usize = 128 * 1024;

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

/// Version-one explicit native-session observation. Its existence neither installs
/// a machine singleton nor qualifies cross-account enforcement or native containment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Owner session incarnation, never a credential.
    pub generation: Generation,
    /// Monotonic publication cursor in this incarnation.
    pub revision: u64,
    /// The owner stopped publishing; retained rows remain its last observation.
    pub closed: bool,
    /// At most MAX_JOBS original native root observations.
    pub jobs: Vec<Job>,
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

impl Snapshot {
    fn validate(&self) -> io::Result<()> {
        if self.jobs.len() > MAX_JOBS {
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
        Ok(())
    }
    /// Encode only the closed projection, with explicit session scope.
    /// # Errors
    /// Invalid/oversized data or serialization failure.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let jobs: Vec<_> = self.jobs.iter().map(|job| json!({"attempt": job.attempt,
            "phase": job.phase.name(), "reserved_bytes": job.reserved_bytes,
            "failure": job.failure.map(|failure| json!({"kind":failure.kind.name(),"os_error":failure.os_error}))
        })).collect();
        let bytes = aterm_json::to_vec(&json!({"schema":SCHEMA,"scope":"explicit-native-session",
            "generation":self.generation.0.to_vec(),"revision":self.revision,"closed":self.closed,"jobs":jobs})).map_err(|_| bad())?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(bad());
        }
        Ok(bytes)
    }
    /// Decode bounded observation data. Unknown versions, scope, fields, phases,
    /// duplicate attempts and malformed errors refuse instead of acquiring defaults.
    /// # Errors
    /// Oversized, unsupported or malformed input. No native authority is created.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(bad());
        }
        let value: Value = aterm_json::from_slice(bytes).map_err(|_| bad())?;
        if !fields(
            &value,
            &[
                "schema",
                "scope",
                "generation",
                "revision",
                "closed",
                "jobs",
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
        let mut jobs = Vec::with_capacity(rows.len());
        for row in rows {
            if !fields(row, &["attempt", "phase", "reserved_bytes", "failure"]) {
                return Err(bad());
            }
            let failure = if row["failure"].is_null() {
                None
            } else {
                let error = &row["failure"];
                if !fields(error, &["kind", "os_error"]) {
                    return Err(bad());
                }
                Some(Failure {
                    kind: error["kind"]
                        .as_str()
                        .and_then(FailureKind::parse)
                        .ok_or_else(bad)?,
                    os_error: if error["os_error"].is_null() {
                        None
                    } else {
                        Some(
                            error["os_error"]
                                .as_i64()
                                .and_then(|n| i32::try_from(n).ok())
                                .ok_or_else(bad)?,
                        )
                    },
                })
            };
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
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        }
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
            ("schema", json!(2)),
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
        let bytes = aterm_json::to_string(&baseline)
            .unwrap()
            .replace("faulted", "running");
        assert!(Snapshot::decode(bytes.as_bytes()).is_err());
        let bytes = aterm_json::to_string(&baseline)
            .unwrap()
            .replace("\"os_error\":null", "\"extra\":null");
        assert!(Snapshot::decode(bytes.as_bytes()).is_err());
        let mut inconsistent = sample();
        inconsistent.jobs[0].phase = Phase::Waiting;
        assert!(inconsistent.encode().is_err());
        assert!(Snapshot::decode(&vec![0; MAX_FRAME_BYTES + 1]).is_err());
    }
}
