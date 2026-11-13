// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only acquisition observations, separate from checkout admission and completion.
//! Snapshot persistence uses Unix owner-private files. Other hosts retain the portable
//! observation/parser types but publish and read no snapshots.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Activity of a repository acquisition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Inspecting an existing checkout.
    Checking,
    /// Fetching changes into an existing checkout.
    Fetching,
    /// Fast-forwarding an existing checkout.
    Updating,
    /// Existing work was preserved instead of updated.
    Deferred,
    /// Waiting for a clone lane.
    Queued,
    /// Acquisition needs more disk space before it can start.
    WaitingForSpace,
    /// Contacting the source.
    Connecting,
    /// Receiving the object pack.
    Downloading,
    /// Resolving pack deltas.
    Resolving,
    /// Writing the checkout.
    Checkout,
    /// Validating the acquired repository.
    Verifying,
    /// Publishing its final destination.
    Placing,
    /// Acquiring required dependencies.
    Dependencies,
    /// Acquisition completed.
    Ready,
    /// Acquisition failed.
    Failed,
    /// The acquisition owner exited without completion.
    Interrupted,
}

impl Phase {
    /// Whether acquisition finished, including deferral, failure or interruption.
    pub const fn is_finished(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Failed | Self::Deferred | Self::Interrupted
        )
    }

    /// A short activity label for the table.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Checking => "Checking",
            Self::Fetching => "Fetching",
            Self::Updating => "Updating",
            Self::Deferred => "Update deferred",
            Self::Queued => "Queued",
            Self::WaitingForSpace => "Waiting for disk space",
            Self::Connecting => "Connecting",
            Self::Downloading => "Downloading",
            Self::Resolving => "Resolving",
            Self::Checkout => "Checking out",
            Self::Verifying => "Verifying",
            Self::Placing => "Placing",
            Self::Dependencies => "Dependencies",
            Self::Ready => "Ready",
            Self::Failed => "Failed",
            Self::Interrupted => "Interrupted",
        }
    }

    /// Stable status grouping: failed, active, queued, ready.
    pub const fn rank(self) -> u8 {
        match self {
            Self::Failed | Self::Interrupted => 0,
            Self::Queued | Self::WaitingForSpace => 2,
            Self::Ready | Self::Deferred => 3,
            _ => 1,
        }
    }
}

/// One timestamped stage transition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    /// Epoch milliseconds.
    pub at_ms: u64,
    /// Entered stage.
    pub phase: Phase,
}

/// What kind of operation failed, without inferring authentication or a retry schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureKind {
    /// A local operation failed or did not finish.
    Local,
    /// An admitted network operation returned failure.
    Network,
}

/// A checkout condition that caused an update to preserve existing work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreservationReason {
    /// Uncommitted changes or untracked files.
    LocalChanges,
    /// No origin is configured.
    MissingOrigin,
    /// Origin has multiple configured URLs.
    MultipleOrigins,
    /// Origin differs from the selected source.
    DifferentOrigin,
    /// Git rewrites origin away from the selected source.
    RewrittenOrigin,
    /// HEAD is detached or is not a branch.
    Detached,
    /// The local branch has no commit.
    NoLocalCommit,
    /// Origin advertises no commit.
    NoRemoteCommit,
    /// The checkout is on a different branch.
    DifferentBranch,
    /// The checkout has commits origin lacks.
    Ahead,
    /// The local and remote branches diverged.
    Diverged,
    /// The producer did not record a more specific condition.
    Unrecorded,
}

/// Safe outcome vocabulary. No command output, branch name or URL enters a snapshot here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeReason {
    /// Operation failure classified by its producer.
    Failed(FailureKind),
    /// Existing checkout preserved for a known condition.
    Preserved(PreservationReason),
    /// The reporting process exited without recording completion.
    ReporterExited,
}

impl OutcomeReason {
    /// Bounded application-owned wording; deliberately accepts no diagnostic text.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Failed(FailureKind::Local) => "A local repository operation did not complete.",
            Self::Failed(FailureKind::Network) => {
                "A network repository operation failed; its remote reason was not recorded."
            }
            Self::Preserved(reason) => match reason {
                PreservationReason::LocalChanges => {
                    "Existing checkout preserved: uncommitted changes or untracked files."
                }
                PreservationReason::MissingOrigin => {
                    "Existing checkout preserved: no origin remote is configured."
                }
                PreservationReason::MultipleOrigins => {
                    "Existing checkout preserved: origin has more than one URL."
                }
                PreservationReason::DifferentOrigin => {
                    "Existing checkout preserved: origin differs from the selected source."
                }
                PreservationReason::RewrittenOrigin => {
                    "Existing checkout preserved: Git rewrites origin away from the selected source."
                }
                PreservationReason::Detached => {
                    "Existing checkout preserved: HEAD is detached or is not on a branch."
                }
                PreservationReason::NoLocalCommit => {
                    "Existing checkout preserved: the local branch has no commit yet."
                }
                PreservationReason::NoRemoteCommit => {
                    "Existing checkout preserved: origin advertises no commits yet."
                }
                PreservationReason::DifferentBranch => {
                    "Existing checkout preserved: it is on a different branch from origin's default."
                }
                PreservationReason::Ahead => {
                    "Existing checkout preserved: it has commits that origin lacks."
                }
                PreservationReason::Diverged => {
                    "Existing checkout preserved: its branch has diverged from origin."
                }
                PreservationReason::Unrecorded => {
                    "Existing checkout preserved; the specific condition was not recorded."
                }
            },
            Self::ReporterExited => "The reporting process exited without recording completion.",
        }
    }
}

/// What the producer knows about retry, separate from outcome and usability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetryDisposition {
    /// The producer has no scheduling authority or recorded retry information.
    Unrecorded,
    /// A known local checkout condition must change before updating can proceed.
    AfterChanges,
}

impl RetryDisposition {
    /// Never promises an automatic retry or asserts that none will happen.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unrecorded => "Retry timing: not recorded.",
            Self::AfterChanges => {
                "Retry timing: not recorded; updating requires local checkout or configuration changes."
            }
        }
    }
}

/// A terminal outcome retains the stage that produced it independently of phase history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeDetail {
    /// Last observed activity before the outcome.
    pub stage: Phase,
    /// Typed, bounded reason; arbitrary diagnostics cannot be serialized as a reason.
    pub reason: OutcomeReason,
    /// Retry knowledge supplied by the producer.
    pub retry: RetryDisposition,
}

/// Last measured state of one repository attempt. Never an authorization record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// Repository name.
    pub name: String,
    /// Final checkout location.
    pub destination: PathBuf,
    /// Private temporary checkout, while known.
    pub staging: Option<PathBuf>,
    /// Acquisition owner's process ID.
    pub pid: u32,
    /// Kernel birth distinguishes a new process reusing the owner's PID.
    pub birth: Option<u64>,
    /// Attempt identity, including time and process-local sequence.
    pub attempt: String,
    /// Attempt start in epoch milliseconds.
    pub started_ms: u64,
    /// Last observation in epoch milliseconds.
    pub updated_ms: u64,
    /// Current activity.
    pub phase: Phase,
    /// Absent in older snapshots and when no terminal detail was recorded.
    #[serde(default)]
    pub outcome: Option<OutcomeDetail>,
    /// Current stage percentage; absent when unmeasured.
    pub percent: Option<u8>,
    /// Git's received amount, including its unit.
    pub received: Option<String>,
    /// Git's measured transfer rate, including its unit.
    pub speed: Option<String>,
    /// Whether the final checkout was published; dependencies may still be running.
    pub placed: bool,
    /// Whether the checkout is usable independently of this activity.
    pub usable: bool,
    /// Bounded stage history.
    pub history: Vec<Transition>,
    /// Up to three earlier attempt observations, rebuilt by the reader in observation-time order.
    pub previous: Vec<Transition>,
}

/// Epoch time for observations only.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl Progress {
    /// A new attempt waiting for a lane.
    pub fn new(name: &str, destination: PathBuf) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let started_ms = now_ms();
        let pid = std::process::id();
        Self {
            name: name.into(),
            destination,
            staging: None,
            pid,
            birth: i32::try_from(pid)
                .ok()
                .and_then(aterm_uds::process::read_birth)
                .map(|b| b.0),
            attempt: format!(
                "{started_ms:020}-{pid:010}-{:020}",
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            started_ms,
            updated_ms: started_ms,
            phase: Phase::Queued,
            outcome: None,
            percent: None,
            received: None,
            speed: None,
            placed: false,
            usable: false,
            history: vec![Transition {
                at_ms: started_ms,
                phase: Phase::Queued,
            }],
            previous: Vec::new(),
        }
    }

    /// Enter a stage, discarding the previous stage's percentage.
    pub fn enter(&mut self, phase: Phase) {
        self.updated_ms = now_ms();
        if self.phase != phase {
            self.phase = phase;
            self.outcome = None;
            self.percent = None;
            self.speed = None;
            if self.history.len() == 32 {
                self.history.remove(0);
            }
            self.history.push(Transition {
                at_ms: self.updated_ms,
                phase,
            });
        }
    }

    /// Complete with a classified failure, retaining the last stage and checkout usability.
    pub fn fail(&mut self, kind: FailureKind) {
        let stage = self.phase;
        self.enter(Phase::Failed);
        self.outcome = Some(OutcomeDetail {
            stage,
            reason: OutcomeReason::Failed(kind),
            retry: RetryDisposition::Unrecorded,
        });
    }

    /// Record why an update preserved an existing checkout, without scheduling a retry.
    pub fn defer(&mut self, reason: PreservationReason) {
        let stage = self.phase;
        self.enter(Phase::Deferred);
        self.outcome = Some(OutcomeDetail {
            stage,
            reason: OutcomeReason::Preserved(reason),
            retry: match reason {
                PreservationReason::LocalChanges
                | PreservationReason::MissingOrigin
                | PreservationReason::MultipleOrigins
                | PreservationReason::DifferentOrigin
                | PreservationReason::RewrittenOrigin
                | PreservationReason::Detached
                | PreservationReason::NoLocalCommit
                | PreservationReason::DifferentBranch => RetryDisposition::AfterChanges,
                _ => RetryDisposition::Unrecorded,
            },
        });
    }

    /// Observe owner loss without inventing a new report time or stage transition.
    /// A completed outcome remains historical after its owner exits.
    pub fn interrupt(&mut self) {
        if self.phase.is_finished() {
            return;
        }
        self.outcome = Some(OutcomeDetail {
            stage: self.phase,
            reason: OutcomeReason::ReporterExited,
            retry: RetryDisposition::Unrecorded,
        });
        self.phase = Phase::Interrupted;
        self.percent = None;
        self.speed = None;
    }

    /// Read a complete Git progress line. Unrecognized output is never published.
    pub fn git_line(&mut self, line: &str) -> bool {
        let line = line.trim();
        let (phase, rest) = if let Some(rest) = line.strip_prefix("Receiving objects:") {
            (
                if self.usable {
                    Phase::Fetching
                } else {
                    Phase::Downloading
                },
                rest,
            )
        } else if let Some(rest) = line.strip_prefix("Resolving deltas:") {
            (Phase::Resolving, rest)
        } else if let Some(rest) = line.strip_prefix("Updating files:") {
            (Phase::Checkout, rest)
        } else {
            return false;
        };
        let Some(percent) = rest
            .split('%')
            .next()
            .and_then(|v| v.trim().parse::<u8>().ok())
            .filter(|v| *v <= 100)
        else {
            return false;
        };
        self.enter(phase);
        self.percent = Some(percent);
        if matches!(phase, Phase::Downloading | Phase::Fetching)
            && let Some((_, amount)) = rest.split_once(", ")
        {
            let (received, speed) = amount.split_once('|').unwrap_or((amount, ""));
            self.received = metric(received.trim().trim_end_matches(", done."), false);
            self.speed = metric(speed.trim().trim_end_matches(", done."), true);
        }
        true
    }
}

fn metric(value: &str, rate: bool) -> Option<String> {
    let mut words = value.split_whitespace();
    let number = words.next()?;
    let unit = words.next()?;
    let numeric = number.parse::<f64>().ok()?;
    let unit = if rate { unit.strip_suffix("/s")? } else { unit };
    (words.next().is_none()
        && numeric.is_finite()
        && numeric >= 0.0
        && matches!(unit, "bytes" | "KiB" | "MiB" | "GiB" | "TiB"))
    .then(|| value.to_owned())
}

/// Bounded incremental parser for Git's carriage-return progress stream.
#[derive(Default)]
pub struct GitProgress {
    line: Vec<u8>,
    oversized: bool,
}

impl GitProgress {
    /// Consume arbitrary chunks, preserving partial lines and UTF-8 between chunks.
    pub fn feed(&mut self, bytes: &[u8], progress: &mut Progress) -> bool {
        let mut changed = false;
        for &byte in bytes {
            if matches!(byte, b'\r' | b'\n') {
                if !self.oversized
                    && let Ok(line) = std::str::from_utf8(&self.line)
                {
                    changed |= progress.git_line(line);
                }
                self.line.clear();
                self.oversized = false;
            } else if self.line.len() < 4096 {
                self.line.push(byte);
            } else {
                self.oversized = true;
            }
        }
        changed
    }

    /// Finish a final unterminated progress line at proven EOF.
    pub fn finish(&mut self, progress: &mut Progress) -> bool {
        self.feed(b"\n", progress)
    }
}

#[cfg(unix)]
fn directory() -> Option<PathBuf> {
    aterm_types::dirs::account_state_dir().map(|p| p.join("repository-progress"))
}

/// Publish atomically. Failure of diagnostic reporting never fails acquisition.
#[cfg(unix)]
pub fn publish(progress: &Progress) {
    let Some(dir) = directory() else {
        return;
    };
    if aterm_uds::private_file::ensure_private_dir(&dir).is_err() {
        return;
    }
    if let Ok(bytes) = aterm_json::to_vec(progress) {
        let _ = aterm_uds::atomic_file::write(
            &dir.join(format!("{}.json", progress.attempt)),
            &bytes,
            0o600,
        );
        if progress.phase.is_finished() {
            prune(&dir, progress);
        }
    }
}

#[cfg(unix)]
fn snapshot(dir: &std::path::Path, entry: &std::fs::DirEntry) -> Option<Progress> {
    let name = entry.file_name();
    let name = name.to_str()?;
    if !name.ends_with(".json") {
        return None;
    }
    let bytes = aterm_uds::private_file::read_private(dir, name, 32768).ok()?;
    let row = aterm_json::from_slice::<Progress>(&bytes).ok()?;
    (name == format!("{}.json", row.attempt) && row.name.len() <= 255 && row.history.len() <= 32)
        .then_some(row)
}

#[cfg(unix)]
fn owner_running(row: &Progress) -> bool {
    let live_birth = i32::try_from(row.pid)
        .ok()
        .and_then(aterm_uds::process::read_birth)
        .map(|birth| birth.0);
    aterm_uds::process::pid_alive(row.pid)
        && !matches!((row.birth, live_birth), (Some(was), Some(now)) if was != now)
}

#[cfg(unix)]
fn attempt_order(row: &Progress) -> (u64, &str) {
    (row.started_ms, row.attempt.as_str())
}

#[cfg(unix)]
fn prune(dir: &std::path::Path, current: &Progress) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut old = entries
        .flatten()
        .filter_map(|entry| {
            let row = snapshot(dir, &entry)?;
            (row.destination == current.destination
                && attempt_order(&row) < attempt_order(current)
                && (row.phase.is_finished() || !owner_running(&row)))
            .then_some(((row.started_ms, row.attempt), entry.path()))
        })
        .collect::<Vec<_>>();
    old.sort_by(|(left, _), (right, _)| right.cmp(left));
    for (_, path) in old.into_iter().skip(3) {
        let _ = std::fs::remove_file(path);
    }
}

/// Read the newest attempt for each destination. No Git or network operations.
#[cfg(unix)]
pub fn read() -> Vec<Progress> {
    let Some(dir) = directory() else {
        return Vec::new();
    };
    read_from(&dir)
}

/// No private snapshot backend is available on this host.
#[cfg(not(unix))]
pub fn publish(_progress: &Progress) {}

/// No private snapshot backend is available on this host.
#[cfg(not(unix))]
pub fn read() -> Vec<Progress> {
    Vec::new()
}

#[cfg(unix)]
fn read_from(dir: &std::path::Path) -> Vec<Progress> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read_rows(entries.flatten().filter_map(|entry| snapshot(dir, &entry)))
}

#[cfg(unix)]
struct Attempts {
    latest: Progress,
    previous: std::collections::BTreeMap<(u64, u64, String), Phase>,
}

#[cfg(unix)]
fn read_rows(rows: impl IntoIterator<Item = Progress>) -> Vec<Progress> {
    use std::collections::{BTreeMap, btree_map::Entry};

    let mut latest = BTreeMap::<PathBuf, Attempts>::new();
    for mut row in rows {
        // History is derived from this scan, not recursively from persisted reader output.
        row.previous = Vec::new();
        // Completed outcomes outlive their owner; every unfinished phase needs one,
        // including disk waits, irrespective of its presentation sort group.
        if !row.phase.is_finished() && !owner_running(&row) {
            row.interrupt();
        }
        let attempts = match latest.entry(row.destination.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(Attempts {
                    latest: row,
                    previous: BTreeMap::new(),
                });
                continue;
            }
            Entry::Occupied(entry) => entry.into_mut(),
        };
        if attempt_order(&attempts.latest) < attempt_order(&row) {
            row = std::mem::replace(&mut attempts.latest, row);
        }
        attempts
            .previous
            .insert((row.updated_ms, row.started_ms, row.attempt), row.phase);
        if attempts.previous.len() > 3 {
            let _ = attempts.previous.pop_first();
        }
    }
    latest
        .into_values()
        .map(|mut attempts| {
            attempts.latest.previous = attempts
                .previous
                .into_iter()
                .map(|((at_ms, _, _), phase)| Transition { at_ms, phase })
                .collect();
            attempts.latest
        })
        .collect()
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
