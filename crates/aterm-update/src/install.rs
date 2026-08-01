// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Staging (mount → extract → verify → publish) and application (lock →
//! re-verify → atomic swap → re-exec) of an update. The ordering here is the
//! security-critical part; see the per-step comments and the crate-level trust
//! model.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_update_core::{FileLock, Sentinel, ensure_private_dir, same_volume};

use crate::manifest::{Manifest, Ready};
use crate::paths::Staging;
use crate::sys::rename_swap;
use crate::{ApplyOutcome, bundle, verify};

/// A freshly-swapped build is auto-reverted after this many consecutive launches
/// that observe the boot sentinel still unconfirmed (a crash loop). `arm` records
/// 0; the first launch observes 1; the revert fires when attempts reach this. A
/// healthy build clears the sentinel via [`confirm_boot_health`] on its first boot,
/// so this only bites a build that never reaches the health checkpoint.
const MAX_BOOT_ATTEMPTS: u32 = 3;

const EXPECTED_BUILD_ENV: &str = "ATERM_UPDATE_EXPECTED_BUILD";
const EXPECTED_COMMIT_ENV: &str = "ATERM_UPDATE_EXPECTED_COMMIT";
const EXPECTED_DIGEST_ENV: &str = "ATERM_UPDATE_EXPECTED_DMG_SHA256";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpectedArtifact {
    build: u64,
    commit: String,
    dmg_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReexecAuthority {
    Absent,
    Matched,
    Invalid,
}

/// Startup authority is interpreted only after the boot-health lane has had a
/// chance to observe/revert an armed trial. This pure reducer is shared by the
/// shipping startup path and its Tier-1 model binding; `ObserveBootHealth` is a
/// fail-closed verdict, never permission to return early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupAuthorityDecision {
    ObserveBootHealth,
    Continue,
    ReturnMatchedReexec,
    ReturnMalformedExpected,
}

#[must_use]
fn startup_authority_decision(
    boot_health_observed: bool,
    reexec: ReexecAuthority,
    expected_artifact_valid: bool,
) -> StartupAuthorityDecision {
    if !boot_health_observed {
        StartupAuthorityDecision::ObserveBootHealth
    } else if reexec == ReexecAuthority::Matched {
        StartupAuthorityDecision::ReturnMatchedReexec
    } else if !expected_artifact_valid {
        StartupAuthorityDecision::ReturnMalformedExpected
    } else {
        StartupAuthorityDecision::Continue
    }
}

fn take_reexec_nonce() -> Option<std::ffi::OsString> {
    // Apply runs before any thread spawn. Clear even malformed authority before
    // health verification helpers can launch codesign/spctl children. Read and
    // clear are ONE critical section (`aterm_log::env::take`) so a one-shot
    // re-exec authority can never be observed twice.
    aterm_log::env::take("ATERM_UPDATE_REEXEC")
}

fn classify_reexec_authority(
    staging: Option<&Staging>,
    raw_nonce: Option<std::ffi::OsString>,
) -> ReexecAuthority {
    let Some(raw_nonce) = raw_nonce else {
        return ReexecAuthority::Absent;
    };
    let Some(staging) = staging else {
        return ReexecAuthority::Invalid;
    };
    let nonce = raw_nonce.to_string_lossy();
    let stamp = staging.reexec_stamp();
    let matched = crate::read_ledger_text(&stamp)
        .is_some_and(|text| !nonce.is_empty() && text.trim() == nonce);
    let _ = std::fs::remove_file(stamp);
    if matched {
        ReexecAuthority::Matched
    } else {
        ReexecAuthority::Invalid
    }
}

fn take_expected_artifact() -> Result<Option<ExpectedArtifact>, String> {
    // Apply runs at the top of main before any thread is spawned. Each key is read
    // and cleared in ONE critical section (`aterm_log::env::take`), so handoff
    // authority is consumed exactly once and verification helpers and user shells
    // never inherit it.
    let raw_build = aterm_log::env::take(EXPECTED_BUILD_ENV);
    let raw_commit = aterm_log::env::take(EXPECTED_COMMIT_ENV);
    let raw_digest = aterm_log::env::take(EXPECTED_DIGEST_ENV);
    match (raw_build, raw_commit, raw_digest) {
        (None, None, None) => Ok(None),
        (Some(build), Some(commit), Some(digest)) => {
            let build = build
                .to_str()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|build| *build != 0)
                .ok_or_else(|| "expected update build is malformed".to_string())?;
            let commit = commit
                .to_str()
                .and_then(|value| canonical_release_commit(Some(value)))
                .ok_or_else(|| "expected update commit is malformed".to_string())?;
            let dmg_sha256 = digest
                .to_str()
                .and_then(canonical_digest)
                .ok_or_else(|| "expected update digest is malformed".to_string())?;
            Ok(Some(ExpectedArtifact {
                build,
                commit,
                dmg_sha256,
            }))
        }
        _ => Err("expected update authority is incomplete".to_string()),
    }
}

fn ready_matches_expected(ready: &Ready, expected: &ExpectedArtifact) -> bool {
    ready.build_number == expected.build
        && canonical_release_commit(ready.commit.as_deref()).as_deref()
            == Some(expected.commit.as_str())
        && canonical_digest(&ready.dmg_sha256).as_deref() == Some(expected.dmg_sha256.as_str())
}

fn set_fd_cloexec(fd: i32, cloexec: bool) -> bool {
    // SAFETY: F_GETFD/F_SETFD touch only the descriptor flag of the supplied fd.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    flags >= 0
        && unsafe {
            libc::fcntl(
                fd,
                libc::F_SETFD,
                if cloexec {
                    flags | libc::FD_CLOEXEC
                } else {
                    flags & !libc::FD_CLOEXEC
                },
            )
        } >= 0
}

fn rearm_or_close_handoff_fd(fd: i32) {
    if set_fd_cloexec(fd, true) {
        return;
    }
    // A live descriptor that cannot be made close-on-exec must not survive into
    // rollback verification helpers. This is the child's duplicate authority;
    // closing it is fail-closed and leaves an overlap parent's original intact.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0 {
        // SAFETY: this branch owns the failed final-exec handoff duplicate.
        unsafe { libc::close(fd) };
    }
}

/// Clear CLOEXEC only for the final same-process image replacement. All update
/// verification helpers ran while these descriptors were closed-on-exec. If
/// exec returns, re-arm every surviving fd before any rollback helper can spawn.
fn exec_preserving_handoff_fds(command: &mut Command, handoff_fds: &[i32]) -> std::io::Error {
    let mut exact = handoff_fds.to_vec();
    exact.sort_unstable();
    if exact.iter().any(|fd| *fd < 3) || exact.windows(2).any(|pair| pair[0] == pair[1]) {
        return std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid or duplicate handoff descriptor authority",
        );
    }
    let mut cleared = Vec::new();
    for fd in exact {
        if !set_fd_cloexec(fd, false) {
            for prior in cleared {
                rearm_or_close_handoff_fd(prior);
            }
            return std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "handoff descriptor set changed before final exec",
            );
        }
        cleared.push(fd);
    }
    let error = command.exec();
    for fd in cleared {
        rearm_or_close_handoff_fd(fd);
    }
    error
}

/// The post-swap re-exec command: the NEW binary at the canonical path, the
/// forwarded argv, the single-use re-exec nonce — and the caller's handoff
/// authority variables restored onto the exec image ONLY. The GUI's prearm
/// consumed those variables out of the ambient environment (so no
/// codesign/PlistBuddy/spctl helper this process spawned could observe them),
/// but the successor image re-runs prearm and must re-validate the inherited
/// handoff: without the restored pairs it classifies the handoff malformed and
/// exits before writing the readiness proof, the parked parent reads EOF
/// (`ChildDied`), and the seamless lane can never succeed.
fn boot_reexec_command(
    new_exe: &std::path::Path,
    reexec_value: &str,
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Command {
    let mut reexec = Command::new(new_exe);
    reexec
        .arg("--window")
        // The swap re-exec targets the canonical Mach-O, losing any argv0
        // alias identity; this code only runs from the WINDOW entry, so pin
        // the mode for the one-binary router BEFORE the forwarded args — the
        // router's scan stops at the first -e/--command/--, so an appended
        // flag would be invisible (or pollute the -e payload). An env-launched
        // headless instance still carries ATERM_HEADLESS here (boot-time
        // apply runs before the entry consumes it).
        .args(std::env::args_os().skip(1))
        .env("ATERM_UPDATE_REEXEC", reexec_value);
    for (key, value) in handoff_env {
        reexec.env(key, value);
    }
    reexec
}

/// The boot-health sentinel for this install (a small file under the private
/// staging root, so the pre-swap old process and the post-swap new process resolve
/// the identical path).
fn boot_sentinel(staging: &Staging) -> Sentinel {
    Sentinel::new(staging.root.join("boot.sentinel"))
}

fn prepare_trial(staging: &Staging, ready: &Ready) -> Result<Sentinel, String> {
    let sentinel = boot_sentinel(staging);
    if let Err(error) = crate::manifest::FailedMark::record_required(
        &staging.trial(),
        ready.build_number,
        &ready.dmg_sha256,
    ) {
        return Err(format!("persist trial identity: {error}"));
    }
    // Sentinel is the transaction commit marker and is published LAST. A crash
    // after trial.toml but before arm leaves no active authority and is harmless;
    // the reverse order could wedge forever with an armed build but no exact digest.
    if let Err(error) = sentinel.arm(ready.build_number) {
        crate::manifest::FailedMark::clear(&staging.trial());
        return Err(format!("arm boot-health trial: {error}"));
    }
    Ok(sentinel)
}

#[must_use]
fn abandoned_preswap_trial(process_build: u64, installed_build: u64, armed_build: u64) -> bool {
    installed_build == process_build && armed_build > process_build
}

fn identity_matches_running(
    sealed_build: u64,
    sealed_commit: &str,
    running_build: u64,
    running_commit: Option<&str>,
) -> bool {
    sealed_build == running_build
        && running_commit.is_none_or(|commit| crate::commit_matches(commit, sealed_commit))
}

/// Recover the sole safe mismatched-sentinel crash cut under apply_lock: this
/// process's OLD build is still canonically installed while the armed build is
/// newer. Because apply_lock is held, a live concurrent swap must finish before
/// this observation; canonical OLD proves NEW is not installed. This covers both a
/// pre-swap crash and a crash immediately after inverse rollback.
fn recover_abandoned_preswap_trial_if_exact(
    staging: &Staging,
    installed: &Path,
    process_build: u64,
    process_commit: Option<&str>,
) -> bool {
    let sentinel = boot_sentinel(staging);
    let Some((armed_build, _)) = sentinel.read_state() else {
        return true;
    };
    let Ok((installed_build, installed_commit)) = verified_bundle_identity(installed) else {
        return false;
    };
    if !abandoned_preswap_trial(process_build, installed_build, armed_build) {
        return false;
    }
    if !identity_matches_running(
        installed_build,
        &installed_commit,
        process_build,
        process_commit,
    ) {
        return false;
    }

    let fixed = rollback_path(installed);
    let Ok((candidate_build, candidate_commit)) = verified_bundle_identity(&fixed) else {
        return false;
    };
    if !same_volume(&fixed, installed)
        || candidate_build != armed_build
        || !trial_authorizes_candidate(staging, candidate_build, &candidate_commit)
    {
        return false;
    }

    if sentinel.confirm().is_err() {
        return false;
    }

    // A crash after the O(1) staged_app→fixed rename (or after arming but before
    // swap) leaves exact NEW at fixed and exact ready authority. Restore it to the
    // published staging path so the update remains retryable. An inverse-swap cut
    // after ready retirement instead has no publisher, so fixed is failed NEW and
    // may be reclaimed only after the successful disarm above.
    let ready = Ready::read(&staging.ready);
    if ready.as_ref().is_some_and(|ready| {
        ready_matches_verified_identity(ready, candidate_build, &candidate_commit)
    }) {
        let _ = remove_path_no_follow(&staging.staged_app);
        if std::fs::rename(&fixed, &staging.staged_app).is_err() {
            return false;
        }
    } else {
        let _ = remove_path_no_follow(&fixed);
    }
    crate::manifest::FailedMark::clear(&staging.trial());
    let receipt_matches = crate::manifest::InstalledReceipt::read(&staging.installed_receipt())
        .is_some_and(|receipt| receipt.matches_sealed(installed_build, &installed_commit));
    if !receipt_matches {
        crate::manifest::InstalledReceipt::clear(&staging.installed_receipt());
    }
    true
}

/// The STABLE path where the swapped-out OLD bundle is retained as the rollback
/// source: a fixed-named sibling of the install (same volume as `installed`, so the
/// revert swap-back is atomic), findable by the re-exec'd new process and by
/// [`confirm_boot_health`] with no per-process pid in the name.
fn rollback_path(installed: &Path) -> PathBuf {
    installed.with_file_name("aterm.app.rollback")
}

fn canonical_digest(digest: &str) -> Option<String> {
    let digest = digest.trim();
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| digest.to_ascii_lowercase())
}

fn ready_matches_verified_identity(ready: &Ready, build: u64, sealed_commit: &str) -> bool {
    ready.build_number == build
        && canonical_digest(&ready.dmg_sha256).is_some()
        && canonical_release_commit(ready.commit.as_deref())
            .is_some_and(|commit| crate::commit_matches(&commit, sealed_commit))
}

fn trial_authorizes_candidate(staging: &Staging, build: u64, sealed_commit: &str) -> bool {
    let Some(trial) = crate::manifest::FailedMark::read(&staging.trial()) else {
        return false;
    };
    let Some(trial_digest) = canonical_digest(&trial.sha256) else {
        return false;
    };
    if trial.build_number != build {
        return false;
    }
    if Ready::read(&staging.ready).is_some_and(|ready| {
        ready_matches_verified_identity(&ready, build, sealed_commit)
            && canonical_digest(&ready.dmg_sha256).as_deref() == Some(trial_digest.as_str())
    }) {
        return true;
    }
    crate::manifest::InstalledReceipt::read(&staging.installed_receipt()).is_some_and(|receipt| {
        receipt.matches_sealed(build, sealed_commit)
            && receipt.dmg_sha256.eq_ignore_ascii_case(&trial_digest)
    })
}

/// Recover the crash cut after same-volume staged_app→fixed but before the
/// sentinel commit marker. Exact ready+verified-bundle identity is sufficient to
/// put the bytes back; a mismatched/corrupt fixed path is preserved and fails
/// closed rather than being mistaken for this transaction.
fn recover_orphaned_prepared_candidate(
    staging: &Staging,
    installed: &Path,
    current_build: u64,
    ready: &Ready,
) -> Result<(), String> {
    if is_non_symlink_dir(&staging.staged_app) || boot_sentinel(staging).read_state().is_some() {
        return Ok(());
    }
    let fixed = rollback_path(installed);
    let (build, commit) = verified_bundle_identity(&fixed)
        .map_err(|error| format!("orphaned fixed candidate is invalid: {error}"))?;
    if !same_volume(&fixed, installed)
        || build <= current_build
        || !ready_matches_verified_identity(ready, build, &commit)
    {
        return Err("orphaned fixed candidate does not match ready authority".to_string());
    }
    std::fs::rename(&fixed, &staging.staged_app)
        .map_err(|error| format!("restore orphaned fixed candidate to staged app: {error}"))
}

/// Materialize and verify NEW at the fixed destination-volume rollback name before
/// the point of no return. The subsequent single RENAME_SWAP therefore places OLD
/// directly at the only path crash-loop recovery knows; there is no post-swap
/// retention rename or process-crash window.
#[derive(Debug)]
struct PreparedSwapCandidate {
    fixed: PathBuf,
    /// Same-volume staging uses an atomic rename instead of a recursive copy. If
    /// anything fails before RENAME_SWAP, the verified bundle must be moved back
    /// so `ready.toml` never advertises missing bytes.
    moved_from_stage: bool,
}

fn remove_path_no_follow(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            std::fs::remove_file(path)
        }
        Ok(_) => std::fs::remove_dir_all(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn is_non_symlink_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
}

fn recover_prepared_candidate(prepared: &PreparedSwapCandidate, staging: &Staging) {
    if prepared.moved_from_stage {
        let _ = remove_path_no_follow(&staging.staged_app);
        if let Err(error) = std::fs::rename(&prepared.fixed, &staging.staged_app) {
            // A marker without its exact bytes must never survive a failed
            // pre-swap transaction.
            let _ = remove_path_no_follow(&prepared.fixed);
            staging.retire_published();
            crate::warn(&format!(
                "could not restore same-volume swap candidate after pre-swap failure: {error}"
            ));
        }
    } else {
        let _ = remove_path_no_follow(&prepared.fixed);
    }
}

fn prepare_fixed_swap_candidate(
    staging: &Staging,
    installed: &Path,
    ready: &Ready,
    current_build: u64,
) -> Result<PreparedSwapCandidate, String> {
    let staged = &staging.staged_app;
    let fixed = rollback_path(installed);
    remove_path_no_follow(&fixed)
        .map_err(|error| format!("remove stale fixed rollback: {error}"))?;

    // The normal macOS layout places ~/Library and /Applications on the same
    // APFS volume. Moving the already-verified bundle is O(1) metadata work and
    // avoids a seconds-long `ditto` copy on the launch/quit path. External-volume
    // installs retain the conservative copy path.
    let moved_from_stage = same_volume(staged, installed);
    if moved_from_stage {
        std::fs::rename(staged, &fixed)
            .map_err(|error| format!("move verified stage to fixed swap path: {error}"))?;
    } else {
        let status = Command::new("/usr/bin/ditto")
            .arg(staged)
            .arg(&fixed)
            .status()
            .map_err(|error| format!("spawn ditto to fixed swap path: {error}"))?;
        if !status.success() {
            let _ = remove_path_no_follow(&fixed);
            return Err(format!("ditto to fixed swap path failed ({status})"));
        }
    }
    let prepared = PreparedSwapCandidate {
        fixed: fixed.clone(),
        moved_from_stage,
    };
    if !same_volume(&fixed, installed) {
        recover_prepared_candidate(&prepared, staging);
        return Err("fixed swap candidate is not on the installed volume".to_string());
    }
    let (fixed_build, sealed_commit) = match verified_bundle_identity(&fixed) {
        Ok(identity) => identity,
        Err(error) => {
            recover_prepared_candidate(&prepared, staging);
            return Err(format!("fixed swap candidate failed verification: {error}"));
        }
    };
    if fixed_build != ready.build_number || fixed_build <= current_build {
        recover_prepared_candidate(&prepared, staging);
        return Err(format!(
            "fixed swap candidate sealed build {fixed_build} != marker {} or not newer than {current_build}",
            ready.build_number
        ));
    }
    if !sealed_commit_matches(ready.commit.as_deref(), &sealed_commit) {
        recover_prepared_candidate(&prepared, staging);
        return Err("fixed swap candidate commit rebind mismatch".to_string());
    }
    Ok(prepared)
}

fn swap_fixed_candidate(
    prepared: &PreparedSwapCandidate,
    installed: &Path,
) -> Result<PathBuf, String> {
    // Revalidate the no-follow shape at the immediate point of no return. Full
    // signature/build/commit verification occurred after materialization; the
    // private parent + apply lock exclude compliant replacement, while this guard
    // ensures a symlink can never be exchanged into the canonical install.
    checked_bundle_exchange(&prepared.fixed, installed, "atomic fixed-path swap")?;
    Ok(prepared.fixed.clone())
}

fn checked_bundle_exchange(a: &Path, b: &Path, operation: &str) -> Result<(), String> {
    if !is_non_symlink_dir(a) || !is_non_symlink_dir(b) {
        return Err(format!(
            "{operation}: bundle changed into a symlink/non-directory"
        ));
    }
    if !same_volume(a, b) {
        return Err(format!("{operation}: bundles are not on one volume"));
    }
    rename_swap(a, b).map_err(|error| format!("{operation} failed: {error}"))
}

/// Read a bundle's SEALED identity (build + commit), refusing anything that is
/// not a plain directory carrying an intact, policy-satisfying signature.
fn verified_bundle_identity(app: &Path) -> Result<(u64, String), String> {
    let metadata =
        std::fs::symlink_metadata(app).map_err(|error| format!("bundle metadata: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("bundle path is not a non-symlink directory".to_string());
    }
    verify::verify_bundle_policy(app, crate::effective_team_id())
        .map_err(|error| format!("bundle policy: {error}"))?;
    let build =
        verify::bundle_build_number(app).map_err(|error| format!("sealed build: {error}"))?;
    let commit =
        verify::bundle_git_commit(app).map_err(|error| format!("sealed commit: {error}"))?;
    let clean_commit = commit.trim();
    if !(7..=40).contains(&clean_commit.len())
        || !clean_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("sealed commit is not clean hexadecimal provenance".to_string());
    }
    Ok((build, clean_commit.to_ascii_lowercase()))
}

fn validate_fixed_rollback(
    rollback: &Path,
    installed: &Path,
    current_build: u64,
) -> Result<(u64, String), String> {
    if !same_volume(rollback, installed) {
        return Err("rollback is not on the installed volume".to_string());
    }
    let (build, commit) = verified_bundle_identity(rollback)?;
    if build == 0 || build >= current_build {
        return Err(format!(
            "rollback sealed build {build} is not a strict predecessor of {current_build}"
        ));
    }
    Ok((build, commit))
}

#[derive(Debug)]
struct VerifiedRollback {
    path: PathBuf,
}

fn ensure_fixed_rollback(installed: &Path, current_build: u64) -> Result<VerifiedRollback, String> {
    let fixed = rollback_path(installed);
    match std::fs::symlink_metadata(&fixed) {
        Ok(_) => {
            // Validation is the whole point of this call — it refuses a rollback
            // whose sealed build is not a strict predecessor. Callers need only
            // the verified PATH; the build number it proved is not carried on.
            validate_fixed_rollback(&fixed, installed, current_build)
                .map_err(|error| format!("fixed rollback is invalid: {error}"))?;
            return Ok(VerifiedRollback { path: fixed });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect fixed rollback: {error}")),
    }
    // No fixed rollback, and nowhere else to look. The pre-fixed-path v0.53
    // updater could leave OLD at `staged_app` or at an `aterm.app.new-*` install
    // sibling, and this used to scan both and migrate the nearest verified
    // predecessor onto the fixed name. The modern swap is one atomic
    // `renamex_np(RENAME_SWAP)` that always leaves OLD at the fixed path, so
    // neither shape can occur, and the builds that produced them are in the
    // retired lineage and cannot reach this one.
    Err("fixed rollback missing".to_string())
}

fn recover_prepared_stage(rollback: &Path, staging: &Staging) {
    if rollback != staging.staged_app {
        let _ = std::fs::remove_dir_all(&staging.staged_app);
        if let Err(error) = std::fs::rename(rollback, &staging.staged_app) {
            let _ = std::fs::remove_dir_all(rollback);
            staging.retire_published();
            crate::warn(&format!(
                "could not restore verified stage after pre-exec rollback: {error}"
            ));
        }
    }
}

fn restore_installed_receipt(
    staging: &Staging,
    previous: Option<&crate::manifest::InstalledReceipt>,
) -> Result<(), String> {
    let path = staging.installed_receipt();
    let restored = if let Some(previous) = previous {
        previous.record_preserving_kind(&path)
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("clear superseded installed receipt: {error}")),
        }
    };
    if let Err(error) = restored {
        // OLD is already canonical again. A receipt for failed NEW must never
        // remain usable as OLD's install proof merely because the kind-preserving
        // rewrite failed. Best-effort removal makes the common failure fail-closed;
        // even if removal itself fails, every reader still binds the receipt to
        // OLD's sealed identity before granting authority.
        let clear = match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(clear_error) if clear_error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(clear_error) => Err(clear_error),
        };
        return match clear {
            Ok(()) => Err(format!(
                "restore previous installed receipt: {error}; superseded receipt cleared"
            )),
            Err(clear_error) => Err(format!(
                "restore previous installed receipt: {error}; superseded receipt could not be cleared: {clear_error}"
            )),
        };
    }
    Ok(())
}

fn previous_receipt_for_sealed_old(
    staging: &Staging,
    old_build: u64,
    old_commit: &str,
) -> Option<crate::manifest::InstalledReceipt> {
    crate::manifest::InstalledReceipt::read(&staging.installed_receipt())
        .filter(|receipt| receipt.matches_sealed(old_build, old_commit))
}

fn ensure_current_trial_receipt(
    staging: &Staging,
    installed: &Path,
    current_build: u64,
    current_commit: Option<&str>,
) -> Result<PathBuf, String> {
    let verified_rollback = ensure_fixed_rollback(installed, current_build)?;
    let (sealed_build, sealed_commit) = verified_bundle_identity(installed)
        .map_err(|error| format!("installed trial is not verified: {error}"))?;
    if sealed_build != current_build {
        return Err(format!(
            "canonical installed build {sealed_build} != running trial {current_build}"
        ));
    }
    if !identity_matches_running(sealed_build, &sealed_commit, current_build, current_commit) {
        return Err("canonical installed commit does not match running binary".to_string());
    }

    let trial = crate::manifest::FailedMark::read(&staging.trial())
        .ok_or_else(|| "installed trial has no exact trial identity".to_string())?;
    let trial_digest = canonical_digest(&trial.sha256)
        .filter(|_| trial.build_number == current_build)
        .ok_or_else(|| "installed trial identity is malformed or for another build".to_string())?;

    if crate::manifest::InstalledReceipt::read(&staging.installed_receipt()).is_some_and(
        |receipt| {
            receipt.matches_sealed(sealed_build, &sealed_commit)
                && receipt.dmg_sha256.eq_ignore_ascii_case(&trial_digest)
        },
    ) {
        return Ok(verified_rollback.path);
    }

    // Process-crash cut after fixed RENAME_SWAP but before receipt commit: ready
    // still carries the exact full commit+digest. Bind it to the sealed installed
    // identity and finish the receipt before health can GC rollback.
    if let Some(ready) = Ready::read(&staging.ready) {
        let commit = canonical_release_commit(ready.commit.as_deref())
            .filter(|_| ready.build_number == sealed_build)
            .filter(|commit| crate::commit_matches(commit, &sealed_commit))
            .ok_or_else(|| {
                "ready recovery record does not match sealed installed trial".to_string()
            })?;
        let ready_digest = canonical_digest(&ready.dmg_sha256)
            .filter(|digest| digest == &trial_digest)
            .ok_or_else(|| "ready recovery digest does not match active trial".to_string())?;
        crate::manifest::InstalledReceipt::record(
            &staging.installed_receipt(),
            sealed_build,
            &commit,
            &ready_digest,
        )?;
        return Ok(verified_rollback.path);
    }

    Err("installed trial has no exact receipt or authorized recovery record".to_string())
}

/// A mounted DMG that detaches itself on drop (best-effort). Mounted at a PRIVATE
/// mountpoint inside our `0700` staging dir (never `/Volumes`), so an abnormal exit
/// can't leak a browsable `/Volumes/aterm*` mount and repeated same-named volumes
/// can't collide (F19).
struct Mounted {
    mountpoint: PathBuf,
}

impl Mounted {
    /// `hdiutil attach -nobrowse -readonly -noautoopen -mountpoint <mp> <dmg>`, mounting
    /// at the caller-chosen private `mp` (created fresh) so we never touch `/Volumes`
    /// and never have to parse a mount table. `mp` must be under our own `0700` dir.
    fn attach(dmg: &Path, mountpoint: &Path) -> Result<Self, String> {
        // RETRY, THEN FALL BACK. A stage that dies here strands the whole fleet on
        // the previous build with a verified DMG already on disk, so this step
        // must not fail on the first refusal.
        //
        // 2026-07-31 (v0.10.0): every machine reported
        //   hdiutil attach failed: hdiutil: attach failed - Device not configured
        // (ENXIO) from inside the app, while the IDENTICAL command — same DMG,
        // same `-mountpoint` under the same 0700 dir, same environment copied
        // from the running process — succeeded every time from a shell. So the
        // refusal is about the attaching PROCESS's moment, not the image: a
        // DiskArbitration/device-attach race the app can lose and a shell does
        // not. Both mitigations below are cheap and no-ops on the happy path.
        let mut attempts: Vec<String> = Vec::new();
        for attempt in 0..3u32 {
            if attempt > 0 {
                // Linear backoff: the races this loses are short-lived.
                std::thread::sleep(std::time::Duration::from_millis(
                    500 * u64::from(attempt),
                ));
            }
            match Self::attach_at(dmg, Some(mountpoint)) {
                Ok(mounted) => return Ok(mounted),
                Err(error) => attempts.push(error),
            }
        }
        // LAST RESORT: let hdiutil pick the mount point itself. `-mountpoint` is
        // what keeps us out of `/Volumes` (and out of the user's Finder), so it
        // is preferred and tried first — but a mounted image we can read beats a
        // fleet that cannot update, and the `Drop` below detaches either shape.
        match Self::attach_at(dmg, None) {
            Ok(mounted) => Ok(mounted),
            Err(error) => {
                attempts.push(error);
                let _ = std::fs::remove_dir_all(mountpoint);
                let size = std::fs::metadata(dmg).map_or_else(
                    |_| "unreadable".to_string(),
                    |meta| format!("{} bytes", meta.len()),
                );
                Err(format!(
                    "hdiutil attach failed after {} attempts ({}, {size}): {}",
                    attempts.len(),
                    dmg.display(),
                    attempts.join(" | ")
                ))
            }
        }
    }

    /// One `hdiutil attach`. `mountpoint` = `Some(dir)` mounts there (the private
    /// 0700 path); `None` lets hdiutil choose, and the chosen path is read back
    /// from its output so `Drop` can detach exactly what was attached.
    fn attach_at(dmg: &Path, mountpoint: Option<&Path>) -> Result<Self, String> {
        let mut cmd = Command::new("/usr/bin/hdiutil");
        cmd.args(["attach", "-nobrowse", "-readonly", "-noautoopen"]);
        if let Some(mountpoint) = mountpoint {
            let _ = std::fs::remove_dir_all(mountpoint);
            std::fs::create_dir_all(mountpoint).map_err(|e| format!("create mountpoint: {e}"))?;
            cmd.arg("-mountpoint").arg(mountpoint);
        }
        let out = cmd
            .arg(dmg)
            .output()
            .map_err(|e| format!("spawn hdiutil attach: {e}"))?;
        if !out.status.success() {
            if let Some(mountpoint) = mountpoint {
                let _ = std::fs::remove_dir_all(mountpoint);
            }
            let stderr = String::from_utf8_lossy(&out.stderr);
            let detail = stderr.trim();
            let detail = if detail.is_empty() {
                "no stderr"
            } else {
                detail
            };
            return Err(match mountpoint {
                Some(_) => format!("private mountpoint: {detail}"),
                None => format!("default mountpoint: {detail}"),
            });
        }
        match mountpoint {
            Some(mountpoint) => Ok(Self {
                mountpoint: mountpoint.to_path_buf(),
            }),
            // hdiutil's plain output is TAB-separated `dev \t type \t mountpoint`;
            // splitting on tabs (not whitespace) keeps a mount path with spaces —
            // which the release DMG's `aterm X.Y.Z` volume name always has — intact.
            None => String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| line.split('\t').nth(2))
                .map(str::trim)
                .find(|candidate| !candidate.is_empty())
                .map(|found| Self {
                    mountpoint: PathBuf::from(found),
                })
                .ok_or_else(|| {
                    "default mountpoint: hdiutil attached but named no mount point".to_string()
                }),
        }
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = Command::new("/usr/bin/hdiutil")
            .args(["detach", "-force"])
            .arg(&self.mountpoint)
            .output();
        // The private mountpoint is our own empty dir once detached; reclaim it.
        let _ = std::fs::remove_dir_all(&self.mountpoint);
    }
}

/// Best-effort reconciliation of leftover private mountpoints (`mnt-*`) from a prior
/// run that was killed mid-stage (its `Mounted::drop` never ran), so stale mounts
/// don't accumulate. Force-detach then remove each. (F19)
fn sweep_stale_mounts(staging: &Staging) {
    let Ok(entries) = std::fs::read_dir(&staging.root) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with("mnt-") {
            let p = e.path();
            let _ = Command::new("/usr/bin/hdiutil")
                .args(["detach", "-force"])
                .arg(&p)
                .output();
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

fn canonical_release_commit(commit: Option<&str>) -> Option<String> {
    let commit = commit?.trim();
    (commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| commit.to_ascii_lowercase())
}

fn sealed_commit_matches(expected: Option<&str>, sealed: &str) -> bool {
    canonical_release_commit(expected)
        .is_some_and(|expected| crate::commit_matches(&expected, sealed))
}

/// PRE-PARK handoff verification (seamless-update overlap seam). Run the full
/// staged-bundle authenticity gate — the tiered codesign policy check plus the
/// sealed CFBundleVersion/commit rebinding that the swap path re-runs at apply
/// time — while every parent PTY reader is still live and consuming.
///
/// WHY THIS EXISTS: without it, the first authenticity verdict on the staged
/// candidate happens inside the handoff child's boot (`apply_staged_if_ready`),
/// i.e. INSIDE the activity-sensitive parked window. A candidate that was going
/// to fail codesign therefore parked the terminal for nothing. Hoisting the
/// verdict here means a bad candidate is refused before a single reader stops.
///
/// This is strictly ADDITIVE authority: the child still re-verifies everything
/// at swap time under `apply_lock` (the TOCTOU defence is unchanged; the disk
/// can mutate between this call and the child's own gate). A `Ok(())` here is
/// a latency optimization plus a warm codesign page cache, never a grant.
///
/// `expected_build`/`expected_commit` bind the check to the exact artifact the
/// updater reducer authorized, mirroring `ready_matches_expected`: a stage that
/// changed identity since authorization fails now instead of after parking.
pub fn preverify_staged_handoff_candidate(
    current_build: u64,
    expected_build: Option<u64>,
    expected_commit: Option<&str>,
) -> Result<(), String> {
    let Some(staging) = Staging::resolve() else {
        return Err("no private staging root is available".to_string());
    };
    preverify_staged_handoff_candidate_at(&staging, current_build, expected_build, expected_commit)
}

/// Injectable core of [`preverify_staged_handoff_candidate`]; the split exists
/// so the refusal ladder is provable against a temp staging root without a
/// signed fixture bundle (a missing/unsigned candidate must refuse BEFORE any
/// caller could park a reader on its behalf).
fn preverify_staged_handoff_candidate_at(
    staging: &Staging,
    current_build: u64,
    expected_build: Option<u64>,
    expected_commit: Option<&str>,
) -> Result<(), String> {
    // Serialize against a concurrent publication/apply exactly like the swap
    // path: verifying a half-published candidate proves nothing. Held only for
    // the verification itself; readers are all still live during this wait.
    let _lock = FileLock::acquire(&staging.apply_lock)
        .map_err(|error| format!("pre-verify lock: {error}"))?;
    let ready = match read_ready(staging, current_build) {
        ReadyState::Newer(ready) => ready,
        ReadyState::NotNewer => {
            return Err("staged build is not strictly newer than the running build".to_string());
        }
        ReadyState::Corrupt => return Err("staged ready marker is unreadable".to_string()),
        ReadyState::Absent => return Err("no verified update is staged".to_string()),
    };
    if let Some(expected) = expected_build
        && ready.build_number != expected
    {
        return Err(format!(
            "staged build {} is not the authorized build {expected}",
            ready.build_number
        ));
    }
    let (staged_build, sealed_commit) = verified_bundle_identity(&staging.staged_app)?;
    // The same sealed-identity rebinds the apply path enforces (F10): the number
    // and provenance SEALED into the bundle must equal the unauthenticated
    // marker and still exceed the running build.
    if staged_build != ready.build_number || staged_build <= current_build {
        return Err(format!(
            "sealed build {staged_build} does not rebind marker build {} over running {current_build}",
            ready.build_number
        ));
    }
    if !sealed_commit_matches(ready.commit.as_deref(), &sealed_commit) {
        return Err("staged bundle commit does not match the ready marker".to_string());
    }
    if let Some(expected) = expected_commit
        && !crate::commit_matches(expected, &sealed_commit)
    {
        return Err("staged bundle commit does not match the authorized artifact".to_string());
    }
    Ok(())
}

/// Publish one already-verified incoming bundle as a short transaction shared
/// with the apply path. The long download/extract/verification work remains under
/// `stage_lock` only; compliant callers therefore acquire locks in the sole nested
/// order `stage_lock -> apply_lock`. Apply never acquires `stage_lock`, so startup
/// cannot wait on a download and the two lanes cannot deadlock.
fn publish_verified_stage(staging: &Staging, incoming: &Path, ready: &Ready) -> Result<(), String> {
    let marker = ready.to_toml()?;
    let _publish_lock =
        FileLock::acquire(&staging.apply_lock).map_err(|error| format!("publish lock: {error}"))?;

    // Invalidate the old generation first. Lock-free status readers may briefly
    // observe "absent", but never an old marker paired with the new bundle.
    let _ = std::fs::remove_file(&staging.ready);
    let _ = std::fs::remove_dir_all(&staging.staged_app);
    std::fs::rename(incoming, &staging.staged_app)
        .map_err(|error| format!("publish staged bundle: {error}"))?;

    // The marker remains the commit point and is written last.
    let tmp = staging.root.join("ready.toml.tmp");
    std::fs::write(&tmp, marker).map_err(|error| format!("write ready marker: {error}"))?;
    std::fs::rename(&tmp, &staging.ready).map_err(|error| format!("commit ready marker: {error}"))
}

/// Stage a verified copy of the bundle from a downloaded (sha256-checked) DMG:
/// mount, `ditto`-extract the `.app`, verify it (codesign/Team-ID/spctl), then
/// publish `staged/aterm.app` + write `ready.toml` LAST. The ready marker's
/// presence is the sole "ready" signal, so writing it last (atomic rename) means
/// a reader never sees a half-staged bundle.
pub fn stage_from_dmg(
    staging: &Staging,
    dmg: &Path,
    manifest: &Manifest,
    expected_team: &str,
) -> Result<(), String> {
    let manifest_commit =
        canonical_release_commit(manifest.commit.as_deref()).ok_or_else(|| {
            "release manifest lacks a clean, valid git commit; refusing to stage".to_string()
        })?;
    ensure_private_dir(&staging.staged_dir()).map_err(|e| format!("staged dir: {e}"))?;
    // Clean up any mount a previously-killed run leaked, then mount at a fresh private
    // mountpoint under our 0700 dir (never /Volumes).
    sweep_stale_mounts(staging);
    let mountpoint = staging.root.join(format!("mnt-{}", std::process::id()));
    let mounted = Mounted::attach(dmg, &mountpoint)?;
    let src = mounted.mountpoint.join("aterm.app");
    if !src.is_dir() {
        return Err(format!("{} not found on mounted DMG", src.display()));
    }

    let incoming = staging.staged_dir().join("aterm.app.incoming");
    let _ = std::fs::remove_dir_all(&incoming);
    // `ditto` (not `cp -R`) preserves extended attributes + the _CodeSignature
    // layout, so the copied bundle's signature stays valid.
    let status = Command::new("/usr/bin/ditto")
        .arg(&src)
        .arg(&incoming)
        .status()
        .map_err(|e| format!("spawn ditto: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(format!("ditto extract failed ({status})"));
    }
    // detach the DMG now; everything we need is in `incoming`.
    drop(mounted);

    // Verify the extracted bundle before publishing it (tiered: full Developer-ID
    // check when a Team ID is pinned, else structural-only — see the crate trust model).
    if let Err(e) = verify::verify_bundle_policy(&incoming, expected_team) {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(format!("staged bundle failed verification: {e}"));
    }
    // Bind the (unauthenticated) manifest build_number to the number actually inside
    // the signed bundle — otherwise a manifest could claim a high build_number while
    // pointing at an OLD genuine signed DMG (a downgrade/replay via repo-write). The
    // CFBundleVersion is codesign-sealed, so reading it AFTER verify_bundle is sound.
    let bundle_build = match verify::bundle_build_number(&incoming) {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&incoming);
            return Err(format!("staged bundle build number unreadable: {e}"));
        }
    };
    if bundle_build != manifest.build_number {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(format!(
            "staged bundle CFBundleVersion {bundle_build} != manifest build_number {} — \
             refusing a manifest/bundle mismatch",
            manifest.build_number
        ));
    }
    let sealed_commit = match verify::bundle_git_commit(&incoming) {
        Ok(commit) => commit,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&incoming);
            return Err(format!("staged bundle commit unreadable: {error}"));
        }
    };
    if !sealed_commit_matches(Some(&manifest_commit), &sealed_commit) {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(format!(
            "staged bundle ATermGitCommit {sealed_commit:?} does not match manifest commit"
        ));
    }
    let team = verify::team_id(&incoming).unwrap_or_else(|_| expected_team.to_string());

    let ready = Ready {
        build_number: manifest.build_number,
        version: manifest.version.clone(),
        // Store canonical lowercase hex (like `dmg_sha256`) so `commit_matches` and any
        // display get a clean value; `None` stays `None`.
        commit: Some(manifest_commit),
        dmg_sha256: manifest.sha256.to_ascii_lowercase(),
        team_id: team,
        staged_at: now_rfc3339(),
        changelog: manifest.changelog.clone(),
    };
    publish_verified_stage(staging, &incoming, &ready)
}

/// Publish the post-swap truth after `ready.toml` has been retired but before
/// exec. The NEW build number is intentional: an overlapping OLD reader sees a
/// ledger mismatch and reconciles neutrally, while the re-exec'd NEW process
/// sees an exact installed/activating outcome instead of the historical stale
/// "staged … applies on next launch" claim.
fn record_activating_status(staging: &Staging, ready: &Ready) {
    crate::status::record(
        staging,
        ready.build_number,
        &format!(
            "installed {} (build {}); activating now",
            ready.version, ready.build_number
        ),
    );
}

/// Apply a staged update if it is ready and strictly newer. On success this
/// re-execs and never returns. See module + crate docs for the full contract.
pub fn apply_staged_if_ready(
    current_build: u64,
    current_commit: Option<&str>,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> ApplyOutcome {
    let reexec_nonce = take_reexec_nonce();
    // Consume/clear child authority while startup is single-threaded, but do not
    // let malformed environment suppress crash-loop health observation/revert.
    // Its verdict is applied only after the mandatory boot-health lane below.
    let expected_artifact = take_expected_artifact();
    // Resolve the private staging root ONCE per launch: boot-health, the re-exec
    // guard, and the swap path below all share it. Each resolve chmods the 0700
    // dir synchronously (and this runs before the first frame), so a single
    // resolve keeps that off the launch critical path.
    let staging = Staging::resolve();
    let reexec_authority = classify_reexec_authority(staging.as_ref(), reexec_nonce);

    // 1. Boot-health check FIRST, on EVERY launch (re-exec OR a manual relaunch):
    //    if a sentinel is armed for THIS build, count the launch and, if the build
    //    is crash-looping (never reached the health checkpoint across
    //    MAX_BOOT_ATTEMPTS launches), revert to the retained OLD bundle and re-exec
    //    it. This must run regardless of the re-exec env, because a user manually
    //    relaunching a crashing build still has to accrue attempts toward the revert
    //    (the re-exec env is only set on the FIRST post-swap launch).
    if let Some(s) = &staging
        && let Some(outcome) = check_boot_health(s, current_build, current_commit, handoff_fds)
    {
        return outcome;
    }
    if reexec_authority == ReexecAuthority::Invalid {
        crate::warn(
            "ignoring a spoofed/stale ATERM_UPDATE_REEXEC (no matching stamp); proceeding with a normal update check",
        );
    }
    match startup_authority_decision(true, reexec_authority, expected_artifact.is_ok()) {
        StartupAuthorityDecision::ObserveBootHealth => {
            return ApplyOutcome::Deferred(
                "startup authority requires boot-health observation".to_string(),
            );
        }
        StartupAuthorityDecision::ReturnMatchedReexec => {
            return ApplyOutcome::NotApplicable;
        }
        StartupAuthorityDecision::ReturnMalformedExpected => {
            let Err(error) = expected_artifact else {
                return ApplyOutcome::Deferred(
                    "startup authority classification changed".to_string(),
                );
            };
            return ApplyOutcome::Deferred(error);
        }
        StartupAuthorityDecision::Continue => {}
    }
    let expected_artifact = match expected_artifact {
        Ok(expected) => expected,
        Err(error) => return ApplyOutcome::Deferred(error),
    };
    if !crate::enabled() {
        return ApplyOutcome::NotApplicable;
    }
    // 2. Must be a real installed bundle.
    let Some(b) = bundle::resolve() else {
        return ApplyOutcome::NotApplicable;
    };
    // Reuse the staging resolved once at the top (no second resolve/chmod).
    let Some(staging) = staging else {
        return ApplyOutcome::NotApplicable;
    };

    // 3. Quick pre-lock peek: skip locking entirely when nothing is staged (the
    //    common case). Anything else — newer, not-newer, or corrupt — is decided
    //    UNDER the lock in step 4, so retirement never races the stager's final
    //    publication transaction (F15).
    if matches!(read_ready(&staging, current_build), ReadyState::Absent)
        && boot_sentinel(&staging).read_state().is_none()
    {
        return ApplyOutcome::NoUpdate;
    }

    // 4. Serialize the swap across concurrent launches.
    let _lock = match FileLock::acquire(&staging.apply_lock) {
        Ok(l) => l,
        Err(e) => return ApplyOutcome::Deferred(format!("lock: {e}")),
    };
    if !recover_abandoned_preswap_trial_if_exact(
        &staging,
        &b.app_root,
        current_build,
        current_commit,
    ) {
        let armed_build = boot_sentinel(&staging)
            .read_state()
            .map_or(0, |(build, _)| build);
        return ApplyOutcome::Deferred(format!(
            "update trial for build {armed_build} is still unconfirmed"
        ));
    }
    if let ReadyState::Newer(orphaned_ready) = read_ready(&staging, current_build)
        && let Err(error) = recover_orphaned_prepared_candidate(
            &staging,
            &b.app_root,
            current_build,
            &orphaned_ready,
        )
    {
        return ApplyOutcome::Deferred(error);
    }
    // Under the lock no other swap is in flight, so it is safe to clear orphaned
    // transient swap copies from a previously interrupted/completed swap.
    // Re-read under the lock and act. A stage in flight may continue downloading
    // under stage_lock, but its final publication takes this same apply_lock. Apply
    // retirement touches only ready+staged_app, never that producer's scratch.
    let ready = match read_ready(&staging, current_build) {
        ReadyState::Newer(r) => r,
        ReadyState::NotNewer => {
            staging.retire_published();
            return ApplyOutcome::NoUpdate;
        }
        ReadyState::Corrupt => {
            crate::warn("ready.toml is unreadable; discarding staged update");
            staging.retire_published();
            return ApplyOutcome::NoUpdate;
        }
        ReadyState::Absent => return ApplyOutcome::NoUpdate,
    };
    if let Some(expected) = expected_artifact.as_ref()
        && !ready_matches_expected(&ready, expected)
    {
        return ApplyOutcome::Deferred(format!(
            "staged update no longer matches authorized handoff artifact {} {} {}",
            expected.build, expected.commit, expected.dmg_sha256
        ));
    }

    // 4b. Honor an operator apply floor (yank): never apply a staged build below the
    //     persisted, monotonic min_build — even though it's genuine and strictly newer
    //     than us — so the owner can retire a bad-but-genuine release after the fact (F5).
    let floor = crate::manifest::Floor::read(&staging.floor());
    if ready.build_number < floor.min_build {
        crate::warn(&format!(
            "staged build {} is below the operator floor {}; discarding (yanked)",
            ready.build_number, floor.min_build
        ));
        staging.retire_published();
        crate::status::record(
            &staging,
            current_build,
            &format!(
                "held: staged build {} below floor {} (yanked)",
                ready.build_number, floor.min_build
            ),
        );
        return ApplyOutcome::NoUpdate;
    }
    if !ready.is_publishable(&staging) {
        staging.retire_published();
        return ApplyOutcome::NoUpdate;
    }

    // 5. Can we even write the install location? Checked BEFORE the (more
    //    expensive) re-verification so a persistently non-writable install (e.g.
    //    an admin-owned /Applications) doesn't re-verify the staged bundle on every
    //    single launch — there's nothing we could do with it anyway.
    if !bundle::parent_writable(&b.app_root) {
        crate::status::record(
            &staging,
            current_build,
            "deferred: install location not writable",
        );
        return ApplyOutcome::Deferred(format!(
            "install location not writable: {}",
            b.app_root.display()
        ));
    }

    // 6. Apply-time re-verification (TOCTOU defence), tiered like stage time.
    // `verified_bundle_identity` starts with symlink_metadata, so policy/build/
    // commit can never authenticate a target while we later rename the symlink.
    let (staged_build, sealed_commit) = match verified_bundle_identity(&staging.staged_app) {
        Ok(identity) => identity,
        Err(error) => {
            crate::warn(&format!(
                "staged bundle re-verification failed: {error}; discarding"
            ));
            staging.retire_published();
            crate::status::record(
                &staging,
                current_build,
                "deferred: staged bundle failed re-verification (discarded)",
            );
            return ApplyOutcome::Deferred(format!("re-verify: {error}"));
        }
    };
    // 6b. Re-bind the codesign-sealed CFBundleVersion at APPLY time too, not just at
    //     stage time: the strictly-newer gate above trusts ready.toml's build_number,
    //     which is unauthenticated local state. Require the number SEALED into the
    //     staged bundle to equal it and to still exceed the running build, so a
    //     swapped-in older-but-genuine bundle (with a rewritten marker) is caught (F10).
    if staged_build != ready.build_number || staged_build <= current_build {
        crate::warn(&format!(
            "staged bundle CFBundleVersion {staged_build} != marker build {} or not newer than \
             running {current_build}; discarding",
            ready.build_number
        ));
        staging.retire_published();
        crate::status::record(
            &staging,
            current_build,
            "deferred: staged bundle build-number rebind mismatch (discarded)",
        );
        return ApplyOutcome::Deferred(format!(
            "build-number rebind: sealed {staged_build} vs marker {}",
            ready.build_number
        ));
    }

    // 6c. Bind source provenance at apply time too. A same-build, same-marker
    // bundle from another commit is not the artifact that staging authorized.
    if !sealed_commit_matches(ready.commit.as_deref(), &sealed_commit) {
        crate::warn("staged bundle commit does not match ready marker; discarding");
        staging.retire_published();
        crate::status::record(
            &staging,
            current_build,
            "deferred: staged bundle commit rebind mismatch (discarded)",
        );
        return ApplyOutcome::Deferred("commit rebind mismatch".to_string());
    }

    // 7. Prepare/verify NEW at the fixed destination-volume recovery path BEFORE
    // the point of no return. One atomic exchange then puts NEW at installed and
    // OLD directly at that fixed path; every process-crash cut is discoverable.
    let prepared = match prepare_fixed_swap_candidate(&staging, &b.app_root, &ready, current_build)
    {
        Ok(prepared) => prepared,
        Err(error) => return ApplyOutcome::Deferred(error),
    };

    // The atomic exchange makes the current canonical bundle the sole rollback
    // source. Prove OLD itself is the signed/sealed running build immediately
    // before arming and swapping, after any slow cross-volume candidate copy. A
    // corrupt/replaced install must never become crash-recovery authority.
    let (old_build, old_commit) = match verified_bundle_identity(&b.app_root) {
        Ok((build, commit))
            if identity_matches_running(build, &commit, current_build, current_commit) =>
        {
            (build, commit)
        }
        Ok((build, commit)) => {
            recover_prepared_candidate(&prepared, &staging);
            return ApplyOutcome::Deferred(format!(
                "current installed rollback source {build}/{commit} != running {current_build}/{:?}",
                current_commit
            ));
        }
        Err(error) => {
            recover_prepared_candidate(&prepared, &staging);
            return ApplyOutcome::Deferred(format!(
                "current installed rollback source is not verified: {error}"
            ));
        }
    };
    // Preserve only a receipt that is authority for the exact sealed OLD bundle
    // just rechecked above. A well-formed but stale receipt is unsigned local
    // state and must not be resurrected after an inverse swap.
    let previous_receipt = previous_receipt_for_sealed_old(&staging, old_build, &old_commit);

    // 8. Arm exact crash-loop authority only after fixed NEW is fully verified and
    // immediately before the atomic swap. A crash after this boundary has one
    // deterministic pre-swap shape: installed=OLD, fixed=NEW, ready+trial exact.
    let sentinel = match prepare_trial(&staging, &ready) {
        Ok(sentinel) => sentinel,
        Err(error) => {
            recover_prepared_candidate(&prepared, &staging);
            return ApplyOutcome::Deferred(error);
        }
    };
    let retained = match swap_fixed_candidate(&prepared, &b.app_root) {
        Ok(rollback) => rollback,
        Err(error) => {
            if let Err(disarm_error) = sentinel.confirm() {
                return ApplyOutcome::Deferred(format!(
                    "{error}; trial disarm failed: {disarm_error}"
                ));
            }
            crate::manifest::FailedMark::clear(&staging.trial());
            recover_prepared_candidate(&prepared, &staging);
            return ApplyOutcome::Deferred(error);
        }
    };

    // Persist exact install provenance only AFTER the fixed-path swap. The atomic
    // receipt replacement retires the previous installed artifact's proof at
    // precisely the next complete transaction.
    let ready_commit = ready
        .commit
        .as_deref()
        .expect("read_ready rejected a missing commit");
    if let Err(error) = crate::manifest::InstalledReceipt::record(
        &staging.installed_receipt(),
        ready.build_number,
        ready_commit,
        &ready.dmg_sha256,
    ) {
        crate::warn(&format!(
            "could not commit installed artifact receipt after swap: {error}; rolling back"
        ));
        if let Err(rollback_error) = restore_rollback(&retained, &b.app_root) {
            // NEW remains installed. Preserve sentinel+trial+receipt state so the
            // physical rollback can be retried; never erase recovery authority.
            return ApplyOutcome::Deferred(format!(
                "installed receipt: {error}; pre-exec rollback also failed: {rollback_error}"
            ));
        }
        if let Err(disarm_error) = sentinel.confirm() {
            return ApplyOutcome::Deferred(format!(
                "installed receipt: {error}; rollback succeeded but trial disarm failed: {disarm_error}"
            ));
        }
        crate::manifest::FailedMark::clear(&staging.trial());
        recover_prepared_stage(&retained, &staging);
        return ApplyOutcome::Deferred(format!("installed receipt: {error}"));
    }
    // Don't let it re-apply on the next launch. This retirement cannot touch the
    // fixed rollback sibling; it removes only ready + the duplicate staged NEW.
    staging.retire_published();
    record_activating_status(&staging, &ready);

    // 9. Write the single-use re-exec nonce stamp (0600, in our 0700 dir) BEFORE exec
    //    so the post-swap guard can prove this launch is genuinely our re-exec (F9). If
    //    a nonce can't be made, pass the legacy marker "1": the guard then finds no
    //    matching stamp and takes the safe normal path (a no-op, since `ready` is
    //    already removed) instead of the cleanup shortcut.
    let nonce = random_nonce().unwrap_or_default();
    let reexec_value = if nonce.is_empty() {
        "1".to_string()
    } else {
        let _ = write_private_file(&staging.reexec_stamp(), nonce.as_bytes());
        nonce
    };

    // 10. Re-exec into the new binary while still holding apply_lock. FileLock is
    // explicitly CLOEXEC, so success releases it atomically with image replacement;
    // an exec error leaves it held through rollback. No competing process can swap
    // a higher build or overwrite our nonce between this swap and this exec.
    crate::log(&format!("applied update {} → re-launching", ready.version));
    // `b.exe` is the canonical path we launched from; after the in-place swap it
    // resolves to the NEW binary at the same location.
    let new_exe = &b.exe;
    let mut reexec = boot_reexec_command(new_exe, &reexec_value, handoff_env);
    let err = exec_preserving_handoff_fds(&mut reexec, handoff_fds); // never returns on success
    // exec ITSELF failed (not a later crash): the nonce stamp we wrote is now stale —
    // remove it (F9); then restore the OLD bundle from the retained rollback source,
    // drop the failed new build, and disarm the sentinel (below).
    let _ = std::fs::remove_file(staging.reexec_stamp());
    crate::warn(&format!(
        "re-exec of {} failed: {err}; rolling back to the previous build",
        new_exe.display()
    ));
    if let Err(error) = restore_rollback(&retained, &b.app_root) {
        crate::warn(&format!(
            "rollback failed: {error}; preserving sentinel, trial, receipt, and fixed rollback"
        ));
        return ApplyOutcome::ReExecFailed(format!("{err}; rollback remains pending: {error}"));
    }
    if let Err(error) = sentinel.confirm() {
        return ApplyOutcome::ReExecFailed(format!(
            "{err}; OLD restored but trial disarm failed: {error}"
        ));
    }
    // `restore_rollback` put OLD back at the install and failed NEW at retained.
    // Disarm succeeded, so transaction-owned cleanup is now safe.
    let _ = std::fs::remove_dir_all(&retained);
    let receipt_restore_error =
        restore_installed_receipt(&staging, previous_receipt.as_ref()).err();
    // Suppress the build whose exec failed so the next check doesn't re-download +
    // re-apply it every interval (C1). We still hold `ready` here.
    //
    // BUDGETED, not permanent: this is ONE failed `execve` after a successful
    // rollback — the machine is sitting on its previous, working build. An exec
    // can fail for reasons that are nothing to do with the artifact (ENOMEM, a
    // transient text-file-busy, an unlucky moment during a Gatekeeper scan), and
    // permanently refusing the only newer build on the channel would strand this
    // machine with no user-visible cause. The crash-loop revert below is the case
    // that stays permanent, because there the build PROVED itself bad.
    crate::manifest::FailedMark::record_stage_failure(
        &staging.failed(),
        ready.build_number,
        &ready.dmg_sha256,
        unix_now_secs(),
    );
    crate::manifest::FailedMark::clear(&staging.trial());
    crate::status::record(
        &staging,
        current_build,
        "re-exec of new build failed (rolled back)",
    );
    ApplyOutcome::ReExecFailed(match receipt_restore_error {
        Some(receipt_error) => format!("{err}; {receipt_error}"),
        None => err.to_string(),
    })
}

/// On every launch: if the boot sentinel is armed for `current_build`, count this
/// launch and, if the build has failed to confirm across [`MAX_BOOT_ATTEMPTS`]
/// launches (a crash loop), revert to the retained OLD bundle and re-exec it.
/// Returns `Some` only in the revert path (which normally re-exec's away and does
/// not return); `None` to continue booting the current build.
fn check_boot_health(
    staging: &Staging,
    current_build: u64,
    current_commit: Option<&str>,
    handoff_fds: &[i32],
) -> Option<ApplyOutcome> {
    let sentinel = boot_sentinel(staging);
    // Cheap non-mutating early-out: nothing armed for us. A sentinel for another
    // build belongs to that build and is never "stale" authority for this process.
    if !matches!(sentinel.read_state(), Some((b, _)) if b == current_build) {
        return None;
    }
    let apply_lock = match FileLock::acquire(&staging.apply_lock) {
        Ok(lock) => lock,
        Err(error) => return Some(ApplyOutcome::Deferred(format!("health lock: {error}"))),
    };
    // Revalidate after acquiring the transaction lock. Another process may have
    // armed a newer build between the cheap peek and this point.
    if !matches!(sentinel.read_state(), Some((b, _)) if b == current_build) {
        return None;
    }
    let b = bundle::resolve()?;
    // COUNT THE LAUNCH FIRST. This used to run after the recovery proof below, so a
    // trial whose proof failed every boot never advanced its attempt counter, never
    // reached MAX_BOOT_ATTEMPTS, and therefore never reverted OR confirmed: the
    // sentinel stayed armed forever and this function returned `Deferred` on every
    // subsequent launch, which makes `apply_staged_if_ready` return early forever.
    // The machine kept running (and kept downloading and staging updates) but could
    // never apply another one, with nothing in the UI to say why. The attempt count
    // must measure LAUNCHES OBSERVED, which is a fact about this boot, not a
    // conclusion that depends on a proof that may itself be what is broken.
    if let Err(error) = sentinel.observe_launch(current_build) {
        return Some(ApplyOutcome::Deferred(format!(
            "trial launch observation: {error}"
        )));
    }
    let verified_rollback =
        match ensure_current_trial_receipt(staging, &b.app_root, current_build, current_commit) {
            Ok(rollback) => rollback,
            Err(error) => {
                // Within budget: the proof may recover on a later boot (a transient
                // read failure, a receipt being rewritten), so keep the trial armed.
                if !sentinel.should_revert(current_build, MAX_BOOT_ATTEMPTS) {
                    return Some(ApplyOutcome::Deferred(format!(
                        "trial recovery proof: {error}"
                    )));
                }
                // Budget exhausted AND the rollback is unprovable, so reverting is
                // not available either. Staying armed is the strictly worse option:
                // this build demonstrably BOOTS — we are executing its code, this
                // many times in a row — and remaining armed only guarantees that no
                // future update can ever apply. Disarm, keep running, and make the
                // reason loud and durable instead of silently bricking the updater.
                let disarm = sentinel.confirm();
                crate::health::Health::record_apply_failure(
                    &staging.health(),
                    &format!(
                        "trial recovery proof failed {MAX_BOOT_ATTEMPTS}x ({error}); disarmed \
                         the boot sentinel to keep updates possible"
                    ),
                );
                crate::status::record(
                    staging,
                    current_build,
                    "recovered a wedged update trial (rollback unprovable); updates re-enabled",
                );
                crate::warn(&format!(
                    "trial recovery proof failed {MAX_BOOT_ATTEMPTS} launches in a row \
                     ({error}); the running build boots, so the boot sentinel was disarmed \
                     rather than blocking every future update"
                ));
                if let Err(disarm_error) = disarm {
                    // Could not even clear the sentinel: report it rather than
                    // pretending the wedge is resolved.
                    return Some(ApplyOutcome::Deferred(format!(
                        "trial recovery proof: {error}; disarm also failed: {disarm_error}"
                    )));
                }
                staging.retire_published();
                return Some(ApplyOutcome::NotApplicable);
            }
        };
    if !sentinel.should_revert(current_build, MAX_BOOT_ATTEMPTS) {
        // An unconfirmed trial owns the fixed rollback path. Do not fall through
        // into another apply transaction that could overwrite its sole OLD copy.
        return Some(ApplyOutcome::NotApplicable);
    }
    Some(revert_to_rollback(
        &b,
        staging,
        &sentinel,
        current_build,
        verified_rollback,
        handoff_fds,
        apply_lock,
    ))
}

/// The trialed build is crash-looping: swap the retained OLD bundle back over the
/// install, discard the failed new build + sentinel + staged bundle, and re-exec
/// the restored OLD binary. A missing/temporarily failing inverse swap preserves
/// all recovery authority and returns Deferred; NEW may still be installed.
fn revert_to_rollback(
    b: &bundle::Bundle,
    staging: &Staging,
    sentinel: &Sentinel,
    current_build: u64,
    verified_rollback: PathBuf,
    handoff_fds: &[i32],
    _apply_lock: FileLock,
) -> ApplyOutcome {
    // check_boot_health acquired apply_lock before observing/incrementing. Keep that
    // same CLOEXEC guard through rollback exec; this function never takes stage_lock.
    if !sentinel.should_revert(current_build, MAX_BOOT_ATTEMPTS) {
        return ApplyOutcome::NotApplicable;
    }
    let rb = verified_rollback;
    if !is_non_symlink_dir(&rb) || !same_volume(&rb, &b.app_root) {
        return ApplyOutcome::Deferred(
            "verified crash-loop rollback changed before inverse swap".to_string(),
        );
    }
    if let Err(e) = restore_rollback(&rb, &b.app_root) {
        crate::warn(&format!(
            "crash-loop revert failed: {e}; preserving recovery authority"
        ));
        return ApplyOutcome::Deferred(format!("crash-loop revert: {e}"));
    }
    if let Err(error) = sentinel.confirm() {
        // OLD is restored and failed NEW remains at fixed. Keep trial/receipt too;
        // the next OLD launch can recognize and finish this inverse-swap cut.
        return ApplyOutcome::Deferred(format!(
            "rollback succeeded but trial disarm failed: {error}"
        ));
    }
    // Poison the crash-looping build so the next background check doesn't re-download +
    // re-apply it into another crash/revert loop (C1). Its DMG sha was recorded beside
    // the sentinel at arm time (we no longer hold `ready`); guard on the build match.
    //
    // PERMANENT on purpose (unlike the re-exec-failure path above): reaching here
    // means the build was swapped in and then failed to confirm boot health
    // MAX_BOOT_ATTEMPTS times in a row. That is the build proving itself bad on
    // this machine, and re-applying it on a timer would just re-enter the
    // crash/revert loop. The escape is a newer build or a re-publish under a
    // different digest — both of which clear the memo by key.
    if let Some(t) = crate::manifest::FailedMark::read(&staging.trial())
        && t.build_number == current_build
    {
        crate::manifest::FailedMark::record(&staging.failed(), t.build_number, &t.sha256);
    }
    // OLD is restored at the install; failed NEW sits at rb. Disarm succeeded, so
    // transaction cleanup cannot leave an armed trial without recovery metadata.
    let _ = std::fs::remove_dir_all(&rb);
    crate::manifest::InstalledReceipt::clear(&staging.installed_receipt());
    crate::manifest::FailedMark::clear(&staging.trial());
    staging.retire_published(); // the staged build is bad — never re-apply it
    crate::status::record(
        staging,
        current_build,
        "reverted crash-looping update to the previous build",
    );
    crate::warn("reverted a crash-looping update to the previous build");
    // Re-exec the restored OLD binary as a FRESH boot: no re-exec env, no sentinel
    // (already cleared), and nothing staged, so it comes up clean on the old build.
    let mut reexec = Command::new(&b.exe);
    reexec
        .args(std::env::args_os().skip(1))
        // NO --window here: the restored ROLLBACK build may predate the
        // one-binary router entirely (its gui parser exits 2 on the unknown
        // flag — a dead relaunch is worse than a mode-imperfect one). A
        // pre-collapse rollback IS the window binary and needs no flag; a
        // one-binary rollback launched flag-less from a TTY degrades to a
        // session but stays alive.
        ;
    let err = exec_preserving_handoff_fds(&mut reexec, handoff_fds);
    crate::warn(&format!("re-exec of restored build failed: {err}"));
    ApplyOutcome::ReExecFailed(err.to_string())
}

fn confirm_trial_health_after_proof(
    staging: &Staging,
    current_build: u64,
    prove: impl FnOnce() -> Result<(), String>,
) -> bool {
    let sentinel = boot_sentinel(staging);
    // Only disarm a sentinel that is for THIS build — never clobber one a
    // concurrent apply just armed for a different (newer) build.
    if !matches!(sentinel.read_state(), Some((b, _)) if b == current_build) {
        return false;
    }
    if prove().is_err() {
        return false;
    }
    // Recheck immediately before the irreversible disarm boundary. apply_lock
    // excludes compliant peers; this also fails closed against an unexpected
    // out-of-band marker replacement.
    if !matches!(sentinel.read_state(), Some((b, _)) if b == current_build)
        || sentinel.confirm().is_err()
    {
        return false;
    }
    // Booted healthy → no crash-loop poison pending. The installed receipt is
    // intentionally independent and survives for overlapping old processes.
    crate::manifest::FailedMark::clear(&staging.trial());
    true
}

/// Complete health confirmation after the caller acquires apply_lock. Returning
/// false means the sentinel belongs to another build (or no trial is active), in
/// which case every transaction-owned artifact must remain untouched.
fn confirm_health_under_apply_lock_with_proof(
    staging: &Staging,
    current_build: u64,
    app_root: &Path,
    prove: impl FnOnce() -> Result<(), String>,
) -> bool {
    if !confirm_trial_health_after_proof(staging, current_build, prove) {
        return false;
    }
    // Successful disarm is the cleanup boundary. Before it, every branch above
    // preserves fixed rollback, trial, receipt, and staged orphan intact.
    let _ = remove_path_no_follow(&rollback_path(app_root));
    // A ready marker means a newer publisher now owns staged_app. Only reclaim the
    // swapped-out orphan when no published generation exists.
    if !staging.ready.exists() {
        let _ = std::fs::remove_dir_all(&staging.staged_app);
    }
    true
}

fn confirm_health_under_apply_lock(
    staging: &Staging,
    current_build: u64,
    current_commit: Option<&str>,
    installed_app: Option<&Path>,
) -> bool {
    let Some(app_root) = installed_app else {
        return false;
    };
    confirm_health_under_apply_lock_with_proof(staging, current_build, app_root, || {
        // This is the hard health-commit gate. Plist fields become sealed evidence
        // only after policy verification; the fixed OLD must itself be verified
        // and strictly older; receipt build+commit+DIGEST must equal active trial.
        ensure_current_trial_receipt(staging, app_root, current_build, current_commit).map(|_| ())
    })
}

/// Confirm the running build reached a healthy checkpoint (window up / first
/// frame): clear the boot sentinel and GC the retained rollback bundle + orphaned
/// post-swap copy. Idempotent and best-effort — call once from the GUI after deep init so
/// that a crash BEFORE this point is caught by [`check_boot_health`], while a
/// crash after it is a normal (non-update) fault the sentinel must not react to.
#[must_use]
pub fn confirm_boot_health(current_build: u64, current_commit: Option<&str>) -> bool {
    let Some(staging) = Staging::resolve() else {
        return true;
    };
    // No trial for this build is already complete. A different build's marker is
    // not ours to retry or mutate.
    if !matches!(
        boot_sentinel(&staging).read_state(),
        Some((build, _)) if build == current_build
    ) {
        return true;
    }
    // Publication/apply lock only: never stage_lock, so a long download cannot
    // delay this off-UI cleanup and no inverse lock ordering exists.
    let Ok(_apply_lock) = FileLock::acquire(&staging.apply_lock) else {
        return false;
    };
    let Some(installed) = bundle::resolve() else {
        return false;
    };
    confirm_health_under_apply_lock(
        &staging,
        current_build,
        current_commit,
        Some(installed.app_root.as_path()),
    )
}

/// Tri-state read of the staging marker, distinguishing a missing marker from a
/// present-but-unparseable one (the latter is discarded rather than wedging
/// updates forever) and folding in the strict downgrade gate.
enum ReadyState {
    Newer(Ready),
    NotNewer,
    Corrupt,
    Absent,
}

fn read_ready(staging: &Staging, current_build: u64) -> ReadyState {
    match Ready::read(&staging.ready) {
        Some(r) if !r.has_canonical_identity() => ReadyState::Corrupt,
        Some(r) if r.build_number > current_build => ReadyState::Newer(r),
        Some(_) => ReadyState::NotNewer,
        None if staging.ready.exists() => ReadyState::Corrupt,
        None => ReadyState::Absent,
    }
}

/// Exec-failure rollback: put the OLD bundle (at `rollback`) back at `installed`,
/// which currently holds the NEW (failed-to-exec) bundle. Both are on the same
/// volume, so this is the inverse atomic exchange.
fn restore_rollback(rollback: &Path, installed: &Path) -> Result<(), String> {
    checked_bundle_exchange(rollback, installed, "atomic rollback restore")
}

/// 16 CSPRNG bytes, hex-encoded (32 chars), for the single-use re-exec nonce (F9).
/// Unguessable so an attacker can't preset a matching env var. Minted through
/// `aterm_uds::rand` — the ONE audited entropy surface — not a hand-rolled
/// `/dev/urandom` read: this runs at EVERY packaged-app launch that finds a
/// staged build (`apply_staged_if_ready`), and the hand-rolled pattern is what
/// caused the 2026-07-04/05 kernel panics elsewhere in the workspace. `None` if
/// the OS CSPRNG is somehow unavailable; the caller then degrades safely to the
/// legacy "1" marker (guard finds no stamp and takes the normal no-op path).
fn random_nonce() -> Option<String> {
    aterm_uds::rand::hex_token::<16>().ok()
}

/// Write `data` to `path` as a fresh `0600` file (truncating any prior content).
fn write_private_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)
}

/// Best-effort RFC3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`), for the
/// human-readable `staged_at`/status fields. Computed in-process (no `/bin/date`
/// fork — this is called from status/health/stage/loop, dozens of times per
/// session). Falls back to the empty string on a pre-epoch clock.
pub(crate) fn now_rfc3339() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => format_rfc3339(d.as_secs()),
        Err(_) => String::new(),
    }
}

/// Unix seconds now, or 0 on a pre-epoch clock. Used for the stage-failure retry
/// budget's deadlines. Zero is the safe fallback: it makes every deadline appear
/// already passed, so a broken clock retries rather than suppresses forever.
#[must_use]
pub(crate) fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Seconds between two of our own RFC3339 instants (`later - earlier`), or
/// `None` when either string is not the exact `YYYY-MM-DDTHH:MM:SSZ` shape this
/// module writes (e.g. the empty pre-epoch fallback). Lexicographic order on
/// these strings IS chronological, so a plain component parse suffices — no
/// calendar math is needed for a difference of epochs re-derived per component.
pub(crate) fn rfc3339_delta_secs(earlier: &str, later: &str) -> Option<u64> {
    fn epoch(s: &str) -> Option<u64> {
        // YYYY-MM-DDTHH:MM:SSZ — 20 bytes, fixed layout.
        if s.len() != 20 || !s.ends_with('Z') {
            return None;
        }
        let (y, mo, d) = (
            s.get(0..4)?.parse::<i64>().ok()?,
            s.get(5..7)?.parse::<i64>().ok()?,
            s.get(8..10)?.parse::<i64>().ok()?,
        );
        let (h, mi, sec) = (
            s.get(11..13)?.parse::<u64>().ok()?,
            s.get(14..16)?.parse::<u64>().ok()?,
            s.get(17..19)?.parse::<u64>().ok()?,
        );
        // days_from_civil (the inverse of format_rfc3339's civil_from_days).
        let y = if mo <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let mp = if mo > 2 { mo - 3 } else { mo + 9 };
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        u64::try_from(days)
            .ok()?
            .checked_mul(86_400)?
            .checked_add(h * 3600 + mi * 60 + sec)
    }
    epoch(later)?.checked_sub(epoch(earlier)?)
}

// The RFC3339 UTC stamp is `aterm_types::rfc3339::format_rfc3339` — one
// workspace home for the Howard-Hinnant civil-calendar math the publisher,
// the updater client, the GUI and atpkg all stamp with.
use aterm_types::rfc3339::format_rfc3339;

#[cfg(test)]
mod tests {

    /// THE ChildDied regression (2026-07-22): the post-swap re-exec image must
    /// carry BOTH the single-use re-exec nonce and the caller's restored
    /// handoff authority pairs — the successor's prearm cannot validate the
    /// inherited overlap handoff without them and exits before writing the
    /// readiness proof. Asserted on the constructed Command (program, `--window`
    /// mode pin first, envs) so the contract is testable off-mac without exec.
    #[test]
    fn boot_reexec_command_pins_mode_and_restores_handoff_env() {
        let handoff_env = vec![(
            std::ffi::OsString::from("ATERM_HANDOFF_PARENT_PID"),
            std::ffi::OsString::from("4242"),
        )];
        let command = super::boot_reexec_command(
            std::path::Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
            "0123456789abcdef",
            &handoff_env,
        );
        assert_eq!(
            command.get_program(),
            std::ffi::OsStr::new("/Applications/aterm.app/Contents/MacOS/aterm")
        );
        assert_eq!(
            command.get_args().next(),
            Some(std::ffi::OsStr::new("--window")),
            "the mode pin must precede the forwarded args"
        );
        let envs: Vec<_> = command.get_envs().collect();
        assert!(
            envs.contains(&(
                std::ffi::OsStr::new("ATERM_UPDATE_REEXEC"),
                Some(std::ffi::OsStr::new("0123456789abcdef")),
            )),
            "re-exec nonce set"
        );
        assert!(
            envs.contains(&(
                std::ffi::OsStr::new("ATERM_HANDOFF_PARENT_PID"),
                Some(std::ffi::OsStr::new("4242")),
            )),
            "handoff authority restored onto the exec image"
        );
    }

    /// `rfc3339_delta_secs` inverts `format_rfc3339` across day/month/year and
    /// leap boundaries, and fails closed on malformed input (the pre-epoch
    /// empty-string fallback) — the duration gate on the persistent-failure
    /// notice depends on both properties.
    #[test]
    fn rfc3339_delta_round_trips_and_fails_closed() {
        for (a, b, want) in [
            (0u64, 61, 61),
            (86_399, 86_401, 2),                   // midnight crossing
            (1_709_164_799, 1_709_164_800, 1),     // Feb 29 2024 (leap) boundary
            (1_784_100_000, 1_784_101_800, 1_800), // a modern 30-min streak
        ] {
            let (ea, eb) = (super::format_rfc3339(a), super::format_rfc3339(b));
            assert_eq!(
                super::rfc3339_delta_secs(&ea, &eb),
                Some(want),
                "{ea}..{eb}"
            );
            assert_eq!(
                super::rfc3339_delta_secs(&eb, &ea),
                None,
                "reversed underflows to None"
            );
        }
        assert_eq!(super::rfc3339_delta_secs("", &super::now_rfc3339()), None);
        assert_eq!(
            super::rfc3339_delta_secs("garbage-not-a-date-Z", "2026-07-15T00:00:00Z"),
            None
        );
    }
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn temp_staging() -> (Staging, std::path::PathBuf) {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("aterm-rr-{}-{n}", std::process::id()));
        std::fs::create_dir_all(root.join("staged")).unwrap();
        std::fs::create_dir_all(root.join("download")).unwrap();
        let s = Staging {
            apply_lock: root.join("apply.lock"),
            stage_lock: root.join("stage.lock"),
            download: root.join("download"),
            staged_app: root.join("staged").join("aterm.app"),
            ready: root.join("ready.toml"),
            status: root.join("status.toml"),
            root: root.clone(),
        };
        (s, root)
    }

    fn write_ready(s: &Staging, build: u64) {
        let r = Ready {
            build_number: build,
            version: format!("0.0.{build}"),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            dmg_sha256: "ab".repeat(32),
            team_id: "T".into(),
            staged_at: String::new(),
            changelog: None,
        };
        std::fs::write(&s.ready, r.to_toml().unwrap()).unwrap();
    }

    #[test]
    fn read_ready_classifies_all_states() {
        let (s, root) = temp_staging();

        // Absent: no marker file.
        assert!(matches!(read_ready(&s, 10), ReadyState::Absent));

        // Corrupt: present but unparseable → must be discardable, not Absent.
        std::fs::write(&s.ready, "this is not valid toml {{{").unwrap();
        assert!(matches!(read_ready(&s, 10), ReadyState::Corrupt));

        // Newer: staged build strictly greater than running.
        write_ready(&s, 20);
        assert!(matches!(read_ready(&s, 10), ReadyState::Newer(_)));

        // NotNewer: equal or lower than running (downgrade gate).
        assert!(matches!(read_ready(&s, 20), ReadyState::NotNewer));
        assert!(matches!(read_ready(&s, 21), ReadyState::NotNewer));

        // Parseable is not sufficient: a short commit cannot authorize apply and
        // must not be surfaced by the status reader as a staged update either.
        let mut corrupt = Ready::read(&s.ready).unwrap();
        corrupt.commit = Some("0123456789ab".into());
        std::fs::write(&s.ready, corrupt.to_toml().unwrap()).unwrap();
        assert!(matches!(read_ready(&s, 10), ReadyState::Corrupt));

        let _ = std::fs::remove_dir_all(root);
    }

    /// Overlap seam 1 (pre-park verification): every refusal on this ladder
    /// happens with a plain `Err` return — the caller (the GUI handoff
    /// starter) receives it BEFORE `park_all_readers`, so an absent, stale,
    /// wrong-artifact, or unverifiable candidate can never cost a parked
    /// reader, a frozen frame, or a doomed child spawn.
    #[test]
    fn preverify_refuses_unverifiable_candidates_before_any_reader_could_park() {
        let (s, root) = temp_staging();

        // Nothing staged: refuse immediately.
        let absent = preverify_staged_handoff_candidate_at(&s, 10, None, None);
        assert!(
            absent.clone().unwrap_err().contains("no verified update"),
            "{absent:?}"
        );

        // Staged but not strictly newer than the running build.
        write_ready(&s, 20);
        let stale = preverify_staged_handoff_candidate_at(&s, 20, None, None);
        assert!(
            stale.clone().unwrap_err().contains("strictly newer"),
            "{stale:?}"
        );

        // Staged build is not the artifact the updater reducer authorized.
        let wrong = preverify_staged_handoff_candidate_at(&s, 10, Some(21), None);
        assert!(
            wrong
                .clone()
                .unwrap_err()
                .contains("not the authorized build"),
            "{wrong:?}"
        );

        // Right identity on the marker, but no verifiable bundle exists at the
        // staged path: the sealed-identity gate must fail closed.
        let unverifiable = preverify_staged_handoff_candidate_at(&s, 10, Some(20), None);
        assert!(unverifiable.is_err(), "{unverifiable:?}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn successful_swap_status_is_new_build_and_has_no_staged_claim() {
        let (s, root) = temp_staging();
        write_ready(&s, 54);
        let ready = Ready::read(&s.ready).unwrap();
        s.retire_published();
        record_activating_status(&s, &ready);

        let text = std::fs::read_to_string(&s.status).expect("activation status written");
        let value: toml::Value = toml::from_str(&text).expect("activation status parses");
        assert_eq!(
            value.get("current_build").and_then(toml::Value::as_integer),
            Some(54)
        );
        assert_eq!(
            value.get("outcome").and_then(toml::Value::as_str),
            Some("installed 0.0.54 (build 54); activating now")
        );
        assert!(
            value.get("staged_build").is_none(),
            "retired ready marker cannot leave a staged status field: {text}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn release_and_ready_commits_bind_to_sealed_bundle_provenance() {
        let full = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(canonical_release_commit(Some(full)), Some(full.to_string()));
        assert!(sealed_commit_matches(Some(full), "0123456789ab"));

        for invalid in [
            None,
            Some(""),
            Some("unknown"),
            Some("0123456"),
            Some("0123456789ab"),
            Some("0123456789abcdef0123456789abcdef0123456"),
            Some("0123456789abcdef0123456789abcdef012345678"),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
            Some("0123456789ab-dirty"),
        ] {
            assert!(canonical_release_commit(invalid).is_none());
        }
        assert!(
            !sealed_commit_matches(Some(full), "fedcba9876543210fedcba9876543210fedcba98"),
            "negative control: same build/digest marker cannot authorize wrong sealed commit"
        );
    }

    #[test]
    fn running_rollback_identity_requires_build_and_commit() {
        let running = "0123456789ab";
        let same = "0123456789abcdef0123456789abcdef01234567";
        let other = "fedcba9876543210fedcba9876543210fedcba98";
        assert!(identity_matches_running(53, same, 53, Some(running)));
        assert!(!identity_matches_running(53, other, 53, Some(running)));
        assert!(!identity_matches_running(54, same, 53, Some(running)));
    }

    #[test]
    fn expected_handoff_authority_is_exact_build_commit_and_digest() {
        let ready = Ready {
            build_number: 54,
            version: "0.54".into(),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            dmg_sha256: "ab".repeat(32),
            team_id: "T".into(),
            staged_at: String::new(),
            changelog: None,
        };
        let exact = ExpectedArtifact {
            build: 54,
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            dmg_sha256: "ab".repeat(32),
        };
        assert!(ready_matches_expected(&ready, &exact));
        let mut wrong_digest = exact.clone();
        wrong_digest.dmg_sha256 = "cd".repeat(32);
        assert!(!ready_matches_expected(&ready, &wrong_digest));
        let mut wrong_commit = exact.clone();
        wrong_commit.commit = "fedcba9876543210fedcba9876543210fedcba98".into();
        assert!(!ready_matches_expected(&ready, &wrong_commit));
        let mut wrong_build = exact;
        wrong_build.build = 55;
        assert!(!ready_matches_expected(&ready, &wrong_build));
    }

    #[test]
    fn inverse_swap_receipt_restore_binds_old_identity_and_fails_closed() {
        let model = aterm_spec::derive::native_update_disk_transaction_model();
        let (staging, root) = temp_staging();
        let old_commit = "c16c6fd7955b0011223344556677889900aabbcc";
        let old_digest = "cd".repeat(32);
        crate::manifest::InstalledReceipt::record(
            &staging.installed_receipt(),
            52,
            old_commit,
            &old_digest,
        )
        .unwrap();
        let previous = previous_receipt_for_sealed_old(&staging, 52, old_commit)
            .expect("exact sealed OLD receipt");
        let mut stale_state = disk_model_ready(&model);
        disk_model_step(&model, &mut stale_state, "CorruptPreviousReceipt");
        assert!(
            previous_receipt_for_sealed_old(&staging, 52, &"f".repeat(40)).is_none(),
            "well-formed but stale local receipt is not rollback authority"
        );
        assert!(
            previous_receipt_for_sealed_old(&staging, 51, old_commit).is_none(),
            "receipt build must bind the sealed OLD build"
        );
        disk_model_step(&model, &mut stale_state, "PrepareFixedNew");
        assert_eq!(stale_state["previous_receipt_saved"], 0);

        crate::manifest::InstalledReceipt::record(
            &staging.installed_receipt(),
            53,
            "0123456789abcdef0123456789abcdef01234567",
            &"ef".repeat(32),
        )
        .unwrap();
        let blocked_tmp = staging
            .installed_receipt()
            .with_extension(format!("toml.{}.tmp", std::process::id()));
        std::fs::create_dir(&blocked_tmp).unwrap();
        let error = restore_installed_receipt(&staging, Some(&previous))
            .expect_err("blocked kind-preserving rewrite must surface failure");
        let mut failure_state = disk_model_ready(&model);
        for action in [
            "PrepareFixedNew",
            "ArmExactTrial",
            "AtomicSwap",
            "RecordExactReceipt",
            "VerifyExactRollback",
            "ExecFails",
            "RestoreExactOld",
            "DisarmRestoredTrialReceiptRestoreFailsClosed",
        ] {
            disk_model_step(&model, &mut failure_state, action);
        }
        assert!(error.contains("superseded receipt cleared"), "{error}");
        assert!(
            !staging.installed_receipt().exists(),
            "failed NEW receipt is cleared instead of surviving as OLD authority"
        );
        assert_eq!(failure_state["receipt_restore_failed"], 1);
        assert_eq!(failure_state["superseded_receipt_cleared"], 1);
        std::fs::remove_dir(&blocked_tmp).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn apply_retirement_never_removes_stage_owned_download_scratch() {
        let (s, root) = temp_staging();
        write_ready(&s, 5);
        std::fs::create_dir_all(&s.staged_app).unwrap();
        std::fs::write(s.download.join("aterm-0.0.5.dmg.part"), b"partial").unwrap();
        let _apply = FileLock::acquire(&s.apply_lock).unwrap();
        s.retire_published();
        assert!(!s.ready.exists());
        assert!(!s.staged_app.exists());
        assert_eq!(
            std::fs::read(s.download.join("aterm-0.0.5.dmg.part")).unwrap(),
            b"partial",
            "apply owns only the published generation; stage scratch survives"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn apply_lock_spans_preexec_and_failure_rollback_window() {
        let (s, root) = temp_staging();
        let apply = FileLock::acquire(&s.apply_lock).unwrap();
        let lock_path = s.apply_lock.clone();
        let (attempting_tx, attempting_rx) = std::sync::mpsc::sync_channel(0);
        let (acquired_tx, acquired_rx) = std::sync::mpsc::sync_channel(0);
        let competitor = std::thread::spawn(move || {
            attempting_tx.send(()).unwrap();
            let _competing_apply = FileLock::acquire(&lock_path).unwrap();
            acquired_tx.send(()).unwrap();
        });
        attempting_rx.recv().unwrap();
        assert!(
            acquired_rx
                .recv_timeout(std::time::Duration::from_millis(25))
                .is_err(),
            "a competing swap cannot enter before exec/rollback resolves"
        );

        // This mutation stands in for the synchronous exec-error rollback path;
        // the same guard remains live across it in apply_staged_if_ready.
        std::fs::write(root.join("rollback-complete"), b"done").unwrap();
        assert!(!root.join("competing-swap").exists());
        drop(apply);
        acquired_rx.recv().unwrap();
        competitor.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_apply_retirement_cannot_erase_a_newer_stage_publisher() {
        let (s, root) = temp_staging();
        write_ready(&s, 5);
        make_app(&s.staged_app, "OLD-STAGE");

        // Apply acquires only the short publication/swap lock. A stager may keep
        // downloading under stage_lock while this guard is held, but must wait at
        // the final publication boundary.
        let apply = FileLock::acquire(&s.apply_lock).unwrap();
        let stage = s.clone();
        let (prepared_tx, prepared_rx) = std::sync::mpsc::sync_channel(0);
        let publisher = std::thread::spawn(move || {
            let _stage_lock = FileLock::acquire(&stage.stage_lock).unwrap();
            let incoming = stage.staged_dir().join("aterm.app.incoming");
            make_app(&incoming, "NEW-STAGE");
            let scratch = stage.download.join("aterm-0.0.6.dmg");
            std::fs::write(&scratch, b"new download").unwrap();
            prepared_tx.send((incoming.clone(), scratch)).unwrap();
            let ready = Ready {
                build_number: 6,
                version: "0.0.6".into(),
                commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
                dmg_sha256: "cd".repeat(32),
                team_id: "T".into(),
                staged_at: String::new(),
                changelog: None,
            };
            publish_verified_stage(&stage, &incoming, &ready)
        });

        let (incoming, scratch) = prepared_rx.recv().unwrap();
        s.retire_published();
        assert!(incoming.is_dir(), "apply must not erase unpublished input");
        assert_eq!(
            std::fs::read(&scratch).unwrap(),
            b"new download",
            "apply must not erase stage-owned download scratch"
        );
        drop(apply);

        publisher.join().unwrap().unwrap();
        assert_eq!(read_id(&s.staged_app), "NEW-STAGE");
        assert!(matches!(
            read_ready(&s, 5),
            ReadyState::Newer(Ready {
                build_number: 6,
                ..
            })
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn trial_arm_failure_keeps_verified_stage_ready_and_unswapped() {
        let (s, root) = temp_staging();
        write_ready(&s, 6);
        make_app(&s.staged_app, "VERIFIED-NEW");
        // Atomic rename cannot replace a directory at the sentinel path.
        std::fs::create_dir_all(s.root.join("boot.sentinel")).unwrap();
        let ready = Ready::read(&s.ready).unwrap();

        assert!(prepare_trial(&s, &ready).is_err());
        assert!(s.ready.exists());
        assert_eq!(read_id(&s.staged_app), "VERIFIED-NEW");
        assert!(
            crate::manifest::FailedMark::read(&s.trial()).is_none(),
            "a failed sentinel arm cannot mint partial trial authority"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    // --- swap / rollback machinery (real same-volume RENAME_SWAP on macOS) --------
    //
    // These prove the security-critical invariants the self-updater must never get
    // wrong: the live install is NEVER left missing, the swapped-out OLD bundle is
    // preserved as a rollback source, and a revert restores the previous build.

    fn make_app(dir: &Path, id: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("id"), id).unwrap();
    }
    fn read_id(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("id")).unwrap()
    }
    fn copied_candidate(path: &Path) -> PreparedSwapCandidate {
        PreparedSwapCandidate {
            fixed: path.to_path_buf(),
            moved_from_stage: false,
        }
    }

    fn disk_model_step(
        model: &aterm_spec::derive::Model,
        state: &mut aterm_spec::interp::State,
        action: &'static str,
    ) {
        let successors = model.successors(action, state);
        assert_eq!(
            successors.len(),
            1,
            "real transaction step must map to exactly one {action} successor: {state:?}"
        );
        *state = successors[0].clone();
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, state),
                "real transaction step violates {}::{} after {action}: {state:?}",
                model.name,
                invariant.name,
            );
        }
    }

    fn disk_model_ready(model: &aterm_spec::derive::Model) -> aterm_spec::interp::State {
        let mut state = model.init_state();
        for action in [
            "ConsumeStartupAuthority",
            "ObserveBootHealth",
            "EnterDiskLane",
        ] {
            disk_model_step(model, &mut state, action);
        }
        state
    }

    fn fixture_identity(path: &Path) -> i64 {
        match std::fs::read_to_string(path.join("id")).as_deref() {
            Ok("OLD") => 1,
            Ok("NEW") => 2,
            Ok(_) => 3,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => panic!("read fixture identity {}: {error}", path.display()),
        }
    }

    /// Project the genuine fixed paths and exact ledgers independently back onto
    /// the derived transaction variables. The fixture id stands in only for the
    /// already-tested codesign-sealed OLD/NEW identity guard; every exchange,
    /// sentinel transition, and receipt write below uses shipping code.
    fn assert_real_disk_projection(
        state: &aterm_spec::interp::State,
        staging: &Staging,
        installed: &Path,
        current_build: u64,
        current_commit: &str,
        current_digest: &str,
    ) {
        let installed_identity = fixture_identity(installed);
        let fixed = rollback_path(installed);
        let fixed_identity = fixture_identity(&fixed);
        assert_eq!(state["installed"], installed_identity);
        assert_eq!(state["fixed"], fixed_identity);
        assert_eq!(
            state["fixed_exact"],
            i64::from(matches!(fixed_identity, 1 | 2))
        );

        let trial_exact =
            matches!(
                boot_sentinel(staging).read_state(),
                Some((build, _)) if build == current_build
            ) && crate::manifest::FailedMark::read(&staging.trial()).is_some_and(|trial| {
                trial.build_number == current_build
                    && trial.sha256.eq_ignore_ascii_case(current_digest)
            });
        assert_eq!(state["trial"], i64::from(trial_exact));

        let receipt_exact = crate::manifest::InstalledReceipt::read(&staging.installed_receipt())
            .is_some_and(|receipt| {
                receipt.matches_sealed(current_build, current_commit)
                    && receipt.dmg_sha256.eq_ignore_ascii_case(current_digest)
            });
        assert_eq!(state["receipt"], i64::from(receipt_exact));
        assert_eq!(state["receipt_exact"], i64::from(receipt_exact));
    }
    /// A same-volume temp root (both bundles must share a volume for RENAME_SWAP).
    fn swap_root(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("aterm-swap-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn fixed_swap_places_old_at_the_only_recovery_path_atomically() {
        let root = swap_root("basic");
        let installed = root.join("installed.app");
        let fixed = rollback_path(&installed);
        make_app(&fixed, "NEW");
        make_app(&installed, "OLD");

        let rollback =
            swap_fixed_candidate(&copied_candidate(&fixed), &installed).expect("fixed swap");
        assert_eq!(read_id(&installed), "NEW");
        assert_eq!(read_id(&rollback), "OLD");
        assert_eq!(rollback, rollback_path(&installed));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restore_rollback_swaps_the_previous_build_back() {
        let root = swap_root("restore");
        let installed = root.join("installed.app");
        let fixed = rollback_path(&installed);
        make_app(&fixed, "NEW");
        make_app(&installed, "OLD");

        let rollback = swap_fixed_candidate(&copied_candidate(&fixed), &installed).unwrap();
        assert_eq!(read_id(&installed), "NEW");
        restore_rollback(&rollback, &installed).unwrap();
        assert_eq!(read_id(&installed), "OLD");
        assert_eq!(read_id(&fixed), "NEW");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn fixed_swap_crash_cuts_are_always_discoverable() {
        let root = swap_root("crash-cuts");
        let installed = root.join("installed.app");
        let fixed = rollback_path(&installed);
        make_app(&installed, "OLD");
        make_app(&fixed, "NEW");

        // Pre-swap crash cut: OLD remains installed; prepared NEW is fixed and
        // harmless. There is no transient/pid-only authority.
        assert_eq!(read_id(&installed), "OLD");
        assert_eq!(read_id(&fixed), "NEW");

        swap_fixed_candidate(&copied_candidate(&fixed), &installed).unwrap();
        // Post-swap crash cut: NEW is installed and OLD is already at the exact
        // fixed path recovery probes. No later rename is required.
        assert_eq!(read_id(&installed), "NEW");
        assert_eq!(read_id(&fixed), "OLD");
        restore_rollback(&fixed, &installed).unwrap();
        assert_eq!(read_id(&installed), "OLD");
        assert_eq!(read_id(&fixed), "NEW");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_fixed_exchange_leaves_old_install_untouched() {
        let root = swap_root("failed-fixed");
        let installed = root.join("installed.app");
        make_app(&installed, "OLD");
        let error = swap_fixed_candidate(&copied_candidate(&root.join("missing.app")), &installed)
            .unwrap_err();
        assert!(error.contains("changed into a symlink/non-directory"));
        assert_eq!(read_id(&installed), "OLD");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn fixed_candidate_symlink_is_never_swapped_into_canonical_install() {
        use std::os::unix::fs::symlink;

        let root = swap_root("fixed-symlink");
        let installed = root.join("installed.app");
        let target = root.join("signed-looking-target.app");
        let fixed = rollback_path(&installed);
        make_app(&installed, "OLD");
        make_app(&target, "TARGET");
        symlink(&target, &fixed).unwrap();

        let error = swap_fixed_candidate(&copied_candidate(&fixed), &installed).unwrap_err();
        assert!(error.contains("symlink/non-directory"));
        assert_eq!(read_id(&installed), "OLD");
        assert!(
            std::fs::symlink_metadata(&fixed)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn inverse_swap_rejects_substituted_symlink_rollback() {
        use std::os::unix::fs::symlink;

        let root = swap_root("rollback-symlink");
        let installed = root.join("installed.app");
        let target = root.join("target.app");
        let rollback = rollback_path(&installed);
        make_app(&installed, "NEW");
        make_app(&target, "OLD-TARGET");
        symlink(&target, &rollback).unwrap();

        let error = restore_rollback(&rollback, &installed).unwrap_err();
        assert!(error.contains("symlink/non-directory"));
        assert_eq!(read_id(&installed), "NEW");
        assert_eq!(read_id(&target), "OLD-TARGET");
        assert!(
            std::fs::symlink_metadata(&rollback)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn same_volume_pre_swap_failure_restores_published_stage() {
        let (s, root) = temp_staging();
        write_ready(&s, 54);
        make_app(&s.staged_app, "NEW");
        let installed = root.join("Applications/aterm.app");
        make_app(&installed, "OLD");
        let fixed = rollback_path(&installed);
        std::fs::rename(&s.staged_app, &fixed).unwrap();
        let prepared = PreparedSwapCandidate {
            fixed: fixed.clone(),
            moved_from_stage: true,
        };
        recover_prepared_candidate(&prepared, &s);
        assert_eq!(read_id(&s.staged_app), "NEW");
        assert!(!fixed.exists());
        assert!(s.ready.exists());
        assert_eq!(read_id(&installed), "OLD");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn old_process_confirmation_cannot_gc_a_newer_trial_or_its_receipt() {
        let (s, root) = temp_staging();
        let sentinel = boot_sentinel(&s);
        sentinel.arm(1000).unwrap();
        crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));
        crate::manifest::InstalledReceipt::record(
            &s.installed_receipt(),
            1000,
            "0123456789abcdef0123456789abcdef01234567",
            &"ab".repeat(32),
        )
        .unwrap();
        make_app(&s.staged_app, "SWAPPED-OUT-OLD");
        let installed = root.join("Applications").join("aterm.app");
        make_app(&installed, "RUNNING-NEW");
        let rollback = rollback_path(&installed);
        make_app(&rollback, "ROLLBACK");

        let _apply = FileLock::acquire(&s.apply_lock).unwrap();
        assert!(
            !confirm_health_under_apply_lock(&s, 999, None, Some(&installed)),
            "an overlapping old process has no authority over the newer trial"
        );
        assert_eq!(
            sentinel.read_state(),
            Some((1000, 0)),
            "non-matching confirm is a no-op"
        );
        assert!(s.trial().exists());
        assert!(s.installed_receipt().exists());
        assert!(s.staged_app.exists());
        assert!(rollback.exists());

        assert!(confirm_health_under_apply_lock_with_proof(
            &s,
            1000,
            &installed,
            || Ok(())
        ));
        assert_eq!(sentinel.read_state(), None, "matching confirm disarms");
        assert!(
            !s.trial().exists(),
            "health-only crash-loop state is cleared"
        );
        assert!(!s.staged_app.exists(), "matching owner may GC swap orphan");
        assert!(!rollback.exists(), "matching owner may GC rollback");
        assert!(
            s.installed_receipt().exists(),
            "exact installed proof survives confirm-before-reconcile ordering"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unresolved_installed_bundle_cannot_disarm_or_gc_trial() {
        let (s, root) = temp_staging();
        let sentinel = boot_sentinel(&s);
        sentinel.arm(1000).unwrap();
        crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));
        let _apply = FileLock::acquire(&s.apply_lock).unwrap();
        assert!(!confirm_health_under_apply_lock(&s, 1000, None, None));
        assert_eq!(sentinel.read_state(), Some((1000, 0)));
        assert!(s.trial().exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn active_trial_digest_rejects_same_build_commit_receipt_with_other_artifact() {
        let (s, root) = temp_staging();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));
        crate::manifest::InstalledReceipt::record(
            &s.installed_receipt(),
            1000,
            commit,
            &"cd".repeat(32),
        )
        .unwrap();
        assert!(
            !trial_authorizes_candidate(&s, 1000, commit),
            "same build+commit with a different DMG must not authorize health or recovery"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Tier-1 conformance for the native update disk machine. This test drives
    /// the real fixed-path exchange, inverse rollback, sentinel, exact receipt,
    /// startup authority reducer, and health cleanup; each physical observation
    /// is projected onto the same model that Tier-0 proves and bug-mutates.
    #[test]
    fn native_update_disk_transaction_model_conforms_to_real_swap_recovery_and_health_guards() {
        let model = aterm_spec::derive::native_update_disk_transaction_model();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let other_commit = "fedcba9876543210fedcba9876543210fedcba98";
        let digest = "ab".repeat(32);
        let build = 1000;

        // Startup authority must not return before the real boot-health lane has
        // run. A malformed expected-artifact tuple and a matching re-exec both
        // produce the observe verdict pre-observation; this is the regression's
        // production negative control.
        assert_eq!(
            startup_authority_decision(false, ReexecAuthority::Absent, false),
            StartupAuthorityDecision::ObserveBootHealth
        );
        assert_eq!(
            startup_authority_decision(false, ReexecAuthority::Matched, true),
            StartupAuthorityDecision::ObserveBootHealth
        );
        assert_eq!(
            startup_authority_decision(true, ReexecAuthority::Absent, false),
            StartupAuthorityDecision::ReturnMalformedExpected
        );
        assert_eq!(
            startup_authority_decision(true, ReexecAuthority::Matched, true),
            StartupAuthorityDecision::ReturnMatchedReexec
        );
        let mut malformed = model.init_state();
        for action in [
            "InheritMalformedAuthority",
            "ConsumeStartupAuthority",
            "ObserveBootHealth",
            "ReturnAfterObservedAuthority",
        ] {
            disk_model_step(&model, &mut malformed, action);
        }

        // The post-swap startup lane, without the retired v0.52 synthesis.
        //
        // That branch recovered a receipt for a machine whose updater deleted
        // `ready.toml` before minting one; it and its model action are gone with
        // the rest of the two-component lineage. What survives is the MODERN
        // recovery — `ready.toml` is still present, so the receipt is rebuilt
        // from evidence the current updater actually writes — and this binds it
        // to the model, then proves each corrupted disk fact disables it.
        let mut modern_state = model.init_state();
        for action in [
            "EnterLegacyPostSwapExact",
            "SupplyModernReadyRecovery",
            "ConsumeStartupAuthority",
            "RecoverModernReceiptFromReady",
        ] {
            disk_model_step(&model, &mut modern_state, action);
        }
        assert_eq!(modern_state["receipt_exact"], 1);
        assert_eq!(modern_state["modern_receipt_recovered"], 1);

        // Each corrupted disk fact must leave its REFUSAL enabled and preserve
        // recovery authority — trial armed, rollback fixed, no receipt minted —
        // so a later launch can still act on unchanged evidence. A corruption
        // that silently disabled both the recovery and its refusal would strand
        // the machine with no path forward, which is what this excludes.
        //
        // `legacy_variant` is the model's mutex: supplying modern ready evidence
        // and corrupting a fact are mutually exclusive scenarios, so these runs
        // deliberately do NOT also supply ready — with `ready_present == 0`,
        // `RecoverModernReceiptFromReady` is disabled by its own guard.
        for (corrupt, refuse) in [
            ("CorruptLegacySentinel", "RefuseLegacySentinelMismatch"),
            (
                "CorruptLegacyCurrentBuild",
                "RefuseLegacyCurrentBuildMismatch",
            ),
            (
                "CorruptLegacyCurrentCommit",
                "RefuseLegacyCurrentCommitMismatch",
            ),
            ("CorruptLegacyTrialBuild", "RefuseLegacyTrialBuildMismatch"),
            (
                "CorruptLegacyTrialDigest",
                "RefuseLegacyTrialDigestMismatch",
            ),
            ("CorruptLegacyRollback", "RefuseLegacyRollbackMismatch"),
        ] {
            let mut rejected_state = model.init_state();
            for action in [
                "EnterLegacyPostSwapExact",
                corrupt,
                "ConsumeStartupAuthority",
            ] {
                disk_model_step(&model, &mut rejected_state, action);
            }
            assert!(
                model
                    .successors("RecoverModernReceiptFromReady", &rejected_state)
                    .is_empty(),
                "{corrupt} left receipt recovery model-enabled"
            );
            disk_model_step(&model, &mut rejected_state, refuse);
            assert_eq!(rejected_state["trial"], 1);
            assert_eq!(rejected_state["fixed"], 1);
            assert_eq!(rejected_state["receipt"], 0);
        }

        // OLD authority is build+commit, not build-only. Likewise NEW handoff
        // authority is the exact marker build+commit+digest tuple.
        assert!(identity_matches_running(
            build,
            commit,
            build,
            Some("0123456789ab")
        ));
        assert!(
            !identity_matches_running(build, other_commit, build, Some("0123456789ab")),
            "negative control: build-only comparison would authorize the wrong OLD"
        );
        let mut wrong_old = disk_model_ready(&model);
        disk_model_step(&model, &mut wrong_old, "CorruptOldCommit");
        assert!(
            model.successors("PrepareFixedNew", &wrong_old).is_empty(),
            "healthy model must reject the same build with the wrong commit"
        );
        let ready = Ready {
            build_number: build,
            version: "1.0.1000".into(),
            commit: Some(commit.into()),
            dmg_sha256: digest.clone(),
            team_id: "T".into(),
            staged_at: String::new(),
            changelog: None,
        };
        let exact = ExpectedArtifact {
            build,
            commit: commit.into(),
            dmg_sha256: digest.clone(),
        };
        assert!(ready_matches_expected(&ready, &exact));
        let mut wrong_new = exact.clone();
        wrong_new.commit = other_commit.into();
        assert!(!ready_matches_expected(&ready, &wrong_new));

        // A genuine failed fixed exchange plus shipping disarm/recovery leaves
        // canonical OLD intact and no partial trial authority.
        let (failed_staging, failed_root) = temp_staging();
        write_ready(&failed_staging, build);
        let failed_installed = failed_root.join("Applications/aterm.app");
        make_app(&failed_installed, "OLD");
        let failed_ready = Ready::read(&failed_staging.ready).unwrap();
        let failed_sentinel = prepare_trial(&failed_staging, &failed_ready).unwrap();
        let mut failed_state = disk_model_ready(&model);
        disk_model_step(&model, &mut failed_state, "RemovePreviousReceipt");
        disk_model_step(&model, &mut failed_state, "PrepareFixedNew");
        disk_model_step(&model, &mut failed_state, "ArmExactTrial");
        let missing = rollback_path(&failed_installed);
        assert!(swap_fixed_candidate(&copied_candidate(&missing), &failed_installed).is_err());
        failed_sentinel.confirm().unwrap();
        crate::manifest::FailedMark::clear(&failed_staging.trial());
        disk_model_step(&model, &mut failed_state, "SwapFailsAndDisarms");
        assert_real_disk_projection(
            &failed_state,
            &failed_staging,
            &failed_installed,
            build,
            commit,
            &digest,
        );
        let _ = std::fs::remove_dir_all(failed_root);

        // Successful fixed-path transaction and exact receipt.
        let (staging, root) = temp_staging();
        write_ready(&staging, build);
        make_app(&staging.staged_app, "OLD");
        let installed = root.join("Applications/aterm.app");
        let fixed = rollback_path(&installed);
        make_app(&installed, "OLD");
        make_app(&fixed, "NEW");
        let mut state = disk_model_ready(&model);
        disk_model_step(&model, &mut state, "RemovePreviousReceipt");
        disk_model_step(&model, &mut state, "PrepareFixedNew");
        assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);

        let ready = Ready::read(&staging.ready).unwrap();
        let _sentinel = prepare_trial(&staging, &ready).unwrap();
        disk_model_step(&model, &mut state, "ArmExactTrial");
        assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);

        swap_fixed_candidate(&copied_candidate(&fixed), &installed).unwrap();
        disk_model_step(&model, &mut state, "AtomicSwap");
        assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);

        crate::manifest::InstalledReceipt::record(
            &staging.installed_receipt(),
            build,
            commit,
            &digest,
        )
        .unwrap();
        std::fs::remove_file(&staging.ready).unwrap();
        disk_model_step(&model, &mut state, "RecordExactReceipt");
        disk_model_step(&model, &mut state, "VerifyExactRollback");
        assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);

        // The GUI first-present edge precedes the worker proof. A failed proof is
        // a strict no-op on every recovery artifact, then a retry may prove,
        // disarm, and garbage-collect in that order.
        disk_model_step(&model, &mut state, "PresentInstalledUi");
        let _apply = FileLock::acquire(&staging.apply_lock).unwrap();
        assert!(!confirm_health_under_apply_lock_with_proof(
            &staging,
            build,
            &installed,
            || Err("injected health-proof failure".to_string()),
        ));
        disk_model_step(&model, &mut state, "HealthProofFails");
        assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);
        assert!(staging.staged_app.exists() && fixed.exists());

        disk_model_step(&model, &mut state, "RetryHealthProof");
        assert!(confirm_health_under_apply_lock_with_proof(
            &staging,
            build,
            &installed,
            || Ok(()),
        ));
        for action in [
            "ProveInstalledHealth",
            "DisarmTrial",
            "GarbageCollectRollback",
        ] {
            disk_model_step(&model, &mut state, action);
        }
        assert_real_disk_projection(&state, &staging, &installed, build, commit, &digest);
        assert!(!staging.staged_app.exists() && !fixed.exists());
        drop(_apply);
        let _ = std::fs::remove_dir_all(root);

        // Exec-failure inverse rollback: a rejected/missing rollback input leaves
        // NEW+exact OLD untouched; the genuine fixed path then restores OLD and
        // successful disarm permits cleanup.
        let (rollback_staging, rollback_root) = temp_staging();
        write_ready(&rollback_staging, build);
        let rollback_installed = rollback_root.join("Applications/aterm.app");
        let rollback_fixed = rollback_path(&rollback_installed);
        make_app(&rollback_installed, "OLD");
        make_app(&rollback_fixed, "NEW");
        let rollback_ready = Ready::read(&rollback_staging.ready).unwrap();
        let rollback_sentinel = prepare_trial(&rollback_staging, &rollback_ready).unwrap();
        let mut rollback_state = disk_model_ready(&model);
        disk_model_step(&model, &mut rollback_state, "RemovePreviousReceipt");
        for action in ["PrepareFixedNew", "ArmExactTrial"] {
            disk_model_step(&model, &mut rollback_state, action);
        }
        swap_fixed_candidate(&copied_candidate(&rollback_fixed), &rollback_installed).unwrap();
        disk_model_step(&model, &mut rollback_state, "AtomicSwap");
        crate::manifest::InstalledReceipt::record(
            &rollback_staging.installed_receipt(),
            build,
            commit,
            &digest,
        )
        .unwrap();
        for action in ["RecordExactReceipt", "VerifyExactRollback", "ExecFails"] {
            disk_model_step(&model, &mut rollback_state, action);
        }
        assert!(
            restore_rollback(
                &rollback_root.join("missing-rollback.app"),
                &rollback_installed,
            )
            .is_err()
        );
        disk_model_step(&model, &mut rollback_state, "RestoreExactOldFails");
        assert_real_disk_projection(
            &rollback_state,
            &rollback_staging,
            &rollback_installed,
            build,
            commit,
            &digest,
        );
        restore_rollback(&rollback_fixed, &rollback_installed).unwrap();
        disk_model_step(&model, &mut rollback_state, "RestoreExactOld");
        rollback_sentinel.confirm().unwrap();
        crate::manifest::FailedMark::clear(&rollback_staging.trial());
        std::fs::remove_dir_all(&rollback_fixed).unwrap();
        restore_installed_receipt(&rollback_staging, None).unwrap();
        disk_model_step(
            &model,
            &mut rollback_state,
            "DisarmRestoredTrialAndClearUnboundReceipt",
        );
        assert_real_disk_projection(
            &rollback_state,
            &rollback_staging,
            &rollback_installed,
            build,
            commit,
            &digest,
        );
        let _ = std::fs::remove_dir_all(rollback_root);
    }

    #[test]
    fn format_rfc3339_matches_known_instants() {
        // Epoch.
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        // A midnight on a clean date: 2025-07-01T00:00:00Z.
        assert_eq!(format_rfc3339(1_751_328_000), "2025-07-01T00:00:00Z");
        // A leap day WITH a time-of-day: 2024-02-29T12:34:56Z. Exercises both the
        // civil-from-days leap handling and the h/m/s split.
        assert_eq!(format_rfc3339(1_709_210_096), "2024-02-29T12:34:56Z");
        // One second before the epoch of the last known instant, to catch an
        // off-by-one in the day/second boundary: 2024-02-28T23:59:59Z.
        assert_eq!(format_rfc3339(1_709_164_799), "2024-02-28T23:59:59Z");
    }
}
