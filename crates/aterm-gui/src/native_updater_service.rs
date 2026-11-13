// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-owned reducer for the native Software Update route.

use std::fmt;

const MAX_SHORT_TEXT_BYTES: usize = 512;
const MAX_MESSAGE_BYTES: usize = 8 * 1024;
const MAX_CHANGELOG_BYTES: usize = 256 * 1024;

/// Whether THIS copy can replace itself, as far as anything has OBSERVED.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum UpdateInstallability {
    /// Nothing has read it yet.
    #[default]
    Unobserved,
    /// A read found a bundle the updater can replace.
    Installable,
    /// A read (or the launch's own path) found none.
    NotInstallable,
}

impl UpdateInstallability {
    #[must_use]
    pub(crate) const fn observed(installable: bool) -> Self {
        if installable {
            Self::Installable
        } else {
            Self::NotInstallable
        }
    }

    /// A read found a replaceable bundle.
    #[must_use]
    pub(crate) fn is_installable(self) -> bool {
        self == Self::Installable
    }

    /// A read found NO replaceable bundle.
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

/// How long a check this window started may stay silent: the watchdog's stall bound and its
/// confirming look. On macOS, progress restarts it ([`NativeUpdaterService::note_check_progress`]), as
/// it stamps that watchdog's heartbeat, so a slow download that keeps moving is no stuck check. Linux
/// reports no progress, so there it counts from the check's start.
#[must_use]
pub(crate) fn check_answers_within() -> std::time::Duration {
    use aterm_update::checker_watch::{CheckerPhase, STALL_CONFIRM, stall_threshold_secs};
    std::time::Duration::from_secs(stall_threshold_secs(CheckerPhase::Checking, 0))
        .saturating_add(STALL_CONFIRM)
}

/// The longest a check this window started may hold update work, progress or none. A report carries
/// no ticket, so a scheduled check's downloads also count for a press whose worker died, and a
/// transfer cut midway every cycle would otherwise renew it for ever. One download's attempts take at
/// most about 12 h, and a late answer still asks for the stage it names.
pub(crate) const CHECK_ANSWER_CEILING: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

/// What the update page says while a check that gave up has no answer
/// ([`UpdaterSnapshot::check_unanswered_since`]).
pub(crate) const CHECK_DID_NOT_ANSWER: &str = "An update check stopped answering.";

/// Exact identity of updater work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UpdaterWorkTicket {
    generation: u64,
    operation: u64,
    kind: UpdaterWorkKind,
}

/// Field-for-field, bounded host projection of [`aterm_update::UpdateStatus`].
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
    /// The health ledger's verdict that the streak is persistent (at least the updater's `PERSISTENT_AFTER` consecutive failures of one class).
    pub(crate) failing_persistent: bool,
    /// The most recent failing class the ledger recorded (`network` / `pipeline` / `manifest` / `stage` / `apply`), empty when not failing.
    pub(crate) failing_kind: String,
    /// Consecutive apply failures.
    pub(crate) failing_applies: u32,
    /// Why the applies in [`Self::failing_applies`] failed.
    pub(crate) apply_failure: String,
    /// Target build of [`Self::apply_failure`] (0 when unknown), paired with [`Self::apply_failures_for_target`].
    pub(crate) apply_failure_build: u64,
    /// See [`Self::apply_failure_build`].
    pub(crate) apply_failures_for_target: u32,
    /// This launch has a bundle the updater could REPLACE.
    pub(crate) installable: Option<bool>,
    /// The stranded verdict ([`aterm_update::UpdateStatus::channel_unreadable`]).
    pub(crate) channel_unreadable: bool,
    /// When the last check completed (Unix seconds; `aterm_update::UpdateStatus::updated_at`), `None` before any.
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
    /// Whether this stage is an installed-bundle activation rather than a download.
    #[must_use]
    pub(crate) fn is_installed_activation(&self) -> bool {
        self.commit.as_deref().is_some_and(|commit| {
            installed_activation_digest(self.build, commit) == self.dmg_sha256
        })
    }
}

/// The activation identity.
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
/// does not hold: word for word the App's `INSTALLED_ACTIVATES_IN_PLACE`.
const INSTALLED_SWITCHES_IN_PLACE: &str =
    "The update is installed; switching to it keeps your shells running";

/// Compare one durable marker to an already-canonical artifact identity, as hex values, not spellings.
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

/// Worker-collected identity of the canonical installed bundle and the exact pre-swap trial artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstalledUpdate {
    pub(crate) build: u64,
    pub(crate) commit: String,
    /// Marketing version of the installed bundle (display only; `None` when unread).
    pub(crate) version: Option<String>,
    pub(crate) receipt_build: Option<u64>,
    pub(crate) receipt_dmg_sha256: Option<String>,
    /// Launches the boot sentinel has counted against a trial of this build (0 when none is armed), read on the updater facts worker.
    pub(crate) trial_launches: u32,
}

impl InstalledUpdate {
    #[must_use]
    pub(super) fn proves_artifact(
        &self,
        build: u64,
        commit: Option<&str>,
        dmg_sha256: &str,
    ) -> bool {
        self.build == build
            && commit.is_some_and(|commit| aterm_update::commit_matches(&self.commit, commit))
            && self.receipt_build == Some(build)
            && self
                .receipt_dmg_sha256
                .as_deref()
                .is_some_and(|trial| trial.eq_ignore_ascii_case(dmg_sha256))
    }

    /// Whether this installed bundle backs an activation of `(build, commit)`.
    #[must_use]
    pub(crate) fn backs_activation(&self, running_build: u64, build: u64, commit: &str) -> bool {
        self.build == build
            && self.build > running_build
            && aterm_update::commit_matches(&self.commit, commit)
    }

    /// The activation stage this installed bundle would present, when it is newer than the running build.
    #[must_use]
    pub(crate) fn activation_stage(
        &self,
        running_build: u64,
        generation: u64,
    ) -> Option<StagedUpdate> {
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

/// Immutable service projection that UI, tab attention, accessibility and inspection all consume.
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
    /// The ledger's persistent-failure verdict as of the last reduced check.
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
    /// Whether this copy can replace itself, as far as a read has OBSERVED ([`DurableUpdateStatus::installable`]).
    pub(crate) installable: UpdateInstallability,
    /// See [`DurableUpdateStatus::channel_unreadable`].
    pub(crate) channel_unreadable: bool,
    /// See [`DurableUpdateStatus::checked_at`].
    pub(crate) checked_at: Option<i64>,
    /// A check started here gave up with no answer ([`NativeUpdaterService::retire_check`]): when it
    /// started, in Unix seconds. It stands until a stage or a disabled updater replaces it, or a
    /// receipt newer than that start shows a check ended on this Mac. Any receipt counts: a check
    /// that recorded a failure and returned `Ok` (the asset host, a rejected manifest) or deferred
    /// stamps one and ends it, and the page then reads as it does after any such check. The ledger's
    /// own words can say "up to date" all the while, so a reconcile's read leaves it, and a check that
    /// returned `Err` without deferring writes no receipt and leaves it too, so the words speak of
    /// that check, not of the latest one. A start stamped by a clock since set back stands until a
    /// receipt passes it: the words go on, and no update is held.
    pub(crate) check_unanswered_since: Option<i64>,
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
    /// Work is already running.
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
    /// Background policy.
    Automatic,
    /// Background policy whose bounded idle-preference window has closed.
    AutomaticPastGrace,
    /// Explicit user/control apply.
    Immediate,
}

impl ApplyMode {
    /// Whether this attempt came from background policy rather than a person.
    #[must_use]
    pub(crate) const fn is_automatic(self) -> bool {
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

/// A linear host command.
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
    /// Test-only exact-artifact ticket for exercising the App-side completion path without the whole reducer.
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
    pub(crate) const fn target_build(&self) -> u64 {
        self.artifact_build
    }

    /// Test-only: make this ticket the current apply, so `abort_apply` admits it and the completion path itself is tested, not only its policy helpers.
    #[cfg(test)]
    pub(crate) fn make_current_apply_for_test(&self, service: &mut NativeUpdaterService) {
        service.snapshot.generation = self.generation;
        service.snapshot.phase = UpdaterPhase::Applying;
        service.snapshot.reexec_count = 1;
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

    /// Whether this attempt activates the installed bundle ([`installed_activation_digest`]) instead of swapping a staged download.
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
    /// The staged marker was consumed and this build (or a newer one) is installed at the canonical app path.
    InstalledNeedsRelaunch { build: u64 },
    /// The attempted artifact is no longer authoritative and was retired in memory.
    Retired,
    /// The callback did not name the currently-live apply authority.
    Ignored,
}

/// Atomic projection of durable updater state observed after a replacement attempt returned.
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

/// Scalar projection Tier-1 binds to `native_updater_model`.
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

/// Process-global updater state.
pub(crate) struct NativeUpdaterService {
    snapshot: UpdaterSnapshot,
    next_operation: u64,
    work_generation: u64,
    close_preflight_ready: bool,
    pending_preflight: Option<ApplyPreflightTicket>,
    active_apply: Option<ApplyAttemptTicket>,
    /// When the check last started, which [`CHECK_ANSWER_CEILING`] counts from.
    check_started: Option<std::time::Instant>,
    /// When the check last started or last reported download or verification progress, which
    /// [`Self::check_answer_by`] counts from.
    check_heard: Option<std::time::Instant>,
    /// [`Self::check_started`] in Unix seconds, which a receipt's time compares with
    /// ([`UpdaterSnapshot::check_unanswered_since`]).
    check_started_unix: i64,
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
                installable: if enabled {
                    UpdateInstallability::Unobserved
                } else {
                    UpdateInstallability::NotInstallable
                },
                channel_unreadable: false,
                checked_at: None,
                check_unanswered_since: None,
            },
            next_operation: 1,
            work_generation: 0,
            close_preflight_ready: false,
            check_started: None,
            check_heard: None,
            check_started_unix: 0,
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
    pub(crate) const fn snapshot(&self) -> &UpdaterSnapshot {
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
        let now = std::time::Instant::now();
        self.check_started = Some(now);
        self.check_heard = Some(now);
        self.check_started_unix =
            aterm_time::unix_secs().map_or(0, |secs| i64::try_from(secs).unwrap_or(i64::MAX));
        CheckStart::Start(ticket)
    }

    /// When the check in `snapshot.active` stops holding this window's update work if it has not
    /// answered: see [`Self::retire_check`]. `None` with no check out.
    #[must_use]
    pub(crate) fn check_answer_by(&self) -> Option<std::time::Instant> {
        if self.snapshot.active?.kind != UpdaterWorkKind::CheckAndStage {
            return None;
        }
        let silent = self.check_heard?.checked_add(check_answers_within())?;
        let longest = self.check_started?.checked_add(CHECK_ANSWER_CEILING)?;
        Some(silent.min(longest))
    }

    /// Whether, at `now`, the check in `snapshot.active` has been out for [`CHECK_ANSWER_CEILING`],
    /// rather than only silent for [`check_answers_within`]. A report carries no ticket, so this says
    /// nothing of whose downloads kept it from going silent.
    #[must_use]
    pub(crate) fn check_hit_ceiling(&self, now: std::time::Instant) -> bool {
        self.check_started
            .and_then(|started| started.checked_add(CHECK_ANSWER_CEILING))
            .is_some_and(|longest| now >= longest)
    }

    /// A check reported download or verification progress at `at`, so its silence counts from there
    /// ([`check_answers_within`]), up to [`CHECK_ANSWER_CEILING`]. A report carries no ticket, so a
    /// scheduled check's download also counts for a press joined behind it, which is waiting on that
    /// download.
    pub(crate) fn note_check_progress(&mut self, at: std::time::Instant) {
        if self
            .snapshot
            .active
            .is_some_and(|active| active.kind == UpdaterWorkKind::CheckAndStage)
        {
            self.check_heard = Some(at);
        }
    }

    /// Give up on the check in `snapshot.active` past [`Self::check_answer_by`], silent that long or
    /// out for [`CHECK_ANSWER_CEILING`] whatever it reported (it panicked past its guard, or is stuck
    /// behind a stuck lane or in a file call that never returns).
    /// Otherwise only that answer would clear `active`, and while it stands a stage another process
    /// wrote is never imported, Install finds nothing staged, and the node job leaves the apply to
    /// this window. The check fails as one that answered with a failure does, and its late answer
    /// meets no active work ([`Self::completion_mismatch`]), though a stage that answer names is
    /// still asked for (`App::finish_native_update_check`). The page says it gave up
    /// ([`UpdaterSnapshot::check_unanswered_since`]), with no outcome of its own, so nothing says it
    /// twice or as if it were still going. It cannot free what the stuck thread
    /// holds: this process's own checks wait on its check lane, and while it holds `stage.lock`
    /// nothing on this Mac stages until it returns or this process quits. Returns whether it gave up
    /// on anything.
    pub(crate) fn retire_check(&mut self, why: &str) -> bool {
        self.last_transitions.clear();
        if !self
            .snapshot
            .active
            .is_some_and(|active| active.kind == UpdaterWorkKind::CheckAndStage)
        {
            return false;
        }
        let before = self.model_state();
        self.snapshot.active = None;
        self.snapshot.phase = UpdaterPhase::Failed;
        self.snapshot.error = Some(why.to_string());
        self.snapshot.outcome = String::new();
        self.snapshot.check_unanswered_since = Some(self.check_started_unix);
        self.record(UpdaterTransitionAction::CheckFailed, before);
        self.publish();
        true
    }

    /// What the launch itself established about this copy before any read.
    #[must_use]
    pub(crate) fn seed_installability(mut self, seed: UpdateInstallability) -> Self {
        if self.snapshot.installable == UpdateInstallability::Unobserved {
            self.snapshot.installable = seed;
        }
        self
    }

    /// Record this copy's installability ([`DurableUpdateStatus::installable`] or bundle probe).
    pub(crate) fn note_installability(&mut self, installable: bool) -> bool {
        let observed = UpdateInstallability::observed(installable);
        if !self.snapshot.enabled || self.snapshot.installable == observed {
            return false;
        }
        self.snapshot.installable = observed;
        self.publish();
        true
    }

    /// Take a newer "last check completed" time from a reconcile (background checks write only the ledger).
    pub(crate) fn note_checked_at(&mut self, checked_at: Option<i64>) -> bool {
        let answered = self.note_receipt_of_a_check(checked_at);
        let newer = checked_at.filter(|at| self.snapshot.checked_at.is_none_or(|held| *at > held));
        if let Some(at) = newer {
            self.snapshot.checked_at = Some(at);
        }
        let changed = answered || newer.is_some();
        if changed {
            self.publish();
        }
        changed
    }

    /// A receipt written after the check that gave up had started: a check did answer on this Mac.
    /// Returns whether that ended [`UpdaterSnapshot::check_unanswered_since`].
    fn note_receipt_of_a_check(&mut self, checked_at: Option<i64>) -> bool {
        let answered = self
            .snapshot
            .check_unanswered_since
            .zip(checked_at)
            .is_some_and(|(since, at)| at > since);
        if answered {
            self.snapshot.check_unanswered_since = None;
        }
        answered
    }

    /// Absorb the ledger's failure verdict from a reconcile that imports no stage (the exact-match early path, or a held stage that refuses a new check).
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

    /// Absorb an apply failure the host just wrote, without waiting for a disk re-read.
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
        self.snapshot.channel_unreadable = status.enabled && status.channel_unreadable;
        if status.checked_at.is_some() {
            self.snapshot.checked_at = status.checked_at;
        }
        let _answered = self.note_receipt_of_a_check(status.checked_at);
        if !status.enabled {
            let before = self.model_state();
            self.snapshot.active = None;
            self.snapshot.phase = UpdaterPhase::Disabled;
            self.snapshot.staged = None;
            self.snapshot.check_unanswered_since = None;
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
            self.snapshot.check_unanswered_since = None;
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

    /// Retire a staged download so an installed-bundle activation can be imported instead.
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

    /// Consume a matching close-preflight result and, only when safe, mint the one-shot command that authorizes the existing seamless apply/re-exec path.
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

    /// The outcome sentence for a build already installed at the canonical app path, which the App imports as an activation stage.
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

    /// The outcome sentence for a physical apply attempt that returned safely.
    #[must_use]
    pub(crate) fn apply_attempt_stopped_outcome(message: &str) -> String {
        format!("The update is still ready; the install stopped: {message}")
    }

    /// Re-arm an authorized artifact when process replacement left this process alive.
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

    /// Reconcile a returned process-replacement attempt against disk state.
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

    /// Retire a non-applying in-memory stage when another process changed or consumed its durable marker.
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

        if staged.is_installed_activation() {
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

    pub(crate) fn apply_ticket_is_current(&self, ticket: &ApplyAttemptTicket) -> bool {
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

    const fn publish(&mut self) {
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
    }
}

/// A commit string that can serve as a stage identity.
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

/// The build of the verified download a durable observation names, when strictly newer than `running_build`.
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
#[path = "native_updater_service_tests.rs"]
mod tests;
