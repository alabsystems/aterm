// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-owned reducer for the native Software Update route.
//!
//! The updater crate does a manual check, download, verification and staging as one blocking
//! [`aterm_update::check_now`]. This reducer keeps that operation single-flight while exposing
//! the logical `Checking -> Available -> Downloading -> Staged` transitions. The host owns
//! threads and process replacement; this type owns acceptance, generations, publication,
//! close-preflight binding and the at-most-once re-exec decision.
//!
//! A Settings view never owns this value: one lives on `App`, worker results are delivered
//! here, and the returned [`UpdaterSnapshot::revision`] is broadcast to every Settings
//! subscriber.

use std::fmt;

const MAX_SHORT_TEXT_BYTES: usize = 512;
const MAX_MESSAGE_BYTES: usize = 8 * 1024;
const MAX_CHANGELOG_BYTES: usize = 256 * 1024;

/// Whether THIS copy can replace itself, as far as anything has OBSERVED. A surface
/// compiled before the first read must claim neither answer: "Applies next launch",
/// an enabled Update to Latest Now and "can't update itself" are each a fact about
/// the copy that only a read of its bundle establishes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum UpdateInstallability {
    /// Nothing has read it yet: the launch could not tell, and no reconcile has
    /// landed.
    #[default]
    Unobserved,
    /// A read found a bundle the updater can replace.
    Installable,
    /// A read (or the launch's own path) found none: a disk image, a translocated
    /// download, a bare binary, a dev-marked build, or a platform with no updater.
    NotInstallable,
}

impl UpdateInstallability {
    #[must_use]
    pub(crate) fn observed(installable: bool) -> Self {
        if installable {
            Self::Installable
        } else {
            Self::NotInstallable
        }
    }

    /// A read found a replaceable bundle — the only state that may claim one.
    #[must_use]
    pub(crate) fn is_installable(self) -> bool {
        self == Self::Installable
    }

    /// A read found NO replaceable bundle — the only state that may say so.
    #[must_use]
    pub(crate) fn cannot_install(self) -> bool {
        self == Self::NotInstallable
    }
}

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

/// Exact identity of updater work. A completion is accepted only when this whole value equals
/// the active ticket; an operation number alone never suffices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UpdaterWorkTicket {
    generation: u64,
    operation: u64,
    kind: UpdaterWorkKind,
}

/// Field-for-field, bounded host projection of [`aterm_update::UpdateStatus`].
///
/// Built only from `aterm_update::check_now`'s value (or the durable `aterm_update::status`
/// reader). There `staged_build` comes from `ready.toml`, published only after download,
/// integrity and authenticity checks, bundle verification and atomic staging; apply
/// re-verifies the staged bundle to close the time-of-check/time-of-use gap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DurableUpdateStatus {
    /// Native host context is separate from enrollment/installation facts.
    pub(crate) linux_host: bool,
    pub(crate) linux: Option<aterm_update::LinuxUpdateStatus>,
    pub(crate) enabled: bool,
    pub(crate) current_build: u64,
    pub(crate) staged_build: Option<u64>,
    pub(crate) staged_version: Option<String>,
    pub(crate) staged_commit: Option<String>,
    pub(crate) staged_dmg_sha256: Option<String>,
    pub(crate) changelog: Option<String>,
    pub(crate) outcome: String,
    pub(crate) failing_checks: u32,
    /// The health ledger's verdict that the streak is persistent (at least the updater's
    /// `PERSISTENT_AFTER` consecutive failures of one class): "this Mac cannot update until X
    /// is fixed", not "one check blipped", carried so the app can show it.
    pub(crate) failing_persistent: bool,
    /// The most recent failing class the ledger recorded (`network` / `pipeline` / `manifest`
    /// / `stage` / `apply`), empty when not failing; not necessarily the one that escalated.
    pub(crate) failing_kind: String,
    /// Consecutive apply failures: the signal for "the staged build will not start", which
    /// `failing_kind` cannot give (an escalated apply lane whose last failure was a network
    /// blip records `network`).
    pub(crate) failing_applies: u32,
    /// Why the applies in [`Self::failing_applies`] failed: the ledger's `last_apply_error`,
    /// which `aterm ctl update status` prints as `apply_failure=`, so a surface can say what
    /// went wrong, not only how often. Read from `aterm_update::apply_lane_report` on the
    /// worker that reads the status, never on the event loop.
    pub(crate) apply_failure: String,
    /// The build [`Self::apply_failure`] was trying to reach (0 when unknown), with
    /// [`Self::apply_failures_for_target`] the consecutive attempts on that exact artifact
    /// (the ledger's `last_apply_failure_target_build` / `apply_failures_for_target`).
    /// [`Self::failing_applies`] cannot say whether this build was tried: it is bound to the
    /// running build, so a fresh stage inherits the count and reason of the one it replaced.
    pub(crate) apply_failure_build: u64,
    /// See [`Self::apply_failure_build`].
    pub(crate) apply_failures_for_target: u32,
    /// This launch has a bundle the updater could REPLACE. `Some(false)` for a run
    /// from the mounted DMG, a Gatekeeper-translocated copy, or a dev-marked install —
    /// states in which no check thread ever starts, so every ledger field is the
    /// pristine default of a machine that structurally cannot update. `None` where
    /// nothing read it: a worker that failed before its read observed no bundle.
    pub(crate) installable: Option<bool>,
    /// The stranded verdict ([`aterm_update::UpdateStatus::channel_unreadable`]): checks
    /// complete but the release channel cannot be read, with zero ledger failures, so
    /// `failing_persistent` never carries it.
    pub(crate) channel_unreadable: bool,
    /// When the last check completed (Unix seconds; `aterm_update::UpdateStatus::updated_at`),
    /// `None` before any: Settings ▸ Software Update's "Checked 12 min ago".
    pub(crate) checked_at: Option<i64>,
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
    /// Whether this stage is an installed-bundle activation rather than a download: its
    /// identity is [`installed_activation_digest`] over its build and sealed commit, and its
    /// backing is the bundle at the running executable's path, not a `ready.toml` marker.
    #[must_use]
    pub(crate) fn is_installed_activation(&self) -> bool {
        self.commit.as_deref().is_some_and(|commit| {
            installed_activation_digest(self.build, commit) == self.dmg_sha256
        })
    }
}

/// The activation identity. When the bundle at this process's path is already a newer build
/// (the release cutter writing into the bundle it launched from, a new `.app` dragged over
/// the old, a sibling aterm's swap), there is no DMG to name, but every latch, budget and
/// ticket here keys on an artifact digest. So the activation is an artifact: a 64-hex digest,
/// domain-separated over the sealed build and commit, that no real DMG digest can collide
/// with. It flows through the same stage, apply and returned machinery as a download; the
/// places that differ (reconciliation, backed by the installed bundle rather than
/// `ready.toml`, and the handoff, which verifies the bundle under the executable and binds no
/// expected artifact for the successor) ask [`StagedUpdate::is_installed_activation`] /
/// [`ApplyAttemptTicket::is_installed_activation`].
///
/// A bundle newer than the process is a staged update whose swap already happened; the
/// seamless handoff takes it over without losing a shell, where a relaunch prompt would
/// leave it inert until someone noticed.
#[must_use]
pub(crate) fn installed_activation_digest(build: u64, commit: &str) -> String {
    use aterm_digest::Sha256;
    let mut hasher = Sha256::new();
    hasher.update(b"aterm installed-bundle activation\0");
    hasher.update(build.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(commit.trim().to_ascii_lowercase().as_bytes());
    aterm_codec::hex::encode_lower(&hasher.finalize())
}

/// The outcome for a newer build installed under the executable whose version this reducer
/// does not hold: word for word the App's `INSTALLED_ACTIVATES_IN_PLACE`. It promises no
/// timing: the switch runs by itself only where a retry is scheduled, else on Install.
const INSTALLED_SWITCHES_IN_PLACE: &str =
    "The update is installed; switching to it keeps your shells running";

/// Compare one durable marker to an already-canonical artifact identity, as hex values, not
/// spellings: `staged_from_status` lowercases imported identities while the marker reader
/// keeps its source text, so a valid uppercase marker would otherwise be imported and then
/// retired.
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
    /// Launches the boot sentinel has counted against a trial of this build (0 when none is
    /// armed), read on the updater facts worker. A structural latch reads it before spending a
    /// launch: past the sentinel's threshold, the launch is the one that reverts it.
    pub(crate) trial_launches: u32,
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

    /// Whether this installed bundle backs an activation of `(build, commit)`: it is that
    /// sealed identity and strictly newer than the running process. The activation's
    /// counterpart of `proves_artifact`; the bundle is the artifact, so no receipt is involved.
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
    pub(crate) fn activation_stage(
        &self,
        running_build: u64,
        generation: u64,
    ) -> Option<StagedUpdate> {
        // The import's identity rule (`staged_from_status`): a bundle whose sealed commit is
        // unusable is no activation and must not retire a download it can never replace.
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

/// Immutable service projection that UI, tab attention, accessibility and inspection all
/// consume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdaterSnapshot {
    pub(crate) linux_host: bool,
    /// Read-only Linux disk facts, never macOS staged/apply authority.
    pub(crate) linux: Option<aterm_update::LinuxUpdateStatus>,
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
    pub(crate) attention_revision: Option<u64>,
    pub(crate) acknowledged_attention_revision: Option<u64>,
    pub(crate) ignored_completions: u64,
    pub(crate) reexec_count: u64,
    /// The ledger's persistent-failure verdict as of the last reduced check: the update screen
    /// headlines it, and the Settings tab's attention badge lights for it as for a staged
    /// build. Cleared when a check reduces to staged or up-to-date.
    pub(crate) failing_persistent: bool,
    /// The failing class behind `failing_persistent` (empty otherwise).
    pub(crate) failing_kind: String,
    /// See [`DurableUpdateStatus::failing_applies`].
    pub(crate) failing_applies: u32,
    /// See [`DurableUpdateStatus::apply_failure`].
    pub(crate) apply_failure: String,
    /// See [`DurableUpdateStatus::apply_failure_build`].
    pub(crate) apply_failure_build: u64,
    /// See [`Self::apply_failure_build`].
    pub(crate) apply_failures_for_target: u32,
    /// Whether this copy can replace itself, as far as a read has OBSERVED
    /// ([`DurableUpdateStatus::installable`]); [`UpdateInstallability::Unobserved`]
    /// until one has.
    pub(crate) installable: UpdateInstallability,
    /// See [`DurableUpdateStatus::channel_unreadable`].
    pub(crate) channel_unreadable: bool,
    /// See [`DurableUpdateStatus::checked_at`].
    pub(crate) checked_at: Option<i64>,
}

impl UpdaterSnapshot {
    #[must_use]
    pub(crate) fn attention_pending(&self) -> bool {
        self.attention_revision.is_some()
            && self.attention_revision != self.acknowledged_attention_revision
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
    /// Background policy: exact stage authority plus a short terminal-idle epoch before
    /// readers may park.
    Automatic,
    /// Background policy whose bounded idle-preference window has closed: every gate still
    /// applies and only the idle wait is dropped, so a never-quiet machine still updates. No
    /// person asked, so wording and the retry budget treat it as background work.
    AutomaticPastGrace,
    /// Explicit user/control apply: bypasses only the idle delay, never safety.
    Immediate,
}

impl ApplyMode {
    /// Whether this attempt came from background policy rather than a person; both automatic
    /// lanes share the retry budget and the non-disruptive surfacing.
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
    Applying,
    GenerationExhausted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClosePreflight {
    Ready,
    Blocked(Vec<String>),
}

/// A linear host command: neither `Clone` nor `Copy`, and executing it consumes it. The
/// reducer issues at most one for an accepted artifact.
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
    /// Test-only exact-artifact ticket for exercising the App-side completion path without
    /// the whole reducer.
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

    /// Test-only: make this ticket the current apply, so `abort_apply` admits it and the
    /// completion path itself is tested, not only its policy helpers.
    #[cfg(test)]
    pub(crate) fn make_current_apply_for_test(&self, service: &mut NativeUpdaterService) {
        service.snapshot.generation = self.generation;
        service.snapshot.phase = UpdaterPhase::Applying;
        service.snapshot.reexec_count = 1;
        // A production ticket is minted from the staged entry, so a staged build keeps its
        // version.
        let version = service
            .snapshot
            .staged
            .as_ref()
            .filter(|staged| staged.build == self.artifact_build)
            .map_or_else(|| "0.0.0".to_string(), |staged| staged.version.clone());
        service.snapshot.staged = Some(StagedUpdate {
            build: self.artifact_build,
            version,
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

    /// Whether this attempt activates the installed bundle at the executable's path (see
    /// [`installed_activation_digest`]) rather than swapping in a staged download.
    #[must_use]
    #[cfg(any(unix, test))]
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
#[cfg(any(unix, test))]
pub(crate) enum ReturnedApplyDisposition {
    /// The exact verified artifact still exists durably and was re-armed.
    Rearmed,
    /// The staged marker was consumed and this build (or a newer one) is installed at the
    /// canonical app path; the App imports it as an activation stage and applies it in place.
    InstalledNeedsRelaunch { build: u64 },
    /// The attempted artifact is no longer authoritative and was retired in memory.
    Retired,
    /// The callback did not name the currently-live apply authority.
    Ignored,
}

/// One atomic projection of the durable updater state observed after a replacement attempt
/// returned, grouped so a stage identity is never paired with another observation's receipt.
#[derive(Clone, Copy, Debug)]
#[cfg(any(unix, test))]
pub(crate) struct ReturnedApplyFacts<'a> {
    durable_enabled: bool,
    durable_staged_build: Option<u64>,
    durable_staged_commit: Option<&'a str>,
    durable_staged_dmg_sha256: Option<&'a str>,
    installed: Option<&'a InstalledUpdate>,
}

#[cfg(any(unix, test))]
impl<'a> ReturnedApplyFacts<'a> {
    #[cfg(any(unix, test))]
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

/// Scalar projection Tier-1 binds to `native_updater_model`; it excludes presentation-only
/// revision data.
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
    Apply,
    AbortApply,
    #[cfg(any(unix, test))]
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
            // Disabled-state reconciliation is modeled separately; it is not CheckFailed.
            Self::Disable => None,
            Self::StartDownload => Some("StartDownload"),
            Self::CompleteDownload => Some("CompleteDownload"),
            Self::MarkCloseReady => Some("MarkCloseReady"),
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
                linux_host: false,
                linux: None,
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
                attention_revision: None,
                acknowledged_attention_revision: None,
                ignored_completions: 0,
                reexec_count: 0,
                failing_persistent: false,
                failing_kind: String::new(),
                failing_applies: 0,
                apply_failure: String::new(),
                apply_failure_build: 0,
                apply_failures_for_target: 0,
                // No updater on this platform is an observation the build makes;
                // whether a copy that has one can replace itself is the reconcile's
                // to read ([`Self::seed_installability`] takes what the launch knows).
                installable: if enabled {
                    UpdateInstallability::Unobserved
                } else {
                    UpdateInstallability::NotInstallable
                },
                channel_unreadable: false,
                checked_at: None,
            },
            next_operation: 1,
            work_generation: 0,
            close_preflight_ready: false,
            pending_preflight: None,
            active_apply: None,
            last_transitions: Vec::new(),
        }
    }

    /// Seed process state from the updater's durable marker before any native view opens.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_durable_status(
        current_build: u64,
        current_version: impl Into<String>,
        status: DurableUpdateStatus,
    ) -> Self {
        let enabled = status.enabled;
        let mut service = Self::new(current_build, current_version, enabled);
        service.snapshot.linux_host = status.linux_host || status.linux.is_some();
        service.snapshot.linux = bounded_linux_status(status.linux.clone());
        service.snapshot.outcome = bounded(status.outcome.clone(), MAX_MESSAGE_BYTES);
        service.snapshot.checked_at = status.checked_at;
        if enabled && let Some(installable) = status.installable {
            service.snapshot.installable = UpdateInstallability::observed(installable);
        }
        if !enabled {
            // A leftover ready marker is no apply authority while updates are disabled.
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
        self.close_preflight_ready = false;
        self.pending_preflight = None;
        self.active_apply = None;
        self.record(action, before);
        self.publish();
        CheckStart::Start(ticket)
    }

    /// What the launch itself established about this copy before any read: a disk
    /// image, a translocated download, a bare binary or a dev-marked build cannot
    /// replace itself, so those never show a neutral flash. Anything else stays
    /// [`UpdateInstallability::Unobserved`] for the reconcile to read.
    #[must_use]
    pub(crate) fn seed_installability(mut self, seed: UpdateInstallability) -> Self {
        if self.snapshot.installable == UpdateInstallability::Unobserved {
            self.snapshot.installable = seed;
        }
        self
    }

    /// Record a read of this copy's installability ([`DurableUpdateStatus::
    /// installable`], or the bundle probe a reconcile makes when the ledger cannot be
    /// read). Returns whether it changed — the caller republishes on `true`.
    pub(crate) fn note_installability(&mut self, installable: bool) -> bool {
        let observed = UpdateInstallability::observed(installable);
        if !self.snapshot.enabled || self.snapshot.installable == observed {
            return false;
        }
        self.snapshot.installable = observed;
        self.publish();
        true
    }

    /// Take a newer "last check completed" time from a reconcile (background checks write
    /// only the ledger). Never moves backwards; returns whether it changed, and the caller
    /// republishes on `true`.
    pub(crate) fn note_checked_at(&mut self, checked_at: Option<i64>) -> bool {
        match checked_at {
            Some(at) if self.snapshot.checked_at.is_none_or(|held| at > held) => {
                self.snapshot.checked_at = Some(at);
                self.publish();
                true
            }
            _ => false,
        }
    }

    /// Absorb the ledger's failure verdict from a reconcile that imports no stage (the
    /// exact-match early path, or a held stage that refuses a new check): otherwise only
    /// `finish_check` writes `failing_persistent`, and an apply-class streak builds while a
    /// stage is held. Badges the false-to-true edge as `finish_check` does.
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
        // The apply reason moves with the apply count: a streak can keep its count while its
        // cause changes (`TimedOut` to `ChildDied`).
        let apply_failure = bounded(status.apply_failure.clone(), MAX_SHORT_TEXT_BYTES);
        if self.snapshot.failing_persistent == now_persistent
            && self.snapshot.failing_kind == kind
            && self.snapshot.failing_applies == status.failing_applies
            && self.snapshot.apply_failure == apply_failure
            && self.snapshot.apply_failure_build == status.apply_failure_build
            && self.snapshot.apply_failures_for_target == status.apply_failures_for_target
            && self.snapshot.channel_unreadable == status.channel_unreadable
        {
            return false;
        }
        // A reconcile must not erase a failure it is merely older than: these facts were read
        // on a worker, `note_apply_failure` writes the same fields on the main thread, and
        // nothing orders the two. A stale read and a genuine reset both look like "no
        // failure", so the discriminator is direction for one target: within a process the
        // artifact-scoped count only climbs (a success `exec`s this process away). A report
        // that lowers it for the same target is stale, and only its apply-lane fields are
        // held back; its check-lane fields are still fresh.
        let stale_apply_lane = status.apply_failure_build == self.snapshot.apply_failure_build
            && status.apply_failures_for_target < self.snapshot.apply_failures_for_target;
        let was_persistent = self.snapshot.failing_persistent;
        self.snapshot.channel_unreadable = status.channel_unreadable;
        if stale_apply_lane {
            self.publish();
            return true;
        }
        self.snapshot.failing_persistent = now_persistent;
        self.snapshot.failing_kind = kind;
        self.snapshot.failing_applies = status.failing_applies;
        self.snapshot.apply_failure = apply_failure;
        self.snapshot.apply_failure_build = status.apply_failure_build;
        self.snapshot.apply_failures_for_target = status.apply_failures_for_target;
        self.publish();
        if now_persistent && !was_persistent {
            self.snapshot.attention_revision = Some(self.snapshot.revision);
        }
        true
    }

    /// Absorb an apply failure the host just wrote, without waiting for a disk re-read: the
    /// ledger write happens on the event loop (`App::record_apply_outcome_in_ledger`), but
    /// worker observations, the only other way apply facts arrive, can be read before it.
    /// `recorded` is read back inside the lock scope that wrote it, so it agrees with the
    /// file. Returns whether anything changed; the caller republishes on `true`.
    pub(crate) fn note_apply_failure(
        &mut self,
        recorded: &aterm_update::RecordedApplyFailure,
    ) -> bool {
        if !self.snapshot.enabled {
            return false;
        }
        let reason = bounded(recorded.reason.clone(), MAX_SHORT_TEXT_BYTES);
        let kind = if recorded.persistent {
            // The class is `apply`: this write is the apply lane's own.
            "apply".to_string()
        } else {
            String::new()
        };
        if self.snapshot.failing_applies == recorded.apply_failures
            && self.snapshot.apply_failure == reason
            && self.snapshot.apply_failure_build == recorded.target_build
            && self.snapshot.apply_failures_for_target == recorded.failures_for_target
            && self.snapshot.failing_persistent == recorded.persistent
            && self.snapshot.failing_kind == kind
        {
            return false;
        }
        let was_persistent = self.snapshot.failing_persistent;
        self.snapshot.failing_applies = recorded.apply_failures;
        self.snapshot.apply_failure = reason;
        self.snapshot.apply_failure_build = recorded.target_build;
        self.snapshot.apply_failures_for_target = recorded.failures_for_target;
        self.snapshot.failing_persistent = recorded.persistent;
        self.snapshot.failing_kind = kind;
        self.publish();
        if recorded.persistent && !was_persistent {
            self.snapshot.attention_revision = Some(self.snapshot.revision);
        }
        true
    }

    /// Reduce the exact result of one `aterm_update::check_now` worker.
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
        self.snapshot.linux_host = status.linux_host || status.linux.is_some();
        self.snapshot.linux = bounded_linux_status(status.linux.clone());
        // The persistent-failure verdict rides every reduced check, so the update screen does
        // not say "You're up to date" while the ledger says this Mac cannot update.
        let was_persistent = self.snapshot.failing_persistent;
        self.snapshot.failing_persistent = status.enabled && status.failing_persistent;
        self.snapshot.failing_kind = if self.snapshot.failing_persistent {
            bounded(status.failing_kind.clone(), MAX_SHORT_TEXT_BYTES)
        } else {
            String::new()
        };
        self.snapshot.failing_applies = status.failing_applies;
        self.snapshot.apply_failure = bounded(status.apply_failure.clone(), MAX_SHORT_TEXT_BYTES);
        self.snapshot.apply_failure_build = status.apply_failure_build;
        self.snapshot.apply_failures_for_target = status.apply_failures_for_target;
        if let Some(installable) = status.installable {
            self.snapshot.installable = UpdateInstallability::observed(installable);
        }
        // Likewise the stranded verdict, which records zero ledger failures, so
        // `failing_persistent` alone can never say it.
        self.snapshot.channel_unreadable = status.enabled && status.channel_unreadable;
        if status.checked_at.is_some() {
            self.snapshot.checked_at = status.checked_at;
        }
        if !status.enabled {
            let before = self.model_state();
            self.snapshot.active = None;
            self.snapshot.phase = UpdaterPhase::Disabled;
            self.snapshot.staged = None;
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
            // `check_now` does availability, download, verify and stage in one worker; record
            // the logical transitions explicitly for inspection and Tier-1.
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
        // A newly persistent failure badges like a staged build; a continuing streak does not
        // re-badge on every check.
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

    /// Retire a staged download so an installed-bundle activation can be imported instead:
    /// once the bundle under the executable is newer than the process, a swap-apply cannot
    /// proceed (its rollback-source proof needs the installed bundle to be the running build).
    /// `false` unless a non-activation stage is held in `Staged`.
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
        self.snapshot.attention_revision = None;
        self.snapshot.acknowledged_attention_revision = None;
        self.snapshot.reexec_count = 0;
        self.close_preflight_ready = false;
        self.pending_preflight = None;
        self.active_apply = None;
        self.snapshot.outcome = INSTALLED_SWITCHES_IN_PLACE.to_string();
        self.record(UpdaterTransitionAction::RetireStage, before);
        self.publish();
        true
    }

    /// Bind a quit-readiness request to the exact current artifact generation.
    pub(crate) fn begin_apply_preflight(&mut self) -> ApplyPreflightStart {
        self.last_transitions.clear();
        if !self.snapshot.enabled {
            return ApplyPreflightStart::Disabled;
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
            return ApplyDecision::Blocked(vec!["aterm updates itself on macOS only".to_string()]);
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

    /// The outcome sentence for a build already installed at the canonical app path, which
    /// the App imports as an activation stage. Names the installed version when it was read.
    #[must_use]
    pub(crate) fn installed_activation_outcome(version: Option<&str>) -> String {
        match version.map(str::trim).filter(|version| !version.is_empty()) {
            Some(version) => format!(
                "{} is installed; switching to it keeps your shells running",
                crate::update_words::aterm_v(version)
            ),
            None => INSTALLED_SWITCHES_IN_PLACE.to_string(),
        }
    }

    /// The outcome sentence for a physical apply attempt that returned safely:
    /// the stage is still ready and the lane's budget decides the retry.
    #[must_use]
    pub(crate) fn apply_attempt_stopped_outcome(message: &str) -> String {
        format!("The update is still ready; the install stopped: {message}")
    }

    /// Re-arm an authorized artifact when process replacement left this process alive. The
    /// generation-bound ticket keeps a late failure from reviving a superseded artifact;
    /// `reexec_count` counts accepted replacements, so it resets and the stage stays retryable.
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
        self.snapshot.error = Some(message.clone());
        self.snapshot.outcome = bounded(
            Self::apply_attempt_stopped_outcome(&message),
            MAX_MESSAGE_BYTES,
        );
        self.close_preflight_ready = false;
        self.pending_preflight = None;
        self.active_apply = None;
        self.record(UpdaterTransitionAction::AbortApply, before);
        self.publish();
        true
    }

    /// Reconcile a returned process-replacement attempt against disk state: a same-build
    /// durable marker re-arms (`AbortApply`); a missing or changed one retires the stage, and
    /// the host imports any different durable stage as a new generation.
    #[cfg(any(unix, test))]
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
        // A returned activation attempt: the installed bundle is the artifact. While it still
        // backs the ticket the stage is re-armed like a durable download (the App's automatic
        // budget decides how often); once the bundle is observed to have moved on, retire. A
        // failed bundle probe (`None`) is unknown, as in `reconcile_durable_stage`. Re-arming
        // authorizes nothing: the next attempt needs its own preflight and verification.
        if ticket.is_installed_activation() {
            let backed = self.snapshot.enabled
                && facts.installed.is_none_or(|installed| {
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
            self.snapshot.attention_revision = None;
            self.snapshot.acknowledged_attention_revision = None;
            self.snapshot.error = None;
            self.snapshot.outcome = bounded(
                format!("The install stopped: {message}; aterm on disk has since changed"),
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
        self.snapshot.attention_revision = None;
        self.snapshot.acknowledged_attention_revision = None;
        self.snapshot.error = None;
        self.snapshot.outcome = if let Some(installed) = installed {
            bounded(
                Self::installed_activation_outcome(installed.version.as_deref()),
                MAX_MESSAGE_BYTES,
            )
        } else {
            bounded(
                format!("The install stopped: {message}; the download on disk has since changed"),
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

    /// Retire a non-applying in-memory stage when another process changed or consumed its
    /// durable marker (a missing marker can mean the build is already installed).
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

        // An activation stage is backed by the installed bundle, not `ready.toml`: it stays
        // while the bundle under the executable is that build and newer than the process;
        // otherwise it retires and the next observation imports what is there.
        if staged.is_installed_activation() {
            // ABSENCE OF EVIDENCE IS NOT EVIDENCE THE BUNDLE CHANGED. `installed`
            // is `None` whenever the facts worker could not PROVE the bundle this
            // pass — the installed-facts probe fails closed, and its `spctl` probe
            // rides a 30 s ceiling that a captive portal or half-open network
            // reaches routinely. Retiring on that observation threw away a
            // perfectly good activation, and nothing re-announced it: the check
            // thread's watch latches one announcement per build and the download
            // lane is silent while a stage covers the release, so the update sat
            // lost until the next relaunch (2026-08-19 audit). Keep the
            // stage; retire only when the bundle is OBSERVED and no longer backs it.
            let Some(installed) = installed else {
                return DurableStageDisposition::Unchanged;
            };
            let backed = self.snapshot.enabled
                && staged.commit.as_deref().is_some_and(|commit| {
                    installed.backs_activation(self.snapshot.current_build, staged.build, commit)
                });
            if backed {
                return DurableStageDisposition::Unchanged;
            }
            let before = self.model_state();
            self.snapshot.phase = UpdaterPhase::Idle;
            self.snapshot.staged = None;
            self.snapshot.error = None;
            self.snapshot.attention_revision = None;
            self.snapshot.acknowledged_attention_revision = None;
            self.snapshot.reexec_count = 0;
            self.close_preflight_ready = false;
            self.pending_preflight = None;
            self.active_apply = None;
            self.snapshot.outcome = "aterm on disk changed; reading it again".to_string();
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
        self.snapshot.attention_revision = None;
        self.snapshot.acknowledged_attention_revision = None;
        self.snapshot.reexec_count = 0;
        self.close_preflight_ready = false;
        self.pending_preflight = None;
        self.active_apply = None;
        self.snapshot.outcome = installed.map_or_else(
            || "Previously staged update is no longer available on disk".to_string(),
            |installed| Self::installed_activation_outcome(installed.version.as_deref()),
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

    /// Log one reducer step for Tier-1 (`native_updater_conformance`), its only reader; a
    /// shipping build records nothing.
    fn record(&mut self, action: UpdaterTransitionAction, before: UpdaterModelState) {
        if cfg!(test) {
            self.last_transitions.push(UpdaterTransition {
                action,
                before,
                after: self.model_state(),
            });
        }
    }

    fn publish(&mut self) {
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
    }
}

/// A commit string that can serve as a stage identity: 7..=40 hex digits. Shared by the
/// durable-status import and the activation import so the two never disagree.
#[must_use]
pub(crate) fn usable_commit_identity(commit: &str) -> bool {
    let commit = commit.trim();
    (7..=40).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn bounded_linux_status(
    linux: Option<aterm_update::LinuxUpdateStatus>,
) -> Option<aterm_update::LinuxUpdateStatus> {
    linux.map(|mut status| {
        status.staged_version = status
            .staged_version
            .map(|value| bounded(value, MAX_SHORT_TEXT_BYTES));
        status.staged_commit = status
            .staged_commit
            .map(|value| bounded(value, MAX_SHORT_TEXT_BYTES));
        status.trial_phase = status
            .trial_phase
            .map(|value| bounded(value, MAX_SHORT_TEXT_BYTES));
        status
    })
}

/// The build of the verified download a durable observation names, when strictly newer than
/// `running_build`: exactly what [`staged_from_status`] would accept and never an activation,
/// so a structural latch counts as a newer release only bytes the reducer would stage.
#[must_use]
pub(crate) fn verified_download_build(
    running_build: u64,
    status: &DurableUpdateStatus,
) -> Option<u64> {
    staged_from_status(running_build, 0, status)
        .filter(|stage| !stage.is_installed_activation())
        .map(|stage| stage.build)
}

fn staged_from_status(
    running_build: u64,
    generation: u64,
    status: &DurableUpdateStatus,
) -> Option<StagedUpdate> {
    if status.linux_host || status.linux.is_some() {
        return None;
    }
    if !status.enabled {
        return None;
    }
    let build = status.staged_build.filter(|build| *build > running_build)?;
    let dmg_sha256 = status
        .staged_dmg_sha256
        .as_deref()
        .map(str::trim)
        .filter(|digest| aterm_codec::hex::is_hex(digest, 64))?
        .to_ascii_lowercase();
    // A download stage's commit is the manifest's full 40-char sha; nothing shorter mints
    // stage authority. An activation carries the bundle's sealed `ATermGitCommit`, a 12-char
    // short sha, and its digest is `installed_activation_digest(build, commit)`, which binds
    // exactly that commit. So: 40 hex for a download, 7..=40 hex when the digest is the
    // activation digest over that commit. Downstream comparisons are prefix-tolerant at >= 7
    // (`commit_matches`, `seamless::normalize_commit`), and an activation can only re-exec
    // the bundle already installed and verified under this executable.
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

    /// A reconcile read before `note_apply_failure`'s write cannot lower the apply lane.
    #[test]
    fn a_reconcile_older_than_the_failure_cannot_lower_the_apply_lane() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        let _ = service.absorb_failure_state(&status(Some(11)));

        let recorded = aterm_update::RecordedApplyFailure {
            apply_failures: 1,
            failures_for_target: 1,
            target_build: 11,
            reason: "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
            persistent: false,
        };
        assert!(
            service.note_apply_failure(&recorded),
            "precondition: the failure is recorded and published"
        );
        assert_eq!(service.snapshot().apply_failures_for_target, 1);

        // The stale reconcile: same target, a count read before the write.
        let mut stale = status(Some(11));
        stale.apply_failure_build = 11;
        stale.apply_failures_for_target = 0;
        stale.failing_applies = 0;
        stale.apply_failure = String::new();
        service.absorb_failure_state(&stale);

        assert_eq!(
            service.snapshot().apply_failures_for_target,
            1,
            "a reconcile older than the failure lowered the artifact-scoped count"
        );
        assert!(
            !service.snapshot().apply_failure.is_empty(),
            "a reconcile older than the failure erased the reason the surfaces render"
        );
    }

    /// The last-check time follows every reduced check and every reconcile (background
    /// checks write only the ledger), forward only.
    #[test]
    fn the_last_check_time_follows_checks_and_reconciles_forward_only() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        assert_eq!(service.snapshot().checked_at, None);
        let CheckStart::Start(ticket) = service.request_check() else {
            panic!("an idle service starts a check");
        };
        let mut checked = status(None);
        checked.checked_at = Some(1_000);
        service.finish_check(ticket, checked);
        assert_eq!(service.snapshot().checked_at, Some(1_000));
        assert!(service.note_checked_at(Some(2_000)));
        assert_eq!(service.snapshot().checked_at, Some(2_000));
        assert!(
            !service.note_checked_at(Some(1_500)),
            "an older read is not news"
        );
        assert!(!service.note_checked_at(None));
        assert_eq!(service.snapshot().checked_at, Some(2_000));
    }

    fn status(staged_build: Option<u64>) -> DurableUpdateStatus {
        DurableUpdateStatus {
            linux_host: false,
            linux: None,
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
            apply_failure: String::new(),
            apply_failure_build: 0,
            apply_failures_for_target: 0,
            installable: Some(true),
            channel_unreadable: false,
            checked_at: None,
        }
    }

    #[test]
    fn linux_update_status_survives_reduction_without_macos_apply_authority() {
        let mut durable = status(Some(12));
        durable.linux_host = true;
        durable.linux = Some(aterm_update::LinuxUpdateStatus {
            installed_build: 10,
            installed_version: String::new(),
            staged_build: Some(12),
            staged_version: Some("1.0.12".into()),
            staged_commit: Some(TEST_COMMIT.into()),
            trial_phase: None,
            trial_starts: 0,
            trial_healthy: false,
            refused_newer: false,
        });
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        let CheckStart::Start(ticket) = service.request_check() else {
            panic!("check admitted")
        };
        assert_eq!(
            service.finish_check(ticket, durable.clone()),
            CheckCompletion::Reduced
        );
        assert_eq!(service.snapshot().linux, durable.linux);
        assert!(service.snapshot().linux_host);
        assert!(service.snapshot().staged.is_none());
        assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
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
            trial_launches: 0,
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
    fn failed_reexec_rearms_only_the_exact_verified_stage() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        let preflight = match service.begin_apply_preflight() {
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

        let retry = match service.begin_apply_preflight() {
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

    // ── The activation lane (an installed bundle newer than the process) ──────

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
            service
                .snapshot()
                .staged
                .as_ref()
                .is_some_and(StagedUpdate::is_installed_activation),
            "the imported stage carries the activation identity"
        );
        ticket
    }

    /// An installed bundle seals a 12-char short sha: a usable identity for an activation
    /// (the durable import and the activation constructor share one rule), never for a
    /// download.
    #[test]
    fn a_short_sha_is_a_usable_stage_identity_and_a_stub_is_not() {
        assert!(usable_commit_identity("248091d23ab0"));
        assert!(usable_commit_identity(" 248091D23AB0 "));
        assert!(usable_commit_identity(&"a".repeat(40)));
        assert!(
            !usable_commit_identity("248091"),
            "six hex is not an identity"
        );
        assert!(!usable_commit_identity(&"a".repeat(41)));
        assert!(!usable_commit_identity("unknown"));
        assert!(!usable_commit_identity("248091d23ab0-dirty"));
        // A download marker (a real DMG digest) with a short sha still mints nothing: the
        // short-sha allowance is bound to the activation digest alone.
        let mut download = DurableUpdateStatus {
            linux_host: false,
            linux: None,
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
            apply_failure: String::new(),
            apply_failure_build: 0,
            apply_failures_for_target: 0,
            installable: Some(true),
            channel_unreadable: false,
            checked_at: None,
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
            trial_launches: 0,
        };
        let stage = installed
            .activation_stage(11, 0)
            .expect("a short-sha bundle newer than the process is an activation");
        assert_eq!(stage.commit.as_deref(), Some("248091d23ab0"), "lowercased");
        assert!(stage.is_installed_activation());
        // …and the durable import accepts exactly what the activation constructed.
        let status = DurableUpdateStatus {
            linux_host: false,
            linux: None,
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
            apply_failure: String::new(),
            apply_failure_build: 0,
            apply_failures_for_target: 0,
            installable: Some(true),
            channel_unreadable: false,
            checked_at: None,
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

    /// NO UPDATE AUTHORITY NAMES A BUILD AT OR BELOW THE RUNNING ONE, so no lane hands a
    /// session to a build older than the one that wrote its records: a download staged
    /// there and an installed bundle there mint no stage and back no activation, and the
    /// one same-image hop (the QA seam, a launch-record renewal) runs this very build.
    /// Round eight of the update audit's U6 (a quiet restart's record read by a build
    /// that predates `quiet`) and U13 (a successor that predates the timeline id carry)
    /// both need such a hop. NEGATIVE CONTROL: one build newer is a stage both ways.
    #[test]
    fn no_update_authority_names_a_build_at_or_below_the_running_one() {
        let running = 11;
        let commit = "0".repeat(40);
        let staged_at = |build: u64| DurableUpdateStatus {
            linux_host: false,
            linux: None,
            enabled: true,
            current_build: running,
            staged_build: Some(build),
            staged_version: Some(format!("0.{build}.0")),
            staged_commit: Some(commit.clone()),
            staged_dmg_sha256: Some("ab".repeat(32)),
            changelog: None,
            outcome: String::new(),
            failing_checks: 0,
            failing_persistent: false,
            failing_kind: String::new(),
            failing_applies: 0,
            apply_failure: String::new(),
            apply_failure_build: 0,
            apply_failures_for_target: 0,
            installable: Some(true),
            channel_unreadable: false,
            checked_at: None,
        };
        let installed_at = |build: u64| InstalledUpdate {
            build,
            commit: commit.clone(),
            version: Some(format!("0.{build}.0")),
            receipt_build: None,
            receipt_dmg_sha256: None,
            trial_launches: 0,
        };
        for build in [running - 1, running] {
            assert!(
                staged_from_status(running, 0, &staged_at(build)).is_none(),
                "a download of build {build} is no stage"
            );
            assert!(verified_download_build(running, &staged_at(build)).is_none());
            assert!(
                installed_at(build).activation_stage(running, 0).is_none(),
                "an installed build {build} is no activation"
            );
            assert!(!installed_at(build).backs_activation(running, build, &commit));
        }
        let newer = running + 1;
        assert!(staged_from_status(running, 0, &staged_at(newer)).is_some());
        assert!(installed_at(newer).activation_stage(running, 0).is_some());
        assert!(installed_at(newer).backs_activation(running, newer, &commit));
    }

    /// While a stage is held `finish_check` cannot run, so the verdict arrives by reconcile:
    /// badged once on the false→true edge, idempotent on repeats, cleared when the ledger
    /// heals.
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
        assert_eq!(
            snap.attention_revision,
            Some(snap.revision),
            "badged on the edge"
        );
        assert!(snap.revision > rev0);
        let rev1 = snap.revision;
        assert!(
            !service.absorb_failure_state(&failing),
            "a repeat changes nothing"
        );
        assert_eq!(
            service.snapshot().revision,
            rev1,
            "no revision churn per cycle"
        );
        let healed = status(Some(11));
        assert!(service.absorb_failure_state(&healed));
        assert!(!service.snapshot().failing_persistent);
    }

    #[test]
    fn the_activation_identity_is_well_formed_pure_and_distinct() {
        let a = installed_activation_digest(11, TEST_COMMIT);
        assert_eq!(a.len(), 64);
        assert!(
            a.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
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
        assert!(
            !ApplyAttemptTicket::for_test(11, TEST_COMMIT, TEST_DIGEST).is_installed_activation()
        );
    }

    /// With no durable marker, an activation stage stands while the bundle under the
    /// executable is still that build and newer, and retires otherwise.
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
        // No observation at all is not "the bundle changed": the facts worker fails closed,
        // so the stage is kept and re-judged next pass.
        stage_activation(&mut service, 11);
        assert_eq!(
            service.reconcile_durable_stage(true, None, None, None, None),
            DurableStageDisposition::Unchanged
        );
        assert!(
            service.snapshot().staged.is_some(),
            "an unobservable bundle must not throw the activation away"
        );
        // …and once the bundle is observed and no longer backs it, it retires.
        let moved_on = installed(12, TEST_COMMIT, None, None);
        assert_eq!(
            service.reconcile_durable_stage(true, None, None, None, Some(&moved_on)),
            DurableStageDisposition::Retired
        );
    }

    #[test]
    fn activation_observation_conforms_in_idle_and_returned_lanes() {
        let model = aterm_models_update::native_update_activation_observation_model();
        let observations = [
            (None, false),
            (Some(installed(11, TEST_COMMIT, None, None)), false),
            (Some(installed(12, TEST_COMMIT, None, None)), true),
            (Some(installed(11, &"f".repeat(40), None, None)), true),
        ];
        for returned in [false, true] {
            for (observation, changed) in &observations {
                let mut service = NativeUpdaterService::new(10, "1.0.10", true);
                stage_activation(&mut service, 11);
                let stage = service.snapshot().staged.clone();
                let mut before = model.init_state();
                if observation.is_some() {
                    let action = if *changed {
                        "ObserveChange"
                    } else {
                        "ObserveMatch"
                    };
                    before = model.successors(action, &before).remove(0);
                }
                if returned {
                    before = model.successors("Return", &before).remove(0);
                    let ApplyPreflightStart::Inspect(ticket) = service.begin_apply_preflight()
                    else {
                        panic!("activation must enter preflight");
                    };
                    let ApplyDecision::Execute(command) =
                        service.finish_apply_preflight(ticket, ClosePreflight::Ready)
                    else {
                        panic!("exact ready preflight must issue one apply authority");
                    };
                    let attempt = command.attempt();
                    let disposition = service.finish_returned_apply(
                        &attempt,
                        ReturnedApplyFacts::new(true, None, None, None, observation.as_ref()),
                        "candidate timed out; installed-bundle probe may be unavailable",
                    );
                    assert_eq!(
                        disposition,
                        if *changed {
                            ReturnedApplyDisposition::Retired
                        } else {
                            ReturnedApplyDisposition::Rearmed
                        }
                    );
                    // The completion consumes the old authority even when its
                    // stage survives. A duplicate cannot spend the retry budget.
                    assert_eq!(
                        service.finish_returned_apply(
                            &attempt,
                            ReturnedApplyFacts::new(true, None, None, None, observation.as_ref()),
                            "duplicate completion",
                        ),
                        ReturnedApplyDisposition::Ignored
                    );
                } else {
                    assert_eq!(
                        service.reconcile_durable_stage(
                            true,
                            None,
                            None,
                            None,
                            observation.as_ref()
                        ),
                        if *changed {
                            DurableStageDisposition::Retired
                        } else {
                            DurableStageDisposition::Unchanged
                        }
                    );
                }
                let retained = service.snapshot().staged.is_some();
                let mut after = before.clone();
                after.insert("reduced", 1);
                after.insert("retained", i64::from(retained));
                assert_eq!(
                    model.successors("Reduce", &before),
                    vec![after.clone()],
                    "real activation reducer must conform: returned={returned}, observation={observation:?}"
                );
                for invariant in &model.invariants {
                    assert!(model.check_invariant(invariant.name, &after));
                }
                if retained {
                    assert_eq!(service.snapshot().staged, stage);
                    assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
                    assert!(service.active_apply.is_none());
                }
                if returned && observation.is_none() {
                    // Negative control: retiring on an unknown observation is rejected.
                    let mut historical = after;
                    historical.insert("retained", 0);
                    assert!(!model.successors("Reduce", &before).contains(&historical));
                    assert!(!model.check_invariant("NoRetirementWithoutEvidence", &historical));
                }
            }
        }
    }

    /// Nothing else: an activation already in place, an in-flight apply and an empty reducer
    /// keep their state.
    #[test]
    fn a_download_stage_retires_for_activation_but_nothing_else_does() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        assert!(
            !service.retire_stage_for_activation(),
            "nothing staged: no-op"
        );
        stage(&mut service, 11);
        assert!(
            service.retire_stage_for_activation(),
            "a download stage retires"
        );
        assert!(service.snapshot().staged.is_none());
        assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
        stage_activation(&mut service, 12);
        assert!(
            !service.retire_stage_for_activation(),
            "an activation stays"
        );
        assert!(service.snapshot().staged.is_some());
        let preflight = match service.begin_apply_preflight() {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            other => panic!("expected preflight, got {other:?}"),
        };
        let _command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
            ApplyDecision::Execute(command) => command,
            other => panic!("expected execute decision, got {other:?}"),
        };
        assert!(
            !service.retire_stage_for_activation(),
            "an in-flight apply is untouchable"
        );
        assert_eq!(service.snapshot().phase, UpdaterPhase::Applying);
    }

    #[test]
    fn consumed_durable_stage_retires_memory_and_reports_installed_activation() {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        stage(&mut service, 11);
        let preflight = match service.begin_apply_preflight() {
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
        assert_eq!(
            service.last_transitions()[0].action,
            UpdaterTransitionAction::RetireApply
        );
        assert!(!service.abort_apply(&attempt, "stale retry callback"));
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
        let preflight = match service.begin_apply_preflight() {
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
    fn disabled_status_never_imports_or_authorizes_a_leftover_stage() {
        let mut disabled = status(Some(11));
        disabled.enabled = false;
        let mut service = NativeUpdaterService::from_durable_status(10, "1.0.10", disabled);
        assert!(!service.snapshot().enabled);
        assert_eq!(service.snapshot().phase, UpdaterPhase::Disabled);
        assert!(service.snapshot().staged.is_none());
        assert_eq!(
            service.begin_apply_preflight(),
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
            service.begin_apply_preflight(),
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
        let ticket = match service.begin_apply_preflight() {
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

    /// NOTHING READ, NOTHING CLAIMED (s-16): a fresh service has observed nothing
    /// about whether this copy can replace itself, so it is neither installable nor
    /// not; a check's read says which, and a platform with no updater knows at
    /// construction. A worker failure that read nothing leaves it unobserved.
    #[test]
    fn installability_is_unobserved_until_a_read_says() {
        let fresh = NativeUpdaterService::new(10, "1.0.10", true);
        assert_eq!(
            fresh.snapshot().installable,
            UpdateInstallability::Unobserved
        );
        assert_eq!(
            NativeUpdaterService::new(10, "1.0.10", false)
                .snapshot()
                .installable,
            UpdateInstallability::NotInstallable,
            "no updater on this platform is known without a read"
        );
        for (read, want) in [
            (Some(false), UpdateInstallability::NotInstallable),
            // NEGATIVE CONTROL: a read that found a bundle says so.
            (Some(true), UpdateInstallability::Installable),
            (None, UpdateInstallability::Unobserved),
        ] {
            let mut service = NativeUpdaterService::new(10, "1.0.10", true);
            let ticket = started(&mut service);
            let mut checked = status(None);
            checked.installable = read;
            let _ = service.finish_check(ticket, checked);
            assert_eq!(service.snapshot().installable, want, "{read:?}");
        }
        // The launch's own path seeds only what it knows; a read still decides.
        let mut seeded = NativeUpdaterService::new(10, "1.0.10", true)
            .seed_installability(UpdateInstallability::NotInstallable);
        assert_eq!(
            seeded.snapshot().installable,
            UpdateInstallability::NotInstallable
        );
        assert!(seeded.note_installability(true));
        assert_eq!(
            seeded.snapshot().installable,
            UpdateInstallability::Installable
        );
        assert!(
            !seeded.note_installability(true),
            "an unchanged read republishes nothing"
        );
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
