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

/// An isolated `.noindex` temporary directory for `aterm-update` tests so staged `.app` bundles
/// and test churn stay out of Spotlight and `fseventsd` across every platform cell.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ScratchDir(aterm_tempfile::TempDir);

#[cfg(test)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl ScratchDir {
    pub(crate) fn path(&self) -> &std::path::Path {
        self.0.path()
    }

    pub(crate) fn into_tempdir(self) -> aterm_tempfile::TempDir {
        self.0
    }

    pub(crate) fn keep(self) -> std::path::PathBuf {
        self.0.keep()
    }
}

#[cfg(test)]
impl std::ops::Deref for ScratchDir {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        self.0.path()
    }
}

#[cfg(test)]
impl AsRef<std::path::Path> for ScratchDir {
    fn as_ref(&self) -> &std::path::Path {
        self.0.path()
    }
}

/// Create a `.noindex` temporary directory for `aterm-update` tests, truncating `label` on a
/// UTF-8 character boundary.
#[cfg(test)]
pub(crate) fn scratch_dir(label: &str) -> ScratchDir {
    let clean: String = label
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let short = &clean[..clean.floor_char_boundary(16)];
    ScratchDir(
        aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-upd-{short}-"))
            .suffix(".noindex")
            .tempdir()
            .expect("create aterm-update test scratch dir"),
    )
}

#[cfg(target_os = "macos")]
mod bundle;
#[cfg(any(target_os = "macos", test))]
mod cadence;
mod cadence_bounds;
#[cfg(target_os = "macos")]
mod check_lane;
#[cfg(target_os = "macos")]
mod check_receipt;
#[cfg(any(target_os = "macos", target_os = "linux", test))]
mod check_wake;
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
/// On a first launch from a copy that goes away, install the running bundle and relaunch
/// from it (design §6.8).
pub mod first_launch;
/// Who raised an update floor, so a revocation can take it back.
mod floor_sources;
#[cfg(target_os = "macos")]
mod github;
#[cfg(target_os = "macos")]
pub use github::local_stage::{LocalStageOptions, stage_local};
#[cfg(target_os = "macos")]
mod health;
/// This copy's update record as the macOS updater reads and writes it.
#[cfg(target_os = "macos")]
mod hold;
#[cfg(target_os = "macos")]
mod install;
mod install_posture;
/// The lab's update records, read through the hook the executable installs.
pub mod lab;
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

pub(crate) fn parse_uint<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn instant_after(
    base: std::time::Instant,
    d: std::time::Duration,
) -> std::time::Instant {
    base.checked_add(d)
        .unwrap_or_else(|| base + std::time::Duration::from_secs(u64::from(u32::MAX)))
}
#[cfg(all(test, target_os = "macos"))]
mod checker_supervision_tests;
/// How each run of the installed app ended.
pub mod recovery_ledger;
mod relaunch;
/// Whether the crash restore has soaked on this machine's daily driver.
pub mod restore_soak;
/// Durable authenticated images for independent service activation.
#[cfg(unix)]
pub mod service_artifact;
/// Where the updater stands: the one decision `aterm update status` and a fleet survey share.
mod standing;
pub use standing::{StatusArm, fleet_fact, staged_tries};
#[cfg(target_os = "macos")]
mod status;
#[cfg(target_os = "macos")]
use aterm_update_core::app_swap as sys;
#[cfg(target_os = "macos")]
mod unreadable;
#[cfg(target_os = "macos")]
mod verify;
pub mod which_copy;
/// The one artifact a window here saw fail to start, which no windowless lane installs.
mod window_start_failure;
pub use window_start_failure::{FailedStartBytes, WindowStartFailure};
/// Windows: the staged build (the local build loop's or the release channel's), its standing
/// against the running copy, and the staged install.
pub mod windows;
/// Windows: the signed release channel's portable zip, verified, unpacked and staged.
pub mod windows_channel;

#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
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
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
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
    let Ok(initial_capacity) = usize::try_from(metadata.len().saturating_add(1)) else {
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

/// The update record: one per copy, its one hold expiring on its schedule.
pub use aterm_update_core::update_record;
/// The resolved GitHub release source and the compiled-in default owner/repo.
pub use aterm_update_core::{DEFAULT_OWNER, DEFAULT_REPO, Source};

/// The update record's file name under a copy's updater folder (`Updates/` on macOS, the
/// executable's `.aterm-update/` on Linux).
pub const RECORD: &str = aterm_update_core::update_record::FILE;

/// The wall clock, in unix seconds (0 where it reads before the epoch).
#[must_use]
pub(crate) fn unix_now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

pub use progress::{Progress, ProgressNotify, ProgressReport, set_progress_observer};

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

/// What one ask of this process's boot-health confirmation came to.
///
/// The gate an answer opens ([`Self::opens_gate`]) is the caller's own: every apply lane is
/// still judged by the durable trial under the apply lock. Only [`Self::settled_build`] names
/// a trial this ask disarmed, the authority to release work deferred for that exact trial.
///
/// The bound: a proof that ran under the apply lock and was refused is retried, and after
/// [`TRIAL_SETTLE_AFTER_REFUSALS`] refusals that lasted [`TRIAL_SETTLE_REFUSAL_SPAN`] of
/// awake time in one process the trial is settled without the proof
/// ([`Self::SettledUnproven`]), never reverted: the launch count alone reverts. Four holds stay
/// unbounded here and are named, not settled ([`Self::Pending`]): a window that never draws a
/// frame, which reaches no window checkpoint, so its trial waits for a window of it to draw or
/// for the launch count; a sentinel the filesystem will not remove; a refusal that says the
/// installed bundle is not the running build while its predecessor verifies, which the launch
/// count reverts; and a process with no awake clock, whose refusals span nothing. The GUI's
/// startup gate does not wait on them past its own patience: the apply lane's admission, under
/// the apply lock, then refuses in these words while the trial stays armed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootHealthVerdict {
    /// Nothing is owed: no trial of this build is armed, another installed copy owns it, or
    /// this ask proved and disarmed it (`settled` then names the build).
    Confirmed { settled: Option<u64> },
    /// The trial was disarmed at its bound without its recovery proof; the build stays
    /// installed and its predecessor stays at its rollback path, keyed to no process, until
    /// the next update replaces it. `why` is the journal line.
    SettledUnproven { build: u64, why: String },
    /// The trial stays armed and the updater names why. `retry_after` is when asking again
    /// can first change the answer; `None` leaves the pace to the caller.
    Pending {
        why: String,
        retry_after: Option<std::time::Duration>,
    },
    /// The updater could not be asked (the apply lock was held, this process resolves no
    /// install, the confirmation panicked). It counts toward no bound of the trial's.
    Unanswered { why: String },
}

impl BootHealthVerdict {
    /// The answer for a process that owes no trial.
    pub const NO_TRIAL: Self = Self::Confirmed { settled: None };

    /// Whether this process owes no further confirmation: confirmed, or settled at the bound.
    #[must_use]
    pub const fn opens_gate(&self) -> bool {
        matches!(self, Self::Confirmed { .. } | Self::SettledUnproven { .. })
    }

    /// The exact build whose trial this ask durably disarmed, proven or at its bound.
    #[must_use]
    pub const fn settled_build(&self) -> Option<u64> {
        match self {
            Self::Confirmed { settled } => *settled,
            Self::SettledUnproven { build, .. } => Some(*build),
            Self::Pending { .. } | Self::Unanswered { .. } => None,
        }
    }
}

/// Refused recovery proofs, counted in one process, after which a boot trial is settled
/// without its proof ([`BootHealthVerdict::SettledUnproven`]).
pub const TRIAL_SETTLE_AFTER_REFUSALS: u32 = PERSISTENT_AFTER;

/// The awake time the refusal must have lasted: the sum of the awake-clock gaps between one
/// counted refusal and the next. A burst of asks is no lasting refusal, and a sleep, a wall
/// clock step, and an ask that ran no proof add nothing.
pub const TRIAL_SETTLE_REFUSAL_SPAN: std::time::Duration = PERSISTENT_SPAN;

/// Whether the in-process updater (the background checker, the Software Update page, the
/// staged bundle or executable swap) runs on this platform at all. Windows has none of it: its
/// release channel is checked and staged by [`windows_channel`] through the staged-build watch
/// ([`staged_notice_enabled`]) and installed by the staged dist's own `install.ps1`.
#[must_use]
pub const fn enabled() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// Whether a window announces a build staged on this machine (`windows::Standing::offered`):
/// Windows, where [`enabled`] is false. Its watch reads the local pointers, and checks the
/// signed release channel when that is due ([`windows_channel::CHECK_EVERY`]).
#[must_use]
pub const fn staged_notice_enabled() -> bool {
    cfg!(windows)
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
/// ([`enabled`]). It gates only the lanes nobody asked for, such as the background checker. Off
/// for this call while [`automatic_at_launch`] has no answer yet.
#[must_use]
pub fn automatic() -> bool {
    automatic_at_launch().unwrap_or(false)
}

/// [`automatic`] as this process launched with it
/// ([`aterm_update_core::settings::update_enabled_at_launch`]: only the process's first ask reads
/// the file): `None` while no read of the setting has answered, which says nothing of a person's
/// `false`.
#[must_use]
pub fn automatic_at_launch() -> Option<bool> {
    if !enabled() {
        return Some(false);
    }
    aterm_update_core::settings::update_enabled_at_launch()
}

/// [`automatic_at_launch`], reading the setting again while nothing is kept
/// ([`aterm_update_core::settings::update_enabled_at_launch_read`]): for a thread that waits on the
/// answer, never one a person waits on.
#[must_use]
pub fn automatic_at_launch_read() -> Option<bool> {
    if !enabled() {
        return Some(false);
    }
    aterm_update_core::settings::update_enabled_at_launch_read()
}

/// What [`automatic_at_launch`] has answered so far, with no file read: `None` until it answered.
#[must_use]
pub fn automatic_kept() -> Option<bool> {
    if !enabled() {
        return Some(false);
    }
    aterm_update_core::settings::update_enabled_kept()
}

/// [`automatic`] as the user config says now, read again at each call: a process whose own
/// [`automatic_at_launch`] differs runs under a setting changed since it started, which its next
/// launch reads. `None` where the read hit a fault that can pass, which says nothing of the setting.
#[must_use]
pub fn automatic_as_saved() -> Option<bool> {
    aterm_update_core::settings::update_enabled_saved().map(|saved| enabled() && saved)
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

/// Counts a start of an already-armed update trial and restores its verified predecessor if
/// the crash-loop budget is exhausted. This never admits or installs a staged update, so it
/// also runs when automatic installation is off. Call before starting services, while this
/// process's image is replaceable and no handoff descriptors need preserving; a successful
/// recovery re-execs the restored build.
#[cfg(target_os = "macos")]
#[must_use]
pub fn recover_boot_trial_exact(
    current_build: u64,
    current_commit: &str,
    witness: TrialWitness,
) -> ApplyOutcome {
    install::recover_boot_trial(current_build, Some(current_commit), witness)
}

/// How long `build` has waited on this machine, by the pending clock each update check keeps for
/// the newest build it sees waiting ([`PENDING_UPDATE_OVERDUE_SECS`]). The node service bounds by
/// it how long a window that does not answer holds a waiting build, across its own restarts. Where
/// that clock follows no build, or one older than `build`, this starts it for `build` now: only a
/// check with a health hook (a window's) keeps the clock, and a frozen window checks nothing, so
/// an asker that waited for a check to start it would wait for good. Where it follows a newer
/// build, that clock's wait is the answer, which is no longer than `build`'s. `None` only where
/// `build` is withdrawn (below the floor) or the update root does not resolve.
#[cfg(target_os = "macos")]
#[must_use]
pub fn pending_update_waited(current_build: u64, build: u64) -> Option<std::time::Duration> {
    let staging = paths::Staging::resolve()?;
    pending_update_waited_at(&staging, current_build, build)
}

/// [`pending_update_waited`] in `staging`.
#[cfg(target_os = "macos")]
fn pending_update_waited_at(
    staging: &paths::Staging,
    current_build: u64,
    build: u64,
) -> Option<std::time::Duration> {
    if withdrawn(staging, build) {
        return None;
    }
    let read = health::Health::read(&staging.health());
    let h = if read.pending_build >= build && !read.pending_since.is_empty() {
        read
    } else {
        note_pending_update(staging, current_build, Some(build), None)
    };
    if h.pending_build < build || h.pending_since.is_empty() {
        return None;
    }
    install::rfc3339_delta_secs(&h.pending_since, &install::now_rfc3339())
        .map(std::time::Duration::from_secs)
}

/// Whether this installed image owns an unconfirmed trial which a headless service must
/// still prove. A live window's trial remains that window's responsibility while its wait lasts.
#[cfg(target_os = "macos")]
#[must_use]
pub fn headless_boot_trial_pending(current_build: u64) -> bool {
    install::headless_boot_trial_pending(current_build)
}

/// The build newer than `current_build` that a fresh start of this installed copy would run: one
/// staged and ready for a boot apply ([`apply_staged_if_ready_preserving_fds_exact`]) whose exact
/// bytes no window here saw fail to start ([`staged_for_a_start`]), or one another process already
/// installed in this image's place. `None` where neither stands, and for a copy outside an
/// installed bundle (a dev build), whose start applies nothing. The node service asks it to
/// decide whether to end its run so launchd starts it again
/// (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.13). An installed build the operator floor
/// yanks, or one whose floor cannot be read, is none: a window refuses to activate it
/// ([`preverify_installed_for_handoff`]), and a start would run it. It reads the stage's marker,
/// the floor and the installed bundle's `Info.plist`, and never the network.
#[cfg(target_os = "macos")]
#[must_use]
pub fn newer_build_for_a_start(current_build: u64) -> Option<u64> {
    let installed = bundle::resolve()?;
    newer_build_for_a_start_in(paths::Staging::resolve().as_ref(), current_build, || {
        installed_build_observed(&installed.app_root)
    })
}

/// [`newer_build_for_a_start`] over `staging` and the reader of the build installed in this
/// image's place (`installed`). Only the staged arm reads the window-start failure record.
#[cfg(target_os = "macos")]
fn newer_build_for_a_start_in(
    staging: Option<&paths::Staging>,
    current_build: u64,
    installed: impl FnOnce() -> Result<u64, String>,
) -> Option<u64> {
    let staged = staging.and_then(staged_for_a_start);
    newer_for_a_start(current_build, staged, installed().ok(), |build| {
        installed_build_yanked(staging, build)
    })
}

/// The staged build a fresh start would install: a publishable stage, unless its exact bytes are
/// ones a window here saw fail to start or that record cannot be read, which a windowless start
/// leaves for a window ([`window_start_failure::headless_refusal`]). The installed build is not
/// filtered this way on purpose: a trial of installed bytes needs its headless settle, which
/// keeps their window re-trial, or it would block every update.
#[cfg(target_os = "macos")]
pub(crate) fn staged_for_a_start(staging: &paths::Staging) -> Option<u64> {
    manifest::Ready::read_publishable(staging)
        .filter(|ready| {
            window_start_failure::holds(staging, ready.build_number, &ready.dmg_sha256) == Ok(false)
        })
        .map(|ready| ready.build_number)
}

/// [`newer_build_for_a_start`]'s choice: the newest of `staged` and `installed` above
/// `current_build`, the installed build only once `yanked` reads that the floor leaves it.
#[cfg(target_os = "macos")]
fn newer_for_a_start(
    current_build: u64,
    staged: Option<u64>,
    installed: Option<u64>,
    yanked: impl Fn(u64) -> Result<bool, String>,
) -> Option<u64> {
    let installed = installed.filter(|build| *build > current_build && yanked(*build) == Ok(false));
    staged
        .filter(|build| *build > current_build)
        .into_iter()
        .chain(installed)
        .max()
}

/// The build installed in this copy's place, where it is older than `current_build`: a boot's
/// crash-loop revert put it back (whose re-exec of it failed, so this process still runs the
/// build it reverted), or a person installed an older release over this one. A fresh start
/// runs it. `None` where the installed build is this one or newer or cannot be read, and for a
/// copy outside an installed bundle. It reads the installed bundle's `Info.plist` alone.
#[cfg(target_os = "macos")]
#[must_use]
pub fn older_build_installed(current_build: u64) -> Option<u64> {
    let installed = bundle::resolve()?;
    older_than(
        current_build,
        installed_build_observed(&installed.app_root).ok(),
    )
}

/// [`older_build_installed`]'s choice: `installed` where it is below `current_build`.
#[cfg(target_os = "macos")]
fn older_than(current_build: u64, installed: Option<u64>) -> Option<u64> {
    installed.filter(|build| *build < current_build)
}

/// Off macOS no installed bundle stands in this process's place.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn older_build_installed(_current_build: u64) -> Option<u64> {
    None
}

/// The build of the bundle installed in this copy's place, the one `aterm update status` on this
/// Mac runs (`~/.local/bin/aterm` is that bundle's): an update may have moved it past the build
/// this process runs, and a revert put it behind. `None` for a copy outside an installed bundle
/// and where its build does not read. It reads the installed bundle's `Info.plist` alone.
#[cfg(target_os = "macos")]
pub(crate) fn installed_bundle_build() -> Option<u64> {
    let installed = bundle::resolve()?;
    installed_build_observed(&installed.app_root).ok()
}

/// Off macOS no bundle is installed in this copy's place; Linux's status reads the build of the
/// executable installed at its path itself. Windows asks for none: its standing is the staged
/// build's ([`windows::Here::fleet_fact`]).
#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) const fn installed_bundle_build() -> Option<u64> {
    None
}

/// Off macOS no start of this process applies a stage this way.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn newer_build_for_a_start(_current_build: u64) -> Option<u64> {
    None
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
        BOOT_TRIAL_UNCONFIRMED_KEY,
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

/// Stable words in the wait for an installed build's boot trial to be confirmed.
pub const BOOT_TRIAL_UNCONFIRMED_KEY: &str = "update trial for build";

/// Whether `reason` is the passing wait for `current_build`'s own boot-health confirmation.
/// The build match prevents a foreign or newer trial from being released by this process's
/// health-proof event.
#[must_use]
pub fn is_boot_health_confirmation_wait(reason: &str, current_build: u64) -> bool {
    is_passing_refusal(reason)
        && reason.contains(&format!(
            "{BOOT_TRIAL_UNCONFIRMED_KEY} {current_build} is still unconfirmed"
        ))
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

/// Longest ordinary enqueue-to-completion window for boot-health confirmation before its caller
/// should retry idempotently. The shared FIFO permits one active reconcile read ahead of this
/// request, then a macOS trial can take the apply lock twice and authenticate both the retained
/// predecessor and the running bundle. Three handoff-preverification bounds cover those bounded
/// operations; the small margin lets the worker publish completion after its last helper exits.
pub const BOOT_HEALTH_CONFIRMATION_BOUND: std::time::Duration = HANDOFF_PREVERIFY_BOUND
    .saturating_add(HANDOFF_PREVERIFY_BOUND)
    .saturating_add(HANDOFF_PREVERIFY_BOUND)
    .saturating_add(std::time::Duration::from_secs(2));

/// The authenticated recipient-history protocol a candidate can recover.
/// This is reader evidence, never writer or execution authority.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NativeRecipientRecovery {
    /// No recognized declaration was authenticated.
    #[default]
    Absent,
    /// ANG5 history in the legacy issuer journal.
    LegacyJournal,
    /// ARC1 enrollment, ARB1 binding and ANG5 history in independent custody.
    BoundJournal,
}

/// What an outgoing build learns from a handoff candidate's bundle once the pre-verify has
/// authenticated it, never from a bundle that failed a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffCandidateFacts {
    /// The candidate's signed handoff policy.
    pub policy: aterm_update_core::handoff_policy::PolicyRead,
    /// The candidate's sealed `Info.plist` declares the chunked rendezvous grant.
    pub grant_chunks: bool,
    /// The candidate's sealed `Info.plist` declares the sessionless rendezvous grant.
    pub grant_empty: bool,
    /// The candidate's sealed `Info.plist` declares it rebuilds a window that held only an empty
    /// Workers list.
    pub grant_desk: bool,
    /// The candidate's sealed `Info.plist` declares this recipient-history protocol.
    /// This does not authorize a writer or attest to other active or rollback images.
    pub recipient_recovery: NativeRecipientRecovery,
}

impl HandoffCandidateFacts {
    /// Read the facts off a bundle that has just passed its checks.
    #[cfg(target_os = "macos")]
    pub(crate) fn read(app_root: &std::path::Path) -> Result<Self, String> {
        Self::admitted(
            aterm_update_core::handoff_policy::read_from_bundle(app_root),
            bundle::Capabilities::read(app_root),
        )
    }

    /// The facts as a pre-verification may pass them on.
    #[cfg(target_os = "macos")]
    fn admitted(
        policy: aterm_update_core::handoff_policy::PolicyRead,
        capabilities: Result<bundle::Capabilities, String>,
    ) -> Result<Self, String> {
        if let aterm_update_core::handoff_policy::PolicyRead::Unread(why) = &policy {
            return Err(verify::mark_passing(format!(
                "the candidate's handoff policy could not be read just then ({why}); the \
                 next attempt reads it again"
            )));
        }
        let plist_unread = |why: String| {
            verify::mark_passing(format!(
                "the candidate's Info.plist could not be read just then ({why}); the next \
                 attempt reads it again"
            ))
        };
        let capabilities = capabilities.map_err(plist_unread)?;
        Ok(Self {
            policy,
            grant_chunks: capabilities.grant_chunks,
            grant_empty: capabilities.grant_empty,
            grant_desk: capabilities.grant_desk,
            recipient_recovery: capabilities.recipient_recovery,
        })
    }
}

#[cfg(target_os = "macos")]
pub use bundle::{
    HANDOFF_GRANT_CHUNKS_KEY, HANDOFF_GRANT_CHUNKS_TOKEN, HANDOFF_GRANT_DESK_KEY,
    HANDOFF_GRANT_DESK_TOKEN, HANDOFF_GRANT_EMPTY_KEY, HANDOFF_GRANT_EMPTY_TOKEN,
    NATIVE_LEGACY_RECIPIENT_RECOVERY_TOKEN, NATIVE_RECIPIENT_RECOVERY_KEY,
    NATIVE_RECIPIENT_RECOVERY_TOKEN, plist_declares_grant_chunks, plist_declares_grant_desk,
    plist_declares_grant_empty, plist_marks_dev_build, plist_recipient_recovery,
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
        grant_empty: false,
        grant_desk: false,
        recipient_recovery: NativeRecipientRecovery::Absent,
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

/// Maximum Unicode characters retained for a refusal's separate diagnostic context.
pub const MAX_APPLY_REFUSAL_DETAIL_CHARS: usize = 400;

/// Records that an apply was refused (blocked, deferred or held) rather than attempted and failed.
#[cfg(target_os = "macos")]
pub fn record_apply_refusal(current_build: u64, reason: &str) {
    record_apply_refusal_with_detail(current_build, reason, "");
}

/// Records a refusal with separate diagnostic context, without classifying that context.
#[cfg(target_os = "macos")]
pub fn record_apply_refusal_with_detail(current_build: u64, reason: &str, detail: &str) {
    let Some(staging) = paths::Staging::resolve() else {
        return;
    };
    health::Health::record_apply_refusal_with_detail(
        &staging.health(),
        current_build,
        reason,
        detail,
    );
    let outcome = match manifest::Ready::read_publishable(&staging) {
        Some(ready) => format!(
            "staged {} (build {}) — NOT applied: {reason}",
            ready.version, ready.build_number
        ),
        None => format!("apply refused: {reason}"),
    };
    status::record(&staging, current_build, &outcome);
}

/// Books that a window running `current_build` launched a successor from `bytes` and saw it fail
/// to start (`signature` says how, in diagnostic words), replacing any earlier such record: the
/// node job's and every headless boot's lanes then leave those exact bytes for a window, and a
/// headless settle of a trial of them keeps their window re-trial at the count it carried. Call
/// it where the failure is judged, before any later withdraw of the same bytes.
///
/// # Errors
/// No private staging root, `bytes` names no artifact (an installed build the receipt does not
/// name), or the write failed: nothing was recorded.
#[cfg(target_os = "macos")]
pub fn record_window_start_failure(
    current_build: u64,
    bytes: FailedStartBytes<'_>,
    signature: &str,
) -> Result<(), String> {
    let staging = paths::Staging::resolve()
        .ok_or_else(|| "no private staging root is available".to_string())?;
    window_start_failure::record(
        &staging,
        current_build,
        bytes,
        signature,
        install::unix_now_secs(),
    )
}

/// Withdraws the record of a window-start failure if it names exactly `bytes`, because a window
/// of those bytes presented (or proved its adoption): `true` when a record was removed.
#[cfg(target_os = "macos")]
pub fn withdraw_window_start_failure(bytes: FailedStartBytes<'_>) -> bool {
    let Some(staging) = paths::Staging::resolve() else {
        return false;
    };
    match window_start_failure::withdraw(&staging, bytes) {
        Ok(removed) => removed,
        Err(error) => {
            warn(&format!(
                "could not withdraw the record of a window-start failure: {error}"
            ));
            false
        }
    }
}

/// The window-start failure on record, or `None` when there is none or it cannot be read.
#[cfg(target_os = "macos")]
#[must_use]
pub fn window_start_failure() -> Option<WindowStartFailure> {
    window_start_failure::read(&paths::Staging::resolve()?)
        .ok()
        .flatten()
}

/// Non-macOS: no window here hands off to a successor this way, so there is nothing to record.
///
/// # Errors
/// Never.
#[cfg(not(target_os = "macos"))]
pub fn record_window_start_failure(
    _current_build: u64,
    _bytes: FailedStartBytes<'_>,
    _signature: &str,
) -> Result<(), String> {
    Ok(())
}

/// Non-macOS counterpart to [`withdraw_window_start_failure`].
#[cfg(not(target_os = "macos"))]
pub fn withdraw_window_start_failure(_bytes: FailedStartBytes<'_>) -> bool {
    false
}

/// Non-macOS counterpart to [`window_start_failure`].
#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn window_start_failure() -> Option<WindowStartFailure> {
    None
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

/// Non-macOS counterpart to [`record_apply_refusal_with_detail`].
#[cfg(not(target_os = "macos"))]
pub fn record_apply_refusal_with_detail(_current_build: u64, _reason: &str, _detail: &str) {}

/// Where the automatic apply lane stands after it stopped retrying an artifact: a journal record
/// `aterm ctl update status` reads (`apply_lane=`, `failure_signature=`, `next_action=`), never a
/// request of a person.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyLaneRecord {
    /// A word for where the lane stands (`resampling`, `awaiting-newer`, `replacing`, `held`).
    pub lane: String,
    /// The build the lane stopped retrying.
    pub target_build: u64,
    /// The failure it stopped on, as a stable signature (`<shape>/<cause key>`).
    pub failure_signature: String,
    /// What aterm does next by itself about [`Self::target_build`].
    pub next_action: String,
    /// RFC3339 UTC of the record, as read back (ignored when recording).
    pub at: String,
}

/// The process that wrote an [`ApplyLaneRecord`]: its pid and kernel birth. The record is that
/// process's account of its own in-memory latch, so it reads as current only there; one an
/// earlier process left (before a restart, a swap or a crash) reads as none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneWriter {
    /// The writer's pid.
    pub pid: u32,
    /// The writer's kernel birth ([`aterm_uds::process::read_birth`]), 0 where it was unreadable.
    pub birth: u64,
}

impl LaneWriter {
    /// This process, read on each call (never cached, so a forked child is not its parent).
    #[must_use]
    pub fn this_process() -> Self {
        let pid = std::process::id();
        let birth = i32::try_from(pid)
            .ok()
            .and_then(aterm_uds::process::read_birth)
            .map_or(0, |birth| birth.0);
        Self { pid, birth }
    }

    /// Whether this writer still runs: a process of its pid lives and, where the writer's birth
    /// was read, was born then (a pid reused by a later process is not it, nor is one whose birth
    /// cannot be read now, as the writer's could). A writer whose birth was unreadable is taken
    /// to run while its pid lives.
    #[must_use]
    pub fn runs(self) -> bool {
        let Some(pid) = i32::try_from(self.pid).ok().filter(|pid| *pid > 0) else {
            return false;
        };
        if self.birth == 0 {
            return aterm_uds::process::pid_alive(self.pid);
        }
        aterm_uds::process::read_birth(pid).is_some_and(|birth| birth.0 == self.birth)
    }
}

/// Records where this process's automatic apply lane stands ([`ApplyLaneRecord`]) under
/// `current_build`.
#[cfg(target_os = "macos")]
pub fn record_apply_lane(current_build: u64, record: &ApplyLaneRecord) {
    let Some(staging) = paths::Staging::resolve() else {
        return;
    };
    health::Health::record_apply_lane(
        &staging.health(),
        current_build,
        LaneWriter::this_process(),
        record,
    );
}

/// Non-macOS: no apply lane exists, so there is nothing to record.
#[cfg(not(target_os = "macos"))]
pub fn record_apply_lane(_current_build: u64, _record: &ApplyLaneRecord) {}

/// Withdraws this process's [`ApplyLaneRecord`] about `target_build`, once the latch it reported
/// is gone.
#[cfg(target_os = "macos")]
pub fn withdraw_apply_lane(target_build: u64) {
    let Some(staging) = paths::Staging::resolve() else {
        return;
    };
    health::Health::withdraw_apply_lane(
        &staging.health(),
        LaneWriter::this_process(),
        target_build,
    );
}

/// Non-macOS: no apply lane exists, so there is nothing to withdraw.
#[cfg(not(target_os = "macos"))]
pub fn withdraw_apply_lane(_target_build: u64) {}

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
    /// Bounded, terminal-escaped diagnostic context; never a policy input.
    pub last_refusal_detail: String,
    /// RFC3339 UTC of [`Self::last_refusal`].
    pub last_refusal_at: String,
    /// The running build's boot trial waiting for a window, in words, with when the wait ends.
    pub trial_wait: String,
    /// The last timed handoff capture, the first freeze rung it seeded and the last landed park.
    pub handoff_capture: Option<HandoffCaptureRecord>,
    /// Where the automatic apply lane stands, recorded by this process under the running build.
    pub lane: Option<ApplyLaneRecord>,
    /// The bytes a window here saw fail to start, which the windowless lanes leave for a window:
    /// only where they are a build newer than the running one, its armed trial's, or its
    /// headless-settled bytes awaiting a window.
    pub window_start_failure: Option<WindowStartFailure>,
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
    let window_start_failure = install::window_start_failure_for_report(&staging, current_build);
    Some(ApplyLaneReport {
        trial_wait,
        handoff_capture,
        window_start_failure,
        lane: ledger.apply_lane_for(current_build, LaneWriter::this_process()),
        last_failure: ledger.last_apply_error,
        last_failure_target_build: ledger.last_apply_failure_target_build,
        failures_for_target: ledger.apply_failures_for_target,
        last_refusal_detail: if standing {
            health::diagnostic_detail(&ledger.last_apply_refusal_detail)
        } else {
            String::new()
        },
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

/// Asks whether the running build's boot trial is settled. With `checkpoint` (a window's first
/// frame, a headless instance's control socket) it runs the trial's recovery proof under the
/// apply lock and confirms, or settles at the bound [`BootHealthVerdict`] states; that ask
/// also releases the rollback bundle an ended window's proven settle kept. Without it it
/// reads only: nothing is locked, written, removed or counted, and only a process that owes
/// no trial is [`BootHealthVerdict::Confirmed`].
#[cfg(target_os = "macos")]
#[must_use]
pub fn confirm_boot_health_exact(
    current_build: u64,
    current_commit: &str,
    witness: TrialWitness,
    checkpoint: bool,
) -> BootHealthVerdict {
    install::confirm_boot_health(current_build, Some(current_commit), witness, checkpoint)
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

/// Linux confirms its exact inode trial at a `checkpoint`, and before one only reads whether
/// a trial is owed; elsewhere nothing is on trial.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn confirm_boot_health_exact(
    _current_build: u64,
    _current_commit: &str,
    _witness: TrialWitness,
    _checkpoint: bool,
) -> BootHealthVerdict {
    #[cfg(target_os = "linux")]
    {
        linux::confirm(_current_build, _current_commit, _checkpoint)
    }
    #[cfg(not(target_os = "linux"))]
    {
        BootHealthVerdict::NO_TRIAL
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
    preverify_installed_locked_with_enter(
        staging,
        app_root,
        current_build,
        expected_build,
        expected_commit,
        read_policy,
        || install::enter_preverify(staging.map(|staging| staging.apply_lock.as_path())),
    )
}

#[cfg(target_os = "macos")]
fn preverify_installed_locked_with_enter<R>(
    staging: Option<&paths::Staging>,
    app_root: &std::path::Path,
    current_build: u64,
    expected_build: u64,
    expected_commit: &str,
    read_policy: &dyn Fn(&std::path::Path) -> R,
    enter: impl FnOnce() -> Result<install::PreverifyGuard, String>,
) -> Result<R, String> {
    let running_trial_unconfirmed = || {
        staging.is_some_and(|staging| {
            install::owned_running_trial_unconfirmed(staging, app_root, current_build)
        })
    };
    if running_trial_unconfirmed() {
        return Err(install::unconfirmed_trial_refusal(current_build));
    }
    let _preverify = enter()?;
    if running_trial_unconfirmed() {
        return Err(install::unconfirmed_trial_refusal(current_build));
    }
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

/// What [`install_over_failed_activation`] found and did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailedActivationInstall {
    /// No activation the window here failed to switch to waits with a newer stage ready to take
    /// its place: the usual answer, read from a few small files. It is also the answer where the
    /// look found one and the same look under the apply lock no longer does.
    NoneFailed,
    /// One waits, and stays installed for now, by a refusal a later look may not repeat: a moment
    /// (a busy apply lock, a verification that did not finish, a window launch of it that may
    /// still switch to it, the disk changing while its bundles were verified), or a gate read from
    /// small files before any bundle was verified (the stage's roster confirmation, this process
    /// running neither build). A later look costs what this one did.
    Held(SupersedeRefusal),
    /// One waits, and what its bundles' verified signatures show, judged under the apply lock,
    /// holds it there: the install path or the retained predecessor sealed with another build or
    /// failing verification with a verdict (not a passing refusal), this process running neither,
    /// or a trial that names other bytes or belongs to another install. Only a change to those
    /// bundles or that trial lets a later look through, so a caller need not verify the same
    /// `(activation, newer)` pair again.
    Refused(String),
    /// It was retired, and the newer stage installed over the build retained for its rollback,
    /// which stays retained.
    Installed {
        /// The retire.
        retired: SupersededActivation,
        /// The build now at the install path, on a fresh trial.
        installed_build: u64,
    },
    /// It was retired and the retained build is back at the install path, but the newer stage
    /// did not go in; the staged lane applies it as any stage.
    RetiredOnly {
        /// The retire.
        retired: SupersededActivation,
        /// Why the stage did not go in.
        why: String,
    },
}

/// The activation a window here failed to switch to, with the stage newer than it that is ready:
/// `(activation, newer)`, read without the apply lock or a signature check. `None` is the usual
/// answer.
#[cfg(target_os = "macos")]
#[must_use]
pub fn failed_activation_waiting() -> Option<(u64, u64)> {
    install::failed_activation_waiting()
}

/// Non-macOS: there is no installed `.app` activation.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn failed_activation_waiting() -> Option<(u64, u64)> {
    None
}

/// A process that is not a window (the node service) takes the install path from an activation
/// the window here could not switch to, and installs the strictly newer verified stage over the
/// build retained for its rollback, which stays retained. It moves nothing while the activation's
/// trial is confirmed, a window launch of it runs or came after the window's last failure, or the
/// stage is not admitted (held, withdrawn, unverified). It verifies the bundles before it
/// takes the apply lock, so the lock is held for the exchanges, the start probe and cheap
/// re-reads.
#[cfg(target_os = "macos")]
#[must_use]
pub fn install_over_failed_activation(
    current_build: u64,
    current_commit: &str,
) -> FailedActivationInstall {
    install::install_over_failed_activation(current_build, current_commit)
}

/// Non-macOS: there is no installed `.app` activation to replace.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn install_over_failed_activation(
    _current_build: u64,
    _current_commit: &str,
) -> FailedActivationInstall {
    FailedActivationInstall::NoneFailed
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
        hold: Option<&update_record::Hold>,
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
        Some(pending_update_overdue_notice(h, hold, current_build))
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
fn pending_update_overdue_notice(
    h: &health::Health,
    hold: Option<&update_record::Hold>,
    current_build: u64,
) -> (String, String) {
    let held = hold
        .filter(|hold| hold.build == h.pending_build)
        .map(|hold| {
            format!(
                "it is held until {} ({}, try {})",
                hold::until_words(hold),
                hold.why,
                hold.tries
            )
        });
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
    let cause = match held {
        Some(held) => format!("{held}; {cause}"),
        None => cause,
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
    if withdrawn(staging, h.pending_build) {
        health::Health::forget_pending_update(&staging.health(), h.pending_build)
    } else {
        h
    }
}

/// Whether `build` is withdrawn in `staging`: below the operator floor. A hold does not withdraw
/// a build: it still waits, and its pending clock keeps running until the hold ends.
#[cfg(target_os = "macos")]
fn withdrawn(staging: &paths::Staging, build: u64) -> bool {
    build < manifest::Floor::read(&staging.floor()).min_build
}

#[cfg(all(test, target_os = "macos"))]
#[path = "lib_persistent_notice_tests.rs"]
mod persistent_notice_tests;

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

/// Start this process's background checker once [`automatic_at_launch`] answers
/// ([`once_automatic_known`]): the platform's start reads [`automatic`] and runs no scheduled
/// check where it is off. The files earlier updaters kept a verdict in are retired first
/// ([`retire_old_records`]), whether or not checks are on.
pub fn spawn_background_check_with_settings(
    current_build: u64,
    provider: CheckSettingsProvider,
    notify: Option<HealthNotify>,
    on_staged: Option<StagedNotify>,
    on_checked: Option<CheckedNotify>,
) {
    retire_old_records();
    once_automatic_known("automatic update checks", move || {
        start_background_check(current_build, provider, notify, on_staged, on_checked);
    });
}

/// How often a start waiting on [`automatic_at_launch`] asks again.
const LAUNCH_SETTING_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// Work run once, on whichever thread gets to it.
type RunOnce = Box<dyn FnOnce() + Send>;

/// A start that waits on [`automatic_at_launch`]: the lane it starts, and the start.
type LaunchStart = (&'static str, RunOnce);

/// What a wait on the launch setting runs on: a re-read of it, a sleep between re-reads, and a
/// thread to do both on ([`LaunchWait::of_this_process`] in aterm, stand-ins in a test).
#[derive(Clone)]
struct LaunchWait {
    read_again: std::sync::Arc<dyn Fn() -> Option<bool> + Send + Sync>,
    sleep: std::sync::Arc<dyn Fn(std::time::Duration) + Send + Sync>,
    spawn: std::sync::Arc<dyn Fn(RunOnce) -> std::io::Result<()> + Send + Sync>,
}

impl LaunchWait {
    /// The file's re-read ([`automatic_at_launch_read`]), the wall clock, and a named thread.
    fn of_this_process() -> Self {
        Self {
            read_again: std::sync::Arc::new(automatic_at_launch_read),
            sleep: std::sync::Arc::new(std::thread::sleep),
            spawn: std::sync::Arc::new(|wait: RunOnce| {
                std::thread::Builder::new()
                    .name("aterm-update-setting".into())
                    .spawn(wait)
                    .map(drop)
            }),
        }
    }

    /// Start a thread that re-reads every [`LAUNCH_SETTING_RETRY`] and runs `start` at the read
    /// that answers, whatever it says (`start` reads it itself). Gives `start` back where no thread
    /// started.
    fn begin(&self, lane: &'static str, start: RunOnce) -> Result<(), (RunOnce, std::io::Error)> {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(start)));
        let waiting = std::sync::Arc::clone(&slot);
        let wait = self.clone();
        let started = (self.spawn)(Box::new(move || {
            let mut reads: u64 = 1;
            loop {
                (wait.sleep)(LAUNCH_SETTING_RETRY);
                reads += 1;
                if (wait.read_again)().is_some() {
                    aterm_log::info!(
                        "aterm-update: [update] enabled read at attempt {reads}; {lane} go on as \
                         it says"
                    );
                    if let Some(start) = take_start(&waiting) {
                        start();
                    }
                    return;
                }
            }
        }));
        match started {
            Ok(()) => Ok(()),
            // A spawn that failed ran nothing, so the start is still in its slot.
            Err(error) => take_start(&slot).map_or(Ok(()), |start| Err((start, error))),
        }
    }
}

fn take_start(slot: &std::sync::Mutex<Option<RunOnce>>) -> Option<RunOnce> {
    slot.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

use aterm_types::MutexExt as _;

/// Starts whose wait on [`automatic_at_launch`] got no thread, and when to try one again.
struct OwedLaunchStarts {
    starts: Vec<LaunchStart>,
    retry_at: Option<std::time::Instant>,
}

/// This process's [`OwedLaunchStarts`]: [`start_owed_launch_waits`] answers them.
static OWED_LAUNCH_STARTS: std::sync::Mutex<OwedLaunchStarts> =
    std::sync::Mutex::new(OwedLaunchStarts {
        starts: Vec::new(),
        retry_at: None,
    });

/// Start `lane` once [`automatic_at_launch`] answers ([`once_known`]); where no thread could wait
/// for it, the start is owed to [`start_owed_launch_waits`].
fn once_automatic_known(lane: &'static str, start: impl FnOnce() + Send + 'static) {
    let now = std::time::Instant::now();
    if let Some(owed) = once_known(
        lane,
        automatic_at_launch(),
        &LaunchWait::of_this_process(),
        Box::new(start),
    ) {
        let mut owed_now = OWED_LAUNCH_STARTS.lock_or_recover();
        owed_now.starts.push(owed);
        owed_now.retry_at = Some(now + LAUNCH_SETTING_RETRY);
    }
}

/// Answer the starts whose wait on [`automatic_at_launch`] got no thread, with no file read: run
/// them once the setting has answered, else, from [`owed_launch_starts_due`] on, try a waiting
/// thread for each again. The timers that already watch the update checker call it (the window's
/// checker watch, a headless node's update loop); a terminal session has none, and its owed starts
/// wait for its next launch.
pub fn start_owed_launch_waits() {
    if OWED_LAUNCH_STARTS.lock_or_recover().starts.is_empty() {
        return;
    }
    start_owed(
        &OWED_LAUNCH_STARTS,
        automatic_kept(),
        std::time::Instant::now(),
        &LaunchWait::of_this_process(),
    );
}

/// When [`start_owed_launch_waits`] has work, for the window to wake for: `None` while nothing is
/// owed.
#[must_use]
pub fn owed_launch_starts_due() -> Option<std::time::Instant> {
    let owed = OWED_LAUNCH_STARTS.lock_or_recover();
    if owed.starts.is_empty() {
        return None;
    }
    owed.retry_at
}

/// [`start_owed_launch_waits`] over `owed`, given what the setting has `kept`, at `now`.
fn start_owed(
    owed: &std::sync::Mutex<OwedLaunchStarts>,
    kept: Option<bool>,
    now: std::time::Instant,
    wait: &LaunchWait,
) {
    let due = {
        let mut owed = owed.lock_or_recover();
        if owed.starts.is_empty() || (kept.is_none() && owed.retry_at.is_some_and(|at| now < at)) {
            return;
        }
        owed.retry_at = Some(now + LAUNCH_SETTING_RETRY);
        std::mem::take(&mut owed.starts)
    };
    let mut still = Vec::new();
    for (lane, start) in due {
        if kept.is_some() {
            aterm_log::info!("aterm-update: [update] enabled read; {lane} go on as it says");
            start();
            continue;
        }
        match wait.begin(lane, start) {
            Ok(()) => {
                aterm_log::info!("aterm-update: a thread now waits on [update] enabled for {lane}")
            }
            Err((start, _)) => still.push((lane, start)),
        }
    }
    owed.lock_or_recover().starts.extend(still);
}

/// Run `start` at once where the setting is `known_now`, else on a thread `wait` begins. A process
/// whose first read of `[update] enabled` hit a fault that can pass neither runs a scheduled check
/// over a person's `false` it could not read, nor, in a window or a headless node, leaves its
/// checks off for the rest of its life once the fault passes. Gives `start` back, as `lane`'s,
/// where no thread began, for [`start_owed`].
fn once_known(
    lane: &'static str,
    known_now: Option<bool>,
    wait: &LaunchWait,
    start: RunOnce,
) -> Option<LaunchStart> {
    if known_now.is_some() {
        start();
        return None;
    }
    aterm_log::warn!(
        "aterm-update: [update] enabled could not be read; {lane} wait, and aterm asks again \
         every {} s",
        LAUNCH_SETTING_RETRY.as_secs()
    );
    match wait.begin(lane, start) {
        Ok(()) => None,
        Err((start, error)) => {
            aterm_log::warn!(
                "aterm-update: no thread to wait on [update] enabled ({error}); a window or \
                 headless node tries again, and a terminal session's {lane} wait for its next \
                 launch"
            );
            Some((lane, start))
        }
    }
}

/// The platform's start of [`spawn_background_check_with_settings`].
fn start_background_check(
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

#[cfg(test)]
#[path = "lib_launch_setting_tests.rs"]
mod launch_setting_tests;

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
        wake: &check_wake::SYSTEM_WOKE,
        wake_settle: cadence::WAKE_SETTLE,
    });
    start_checker_once(
        &CHECKER_ARGS,
        &checker_watch::WATCH,
        args,
        spawn_checker_thread,
    );
}

/// Publish one process-wide argument set before reserving its initial worker.
/// A repeated startup cannot retire that worker or replace its restart arguments.
#[cfg(target_os = "macos")]
fn start_checker_once(
    stored: &std::sync::OnceLock<std::sync::Arc<CheckerArgs>>,
    watch: &checker_watch::CheckerWatch,
    args: std::sync::Arc<CheckerArgs>,
    spawn: impl FnOnce(u64, std::sync::Arc<CheckerArgs>) -> std::io::Result<()>,
) {
    if stored.set(std::sync::Arc::clone(&args)).is_err() {
        return;
    }
    let generation = watch.register(
        cadence::Cadence::new(std::time::Duration::from_secs(cadence::INTERVAL_SECS))
            .max_wait()
            .as_secs(),
    );
    if let Err(error) = spawn(generation, args) {
        watch.unstarted(generation);
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
fn asked_check<T>(automatic: bool, sweep: &UncheckedSweep, check: impl FnOnce() -> T) -> T {
    let result = check();
    if !automatic {
        sweep.start();
    }
    result
}

/// Tell the background check that the system just woke.
#[cfg(target_os = "macos")]
pub fn note_system_woke() {
    check_wake::SYSTEM_WOKE.post();
}

/// Observe the host's settled network changes during the existing checker
/// wait. The consuming probe must do no network or filesystem work.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn set_network_change_probe(probe: Box<dyn Fn() -> bool + Send + Sync>) {
    check_wake::SYSTEM_WOKE.set_network_probe(probe);
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
const LANE_BUSY_LOG_EVERY: u64 = 12 * 60 / cadence::LANE_BUSY_RETRY.as_secs();

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
            let Some(_worker) = checker_watch::WATCH.worker(generation) else {
                return;
            };
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
            checker_watch::WATCH.unstarted(next);
            warn(&format!(
                "the update check stopped answering; aterm could not start a fresh one: {error}"
            ));
            None
        }
    }
}

/// Replace an enrolled Linux checker through the same host watchdog.
#[cfg(target_os = "linux")]
pub fn respawn_stalled_checker(stalled_generation: u64) -> Option<u64> {
    linux::respawn_stalled_checker(stalled_generation)
}

/// No checker runs on unsupported platforms.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
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

/// A process that took an update lock (`checker.lock`, `stage.lock`, `apply.lock`) writes its pid
/// into it.
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

/// The pid the last process to take an update lock wrote into it ([`note_checker_lock_holder`]).
#[cfg(target_os = "macos")]
fn checker_lock_holder(path: &std::path::Path) -> Option<u32> {
    let pid: u32 = parse_uint(read_ledger_text(path)?.trim())?;
    let raw = i32::try_from(pid).ok().filter(|raw| *raw > 0)?;
    // SAFETY: signal 0 delivers nothing; it only asks whether `raw` exists.
    let exists = unsafe { libc::kill(raw, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    exists.then_some(pid)
}

/// Take `apply.lock`, waiting at most `wait`, naming this process as its holder; a wait that times
/// out resumes a stopped holder ([`resume_stopped_lock_holder`]).
#[cfg(target_os = "macos")]
pub(crate) fn take_apply_lock(
    path: &std::path::Path,
    wait: std::time::Duration,
) -> std::io::Result<aterm_update_core::FileLock> {
    take_apply_lock_with(path, wait, is_an_aterm)
}

/// [`take_apply_lock`], asking `is_aterm` whether a stopped holder is one.
#[cfg(target_os = "macos")]
fn take_apply_lock_with(
    path: &std::path::Path,
    wait: std::time::Duration,
    is_aterm: impl FnOnce(u32) -> bool,
) -> std::io::Result<aterm_update_core::FileLock> {
    match aterm_update_core::FileLock::acquire_within(path, wait) {
        Ok(lock) => {
            note_checker_lock_holder(path);
            Ok(lock)
        }
        Err(error) => {
            if error.kind() == std::io::ErrorKind::TimedOut {
                resume_stopped_lock_holder(path, is_aterm);
            }
            Err(error)
        }
    }
}

/// Continue the process the held update lock at `path` names, if a stop is what holds it.
///
/// `aterm update check` at Ctrl-Z, or any interactive `aterm` whose background check holds the lock
/// when its job stops, holds that lock until it runs again, and every aterm on this Mac waits on it.
/// The lock itself stays: taking it from a live process would break one swap per build, so the
/// holder finishes what it was doing and its guard lets go. The file names whoever last wrote it,
/// which is not always who holds it now, so only ever `SIGCONT`, only to another aterm that has this
/// lock open, and only after a wait timed out. A holder that stops again on its terminal (a job left
/// in the background that reads it) gets its next run at the next wait that times out. Returns
/// whether it sent one.
#[cfg(target_os = "macos")]
fn resume_stopped_lock_holder(path: &std::path::Path, is_aterm: impl FnOnce(u32) -> bool) -> bool {
    let Some(pid) = checker_lock_holder(path).filter(|pid| *pid != std::process::id()) else {
        return false;
    };
    if !aterm_uds::process::is_stopped(pid) || !is_aterm(pid) {
        return false;
    }
    if !aterm_uds::holders::scan_file_holders(path, &[]).is_ok_and(|scan| scan.holds(pid)) {
        return false;
    }
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: no pointer; `raw` is a live process of this account that the checks above named.
    let sent = unsafe { libc::kill(raw, libc::SIGCONT) } == 0;
    if sent {
        log(&format!(
            "{}: pid {pid}, which it names and which has it open, was stopped, so it is resumed to \
             finish and let go",
            path.display()
        ));
    }
    sent
}

/// Whether `pid` runs an `aterm` executable, as a holder of an update lock does.
#[cfg(target_os = "macos")]
fn is_an_aterm(pid: u32) -> bool {
    matches!(
        aterm_uds::holders::exe_of(pid),
        aterm_uds::holders::Exe::At(path)
            if path.file_name().is_some_and(|name| name == "aterm")
    )
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
    let _progress = progress::checker_scope(watch, generation);
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
            let record = hold::load(&staging);
            let held = record.live_hold(install::unix_now_secs());
            if let Some((title, body)) =
                announcer.tick_overdue(&h, held, &now, current_build, automatic_apply_on())
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
            Some(pause) => {
                cadence::sleep_watching_for_wake(args.wake, wake_seen.get(), pause, || {
                    watch.is_current(generation)
                })
            }
            None => cadence::wait(schedule, args.wake, wake_seen.get(), watch, generation).1,
        }
    };
    let settle = |schedule: &mut cadence::Cadence,
                  failures: &mut cadence::FailureLog,
                  woke_at: &mut Option<std::time::Instant>| {
        settle_after_wake(
            schedule,
            failures,
            woke_at,
            args.wake_settle,
            args.wake,
            &wake_seen,
            std::thread::sleep,
        );
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
                        schedule.hold_until(crate::instant_after(
                            std::time::Instant::now(),
                            std::time::Duration::from_secs(until.saturating_sub(now_unix)),
                        ));
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
                                    crate::instant_after(
                                        now,
                                        std::time::Duration::from_secs(
                                            until.saturating_sub(now_unix),
                                        ),
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
                    // A check stuck on the lane hides no stage a sibling or the node job wrote.
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
                }
            }
            if strand_notice.due(unreadable::is_stranded()) && args.has_health_hook() {
                let (title, body) = unreadable::notification();
                args.send_health(title, body);
            }
            speak_health(&mut announcer);
            if !stamp(CheckerPhase::Waiting) {
                return Cycle::Superseded(last.get());
            }
            let next = schedule.after_lane(lane_busy.consecutive() != 0);
            if matches!(wait(&next), cadence::Waited::Woke(_)) {
                settle(&mut schedule, &mut failures, &mut woke_at);
            }
            Cycle::Next
        }));
        match cycle {
            Ok(Cycle::Next) => {}
            Ok(Cycle::Superseded(phase)) => return CheckerExit::Superseded { phase },
            Err(payload) => {
                // A panic's message can quote what the cycle held (a release's or a config's
                // text): aterm.log names it by its kind and length, and the panic hook keeps
                // the message on stderr and in the crash report (DESIGN-session-log §3.6).
                warn(&format!(
                    "an update check cycle panicked ({}); the checker carries on after a \
                     backed-off wait",
                    aterm_log::words::Panic::of(payload.as_ref())
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
/// Acknowledge only posts already present, so a wake during settling remains pending.
#[cfg(target_os = "macos")]
fn settle_after_wake(
    schedule: &mut cadence::Cadence,
    failures: &mut cadence::FailureLog,
    woke_at: &mut Option<std::time::Instant>,
    settle: std::time::Duration,
    wake: &cadence::WakeSignal,
    seen: &std::cell::Cell<u64>,
    sleep: impl FnOnce(std::time::Duration),
) {
    seen.set(wake.posts());
    log(&format!(
        "wake signal or network change; checking for updates in {}s",
        settle.as_secs()
    ));
    schedule.woke();
    *failures = cadence::FailureLog::default();
    *woke_at = Some(std::time::Instant::now());
    sleep(settle);
}

/// The installed bundle's build number (`CFBundleVersion`).
#[cfg(target_os = "macos")]
fn installed_build_observed(app_root: &std::path::Path) -> Result<u64, String> {
    observed_build_with(
        &INSTALLED_BUILD,
        app_root,
        verify::bundle_build_number_from_file,
    )
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
    read: impl FnOnce(std::fs::File) -> Result<u64, String>,
) -> Result<u64, String> {
    use std::os::unix::fs::MetadataExt as _;
    // Keep the cache identity and helper input on one file across bundle-path swaps.
    let (file, metadata) = aterm_uds::regular_file::open(&app_root.join("Contents/Info.plist"))
        .map_err(|error| format!("open installed Info.plist: {error}"))?;
    let identity = |m: std::fs::Metadata| {
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
    };
    let before = identity(metadata);
    let lock = || {
        cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if let Some((seen, build)) = lock().as_ref()
        && &before == seen
    {
        return Ok(*build);
    }
    let build = read(
        file.try_clone()
            .map_err(|error| format!("copy installed Info.plist handle: {error}"))?,
    )?;
    let after = file.metadata().ok().map(identity);
    *lock() = (after.as_ref() == Some(&before)).then_some((before, build));
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
    /// Availability reported by the producer: [`status`] uses platform support on
    /// macOS and also requires readable enrollment state on Linux. It does not
    /// read the live `[update] enabled` preference; a GUI control response replaces
    /// this field with that preference after obtaining its live settings snapshot.
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
    /// The update record's hold on a build newer than the running one: live, or lapsed and kept
    /// as history until a start, a newer build or a person ends it.
    pub hold: Option<HoldView>,
    /// The update record's window-start failure where it names the exact bytes of the staged
    /// build, newer than the running one ([`HoldView::start_failure_of`]), live or lapsed: while
    /// it holds, no start without a window (a headless boot, the node job) installs that stage.
    pub start_failure: Option<HoldView>,
    /// How the last hold ended.
    pub last: Option<update_record::Ended>,
    /// The newest build the check lane saw.
    pub offered: Option<u64>,
    /// Whether a build newer than the running one was offered and is not running yet.
    pub behind: bool,
}

/// The update record's hold, as status reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldView {
    pub build: u64,
    pub version: String,
    pub cause: update_record::Cause,
    pub witness: update_record::Witness,
    pub why: String,
    /// How many failures of this artifact in a row (1 for the first).
    pub tries: u8,
    /// When it lapses (unix seconds).
    pub until: u64,
    /// Whether it holds now; a lapsed hold means the build is tried again.
    pub live: bool,
    /// Whether another Mac's proven start of it cut it short.
    pub peer_lifted: bool,
}

impl HoldView {
    /// The held build as a person names it: its version, or "build N" where none was recorded.
    #[must_use]
    pub fn name(&self) -> String {
        if self.version.is_empty() {
            format!("build {}", self.build)
        } else {
            self.version.clone()
        }
    }

    /// `record`'s hold on a build newer than `running`, as of `now`.
    #[must_use]
    pub fn of(record: &update_record::UpdateRecord, running: u64, now: u64) -> Option<Self> {
        record
            .hold
            .as_ref()
            .filter(|hold| hold.build > running)
            .map(|hold| Self::view(hold, now))
    }

    /// `record`'s window-start failure ([`update_record::UpdateRecord::start_failure`]) where it
    /// names the exact bytes of the stage of build `staged` (`staged_sha256`, its DMG's digest in
    /// hex of either case) and that build is newer than `running`, as of `now`: what keeps every
    /// start with no window from installing that stage while it holds
    /// ([`update_record::UpdateRecord::holds_windowless`]). It is named by the stage's `version`
    /// where the failure recorded none, as a window books none.
    #[must_use]
    pub fn start_failure_of(
        record: &update_record::UpdateRecord,
        running: u64,
        staged: u64,
        version: &str,
        staged_sha256: &str,
        now: u64,
    ) -> Option<Self> {
        let failed = record.start_failure.as_ref().filter(|failed| {
            failed.build == staged
                && staged > running
                && update_record::Sha::parse(staged_sha256.trim()) == Some(failed.sha)
        })?;
        let mut view = Self::view(failed, now);
        if view.version.is_empty() {
            version.clone_into(&mut view.version);
        }
        Some(view)
    }

    /// `hold` as status reports it at `now`.
    fn view(hold: &update_record::Hold, now: u64) -> Self {
        Self {
            build: hold.build,
            version: hold.version.clone(),
            cause: hold.cause,
            witness: hold.witness,
            why: hold.why.clone(),
            tries: hold.tries,
            until: hold.until,
            live: hold.live(now),
            peer_lifted: hold.peer_lifted,
        }
    }
}

/// This copy's update record file, where the platform keeps one.
#[must_use]
pub fn record_path() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        paths::Staging::resolve().map(|staging| staging.record())
    }
    #[cfg(target_os = "linux")]
    {
        linux::record_path()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Delete, once per process, the files earlier updaters kept a verdict in (the quarantine
/// ledger, the failed-candidate memo and the window-start failure record, `paths::RETIRED`),
/// best effort. Nothing reads them and nothing is imported from them: a build they held is
/// tried again on the next check. Every start of the background checker (the node's, a
/// window's, a terminal session's) calls it, and every check retires them from its own
/// staging root.
pub fn retire_old_records() {
    #[cfg(target_os = "macos")]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            if let Some(staging) = paths::Staging::resolve() {
                staging.retire();
            }
        });
    }
}

/// The node service ends its run because the running `build`'s boot trial could not prove its
/// listener, for `why`: record the miss and its reason in the update record before the run ends,
/// so a revert the next launches make holds the build with that reason. Returns whether it was
/// noted (no trial of `build` armed, or a record that could not be written, notes nothing).
#[cfg(target_os = "macos")]
pub fn note_trial_miss(build: u64, why: &str) -> bool {
    paths::Staging::resolve().is_some_and(|staging| install::note_trial_miss(&staging, build, why))
}

/// A person or an agent asks to try again (`aterm update retry`, Settings' Try Now): the hold
/// and the window-start failure end ([`update_record::EndHow::Person`]), so the next check
/// stages the held build at once and a headless start may install bytes a window saw fail.
/// Returns the hold it ended (the hold where both held), or `None` when nothing was held.
///
/// # Errors
/// The record could not be written.
pub fn retry() -> Result<Option<update_record::Hold>, String> {
    let Some(path) = record_path() else {
        return Ok(None);
    };
    let now = unix_now();
    update_record::UpdateRecord::edit(&path, |record| {
        let hold = record
            .live_hold(now)
            .or_else(|| record.live_start_failure(now))
            .cloned();
        record
            .end_hold(update_record::Ending::Person, now)
            .then_some(hold)
            .flatten()
    })
    .map(|(_, ended)| ended)
    .map_err(|error| {
        format!(
            "could not write the update record {}: {error}",
            path.display()
        )
    })
}

/// Whether a status `outcome` says this copy is up to date: the one claim [`held_outcome`] never
/// lets stand while a newer build is held or offered, and the only words that speak for the
/// channel on a surface.
#[must_use]
pub fn says_up_to_date(outcome: &str) -> bool {
    outcome
        .trim_start()
        .get(..10)
        .is_some_and(|head| head.eq_ignore_ascii_case("up to date"))
}

/// What the update record adds to a status report of `running` at `now`: its hold on a newer
/// build, whether the copy is behind the newest offer, and `outcome` as [`held_outcome`] lets
/// it stand.
#[must_use]
pub fn hold_report(
    record: &update_record::UpdateRecord,
    running: u64,
    outcome: String,
    now: u64,
) -> (Option<HoldView>, bool, String) {
    let hold = HoldView::of(record, running, now);
    let behind = record.offered.is_some_and(|offered| offered > running);
    let outcome = held_outcome(outcome, hold.as_ref(), behind, now);
    (hold, behind, outcome)
}

/// `outcome` as status reports it at `now`: never "up to date" while a newer build is held or
/// the copy is behind the newest offer the floors admit.
#[must_use]
pub fn held_outcome(outcome: String, hold: Option<&HoldView>, behind: bool, now: u64) -> String {
    if !says_up_to_date(&outcome) {
        return outcome;
    }
    match hold {
        Some(hold) if hold.live => format!(
            "held: aterm {} is held until {} ({}, try {}); `aterm update retry` tries it now",
            hold.name(),
            update_record::clock_words(hold.until, now),
            hold.why,
            hold.tries
        ),
        Some(hold) => format!(
            "behind: aterm {} was held ({}) and is tried again on the next check",
            hold.name(),
            hold.why
        ),
        None if behind => {
            "behind: a newer release is out and not installed yet; an update check stages it"
                .to_string()
        }
        None => outcome,
    }
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
            hold: None,
            start_failure: None,
            last: None,
            offered: None,
            behind: false,
        }
    }
}

/// The loop's schedule after a healthy check ([`after_healthy_check`]): the one-minute re-check
/// while the head in flight's count is within `cadence::IN_FLIGHT_CHECKS`, else the base interval.
#[cfg(all(test, target_os = "macos"))]
#[path = "lib_after_healthy_check_tests.rs"]
mod after_healthy_check_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "lib_sibling_stage_tests.rs"]
mod sibling_stage_tests;

#[cfg(test)]
#[path = "lib_status_summary_tests.rs"]
mod status_summary_tests;

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
    Some(status_at(
        &staging,
        current_build,
        bundle::resolve().is_some(),
    ))
}

/// [`status`] of the update root `staging`, for a copy that is `installable` or not. Every field
/// is read from the machine's ledgers, which each process reads alike: whether the channel is
/// unreadable is the status ledger's mark of the last check, made by whichever process made
/// that check ([`unreadable::NOTE`]), for this build.
#[cfg(target_os = "macos")]
fn status_at(staging: &paths::Staging, current_build: u64, installable: bool) -> UpdateStatus {
    let (mut checked_from_build, mut outcome) = (current_build, String::new());
    let mut stranded = false;
    let updated_at = check_receipt::completed_at(staging).unwrap_or_default();
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
        stranded = v.get("delivery").and_then(aterm_toml::Value::as_str) == Some(unreadable::NOTE);
    }
    let channel_unreadable = stranded && checked_from_build == current_build;
    let mut ready = manifest::Ready::read_publishable(staging);
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
    let record = hold::load(staging);
    let now = install::unix_now_secs();
    let (hold, behind, held) = hold_report(
        &record,
        current_build,
        reconciliation.outcome.into_string(),
        now,
    );
    outcome = held;
    let start_failure = ready.as_ref().and_then(|ready| {
        HoldView::start_failure_of(
            &record,
            current_build,
            ready.build_number,
            &ready.version,
            &ready.dmg_sha256,
            now,
        )
    });
    let h = health::Health::read(&staging.health());
    UpdateStatus {
        linux: None,
        enabled: enabled(),
        installable,
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
        channel_unreadable,
        hold,
        start_failure,
        last: record.last,
        offered: record.offered,
        behind,
    }
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
/// An already-running check is reported without queuing another request behind it.
#[cfg(target_os = "macos")]
pub fn check_now(current_build: u64, source: &Source) -> UpdateStatus {
    // Retain the more specific diagnosis when the watchdog already retired
    // the check that still holds the lane.
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
        let completed = asked_check(automatic(), &UNCHECKED_SWEEP, || {
            check_lane().run_or_join(current_build, source, || {
                if let Err(error) =
                    github::check_and_stage_for(current_build, source, github::Asked::Person)
                {
                    warn(&format!("manual update check failed: {error}"));
                }
            })
        });
        if !completed {
            let why = "an update check is already running; showing its current status".to_string();
            return status(current_build).map_or_else(
                || UpdateStatus::empty(true, current_build, why.clone()),
                |mut status| {
                    status.outcome = why.clone();
                    status
                },
            );
        }
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
/// still holds this process's check lane. This supplies a more specific diagnosis
/// than the general busy result.
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
#[path = "lib_log_capture_tests.rs"]
pub(crate) mod log_capture;

#[cfg(all(test, target_os = "macos"))]
#[path = "lib_checker_gate_tests.rs"]
mod checker_gate_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "lib_checker_skip_tests.rs"]
mod checker_skip_tests;

/// The per-cycle read of the installed bundle's build number spawns PlistBuddy only when the
/// plist moved, and the floor that yanks it is a gate read.
#[cfg(all(test, target_os = "macos"))]
#[path = "lib_installed_build_tests.rs"]
mod installed_build_tests;

#[cfg(test)]
#[path = "lib_rfc3339_age_tests.rs"]
mod rfc3339_age_tests;

#[cfg(test)]
#[path = "lib_commit_match_tests.rs"]
mod commit_match_tests;

#[cfg(test)]
#[path = "lib_team_pin_tests.rs"]
mod team_pin_tests;

/// A policy this process could not read just then refuses the pre-verification as a passing
/// condition.
#[cfg(all(test, target_os = "macos"))]
#[path = "lib_handoff_candidate_facts_tests.rs"]
mod handoff_candidate_facts_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "lib_node_start_tests.rs"]
mod node_start_tests;
