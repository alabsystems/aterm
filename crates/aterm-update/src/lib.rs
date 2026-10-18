// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm-update`: silent, signature-pinned in-app self-update for the macOS
//! `aterm.app` (and, through [`linux`], an enrolled Linux executable).
//!
//! On macOS, two entry points apply only to an eligible installed `.app` with
//! updating enabled. Linux enrollment and transactions live in [`linux`].
//!
//! * [`apply_staged_if_ready_preserving_fds_exact`]: call very early in `main()`,
//!   before any window or thread. If a previous run staged a verified, newer build,
//!   it atomically swaps it into place and re-execs the new binary (same PID, tty
//!   and parent); otherwise it returns. This is the one boot apply entry point.
//! * [`spawn_background_check_with_settings`]: call once the GUI is up. A detached
//!   thread finds a newer release on the channel, downloads and verifies it, and
//!   stages it for the in-session apply lane (the GUI hands the live session over
//!   in place); the next launch applies it only if no handoff completes. It never
//!   touches the UI or blocks the event loop.
//!
//! # Delivery model
//!
//! * Staging: the checker checks at launch, then every `cadence::INTERVAL_SECS`
//!   (±20% jitter; a check that could not reach the network retries sooner, on
//!   `cadence::OFFLINE_RETRY`), so a running app stages a release within one
//!   jittered interval plus the download.
//! * Applying: in-session through the overlap handoff, at the first quiet moment
//!   and at the latest within `aterm-gui`'s `LANDS_WITHIN` of the stage, or on one
//!   click. The top of the next `main()` is the fallback for a stage no handoff
//!   completed.
//!
//! A machine that never runs aterm is never updated, but the first launch after a
//! release stages it and the running app applies it: at worst one apply behind.
//!
//! The app schedules its own checks; no LaunchAgent is installed. Explicit
//! `aterm update check` uses the same binary and staging locks.
//!
//! # Trust model
//!
//! Two gates always hold:
//! 1. No downgrade: the candidate's build number is strictly greater than the
//!    running build and never below the persisted monotonic `min_build`/high-water
//!    floor (blocks replay, rollback and yanked builds).
//! 2. Integrity: the downloaded container's SHA-256 equals the manifest's.
//!
//! Authenticity comes from every configured tier (defense in depth):
//!
//! * Tier REPO (a fork with no master of its own): "it came from my channel
//!   repository over TLS", plus a structural `codesign --verify` (an ad-hoc
//!   signature suffices; arm64 requires one to run) to catch corruption. No Apple
//!   anchor, Team ID or notarization.
//! * Tier ROSTER (this tree): the paper master is compiled in
//!   (`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`), so every release must carry
//!   the master-signed machine roster and an Ed25519 appcast signature by a machine
//!   the roster names and has not revoked (`github::authorize_by_roster`). The
//!   manifest pins the SHA-256 of every downloadable container (DMG and zip), so
//!   this authenticates whichever one is staged, even against a repo-write
//!   attacker. `atpkg` verifies its index under the same roster.
//! * Tier APPLE (optional, additive): if [`PINNED_TEAM_ID`] is set, `codesign
//!   --verify` also runs with a designated requirement pinning the Apple anchor,
//!   Developer-ID chain and Team ID, plus `spctl -a -t exec` notarization.
//!
//! Everything shells out to `codesign`/`spctl`/`hdiutil`/`ditto`/`curl`; the
//! container's SHA-256 is `aterm_update_core::sha256_file`'s (in process, on the CPU's
//! SHA-256 instructions) and the Ed25519 checks are `aterm_update_core::roster`'s.
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

#[cfg(target_os = "macos")]
mod bundle;
// The schedule is portable; every target tests the real cadence against the
// watchdog budget, even though the background loop currently ships on macOS.
#[cfg(any(target_os = "macos", test))]
mod cadence;
/// The schedule's constants and [`STALE_CHECK_AFTER`], compiled on every target.
mod cadence_bounds;
#[cfg(target_os = "macos")]
mod check_lane;
#[cfg(target_os = "macos")]
mod check_receipt;
/// The background check loop's heartbeat and the pure judgement of it that the
/// window's watchdog, `aterm ctl update status` and the tests share.
pub mod checker_watch;
/// A dev build's standing against the public channel: one read-only HEAD of the
/// evergreen appcast, at the window's start and daily.
pub mod dev_channel;
/// The durability flushes every atomic publish in this crate takes.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod durable;
#[cfg(unix)]
mod exec_plain;
/// Who raised an update floor, so a revocation can take it back: the one rule both
/// lanes' floors keep.
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
/// Compiled into macOS test builds too, so a macOS test run covers its transaction
/// tests (each one that needs the Linux kernel itself is marked so). Never into a
/// Windows one: the lane is Unix through and through (`std::os::unix`, `libc`'s
/// open flags, [`floor_sources`]), and `verify cells` builds this crate's tests
/// for the Windows cells.
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub mod linux;
/// What the Linux background check tells the window about update health: the pure
/// rule `linux`'s loop applies, compiled into every test build like `linux_trial`.
#[cfg(any(target_os = "linux", test))]
mod linux_notice;
/// When one more start of a replaced Linux executable rolls it back: the pure rule
/// `linux` applies, compiled into every test build so the macOS tests run it too.
#[cfg(any(target_os = "linux", test))]
mod linux_trial;
#[cfg(target_os = "macos")]
mod manifest;
#[cfg(target_os = "macos")]
mod paths;
mod progress;

/// Machine-readable compiled identity; no installation or updater state access.
pub fn binary_identity_json(
    identity: &aterm_update_core::linux::BinaryIdentity,
) -> Result<String, String> {
    aterm_json::to_string(identity).map_err(|error| error.to_string())
}
/// How each run of the installed app ended (the PTY keeper's census,
/// `docs/DESIGN-pty-keeper-2026-09-26.md` §7). `aterm-gui` writes a row per windowed
/// launch; `aterm doctor` reads them. Portable: a platform with no crash marker
/// has no rows.
pub mod recovery_ledger;
// Not macOS-only: every platform relaunches aterm (see the module's own doc).
mod relaunch;
#[cfg(target_os = "macos")]
mod status;
#[cfg(target_os = "macos")]
mod sys;
#[cfg(target_os = "macos")]
mod unreadable;
#[cfg(target_os = "macos")]
mod verify;
// The checker's supervision: the real loop, driven to each way it could stall
// silently.
#[cfg(all(test, target_os = "macos"))]
mod checker_supervision_tests;
// Every platform names the copy of aterm it is running
// (`docs/DESIGN-which-copy-runs-2026-08-27.md`); only the other-copy probe is `.app`-shaped.
pub mod which_copy;

// The FailedMark writer/reader suppression projection (proof/test builds only),
// re-exported at the crate root so `manifest`'s `#[refines]` anchors can name it
// as `aterm_update::…`, like `status_reconciliation_projection`.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
pub use manifest::failed_mark_suppression_projection;

// The roster-freshness projection (proof and test builds only), named by the
// `#[refines]` anchors on the swap's stage gates and the check's roster observation.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
pub use github::roster_freshness_projection;

/// The plain `exec` that puts SIGPIPE back when it fails (`exec_plain`): the GUI's
/// cold-lane replacement continues as the window after a refused exec.
#[cfg(unix)]
pub use exec_plain::exec_keeping_sigpipe;

/// Hard cap for every small TOML ledger/marker consumed by the updater. The cap is
/// checked on the opened descriptor before allocation or parsing, then enforced again
/// with `take` so a concurrent grow cannot turn a local marker into unbounded work.
#[cfg(target_os = "macos")]
const MAX_LEDGER_BYTES: u64 = 256 * 1024;

#[cfg(target_os = "macos")]
fn read_ledger_text(path: &std::path::Path) -> Option<String> {
    read_ledger_text_checked(path).ok().flatten()
}

/// [`read_ledger_text`] that keeps the one distinction a READ-THEN-REPLACE writer
/// needs: `Ok(None)` is a file that is absent or is not a ledger at all (not a
/// regular file, over the cap, not UTF-8) — the same answer every time it is asked —
/// while `Err` is a present file the read could not get through this time (`EMFILE`,
/// `EIO`, a mode this uid cannot open). A writer that starts from defaults on `Err`
/// replaces a record it never saw.
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

/// The resolved GitHub release source and the compiled-in default owner/repo,
/// re-exported unchanged from [`aterm_update_core`]: the GUI calls
/// `aterm_update::Source::resolve(..)` and passes the result to
/// [`spawn_background_check_with_settings`], so it must be the same type.
pub use aterm_update_core::{DEFAULT_OWNER, DEFAULT_REPO, Source};

pub use progress::{Progress, ProgressNotify, set_progress_observer};

/// Re-exported for every OTHER lane that re-launches aterm forwarding its own
/// argv (the GUI's cold-exec/seamless/Windows successor spawns): strip the
/// leading `--window` mode pins so no relaunch path can re-grow the argv the
/// boot swap pins exactly once. A RUNNING window's successor forwards its flags
/// and nothing its launch asked for once (`running_window_successor_args`).
pub use relaunch::{
    RELAUNCH_OF_RUNNING_ENV, reexec_forwarded_args, running_window_successor_args,
    window_successor_args,
};

/// The Apple Developer Team ID for the optional Tier APPLE anchor, from the
/// committed constant `aterm_update_core::pins::APPLE_TEAM_ID` (never the build
/// environment). Empty does not disable the updater; it skips the
/// codesign/notarization anchor and leaves the repo and roster tiers. Non-empty
/// additionally requires the swapped bundle be Developer-ID signed by this team.
pub const PINNED_TEAM_ID: &str = aterm_update_core::pins::APPLE_TEAM_ID;

/// Runtime raise of the Tier APPLE anchor, from `[update] require_team_id` in the
/// GUI config. Set once, early in `main`, by [`set_required_team_id`].
static REQUIRED_TEAM_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Opts in to Developer-ID and notarization enforcement at runtime, without a
/// rebuild.
///
/// Shipped aterm pins a real Team ID, so [`PINNED_TEAM_ID`] is non-empty and this
/// is a no-op there. For forks and self-hosted builds with an empty pin (structural
/// `codesign --verify` only), it lets a deployment require a Developer ID from
/// settings.
///
/// One-way, so a config file can never downgrade a compiled trust anchor:
///
/// * compiled pin non-empty ⇒ the compiled pin wins; this call is ignored;
/// * compiled pin empty + `Some(team)` ⇒ that team is now required;
/// * compiled pin empty + `None`/blank ⇒ unchanged (structural-only).
///
/// Backed by a `OnceLock`: only the first call takes effect, so a later config
/// reload cannot loosen the anchor mid-session.
pub fn set_required_team_id(team: Option<&str>) {
    if !PINNED_TEAM_ID.is_empty() {
        return;
    }
    let Some(team) = team.map(str::trim).filter(|t| !t.is_empty()) else {
        return;
    };
    let _ = REQUIRED_TEAM_ID.set(team.to_string());
}

/// The Team ID verification must satisfy: the compiled-in pin if any, else what
/// [`set_required_team_id`] installed, else empty (structural-only Tier REPO).
/// Every bundle-verification call site reads this, never [`PINNED_TEAM_ID`], so no
/// call site can bypass the runtime opt-in.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn effective_team_id() -> &'static str {
    if !PINNED_TEAM_ID.is_empty() {
        return PINNED_TEAM_ID;
    }
    REQUIRED_TEAM_ID.get().map_or("", String::as_str)
}

/// SHA-256 of a raw 32-byte Ed25519 key, for shipping introspection.
///
/// `Ok(None)` is the explicit no-pin state; an invalid or non-32-byte pin is an
/// error, never reported as absent. Hashes the decoded key bytes signature
/// verification consumes, not a base64 literal.
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

/// The update pin: SHA-256 of the raw 32 bytes of `pins::PAPER_MASTER_PUBKEYS[0]`
/// as 64 lowercase hex, `empty` when the roster tier is unarmed (a fork), or
/// `invalid` for a malformed pin. `aterm-gui`'s `build.rs` embeds the same value in
/// the Mach-O `__aterm_upin` record and the release cutter checks every shipped
/// slice carries it; after a master rotation it tells a stranded client from a
/// healthy one.
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

/// Outcome of an [`apply_staged_if_ready_preserving_fds_exact`] call. Every variant
/// is non-fatal: the caller keeps launching the current build. A successful apply
/// never returns (it `exec`s the new binary), so there is no "applied" variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Not an installed `.app` launch (dev build / `cargo run` / translocated /
    /// run from a mounted DMG), or the updater is disabled. No-op.
    NotApplicable,
    /// Nothing staged, or what's staged is not strictly newer. No-op.
    NoUpdate,
    /// A newer build is staged but cannot be applied now (e.g. the install location
    /// is not writable, or apply-time re-verification failed). The stage is kept
    /// for a later attempt; carries a reason.
    Deferred(String),
    /// The bundle was swapped but re-exec into the new binary failed; the swap
    /// was rolled back and the caller should keep running the old build.
    ReExecFailed(String),
}

/// What a launch of this build can PROVE about its boot trial — handed to the boot
/// apply, which counts the launch, and to the health confirmation, which settles
/// the trial.
///
/// macOS keeps ONE trial per swapped build, and the build it proves is the app a
/// person opens: its windows. A `--headless` launch proves the engine and the
/// control socket and nothing about the window path; were it to confirm the trial,
/// it would delete the retained predecessor while the build's windows could still
/// crash before their first frame, leaving a crash-looping release with nothing to
/// revert to. Linux keeps the same shape per lane ([`linux_session_started`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialWitness {
    /// A launch that opens windows.
    Window,
    /// A `--headless` launch: counted toward only a trial no window has launched —
    /// the headless self-update lane, whose successor is itself headless — and
    /// settling only its headless path. Its confirmation of such a trial disarms it
    /// (later updates are not held) but keeps the previous build for the build's
    /// next window launch, which puts it back on trial; one a
    /// window has launched it leaves for a window altogether.
    Headless,
}

/// Whether the updater runs on this platform at all: macOS or Linux. Every lane a PERSON starts
/// reads this and nothing more — `aterm update check` / Settings ▸ Software Update's
/// Check for Updates ([`check_now`]), Update Now and `aterm ctl update apply` (the
/// window's apply lane), and the launch-time swap of a build already staged. It
/// requires no pinned anchor — the default Tier REPO works with none (see the
/// crate-level trust model), so an internal build with no Apple Developer ID and no
/// signing key still self-updates. Dev-build inertness comes from [`bundle::resolve`],
/// not from a missing pin, and has two sources there: the LAYOUT (a `cargo run` /
/// `target/` binary is not an installed `.app`) and the codesign-sealed
/// [`bundle::DEV_BUILD_KEY`] mark (a local build installed in place, which layout alone
/// cannot distinguish from a release). Linux instead requires explicit enrollment
/// of its safe executable path and always authenticates signed native artifacts;
/// merely running a checkout does not enroll it. Other platforms are unsupported.
///
/// Not the setting: "Check for updates automatically" (`[update] enabled`) is
/// [`automatic`]'s and switches off only the background checker, so checks and
/// applies a person asks for keep working with it off.
#[must_use]
pub fn enabled() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// Whether the running copy is a dev-marked bundle (`ATermDevBuild` in its
/// `Info.plist`, written by `targo --unverified ship dev-app`), which the updater leaves alone
/// wherever it sits, so a "move it into Applications" remedy would be wrong. One
/// canonicalize and one bounded plist read. `false` for a bare binary, a disk-image
/// or translocated launch, and off macOS.
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

/// "Check for updates automatically" (`[update] enabled`, Settings ▸ Software
/// Update, read once per process by [`aterm_update_core::settings::update_enabled`])
/// on a platform the updater runs on ([`enabled`]). It gates the one lane nobody
/// asked for: the background checker ([`spawn_background_check_with_settings`]) in
/// the window and every terminal session, and, in a dev-marked copy, the window's
/// daily look at the channel ([`dev_channel::spawn_watch`]). Applying a staged
/// build is `[update] auto_apply`'s question.
#[must_use]
pub fn automatic() -> bool {
    enabled() && aterm_update_core::settings::update_enabled()
}

/// Applies a staged update if one is ready and strictly newer than `current_build`
/// (the running build number). On success this does not return: it re-execs the
/// swapped-in binary. See the crate-level trust model.
///
/// Inherited PTY/proof descriptors (`handoff_fds`) stay CLOEXEC for every
/// verification helper and are exposed only to the updater's final exec image.
/// `handoff_env` rides the same contract: authority variables the caller's
/// prearm consumed out of the ambient environment (so no helper can see them),
/// restored exclusively onto the final exec image — the re-exec'd successor
/// must re-validate the inherited handoff and cannot without them. The canonical
/// OLD rollback source and health trial are bound to `current_commit`, the
/// compiled git commit of the running binary, preventing
/// same-build/different-source reuse.
///
/// `witness` is what this launch can prove about a boot trial ([`TrialWitness`]).
#[cfg(target_os = "macos")]
#[must_use]
pub fn apply_staged_if_ready_preserving_fds_exact(
    current_build: u64,
    current_commit: &str,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target_is_this_build: bool,
    witness: TrialWitness,
) -> ApplyOutcome {
    install::apply_staged_if_ready(
        current_build,
        Some(current_commit),
        handoff_fds,
        handoff_env,
        handoff_target_is_this_build,
        witness,
    )
}

/// The clause an apply-path refusal carries when its cause can clear by itself: a
/// verification helper past its deadline or the apply budget, a helper the kernel
/// would not start just then (`EAGAIN`, `ENOMEM`, `EMFILE`, `ENFILE`), or the apply
/// lock held by another process past its bounded wait.
///
/// A clause in the text rather than a type because these refusals reach every
/// reader as a `String` (the apply lane's `Result<_, String>`,
/// `ApplyOutcome::Deferred`, the GUI's cached pre-park verdict, the ledger's
/// `last_failure`), and the readers that decide how long to wait (`aterm-gui`'s
/// physical-failure shape, [`refusal_needs_person`]) hold only that string. It
/// keeps a timeout on a loaded machine from reading as a structural verdict or a
/// person's to clear.
///
/// Minted through one spelling, `verify::mark_passing`, where each cause is
/// decided (`verify.rs`'s `timed_out`, helper spawns and `lock_wait_refusal`;
/// a full disk in `install.rs` and the check's download), so the classification
/// cannot drift from the wording. A reason without it keeps the verdict it had.
pub const PASSING_REFUSAL_KEY: &str = "a passing condition, not a verdict on the update";

/// The key every "the install location cannot be written" refusal carries (the
/// boot swap's step 5, the activation supersede, the pre-park check), so
/// [`refusal_needs_person`] and the GUI's wording can name it.
pub const INSTALL_LOCATION_REFUSAL_KEY: &str = "install location not writable";

/// True when a refusal is the non-writable install location's
/// ([`INSTALL_LOCATION_REFUSAL_KEY`]).
#[must_use]
pub fn is_install_location_refusal(reason: &str) -> bool {
    reason.contains(INSTALL_LOCATION_REFUSAL_KEY)
}

/// Whether an apply-path refusal is a passing moment ([`PASSING_REFUSAL_KEY`]):
/// retried on the transient schedule, never a structural verdict, never a
/// person's to clear.
#[must_use]
pub fn is_passing_refusal(reason: &str) -> bool {
    reason.contains(PASSING_REFUSAL_KEY)
}

/// The words every "this stage waits for an update check to confirm its signer"
/// refusal carries: the swap's gate 4d, and
/// the pre-park checks that ask the same question
/// (`install::roster_freshness_gate`, `Ready::roster_authorization_stale`). Kept
/// here, beside [`PASSING_REFUSAL_KEY`], so a reader of the refusal on any platform
/// can name it ([`is_roster_confirmation_wait`]).
pub const ROSTER_UNCONFIRMED_KEY: &str = "confirmed against a current machine roster";

/// Whether an apply-path refusal is the wait for an update check to confirm the
/// stage's roster authorization ([`ROSTER_UNCONFIRMED_KEY`]), always a PASSING one.
///
/// Not a moment the next attempt may win, and never a failure: the refusal lasts
/// until a check reads the machine roster, which no apply attempt does, so a lane
/// that retried it on the transient schedule spent its budget (and, in the window,
/// took the manual-only latch and the failing alarm's streak) on what was only a
/// check that had not run yet — offline, or still starting after a launch. The
/// window's automatic lane HOLDS on it instead and goes on when a check reports the
/// stage.
#[must_use]
pub fn is_roster_confirmation_wait(reason: &str) -> bool {
    is_passing_refusal(reason) && reason.contains(ROSTER_UNCONFIRMED_KEY)
}

/// Whether a refusal is a person's to clear: the installed bundle cannot be the
/// swap's rollback source (`install::rollback_source_refusal`), its folder cannot
/// be written (`install::install_location_refusal`), or the app does not run from
/// an installed bundle; nothing can be swapped in until the app or its folder
/// changes.
///
/// Never a passing refusal: a check that did not finish says nothing about the
/// bundle, so a reason carrying [`PASSING_REFUSAL_KEY`] never reads as a person's.
#[cfg(target_os = "macos")]
pub fn refusal_needs_person(reason: &str) -> bool {
    !is_passing_refusal(reason)
        && (install::is_rollback_source_refusal(reason)
            || is_install_location_refusal(reason)
            || reason.contains("does not run from an installed bundle"))
}

#[cfg(not(target_os = "macos"))]
pub fn refusal_needs_person(_reason: &str) -> bool {
    false
}

/// Overlap-handoff pre-park verification: authenticates the staged candidate
/// (codesign policy plus sealed build/commit rebinding, bound to the authorized
/// artifact identity) and proves the bundle it would replace can become the
/// swap's rollback source, while the caller's PTY readers are all still live. The
/// handoff child re-runs the complete gate at swap time; this only moves the first
/// verdict out of the parked window so a doomed candidate never parks a reader. See
/// `install::preverify_staged_handoff_candidate` and
/// `install::preverify_installed_rollback_source`.
///
/// `Ok` carries the candidate's handoff policy file
/// ([`aterm_update_core::handoff_policy`]) and whether its sealed `Info.plist`
/// declares the chunked rendezvous grant ([`HandoffCandidateFacts`]), read from the
/// verified stage after every check passed.
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

/// The code-signing identifier of a handoff candidate: the staged `.app`, or, for an
/// activation, the installed bundle `activation` names. The launched lane's rendezvous
/// admits only a dialer whose code carries an aterm release's identifier, a clause the
/// candidate's own check (`codesign -R` with the Developer ID requirement) does not
/// test; reading it before the launch lets a candidate signed otherwise take the fork
/// lane, whose gate does not ask, instead of being launched only to be turned away at
/// its dial. A display of the bundle the pre-verification (this worker's, or the cached
/// pass) authenticated; it grants nothing.
///
/// # Errors
/// No staging root, or `codesign` did not answer (a passing refusal when it
/// ended on a moment).
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

/// WHAT A CACHED PASS CANNOT VOUCH FOR, for a handoff whose full
/// pre-verification a fresh cached pass let the worker skip: `Some(refusal)`
/// when `target_build` is below the operator apply floor this machine's staging
/// root records — the floor ratchets on
/// OBSERVATION, so it can rise inside the pass's freshness window — and, for a
/// STAGED candidate (`staged_commit` is the commit the pass was cached for), when
/// that stage is no longer the published one (a check that observes a revocation
/// retires it). Either way the cached pass would otherwise
/// park every reader for a successor whose own gate finds nothing to apply. An
/// activation (`None`) has no stage, so it is asked the floor and whether the
/// install path still holds it (another window may have retired it since the pass
/// was cached). Small reads. Without a staging root an
/// activation has nothing yanked (`None`), and a staged candidate has no stage,
/// which refuses.
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

/// The longest one handoff pre-verification runs ([`preverify_staged_for_handoff`],
/// [`preverify_installed_for_handoff`]): the bounded wait for the apply lock plus
/// the verification budget opened once it is held. Every helper answers inside that
/// budget or is a passing refusal. A caller waiting on a pre-verification derives
/// its ceiling from this.
#[cfg(target_os = "macos")]
pub const HANDOFF_PREVERIFY_BOUND: std::time::Duration = std::time::Duration::from_secs(
    install::APPLY_LOCK_WAIT.as_secs() + verify::APPLY_BUDGET.as_secs(),
);

/// Off macOS a pre-verification checks nothing and returns at once.
#[cfg(not(target_os = "macos"))]
pub const HANDOFF_PREVERIFY_BOUND: std::time::Duration = std::time::Duration::ZERO;

/// What an outgoing build learns from a handoff candidate's bundle ONCE the
/// pre-verify has authenticated it — never from a bundle that failed a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffCandidateFacts {
    /// The candidate's signed handoff policy.
    pub policy: aterm_update_core::handoff_policy::PolicyRead,
    /// The candidate's sealed `Info.plist` declares the chunked rendezvous grant
    /// ([`HANDOFF_GRANT_CHUNKS_KEY`]): it can take more sessions than one
    /// descriptor message carries. Read from the bytes the signature check
    /// verified.
    pub grant_chunks: bool,
}

impl HandoffCandidateFacts {
    /// Read both facts off a bundle that has just passed its checks — or refuse, as a
    /// passing condition, when either file was not READ ([`Self::admitted`]).
    #[cfg(target_os = "macos")]
    pub(crate) fn read(app_root: &std::path::Path) -> Result<Self, String> {
        Self::admitted(
            aterm_update_core::handoff_policy::read_from_bundle(app_root),
            bundle::declares_grant_chunks(app_root),
        )
    }

    /// The facts as a pre-verification may pass them on: a file this process could
    /// not read just then (no descriptor left, an I/O error) is refused as a passing
    /// condition, so it is never cached with the candidate's pass as a verdict — a
    /// policy as "the successor asks nothing" (`PolicyRead::Unread`), and the
    /// `Info.plist` as "the successor takes no chunked grant" (`grant_chunks` an
    /// `Err`: both are read off the same verified bundle at the same instant, and
    /// both are cached beside the pass).
    /// The handoff worker files it transient and the next attempt reads them again.
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
/// The only overlap lane off macOS is the same-binary `ATERM_DEBUG_SEAMLESS_REEXEC`
/// QA path, which skips pre-verify.
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

/// Records that a staged build failed to become the running build, durably and
/// visibly to `aterm-ctl update status`. The apply lane is the GUI's, but the
/// ledger is this crate's, so the GUI reports outcomes here.
///
/// `reason` should be the typed outcome (`ChildDied`, `AdoptionMismatch`,
/// `ActivityRevoked`, `TimedOut`, `PreparationFailed`, `re-exec failed`): it decides
/// between "the machine was too busy" and "these two builds cannot hand off".
/// `target_build` names the artifact the attempt was trying to reach; see
/// [`health::Health::apply_failures_for_target`].
///
/// Returns what it just wrote, so a surface can report the first failure without
/// waiting for a later re-read. `None` means nothing was recorded (no staging
/// root), never "zero failures". Best-effort: observability never blocks or fails
/// an update.
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

/// Exactly what one [`record_apply_failure`] left in the ledger, read back inside
/// the lock scope that wrote it, so a concurrent `expire_stale_apply_streak` cannot
/// make it report a different streak.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordedApplyFailure {
    /// The ESCALATION streak after this write (`UpdateStatus::failing_applies`).
    pub apply_failures: u32,
    /// Consecutive failures of [`Self::target_build`] after this write.
    pub failures_for_target: u32,
    /// The artifact this failure was about (0 when the caller did not know one).
    pub target_build: u64,
    /// The stored reason, already truncated exactly as the ledger stores it.
    pub reason: String,
    /// Whether the ledger now reads as persistently failing — the loud verdict, so
    /// a surface can escalate at the write instead of at the next check.
    pub persistent: bool,
}

/// Records that an apply was refused (blocked, deferred or held) rather than
/// attempted and failed.
///
/// Refusals stay out of the failure streaks (a busy terminal must not escalate),
/// but they must be observable: this writes the reason to the health ledger
/// ([`apply_lane_report`], the control socket's `apply_refusal=`) and stops the
/// status outcome advertising the stage as though nothing had tried to apply it.
///
/// `reason` must name what refused and why. Best-effort and request-rate only (it
/// takes the ledger lock and writes two small files): explicit apply requests and
/// terminal verdicts, never a per-frame or per-poll path.
#[cfg(target_os = "macos")]
pub fn record_apply_refusal(current_build: u64, reason: &str) {
    let Some(staging) = paths::Staging::resolve() else {
        return;
    };
    health::Health::record_apply_refusal(&staging.health(), current_build, reason);
    // Name the artifact when one is publishable. `status::record` re-derives
    // `staged_build` from the same marker; if the marker moves between the two
    // reads, a newer stage just landed and the next status write re-derives both.
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

/// What the ledger knows about the seamless handoff's freeze: the last timed dry
/// run of the park's capture (taken while a successor booted, over how many
/// sessions, the first freeze rung it seeded, the build that took it) and the last
/// landed park's real cost. See [`health::Health::handoff_capture_us`] and
/// `handoff_park_us`.
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
    /// The last landed park's real cost, the readers' stop to a capture ready
    /// to hand over, in microseconds (0 when none was kept).
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

/// Persists one timed handoff capture in the health ledger, so the next update
/// seeds its first freeze rung from it and `update status` can say where the rung
/// came from. Best-effort and request-rate: once per update attempt, off the GUI's
/// main thread (the ledger lock is a file lock another process may hold). A failed
/// write leaves the rung at its default.
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

/// Persists what one landed seamless park cost (the readers' stop to a capture
/// ready to hand over), so the next update's first freeze rung is budgeted from the
/// real freeze, which the dry run cannot time. Best-effort, request-rate, off the
/// GUI's main thread like [`record_handoff_capture`].
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

/// What the ledger holds about the handoff's freeze (the last timed capture and the
/// last landed park), or `None` with neither or no staging root. A prior, not a
/// verdict: the caller clamps what it seeds from it, so a stale or foreign number
/// costs at most the rung's ceiling.
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
///
/// Kept out of [`UpdateStatus`], which is the check lane's projection; the lanes
/// fail independently. The escalation streak is absent because
/// `UpdateStatus::failing_applies` is its one source; this carries the reasons and
/// the artifact they are about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyLaneReport {
    /// Reason of the most recent apply failure; empty when none is standing.
    pub last_failure: String,
    /// The build [`Self::last_failure`] was trying to reach (0 when unknown); with
    /// [`Self::failures_for_target`], the consecutive attempts on that artifact.
    /// `failing_applies` cannot answer this: it is bound to the running build, so a
    /// newer download would inherit the count and reason of the one it replaced.
    pub last_failure_target_build: u64,
    /// See [`Self::last_failure_target_build`].
    pub failures_for_target: u32,
    /// Reason of the most recent apply REFUSAL — a verdict that stopped an apply
    /// before it could fail. Empty once an apply succeeds or hard-fails.
    pub last_refusal: String,
    /// RFC3339 UTC of [`Self::last_refusal`]; empty when there is none.
    pub last_refusal_at: String,
    /// The running build's boot trial WAITING FOR A WINDOW, in words, with when the
    /// wait ends (law L5): a window of this
    /// build has started and only a window can confirm or revert it, so its
    /// headless launches settle nothing until then. Empty when the build is not in
    /// that wait. Read from the trial's own records on every report, never stored,
    /// so it ends with the wait and never takes the place of [`Self::last_refusal`],
    /// which another running build may be keeping its own reason in. While a
    /// launch's confirmation of the armed trial has been refused by its recovery
    /// proof, it says that instead, in the past tense with when it was first refused
    /// and the proof's reason (`health::Health::trial_proof_failure`), and that a
    /// window of this build retries it.
    pub trial_wait: String,
    /// The last timed handoff capture, the first freeze rung it seeded and the
    /// last landed park; `None` until one was measured. Not bound to the running
    /// build: it measures the machine, and the next update seeds from it.
    pub handoff_capture: Option<HandoffCaptureRecord>,
}

/// Reads the apply lane's durable record as it applies to `current_build`.
///
/// A refusal recorded by a build that is no longer running is dropped: a
/// successful in-session apply execs away without clearing the slot, so the
/// running build is the expiry. `None` only when there is no staging root.
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

/// Non-macOS: `witness` is not read — a Linux launch that opens no window never runs
/// this (it starts through [`linux_session_started`]), and the lanes keep their own
/// record.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn apply_staged_if_ready_preserving_fds_exact(
    _current_build: u64,
    _current_commit: &str,
    _handoff_fds: &[i32],
    _handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    _handoff_target_is_this_build: bool,
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

/// Confirms the running build reached a healthy checkpoint (window up / first frame):
/// clears the boot-health sentinel and keeps the retained rollback bundle for the window
/// that started from it, binding both to the compiled git commit of the running binary;
/// the first launch after that window is gone deletes it. Call **once, from the GUI, after
/// deep init** so a crash BEFORE this point is caught and auto-reverted by
/// [`apply_staged_if_ready_preserving_fds_exact`]'s boot-health check on the next
/// launch(es), while a crash AFTER it is a normal fault the updater ignores.
/// Idempotent, best-effort, and a no-op when the last launch was not a self-update. A
/// `--headless` launch ([`TrialWitness::Headless`]) settles only the headless path of a
/// trial no window has launched, keeping the previous build for the next window launch
/// to re-trial; one a window launched is left for a window. Either way this answers
/// `true` — nothing for the caller to retry.
#[cfg(target_os = "macos")]
#[must_use]
pub fn confirm_boot_health_exact(
    current_build: u64,
    current_commit: &str,
    witness: TrialWitness,
) -> bool {
    install::confirm_boot_health(current_build, Some(current_commit), witness)
}

/// A terminal session of this executable is starting (an interactive launch): on
/// Linux it counts against a replaced executable's trial, as a window's launch does
/// ([`apply_staged_if_ready_preserving_fds_exact`]), and returns whether this launch
/// is a start of that trial; the caller then confirms it with
/// [`confirm_linux_session`] once the session proves healthy (its shell's first
/// output; staying up silent proves nothing), so a terminal-only copy can
/// confirm an update. `false` elsewhere, and on Linux when there is nothing to
/// prove: no trial, a confirmed one, or a process running some other file.
///
/// A `--headless` GUI instance starts through here instead of the boot apply: it
/// opens no window, so it proves what a session proves, and its
/// [`confirm_boot_health_exact`] confirms the session lane, never the window's.
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

/// How long a terminal session that counted a start of a replaced Linux executable's
/// trial waits for its shell's first output before it defers the trial's verdict
/// ([`defer_linux_session_verdict`]): half the window in which a start may still
/// confirm itself (`linux_trial::START_SETTLES_SECS`, 20 s; a test pins the half), so
/// the move has the other half to land in: a try that finds the update lock held (a
/// sibling's start hashing the executable) is tried again every second until it lands,
/// the stamp it moves settles, or the shell speaks.
pub const LINUX_SESSION_SILENT_DEFER: std::time::Duration = std::time::Duration::from_secs(10);

/// A terminal session [`linux_session_started`] counted as one of a pending trial's
/// starts has been up for [`LINUX_SESSION_SILENT_DEFER`] and its shell has printed
/// nothing yet: on Linux, move the trial's stamp forward ONCE, if this start's is still
/// the one the trial holds, so the next start over the budget (a tab opened before the
/// first prompt) is judged against the move rather than rolling a slow-starting healthy
/// build back for good. It confirms NOTHING: a build that hangs before its shell speaks
/// is rolled back one window later, and only the shell's first output confirms
/// ([`confirm_linux_session`]). `true` when it moved; `false` everywhere else.
///
/// A try that did not land (the update lock busy past its half second, a read that
/// failed) is tried again after `pause(span)`, which waits `span` and answers `false`
/// to stop — once the shell has spoken — until the stamp the move would move is no
/// longer young (a single try, lost to a sibling's hash of the executable, would
/// leave the burst to be rolled back after all).
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

/// Read, now, the path of the executable this process runs, which the Linux update
/// lane keeps for the life of the process. The GUI entry calls it where the boot apply
/// ([`apply_staged_if_ready_preserving_fds_exact`]) would have read it, for a launch
/// whose start is counted only later ([`linux_window_opened`]): read first after a
/// sibling aterm replaced the executable, the path names the unlinked file
/// (`<path> (deleted)`), and the process then runs no background check and reports
/// only that refusal until it exits. A no-op off Linux, and after the first read.
pub fn linux_capture_executable() {
    #[cfg(target_os = "linux")]
    linux::capture_context();
}

/// A WINDOWED Linux launch has opened its display (its event loop is built): count
/// it against a replaced executable's trial, as the boot apply
/// ([`apply_staged_if_ready_preserving_fds_exact`]) counts a launch whose display
/// answered before it opened. The GUI entry calls it for a launch whose display it
/// could not vouch for beforehand (an `ssh -X` forward, or nothing answering where
/// the window will connect), so a launch that cannot open a display is no start of
/// the trial and a forwarded display that opens still is. `NotApplicable` off Linux.
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
    /// Confirmed, or nothing was waiting on this process (no trial, one already
    /// confirmed, another file running). A session's confirmation proves the
    /// session lane only: the window lane keeps counting its own starts until a
    /// window confirms, and an unconfirmed window start never refuses a session.
    Done,
    /// Could not be recorded (the update lock was busy, a read failed): try again.
    Failed,
}

/// A terminal session of this executable proved healthy (its shell's first output)
/// after [`linux_session_started`] said it is a start of a pending trial: on Linux,
/// confirms that trial for the session lane ([`LinuxSessionConfirm`] says what came of
/// it). A window confirms with [`confirm_boot_health_exact`], since a session's prompt
/// proves nothing about the window lane. [`LinuxSessionConfirm::Done`] everywhere else.
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

/// Linux confirms its exact inode trial — a `--headless` instance its session lane,
/// by the record its start left ([`linux_session_started`]), so `witness` is not
/// read; other platforms have nothing to confirm.
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

/// Sealed installed-bundle identity plus the durable exact-artifact receipt written
/// by the most recent successful swap. A returned apply may claim "installed" only
/// when all four values bind to its ticket; build number alone is insufficient.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledUpdateFacts {
    pub build_number: u64,
    pub git_commit: String,
    /// The bundle's marketing version (`CFBundleShortVersionString`), display only:
    /// what the update screen names when the installed bundle is newer than the
    /// running process and about to be activated. `None` when the plist lacks it.
    pub version: Option<String>,
    pub receipt_build_number: Option<u64>,
    pub receipt_dmg_sha256: Option<String>,
    /// The installed build is below the operator apply floor (`Floor::min_build`, a
    /// yank). Still reported, but activating it is refused at every gate, and the
    /// GUI does not stage it as one.
    pub yanked: bool,
}

/// Collect installed provenance and the trial marker. This may spawn PlistBuddy and
/// read a ledger, so GUI callers must invoke it only on their updater facts worker.
/// `None` both when there is no installed bundle and when it could not be PROVEN
/// this pass; a reader that must tell those apart reads
/// [`installed_update_facts_checked`].
#[cfg(target_os = "macos")]
#[must_use]
pub fn installed_update_facts() -> Option<InstalledUpdateFacts> {
    installed_update_facts_checked().ok().flatten()
}

/// [`installed_update_facts`], with WHY nothing is reported: `Ok(None)` when there
/// is no installed bundle layout, `Err` with the refusal when one is there and its
/// verification or sealed plist read did not pass. A refusal carrying
/// [`PASSING_REFUSAL_KEY`] says the check did not FINISH — `spctl` past its 30 s
/// ceiling on a captive-portal network, a `codesign` killed or out of its budget —
/// and so says nothing about the bundle: absence of proof is not
/// proof the installed bundle changed.
#[cfg(target_os = "macos")]
pub fn installed_update_facts_checked() -> Result<Option<InstalledUpdateFacts>, String> {
    // Pure observation — a dev-marked build's provenance is no less true, so report
    // it rather than blanking the panel. See `bundle::resolve_layout`.
    let Some(installed) = bundle::resolve_layout() else {
        return Ok(None);
    };
    // Info.plist values are only codesign-sealed evidence after the complete
    // configured policy gate succeeds. This runs on the updater facts worker,
    // never the event loop, so fail-closed verification adds no input latency.
    // Every helper runs at once (`verify::check_bundle`), read in the serial order.
    let checks = verify::check_bundle(&installed.app_root, effective_team_id(), true);
    checks.policy?;
    let build_number = checks.build?;
    let git_commit = checks.commit?;
    let version = checks.short_version.and_then(Result::ok);
    let staging = paths::Staging::resolve();
    let receipt = staging
        .as_ref()
        .and_then(|staging| manifest::InstalledReceipt::read(&staging.installed_receipt()))
        .filter(|receipt| receipt.matches_sealed(build_number, &git_commit));
    let yanked = staging
        .as_ref()
        .is_some_and(|staging| build_number < manifest::Floor::read(&staging.floor()).min_build);
    Ok(Some(InstalledUpdateFacts {
        build_number,
        git_commit,
        version,
        receipt_build_number: receipt.as_ref().map(|receipt| receipt.build_number),
        receipt_dmg_sha256: receipt.map(|receipt| receipt.dmg_sha256),
        yanked,
    }))
}

/// The activation pre-verify: before the GUI parks a reader to hand off to a newer
/// build already at its own bundle path (installed by another producer: the release
/// cutter, a user dragging a new `.app` over the old one, a sibling aterm's swap),
/// proves that bundle is exactly what the reducer authorized: a non-symlink
/// directory that passes the complete codesign policy, whose sealed build and commit
/// equal `expected_build` / `expected_commit`, and whose build is strictly newer
/// than the running one. Anything else refuses, so a bundle swapped again before
/// the handoff is caught before the terminal is touched. Runs on the handoff
/// worker, never the event loop.
///
/// `Ok` carries that bundle's handoff policy file
/// ([`aterm_update_core::handoff_policy`]) and chunked-grant declaration
/// ([`HandoffCandidateFacts`]), read only once every check has passed.
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

/// [`preverify_installed_at`] under `staging`'s apply lock (bounded, like the staged
/// lane's check), with the operator floor read under it.
///
/// A sibling aterm's staged swap renames a new bundle onto this path while holding
/// that lock; unlocked, it could land between the codesign check and the policy
/// read, caching the replacement's policy under the verified bundle's identity.
/// Under the lock a swap happens wholly before the check or wholly after the read.
/// (A bundle dragged over by hand takes no lock; the swap-time gate catches that.)
/// No staging root means no sibling can be swapping, so no lock.
#[cfg(target_os = "macos")]
fn preverify_installed_locked<R>(
    staging: Option<&paths::Staging>,
    app_root: &std::path::Path,
    current_build: u64,
    expected_build: u64,
    expected_commit: &str,
    read_policy: &dyn Fn(&std::path::Path) -> R,
) -> Result<R, String> {
    // The lock, then the verification budget under it: the same door the staged
    // candidate's check uses, so this is bounded by `HANDOFF_PREVERIFY_BOUND` too.
    let _preverify = install::enter_preverify(staging.map(|staging| staging.apply_lock.as_path()))?;
    // A floor that could not be read refuses as a moment.
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

/// The checks of [`preverify_installed_for_handoff`] against the bundle at
/// `app_root` and the operator floor `floor_min_build`, with the policy reader
/// injected so a test can prove a refused bundle's policy is never read.
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
    // The operator apply floor (a yank) gates an activation exactly as it gates a
    // staged swap (`install.rs`).
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
    // Every refusal returned above: the policy is read from a bundle that passed
    // its codesign policy and IS the authorized successor, or not at all.
    Ok(read_policy(app_root))
}

/// This process is a handoff candidate that has not taken over yet. Set by the GUI
/// at boot when the launch carries an overlap handoff, cleared when the outgoing
/// process commits it (never, if the candidate is rejected and exits).
///
/// It gates only [`health::Health::expire_stale_apply_streak`]: a candidate's own
/// background check must not expire the apply streak its own attempt is about to
/// add to, or `failing_applies` could never pass 1 and reach the persistent
/// verdict. Scoped to the candidate so an ordinary launch (headless, `--version`,
/// a session that never presents a frame) still heals a stale streak on its first
/// check.
static UNCOMMITTED_CANDIDATE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Mark this process an uncommitted handoff candidate (`true` at boot when the
/// launch carries a handoff; `false` once it is committed).
pub fn set_uncommitted_handoff_candidate(uncommitted: bool) {
    UNCOMMITTED_CANDIDATE.store(uncommitted, std::sync::atomic::Ordering::SeqCst);
}

/// Whether this process is a handoff candidate that has not been committed.
#[must_use]
pub fn is_uncommitted_handoff_candidate() -> bool {
    UNCOMMITTED_CANDIDATE.load(std::sync::atomic::Ordering::SeqCst)
}

/// Blocks the calling background thread while this process is an uncommitted
/// handoff candidate, up to `bound`; `true` when it waited at all. The outgoing
/// process may still reject it, so nothing it does before Commit may reach the
/// network, the store or the ledger. Polled every 50 ms: the flag is cleared on the
/// main thread at `Wake::ActivateCommittedHandoff`. The bound outlasts the late
/// park's grant budget so a candidate its parent never answered cannot spin.
pub fn wait_while_uncommitted_handoff_candidate(bound: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    let mut waited = false;
    while is_uncommitted_handoff_candidate() && started.elapsed() < bound {
        waited = true;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    waited
}

/// The bound on [`wait_while_uncommitted_handoff_candidate`] for the two
/// background lanes a successor starts at boot: longer than the successor's own
/// grant-hold budget, so the lanes start only after Commit or after the
/// candidate has already been told to exit.
pub const UNCOMMITTED_CANDIDATE_HOLD_BOUND: std::time::Duration =
    std::time::Duration::from_secs(10 * 60);

/// Consecutive unconfirmed launches after which a freshly swapped build is
/// auto-reverted to its predecessor and marked failed. Exposed because a caller
/// that declines to forgive a counted trial launch spends this budget; `aterm-gui`'s
/// handoff lane compile-time-asserts its structural budget against it.
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

/// A successor of `build` leaving by rule (its parent closed the rendezvous, or it
/// refused the handoff before the transfer) gives back the boot-trial launch this
/// process counted at boot, if any and not yet given back: it did not fail the
/// build, and three such exits must not revert a healthy build. Named by this
/// process's pid and kernel birth stamp ([`aterm_update_core::LaunchName`]), so
/// neither an earlier kept launch nor a sibling window's launch of the same trial
/// is taken for it, and the parent settling the same death
/// ([`forgive_trial_launch_of`]) gives back nothing more. `true` when a launch was
/// given back, or owed back to the next holder of the apply lock when another aterm
/// held it past the wait.
#[cfg(target_os = "macos")]
pub fn forgive_own_trial_launch(build: u64) -> bool {
    install::forgive_own_trial_launch(build)
}

/// Non-macOS: nothing to forgive.
#[cfg(not(target_os = "macos"))]
pub fn forgive_own_trial_launch(_build: u64) -> bool {
    false
}

/// The outgoing process gives back the boot-trial launch of `target_build` its
/// candidate counted (process `pid`, born at `started`, the kernel's
/// `pbi_start_tvsec`/`pbi_start_tvusec`, else named by pid alone), because the
/// candidate did not fail the build: the parent ended it for its own reason
/// (readiness deadline, user activity, proof mismatch, a session closing), or it
/// died in a way that is no evidence against the bytes. Left counted, bounded
/// automatic re-attempts would walk a healthy build to `MAX_BOOT_ATTEMPTS`. Runs on
/// the handoff worker after the reap, under the apply lock like
/// `check_boot_health`; a lock another aterm holds past the wait leaves the
/// give-back OWED, and the next holder of the lock pays it before it reads the
/// count. Best-effort: nothing here can fail an apply.
///
/// By name, never by the count's movement: several windows' launches of one trial
/// overlap, so a count delta could erase an earlier kept launch (the crash signal)
/// or skip the candidate's own. A candidate never counted, or one that gave its
/// launch back itself ([`forgive_own_trial_launch`]), gives nothing back here.
/// `true` when a launch was given back, or owed back.
#[cfg(target_os = "macos")]
pub fn forgive_trial_launch_of(target_build: u64, pid: u32, started: Option<(u64, u64)>) -> bool {
    // A stamp that does not fit is no stamp: the pid alone names the launch.
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

/// A launched handoff successor names itself before its boot apply can count a
/// trial launch: it writes at `stamp` (beside its parent's rendezvous, in the
/// parent's private directory) the name the sentinel counts its launch under, its
/// pid and kernel birth stamp. LaunchServices reports a successor that dies before
/// checking in with no pid, so the stamp is its parent's one name for it
/// ([`forgive_stamped_trial_launch`]). Written once, never followed or replaced:
/// the boot apply's re-exec keeps pid and birth stamp, so it is the same launch.
/// Best effort: an unwritten stamp leaves the launch counted, which keeps the
/// crash signal.
#[cfg(target_os = "macos")]
pub fn stamp_trial_launch(stamp: &std::path::Path) -> std::io::Result<()> {
    install::stamp_trial_launch(stamp)
}

/// Gives back the counted launch of `target_build` that the launched successor
/// named in its stamp at `stamp` ([`stamp_trial_launch`]): the successor
/// LaunchServices reported terminated before it checked in, with no pid. The
/// by-pid counterpart is [`forgive_trial_launch_of`]. `true` when a launch was
/// given back, or owed back.
///
/// By the stamped name, never a guess: the newest launch without a live process may
/// be a sibling's crash the trial must keep. A candidate that counted nothing or
/// gave its launch back ([`forgive_own_trial_launch`]) stamped a name that is not
/// on the line, so nothing is given back; nor for no stamp, a torn one, or a launch
/// the kernel would not name.
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

/// Non-macOS has neither an installed bundle nor a self-update receipt.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn installed_update_facts() -> Option<InstalledUpdateFacts> {
    None
}

/// Non-macOS has no installed bundle to prove.
#[cfg(not(target_os = "macos"))]
pub fn installed_update_facts_checked() -> Result<Option<InstalledUpdateFacts>, String> {
    Ok(None)
}

/// What [`supersede_installed_activation`] did: the unconfirmed activation it
/// retired, the running build it put back at the install path, the verified stage
/// that applies over it next, and the launches the retired boot trial had counted
/// (read, never added to).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupersededActivation {
    pub activation_build: u64,
    pub restored_build: u64,
    pub newer_build: u64,
    pub trial_launches: u32,
    /// `Some(why)` when the exchange happened (the running build is back at the
    /// install path) but the activation's boot trial could not be disarmed. Still a
    /// retire, not a refusal: `recover_abandoned_preswap_trial_if_exact` finishes
    /// that crash cut on the next boot apply, which the newer build's candidate
    /// runs before it swaps anything, and the lane stages the newer build as after
    /// a clean retire.
    pub trial_disarm_deferred: Option<String>,
}

/// Why [`supersede_installed_activation`] moved nothing, and whether the same disk
/// could let it through later. The GUI offers a refused release again after a
/// backoff whatever the kind; the kind decides what it says, since only
/// [`Self::Settled`] is one a person has to look at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SupersedeRefusal {
    /// About the moment: the apply lock was busy past the background wait, a
    /// bundle's verification failed or ran out of budget (the Gatekeeper assessment
    /// uses the network), the stage was being replaced, or the exchange's rename
    /// failed. Nothing says the next try is refused too.
    ForNow(String),
    /// About the disk: it is not the one shape the retire can undo (another bundle
    /// installed, the trial armed for another build or install, the retained
    /// predecessor not the running build, the stage yanked). Only a change to the
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
    pub fn is_for_now(&self) -> bool {
        matches!(self, Self::ForNow(_))
    }
}

impl std::fmt::Display for SupersedeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

/// A newer verified stage retires an unconfirmed activation. The GUI calls this,
/// off its event loop, when its automatic lane has converged structurally on the
/// installed activation of `activation_build` (a bundle this process's
/// launched-lane candidate installed and then failed to hand off to, twice) and a
/// verified download of `newer_build` is staged behind it.
///
/// Nothing else can carry `newer_build` onto the machine: the running process
/// cannot swap it in (the installed bundle is not the running build, so there is
/// no rollback source), and the activation's successor refuses to boot-apply past
/// the build it was authorized as. So the running build is put back at the install
/// path (the crash-loop revert's inverse exchange), the activation's trial is
/// disarmed without counting a launch, and the ordinary staged lane applies
/// `newer_build` with a fresh trial. The activation is never booted again. See
/// `install::supersede_installed_activation` for the preconditions and crash cuts.
///
/// # Errors
/// A [`SupersedeRefusal`] when nothing moved; see its kinds.
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

/// A GUI-supplied hook the background check uses to surface self-healing events
/// (`(title, body)`, e.g. shown as an OS notification), so health problems do not
/// stay buried in `status.toml`.
///
/// It also carries the recovery: once this process has reported a persistent
/// failure and a later check finds the ledger healed, it is called with
/// [`HEALTH_RECOVERED_TITLE`] and, as the body, the class this process announced
/// (`"apply"`, `"manifest"`, …), so a surface still showing the failure can clear
/// it. The heal proves only that class: it says nothing about a warning another
/// source raised (the GUI's convergence or overdue notice). No notification is
/// owed for it; the warning leaving is the message.
pub type HealthNotify = Box<dyn Fn(String, String) + Send>;

/// The `title` [`HealthNotify`] is called with when a persistent failure this
/// process reported has healed. Pinned so the GUI matches on it, not the body.
pub const HEALTH_RECOVERED_TITLE: &str = "aterm auto-update recovered";

/// A GUI-supplied hook fired when a strictly newer build has just been staged, so
/// the GUI can arm the in-session apply lane and show the update-ready nudge.
/// `(build, version)`.
pub type StagedNotify = Box<dyn Fn(u64, String) + Send>;

/// A GUI-supplied hook fired after a background check pass whose recorded outcome
/// CHANGED — Linux's only word to the window that the copy's update state moved (a
/// newer executable installed or held, a trial resolved), so it re-reads it. The
/// health hook ([`HealthNotify`]) carries only failures that persist and their
/// recovery, so without this Settings ▸ Software Update would keep the state from
/// when it was last opened. macOS reports through [`StagedNotify`] and the progress
/// observer, so it does not call it.
pub type CheckedNotify = Box<dyn Fn() + Send>;

/// One process-wide network/staging lane shared by the periodic scheduler and
/// every user-triggered check. The GUI reducer adds generation semantics above
/// this seam; the mutex ensures older callers cannot create a second concurrent
/// download/verify transaction underneath it.
#[cfg(target_os = "macos")]
fn check_lane() -> &'static check_lane::Lane {
    static LANE: check_lane::Lane = check_lane::Lane::new();
    &LANE
}

/// Whether the persistent-failure notice should be spoken for `class`, given the
/// class this process already announced (`None` = none yet).
///
/// Keyed by class, not a once-per-process `bool`: each class asks the user for a
/// different thing (a `manifest` escalation says the newest release cannot be
/// trusted, an `apply` one says a verified build will not start), so a machine that
/// announced one lane and then stranded another must say so. The SAME class never
/// nags once per check. A class that heals and later breaks again is a new episode
/// and speaks again; the caller's 30-minute persistence gate, measured on that
/// class's own clock, bounds how often that can happen.
///
/// KEYED BY THE NOTICE A PERSON READS, NOT THE LEDGER'S SPELLING: `stage` and
/// `pipeline` are one notice, "Couldn't download updates" ([`health_class_canonical`]),
/// and the window reads that title back as `pipeline` alone, so a successor that took
/// up its parent's download warning ([`adopt_announced_health`]) over a `stage` streak
/// must not say it a second time.
#[cfg(target_os = "macos")]
fn persistent_notice_is_new(announced: Option<&str>, class: &str) -> bool {
    announced.map(health_class_canonical) != Some(health_class_canonical(class))
}

/// Whether the loud "auto-update is failing" notice is owed now, given what was
/// last announced and which class (if any) has escalated. Folds "nothing is
/// persistently failing" into the keyed latch so a caller cannot answer only one.
#[cfg(target_os = "macos")]
fn persistent_notice_is_owed(announced: Option<&str>, class: Option<&str>) -> bool {
    class.is_some_and(|class| persistent_notice_is_new(announced, class))
}

/// The `title` [`HealthNotify`] is called with when a persistent failure of `class`
/// is first announced, naming the half of updating that is broken: `apply`, a
/// verified build will not start; `pipeline` / `stage`, downloads fail or will not
/// verify; `manifest` (and any future class), the newest release cannot be read or
/// trusted. Worded in the `Couldn't …` failure grammar; [`health_failing_class`]
/// reads a title back to its class.
#[must_use]
pub fn health_failing_title(class: &str) -> &'static str {
    match health_class_canonical(class) {
        "apply" => "Couldn't install updates",
        "pipeline" => "Couldn't download updates",
        _ => "Couldn't check for updates",
    }
}

/// The class a persistent-failure title stands for (`apply`, `pipeline`, whose
/// `stage` sibling shares the title, or `manifest`), for a title
/// [`health_failing_title`] writes now or wrote in its older wording (`aterm can't
/// install updates` / `download` / `check for`), so a row carried from an older
/// build still names its class. `None` for any other title. Kept beside the writer
/// so the two cannot drift.
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

/// The class whose title `class` shares: `stage` is the download half, and any
/// class this build does not know is the check half.
fn health_class_canonical(class: &str) -> &'static str {
    match class {
        "apply" => "apply",
        "pipeline" | "stage" => "pipeline",
        _ => "manifest",
    }
}

/// The instant a persistent-failure notice's body dates its streak from (the RFC
/// 3339 stamp after "since " that `persistent_failure_notice` writes), or `None`.
/// Kept beside the writer so the row's "since <date>" cannot drift from it.
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

/// The `title` [`HealthNotify`] is called with when the class this process already
/// announced is still failing and its count has moved: the log and Settings restate
/// the number in place; no notification or new row is owed. The body is the
/// announcement's sentence with the new count.
pub const HEALTH_RESTATED_TITLE: &str = "aterm auto-update still failing";

/// The `title` [`HealthNotify`] is called with when this machine cannot READ its
/// release channel, and so is not updating at all (once per strand,
/// `unreadable::StrandNotice`). It names NO LEDGER CLASS, on purpose: a
/// configuration state is not a health-ledger failure (`unreadable::announce`
/// records none), so [`health_failing_class`] reads nothing from it and the
/// ledger's healing is no proof it is over. Pinned so the window can tell it
/// from the ledger's words: a handed-off successor adopts as announced only a
/// warning whose words name a ledger class ([`adopt_announced_health`]) — one
/// that adopted this as the check class would send the check class's recovery
/// on its first cycle (the ledger was never failing), take the strand's row
/// down and put "aterm updates work again" on record over a machine still
/// stranded.
pub const STRANDED_TITLE: &str = "aterm is not updating on this machine";

/// The `title` [`HealthNotify`] is called with when an executable the Linux lane installed
/// has waited over an hour for the next aterm a person starts, which finishes the update
/// (`linux_notice::InstallWatch`; the review of ATERM-KI-1015-2, U21). It is NO FAILURE and
/// names no ledger class: the install worked, and on Linux no lane starts an installed
/// executable inside a running aterm, so this wait is how every automatic install there
/// ends. The window shows it as a notice of its own, with the body as its sentence, and
/// never as the install half's warning: it reaches no health latch, no digest line and
/// no banner. Pinned so the window matches on it; [`health_failing_class`] reads no class
/// from it.
pub const INSTALL_AWAITS_START_TITLE: &str = "Start a new aterm to finish the update";

/// The `title` [`HealthNotify`] is called with when that wait is over
/// ([`INSTALL_AWAITS_START_TITLE`]): a start finished the install, or its trial ended
/// without one (rolled back, or kept). The window takes the notice down. The body names
/// the installed version. It is never shown.
pub const INSTALL_WAIT_OVER_TITLE: &str = "aterm update no longer waits for a start";

/// What the check loop tells the GUI about the ledger's health, decided from the
/// shared ledger on every cycle, so a cycle skipped because a sibling already
/// checked reads the same evidence as one this process ran (including the
/// recovery line).
#[cfg(target_os = "macos")]
struct HealthAnnouncer {
    /// The clock at thread start: a streak whose latest failure predates this
    /// process is history, not news, and never re-notifies on every launch.
    started: String,
    /// The class this process announced and the count it spoke; `None` = none yet.
    /// Keyed on the class (not a bool) so a second stranded lane still speaks, and
    /// carrying the count so a moved count restates without re-notifying. The
    /// check loop's announcer takes it up from the process before each tick
    /// ([`Self::tick_for_process`]).
    announced: Option<(&'static str, u32)>,
    /// The pending build this process already announced as overdue
    /// ([`Self::tick_overdue`]); `None` = none yet. Once per build per process: the
    /// notice stands as a row until the machine moves.
    overdue_announced: Option<u64>,
}

#[cfg(target_os = "macos")]
impl HealthAnnouncer {
    fn new(started: String) -> Self {
        Self {
            started,
            announced: None,
            overdue_announced: None,
        }
    }

    /// The loud notice owed because a newer build has waited on this machine for
    /// more than [`PENDING_UPDATE_OVERDUE_SECS`] while `current_build` still runs,
    /// or `None`.
    ///
    /// Escalates on the machine's state, not a streak count: a stranded apply lane
    /// may never reach [`PERSISTENT_AFTER`] failures of one class (a structural
    /// failure converges after two attempts; a capture refusal filed as a park miss
    /// never counts). "A newer build has been here an hour and is not running" is
    /// always true of a stuck update and never of a healthy one.
    ///
    /// Pure over the ledger; the ledger's pending clock
    /// ([`health::Health::note_pending_update`]) makes the hour an hour rather than
    /// "since the last re-stage". `automatic` is the host's `[update] auto_apply`
    /// ([`set_automatic_apply`]): with it off a staged build waits for a person by
    /// design and is never announced as a failure.
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

    /// The `(title, body)` owed to the GUI for the ledger as it stands at `now`,
    /// or `None`. Pure over the ledger so the law is testable without a thread.
    fn tick(
        &mut self,
        h: &health::Health,
        now: &str,
        current_build: u64,
    ) -> Option<(String, String)> {
        let active = h.last_failure_at.as_str() >= self.started.as_str();
        // Duration gate beside the count gate: three failures can come within a few
        // minutes, a blip rather than the broken lane the loud notice promises, so the
        // streak must also have stood for [`PERSISTENT_SPAN`] (a class a check books
        // already spans it between its own failures: `Health::persistent_class`).
        // Measured on the escalating class's own clock: `failing_since` is any-class
        // and is kept while an apply streak survives (`Health::record_success`), so
        // reading it would let an old apply failure backdate a new blip past the gate.
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
                // The same class, a moved count: restate quietly.
                self.announced = Some((class, count));
                let (_, body) = persistent_failure_notice(class, count, h, current_build);
                return Some((HEALTH_RESTATED_TITLE.to_string(), body));
            }
            return None;
        }
        if !h.is_persistent()
            && let Some((class, _)) = self.announced.take()
        {
            // All healed: any class may speak again. A failure this process
            // reported gets its recovery reported once, naming the class, so the
            // standing row can leave (see [`HealthNotify`]).
            return Some((HEALTH_RECOVERED_TITLE.to_string(), class.to_string()));
        }
        None
    }

    /// [`Self::tick`] as THIS PROCESS's announcer: what the process has
    /// announced is taken up from `process` first, and this tick's verdict is
    /// left there ([`AnnouncedHealth`]).
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

/// The classes [`health::Health::persistent_class`] can name, in the order an
/// ADOPTED class outranks another ([`AnnouncedHealth::adopt`]): the recovery of a
/// later one proves more to the window (`apply` > the download half > the
/// check half), so a successor whose parent said two keeps the one whose
/// recovery answers both.
const ANNOUNCED_CLASSES: [&str; 4] = ["manifest", "stage", "pipeline", "apply"];

/// WHAT THIS PROCESS HAS ANNOUNCED about the ledger's health — the class and
/// the count a [`HealthAnnouncer`] last spoke — held for the PROCESS, never for
/// one checker thread.
///
/// The window latches each failing class it raised until a recovery
/// ([`HEALTH_RECOVERED_TITLE`]) answers it, and only an announcer that remembers
/// announcing sends one. A replacement generation the watchdog starts, and a
/// handed-off successor whose PARENT did the announcing, would otherwise clear the
/// streak (`Health::record_success`) with nothing sent, leaving the window's latch
/// standing and the next failing episode reaching only the log. So every announcer
/// of the process takes this up before it ticks and leaves its verdict here, and a
/// successor adopts what its parent said ([`adopt_announced_health`]).
///
/// One word, no lock: the class's 1-based index in [`ANNOUNCED_CLASSES`] above
/// bit 32, the count below it, `0` for nothing announced.
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

    /// The word as it stands: what an announcer takes up
    /// ([`HealthAnnouncer::tick_for_process`]), and hands back to
    /// [`Self::settle`].
    #[cfg(target_os = "macos")]
    fn load(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Leave `announced` — a tick's verdict over the word `taken` — unless the
    /// word moved meanwhile (a successor's adoption, another generation's
    /// verdict): that one stands, and the next tick takes it up.
    #[cfg(target_os = "macos")]
    fn settle(&self, taken: u64, announced: Option<(&str, u32)>) {
        let _ = self.0.compare_exchange(
            taken,
            Self::encode(announced),
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
    }

    /// Take up `class` as ANNOUNCED — by this process's parent — unless a class
    /// that outranks it already is ([`ANNOUNCED_CLASSES`]). Its count is
    /// unknown here, so a streak still failing is restated
    /// ([`HEALTH_RESTATED_TITLE`], quiet), never announced again; a healed one
    /// sends the recovery.
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

/// The process's [`AnnouncedHealth`]: the macOS check loop's announcers read and
/// write it ([`HealthAnnouncer::tick_for_process`]).
static ANNOUNCED_HEALTH: AnnouncedHealth = AnnouncedHealth::new();

/// AN UPDATE'S SUCCESSOR TAKES UP WHAT ITS PARENT ANNOUNCED: `class` is the
/// updater class of a health warning the parent raised and the landing does not
/// answer (`manifest` for "Couldn't check for updates", `pipeline` for "Couldn't
/// download updates"), read from the warning's own words
/// ([`health_failing_class`]). This process's check loop then treats it as its
/// own announcement: a streak still failing is restated quietly, never announced
/// a second time, and the ledger's healing sends [`HEALTH_RECOVERED_TITLE`]
/// naming it — the proof the window waits for before it takes down the warning
/// it carried, and records the healing.
///
/// A warning whose words name no ledger class — the strand's
/// ([`STRANDED_TITLE`]), a title this build does not know — is never handed
/// over: no announcer said it, and a
/// ledger that was never failing would "heal" it on the first cycle.
///
/// In memory only (nothing reaches the ledger, so a candidate the parent rejects
/// has changed nothing). A lane with no handoff successor (Linux) never reads it.
pub fn adopt_announced_health(class: &str) {
    ANNOUNCED_HEALTH.adopt(class);
}

/// The loud notice for the class that escalated, as `(title, body)`: the title says
/// which half of updating is broken ([`health_failing_title`]), the body the count,
/// the date ([`health_notice_since`] reads it back), the cause and where the rest
/// is. The log line and Settings carry it whole; the update row keeps the date.
/// Count and sentence both come from the class that escalated.
#[cfg(target_os = "macos")]
fn persistent_failure_notice(
    class: &str,
    count: u32,
    h: &health::Health,
    current_build: u64,
) -> (String, String) {
    // The stage streak is described by its own reason: a `network` blip does not end
    // a standing stage streak but does overwrite `last_error`, and the arms below
    // read passing-ness off the text they quote.
    let stage_reason = if h.last_diagnosis_error.is_empty() {
        h.last_error.as_str()
    } else {
        h.last_diagnosis_error.as_str()
    };
    let cause = match class {
        // A stranded client: the reason already names the anchor and the reinstall;
        // no release the publisher cuts can be verified by this build.
        "manifest" if github::is_stale_anchor_refusal(&h.last_error) => format!(
            "{} — this machine stays on build {current_build} until it is reinstalled",
            h.last_error
        ),
        "manifest" => format!(
            "the newest release cannot be trusted ({}) — this machine stays on build \
             {current_build} until it is republished",
            h.last_error
        ),
        // A stage that could not FINISH, every time: a helper past its deadline or
        // refused a start, a copy past its budget. Named by the step that did not
        // finish: the verdict words around it ("failed verification", "treating as
        // a rejection") are the refusal's plumbing, not a finding about the build.
        // The retry is kept: it tells the reader nothing needs doing — and how it is
        // paced (`manifest::MOMENT_BACKOFF_SECS`). A full disk (the download, the
        // unpack, or the free-space check) is not that: its step already names the
        // one action and the retry, so it is said alone.
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
        // Not "will not start": a handover the terminal was too busy for, or one
        // called off, never started anything wrong. And never the ledger's raw
        // reason ("overlap handoff failed safely: …" reads as a success): its
        // cause in words is on Software Update, the raw text in `-v` and ctl.
        "apply" => "an update is downloaded and verified but didn\u{2019}t install".to_string(),
        // Not this build's pipeline: another aterm holds the stage lock and shows
        // no sign of life. The ledger's error is the whole sentence (an older
        // build's carried a "stage lock: " prefix).
        "pipeline" if github::is_stopped_stage_holder_refusal(&h.last_error) => h
            .last_error
            .strip_prefix("stage lock: ")
            .unwrap_or(&h.last_error)
            .to_string(),
        // A check this Mac could not finish, every time — the floor it could not read
        // — is named by the step that did not finish, whose words
        // carry the remedy; "the pipeline is broken" would send the reader after a
        // download fault that does not exist.
        "pipeline" if is_passing_refusal(stage_reason) => format!(
            "update checks could not finish on this Mac: {}",
            passing_step(stage_reason)
        ),
        // "pipeline", and any future class, keeps the original text.
        _ => "release manifests exist but cannot be downloaded — this build's update \
              pipeline is likely broken"
            .to_string(),
    };
    // The noun names the lane the count came from: an apply streak counts failed
    // attempts to install a build every check fetched fine, not failed checks.
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

/// The step a passing stage refusal names (`verify::timed_out`, `mark_passing`,
/// `Mounted::attach`'s joined attempts), without the words that only mark it: the
/// key ([`PASSING_REFUSAL_KEY`], or the ledger's cut form " … (key)"), the
/// verifier's "; treating as a rejection/failure", and the stage's "staged bundle
/// failed verification: ", which a step that did not finish contradicts.
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

/// The loud notice for a newer build that has waited on this machine past
/// [`PENDING_UPDATE_OVERDUE_SECS`], as `(title, body)`, carrying the typed cause the
/// apply lane left in the ledger: its failure count and reason for that build, the
/// standing schedule or refusal it recorded, or that no apply attempt was recorded.
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

/// The pending-update clock brought up to date for what this cycle observed
/// ([`health::Health::note_pending_update`]), and dropped for a withdrawn build.
///
/// `staged` is the publishable stage's build ([`manifest::Ready::read_publishable`],
/// which already hides a withdrawn stage) and `installed` a bundle installed under
/// this older image; the newer of the two that is still an update is the build
/// waiting here. A bundle below the operator floor (a yank) is no update wherever it
/// sits: the bundle probe does not announce it and the GUI's facts worker drops it,
/// so nothing will ever activate it; observed as waiting, it would keep its clock
/// and an hour later be announced as stuck with "no apply attempt".
///
/// The clock survives an observation that finds nothing (the swap consumes the
/// stage for a moment; resetting on absence would keep a stranded machine "just
/// staged" forever). A withdrawal is a recorded fact: a build quarantined here (it
/// crash-looped and was reverted) or below the operator floor (yanked) will never
/// be applied, so it ends the clock rather than being announced as waiting. The
/// revert and the yank report themselves through their own records.
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

    /// The row's date is read from the sentence it is cut from: every class's body
    /// carries the stamp, and a body without one yields nothing.
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

    /// A title reads back to its class, in current and older wording: the GUI keys
    /// the heal on the class, so a warning an older build raised is still healed.
    /// Negative controls: the restatement, the recovery, the watchdog's title and a
    /// near miss name no class.
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

    /// A newer build waiting for an hour is announced once: two failed applies
    /// (under `PERSISTENT_AFTER`) produce no streak notice, and nothing is said
    /// before the hour; past it, the loud title, once. A caught-up machine is quiet
    /// and a later build is news again.
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
        // Two failures: the streak notice is not owed.
        assert!(stranded.persistent_class().is_none());
        // With automatic apply switched off a staged build waits for a person by
        // design: never "failing", however long it waits.
        assert!(
            announcer
                .tick_overdue(&stranded, "2026-09-25T00:00:00Z", 1790019739, false)
                .is_none(),
            "auto_apply = false is not a failure"
        );
        // …and inside the hour the overdue one is not either.
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
        // Caught up: quiet, and the latch resets for the next episode.
        assert!(
            announcer
                .tick_overdue(&stranded, "2026-09-23T00:00:00Z", 1790120495, true)
                .is_none()
        );
        // A different build, never attempted: a new episode is news.
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

    /// A withdrawn build is not waiting. The pending clock survives an observation
    /// that finds nothing, but a stage retired because its build crash-looped here
    /// (quarantined), was yanked below the operator floor, or was signed by a
    /// machine the roster revoked will never be applied and must not be announced.
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
                // The check retires a stage signed by a machine the roster revoked:
                // nothing on disk records it, so the retiring code drops the clock.
                "revocation" => super::github::retire_revoked_stage(&s, 11),
                _ => {}
            }
            // Nothing staged, the installed bundle is the running build.
            let h = super::note_pending_update(&s, 10, None, None);
            let said =
                super::HealthAnnouncer::new(started.clone()).tick_overdue(&h, later, 10, true);
            if withdrawal == "none" {
                // Negative control: a stage merely consumed keeps its clock.
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

    /// A YANKED INSTALLED BUNDLE IS NOT WAITING EITHER. The check loop observes a
    /// bundle installed under this older image on every cycle, beside the stage. A
    /// bundle below the operator floor is no update wherever it sits: the bundle probe
    /// does not announce it and the GUI's facts worker drops it, so nothing will ever
    /// activate it. Its clock ran on regardless, because the withdrawal was judged only
    /// when nothing newer was observed, and an hour later, and at every relaunch,
    /// "Couldn't install updates" said build 11 had been waiting with "no apply
    /// attempt".
    ///
    /// FAILS WITHOUT THE FIX: the yanked installed build kept `pending_build == 11`, and
    /// the backdated clock raised the overdue notice.
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
            // The clock started while build 11 was an update, as the swap left it.
            super::note_pending_update(&s, 10, None, Some(11));
            backdate(&s);
            if yanked {
                super::manifest::Floor::bump_and_write(&s.floor(), 12, 0, 0);
            }
            // Nothing staged; 11 is the installed bundle's build, as `speak_health`
            // observes it every cycle.
            let h = super::note_pending_update(&s, 10, None, Some(11));
            let said =
                super::HealthAnnouncer::new(started.clone()).tick_overdue(&h, later, 10, true);
            if yanked {
                assert_eq!(h.pending_build, 0, "a yanked build is never pending");
                assert!(said.is_none(), "build 11 is yanked, not waiting: {said:?}");
            } else {
                // Negative control: an installed update nothing activated is told.
                assert_eq!(h.pending_build, 11);
                assert!(said.is_some(), "a stranded machine must still be told");
            }
            // The yank's successor, once staged beside the yanked bundle, is what
            // waits, on a clock of its own.
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

    /// The standing row's count follows the ledger: the announced class restates
    /// its number through [`HEALTH_RESTATED_TITLE`] when the count moves (no second
    /// loud notice), and speaks the loud title again only for a different class or
    /// a new episode after healing. The decision is a pure function of the ledger,
    /// so the loop can ask it on a skipped cycle too.
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
        // Not yet 30 minutes on the class's clock: nothing.
        assert!(
            announcer
                .tick(
                    &ledger(3, "2026-09-13T22:50:00Z"),
                    "2026-09-13T23:00:00Z",
                    7
                )
                .is_none()
        );
        // Past the gate: the loud announcement, once.
        let (title, _) = announcer
            .tick(
                &ledger(3, "2026-09-13T23:12:50Z"),
                "2026-09-13T23:13:44Z",
                7,
            )
            .expect("the first escalation is announced");
        assert_eq!(title, super::health_failing_title("apply"));
        // Same class, same count: silence.
        assert!(
            announcer
                .tick(
                    &ledger(3, "2026-09-13T23:12:50Z"),
                    "2026-09-13T23:37:45Z",
                    7
                )
                .is_none()
        );
        // Same class, the count moved: a QUIET restate carrying the new number.
        let (title, body) = announcer
            .tick(
                &ledger(6, "2026-09-14T00:10:00Z"),
                "2026-09-14T00:12:34Z",
                7,
            )
            .expect("a moved count is restated");
        assert_eq!(title, super::HEALTH_RESTATED_TITLE);
        assert!(body.starts_with('6'), "the new number: {body}");
        // A failure that predates this process is history: no restate for it.
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
        // Healed: the recovery line, once, then nothing.
        let healed = Health::default();
        let (title, body) = announcer
            .tick(&healed, "2026-09-14T02:00:00Z", 7)
            .expect("a failure this process reported gets its recovery reported");
        assert_eq!(title, super::HEALTH_RECOVERED_TITLE);
        assert_eq!(body, "apply", "the heal names the class it proves");
        assert!(announcer.tick(&healed, "2026-09-14T02:01:00Z", 7).is_none());
        // A new episode after healing speaks the loud title again.
        let (title, _) = announcer
            .tick(
                &ledger(3, "2026-09-14T03:30:00Z"),
                "2026-09-14T03:31:00Z",
                7,
            )
            .expect("a new episode is news");
        assert_eq!(title, super::health_failing_title("apply"));
    }

    /// THREE QUICK FAILURES RAISE NO NOTICE, HOWEVER LONG AGO THEY BEGAN. At the
    /// two-minute cadence a GitHub incident fails three checks in six minutes; a
    /// laptop that sleeps on that streak and fails its first check on waking
    /// (weather, booked `network`) must not wake to "Couldn't download updates". The
    /// same streak spanning half an hour between its own failures is announced.
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

    /// A ledger whose `manifest` streak has persisted, its last failure at `last`.
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

    /// ROUND-SEVEN UPDATE AUDIT, FINDING 206 — A SUCCESSOR SENDS THE RECOVERY OF
    /// WHAT ITS PARENT ANNOUNCED. The parent announced "Couldn't check for
    /// updates" and the window latched it; a build landed and the successor's
    /// first check came back up to date, which clears the streak in the ledger.
    /// The successor's announcer, a fresh one, had announced nothing, so it sent
    /// nothing: the carried warning stood latched for the launch, and the next
    /// failing episode reached only the log. Adopted, the class is healed out loud
    /// (the recovery, naming it), and the process is free to announce again.
    /// NEGATIVE CONTROL: while the adopted streak still fails, the successor
    /// restates it quietly — it never announces the parent's class a second time.
    #[test]
    fn a_successor_sends_the_recovery_of_the_class_its_parent_announced() {
        let started = "2026-09-29T10:00:00Z".to_string();
        // Still failing after the landing: a quiet restatement, never the loud title.
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

        // NEGATIVE (group RL): the window adopted nothing — the parent's warning
        // named no ledger class (the strand's, `STRANDED_TITLE`) — and the ledger
        // was never failing: no recovery is owed, so none heals that warning.
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

        // The successor's first check is up to date: the ledger healed.
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
        // A later episode — past the 30-minute gate on its own clock — is news.
        let mut later = failing_checks("2026-09-29T15:40:00Z");
        later.manifest_since = "2026-09-29T15:00:00Z".to_string();
        let (title, _) = announcer
            .tick_for_process(&process, &later, "2026-09-29T15:40:01Z", 7)
            .expect("a new episode is announced");
        assert_eq!(title, super::health_failing_title("manifest"));
    }

    /// ... and so does a REPLACEMENT generation for what the one it replaced
    /// announced (the same flaw inside one process: the window's watchdog starts
    /// a fresh check loop in place of a stalled one, and its announcer began at
    /// nothing, so the replacement's first healthy check healed the ledger with
    /// nothing sent). NEGATIVE CONTROL: without the process's word between them,
    /// the fresh announcer sends nothing — the defect.
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

    /// An adopted class never demotes one that outranks it: a parent that said
    /// both halves hands over the download's, whose recovery answers both in the
    /// window; and a class this process announced itself stands against an
    /// adoption of a lesser one.
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

    /// ROUND-EIGHT UPDATE AUDIT, FINDING 301 — A TAKEN-UP DOWNLOAD WARNING IS NOT SAID
    /// AGAIN FOR ITS SIBLING CLASS. The parent said "Couldn't download updates", which
    /// both `stage` and `pipeline` speak; the window reads it back as `pipeline` and
    /// the successor takes that up. Its own streak is `stage`: keyed by the ledger's
    /// spelling, that was a different class and the same words were announced a
    /// second time. It is restated, quietly. NEGATIVE CONTROL: with nothing taken up,
    /// the `stage` streak is announced.
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
        // NEGATIVE CONTROL: nothing taken up — the streak is news.
        let process = super::AnnouncedHealth::new();
        let mut announcer = super::HealthAnnouncer::new(started);
        let (title, _) = announcer
            .tick_for_process(&process, &stage, "2026-09-29T10:02:01Z", 7)
            .expect("announced");
        assert_eq!(title, super::health_failing_title("stage"));
    }
}

/// Supplies the current configured channel at the start of each background cycle.
/// The callback runs on the checker thread and must return within a bounded time.
/// `None` skips network work and retries the provider in five seconds (once-only
/// mode stops); a running check retains its source snapshot. Linux samples whole
/// [`CheckSettings`] instead.
#[cfg(not(target_os = "linux"))]
pub type SourceProvider = std::sync::Arc<dyn Fn() -> Option<Source> + Send + Sync>;

/// One request-local configuration observation. The provider is sampled again
/// before Linux replacement, so a reload during download cannot bypass a veto.
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
            // No check ran, but the stage on disk is still there: the durable
            // status says so with this request's outcome, so a driver reading
            // `relaunch_ready=` does not conclude nothing was staged.
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

/// Starts a replaced Linux executable gets to confirm itself
/// ([`LinuxUpdateStatus::trial_starts`]) — a window's launch or an interactive terminal
/// session's; the next start restores the previous one, once no earlier start can still
/// confirm (`linux_trial`: a burst of session starts waits for its first prompt).
pub const LINUX_TRIAL_LAUNCHES: u32 = 3;

/// Linux disk-stage/trial facts are not a macOS DMG or a live-session handoff.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinuxUpdateStatus {
    pub installed_build: u64,
    /// The installed executable's own release version — after a check that installed
    /// a release, the NEW build's, not the running process's.
    pub installed_version: String,
    pub staged_build: Option<u64>,
    pub staged_version: Option<String>,
    pub staged_commit: Option<String>,
    pub trial_phase: Option<String>,
    pub trial_starts: u32,
    pub trial_healthy: bool,
    /// This copy rolled back a build newer than the one it runs, and won't install
    /// it again: "up to date" would hide that it runs an older release on purpose.
    pub refused_newer: bool,
    /// The build this copy refuses ([`Self::refused_newer`]) was rolled back because its
    /// starts spent their budget ([`LINUX_TRIAL_LAUNCHES`]) without proving it — a crash
    /// loop — and not by `aterm update rollback` of a build that ran.
    pub refused_unstarted: bool,
    /// The installed build's trial ended at a crash loop no rollback could answer: its
    /// starts spent their budget ([`LINUX_TRIAL_LAUNCHES`]) without proving it, and the
    /// build before it can't come back, so it stays until a newer release installs over it.
    pub kept_unproven: bool,
}

/// The same single background checker, sampling its source before each cycle so
/// configuration reloads take effect without restarting the process.
///
/// Supervised: the thread stamps [`checker_watch::WATCH`] as it goes; each cycle
/// runs under `catch_unwind` (a panic is a logged cycle, not the end of updating);
/// the machine-wide `checker.lock` is taken with a bound
/// ([`checker_watch::CHECKER_LOCK_WAIT`]) and a cycle that cannot get it defers;
/// and the arguments are kept so the window's watchdog can start a replacement
/// under a new generation ([`respawn_stalled_checker`]) when the stamp goes stale.
#[cfg(target_os = "macos")]
pub fn spawn_background_check_with_source(
    current_build: u64,
    source_provider: SourceProvider,
    notify: Option<HealthNotify>,
    on_staged: Option<StagedNotify>,
) {
    if !automatic() {
        // Said once per process, where the updater's lines go: the one record of why this
        // machine is not checking by itself. A typed or clicked check still runs.
        if enabled() {
            log(
                "automatic update checks are off — [update] enabled = false (Settings ▸ \
                 Software Update); Check for Updates still checks when asked",
            );
        }
        return;
    }
    // Not an installed `.app` (dev build, `target/` binary): nothing to swap, so no
    // thread. This is what keeps dev runs inert.
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
        check: std::sync::Arc::new(github::check_and_stage),
        pause: None,
        wake: &cadence::SYSTEM_WOKE,
    });
    let generation = checker_watch::WATCH.register(
        cadence::Cadence::new(std::time::Duration::from_secs(cadence::INTERVAL_SECS))
            .max_wait()
            .as_secs(),
    );
    // Kept for a replacement, which runs on the first spawn's arguments. A second
    // spawn's `register` would take the next generation and retire the first
    // thread, so there is still one checker.
    let _ = CHECKER_ARGS.set(std::sync::Arc::clone(&args));
    if let Err(error) = spawn_checker_thread(generation, args) {
        warn(&format!("could not start the update checker: {error}"));
    }
}

/// Tell the background check that the system just woke (the window's
/// `NSWorkspaceDidWakeNotification`): a wait under way ends at once, and the check
/// comes [`cadence::WAKE_SETTLE`] later, after the backoff a sleeping network earned
/// is reset (`settle_after_wake`). Without it the loop notices a wake only by the
/// wall clock, up to [`cadence::WAKE_SLICE`] late. Cheap and callable from any
/// thread: it takes one uncontended lock and notifies.
#[cfg(target_os = "macos")]
pub fn note_system_woke() {
    cadence::SYSTEM_WOKE.post();
}

/// How long the check loop pauses after the host did not answer its settings query
/// (the query itself is bounded by the host — the window's is 2 s).
#[cfg(target_os = "macos")]
const SETTINGS_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// Every how many consecutive settings misses the loop says so again (~7 s each:
/// the 2 s query plus [`SETTINGS_RETRY`], so about every seven minutes).
#[cfg(target_os = "macos")]
const SETTINGS_MISS_LOG_EVERY: u64 = 60;

/// Every how many consecutive `checker.lock` deferrals the loop says so again
/// (one per cycle: about an hour of them at the base interval).
#[cfg(target_os = "macos")]
const CHECKER_DEFERRAL_LOG_EVERY: u64 = 3600 / cadence::INTERVAL_SECS;

/// Every how many consecutive cycles that found this process's own check lane busy
/// the loop says so again.
#[cfg(target_os = "macos")]
const LANE_BUSY_LOG_EVERY: u64 = 6;

/// What the check loop runs on, shared by the thread and any replacement for it.
///
/// The two GUI hooks are `Send` but not `Sync` (their public types say so), and a
/// replacement may call them while the thread it replaced still holds a reference,
/// so each sits behind a mutex; a call only posts an event-loop message.
#[cfg(target_os = "macos")]
struct CheckerArgs {
    current_build: u64,
    source_provider: SourceProvider,
    health_hook: std::sync::Mutex<Option<HealthNotify>>,
    staged_hook: std::sync::Mutex<Option<StagedNotify>>,
    /// The pause after an unanswered settings query ([`SETTINGS_RETRY`]; a test
    /// passes zero).
    settings_retry: std::time::Duration,
    /// Where the loop's own reads of the staging root go (the checker lock, the
    /// dedup receipt, the health ledger): [`paths::Staging::resolve`] in the
    /// shipping loop, a scratch root in a test (the check resolves its own).
    staging: std::sync::Arc<dyn Fn() -> Option<paths::Staging> + Send + Sync>,
    /// How long a cycle waits for `checker.lock` before it defers
    /// ([`checker_watch::CHECKER_LOCK_WAIT`]; a test passes a short one).
    checker_lock_wait: std::time::Duration,
    /// After how many consecutive deferrals to a holder showing no progress the
    /// loop checks without the lock ([`checker_watch::CHECKER_UNGATED_AFTER`]; the
    /// deferral streak's test passes `u64::MAX`). The lock wait also sets the
    /// holder's side: its beat writes the lock every quarter of it, and a sibling
    /// reads a write within half of it as progress ([`holder_fresh`]).
    checker_ungated_after: u64,
    /// The network check and stage: [`github::check_and_stage`] in the shipping
    /// loop, a counting stand-in in a test (which never reaches the network).
    check: std::sync::Arc<CheckFn>,
    /// Every wait between cycles: `None` is the cadence's own (jittered, backed
    /// off, wake-aware — the shipping loop); a test passes a fixed pause, which a
    /// post to [`Self::wake`] ends as it ends the cadence's.
    pause: Option<std::time::Duration>,
    /// The wake signal every wait watches: [`cadence::SYSTEM_WOKE`], which the window
    /// posts on `NSWorkspaceDidWakeNotification` ([`note_system_woke`]); a test
    /// passes its own.
    wake: &'static cadence::WakeSignal,
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

/// One network check and stage for `(current build, channel)`
/// ([`github::check_and_stage`]'s shape).
#[cfg(target_os = "macos")]
type CheckFn = dyn Fn(u64, &Source) -> Result<Option<String>, String> + Send + Sync;

/// The arguments the process's checker was started with — what
/// [`respawn_stalled_checker`] starts a replacement on.
#[cfg(target_os = "macos")]
static CHECKER_ARGS: std::sync::OnceLock<std::sync::Arc<CheckerArgs>> = std::sync::OnceLock::new();

/// Start one checker thread under `generation`. The handle is dropped on purpose —
/// nothing joins a loop that never ends — and the heartbeat, not the handle, is
/// how its life is watched: a thread that could not even be created leaves its
/// `Starting` stamp to go stale, and the watchdog tries again. The caller says
/// the outcome.
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

/// Replace a STALLED checker (the window's watchdog): retire
/// `stalled_generation` and start a thread under the next one. `Some(new
/// generation)` when a replacement was started; `None` when there is nothing to
/// replace (no checker in this process, or `stalled_generation` is no longer the
/// live one — a second look at the same stall starts nothing), the replacement
/// budget ([`checker_watch::MAX_RESPAWNS`]) is spent, or its thread could not be
/// created.
///
/// The stalled thread cannot be killed; every stamp it makes from here on is
/// refused, and it exits on that cue wherever it wakes, so there are never two
/// checkers.
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

/// Linux supervises its own enrolled checker; other platforms run none.
#[cfg(not(target_os = "macos"))]
pub fn respawn_stalled_checker(_stalled_generation: u64) -> Option<u64> {
    None
}

/// Why a checker thread returned. The loop has no other way out: a cycle that
/// fails, finds nothing, defers or panics is followed by the next one.
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
    /// Another process held it past the bound: record a deferral and wait for the
    /// next cycle. Its check covers this interval, and waiting on a stopped or hung
    /// holder forever would stall this loop.
    Deferred,
    /// The lock itself failed (not contention): the gate is a cost device, never a
    /// correctness one, so the cycle proceeds ungated, as it always has.
    Unavailable,
}

/// A cycle that took `checker.lock` writes its pid into it, so a sibling that finds
/// the lock held for many cycles can name the holder. Best-effort, and not the lock
/// itself: the `flock` is on the open file description, which this write never
/// touches. An older build writes nothing, and the reader then names no pid.
#[cfg(target_os = "macos")]
fn note_checker_lock_holder(path: &std::path::Path) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    // The lock file exists (this cycle holds it): never created here, and never
    // through a link.
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        let _ = writeln!(file, "{}", std::process::id());
    }
}

/// The pid the last process to take `checker.lock` wrote into it
/// ([`note_checker_lock_holder`]), if readable and that process is still running.
/// An older holder writes nothing and leaves the last writer's pid, so the log says
/// the lock names it, not that it holds it.
#[cfg(target_os = "macos")]
fn checker_lock_holder(path: &std::path::Path) -> Option<u32> {
    let pid: u32 = read_ledger_text(path)?.trim().parse().ok()?;
    let raw = i32::try_from(pid).ok().filter(|raw| *raw > 0)?;
    // SAFETY: signal 0 delivers nothing; it only asks whether `raw` exists.
    let exists = unsafe { libc::kill(raw, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    exists.then_some(pid)
}

/// When this machine last completed an update check (the check receipt's
/// `updated_at`, written only by a completed check in any process), when that is
/// more than `stale` ago; `None` when fresher or there is no receipt.
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

/// Whether the process holding `checker.lock` at `path` has shown it is still
/// working within `fresh`: the lock file was written that recently, by the holder
/// taking it ([`note_checker_lock_holder`]) or by its [`HolderBeat`], which rewrites
/// it while that holder's checker is alive and its heartbeat fresh (downloads
/// included).
///
/// This tells a stopped holder from one mid-check: a stopped process writes
/// nothing, and a hung checker's beat stops with its heartbeat. An older-build
/// holder writes nothing after taking the lock and reads as not progressing. A
/// stamp from the future (the clock stepped back) is no progress, so a wrong clock
/// can defer no check.
///
/// The stamp is read before the clock, so a beat landing between the two reads
/// cannot look like a future stamp; the remaining skew is the clocks' granularity
/// (nanosecond file stamps vs microsecond `SystemTime::now` on macOS), which
/// [`HOLDER_STAMP_SLACK`] absorbs.
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

/// How far past this process's clock a lock stamp read first may be and still count
/// as written now ([`checker_holder_progressing`]): the clocks' granularity, not a
/// clock that stepped back.
#[cfg(target_os = "macos")]
const HOLDER_STAMP_SLACK: std::time::Duration = std::time::Duration::from_millis(5);

/// How recently a holder must have written `checker.lock` to count as making
/// progress ([`checker_holder_progressing`]): half this process's lock wait. A live
/// holder's [`HolderBeat`] writes every quarter of that wait, so half leaves a whole
/// beat of slack while a lock taken before the wait and never written again is
/// always past it.
#[cfg(target_os = "macos")]
fn holder_fresh(lock_wait: std::time::Duration) -> std::time::Duration {
    lock_wait / 2
}

/// The holder's beat: while a cycle holds `checker.lock` and its checker is alive
/// (this generation still live, its heartbeat fresh for its phase,
/// [`checker_watch::classify`]), a thread rewrites the lock every quarter of the
/// lock wait, so a sibling can tell a holder that is checking (a slow download
/// included, since its progress re-stamps the heartbeat) from one that is stopped
/// or hung ([`checker_holder_progressing`]). Joined on drop, before the lock is let
/// go ([`HeldGate`]'s field order), so it never writes this pid into a lock another
/// process then holds.
#[cfg(target_os = "macos")]
struct HolderBeat {
    stop: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
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

    /// Rewrites the lock at `path` every `every` while `alive` answers yes: the one
    /// beat both machine-wide locks' holders keep, alive while the checker doing the
    /// work is ([`checker_alive`]). `checker.lock`'s holder asks through
    /// [`Self::start`]; `stage.lock`'s through [`stage_holder_alive`]
    /// (`github::take_stage_lock`).
    fn start_with(
        path: std::path::PathBuf,
        every: std::time::Duration,
        alive: impl Fn() -> bool + Send + 'static,
    ) -> Self {
        let stop = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let flag = std::sync::Arc::clone(&stop);
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
                }
            })
            .ok();
        Self { stop, thread }
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

/// Whether a lock holder's work is alive, asked by its [`HolderBeat`] before each
/// stamp.
#[cfg(target_os = "macos")]
pub(crate) type HolderAlive = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// The one liveness rule for a lock holder: `generation` of `watch` is still the
/// live checker and its heartbeat is fresh for its phase
/// ([`checker_watch::classify`]). A stopped process runs no beat; a checker hung
/// inside its check lets its heartbeat go stale and the watchdog retires its
/// generation, so either way the answer turns to no and a waiting sibling can say
/// so.
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
    /// The liveness of the check running on this thread, set by the loop around its
    /// check ([`CheckAliveScope`]) so the stage lock's beat judges that check's own
    /// generation.
    static CHECK_ALIVE: std::cell::RefCell<Option<HolderAlive>> =
        const { std::cell::RefCell::new(None) };
}

/// While alive, the check on this thread is judged by `alive`
/// ([`stage_holder_alive`]); on drop (a panic included) the thread's previous
/// answer is restored.
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
    /// Whether the check running on this thread began inside a wake's grace
    /// (`cadence::after_wake`), set by the loop around its check ([`AfterWakeScope`]).
    static CHECK_AFTER_WAKE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// While alive, the check on this thread began inside a wake's grace when `after_wake`;
/// on drop (a panic included) the thread's previous answer is restored.
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

/// Whether the check running on this thread began inside a wake's grace — the loop's
/// own check, after a wake it detected ([`AfterWakeScope`]). A failure it books after
/// the channel answered (an asset GET) is then the network still coming up: weather,
/// which `github` books as `network`, not a verdict on the download pipeline. `false`
/// for a check with no loop around it ("Check for Updates", `check_now`).
#[cfg(target_os = "macos")]
pub(crate) fn check_follows_wake() -> bool {
    CHECK_AFTER_WAKE.with(std::cell::Cell::get)
}

/// What the `stage.lock` holder's beat asks before each stamp: the loop's own check
/// answers by its generation ([`checker_alive`], set by [`CheckAliveScope`]); a
/// check with no generation ("Check for Updates", `check_now`) by this process's
/// heartbeat as it stands. A process with no checker has no watchdog to judge a
/// hang by and keeps stamping while it runs.
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

/// A cycle's hold on `checker.lock`, with its [`HolderBeat`]. The beat is declared
/// FIRST so it drops first: it stops before the lock is released.
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

/// The check loop, under `generation` of `watch`. Returns only when a replacement
/// has taken over.
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
    // One base cadence (`cadence::INTERVAL_SECS`, no knob). A check spends no
    // metered requests: one HEAD of the evergreen
    // github.com/…/releases/latest/download/aterm-appcast.toml, whose 302 names the
    // newest tag, and tag-specific GETs on the same host only when that tag moved.
    // `cadence` owns the jitter, the failure backoff and the early return after
    // sleep.
    let interval = cadence::INTERVAL_SECS;
    let mut schedule = cadence::Cadence::new(std::time::Duration::from_secs(interval));
    let mut failures = cadence::FailureLog::default();
    // The last wake a wait detected: a check that fails inside its grace is a network
    // still coming up (`cadence::after_wake`), whatever it failed on.
    let mut woke_at: Option<std::time::Instant> = None;
    // The wake posts this loop has answered (`cadence::WakeSignal`): every wait ends on
    // one past them, and each settle answers the posts made before it ends.
    let wake_seen = std::cell::Cell::new(args.wake.posts());
    // Once per strand (`unreadable::StrandNotice`): announced once, then it
    // lives in `status.toml`; a readable check re-arms it.
    let mut strand_notice = unreadable::StrandNotice::default();
    // Per-process dedup, seeded from the clock at thread start so history
    // never re-notifies on every launch: the persistent-failure notice
    // requires the streak's latest failure to postdate this thread (RFC3339
    // strings compare chronologically), so a stale streak an earlier process
    // left — or a build that no longer checks at all — stays quiet. Keyed on
    // the class announced (`persistent_notice_is_new`), and WHAT was announced
    // is the PROCESS's, not this generation's (`ANNOUNCED_HEALTH`): a
    // replacement generation, and a successor for what its parent said, owe
    // the recovery of a class announced before them.
    let mut announcer = HealthAnnouncer::new(install::now_rfc3339());
    // The ledger's verdict, spoken to the GUI on every cycle, the skip path
    // included (the ledger is shared, so a cycle a sibling checked carries the same
    // evidence). Beside the streaks, the newer build waiting here (the staged
    // marker, or a bundle installed under this older image) is noted in the
    // ledger's pending clock each cycle, and one waiting past
    // `PENDING_UPDATE_OVERDUE_SECS` is announced with its typed cause.
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
    // The installed bundle last announced to the GUI (by build), re-announced when
    // the bundle moves again; and the last (build, error) that could not be
    // verified, so a permanently unverifiable bundle is logged once.
    let mut announced_installed: Option<u64> = None;
    let mut unverifiable_installed: Option<(u64, String)> = None;
    // The newest staged build this process has told `on_staged` about, so a
    // stage a SIBLING process published is announced once per build from the
    // skip path (see `announce_sibling_stage`).
    let mut announced_stage: Option<u64> = None;
    // An uncommitted candidate checks nothing: a handoff successor holds this thread
    // until the outgoing process commits to it, since the parent may still reject
    // it and two processes would read the stage with two opinions of it.
    if wait_while_uncommitted_handoff_candidate(UNCOMMITTED_CANDIDATE_HOLD_BOUND) {
        debug("the first update check waited for the handoff to commit");
    }
    // The last phase this generation stamped successfully — the one it names when a
    // refused stamp tells it that it has been replaced.
    let last = std::cell::Cell::new(CheckerPhase::Starting);
    let stamp = |phase: CheckerPhase| -> bool {
        let live = watch.beat(generation, phase);
        if live {
            last.set(phase);
        }
        live
    };
    // Every wait between cycles: the cadence's own, or a test's fixed pause; either
    // ends on a posted wake.
    let wait = |schedule: &cadence::Cadence| -> cadence::Waited {
        match args.pause {
            Some(pause) => cadence::sleep_watching_for_wake(args.wake, wake_seen.get(), pause),
            None => cadence::wait(schedule, args.wake, wake_seen.get()).1,
        }
    };
    // After a wait a wake ended: settle, then answer every post made by then.
    let settle = |schedule: &mut cadence::Cadence,
                  failures: &mut cadence::FailureLog,
                  woke_at: &mut Option<std::time::Instant>| {
        settle_after_wake(schedule, failures, woke_at);
        wake_seen.set(args.wake.posts());
    };
    // Repeats (an unanswered settings query, a held `checker.lock`, a busy lane) are
    // logged when they start, every Nth repeat, and when they end.
    let mut settings_misses = StreakLog::new(SETTINGS_MISS_LOG_EVERY);
    let mut deferrals = StreakLog::new(CHECKER_DEFERRAL_LOG_EVERY);
    // Of those, the cycles in a row behind a holder that showed no progress
    // (`checker_holder_progressing`): the count that ungates and is published.
    let mut stalled_deferrals: u64 = 0;
    // Whether this deferral streak has said it checks without the lock (once).
    let mut ungated_said = false;
    let mut lane_busy = StreakLog::new(LANE_BUSY_LOG_EVERY);
    loop {
        if !stamp(CheckerPhase::Settings) {
            return CheckerExit::Superseded { phase: last.get() };
        }
        // One cycle, unwind-isolated: a panic anywhere in a cycle (the host's
        // settings hook, a ledger parse, a helper) is one logged cycle. The lane
        // mutex it poisoned recovers on the next take (`check_lane::Lane::try_lock`),
        // the schedule backs off as for any failure, and the loop goes round again.
        let cycle = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Cycle {
            let settings = (args.source_provider)();
            // The query answered (or gave up): a recovery is logged first, so the
            // line lands even when this generation is about to learn it was
            // replaced.
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
                // A failed bounded config query is not permission to use a stale
                // channel: retry the provider without holding any lane. The first
                // miss is logged, a streak is restated every `SETTINGS_MISS_LOG_EVERY`
                // misses at WARN, and the answer that ends it is logged above.
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
            // The bundle under our own executable may have moved on without a
            // stage (the release cutter rewriting it, a user dragging a new `.app`
            // over it, a sibling's swap), none of which writes `ready.toml`. A stat of
            // its plist per cycle (read again only when it moved), plus the GUI's
            // codesign policy only when it says newer, lets the activation lane fire
            // on its own. A dev-marked bundle is skipped (`bundle::resolve` is
            // dev-mark-aware): it can never pass the shipped tier, so verifying it
            // would only spawn codesign forever.
            if args.has_staged_hook()
                && let Some(installed) = bundle::resolve()
                && let Ok(installed_build) = installed_build_observed(&installed.app_root)
                && installed_build > current_build
                // A build below the operator apply floor (a yank) is not an update
                // wherever it sits; announcing it would stage something every
                // handoff then refuses.
                && (args.staging)().is_none_or(|s| {
                    installed_build >= manifest::Floor::read(&s.floor()).min_build
                })
                && announced_installed != Some(installed_build)
            {
                // The version is a plist read, so it names the bundle whether or
                // not it verifies; read only when a line is about to say it.
                let version = || {
                    verify::bundle_short_version(&installed.app_root)
                        .unwrap_or_else(|_| format!("build {installed_build}"))
                };
                // Announce only what the GUI can import: the plist lands minutes
                // before the bundle is signed and notarized, and the GUI's facts
                // worker imports nothing it cannot verify and would not be told
                // again. So verify here before latching; an unverifiable newer bundle
                // is retried next cycle, not remembered.
                match verify::verify_bundle_policy(&installed.app_root, effective_team_id()) {
                    Ok(()) => {
                        announced_installed = Some(installed_build);
                        let version = version();
                        // "Switches to it" only when the host lands it by itself:
                        // with `auto_apply = false`, unsaved work, or a handoff this
                        // process cannot run, the build waits for a person.
                        log(&if automatic_apply_on() {
                            format!("update {version} is installed; aterm switches to it in place")
                        } else {
                            format!("update {version} is installed")
                        });
                        args.send_staged(installed_build, version);
                    }
                    Err(error) => {
                        // Once per (build, reason): the notarize window is minutes and
                        // a broken seal is forever; neither deserves a line per cycle.
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
            match check_lane().try_lock() {
                Some(mut lane) => {
                    // A retired generation that woke here leaves without marking
                    // the lane as its own.
                    if !watch.is_current(generation) {
                        return Cycle::Superseded(last.get());
                    }
                    // Marked held for exactly as long as the lane is: a stall from
                    // here on keeps the lane from any replacement, and the watchdog
                    // and the status line must be able to say so (`hold_lane`).
                    let lane_mark = watch.hold_lane(generation);
                    if let Some(cycles) = lane_busy.clear() {
                        debug(&format!(
                            "update checks can run again after {cycles} blocked cycle(s)"
                        ));
                    }
                    // Cross-process dedup. The lane mutex above is process-local and
                    // every terminal session runs this same loop, so N processes must
                    // cost about one check per interval: the flock serializes checkers
                    // machine-wide (the holder is bounded by the network timeouts), and
                    // the ledger re-read under it turns "another process just checked"
                    // into a quiet skip. The freshness window is 70% of the base,
                    // below the jittered minimum wait (80%), so a process cannot
                    // mistake its own previous stamp for another checker's and starve
                    // itself. A deferred stamp's window is wider than this process's
                    // next wait, so the deferring process aims its timer at that
                    // window's end (`Cadence::deferred`, the rate-limited arm below).
                    //
                    // Bounded: a cycle that waits out `CHECKER_LOCK_WAIT` records a
                    // typed deferral and takes the ordinary wait, so a stopped or hung
                    // sibling cannot park this thread (and every manual check queued
                    // behind the lane); whoever holds the lock is checking for this
                    // machine, and if it never finishes this process says so every few
                    // cycles.
                    let checker_staging = (args.staging)();
                    if !stamp(CheckerPhase::LockWait) {
                        return Cycle::Superseded(last.get());
                    }
                    let checker_lock = checker_staging
                        .as_ref()
                        .map(|s| s.status.with_file_name("checker.lock"));
                    let gate = checker_lock
                        .as_deref()
                        .map(|path| checker_gate(path, args.checker_lock_wait));
                    if !watch.is_current(generation) {
                        return Cycle::Superseded(CheckerPhase::LockWait);
                    }
                    // A holder that is not making progress: past
                    // `CHECKER_UNGATED_AFTER` deferrals in a row to a holder showing no
                    // progress, or with no check completed on this machine for
                    // `STALE_CHECK_AFTER` and the holder showing none, this cycle
                    // checks without the lock (the gate is a cost device). The streak
                    // stands, published and ended only by a cycle that gets the lock
                    // or a holder seen working, so the window's watchdog can say so.
                    //
                    // Progress is what the holder wrote: the lock file, rewritten by
                    // its `HolderBeat` while its checker is alive, so a holder deep in
                    // a slow download is not called stopped. A holder seen working is
                    // checking for this machine: its deferral is the ordinary kind,
                    // and it ends the streak.
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
                            // The cycles in a row behind a holder showing no
                            // progress: what ungates, and what the window's
                            // watchdog is shown.
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
                                // Said once per streak, when it starts checking
                                // without the lock, naming the holder.
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
                            // Published: the streak behind a holder showing no
                            // progress, which is what the window's watchdog warns
                            // on — never a holder that is still checking.
                            watch.set_deferrals(generation, stalled_deferrals);
                            if !ungated {
                                if !stamp(CheckerPhase::Waiting) {
                                    return Cycle::Superseded(last.get());
                                }
                                // The same release-then-wait as a dedup skip: neither
                                // the lane nor a flock is held across the sleep, so a
                                // manual check in this process runs at once.
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
                            // A held gate came from `checker_lock`: it is there.
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
                    // Past the gate, a deferral streak is over, unless this cycle is
                    // past it only because its holder is not making progress.
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
                        // A sibling's completed check is this machine checking.
                        watch.note_check(generation);
                        // The ledger is read only when there is a window to tell.
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
                        // Point the timer at the window's end (scattered), not the
                        // base's next tick, so the same skip is not re-read and re-logged
                        // every tick of the window.
                        let until = skip_timer_target(window_expiry, cadence::entropy_byte());
                        schedule.hold_until(
                            std::time::Instant::now()
                                + std::time::Duration::from_secs(until.saturating_sub(now_unix)),
                        );
                        // Release the flock and the local lane before sleeping (the
                        // former blocks siblings, the latter this process's manual
                        // checks), then take the loop tail's jittered wait (a bare
                        // `continue` would spin). A wake in it is settled as the
                        // tail's is: the next cycle may be the one that checks.
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
                    // Stamped before the check so the ledger can be asked afterwards
                    // whether this check recorded a failure (see the `Ok(None)` arm).
                    // RFC 3339 strings compare chronologically.
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
                    check_lane().complete(&mut lane, current_build, &source);
                    // A check that outlived its generation has already written what
                    // it found to the ledger and `status.toml`; its replacement's
                    // next cycle reads them and announces anything staged, so the
                    // stale thread reports nothing and leaves.
                    if !watch.is_current(generation) {
                        return Cycle::Superseded(CheckerPhase::Checking);
                    }
                    watch.note_check(generation);
                    match result {
                        Ok(Some(v)) => {
                            emit(failures.success());
                            after_healthy_check(&mut schedule);
                            // "is staged", not "was staged just now": the check also
                            // answers `Some` for a build already published and only
                            // waiting to be applied (a stage backoff never gates an
                            // apply). The line carries the ledger's failure count and
                            // reason for this build, since the apply lane's stand-down
                            // lives only in GUI memory.
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
                                         ({}); the next launch is the fallback",
                                        r.failures_for_target, r.last_failure
                                    ));
                                }
                                None => {
                                    log(&format!("update {v} is staged"));
                                }
                            }
                            // Surface the staged build to the GUI so it can arm the
                            // in-session apply lane and show the update-ready nudge.
                            // The build number comes from the ready marker (status
                            // reads it, no I/O).
                            if args.has_staged_hook()
                                && let Some(b) = status(current_build).and_then(|s| s.staged_build)
                            {
                                announced_stage = Some(b);
                                args.send_staged(b, v);
                            }
                        }
                        Ok(None) if unreadable::is_stranded() => {
                            // Not a success: GitHub answered that this machine cannot
                            // read the channel at all. Back off; the backoff clears on
                            // the first readable check, so a channel repaired
                            // mid-session is noticed within `max(MAX_BACKOFF,
                            // MAX_BACKOFF_INTERVALS × base)`.
                            schedule.failed();
                        }
                        Ok(None) if github::rate_limited() => {
                            // The download host asked us to slow down: a cadence
                            // problem, not a broken updater. Lengthen the wait but emit
                            // no failure line and no ledger entry, so a 429 never
                            // accrues the persistent-failure streak. Wake no earlier
                            // than the machine-wide window the deferral just stamped,
                            // read from the staging root the dedup gate read, still
                            // under this cycle's checker lock.
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
                            // A completed check that found nothing to do is a success,
                            // unless it wrote a failure to the ledger on its way here:
                            // the manifest dead end (an authoritative release that
                            // cannot be trusted) records its class and returns
                            // `Ok(None)`. Ask the ledger, not the return value, so such
                            // a check backs off instead of running at full cadence.
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
                        // A network that could not be reached at all (the first check
                        // after a cold boot, Wi-Fi still joining), or any failure inside
                        // a wake's grace (a TLS failure, a reset, a portal's page from a
                        // network still coming up), is retried within seconds on the
                        // cadence's short rungs and logged at INFO while those run: it
                        // is expected, and a WARN would bury real ones.
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
                    // Logged when it starts and every few cycles: a manual check
                    // overlapping one tick is one line, and a lane held by a stalled
                    // checker a replacement took over from (`respawn_stalled_checker`)
                    // is a streak, not a flood.
                    //
                    // Named when it is that stall: a generation the watchdog retired
                    // in its lock wait or check still holds the lane, so no check runs
                    // in this process until it lets go. That is a WARN from the first
                    // cycle, and the status line keeps the stall up
                    // (`checker_watch::standing_stall`).
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
            // A machine that cannot read its release channel can never update, and
            // this is deliberately not a health-ledger failure (a configuration
            // state, not a transient fault). Raise it on the broken-pipeline
            // notice's channel, once per strand, so the user learns this Mac is
            // stranded.
            if strand_notice.due(unreadable::is_stranded()) && args.has_health_hook() {
                let (title, body) = unreadable::notification();
                args.send_health(title, body);
            }
            speak_health(&mut announcer);
            // The heartbeat is judged against the schedule's longest wait, restated
            // every cycle from the schedule so a moving ceiling never leaves the
            // watchdog judging a stale wait.
            watch.set_max_interval(generation, schedule.max_wait().as_secs());
            if !stamp(CheckerPhase::Waiting) {
                return Cycle::Superseded(last.get());
            }
            // Jittered, backed-off, wake-aware wait. A detected wake returns early and
            // is settled before the next check (`settle_after_wake`).
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
                // A wake that ends this wait is settled and answered like every
                // other: left unanswered, its post would end every later wait at
                // once, and a cycle that keeps panicking would spin.
                if matches!(wait(&schedule), cadence::Waited::Woke(_)) {
                    settle(&mut schedule, &mut failures, &mut woke_at);
                }
            }
        }
    }
}

/// After a wait that ended on a detected wake: the backoff is cleared (the outage
/// belonged to a network this Mac may no longer be on), and so is the failure log's
/// "still failing" carry-over (a pre-sleep DNS failure is not evidence about the
/// post-wake network); the wake is dated for [`cadence::after_wake`]; and the network
/// gets [`cadence::WAKE_SETTLE`] to associate before the next check.
#[cfg(target_os = "macos")]
fn settle_after_wake(
    schedule: &mut cadence::Cadence,
    failures: &mut cadence::FailureLog,
    woke_at: &mut Option<std::time::Instant>,
) {
    log(&format!(
        "woke from sleep; checking for updates in {}s",
        cadence::WAKE_SETTLE.as_secs()
    ));
    schedule.woke();
    *failures = cadence::FailureLog::default();
    *woke_at = Some(std::time::Instant::now());
    std::thread::sleep(cadence::WAKE_SETTLE);
}

/// The installed bundle's build number (`CFBundleVersion`), read with PlistBuddy
/// only when its `Info.plist` has moved since the last read ([`INSTALLED_BUILD`]).
///
/// OBSERVATION, never trust: its two per-cycle callers only notice a newer build
/// installed under this older image — the pending-update clock, and the bundle
/// probe, which verifies the bundle (`verify::verify_bundle_policy`) before it
/// announces anything. Every trust decision reads the plist afresh after a verify
/// (`verify::bundle_build_number`). Every cycle of every window used to spawn
/// PlistBuddy twice for a number that changes once a release.
#[cfg(target_os = "macos")]
fn installed_build_observed(app_root: &std::path::Path) -> Result<u64, String> {
    observed_build_with(&INSTALLED_BUILD, app_root, verify::bundle_build_number)
}

/// What identifies one version of a bundle's `Info.plist`: the bundle, and the
/// file's device, inode, size, modification and status-change times. A bundle
/// dragged over or swapped in is a new inode; a plist rewritten in place moves its
/// mtime, and its ctime even under a restored mtime (no caller can set a ctime).
#[cfg(target_os = "macos")]
type PlistIdentity = (std::path::PathBuf, u64, u64, u64, i64, i64, i64, i64);

/// The last build [`installed_build_observed`] read, and the plist it read it from.
#[cfg(target_os = "macos")]
static INSTALLED_BUILD: std::sync::Mutex<Option<(PlistIdentity, u64)>> =
    std::sync::Mutex::new(None);

/// [`installed_build_observed`] over an injected cache and reader. The plist is
/// stat'ed BEFORE it is read, so a rewrite between the two leaves the older identity
/// beside the newer number and the next stat reads again; a read that fails (a
/// helper past its deadline, a swap in progress) is not remembered. The cache is
/// not held across the read: a checker generation stuck in the helper must not
/// hold up the replacement the watchdog starts, and two reads racing to store can
/// only leave an identity older than its file, which the next stat reads again.
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

/// A sibling's stage is this process's news too: a skipping cycle never reaches the
/// check that would answer `Some`, and the sibling's `on_staged` fired in its own
/// process. Reads the shared stage and announces a newer one once per build
/// (`announced` is the latch, shared with the check path's announcement), so this
/// process's apply lane arms. Returns whether it announced.
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

/// The schedule after a healthy check: the base interval, or, when the check found
/// the channel head's app build in flight (`github::head_in_flight`: a release
/// published source-first, its tag above the last one the machine authorized), the
/// quick [`cadence::IN_FLIGHT_RETRY`] wait while the receipt's machine-wide count is
/// within [`cadence::IN_FLIGHT_CHECKS`]. Said at the first such check and when they are
/// spent; the ones between, a minute apart, at DEBUG.
#[cfg(target_os = "macos")]
fn after_healthy_check(schedule: &mut cadence::Cadence) {
    let in_flight = github::head_in_flight();
    schedule.succeeded(in_flight.as_ref().map_or(0, |(_, checks)| *checks));
    let Some((head, checks)) = in_flight else {
        return;
    };
    match cadence::in_flight_retry(checks) {
        Some(rung) => {
            // A rung never outwaits the base (`Cadence::nominal_at`): say the wait taken.
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

/// Route a [`cadence::LogAction`] to the app log. `None` (nothing to say) is the
/// common case, so the call sites stay one line.
#[cfg(target_os = "macos")]
fn emit(action: Option<cadence::LogAction>) {
    match action {
        Some(cadence::LogAction::Warn(text)) => warn(&text),
        Some(cadence::LogAction::Log(text)) => log(&text),
        Some(cadence::LogAction::Suppress) | None => {}
    }
}

/// No-op where nothing updates (Linux runs its own checker, reached through
/// [`spawn_background_check_with_settings`]).
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn spawn_background_check_with_source(
    _current_build: u64,
    _source_provider: SourceProvider,
    _notify: Option<HealthNotify>,
    _on_staged: Option<StagedNotify>,
) {
}

/// Consecutive same-class failed checks at which the failure is called persistent
/// (drives [`UpdateStatus::is_failing_persistently`], the status wording, the GUI
/// notification, and `aterm-ctl update`'s `persistent=` field). Cross-platform so
/// status consumers can reason about it; the macOS `health` ledger uses the same
/// threshold, beside [`PERSISTENT_SPAN`].
pub const PERSISTENT_AFTER: u32 = 3;

/// How long a streak a check books must span, on its class's own clock, before the
/// macOS ledger calls it persistent (`health::Health::persistent_class`), and how
/// long any escalated class must have stood before the loud notice speaks. Half an
/// hour: a GitHub incident or a slow asset upload is over well inside it, and a broken
/// pipeline is still named within the hour. Time, not checks, so the check cadence
/// (`cadence::INTERVAL_SECS`) can change without moving it.
pub const PERSISTENT_SPAN: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// How long a newer build may wait on this machine (staged, or installed under an
/// older running image) before the updater raises the loud notice
/// (`health_failing_title("apply")`, "Couldn't install updates") with the typed
/// cause.
///
/// One hour is far past the automatic apply ladder's bound (aterm-gui's
/// `LANDS_WITHIN` plus `SWITCH_ALLOWANCE`) and the physical lane's first retry
/// (600 s); the GUI asserts against this constant at compile time. An update not
/// landed by then is stuck. Cross-platform so the GUI's assert can name it.
pub const PENDING_UPDATE_OVERDUE_SECS: u64 = 60 * 60;

/// Whether the host's automatic apply lane is on (`[update] auto_apply`), as the
/// GUI last reported through [`set_automatic_apply`]. Defaults to on, the shipped
/// default.
static AUTOMATIC_APPLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Tells the updater whether the host applies staged builds by itself. The overdue
/// notice ([`PENDING_UPDATE_OVERDUE_SECS`]) claims an update that should have landed
/// on its own has not; with `[update] auto_apply = false`, unsaved work a person has
/// to save, or an in-session handoff the process cannot run, the build waits for a
/// person by design and the host already says so. Those postures are the GUI's, so
/// it reports `on` = "lands by itself" here whenever it changes.
pub fn set_automatic_apply(on: bool) {
    AUTOMATIC_APPLY.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// See [`set_automatic_apply`].
#[cfg(target_os = "macos")]
fn automatic_apply_on() -> bool {
    AUTOMATIC_APPLY.load(std::sync::atomic::Ordering::Relaxed)
}

/// Serializes the tests that mutate the process-global "this machine cannot read
/// its release channel" latch (`unreadable::STRANDED`), since a crate's tests run
/// as threads of one process.
#[cfg(all(test, target_os = "macos"))]
pub(crate) static STRANDED_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A snapshot of the updater's state for the "Check for Updates" menu and the
/// `aterm-ctl update` query, read from the durable status/ready markers (the last
/// background check and any staged build) without network I/O.
#[derive(Debug, Clone)]
pub struct UpdateStatus {
    pub linux: Option<LinuxUpdateStatus>,
    /// Whether the updater is configured to act on this build/machine.
    pub enabled: bool,
    /// Whether this launch has a bundle the updater could replace. `false` for a run
    /// from the mounted DMG, a Gatekeeper-translocated copy, or a dev-marked install
    /// (`bundle::resolve` returns `None`): no checker thread starts and the ledger
    /// stays empty, so the panel must not report "You're up to date".
    pub installable: bool,
    /// The running build number.
    pub current_build: u64,
    /// Build number staged for next-launch apply, if any.
    pub staged_build: Option<u64>,
    /// Human version of the staged build, if any.
    pub staged_version: Option<String>,
    /// Git commit of the staged build's source (from the ready marker, copied from
    /// the release manifest at stage time), if known, so a controller can compare it
    /// against a repo commit before applying.
    pub staged_commit: Option<String>,
    /// Canonical SHA-256 of the staged DMG from the ready marker. Unlike commit
    /// provenance, this identifies the exact artifact bytes authorized for apply.
    pub staged_dmg_sha256: Option<String>,
    /// "What changed" notes for the staged build (from the manifest), if any.
    pub changelog: Option<String>,
    /// The updater's last decision (e.g. `"up to date (latest release build 824)"`).
    pub outcome: String,
    /// RFC3339 UTC time of the last completed-check receipt (empty if absent).
    /// Use [`last_check_at`] to require the current source and running build.
    pub updated_at: String,
    /// Self-healing ledger snapshot: total consecutive failed checks across classes
    /// (0 = healthy).
    pub failing_checks: u32,
    /// Class of the most recent failure: `"network"` / `"pipeline"` / `"manifest"` /
    /// `"stage"` / `"apply"` / `""`.
    pub failing_kind: String,
    /// Consecutive apply-lane failures: a verified build was staged but never became
    /// the running build. Separate from `failing_checks` because "can this machine
    /// download an update?" and "can it move to it?" are different questions; a zero
    /// acquisition score beside a non-zero value here is a stuck machine.
    pub failing_applies: u32,
    /// RFC3339 UTC start of the current unhealthy period (empty if healthy).
    pub failing_since: String,
    /// Whether the ledger's `pipeline` streak crossed [`PERSISTENT_AFTER`] — the
    /// "this build cannot download while releases are right there" state.
    pub failing_persistent: bool,
    /// The class of the standing acquisition streak (the one
    /// [`Self::failing_checks`] counts), or `""` when every acquisition streak is
    /// clear. Distinct from [`Self::failing_kind`] (the most recent failure of any
    /// class, apply included), so `failing=<n>:<kind>` never pairs an acquisition
    /// count with an apply label.
    pub failing_checks_kind: String,
    /// The stranded verdict: the last completed check proved this machine cannot
    /// read its release channel (401/403/404 with nothing to try, or a renamed repo)
    /// and will never update until an operator acts. Records no health-ledger
    /// failure (a configuration state is not a transient fault), so
    /// [`Self::failing_persistent`] cannot say it and surfaces must read this field.
    /// The explanation and remedy are in [`Self::outcome`].
    pub channel_unreadable: bool,
}

impl UpdateStatus {
    /// A single-line human summary (for a menu title / `OK` status line).
    #[must_use]
    pub fn summary(&self) -> String {
        // A Ready marker certifies staged bytes, not permission to apply them: keep
        // the recorded decision, including refusals with zero failures. Health
        // classes stay the ledger's decision; rendering must not infer one here.
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

    /// Whether the ledger says update checks are PERSISTENTLY failing on the pipeline
    /// class (releases visible, downloads impossible) — the surface-it-loudly state.
    #[must_use]
    pub fn is_failing_persistently(&self) -> bool {
        self.failing_persistent
    }

    /// An empty (all-defaults) snapshot for the stub/fallback constructors.
    fn empty(enabled: bool, current_build: u64, outcome: String) -> Self {
        Self {
            linux: None,
            enabled,
            // The fallback/stub path has no bundle to speak for; claiming one would
            // be the very over-claim `installable` exists to prevent.
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

/// "Check for updates automatically" is not whether the updater runs: [`enabled`] is
/// the platform alone, so a typed `aterm update check`, a clicked Check for Updates
/// and Update Now are never refused by the switch; only the background checker
/// ([`automatic`]) is, and it can never be on where the updater cannot run.
/// The loop's schedule after a healthy check ([`after_healthy_check`]), at the shipped
/// base: the one-minute re-check while the head in flight's count (the receipt's) is
/// within `cadence::IN_FLIGHT_CHECKS`, the base interval once those are spent, and the
/// base interval (the negative control) after a check that found no head in flight.
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

    /// A skip with a newer staged build calls `on_staged` exactly once across two
    /// skips; a later, newer stage is news again. Negative controls: a stage not newer
    /// than the running build, no stage, and no hook announce nothing.
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
        // A still newer stage is news.
        let newest = staged(Some(86));
        assert!(announce_sibling_stage(
            &mut announced,
            81,
            Some(&newest),
            Some(&hook)
        ));
        assert_eq!(calls.lock().unwrap().len(), 2);

        // Negative controls.
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

    /// A staged build never conceals the decision recorded beside it: the
    /// stage-first summary once replaced every refusal and failure with the same
    /// "staged and ready to apply" line.
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
///
/// The release manifest (`Manifest::commit`) carries the full 40-hex commit, while
/// the compiled-in `ATERM_GIT_COMMIT` and the bundle's `ATermGitCommit` plist key
/// carry a 12-hex short form, optionally with `-dirty`, so plain `==` would report a
/// false mismatch. Every commit comparison routes through this one function.
///
/// After `trim` + lowercase:
/// * empty, `"unknown"`, a `-dirty` suffix, or any non-hex input ⇒ never matches (a
///   dirty tree is not that commit);
/// * otherwise the shorter must be a hex prefix of the longer and at least 7 chars
///   (git's default abbreviation floor), so a stub can't match everything. Two full
///   hashes reduce to equality.
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
    // Every phrasing that asserts "a build is staged" must be listed here, or a stale
    // line survives `reconcile_status_outcome` and keeps advertising a stage that no
    // longer exists. The stage/backoff lanes write "ready to apply"; older builds'
    // lines all begin "staged ", which the prefix covers.
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
/// `NativeUpdateStatusReconciliation.ReconcileStatus` transition. Proof/test builds
/// only; its inputs are observations of the real reducer (including the branch it
/// returned), not a second implementation of its rule.
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
    // A marker for the running build (or an older one) is residue, not a staged
    // update. The rule lives in the reconciler so every caller and the Tier-1
    // projection see the same strict-newer decision.
    let ready_present = ready_build.is_some_and(|build| build > running_build);
    let claims_staged = persisted_claims_stage(&persisted);
    if !ready_present && (checked_from_build != running_build || claims_staged) {
        StatusReconciliation {
            current_build: running_build,
            ready_present,
            // The state first; the reason only where it is true (a ledger another
            // build of aterm wrote). No build number: `--version` and About own it.
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

/// Whether a failed check takes the quick [`cadence::OFFLINE_RETRY`] rungs: curl
/// could not reach the network ([`cadence::is_network_unreachable`]) and the health
/// ledger filed this check's failure (`last_failure_at` at or after
/// `check_started`) as `network`, so the channel was never reached. The same curl
/// exit on the container download is a `pipeline` failure: the network is up, and
/// quick retries would re-download the container and report a slow link as failing.
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

/// How much a recorded deferral widens the machine-wide freshness window, as a
/// multiple of the base interval.
///
/// A 429 is per IP, so the retreat must be per machine: `Cadence::failed` lengthens
/// only the wait of the process that saw it, while siblings judge its receipt
/// against their own base. Two intervals (a fresh window of 1.4 of them) outlast
/// every sibling's next wait (0.8–1.2 of the base) and the deferring process's own
/// first rung (one base interval), so the deferring process aims its own timer at
/// this window's end (`Cadence::deferred`). Never shorter than
/// [`DEFERRED_WINDOW_FLOOR`].
#[cfg(target_os = "macos")]
const DEFERRED_WINDOW_INTERVALS: u32 = 2;

/// The shortest window a recorded deferral holds the machine off the download host
/// for, before the 70 % freshness cut: twenty minutes, a fresh window of 14, which is
/// what two of the old ten-minute intervals held. A host that answered 429 asked for
/// time, not for a number of checks, so a faster cadence must not shorten the
/// retreat. Inside the deferring process's backoff ceiling (`Cadence::cap`, at
/// least `MAX_BACKOFF`, 15 minutes), so its own held timer still reaches the end.
#[cfg(target_os = "macos")]
const DEFERRED_WINDOW_FLOOR: std::time::Duration = std::time::Duration::from_secs(20 * 60);

#[cfg(target_os = "macos")]
const _: () = assert!(
    DEFERRED_WINDOW_FLOOR.as_secs() * 7 / 10 + SKIP_JITTER_SECS
        <= cadence_bounds::MAX_BACKOFF.as_secs(),
    "a deferring process whose held timer is capped short of its own window skips on its own note"
);

/// How far past a sibling's window a skipping process scatters its wake (0–60 s,
/// from one entropy byte), so siblings released by the same receipt do not all check
/// in the same second; also how far ahead of the clock a receipt stamp may sit
/// before it is treated as absent.
#[cfg(target_os = "macos")]
const SKIP_JITTER_SECS: u64 = 60;

/// Why this cycle must not spend a network check, if it must not: whether the
/// source/build-bound receipt records a check completed within the window, i.e.
/// another aterm process (the window or a sibling session) already made this
/// interval's check.
///
/// The window is 70% of the base interval, below the jittered minimum wait (80%),
/// so a process cannot mistake its own previous healthy stamp for another checker's
/// and starve itself.
///
/// Deferrals stamp the receipt and widen the window by
/// [`DEFERRED_WINDOW_INTERVALS`], so a machine just told to slow down is not
/// re-poked by a sibling on its own faster timer. That window also outlasts the
/// deferring process's first backoff rung, so that process holds its timer to the
/// window's end (`Cadence::deferred`). The next healthy check overwrites the
/// receipt and the window returns to the base.
#[cfg(all(target_os = "macos", test))]
fn checker_skip(staging: &paths::Staging, base: std::time::Duration) -> Option<String> {
    checker_skip_at(staging, base, unix_now_secs()).map(|(reason, _)| reason)
}

/// Where a skipping process points its timer: the sibling window's expiry,
/// scattered by 0–[`SKIP_JITTER_SECS`], since every fresh sibling derives the same
/// epoch from the same receipt and would otherwise wake in the same second.
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

/// [`checker_skip`] for this build and source at an injected `now`: the reason, and the
/// epoch the fresh window ends at (for [`skip_timer_target`]). One clock read and one
/// parse decide both.
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
    // This file is written only after a completed check; status/apply writes
    // cannot refresh its timestamp, deferral or source.
    let checked = v.get("updated_at").and_then(aterm_toml::Value::as_str)?;
    if checked.is_empty() {
        return None;
    }
    // The widened window belongs to the host's backoff, and only the check receipt's
    // own `outcome = "deferred"` says so: the apply lane writes "deferred: install
    // location not writable" and similar for deferrals that touched no network, which
    // must not cost a check holiday.
    let deferred = v
        .get("outcome")
        .and_then(aterm_toml::Value::as_str)
        .is_some_and(|outcome| outcome == "deferred");
    // A check that found the channel head's app build in flight asked for the next
    // one on the quick wait (`cadence::IN_FLIGHT_RETRY`), keyed on this receipt's
    // count. The window may not outlast that rung, and whichever process checks next
    // reads the same count, so the machine walks one ladder however many processes
    // run it. A deferral never records a head in flight
    // (`github::check_and_stage`), and its widened window stands.
    let window = if deferred {
        base.saturating_mul(DEFERRED_WINDOW_INTERVALS)
            .max(DEFERRED_WINDOW_FLOOR)
    } else {
        cadence::in_flight_retry(check_receipt::head_in_flight_checks(&v))
            .map_or(base, |rung| rung.min(base))
    };
    let fresh_window = window.as_secs().saturating_mul(7) / 10;
    // A stamp ahead of the clock is treated as absent: a clock stepped backwards
    // after the write (an NTP correction) would otherwise hold every checker on the
    // machine, and with every loop skipping nothing would overwrite it.
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

// The macOS check lane's clock (the checker window and its receipt) is the install
// lane's: `0` when the clock cannot be read, so every window reads as expired, which
// only releases. The Linux updater keeps its own.
#[cfg(target_os = "macos")]
use install::unix_now_secs;

/// How the last check's assets were delivered, from the status ledger: `deferred`
/// (the host asked us to wait) or `blocked` (the download host did not serve an
/// asset the release names), when the last check did not simply succeed. `None`
/// for a healthy check, and [`Self::status_line_suffix`] then adds nothing. Read
/// separately from [`UpdateStatus`] (same file) so hand-built `UpdateStatus` values
/// keep compiling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delivery {
    pub note: Option<String>,
}

impl Delivery {
    /// Parse the delivery note out of a `status.toml` text; absent ⇒ `None`.
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

    /// The ` delivery=<note>` token for the `aterm ctl update status` line, with a
    /// leading space so the caller can splice it onto the line as-is. Empty for a
    /// healthy check.
    #[must_use]
    pub fn status_line_suffix(&self) -> String {
        self.note
            .as_deref()
            .map(|note| format!(" delivery={note}"))
            .unwrap_or_default()
    }
}

/// The [`Delivery`] facts in this machine's status ledger, or `None` when there is no
/// ledger to read. No network I/O.
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

/// Why [`status`] answered `None` on macOS, in a person's words: the one
/// place both `aterm update status` and `aterm ctl update status` take them
/// from, so the two never disagree about a ledger neither can read.
/// `Staging::resolve` gives up when `ensure_private_dir` fails on `Updates` or
/// its `download` folder; this re-runs it and names what failed: its own
/// ownership/mode/symlink refusal, or the system's error (a full disk, a
/// read-only volume, a parent it may not enter).
#[cfg(target_os = "macos")]
#[must_use]
pub fn ledger_unreadable_cause() -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return "HOME is not set".to_string();
    };
    let root = std::path::PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("aterm")
        .join("Updates");
    let shown = "~/Library/Application Support/aterm/Updates";
    let probe = aterm_update_core::ensure_private_dir(&root)
        .and_then(|()| aterm_update_core::ensure_private_dir(&root.join("download")));
    match probe {
        // An error with no OS code is `ensure_private_dir`'s own refusal.
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

/// Reads the durable updater state (last outcome and any staged build's
/// version/changelog) without network I/O. `None` only when the staging area can't
/// be resolved (no `HOME`); otherwise a snapshot even if no check has run yet.
#[cfg(target_os = "macos")]
#[must_use]
pub fn status(current_build: u64) -> Option<UpdateStatus> {
    let staging = paths::Staging::resolve()?;
    // Outcomes come from the any-writer status marker; only the completed-check
    // receipt can supply the last-check timestamp.
    let (mut checked_from_build, mut outcome) = (current_build, String::new());
    let updated_at = check_receipt::completed_at(&staging).unwrap_or_default();
    if let Some(text) = read_ledger_text(&staging.status)
        && let Ok(v) = text.parse::<aterm_toml::Value>()
    {
        // Best-effort (via `u64::try_from` — the verifier cannot lower i64<->u64
        // `as` casts): absent or negative reads keep the running build.
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
    // Staged build details come from the ready marker (present only when one is
    // staged).
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
    // Self-healing ledger snapshot (per-class failure streaks).
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
/// General status/apply writes cannot refresh it; an absent or older-format
/// receipt returns `None` rather than advertising a check that has not run.
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

/// When this process's background check loop next looks at the channel (RFC 3339
/// UTC), or `None` while no wait is under way (no loop in this process, or it is
/// settling after a wake or checking). This is the loop's own timer (the ladder, a
/// hold to a sibling's or a deferral's window, the quick in-flight rungs); a sibling
/// may check sooner and this one then skips. Beside [`last_check_at`] it answers
/// "when was the channel read, and when next" on `aterm ctl update status`.
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

/// Linux reports its enrolled executable state; other platforms have no updater.
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

/// How old a completed check may be before `aterm ctl update status` flags the
/// ledger as frozen (`stale_check=`). Derived from the check cadence
/// (`cadence_bounds::STALE_CHECK_AFTER`): five of the longest waits the loop
/// legitimately takes, about four hours.
pub const STALE_CHECK_AFTER: std::time::Duration = cadence_bounds::STALE_CHECK_AFTER;

/// Whether `stamp`, an RFC 3339 UTC timestamp as this crate writes them
/// ([`UpdateStatus::updated_at`], the health ledger), is older than `secs` ago.
///
/// The fixed-shape UTC strings compare chronologically, so this is one lexical
/// comparison against `now - secs` rendered the same way. An empty or odd-shaped
/// stamp is not stale (a never-checked ledger has its own signals, and a malformed
/// one must not raise a flag): anything not starting with an ASCII digit is refused.
#[must_use]
pub fn rfc3339_older_than(stamp: &str, secs: u64) -> bool {
    if !stamp.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return false;
    }
    let now = aterm_time::unix_secs().unwrap_or(0);
    let threshold = aterm_types::rfc3339::format_rfc3339(now.saturating_sub(secs));
    !threshold.is_empty() && *stamp < *threshold
}

/// Run ONE update check + stage synchronously and return the resulting [`UpdateStatus`]
/// (the "Check for Updates" action). BLOCKS on network + disk (download/verify/stage up
/// to tens of seconds), so callers MUST run it off the UI/event-loop thread. A person
/// asked for it, so "Check for updates automatically" being off does not stop it
/// ([`enabled`], not [`automatic`]), and a staging MOMENT's re-download memo does not
/// hold it ([`github::Asked::Person`]: the refusal told them to free some space and
/// check); on an uninstalled copy it just reports state.
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
        check_lane().run_or_join(current_build, source, || {
            if let Err(error) =
                github::check_and_stage_for(current_build, source, github::Asked::Person)
            {
                warn(&format!("manual update check failed: {error}"));
            }
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

/// Why a manual check cannot run right now, or `None` when it can: a retired
/// checker generation of `watch` still holds this process's check lane
/// ([`checker_watch::CheckerBeat::blocked_by`]), and a check would only queue
/// behind it.
#[cfg(target_os = "macos")]
fn manual_check_blocked(watch: &checker_watch::CheckerWatch) -> Option<String> {
    let holder = watch.snapshot()?.blocked_by()?;
    Some(format!(
        "update check could not run: an earlier check stopped answering while {}",
        holder.phase.words()
    ))
}

/// Stub where nothing updates (Linux checks through [`check_now_with_settings`]'s own
/// lane).
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn check_now(current_build: u64, _source: &Source) -> UpdateStatus {
    UpdateStatus::empty(
        false,
        current_build,
        "auto-update is unsupported on this platform".into(),
    )
}

/// Emits an informational updater line through `aterm_log` (the global logger
/// `aterm-gui` installs before the updater runs), so it lands in the app log file,
/// visible for a Finder-launched `.app`. Low-volume: most runs print nothing. A
/// no-op if no logger is installed (e.g. a dev harness).
#[cfg(target_os = "macos")]
pub(crate) fn log(msg: &str) {
    #[cfg(test)]
    log_capture::record(aterm_log::Level::Info, msg);
    aterm_log::info!("aterm-update: {msg}");
}

/// Emits an updater line at DEBUG (see [`log`]): a routine fact kept out of the
/// default log (the lane this process reads, the machine that signed a release it
/// already named once).
#[cfg(target_os = "macos")]
pub(crate) fn debug(msg: &str) {
    #[cfg(test)]
    log_capture::record(aterm_log::Level::Debug, msg);
    aterm_log::debug!("aterm-update: {msg}");
}

/// Whether `key` is new to this process, for a line said once per process (or once
/// per release per process). Under `cfg(test)` the memory is per thread, like
/// [`log_capture`], so a sibling test cannot swallow a test's first sighting.
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
///
/// Some log contracts are a level, e.g. the roster-authorized rotation note must be
/// INFO exactly once (see `github::fetch_authoritative_release`). `aterm_log`'s
/// global logger is process-wide and shared by parallel test threads, so under
/// `cfg(test)` [`log`]/[`warn`] feed a thread-local here: each test sees exactly
/// its own thread's lines.
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod log_capture {
    use std::cell::RefCell;

    thread_local! {
        static LINES: RefCell<Vec<(aterm_log::Level, String)>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn record(level: aterm_log::Level, msg: &str) {
        LINES.with(|lines| lines.borrow_mut().push((level, msg.to_string())));
    }

    /// Drain this thread's captured lines. Call once BEFORE the action under test to
    /// clear residue from earlier code on the same thread, and again after to read.
    pub(crate) fn take() -> Vec<(aterm_log::Level, String)> {
        LINES.with(|lines| std::mem::take(&mut *lines.borrow_mut()))
    }
}

// The channel-failure classifier and health ledger are macOS-only; these
// integration tests compile only where the background updater lane ships.
#[cfg(all(test, target_os = "macos"))]
mod checker_gate_tests {
    use super::unreachable_before_the_channel;

    /// The quick rungs are for a channel that was never reached: a DNS error on the
    /// HEAD, filed `network` by this check, takes them. The same curl exit on the
    /// container download (filed `pipeline`), a `network` record an earlier check
    /// left, and a `network` failure that is not a transport exit do not.
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

    /// A check receipt exactly as `check_receipt::record` writes one: `outcome` is
    /// `completed` or `deferred`.
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

    /// A skipping process sleeps to the window's end: a sibling's fresh check hands
    /// it the window's expiry (`checked_at + 0.7 × base`), scattered by 0–60 s so
    /// fresh siblings do not wake in the same second.
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

    /// A STAMP FROM THE FUTURE HOLDS NO CHECKER. After a clock step backwards (NTP
    /// correcting a wrong clock, a VM restored from a snapshot) a stamp ahead of now
    /// once made every loop on the machine skip until wall time passed it, and
    /// nothing overwrote the stamp because every loop skipped.
    #[test]
    fn a_stamp_ahead_of_the_clock_defers_no_check() {
        let s = staging("future");
        write_receipt(&s, 0, "completed");
        assert!(checker_skip_at(&s, BASE, now() - 2 * 3600).is_none());
        let _ = std::fs::remove_dir_all(&s.root);
    }

    /// The retreat is per machine, because a 429 is: a sibling on its own
    /// un-backed-off timer must not re-poke the host during the backoff.
    #[test]
    fn a_recorded_deferral_holds_off_every_process_not_just_the_one_that_saw_it() {
        let s = staging("deferred");
        write_receipt(&s, 25 * 60, "deferred");
        let reason = checker_skip(&s, BASE).expect("the deferral widens the window");
        assert!(reason.contains("deferred"), "{reason}");
        // Bounded, not permanent: past the widened window (70% of 2×base = 42 min) the
        // machine tries again, and one healthy check restores the base window.
        write_receipt(&s, 50 * 60, "deferred");
        assert!(checker_skip(&s, BASE).is_none(), "the retreat self-heals");
        write_receipt(&s, 25 * 60, "completed");
        assert!(
            checker_skip(&s, BASE).is_none(),
            "a healthy check returns the window to the base interval"
        );
        // Negative control: only the receipt's own `deferred` widens; an apply-lane
        // sentence that merely contains "deferred" (it touched no network) does not.
        write_receipt(&s, 25 * 60, "deferred: install location not writable");
        assert!(checker_skip(&s, BASE).is_none());
        let _ = std::fs::remove_dir_all(&s.root);
    }

    /// A process never skips its own deferred note: the deferring process's timer
    /// lands at or past the window it stamped (scattered as a skip's is), so the
    /// check it wakes for is not skipped, for every entropy byte. Negative control:
    /// the ladder alone (`failed`) wakes inside the window for every byte, and the
    /// gate skips it.
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

    /// A 429 ASKED FOR TIME, NOT A NUMBER OF CHECKS. At the shipped two-minute base two
    /// intervals are a 168 s window, so the retreat is floored
    /// ([`super::DEFERRED_WINDOW_FLOOR`]): a deferral 13 minutes old still holds every
    /// process off the host, one 15 minutes old does not, and the deferring process's
    /// own held timer at that base still wakes past the window, for every entropy byte.
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
        // NEGATIVE CONTROL: a completed check 13 minutes old holds nobody.
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

    /// The `delivery=` token appears only when the ledger recorded a note, so a
    /// healthy line carries nothing.
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

/// The per-cycle read of the installed bundle's build number spawns PlistBuddy only
/// when the plist moved ([`super::observed_build_with`]).
#[cfg(all(test, target_os = "macos"))]
mod installed_build_tests {
    use std::cell::Cell;
    use std::sync::Mutex;

    /// AN UNCHANGED PLIST IS READ ONCE. Cycles over the same `Info.plist` reuse the
    /// number its one read gave; a plist rewritten in place (another size, or the
    /// same size under a restored mtime), or a bundle swapped in by rename (a new
    /// inode), is read again; a read that failed is not remembered; and with no plist
    /// to stat, every call reads (and fails as the reader does).
    #[test]
    fn an_unchanged_plist_is_read_once_and_a_moved_one_again() {
        let dir = aterm_tempfile::TempDir::new().expect("scratch");
        let app = dir.path().join("aterm.app");
        let plist = app.join("Contents/Info.plist");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        let cache = Mutex::new(None);
        let reads = Cell::new(0u32);
        // What the stand-in PlistBuddy answers; `None` is a read that failed.
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
        // "never checked" and "someone hand-edited the ledger" both have their own
        // signals; a scary staleness flag must not be one formatting slip away.
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

    use super::{
        ReconciledStatusOutcome, StatusReconciliation, commit_matches, compiled_update_pin_sha256,
        persisted_claims_stage, reconcile_status_outcome, status_reconciliation_projection,
        update_pubkey_sha256,
    };
    // The predicate under test is itself macOS-gated (the notice belongs to the
    // updater, which has no other lane) — the import and its test ride the same cfg.
    #[cfg(target_os = "macos")]
    use super::persistent_notice_is_owed;

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

        // The update pin is the paper master's fingerprint: armed in this tree, so 64
        // hex, exactly the master head's.
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
        // None plus older/equal/newer concrete Ready markers. Some(3) is needed
        // to exercise a strictly-newer marker for running build 2; the model
        // abstracts all concrete marker builds to `ready_present ∈ {0,1}`.
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

        // Historical defect 1: a mismatched ledger relabels the running caller.
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

        // Historical defect 2: mere marker presence (here equal, not newer) is
        // treated as an active stage and preserves stale staged prose.
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

        // The over-correction, one conjunct of the neutralization rule dropped at a
        // time. The real reducer PRESERVES both outcomes; each mutant rewrites one.
        for (running, ledger, ready, persisted, mutant, law) in [
            // A strictly newer Ready marker is the authority for staged details.
            (
                1,
                2,
                Some(2),
                "staged 0.2 (build 2) — applies on next launch",
                "NeutralizeDespiteReady",
                "ReadyPreservesPersistedOutcome",
            ),
            // A same-build honest terminal outcome stays useful.
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
        // The model lattice drives one staged phrasing; these are the other
        // sentences the lanes persist that claim a stage.
        let cases = [
            // An overlapping OLD process reading the NEW activation ledger.
            (
                53,
                54,
                None,
                "installed 0.54 (build 54); activating now",
                false,
            ),
            // The wording the stage/backoff lanes write now. It makes the same claim
            // as the older "applies on next launch" phrasing, and the claim, not the
            // phrasing, is what must be suppressed once the marker is gone.
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

    /// One row per pairing, with the verdict and the reason. The binary stamps a 12-hex
    /// `GIT_COMMIT` while the appcast carries all 40, so an abbreviation must match its
    /// full hash (in either order), and every shape that is not provably the same commit
    /// must not.
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
            // A dirty build is not reproducibly its base commit: conservative non-match.
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

    /// A second persistent class must still be able to speak: a machine that could
    /// not download (`pipeline`) and later also could not apply is told about both.
    ///
    /// Mutation: make the predicate `announced.is_none()` and the different-class
    /// assertion fails.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_second_persistent_class_still_speaks_but_the_same_one_stays_quiet() {
        // Nothing escalating: nothing to say, whatever was announced before.
        assert!(!persistent_notice_is_owed(None, None));
        assert!(!persistent_notice_is_owed(Some("pipeline"), None));
        // The first escalation always speaks.
        assert!(persistent_notice_is_owed(None, Some("pipeline")));
        // The SAME class on the next tick is the "once per streak" promise: the loud
        // notice must not become a nag.
        assert!(!persistent_notice_is_owed(
            Some("pipeline"),
            Some("pipeline")
        ));
        // A different class is news.
        assert!(persistent_notice_is_owed(Some("pipeline"), Some("apply")));
        // …and having spoken about it, it goes quiet too.
        assert!(!persistent_notice_is_owed(Some("apply"), Some("apply")));
        // A class escalating again after another was announced is a new episode.
        assert!(persistent_notice_is_owed(Some("apply"), Some("pipeline")));
        // Two spellings of ONE notice ("Couldn't download updates") are one class
        // to the person.
        assert!(!persistent_notice_is_owed(Some("pipeline"), Some("stage")));
        assert!(!persistent_notice_is_owed(Some("stage"), Some("pipeline")));
    }
}

#[cfg(test)]
mod team_pin_tests {
    /// The runtime opt-in is one-way and a compiled pin is absolute. This tree pins
    /// a real Team ID, so the pin decides and every runtime call (blank, different,
    /// anything) is a no-op: a settings file must not bypass verification or swap
    /// the team a build was armed with.
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

        // Every runtime call is inert against a compiled pin.
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

/// A policy this process could not read just then refuses the pre-verification as
/// a passing condition.
#[cfg(all(test, target_os = "macos"))]
mod handoff_candidate_facts_tests {
    use super::*;
    use aterm_update_core::handoff_policy::{PolicyFile, PolicyRead};

    /// An unread policy is never passed on as "asks nothing": a transient `EMFILE` on
    /// the policy's `open(2)` after the candidate's codesign passed is a passing
    /// refusal, never cached by the arm-time verifier, filed transient by the handoff
    /// worker, and read again at the next attempt.
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
        // Negative controls: every read that reached the file passes on as before,
        // a malformed file included (a verdict the producer ignores).
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

    /// AN `Info.plist` THE SYSTEM REFUSED FOR NOW IS NO VERDICT EITHER. The
    /// chunked-grant declaration is read off the same verified bundle at the same
    /// instant as the policy and cached beside the pass, for which `park_policy` keeps
    /// no freshness bound: read as "not declared", one `EMFILE` on a Dock-launched
    /// window near its descriptor limit sent a desk of more than 62 sessions to the
    /// fork lane — or launched a successor with the one-message grant, which the park
    /// then stood down on — for the cache's life, for a candidate that declares the
    /// capability. RED before the fix:
    /// `Ok(HandoffCandidateFacts { grant_chunks: false, .. })`.
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
        // Negative controls: an error the same file would give again stays a
        // verdict (not declared), and a read that reached the bytes says what
        // they declare.
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

    /// The same, read off a real bundle: a staged `.app` whose `Info.plist`
    /// declares the grant passes on `true`, and one with no `Info.plist` at all
    /// (the absent file is a verdict) passes on `false`.
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
