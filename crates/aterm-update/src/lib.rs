// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(
    all(anchor_rules, any(test, all(debug_assertions, target_os = "macos"))),
    feature(macro_attr)
)]

//! Signature-pinned in-app self-update for macOS app bundles and enrolled `linux` binaries.
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

#[cfg(target_os = "macos")]
mod bundle;
#[cfg(any(target_os = "macos", test))]
mod cadence;
mod cadence_bounds;
#[cfg(target_os = "macos")]
mod check_lane;
#[cfg(target_os = "macos")]
mod check_receipt;
/// The background check loop's heartbeat and the pure judgement of it that the window's watchdog,
/// `aterm ctl update status` and the tests share.
pub mod checker_watch;
/// A dev build's standing against the public channel.
pub mod dev_channel;
/// The durability flushes every atomic publish in this crate takes.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod durable;
#[cfg(unix)]
mod exec_plain;
/// Who raised an update floor, so a revocation can take it back.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod floor_sources;
#[cfg(target_os = "macos")]
mod github;
#[cfg(target_os = "macos")]
mod health;
#[cfg(target_os = "macos")]
mod install;
mod install_posture;
/// Signed single-executable Linux update transactions and explicit enrollment.
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub mod linux;
/// What the Linux background check tells the window about update health.
#[cfg(any(target_os = "linux", test))]
mod linux_notice;
/// When one more start of a replaced Linux executable rolls it back.
#[cfg(any(target_os = "linux", test))]
mod linux_trial;
#[cfg(target_os = "macos")]
mod manifest;
#[cfg(target_os = "macos")]
mod paths;
mod progress;

/// Machine-readable compiled identity.
pub fn binary_identity_json(
    identity: &aterm_update_core::linux::BinaryIdentity,
) -> Result<String, String> {
    aterm_json::to_string(identity).map_err(|error| error.to_string())
}
#[cfg(all(test, target_os = "macos"))]
mod checker_supervision_tests;
/// How each run of the installed app ended.
pub mod recovery_ledger;
mod relaunch;
/// Whether the crash restore has soaked on this machine's daily driver.
pub mod restore_soak;
#[cfg(target_os = "macos")]
mod status;
#[cfg(target_os = "macos")]
mod sys;
#[cfg(target_os = "macos")]
mod unreadable;
#[cfg(target_os = "macos")]
mod verify;
pub mod which_copy;

#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
pub use manifest::failed_mark_suppression_projection;

#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
pub use github::roster_freshness_projection;

#[cfg(all(target_os = "macos", debug_assertions))]
pub use install::dmg_acceptance;

/// The plain `exec` that puts sigpipe back when it fails (`exec_plain`).
#[cfg(unix)]
pub use exec_plain::exec_keeping_sigpipe;

/// Hard cap for every small TOML ledger/marker consumed by the updater.
#[cfg(target_os = "macos")]
const MAX_LEDGER_BYTES: u64 = 256 * 1024;

#[cfg(target_os = "macos")]
fn read_ledger_text(path: &std::path::Path) -> Option<String> {
    read_ledger_text_checked(path).ok().flatten()
}

/// [`read_ledger_text`] that keeps the one distinction a read-then-replace writer needs:
/// `Ok(None)` is a file absent or not a ledger, `Err` a present file this read could not get
/// through, which a writer must not replace with defaults.
#[cfg(target_os = "macos")]
fn read_ledger_text_checked(path: &std::path::Path) -> std::io::Result<Option<String>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_LEDGER_BYTES {
        return Ok(None);
    }
    let Ok(initial_capacity) = usize::try_from(metadata.len()) else {
        return Ok(None);
    };
    let mut bytes = Vec::with_capacity(initial_capacity);
    file.take(MAX_LEDGER_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > MAX_LEDGER_BYTES) {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}

/// The resolved GitHub release source and the compiled-in default owner/repo.
pub use aterm_update_core::{DEFAULT_OWNER, DEFAULT_REPO, Source};

pub use progress::{Progress, ProgressNotify, set_progress_observer};

/// Argv forwarding for every lane that relaunches aterm: the leading `--window` pins the boot
/// swap adds are stripped, so no relaunch re-grows them.
pub use relaunch::{
    RELAUNCH_OF_RUNNING_ENV, reexec_forwarded_args, running_window_successor_args,
    window_successor_args,
};

/// The Apple Developer Team ID for the optional Tier APPLE anchor.
pub const PINNED_TEAM_ID: &str = aterm_update_core::pins::APPLE_TEAM_ID;

static REQUIRED_TEAM_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Opts in to Developer-ID and notarization enforcement at runtime, without a rebuild.
pub fn set_required_team_id(team: Option<&str>) {
    if !PINNED_TEAM_ID.is_empty() {
        return;
    }
    let Some(team) = team.map(str::trim).filter(|t| !t.is_empty()) else {
        return;
    };
    let _ = REQUIRED_TEAM_ID.set(team.to_string());
}

/// The Team ID verification must satisfy.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn effective_team_id() -> &'static str {
    if !PINNED_TEAM_ID.is_empty() {
        return PINNED_TEAM_ID;
    }
    REQUIRED_TEAM_ID.get().map_or("", String::as_str)
}

/// SHA-256 of a raw 32-byte Ed25519 key, for shipping introspection.
pub fn update_pubkey_sha256(encoded: &str) -> Result<Option<String>, String> {
    use aterm_digest::Sha256;

    if encoded.is_empty() {
        return Ok(None);
    }
    let raw = aterm_codec::base64::decode_strict(encoded.as_bytes())
        .map_err(|_| "compiled update public key is not standard base64".to_string())?;
    if raw.len() != 32 {
        return Err(format!(
            "compiled update public key decodes to {} bytes, not 32",
            raw.len()
        ));
    }
    Ok(Some(aterm_codec::hex::encode_lower(&Sha256::digest(&raw))))
}

/// The update pin: SHA-256 of the raw 32 bytes of `pins::PAPER_MASTER_PUBKEYS[0]` as 64 lowercase.
#[must_use]
pub fn compiled_update_pin_sha256() -> String {
    fingerprint_state(
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS
            .first()
            .copied()
            .unwrap_or(""),
    )
}

fn fingerprint_state(encoded: &str) -> String {
    match update_pubkey_sha256(encoded) {
        Ok(Some(fingerprint)) => fingerprint,
        Ok(None) => "empty".to_string(),
        Err(_) => "invalid".to_string(),
    }
}

/// Outcome of an [`apply_staged_if_ready_preserving_fds_exact`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Not an installed `.app` launch.
    NotApplicable,
    /// Nothing staged, or what's staged is not strictly newer.
    NoUpdate,
    /// A newer build is staged but cannot be applied now.
    Deferred(String),
    /// The bundle was swapped but re-exec into the new binary failed.
    ReExecFailed(String),
    /// A LaunchServices-launched seamless successor found its build crash-looping and put the
    /// previous build back without re-executing it; the caller must exit now.
    Reverted(String),
    /// A macOS window launch ([`ImageReplacement::WindowKeepsImage`]) found this stage ready.
    LeftForSession {
        /// The staged build left.
        build: u64,
        /// Its container's SHA-256, as `ready.toml` names it.
        dmg_sha256: String,
    },
}

/// What a launch's boot apply may do to the process's image, decided once by the caller. An
/// `execve` soon after LaunchServices launched a process leaves LaunchServices answering pid -1
/// for the app for the rest of its life, so a window that may have been launched that way keeps
/// its image when its own lane can install the stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageReplacement {
    /// The boot may swap a ready stage and exec the build it installed.
    Replaceable,
    /// A macOS window no parent handed off to: it leaves a ready stage for its window's
    /// in-session lane ([`ApplyOutcome::LeftForSession`]). With `[update] auto_apply` on, once
    /// [`LEFT_STAGE_ESCAPE`] earlier launches left the stage and ended without it landing, or the
    /// record cannot count this launch, the boot swaps and execs it.
    WindowKeepsImage,
    /// LaunchServices started this process as a seamless successor (it was handed a rendezvous).
    LaunchedSuccessor,
}

/// How many launches that left one stage for their window's automatic lane must have ended,
/// with the stage still there, before the next window launch of it swaps and execs it at boot.
pub const LEFT_STAGE_ESCAPE: usize = 2;

/// What a launch of this build can prove about its boot trial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialWitness {
    /// A launch that opens windows.
    Window,
    /// A `--headless` launch. It proves only the engine and control socket, so a trial a window
    /// launched waits for a window while that wait lasts.
    Headless,
}

/// Whether the updater runs on this platform at all.
#[must_use]
pub const fn enabled() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// Whether the running copy has a bundle the updater could replace.
#[must_use]
pub fn running_copy_installable() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        Some(bundle::resolve().is_some())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Whether the running copy is a dev-marked bundle.
#[must_use]
pub fn running_is_dev_marked() -> bool {
    #[cfg(target_os = "macos")]
    {
        bundle::resolve_layout().is_some_and(|b| bundle::is_dev_marked(&b.app_root))
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// "Check for updates automatically" (`[update] enabled`) on a platform the updater runs on
/// ([`enabled`]). It gates only the lanes nobody asked for, such as the background checker.
#[must_use]
pub fn automatic() -> bool {
    enabled() && aterm_update_core::settings::update_enabled()
}

/// Applies a staged update if one is ready and strictly newer than `current_build`.
#[cfg(target_os = "macos")]
#[must_use]
pub fn apply_staged_if_ready_preserving_fds_exact(
    current_build: u64,
    current_commit: &str,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target_is_this_build: bool,
    image: ImageReplacement,
    witness: TrialWitness,
) -> ApplyOutcome {
    install::apply_staged_if_ready(
        current_build,
        Some(current_commit),
        handoff_fds,
        handoff_env,
        handoff_target_is_this_build,
        image,
        witness,
    )
}

/// The parent's swap before it asks LaunchServices for a seamless successor.
#[cfg(target_os = "macos")]
pub fn swap_staged_for_launch(
    current_build: u64,
    current_commit: &str,
    expected: (u64, &str, &str),
) -> Result<u64, ApplyOutcome> {
    let (build, commit, dmg_sha256) = expected;
    let expected = install::ExpectedArtifact::from_parts(build, commit, dmg_sha256)
        .map_err(ApplyOutcome::Deferred)?;
    install::swap_staged_for_launch(current_build, current_commit, &expected)
}

/// Off macOS no launch goes through LaunchServices, so there is nothing to swap first.
#[cfg(not(target_os = "macos"))]
pub fn swap_staged_for_launch(
    _current_build: u64,
    _current_commit: &str,
    _expected: (u64, &str, &str),
) -> Result<u64, ApplyOutcome> {
    Err(ApplyOutcome::NotApplicable)
}

/// The clause an apply-path refusal carries when its cause can clear by itself.
pub const PASSING_REFUSAL_KEY: &str = "a passing condition, not a verdict on the update";

/// The key every "the install location cannot be written" refusal carries.
pub const INSTALL_LOCATION_REFUSAL_KEY: &str = "install location not writable";

/// True when a refusal is the non-writable install location's ([`INSTALL_LOCATION_REFUSAL_KEY`]).
#[must_use]
pub fn is_install_location_refusal(reason: &str) -> bool {
    reason.contains(INSTALL_LOCATION_REFUSAL_KEY)
}

/// A tool's output, quoted into a refusal so it can never speak for it.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn quote_tool_output(text: &str) -> String {
    let mut quoted = text.to_string();
    for key in [
        PASSING_REFUSAL_KEY,
        INSTALL_LOCATION_REFUSAL_KEY,
        ROSTER_UNCONFIRMED_KEY,
        NOT_INSTALLED_KEY,
        install::ROLLBACK_SOURCE_REFUSAL_KEY,
        github::STALE_ANCHOR_KEY,
    ] {
        if quoted.contains(key) {
            quoted = quoted.replace(key, &key.replace(' ', "\u{a0}"));
        }
    }
    quoted
}

/// Whether an apply-path refusal is a passing moment ([`PASSING_REFUSAL_KEY`]).
#[must_use]
pub fn is_passing_refusal(reason: &str) -> bool {
    reason.contains(PASSING_REFUSAL_KEY)
}

/// The words every "this stage waits for an update check to confirm its signer" refusal carries.
pub const ROSTER_UNCONFIRMED_KEY: &str = "confirmed against a current machine roster";

/// Whether an apply-path refusal is the wait for an update check to confirm the stage's roster.
#[must_use]
pub fn is_roster_confirmation_wait(reason: &str) -> bool {
    is_passing_refusal(reason) && reason.contains(ROSTER_UNCONFIRMED_KEY)
}

/// Whether a refusal is a person's to clear.
#[cfg(target_os = "macos")]
pub fn refusal_needs_person(reason: &str) -> bool {
    !is_passing_refusal(reason)
        && (install::is_rollback_source_refusal(reason)
            || is_install_location_refusal(reason)
            || reason.contains(NOT_INSTALLED_KEY))
}

/// The words of the refusal for an app that does not run from an installed bundle.
#[cfg(target_os = "macos")]
const NOT_INSTALLED_KEY: &str = "does not run from an installed bundle";

#[cfg(not(target_os = "macos"))]
pub fn refusal_needs_person(_reason: &str) -> bool {
    false
}

/// Overlap-handoff pre-park verification.
#[cfg(target_os = "macos")]
pub fn preverify_staged_for_handoff(
    current_build: u64,
    current_commit: Option<&str>,
    expected_build: Option<u64>,
    expected_commit: Option<&str>,
) -> Result<HandoffCandidateFacts, String> {
    install::preverify_staged_handoff_candidate(
        current_build,
        current_commit,
        expected_build,
        expected_commit,
    )
}

/// The code-signing identifier of a handoff candidate (the staged `.app`, or `activation`),
/// which the launched lane's rendezvous gates its dialer on.
///
/// # Errors
/// No staging root, or `codesign` failed or named no identifier.
#[cfg(target_os = "macos")]
pub fn handoff_candidate_signing_identifier(
    activation: Option<&std::path::Path>,
) -> Result<String, String> {
    match activation {
        Some(bundle) => verify::signing_identifier(bundle),
        None => {
            let staging = paths::Staging::resolve()
                .ok_or_else(|| "no private staging root is available".to_string())?;
            verify::signing_identifier(&staging.staged_app)
        }
    }
}

/// What a cached pass cannot vouch for.
#[cfg(target_os = "macos")]
pub fn handoff_cached_pass_refusal(
    target_build: u64,
    staged_commit: Option<&str>,
) -> Option<String> {
    let Some(staging) = paths::Staging::resolve() else {
        return staged_commit.map(|_| {
            format!("no staging root holds staged build {target_build}; not handing off to it")
        });
    };
    match staged_commit {
        Some(commit) => install::handoff_cached_pass_refusal_at(&staging, target_build, commit),
        None => install::handoff_activation_cached_pass_refusal_at(&staging, target_build, || {
            bundle::resolve_layout()
                .map(|installed| verify::bundle_build_number(&installed.app_root))
        }),
    }
}

/// Off macOS there is no bundle staging root, so no floor to refuse by.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn handoff_cached_pass_refusal(
    target_build: u64,
    staged_commit: Option<&str>,
) -> Option<String> {
    let _ = (target_build, staged_commit);
    None
}

/// The longest one handoff pre-verification runs.
#[cfg(target_os = "macos")]
pub const HANDOFF_PREVERIFY_BOUND: std::time::Duration = std::time::Duration::from_secs(
    install::APPLY_LOCK_WAIT.as_secs() + verify::APPLY_BUDGET.as_secs(),
);

/// Off macOS a pre-verification checks nothing and returns.
#[cfg(not(target_os = "macos"))]
pub const HANDOFF_PREVERIFY_BOUND: std::time::Duration = std::time::Duration::ZERO;

/// What an outgoing build learns from a handoff candidate's bundle once the pre-verify has
/// authenticated it, never from a bundle that failed a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffCandidateFacts {
    /// The candidate's signed handoff policy.
    pub policy: aterm_update_core::handoff_policy::PolicyRead,
    /// The candidate's sealed `Info.plist` declares the chunked rendezvous grant.
    pub grant_chunks: bool,
}

impl HandoffCandidateFacts {
    /// Read both facts off a bundle that has just passed its checks.
    #[cfg(target_os = "macos")]
    pub(crate) fn read(app_root: &std::path::Path) -> Result<Self, String> {
        Self::admitted(
            aterm_update_core::handoff_policy::read_from_bundle(app_root),
            bundle::declares_grant_chunks(app_root),
        )
    }

    /// The facts as a pre-verification may pass them on.
    #[cfg(target_os = "macos")]
    fn admitted(
        policy: aterm_update_core::handoff_policy::PolicyRead,
        grant_chunks: Result<bool, String>,
    ) -> Result<Self, String> {
        if let aterm_update_core::handoff_policy::PolicyRead::Unread(why) = &policy {
            return Err(verify::mark_passing(format!(
                "the candidate's handoff policy could not be read just then ({why}); the \
                 next attempt reads it again"
            )));
        }
        let grant_chunks = grant_chunks.map_err(|why| {
            verify::mark_passing(format!(
                "the candidate's Info.plist could not be read just then ({why}); the next \
                 attempt reads it again"
            ))
        })?;
        Ok(Self {
            policy,
            grant_chunks,
        })
    }
}

#[cfg(target_os = "macos")]
pub use bundle::{
    HANDOFF_GRANT_CHUNKS_KEY, HANDOFF_GRANT_CHUNKS_TOKEN, plist_declares_grant_chunks,
    plist_marks_dev_build,
};

/// Non-macOS: there is no `.app` bundle, so nothing to pre-verify, refuse or read.
#[cfg(not(target_os = "macos"))]
pub fn preverify_staged_for_handoff(
    _current_build: u64,
    _current_commit: Option<&str>,
    _expected_build: Option<u64>,
    _expected_commit: Option<&str>,
) -> Result<HandoffCandidateFacts, String> {
    Ok(HandoffCandidateFacts {
        policy: aterm_update_core::handoff_policy::PolicyRead::Absent,
        grant_chunks: false,
    })
}

/// Records that a staged build failed to become the running build.
#[cfg(target_os = "macos")]
pub fn record_apply_failure(
    current_build: u64,
    target_build: u64,
    reason: &str,
) -> Option<RecordedApplyFailure> {
    let staging = paths::Staging::resolve()?;
    let ledger = health::Health::record_apply_failure(
        &staging.health(),
        current_build,
        target_build,
        reason,
    );
    status::record(
        &staging,
        current_build,
        &format!("staged build did not apply: {reason}"),
    );
    let persistent = ledger.is_persistent();
    Some(RecordedApplyFailure {
        apply_failures: ledger.apply_failures,
        failures_for_target: ledger.apply_failures_for_target,
        target_build: ledger.last_apply_failure_target_build,
        reason: ledger.last_apply_error,
        persistent,
    })
}

/// Exactly what one [`record_apply_failure`] left in the ledger.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordedApplyFailure {
    /// The escalation streak after this write (`UpdateStatus::failing_applies`).
    pub apply_failures: u32,
    /// Consecutive failures of [`Self::target_build`] after this write.
    pub failures_for_target: u32,
    /// The artifact this failure was about (0 when the caller did not know one).
    pub target_build: u64,
    /// The stored reason, already truncated exactly as the ledger stores it.
    pub reason: String,
    /// Whether the ledger now reads as persistently failing.
    pub persistent: bool,
}

/// Records that an apply was refused (blocked, deferred or held) rather than attempted and failed.
#[cfg(target_os = "macos")]
pub fn record_apply_refusal(current_build: u64, reason: &str) {
    let Some(staging) = paths::Staging::resolve() else {
        return;
    };
    health::Health::record_apply_refusal(&staging.health(), current_build, reason);
    let outcome = match manifest::Ready::read_publishable(&staging) {
        Some(ready) => format!(
            "staged {} (build {}) — NOT applied: {reason}",
            ready.version, ready.build_number
        ),
        None => format!("apply refused: {reason}"),
    };
    status::record(&staging, current_build, &outcome);
}

/// Non-macOS: no apply lane exists, so there is nothing to record.
#[cfg(not(target_os = "macos"))]
pub fn record_apply_failure(
    _current_build: u64,
    _target_build: u64,
    _reason: &str,
) -> Option<RecordedApplyFailure> {
    None
}

/// Non-macOS counterpart to [`record_apply_refusal`].
#[cfg(not(target_os = "macos"))]
pub fn record_apply_refusal(_current_build: u64, _reason: &str) {}

/// What the ledger knows about the seamless handoff's freeze.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HandoffCaptureRecord {
    /// The dry run's capture, in microseconds (0 when only a park was kept).
    pub capture_us: u64,
    /// How many sessions it carried.
    pub sessions: u32,
    /// The first automatic freeze rung it seeded, in milliseconds.
    pub freeze_seed_ms: u64,
    /// The build that was running when it was measured (0 when unknown).
    pub build: u64,
    /// RFC3339 UTC of the measurement.
    pub at: String,
    /// The last landed park's real cost, the readers' stop to a capture ready to hand over, in
    /// microseconds (0 when none was kept).
    pub park_us: u64,
    /// The build that was running when that park landed (0 when unknown).
    pub park_build: u64,
}

#[cfg(target_os = "macos")]
impl HandoffCaptureRecord {
    fn of(ledger: &health::Health) -> Option<Self> {
        (ledger.handoff_capture_us > 0 || ledger.handoff_park_us > 0).then(|| Self {
            capture_us: ledger.handoff_capture_us,
            sessions: ledger.handoff_capture_sessions,
            freeze_seed_ms: ledger.handoff_freeze_seed_ms,
            build: ledger.handoff_capture_build,
            at: ledger.handoff_capture_at.clone(),
            park_us: ledger.handoff_park_us,
            park_build: ledger.handoff_park_build,
        })
    }
}

/// Persists one timed handoff capture in the health ledger.
#[cfg(target_os = "macos")]
pub fn record_handoff_capture(current_build: u64, record: &HandoffCaptureRecord) {
    let Some(staging) = paths::Staging::resolve() else {
        return;
    };
    health::Health::record_handoff_capture(
        &staging.health(),
        current_build,
        record.capture_us,
        record.sessions,
        record.freeze_seed_ms,
    );
}

/// Non-macOS: no seamless handoff runs, so there is nothing to record.
#[cfg(not(target_os = "macos"))]
pub fn record_handoff_capture(_current_build: u64, _record: &HandoffCaptureRecord) {}

/// Persists what one landed seamless park cost.
#[cfg(target_os = "macos")]
pub fn record_handoff_park(current_build: u64, park: std::time::Duration) {
    let Some(staging) = paths::Staging::resolve() else {
        return;
    };
    health::Health::record_handoff_park(
        &staging.health(),
        current_build,
        u64::try_from(park.as_micros()).unwrap_or(u64::MAX),
    );
}

/// Non-macOS counterpart to [`record_handoff_park`].
#[cfg(not(target_os = "macos"))]
pub fn record_handoff_park(_current_build: u64, _park: std::time::Duration) {}

/// What the ledger holds about the handoff's freeze.
#[cfg(target_os = "macos")]
#[must_use]
pub fn handoff_capture_prior() -> Option<HandoffCaptureRecord> {
    let staging = paths::Staging::resolve()?;
    HandoffCaptureRecord::of(&health::Health::read(&staging.health()))
}

/// Non-macOS counterpart to [`handoff_capture_prior`].
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn handoff_capture_prior() -> Option<HandoffCaptureRecord> {
    None
}

/// The apply lane's answer to "a build is staged; why is it not running?".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyLaneReport {
    /// Reason of the most recent apply failure.
    pub last_failure: String,
    /// The build [`Self::last_failure`] was trying to reach (0 when unknown).
    pub last_failure_target_build: u64,
    /// See [`Self::last_failure_target_build`].
    pub failures_for_target: u32,
    /// Reason of the most recent apply refusal.
    pub last_refusal: String,
    /// RFC3339 UTC of [`Self::last_refusal`].
    pub last_refusal_at: String,
    /// The running build's boot trial waiting for a window, in words, with when the wait ends.
    pub trial_wait: String,
    /// The last timed handoff capture, the first freeze rung it seeded and the last landed park.
    pub handoff_capture: Option<HandoffCaptureRecord>,
}

/// Reads the apply lane's durable record as it applies to `current_build`.
#[cfg(target_os = "macos")]
#[must_use]
pub fn apply_lane_report(current_build: u64) -> Option<ApplyLaneReport> {
    let staging = paths::Staging::resolve()?;
    let ledger = health::Health::read(&staging.health());
    let standing = ledger.apply_refusal_applies_to(current_build);
    let handoff_capture = HandoffCaptureRecord::of(&ledger);
    let trial_wait =
        install::window_trial_wait_at(&staging, current_build, install::unix_now_secs())
            .unwrap_or_default();
    Some(ApplyLaneReport {
        trial_wait,
        handoff_capture,
        last_failure: ledger.last_apply_error,
        last_failure_target_build: ledger.last_apply_failure_target_build,
        failures_for_target: ledger.apply_failures_for_target,
        last_refusal: if standing {
            ledger.last_apply_refusal
        } else {
            String::new()
        },
        last_refusal_at: if standing {
            ledger.last_apply_refusal_at
        } else {
            String::new()
        },
    })
}

/// Non-macOS: there is no apply lane, so there is nothing to report.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn apply_lane_report(_current_build: u64) -> Option<ApplyLaneReport> {
    None
}

/// Off macOS: Linux runs its own boot apply (`witness` is not read); elsewhere nothing applies.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn apply_staged_if_ready_preserving_fds_exact(
    _current_build: u64,
    _current_commit: &str,
    _handoff_fds: &[i32],
    _handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    _handoff_target_is_this_build: bool,
    _image: ImageReplacement,
    _witness: TrialWitness,
) -> ApplyOutcome {
    #[cfg(target_os = "linux")]
    {
        linux::boot(_current_build, _current_commit)
    }
    #[cfg(not(target_os = "linux"))]
    {
        ApplyOutcome::NotApplicable
    }
}

/// Confirms the running build reached a healthy checkpoint (window up / first frame).
#[cfg(target_os = "macos")]
#[must_use]
pub fn confirm_boot_health_exact(
    current_build: u64,
    current_commit: &str,
    witness: TrialWitness,
) -> bool {
    install::confirm_boot_health(current_build, Some(current_commit), witness)
}

/// A terminal session of this executable is starting (an interactive launch). On Linux it counts
/// against a replaced executable's trial and returns whether this launch is a start of it; the
/// caller confirms it with [`confirm_linux_session`] once the shell first speaks.
#[must_use]
pub fn linux_session_started(_current_build: u64, _current_commit: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::session_started(_current_build, _current_commit)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// How long a terminal session that counted a start of a replaced Linux executable's trial waits
/// for its shell's first output before deferring the trial's verdict: half of
/// `linux_trial::START_SETTLES_SECS`, leaving the other half for the move to land.
pub const LINUX_SESSION_SILENT_DEFER: std::time::Duration = std::time::Duration::from_secs(10);

/// A session [`linux_session_started`] counted as a pending trial's start has been up for
/// [`LINUX_SESSION_SILENT_DEFER`] with a silent shell: on Linux, moves the trial's stamp forward
/// once, so a slow-starting healthy build is not rolled back. It confirms nothing; `true` when
/// it moved. A try that did not land is retried after `pause(span)`, which answers `false` to
/// stop.
#[must_use]
pub fn defer_linux_session_verdict(
    _current_build: u64,
    _current_commit: &str,
    _pause: impl FnMut(std::time::Duration) -> bool,
) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::defer_session_verdict(_current_build, _current_commit, _pause)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Reads now the path of the executable this process runs, which the Linux lane keeps for the
/// process's life: read first after a sibling replaced the executable, it would name the
/// unlinked file and the process would run no background check. A no-op off Linux.
pub fn linux_capture_executable() {
    #[cfg(target_os = "linux")]
    linux::capture_context();
}

/// A windowed Linux launch has opened its display (its event loop is built): on Linux, counts it
/// against a replaced executable's trial, for a launch whose display the boot apply could not
/// vouch for. `NotApplicable` elsewhere.
#[must_use]
pub fn linux_window_opened(_current_build: u64, _current_commit: &str) -> ApplyOutcome {
    #[cfg(target_os = "linux")]
    {
        linux::boot(_current_build, _current_commit)
    }
    #[cfg(not(target_os = "linux"))]
    {
        ApplyOutcome::NotApplicable
    }
}

/// What a terminal session's confirmation of a replaced Linux executable came to
/// ([`confirm_linux_session`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxSessionConfirm {
    /// Confirmed, or nothing was waiting on this process.
    Done,
    /// Could not be recorded (the update lock was busy, a read failed).
    Failed,
}

/// A terminal session of this executable proved healthy (its shell's first output).
#[must_use]
pub fn confirm_linux_session(_current_build: u64, _current_commit: &str) -> LinuxSessionConfirm {
    #[cfg(target_os = "linux")]
    {
        linux::confirm_session(_current_build, _current_commit)
    }
    #[cfg(not(target_os = "linux"))]
    {
        LinuxSessionConfirm::Done
    }
}

/// Linux confirms its exact inode trial.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn confirm_boot_health_exact(
    _current_build: u64,
    _current_commit: &str,
    _witness: TrialWitness,
) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::confirm(_current_build, _current_commit)
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Sealed installed-bundle identity plus the durable exact-artifact receipt written by the most
/// recent successful swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledUpdateFacts {
    pub build_number: u64,
    pub git_commit: String,
    /// The bundle's marketing version (`CFBundleShortVersionString`), for display only.
    pub version: Option<String>,
    pub receipt_build_number: Option<u64>,
    pub receipt_dmg_sha256: Option<String>,
    /// The installed build is below the operator apply floor (`Floor::min_build`, a yank).
    pub yanked: bool,
}

/// Collect installed provenance and the trial marker.
#[cfg(target_os = "macos")]
pub fn installed_update_facts_checked() -> Result<Option<InstalledUpdateFacts>, String> {
    let Some(installed) = bundle::resolve_layout() else {
        return Ok(None);
    };
    let checks = verify::check_bundle(&installed.app_root, effective_team_id(), true);
    installed_facts_from(checks, paths::Staging::resolve().as_ref()).map(Some)
}

/// The installed bundle's facts from its `checks` and the staging root's records.
#[cfg(target_os = "macos")]
fn installed_facts_from(
    checks: verify::BundleChecks,
    staging: Option<&paths::Staging>,
) -> Result<InstalledUpdateFacts, String> {
    checks.policy?;
    let build_number = checks.build?;
    let git_commit = checks.commit?;
    let version = checks.short_version.and_then(Result::ok);
    let receipt = staging
        .and_then(|staging| manifest::InstalledReceipt::read(&staging.installed_receipt()))
        .filter(|receipt| receipt.matches_sealed(build_number, &git_commit));
    let yanked = installed_build_yanked(staging, build_number)?;
    Ok(InstalledUpdateFacts {
        build_number,
        git_commit,
        version,
        receipt_build_number: receipt.as_ref().map(|receipt| receipt.build_number),
        receipt_dmg_sha256: receipt.map(|receipt| receipt.dmg_sha256),
        yanked,
    })
}

/// Whether the operator floor yanks the installed bundle's `build`.
#[cfg(target_os = "macos")]
fn installed_build_yanked(staging: Option<&paths::Staging>, build: u64) -> Result<bool, String> {
    staging.map_or(Ok(false), |staging| {
        manifest::Floor::read_for_gate(&staging.floor()).map(|floor| build < floor.min_build)
    })
}

/// Whether the checker announces `installed`.
#[cfg(target_os = "macos")]
fn installed_announcement_due(
    staging: Option<&paths::Staging>,
    installed: u64,
    current: u64,
    announced: Option<u64>,
) -> bool {
    installed > current
        && installed_build_yanked(staging, installed) == Ok(false)
        && announced != Some(installed)
}

/// The activation pre-verify.
#[cfg(target_os = "macos")]
pub fn preverify_installed_for_handoff(
    current_build: u64,
    expected_build: u64,
    expected_commit: &str,
) -> Result<HandoffCandidateFacts, String> {
    let installed = bundle::resolve_layout()
        .ok_or_else(|| "no installed bundle at this executable's path".to_string())?;
    preverify_installed_locked(
        paths::Staging::resolve().as_ref(),
        &installed.app_root,
        current_build,
        expected_build,
        expected_commit,
        &HandoffCandidateFacts::read,
    )
    .and_then(std::convert::identity)
}

/// [`preverify_installed_at`] under `staging`'s apply lock.
#[cfg(target_os = "macos")]
fn preverify_installed_locked<R>(
    staging: Option<&paths::Staging>,
    app_root: &std::path::Path,
    current_build: u64,
    expected_build: u64,
    expected_commit: &str,
    read_policy: &dyn Fn(&std::path::Path) -> R,
) -> Result<R, String> {
    let _preverify = install::enter_preverify(staging.map(|staging| staging.apply_lock.as_path()))?;
    let floor = staging
        .map(|staging| manifest::Floor::read_for_gate(&staging.floor()))
        .transpose()?
        .map_or(0, |floor| floor.min_build);
    preverify_installed_at(
        app_root,
        floor,
        current_build,
        expected_build,
        expected_commit,
        read_policy,
    )
}

/// The checks of [`preverify_installed_for_handoff`] against the bundle at `app_root`.
#[cfg(target_os = "macos")]
fn preverify_installed_at<R>(
    app_root: &std::path::Path,
    floor_min_build: u64,
    current_build: u64,
    expected_build: u64,
    expected_commit: &str,
    read_policy: &dyn Fn(&std::path::Path) -> R,
) -> Result<R, String> {
    let (build, commit) = install::verified_bundle_identity_at(app_root)?;
    if build < floor_min_build {
        return Err(format!(
            "installed bundle build {build} is below the operator apply floor \
             {floor_min_build} (yanked); not activating it"
        ));
    }
    if build != expected_build {
        return Err(format!(
            "installed bundle is build {build}, not the authorized build {expected_build}"
        ));
    }
    if !commit_matches(&commit, expected_commit) {
        return Err(format!(
            "installed bundle commit {commit} is not the authorized commit {expected_commit}"
        ));
    }
    if build <= current_build {
        return Err(format!(
            "installed bundle build {build} is not newer than the running build {current_build}"
        ));
    }
    Ok(read_policy(app_root))
}

static UNCOMMITTED_CANDIDATE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Mark this process an uncommitted handoff candidate.
pub fn set_uncommitted_handoff_candidate(uncommitted: bool) {
    UNCOMMITTED_CANDIDATE.store(uncommitted, std::sync::atomic::Ordering::SeqCst);
}

/// Whether this process is a handoff candidate that has not been committed.
#[must_use]
pub fn is_uncommitted_handoff_candidate() -> bool {
    UNCOMMITTED_CANDIDATE.load(std::sync::atomic::Ordering::SeqCst)
}

/// Blocks the calling background thread while this process is an uncommitted handoff candidate.
pub fn wait_while_uncommitted_handoff_candidate(bound: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    let mut waited = false;
    while is_uncommitted_handoff_candidate() && started.elapsed() < bound {
        waited = true;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    waited
}

/// The bound on [`wait_while_uncommitted_handoff_candidate`] for a successor's boot lanes:
/// past its grant-hold budget, so they start only after Commit or once it was told to exit.
pub const UNCOMMITTED_CANDIDATE_HOLD_BOUND: std::time::Duration =
    std::time::Duration::from_secs(10 * 60);

/// Consecutive unconfirmed launches after which a freshly swapped build is auto-reverted.
#[cfg(target_os = "macos")]
pub const MAX_BOOT_ATTEMPTS: u32 = install::MAX_BOOT_ATTEMPTS;

/// Launches the boot sentinel has counted for `build` right now.
#[cfg(target_os = "macos")]
#[must_use]
pub fn trial_launch_count(build: u64) -> u32 {
    install::trial_launch_count(build)
}

/// Non-macOS: no sentinel, nothing counted.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn trial_launch_count(_build: u64) -> u32 {
    0
}

/// A successor of `build` leaving by rule (its parent closed the rendezvous, or it refused the
/// handoff) gives back the boot-trial launch it counted, if not given back yet: it did not fail
/// the build. `true` when a launch was given back, or owed to the next apply-lock holder.
#[cfg(target_os = "macos")]
pub fn forgive_own_trial_launch(build: u64) -> bool {
    install::forgive_own_trial_launch(build)
}

/// Non-macOS: nothing to forgive.
#[cfg(not(target_os = "macos"))]
pub fn forgive_own_trial_launch(_build: u64) -> bool {
    false
}

/// The outgoing process gives back the boot-trial launch of `target_build` its candidate counted.
#[cfg(target_os = "macos")]
pub fn forgive_trial_launch_of(target_build: u64, pid: u32, started: Option<(u64, u64)>) -> bool {
    let started = started.and_then(|(seconds, micros)| {
        Some((i64::try_from(seconds).ok()?, i64::try_from(micros).ok()?))
    });
    install::forgive_trial_launch_of(target_build, pid, started)
}

/// Non-macOS: nothing to forgive.
#[cfg(not(target_os = "macos"))]
pub fn forgive_trial_launch_of(
    _target_build: u64,
    _pid: u32,
    _started: Option<(u64, u64)>,
) -> bool {
    false
}

/// A launched handoff successor names itself before its boot apply can count a trial launch.
#[cfg(target_os = "macos")]
pub fn stamp_trial_launch(stamp: &std::path::Path) -> std::io::Result<()> {
    install::stamp_trial_launch(stamp)
}

/// The launch the stamp at `stamp` names ([`stamp_trial_launch`]).
#[cfg(target_os = "macos")]
#[must_use]
pub fn read_launch_stamp(stamp: &std::path::Path) -> Option<aterm_update_core::LaunchName> {
    install::read_trial_launch_stamp(stamp)
}

/// Gives back the counted launch of `target_build` that the launched successor named in its stamp.
#[cfg(target_os = "macos")]
pub fn forgive_stamped_trial_launch(target_build: u64, stamp: &std::path::Path) -> bool {
    install::forgive_stamped_trial_launch(target_build, stamp)
}

/// Non-macOS: there is no `.app` bundle to activate.
#[cfg(not(target_os = "macos"))]
pub fn preverify_installed_for_handoff(
    _current_build: u64,
    _expected_build: u64,
    _expected_commit: &str,
) -> Result<HandoffCandidateFacts, String> {
    Err("installed-bundle activation is macOS-only".to_string())
}

/// Non-macOS has no installed bundle to prove.
#[cfg(not(target_os = "macos"))]
pub fn installed_update_facts_checked() -> Result<Option<InstalledUpdateFacts>, String> {
    Ok(None)
}

/// What [`supersede_installed_activation`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupersededActivation {
    pub activation_build: u64,
    pub restored_build: u64,
    pub newer_build: u64,
    pub trial_launches: u32,
    /// `Some(why)` when the exchange happened but the activation's boot trial could not be
    /// disarmed; the next boot apply finishes it.
    pub trial_disarm_deferred: Option<String>,
}

/// Why [`supersede_installed_activation`] moved nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SupersedeRefusal {
    /// About the moment: a busy apply lock, a verification that failed or ran out of budget, a
    /// stage or window launch still in flight, or a failed exchange. A later try may pass.
    ForNow(String),
    /// About the disk: it is not the one shape the retire can undo, and only a change to the
    /// disk lets a later try through.
    Settled(String),
}

impl SupersedeRefusal {
    /// The words of the refusal, whatever its kind.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::ForNow(reason) | Self::Settled(reason) => reason,
        }
    }

    /// Whether the refusal is about the moment rather than the disk.
    #[must_use]
    pub const fn is_for_now(&self) -> bool {
        matches!(self, Self::ForNow(_))
    }
}

impl std::fmt::Display for SupersedeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

/// A newer verified stage retires an unconfirmed activation: the running build goes back to the
/// install path and the activation's trial is disarmed without counting a launch, so the staged
/// lane applies `newer_build` with a fresh trial.
///
/// # Errors
/// A [`SupersedeRefusal`] when nothing moved.
#[cfg(target_os = "macos")]
pub fn supersede_installed_activation(
    current_build: u64,
    current_commit: &str,
    activation_build: u64,
    activation_commit: &str,
    newer_build: u64,
) -> Result<SupersededActivation, SupersedeRefusal> {
    install::supersede_installed_activation(
        current_build,
        current_commit,
        (activation_build, activation_commit),
        newer_build,
    )
}

/// Non-macOS: there is no installed `.app` activation to retire.
///
/// # Errors
/// Always [`SupersedeRefusal::Settled`].
#[cfg(not(target_os = "macos"))]
pub fn supersede_installed_activation(
    _current_build: u64,
    _current_commit: &str,
    _activation_build: u64,
    _activation_commit: &str,
    _newer_build: u64,
) -> Result<SupersededActivation, SupersedeRefusal> {
    Err(SupersedeRefusal::Settled(
        "installed-bundle activation is macOS-only".to_string(),
    ))
}

/// A GUI-supplied hook the background check uses to surface self-healing events.
pub type HealthNotify = Box<dyn Fn(String, String) + Send>;

/// The `title` [`HealthNotify`] is called with when a persistent failure this process reported
/// has healed.
pub const HEALTH_RECOVERED_TITLE: &str = "aterm auto-update recovered";

/// A GUI-supplied hook fired when a strictly newer build has just been staged.
pub type StagedNotify = Box<dyn Fn(u64, String) + Send>;

/// A GUI-supplied hook fired after a background check pass whose recorded outcome changed.
pub type CheckedNotify = Box<dyn Fn() + Send>;

/// One process-wide network/staging lane shared by the periodic scheduler and every
/// user-triggered check.
#[cfg(target_os = "macos")]
fn check_lane() -> &'static check_lane::Lane {
    static LANE: check_lane::Lane = check_lane::Lane::new();
    &LANE
}

/// Whether the persistent-failure notice should be spoken for `class`.
#[cfg(target_os = "macos")]
fn persistent_notice_is_new(announced: Option<&str>, class: &str) -> bool {
    announced.map(health_class_canonical) != Some(health_class_canonical(class))
}

/// Whether the loud "auto-update is failing" notice is owed now.
#[cfg(target_os = "macos")]
fn persistent_notice_is_owed(announced: Option<&str>, class: Option<&str>) -> bool {
    class.is_some_and(|class| persistent_notice_is_new(announced, class))
}

/// The `title` [`HealthNotify`] is called with when a persistent failure of `class` is first
/// announced, naming the half of updating that is broken.
#[must_use]
pub fn health_failing_title(class: &str) -> &'static str {
    match health_class_canonical(class) {
        "apply" => "Couldn't install updates",
        "pipeline" => "Couldn't download updates",
        _ => "Couldn't check for updates",
    }
}

/// The class a persistent-failure title stands for, from the title [`health_failing_title`]
/// writes or its older wording; `None` for any other title.
#[must_use]
pub fn health_failing_class(title: &str) -> Option<&'static str> {
    const LEGACY: [(&str, &str); 3] = [
        ("aterm can't install updates", "apply"),
        ("aterm can't download updates", "pipeline"),
        ("aterm can't check for updates", "manifest"),
    ];
    ["apply", "pipeline", "manifest"]
        .into_iter()
        .find(|class| health_failing_title(class) == title)
        .or_else(|| {
            LEGACY
                .iter()
                .find(|(said, _)| *said == title)
                .map(|(_, class)| *class)
        })
}

fn health_class_canonical(class: &str) -> &'static str {
    match class {
        "apply" => "apply",
        "pipeline" | "stage" => "pipeline",
        _ => "manifest",
    }
}

/// The instant a persistent-failure notice's body dates its streak from (the stamp after
/// "since "), or `None`.
#[must_use]
pub fn health_notice_since(body: &str) -> Option<&str> {
    let (_, rest) = body.split_once(" since ")?;
    let stamp = rest.split_whitespace().next()?.trim_end_matches(':');
    let date = stamp.get(..10)?;
    let well_formed = date.char_indices().all(|(i, c)| match i {
        4 | 7 => c == '-',
        _ => c.is_ascii_digit(),
    });
    well_formed.then_some(stamp)
}

/// The `title` [`HealthNotify`] is called with when the class this process already announced
/// is still failing and its count has moved; the body restates the count.
pub const HEALTH_RESTATED_TITLE: &str = "aterm auto-update still failing";

/// The `title` [`HealthNotify`] is called with when this machine cannot read its release
/// channel, and so is not updating at all. It names no ledger class.
pub const STRANDED_TITLE: &str = "aterm is not updating on this machine";

/// The `title` [`HealthNotify`] is called with when an executable the Linux lane installed has
/// waited over an hour for the next aterm a person starts, which finishes the update.
pub const INSTALL_AWAITS_START_TITLE: &str = "Start a new aterm to finish the update";

/// The `title` [`HealthNotify`] is called with when that wait is over, for the window to take
/// its notice down; it is never shown.
pub const INSTALL_WAIT_OVER_TITLE: &str = "aterm update no longer waits for a start";

/// The `title` [`HealthNotify`] is called with when a crash loop ended a Linux install's trial.
pub const INSTALL_CRASH_LOOP_TITLE: &str = "An aterm update didn\u{2019}t start properly";

/// What the check loop tells the GUI about the ledger's health.
#[cfg(target_os = "macos")]
struct HealthAnnouncer {
    /// The clock at thread start.
    started: String,
    /// The class this process announced and the count it spoke.
    announced: Option<(&'static str, u32)>,
    /// The pending build this process already announced as overdue ([`Self::tick_overdue`]).
    overdue_announced: Option<u64>,
}

#[cfg(target_os = "macos")]
impl HealthAnnouncer {
    const fn new(started: String) -> Self {
        Self {
            started,
            announced: None,
            overdue_announced: None,
        }
    }

    fn tick_overdue(
        &mut self,
        h: &health::Health,
        now: &str,
        current_build: u64,
        automatic: bool,
    ) -> Option<(String, String)> {
        if !automatic {
            return None;
        }
        if h.pending_build <= current_build || h.pending_since.is_empty() {
            self.overdue_announced = None;
            return None;
        }
        if self.overdue_announced == Some(h.pending_build) {
            return None;
        }
        let waited = install::rfc3339_delta_secs(&h.pending_since, now)?;
        if waited < PENDING_UPDATE_OVERDUE_SECS {
            return None;
        }
        self.overdue_announced = Some(h.pending_build);
        Some(pending_update_overdue_notice(h, current_build))
    }

    fn tick(
        &mut self,
        h: &health::Health,
        now: &str,
        current_build: u64,
    ) -> Option<(String, String)> {
        let active = h.last_failure_at.as_str() >= self.started.as_str();
        let escalated = h.persistent_class();
        let long_lived = escalated.is_some_and(|(class, _)| {
            install::rfc3339_delta_secs(h.class_since(class), now)
                .is_some_and(|d| d >= PERSISTENT_SPAN.as_secs())
        });
        if let Some((class, count)) = escalated
            && active
            && long_lived
        {
            let announced_class = self.announced.map(|(c, _)| c);
            if persistent_notice_is_owed(announced_class, Some(class)) {
                self.announced = Some((class, count));
                return Some(persistent_failure_notice(class, count, h, current_build));
            }
            if self.announced != Some((class, count)) {
                self.announced = Some((class, count));
                let (_, body) = persistent_failure_notice(class, count, h, current_build);
                return Some((HEALTH_RESTATED_TITLE.to_string(), body));
            }
            return None;
        }
        if !h.is_persistent()
            && let Some((class, _)) = self.announced.take()
        {
            return Some((HEALTH_RECOVERED_TITLE.to_string(), class.to_string()));
        }
        None
    }

    fn tick_for_process(
        &mut self,
        process: &AnnouncedHealth,
        h: &health::Health,
        now: &str,
        current_build: u64,
    ) -> Option<(String, String)> {
        let taken = process.load();
        self.announced = AnnouncedHealth::decode(taken);
        let owed = self.tick(h, now, current_build);
        process.settle(taken, self.announced);
        owed
    }
}

/// The classes `Health::persistent_class` can name, in the order an adopted class outranks
/// another ([`AnnouncedHealth::adopt`]).
const ANNOUNCED_CLASSES: [&str; 4] = ["manifest", "stage", "pipeline", "apply"];

/// What this process has announced about the ledger's health, held for the process rather than
/// one checker thread: a replacement generation, or a successor whose parent announced, must
/// still send the recovery the window's latch waits for. One word, no lock: the class's 1-based
/// index in [`ANNOUNCED_CLASSES`] above bit 32, the count below it, `0` for nothing announced.
pub(crate) struct AnnouncedHealth(std::sync::atomic::AtomicU64);

impl AnnouncedHealth {
    pub(crate) const fn new() -> Self {
        Self(std::sync::atomic::AtomicU64::new(0))
    }

    fn encode(announced: Option<(&str, u32)>) -> u64 {
        announced
            .and_then(|(class, count)| {
                let index = ANNOUNCED_CLASSES.iter().position(|c| *c == class)?;
                Some(((index as u64 + 1) << 32) | u64::from(count))
            })
            .unwrap_or(0)
    }

    #[cfg(target_os = "macos")]
    fn decode(word: u64) -> Option<(&'static str, u32)> {
        let index = usize::try_from(word >> 32).ok()?.checked_sub(1)?;
        let class = ANNOUNCED_CLASSES.get(index)?;
        Some((class, (word & u64::from(u32::MAX)) as u32))
    }

    /// The word as it stands: what an announcer takes up ([`HealthAnnouncer::tick_for_process`]).
    #[cfg(target_os = "macos")]
    fn load(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Leaves `announced`, a tick's verdict over the word `taken`, unless the word moved
    /// meanwhile (an adoption, another generation's verdict); then that one stands.
    #[cfg(target_os = "macos")]
    fn settle(&self, taken: u64, announced: Option<(&str, u32)>) {
        let _ = self.0.compare_exchange(
            taken,
            Self::encode(announced),
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
    }

    /// Takes up `class` as announced by this process's parent, unless a class that outranks it
    /// already is. Its count is unknown here, so a streak still failing is restated, never
    /// announced again.
    pub(crate) fn adopt(&self, class: &str) {
        let adopted = Self::encode(Some((health_class_canonical(class), 0)));
        let _ = self.0.try_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |word| (word >> 32 < adopted >> 32).then_some(adopted),
        );
    }

    /// The class the process stands announced for, if any.
    #[cfg(all(test, target_os = "macos"))]
    fn class(&self) -> Option<&'static str> {
        Self::decode(self.load()).map(|(class, _)| class)
    }
}

static ANNOUNCED_HEALTH: AnnouncedHealth = AnnouncedHealth::new();

/// An update's successor takes up what its parent announced.
pub fn adopt_announced_health(class: &str) {
    ANNOUNCED_HEALTH.adopt(class);
}

/// The loud notice for the class that escalated, as `(title, body)`.
#[cfg(target_os = "macos")]
fn persistent_failure_notice(
    class: &str,
    count: u32,
    h: &health::Health,
    current_build: u64,
) -> (String, String) {
    let stage_reason = if h.last_diagnosis_error.is_empty() {
        h.last_error.as_str()
    } else {
        h.last_diagnosis_error.as_str()
    };
    let cause = match class {
        "manifest" if github::is_stale_anchor_refusal(&h.last_error) => format!(
            "{} — this machine stays on build {current_build} until it is reinstalled",
            h.last_error
        ),
        "manifest" => format!(
            "the newest release cannot be trusted ({}) — this machine stays on build \
             {current_build} until it is republished",
            h.last_error
        ),
        "stage" if is_passing_refusal(stage_reason) => {
            let step = passing_step(stage_reason);
            if step.contains("free some space") {
                format!("the update could not be staged on this Mac: {step}")
            } else {
                format!(
                    "updates download, but staging them could not finish on this Mac: \
                     {step}; aterm retries it by itself, less often while this lasts"
                )
            }
        }
        "stage" => format!("updates download but will not verify or install ({stage_reason})"),
        "apply" => "an update is downloaded and verified but didn\u{2019}t install".to_string(),
        "pipeline" if github::is_stopped_stage_holder_refusal(&h.last_error) => h
            .last_error
            .strip_prefix("stage lock: ")
            .unwrap_or(&h.last_error)
            .to_string(),
        "pipeline" if is_passing_refusal(stage_reason) => format!(
            "update checks could not finish on this Mac: {}",
            passing_step(stage_reason)
        ),
        _ => "release manifests exist but cannot be downloaded — this build's update \
              pipeline is likely broken"
            .to_string(),
    };
    let counted = if class == "apply" {
        "failed attempts to install"
    } else {
        "failed checks"
    };
    (
        health_failing_title(class).to_string(),
        format!(
            "{count} {counted} in a row since {}: {cause}.",
            h.class_since(class)
        ),
    )
}

/// The step a passing stage refusal names.
#[cfg(target_os = "macos")]
fn passing_step(reason: &str) -> String {
    let key = format!(" ({PASSING_REFUSAL_KEY})");
    let step = match reason.strip_suffix(&format!(" \u{2026}{key}")) {
        Some(head) => format!("{head}\u{2026}"),
        None => reason.strip_suffix(&key).unwrap_or(reason).to_string(),
    };
    step.strip_prefix("staged bundle failed verification: ")
        .unwrap_or(&step)
        .replace(&key, "")
        .replace("; treating as a rejection", "")
        .replace("; treating as a failure", "")
}

/// The loud notice for a newer build that has waited on this machine.
#[cfg(target_os = "macos")]
fn pending_update_overdue_notice(h: &health::Health, current_build: u64) -> (String, String) {
    let failed = (h.last_apply_failure_target_build == h.pending_build
        && h.apply_failures_for_target > 0)
        .then(|| format!("installing it failed {}×", h.apply_failures_for_target));
    let standing = h
        .apply_refusal_applies_to(current_build)
        .then(|| h.last_apply_refusal.clone());
    let cause = match (failed, standing) {
        (Some(failed), Some(standing)) => format!("{failed}; {standing}"),
        (Some(failed), None) => failed,
        (None, Some(standing)) => format!("it is held back: {standing}"),
        (None, None) => "no apply attempt has been recorded for it".to_string(),
    };
    (
        health_failing_title("apply").to_string(),
        format!(
            "build {} has been waiting on this machine since {} and build {current_build} is \
             still running: {cause}.",
            h.pending_build, h.pending_since
        ),
    )
}

/// The pending-update clock brought up to date for what this cycle observed.
#[cfg(target_os = "macos")]
fn note_pending_update(
    staging: &paths::Staging,
    current_build: u64,
    staged: Option<u64>,
    installed: Option<u64>,
) -> health::Health {
    let floor = manifest::Floor::read(&staging.floor()).min_build;
    let observed = [staged, installed]
        .into_iter()
        .flatten()
        .filter(|build| *build >= floor)
        .max();
    let h = health::Health::note_pending_update(&staging.health(), current_build, observed);
    if observed.is_some_and(|build| build > current_build) || h.pending_build <= current_build {
        return h;
    }
    let withdrawn = manifest::Quarantine::read(&staging.quarantine()).holds_build(h.pending_build)
        || manifest::FailedMark::read(&staging.failed())
            .is_some_and(|mark| mark.is_quarantine() && mark.build_number == h.pending_build)
        || h.pending_build < floor;
    if withdrawn {
        health::Health::forget_pending_update(&staging.health(), h.pending_build)
    } else {
        h
    }
}

#[cfg(all(test, target_os = "macos"))]
mod persistent_notice_tests {

    #[test]
    fn the_notice_body_hands_back_the_date_it_was_written_with() {
        for (class, count) in [
            ("apply", 3),
            ("pipeline", 20),
            ("stage", 4),
            ("manifest", 5),
        ] {
            let ledger = super::health::Health {
                apply_since: "2026-09-14T05:42:50Z".to_string(),
                pipeline_since: "2026-09-14T05:42:50Z".to_string(),
                stage_since: "2026-09-14T05:42:50Z".to_string(),
                manifest_since: "2026-09-14T05:42:50Z".to_string(),
                last_error: "signature did not verify".to_string(),
                last_apply_error: "the successor died".to_string(),
                ..super::health::Health::default()
            };
            let (title, body) = super::persistent_failure_notice(class, count, &ledger, 7);
            assert_eq!(title, super::health_failing_title(class));
            assert_eq!(
                super::health_notice_since(&body),
                Some("2026-09-14T05:42:50Z"),
                "{class}: {body}"
            );
        }
        assert_eq!(super::health_notice_since("no date here"), None);
        assert_eq!(
            super::health_notice_since("broken since yesterday: it failed"),
            None
        );
    }

    #[test]
    fn a_failing_title_reads_back_to_its_class_old_words_and_new() {
        for class in ["apply", "pipeline", "stage", "manifest", "future"] {
            let title = super::health_failing_title(class);
            assert!(title.starts_with("Couldn't "), "{title}");
            let read = super::health_failing_class(title).expect("today's title");
            assert_eq!(super::health_failing_title(read), title, "{class}");
        }
        for (old, class) in [
            ("aterm can't install updates", "apply"),
            ("aterm can't download updates", "pipeline"),
            ("aterm can't check for updates", "manifest"),
        ] {
            assert_eq!(super::health_failing_class(old), Some(class), "{old}");
        }
        for other in [
            super::HEALTH_RESTATED_TITLE,
            super::HEALTH_RECOVERED_TITLE,
            super::STRANDED_TITLE,
            "aterm's update check stopped",
            "Couldn't install update",
            "",
        ] {
            assert_eq!(super::health_failing_class(other), None, "{other:?}");
        }
    }

    #[test]
    fn an_overdue_pending_build_is_announced_once() {
        use super::health::Health;
        let mut announcer = super::HealthAnnouncer::new("2026-09-22T00:00:00Z".to_string());
        let stranded = Health {
            apply_failures: 2,
            apply_since: "2026-09-22T00:11:30Z".to_string(),
            last_failure_at: "2026-09-22T00:11:31Z".to_string(),
            kind: "apply".to_string(),
            last_apply_error: "overlap handoff failed safely: handoff proof ended \
                               AdoptionMismatch"
                .to_string(),
            last_apply_failure_build: 1790019739,
            last_apply_failure_target_build: 1790120495,
            apply_failures_for_target: 2,
            last_apply_refusal: "automatic apply of build 1790120495 is out of retries".to_string(),
            last_apply_refusal_at: "2026-09-22T00:11:32Z".to_string(),
            last_apply_refusal_build: 1790019739,
            pending_build: 1790120495,
            pending_since: "2026-09-22T00:10:45Z".to_string(),
            ..Health::default()
        };
        assert!(stranded.persistent_class().is_none());
        assert!(
            announcer
                .tick_overdue(&stranded, "2026-09-25T00:00:00Z", 1790019739, false)
                .is_none(),
            "auto_apply = false is not a failure"
        );
        assert!(
            announcer
                .tick_overdue(&stranded, "2026-09-22T01:10:00Z", 1790019739, true)
                .is_none(),
            "fifty-nine minutes is not overdue"
        );
        let (title, _) = announcer
            .tick_overdue(&stranded, "2026-09-22T01:11:00Z", 1790019739, true)
            .expect("past the hour the stranded machine is announced");
        assert_eq!(
            title,
            super::health_failing_title("apply"),
            "an update that will not land is the install half failing"
        );
        assert!(
            announcer
                .tick_overdue(&stranded, "2026-09-22T09:00:00Z", 1790019739, true)
                .is_none(),
            "once per build: the row stands, the notice does not repeat"
        );
        assert!(
            announcer
                .tick_overdue(&stranded, "2026-09-23T00:00:00Z", 1790120495, true)
                .is_none()
        );
        let unattempted = Health {
            pending_build: 1790300000,
            pending_since: "2026-09-23T00:00:00Z".to_string(),
            ..Health::default()
        };
        assert!(
            announcer
                .tick_overdue(&unattempted, "2026-09-23T02:00:00Z", 1790120495, true)
                .is_some()
        );
    }

    #[test]
    fn a_withdrawn_build_is_never_announced_as_waiting() {
        use super::health::Health;
        let started = "2026-09-22T00:00:00Z".to_string();
        let later = "2026-09-22T05:00:00Z";
        let backdate = |s: &super::paths::Staging| {
            let mut h = Health::read(&s.health());
            h.pending_since = "2026-09-22T01:00:00Z".to_string();
            std::fs::write(s.health(), aterm_toml::to_string(&h).unwrap()).unwrap();
        };
        for withdrawal in ["none", "quarantine", "yank", "revocation"] {
            let s = super::paths::Staging::scratch("pending-withdrawn");
            super::note_pending_update(&s, 10, Some(11), None);
            backdate(&s);
            match withdrawal {
                "quarantine" => super::manifest::quarantine_artifact(
                    &s,
                    11,
                    &"ab".repeat(32),
                    aterm_durable::Flush::Durable,
                ),
                "yank" => super::manifest::Floor::bump_and_write(&s.floor(), 12, 0, 0),
                "revocation" => super::github::retire_revoked_stage(&s, 11),
                _ => {}
            }
            let h = super::note_pending_update(&s, 10, None, None);
            let said =
                super::HealthAnnouncer::new(started.clone()).tick_overdue(&h, later, 10, true);
            if withdrawal == "none" {
                assert!(said.is_some(), "a stranded machine must still be told");
            } else {
                assert!(
                    said.is_none(),
                    "{withdrawal}: build 11 is withdrawn, not waiting: {said:?}"
                );
                assert_eq!(h.pending_build, 0, "{withdrawal}");
            }
            let _ = std::fs::remove_dir_all(&s.root);
        }
    }

    #[test]
    fn a_yanked_installed_bundle_is_never_announced_as_waiting() {
        use super::health::Health;
        let started = "2026-09-22T00:00:00Z".to_string();
        let later = "2026-09-22T05:00:00Z";
        let backdate = |s: &super::paths::Staging| {
            let mut h = Health::read(&s.health());
            h.pending_since = "2026-09-22T01:00:00Z".to_string();
            std::fs::write(s.health(), aterm_toml::to_string(&h).unwrap()).unwrap();
        };
        for yanked in [false, true] {
            let s = super::paths::Staging::scratch("pending-installed-yanked");
            super::note_pending_update(&s, 10, None, Some(11));
            backdate(&s);
            if yanked {
                super::manifest::Floor::bump_and_write(&s.floor(), 12, 0, 0);
            }
            let h = super::note_pending_update(&s, 10, None, Some(11));
            let said =
                super::HealthAnnouncer::new(started.clone()).tick_overdue(&h, later, 10, true);
            if yanked {
                assert_eq!(h.pending_build, 0, "a yanked build is never pending");
                assert!(said.is_none(), "build 11 is yanked, not waiting: {said:?}");
            } else {
                assert_eq!(h.pending_build, 11);
                assert!(said.is_some(), "a stranded machine must still be told");
            }
            let h = super::note_pending_update(&s, 10, Some(12), Some(11));
            assert_eq!(h.pending_build, 12, "yanked: {yanked}");
            assert!(
                super::HealthAnnouncer::new(started.clone())
                    .tick_overdue(&h, &super::install::now_rfc3339(), 10, true)
                    .is_none(),
                "the successor's clock starts now"
            );
        }
    }

    #[test]
    fn the_announcer_restates_a_moved_count_quietly_and_re_announces_only_a_new_class() {
        use super::health::Health;
        let started = "2026-09-13T22:00:00Z".to_string();
        let mut announcer = super::HealthAnnouncer::new(started);
        let ledger = |apply_failures: u32, last: &str| Health {
            apply_failures,
            apply_since: "2026-09-13T22:40:00Z".to_string(),
            last_failure_at: last.to_string(),
            kind: "apply".to_string(),
            last_apply_error: "the successor died".to_string(),
            ..Health::default()
        };
        assert!(
            announcer
                .tick(
                    &ledger(3, "2026-09-13T22:50:00Z"),
                    "2026-09-13T23:00:00Z",
                    7
                )
                .is_none()
        );
        let (title, _) = announcer
            .tick(
                &ledger(3, "2026-09-13T23:12:50Z"),
                "2026-09-13T23:13:44Z",
                7,
            )
            .expect("the first escalation is announced");
        assert_eq!(title, super::health_failing_title("apply"));
        assert!(
            announcer
                .tick(
                    &ledger(3, "2026-09-13T23:12:50Z"),
                    "2026-09-13T23:37:45Z",
                    7
                )
                .is_none()
        );
        let (title, body) = announcer
            .tick(
                &ledger(6, "2026-09-14T00:10:00Z"),
                "2026-09-14T00:12:34Z",
                7,
            )
            .expect("a moved count is restated");
        assert_eq!(title, super::HEALTH_RESTATED_TITLE);
        assert!(body.starts_with('6'), "the new number: {body}");
        let mut fresh = super::HealthAnnouncer::new("2026-09-14T01:00:00Z".to_string());
        assert!(
            fresh
                .tick(
                    &ledger(6, "2026-09-14T00:10:00Z"),
                    "2026-09-14T01:05:00Z",
                    7
                )
                .is_none()
        );
        let healed = Health::default();
        let (title, body) = announcer
            .tick(&healed, "2026-09-14T02:00:00Z", 7)
            .expect("a failure this process reported gets its recovery reported");
        assert_eq!(title, super::HEALTH_RECOVERED_TITLE);
        assert_eq!(body, "apply", "the heal names the class it proves");
        assert!(announcer.tick(&healed, "2026-09-14T02:01:00Z", 7).is_none());
        let (title, _) = announcer
            .tick(
                &ledger(3, "2026-09-14T03:30:00Z"),
                "2026-09-14T03:31:00Z",
                7,
            )
            .expect("a new episode is news");
        assert_eq!(title, super::health_failing_title("apply"));
    }

    #[test]
    fn a_quick_streak_raises_no_notice_and_a_spanning_one_does() {
        let started = "2026-10-15T09:00:00Z".to_string();
        let streak = |newest: &str| super::health::Health {
            pipeline_failures: 3,
            pipeline_since: "2026-10-15T10:00:00Z".to_string(),
            last_diagnosis_at: newest.to_string(),
            last_diagnosis_error: "release manifests exist but could not be fetched".to_string(),
            network_failures: 1,
            network_since: "2026-10-15T18:00:00Z".to_string(),
            last_failure_at: "2026-10-15T18:00:00Z".to_string(),
            kind: "network".to_string(),
            last_error: "curl: (6) Could not resolve host: github.com".to_string(),
            ..super::health::Health::default()
        };
        let woke = "2026-10-15T18:00:01Z";
        let mut announcer = super::HealthAnnouncer::new(started.clone());
        assert_eq!(
            announcer.tick(&streak("2026-10-15T10:06:00Z"), woke, 7),
            None,
            "three failures inside six minutes"
        );
        let mut announcer = super::HealthAnnouncer::new(started);
        let (title, _) = announcer
            .tick(&streak("2026-10-15T10:30:00Z"), woke, 7)
            .expect("three failures spanning half an hour are news");
        assert_eq!(title, super::health_failing_title("pipeline"));
    }

    fn failing_checks(last: &str) -> super::health::Health {
        super::health::Health {
            manifest_failures: 20,
            manifest_since: "2026-09-14T22:04:36Z".to_string(),
            last_failure_at: last.to_string(),
            last_diagnosis_at: last.to_string(),
            kind: "manifest".to_string(),
            last_error: "the channel's manifest would not verify".to_string(),
            ..super::health::Health::default()
        }
    }

    #[test]
    fn a_successor_sends_the_recovery_of_the_class_its_parent_announced() {
        let started = "2026-09-29T10:00:00Z".to_string();
        let process = super::AnnouncedHealth::new();
        process.adopt("manifest");
        let mut announcer = super::HealthAnnouncer::new(started.clone());
        let (title, _) = announcer
            .tick_for_process(
                &process,
                &failing_checks("2026-09-29T10:02:00Z"),
                "2026-09-29T10:02:01Z",
                7,
            )
            .expect("the adopted streak's count is restated");
        assert_eq!(
            title,
            super::HEALTH_RESTATED_TITLE,
            "NEGATIVE: the parent's class is not announced a second time"
        );
        assert_eq!(process.class(), Some("manifest"));

        let process = super::AnnouncedHealth::new();
        let mut announcer = super::HealthAnnouncer::new(started.clone());
        assert!(
            announcer
                .tick_for_process(
                    &process,
                    &super::health::Health::default(),
                    "2026-09-29T10:02:01Z",
                    7
                )
                .is_none()
        );

        let process = super::AnnouncedHealth::new();
        process.adopt("manifest");
        let mut announcer = super::HealthAnnouncer::new(started.clone());
        let healed = super::health::Health::default();
        let (title, body) = announcer
            .tick_for_process(&process, &healed, "2026-09-29T10:02:01Z", 7)
            .expect("the adopted class's healing is sent");
        assert_eq!(title, super::HEALTH_RECOVERED_TITLE);
        assert_eq!(body, "manifest", "naming the class it proves");
        assert_eq!(process.class(), None, "said once");
        assert!(
            announcer
                .tick_for_process(&process, &healed, "2026-09-29T10:03:00Z", 7)
                .is_none()
        );
        let mut later = failing_checks("2026-09-29T15:40:00Z");
        later.manifest_since = "2026-09-29T15:00:00Z".to_string();
        let (title, _) = announcer
            .tick_for_process(&process, &later, "2026-09-29T15:40:01Z", 7)
            .expect("a new episode is announced");
        assert_eq!(title, super::health_failing_title("manifest"));
    }

    #[test]
    fn a_replacement_generation_sends_the_recovery_its_predecessor_owed() {
        let started = "2026-09-29T10:00:00Z".to_string();
        let process = super::AnnouncedHealth::new();
        let mut stalled = super::HealthAnnouncer::new(started.clone());
        let (title, _) = stalled
            .tick_for_process(
                &process,
                &failing_checks("2026-09-29T10:40:00Z"),
                "2026-09-29T10:40:01Z",
                7,
            )
            .expect("announced");
        assert_eq!(title, super::health_failing_title("manifest"));
        let healed = super::health::Health::default();
        let mut replacement = super::HealthAnnouncer::new("2026-09-29T11:00:00Z".to_string());
        assert!(
            replacement
                .tick(&healed, "2026-09-29T11:05:00Z", 7)
                .is_none(),
            "NEGATIVE: a fresh announcer alone owes nothing"
        );
        let mut replacement = super::HealthAnnouncer::new("2026-09-29T11:00:00Z".to_string());
        let (title, body) = replacement
            .tick_for_process(&process, &healed, "2026-09-29T11:05:00Z", 7)
            .expect("the replacement heals what the process announced");
        assert_eq!(title, super::HEALTH_RECOVERED_TITLE);
        assert_eq!(body, "manifest");
    }

    #[test]
    fn an_adopted_class_keeps_the_one_whose_recovery_answers_most() {
        let process = super::AnnouncedHealth::new();
        process.adopt("manifest");
        process.adopt("pipeline");
        assert_eq!(process.class(), Some("pipeline"));
        process.adopt("manifest");
        assert_eq!(process.class(), Some("pipeline"));
        let process = super::AnnouncedHealth::new();
        process.adopt("stage");
        assert_eq!(process.class(), Some("pipeline"), "one title, one class");
        process.adopt("future");
        assert_eq!(process.class(), Some("pipeline"));
    }

    #[test]
    fn a_taken_up_download_warning_is_not_said_again_for_its_sibling_class() {
        let started = "2026-09-29T10:00:00Z".to_string();
        let stage = super::health::Health {
            stage_failures: 4,
            stage_since: "2026-09-29T09:00:00Z".to_string(),
            last_failure_at: "2026-09-29T10:02:00Z".to_string(),
            last_diagnosis_at: "2026-09-29T10:02:00Z".to_string(),
            kind: "stage".to_string(),
            last_error: "the DMG would not verify".to_string(),
            ..super::health::Health::default()
        };
        let process = super::AnnouncedHealth::new();
        process.adopt("pipeline");
        let mut announcer = super::HealthAnnouncer::new(started.clone());
        let (title, _) = announcer
            .tick_for_process(&process, &stage, "2026-09-29T10:02:01Z", 7)
            .expect("the count is restated");
        assert_eq!(title, super::HEALTH_RESTATED_TITLE);
        let process = super::AnnouncedHealth::new();
        let mut announcer = super::HealthAnnouncer::new(started);
        let (title, _) = announcer
            .tick_for_process(&process, &stage, "2026-09-29T10:02:01Z", 7)
            .expect("announced");
        assert_eq!(title, super::health_failing_title("stage"));
    }
}

/// Supplies the current configured channel at the start of each background cycle.
#[cfg(not(target_os = "linux"))]
pub type SourceProvider = std::sync::Arc<dyn Fn() -> Option<Source> + Send + Sync>;

/// One request-local configuration observation.
#[derive(Clone, Debug)]
pub struct CheckSettings {
    pub source: Source,
    pub auto_apply: bool,
}

pub type CheckSettingsProvider = std::sync::Arc<dyn Fn() -> Option<CheckSettings> + Send + Sync>;

pub fn check_now_with_settings(
    current_build: u64,
    provider: &CheckSettingsProvider,
) -> UpdateStatus {
    #[cfg(target_os = "linux")]
    {
        linux::check_with_settings(current_build, provider, true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        match provider() {
            Some(settings) => check_now(current_build, &settings.source),
            None => {
                let outcome = "current update settings could not be read".to_string();
                status(current_build).map_or_else(
                    || UpdateStatus::empty(enabled(), current_build, outcome.clone()),
                    |mut durable| {
                        durable.outcome = outcome.clone();
                        durable
                    },
                )
            }
        }
    }
}

pub fn spawn_background_check_with_settings(
    current_build: u64,
    provider: CheckSettingsProvider,
    notify: Option<HealthNotify>,
    on_staged: Option<StagedNotify>,
    on_checked: Option<CheckedNotify>,
) {
    #[cfg(target_os = "linux")]
    {
        let _ = on_staged;
        linux::spawn_background_check_with_settings(current_build, provider, notify, on_checked);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = on_checked;
        spawn_background_check_with_source(
            current_build,
            std::sync::Arc::new(move || provider().map(|settings| settings.source)),
            notify,
            on_staged,
        );
    }
}

/// Starts a replaced Linux executable gets to confirm itself ([`LinuxUpdateStatus::trial_starts`]).
pub const LINUX_TRIAL_LAUNCHES: u32 = 3;

/// Linux disk-stage/trial facts are not a macOS DMG or a live-session handoff.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinuxUpdateStatus {
    pub installed_build: u64,
    /// The installed executable's own release version.
    pub installed_version: String,
    pub staged_build: Option<u64>,
    pub staged_version: Option<String>,
    pub staged_commit: Option<String>,
    pub trial_phase: Option<String>,
    pub trial_starts: u32,
    pub trial_healthy: bool,
    /// This copy rolled back a build newer than the one it runs, and won't install it again.
    pub refused_newer: bool,
}

/// Starts the process's one background checker, sampling its source before each cycle so a
/// configuration reload takes effect without a restart. Each cycle runs under `catch_unwind`,
/// takes `checker.lock` within a bound, and stamps [`checker_watch::WATCH`], so the window's
/// watchdog can start a replacement once the stamp goes stale ([`respawn_stalled_checker`]).
#[cfg(target_os = "macos")]
pub fn spawn_background_check_with_source(
    current_build: u64,
    source_provider: SourceProvider,
    notify: Option<HealthNotify>,
    on_staged: Option<StagedNotify>,
) {
    spawn_background_check_in(
        automatic(),
        &UNCHECKED_SWEEP,
        current_build,
        source_provider,
        notify,
        on_staged,
    );
}

/// [`spawn_background_check_with_source`] with automatic checks on or off (`automatic`); while
/// they are off, `sweep` stands in for the scheduled checks' sweep of `staged/`.
#[cfg(target_os = "macos")]
fn spawn_background_check_in(
    automatic: bool,
    sweep: &UncheckedSweep,
    current_build: u64,
    source_provider: SourceProvider,
    notify: Option<HealthNotify>,
    on_staged: Option<StagedNotify>,
) {
    if !automatic {
        // No scheduled check sweeps here, and a bundle an earlier stage or another process's
        // asked check left must still go.
        sweep.start_resident();
        // The one record of why this machine is not checking by itself; a typed or clicked
        // check still runs.
        if enabled() {
            log(
                "automatic update checks are off — [update] enabled = false (Settings ▸ \
                 Software Update); Check for Updates still checks when asked",
            );
        }
        return;
    }
    if bundle::resolve().is_none() {
        return;
    }
    let args = std::sync::Arc::new(CheckerArgs {
        current_build,
        source_provider,
        health_hook: std::sync::Mutex::new(notify),
        staged_hook: std::sync::Mutex::new(on_staged),
        settings_retry: SETTINGS_RETRY,
        staging: std::sync::Arc::new(paths::Staging::resolve),
        checker_lock_wait: checker_watch::CHECKER_LOCK_WAIT,
        checker_ungated_after: checker_watch::CHECKER_UNGATED_AFTER,
        lane: check_lane(),
        gate: std::sync::Arc::new(checker_gate),
        check: std::sync::Arc::new(github::check_and_stage),
        pause: None,
        wake: &cadence::SYSTEM_WOKE,
        wake_settle: cadence::WAKE_SETTLE,
    });
    let generation = checker_watch::WATCH.register(
        cadence::Cadence::new(std::time::Duration::from_secs(cadence::INTERVAL_SECS))
            .max_wait()
            .as_secs(),
    );
    let _ = CHECKER_ARGS.set(std::sync::Arc::clone(&args));
    if let Err(error) = spawn_checker_thread(generation, args) {
        warn(&format!("could not start the update checker: {error}"));
    }
}

/// This process's sweep of the installed copy's `staged/` while automatic checks are off.
#[cfg(target_os = "macos")]
static UNCHECKED_SWEEP: std::sync::LazyLock<UncheckedSweep> = std::sync::LazyLock::new(|| {
    UncheckedSweep::new(
        || bundle::resolve().and_then(|_| paths::Staging::resolve()),
        std::thread::sleep,
    )
});

/// The sweep of `staged/` where no scheduled check runs: what earlier stages left goes as each
/// exec stamp is spent ([`install::sweep_left_incoming_until_none_held`]). One thread runs at
/// a time, and a start while it runs sends it round once more. The launch's thread stays for
/// the life of the process; an asked check's ends once nothing is held.
#[cfg(target_os = "macos")]
#[derive(Clone)]
struct UncheckedSweep {
    state: std::sync::Arc<std::sync::Mutex<UncheckedSweepState>>,
    staging: std::sync::Arc<dyn Fn() -> Option<paths::Staging> + Send + Sync>,
    sleep: std::sync::Arc<dyn Fn(std::time::Duration) + Send + Sync>,
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct UncheckedSweepState {
    running: bool,
    again: bool,
    /// Once nothing is held the thread lists `staged/` again every `FIRST_EXEC_HANG`.
    resident: bool,
}

#[cfg(target_os = "macos")]
impl UncheckedSweep {
    fn new(
        staging: impl Fn() -> Option<paths::Staging> + Send + Sync + 'static,
        sleep: impl Fn(std::time::Duration) + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: std::sync::Arc::default(),
            staging: std::sync::Arc::new(staging),
            sleep: std::sync::Arc::new(sleep),
        }
    }

    /// Starts the sweep off the caller's thread, or has the running one go round again; it
    /// ends once nothing is held.
    fn start(&self) {
        self.start_staying(false);
    }

    /// [`Self::start`] for the process's life, so a bundle another process's asked check left
    /// goes once its hold is spent; it ends when there is no `staged/` to sweep.
    fn start_resident(&self) {
        self.start_staying(true);
    }

    fn start_staying(&self, resident: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.resident |= resident;
        if state.running {
            state.again = true;
            return;
        }
        let sweep = self.clone();
        match std::thread::Builder::new()
            .name("aterm-update-sweep".into())
            .spawn(move || sweep.run())
        {
            Ok(_) => state.running = true,
            Err(error) => warn(&format!("could not start the update sweep: {error}")),
        }
    }

    fn run(&self) {
        loop {
            let staging = (self.staging)();
            if let Some(staging) = &staging {
                install::sweep_left_incoming_until_none_held(staging, &*self.sleep);
            }
            // Under the lock a start reads, so one that came after the last sweep is not lost.
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if std::mem::take(&mut state.again) {
                continue;
            }
            if staging.is_none() || !state.resident {
                state.running = false;
                return;
            }
            drop(state);
            (self.sleep)(aterm_uds::deadline::FIRST_EXEC_HANG);
        }
    }
}

/// Runs a check someone asked for. With automatic checks off no scheduled check follows it, so
/// `sweep` takes what it left; a CLI's sweep dies with it, and a running app's or the next
/// launch's takes the rest.
#[cfg(target_os = "macos")]
fn asked_check(automatic: bool, sweep: &UncheckedSweep, check: impl FnOnce()) {
    check();
    if !automatic {
        sweep.start();
    }
}

/// Tell the background check that the system just woke.
#[cfg(target_os = "macos")]
pub fn note_system_woke() {
    cadence::SYSTEM_WOKE.post();
}

/// How long the check loop pauses after the host did not answer its settings query.
#[cfg(target_os = "macos")]
const SETTINGS_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// Every how many consecutive settings misses the loop says so again.
#[cfg(target_os = "macos")]
const SETTINGS_MISS_LOG_EVERY: u64 = 60;

/// Every how many consecutive `checker.lock` deferrals the loop says so again.
#[cfg(target_os = "macos")]
const CHECKER_DEFERRAL_LOG_EVERY: u64 = 3600 / cadence::INTERVAL_SECS;

/// Every how many consecutive cycles that found this process's own check lane busy the loop
/// says so again.
#[cfg(target_os = "macos")]
const LANE_BUSY_LOG_EVERY: u64 = 6;

/// What the check loop runs on, shared by the thread and any replacement for it.
#[cfg(target_os = "macos")]
struct CheckerArgs {
    current_build: u64,
    source_provider: SourceProvider,
    health_hook: std::sync::Mutex<Option<HealthNotify>>,
    staged_hook: std::sync::Mutex<Option<StagedNotify>>,
    /// The pause after an unanswered settings query ([`SETTINGS_RETRY`]; a test passes zero).
    settings_retry: std::time::Duration,
    /// Where the loop's own reads of the staging root go.
    staging: std::sync::Arc<dyn Fn() -> Option<paths::Staging> + Send + Sync>,
    /// How long a cycle waits for `checker.lock` before it defers.
    checker_lock_wait: std::time::Duration,
    /// After how many consecutive deferrals to a holder showing no progress the loop checks
    /// without the lock.
    checker_ungated_after: u64,
    /// The check lane a cycle takes.
    lane: &'static check_lane::Lane,
    /// How a cycle asks for `checker.lock`, with [`Self::checker_lock_wait`].
    gate: std::sync::Arc<GateFn>,
    /// The network check and stage.
    check: std::sync::Arc<CheckFn>,
    /// Every wait between cycles.
    pause: Option<std::time::Duration>,
    /// The wake signal every wait watches.
    wake: &'static cadence::WakeSignal,
    /// How long the network gets after a wake before the next check.
    wake_settle: std::time::Duration,
}

#[cfg(target_os = "macos")]
impl CheckerArgs {
    fn has_health_hook(&self) -> bool {
        self.health_hook.lock().is_ok_and(|hook| hook.is_some())
    }

    fn send_health(&self, title: String, body: String) {
        if let Ok(hook) = self.health_hook.lock()
            && let Some(cb) = hook.as_ref()
        {
            cb(title, body);
        }
    }

    fn has_staged_hook(&self) -> bool {
        self.staged_hook.lock().is_ok_and(|hook| hook.is_some())
    }

    fn send_staged(&self, build: u64, version: String) {
        if let Ok(hook) = self.staged_hook.lock()
            && let Some(cb) = hook.as_ref()
        {
            cb(build, version);
        }
    }
}

/// One network check and stage for `(current build, channel)`.
#[cfg(target_os = "macos")]
type CheckFn = dyn Fn(u64, &Source) -> Result<Option<String>, String> + Send + Sync;

/// One cycle's ask for the machine-wide checker lock at a path, waiting at most the given bound.
#[cfg(target_os = "macos")]
type GateFn = dyn Fn(&std::path::Path, std::time::Duration) -> CheckerGate + Send + Sync;

/// The arguments the process's checker was started with.
#[cfg(target_os = "macos")]
static CHECKER_ARGS: std::sync::OnceLock<std::sync::Arc<CheckerArgs>> = std::sync::OnceLock::new();

/// Start one checker thread under `generation`.
#[cfg(target_os = "macos")]
fn spawn_checker_thread(generation: u64, args: std::sync::Arc<CheckerArgs>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-update".into())
        .spawn(move || {
            let CheckerExit::Superseded { phase } =
                run_checker(&checker_watch::WATCH, generation, &args);
            warn(&format!(
                "a stuck update check woke while {} after aterm replaced it; it stops without \
                 checking",
                phase.words()
            ));
        })
        .map(drop)
}

/// Replace a stalled checker (the window's watchdog).
#[cfg(target_os = "macos")]
pub fn respawn_stalled_checker(stalled_generation: u64) -> Option<u64> {
    let args = std::sync::Arc::clone(CHECKER_ARGS.get()?);
    let next = checker_watch::WATCH.supersede(stalled_generation)?;
    match spawn_checker_thread(next, args) {
        Ok(()) => {
            warn("the update check stopped answering; aterm started a fresh one in its place");
            Some(next)
        }
        Err(error) => {
            checker_watch::WATCH.unstarted();
            warn(&format!(
                "the update check stopped answering; aterm could not start a fresh one: {error}"
            ));
            None
        }
    }
}

/// Linux supervises its own enrolled checker.
#[cfg(not(target_os = "macos"))]
pub fn respawn_stalled_checker(_stalled_generation: u64) -> Option<u64> {
    None
}

/// Why a checker thread returned.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckerExit {
    /// A replacement took over; this thread was last in `phase`.
    Superseded { phase: checker_watch::CheckerPhase },
}

/// How one cycle of [`run_checker`] ended.
#[cfg(target_os = "macos")]
enum Cycle {
    /// Go round again.
    Next,
    /// This generation was replaced while the cycle was in the named phase.
    Superseded(checker_watch::CheckerPhase),
}

/// What one cycle got when it asked for the machine-wide `checker.lock`.
#[cfg(target_os = "macos")]
enum CheckerGate {
    /// This cycle is the machine's checker until the guard drops.
    Held(aterm_update_core::FileLock),
    /// Another process held it past the bound.
    Deferred,
    /// The lock itself failed (not contention).
    Unavailable,
}

/// A cycle that took `checker.lock` writes its pid into it.
#[cfg(target_os = "macos")]
fn note_checker_lock_holder(path: &std::path::Path) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        let _ = writeln!(file, "{}", std::process::id());
    }
}

/// The pid the last process to take `checker.lock` wrote into it ([`note_checker_lock_holder`]).
#[cfg(target_os = "macos")]
fn checker_lock_holder(path: &std::path::Path) -> Option<u32> {
    let pid: u32 = read_ledger_text(path)?.trim().parse().ok()?;
    let raw = i32::try_from(pid).ok().filter(|raw| *raw > 0)?;
    // SAFETY: signal 0 delivers nothing; it only asks whether `raw` exists.
    let exists = unsafe { libc::kill(raw, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    exists.then_some(pid)
}

/// When this machine last completed an update check.
#[cfg(target_os = "macos")]
fn machine_check_stale_since(
    staging: &paths::Staging,
    stale: std::time::Duration,
) -> Option<String> {
    let text = read_ledger_text(&check_receipt::path(staging))?;
    let v = text.parse::<aterm_toml::Value>().ok()?;
    let checked = v.get("updated_at").and_then(aterm_toml::Value::as_str)?;
    rfc3339_older_than(checked, stale.as_secs()).then(|| checked.to_string())
}

/// Whether the process holding `checker.lock` at `path` has shown it is still working.
#[cfg(target_os = "macos")]
fn checker_holder_progressing(path: &std::path::Path, fresh: std::time::Duration) -> bool {
    let Ok(written) = std::fs::symlink_metadata(path).and_then(|meta| meta.modified()) else {
        return false;
    };
    match std::time::SystemTime::now().duration_since(written) {
        Ok(age) => age < fresh,
        Err(ahead) => ahead.duration() <= HOLDER_STAMP_SLACK,
    }
}

/// How far past this process's clock a lock stamp read first may be and still count as written now.
#[cfg(target_os = "macos")]
const HOLDER_STAMP_SLACK: std::time::Duration = std::time::Duration::from_millis(5);

/// How recently a holder must have written `checker.lock` to count as making progress.
#[cfg(target_os = "macos")]
fn holder_fresh(lock_wait: std::time::Duration) -> std::time::Duration {
    lock_wait / 2
}

/// The holder's beat: while a cycle holds `checker.lock` and its checker is alive, a thread
/// rewrites the lock every quarter of the lock wait, so a sibling can tell it from a stopped one.
/// It is joined on drop, before the lock is let go, so it never stamps a lock another process
/// then holds.
#[cfg(target_os = "macos")]
struct HolderBeat {
    stop: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Test-only: the beats this thread has finished.
    #[cfg(test)]
    beats: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

#[cfg(target_os = "macos")]
impl HolderBeat {
    fn start(
        path: std::path::PathBuf,
        watch: &'static checker_watch::CheckerWatch,
        generation: u64,
        every: std::time::Duration,
    ) -> Self {
        let alive = checker_alive(watch, generation);
        Self::start_with(path, every, move || alive())
    }

    fn start_with(
        path: std::path::PathBuf,
        every: std::time::Duration,
        alive: impl Fn() -> bool + Send + 'static,
    ) -> Self {
        let stop = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let flag = std::sync::Arc::clone(&stop);
        #[cfg(test)]
        let beats = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        #[cfg(test)]
        let counted = std::sync::Arc::clone(&beats);
        let thread = std::thread::Builder::new()
            .name("aterm-update-lock-beat".into())
            .spawn(move || {
                let (stopped, wake) = &*flag;
                let mut guard = stopped.lock().unwrap_or_else(|p| p.into_inner());
                loop {
                    guard = wake
                        .wait_timeout(guard, every)
                        .unwrap_or_else(|p| p.into_inner())
                        .0;
                    if *guard {
                        return;
                    }
                    if alive() {
                        note_checker_lock_holder(&path);
                    }
                    #[cfg(test)]
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
            .ok();
        Self {
            stop,
            thread,
            #[cfg(test)]
            beats,
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for HolderBeat {
    fn drop(&mut self) {
        let (stopped, wake) = &*self.stop;
        *stopped.lock().unwrap_or_else(|p| p.into_inner()) = true;
        wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Whether a lock holder's work is alive, asked by its [`HolderBeat`] before each stamp.
#[cfg(target_os = "macos")]
pub(crate) type HolderAlive = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// The one liveness rule for a lock holder.
#[cfg(target_os = "macos")]
fn checker_alive(watch: &'static checker_watch::CheckerWatch, generation: u64) -> HolderAlive {
    std::sync::Arc::new(move || {
        watch.is_current(generation) && watch.snapshot().is_some_and(|beat| beat_fresh(&beat))
    })
}

/// Whether `beat` is fresh for its phase now.
#[cfg(target_os = "macos")]
fn beat_fresh(beat: &checker_watch::CheckerBeat) -> bool {
    matches!(
        checker_watch::classify(beat, checker_watch::now_secs()),
        checker_watch::CheckerVerdict::Fresh { .. }
    )
}

#[cfg(target_os = "macos")]
thread_local! {
    static CHECK_ALIVE: std::cell::RefCell<Option<HolderAlive>> =
        const { std::cell::RefCell::new(None) };
}

/// While alive, the check on this thread is judged by `alive` ([`stage_holder_alive`]).
#[cfg(target_os = "macos")]
struct CheckAliveScope(Option<HolderAlive>);

#[cfg(target_os = "macos")]
impl CheckAliveScope {
    fn enter(alive: HolderAlive) -> Self {
        Self(CHECK_ALIVE.with(|slot| slot.replace(Some(alive))))
    }
}

#[cfg(target_os = "macos")]
impl Drop for CheckAliveScope {
    fn drop(&mut self) {
        let previous = self.0.take();
        CHECK_ALIVE.with(|slot| *slot.borrow_mut() = previous);
    }
}

#[cfg(target_os = "macos")]
thread_local! {
    static CHECK_AFTER_WAKE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// While alive, the check on this thread began inside a wake's grace when `after_wake`.
#[cfg(target_os = "macos")]
struct AfterWakeScope(bool);

#[cfg(target_os = "macos")]
impl AfterWakeScope {
    fn enter(after_wake: bool) -> Self {
        Self(CHECK_AFTER_WAKE.with(|slot| slot.replace(after_wake)))
    }
}

#[cfg(target_os = "macos")]
impl Drop for AfterWakeScope {
    fn drop(&mut self) {
        CHECK_AFTER_WAKE.with(|slot| slot.set(self.0));
    }
}

/// Whether the check running on this thread began inside a wake's grace.
#[cfg(target_os = "macos")]
pub(crate) fn check_follows_wake() -> bool {
    CHECK_AFTER_WAKE.with(std::cell::Cell::get)
}

/// What the `stage.lock` holder's beat asks before each stamp.
#[cfg(target_os = "macos")]
pub(crate) fn stage_holder_alive() -> HolderAlive {
    CHECK_ALIVE
        .with(|slot| slot.borrow().clone())
        .unwrap_or_else(|| {
            std::sync::Arc::new(|| {
                checker_watch::WATCH
                    .snapshot()
                    .is_none_or(|beat| beat_fresh(&beat))
            })
        })
}

/// A cycle's hold on `checker.lock`, with its [`HolderBeat`]. The beat is declared first so it
/// drops, and stops, before the lock is released.
#[cfg(target_os = "macos")]
struct HeldGate {
    _beat: Option<HolderBeat>,
    _lock: aterm_update_core::FileLock,
}

/// Ask for the machine-wide checker lock at `path`, waiting at most `wait`.
#[cfg(target_os = "macos")]
fn checker_gate(path: &std::path::Path, wait: std::time::Duration) -> CheckerGate {
    match aterm_update_core::FileLock::acquire_within(path, wait) {
        Ok(lock) => CheckerGate::Held(lock),
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => CheckerGate::Deferred,
        Err(_) => CheckerGate::Unavailable,
    }
}

/// The text of a caught panic's payload, for its log line.
#[cfg(target_os = "macos")]
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a non-text panic payload".to_string())
}

/// The check loop, under `generation` of `watch`.
#[cfg(target_os = "macos")]
fn run_checker(
    watch: &'static checker_watch::CheckerWatch,
    generation: u64,
    args: &CheckerArgs,
) -> CheckerExit {
    use checker_watch::{CheckerPhase, Streak, StreakLog};
    let current_build = args.current_build;
    if !watch.beat(generation, CheckerPhase::Starting) {
        return CheckerExit::Superseded {
            phase: CheckerPhase::Starting,
        };
    }
    let interval = cadence::INTERVAL_SECS;
    let mut schedule = cadence::Cadence::new(std::time::Duration::from_secs(interval));
    let mut failures = cadence::FailureLog::default();
    let mut woke_at: Option<std::time::Instant> = None;
    let wake_seen = std::cell::Cell::new(args.wake.posts());
    let mut strand_notice = unreadable::StrandNotice::default();
    let mut announcer = HealthAnnouncer::new(install::now_rfc3339());
    let speak_health = |announcer: &mut HealthAnnouncer| {
        if args.has_health_hook()
            && let Some(staging) = (args.staging)()
        {
            let staged =
                manifest::Ready::read_publishable(&staging).map(|ready| ready.build_number);
            let installed = bundle::resolve()
                .and_then(|installed| installed_build_observed(&installed.app_root).ok())
                .filter(|build| *build > current_build);
            let h = note_pending_update(&staging, current_build, staged, installed);
            let now = install::now_rfc3339();
            if let Some((title, body)) =
                announcer.tick_for_process(&ANNOUNCED_HEALTH, &h, &now, current_build)
            {
                args.send_health(title, body);
            }
            if let Some((title, body)) =
                announcer.tick_overdue(&h, &now, current_build, automatic_apply_on())
            {
                args.send_health(title, body);
            }
        }
    };
    let mut announced_installed: Option<u64> = None;
    let mut unverifiable_installed: Option<(u64, String)> = None;
    let mut announced_stage: Option<u64> = None;
    if wait_while_uncommitted_handoff_candidate(UNCOMMITTED_CANDIDATE_HOLD_BOUND) {
        debug("the first update check waited for the handoff to commit");
    }
    let last = std::cell::Cell::new(CheckerPhase::Starting);
    let stamp = |phase: CheckerPhase| -> bool {
        let live = watch.beat(generation, phase);
        if live {
            last.set(phase);
        }
        live
    };
    let wait = |schedule: &cadence::Cadence| -> cadence::Waited {
        match args.pause {
            Some(pause) => cadence::sleep_watching_for_wake(args.wake, wake_seen.get(), pause),
            None => cadence::wait(schedule, args.wake, wake_seen.get()).1,
        }
    };
    let settle = |schedule: &mut cadence::Cadence,
                  failures: &mut cadence::FailureLog,
                  woke_at: &mut Option<std::time::Instant>| {
        settle_after_wake(schedule, failures, woke_at, args.wake_settle);
        wake_seen.set(args.wake.posts());
    };
    let mut settings_misses = StreakLog::new(SETTINGS_MISS_LOG_EVERY);
    let mut deferrals = StreakLog::new(CHECKER_DEFERRAL_LOG_EVERY);
    let mut stalled_deferrals: u64 = 0;
    let mut ungated_said = false;
    let mut lane_busy = StreakLog::new(LANE_BUSY_LOG_EVERY);
    loop {
        if !stamp(CheckerPhase::Settings) {
            return CheckerExit::Superseded { phase: last.get() };
        }
        let cycle = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Cycle {
            let settings = (args.source_provider)();
            if settings.is_some()
                && let Some(misses) = settings_misses.clear()
            {
                watch.set_settings_misses(generation, 0);
                log(&format!(
                    "update checks resumed: the settings query answered after {misses} \
                     unanswered attempt(s)"
                ));
            }
            if !watch.is_current(generation) {
                return Cycle::Superseded(CheckerPhase::Settings);
            }
            let Some(source) = settings else {
                match settings_misses.note() {
                    Streak::Began => log(&format!(
                        "update checks paused: the settings query went unanswered; asking \
                         again every {} s, and no check runs until it answers",
                        args.settings_retry.as_secs()
                    )),
                    Streak::Continues(misses) => warn(&format!(
                        "update checks still paused: the settings query has gone unanswered \
                         {misses} times in a row; no check runs until it answers"
                    )),
                    Streak::Quiet => {}
                }
                watch.set_settings_misses(generation, settings_misses.consecutive());
                if !stamp(CheckerPhase::Settings) {
                    return Cycle::Superseded(last.get());
                }
                std::thread::sleep(args.settings_retry);
                return Cycle::Next;
            };
            if !stamp(CheckerPhase::BundleProbe) {
                return Cycle::Superseded(last.get());
            }
            if args.has_staged_hook()
                && let Some(installed) = bundle::resolve()
                && let Ok(installed_build) = installed_build_observed(&installed.app_root)
                && installed_announcement_due(
                    (args.staging)().as_ref(),
                    installed_build,
                    current_build,
                    announced_installed,
                )
            {
                let version = || {
                    verify::bundle_short_version(&installed.app_root)
                        .unwrap_or_else(|_| format!("build {installed_build}"))
                };
                match verify::verify_bundle_policy(&installed.app_root, effective_team_id()) {
                    Ok(()) => {
                        announced_installed = Some(installed_build);
                        let version = version();
                        log(&if automatic_apply_on() {
                            format!("update {version} is installed; aterm switches to it in place")
                        } else {
                            format!("update {version} is installed")
                        });
                        args.send_staged(installed_build, version);
                    }
                    Err(error) => {
                        let key = (installed_build, error.clone());
                        if unverifiable_installed.as_ref() != Some(&key) {
                            log(&format!(
                                "update {} is installed but does not verify ({error}); \
                                 re-checking each cycle",
                                version()
                            ));
                            unverifiable_installed = Some(key);
                        }
                    }
                }
            }
            match args.lane.try_lock() {
                Some(mut lane) => {
                    // A retired generation that woke here leaves without marking
                    // the lane as its own.
                    if !watch.is_current(generation) {
                        return Cycle::Superseded(last.get());
                    }
                    let lane_mark = watch.hold_lane(generation);
                    if let Some(cycles) = lane_busy.clear() {
                        debug(&format!(
                            "update checks can run again after {cycles} blocked cycle(s)"
                        ));
                    }
                    let checker_staging = (args.staging)();
                    if !stamp(CheckerPhase::LockWait) {
                        return Cycle::Superseded(last.get());
                    }
                    let checker_lock = checker_staging
                        .as_ref()
                        .map(|s| s.status.with_file_name("checker.lock"));
                    let gate = checker_lock
                        .as_deref()
                        .map(|path| (args.gate)(path, args.checker_lock_wait));
                    if !watch.is_current(generation) {
                        return Cycle::Superseded(CheckerPhase::LockWait);
                    }
                    let mut ungated = false;
                    let _checker_gate = match gate {
                        Some(CheckerGate::Deferred) => {
                            let progressing = checker_lock.as_deref().is_some_and(|path| {
                                checker_holder_progressing(
                                    path,
                                    holder_fresh(args.checker_lock_wait),
                                )
                            });
                            let streak = deferrals.note();
                            stalled_deferrals = if progressing {
                                0
                            } else {
                                stalled_deferrals.saturating_add(1)
                            };
                            let ledger_stale = checker_staging
                                .as_ref()
                                .and_then(|s| machine_check_stale_since(s, STALE_CHECK_AFTER));
                            ungated = !progressing
                                && (stalled_deferrals >= args.checker_ungated_after
                                    || ledger_stale.is_some());
                            if progressing {
                                ungated_said = false;
                            }
                            let holder = checker_lock
                                .as_deref()
                                .and_then(checker_lock_holder)
                                .map_or_else(
                                    || "another aterm process".to_string(),
                                    |pid| {
                                        format!("another aterm process (the lock names pid {pid})")
                                    },
                                );
                            match (streak, ungated) {
                                (Streak::Began, false) => log(&format!(
                                    "update check deferred: another aterm process has held the \
                                     checker lock for more than {} s — it is checking for this \
                                     machine; this process waits for its next cycle",
                                    args.checker_lock_wait.as_secs()
                                )),
                                (Streak::Continues(cycles), false) if progressing => {
                                    log(&format!(
                                        "update check deferred {cycles} cycles in a row: \
                                         another aterm process still holds the checker lock \
                                         and is still checking (it wrote the lock within the \
                                         last {} s) — a long download, not a stopped aterm",
                                        holder_fresh(args.checker_lock_wait).as_secs()
                                    ));
                                }
                                (Streak::Continues(cycles), false) => warn(&format!(
                                    "update check deferred {cycles} cycles in a row: another \
                                     aterm process has held the checker lock through each of \
                                     them (a stopped or hung aterm holds it until it is \
                                     continued or quit)"
                                )),
                                (Streak::Quiet, false) => {}
                                (_, true) if !ungated_said => {
                                    ungated_said = true;
                                    let why = ledger_stale.map_or_else(
                                        || format!("through {stalled_deferrals} cycles in a row"),
                                        |since| {
                                            format!(
                                                "and no aterm on this machine has completed an \
                                                 update check since {since}"
                                            )
                                        },
                                    );
                                    warn(&format!(
                                        "update check without the checker lock: {holder} has \
                                         held it {why} without finishing a check — it may be \
                                         stopped or hung; this process checks without waiting \
                                         for it until it lets go"
                                    ));
                                }
                                (Streak::Continues(cycles), true) => warn(&format!(
                                    "update checks still run without the checker lock: {holder} \
                                     has held it through {cycles} cycles in a row"
                                )),
                                (_, true) => {}
                            }
                            watch.set_deferrals(generation, stalled_deferrals);
                            if !ungated {
                                if !stamp(CheckerPhase::Waiting) {
                                    return Cycle::Superseded(last.get());
                                }
                                check_lane::after_skip((), (lane, lane_mark), interval, || {
                                    speak_health(&mut announcer);
                                    if matches!(wait(&schedule), cadence::Waited::Woke(_)) {
                                        settle(&mut schedule, &mut failures, &mut woke_at);
                                    }
                                });
                                return Cycle::Next;
                            }
                            None
                        }
                        Some(CheckerGate::Held(lock)) => {
                            let beat = checker_lock.clone().map(|path| {
                                note_checker_lock_holder(&path);
                                HolderBeat::start(
                                    path,
                                    watch,
                                    generation,
                                    args.checker_lock_wait / 4,
                                )
                            });
                            Some(HeldGate {
                                _beat: beat,
                                _lock: lock,
                            })
                        }
                        Some(CheckerGate::Unavailable) | None => None,
                    };
                    if !ungated && let Some(cycles) = deferrals.clear() {
                        ungated_said = false;
                        stalled_deferrals = 0;
                        watch.set_deferrals(generation, 0);
                        log(&format!(
                            "update checks resumed: this cycle got past the checker lock after \
                             {cycles} deferred cycle(s)"
                        ));
                    }
                    let now_unix = unix_now_secs();
                    if let Some((reason, window_expiry)) = checker_staging.as_ref().and_then(|s| {
                        checker_skip_for(s, current_build, &source, schedule.base(), now_unix)
                    }) {
                        log(&reason);
                        watch.note_check(generation);
                        if let Ok(hook) = args.staged_hook.lock()
                            && let Some(hook) = hook.as_ref()
                        {
                            announce_sibling_stage(
                                &mut announced_stage,
                                current_build,
                                status(current_build).as_ref(),
                                Some(hook),
                            );
                        }
                        let until = skip_timer_target(window_expiry, cadence::entropy_byte());
                        schedule.hold_until(
                            std::time::Instant::now()
                                + std::time::Duration::from_secs(until.saturating_sub(now_unix)),
                        );
                        if !stamp(CheckerPhase::Waiting) {
                            return Cycle::Superseded(last.get());
                        }
                        check_lane::after_skip(_checker_gate, (lane, lane_mark), interval, || {
                            speak_health(&mut announcer);
                            if matches!(wait(&schedule), cadence::Waited::Woke(_)) {
                                settle(&mut schedule, &mut failures, &mut woke_at);
                            }
                        });
                        return Cycle::Next;
                    }
                    let check_started = install::now_rfc3339();
                    let check_began = std::time::Instant::now();
                    if !stamp(CheckerPhase::Checking) {
                        return Cycle::Superseded(last.get());
                    }
                    let result = {
                        let _alive = CheckAliveScope::enter(checker_alive(watch, generation));
                        let _woke =
                            AfterWakeScope::enter(cadence::after_wake(woke_at, check_began));
                        (args.check)(current_build, &source)
                    };
                    args.lane.complete(&mut lane, current_build, &source);
                    if !watch.is_current(generation) {
                        return Cycle::Superseded(CheckerPhase::Checking);
                    }
                    watch.note_check(generation);
                    match result {
                        Ok(Some(v)) => {
                            emit(failures.success());
                            after_healthy_check(&mut schedule);
                            let staged_build = status(current_build).and_then(|s| s.staged_build);
                            let lane = apply_lane_report(current_build).filter(|r| {
                                staged_build.is_some_and(|b| b == r.last_failure_target_build)
                                    && r.failures_for_target > 0
                            });
                            match lane {
                                Some(r) if refusal_needs_person(&r.last_failure) => {
                                    log(&format!(
                                        "update {v} is staged but cannot be installed on \
                                         this copy ({}× refused: {}); run `aterm ctl update \
                                         status`",
                                        r.failures_for_target, r.last_failure
                                    ));
                                }
                                Some(r) => {
                                    log(&format!(
                                        "update {v} is staged; installing it failed {}× \
                                         ({}); it stays staged for the next attempt or \
                                         launch",
                                        r.failures_for_target, r.last_failure
                                    ));
                                }
                                None => {
                                    log(&format!("update {v} is staged"));
                                }
                            }
                            if args.has_staged_hook()
                                && let Some(b) = status(current_build).and_then(|s| s.staged_build)
                            {
                                announced_stage = Some(b);
                                args.send_staged(b, v);
                            }
                        }
                        Ok(None) if unreadable::is_stranded() => {
                            schedule.failed();
                        }
                        Ok(None) if github::rate_limited() => {
                            let now_unix = unix_now_secs();
                            let now = std::time::Instant::now();
                            let window_end = checker_staging
                                .as_ref()
                                .and_then(|s| {
                                    checker_skip_for(
                                        s,
                                        current_build,
                                        &source,
                                        schedule.base(),
                                        now_unix,
                                    )
                                })
                                .map(|(_, expiry)| {
                                    let until = skip_timer_target(expiry, cadence::entropy_byte());
                                    now + std::time::Duration::from_secs(
                                        until.saturating_sub(now_unix),
                                    )
                                });
                            schedule.deferred(window_end, now, cadence::entropy_byte());
                        }
                        Ok(None) => {
                            let recorded_failure = (args.staging)().is_some_and(|s| {
                                health::Health::read(&s.health()).last_failure_at.as_str()
                                    >= check_started.as_str()
                            });
                            if recorded_failure {
                                emit(Some(failures.failure(
                                    "no usable release this check — run \
                                     `aterm ctl update status` for the reason",
                                )));
                                schedule.failed();
                            } else {
                                emit(failures.success());
                                after_healthy_check(&mut schedule);
                            }
                        }
                        Err(e) => {
                            let offline = cadence::after_wake(woke_at, check_began)
                                || (args.staging)().is_some_and(|s| {
                                    unreachable_before_the_channel(
                                        &e,
                                        &health::Health::read(&s.health()),
                                        &check_started,
                                    )
                                });
                            emit(Some(cadence::note_failure(
                                &mut schedule,
                                &mut failures,
                                &e,
                                offline,
                            )));
                        }
                    }
                }
                None => {
                    let blocked = watch
                        .snapshot()
                        .filter(|beat| beat.generation == generation)
                        .and_then(|beat| beat.blocked_by());
                    match (lane_busy.note(), blocked) {
                        (Streak::Began, None) => {
                            debug("update check already running; this cycle skips it");
                        }
                        (Streak::Began, Some(holder)) => warn(&format!(
                            "update checks cannot run: an earlier check stopped answering \
                             while {}; checks resume when it finishes",
                            holder.phase.words()
                        )),
                        (Streak::Continues(cycles), None) => warn(&format!(
                            "update checks have been skipped for {cycles} cycles in a row while \
                             a manual check runs"
                        )),
                        (Streak::Continues(cycles), Some(holder)) => warn(&format!(
                            "update checks still cannot run after {cycles} cycles: an earlier \
                             check stopped answering {} s ago while {}",
                            checker_watch::now_secs().saturating_sub(holder.at_secs),
                            holder.phase.words()
                        )),
                        (Streak::Quiet, _) => {}
                    }
                }
            }
            if strand_notice.due(unreadable::is_stranded()) && args.has_health_hook() {
                let (title, body) = unreadable::notification();
                args.send_health(title, body);
            }
            speak_health(&mut announcer);
            watch.set_max_interval(generation, schedule.max_wait().as_secs());
            if !stamp(CheckerPhase::Waiting) {
                return Cycle::Superseded(last.get());
            }
            if matches!(wait(&schedule), cadence::Waited::Woke(_)) {
                settle(&mut schedule, &mut failures, &mut woke_at);
            }
            Cycle::Next
        }));
        match cycle {
            Ok(Cycle::Next) => {}
            Ok(Cycle::Superseded(phase)) => return CheckerExit::Superseded { phase },
            Err(payload) => {
                warn(&format!(
                    "an update check cycle panicked ({}); the checker carries on after a \
                     backed-off wait",
                    panic_text(payload.as_ref())
                ));
                schedule.failed();
                if !stamp(CheckerPhase::Waiting) {
                    return CheckerExit::Superseded { phase: last.get() };
                }
                if matches!(wait(&schedule), cadence::Waited::Woke(_)) {
                    settle(&mut schedule, &mut failures, &mut woke_at);
                }
            }
        }
    }
}

/// After a wait that ended on a detected wake: clears the backoff and the failure log, dates
/// the wake for [`cadence::after_wake`], and gives the network `settle` before the next check.
#[cfg(target_os = "macos")]
fn settle_after_wake(
    schedule: &mut cadence::Cadence,
    failures: &mut cadence::FailureLog,
    woke_at: &mut Option<std::time::Instant>,
    settle: std::time::Duration,
) {
    log(&format!(
        "woke from sleep; checking for updates in {}s",
        settle.as_secs()
    ));
    schedule.woke();
    *failures = cadence::FailureLog::default();
    *woke_at = Some(std::time::Instant::now());
    std::thread::sleep(settle);
}

/// The installed bundle's build number (`CFBundleVersion`).
#[cfg(target_os = "macos")]
fn installed_build_observed(app_root: &std::path::Path) -> Result<u64, String> {
    observed_build_with(&INSTALLED_BUILD, app_root, verify::bundle_build_number)
}

/// What identifies one version of a bundle's `Info.plist`.
#[cfg(target_os = "macos")]
type PlistIdentity = (std::path::PathBuf, u64, u64, u64, i64, i64, i64, i64);

/// The last build [`installed_build_observed`] read, and the plist it read it from.
#[cfg(target_os = "macos")]
static INSTALLED_BUILD: std::sync::Mutex<Option<(PlistIdentity, u64)>> =
    std::sync::Mutex::new(None);

/// [`installed_build_observed`] over an injected cache and reader.
#[cfg(target_os = "macos")]
fn observed_build_with(
    cache: &std::sync::Mutex<Option<(PlistIdentity, u64)>>,
    app_root: &std::path::Path,
    read: impl FnOnce(&std::path::Path) -> Result<u64, String>,
) -> Result<u64, String> {
    use std::os::unix::fs::MetadataExt as _;
    let identity = std::fs::metadata(app_root.join("Contents/Info.plist"))
        .ok()
        .map(|m| {
            (
                app_root.to_path_buf(),
                m.dev(),
                m.ino(),
                m.size(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        });
    let lock = || {
        cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if let (Some(identity), Some((seen, build))) = (identity.as_ref(), lock().as_ref())
        && identity == seen
    {
        return Ok(*build);
    }
    let build = read(app_root)?;
    *lock() = identity.map(|identity| (identity, build));
    Ok(build)
}

/// A sibling's stage is this process's news.
#[cfg(target_os = "macos")]
fn announce_sibling_stage(
    announced: &mut Option<u64>,
    current_build: u64,
    status: Option<&UpdateStatus>,
    on_staged: Option<&StagedNotify>,
) -> bool {
    let (Some(cb), Some(status)) = (on_staged, status) else {
        return false;
    };
    let Some(build) = status.staged_build.filter(|build| *build > current_build) else {
        return false;
    };
    if *announced == Some(build) {
        return false;
    }
    *announced = Some(build);
    let version = status
        .staged_version
        .clone()
        .unwrap_or_else(|| format!("build {build}"));
    log(&format!(
        "update {version} is staged by another aterm process"
    ));
    cb(build, version);
    true
}

/// The schedule after a healthy check.
#[cfg(target_os = "macos")]
fn after_healthy_check(schedule: &mut cadence::Cadence) {
    let in_flight = github::head_in_flight();
    schedule.succeeded(in_flight.as_ref().map_or(0, |(_, checks)| *checks));
    let Some((head, checks)) = in_flight else {
        return;
    };
    match cadence::in_flight_retry(checks) {
        Some(rung) => {
            let line = format!(
                "aterm {head} is published but not downloadable yet; checking again in ~{} s",
                rung.min(schedule.base()).as_secs()
            );
            if checks == 1 {
                log(&line);
            } else {
                debug(&line);
            }
        }
        None if checks == cadence::IN_FLIGHT_CHECKS + 1 => log(&format!(
            "aterm {head} is still not downloadable after {} checks; back to the regular \
             interval",
            cadence::IN_FLIGHT_CHECKS
        )),
        None => {}
    }
}

/// Route a [`cadence::LogAction`] to the app log.
#[cfg(target_os = "macos")]
fn emit(action: Option<cadence::LogAction>) {
    match action {
        Some(cadence::LogAction::Warn(text)) => warn(&text),
        Some(cadence::LogAction::Log(text)) => log(&text),
        Some(cadence::LogAction::Suppress) | None => {}
    }
}

/// No-op where nothing updates.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn spawn_background_check_with_source(
    _current_build: u64,
    _source_provider: SourceProvider,
    _notify: Option<HealthNotify>,
    _on_staged: Option<StagedNotify>,
) {
}

/// Consecutive same-class failed checks at which the failure is called persistent.
pub const PERSISTENT_AFTER: u32 = 3;

/// How long a streak a check books must span, on its class's own clock.
pub const PERSISTENT_SPAN: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// How long a newer build may wait on this machine.
pub const PENDING_UPDATE_OVERDUE_SECS: u64 = 60 * 60;

static AUTOMATIC_APPLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Tells the updater whether the host applies staged builds by itself.
pub fn set_automatic_apply(on: bool) {
    AUTOMATIC_APPLY.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// See [`set_automatic_apply`].
#[cfg(target_os = "macos")]
fn automatic_apply_on() -> bool {
    AUTOMATIC_APPLY.load(std::sync::atomic::Ordering::Relaxed)
}

/// Serializes the tests that mutate the process-global stranded latch (`unreadable::STRANDED`),
/// since a crate's tests run as threads of one process.
#[cfg(all(test, target_os = "macos"))]
pub(crate) static STRANDED_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A snapshot of the updater's state for the "Check for Updates" menu and the `aterm-ctl update`
/// query, read from the durable markers without network I/O.
#[derive(Debug, Clone)]
pub struct UpdateStatus {
    pub linux: Option<LinuxUpdateStatus>,
    /// Whether the updater is configured to act on this build/machine.
    pub enabled: bool,
    /// Whether this launch has a bundle the updater could replace.
    pub installable: bool,
    /// The running build number.
    pub current_build: u64,
    /// Build number staged for next-launch apply.
    pub staged_build: Option<u64>,
    /// Human version of the staged build.
    pub staged_version: Option<String>,
    /// Git commit of the staged build's source.
    pub staged_commit: Option<String>,
    /// Canonical SHA-256 of the staged DMG from the ready marker.
    pub staged_dmg_sha256: Option<String>,
    /// "What changed" notes for the staged build (from the manifest).
    pub changelog: Option<String>,
    /// The updater's last decision.
    pub outcome: String,
    /// RFC3339 UTC time of the last completed-check receipt (empty if absent).
    pub updated_at: String,
    /// Consecutive failed checks in the standing acquisition streak (0 when healthy).
    pub failing_checks: u32,
    /// Class of the most recent failure.
    pub failing_kind: String,
    /// Consecutive apply-lane failures.
    pub failing_applies: u32,
    /// RFC3339 UTC start of the current unhealthy period (empty if healthy).
    pub failing_since: String,
    /// Whether a stranding failure class (`pipeline`, `manifest`, `stage` or `apply`) is
    /// persistent ([`PERSISTENT_AFTER`]).
    pub failing_persistent: bool,
    /// The class of the standing acquisition streak (the one [`Self::failing_checks`] counts).
    pub failing_checks_kind: String,
    /// Whether the last completed check found this machine unable to read its release channel.
    /// It records no ledger failure, so [`Self::failing_persistent`] never says it.
    pub channel_unreadable: bool,
}

impl UpdateStatus {
    /// A single-line human summary (for a menu title / `OK` status line).
    #[must_use]
    pub fn summary(&self) -> String {
        let fallback;
        let text = if !self.outcome.trim().is_empty() {
            self.outcome.trim()
        } else {
            fallback = match (&self.staged_version, self.staged_build) {
                (Some(v), Some(b)) => format!("update {v} (build {b}) staged"),
                _ if !self.enabled => "aterm updates itself on macOS only".to_string(),
                _ => "no update check has run yet".to_string(),
            };
            &fallback
        };
        let mut chars = text.chars();
        let mut line: String = chars
            .by_ref()
            .take(512)
            .map(|c| {
                if c.is_whitespace() || c.is_control() {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        if chars.next().is_some() {
            line.pop();
            line.push('…');
        }
        line
    }

    /// Whether a stranding failure class is persistently failing ([`Self::failing_persistent`]).
    #[must_use]
    pub const fn is_failing_persistently(&self) -> bool {
        self.failing_persistent
    }

    const fn empty(enabled: bool, current_build: u64, outcome: String) -> Self {
        Self {
            linux: None,
            enabled,
            installable: false,
            current_build,
            staged_build: None,
            staged_version: None,
            staged_commit: None,
            staged_dmg_sha256: None,
            changelog: None,
            outcome,
            updated_at: String::new(),
            failing_checks: 0,
            failing_kind: String::new(),
            failing_applies: 0,
            failing_since: String::new(),
            failing_persistent: false,
            failing_checks_kind: String::new(),
            channel_unreadable: false,
        }
    }
}

/// The loop's schedule after a healthy check ([`after_healthy_check`]): the one-minute re-check
/// while the head in flight's count is within `cadence::IN_FLIGHT_CHECKS`, else the base interval.
#[cfg(all(test, target_os = "macos"))]
mod after_healthy_check_tests {
    use super::{after_healthy_check, cadence, github};
    use std::time::Duration;

    #[test]
    fn a_healthy_check_that_found_a_head_in_flight_takes_its_quick_rung() {
        let _guard = super::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let base = Duration::from_secs(cadence::INTERVAL_SECS);
        let mut schedule = cadence::Cadence::new(base);
        let spent = cadence::IN_FLIGHT_CHECKS;
        for (latched, wait) in [
            (Some(("v0.92.0".to_string(), 2)), Duration::from_secs(60)),
            (
                Some(("v0.92.0".to_string(), spent)),
                Duration::from_secs(60),
            ),
            (Some(("v0.92.0".to_string(), spent + 1)), base),
            (None, base),
        ] {
            github::set_head_in_flight(latched.clone());
            after_healthy_check(&mut schedule);
            assert_eq!(schedule.nominal(), wait, "{latched:?}");
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod sibling_stage_tests {
    use super::{StagedNotify, UpdateStatus, announce_sibling_stage};
    use std::sync::{Arc, Mutex};

    fn staged(build: Option<u64>) -> UpdateStatus {
        let mut status = UpdateStatus::empty(true, 81, "another aterm process completed".into());
        status.staged_build = build;
        status.staged_version = build.map(|b| format!("0.{b}.0"));
        status
    }

    #[test]
    fn a_skip_announces_a_siblings_newer_stage_once_per_build() {
        let calls: Arc<Mutex<Vec<(u64, String)>>> = Arc::default();
        let sink = Arc::clone(&calls);
        let hook: StagedNotify = Box::new(move |build, version| {
            sink.lock().unwrap().push((build, version));
        });
        let mut announced = None;
        let newer = staged(Some(85));
        assert!(announce_sibling_stage(
            &mut announced,
            81,
            Some(&newer),
            Some(&hook)
        ));
        assert!(!announce_sibling_stage(
            &mut announced,
            81,
            Some(&newer),
            Some(&hook)
        ));
        assert_eq!(*calls.lock().unwrap(), vec![(85, "0.85.0".to_string())]);
        let newest = staged(Some(86));
        assert!(announce_sibling_stage(
            &mut announced,
            81,
            Some(&newest),
            Some(&hook)
        ));
        assert_eq!(calls.lock().unwrap().len(), 2);

        let mut fresh = None;
        for (label, current, status) in [
            ("same build as running", 85, staged(Some(85))),
            ("older than running", 90, staged(Some(85))),
            ("nothing staged", 81, staged(None)),
        ] {
            assert!(
                !announce_sibling_stage(&mut fresh, current, Some(&status), Some(&hook)),
                "{label}"
            );
        }
        assert!(
            !announce_sibling_stage(&mut fresh, 81, None, Some(&hook)),
            "no status"
        );
        assert!(
            !announce_sibling_stage(&mut fresh, 81, Some(&newer), None),
            "no hook"
        );
        assert_eq!(fresh, None, "nothing was latched without an announcement");
        assert_eq!(calls.lock().unwrap().len(), 2);
    }
}

#[cfg(test)]
mod status_summary_tests {
    use super::UpdateStatus;

    fn staged(outcome: &str) -> UpdateStatus {
        let mut status = UpdateStatus::empty(true, 81, outcome.to_string());
        status.staged_build = Some(85);
        status.staged_version = Some("0.85.0".to_string());
        status
    }

    #[test]
    fn a_staged_summary_preserves_refusals_and_independent_failure_classes() {
        for (outcome, checks, applies) in [
            ("staged 0.85.0 (build 85) — NOT applied: no successor", 0, 0),
            (
                "staged build did not apply: child exited before Ready",
                0,
                1,
            ),
            ("update check failed: network unavailable", 1, 0),
        ] {
            let mut status = staged(outcome);
            status.failing_checks = checks;
            status.failing_applies = applies;
            assert_eq!(status.summary(), outcome);
        }
    }

    #[test]
    fn summary_is_one_bounded_unicode_line_for_outcomes_and_fallbacks() {
        let status = staged(" apply refused:\nkeep\tthese sessions open\r\n");
        assert_eq!(status.summary(), "apply refused: keep these sessions open");
        let long = format!("apply refused: {}", "猫".repeat(600));
        let line = staged(&long).summary();
        assert!(line.starts_with("apply refused: "));
        assert_eq!(line.chars().count(), 512);
        assert!(line.ends_with('…'));
        assert_eq!(staged(&"猫".repeat(512)).summary(), "猫".repeat(512));
        let mut status = staged("");
        status.staged_version = Some("猫\n".repeat(600));
        let line = status.summary();
        assert_eq!(line.chars().count(), 512);
        assert!(!line.contains('\n'));
        assert!(line.ends_with('…'));
    }
}

/// Canonical equivalence for two git commit stamps from aterm's build metadata.
#[must_use]
pub fn commit_matches(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> Option<String> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() || s == "unknown" || s.ends_with("-dirty") {
            return None;
        }
        if !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        Some(s)
    }
    match (norm(a), norm(b)) {
        (Some(x), Some(y)) => {
            let (short, long) = if x.len() <= y.len() {
                (&x, &y)
            } else {
                (&y, &x)
            };
            short.len() >= 7 && long.starts_with(short.as_str())
        }
        _ => false,
    }
}

/// Whether a persisted status outcome asserts that a build is staged.
#[cfg(any(target_os = "macos", test, debug_assertions))]
fn persisted_claims_stage(persisted: &str) -> bool {
    let persisted = persisted.trim_start();
    persisted.starts_with("staged ") || persisted.contains("ready to apply")
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
enum ReconciledStatusOutcome {
    Preserved(String),
    Neutralized(String),
}

#[cfg(any(target_os = "macos", test))]
impl ReconciledStatusOutcome {
    fn into_string(self) -> String {
        match self {
            Self::Preserved(outcome) | Self::Neutralized(outcome) => outcome,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
struct StatusReconciliation {
    current_build: u64,
    ready_present: bool,
    outcome: ReconciledStatusOutcome,
}

/// Projects one observed call of [`reconcile_status_outcome`] onto the bounded
/// `NativeUpdateStatusReconciliation.ReconcileStatus` transition.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn status_reconciliation_projection(
    running_build: u64,
    checked_from_build: u64,
    ready_present: bool,
    persisted: &str,
    reported_build: u64,
    reported_outcome: &str,
    neutralized: bool,
) -> (
    std::collections::BTreeMap<&'static str, i64>,
    std::collections::BTreeMap<&'static str, i64>,
) {
    let previous = std::collections::BTreeMap::from([
        ("phase", 1),
        (
            "running_build",
            i64::try_from(running_build).expect("bounded running build fits i64"),
        ),
        (
            "ledger_build",
            i64::try_from(checked_from_build).expect("bounded ledger build fits i64"),
        ),
        ("ready_present", i64::from(ready_present)),
        (
            "persisted_staged_claim",
            i64::from(persisted_claims_stage(persisted)),
        ),
        ("reported_build", 0),
        ("reported_staged_claim", 0),
        ("neutralized", 0),
    ]);
    let mut next = previous.clone();
    next.insert("phase", 2);
    next.insert(
        "reported_build",
        i64::try_from(reported_build).expect("bounded reported build fits i64"),
    );
    next.insert(
        "reported_staged_claim",
        i64::from(persisted_claims_stage(reported_outcome)),
    );
    next.insert("neutralized", i64::from(neutralized));
    (previous, next)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateStatusReconciliation",
        action = "ReconcileStatus",
        project = "aterm_update::status_reconciliation_projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStatusReconciliation",
        action = "PickStatusInputs",
        reason = "Bounded nondeterministic environment selection, not a shipping updater transition; Tier-1 exhaustively enumerates every projected input class before driving ReconcileStatus."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStatusReconciliation",
        action = "ReconcileTrustingLedger",
        reason = "Buggy=1 negative control only; the reducer reports the caller's running_build and neutralizes a mismatched ledger without a strictly newer Ready marker."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStatusReconciliation",
        action = "ReconcileOnMarkerPresence",
        reason = "Buggy=1 negative control only; Ready counts only when its build is strictly newer than the caller, so mere marker presence never preserves a staged claim."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStatusReconciliation",
        action = "NeutralizeDespiteReady",
        reason = "Buggy=1 negative control only; neutralization requires !ready_present, so an effective Ready marker always preserves its outcome."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStatusReconciliation",
        action = "NeutralizeEveryAbsentReady",
        reason = "Buggy=1 negative control only; without Ready the reducer neutralizes only a mismatched ledger build or a staged claim, preserving honest terminal outcomes."
    )
)]
#[cfg(any(target_os = "macos", test))]
fn reconcile_status_outcome(
    running_build: u64,
    checked_from_build: u64,
    ready_build: Option<u64>,
    persisted: String,
) -> StatusReconciliation {
    let ready_present = ready_build.is_some_and(|build| build > running_build);
    let claims_staged = persisted_claims_stage(&persisted);
    if !ready_present && (checked_from_build != running_build || claims_staged) {
        StatusReconciliation {
            current_build: running_build,
            ready_present,
            outcome: ReconciledStatusOutcome::Neutralized(if checked_from_build != running_build {
                "no update is staged (checked from another build of aterm)".to_string()
            } else {
                "no update is staged".to_string()
            }),
        }
    } else {
        StatusReconciliation {
            current_build: running_build,
            ready_present,
            outcome: ReconciledStatusOutcome::Preserved(persisted),
        }
    }
}

/// Whether a failed check takes the quick [`cadence::OFFLINE_RETRY`] rungs.
#[cfg(target_os = "macos")]
fn unreachable_before_the_channel(
    error: &str,
    health: &health::Health,
    check_started: &str,
) -> bool {
    cadence::is_network_unreachable(error)
        && health.kind == "network"
        && health.last_failure_at.as_str() >= check_started
}

/// How much a recorded deferral widens the machine-wide freshness window.
#[cfg(target_os = "macos")]
const DEFERRED_WINDOW_INTERVALS: u32 = 2;

/// The shortest window a recorded deferral holds the machine off the download host.
#[cfg(target_os = "macos")]
const DEFERRED_WINDOW_FLOOR: std::time::Duration = std::time::Duration::from_secs(20 * 60);

#[cfg(target_os = "macos")]
const _: () = assert!(
    DEFERRED_WINDOW_FLOOR.as_secs() * 7 / 10 + SKIP_JITTER_SECS
        <= cadence_bounds::MAX_BACKOFF.as_secs(),
    "a deferring process whose held timer is capped short of its own window skips on its own note"
);

/// How far past a sibling's window a skipping process scatters its wake.
#[cfg(target_os = "macos")]
const SKIP_JITTER_SECS: u64 = 60;

/// Why this cycle must not spend a network check, if it must not.
#[cfg(all(target_os = "macos", test))]
fn checker_skip(staging: &paths::Staging, base: std::time::Duration) -> Option<String> {
    checker_skip_at(staging, base, unix_now_secs()).map(|(reason, _)| reason)
}

/// Where a skipping process points its timer.
#[cfg(target_os = "macos")]
fn skip_timer_target(window_expiry: u64, entropy: u8) -> u64 {
    window_expiry.saturating_add(u64::from(entropy) * SKIP_JITTER_SECS / 256)
}

#[cfg(all(target_os = "macos", test))]
fn checker_skip_at(
    staging: &paths::Staging,
    base: std::time::Duration,
    now: u64,
) -> Option<(String, u64)> {
    checker_skip_for(
        staging,
        42,
        &Source {
            owner: "fixture".into(),
            repo: "channel".into(),
        },
        base,
        now,
    )
}

/// `checker_skip` for this build and source at an injected `now`.
#[cfg(target_os = "macos")]
fn checker_skip_for(
    staging: &paths::Staging,
    current_build: u64,
    source: &Source,
    base: std::time::Duration,
    now: u64,
) -> Option<(String, u64)> {
    let text = read_ledger_text(&check_receipt::path(staging))?;
    let v = text.parse::<aterm_toml::Value>().ok()?;
    if !check_receipt::matches(&v, current_build, source) {
        return None;
    }
    let checked = v.get("updated_at").and_then(aterm_toml::Value::as_str)?;
    if checked.is_empty() {
        return None;
    }
    let deferred = v
        .get("outcome")
        .and_then(aterm_toml::Value::as_str)
        .is_some_and(|outcome| outcome == "deferred");
    let window = if deferred {
        base.saturating_mul(DEFERRED_WINDOW_INTERVALS)
            .max(DEFERRED_WINDOW_FLOOR)
    } else {
        cadence::in_flight_retry(check_receipt::head_in_flight_checks(&v))
            .map_or(base, |rung| rung.min(base))
    };
    let fresh_window = window.as_secs().saturating_mul(7) / 10;
    let checked_epoch = u64::try_from(aterm_types::rfc3339::parse_utc(checked)?).ok()?;
    if checked_epoch > now.saturating_add(SKIP_JITTER_SECS) {
        return None;
    }
    if now.saturating_sub(checked_epoch) >= fresh_window {
        return None;
    }
    Some((
        String::from(if deferred {
            "the shared update ledger records a deferred check — this machine is \
             holding off the download host for the rest of the backoff"
        } else {
            "another aterm process completed this interval's update check"
        }),
        checked_epoch.saturating_add(fresh_window),
    ))
}

#[cfg(target_os = "macos")]
use install::unix_now_secs;

/// How the last check's assets were delivered, from the status ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delivery {
    pub note: Option<String>,
}

impl Delivery {
    /// Parse the delivery note out of a `status.toml` text.
    #[must_use]
    #[cfg(target_os = "macos")]
    pub fn from_ledger_text(text: &str) -> Self {
        let note = text.parse::<aterm_toml::Value>().ok().and_then(|v| {
            v.get("delivery")
                .and_then(aterm_toml::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });
        Self { note }
    }

    /// The ` delivery=<note>` token for the `aterm ctl update status` line.
    #[must_use]
    pub fn status_line_suffix(&self) -> String {
        self.note
            .as_deref()
            .map(|note| format!(" delivery={note}"))
            .unwrap_or_default()
    }
}

/// The [`Delivery`] facts in this machine's status ledger.
#[cfg(target_os = "macos")]
#[must_use]
pub fn delivery() -> Option<Delivery> {
    let staging = paths::Staging::resolve()?;
    let text = read_ledger_text(&staging.status)?;
    Some(Delivery::from_ledger_text(&text))
}

/// Non-macOS stub: no ledger.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn delivery() -> Option<Delivery> {
    None
}

/// Why [`fn@status`] answered `None` on macOS, in a person's words.
#[cfg(target_os = "macos")]
#[must_use]
pub fn ledger_unreadable_cause() -> String {
    let Some(base) = aterm_update_core::seal_guard::updates_base() else {
        return "HOME is not set".to_string();
    };
    let root = base.join("Updates");
    let shown = "~/Library/Application Support/aterm/Updates";
    let probe = aterm_update_core::ensure_private_dir(&root)
        .and_then(|()| aterm_update_core::ensure_private_dir(&root.join("download")));
    match probe {
        Err(e)
            if e.raw_os_error().is_none() && e.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            format!("{shown} isn\u{2019}t a private directory of yours (mode 0700, not a symlink)")
        }
        Err(e) => {
            let text = e.to_string();
            let text = text.split(" (os error").next().unwrap_or(&text);
            format!("{shown}: {text}")
        }
        Ok(()) => format!("{shown} couldn\u{2019}t be opened; it opens now, so check again"),
    }
}

/// Reads the durable updater state (last outcome and any staged build's version/changelog).
#[cfg(target_os = "macos")]
#[must_use]
pub fn status(current_build: u64) -> Option<UpdateStatus> {
    let staging = paths::Staging::resolve()?;
    let (mut checked_from_build, mut outcome) = (current_build, String::new());
    let updated_at = check_receipt::completed_at(&staging).unwrap_or_default();
    if let Some(text) = read_ledger_text(&staging.status)
        && let Ok(v) = text.parse::<aterm_toml::Value>()
    {
        checked_from_build = v
            .get("current_build")
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(checked_from_build);
        outcome = v
            .get("outcome")
            .and_then(aterm_toml::Value::as_str)
            .unwrap_or("")
            .to_string();
    }
    let mut ready = manifest::Ready::read_publishable(&staging);
    let reconciliation = reconcile_status_outcome(
        current_build,
        checked_from_build,
        ready.as_ref().map(|ready| ready.build_number),
        outcome,
    );
    if !reconciliation.ready_present {
        ready = None;
    }
    let current_build = reconciliation.current_build;
    outcome = reconciliation.outcome.into_string();
    let h = health::Health::read(&staging.health());
    Some(UpdateStatus {
        linux: None,
        enabled: enabled(),
        installable: bundle::resolve().is_some(),
        current_build,
        staged_build: ready.as_ref().map(|r| r.build_number),
        staged_version: ready.as_ref().map(|r| r.version.clone()),
        staged_commit: ready.as_ref().and_then(|r| r.commit.clone()),
        staged_dmg_sha256: ready.as_ref().map(|r| r.dmg_sha256.clone()),
        changelog: ready.as_ref().and_then(|r| r.changelog.clone()),
        outcome,
        updated_at,
        failing_checks: h.acquisition_failures(),
        failing_persistent: h.is_persistent(),
        failing_checks_kind: h
            .standing_acquisition_class()
            .unwrap_or_default()
            .to_string(),
        failing_kind: h.kind,
        failing_applies: h.apply_failures,
        failing_since: h.failing_since,
        channel_unreadable: unreadable::is_stranded(),
    })
}

/// Timestamp of the last completed check for this source and running build.
#[cfg(target_os = "macos")]
#[must_use]
pub fn last_check_at(current_build: u64, source: &Source) -> Option<String> {
    let staging = paths::Staging::resolve()?;
    let text = read_ledger_text(&check_receipt::path(&staging))?;
    let value: aterm_toml::Value = text.parse().ok()?;
    if !check_receipt::matches(&value, current_build, source) {
        return None;
    }
    value
        .get("updated_at")
        .and_then(aterm_toml::Value::as_str)
        .filter(|stamp| !stamp.is_empty())
        .map(str::to_owned)
}

/// When this process's background check loop next looks at the channel (RFC 3339 UTC).
#[cfg(target_os = "macos")]
#[must_use]
pub fn next_check_at() -> Option<String> {
    cadence::next_check_unix().map(aterm_types::rfc3339::format_rfc3339)
}

/// Non-macOS stub: the Linux updater keeps its own schedule in its state file.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn next_check_at() -> Option<String> {
    None
}

/// Non-macOS stub: no updater check receipts.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn last_check_at(_current_build: u64, _source: &Source) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        linux::last_check_at(_current_build, _source)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Linux reports its enrolled executable state.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn status(_current_build: u64) -> Option<UpdateStatus> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::status(_current_build))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// How old a completed check may be before `aterm ctl update status` flags the ledger as frozen.
pub const STALE_CHECK_AFTER: std::time::Duration = cadence_bounds::STALE_CHECK_AFTER;

/// Whether `stamp`, an RFC 3339 UTC timestamp as this crate writes them, is older than `secs`
/// ago. A stamp that does not start with an ASCII digit is never stale.
#[must_use]
pub fn rfc3339_older_than(stamp: &str, secs: u64) -> bool {
    if !stamp.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return false;
    }
    let now = aterm_time::unix_secs().unwrap_or(0);
    let threshold = aterm_types::rfc3339::format_rfc3339(now.saturating_sub(secs));
    !threshold.is_empty() && *stamp < *threshold
}

/// Run one update check + stage synchronously and return the resulting [`UpdateStatus`].
#[cfg(target_os = "macos")]
pub fn check_now(current_build: u64, source: &Source) -> UpdateStatus {
    // Not queued behind a stuck check: a checker generation the watchdog retired
    // inside its check still holds the lane, and `run_or_join` would wait on it
    // without bound. Say so at once instead.
    if enabled()
        && let Some(why) = manual_check_blocked(&checker_watch::WATCH)
    {
        warn(&why);
        return status(current_build)
            .map(|mut status| {
                status.outcome.clone_from(&why);
                status
            })
            .unwrap_or_else(|| UpdateStatus::empty(true, current_build, why));
    }
    if enabled() {
        asked_check(automatic(), &UNCHECKED_SWEEP, || {
            check_lane().run_or_join(current_build, source, || {
                if let Err(error) =
                    github::check_and_stage_for(current_build, source, github::Asked::Person)
                {
                    warn(&format!("manual update check failed: {error}"));
                }
            });
        });
    }
    status(current_build).unwrap_or_else(|| {
        UpdateStatus::empty(
            enabled(),
            current_build,
            if enabled() {
                "update check could not run".into()
            } else {
                "aterm updates itself on macOS only".into()
            },
        )
    })
}

/// Why a manual check cannot run right now, or `None` when it can: a retired checker generation
/// still holds this process's check lane, and a check would only queue behind it.
#[cfg(target_os = "macos")]
fn manual_check_blocked(watch: &checker_watch::CheckerWatch) -> Option<String> {
    let holder = watch.snapshot()?.blocked_by()?;
    Some(format!(
        "update check could not run: an earlier check stopped answering while {}",
        holder.phase.words()
    ))
}

/// Stub where nothing updates (Linux checks through [`check_now_with_settings`]'s own lane).
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn check_now(current_build: u64, _source: &Source) -> UpdateStatus {
    UpdateStatus::empty(
        false,
        current_build,
        "auto-update is unsupported on this platform".into(),
    )
}

/// Emits an informational updater line through `aterm_log`.
#[cfg(target_os = "macos")]
pub(crate) fn log(msg: &str) {
    #[cfg(test)]
    log_capture::record(aterm_log::Level::Info, msg);
    aterm_log::info!("aterm-update: {msg}");
}

/// Emits an updater line at DEBUG (see [`log`]).
#[cfg(target_os = "macos")]
pub(crate) fn debug(msg: &str) {
    #[cfg(test)]
    log_capture::record(aterm_log::Level::Debug, msg);
    aterm_log::debug!("aterm-update: {msg}");
}

/// Whether `key` is new to this process, for a line said once per process.
#[cfg(target_os = "macos")]
pub(crate) fn first_in_process(key: &str) -> bool {
    #[cfg(not(test))]
    {
        static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let mut seen = SEEN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if seen.iter().any(|k| k == key) {
            return false;
        }
        seen.push(key.to_string());
        true
    }
    #[cfg(test)]
    {
        thread_local! {
            static SEEN: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        SEEN.with(|seen| {
            let mut seen = seen.borrow_mut();
            if seen.iter().any(|k| k == key) {
                return false;
            }
            seen.push(key.to_string());
            true
        })
    }
}

/// Emit a non-fatal updater warning to the app log (see [`log`]).
#[cfg(target_os = "macos")]
pub(crate) fn warn(msg: &str) {
    #[cfg(test)]
    log_capture::record(aterm_log::Level::Warn, msg);
    aterm_log::warn!("aterm-update: {msg}");
}

/// Test-only capture of the updater's own log lines, with their level.
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod log_capture {
    use std::cell::RefCell;

    thread_local! {
        static LINES: RefCell<Vec<(aterm_log::Level, String)>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn record(level: aterm_log::Level, msg: &str) {
        LINES.with(|lines| lines.borrow_mut().push((level, msg.to_string())));
    }

    /// Drain this thread's captured lines.
    pub(crate) fn take() -> Vec<(aterm_log::Level, String)> {
        LINES.with(|lines| std::mem::take(&mut *lines.borrow_mut()))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod checker_gate_tests {
    use super::unreachable_before_the_channel;

    #[test]
    fn only_this_checks_unreachable_channel_takes_the_quick_rungs() {
        let dns = "curl HEAD https://github.com/alabsystems/aterm/releases/latest/download/\
                   aterm-appcast.toml failed (exit status: 6): curl: (6) Could not resolve \
                   host: github.com";
        let stalled = "zip download failed: curl download failed (exit status: 28): curl: \
                       (28) Operation too slow. Less than 4096 bytes/sec transferred the last \
                       30 seconds";
        let started = "2026-09-23T12:28:12Z";
        let filed = |kind: &str, at: &str| crate::health::Health {
            kind: kind.to_string(),
            last_failure_at: at.to_string(),
            ..Default::default()
        };
        assert!(unreachable_before_the_channel(
            dns,
            &filed("network", started),
            started
        ));
        assert!(unreachable_before_the_channel(
            dns,
            &filed("network", "2026-09-23T12:28:15Z"),
            started
        ));
        assert!(
            !unreachable_before_the_channel(stalled, &filed("pipeline", started), started),
            "a stalled container download is not a network that is down"
        );
        assert!(
            !unreachable_before_the_channel(
                dns,
                &filed("network", "2026-09-23T12:18:12Z"),
                started
            ),
            "an earlier check's record says nothing about this one"
        );
        assert!(!unreachable_before_the_channel(
            "parse appcast: EOF while parsing",
            &filed("network", started),
            started
        ));
        assert!(!unreachable_before_the_channel(
            dns,
            &crate::health::Health::default(),
            started
        ));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod checker_skip_tests {
    use std::time::Duration;

    use super::{
        SKIP_JITTER_SECS, checker_skip, checker_skip_at, paths::Staging, skip_timer_target,
    };

    fn staging(name: &str) -> crate::paths::ScratchStaging {
        Staging::scratch(&format!("checker-{name}"))
    }

    fn now() -> u64 {
        aterm_time::unix_secs().expect("clock")
    }

    fn write_receipt(s: &Staging, age_secs: u64, outcome: &str) {
        let stamp = aterm_types::rfc3339::format_rfc3339(now().saturating_sub(age_secs));
        std::fs::write(
            super::check_receipt::path(s),
            format!(
                "schema = 1\ncurrent_build = 42\nsource = \"fixture/channel\"\n\
                 updated_at = \"{stamp}\"\noutcome = \"{outcome}\"\n"
            ),
        )
        .expect("write receipt");
    }

    const BASE: Duration = Duration::from_secs(30 * 60);

    #[test]
    fn a_skipping_process_points_its_timer_at_the_windows_end() {
        let s = staging("window-end");
        write_receipt(&s, 60, "completed");
        let now = now();
        let (_, expiry) = checker_skip_at(&s, BASE, now).expect("a fresh stamp skips");
        let fresh = BASE.as_secs() * 7 / 10;
        assert!(
            ((now - 60 + fresh - 1)..=(now - 60 + fresh + 1)).contains(&expiry),
            "checked_at + 0.7 × base: {expiry} vs now={now}"
        );
        assert_eq!(
            skip_timer_target(expiry, 0),
            expiry,
            "no entropy, no scatter"
        );
        let scattered = skip_timer_target(expiry, 255);
        assert!(
            (expiry..expiry + SKIP_JITTER_SECS).contains(&scattered),
            "the scatter is under a minute: {scattered} vs {expiry}"
        );
        let _ = std::fs::remove_dir_all(&s.root);
    }

    #[test]
    fn a_stamp_ahead_of_the_clock_defers_no_check() {
        let s = staging("future");
        write_receipt(&s, 0, "completed");
        assert!(checker_skip_at(&s, BASE, now() - 2 * 3600).is_none());
        let _ = std::fs::remove_dir_all(&s.root);
    }

    #[test]
    fn a_recorded_deferral_holds_off_every_process_not_just_the_one_that_saw_it() {
        let s = staging("deferred");
        write_receipt(&s, 25 * 60, "deferred");
        let reason = checker_skip(&s, BASE).expect("the deferral widens the window");
        assert!(reason.contains("deferred"), "{reason}");
        write_receipt(&s, 50 * 60, "deferred");
        assert!(checker_skip(&s, BASE).is_none(), "the retreat self-heals");
        write_receipt(&s, 25 * 60, "completed");
        assert!(
            checker_skip(&s, BASE).is_none(),
            "a healthy check returns the window to the base interval"
        );
        write_receipt(&s, 25 * 60, "deferred: install location not writable");
        assert!(checker_skip(&s, BASE).is_none());
        let _ = std::fs::remove_dir_all(&s.root);
    }

    #[test]
    fn the_deferring_process_wakes_past_its_own_deferred_window() {
        use super::cadence::Cadence;
        let s = staging("own-deferral");
        write_receipt(&s, 0, "deferred");
        let stamped = now();
        let (_, expiry) = checker_skip_at(&s, BASE, stamped).expect("the deferral's window");
        let at = std::time::Instant::now();
        for entropy in [0u8, 128, 255] {
            let until = skip_timer_target(expiry, entropy);
            let window_end = at + Duration::from_secs(until - stamped);
            let mut schedule = Cadence::new(BASE);
            schedule.deferred(Some(window_end), at, entropy);
            let wake = stamped + schedule.delay_at(at, entropy).as_secs();
            assert!(
                checker_skip_at(&s, BASE, wake).is_none(),
                "entropy {entropy}: woke at +{}s, inside its own window",
                wake - stamped
            );
            let mut ladder = Cadence::new(BASE);
            ladder.failed();
            let early = stamped + ladder.delay_at(at, entropy).as_secs();
            assert!(
                checker_skip_at(&s, BASE, early)
                    .is_some_and(|(reason, _)| reason.contains("deferred")),
                "entropy {entropy}: the ladder alone woke at +{}s",
                early - stamped
            );
        }
        let _ = std::fs::remove_dir_all(&s.root);
    }

    #[test]
    fn a_deferral_holds_the_machine_off_for_minutes_however_fast_the_base() {
        use super::cadence::{Cadence, INTERVAL_SECS};
        let base = Duration::from_secs(INTERVAL_SECS);
        let s = staging("deferral-floor");
        write_receipt(&s, 13 * 60, "deferred");
        assert!(
            checker_skip(&s, base).is_some_and(|reason| reason.contains("deferred")),
            "13 minutes into the retreat, every process still holds off"
        );
        write_receipt(&s, 15 * 60, "deferred");
        assert!(checker_skip(&s, base).is_none(), "and past it, they check");
        write_receipt(&s, 13 * 60, "completed");
        assert!(checker_skip(&s, base).is_none());

        write_receipt(&s, 0, "deferred");
        let stamped = now();
        let (_, expiry) = checker_skip_at(&s, base, stamped).expect("the deferral's window");
        assert!(expiry >= stamped + 10 * 60, "{}s", expiry - stamped);
        let at = std::time::Instant::now();
        for entropy in [0u8, 128, 255] {
            let until = skip_timer_target(expiry, entropy);
            let mut schedule = Cadence::new(base);
            schedule.deferred(Some(at + Duration::from_secs(until - stamped)), at, entropy);
            let wake = stamped + schedule.delay_at(at, entropy).as_secs();
            assert!(
                checker_skip_at(&s, base, wake).is_none(),
                "entropy {entropy}: woke at +{}s, inside its own window",
                wake - stamped
            );
        }
        let _ = std::fs::remove_dir_all(&s.root);
    }

    #[test]
    fn a_missing_or_empty_ledger_never_defers_a_check() {
        let s = staging("missing");
        assert!(checker_skip(&s, BASE).is_none(), "no ledger: check");
        std::fs::write(
            super::check_receipt::path(&s),
            "schema = 1\ncurrent_build = 42\nsource = \"fixture/channel\"\nupdated_at = \"\"\n",
        )
        .expect("write");
        assert!(checker_skip(&s, BASE).is_none(), "empty stamp: check");
        std::fs::write(super::check_receipt::path(&s), "not toml at all {{{").expect("write");
        assert!(checker_skip(&s, BASE).is_none(), "unparseable: check");
        std::fs::write(
            super::check_receipt::path(&s),
            format!(
                "schema = 1\nupdated_at = \"{}\"\n",
                aterm_types::rfc3339::format_rfc3339(now())
            ),
        )
        .expect("write");
        assert!(
            checker_skip(&s, BASE).is_none(),
            "legacy, no provenance: check"
        );
        let _ = std::fs::remove_dir_all(&s.root);
    }

    #[test]
    fn the_status_line_carries_delivery_only_when_recorded() {
        use super::Delivery;
        assert_eq!(
            Delivery::from_ledger_text("schema = 1\noutcome = \"up to date\"\n")
                .status_line_suffix(),
            ""
        );
        assert_eq!(
            Delivery::from_ledger_text("not toml {{{").status_line_suffix(),
            ""
        );
        for note in ["deferred", "blocked"] {
            assert_eq!(
                Delivery::from_ledger_text(&format!("schema = 1\ndelivery = \"{note}\"\n"))
                    .status_line_suffix(),
                format!(" delivery={note}")
            );
        }
    }
}

/// The per-cycle read of the installed bundle's build number spawns PlistBuddy only when the
/// plist moved, and the floor that yanks it is a gate read.
#[cfg(all(test, target_os = "macos"))]
mod installed_build_tests {
    use std::cell::Cell;
    use std::sync::Mutex;

    #[test]
    fn an_unreadable_floor_does_not_admit_the_installed_build() {
        use std::os::unix::fs::PermissionsExt;
        let staging = crate::paths::Staging::scratch("installed-yank-floor");
        let checks = |build: u64| crate::verify::BundleChecks {
            policy: Ok(()),
            build: Ok(build),
            commit: Ok("0123456789abcdef0123456789abcdef01234567".to_string()),
            team: None,
            short_version: Some(Ok(format!("0.{build}.0"))),
        };
        let facts = |build| super::installed_facts_from(checks(build), Some(&staging));
        let due = |build| super::installed_announcement_due(Some(&staging), build, 500, None);
        std::fs::write(staging.floor(), "min_build = 530\n").unwrap();
        assert_eq!(super::installed_build_yanked(Some(&staging), 520), Ok(true));
        assert_eq!(
            super::installed_build_yanked(Some(&staging), 530),
            Ok(false)
        );
        assert_eq!(super::installed_build_yanked(None, 520), Ok(false));
        assert!(facts(520).is_ok_and(|facts| facts.yanked));
        assert!(facts(530).is_ok_and(|facts| !facts.yanked && facts.build_number == 530));
        assert!(!due(520), "a yanked build is not announced");
        assert!(due(530));
        assert!(!super::installed_announcement_due(
            Some(&staging),
            530,
            500,
            Some(530)
        ));
        assert!(!super::installed_announcement_due(
            Some(&staging),
            530,
            530,
            None
        ));
        assert!(super::installed_announcement_due(None, 520, 500, None));
        let mode =
            |bits| std::fs::set_permissions(staging.floor(), std::fs::Permissions::from_mode(bits));
        mode(0o000).unwrap();
        if std::fs::File::open(staging.floor()).is_ok() {
            aterm_uds::host::could_not_run(
                "a mode-0 file opened anyway: root, or a file system that ignores modes",
            );
        }
        let unreadable = super::installed_build_yanked(Some(&staging), 520);
        let reported = facts(520);
        let announced = due(520);
        mode(0o600).unwrap();
        assert!(
            unreadable
                .as_ref()
                .is_err_and(|e| crate::is_passing_refusal(e)),
            "an unreadable floor must be a passing refusal, not a verdict: {unreadable:?}"
        );
        assert!(
            reported
                .as_ref()
                .is_err_and(|e| crate::is_passing_refusal(e)),
            "the facts reported an installed build an unreadable floor may yank: {reported:?}"
        );
        assert!(
            !announced,
            "the checker announced an installed build an unreadable floor may yank"
        );
    }

    #[test]
    fn an_unchanged_plist_is_read_once_and_a_moved_one_again() {
        let dir = aterm_tempfile::TempDir::new().expect("scratch");
        let app = dir.path().join("aterm.app");
        let plist = app.join("Contents/Info.plist");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        let cache = Mutex::new(None);
        let reads = Cell::new(0u32);
        let answer = Cell::new(Some(900u64));
        let observe = || {
            super::observed_build_with(&cache, &app, |_| {
                reads.set(reads.get() + 1);
                answer
                    .get()
                    .ok_or_else(|| "PlistBuddy CFBundleVersion timed out".to_string())
            })
        };
        std::fs::write(&plist, "build 900").unwrap();
        for _ in 0..5 {
            assert_eq!(observe(), Ok(900));
        }
        assert_eq!(reads.get(), 1, "five cycles, one read");

        answer.set(Some(901));
        std::fs::write(&plist, "build 9010").unwrap();
        assert_eq!(observe(), Ok(901), "rewritten, another size");
        assert_eq!(reads.get(), 2);

        let mtime = std::fs::metadata(&plist).unwrap().modified().unwrap();
        answer.set(Some(902));
        std::fs::write(&plist, "build 9020").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&plist)
            .and_then(|f| f.set_modified(mtime))
            .unwrap();
        assert_eq!(observe(), Ok(902), "the same size under a restored mtime");
        assert_eq!(reads.get(), 3);

        answer.set(Some(903));
        let swapped = app.join("Contents/Info.plist.new");
        std::fs::write(&swapped, "build 9030").unwrap();
        std::fs::rename(&swapped, &plist).unwrap();
        assert_eq!(observe(), Ok(903), "a new inode");
        assert_eq!(reads.get(), 4);
        assert_eq!(observe(), Ok(903));
        assert_eq!(reads.get(), 4, "and then remembered");

        answer.set(None);
        std::fs::write(&plist, "build 9040").unwrap();
        assert!(observe().is_err());
        answer.set(Some(904));
        assert_eq!(observe(), Ok(904), "a failed read is not remembered");
        assert_eq!(reads.get(), 6);

        std::fs::remove_file(&plist).unwrap();
        answer.set(None);
        assert!(observe().is_err());
        assert!(observe().is_err());
        assert_eq!(reads.get(), 8, "nothing to stat: every call reads");
    }
}

#[cfg(test)]
mod rfc3339_age_tests {
    use super::rfc3339_older_than;

    #[test]
    fn empty_and_malformed_stamps_are_never_stale() {
        assert!(!rfc3339_older_than("", 60));
        assert!(!rfc3339_older_than("never", 60));
        assert!(!rfc3339_older_than("-", 60));
    }

    #[test]
    fn ancient_stamps_are_stale_and_future_stamps_are_not() {
        assert!(rfc3339_older_than("2001-01-01T00:00:00Z", 60));
        assert!(!rfc3339_older_than("2999-01-01T00:00:00Z", 60));
    }
}

#[cfg(test)]
mod commit_match_tests {
    use std::collections::BTreeSet;

    #[cfg(target_os = "macos")]
    use super::persistent_notice_is_owed;
    use super::{
        ReconciledStatusOutcome, StatusReconciliation, commit_matches, compiled_update_pin_sha256,
        persisted_claims_stage, reconcile_status_outcome, status_reconciliation_projection,
        update_pubkey_sha256,
    };

    #[test]
    fn update_pin_fingerprint_hashes_decoded_key_and_fails_closed() {
        let encoded = aterm_codec::base64::encode(&[0_u8; 32]).unwrap();
        assert_eq!(
            update_pubkey_sha256(&encoded).unwrap().as_deref(),
            Some("66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925")
        );
        assert_eq!(update_pubkey_sha256("").unwrap(), None);
        assert!(update_pubkey_sha256("not-base64").is_err());
        let short = aterm_codec::base64::encode(&[0_u8; 31]).unwrap();
        assert!(update_pubkey_sha256(&short).is_err());

        let compiled = compiled_update_pin_sha256();
        assert!(
            compiled.len() == 64 && compiled.bytes().all(|b| b.is_ascii_hexdigit()),
            "the armed master prints its fingerprint: {compiled}"
        );
        assert_eq!(
            compiled,
            update_pubkey_sha256(aterm_update_core::pins::PAPER_MASTER_PUBKEYS[0])
                .unwrap()
                .unwrap()
        );
    }

    fn observed_outcome(reconciliation: &StatusReconciliation) -> (&str, bool) {
        match &reconciliation.outcome {
            ReconciledStatusOutcome::Preserved(outcome) => (outcome, false),
            ReconciledStatusOutcome::Neutralized(outcome) => (outcome, true),
        }
    }

    #[test]
    fn status_reconciliation_exhaustively_refines_the_model() {
        let model = aterm_models_update::native_update_status_reconciliation_model();
        let reachable_inputs: BTreeSet<_> = model
            .successors("PickStatusInputs", &model.init_state())
            .into_iter()
            .collect();
        assert_eq!(
            reachable_inputs.len(),
            16,
            "2 running × 2 ledger × 2 Ready × 2 persisted classes"
        );

        let persisted_cases = [
            ("staged 0.2 (build 2) — applies on next launch", true),
            ("network check failed", false),
        ];
        let ready_builds = [None, Some(1), Some(2), Some(3)];
        let mut projected_inputs = BTreeSet::new();
        let mut calls = 0usize;

        for running_build in 1..=2 {
            for checked_from_build in 1..=2 {
                for ready_build in ready_builds {
                    for (persisted, staged_claim) in persisted_cases {
                        assert_eq!(persisted_claims_stage(persisted), staged_claim);
                        let reconciliation = reconcile_status_outcome(
                            running_build,
                            checked_from_build,
                            ready_build,
                            persisted.to_string(),
                        );
                        let expected_ready = ready_build.is_some_and(|build| build > running_build);
                        assert_eq!(
                            reconciliation.current_build, running_build,
                            "the ledger may never relabel its caller"
                        );
                        assert_eq!(
                            reconciliation.ready_present, expected_ready,
                            "Ready must be effective iff its build is strictly newer"
                        );

                        let expected_neutralized = !expected_ready
                            && (checked_from_build != running_build || staged_claim);
                        let (reported_outcome, neutralized) = observed_outcome(&reconciliation);
                        assert_eq!(neutralized, expected_neutralized);
                        if expected_neutralized {
                            let expected = if checked_from_build == running_build {
                                "no update is staged"
                            } else {
                                "no update is staged (checked from another build of aterm)"
                            };
                            assert_eq!(reported_outcome, expected);
                        } else {
                            assert_eq!(reported_outcome, persisted);
                        }

                        let (previous, next) = status_reconciliation_projection(
                            running_build,
                            checked_from_build,
                            reconciliation.ready_present,
                            persisted,
                            reconciliation.current_build,
                            reported_outcome,
                            neutralized,
                        );
                        assert!(
                            reachable_inputs.contains(&previous),
                            "real input projection is unreachable: {previous:?}"
                        );
                        projected_inputs.insert(previous.clone());

                        let label = format!(
                            "status reconciliation running={running_build} ledger={checked_from_build} ready={ready_build:?} staged={staged_claim}"
                        );
                        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
                            &model,
                            &[],
                            &previous,
                            &next,
                            Some("ReconcileStatus"),
                            &label,
                        );
                        assert!(admitted, "real reducer transition rejected: {why}");
                        for invariant in &model.invariants {
                            assert!(
                                model.check_invariant(invariant.name, &next),
                                "real reducer violated {}: {next:?}",
                                invariant.name
                            );
                        }
                        calls += 1;
                    }
                }
            }
        }

        assert_eq!(
            calls, 32,
            "the concrete decision lattice must stay exhaustive"
        );
        assert_eq!(
            projected_inputs, reachable_inputs,
            "Tier-1 must cover every bounded PickStatusInputs class"
        );
    }

    #[test]
    fn status_reconciliation_model_negative_controls_are_non_vacuous() {
        let model = aterm_models_update::native_update_status_reconciliation_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);

        let (caller_previous, caller_bug) = status_reconciliation_projection(
            1,
            2,
            false,
            "network check failed",
            2,
            "network check failed",
            false,
        );
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &caller_previous,
            &caller_bug,
            Some("ReconcileStatus"),
            "status reconciliation caller-build negative control",
        );
        assert!(!admitted, "healthy model admitted caller relabeling: {why}");
        assert!(
            buggy
                .successors("ReconcileTrustingLedger", &caller_previous)
                .contains(&caller_bug),
            "Buggy=1 must reproduce caller relabeling"
        );
        assert!(!buggy.check_invariant("CallerBuildIsAuthoritative", &caller_bug));

        let persisted = "staged 0.2 (build 2) — applies on next launch";
        let real = reconcile_status_outcome(2, 2, Some(2), persisted.to_string());
        assert!(
            !real.ready_present,
            "an equal-build Ready marker must be treated as absent"
        );
        let (ready_previous, _) = status_reconciliation_projection(
            2,
            2,
            real.ready_present,
            persisted,
            real.current_build,
            observed_outcome(&real).0,
            observed_outcome(&real).1,
        );
        let (_, ready_bug) =
            status_reconciliation_projection(2, 2, false, persisted, 2, persisted, false);
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &ready_previous,
            &ready_bug,
            Some("ReconcileStatus"),
            "status reconciliation strict-newer negative control",
        );
        assert!(!admitted, "healthy model admitted stale Ready prose: {why}");
        assert!(
            buggy
                .successors("ReconcileOnMarkerPresence", &ready_previous)
                .contains(&ready_bug),
            "Buggy=1 must reproduce the mere-presence defect"
        );
        assert!(!buggy.check_invariant("AbsentReadyCannotAdvertiseStage", &ready_bug));

        for (running, ledger, ready, persisted, mutant, law) in [
            (
                1,
                2,
                Some(2),
                "staged 0.2 (build 2) — applies on next launch",
                "NeutralizeDespiteReady",
                "ReadyPreservesPersistedOutcome",
            ),
            (
                2,
                2,
                None,
                "up to date (latest release build 2)",
                "NeutralizeEveryAbsentReady",
                "HonestTerminalOutcomeIsPreserved",
            ),
        ] {
            let real = reconcile_status_outcome(running, ledger, ready, persisted.to_string());
            let (reported, neutralized) = observed_outcome(&real);
            assert!(!neutralized, "the real reducer preserves {persisted:?}");
            assert_eq!(reported, persisted);
            let (previous, preserved) = status_reconciliation_projection(
                running,
                ledger,
                real.ready_present,
                persisted,
                real.current_build,
                reported,
                neutralized,
            );
            let rewritten = buggy.successors(mutant, &previous)[0].clone();
            assert_ne!(rewritten, preserved);
            let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                &previous,
                &rewritten,
                Some("ReconcileStatus"),
                "status reconciliation over-correction negative control",
            );
            assert!(!admitted, "healthy model admitted {mutant}: {why}");
            assert!(
                !buggy.check_invariant(law, &rewritten),
                "{mutant} escapes {law}"
            );
        }
    }

    #[test]
    fn status_reconciliation_never_reports_an_old_build_or_absent_stage() {
        let cases = [
            (
                53,
                54,
                None,
                "installed 0.54 (build 54); activating now",
                false,
            ),
            (
                53,
                53,
                None,
                "skipping re-stage of build 54 for another 5m; NOT skipping apply: \
                 staged 0.54 (build 54) is verified and ready to apply",
                false,
            ),
        ];
        for (running, ledger, ready, persisted, preserve) in cases {
            let reconciliation =
                reconcile_status_outcome(running, ledger, ready, persisted.to_string());
            assert_eq!(reconciliation.current_build, running);
            let got = reconciliation.outcome.into_string();
            if preserve {
                assert_eq!(got, persisted);
            } else {
                let expected = if ledger == running {
                    "no update is staged"
                } else {
                    "no update is staged (checked from another build of aterm)"
                };
                assert_eq!(got, expected);
                assert!(!got.contains("applies on next launch"));
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ledger_reader_rejects_oversized_and_non_regular_inputs() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-update-ledger-cap-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let valid = dir.join("valid.toml");
        std::fs::write(&valid, b"enabled = true\n").unwrap();
        assert_eq!(
            super::read_ledger_text(&valid).as_deref(),
            Some("enabled = true\n")
        );

        let oversized = dir.join("oversized.toml");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(super::MAX_LEDGER_BYTES + 1).unwrap();
        assert!(super::read_ledger_text(&oversized).is_none());
        assert!(super::read_ledger_text(&dir).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn commit_matches_classifies_every_pairing() {
        const FULL: &str = "4e91d3334041788ad92f5b6568cb648cb25805d6";
        for (a, b, matches, why) in [
            (
                "4e91d3334041",
                FULL,
                true,
                "12-hex binary stamp vs 40-hex appcast commit",
            ),
            (FULL, "4e91d3334041", true, "order must not matter"),
            (
                "  4E91D3334041 ",
                FULL,
                true,
                "case and surrounding whitespace are ignored",
            ),
            (FULL, FULL, true, "identical full hashes"),
            (
                "4e91d3334041",
                "deadbeefcafe0000000000000000000000000000",
                false,
                "different commits",
            ),
            ("4e91d3334041-dirty", FULL, false, "dirty vs its own base"),
            (
                "4e91d3334041-dirty",
                "4e91d3334041-dirty",
                false,
                "dirty vs itself",
            ),
            ("unknown", FULL, false, "the unknown stamp"),
            ("", FULL, false, "empty"),
            (
                "-",
                FULL,
                false,
                "the control socket's no-staged-commit sentinel",
            ),
            ("nothexatall12", FULL, false, "non-hex"),
            (
                "4e91d3",
                FULL,
                false,
                "6 hex is below git's abbreviation floor",
            ),
            (
                "4e91d33",
                FULL,
                true,
                "7 hex is git's floor and is accepted",
            ),
        ] {
            assert_eq!(commit_matches(a, b), matches, "{why}: {a:?} vs {b:?}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_second_persistent_class_still_speaks_but_the_same_one_stays_quiet() {
        assert!(!persistent_notice_is_owed(None, None));
        assert!(!persistent_notice_is_owed(Some("pipeline"), None));
        assert!(persistent_notice_is_owed(None, Some("pipeline")));
        assert!(!persistent_notice_is_owed(
            Some("pipeline"),
            Some("pipeline")
        ));
        assert!(persistent_notice_is_owed(Some("pipeline"), Some("apply")));
        assert!(!persistent_notice_is_owed(Some("apply"), Some("apply")));
        assert!(persistent_notice_is_owed(Some("apply"), Some("pipeline")));
        assert!(!persistent_notice_is_owed(Some("pipeline"), Some("stage")));
        assert!(!persistent_notice_is_owed(Some("stage"), Some("pipeline")));
    }
}

#[cfg(test)]
mod team_pin_tests {
    #[test]
    fn the_runtime_team_requirement_can_only_tighten() {
        assert!(
            !super::PINNED_TEAM_ID.is_empty(),
            "Tier APPLE is armed; an empty pin changes this test's meaning"
        );
        assert_eq!(
            super::effective_team_id(),
            super::PINNED_TEAM_ID,
            "the pin decides"
        );

        super::set_required_team_id(None);
        super::set_required_team_id(Some("   "));
        super::set_required_team_id(Some("ZZZZZ99999"));
        assert_eq!(
            super::effective_team_id(),
            super::PINNED_TEAM_ID,
            "a settings file can neither relax nor replace a compiled pin"
        );
    }
}

/// A policy this process could not read just then refuses the pre-verification as a passing
/// condition.
#[cfg(all(test, target_os = "macos"))]
mod handoff_candidate_facts_tests {
    use super::*;
    use aterm_update_core::handoff_policy::{PolicyFile, PolicyRead};

    #[test]
    fn an_unread_policy_refuses_the_preverification_as_a_passing_condition() {
        let unread = PolicyRead::Unread(
            "/x/Contents/Resources/aterm-handoff-policy.toml: unreadable: Too many open files \
             (os error 24)"
                .into(),
        );
        let refused = HandoffCandidateFacts::admitted(unread, Ok(false))
            .expect_err("an unread policy is not a pass");
        assert!(is_passing_refusal(&refused), "{refused}");
        for policy in [
            PolicyRead::Absent,
            PolicyRead::Ignored("not TOML".into()),
            PolicyRead::Parsed(PolicyFile::default()),
        ] {
            assert_eq!(
                HandoffCandidateFacts::admitted(policy.clone(), Ok(true)),
                Ok(HandoffCandidateFacts {
                    policy: policy.clone(),
                    grant_chunks: true,
                }),
                "{policy:?}"
            );
        }
    }

    #[test]
    fn an_info_plist_read_the_system_refused_for_now_refuses_the_preverification() {
        let declared = "<plist><dict><key>ATermHandoffGrantChunks</key>\
                        <string>chunks1</string></dict></plist>";
        let plist = std::path::Path::new("/x/aterm.app/Contents/Info.plist");
        let read = |errno: i32| {
            bundle::grant_chunks_judged(plist, Err(std::io::Error::from_raw_os_error(errno)))
        };
        for errno in [libc::EMFILE, libc::ENFILE, libc::ENOMEM, libc::EIO] {
            let refused = HandoffCandidateFacts::admitted(PolicyRead::Absent, read(errno))
                .expect_err("a moment's read of the Info.plist is not a pass");
            assert!(is_passing_refusal(&refused), "errno {errno}: {refused}");
        }
        assert_eq!(
            HandoffCandidateFacts::admitted(PolicyRead::Absent, read(libc::EACCES)),
            Ok(HandoffCandidateFacts {
                policy: PolicyRead::Absent,
                grant_chunks: false,
            })
        );
        assert_eq!(
            bundle::grant_chunks_judged(plist, Ok(Some(declared.to_string()))),
            Ok(true)
        );
        assert_eq!(bundle::grant_chunks_judged(plist, Ok(None)), Ok(false));
    }

    #[test]
    fn the_chunked_grant_is_read_off_a_bundle_on_disk() {
        let root = std::env::temp_dir().join(format!(
            "aterm-update-grant-chunks-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let contents = root.join("aterm.app/Contents");
        std::fs::create_dir_all(&contents).unwrap();
        let app = root.join("aterm.app");
        assert_eq!(
            HandoffCandidateFacts::read(&app).map(|facts| facts.grant_chunks),
            Ok(false)
        );
        std::fs::write(
            contents.join("Info.plist"),
            "<plist><dict><key>ATermHandoffGrantChunks</key><string>chunks1</string>\
             </dict></plist>",
        )
        .unwrap();
        let read = HandoffCandidateFacts::read(&app);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(read.map(|facts| facts.grant_chunks), Ok(true));
    }
}
