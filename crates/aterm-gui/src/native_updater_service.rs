// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-owned reducer for the native Software Update route.
//!
//! The updater crate deliberately performs a manual check, download, verification, and
//! staging as one blocking [`aterm_update::check_now`] operation.  This reducer keeps that
//! physical operation single-flight while exposing the logical `Checking -> Available ->
//! Downloading -> Staged` transitions required by the native-app contract.  The host owns
//! threads and process replacement; this type owns acceptance, generations, publication,
//! close-preflight binding, and the at-most-once re-exec decision.
//!
//! A Settings view never owns this value.  Store one on `App`, deliver worker results here,
//! then broadcast the returned [`UpdaterSnapshot::revision`] to every Settings subscriber.

use std::fmt;

const MAX_SHORT_TEXT_BYTES: usize = 512;
const MAX_MESSAGE_BYTES: usize = 8 * 1024;
const MAX_CHANGELOG_BYTES: usize = 256 * 1024;

/// Stable logical phase shared by presentation, inspection, and apply policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdaterPhase {
    Disabled,
    Idle,
    Checking,
    Available,
    Downloading,
    Staged,
    Applying,
    Failed,
}

/// The physical work represented by a generation-stamped ticket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdaterWorkKind {
    CheckAndStage,
    Download,
}

/// Exact identity of updater work.  Completions are accepted only when this entire value
/// equals the service's active ticket; an operation number alone is never sufficient.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UpdaterWorkTicket {
    generation: u64,
    operation: u64,
    kind: UpdaterWorkKind,
}

/// Field-for-field, bounded host projection of [`aterm_update::UpdateStatus`].
///
/// Construct this only from the value returned by `aterm_update::check_now` (or the
/// durable `aterm_update::status` reader).  In that API, `staged_build` comes from
/// `ready.toml`, which is published only after download, integrity/authenticity checks,
/// bundle verification, and atomic staging have completed.  Apply still re-verifies the
/// staged bundle inside `aterm_update` to close the time-of-check/time-of-use gap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DurableUpdateStatus {
    pub(crate) enabled: bool,
    pub(crate) current_build: u64,
    pub(crate) staged_build: Option<u64>,
    pub(crate) staged_version: Option<String>,
    pub(crate) staged_commit: Option<String>,
    pub(crate) staged_dmg_sha256: Option<String>,
    pub(crate) changelog: Option<String>,
    pub(crate) outcome: String,
    pub(crate) failing_checks: u32,
    /// The durable health ledger's verdict that the streak is PERSISTENT (≥ the
    /// updater's `PERSISTENT_AFTER` consecutive failures of one class): "this Mac
    /// cannot update until X is fixed", not "one check blipped". Carried so the app
    /// can SHOW it — measured 2026-08-18: a fleet-wide publisher-side rejection ran
    /// for eight hours, visible only in `aterm ctl update status` and the log,
    /// while the update screen said "You're up to date."
    pub(crate) failing_persistent: bool,
    /// The failing class the ledger recorded (`network` / `pipeline` / `manifest` /
    /// `stage` / `apply`), empty when not failing. This is the MOST RECENT class,
    /// not necessarily the one that escalated — see [`Self::failing_applies`].
    pub(crate) failing_kind: String,
    /// Consecutive APPLY failures. The exact signal for "the staged build will not
    /// start", which `failing_kind` cannot give: a machine whose apply lane
    /// escalated but whose last failure was a network blip records `network`
    /// (2026-08-19 round-4 skeptics).
    pub(crate) failing_applies: u32,
}

/// Verified artifact identity retained across Settings-view close/reopen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StagedUpdate {
    pub(crate) build: u64,
    pub(crate) version: String,
    pub(crate) commit: Option<String>,
    pub(crate) dmg_sha256: String,
    pub(crate) changelog: Option<String>,
    pub(crate) generation: u64,
}

impl StagedUpdate {
    /// Whether this stage is an INSTALLED-BUNDLE ACTIVATION rather than a downloaded
    /// artifact: its identity is [`installed_activation_digest`] over its own build and
    /// sealed commit, and its backing is the bundle at the running executable's path,
    /// not a `ready.toml` marker (see [`installed_activation_digest`]).
    #[must_use]
    pub(crate) fn is_installed_activation(&self) -> bool {
        self.commit
            .as_deref()
            .is_some_and(|commit| installed_activation_digest(self.build, commit) == self.dmg_sha256)
    }
}

/// THE ACTIVATION IDENTITY. When the bundle at this process's own path is already a
/// NEWER build than the process itself — installed by another producer: the release
/// cutter writing into the bundle it was launched from, a user dragging a new `.app`
/// over the old one, a sibling aterm that swapped it — there is no downloaded DMG to
/// name, but every latch, budget and ticket in this reducer keys on an artifact digest.
/// So the activation IS an artifact: a well-formed 64-hex identity derived from the
/// sealed build number and commit, distinct per installed build, impossible to confuse
/// with any real DMG digest by construction (a real digest is a hash of bytes nobody
/// can choose to collide with this domain-separated one). It flows through the SAME
/// stage → apply → returned machinery as a download; the two places that must know
/// the difference — durable/returned reconciliation (backed by the installed bundle,
/// not by `ready.toml`) and the handoff's pre-park verification (verify the bundle
/// under the executable, bind no expected artifact for the successor) — ask
/// [`StagedUpdate::is_installed_activation`] / [`ApplyAttemptTicket::is_installed_activation`].
///
/// Why activate at all instead of asking for a relaunch: the seamless handoff exists
/// precisely so a newer build takes over without losing a shell. "Relaunch once to
/// activate it" (the previous answer) left a verified, notarized, already-installed
/// build sitting inert until a human noticed a log line — measured 2026-08-18: the
/// v0.25.0 roll-forward stayed un-activated for hours in the very window it was cut
/// from. A bundle newer than the process is exactly a staged update whose swap has
/// already happened; the only remaining step is the handoff.
#[must_use]
pub(crate) fn installed_activation_digest(build: u64, commit: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"aterm installed-bundle activation\0");
    hasher.update(build.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(commit.trim().to_ascii_lowercase().as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Compare one durable marker to an already-canonical artifact identity.
///
/// SHA-1/SHA-256 hex has a single byte meaning but two textual letter cases.
/// `staged_from_status` canonicalizes imported identities to lowercase, while the
/// durable marker reader intentionally preserves its source text. Reconciliation
/// must therefore compare canonical hex values, not their incidental spelling, or
/// a valid uppercase marker is imported and then retired by the next observation.
#[must_use]
pub(crate) fn durable_artifact_identity_matches(
    durable_build: Option<u64>,
    durable_commit: Option<&str>,
    durable_dmg_sha256: Option<&str>,
    expected_build: u64,
    expected_commit: Option<&str>,
    expected_dmg_sha256: &str,
) -> bool {
    fn exact_hex(left: Option<&str>, right: Option<&str>, bytes: usize) -> bool {
        let (Some(left), Some(right)) = (left, right) else {
            return false;
        };
        let left = left.trim();
        let right = right.trim();
        let width = bytes.saturating_mul(2);
        left.len() == width
            && right.len() == width
            && left.bytes().all(|byte| byte.is_ascii_hexdigit())
            && right.bytes().all(|byte| byte.is_ascii_hexdigit())
            && left.eq_ignore_ascii_case(right)
    }

    durable_build == Some(expected_build)
        && exact_hex(durable_commit, expected_commit, 20)
        && exact_hex(durable_dmg_sha256, Some(expected_dmg_sha256), 32)
}

/// Worker-collected identity of the canonical installed bundle and the exact
/// pre-swap trial artifact. Build-only evidence must never authorize a relaunch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstalledUpdate {
    pub(crate) build: u64,
    pub(crate) commit: String,
    /// Marketing version of the installed bundle (display only; `None` when unread).
    pub(crate) version: Option<String>,
    pub(crate) receipt_build: Option<u64>,
    pub(crate) receipt_dmg_sha256: Option<String>,
}

impl InstalledUpdate {
    #[must_use]
    fn proves_artifact(&self, build: u64, commit: Option<&str>, dmg_sha256: &str) -> bool {
        self.build == build
            && commit.is_some_and(|commit| aterm_update::commit_matches(&self.commit, commit))
            && self.receipt_build == Some(build)
            && self
                .receipt_dmg_sha256
                .as_deref()
                .is_some_and(|trial| trial.eq_ignore_ascii_case(dmg_sha256))
    }

    /// Whether this installed bundle BACKS an activation of `(build, commit)`: it is that
    /// exact sealed identity and it is strictly newer than the running process. This is
    /// the activation's counterpart of `proves_artifact` — the bundle under the executable
    /// is the artifact, so no receipt is involved.
    #[must_use]
    pub(crate) fn backs_activation(&self, running_build: u64, build: u64, commit: &str) -> bool {
        self.build == build
            && self.build > running_build
            && aterm_update::commit_matches(&self.commit, commit)
    }

    /// The activation stage this installed bundle would present, when it is newer than
    /// the running build: the sealed identity as a [`StagedUpdate`] whose digest is the
    /// [`installed_activation_digest`]. `None` when the bundle is not newer.
    #[must_use]
    pub(crate) fn activation_stage(&self, running_build: u64, generation: u64) -> Option<StagedUpdate> {
        // The same identity rule the import applies (`staged_from_status`), so a
        // bundle whose sealed commit is unusable is not an activation at all —
        // it must not retire a download it can never replace.
        (self.build > running_build && usable_commit_identity(&self.commit)).then(|| {
            let commit = self.commit.trim().to_ascii_lowercase();
            StagedUpdate {
                build: self.build,
                version: bounded(
                    self.version
                        .clone()
                        .unwrap_or_else(|| format!("build {}", self.build)),
                    MAX_SHORT_TEXT_BYTES,
                ),
                dmg_sha256: installed_activation_digest(self.build, &commit),
                commit: Some(commit),
                changelog: None,
                generation,
            }
        })
    }
}

/// Immutable service projection.  UI, tab attention, accessibility, and inspection all
/// consume this same value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdaterSnapshot {
    pub(crate) revision: u64,
    pub(crate) generation: u64,
    pub(crate) phase: UpdaterPhase,
    pub(crate) enabled: bool,
    pub(crate) current_build: u64,
    pub(crate) current_version: String,
    pub(crate) active: Option<UpdaterWorkTicket>,
    pub(crate) staged: Option<StagedUpdate>,
    pub(crate) outcome: String,
    pub(crate) error: Option<String>,
    pub(crate) install_on_clean_quit: bool,
    pub(crate) attention_revision: Option<u64>,
    pub(crate) acknowledged_attention_revision: Option<u64>,
    pub(crate) ignored_completions: u64,
    pub(crate) reexec_count: u64,
    /// The health ledger's PERSISTENT-failure verdict as of the last reduced check
    /// (`DurableUpdateStatus::failing_persistent`): the update screen headlines it,
    /// and the Settings tab's attention badge lights for it exactly as it does for a
    /// staged build — a machine that cannot update is at least as worth a glance as
    /// one that can. Cleared the moment a check reduces to staged or up-to-date.
    pub(crate) failing_persistent: bool,
    /// The failing class behind `failing_persistent` (empty otherwise).
    pub(crate) failing_kind: String,
    /// Consecutive APPLY failures from the ledger — the exact "the staged build
    /// will not start" signal (`failing_kind` is only the most recent class).
    pub(crate) failing_applies: u32,
}

impl UpdaterSnapshot {
    #[must_use]
    pub(crate) fn attention_pending(&self) -> bool {
        self.attention_revision.is_some()
            && self.attention_revision != self.acknowledged_attention_revision
    }

    /// No determinate progress is invented: today's `aterm_update::check_now` API does
    /// not provide a byte denominator, so the native route must render an indeterminate
    /// busy state while this is true.
    #[must_use]
    pub(crate) fn has_determinate_progress(&self) -> bool {
        false
    }
}

/// Decision returned when a view asks the process service to check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckStart {
    /// The host must spawn exactly one `aterm_update::check_now` worker for this ticket.
    Start(UpdaterWorkTicket),
    /// Work is already running; subscribe to revisions rather than spawning another job.
    Joined(UpdaterWorkTicket),
    Rejected(CheckBlock),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckBlock {
    Disabled,
    UpdateAlreadyStaged,
    Applying,
    GenerationExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IgnoredCompletion {
    StaleGeneration,
    WrongOperation,
    WrongKind,
    NoActiveWork,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckCompletion {
    Reduced,
    Ignored(IgnoredCompletion),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplyMode {
    /// Background policy: exact stage authority plus a short terminal-idle
    /// epoch is required before readers may park.
    Automatic,
    /// Background policy whose bounded idle-preference window has closed. Exact
    /// stage authority is unchanged and every safety gate still applies; only
    /// the idle wait — and the activity revocation that mirrors it — is
    /// dropped, so a machine that is never quiet still gets the update instead
    /// of deferring until the user clicks Install. Distinct from `Immediate`
    /// because no user asked for this one, so the log/notification wording and
    /// the automatic retry budget still treat it as background work.
    AutomaticPastGrace,
    /// Explicit user/control apply: bypasses only the idle delay, never safety.
    Immediate,
    CleanQuit,
}

impl ApplyMode {
    /// Whether this attempt came from background policy rather than a person.
    /// Both automatic lanes share the retry budget and the non-disruptive
    /// surfacing; they differ only in whether idleness is still awaited.
    #[must_use]
    pub(crate) fn is_automatic(self) -> bool {
        matches!(self, Self::Automatic | Self::AutomaticPastGrace)
    }
}

/// Generation-bound request for the host's process-wide quit-readiness reducer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ApplyPreflightTicket {
    generation: u64,
    artifact_build: u64,
    operation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplyPreflightStart {
    Inspect(ApplyPreflightTicket),
    Joined(ApplyPreflightTicket),
    Disabled,
    NotStaged,
    NotDeferred,
    Applying,
    GenerationExhausted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClosePreflight {
    Ready,
    Blocked(Vec<String>),
}

/// A linear host command: it is intentionally neither `Clone` nor `Copy`, and executing
/// it consumes the value.  The reducer issues at most one for an accepted artifact.
#[must_use = "an authorized updater re-exec command must be executed or explicitly dropped"]
pub(crate) struct ReexecCommand {
    build: u64,
    version: String,
    commit: String,
    dmg_sha256: String,
    generation: u64,
    operation: u64,
}

/// Identity retained by the old process while it attempts the authorized handoff.
/// A failure may re-arm only the exact staged artifact that minted this ticket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApplyAttemptTicket {
    generation: u64,
    artifact_build: u64,
    artifact_commit: String,
    artifact_dmg_sha256: String,
    operation: u64,
}

impl fmt::Debug for ReexecCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReexecCommand")
            .field("build", &self.build)
            .field("version", &self.version)
            .field("commit", &self.commit)
            .field("dmg_sha256", &self.dmg_sha256)
            .field("generation", &self.generation)
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}

impl ReexecCommand {
    #[must_use]
    pub(crate) fn attempt(&self) -> ApplyAttemptTicket {
        ApplyAttemptTicket {
            generation: self.generation,
            artifact_build: self.build,
            artifact_commit: self.commit.clone(),
            artifact_dmg_sha256: self.dmg_sha256.clone(),
            operation: self.operation,
        }
    }

    /// Consume the one-shot authority while invoking the existing seamless apply path.
    pub(crate) fn execute<T>(self, apply: impl FnOnce() -> T) -> T {
        apply()
    }
}

impl ApplyAttemptTicket {
    /// Test-only exact-artifact ticket for retry-budget proofs that exercise
    /// the App-side completion path without driving the whole reducer.
    #[cfg(test)]
    pub(crate) fn for_test(build: u64, commit: &str, dmg_sha256: &str) -> Self {
        Self {
            generation: 1,
            artifact_build: build,
            artifact_commit: commit.to_string(),
            artifact_dmg_sha256: dmg_sha256.to_string(),
            operation: 1,
        }
    }

    /// Expected compiled build the child must report before parent commit.
    #[must_use]
    pub(crate) fn target_build(&self) -> u64 {
        self.artifact_build
    }

    /// Test-only: drive a service into the exact state where this ticket is the
    /// CURRENT apply, so `abort_apply` admits it. Without this seam the
    /// completion path could only be tested by calling its policy helpers
    /// directly — which is precisely how a permanently-latching completion path
    /// survived a green suite: the helpers were tested, the path was not.
    #[cfg(test)]
    pub(crate) fn make_current_apply_for_test(&self, service: &mut NativeUpdaterService) {
        service.snapshot.generation = self.generation;
        service.snapshot.phase = UpdaterPhase::Applying;
        service.snapshot.reexec_count = 1;
        service.snapshot.staged = Some(StagedUpdate {
            build: self.artifact_build,
            version: "0.0.0".to_string(),
            commit: Some(self.artifact_commit.clone()),
            dmg_sha256: self.artifact_dmg_sha256.clone(),
            changelog: None,
            generation: self.generation,
        });
        service.active_apply = Some(self.clone());
    }

    /// Exact staged artifact digest bound into this apply authority.
    #[must_use]
    pub(crate) fn target_dmg_sha256(&self) -> &str {
        &self.artifact_dmg_sha256
    }

    #[must_use]
    pub(crate) fn target_commit(&self) -> &str {
        &self.artifact_commit
    }

    /// Whether this attempt activates the INSTALLED bundle at the executable's path
    /// (see [`installed_activation_digest`]) rather than swapping in a staged download.
    #[must_use]
    pub(crate) fn is_installed_activation(&self) -> bool {
        installed_activation_digest(self.artifact_build, &self.artifact_commit)
            == self.artifact_dmg_sha256
    }
}

#[derive(Debug)]
pub(crate) enum ApplyDecision {
    Execute(ReexecCommand),
    Blocked(Vec<String>),
    Ignored,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReturnedApplyDisposition {
    /// The exact verified artifact still exists durably and was re-armed.
    Rearmed,
    /// The staged marker was consumed and this build (or a superseding one) is now
    /// installed at the canonical app path; only a user-visible relaunch remains.
    InstalledNeedsRelaunch { build: u64 },
    /// The attempted artifact is no longer authoritative and was retired in memory.
    Retired,
    /// The callback did not name the currently-live apply authority.
    Ignored,
}

/// One atomic projection of the durable updater state observed after a process
/// replacement attempt returned. Grouping these fields prevents a caller from
/// accidentally pairing a stage identity with an installed receipt from another
/// observation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReturnedApplyFacts<'a> {
    durable_enabled: bool,
    durable_staged_build: Option<u64>,
    durable_staged_commit: Option<&'a str>,
    durable_staged_dmg_sha256: Option<&'a str>,
    installed: Option<&'a InstalledUpdate>,
}

impl<'a> ReturnedApplyFacts<'a> {
    pub(crate) const fn new(
        durable_enabled: bool,
        durable_staged_build: Option<u64>,
        durable_staged_commit: Option<&'a str>,
        durable_staged_dmg_sha256: Option<&'a str>,
        installed: Option<&'a InstalledUpdate>,
    ) -> Self {
        Self {
            durable_enabled,
            durable_staged_build,
            durable_staged_commit,
            durable_staged_dmg_sha256,
            installed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DurableStageDisposition {
    Unchanged,
    InstalledNeedsRelaunch { build: u64 },
    Retired,
}

/// Exact scalar projection used by Tier-1 to bind this reducer to
/// `native_updater_model`.  It deliberately excludes presentation-only revision data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UpdaterModelState {
    pub(crate) phase: u8,
    pub(crate) request_generation: u64,
    pub(crate) work_generation: u64,
    pub(crate) artifact_generation: u64,
    pub(crate) active_work: bool,
    pub(crate) stale_completion_pending: bool,
    pub(crate) verified: bool,
    pub(crate) close_preflight: bool,
    pub(crate) install_on_clean_quit: bool,
    pub(crate) reexec_count: u64,
    pub(crate) stale_staged: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdaterTransitionAction {
    StartCheck,
    RetryCheck,
    CheckAvailable,
    CheckFailed,
    Disable,
    StartDownload,
    CompleteDownload,
    MarkCloseReady,
    InstallOnCleanQuit,
    Apply,
    AbortApply,
    RetireApply,
    RetireStage,
    CheckUpToDate,
}

impl UpdaterTransitionAction {
    /// Exact derived-model action name for Tier-1 transition validation.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn model_action(self) -> Option<&'static str> {
        match self {
            Self::StartCheck => Some("StartCheck"),
            Self::RetryCheck => Some("RetryCheck"),
            Self::CheckAvailable => Some("CheckAvailable"),
            Self::CheckFailed => Some("CheckFailed"),
            // Disabled-state reconciliation is modeled separately from the original
            // updater lifecycle; Tier-1 must not pretend this is CheckFailed.
            Self::Disable => None,
            Self::StartDownload => Some("StartDownload"),
            Self::CompleteDownload => Some("CompleteDownload"),
            Self::MarkCloseReady => Some("MarkCloseReady"),
            Self::InstallOnCleanQuit => Some("InstallOnCleanQuit"),
            Self::Apply => Some("Apply"),
            Self::AbortApply => Some("AbortApply"),
            Self::RetireApply => Some("RetireApply"),
            Self::RetireStage => Some("RetireStage"),
            Self::CheckUpToDate => Some("CheckUpToDate"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UpdaterTransition {
    pub(crate) action: UpdaterTransitionAction,
    pub(crate) before: UpdaterModelState,
    pub(crate) after: UpdaterModelState,
}

/// Process-global updater state.  It contains no view/window identity by design.
pub(crate) struct NativeUpdaterService {
    snapshot: UpdaterSnapshot,
    next_operation: u64,
    work_generation: u64,
    close_preflight_ready: bool,
    pending_preflight: Option<ApplyPreflightTicket>,
    active_apply: Option<ApplyAttemptTicket>,
    last_transitions: Vec<UpdaterTransition>,
}

impl NativeUpdaterService {
    #[must_use]
    pub(crate) fn new(
        current_build: u64,
        current_version: impl Into<String>,
        enabled: bool,
    ) -> Self {
        Self {
            snapshot: UpdaterSnapshot {
                revision: 0,
                generation: 0,
                phase: if enabled {
                    UpdaterPhase::Idle
                } else {
                    UpdaterPhase::Disabled
                },
                enabled,
                current_build,
                current_version: bounded(current_version.into(), MAX_SHORT_TEXT_BYTES),
                active: None,
                staged: None,
                outcome: String::new(),
                error: None,
                install_on_clean_quit: false,
                attention_revision: None,
                acknowledged_attention_revision: None,
                ignored_completions: 0,
                reexec_count: 0,
                failing_persistent: false,
                failing_kind: String::new(),
                failing_applies: 0,
            },
            next_operation: 1,
            work_generation: 0,
            close_preflight_ready: false,
            pending_preflight: None,
            active_apply: None,
            last_transitions: Vec::with_capacity(3),
        }
    }

    /// Seed process state from the updater's durable marker before any native view opens.
    /// This is initialization, not an asynchronous completion; subsequent work is fully
    /// generation checked.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_durable_status(
        current_build: u64,
        current_version: impl Into<String>,
        status: DurableUpdateStatus,
    ) -> Self {
        let enabled = status.enabled;
        let mut service = Self::new(current_build, current_version, enabled);
        service.snapshot.outcome = bounded(status.outcome.clone(), MAX_MESSAGE_BYTES);
        if !enabled {
            // A leftover ready marker is not apply authority when this build/machine
            // has disabled updates. Keep it invisible and non-actionable.
            service.snapshot.phase = UpdaterPhase::Disabled;
        } else if let Some(artifact) = staged_from_status(current_build, 1, &status) {
            service.snapshot.generation = 1;
            service.work_generation = 1;
            service.snapshot.phase = UpdaterPhase::Staged;
            service.snapshot.staged = Some(artifact);
            service.snapshot.revision = 1;
            service.snapshot.attention_revision = Some(1);
        } else if enabled && status.failing_checks > 0 {
            service.snapshot.phase = UpdaterPhase::Failed;
            service.snapshot.error = Some(failure_message(&status));
            service.snapshot.revision = 1;
        }
        service
    }

    #[must_use]
    pub(crate) fn snapshot(&self) -> &UpdaterSnapshot {
        &self.snapshot
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn last_transitions(&self) -> &[UpdaterTransition] {
        &self.last_transitions
    }

    #[must_use]
    pub(crate) fn model_state(&self) -> UpdaterModelState {
        let artifact_generation = self
            .snapshot
            .staged
            .as_ref()
            .map_or(0, |staged| staged.generation);
        UpdaterModelState {
            phase: match self.snapshot.phase {
                UpdaterPhase::Disabled | UpdaterPhase::Idle => 0,
                UpdaterPhase::Checking => 1,
                UpdaterPhase::Available => 2,
                UpdaterPhase::Downloading => 3,
                UpdaterPhase::Staged => 4,
                UpdaterPhase::Applying => 5,
                UpdaterPhase::Failed => 6,
            },
            request_generation: self.snapshot.generation,
            work_generation: self.work_generation,
            artifact_generation,
            active_work: self.snapshot.active.is_some(),
            stale_completion_pending: false,
            verified: self.snapshot.staged.is_some(),
            close_preflight: self.close_preflight_ready,
            install_on_clean_quit: self.snapshot.install_on_clean_quit,
            reexec_count: self.snapshot.reexec_count,
            stale_staged: false,
        }
    }

    /// Join an existing check or mint the sole physical worker ticket.
    pub(crate) fn request_check(&mut self) -> CheckStart {
        self.last_transitions.clear();
        if !self.snapshot.enabled {
            return CheckStart::Rejected(CheckBlock::Disabled);
        }
        if let Some(active) = self.snapshot.active {
            return CheckStart::Joined(active);
        }
        let action = match self.snapshot.phase {
            UpdaterPhase::Disabled => return CheckStart::Rejected(CheckBlock::Disabled),
            UpdaterPhase::Idle => UpdaterTransitionAction::StartCheck,
            UpdaterPhase::Failed => UpdaterTransitionAction::RetryCheck,
            UpdaterPhase::Staged => {
                return CheckStart::Rejected(CheckBlock::UpdateAlreadyStaged);
            }
            UpdaterPhase::Applying => return CheckStart::Rejected(CheckBlock::Applying),
            // Available/Downloading are atomic microstates for the combined updater API.
            UpdaterPhase::Checking | UpdaterPhase::Available | UpdaterPhase::Downloading => {
                return CheckStart::Rejected(CheckBlock::GenerationExhausted);
            }
        };
        let Some(generation) = self.snapshot.generation.checked_add(1) else {
            return CheckStart::Rejected(CheckBlock::GenerationExhausted);
        };
        let Some(operation) = self.allocate_operation() else {
            return CheckStart::Rejected(CheckBlock::GenerationExhausted);
        };
        let ticket = UpdaterWorkTicket {
            generation,
            operation,
            kind: UpdaterWorkKind::CheckAndStage,
        };
        let before = self.model_state();
        self.snapshot.generation = generation;
        self.snapshot.phase = UpdaterPhase::Checking;
        self.snapshot.active = Some(ticket);
        self.snapshot.error = None;
        self.snapshot.outcome = "Checking for updates".to_string();
        self.snapshot.staged = None;
        self.snapshot.install_on_clean_quit = false;
        self.close_preflight_ready = false;
        self.pending_preflight = None;
        self.active_apply = None;
        self.record(action, before);
        self.publish();
        CheckStart::Start(ticket)
    }

    /// Reduce the exact result of one `aterm_update::check_now` worker.
    /// Absorb the ledger's FAILURE verdict from a reconcile that imports no stage
    /// (the exact-match early path, or a held stage that refuses a new check):
    /// `finish_check` is otherwise the only writer of `failing_persistent`, and an
    /// apply-class streak builds while a stage is HELD — so the headline the
    /// health notice pointed at never changed (2026-08-19 round-3 audit). Badges
    /// the false→true edge exactly as `finish_check` does.
    pub(crate) fn absorb_failure_state(&mut self, status: &DurableUpdateStatus) -> bool {
        if !self.snapshot.enabled || !status.enabled {
            return false;
        }
        let now_persistent = status.failing_persistent;
        let kind = if now_persistent {
            bounded(status.failing_kind.clone(), MAX_SHORT_TEXT_BYTES)
        } else {
            String::new()
        };
        if self.snapshot.failing_persistent == now_persistent
            && self.snapshot.failing_kind == kind
            && self.snapshot.failing_applies == status.failing_applies
        {
            return false;
        }
        let was_persistent = self.snapshot.failing_persistent;
        self.snapshot.failing_persistent = now_persistent;
        self.snapshot.failing_kind = kind;
        self.snapshot.failing_applies = status.failing_applies;
        self.publish();
        if now_persistent && !was_persistent {
            self.snapshot.attention_revision = Some(self.snapshot.revision);
        }
        true
    }

    pub(crate) fn finish_check(
        &mut self,
        ticket: UpdaterWorkTicket,
        status: DurableUpdateStatus,
    ) -> CheckCompletion {
        self.last_transitions.clear();
        if let Some(reason) = self.completion_mismatch(ticket, UpdaterWorkKind::CheckAndStage) {
            self.snapshot.ignored_completions = self.snapshot.ignored_completions.saturating_add(1);
            self.publish();
            return CheckCompletion::Ignored(reason);
        }

        self.snapshot.outcome = bounded(status.outcome.clone(), MAX_MESSAGE_BYTES);
        self.snapshot.enabled = status.enabled;
        // The persistent-failure verdict rides every reduced check: it is what the
        // update screen shows instead of "You're up to date" while the ledger says
        // this Mac cannot update. A staged or up-to-date reduction clears it below.
        let was_persistent = self.snapshot.failing_persistent;
        self.snapshot.failing_persistent = status.enabled && status.failing_persistent;
        self.snapshot.failing_kind = if self.snapshot.failing_persistent {
            bounded(status.failing_kind.clone(), MAX_SHORT_TEXT_BYTES)
        } else {
            String::new()
        };
        self.snapshot.failing_applies = status.failing_applies;
        if !status.enabled {
            let before = self.model_state();
            self.snapshot.active = None;
            self.snapshot.phase = UpdaterPhase::Disabled;
            self.snapshot.staged = None;
            self.snapshot.install_on_clean_quit = false;
            self.snapshot.attention_revision = None;
            self.snapshot.acknowledged_attention_revision = None;
            self.snapshot.reexec_count = 0;
            self.snapshot.error = Some("updater became unavailable during check".to_string());
            self.close_preflight_ready = false;
            self.pending_preflight = None;
            self.active_apply = None;
            self.record(UpdaterTransitionAction::Disable, before);
        } else if let Some(artifact) =
            staged_from_status(self.snapshot.current_build, ticket.generation, &status)
        {
            // `check_now` combines availability, download, verify, and atomic stage in
            // one physical worker.  Preserve that fact while making its logical reducer
            // transitions explicit for inspection and Tier-1 conformance.
            let before_available = self.model_state();
            self.snapshot.active = None;
            self.snapshot.phase = UpdaterPhase::Available;
            self.record(UpdaterTransitionAction::CheckAvailable, before_available);

            let before_download = self.model_state();
            self.snapshot.phase = UpdaterPhase::Downloading;
            self.work_generation = ticket.generation;
            self.snapshot.active = Some(UpdaterWorkTicket {
                kind: UpdaterWorkKind::Download,
                ..ticket
            });
            self.record(UpdaterTransitionAction::StartDownload, before_download);

            let before_staged = self.model_state();
            self.snapshot.active = None;
            self.snapshot.phase = UpdaterPhase::Staged;
            self.snapshot.staged = Some(artifact);
            self.snapshot.error = None;
            self.snapshot.install_on_clean_quit = false;
            self.close_preflight_ready = false;
            self.record(UpdaterTransitionAction::CompleteDownload, before_staged);
        } else if status.failing_checks > 0 {
            let before = self.model_state();
            self.snapshot.active = None;
            self.snapshot.phase = UpdaterPhase::Failed;
            self.snapshot.error = Some(failure_message(&status));
            self.record(UpdaterTransitionAction::CheckFailed, before);
        } else {
            let before = self.model_state();
            self.snapshot.active = None;
            self.snapshot.phase = UpdaterPhase::Idle;
            self.snapshot.error = None;
            self.record(UpdaterTransitionAction::CheckUpToDate, before);
        }
        self.publish();
        if self.snapshot.phase == UpdaterPhase::Staged {
            self.snapshot.attention_revision = Some(self.snapshot.revision);
        }
        // A NEWLY persistent failure asks for attention like a staged build does —
        // the same badge, acknowledged the same way (opening the update screen); a
        // continuing streak does not re-badge on every check.
        if self.snapshot.failing_persistent && !was_persistent {
            self.snapshot.attention_revision = Some(self.snapshot.revision);
        }
        CheckCompletion::Reduced
    }

    /// Quiet the one announcement for an exact published update without hiding it.
    pub(crate) fn acknowledge_attention(&mut self, revision: u64) -> bool {
        self.last_transitions.clear();
        if self.snapshot.attention_revision != Some(revision)
            || self.snapshot.acknowledged_attention_revision == Some(revision)
        {
            return false;
        }
        self.snapshot.acknowledged_attention_revision = Some(revision);
        self.publish();
        true
    }

    /// Mark the current artifact for the normal clean-quit lane.  This is policy only;
    /// it never emits a re-exec command by itself.
    pub(crate) fn install_when_safe(&mut self) -> bool {
        self.last_transitions.clear();
        if !self.snapshot.enabled
            || self.snapshot.phase != UpdaterPhase::Staged
            || self.snapshot.install_on_clean_quit
        {
            return false;
        }
        let before = self.model_state();
        self.snapshot.install_on_clean_quit = true;
        self.record(UpdaterTransitionAction::InstallOnCleanQuit, before);
        self.publish();
        true
    }

    /// Retire a staged DOWNLOAD so an installed-bundle ACTIVATION can be imported in
    /// its place: once the bundle under the executable is newer than the running
    /// process, a swap-apply of any downloaded stage can no longer proceed (its
    /// rollback-source proof needs the installed bundle to still be the running
    /// build), so the activation outranks it. A no-op — `false` — unless the reducer
    /// holds a non-activation stage in `Staged` (an in-flight apply, an activation
    /// already in place, or nothing staged all keep their state).
    pub(crate) fn retire_stage_for_activation(&mut self) -> bool {
        self.last_transitions.clear();
        let Some(staged) = self.snapshot.staged.as_ref() else {
            return false;
        };
        if self.snapshot.phase != UpdaterPhase::Staged || staged.is_installed_activation() {
            return false;
        }
        let before = self.model_state();
        self.snapshot.phase = UpdaterPhase::Idle;
        self.snapshot.staged = None;
        self.snapshot.error = None;
        self.snapshot.install_on_clean_quit = false;
        self.snapshot.attention_revision = None;
        self.snapshot.acknowledged_attention_revision = None;
        self.snapshot.reexec_count = 0;
        self.close_preflight_ready = false;
        self.pending_preflight = None;
        self.active_apply = None;
        self.snapshot.outcome =
            "A newer build is already installed at this bundle's path; activating it".to_string();
        self.record(UpdaterTransitionAction::RetireStage, before);
        self.publish();
        true
    }

    /// Bind a quit-readiness request to the exact current artifact generation.
    pub(crate) fn begin_apply_preflight(&mut self, mode: ApplyMode) -> ApplyPreflightStart {
        self.last_transitions.clear();
        if !self.snapshot.enabled {
            return ApplyPreflightStart::Disabled;
        }
        if mode == ApplyMode::CleanQuit && !self.snapshot.install_on_clean_quit {
            return ApplyPreflightStart::NotDeferred;
        }
        if self.snapshot.phase == UpdaterPhase::Applying {
            return ApplyPreflightStart::Applying;
        }
        let Some(staged) = self.snapshot.staged.as_ref() else {
            return ApplyPreflightStart::NotStaged;
        };
        if self.snapshot.phase != UpdaterPhase::Staged {
            return ApplyPreflightStart::NotStaged;
        }
        if let Some(ticket) = self.pending_preflight {
            return ApplyPreflightStart::Joined(ticket);
        }
        let generation = staged.generation;
        let artifact_build = staged.build;
        let Some(operation) = self.allocate_operation() else {
            return ApplyPreflightStart::GenerationExhausted;
        };
        let ticket = ApplyPreflightTicket {
            generation,
            artifact_build,
            operation,
        };
        self.pending_preflight = Some(ticket);
        self.publish();
        ApplyPreflightStart::Inspect(ticket)
    }

    /// Consume a matching close-preflight result and, only when safe, mint the one-shot
    /// command that authorizes the existing seamless apply/re-exec path.
    pub(crate) fn finish_apply_preflight(
        &mut self,
        ticket: ApplyPreflightTicket,
        result: ClosePreflight,
    ) -> ApplyDecision {
        self.last_transitions.clear();
        if self.pending_preflight != Some(ticket) {
            return ApplyDecision::Ignored;
        }
        if !self.snapshot.enabled {
            self.pending_preflight = None;
            self.close_preflight_ready = false;
            self.publish();
            return ApplyDecision::Blocked(vec![
                "Automatic updates are disabled on this build".to_string(),
            ]);
        }
        let Some(staged) = self.snapshot.staged.as_ref() else {
            self.pending_preflight = None;
            return ApplyDecision::Ignored;
        };
        if self.snapshot.phase != UpdaterPhase::Staged
            || staged.generation != ticket.generation
            || staged.build != ticket.artifact_build
            || self.snapshot.generation != ticket.generation
            || self.snapshot.reexec_count != 0
        {
            self.pending_preflight = None;
            return ApplyDecision::Ignored;
        }

        match result {
            ClosePreflight::Blocked(reasons) => {
                self.pending_preflight = None;
                let reasons = reasons
                    .into_iter()
                    .map(|reason| bounded(reason, MAX_MESSAGE_BYTES))
                    .collect::<Vec<_>>();
                self.publish();
                ApplyDecision::Blocked(reasons)
            }
            ClosePreflight::Ready => {
                let Some(commit) = staged.commit.clone() else {
                    self.pending_preflight = None;
                    self.close_preflight_ready = false;
                    self.publish();
                    return ApplyDecision::Blocked(vec![
                        "Verified update is missing sealed source provenance".to_string(),
                    ]);
                };
                let command = ReexecCommand {
                    build: staged.build,
                    version: staged.version.clone(),
                    commit,
                    dmg_sha256: staged.dmg_sha256.clone(),
                    generation: staged.generation,
                    operation: ticket.operation,
                };
                let before_ready = self.model_state();
                self.close_preflight_ready = true;
                self.record(UpdaterTransitionAction::MarkCloseReady, before_ready);

                let before_apply = self.model_state();
                self.pending_preflight = None;
                self.snapshot.phase = UpdaterPhase::Applying;
                self.snapshot.reexec_count = 1;
                self.active_apply = Some(command.attempt());
                self.record(UpdaterTransitionAction::Apply, before_apply);
                self.publish();
                ApplyDecision::Execute(command)
            }
        }
    }

    /// Re-arm an authorized artifact when process replacement left this process alive.
    ///
    /// The attempt identity is generation-bound, so a late failure can never revive an
    /// artifact superseded by newer updater work.  `reexec_count` counts an accepted
    /// replacement, not a syscall that returned an error; reset it so the retained,
    /// verified stage remains genuinely retryable.
    pub(crate) fn abort_apply(
        &mut self,
        ticket: &ApplyAttemptTicket,
        message: impl Into<String>,
    ) -> bool {
        self.last_transitions.clear();
        if !self.apply_ticket_is_current(ticket) {
            return false;
        }

        let before = self.model_state();
        let message = bounded(message.into(), MAX_MESSAGE_BYTES);
        self.snapshot.phase = UpdaterPhase::Staged;
        self.snapshot.reexec_count = 0;
        // A physical replacement attempt that returned must never be retried by
        // the same quit gesture (or by a replayed teardown intent). The artifact
        // remains staged for an explicit fresh request, but clean-quit authority
        // is consumed by this failed attempt.
        self.snapshot.install_on_clean_quit = false;
        self.snapshot.error = Some(message.clone());
        self.snapshot.outcome = bounded(
            format!("Update remains ready; last relaunch attempt stopped safely: {message}"),
            MAX_MESSAGE_BYTES,
        );
        self.close_preflight_ready = false;
        self.pending_preflight = None;
        self.active_apply = None;
        self.record(UpdaterTransitionAction::AbortApply, before);
        self.publish();
        true
    }

    /// Reconcile a returned process-replacement attempt against authoritative disk state.
    ///
    /// A same-build durable marker is retryable and takes the ordinary `AbortApply`
    /// transition. A missing or changed marker can never revive the in-memory artifact:
    /// retire it, then let the host import any different durable stage as a new generation.
    pub(crate) fn finish_returned_apply(
        &mut self,
        ticket: &ApplyAttemptTicket,
        facts: ReturnedApplyFacts<'_>,
        message: impl Into<String>,
    ) -> ReturnedApplyDisposition {
        if !self.apply_ticket_is_current(ticket) {
            self.last_transitions.clear();
            return ReturnedApplyDisposition::Ignored;
        }
        // A returned ACTIVATION attempt: the installed bundle is the artifact. While it
        // still backs the ticket the stage is re-armed exactly like a durable download
        // (the App's bounded automatic budget decides how many more times, then the
        // manual-only latch); once the bundle has moved on, retire.
        if ticket.is_installed_activation() {
            let backed = self.snapshot.enabled
                && facts.installed.is_some_and(|installed| {
                    installed.backs_activation(
                        self.snapshot.current_build,
                        ticket.artifact_build,
                        &ticket.artifact_commit,
                    )
                });
            if backed {
                return if self.abort_apply(ticket, message) {
                    ReturnedApplyDisposition::Rearmed
                } else {
                    ReturnedApplyDisposition::Ignored
                };
            }
            self.last_transitions.clear();
            let before = self.model_state();
            let message = bounded(message.into(), MAX_MESSAGE_BYTES);
            self.snapshot.phase = UpdaterPhase::Idle;
            self.snapshot.staged = None;
            self.snapshot.reexec_count = 0;
            self.snapshot.install_on_clean_quit = false;
            self.snapshot.attention_revision = None;
            self.snapshot.acknowledged_attention_revision = None;
            self.snapshot.error = None;
            self.snapshot.outcome = bounded(
                format!("Returned activation attempt was retired because the installed bundle changed: {message}"),
                MAX_MESSAGE_BYTES,
            );
            self.close_preflight_ready = false;
            self.pending_preflight = None;
            self.active_apply = None;
            self.record(UpdaterTransitionAction::RetireApply, before);
            self.publish();
            return ReturnedApplyDisposition::Retired;
        }
        let installed = facts.installed.filter(|installed| {
            installed.build > self.snapshot.current_build
                && installed.proves_artifact(
                    ticket.artifact_build,
                    Some(&ticket.artifact_commit),
                    &ticket.artifact_dmg_sha256,
                )
        });
        let durable_exact = durable_artifact_identity_matches(
            facts.durable_staged_build,
            facts.durable_staged_commit,
            facts.durable_staged_dmg_sha256,
            ticket.artifact_build,
            Some(&ticket.artifact_commit),
            &ticket.artifact_dmg_sha256,
        );
        if installed.is_none() && self.snapshot.enabled && facts.durable_enabled && durable_exact {
            return if self.abort_apply(ticket, message) {
                ReturnedApplyDisposition::Rearmed
            } else {
                ReturnedApplyDisposition::Ignored
            };
        }

        self.last_transitions.clear();
        let before = self.model_state();
        let message = bounded(message.into(), MAX_MESSAGE_BYTES);
        self.snapshot.phase = UpdaterPhase::Idle;
        self.snapshot.staged = None;
        self.snapshot.reexec_count = 0;
        self.snapshot.install_on_clean_quit = false;
        self.snapshot.attention_revision = None;
        self.snapshot.acknowledged_attention_revision = None;
        self.snapshot.error = None;
        self.snapshot.outcome = if let Some(installed) = installed {
            bounded(
                format!(
                    "Update build {} is installed and will activate on relaunch",
                    installed.build
                ),
                MAX_MESSAGE_BYTES,
            )
        } else {
            bounded(
                format!(
                    "Returned update attempt was retired because its durable stage changed: {message}"
                ),
                MAX_MESSAGE_BYTES,
            )
        };
        self.close_preflight_ready = false;
        self.pending_preflight = None;
        self.active_apply = None;
        self.record(UpdaterTransitionAction::RetireApply, before);
        self.publish();
        installed.map_or(ReturnedApplyDisposition::Retired, |installed| {
            ReturnedApplyDisposition::InstalledNeedsRelaunch {
                build: installed.build,
            }
        })
    }

    /// Retire a non-applying in-memory stage when another process changed or consumed
    /// its authoritative durable marker. A missing marker can mean the exact build is
    /// already installed; either way, the stale stage and arrow must disappear.
    pub(crate) fn reconcile_durable_stage(
        &mut self,
        durable_enabled: bool,
        durable_staged_build: Option<u64>,
        durable_staged_commit: Option<&str>,
        durable_staged_dmg_sha256: Option<&str>,
        installed: Option<&InstalledUpdate>,
    ) -> DurableStageDisposition {
        self.last_transitions.clear();
        let Some(staged) = self.snapshot.staged.as_ref() else {
            return DurableStageDisposition::Unchanged;
        };
        if self.snapshot.phase != UpdaterPhase::Staged {
            return DurableStageDisposition::Unchanged;
        }

        // AN ACTIVATION STAGE IS BACKED BY THE INSTALLED BUNDLE, not by `ready.toml`:
        // it stays exactly as long as the bundle under the executable is still that
        // build and still newer than the running process. When the bundle changes
        // again (a newer one lands, or it is rolled back under us) the stage retires
        // and the next observation imports whatever is there now.
        if staged.is_installed_activation() {
            let backed = self.snapshot.enabled
                && installed.is_some_and(|installed| {
                    staged.commit.as_deref().is_some_and(|commit| {
                        installed.backs_activation(self.snapshot.current_build, staged.build, commit)
                    })
                });
            if backed {
                return DurableStageDisposition::Unchanged;
            }
            let before = self.model_state();
            self.snapshot.phase = UpdaterPhase::Idle;
            self.snapshot.staged = None;
            self.snapshot.error = None;
            self.snapshot.install_on_clean_quit = false;
            self.snapshot.attention_revision = None;
            self.snapshot.acknowledged_attention_revision = None;
            self.snapshot.reexec_count = 0;
            self.close_preflight_ready = false;
            self.pending_preflight = None;
            self.active_apply = None;
            self.snapshot.outcome =
                "The installed bundle changed under this process; re-observing".to_string();
            self.record(UpdaterTransitionAction::RetireStage, before);
            self.publish();
            return DurableStageDisposition::Retired;
        }

        let installed = installed.filter(|installed| {
            installed.build > self.snapshot.current_build
                && installed.proves_artifact(
                    staged.build,
                    staged.commit.as_deref(),
                    &staged.dmg_sha256,
                )
        });
        let durable_exact = durable_artifact_identity_matches(
            durable_staged_build,
            durable_staged_commit,
            durable_staged_dmg_sha256,
            staged.build,
            staged.commit.as_deref(),
            &staged.dmg_sha256,
        );
        if self.snapshot.enabled && durable_enabled && installed.is_none() && durable_exact {
            return DurableStageDisposition::Unchanged;
        }
        let before = self.model_state();
        self.snapshot.phase = UpdaterPhase::Idle;
        self.snapshot.staged = None;
        self.snapshot.error = None;
        self.snapshot.install_on_clean_quit = false;
        self.snapshot.attention_revision = None;
        self.snapshot.acknowledged_attention_revision = None;
        self.snapshot.reexec_count = 0;
        self.close_preflight_ready = false;
        self.pending_preflight = None;
        self.active_apply = None;
        self.snapshot.outcome = installed.map_or_else(
            || "Previously staged update is no longer available on disk".to_string(),
            |installed| {
                format!(
                    "Update build {} is installed and will activate on relaunch",
                    installed.build
                )
            },
        );
        self.record(UpdaterTransitionAction::RetireStage, before);
        self.publish();
        installed.map_or(DurableStageDisposition::Retired, |installed| {
            DurableStageDisposition::InstalledNeedsRelaunch {
                build: installed.build,
            }
        })
    }

    fn apply_ticket_is_current(&self, ticket: &ApplyAttemptTicket) -> bool {
        let matches_stage = self.snapshot.staged.as_ref().is_some_and(|staged| {
            staged.generation == ticket.generation && staged.build == ticket.artifact_build
        });
        self.snapshot.phase == UpdaterPhase::Applying
            && self.snapshot.generation == ticket.generation
            && matches_stage
            && self.snapshot.reexec_count == 1
            && self.active_apply.as_ref() == Some(ticket)
    }

    fn completion_mismatch(
        &self,
        ticket: UpdaterWorkTicket,
        expected_kind: UpdaterWorkKind,
    ) -> Option<IgnoredCompletion> {
        if ticket.kind != expected_kind {
            return Some(IgnoredCompletion::WrongKind);
        }
        let Some(active) = self.snapshot.active else {
            return Some(if ticket.generation < self.snapshot.generation {
                IgnoredCompletion::StaleGeneration
            } else {
                IgnoredCompletion::NoActiveWork
            });
        };
        if ticket.generation != active.generation {
            return Some(IgnoredCompletion::StaleGeneration);
        }
        if ticket.operation != active.operation {
            return Some(IgnoredCompletion::WrongOperation);
        }
        (active.kind != expected_kind).then_some(IgnoredCompletion::WrongKind)
    }

    fn allocate_operation(&mut self) -> Option<u64> {
        let operation = self.next_operation;
        self.next_operation = self.next_operation.checked_add(1)?;
        Some(operation)
    }

    fn record(&mut self, action: UpdaterTransitionAction, before: UpdaterModelState) {
        self.last_transitions.push(UpdaterTransition {
            action,
            before,
            after: self.model_state(),
        });
    }

    fn publish(&mut self) {
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
    }
}

/// A commit string that can serve as a stage identity: 7..=40 hex digits (a short
/// or full sha). Shared by the durable-status import and the activation import so
/// the two can never disagree about whether an installed bundle is stageable.
#[must_use]
pub(crate) fn usable_commit_identity(commit: &str) -> bool {
    let commit = commit.trim();
    (7..=40).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn staged_from_status(
    running_build: u64,
    generation: u64,
    status: &DurableUpdateStatus,
) -> Option<StagedUpdate> {
    if !status.enabled {
        return None;
    }
    let build = status.staged_build.filter(|build| *build > running_build)?;
    let dmg_sha256 = status
        .staged_dmg_sha256
        .as_deref()
        .map(str::trim)
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))?
        .to_ascii_lowercase();
    // A DOWNLOAD stage's commit is the manifest's full 40-char sha and nothing
    // shorter mints stage authority (`missing_or_unsealed_commit_never_mints_…`).
    // An ACTIVATION carries the installed bundle's sealed `ATermGitCommit`, which
    // the build writes as the 12-char short sha (`ATermGitCommit = 248091d23ab0`
    // on v0.27.0), and its digest is `installed_activation_digest(build, commit)`
    // — self-certifying: the digest binds exactly this commit. Demanding 40 for
    // it rejected every activation import in the field: the reducer logged
    // "activating it in place", staged nothing, and the control apply answered
    // "No newer verified update is staged" — the parking the lane exists to end.
    // So: 40 hex for a download; 7..=40 hex when the digest IS the activation
    // digest over that very commit. Every downstream comparison is prefix-
    // tolerant at >= 7 (`commit_matches`, `seamless::normalize_commit`), and an
    // activation can only ever re-exec the bundle already installed and verified
    // under this executable.
    let commit = status
        .staged_commit
        .as_deref()
        .map(str::trim)
        .filter(|commit| usable_commit_identity(commit))?
        .to_ascii_lowercase();
    if commit.len() != 40 && installed_activation_digest(build, &commit) != dmg_sha256 {
        return None;
    }
    let version = status
        .staged_version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("build {build}"));
    Some(StagedUpdate {
        build,
        version: bounded(version, MAX_SHORT_TEXT_BYTES),
        commit: Some(bounded(commit, MAX_SHORT_TEXT_BYTES)),
        dmg_sha256,
        changelog: status
            .changelog
            .clone()
            .map(|changelog| bounded(changelog, MAX_CHANGELOG_BYTES)),
        generation,
    })
}

fn failure_message(status: &DurableUpdateStatus) -> String {
    if status.outcome.trim().is_empty() {
        format!("{} update check(s) failed", status.failing_checks)
    } else {
        bounded(status.outcome.clone(), MAX_MESSAGE_BYTES)
    }
}

fn bounded(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.truncate(end);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_DIGEST: &str = "abababababababababababababababababababababababababababababababab";
    const TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn status(staged_build: Option<u64>) -> DurableUpdateStatus {
        DurableUpdateStatus {
            enabled: true,
            current_build: 10,
            staged_build,
            staged_version: staged_build.map(|build| format!("1.0.{build}")),
            staged_commit: Some(TEST_COMMIT.to_string()),
            staged_dmg_sha256: staged_build.map(|_| TEST_DIGEST.to_string()),
            changelog: Some("# Better\n\nFast and safe.".to_string()),
            outcome: if staged_build.is_some() {
                "staged".to_string()
            } else {
                "up to date".to_string()
            },
            failing_checks: 0,
            failing_persistent: false,
            failing_kind: String::new(),
            failing_applies: 0,
        }
    }

    fn installed(
        build: u64,
        commit: &str,
        receipt_build: Option<u64>,
        receipt_digest: Option<&str>,
    ) -> InstalledUpdate {
        InstalledUpdate {
            build,
            commit: commit.to_string(),
            version: None,
            receipt_build,
            receipt_dmg_sha256: receipt_digest.map(str::to_string),
        }
    }

    fn exact_installed(build: u64) -> InstalledUpdate {
        installed(
            build,
            "0123456789abcdef0123456789abcdef01234567",
            Some(build),
            Some(TEST_DIGEST),
        )
    }

    fn started(service: &mut NativeUpdaterService) -> UpdaterWorkTicket {
        match service.request_check() {
            CheckStart::Start(ticket) => ticket,
            other => panic!("expected a new check, got {other:?}"),
        }
    }

    fn stage(service: &mut NativeUpdaterService, build: u64) -> UpdaterWorkTicket {
        let ticket = started(service);
        assert_eq!(
            service.finish_check(ticket, status(Some(build))),
            CheckCompletion::Reduced
        );
        ticket
    }

    #[test]
    fn one_physical_check_is_joined_and_replayed_completion_is_inert() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        let first = started(&mut service);
        assert_eq!(service.request_check(), CheckStart::Joined(first));
        assert_eq!(service.snapshot().generation, 1);

        assert_eq!(
            service.finish_check(first, status(None)),
            CheckCompletion::Reduced
        );
        let second = started(&mut service);
        assert_eq!(second.generation, 2);
        let revision_before_stale = service.snapshot().revision;
        assert_eq!(
            service.finish_check(first, status(Some(11))),
            CheckCompletion::Ignored(IgnoredCompletion::StaleGeneration)
        );
        assert_eq!(
            service.snapshot().revision,
            revision_before_stale + 1,
            "one ignored-completion counter publication, never a duplicate revision"
        );
        assert_eq!(service.snapshot().phase, UpdaterPhase::Checking);
        assert!(service.snapshot().staged.is_none());
        assert_eq!(service.snapshot().active, Some(second));
    }

    #[test]
    fn combined_updater_api_has_explicit_verified_stage_microtrace() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        let ticket = stage(&mut service, 11);
        let actions = service
            .last_transitions()
            .iter()
            .map(|transition| transition.action)
            .collect::<Vec<_>>();
        assert_eq!(
            actions,
            [
                UpdaterTransitionAction::CheckAvailable,
                UpdaterTransitionAction::StartDownload,
                UpdaterTransitionAction::CompleteDownload,
            ]
        );
        assert_eq!(
            actions
                .iter()
                .filter_map(|action| action.model_action())
                .collect::<Vec<_>>(),
            ["CheckAvailable", "StartDownload", "CompleteDownload"]
        );
        let snapshot = service.snapshot();
        assert_eq!(snapshot.phase, UpdaterPhase::Staged);
        assert_eq!(
            snapshot.staged.as_ref().map(|staged| staged.build),
            Some(11)
        );
        assert_eq!(
            snapshot.staged.as_ref().map(|staged| staged.generation),
            Some(ticket.generation)
        );
        assert!(!snapshot.has_determinate_progress());
        assert!(snapshot.attention_pending());
    }

    #[test]
    fn stale_result_cannot_replace_current_generation_artifact() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        let old = started(&mut service);
        let mut failed = status(None);
        failed.failing_checks = 1;
        failed.outcome = "offline".to_string();
        assert_eq!(service.finish_check(old, failed), CheckCompletion::Reduced);
        let current = started(&mut service);
        assert_eq!(
            service.finish_check(old, status(Some(99))),
            CheckCompletion::Ignored(IgnoredCompletion::StaleGeneration)
        );
        assert!(service.snapshot().staged.is_none());
        assert_eq!(
            service.finish_check(current, status(Some(12))),
            CheckCompletion::Reduced
        );
        assert_eq!(
            service
                .snapshot()
                .staged
                .as_ref()
                .map(|staged| staged.build),
            Some(12)
        );
    }

    #[test]
    fn apply_is_safe_current_and_at_most_once() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        assert!(service.install_when_safe());

        let blocked_ticket = match service.begin_apply_preflight(ApplyMode::CleanQuit) {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("expected preflight, got {other:?}"),
        };
        assert!(matches!(
            service.finish_apply_preflight(
                blocked_ticket,
                ClosePreflight::Blocked(vec!["dirty editor".to_string()])
            ),
            ApplyDecision::Blocked(reasons) if reasons == ["dirty editor"]
        ));
        assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
        assert_eq!(service.snapshot().reexec_count, 0);

        let ready_ticket = match service.begin_apply_preflight(ApplyMode::CleanQuit) {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("expected second preflight, got {other:?}"),
        };
        let command = match service.finish_apply_preflight(ready_ticket, ClosePreflight::Ready) {
            ApplyDecision::Execute(command) => command,
            other => panic!("expected execute decision, got {other:?}"),
        };
        assert_eq!(command.build, 11);
        assert_eq!(service.snapshot().phase, UpdaterPhase::Applying);
        assert_eq!(service.snapshot().reexec_count, 1);
        assert!(matches!(
            service.finish_apply_preflight(ready_ticket, ClosePreflight::Ready),
            ApplyDecision::Ignored
        ));
        assert_eq!(
            service.begin_apply_preflight(ApplyMode::Immediate),
            ApplyPreflightStart::Applying
        );

        let mut calls = 0;
        command.execute(|| calls += 1);
        assert_eq!(calls, 1);
    }

    #[test]
    fn failed_reexec_rearms_only_the_exact_verified_stage() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        let preflight = match service.begin_apply_preflight(ApplyMode::Immediate) {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("expected preflight, got {other:?}"),
        };
        let command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
            ApplyDecision::Execute(command) => command,
            other => panic!("expected execute decision, got {other:?}"),
        };
        let attempt = command.attempt();
        assert!(service.abort_apply(&attempt, "exec returned EIO"));
        assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
        assert_eq!(service.snapshot().reexec_count, 0);
        assert_eq!(
            service.snapshot().staged.as_ref().map(|stage| stage.build),
            Some(11)
        );
        assert!(
            service
                .snapshot()
                .error
                .as_deref()
                .is_some_and(|error| error.contains("EIO"))
        );

        let retry = match service.begin_apply_preflight(ApplyMode::Immediate) {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("failed attempt must stay retryable, got {other:?}"),
        };
        let retry_command = match service.finish_apply_preflight(retry, ClosePreflight::Ready) {
            ApplyDecision::Execute(command) => command,
            other => panic!("expected retry command, got {other:?}"),
        };
        assert_ne!(retry_command.attempt(), attempt);
        assert_eq!(
            service.finish_returned_apply(
                &attempt,
                ReturnedApplyFacts::new(
                    true,
                    Some(11),
                    Some(TEST_COMMIT),
                    Some(TEST_DIGEST),
                    None,
                ),
                "replayed failure from first attempt"
            ),
            ReturnedApplyDisposition::Ignored,
            "an async completion carries its exact ticket, never current-by-lookup authority"
        );
        assert_eq!(service.snapshot().phase, UpdaterPhase::Applying);
    }

    // ── the ACTIVATION lane (an installed bundle newer than the process) ──────

    /// A durable status shaped the way the App imports an installed-bundle
    /// activation: the sealed identity under `installed_activation_digest`.
    fn activation_status(build: u64) -> DurableUpdateStatus {
        let mut status = status(Some(build));
        status.staged_dmg_sha256 = Some(installed_activation_digest(build, TEST_COMMIT));
        status
    }

    fn stage_activation(service: &mut NativeUpdaterService, build: u64) -> UpdaterWorkTicket {
        let ticket = started(service);
        assert_eq!(
            service.finish_check(ticket, activation_status(build)),
            CheckCompletion::Reduced
        );
        assert!(
            service.snapshot().staged.as_ref().is_some_and(StagedUpdate::is_installed_activation),
            "the imported stage carries the activation identity"
        );
        ticket
    }

    /// The activation identity is a well-formed 64-hex artifact digest, a pure
    /// function of (build, commit), and can never be mistaken for a download.
    /// The field bug behind v0.26.0's dead activation lane: an installed bundle
    /// seals a 12-char short sha and the import demanded 40, so "activating it in
    /// place" was logged and nothing was staged. Both the durable import and the
    /// activation constructor now share one identity rule.
    #[test]
    fn a_short_sha_is_a_usable_stage_identity_and_a_stub_is_not() {
        assert!(usable_commit_identity("248091d23ab0"));
        assert!(usable_commit_identity(" 248091D23AB0 "));
        assert!(usable_commit_identity(&"a".repeat(40)));
        assert!(!usable_commit_identity("248091"), "six hex is not an identity");
        assert!(!usable_commit_identity(&"a".repeat(41)));
        assert!(!usable_commit_identity("unknown"));
        assert!(!usable_commit_identity("248091d23ab0-dirty"));
        // A DOWNLOAD marker (a real DMG digest) with a short sha still mints nothing:
        // the short-sha allowance is bound to the activation digest and only it.
        let mut download = DurableUpdateStatus {
            enabled: true,
            current_build: 11,
            staged_build: Some(12),
            staged_version: Some("0.27.0".to_string()),
            staged_commit: Some("248091d23ab0".to_string()),
            staged_dmg_sha256: Some("ab".repeat(32)),
            changelog: None,
            outcome: String::new(),
            failing_checks: 0,
            failing_persistent: false,
            failing_kind: String::new(),
            failing_applies: 0,
        };
        assert!(staged_from_status(11, 0, &download).is_none());
        download.staged_commit = Some("0".repeat(40));
        assert!(staged_from_status(11, 0, &download).is_some());

        let installed = InstalledUpdate {
            build: 12,
            commit: "248091D23AB0".to_string(),
            version: Some("0.27.0".to_string()),
            receipt_build: None,
            receipt_dmg_sha256: None,
        };
        let stage = installed
            .activation_stage(11, 0)
            .expect("a short-sha bundle newer than the process is an activation");
        assert_eq!(stage.commit.as_deref(), Some("248091d23ab0"), "lowercased");
        assert!(stage.is_installed_activation());
        // …and the durable import accepts exactly what the activation constructed.
        let status = DurableUpdateStatus {
            enabled: true,
            current_build: 11,
            staged_build: Some(12),
            staged_version: Some("0.27.0".to_string()),
            staged_commit: stage.commit.clone(),
            staged_dmg_sha256: Some(stage.dmg_sha256.clone()),
            changelog: None,
            outcome: String::new(),
            failing_checks: 0,
            failing_persistent: false,
            failing_kind: String::new(),
            failing_applies: 0,
        };
        let imported = staged_from_status(11, 0, &status).expect("the import must accept it");
        assert_eq!(imported.commit, stage.commit);
        assert_eq!(imported.dmg_sha256, stage.dmg_sha256);
        // A bundle sealing an unusable commit is not an activation at all — it must
        // never retire a download it cannot replace.
        let stub = InstalledUpdate {
            commit: "unknown".to_string(),
            ..installed
        };
        assert!(stub.activation_stage(11, 0).is_none());
    }

    /// The ledger's persistent verdict reaches the snapshot while a stage is HELD
    /// (an apply-class streak builds then, and `finish_check` cannot run): once, with
    /// the attention badge on the false→true edge, idempotent on repeats (no revision
    /// churn), and cleared when the ledger heals.
    #[test]
    fn absorbing_a_persistent_failure_while_staged_badges_once_and_is_idempotent() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        let rev0 = service.snapshot().revision;
        let mut failing = status(Some(11));
        failing.failing_persistent = true;
        failing.failing_kind = "apply".to_string();
        assert!(service.absorb_failure_state(&failing));
        let snap = service.snapshot();
        assert!(snap.failing_persistent && snap.failing_kind == "apply");
        assert!(snap.staged.is_some(), "the stage is untouched");
        assert_eq!(snap.attention_revision, Some(snap.revision), "badged on the edge");
        assert!(snap.revision > rev0);
        let rev1 = snap.revision;
        assert!(!service.absorb_failure_state(&failing), "a repeat changes nothing");
        assert_eq!(service.snapshot().revision, rev1, "no revision churn per cycle");
        let healed = status(Some(11));
        assert!(service.absorb_failure_state(&healed));
        assert!(!service.snapshot().failing_persistent);
    }

    #[test]
    fn the_activation_identity_is_well_formed_pure_and_distinct() {
        let a = installed_activation_digest(11, TEST_COMMIT);
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(a, installed_activation_digest(11, TEST_COMMIT), "pure");
        assert_eq!(
            a,
            installed_activation_digest(11, &TEST_COMMIT.to_ascii_uppercase()),
            "commit case does not fork the identity"
        );
        assert_ne!(a, installed_activation_digest(12, TEST_COMMIT), "per build");
        assert_ne!(a, TEST_DIGEST, "not a download digest");
        let ticket = ApplyAttemptTicket::for_test(11, TEST_COMMIT, &a);
        assert!(ticket.is_installed_activation());
        assert!(!ApplyAttemptTicket::for_test(11, TEST_COMMIT, TEST_DIGEST).is_installed_activation());
    }

    /// An activation stage is BACKED BY THE INSTALLED BUNDLE, not by `ready.toml`:
    /// with no durable marker at all it is Unchanged while the bundle under the
    /// executable is still that build and still newer; the moment the bundle is no
    /// longer newer (or is a different build) it retires.
    #[test]
    fn an_activation_stage_is_backed_by_the_installed_bundle_not_the_ready_marker() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage_activation(&mut service, 11);
        let backing = installed(11, TEST_COMMIT, None, None);
        assert_eq!(
            service.reconcile_durable_stage(true, None, None, None, Some(&backing)),
            DurableStageDisposition::Unchanged,
            "no durable marker is needed; the bundle is the backing"
        );
        assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
        // A different (also newer) build under the executable: this activation is
        // stale — retire, and let the next observation import the real one.
        let moved_on = installed(12, TEST_COMMIT, None, None);
        assert_eq!(
            service.reconcile_durable_stage(true, None, None, None, Some(&moved_on)),
            DurableStageDisposition::Retired
        );
        assert!(service.snapshot().staged.is_none());
        assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
        assert_eq!(
            service.last_transitions()[0].action,
            UpdaterTransitionAction::RetireStage
        );
        // Rolled back under us (no longer newer): retire too.
        stage_activation(&mut service, 11);
        let rolled_back = installed(10, TEST_COMMIT, None, None);
        assert_eq!(
            service.reconcile_durable_stage(true, None, None, None, Some(&rolled_back)),
            DurableStageDisposition::Retired
        );
        // And with no installed observation at all: nothing backs it — retire.
        stage_activation(&mut service, 11);
        assert_eq!(
            service.reconcile_durable_stage(true, None, None, None, None),
            DurableStageDisposition::Retired
        );
    }

    /// A RETURNED activation attempt (the handoff came back without committing)
    /// re-arms while the bundle still backs it — the App's bounded automatic budget
    /// and manual-only latch decide how many more times — and retires once the
    /// bundle has moved on. Never a "relaunch once" dead end, never a loop.
    #[test]
    fn a_returned_activation_rearms_while_backed_and_retires_when_the_bundle_moves_on() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage_activation(&mut service, 11);
        let preflight = match service.begin_apply_preflight(ApplyMode::Immediate) {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("expected preflight, got {other:?}"),
        };
        let command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
            ApplyDecision::Execute(command) => command,
            other => panic!("expected execute decision, got {other:?}"),
        };
        let attempt = command.attempt();
        assert!(attempt.is_installed_activation());
        let backing = installed(11, TEST_COMMIT, None, None);
        assert_eq!(
            service.finish_returned_apply(
                &attempt,
                ReturnedApplyFacts::new(true, None, None, None, Some(&backing)),
                "overlap handoff failed safely: handoff proof ended TimedOut"
            ),
            ReturnedApplyDisposition::Rearmed,
            "the bundle still backs the activation: re-armed for the budget to decide"
        );
        assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
        assert!(service.snapshot().staged.as_ref().is_some_and(StagedUpdate::is_installed_activation));

        // Second attempt, but by the time it returns the bundle moved on: retire.
        let preflight = match service.begin_apply_preflight(ApplyMode::Immediate) {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("expected preflight, got {other:?}"),
        };
        let command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
            ApplyDecision::Execute(command) => command,
            other => panic!("expected execute decision, got {other:?}"),
        };
        let attempt = command.attempt();
        let moved_on = installed(12, TEST_COMMIT, None, None);
        assert_eq!(
            service.finish_returned_apply(
                &attempt,
                ReturnedApplyFacts::new(true, None, None, None, Some(&moved_on)),
                "overlap handoff failed safely: handoff proof ended ChildDied"
            ),
            ReturnedApplyDisposition::Retired
        );
        assert!(service.snapshot().staged.is_none());
        assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
    }

    /// A staged DOWNLOAD retires in favour of an activation (the App calls this when
    /// the bundle under the executable turns out newer than the process); an
    /// activation already in place, an in-flight apply, and an empty reducer are
    /// all left alone.
    #[test]
    fn a_download_stage_retires_for_activation_but_nothing_else_does() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        assert!(!service.retire_stage_for_activation(), "nothing staged: no-op");
        stage(&mut service, 11);
        assert!(service.retire_stage_for_activation(), "a download stage retires");
        assert!(service.snapshot().staged.is_none());
        assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
        stage_activation(&mut service, 12);
        assert!(!service.retire_stage_for_activation(), "an activation stays");
        assert!(service.snapshot().staged.is_some());
        let preflight = match service.begin_apply_preflight(ApplyMode::Immediate) {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("expected preflight, got {other:?}"),
        };
        let _command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
            ApplyDecision::Execute(command) => command,
            other => panic!("expected execute decision, got {other:?}"),
        };
        assert!(!service.retire_stage_for_activation(), "an in-flight apply is untouchable");
        assert_eq!(service.snapshot().phase, UpdaterPhase::Applying);
    }

    #[test]
    fn consumed_durable_stage_retires_memory_and_reports_installed_relaunch() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        let preflight = match service.begin_apply_preflight(ApplyMode::Immediate) {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("expected preflight, got {other:?}"),
        };
        let command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
            ApplyDecision::Execute(command) => command,
            other => panic!("expected execute decision, got {other:?}"),
        };
        let attempt = command.attempt();

        let installed = exact_installed(11);
        assert_eq!(
            service.finish_returned_apply(
                &attempt,
                ReturnedApplyFacts::new(true, None, None, None, Some(&installed)),
                "child failed readiness after bundle swap"
            ),
            ReturnedApplyDisposition::InstalledNeedsRelaunch { build: 11 }
        );
        assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
        assert!(service.snapshot().staged.is_none());
        assert_eq!(service.snapshot().reexec_count, 0);
        assert!(service.snapshot().outcome.contains("installed"));
        assert_eq!(
            service.last_transitions()[0].action,
            UpdaterTransitionAction::RetireApply
        );
        assert!(!service.abort_apply(&attempt, "stale retry callback"));
    }

    #[test]
    fn returned_apply_rearms_only_while_exact_durable_stage_survives() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        let preflight = match service.begin_apply_preflight(ApplyMode::Immediate) {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("expected preflight, got {other:?}"),
        };
        let command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
            ApplyDecision::Execute(command) => command,
            other => panic!("expected execute decision, got {other:?}"),
        };
        assert_eq!(
            service.finish_returned_apply(
                &command.attempt(),
                ReturnedApplyFacts::new(
                    true,
                    Some(11),
                    Some(TEST_COMMIT),
                    Some(TEST_DIGEST),
                    None,
                ),
                "exec returned EBUSY"
            ),
            ReturnedApplyDisposition::Rearmed
        );
        assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
        assert_eq!(service.snapshot().reexec_count, 0);
        assert_eq!(
            service.snapshot().staged.as_ref().map(|stage| stage.build),
            Some(11)
        );
    }

    #[test]
    fn durable_hex_identity_is_case_canonical_across_reconcile_and_returned_apply() {
        let upper_commit = TEST_COMMIT.to_ascii_uppercase();
        let upper_digest = TEST_DIGEST.to_ascii_uppercase();

        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        assert_eq!(
            service.reconcile_durable_stage(
                true,
                Some(11),
                Some(&upper_commit),
                Some(&upper_digest),
                None,
            ),
            DurableStageDisposition::Unchanged,
            "hex letter case is not a different staged artifact"
        );
        assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);

        let (mut service, attempt) = applying_attempt();
        assert_eq!(
            service.finish_returned_apply(
                &attempt,
                ReturnedApplyFacts::new(
                    true,
                    Some(11),
                    Some(&upper_commit),
                    Some(&upper_digest),
                    None,
                ),
                "exec returned after observing equivalent uppercase marker",
            ),
            ReturnedApplyDisposition::Rearmed,
            "a returned apply must rearm the same digest regardless of hex spelling"
        );
        assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
    }

    #[test]
    fn another_process_consuming_stage_clears_stale_arrow_and_reports_install() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        assert_eq!(
            service.reconcile_durable_stage(
                true,
                Some(11),
                Some(TEST_COMMIT),
                Some(TEST_DIGEST),
                None
            ),
            DurableStageDisposition::Unchanged
        );
        assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);

        let installed = exact_installed(11);
        assert_eq!(
            service.reconcile_durable_stage(true, None, None, None, Some(&installed)),
            DurableStageDisposition::InstalledNeedsRelaunch { build: 11 }
        );
        assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
        assert!(service.snapshot().staged.is_none());
        assert!(service.snapshot().attention_revision.is_none());
        assert_eq!(
            service.last_transitions()[0].action,
            UpdaterTransitionAction::RetireStage
        );
    }

    fn applying_attempt() -> (NativeUpdaterService, ApplyAttemptTicket) {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        let preflight = match service.begin_apply_preflight(ApplyMode::Immediate) {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("expected preflight, got {other:?}"),
        };
        let command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
            ApplyDecision::Execute(command) => command,
            other => panic!("expected execute decision, got {other:?}"),
        };
        let attempt = command.attempt();
        (service, attempt)
    }

    #[test]
    fn durable_installed_ticket_reconciliation_matrix_is_fail_closed() {
        let durable_cases = [
            (None, None, None),
            (Some(11), Some(TEST_COMMIT), Some(TEST_DIGEST)),
            (Some(11), Some("different-commit"), Some(TEST_DIGEST)),
            (
                Some(11),
                Some(TEST_COMMIT),
                Some("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"),
            ),
            (Some(12), Some("superseding-commit"), Some(TEST_DIGEST)),
        ];
        let installed_cases = [
            None,
            Some(installed(
                10,
                "0123456789abcdef",
                Some(11),
                Some(TEST_DIGEST),
            )),
            Some(exact_installed(11)),
            Some(installed(
                11,
                "fedcba9876543210fedcba9876543210fedcba98",
                Some(11),
                Some(TEST_DIGEST),
            )),
            Some(installed(
                11,
                "0123456789abcdef0123456789abcdef01234567",
                Some(11),
                Some("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"),
            )),
            Some(installed(
                12,
                "0123456789abcdef0123456789abcdef01234567",
                Some(12),
                Some(TEST_DIGEST),
            )),
        ];

        for durable_enabled in [false, true] {
            for (durable_build, durable_commit, durable_digest) in durable_cases {
                for installed in &installed_cases {
                    for stale_ticket in [false, true] {
                        let (mut service, attempt) = applying_attempt();
                        let ticket = if stale_ticket {
                            ApplyAttemptTicket {
                                operation: attempt.operation.saturating_add(1),
                                ..attempt.clone()
                            }
                        } else {
                            attempt.clone()
                        };
                        let disposition = service.finish_returned_apply(
                            &ticket,
                            ReturnedApplyFacts::new(
                                durable_enabled,
                                durable_build,
                                durable_commit,
                                durable_digest,
                                installed.as_ref(),
                            ),
                            "matrix completion",
                        );
                        let installed_wins = installed.as_ref().is_some_and(|installed| {
                            installed.proves_artifact(11, Some(TEST_COMMIT), TEST_DIGEST)
                        });
                        let durable_exact = durable_enabled
                            && durable_build == Some(11)
                            && durable_commit == Some(TEST_COMMIT)
                            && durable_digest == Some(TEST_DIGEST);
                        let expected = if stale_ticket {
                            ReturnedApplyDisposition::Ignored
                        } else if installed_wins {
                            ReturnedApplyDisposition::InstalledNeedsRelaunch { build: 11 }
                        } else if durable_exact {
                            ReturnedApplyDisposition::Rearmed
                        } else {
                            ReturnedApplyDisposition::Retired
                        };
                        assert_eq!(
                            disposition, expected,
                            "enabled={durable_enabled} durable=({durable_build:?},{durable_commit:?},{durable_digest:?}) installed={installed:?} stale={stale_ticket}"
                        );
                        if stale_ticket {
                            assert_eq!(service.snapshot().phase, UpdaterPhase::Applying);
                            assert_eq!(service.snapshot().reexec_count, 1);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn installed_bundle_outranks_a_surviving_exact_ready_marker() {
        let (mut service, attempt) = applying_attempt();
        let installed = exact_installed(11);
        assert_eq!(
            service.finish_returned_apply(
                &attempt,
                ReturnedApplyFacts::new(
                    true,
                    Some(11),
                    Some(TEST_COMMIT),
                    Some(TEST_DIGEST),
                    Some(&installed),
                ),
                "ready cleanup raced bundle swap",
            ),
            ReturnedApplyDisposition::InstalledNeedsRelaunch { build: 11 }
        );
        assert!(service.snapshot().staged.is_none());
        assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
    }

    #[test]
    fn same_build_replacement_requires_exact_commit_and_dmg_digest() {
        let (mut service, attempt) = applying_attempt();
        assert_eq!(
            service.finish_returned_apply(
                &attempt,
                ReturnedApplyFacts::new(
                    true,
                    Some(11),
                    Some("replacement-commit"),
                    Some(TEST_DIGEST),
                    None,
                ),
                "same build marker was replaced",
            ),
            ReturnedApplyDisposition::Retired
        );
        assert!(service.snapshot().staged.is_none());

        let (mut service, attempt) = applying_attempt();
        assert_eq!(
            service.finish_returned_apply(
                &attempt,
                ReturnedApplyFacts::new(
                    true,
                    Some(11),
                    Some(TEST_COMMIT),
                    Some("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"),
                    None,
                ),
                "same provenance was rebuilt into different bytes",
            ),
            ReturnedApplyDisposition::Retired
        );
        assert!(service.snapshot().staged.is_none());
    }

    #[test]
    fn disabled_status_never_imports_or_authorizes_a_leftover_stage() {
        let mut disabled = status(Some(11));
        disabled.enabled = false;
        let mut service = NativeUpdaterService::from_durable_status(10, "1.0.10", disabled);
        assert!(!service.snapshot().enabled);
        assert_eq!(service.snapshot().phase, UpdaterPhase::Disabled);
        assert!(service.snapshot().staged.is_none());
        assert!(!service.install_when_safe());
        assert_eq!(
            service.begin_apply_preflight(ApplyMode::Immediate),
            ApplyPreflightStart::Disabled
        );

        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        let check = started(&mut service);
        let mut disabled_completion = status(Some(12));
        disabled_completion.enabled = false;
        assert_eq!(
            service.finish_check(check, disabled_completion),
            CheckCompletion::Reduced
        );
        assert!(!service.snapshot().enabled);
        assert_eq!(service.snapshot().phase, UpdaterPhase::Disabled);
        assert!(service.snapshot().staged.is_none());
        assert_eq!(
            service.request_check(),
            CheckStart::Rejected(CheckBlock::Disabled)
        );
        assert_eq!(
            service.begin_apply_preflight(ApplyMode::Immediate),
            ApplyPreflightStart::Disabled
        );
    }

    #[test]
    fn missing_or_unsealed_commit_never_mints_production_stage_authority() {
        let invalid = [
            None,
            Some(String::new()),
            Some("unknown".to_string()),
            Some("0123456".to_string()),
            Some("0123456789ab".to_string()),
            Some("0".repeat(39)),
            Some("0".repeat(41)),
            Some("0".repeat(64)),
            Some(format!("{}-dirty", TEST_COMMIT)),
            Some("not-hex!".to_string()),
        ];
        for commit in invalid {
            let mut durable = status(Some(11));
            durable.staged_commit = commit.clone();
            let service = NativeUpdaterService::from_durable_status(10, "1.0.10", durable);
            assert!(
                service.snapshot().staged.is_none(),
                "invalid commit {commit:?} must not produce apply authority"
            );
        }
    }

    #[test]
    fn wrong_preflight_identity_cannot_authorize_reexec() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        let ticket = match service.begin_apply_preflight(ApplyMode::Immediate) {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("expected preflight, got {other:?}"),
        };
        let forged = ApplyPreflightTicket {
            operation: ticket.operation.saturating_add(1),
            ..ticket
        };
        assert!(matches!(
            service.finish_apply_preflight(forged, ClosePreflight::Ready),
            ApplyDecision::Ignored
        ));
        assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
        assert_eq!(service.snapshot().reexec_count, 0);
    }

    #[test]
    fn attention_acknowledges_one_revision_without_hiding_update() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        let attention = service
            .snapshot()
            .attention_revision
            .expect("stage publishes attention");
        assert!(service.acknowledge_attention(attention));
        assert!(!service.snapshot().attention_pending());
        assert!(service.snapshot().staged.is_some());
        assert!(!service.acknowledge_attention(attention));
    }

    #[test]
    fn disabled_service_never_mints_work() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", false);
        assert_eq!(
            service.request_check(),
            CheckStart::Rejected(CheckBlock::Disabled)
        );
        assert!(service.snapshot().active.is_none());
    }

    #[test]
    fn durable_strings_are_bounded_on_utf8_boundaries() {
        let mut oversized = status(Some(11));
        oversized.changelog = Some("🚀".repeat(MAX_CHANGELOG_BYTES));
        let service = NativeUpdaterService::from_durable_status(10, "1.0.10", oversized);
        let notes = service
            .snapshot()
            .staged
            .as_ref()
            .and_then(|staged| staged.changelog.as_deref())
            .expect("staged notes");
        assert!(notes.len() <= MAX_CHANGELOG_BYTES);
        assert!(notes.is_char_boundary(notes.len()));
    }
}
