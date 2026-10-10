// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Staging (unpack → verify → publish) and application (lock → re-verify → atomic
//! swap → re-exec) of an update. The ordering is the security-critical part; see
//! the per-step comments and the crate-level trust model.
//!
//! The same signed bundle unpacks two ways: [`stage_from_zip`] (`ditto -x -k`,
//! preferred) and [`stage_from_dmg`] (`hdiutil attach`, for releases without a
//! zip). Both go through `verify_and_publish_incoming`, so the container never
//! changes what is proved. The zip path reads the archive's central directory and
//! refuses one that would fill the volume before `ditto` runs
//! ([`checked_zip_extraction_claim`]).

use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_update_core::{FileLock, LaunchName, Sentinel, ensure_private_dir, same_volume};

use crate::manifest::{Manifest, Ready};
use crate::paths::Staging;
use crate::sys::rename_swap;
use crate::{ApplyOutcome, bundle, verify};

/// A freshly swapped build is auto-reverted after this many consecutive launches
/// that find the boot sentinel still unconfirmed (a crash loop). `arm` records 0,
/// the first launch observes 1, and the revert fires at this count. A healthy build
/// clears the sentinel via [`confirm_boot_health`] on its first boot.
pub(crate) const MAX_BOOT_ATTEMPTS: u32 = 3;

/// How long a launch waits for the apply lock before starting on the installed
/// build. Short: every legitimate holder returns in milliseconds or is itself
/// bounded, so reaching this means the holder is wedged, and a wedged holder must
/// not stop a terminal from opening.
pub(crate) const APPLY_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long the background lane (the checker's stage and publish, and the floor
/// ratchet under them) waits for the stage, apply or floor lock before giving the
/// cycle up, so a stopped or hung holder in another aterm process cannot park the
/// checker thread. Generous, because nothing here is on a launch: every legitimate
/// apply/floor lock holder is bounded well inside it, and the stage lock is held
/// across a sibling's download, which the next cycle finds staged. A timeout is
/// that cycle's ordinary failure, retried on the loop's backoff.
pub(crate) const BACKGROUND_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Ceiling on the cross-volume `ditto` of the candidate bundle. Generous: a
/// several-hundred-megabyte bundle onto a slow external disk is legitimate; this
/// bounds only a volume that went away mid-copy.
const CROSS_VOLUME_COPY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Each copy owns a fresh private directory on the destination volume, so a helper
/// that outlives its deadline can write only here, never into a later attempt, the
/// published stage, or the fixed rollback path. Unconfirmed attempts keep their
/// directory; its random name is never reused or swept as `zx-*` scratch.
struct IsolatedCopy {
    root: PathBuf,
    payload: PathBuf,
}

impl IsolatedCopy {
    fn create(destination: &Path) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt;
        let parent = destination
            .parent()
            .ok_or_else(|| "copy destination has no parent directory".to_string())?;
        let nonce = random_nonce().ok_or_else(|| "copy attempt nonce unavailable".to_string())?;
        let root = parent.join(format!(".aterm-copy-{nonce}"));
        // Exclusive creation rejects a collision; never adopt an existing attempt.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|error| format!("create isolated copy directory: {error}"))?;
        if let Err(error) = std::fs::write(root.join("copy-layout"), "aterm-copy-v1\n") {
            let _ = std::fs::remove_dir(&root);
            return Err(format!("record isolated copy layout: {error}"));
        }
        // Who started this attempt, for the reaper's lapsed rule
        // ([`reap_abandoned_copy_attempts`]). Best effort: an attempt without it
        // reads as a live caller's, the conservative reading.
        let _ = std::fs::write(root.join(COPY_OWNER), format!("{}\n", std::process::id()));
        Ok(Self {
            payload: root.join("payload"),
            root,
        })
    }
}

/// The file in a copy attempt naming the pid of the process that started it
/// ([`IsolatedCopy::create`]).
const COPY_OWNER: &str = "owner";

/// How long a copy attempt must exist before the reaper's lapsed rule may reclaim
/// it ([`reap_abandoned_copy_attempts`]): a day, against copy ceilings of minutes
/// ([`STAGE_UNPACK_TIMEOUT`], [`CROSS_VOLUME_COPY_TIMEOUT`]).
const STALE_COPY_ATTEMPT_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Whether the process an attempt's [`COPY_OWNER`] names is gone. Only a definite
/// `ESRCH` is gone: a missing or unreadable record, or a pid that exists (possibly
/// reused, the conservative direction), is a live caller.
fn copy_owner_gone(root: &Path) -> bool {
    let Some(pid) = crate::read_ledger_text(&root.join(COPY_OWNER))
        .and_then(|text| text.trim().parse::<i32>().ok())
        .filter(|pid| *pid > 0)
    else {
        return false;
    };
    // SAFETY: signal 0 delivers nothing; it only asks whether `pid` exists.
    let asked = unsafe { libc::kill(pid, 0) };
    asked != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// Whether an attempt has existed past [`STALE_COPY_ATTEMPT_AFTER`], by its
/// `copy-layout` marker (written once at creation). A stamp from the future (a
/// clock stepped back) is young.
fn copy_attempt_lapsed(root: &Path) -> bool {
    std::fs::symlink_metadata(root.join("copy-layout"))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|written| std::time::SystemTime::now().duration_since(written).ok())
        .is_some_and(|age| age >= STALE_COPY_ATTEMPT_AFTER)
}

/// Reclaims copy attempts no writer and no caller can still use: those past
/// [`STALE_COPY_ATTEMPT_AFTER`] that their caller gave up or outlived. Completion
/// alone is never enough: a live caller may have finished copying but not promoted
/// yet.
///
/// Lapsed: an attempt past [`STALE_COPY_ATTEMPT_AFTER`] that its caller abandoned,
/// or whose caller is gone ([`copy_owner_gone`]). This covers a caller killed
/// mid-copy (never marks abandoned) and a writer not confirmed stopped at its
/// deadline; the age stands in for the writer's end. A live caller's attempt it has
/// not given up is never reclaimed, however old.
///
/// The background checker owns this sweep; copy allocation creates only its own
/// directory and marker.
pub(crate) fn reap_abandoned_copy_attempts(parent: &Path) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    // SAFETY: geteuid has no pointer arguments or preconditions.
    let owner = unsafe { libc::geteuid() };
    // Bound expensive inspection/reclamation, not unrelated directory names.
    // This is best effort: pending attempts can consume the candidate budget.
    for entry in entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".aterm-copy-")
        })
        .take(128)
    {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(nonce) = name.strip_prefix(".aterm-copy-") else {
            continue;
        };
        if !aterm_codec::hex::is_hex(nonce, 32) {
            continue;
        }
        let root = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&root) else {
            continue;
        };
        if !metadata.is_dir()
            || metadata.uid() != owner
            || metadata.permissions().mode() & 0o077 != 0
            || crate::read_ledger_text(&root.join("copy-layout")).as_deref()
                != Some("aterm-copy-v1\n")
        {
            continue;
        }
        let abandoned = crate::read_ledger_text(&root.join("abandoned")).as_deref() == Some("1\n");
        if copy_attempt_lapsed(&root) && (abandoned || copy_owner_gone(&root)) {
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

/// The shipping promotion boundary, shared by DMG extraction, ZIP extraction,
/// and cross-volume apply. `run` must report success only after the copy exits.
fn copy_into_isolated_destination(
    args: &[std::ffi::OsString],
    what: &str,
    run: impl FnOnce(
        &[std::ffi::OsString],
        &Path,
    ) -> Result<(std::process::ExitStatus, String), crate::verify::HelperFailure>,
) -> Result<(std::process::ExitStatus, String), String> {
    let (destination, prefix) = args
        .split_last()
        .ok_or_else(|| format!("{what}: missing copy destination"))?;
    let destination = Path::new(destination);
    let attempt = IsolatedCopy::create(destination)?;
    let mut isolated_args = prefix.to_vec();
    isolated_args.push(attempt.payload.as_os_str().to_os_string());
    let completed = run(&isolated_args, &attempt.root);
    let result = match completed {
        Err(failure) if failure.writer_stopped => Err(failure.message),
        Err(failure) => {
            // A writer not confirmed stopped may still be copying: keep its
            // attempt, marked abandoned, for the reaper's lapsed rule. A late
            // completion licenses cleanup only, never promotion.
            let marker_error = std::fs::write(attempt.root.join("abandoned"), "1\n")
                .err()
                .map(|error| format!("; could not mark abandoned copy: {error}"))
                .unwrap_or_default();
            return Err(format!(
                "{}; unconfirmed copy remains isolated at {}{marker_error}",
                failure.message,
                attempt.root.display()
            ));
        }
        Ok((status, stderr)) if status.success() => {
            if !is_non_symlink_dir(&attempt.payload) {
                Err(format!("{what}: copied payload is not a real directory"))
            } else {
                std::fs::rename(&attempt.payload, destination)
                    .map(|()| (status, stderr))
                    .map_err(|error| format!("{what}: promote completed copy: {error}"))
            }
        }
        Ok(completed) => Ok(completed),
    };
    // A terminal copy owns no live writer.
    let _ = std::fs::remove_dir_all(&attempt.root);
    result
}

/// Run `/usr/bin/ditto` into an isolated attempt, bounded by `limit`, then promote
/// only its completed output.
fn ditto_bounded(
    args: &[std::ffi::OsString],
    what: &str,
    limit: std::time::Duration,
) -> Result<(std::process::ExitStatus, String), String> {
    copy_into_isolated_destination(args, what, |args, _| {
        crate::verify::status_bounded_with_stderr_observed(
            aterm_uds::spawnfd::command("/usr/bin/ditto").args(args),
            what,
            limit,
        )
    })
}

/// Ceiling on one staging unpack (the DMG copy or the zip extract). A release is
/// ~100 MB; minutes means a wedged volume, not a slow one.
const STAGE_UNPACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Slack the free-space check demands over the declared size: filesystem
/// metadata, the extract's own scratch dir, and the verify step's writes.
pub(crate) const STAGE_FREE_SPACE_HEADROOM: u64 = 64 * 1024 * 1024;

/// The one action a full disk's refusal names — the same words for the download,
/// the unpack and the free-space check. A disk that STAYS full gets the re-download
/// memo (scheduled checks skip the release for 15 min up to 4 h); a check a person
/// asks for passes it (`github::stage_backoff`), so that is the action named.
pub(crate) const FREE_SPACE_REMEDY: &str =
    "free some space and check for updates, which retries it at once";

/// A `ditto` failure that says WHY, from the tail of its stderr.
fn ditto_failure(what: &str, status: std::process::ExitStatus, stderr: &str) -> String {
    match crate::verify::last_stderr_line(stderr) {
        Some(line) => format!("{what} ({status}): {line}"),
        None => format!("{what} ({status})"),
    }
}

/// A staging unpack's `ditto` failure ([`ditto_failure`]). A volume that filled
/// while it copied — [`refuse_without_room`] passed, and something else wrote in
/// the meantime — is a PASSING refusal like that one's (design law L4), worded as
/// that one is: the volume holding `dir`, what it has free, and the one action.
/// It is recognised by the `strerror` words of `ENOSPC` or `EDQUOT` on any line of
/// its stderr (a zip extract that hit the full disk goes on to print "Couldn't
/// read pkzip signature." LAST, measured on a full volume, which blames the
/// archive); `ditto`'s own line goes to the log. Anything else keeps the verdict
/// it always had.
pub(crate) fn unpack_failure(
    dir: &Path,
    what: &str,
    status: std::process::ExitStatus,
    stderr: &str,
) -> String {
    let full = stderr.lines().rev().map(str::trim).find(|line| {
        ["No space left on device", "Disc quota exceeded"]
            .iter()
            .any(|words| line.contains(words))
    });
    let Some(line) = full else {
        return ditto_failure(what, status, stderr);
    };
    crate::warn(&format!("{what} ({status}): {line}"));
    // A quota stop does not fill the disk, and `statvfs`'s free figure need not
    // reflect the quota, so its words name the quota and quote no figure.
    if line.contains("Disc quota exceeded") {
        return crate::verify::mark_passing(format!(
            "this user's disk quota ran out while unpacking the update into {}; \
             {FREE_SPACE_REMEDY}",
            dir.display()
        ));
    }
    let room = free_bytes_at(dir).map_or_else(String::new, |free| {
        format!(" ({} MiB free there)", free.div_ceil(1024 * 1024))
    });
    crate::verify::mark_passing(format!(
        "the disk holding {} filled while unpacking the update{room}; {FREE_SPACE_REMEDY}",
        dir.display()
    ))
}

/// Bytes still available to this user on the volume holding `path`, or `None`
/// when the volume will not say (an unmounted or foreign path).
pub(crate) fn free_bytes_at(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` is zeroed storage the call fills; `c_path` outlives it.
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &raw mut vfs) };
    if rc != 0 {
        return None;
    }
    // `f_bavail` is what an unprivileged writer may take; `f_frsize` is the unit it
    // is counted in. Widen the available block count before multiplication.
    Some(u64::from(vfs.f_bavail) * vfs.f_frsize)
}

/// Refuse to unpack `needed` bytes into `dir` when the volume cannot hold them plus
/// [`STAGE_FREE_SPACE_HEADROOM`]. The message names both numbers and
/// the volume, so a full disk reads as a full disk in the ledger and the pull-down
/// instead of "ditto zip extract failed (exit status: 1)" — and no container is
/// re-unpacked (or, on the zip lane, extracted at all) to learn what `statvfs`
/// already knows. Not a size ceiling: `checked_zip_extraction_claim` bounds the
/// fleet-wide cost; this bounds THIS machine's. A volume that will not report its
/// free space is let through — `ditto` then decides, with its stderr kept.
///
/// A PASSING refusal (design law L4, as the install-location probe's `ENOSPC` is):
/// a full disk says nothing about the release, so the stage lane books it as a
/// moment (`github::book_stage_failure`): the first costs nothing, a disk that stays
/// full is re-downloaded on the moment's gentler schedule, and a check a person asks
/// for passes that memo ([`FREE_SPACE_REMEDY`]). Booked as a verdict it opened the
/// 15 min to 24 h backoff, and a disk freed after four full checks still skipped the
/// release for a day.
pub(crate) fn refuse_without_room(dir: &Path, needed: u64, what: &str) -> Result<(), String> {
    let Some(free) = free_bytes_at(dir) else {
        return Ok(());
    };
    let wanted = needed.saturating_add(STAGE_FREE_SPACE_HEADROOM);
    if free >= wanted {
        return Ok(());
    }
    let mib = |bytes: u64| bytes.div_ceil(1024 * 1024);
    Err(crate::verify::mark_passing(format!(
        "not enough free space to unpack {what}: {} MiB free on the volume holding {}, \
         {} MiB needed ({} MiB declared plus {} MiB headroom); {FREE_SPACE_REMEDY}",
        mib(free),
        dir.display(),
        mib(wanted),
        mib(needed),
        mib(STAGE_FREE_SPACE_HEADROOM)
    )))
}

/// Bytes a copy of the tree at `root` writes: every regular file's length, links
/// and directories counting nothing. Symlinks are not followed (the mounted image
/// controls them).
fn tree_bytes(root: &Path) -> u64 {
    let mut total = 0_u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

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
    // health verification helpers can launch codesign/spctl children. Read and clear
    // are one critical section (`aterm_log::env::take`), so a one-shot re-exec
    // authority can never be observed twice.
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
    // and cleared in one critical section (`aterm_log::env::take`), so handoff
    // authority is consumed exactly once and helpers and user shells never inherit it.
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
///
/// The exec is `exec_keeping_sigpipe`: `Command::exec` resets SIGPIPE to its default and
/// leaves it there when the exec fails, and the rollback arm that follows a failed exec
/// carries on in this process, which would then die on its next write to a peer that has
/// gone.
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
    let error = crate::exec_plain::exec_keeping_sigpipe(command);
    for fd in cleared {
        rearm_or_close_handoff_fd(fd);
    }
    error
}

// The relaunch argv filter lives in `crate::relaunch` — every platform
// relaunches aterm, so it cannot live inside this macOS-only module.
use crate::relaunch::window_successor_args;

/// The post-swap re-exec command: the new binary at the canonical path, the
/// forwarded argv, the single-use re-exec nonce, and the caller's handoff authority
/// variables restored onto the exec image only. The GUI's prearm removed those
/// from the environment (so no codesign/PlistBuddy/spctl helper could observe
/// them), but the successor re-runs prearm and must re-validate the inherited
/// handoff; without them it classifies the handoff malformed and exits.
///
/// `ATERM_UPDATED_FROM` names the build this image was: the successor's `main`
/// reads it for the post-update notice and clears it.
fn boot_reexec_command(
    new_exe: &std::path::Path,
    reexec_value: &str,
    updated_from: u64,
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Command {
    let mut reexec = Command::new(new_exe);
    reexec
        // The swap re-exec targets the canonical Mach-O, losing any argv0 alias, and
        // only runs from the window entry, so pin the mode for the one-binary router
        // before the forwarded args (the router's scan stops at the first
        // -e/--command/--, so an appended flag would be invisible). A headless
        // instance's `--headless` rides the forwarded args, which are stripped of
        // earlier swaps' pins, so exactly one `--window` survives any number of
        // updates.
        .args(window_successor_args(std::env::args_os().skip(1)))
        .env("ATERM_UPDATE_REEXEC", reexec_value)
        .env("ATERM_UPDATED_FROM", updated_from.to_string());
    for (key, value) in handoff_env {
        reexec.env(key, value);
    }
    reexec
}

/// The boot-health sentinel for this install: a small file under the private
/// staging root, so the pre-swap old process and the post-swap new process resolve
/// the same path.
fn boot_sentinel(staging: &Staging) -> Sentinel {
    Sentinel::new(staging.root.join("boot.sentinel"))
}

/// Consecutive launches that observed a boot sentinel armed for a build this
/// process is not running and could not recover.
///
/// Its own file, not [`Sentinel::observe_launch`] on the boot sentinel: that count
/// belongs to the trialed build, and two installs (e.g. `/Applications` and
/// `~/Applications`) share one staging root, so counting another build's launches
/// there could crash-loop-revert a build that never crashed. Reuses the `Sentinel`
/// type for its atomic write shape: `arm` records `"<build> 0"`, `observe_launch`
/// counts only while the recorded build matches, `confirm` deletes it.
fn foreign_trial_counter(staging: &Staging) -> Sentinel {
    Sentinel::new(foreign_trial_path(staging))
}

/// The counter's path, needed on its own to compare its mtime against the boot
/// sentinel's — see [`escape_wedged_foreign_trial`].
fn foreign_trial_path(staging: &Staging) -> PathBuf {
    staging.root.join("foreign-trial")
}

/// How long another PRESENT install's trial must have sat untouched before a
/// sibling's budgeted escape may disarm it (see `escape_wedged_foreign_trial`).
const FOREIGN_TRIAL_OWNER_IDLE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Last-modified time of `path`, or `None` when missing or unstat-able. Compares
/// two files in the same directory (a clock jump moves both or neither), and, only
/// for the present-owner idle test in `escape_wedged_foreign_trial`, against the
/// wall clock, where a future stamp reads as "not idle", the fail-safe direction.
fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Launches of the trialed build closer together than this count as one. Parallel
/// starts (headless engines from agent tooling, `open -n -a aterm`, a launcher
/// script) are not crashes; a real crash loop's relaunches each pass dyld and GPU
/// init, so they are seconds apart and still reach the budget.
const BOOT_LAUNCH_BURST_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether this launch is part of a burst the sentinel already counted: an observed
/// trial (attempts > 0; the arm itself is never a burst, so the first launch always
/// counts) whose file was written inside [`BOOT_LAUNCH_BURST_WINDOW`]. Pure.
fn launch_is_burst(attempts: u32, sentinel_age: Option<std::time::Duration>) -> bool {
    attempts > 0 && sentinel_age.is_some_and(|age| age < BOOT_LAUNCH_BURST_WINDOW)
}

/// A process that launched the trialed build, named by pid AND start time so a
/// reused pid never reads as the launch it replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrialLauncher {
    pid: u32,
    started: (i64, i64),
}

impl TrialLauncher {
    /// `pid` as it runs now, or `None` when it is gone (or a zombie).
    fn of(pid: u32) -> Option<Self> {
        process_start(pid).map(|started| Self { pid, started })
    }

    /// This launch's process, or `None` when the kernel will not say.
    fn this_process() -> Option<Self> {
        Self::of(std::process::id())
    }

    /// Whether the process that made this launch is still running — the same pid
    /// with the same start time, and not a zombie.
    fn is_alive(&self) -> bool {
        Self::of(self.pid) == Some(*self)
    }

    /// The name the sentinel counts this launch under ([`Sentinel::observe_launch_by`]).
    fn name(self) -> LaunchName {
        LaunchName {
            pid: self.pid,
            started: self.started,
        }
    }
}

/// `pid`'s start time from the kernel, or `None` when there is no such process,
/// it is a zombie, or the answer is about some other pid.
fn process_start(pid: u32) -> Option<(i64, i64)> {
    /// `SZOMB` in `<sys/proc.h>`: exited, not yet reaped — a death, for this rule.
    const SZOMB: u32 = 5;
    let raw_pid = i32::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` points at `size` writable bytes of exactly the structure
    // PROC_PIDTBSDINFO fills; libproc returns the number of bytes it wrote.
    let read = unsafe {
        libc::proc_pidinfo(
            raw_pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: the exact-size success above initialized the whole record.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid || info.pbi_status == SZOMB {
        return None;
    }
    Some((
        i64::try_from(info.pbi_start_tvsec).ok()?,
        i64::try_from(info.pbi_start_tvusec).ok()?,
    ))
}

/// Where the processes that launched the armed trial are recorded, beside the
/// sentinel: `<build>` on the first line, then `<pid> <seconds> <micros>` per launch,
/// followed by ` headless` for a launch that opened no window. A separate file on
/// purpose: the sentinel's own `"<build> <attempts>"` line is what every build reads
/// and writes, and an older one keeps doing exactly that. An older build reads the
/// first three fields of a line and ignores the rest; a line without the fourth (an
/// older build's) reads as a window's launch, the meaning every launch had before.
fn trial_launchers_path(staging: &Staging) -> PathBuf {
    staging.root.join("boot.sentinel.launchers")
}

/// The most launches the record keeps; the oldest go first.
const MAX_TRIAL_LAUNCHERS: usize = 32;

/// The fourth field of a launcher's line that marks a launch that opened no window.
const HEADLESS_LAUNCHER_MARK: &str = "headless";

/// A launch of the armed trial as the record keeps it: the process, and what it can
/// prove about the trial ([`crate::TrialWitness`]) — which decides whose overlap it
/// exempts ([`count_trial_launch`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RecordedLauncher {
    launcher: TrialLauncher,
    witness: crate::TrialWitness,
}

fn read_trial_launchers(staging: &Staging, build: u64) -> Vec<RecordedLauncher> {
    let Some(text) = crate::read_ledger_text(&trial_launchers_path(staging)) else {
        return Vec::new();
    };
    let mut lines = text.lines();
    if lines
        .next()
        .and_then(|line| line.trim().parse::<u64>().ok())
        != Some(build)
    {
        return Vec::new();
    }
    lines
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let launcher = TrialLauncher {
                pid: fields.next()?.parse().ok()?,
                started: (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?),
            };
            let witness = if fields.next() == Some(HEADLESS_LAUNCHER_MARK) {
                crate::TrialWitness::Headless
            } else {
                crate::TrialWitness::Window
            };
            Some(RecordedLauncher { launcher, witness })
        })
        .collect()
}

fn write_trial_launchers(staging: &Staging, build: u64, launchers: &[RecordedLauncher]) {
    let skip = launchers.len().saturating_sub(MAX_TRIAL_LAUNCHERS);
    let mut text = format!("{build}\n");
    for RecordedLauncher { launcher, witness } in &launchers[skip..] {
        let mark = match witness {
            crate::TrialWitness::Window => String::new(),
            crate::TrialWitness::Headless => format!(" {HEADLESS_LAUNCHER_MARK}"),
        };
        text.push_str(&format!(
            "{} {} {}{mark}\n",
            launcher.pid, launcher.started.0, launcher.started.1
        ));
    }
    if let Err(error) = write_private_file(&trial_launchers_path(staging), text.as_bytes()) {
        crate::warn(&format!(
            "could not record the launches of build {build}'s trial: {error}; overlapping \
             launches of it may be counted as relaunches"
        ));
    }
}

/// Where a trial records that a WINDOW has launched it:
/// `<build> <artifact digest> <unix seconds of the latest window launch>`,
/// written under the apply lock by every launch that opens windows.
///
/// KEYED BY THE TRIAL'S IDENTITY, never by the build number alone: a record reads
/// as the ARMED trial's only while the sentinel is armed for that build AND the
/// trial marker (`trial.toml`) names the same build and digest. Every end of a
/// trial — a confirm, a revert, any disarm — leaves the record naming a trial that
/// is no longer armed, so it reads as no record whether or not it was removed; and
/// a later trial of the same build number under another digest (a re-publish,
/// armed by an older image that does not know this file) is not mistaken for the
/// one a window launched. The file is also removed where a trial is armed, and at
/// every end of a trial this crate owns ([`clear_trial_identity`]).
fn trial_window_launch_path(staging: &Staging) -> PathBuf {
    staging.root.join("boot.sentinel.windowed")
}

/// How long a trial a WINDOW has launched waits for a window to settle it — confirm
/// it at a first frame, or count the launches that crash before one — before a
/// headless launch counts toward it and confirms it again (law L5: no wait without
/// a deadline). Measured from the LATEST window launch of
/// the trial, and longer than any gap the window lane's own schedule leaves between
/// two of its launches (its structural re-sample waits a day), so a window lane
/// still at work always launches again inside it. What it bounds is a machine
/// where windows stopped launching the build — a person who quit the app and runs
/// only harnesses — whose trial would otherwise stay armed for ever, and with it
/// every later update. Said on each such headless launch, with this deadline, where
/// `update status` reads it.
pub(crate) const WINDOW_TRIAL_HEADLESS_WAIT: std::time::Duration =
    std::time::Duration::from_secs(48 * 60 * 60);

/// What the window-launch record says about the armed trial of `build`, at `now`
/// (unix seconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowLaunch {
    /// No window has launched the armed trial: a headless launch counts toward it
    /// and settles it, as it always did — the headless self-update lane.
    None,
    /// A window has, within [`WINDOW_TRIAL_HEADLESS_WAIT`]: a headless launch
    /// neither counts nor settles it, until `until` (unix seconds).
    Waiting { until: u64 },
    /// A window has, but none for [`WINDOW_TRIAL_HEADLESS_WAIT`]: headless launches
    /// count and settle it again.
    Lapsed,
}

fn trial_window_launch(staging: &Staging, build: u64, now: u64) -> WindowLaunch {
    let Some(text) = crate::read_ledger_text(&trial_window_launch_path(staging)) else {
        return WindowLaunch::None;
    };
    let mut fields = text.split_whitespace();
    let (Some(recorded), Some(digest), Some(at)) = (
        fields.next().and_then(|field| field.parse::<u64>().ok()),
        fields.next(),
        fields.next().and_then(|field| field.parse::<u64>().ok()),
    ) else {
        return WindowLaunch::None;
    };
    let armed = matches!(boot_sentinel(staging).read_state(), Some((armed, _)) if armed == build);
    let this_trial = crate::manifest::FailedMark::read(&staging.trial())
        .is_some_and(|trial| trial.matches(build, digest));
    if recorded != build || !armed || !this_trial {
        return WindowLaunch::None;
    }
    // A record from further ahead than the wait itself (a clock set back since it
    // was written) is one the clock cannot place, and holds nothing: the wait is
    // bounded either way.
    if now.abs_diff(at) < WINDOW_TRIAL_HEADLESS_WAIT.as_secs() {
        WindowLaunch::Waiting {
            until: at.saturating_add(WINDOW_TRIAL_HEADLESS_WAIT.as_secs()),
        }
    } else {
        WindowLaunch::Lapsed
    }
}

/// Record that a launch that opens windows booted `build`'s trial, now. A record
/// that cannot be written — or no trial identity to key it by — is said: a
/// headless launch then counts and confirms as before.
fn record_window_launch(staging: &Staging, build: u64) {
    let Some(trial) = crate::manifest::FailedMark::read(&staging.trial())
        .filter(|trial| trial.build_number == build && !trial.sha256.is_empty())
    else {
        crate::warn(&format!(
            "build {build}'s trial has no identity to record a window launch against; a \
             headless launch of it may count toward it and confirm it"
        ));
        return;
    };
    if let Err(error) = write_private_file(
        &trial_window_launch_path(staging),
        format!("{build} {} {}\n", trial.sha256, unix_now_secs()).as_bytes(),
    ) {
        crate::warn(&format!(
            "could not record that a window launched build {build}'s trial: {error}; a \
             headless launch of it may count toward it and confirm it"
        ));
    }
}

/// A trial ENDS: its identity marker goes, and with it the record of the windows
/// that launched it. Every path that clears the trial marker calls this.
fn clear_trial_identity(staging: &Staging) {
    crate::manifest::FailedMark::clear(&staging.trial());
    let _ = std::fs::remove_file(trial_window_launch_path(staging));
}

/// What `update status` says while `build`'s trial waits for a window (law L5).
fn window_trial_wait_notice(build: u64, until: u64) -> String {
    format!(
        "build {build} is on trial after its update and no window of it has confirmed it \
         yet: open aterm. After {}, a headless start confirms it instead",
        aterm_types::rfc3339::format_rfc3339(until)
    )
}

/// The trial of `build` WAITING FOR A WINDOW, as `update status` says it
/// ([`window_trial_wait_notice`]), or `None` when it is not waiting.
///
/// COMPUTED FROM THE TRIAL'S OWN RECORDS, NEVER STORED: written into the health
/// ledger's one apply-refusal slot it would replace the older build's standing
/// reason and book a "refusal" where nothing failed. Read from the sentinel, the
/// trial's identity and the window-launch record, it is exactly as current as the
/// wait and touches nothing another process wrote.
pub(crate) fn window_trial_wait_at(staging: &Staging, build: u64, now: u64) -> Option<String> {
    match trial_window_launch(staging, build, now) {
        WindowLaunch::Waiting { until } => Some(window_trial_wait_notice(build, until)),
        WindowLaunch::None | WindowLaunch::Lapsed => None,
    }
}

/// Where a trial a HEADLESS launch settled keeps its window lane open:
/// `<build> <artifact digest> <unix seconds of the settle>` on
/// the first line, the install the trial was settled on (`app_root`) on the second.
///
/// WHY. A headless launch (a harness, `aterm --headless`) can reach the trial
/// before any window does. Confirming the whole trial then would delete the
/// predecessor, leaving nothing to count or revert to if the windows later crash
/// before their first frame.
///
/// So a headless confirmation settles the trial for the headless path only: the
/// sentinel disarms and the success is booked as before — later updates are not
/// held, and headless launches stop counting — but the predecessor stays at the
/// fixed rollback path and this record says whose it is. The next WINDOW launch of
/// the build from that install puts it back on trial ([`rearm_window_unproven_trial`]):
/// its crashes are counted and revert to the predecessor, and its first frame
/// confirms the build and deletes the predecessor, exactly as for a trial a window
/// started. With no window launch within [`WINDOW_TRIAL_HEADLESS_WAIT`] the record
/// lapses; the predecessor is then replaced by the next update's swap.
///
/// A separate file an older build neither reads nor writes: without it, a
/// headless confirmation is the whole trial's.
fn window_unproven_path(staging: &Staging) -> PathBuf {
    staging.root.join("boot.sentinel.window-unproven")
}

/// The record at [`window_unproven_path`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct WindowUnproven {
    build: u64,
    digest: String,
    at: u64,
    install_root: PathBuf,
}

impl WindowUnproven {
    /// The record, or `None` when there is none or it does not parse — which is
    /// read as no record: the window lane then settles as it did before it existed.
    fn read(staging: &Staging) -> Option<Self> {
        let text = crate::read_ledger_text(&window_unproven_path(staging))?;
        let mut lines = text.lines();
        let mut fields = lines.next()?.split_whitespace();
        let build = fields
            .next()?
            .parse::<u64>()
            .ok()
            .filter(|build| *build != 0)?;
        let digest = canonical_digest(fields.next()?)?;
        let at = fields.next()?.parse::<u64>().ok()?;
        let install_root = PathBuf::from(lines.next()?);
        install_root.is_absolute().then_some(Self {
            build,
            digest,
            at,
            install_root,
        })
    }

    fn record(&self, staging: &Staging) -> std::io::Result<()> {
        write_private_file(
            &window_unproven_path(staging),
            format!(
                "{} {} {}\n{}\n",
                self.build,
                self.digest,
                self.at,
                self.install_root.display()
            )
            .as_bytes(),
        )
    }

    /// Whether the record has lapsed at `now` (unix seconds): past
    /// [`WINDOW_TRIAL_HEADLESS_WAIT`] since the settle, or written further ahead than
    /// that (a clock set back since), which the clock cannot place.
    fn lapsed(&self, now: u64) -> bool {
        now.abs_diff(self.at) >= WINDOW_TRIAL_HEADLESS_WAIT.as_secs()
    }
}

fn clear_window_unproven(staging: &Staging) {
    let _ = std::fs::remove_file(window_unproven_path(staging));
}

/// Whether a window launch of `build` may find its window lane open — the cheap,
/// lock-free peek [`check_boot_health_with_lock_wait`] takes before it takes the
/// apply lock for a re-trial.
fn window_unproven_names(staging: &Staging, build: u64) -> bool {
    WindowUnproven::read(staging).is_some_and(|record| record.build == build)
}

/// PUT A BUILD A HEADLESS LAUNCH SETTLED BACK ON TRIAL for this window launch (see
/// [`window_unproven_path`]): arm `build`'s trial again over the retained
/// predecessor, as `prepare_trial` arms one — the identity marker, then the
/// sentinel — so the caller counts this launch against it. Runs under the apply
/// lock; `true` once the trial is armed.
///
/// Nothing is armed, and the launch goes on as any launch of a settled build, when:
/// * another trial is armed (it owns the fixed rollback path);
/// * the record is another build's, or another install's;
/// * a publication is ready (`ready.toml`): the next swap supersedes this build, and
///   its trial takes this build as its predecessor — a crash cut of that swap may
///   also have left the NEW candidate at the fixed path, which must never be
///   mistaken for this build's predecessor;
/// * the record has lapsed, or its predecessor, or the installed receipt naming
///   exactly this artifact, is gone — then the record is dropped.
fn rearm_window_unproven_trial(
    staging: &Staging,
    sentinel: &Sentinel,
    build: u64,
    app_root: &Path,
    now: u64,
) -> bool {
    let Some(record) = WindowUnproven::read(staging).filter(|record| record.build == build) else {
        return false;
    };
    if sentinel.read_state().is_some() || record.install_root != app_root || staging.ready.exists()
    {
        return false;
    }
    let drop_record = |why: &str| {
        crate::log(&format!(
            "build {build} was settled by a headless launch, and this window launch does not \
             put it back on trial: {why}"
        ));
        clear_window_unproven(staging);
        false
    };
    if record.lapsed(now) {
        return drop_record(&format!(
            "no window launched it within {} h",
            WINDOW_TRIAL_HEADLESS_WAIT.as_secs() / 3600
        ));
    }
    let fixed = rollback_path(app_root);
    if !is_non_symlink_dir(&fixed) || !same_volume(&fixed, app_root) {
        return drop_record("its retained predecessor is gone");
    }
    if !crate::manifest::InstalledReceipt::read(&staging.installed_receipt()).is_some_and(
        |receipt| {
            receipt.build_number == build && receipt.dmg_sha256.eq_ignore_ascii_case(&record.digest)
        },
    ) {
        return drop_record("the installed receipt no longer names that artifact");
    }
    if let Err(error) = crate::manifest::FailedMark::record_required(
        &staging.trial(),
        build,
        &record.digest,
        Some(app_root),
    ) {
        crate::warn(&format!(
            "could not put build {build} back on trial for a window launch (persist trial \
             identity: {error}); this launch is not counted against it"
        ));
        return false;
    }
    // A re-trial has no launches yet — the headless launchers of the settled trial
    // least of all — and no window has launched it until this one is counted.
    let _ = std::fs::remove_file(trial_launchers_path(staging));
    let _ = std::fs::remove_file(trial_window_launch_path(staging));
    if let Err(error) = sentinel.arm(build) {
        crate::manifest::FailedMark::clear(&staging.trial());
        crate::warn(&format!(
            "could not put build {build} back on trial for a window launch (arm: {error}); \
             this launch is not counted against it"
        ));
        return false;
    }
    clear_window_unproven(staging);
    crate::log(&format!(
        "build {build} was settled by a headless launch, which proves nothing about its \
         windows: this window launch puts it back on trial over the retained predecessor \
         at {}",
        fixed.display()
    ));
    true
}

/// Count this launch of the armed trial `current_build`, or say why it is not
/// counted. Runs under the apply lock.
///
/// The sentinel counts launches while the trial is unconfirmed, as the crash
/// signal. Two kinds of launch are not that signal:
///
/// * a burst ([`launch_is_burst`]): one launch event that started several processes
///   within [`BOOT_LAUNCH_BURST_WINDOW`];
/// * an overlapping handoff target: the live parent's authorized handoff target of
///   this build (`handoff_target`), launched while an earlier launch of this trial
///   is still running. It has not crashed; it is waiting for its own Commit, which
///   is when a handoff candidate confirms (several windows of one build each run a
///   candidate).
///
/// Only a handoff target is exempted by an overlap: otherwise an earlier launcher
/// that stays alive and unconfirmed would shield every later launch from the count,
/// the budget-exhausted escape in `check_boot_health` would never be reached, and no
/// later release would install while it lives. Each window hands off to at most one
/// target of a build, so the exemption is bounded.
///
/// A crash loop still counts every launch: each dies before the next begins.
///
/// A HEADLESS LAUNCH (`witness`) of a trial a WINDOW has launched is neither counted
/// nor recorded as a launcher: it proves nothing about the window path. Counted, a
/// harness's healthy instances would push a healthy build toward its revert;
/// recorded, a live one would exempt the window's candidates as overlapping handoff
/// targets so their crashes went uncounted. A headless launch of a trial no window
/// has launched — the headless self-update lane — counts, and is recorded as a
/// HEADLESS launcher, whose overlap exempts only a headless target
/// ([`overlap_exempts`]).
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "Boots",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "SiblingBoots",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "SiblingLaunchedCold",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
fn count_trial_launch(
    staging: &Staging,
    sentinel: &Sentinel,
    current_build: u64,
    launcher: Option<TrialLauncher>,
    handoff_target: bool,
    witness: crate::TrialWitness,
) -> Result<(), String> {
    match witness {
        crate::TrialWitness::Headless => {
            match trial_window_launch(staging, current_build, unix_now_secs()) {
                WindowLaunch::Waiting { until } => {
                    crate::log(&format!(
                        "boot sentinel for build {current_build}: a window has launched this \
                         trial, and a headless launch proves nothing about the window path, so \
                         it is not counted (attempt {} stands) — until {}, when headless \
                         launches count again",
                        sentinel.read_state().map_or(0, |(_, attempts)| attempts),
                        aterm_types::rfc3339::format_rfc3339(until)
                    ));
                    return Ok(());
                }
                WindowLaunch::Lapsed => crate::log(&format!(
                    "boot sentinel for build {current_build}: no window has launched this \
                     trial for {} h, so this headless launch counts toward it again",
                    WINDOW_TRIAL_HEADLESS_WAIT.as_secs() / 3600
                )),
                WindowLaunch::None => {}
            }
        }
        crate::TrialWitness::Window => record_window_launch(staging, current_build),
    }
    let mut launchers = read_trial_launchers(staging, current_build);
    launchers.retain(|recorded| recorded.launcher.is_alive());
    let overlapped = launchers
        .iter()
        .find(|live| {
            handoff_target
                && launcher.is_none_or(|this| this.pid != live.launcher.pid)
                && overlap_exempts(witness, live.witness)
        })
        .map(|live| live.launcher);
    // The age of the last COUNTED launch, read before a payment below rewrites the
    // line: a give-back is no launch, and must not make this one read as its burst.
    let sentinel_age = mtime(&staging.root.join("boot.sentinel")).and_then(|t| t.elapsed().ok());
    // What a give-back that met a held lock owes is paid before this launch is
    // counted: the count a revert reads never stands on a launch its killer gave
    // back.
    pay_owed_trial_launches(staging, sentinel);
    let attempts_now = sentinel.read_state().map_or(0, |(_, attempts)| attempts);
    if launch_is_burst(attempts_now, sentinel_age) {
        crate::log(&format!(
            "boot sentinel for build {current_build}: a launch {} ms after the last counted \
             one is the same burst and is not counted (attempt {attempts_now} stands)",
            sentinel_age.map_or(0, |age| age.as_millis())
        ));
    } else if let Some(live) = overlapped {
        crate::log(&format!(
            "boot sentinel for build {current_build}: this handoff target launched while an \
             earlier launch of it (pid {}) is still running, so it is not a relaunch after a \
             crash and is not counted (attempt {attempts_now} stands)",
            live.pid
        ));
    } else {
        // NAMED, so exactly this launch can be given back, once
        // (`forgive_trial_launch_of_at`); one the kernel will not name stays counted.
        sentinel
            .observe_launch_by(current_build, launcher.map(TrialLauncher::name))
            .map_err(|error| format!("trial launch observation: {error}"))?;
    }
    if let Some(this) = launcher
        && !launchers.iter().any(|recorded| recorded.launcher == this)
    {
        launchers.push(RecordedLauncher {
            launcher: this,
            witness,
        });
    }
    write_trial_launchers(staging, current_build, &launchers);
    Ok(())
}

/// Whether a live launch of the trial by `live` exempts an overlapping handoff
/// target launched by `target` from the count ([`count_trial_launch`]).
///
/// A WINDOW's target is exempted only beside a live WINDOW launch. The exemption
/// exists because several windows each run their own candidate of the one build; a
/// headless instance is no such sibling, and exempting beside it (a harness alive
/// for hours) would leave every crashing window candidate uncounted and the build
/// never reverted. A headless target is exempted beside any live launch.
fn overlap_exempts(target: crate::TrialWitness, live: crate::TrialWitness) -> bool {
    target == crate::TrialWitness::Headless || live == crate::TrialWitness::Window
}

fn prepare_trial(
    staging: &Staging,
    ready: &Ready,
    install_root: &Path,
) -> Result<Sentinel, String> {
    let sentinel = boot_sentinel(staging);
    // The trial is bound to the install it swaps: see `trial_owned_by`.
    if let Err(error) = crate::manifest::FailedMark::record_required(
        &staging.trial(),
        ready.build_number,
        &ready.dmg_sha256,
        Some(install_root),
    ) {
        return Err(format!("persist trial identity: {error}"));
    }
    // The sentinel is the transaction's commit marker and is published last. A
    // crash after trial.toml but before arm leaves no active authority; the reverse
    // order could wedge forever with an armed build but no exact digest.
    if let Err(error) = sentinel.arm(ready.build_number) {
        clear_trial_identity(staging);
        return Err(format!("arm boot-health trial: {error}"));
    }
    // A fresh trial owns the sentinel now, so launches counted against the previous
    // armed build are answered; the budget never accumulates across trials.
    let _ = foreign_trial_counter(staging).confirm();
    // A fresh trial has no launches yet: the record of an earlier one of the same
    // build must not read as this one's. Nor does the build this swap replaces keep
    // a window lane open: it is the new trial's predecessor now.
    let _ = std::fs::remove_file(trial_launchers_path(staging));
    let _ = std::fs::remove_file(trial_window_launch_path(staging));
    // …nor any launch of an earlier trial still owed back.
    let _ = remove_path_no_follow(&owed_give_backs_dir(staging));
    clear_window_unproven(staging);
    Ok(sentinel)
}

/// Whether the armed trial (if any) is owned by the install at `app_root`. The
/// sentinel and `trial.toml` are per user, but a build can be installed at several
/// paths (`~/Applications/aterm.app` beside `/Applications/aterm.app`). A
/// same-build process launched from a bundle the trial did not swap must neither
/// count launches against it, nor confirm it, nor disarm it as dead authority. A
/// marker without a recorded root (an older format) is treated as owned.
#[must_use]
fn trial_owned_by(staging: &Staging, app_root: &Path) -> bool {
    match crate::manifest::FailedMark::read(&staging.trial()).and_then(|trial| trial.install_root) {
        None => true,
        Some(recorded) => {
            let recorded = Path::new(&recorded);
            // An install that is gone (moved or deleted mid-trial) owns nothing, or
            // every launch of this build from elsewhere would defer forever; whoever
            // runs this build now inherits the trial and its escapes. Gone means
            // `NotFound` exactly: a TCC-protected folder answers another error and
            // still owns its trial. An ejected volume would read as gone, but no
            // trial records one: `bundle::resolve_from` refuses every `/Volumes/`
            // path, so nothing there is ever swapped or armed.
            match std::fs::symlink_metadata(recorded) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => false,
                Ok(_) => same_install_root(recorded, app_root),
            }
        }
    }
}

/// Path equality for install roots, tolerant of a canonicalizable spelling: the one
/// "is this the same copy of aterm.app?" test, shared by the trial-ownership rule
/// and `which_copy`, so a symlinked spelling reads the same on both.
#[must_use]
pub(crate) fn same_install_root(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

#[must_use]
fn abandoned_preswap_trial(process_build: u64, installed_build: u64, armed_build: u64) -> bool {
    installed_build == process_build && armed_build > process_build
}

/// Dead authority: the sentinel names a build strictly older than the bundle we are
/// canonically installed as and running. Only an out-of-band install (a
/// `tools/install.sh` run, a DMG drag) during an armed trial produces it; a trial
/// legitimately survives two crashing launches (`MAX_BOOT_ATTEMPTS` is 3), which is
/// when a user might hand-install a different build.
///
/// Disjoint from [`abandoned_preswap_trial`]: there the armed build is above the
/// installed one, here below it.
#[must_use]
fn dead_authority_trial(process_build: u64, installed_build: u64, armed_build: u64) -> bool {
    installed_build == process_build && armed_build < installed_build
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

/// The terminal transition for a [`dead_authority_trial`]: disarm, and drop the
/// artifacts that trial owned.
///
/// Leaving it armed would be permanent: it can no longer be confirmed (its build is
/// not installed) and must not be reverted (that would install an older build),
/// and `check_boot_health` / `confirm_boot_health` /
/// `confirm_trial_health_after_proof` all return early unless the armed build is the
/// running one, so every future apply would be refused.
///
/// The caller must have proved the shape first; `check_boot_health` already
/// returned for `armed == process_build`, so this cannot race a live trial of ours.
fn disarm_dead_authority_trial(
    staging: &Staging,
    installed: &Path,
    armed_build: u64,
    installed_build: u64,
    installed_commit: &str,
    process_build: u64,
) -> bool {
    if boot_sentinel(staging).confirm().is_err() {
        return false;
    }
    clear_trial_identity(staging);
    // Its retained rollback source is older than the running build and has no
    // sentinel behind it; drop it so a later `recover_orphaned_prepared_candidate`
    // cannot mistake it for this transaction.
    let _ = remove_path_no_follow(&rollback_path(installed));
    if !crate::manifest::InstalledReceipt::read(&staging.installed_receipt())
        .is_some_and(|receipt| receipt.matches_sealed(installed_build, installed_commit))
    {
        crate::manifest::InstalledReceipt::clear(&staging.installed_receipt());
    }
    crate::warn(&format!(
        "boot sentinel armed for build {armed_build} is older than the installed and running \
         build {installed_build} (the bundle was replaced out of band); disarmed it rather \
         than blocking every future update"
    ));
    crate::status::record(
        staging,
        process_build,
        &format!(
            "cleared a stale update trial for build {armed_build} (build {installed_build} is \
             installed); updates re-enabled"
        ),
    );
    true
}

/// Whether this install may recover an abandoned pre-swap trial: the shape matches,
/// the trial is this install's (`trial_owned_by`; the staging root is shared by
/// every copy on the machine), and the running build is the installed one.
fn may_recover_abandoned_preswap(
    staging: &Staging,
    installed: &Path,
    (process_build, installed_build, armed_build): (u64, u64, u64),
    identity_matches: bool,
) -> bool {
    abandoned_preswap_trial(process_build, installed_build, armed_build)
        && trial_owned_by(staging, installed)
        && identity_matches
}

/// Recovers the one safe mismatched-sentinel crash cut under the apply lock: this
/// process's old build is still canonically installed while the armed build is
/// newer. With the lock held, a concurrent swap has finished, so canonical OLD
/// proves NEW is not installed. Covers a pre-swap crash and a crash right after an
/// inverse rollback.
///
/// Also the one place a [`dead_authority_trial`] (a sentinel armed for a build
/// older than the running one) is retired.
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
    // After identity resolution on purpose: an installed bundle we cannot verify
    // fails closed and disarms nothing. Only the install the trial swapped may
    // retire it: a newer sibling bundle at another path also sees "armed for an
    // older build", and must not strip the live install's crash-loop protection.
    if dead_authority_trial(process_build, installed_build, armed_build)
        && trial_owned_by(staging, installed)
        && identity_matches_running(
            installed_build,
            &installed_commit,
            process_build,
            process_commit,
        )
    {
        // No `retire_published()` here (unlike the check_boot_health escape hatch):
        // returning true lets the caller apply a legitimately staged newer build in
        // this same launch.
        return disarm_dead_authority_trial(
            staging,
            installed,
            armed_build,
            installed_build,
            &installed_commit,
            process_build,
        );
    }
    if !may_recover_abandoned_preswap(
        staging,
        installed,
        (process_build, installed_build, armed_build),
        identity_matches_running(
            installed_build,
            &installed_commit,
            process_build,
            process_commit,
        ),
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

    // A crash after the O(1) staged_app→fixed rename (or after arming, before the
    // swap) leaves exact NEW at fixed and exact ready authority: restore it to the
    // published staging path so the update stays retryable. An inverse-swap cut
    // after ready retirement has no publisher, so fixed is failed NEW and may be
    // reclaimed only after the successful disarm above.
    let ready = Ready::read(&staging.ready);
    if ready.as_ref().is_some_and(|ready| {
        ready_matches_verified_identity(ready, candidate_build, &candidate_commit)
    }) {
        // Cross-volume staging copies staged_app -> fixed and keeps the staged copy,
        // so when the staged bundle already carries the exact candidate identity the
        // fixed twin is redundant: reclaim it and keep the publication (renaming
        // back would fail EXDEV on every external-volume install).
        if verified_bundle_identity(&staging.staged_app)
            .is_ok_and(|(build, commit)| build == candidate_build && commit == candidate_commit)
        {
            let _ = remove_path_no_follow(&fixed);
        } else {
            let _ = remove_path_no_follow(&staging.staged_app);
            if std::fs::rename(&fixed, &staging.staged_app).is_err() {
                // EXDEV (or any restore failure): the bytes cannot reach the
                // staging path. Retire the publication and reclaim the fixed
                // candidate so this launch settles at NoUpdate and the next check
                // re-stages cleanly. Pre-swap the fixed path is NEW, never a
                // rollback source, so reclaiming it is safe.
                crate::warn(&format!(
                    "cannot restore the fixed candidate (build {candidate_build}) to the \
                     staging path (cross-volume?); retiring the publication so the next \
                     check re-stages"
                ));
                staging.retire_published();
                let _ = remove_path_no_follow(&fixed);
            }
        }
    } else {
        let _ = remove_path_no_follow(&fixed);
    }
    clear_trial_identity(staging);
    let receipt_matches = crate::manifest::InstalledReceipt::read(&staging.installed_receipt())
        .is_some_and(|receipt| receipt.matches_sealed(installed_build, &installed_commit));
    if !receipt_matches {
        crate::manifest::InstalledReceipt::clear(&staging.installed_receipt());
    }
    true
}

/// Budgeted escape for a boot sentinel armed for a build that is not the running
/// one and that [`recover_abandoned_preswap_trial_if_exact`] could not resolve.
///
/// Such a sentinel is otherwise permanent: every lane that clears one
/// (`check_boot_health`, `confirm_boot_health`, `confirm_trial_health_after_proof`)
/// returns early unless the armed build is the running one, and [`prepare_trial`],
/// the only writer, is gated behind the failing check. Every launch would return
/// `Deferred` while the stager keeps downloading. The shapes that reach here are
/// those the exact recovery refuses (a rollback sibling that is not the armed
/// build, an unverifiable install, an armed build below a running build we are not
/// canonically installed as).
///
/// So it is budgeted like the same-build lane: after [`MAX_BOOT_ATTEMPTS`] launches
/// that observed the same unrecoverable armed build, disarm. The count lives in
/// [`foreign_trial_counter`], never in the boot sentinel.
///
/// Returns whether the sentinel was disarmed. The caller still defers on the
/// disarming launch (recovery state was just mutated; the next launch takes the
/// normal path, since `recover_orphaned_prepared_candidate` requires an absent
/// sentinel). [`apply_staged_if_ready`]'s refusal line supersedes this launch's
/// status line; the durable record of the repair is the health ledger entry and
/// the log warning below.
fn escape_wedged_foreign_trial(staging: &Staging, current_build: u64, armed_build: u64) -> bool {
    let counter = foreign_trial_counter(staging);
    // A live trial is not a wedged one. The staging root is per user, not per
    // install, so another installation can be mid-trial on a build this process does
    // not run; disarming it would remove the crash-loop protection from the build
    // that is crash-looping.
    //
    // Only the trial's owner advances the sentinel (`observe_launch` counts only when
    // the armed build is the running one), rewriting the file. So compare the
    // sentinel's mtime with our counter's: advanced ⇒ the owner is alive ⇒ restart the
    // budget and defer; frozen across our whole budget ⇒ nobody is left to confirm or
    // revert it.
    let sentinel = boot_sentinel(staging);
    let counter_state = counter.read_state();
    let advanced = mtime(&staging.root.join("boot.sentinel"))
        .zip(mtime(&foreign_trial_path(staging)))
        .is_some_and(|(trial, ours)| trial > ours);
    if advanced {
        // The owner moved since we last counted: restart the budget and do not count
        // this launch; an actively observed trial is not abandoned.
        let _ = counter.arm(armed_build);
        return false;
    }
    if !matches!(counter_state, Some((build, _)) if build == armed_build) {
        // A different armed build than the one we were counting (or none yet):
        // start this build's budget from zero rather than inheriting it.
        let _ = counter.arm(armed_build);
    }
    let observed = counter.observe_launch(armed_build).unwrap_or(0);
    if observed < MAX_BOOT_ATTEMPTS {
        return false;
    }
    // A trial owned by another install that still exists keeps its protection
    // unless that owner has been idle for `FOREIGN_TRIAL_OWNER_IDLE` (the owner's
    // every launch rewrites the sentinel), so a sibling copy's launches cannot strip
    // the crash-loop revert from the install that is mid-trial.
    let owned_elsewhere = crate::manifest::FailedMark::read(&staging.trial())
        .and_then(|trial| trial.install_root)
        .is_some_and(|root| {
            let root = Path::new(&root);
            // Present unless the path is demonstrably gone (`NotFound`): a
            // TCC-protected folder still owns its trial, as `trial_owned_by` judges
            // it (which also says why an ejected volume never gets here).
            let present = !matches!(
                std::fs::symlink_metadata(root),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound
            );
            present
                && bundle::resolve_layout().is_none_or(|b| !same_install_root(root, &b.app_root))
        });
    if owned_elsewhere {
        let idle_long_enough = mtime(&staging.root.join("boot.sentinel"))
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age >= FOREIGN_TRIAL_OWNER_IDLE);
        if !idle_long_enough {
            return false;
        }
    }
    if sentinel.confirm().is_err() {
        return false;
    }
    clear_trial_identity(staging);
    let _ = counter.confirm();
    crate::health::Health::record_apply_failure(
        &staging.health(),
        current_build,
        armed_build,
        &format!(
            "update trial for build {armed_build} was unrecoverable across {MAX_BOOT_ATTEMPTS} \
             launches of build {current_build}; disarmed the boot sentinel to keep updates \
             possible"
        ),
    );
    crate::status::record(
        staging,
        current_build,
        "recovered a wedged update trial (armed build is not installed); updates re-enabled",
    );
    crate::warn(&format!(
        "boot sentinel armed for build {armed_build} could not be recovered in \
         {MAX_BOOT_ATTEMPTS} launches of build {current_build}; disarmed it rather than \
         blocking every future update"
    ));
    true
}

/// The stable path where the swapped-out OLD bundle is retained as the rollback
/// source: a fixed-named sibling of the install (same volume, so the revert
/// swap-back is atomic), findable by the re-exec'd new process and by
/// [`confirm_boot_health`] without a pid in the name.
fn rollback_path(installed: &Path) -> PathBuf {
    installed.with_file_name("aterm.app.rollback")
}

fn canonical_digest(digest: &str) -> Option<String> {
    let digest = digest.trim();
    aterm_codec::hex::is_hex(digest, 64).then(|| digest.to_ascii_lowercase())
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

/// Recovers the crash cut after the same-volume staged_app→fixed move but before
/// the sentinel commit marker. Exact ready plus verified-bundle identity is enough
/// to put the bytes back; a mismatched or corrupt fixed path is preserved and never
/// adopted. When there is nothing to adopt, the dangling marker is retired (see
/// that branch).
fn recover_orphaned_prepared_candidate(
    staging: &Staging,
    installed: &Path,
    current_build: u64,
    ready: &Ready,
) -> Result<(), String> {
    if is_non_symlink_dir(&staging.staged_app) || boot_sentinel(staging).read_state().is_some() {
        return Ok(());
    }
    // Past this point `ready.toml` advertises bytes that are not at `staged_app`, and
    // the fixed path is the only place they could still be.
    let fixed = rollback_path(installed);
    let recoverable = verified_bundle_identity(&fixed).is_ok_and(|(build, commit)| {
        same_volume(&fixed, installed)
            && build > current_build
            && ready_matches_verified_identity(ready, build, &commit)
    });
    if !recoverable {
        // The marker names bytes that exist nowhere (typically the fixed path is just
        // the previous build's retained rollback, or absent). Deferring would be
        // permanent, since this runs before the `is_publishable` gate that could
        // retire the marker. So retire it and report success: retirement touches only
        // `ready` and the already-absent `staged_app`, the caller re-reads `Absent` →
        // `NoUpdate`, and the next check re-stages. The fixed path is left alone: it
        // may be the rollback source crash recovery depends on.
        crate::warn(&format!(
            "ready.toml advertises build {} but its staged bundle is gone and {} holds no \
             matching candidate; retiring the dangling marker so the next check re-stages",
            ready.build_number,
            fixed.display()
        ));
        staging.retire_published();
        return Ok(());
    }
    if let Err(error) = std::fs::rename(&fixed, &staging.staged_app) {
        // EXDEV on an external-volume install (fixed sits on the install volume,
        // staging under HOME) or any other restore failure: as in the
        // nothing-to-adopt branch, retire the marker, report success and let the
        // next check re-stage. The fixed path is left alone (it may be the rollback
        // source a later recovery reads).
        crate::warn(&format!(
            "cannot restore the orphaned fixed candidate to the staging path ({error}); \
             retiring the dangling marker so the next check re-stages"
        ));
        staging.retire_published();
    }
    Ok(())
}

/// Materializes and verifies NEW at the fixed destination-volume rollback name
/// before the point of no return, so the single RENAME_SWAP places OLD directly at
/// the only path crash-loop recovery knows; no post-swap rename or crash window.
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
    staged_identity: (u64, &str),
) -> Result<PreparedSwapCandidate, String> {
    let staged = &staging.staged_app;
    let fixed = rollback_path(installed);
    remove_path_no_follow(&fixed)
        .map_err(|error| format!("remove stale fixed rollback: {error}"))?;

    // The usual macOS layout puts ~/Library and /Applications on one APFS volume, so
    // moving the already-verified bundle is O(1) metadata work, avoiding a `ditto`
    // copy on the launch path. External-volume installs keep the copy path.
    let moved_from_stage = same_volume(staged, installed);
    if moved_from_stage {
        std::fs::rename(staged, &fixed)
            .map_err(|error| format!("move verified stage to fixed swap path: {error}"))?;
    } else {
        // The one slow step here: a large copy between volumes, while the apply lock is
        // held and before the window exists. Bounded so an unplugged or wedged external
        // volume cannot hang the launch and every launch queued on the lock.
        let copy_started = std::time::Instant::now();
        let (status, stderr) = ditto_bounded(
            &[staged.into(), fixed.as_os_str().to_os_string()],
            "ditto to fixed swap path",
            CROSS_VOLUME_COPY_TIMEOUT,
        )
        .inspect_err(|_| {
            let _ = remove_path_no_follow(&fixed);
        })?;
        if !status.success() {
            let _ = remove_path_no_follow(&fixed);
            return Err(ditto_failure(
                "ditto to fixed swap path failed",
                status,
                &stderr,
            ));
        }
        // Disk time, not helper time: a slow volume must not spend the
        // verification budget that the checks after this copy still need.
        crate::verify::ApplyBudget::extend(copy_started.elapsed());
    }
    let prepared = PreparedSwapCandidate {
        fixed: fixed.clone(),
        moved_from_stage,
    };
    if !same_volume(&fixed, installed) {
        recover_prepared_candidate(&prepared, staging);
        return Err("fixed swap candidate is not on the installed volume".to_string());
    }
    // The candidate's sealed identity. On the same-volume path this is the
    // identity the CALLER just read, because `rename(2)` moved the very inode it read it
    // from: same bytes, same signature, same plist. Re-running the five
    // codesign/spctl/PlistBuddy helpers over it would re-answer a question
    // nothing could have changed the answer to — the apply lock is held, the
    // parent is a private directory, and `checked_bundle_exchange` revalidates
    // the no-follow shape again at the point of no return. That redundant pass
    // was five of the sixteen helper spawns on the launch path.
    //
    // The cross-volume path does re-verify: `ditto` produced new bytes at a new path.
    let (fixed_build, sealed_commit) = if moved_from_stage {
        (staged_identity.0, staged_identity.1.to_string())
    } else {
        match verified_bundle_identity(&fixed) {
            Ok(identity) => identity,
            Err(error) => {
                recover_prepared_candidate(&prepared, staging);
                return Err(format!("fixed swap candidate failed verification: {error}"));
            }
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
    // Revalidate the no-follow shape at the point of no return. Full
    // signature/build/commit verification ran after materialization; the private
    // parent and apply lock exclude compliant replacement, and this guard ensures a
    // symlink can never be exchanged into the canonical install.
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
    rename_swap(a, b).map_err(|error| format!("{operation} failed: {error}"))?;
    sync_swap_parents(a, b);
    Ok(())
}

/// Makes a completed swap durable the way `manifest::write_durable` does a ledger
/// record: the exchange is a directory-entry change, so sync the directory holding
/// each name (once when shared). Best effort: the swap already happened, and a
/// filesystem that cannot sync a directory stays non-durable rather than failing.
fn sync_swap_parents(a: &Path, b: &Path) {
    let sync = |dir: &Path| {
        let _ = crate::durable::sync_dir(dir);
    };
    let parent_a = a.parent();
    let parent_b = b.parent();
    if let Some(dir) = parent_a {
        sync(dir);
    }
    if let Some(dir) = parent_b
        && parent_a != Some(dir)
    {
        sync(dir);
    }
}

/// Reads a bundle's sealed identity (build + commit), refusing anything that is not
/// a plain directory with an intact, policy-satisfying signature. The crate-visible
/// door onto [`verified_bundle_identity`] for the installed-bundle activation
/// pre-verify (`crate::preverify_installed_for_handoff`): the same gate the staged
/// candidate uses, aimed at the bundle under the running executable.
pub(crate) fn verified_bundle_identity_at(app: &Path) -> Result<(u64, String), String> {
    verified_bundle_identity(app)
}

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
    /// The sealed build the validation proved. The revert names it and compares it
    /// with the operator floor: a crash loop is worse than a yank, so the revert
    /// still happens, but a restored build below the floor is logged rather than
    /// silently running a yanked build.
    build: u64,
}

fn ensure_fixed_rollback(installed: &Path, current_build: u64) -> Result<VerifiedRollback, String> {
    let fixed = rollback_path(installed);
    match std::fs::symlink_metadata(&fixed) {
        Ok(_) => {
            // Validation is the whole point of this call — it refuses a rollback
            // whose sealed build is not a strict predecessor.
            let (build, _) = validate_fixed_rollback(&fixed, installed, current_build)
                .map_err(|error| format!("fixed rollback is invalid: {error}"))?;
            return Ok(VerifiedRollback { path: fixed, build });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect fixed rollback: {error}")),
    }
    // No fixed rollback, and nowhere else to look: the atomic
    // `renamex_np(RENAME_SWAP)` swap always leaves OLD at the fixed path.
    Err("fixed rollback missing".to_string())
}

/// The swap landed but its installed receipt could not be written: put the previous
/// build back, disarm the trial, and leave the verified stage as it stood before the
/// swap so the next launch retries the same bytes.
///
/// After [`restore_rollback`] the fixed path (`retained`, i.e. `prepared.fixed`)
/// holds the candidate again (the pre-swap shape), so the stage is recovered as
/// every pre-swap failure recovers it, by [`recover_prepared_candidate`]: a
/// candidate moved out of the stage is moved back, and a copied one (cross-volume
/// staging, where `staged_app` still holds the published bytes) is reclaimed.
fn roll_back_unreceipted_swap(
    prepared: &PreparedSwapCandidate,
    retained: &Path,
    installed: &Path,
    sentinel: &Sentinel,
    staging: &Staging,
    error: &str,
) -> ApplyOutcome {
    debug_assert_eq!(retained, prepared.fixed.as_path());
    crate::warn(&format!(
        "could not commit installed artifact receipt after swap: {error}; rolling back"
    ));
    if let Err(rollback_error) = restore_rollback(retained, installed) {
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
    clear_trial_identity(staging);
    recover_prepared_candidate(prepared, staging);
    ApplyOutcome::Deferred(format!("installed receipt: {error}"))
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
        // OLD is canonical again. A receipt for failed NEW must never stay usable as
        // OLD's install proof because the rewrite failed. Removal makes the common
        // failure fail closed; even if it fails, every reader binds the receipt to
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
) -> Result<VerifiedRollback, String> {
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
        return Ok(verified_rollback);
    }

    // Process-crash cut after the fixed RENAME_SWAP but before the receipt commit:
    // ready still carries the exact full commit and digest. Bind it to the sealed
    // installed identity and finish the receipt before health can GC the rollback.
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
        return Ok(verified_rollback);
    }

    Err("installed trial has no exact receipt or authorized recovery record".to_string())
}

/// A mounted DMG that detaches itself on drop (best-effort). Mounted at a private
/// mountpoint inside our `0700` staging dir (never `/Volumes`), so an abnormal exit
/// cannot leak a browsable `/Volumes/aterm*` mount and same-named volumes cannot
/// collide.
///
/// Every `hdiutil` is bounded: these run on the checker thread while it holds the
/// check lane and `checker.lock`, so a hung attach or detach must not park it. A
/// timeout is a failed attach (retried, then the stage fails for this check) or a
/// detach given up on (the `mnt-*` sweep reclaims it next time).
struct Mounted {
    mountpoint: PathBuf,
}

impl Mounted {
    /// `hdiutil attach -nobrowse -readonly -noautoopen -mountpoint <mp> <dmg>`, mounting
    /// at the caller-chosen private `mp` (created fresh) so we never touch `/Volumes`
    /// and never have to parse a mount table. `mp` must be under our own `0700` dir.
    fn attach(dmg: &Path, mountpoint: &Path) -> Result<Self, String> {
        // Retry, then fall back: a stage that dies here leaves the machine on the
        // previous build with a verified DMG on disk. Both mitigations are no-ops on
        // the happy path and right for a transient refusal (memory pressure, a slow
        // device).
        //
        // They do not fix an `ENXIO` ("Device not configured") from an orphaned
        // process: after a seamless overlap update the surviving aterm can be a
        // fork-child whose launchd job has exited, and DiskImages registers with
        // `com.apple.hdiejectd` through that dead bootstrap context, which can never
        // succeed. The fix is to avoid hdiutil (`stage_from_zip`); this path serves
        // releases whose manifest carries no zip.
        let mut attempts: Vec<String> = Vec::new();
        for attempt in 0..3u32 {
            if attempt > 0 {
                // Linear backoff: the races this loses are short-lived.
                std::thread::sleep(std::time::Duration::from_millis(500 * u64::from(attempt)));
            }
            match Self::attach_at(dmg, Some(mountpoint)) {
                Ok(mounted) => return Ok(mounted),
                Err(error) => attempts.push(error),
            }
        }
        // Last resort: let hdiutil pick the mount point. `-mountpoint` keeps us out
        // of `/Volumes` (and Finder), so it is tried first, but a readable image
        // beats one that cannot update; `Drop` detaches either shape.
        match Self::attach_at(dmg, None) {
            Ok(mounted) => Ok(mounted),
            Err(error) => {
                attempts.push(error);
                let _ = std::fs::remove_dir_all(mountpoint);
                let size = std::fs::metadata(dmg).map_or_else(
                    |_| "unreadable".to_string(),
                    |meta| format!("{} bytes", meta.len()),
                );
                // Passing only if every attempt was: one attach past its limit beside
                // an ENXIO is still the ENXIO's verdict, so each attempt's own key is
                // removed and the whole is marked once, only when nothing but moments
                // refused it.
                let passing = attempts
                    .iter()
                    .all(|error| crate::is_passing_refusal(error));
                let key = format!(" ({})", crate::PASSING_REFUSAL_KEY);
                let joined = attempts
                    .iter()
                    .map(|error| error.replace(&key, ""))
                    .collect::<Vec<_>>()
                    .join(" | ");
                let message = format!(
                    "hdiutil attach failed after {} attempts ({}, {size}): {joined}",
                    attempts.len(),
                    dmg.display(),
                );
                Err(if passing {
                    crate::verify::mark_passing(message)
                } else {
                    message
                })
            }
        }
    }

    /// One `hdiutil attach`. `mountpoint` = `Some(dir)` mounts there (the private
    /// 0700 path); `None` lets hdiutil choose, and the chosen path is read back
    /// from its output so `Drop` can detach exactly what was attached.
    fn attach_at(dmg: &Path, mountpoint: Option<&Path>) -> Result<Self, String> {
        let mut cmd = aterm_uds::spawnfd::command("/usr/bin/hdiutil");
        cmd.args(["attach", "-nobrowse", "-readonly", "-noautoopen"]);
        if let Some(mountpoint) = mountpoint {
            let _ = std::fs::remove_dir_all(mountpoint);
            std::fs::create_dir_all(mountpoint).map_err(|e| format!("create mountpoint: {e}"))?;
            cmd.arg("-mountpoint").arg(mountpoint);
        }
        let out =
            crate::verify::output_within(cmd.arg(dmg), "hdiutil attach", HDIUTIL_ATTACH_WAIT)?;
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
            // hdiutil's plain output is tab-separated `dev \t type \t mountpoint`;
            // splitting on tabs keeps a mount path with spaces (the release DMG's
            // `aterm X.Y.Z` volume name) intact.
            None => {
                let parsed = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(|line| line.split('\t').nth(2))
                    .map(str::trim)
                    .find(|candidate| !candidate.is_empty())
                    .map(PathBuf::from);
                match parsed {
                    Some(found) => Ok(Self { mountpoint: found }),
                    // The image attached; only naming it failed. There is no
                    // `Mounted` guard yet, so detach by device node (column 0,
                    // present even when the mount column is not) before reporting
                    // the failure.
                    None => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        for dev in stdout
                            .lines()
                            .filter_map(|line| line.split('\t').next())
                            .map(str::trim)
                            .filter(|dev| dev.starts_with("/dev/"))
                        {
                            let _ = crate::verify::output_within(
                                aterm_uds::spawnfd::command("/usr/bin/hdiutil")
                                    .args(["detach", "-force"])
                                    .arg(dev),
                                "hdiutil detach",
                                HDIUTIL_DETACH_WAIT,
                            );
                        }
                        Err(
                            "default mountpoint: hdiutil attached but named no mount point"
                                .to_string(),
                        )
                    }
                }
            }
        }
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = crate::verify::output_within(
            aterm_uds::spawnfd::command("/usr/bin/hdiutil")
                .args(["detach", "-force"])
                .arg(&self.mountpoint),
            "hdiutil detach",
            HDIUTIL_DETACH_WAIT,
        );
        // The private mountpoint is our own empty dir once detached; reclaim it.
        let _ = std::fs::remove_dir_all(&self.mountpoint);
    }
}

/// The bound on one `hdiutil attach` ([`Mounted`]): it reads and checksums the
/// whole image, seconds for a release DMG on any disk; minutes means it is hung.
const HDIUTIL_ATTACH_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// The bound on one `hdiutil detach -force` ([`Mounted`]): immediate when the
/// image is attached and when it is not.
const HDIUTIL_DETACH_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Best-effort reconciliation of leftover private mountpoints (`mnt-*`) from a run
/// killed mid-stage (its `Mounted::drop` never ran): force-detach, then remove each.
fn sweep_stale_mounts(staging: &Staging) {
    let Ok(entries) = std::fs::read_dir(&staging.root) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with("mnt-") {
            let p = e.path();
            let _ = crate::verify::output_within(
                aterm_uds::spawnfd::command("/usr/bin/hdiutil")
                    .args(["detach", "-force"])
                    .arg(&p),
                "hdiutil detach",
                HDIUTIL_DETACH_WAIT,
            );
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

/// The zip path's counterpart to [`sweep_stale_mounts`]: reclaims leftover `zx-*`
/// extract dirs from a run killed mid-stage. Nothing needs detaching, but an
/// abandoned extract can be a whole app bundle in Application Support.
fn sweep_stale_extracts(staging: &Staging) {
    let Ok(entries) = std::fs::read_dir(staging.staged_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with("zx-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Both lanes sweep both kinds of scratch, before they unpack: a `mnt-<pid>` a DMG
/// stage left behind (with its image possibly still attached) is reclaimed even
/// when the zip lane runs, and even when `ditto` cannot open the container.
fn sweep_stale_scratch(staging: &Staging) {
    sweep_stale_mounts(staging);
    sweep_stale_extracts(staging);
}

fn canonical_release_commit(commit: Option<&str>) -> Option<String> {
    let commit = commit?.trim();
    aterm_codec::hex::is_hex(commit, 40).then(|| commit.to_ascii_lowercase())
}

fn sealed_commit_matches(expected: Option<&str>, sealed: &str) -> bool {
    canonical_release_commit(expected)
        .is_some_and(|expected| crate::commit_matches(&expected, sealed))
}

/// Pre-park handoff verification (the seamless-update overlap seam): runs the full
/// staged-bundle authenticity gate (the tiered codesign policy check plus the sealed
/// CFBundleVersion/commit rebinding the swap re-runs at apply time) while every
/// parent PTY reader is still live, so a candidate that would fail codesign is
/// refused before a single reader stops rather than inside the parked window.
///
/// Strictly additive: the child still re-verifies everything at swap time under
/// `apply_lock` (the disk can change between this call and the child's gate). `Ok`
/// here is a latency optimization, never a grant.
///
/// `expected_build`/`expected_commit` bind the check to the exact artifact the
/// reducer authorized (like `ready_matches_expected`): a stage that changed identity
/// since authorization fails now. `current_commit` is the running image's
/// provenance, for the swap's second precondition: the outgoing bundle must be able
/// to become the rollback source ([`preverify_installed_rollback_source`]).
///
/// Only on a pass does it read the candidate's handoff policy
/// (`aterm_update_core::handoff_policy`): after every check has authenticated the
/// bundle and still under the apply lock, so the file read is the one the verified
/// signature seals.
pub fn preverify_staged_handoff_candidate(
    current_build: u64,
    current_commit: Option<&str>,
    expected_build: Option<u64>,
    expected_commit: Option<&str>,
) -> Result<crate::HandoffCandidateFacts, String> {
    let Some(staging) = Staging::resolve() else {
        return Err("no private staging root is available".to_string());
    };
    preverify_staged_handoff_candidate_at(
        &staging,
        bundle::resolve().as_ref().map(|b| b.app_root.as_path()),
        current_build,
        current_commit,
        expected_build,
        expected_commit,
        &crate::HandoffCandidateFacts::read,
    )
    .and_then(std::convert::identity)
}

/// The key marking the refusal of an installed bundle that cannot become the
/// rollback source a swap installs at the fixed path (apply step 7: a corrupt or
/// replaced install must never become crash-recovery authority). The seamless
/// apply asks this before parking ([`preverify_installed_rollback_source`]), the
/// cold-launch apply at step 7; neither lane can update such an install (e.g. a
/// local ad-hoc build swapped into `/Applications` without the `ATermDevBuild`
/// mark). Both append [`ROLLBACK_SOURCE_REMEDY`]; the trouble classifier keys on
/// this clause to show the cause and remedy and to stop scheduling retries.
pub const ROLLBACK_SOURCE_REFUSAL_KEY: &str = "cannot be the rollback source";

/// The one sentence that names the way out of an unverifiable install.
pub const ROLLBACK_SOURCE_REMEDY: &str = "this install cannot update itself: reinstall the      signed release (tools/install.sh, or drag it from the release DMG) — or, for a local      build, mark it ATermDevBuild (tools/dev-app.sh) so the updater leaves it alone";

/// The refusal both apply lanes mint when the INSTALLED bundle cannot be the
/// swap's rollback source, with the remedy attached.
pub fn rollback_source_refusal(installed: &Path, error: &str) -> String {
    format!(
        "the installed bundle at {} {ROLLBACK_SOURCE_REFUSAL_KEY} the swap installs: {error};          {ROLLBACK_SOURCE_REMEDY}",
        installed.display()
    )
}

/// True when a refusal string is the installed-bundle one — permanent for this
/// process until a person changes the bundle, never a retry candidate.
pub fn is_rollback_source_refusal(reason: &str) -> bool {
    reason.contains(ROLLBACK_SOURCE_REFUSAL_KEY)
}

/// The one sentence that names the way out of a non-writable install location
/// ([`crate::INSTALL_LOCATION_REFUSAL_KEY`]).
pub(crate) const INSTALL_LOCATION_REMEDY: &str = "this install cannot update itself: move aterm.app to \
     a folder you can write to (such as ~/Applications), or have an administrator install \
     the update";

/// The refusal for an install whose parent folder this user cannot write (an
/// admin-installed `/Applications` run by a standard user, or an MDM-managed one).
/// The swap renames inside that folder, so no lane can install anything until a
/// person moves the app or grants the write; the refusal gives the remedy.
pub(crate) fn install_location_refusal(app_root: &Path) -> String {
    format!(
        "{}: {}; {INSTALL_LOCATION_REMEDY}",
        crate::INSTALL_LOCATION_REFUSAL_KEY,
        app_root.display()
    )
}

/// The swap's write, checked on every lane that needs it (the pre-park check, the
/// boot apply's step 5, the activation supersede): `Ok` when the parent folder took
/// a probe entry.
///
/// A write this user may not make is [`install_location_refusal`], the person's
/// (move the app, or an administrator installs); a write that failed for a moment
/// (full disk, quota, I/O error) is marked [`crate::PASSING_REFUSAL_KEY`] and
/// retried.
pub(crate) fn install_location_check(app_root: &Path) -> Result<(), String> {
    install_location_verdict(app_root, bundle::probe_parent_write(app_root))
}

/// [`install_location_check`]'s mapping of one probe answer, apart from the probe.
fn install_location_verdict(app_root: &Path, probe: bundle::ParentWrite) -> Result<(), String> {
    match probe {
        bundle::ParentWrite::Writable => Ok(()),
        bundle::ParentWrite::NotWritable(_) => Err(install_location_refusal(app_root)),
        bundle::ParentWrite::Passing(error) => Err(crate::verify::mark_passing(format!(
            "the install location {} could not be written just then: {error}",
            app_root.display()
        ))),
    }
}

/// The refusal for an installed bundle whose verification returned `error`, on
/// every lane that asks (the pre-park check below and the cold launch's steps 7-pre
/// and 7).
///
/// Only a verdict becomes the installed-source refusal ([`rollback_source_refusal`],
/// which suspends automatic apply until a person reinstalls). A verification that
/// did not finish (`codesign` past the apply budget, a helper the kernel would not
/// start, the apply lock held by a sibling) says nothing about the bundle, so a
/// passing error is returned with its location and its own
/// [`crate::PASSING_REFUSAL_KEY`], and every reader retries it.
fn installed_source_refusal(installed: &Path, error: &str) -> String {
    if crate::is_passing_refusal(error) {
        format!(
            "the installed bundle at {} could not be verified just then: {error}",
            installed.display()
        )
    } else {
        rollback_source_refusal(installed, error)
    }
}

/// Whether a failed check of the staged bundle is a verdict on its bytes, the only
/// kind that may discard the stage or record a failure memo against the artifact. A
/// check that did not finish ([`crate::is_passing_refusal`]) is not: the same bytes
/// are checked again next time.
fn stage_failure_is_a_verdict(error: &str) -> bool {
    !crate::is_passing_refusal(error)
}

/// The swap's second precondition, checked before the park: can the running bundle
/// become the rollback source the swap installs at the fixed path? The boot apply
/// asks the same at step 7 and still does; asking here, while every reader is live,
/// keeps a doomed successor (which would stay the old build and refuse its target)
/// from ever parking the terminal. Strictly additive.
fn preverify_installed_rollback_source(
    installed: Option<&Path>,
    current_build: u64,
    current_commit: Option<&str>,
) -> Result<(), String> {
    preverify_installed_rollback_source_with(
        installed,
        current_build,
        current_commit,
        &verified_bundle_identity,
    )
}

/// A sealed-identity reader: `(build, commit)` of the bundle at a path, or the
/// refusal (`verified_bundle_identity` in production).
type IdentityReader = dyn Fn(&Path) -> Result<(u64, String), String>;

/// Injectable core of [`preverify_installed_rollback_source`]: `verify` is the
/// sealed-identity reader, injected like [`preverify_staged_handoff_candidate_at`]'s
/// policy reader, so a timed-out verification's refusal is testable without a signed
/// fixture bundle.
fn preverify_installed_rollback_source_with(
    installed: Option<&Path>,
    current_build: u64,
    current_commit: Option<&str>,
    verify: &IdentityReader,
) -> Result<(), String> {
    let Some(installed) = installed else {
        // `bundle::resolve` is what the apply calls, and `None` there is
        // `ApplyOutcome::NotApplicable`: the updater is inert for this process (no
        // `.app` layout, a translocated or DMG launch, or the `ATermDevBuild` mark).
        // A handoff whose successor's swap is a no-op can only end as a wrong-build
        // refusal, so say so now.
        return Err(
            "this process does not run from an installed bundle the updater may replace, so a \
             staged build cannot become its successor"
                .to_string(),
        );
    };
    // The swap's write, checked before the park: step 5 of the boot apply refuses a
    // non-writable install location, but only inside the successor after every
    // reader parked, where it reads as a structural failure. Cheap (one
    // mkdir+rmdir), so it goes first.
    install_location_check(installed)?;
    match verify(installed) {
        Ok((build, commit)) => {
            if identity_matches_running(build, &commit, current_build, current_commit) {
                Ok(())
            } else {
                Err(format!(
                    "the installed bundle at {} is sealed {build}/{commit}, which is not the \
                     running {current_build}/{}: the swap would have no verified rollback \
                     source, so no staged build can replace it",
                    installed.display(),
                    current_commit.unwrap_or("?")
                ))
            }
        }
        Err(error) => Err(installed_source_refusal(installed, &error)),
    }
}

/// What a fresh cached pass cannot vouch for when the candidate is the INSTALLED
/// ACTIVATION `build`: the operator floor ([`handoff_apply_floor_refusal_at`]), and
/// that the install path still holds it. Another window's retire
/// (`supersede_installed_activation`) may have put the running build back there
/// since the pass was cached; launching it would take a stage this attempt never
/// authorized. `installed_build` reads the build the
/// install path holds (its `Info.plist`, unauthenticated, which only ever refuses);
/// a read that fails refuses too. Either answer is a moment: the next attempt reads
/// the disk again, and the lane's reconcile retires an activation that is gone.
pub(crate) fn handoff_activation_cached_pass_refusal_at(
    staging: &Staging,
    build: u64,
    installed_build: impl FnOnce() -> Option<Result<u64, String>>,
) -> Option<String> {
    if let Some(refusal) = handoff_apply_floor_refusal_at(staging, build) {
        return Some(refusal);
    }
    match installed_build()? {
        Ok(installed) if installed == build => None,
        Ok(installed) => Some(crate::verify::mark_passing(format!(
            "the install path now holds build {installed}, not the activation {build}; not \
             handing off to it"
        ))),
        Err(error) => Some(crate::verify::mark_passing(format!(
            "could not read which build the install path holds ({error}); not handing off to \
             the activation {build}"
        ))),
    }
}

/// THE OPERATOR APPLY FLOOR, as a handoff to `build` must respect it:
/// `Some(refusal)` when `build` is below the
/// floor `staging` records — a yank the swap's gate 4b would answer by retiring
/// the stage and leaving the successor the old build. Read-only and cheap (one
/// small TOML read, no lock, no `codesign`), so it is asked on its own by an
/// ACTIVATION worker a fresh cached pass let skip its verification: the floor can
/// rise after the pass was cached (a check observes a manifest with a higher
/// `min_build`), and the cache knows nothing of it. A STAGED worker asks
/// [`handoff_cached_pass_refusal_at`], which reads the stage as well.
///
/// AND THE REVOCATION SET: a staged marker for `build` signed by a machine a roster
/// has since revoked is refused the same way. It is read off the marker, so it
/// finds a revoked stage only while the stage is still on disk — the check lane
/// retires one the moment it observes the revocation, which is why a staged
/// handoff asks whether its stage is still there.
pub(crate) fn handoff_apply_floor_refusal_at(staging: &Staging, build: u64) -> Option<String> {
    let marker = Ready::read(&staging.ready).filter(|ready| ready.build_number == build);
    handoff_floor_refusal(
        staging,
        build,
        marker
            .as_ref()
            .and_then(|ready| ready.machine_id.as_deref()),
    )
}

/// [`handoff_apply_floor_refusal_at`] for `build` signed by `machine` (the stage's
/// marker, when the caller has it in hand).
///
/// A floor that is there but could not be read refuses too, as a passing moment
/// ([`crate::manifest::Floor::read_for_gate`]): it withdraws nothing
/// it holds. Nothing parks past it, and the next attempt reads the floor again.
fn handoff_floor_refusal(staging: &Staging, build: u64, machine: Option<&str>) -> Option<String> {
    let floor = match crate::manifest::Floor::read_for_gate(&staging.floor()) {
        Ok(floor) => floor,
        Err(refusal) => return Some(format!("{refusal}; not handing off to build {build}")),
    };
    if floor.revokes(machine) {
        return Some(format!(
            "staged build {build} was signed by machine {}, which the machine roster has \
             revoked; not handing off to it",
            machine.unwrap_or("?")
        ));
    }
    let floor = floor.min_build;
    (build < floor).then(|| {
        format!(
            "build {build} is below the operator apply floor {floor} (yanked); not handing off \
             to it"
        )
    })
}

/// WHAT A CACHED PASS CANNOT VOUCH FOR, asked by a worker that skips the staged
/// candidate's full pre-verification on it: `Some(refusal)`
/// unless the stage the pass was cached for — `build` at `commit` — is still the
/// published one, not quarantined, and admitted by the floor
/// ([`handoff_apply_floor_refusal_at`]).
///
/// The floor alone is not enough: its revocation arm reads the ready marker, which
/// a revocation's retirement removes first, and a stage can also be quarantined by
/// a sibling's revert or replaced by a newer one — each would launch a successor
/// whose own gate finds no stage after every reader parked. Read-only and cheap
/// like the floor (two small TOML reads, no lock, no
/// `codesign`); the refusals are the full check's own (`read_ready`'s ladder,
/// [`crate::manifest::artifact_quarantined`]), so either lane files them alike.
pub(crate) fn handoff_cached_pass_refusal_at(
    staging: &Staging,
    build: u64,
    commit: &str,
) -> Option<String> {
    // The STAGE'S STATE — gone, unreadable, another build — is a moment, as the full
    // check files it: a sibling that consumed or replaced the
    // shared stage, or a check that retired it, says nothing about this build. Still
    // a refusal: nothing is handed off to a stage that is not there.
    let stage_moment = crate::verify::mark_passing;
    let Some(ready) = Ready::read(&staging.ready) else {
        return Some(stage_moment(if staging.ready.exists() {
            "staged ready marker is unreadable".to_string()
        } else {
            format!("staged build {build} is no longer staged; not handing off to it")
        }));
    };
    if ready.build_number != build {
        return Some(stage_moment(format!(
            "staged build {} is not the authorized build {build}",
            ready.build_number
        )));
    }
    if !ready
        .commit
        .as_deref()
        .is_some_and(|staged| crate::commit_matches(commit, staged))
    {
        return Some(format!(
            "staged build {build} is not the authorized commit {commit}; not handing off to it"
        ));
    }
    if crate::manifest::artifact_quarantined(staging, build, &ready.dmg_sha256) {
        return Some(format!(
            "staged build {build} is quarantined: it crash-looped on this machine and was reverted"
        ));
    }
    if let Some(refusal) = handoff_apply_floor_refusal_at(staging, build) {
        return Some(refusal);
    }
    // A stage whose roster authorization no check has confirmed for a day: the
    // successor's gate 4d would refuse it after every reader parked. Passing; the
    // check that confirms it ends the wait.
    if let Some(refusal) = ready.roster_authorization_stale(
        unix_now_secs(),
        aterm_update_core::pins::roster_tier_armed(),
    ) {
        return Some(refusal);
    }
    // The marker names it but its bundle is not the published one — a retirement
    // that removed the bundle first, or a sibling's publish that replaced it, or an
    // identity read that failed just then: the shared stage again, a moment, and
    // still no stage to hand off to. A bundle that really changed meets
    // the full check once the pass expires.
    (!ready.is_published_bundle(staging)).then(|| {
        stage_moment(format!(
            "the staged bundle of build {build} is no longer the published stage; not handing \
             off to it"
        ))
    })
}

/// What a handoff pre-verification holds while it runs: the apply lock (when
/// there is a staging root to take it in) and, under it, the verification
/// budget. Declared budget first so the budget closes before the lock is let go.
pub(crate) struct PreverifyGuard {
    _budget: verify::ApplyBudget,
    _lock: Option<FileLock>,
}

/// Enters a handoff pre-verification: takes `apply_lock` within
/// [`APPLY_LOCK_WAIT`], then opens [`verify::APPLY_BUDGET`] for everything the check
/// runs. The one door both pre-verifications (the staged candidate's and the
/// activation's) go through, so [`crate::HANDOFF_PREVERIFY_BOUND`] (lock wait plus
/// budget) is the bound they run under. A helper that outruns the budget is a
/// passing refusal (`verify::timed_out`), retried, never a verdict on the bundle.
pub(crate) fn enter_preverify(apply_lock: Option<&Path>) -> Result<PreverifyGuard, String> {
    let lock = apply_lock
        .map(|path| FileLock::acquire_within(path, APPLY_LOCK_WAIT))
        .transpose()
        .map_err(|error| verify::lock_wait_refusal("pre-verify lock", &error))?;
    Ok(PreverifyGuard {
        _budget: verify::ApplyBudget::start(verify::APPLY_BUDGET),
        _lock: lock,
    })
}

/// Injectable core of [`preverify_staged_handoff_candidate`], so the refusal ladder
/// is testable against a temp staging root without a signed fixture (a missing or
/// unsigned candidate must refuse before any caller could park a reader).
/// `installed` is the bundle root the swap would replace, and `read_policy` the
/// handoff-policy reader, so a test can prove it is never called on a bundle that
/// failed any check.
fn preverify_staged_handoff_candidate_at<R>(
    staging: &Staging,
    installed: Option<&Path>,
    current_build: u64,
    current_commit: Option<&str>,
    expected_build: Option<u64>,
    expected_commit: Option<&str>,
    read_policy: &dyn Fn(&Path) -> R,
) -> Result<R, String> {
    // Serialize against a concurrent publication/apply like the swap path:
    // verifying a half-published candidate proves nothing.
    //
    // Bounded, like the launch path's taker, because the seamless worker may call
    // this with the terminal's readers parked: a wedged holder (SIGSTOPped, a
    // debugger, a dead volume) must not freeze the screen. A timeout is an ordinary
    // refusal: overlap rolls back, readers resume, the stage stays armed.
    //
    // Once the lock is held the guard opens the apply's verification budget, so
    // every `codesign`, `spctl` and `PlistBuddy` below answers inside
    // `verify::APPLY_BUDGET` (the bound `crate::HANDOFF_PREVERIFY_BOUND` states).
    let _preverify = enter_preverify(Some(&staging.apply_lock))?;
    // THE SHARED STAGE'S STATE IS A MOMENT, NOT A VERDICT. The stage is per-user
    // and shared: a sibling aterm that
    // landed this update first retired its marker, one that published a newer
    // stage replaced it, and a read the kernel refused just then (`EMFILE` in a
    // Dock-launched window) reads as an unreadable marker. None of those says
    // anything about the authorized build, so each is marked passing — and the
    // install is still refused. EXACTLY these four, named here; every refusal
    // below (quarantine, the operator floor, the sealed identity, the installed
    // rollback source) keeps its verdict.
    let stage_moment = crate::verify::mark_passing;
    let ready = match read_ready(staging, current_build) {
        ReadyState::Newer(ready) => ready,
        ReadyState::NotNewer => {
            return Err(stage_moment(
                "staged build is not strictly newer than the running build".to_string(),
            ));
        }
        ReadyState::Corrupt => {
            return Err(stage_moment(
                "staged ready marker is unreadable".to_string(),
            ));
        }
        ReadyState::Absent => return Err(stage_moment("no verified update is staged".to_string())),
    };
    if let Some(expected) = expected_build
        && ready.build_number != expected
    {
        return Err(stage_moment(format!(
            "staged build {} is not the authorized build {expected}",
            ready.build_number
        )));
    }
    // A stage an older stager published after this artifact was quarantined is not a
    // candidate, however well it verifies.
    if crate::manifest::artifact_quarantined(staging, ready.build_number, &ready.dmg_sha256) {
        return Err(format!(
            "staged build {} is quarantined: it crash-looped on this machine and was reverted",
            ready.build_number
        ));
    }
    // The operator apply floor (a yank), as the swap's gate 4b reads it. The floor
    // ratchets when a manifest is observed, before any download, and only an apply
    // retires a stage below it, so a stage armed before the yank still reaches here.
    // Read-only: the apply that reaches gate 4b retires the stage. Checked before
    // the sealed-identity read (`codesign`), so a yanked stage costs none.
    if let Some(refusal) = handoff_apply_floor_refusal_at(staging, ready.build_number) {
        return Err(refusal);
    }
    // The swap's gate 4d, asked before anything parks: a stage
    // no check has confirmed against a current roster for a day waits for one, so
    // the in-session lane cannot swap what the launch just declined. Passing, and
    // before `codesign`, so the wait costs none.
    if let Some(refusal) = ready.roster_authorization_stale(
        unix_now_secs(),
        aterm_update_core::pins::roster_tier_armed(),
    ) {
        return Err(refusal);
    }
    let (staged_build, sealed_commit) = verified_bundle_identity(&staging.staged_app)?;
    // The sealed-identity rebinds the apply path enforces: the number and provenance
    // sealed into the bundle must equal the unauthenticated marker and still exceed
    // the running build.
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
    // Last: every check above is about the incoming bytes and is the more common
    // refusal; this one is about the machine's install and, when it fires, it fires
    // until a person changes the install.
    preverify_installed_rollback_source(installed, current_build, current_commit)?;
    // The policy is read here and nowhere earlier: every refusal above returned
    // before this line, so a bundle that failed its codesign policy, sealed identity
    // or authorization never has its policy read; `_lock` is still held, so the
    // stage read is the one just verified.
    Ok(read_policy(&staging.staged_app))
}

/// Upper bound on the changelog text a `ready.toml` may carry.
///
/// Every reader reaches the marker through `read_ledger_text`, which returns `None`
/// above `MAX_LEDGER_BYTES`, so an oversized changelog would make the marker
/// invisible: the stage would report success while the stage-cover check
/// re-downloaded every tick and every launch read the marker as `Corrupt`. The
/// authoritative changelog lives in the release.
const MAX_READY_CHANGELOG_CHARS: usize = 4096;

/// Appended in place of the discarded tail so a clamped changelog reads as truncated
/// rather than as a release note that stops mid-sentence.
const READY_CHANGELOG_TRUNCATED: &str = "\n… (changelog truncated)";

/// Clamps the manifest's changelog to what a marker may carry. The cut is by
/// characters, never bytes: slicing a `str` inside a multi-byte UTF-8 sequence
/// panics, and `char_indices().nth(N)` yields a char boundary by construction.
fn clamp_ready_changelog(changelog: Option<&str>) -> Option<String> {
    let text = changelog?;
    match text.char_indices().nth(MAX_READY_CHANGELOG_CHARS) {
        None => Some(text.to_string()),
        Some((cut, _)) => Some(format!("{}{READY_CHANGELOG_TRUNCATED}", &text[..cut])),
    }
}

/// Publishes one already-verified incoming bundle as a short transaction shared
/// with the apply path. The long download/extract/verification work stays under
/// `stage_lock` only, so locks nest only as `stage_lock -> apply_lock`. Apply never
/// takes `stage_lock`, so startup cannot wait on a download and the lanes cannot
/// deadlock.
fn publish_verified_stage(staging: &Staging, incoming: &Path, ready: &Ready) -> Result<(), String> {
    publish_verified_stage_within(staging, incoming, ready, BACKGROUND_LOCK_WAIT)
}

/// [`publish_verified_stage`] with the apply-lock wait named, so a test can run a
/// sibling's hold out in milliseconds rather than two minutes.
fn publish_verified_stage_within(
    staging: &Staging,
    incoming: &Path,
    ready: &Ready,
    lock_wait: std::time::Duration,
) -> Result<(), String> {
    let marker = ready.to_toml()?;
    // A marker above the shared ledger cap is absent to every reader
    // (`read_ledger_text` refuses it): status, stage coverage and the apply gate would
    // all see "nothing staged" beside a verified bundle. `clamp_ready_changelog`
    // bounds the one unbounded field; this guard stops a future field from doing the
    // same. It runs before the lock and before the old generation is invalidated, so
    // a rejected publish leaves the previous readable stage as it was.
    if u64::try_from(marker.len()).unwrap_or(u64::MAX) > crate::MAX_LEDGER_BYTES {
        return Err(format!(
            "refusing to commit a {}-byte ready marker: the ledger cap every reader \
             enforces is {} bytes, and a marker above it reads as absent",
            marker.len(),
            crate::MAX_LEDGER_BYTES
        ));
    }
    // Bounded: this runs on the checker thread, under the stage lock.
    // A wait run out is a sibling's hold, a moment: it is
    // marked passing like every other apply-lock wait, so `book_stage_failure`
    // does not back a healthy, verified build off.
    let _publish_lock = FileLock::acquire_within(&staging.apply_lock, lock_wait)
        .map_err(|error| verify::lock_wait_refusal("publish lock", &error))?;
    // UNDER THE LOCK THE CRASH-LOOP REVERT HOLDS. This
    // stage was decided before the download — minutes ago — and a revert may have
    // quarantined this very artifact since: then it is not published. The other
    // order is safe too: a revert after this publish retires the stage.
    if crate::manifest::artifact_quarantined(staging, ready.build_number, &ready.dmg_sha256) {
        return Err(format!(
            "stage refused: build {} ({}) is quarantined — it crash-looped on this machine \
             and was reverted while this check was downloading it",
            ready.build_number, ready.dmg_sha256
        ));
    }
    // …AND UNDER THE LOCK THE FLOOR'S READERS AND THE CHECK LANE'S RETIREMENT HOLD.
    // The roster generation was judged before the
    // download, minutes ago; a sibling's check may since have admitted a roster that
    // REVOKES this build's signer (or seen a yank), recorded it in the floor, and
    // found no published stage to retire. Publishing now would arm this process's
    // apply lane for a withdrawn machine's build. Either order is safe: a withdrawal
    // recorded after this publish finds the stage and retires it.
    // A floor that could not be read is no floor's word that the build is still
    // admitted: refused as a moment, and the next check publishes it.
    if let Some(why) = crate::manifest::Floor::read_for_gate(&staging.floor())
        .map_err(|refusal| format!("stage refused: {refusal}"))?
        .withdraws(ready.build_number, ready.machine_id.as_deref())
    {
        return Err(format!(
            "stage refused: {why} — withdrawn while this check was downloading it"
        ));
    }

    // Invalidate the old generation first. Lock-free status readers may briefly
    // observe "absent", but never an old marker paired with the new bundle.
    let _ = std::fs::remove_file(&staging.ready);
    let _ = std::fs::remove_dir_all(&staging.staged_app);
    std::fs::rename(incoming, &staging.staged_app)
        .map_err(|error| format!("publish staged bundle: {error}"))?;

    // The marker is the commit point and is written last, durably: it is the
    // fallback link of the recovery-proof chain with the trial identity and the
    // installed receipt (`ensure_current_trial_receipt` falls back to it), so a
    // zero-length one after a kernel panic would strand that repair.
    crate::manifest::write_durable(&staging.ready, &marker, "ready marker")
}

/// Stages a verified copy of the bundle from a downloaded (SHA-256-checked) DMG:
/// mount, `ditto` the `.app` out, verify it (codesign/Team ID/spctl), publish
/// `staged/aterm.app`, and write `ready.toml` last. The marker's presence is the
/// sole "ready" signal, so writing it last (atomic rename) means a reader never sees
/// a half-staged bundle.
///
/// [`stage_from_zip`] is preferred when the release carries a zip; both share
/// [`verify_and_publish_incoming`] and differ only in how the bundle is unpacked.
/// `roster_read_at` is when the check that authorized `manifest` began reading the
/// machine roster: the marker's roster confirmation ([`staged_ready_marker`]).
pub fn stage_from_dmg(
    staging: &Staging,
    dmg: &Path,
    manifest: &Manifest,
    expected_team: &str,
    roster_read_at: &str,
) -> Result<(), String> {
    let manifest_commit = staging_manifest_commit(manifest)?;
    ensure_private_dir(&staging.staged_dir()).map_err(|e| format!("staged dir: {e}"))?;
    // Clean up any mount a previously-killed run leaked, then mount at a fresh private
    // mountpoint under our 0700 dir (never /Volumes).
    sweep_stale_scratch(staging);
    let mountpoint = staging.root.join(format!("mnt-{}", std::process::id()));
    let mounted = Mounted::attach(dmg, &mountpoint)?;
    let src = mounted.mountpoint.join("aterm.app");
    // `symlink_metadata`, as on the zip path: the container controls this entry,
    // and a symlink would be copied into `staged/` as a link that can never be
    // applied but is published as ready.
    match std::fs::symlink_metadata(&src) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "{} on the mounted DMG is not a real directory",
                src.display()
            ));
        }
        Err(_) => return Err(format!("{} not found on mounted DMG", src.display())),
    }

    let incoming = staging.staged_dir().join("aterm.app.incoming");
    let _ = std::fs::remove_dir_all(&incoming);
    // The mounted bundle's size is what the copy will write; refuse a volume that
    // cannot hold it before a byte lands (`refuse_without_room`).
    refuse_without_room(&staging.staged_dir(), tree_bytes(&src), "the update bundle")?;
    // `ditto` (not `cp -R`) preserves extended attributes + the _CodeSignature
    // layout, so the copied bundle's signature stays valid.
    let (status, stderr) = ditto_bounded(
        &[
            src.as_os_str().to_os_string(),
            incoming.as_os_str().to_os_string(),
        ],
        "ditto",
        STAGE_UNPACK_TIMEOUT,
    )
    .inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&incoming);
    })?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(unpack_failure(
            &staging.staged_dir(),
            "ditto extract failed",
            status,
            &stderr,
        ));
    }
    // detach the DMG now; everything we need is in `incoming`.
    drop(mounted);

    verify_and_publish_incoming(
        staging,
        &incoming,
        manifest,
        &manifest_commit,
        expected_team,
        roster_read_at,
    )
}

// ---------------------------------------------------------------------------
// Bounding the zip extract: what unpacking this archive would cost, read from the
// archive's own central directory before `ditto` may run.
// ---------------------------------------------------------------------------

// Imports scoped to the zip reader below.
use std::io::{BufReader, Read, Seek, SeekFrom};

/// Hard ceiling on the uncompressed bytes an update archive may declare before
/// [`stage_from_zip`] hands it to `ditto`.
///
/// The download is capped ([`aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND`]) and
/// its SHA-256 checked against the signed manifest before extraction, but deflate
/// expands, `ditto` has no limit of its own, and one bad release is unpacked by
/// every machine on the channel at once. A full volume cannot stage the fix, so
/// refusing on the archive's declared size keeps the fleet on the previous build
/// with a stated reason.
///
/// It equals the download bound and must never sit below it: the release cutter
/// validates every asset against that constant (`UPDATER_MAX_DMG_BYTES` in
/// `aterm-release`) and has no extraction gate, so a stricter cap here would make a
/// shipped container unstageable everywhere. A cut can seal a large
/// `Contents/Resources/toolchain-seed` of already-compressed artifacts into the
/// bundle, so the cap is tied to the bound rather than guessed.
const MAX_EXTRACT_BYTES: u64 = aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND;

/// Companion ceiling on the archive's entry count, since bytes alone do not bound
/// the damage: a million empty files declare zero bytes and still cost a block and
/// an inode apiece. A real bundle has about 48 entries; this holds the
/// block-overhead worst case near 200 MiB (50k entries × one 4 KiB block).
const MAX_EXTRACT_ENTRIES: u64 = 50_000;

/// The little-endian record signatures this reader walks, as the `u32` each
/// decodes to (`PK\x05\x06` reads back as `0x0605_4b50`).
const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const CENTRAL_HEADER_SIGNATURE: u32 = 0x0201_4b50;

/// Fixed (pre-comment) length of an End Of Central Directory record.
const EOCD_FIXED_LEN: u64 = 22;
/// Maximum archive comment length, i.e. how far back from EOF the EOCD can hide.
const MAX_ZIP_COMMENT: u64 = 65_535;
/// Length of the ZIP64 EOCD locator, which sits immediately before the EOCD.
const ZIP64_LOCATOR_LEN: u64 = 20;
/// Length of the ZIP64 EOCD record through its central-directory offset field
/// (the last field this reader needs; the record may legally be longer).
const ZIP64_EOCD_LEN: u64 = 56;
/// Fixed (pre-name/extra/comment) length of one central directory file header.
const CENTRAL_HEADER_FIXED_LEN: usize = 46;
/// Header id of the ZIP64 extended information extra field.
const ZIP64_EXTRA_ID: u16 = 0x0001;

/// What an archive's own central directory says unpacking it will cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ZipExtractClaim {
    entries: u64,
    uncompressed_bytes: u64,
}

/// The three central-directory facts an end record carries, widened to the `u64`
/// the ZIP64 record uses so the classic and ZIP64 shapes share one walker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryExtent {
    entries: u64,
    offset: u64,
    size: u64,
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

fn le_u64(bytes: &[u8], at: usize) -> Option<u64> {
    let end = at.checked_add(8)?;
    Some(u64::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

/// Reads exactly `len` bytes at an absolute offset. A short read is an error: every
/// caller reads a fixed-layout record, so an early end means the record is absent.
fn read_exact_at(file: &mut std::fs::File, offset: u64, len: u64) -> Result<Vec<u8>, String> {
    let len = usize::try_from(len)
        .map_err(|_| format!("update zip record of {len} bytes does not fit in memory"))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("seek update zip to {offset}: {error}"))?;
    let mut buffer = vec![0u8; len];
    file.read_exact(&mut buffer)
        .map_err(|error| format!("read {len} bytes of update zip at {offset}: {error}"))?;
    Ok(buffer)
}

/// Locates the End Of Central Directory record and reads its classic 32-bit view of
/// the directory. Returns the record's absolute offset too, because everything the
/// directory describes must lie strictly before it.
fn find_end_of_central_directory(
    file: &mut std::fs::File,
    file_len: u64,
) -> Result<(u64, DirectoryExtent), String> {
    let window = file_len.min(EOCD_FIXED_LEN.saturating_add(MAX_ZIP_COMMENT));
    // `window <= file_len` by construction, so this cannot underflow.
    let start = file_len - window;
    let tail = read_exact_at(file, start, window)?;
    let fixed = usize::try_from(EOCD_FIXED_LEN)
        .map_err(|_| "update zip end record length does not fit in memory".to_string())?;
    let Some(highest) = tail.len().checked_sub(fixed) else {
        return Err("update zip is smaller than an end-of-central-directory record".to_string());
    };
    // The four-byte signature also occurs inside compressed data and the archive
    // comment, so a candidate is real only if its declared comment length runs
    // exactly to end of file.
    let mut found: Option<(u64, DirectoryExtent)> = None;
    for index in (0..=highest).rev() {
        let Some(record) = tail.get(index..) else {
            continue;
        };
        if le_u32(record, 0) != Some(EOCD_SIGNATURE) {
            continue;
        }
        let (Some(entries), Some(size), Some(offset), Some(comment_len)) = (
            le_u16(record, 10),
            le_u32(record, 12),
            le_u32(record, 16),
            le_u16(record, 20),
        ) else {
            continue;
        };
        if usize::from(comment_len) != record.len().saturating_sub(fixed) {
            continue;
        }
        // Two valid candidates means a decoy trailer, and nothing here can know which
        // one `ditto` will follow, so refuse. An honest archive cannot hit this by
        // accident: it needs the signature and a comment length landing exactly on
        // EOF, ~2^-48 per position over a ~64 KiB scan.
        if found.is_some() {
            return Err(
                "update zip has more than one end-of-central-directory record; refusing \
                 an archive whose trailer is ambiguous"
                    .to_string(),
            );
        }
        let index = u64::try_from(index)
            .map_err(|_| "update zip end record offset does not fit in u64".to_string())?;
        found = Some((
            start.saturating_add(index),
            DirectoryExtent {
                entries: u64::from(entries),
                offset: u64::from(offset),
                size: u64::from(size),
            },
        ));
    }
    found.ok_or_else(|| "update zip has no end-of-central-directory record".to_string())
}

/// The ZIP64 view of the directory, when the archive carries one. `Ok(None)` means
/// no ZIP64 locator (an ordinary small archive); once a locator is present every
/// further problem is a refusal, since the archive declared the 32-bit record
/// cannot describe it.
fn zip64_directory_extent(
    file: &mut std::fs::File,
    eocd_offset: u64,
) -> Result<Option<DirectoryExtent>, String> {
    let Some(locator_offset) = eocd_offset.checked_sub(ZIP64_LOCATOR_LEN) else {
        return Ok(None);
    };
    let locator = read_exact_at(file, locator_offset, ZIP64_LOCATOR_LEN)?;
    if le_u32(&locator, 0) != Some(ZIP64_LOCATOR_SIGNATURE) {
        return Ok(None);
    }
    let record_offset =
        le_u64(&locator, 8).ok_or_else(|| "update zip ZIP64 locator is truncated".to_string())?;
    let record_end = record_offset
        .checked_add(ZIP64_EOCD_LEN)
        .ok_or_else(|| "update zip ZIP64 end record offset overflows".to_string())?;
    if record_end > locator_offset {
        return Err("update zip ZIP64 end record does not lie before its locator".to_string());
    }
    let record = read_exact_at(file, record_offset, ZIP64_EOCD_LEN)?;
    if le_u32(&record, 0) != Some(ZIP64_EOCD_SIGNATURE) {
        return Err("update zip ZIP64 locator does not point at a ZIP64 end record".to_string());
    }
    let (Some(entries), Some(size), Some(offset)) = (
        le_u64(&record, 32),
        le_u64(&record, 40),
        le_u64(&record, 48),
    ) else {
        return Err("update zip ZIP64 end record is truncated".to_string());
    };
    Ok(Some(DirectoryExtent {
        entries,
        offset,
        size,
    }))
}

/// The uncompressed size from a central directory entry's ZIP64 extended
/// information field, which carries only the values that overflowed 32 bits, the
/// uncompressed size first, so for an entry whose 32-bit size saturated it is at
/// data offset 0.
fn zip64_uncompressed_size(extra: &[u8]) -> Option<u64> {
    let mut at = 0usize;
    while at < extra.len() {
        let id = le_u16(extra, at)?;
        let len = le_u16(extra, at.checked_add(2)?)?;
        let data_at = at.checked_add(4)?;
        let data_end = data_at.checked_add(usize::from(len))?;
        let data = extra.get(data_at..data_end)?;
        if id == ZIP64_EXTRA_ID {
            return le_u64(data, 0);
        }
        // `data_end >= at + 4`, so this is strictly increasing and terminates.
        at = data_end;
    }
    None
}

/// Walks the central directory, summing uncompressed sizes and counting entries.
/// The caps are enforced inside the walk, so a directory claiming a billion entries
/// costs a bounded read.
fn central_directory_claim(
    file: &mut std::fs::File,
    extent: DirectoryExtent,
) -> Result<ZipExtractClaim, String> {
    file.seek(SeekFrom::Start(extent.offset))
        .map_err(|error| format!("seek update zip central directory: {error}"))?;
    let mut reader = BufReader::new(file);
    // The caller proved the extent lies inside a file the download cap bounded, so
    // this cannot fail on any host aterm runs on; a conversion rather than a cast so
    // a narrower target refuses instead of wrapping.
    let mut remaining = usize::try_from(extent.size)
        .map_err(|_| "update zip central directory does not fit in memory".to_string())?;
    let mut claim = ZipExtractClaim {
        entries: 0,
        uncompressed_bytes: 0,
    };
    while remaining > 0 {
        let Some(after_fixed) = remaining.checked_sub(CENTRAL_HEADER_FIXED_LEN) else {
            return Err("update zip central directory ends inside a file header".to_string());
        };
        let mut header = [0u8; CENTRAL_HEADER_FIXED_LEN];
        reader
            .read_exact(&mut header)
            .map_err(|error| format!("read update zip central directory header: {error}"))?;
        if le_u32(&header, 0) != Some(CENTRAL_HEADER_SIGNATURE) {
            return Err("update zip central directory is not a chain of file headers".to_string());
        }
        let (Some(uncompressed), Some(name_len), Some(extra_len), Some(comment_len)) = (
            le_u32(&header, 24),
            le_u16(&header, 28),
            le_u16(&header, 30),
            le_u16(&header, 32),
        ) else {
            return Err("update zip central directory header is truncated".to_string());
        };
        // Three `u16` lengths sum to at most 196_605; no overflow on any target.
        let variable = usize::from(name_len) + usize::from(extra_len) + usize::from(comment_len);
        if variable > after_fixed {
            return Err(
                "update zip central directory entry runs past its declared extent".to_string(),
            );
        }
        let mut variable_bytes = vec![0u8; variable];
        reader
            .read_exact(&mut variable_bytes)
            .map_err(|error| format!("read update zip central directory entry: {error}"))?;
        let uncompressed = if uncompressed == u32::MAX {
            // 0xFFFFFFFF is the ZIP64 escape. It is also a legal literal size, but a
            // ~4 GiB member alone exceeds MAX_EXTRACT_BYTES, so reading it as the
            // escape can only be stricter.
            let extra = variable_bytes
                .get(usize::from(name_len)..)
                .and_then(|rest| rest.get(..usize::from(extra_len)))
                .ok_or_else(|| "update zip central directory entry is truncated".to_string())?;
            zip64_uncompressed_size(extra).ok_or_else(|| {
                "update zip entry escapes to a ZIP64 size but carries no ZIP64 extra field"
                    .to_string()
            })?
        } else {
            u64::from(uncompressed)
        };
        claim.entries = claim.entries.saturating_add(1);
        claim.uncompressed_bytes = claim.uncompressed_bytes.saturating_add(uncompressed);
        if claim.entries > MAX_EXTRACT_ENTRIES {
            return Err(format!(
                "update zip declares more than {MAX_EXTRACT_ENTRIES} entries; \
                 refusing to extract it"
            ));
        }
        if claim.uncompressed_bytes > MAX_EXTRACT_BYTES {
            return Err(format!(
                "update zip declares at least {} uncompressed bytes, over the \
                 {MAX_EXTRACT_BYTES}-byte extraction cap; refusing to extract it",
                claim.uncompressed_bytes
            ));
        }
        // `variable <= after_fixed` was just checked.
        remaining = after_fixed - variable;
    }
    if claim.entries != extent.entries {
        return Err(format!(
            "update zip central directory holds {} entries but its end record declares {}",
            claim.entries, extent.entries
        ));
    }
    Ok(claim)
}

/// Reads the archive's central directory and refuses anything claiming to unpack
/// to more than [`MAX_EXTRACT_BYTES`] or [`MAX_EXTRACT_ENTRIES`].
///
/// Hand-rolled: it reads four fixed-layout records and never inflates a byte, so
/// the update path takes no zip dependency.
///
/// Fails closed: every malformed, truncated, ambiguous or unreadable directory is a
/// refusal. The entry count the walk observes must equal the count the end record
/// declares, so a directory describing a different archive than its trailer cannot
/// slip past.
///
/// It bounds what the archive declares, in the record any extractor must consult.
/// It is not a proof about `ditto`: an extractor streaming local headers could be
/// fed members the directory never mentions, bounded only by the download cap times
/// deflate's expansion (closing that needs a watchdog on the extract directory). It
/// catches the realistic failure, a release built wrong; a hostile signer already
/// ships the binary.
fn checked_zip_extraction_claim(zip: &Path) -> Result<ZipExtractClaim, String> {
    let mut file = std::fs::File::open(zip).map_err(|error| format!("open update zip: {error}"))?;
    let file_len = file
        .metadata()
        .map_err(|error| format!("stat update zip: {error}"))?
        .len();
    let (eocd_offset, classic) = find_end_of_central_directory(&mut file, file_len)?;
    let extent = match zip64_directory_extent(&mut file, eocd_offset)? {
        Some(zip64) => {
            // Where the 32-bit record is not saturated it must agree with the 64-bit
            // one; a disagreement means two archives are described, and neither is
            // trusted.
            let disagrees = (classic.entries != u64::from(u16::MAX)
                && classic.entries != zip64.entries)
                || (classic.size != u64::from(u32::MAX) && classic.size != zip64.size)
                || (classic.offset != u64::from(u32::MAX) && classic.offset != zip64.offset);
            if disagrees {
                return Err(
                    "update zip ZIP64 and 32-bit end-of-central-directory records disagree"
                        .to_string(),
                );
            }
            zip64
        }
        None => {
            // A sentinel with no ZIP64 record behind it is a shape this reader cannot
            // describe: a refusal, since the saturated field is the one the cap is
            // measured against.
            if classic.entries == u64::from(u16::MAX)
                || classic.size == u64::from(u32::MAX)
                || classic.offset == u64::from(u32::MAX)
            {
                return Err(
                    "update zip saturates its 32-bit end record but carries no ZIP64 one"
                        .to_string(),
                );
            }
            classic
        }
    };
    let directory_end = extent
        .offset
        .checked_add(extent.size)
        .ok_or_else(|| "update zip central directory extent overflows".to_string())?;
    if directory_end > eocd_offset {
        return Err("update zip central directory does not lie before its end record".to_string());
    }
    central_directory_claim(&mut file, extent)
}

/// Stages a verified copy of the bundle from a downloaded (SHA-256-checked) ZIP:
/// `ditto -x -k` extract, then the same verification and publication as
/// [`stage_from_dmg`] ([`verify_and_publish_incoming`]).
///
/// Preferred over the DMG path because `hdiutil attach` fails with ENXIO in an
/// orphaned process (after a seamless overlap update the surviving aterm's launchd
/// job may be gone, and DiskImages' `com.apple.hdiejectd` lookup in that dead
/// bootstrap domain cannot succeed). `ditto` needs no XPC service.
///
/// The trust model is unchanged: the zip's bytes are checked against the manifest
/// digest first, and the extracted bundle goes through the same
/// codesign/Team-ID/sealed-identity gate. The digest does not bound expansion, so
/// the unpack is gated on [`checked_zip_extraction_claim`] first (see
/// [`MAX_EXTRACT_BYTES`]). `roster_read_at` is as for [`stage_from_dmg`].
pub fn stage_from_zip(
    staging: &Staging,
    zip: &Path,
    manifest: &Manifest,
    expected_team: &str,
    roster_read_at: &str,
) -> Result<(), String> {
    let manifest_commit = staging_manifest_commit(manifest)?;
    // Bound the unpack before any filesystem work: `ditto` has no size limit, so the
    // archive's central directory is the only place to learn the cost, and refusing
    // here writes nothing.
    let claim = checked_zip_extraction_claim(zip)?;
    crate::log(&format!(
        "update zip declares {} entries / {} uncompressed bytes \
         (caps {MAX_EXTRACT_ENTRIES} / {MAX_EXTRACT_BYTES})",
        claim.entries, claim.uncompressed_bytes
    ));
    ensure_private_dir(&staging.staged_dir()).map_err(|e| format!("staged dir: {e}"))?;
    // Reclaim any extract dir (and the DMG lane's mounts) a killed run leaked
    // (`sweep_stale_scratch`).
    sweep_stale_scratch(staging);
    // Extract into a fresh scratch dir, not `staged/`: the archive's root entry is
    // `aterm.app` (`--keepParent`), which would collide with the published
    // `staged/aterm.app`.
    let extract = staging
        .staged_dir()
        .join(format!("zx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&extract);
    // The archive's declared size against the volume's free space
    // (`refuse_without_room`).
    refuse_without_room(
        &staging.staged_dir(),
        claim.uncompressed_bytes,
        "the update zip",
    )?;
    std::fs::create_dir_all(&extract).map_err(|e| format!("create zip extract dir: {e}"))?;
    // `ditto -x -k` (not `unzip`) restores extended attributes and the sequestered
    // resource forks, so the extracted bundle's codesign seal stays intact.
    let (status, stderr) = ditto_bounded(
        &[
            "-x".into(),
            "-k".into(),
            zip.as_os_str().to_os_string(),
            extract.as_os_str().to_os_string(),
        ],
        "ditto zip extract",
        STAGE_UNPACK_TIMEOUT,
    )
    .inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&extract);
    })?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&extract);
        return Err(unpack_failure(
            &staging.staged_dir(),
            "ditto zip extract failed",
            status,
            &stderr,
        ));
    }
    let src = extract.join("aterm.app");
    // `symlink_metadata`, not `is_dir`: the archive controls this entry, and a
    // symlink named `aterm.app` would follow through `is_dir` and be renamed into
    // `staged/` as a link. The apply path and status reader refuse a symlinked
    // staged bundle, so it could never install, but it would be published as
    // "ready", a retry loop nobody can clear. Refuse it here.
    match std::fs::symlink_metadata(&src) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            let _ = std::fs::remove_dir_all(&extract);
            return Err(format!(
                "{} in the update zip is not a real directory",
                src.display()
            ));
        }
        Err(_) => {
            let _ = std::fs::remove_dir_all(&extract);
            return Err(format!("{} not found in the update zip", src.display()));
        }
    }

    let incoming = staging.staged_dir().join("aterm.app.incoming");
    let _ = std::fs::remove_dir_all(&incoming);
    // A rename inside `staged/` (same directory, so necessarily the same volume)
    // moves the bundle without re-copying it — nothing is re-materialized, so no
    // extended attribute or signature byte can be lost in transit.
    if let Err(error) = std::fs::rename(&src, &incoming) {
        let _ = std::fs::remove_dir_all(&extract);
        return Err(format!("move extracted bundle into place: {error}"));
    }
    // The `__MACOSX` sidecar and the scratch dir have served their purpose.
    let _ = std::fs::remove_dir_all(&extract);

    verify_and_publish_incoming(
        staging,
        &incoming,
        manifest,
        &manifest_commit,
        expected_team,
        roster_read_at,
    )
}

/// The manifest's git commit in canonical form: the one identity every stage binds
/// the signed bundle back to, shared so neither container path can accept a
/// manifest the other would refuse.
fn staging_manifest_commit(manifest: &Manifest) -> Result<String, String> {
    canonical_release_commit(manifest.commit.as_deref()).ok_or_else(|| {
        "release manifest lacks a clean, valid git commit; refusing to stage".to_string()
    })
}

/// Verifies an already-extracted incoming bundle and publishes it as the staged
/// update. This is the whole security gate of staging: codesign policy, the
/// manifest↔sealed CFBundleVersion rebind, the sealed-commit rebind, then the ready
/// marker written last. The single copy shared by [`stage_from_dmg`] and
/// [`stage_from_zip`], so the container can never change what is proved.
///
/// Every failure path removes `incoming`, so a refused candidate leaves no
/// half-staged bundle.
///
/// `roster_read_at` (RFC3339; empty for a clock that could not be read) is when the
/// check that authorized `manifest` began reading the machine roster it judged by:
/// the marker's roster confirmation ([`staged_ready_marker`]).
fn verify_and_publish_incoming(
    staging: &Staging,
    incoming: &Path,
    manifest: &Manifest,
    manifest_commit: &str,
    expected_team: &str,
    roster_read_at: &str,
) -> Result<(), String> {
    // Verify the extracted bundle before publishing it (tiered: full Developer-ID
    // check when a Team ID is pinned, else structural-only — see the crate trust model).
    if let Err(e) = verify::verify_bundle_policy(incoming, expected_team) {
        let _ = std::fs::remove_dir_all(incoming);
        return Err(format!("staged bundle failed verification: {e}"));
    }
    // Bind the (unauthenticated) manifest build_number to the number inside the
    // signed bundle, or a manifest could claim a high build_number while pointing at
    // an old genuine signed bundle (downgrade/replay via repo write). CFBundleVersion
    // is codesign-sealed, so reading it after verify_bundle is sound.
    let bundle_build = match verify::bundle_build_number(incoming) {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_dir_all(incoming);
            return Err(format!("staged bundle build number unreadable: {e}"));
        }
    };
    if bundle_build != manifest.build_number {
        let _ = std::fs::remove_dir_all(incoming);
        return Err(format!(
            "staged bundle CFBundleVersion {bundle_build} != manifest build_number {} — \
             refusing a manifest/bundle mismatch",
            manifest.build_number
        ));
    }
    let sealed_commit = match verify::bundle_git_commit(incoming) {
        Ok(commit) => commit,
        Err(error) => {
            let _ = std::fs::remove_dir_all(incoming);
            return Err(format!("staged bundle commit unreadable: {error}"));
        }
    };
    if !sealed_commit_matches(Some(manifest_commit), &sealed_commit) {
        let _ = std::fs::remove_dir_all(incoming);
        return Err(format!(
            "staged bundle ATermGitCommit {sealed_commit:?} does not match manifest commit"
        ));
    }
    let team = verify::team_id(incoming).unwrap_or_else(|_| expected_team.to_string());

    let ready = staged_ready_marker(
        manifest,
        manifest_commit,
        team,
        &now_rfc3339(),
        roster_read_at,
    );
    publish_verified_stage(staging, incoming, &ready)
}

/// The ready marker a verified stage of `manifest` is published under, written at
/// `staged_at` by a check that began reading the machine roster at
/// `roster_read_at` (both RFC3339; empty when the clock could not be read).
///
/// THE ROSTER CONFIRMATION IS THE ROSTER'S READ, NOT THE PUBLISH: the download,
/// extract and `codesign` after the read may take hours, and stamping at the
/// publish would add all of it to the window a revocation can hide in
/// (`publish_verified_stage` re-reads only revocations already on the floor). An
/// empty `roster_read_at` stamps nothing, which the swap reads as unconfirmed.
fn staged_ready_marker(
    manifest: &Manifest,
    manifest_commit: &str,
    team_id: String,
    staged_at: &str,
    roster_read_at: &str,
) -> Ready {
    Ready {
        build_number: manifest.build_number,
        version: manifest.version.clone(),
        // Store canonical lowercase hex (like `dmg_sha256`) so `commit_matches` and any
        // display get a clean value; `None` stays `None`.
        commit: Some(manifest_commit.to_string()),
        // The manifest's `sha256` (the DMG digest) stays the staged artifact's
        // identity even when the bytes arrived as a zip: the release manifest, the
        // ready marker, the failed-candidate memo and the apply-time expected-artifact
        // handoff all key on it.
        dmg_sha256: manifest.sha256.to_ascii_lowercase(),
        team_id,
        roster_confirmed_at: (!roster_read_at.is_empty()).then(|| roster_read_at.to_string()),
        staged_at: staged_at.to_string(),
        // Clamped, never copied verbatim: the changelog is publisher text with no
        // length bound, and an oversized one would push the marker past
        // `MAX_LEDGER_BYTES`, making it unreadable (`clamp_ready_changelog`).
        changelog: clamp_ready_changelog(manifest.changelog.as_deref()),
        // The pair the apply gate re-checks against the ratcheted floor. Both are in
        // the manifest's signed bytes, so this carries the stage-time authorization
        // forward without trusting anything new.
        machine_id: manifest.machine_id.clone(),
        roster_seq: manifest.roster_seq,
    }
}

/// Publishes the post-swap truth after `ready.toml` is retired but before exec. The
/// new build number is intentional: an overlapping OLD reader sees a ledger
/// mismatch and reconciles neutrally, while the re-exec'd NEW process sees an exact
/// installed/activating outcome.
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

/// The boot swap's refusal while ANOTHER update trial holds the install — a
/// sibling install's, or an older build's that never confirmed — MARKED PASSING:
/// the trial confirms, reverts, or is escaped (`escape_wedged_foreign_trial`) by
/// itself, and none of that is a verdict on the staged build's bytes. Unmarked, a
/// candidate's parent would file it STRUCTURAL against a healthy release.
fn unconfirmed_trial_refusal(armed_build: u64) -> String {
    crate::verify::mark_passing(format!(
        "update trial for build {armed_build} is still unconfirmed"
    ))
}

/// The one outcome shape that means "a newer build is staged and this launch
/// REFUSED to apply it" — the entire answer to an operator's "the build is
/// staged, so why is it not running?".
///
/// `NotApplicable`/`NoUpdate` are the common every-launch case and not refusals, so
/// recording them would rewrite `status.toml` on every launch. `ReExecFailed` is a
/// failure that already recorded its own status line;
/// [`crate::health::Health::record_apply_failure`] clears refusals by design, so
/// routing a failure through the refusal slot would invert that ordering.
fn boot_apply_refusal_reason(outcome: &ApplyOutcome) -> Option<&str> {
    match outcome {
        ApplyOutcome::Deferred(reason) => Some(reason),
        ApplyOutcome::NotApplicable | ApplyOutcome::NoUpdate | ApplyOutcome::ReExecFailed(_) => {
            None
        }
    }
}

/// Applies a staged update if it is ready and strictly newer. On success this
/// re-execs and never returns. See the module and crate docs for the contract.
///
/// The body is `apply_staged_if_ready_inner`; this wrapper records every refusal
/// the boot lane can return in one place (`status.toml`, the health ledger, the app
/// log), so a lane refusing on every cold launch is observable, like
/// [`crate::record_apply_refusal`] on the in-session lane.
///
/// Recording happens here, after the inner call returned: the apply lock (and
/// `check_boot_health`'s) are released by then, so the health ledger lock is never
/// taken underneath them. Do not move the writes to each return.
pub fn apply_staged_if_ready(
    current_build: u64,
    current_commit: Option<&str>,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target_is_this_build: bool,
    witness: crate::TrialWitness,
) -> ApplyOutcome {
    let outcome = apply_staged_if_ready_inner(
        current_build,
        current_commit,
        handoff_fds,
        handoff_env,
        handoff_target_is_this_build,
        witness,
    );
    if let Some(reason) = boot_apply_refusal_reason(&outcome) {
        crate::warn(&format!("boot apply refused: {reason}"));
        if let Some(staging) = Staging::resolve() {
            record_boot_apply_refusal(&staging, current_build, reason);
        }
    }
    outcome
}

/// Book one boot-lane refusal: the health ledger's standing refusal and
/// `status.toml`, after every lock the lane took is let go.
///
/// A WAIT FOR A ROSTER CONFIRMATION (gate 4d) also restarts the
/// stage's pending clock ([`crate::health::Health::restart_pending_update`]). That
/// clock counts from the stage's first sighting, so across the days the app was
/// closed, and the overdue notice ("a newer build has waited an hour") would fire at
/// the first checker cycle of every such launch, before or beside the check that
/// confirms the stage within seconds. Restarted here, the notice says so only if the
/// wait outlasts its hour (law L5); the check that confirms restarts it again
/// ([`confirm_roster_authorization`]), for the lane that then installs it.
fn record_boot_apply_refusal(staging: &Staging, current_build: u64, reason: &str) {
    crate::health::Health::record_apply_refusal(&staging.health(), current_build, reason);
    crate::status::record(
        staging,
        current_build,
        &format!("boot apply refused: {reason}"),
    );
    if reason.contains(crate::manifest::ROSTER_UNCONFIRMED_KEY)
        && let Some(waiting) = Ready::read(&staging.ready)
    {
        crate::health::Health::restart_pending_update(&staging.health(), waiting.build_number);
    }
}

fn apply_staged_if_ready_inner(
    current_build: u64,
    current_commit: Option<&str>,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target_is_this_build: bool,
    witness: crate::TrialWitness,
) -> ApplyOutcome {
    // The swap's own cost, for the image that pays it: on a download lane the parent
    // launches the current binary, so this image dittos the bundle and `execve`s, and
    // `exec_preserving_handoff_fds` never returns to a timer in the GUI's
    // `main_entry`. During a handoff every millisecond here is spent with the
    // outgoing process's readers parked.
    let apply_started = std::time::Instant::now();
    let reexec_nonce = take_reexec_nonce();
    // Consume/clear child authority while startup is single-threaded, but do not
    // let malformed environment suppress crash-loop health observation/revert.
    // Its verdict is applied only after the mandatory boot-health lane below.
    let expected_artifact = take_expected_artifact();
    // Resolve the private staging root once per launch: boot-health, the re-exec
    // guard and the swap path share it. Each resolve chmods the 0700 dir
    // synchronously before the first frame, so one resolve keeps that off the launch.
    let staging = Staging::resolve();
    let reexec_authority = classify_reexec_authority(staging.as_ref(), reexec_nonce);

    // 1. Boot-health check first, on every launch (re-exec or manual relaunch): if a
    //    sentinel is armed for this build, count the launch and, if the build is
    //    crash-looping (no health checkpoint across MAX_BOOT_ATTEMPTS launches),
    //    revert to the retained OLD bundle and re-exec it. It runs regardless of the
    //    re-exec env (set only on the first post-swap launch), so manual relaunches of
    //    a crashing build still accrue attempts.
    if let Some(s) = &staging
        && let Some(outcome) = check_boot_health(
            s,
            current_build,
            current_commit,
            handoff_fds,
            handoff_env,
            handoff_target_is_this_build,
            witness,
        )
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
    // We are the authorized candidate of a seamless handoff (the outgoing process's
    // target names this build): an activation successor, or a download successor
    // already swapped and re-exec'd. Everything above still ran (the re-exec nonce
    // and stamp consumed, the boot-trial launch counted, startup authority settled),
    // but nothing below may swap: re-exec'ing a newer stage would be a build the
    // parent did not authorize, and it would refuse the target and drop the adopted
    // PTYs. The newer stage keeps for the successor's own apply lane (or the next
    // launch). `NoUpdate` is not a refusal, so the ledger stays quiet.
    if handoff_target_is_this_build {
        crate::log(&format!(
            "boot apply: build {current_build} is the handoff's authorized target; any newer \
             stage is left for the successor's own apply lane (or the next launch)"
        ));
        return ApplyOutcome::NoUpdate;
    }
    // 2. Must be a real installed bundle.
    let Some(b) = bundle::resolve() else {
        return ApplyOutcome::NotApplicable;
    };
    // Reuse the staging resolved once at the top (no second resolve/chmod).
    let Some(staging) = staging else {
        return ApplyOutcome::NotApplicable;
    };

    // 3–4. The pre-lock peek, the lock, the work that must hold it, and the decision
    //    made under it: one function ([`apply_lock_then_ready`]) that the Tier-1
    //    test `two_processes_arming_one_build_swap_it_once` drives with a sibling's
    //    whole apply injected while this launch waits for the lock.
    //
    // Bounded, unlike every other taker of this lock: we are before the window, so
    // waiting here is a terminal that has not appeared. Every legitimate holder
    // returns in milliseconds (publish is remove+rename+write) or is itself bounded
    // (verification by the apply budget, a cross-volume copy by its ceiling). A
    // holder that outlasts this is wedged, and we start on the build we have.
    let (_lock, _budget, ready) = match apply_lock_then_ready(
        &staging,
        current_build,
        || {
            // A sibling's hold past the wait is a moment, marked so: a seamless
            // successor that returns this stays the old build and refuses its
            // target, and the parent must read that as "try again later", not as
            // the new bytes' verdict.
            FileLock::acquire_within(&staging.apply_lock, APPLY_LOCK_WAIT)
                .map_err(|e| ApplyOutcome::Deferred(crate::verify::lock_wait_refusal("lock", &e)))
        },
        || {
            // Everything from here to the swap is verification work on the launch path.
            // One ceiling over all of it (the guard restores any outer budget on drop).
            let budget = crate::verify::ApplyBudget::start(crate::verify::APPLY_BUDGET);
            settle_trials_under_apply_lock(&staging, &b.app_root, current_build, current_commit)?;
            Ok(budget)
        },
    ) {
        Ok(held) => held,
        Err(outcome) => return outcome,
    };
    if let Some(expected) = expected_artifact.as_ref()
        && !ready_matches_expected(&ready, expected)
    {
        return ApplyOutcome::Deferred(format!(
            "staged update no longer matches authorized handoff artifact {} {} {}",
            expected.build, expected.commit, expected.dmg_sha256
        ));
    }

    // 4b/4c/4d. The floor's withdrawals (a yank, a revoked signer), the published
    // stage, and the age of its roster confirmation.
    if let Some(outcome) = stage_admission_gates(&staging, current_build, &ready, unix_now_secs()) {
        return outcome;
    }

    // 5. Can we write the install location? Checked before the more expensive
    //    re-verification, so a persistently non-writable install (e.g. an
    //    admin-owned /Applications) does not re-verify the stage on every launch.
    if let Err(refusal) = install_location_check(&b.app_root) {
        crate::status::record(
            &staging,
            current_build,
            if crate::is_passing_refusal(&refusal) {
                "deferred: install location could not be written just then"
            } else {
                "deferred: install location not writable"
            },
        );
        return ApplyOutcome::Deferred(refusal);
    }

    // 6. Apply-time re-verification (TOCTOU defence), tiered like stage time.
    // `verified_bundle_identity` starts with symlink_metadata, so policy/build/
    // commit can never authenticate a target while we later rename the symlink.
    let (staged_build, sealed_commit) = match verified_bundle_identity(&staging.staged_app) {
        Ok(identity) => identity,
        // A verification that did not finish keeps the stage: discarding answers a
        // verdict on the bytes, and a `codesign` out of the apply budget is not one
        // (discarding would cost a re-download). The swap re-verifies at the next
        // attempt anyway, so keeping it trusts nothing.
        Err(error) if !stage_failure_is_a_verdict(&error) => {
            crate::warn(&format!(
                "staged bundle re-verification did not finish: {error}; keeping the stage \
                 for the next attempt"
            ));
            crate::status::record(
                &staging,
                current_build,
                "deferred: staged bundle re-verification did not finish (kept)",
            );
            return ApplyOutcome::Deferred(format!("re-verify: {error}"));
        }
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
    // 6b. Re-bind the codesign-sealed CFBundleVersion at apply time too: the
    //     strictly-newer gate above trusts ready.toml's build_number, which is
    //     unauthenticated local state. The number sealed into the staged bundle must
    //     equal it and still exceed the running build, so a swapped-in
    //     older-but-genuine bundle with a rewritten marker is caught.
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

    // 7-pre. On a cross-volume install only, ask early whether the installed bundle
    // can be the rollback source at all. The authoritative answer is re-asked after
    // the candidate is prepared (the swap trusts nothing older than the exchange),
    // but cross-volume preparation is a ≤120 s `ditto` plus a five-helper re-verify,
    // wasted on every launch of an install that cannot verify. On the same-volume
    // path preparation is two renames, and an early pass would add five helper
    // spawns to every launch (see `prepare_fixed_swap_candidate`).
    if !same_volume(&staging.staged_app, &b.app_root)
        && let Err(error) = verified_bundle_identity(&b.app_root)
    {
        return ApplyOutcome::Deferred(format!(
            "current installed rollback source is not verified: {}",
            installed_source_refusal(&b.app_root, &error)
        ));
    }
    // 7. Prepare and verify NEW at the fixed destination-volume recovery path before
    // the point of no return. One atomic exchange then puts NEW at installed and OLD
    // at that fixed path; every process-crash cut is discoverable.
    let prepared = match prepare_fixed_swap_candidate(
        &staging,
        &b.app_root,
        &ready,
        current_build,
        (staged_build, &sealed_commit),
    ) {
        Ok(prepared) => prepared,
        Err(error) => return ApplyOutcome::Deferred(error),
    };

    // The atomic exchange makes the current canonical bundle the sole rollback
    // source. Prove OLD is the signed, sealed running build immediately before
    // arming and swapping (after any slow cross-volume copy): a corrupt or replaced
    // install must never become crash-recovery authority.
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
            // The same refusal the seamless lane mints, remedy included; only a
            // verdict carries the remedy (a check that did not finish is retried).
            return ApplyOutcome::Deferred(format!(
                "current installed rollback source is not verified: {}",
                installed_source_refusal(&b.app_root, &error)
            ));
        }
    };
    // Preserve only a receipt that is authority for the exact sealed OLD bundle just
    // rechecked. A well-formed but stale receipt is unsigned local state and must not
    // be resurrected after an inverse swap.
    let previous_receipt = previous_receipt_for_sealed_old(&staging, old_build, &old_commit);

    // 7b. The candidate must be able to start. Every check so far is cryptographic,
    // and the crash-loop sentinel runs inside the new build's own `main`, so a bundle
    // dyld or Gatekeeper rejects before `main` would be swapped in and never
    // reverted. Start it once here, while OLD is installed and backing out is free.
    if let Err(error) = crate::verify::probe_bundle_starts(&prepared.fixed, ready.build_number) {
        recover_prepared_candidate(&prepared, &staging);
        // A probe that could not run to an answer (timed out, or the kernel would
        // not start it) is not a candidate that cannot start: no failure memo, whose
        // backoff would suppress re-staging healthy bytes.
        if stage_failure_is_a_verdict(&error) {
            crate::manifest::FailedMark::record_stage_failure(
                &staging.failed(),
                ready.build_number,
                &ready.dmg_sha256,
                unix_now_secs(),
            );
        }
        return ApplyOutcome::Deferred(format!("staged candidate cannot start: {error}"));
    }

    // 8. Arm exact crash-loop authority only after fixed NEW is fully verified and
    // immediately before the atomic swap. A crash after this point has one
    // deterministic pre-swap shape: installed=OLD, fixed=NEW, ready+trial exact.
    let sentinel = match prepare_trial(&staging, &ready, &b.app_root) {
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
            clear_trial_identity(&staging);
            recover_prepared_candidate(&prepared, &staging);
            return ApplyOutcome::Deferred(error);
        }
    };

    // Persist exact install provenance only after the fixed-path swap. The atomic
    // receipt replacement retires the previous artifact's proof at exactly the next
    // complete transaction.
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
        return roll_back_unreceipted_swap(
            &prepared,
            &retained,
            &b.app_root,
            &sentinel,
            &staging,
            &error,
        );
    }
    // Don't let it re-apply on the next launch. This retirement cannot touch the
    // fixed rollback sibling; it removes only ready + the duplicate staged NEW.
    staging.retire_published();
    record_activating_status(&staging, &ready);

    // 9. Write the single-use re-exec nonce stamp (0600, in our 0700 dir) before exec
    //    so the post-swap guard can prove this launch is our re-exec. If no nonce can
    //    be made, pass the marker "1": the guard finds no matching stamp and takes the
    //    safe normal path (a no-op, since `ready` is already removed).
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
    crate::log(&format!(
        "applied update {} in {} ms{} → exec into the new binary",
        ready.version,
        apply_started.elapsed().as_millis(),
        if handoff_fds.is_empty() {
            ""
        } else {
            " (with the outgoing process's readers parked)"
        }
    ));
    // `b.exe` is the canonical path we launched from; after the in-place swap it
    // resolves to the NEW binary at the same location.
    let new_exe = &b.exe;
    let mut reexec = boot_reexec_command(new_exe, &reexec_value, current_build, handoff_env);
    let err = exec_preserving_handoff_fds(&mut reexec, handoff_fds); // never returns on success
    // exec itself failed (not a later crash): the nonce stamp is now stale, so remove
    // it; then restore the OLD bundle from the retained rollback source, drop the
    // failed new build, and disarm the sentinel (below).
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
    // Suppress the build whose exec failed so the next check does not re-download
    // and re-apply it every interval. We still hold `ready` here.
    //
    // Budgeted, not permanent: this is one failed `execve` after a successful
    // rollback, and an exec can fail for reasons unrelated to the artifact (ENOMEM,
    // text-file-busy, a Gatekeeper scan). The crash-loop revert below stays
    // permanent, because there the build proved itself bad.
    crate::manifest::FailedMark::record_stage_failure(
        &staging.failed(),
        ready.build_number,
        &ready.dmg_sha256,
        unix_now_secs(),
    );
    clear_trial_identity(&staging);
    // Booked in the failure ledger: a swap rolled back because the new binary
    // would not exec is an apply that failed, and the wrapper leaves
    // `ReExecFailed` out of the refusal slot.
    crate::health::Health::record_apply_failure(
        &staging.health(),
        current_build,
        ready.build_number,
        &format!(
            "re-exec of build {} failed (rolled back): {err}",
            ready.build_number
        ),
    );
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

/// Which boot-health lane a counted launch takes ([`boot_health_lane`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BootHealthLane {
    /// Count the launch and prove the trial's rollback now (`ensure_current_trial_receipt`:
    /// the installed bundle's identity, the retained predecessor, the receipt).
    FullProof,
    /// Count the launch only; the rollback proof runs at the health checkpoint.
    CountOnly,
}

/// The first counted launch of a handoff target is count-only. The image that
/// swapped the bundle proved both identities and recorded the receipt just before
/// stamping the re-exec, and re-proving them here would run codesign inside the
/// parked window. So the live parent's authorized target
/// (`handoff_target_is_this_build`) on the trial's first counted launch skips the
/// proof: the launch is still counted (a crash still climbs toward the revert), and
/// the same proof runs at `confirm_boot_health`, after Commit, where a healthy
/// trial is disarmed anyway. Any later counted launch proves in full.
fn boot_health_lane(handoff_target: bool, attempts_after_launch: u32) -> BootHealthLane {
    if handoff_target && attempts_after_launch <= 1 {
        BootHealthLane::CountOnly
    } else {
        BootHealthLane::FullProof
    }
}

#[cfg(test)]
mod boot_health_lane_tests {
    use super::{BootHealthLane, boot_health_lane};

    /// Only a handoff target's FIRST counted launch skips the proof; a relaunch,
    /// a sibling launch, or any non-handoff launch proves in full.
    #[test]
    fn only_a_handoff_targets_first_counted_launch_is_count_only() {
        assert_eq!(boot_health_lane(true, 0), BootHealthLane::CountOnly);
        assert_eq!(boot_health_lane(true, 1), BootHealthLane::CountOnly);
        assert_eq!(boot_health_lane(true, 2), BootHealthLane::FullProof);
        assert_eq!(boot_health_lane(false, 0), BootHealthLane::FullProof);
        assert_eq!(boot_health_lane(false, 1), BootHealthLane::FullProof);
    }
}

/// On every launch: if the boot sentinel is armed for `current_build`, counts this
/// launch and, if the build has failed to confirm across [`MAX_BOOT_ATTEMPTS`]
/// launches (a crash loop), reverts to the retained OLD bundle and re-execs it.
/// Returns `Some` only in the revert path (which normally execs away); `None` to
/// continue booting the current build.
fn check_boot_health(
    staging: &Staging,
    current_build: u64,
    current_commit: Option<&str>,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target: bool,
    witness: crate::TrialWitness,
) -> Option<ApplyOutcome> {
    check_boot_health_with_lock_wait(
        staging,
        current_build,
        current_commit,
        handoff_fds,
        handoff_env,
        handoff_target,
        witness,
        APPLY_LOCK_WAIT,
        bundle::resolve_layout,
    )
}

/// [`check_boot_health`] with the apply-lock wait and the install resolver named
/// (`resolve_installed`: `bundle::resolve_layout` in production).
#[allow(clippy::too_many_arguments)]
fn check_boot_health_with_lock_wait(
    staging: &Staging,
    current_build: u64,
    current_commit: Option<&str>,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target: bool,
    witness: crate::TrialWitness,
    lock_wait: std::time::Duration,
    resolve_installed: impl FnOnce() -> Option<bundle::Bundle>,
) -> Option<ApplyOutcome> {
    let sentinel = boot_sentinel(staging);
    let armed_for_this_build =
        |sentinel: &Sentinel| matches!(sentinel.read_state(), Some((b, _)) if b == current_build);
    // A WINDOW LAUNCH OF A BUILD A HEADLESS LAUNCH SETTLED may put it back on
    // trial: see `window_unproven_path`. Peeked here,
    // decided under the lock.
    let retrial = witness == crate::TrialWitness::Window
        && !armed_for_this_build(&sentinel)
        && window_unproven_names(staging, current_build);
    // Cheap non-mutating early-out: nothing armed for us. A sentinel for another
    // build belongs to that build and is never "stale" authority for this process.
    if !armed_for_this_build(&sentinel) && !retrial {
        return None;
    }
    let apply_lock = match FileLock::acquire_within(&staging.apply_lock, lock_wait) {
        Ok(lock) => lock,
        // A re-trial that cannot take the lock is not attempted: nothing is armed,
        // and this launch goes on exactly as it would have without the record.
        Err(error) if retrial => {
            crate::warn(&format!(
                "build {current_build} was settled by a headless launch, and this window \
                 launch could not take the apply lock to put it back on trial ({}); it is \
                 not counted against it",
                crate::verify::lock_wait_refusal("health lock", &error)
            ));
            return None;
        }
        Err(error) => {
            return Some(ApplyOutcome::Deferred(crate::verify::lock_wait_refusal(
                "health lock",
                &error,
            )));
        }
    };
    // Revalidate after acquiring the transaction lock. Another process may have
    // armed a newer build between the cheap peek and this point.
    if !armed_for_this_build(&sentinel) && !retrial {
        return None;
    }
    // Finalizing a swap a previous run already made — installs nothing — so the dev
    // mark must not strand an armed trial. See `bundle::resolve_layout`.
    let b = resolve_installed()?;
    if !armed_for_this_build(&sentinel)
        && !rearm_window_unproven_trial(
            staging,
            &sentinel,
            current_build,
            &b.app_root,
            unix_now_secs(),
        )
    {
        return None;
    }
    // NOT OUR TRIAL: a same-build process launched from a bundle the trial did not
    // swap must not count, revert or disarm it — see `trial_owned_by`.
    if !trial_owned_by(staging, &b.app_root) {
        crate::log(&format!(
            "boot sentinel armed for build {current_build} belongs to another install of \
             this build; this launch from {} is not counted against it",
            b.app_root.display()
        ));
        return None;
    }
    // Count the launch first. The attempt count measures launches observed, a fact
    // about this boot; if it waited on the recovery proof below, a trial whose proof
    // fails every boot would never reach MAX_BOOT_ATTEMPTS, never revert or confirm,
    // and block every future apply.
    //
    // A burst is one launch: another process of the trial build starting within
    // `BOOT_LAUNCH_BURST_WINDOW` of the last counted one (`launch_is_burst`).
    //
    // AN OVERLAPPING HANDOFF TARGET IS NOT A RELAUNCH: the
    // authorized handoff target of this build, launched while an earlier launch of
    // this trial is still running — see `count_trial_launch`. Any other launch
    // counts, however many earlier launchers live, or this escape is lost.
    //
    // A HEADLESS LAUNCH OF A TRIAL A WINDOW HAS LAUNCHED IS NO EVIDENCE either way
    // — see `count_trial_launch`.
    if let Err(error) = count_trial_launch(
        staging,
        &sentinel,
        current_build,
        TrialLauncher::this_process(),
        handoff_target,
        witness,
    ) {
        return Some(ApplyOutcome::Deferred(error));
    }
    let attempts_after = sentinel.read_state().map_or(0, |(_, attempts)| attempts);
    if boot_health_lane(handoff_target, attempts_after) == BootHealthLane::CountOnly
        && !sentinel.should_revert(current_build, MAX_BOOT_ATTEMPTS)
    {
        crate::log(&format!(
            "boot sentinel for build {current_build}: launch {attempts_after} counted — the \
             first launch of the live parent's handoff target; the rollback proof runs at \
             the health checkpoint, off the park"
        ));
        return Some(ApplyOutcome::NotApplicable);
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
                // Budget exhausted. Revert whenever a verified predecessor sits at
                // the fixed rollback path: this trial can no longer be confirmed and
                // the build is failing to reach its checkpoint. The proof that failed
                // is a conjunction (rollback ∧ sealed identity ∧ trial marker ∧
                // receipt), and only its first term decides whether a revert can be
                // performed, so re-derive that term alone and revert on it.
                if let Ok(rollback) = ensure_fixed_rollback(&b.app_root, current_build) {
                    crate::warn(&format!(
                        "trial recovery proof failed {MAX_BOOT_ATTEMPTS} launches in a row \
                         ({error}), but the retained predecessor at {} verifies — reverting \
                         to it rather than leaving an unconfirmable build installed",
                        rollback.path.display()
                    ));
                    return Some(revert_to_rollback(
                        &b,
                        staging,
                        &sentinel,
                        current_build,
                        rollback,
                        RollbackHandoff {
                            fds: handoff_fds,
                            env: handoff_env,
                        },
                        apply_lock,
                    ));
                }
                // No verified predecessor: reverting is unavailable. Staying armed
                // would only guarantee no future update can apply, while this build
                // demonstrably boots. Disarm, keep running, and make the reason loud
                // and durable.
                let disarm = sentinel.confirm();
                crate::health::Health::record_apply_failure(
                    &staging.health(),
                    current_build,
                    // The trial under recovery IS this build: the sentinel is armed
                    // for `current_build` and every arm above revalidated it.
                    current_build,
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
        RollbackHandoff {
            fds: handoff_fds,
            env: handoff_env,
        },
        apply_lock,
    ))
}

/// The crash-loop revert's re-exec of the restored build: the launch's own
/// argv `args`, pinned to its mode exactly as the boot swap's re-exec is
/// ([`window_successor_args`]), and the handoff authority variables the GUI's
/// prearm consumed.
///
/// THE PIN. The revert runs before the launch is served, so a window started as
/// `aterm --window` from a shell still has that terminal on stdin: handed the
/// stripped argv alone, the one-binary router would read it as a SESSION and
/// bring the restored build up nested in that terminal. Every build a revert can
/// restore postdates the router (d2a29b6c7, an ancestor of v0.21.0) and strips
/// the pin: a rollback slot holds the build that applied the update, and installs
/// older than the router cannot update.
fn rollback_reexec_command(
    restored_exe: &Path,
    args: impl Iterator<Item = std::ffi::OsString>,
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Command {
    let mut reexec = Command::new(restored_exe);
    for (key, value) in handoff_env {
        reexec.env(key, value);
    }
    reexec.args(window_successor_args(args));
    reexec
}

/// Captured descriptors and the authority environment restored together when
/// rollback re-execs the predecessor during an inherited handoff.
struct RollbackHandoff<'a> {
    fds: &'a [i32],
    env: &'a [(std::ffi::OsString, std::ffi::OsString)],
}

/// The trialed build is crash-looping: swaps the retained OLD bundle back over the
/// install, discards the failed new build, sentinel and staged bundle, and re-execs
/// the restored OLD binary. A missing or temporarily failing inverse swap keeps all
/// recovery authority and returns Deferred; NEW may still be installed.
fn revert_to_rollback(
    b: &bundle::Bundle,
    staging: &Staging,
    sentinel: &Sentinel,
    current_build: u64,
    verified_rollback: VerifiedRollback,
    handoff: RollbackHandoff<'_>,
    _apply_lock: FileLock,
) -> ApplyOutcome {
    let RollbackHandoff {
        fds: handoff_fds,
        env: handoff_env,
    } = handoff;
    // check_boot_health acquired apply_lock before observing/incrementing. Keep that
    // same CLOEXEC guard through rollback exec; this function never takes stage_lock.
    if !sentinel.should_revert(current_build, MAX_BOOT_ATTEMPTS) {
        return ApplyOutcome::NotApplicable;
    }
    let VerifiedRollback {
        path: rb,
        build: restored_build,
    } = verified_rollback;
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
    // Poison the crash-looping build so the next check does not re-download and
    // re-apply it into another crash/revert loop. Its DMG sha was recorded beside the
    // sentinel at arm time (we no longer hold `ready`); guard on the build match.
    //
    // Permanent, unlike the re-exec failure path: the build failed to confirm boot
    // health MAX_BOOT_ATTEMPTS times in a row on this machine. The escape is a newer
    // build or a re-publish under a different digest, which clear the memo by key.
    if let Some(t) = crate::manifest::FailedMark::read(&staging.trial())
        && t.build_number == current_build
    {
        // Quarantine, not `record`: `FailedMark::suppresses` treats a zero
        // `retry_after` as already elapsed, so a "forever" memo would be ignored.
        // It also goes to its own ledger, since `failed.toml` holds one artifact
        // and the next stage event of another build overwrites it.
        crate::manifest::quarantine_artifact(staging, t.build_number, &t.sha256);
    }
    // OLD is restored at the install; failed NEW sits at rb. Disarm succeeded, so
    // transaction cleanup cannot leave an armed trial without recovery metadata.
    let _ = std::fs::remove_dir_all(&rb);
    crate::manifest::InstalledReceipt::clear(&staging.installed_receipt());
    clear_trial_identity(staging);
    staging.retire_published(); // the staged build is bad — never re-apply it
    // Booked in the failure ledger: an update that landed and was taken back is a
    // failed apply, so `update status`, the pull-down and the persistent notice see
    // it.
    //
    // The floor, stated: the activation lane refuses an installed bundle below the
    // operator floor, while the revert restores whatever verified predecessor the
    // fixed path holds (a crash loop is worse than a yank). The ledger, the status
    // line and the notice name the breach and the remedy rather than disagree
    // silently.
    let floor = crate::manifest::Floor::read(&staging.floor()).min_build;
    let breach = (restored_build < floor).then(|| {
        format!(
            " — the restored build {restored_build} is below the operator floor {floor} \
             (a yanked release); reinstall the current release"
        )
    });
    let breach = breach.as_deref().unwrap_or("");
    crate::health::Health::record_apply_failure(
        &staging.health(),
        current_build,
        current_build,
        &format!(
            "crash-loop revert: build {current_build} failed to confirm boot health \
             {MAX_BOOT_ATTEMPTS} times in a row and was reverted to build \
             {restored_build}{breach}"
        ),
    );
    crate::status::record(
        staging,
        current_build,
        &format!("reverted crash-looping update to the previous build {restored_build}{breach}"),
    );
    crate::warn(&format!(
        "reverted a crash-looping update to the previous build {restored_build}{breach}"
    ));
    // Re-exec the restored OLD binary as a fresh boot: no re-exec env, no sentinel
    // (already cleared), nothing staged.
    //
    // The handoff authority rides along: the handoff descriptors are kept open
    // across this exec, and the authority variables the GUI's prearm consumed are
    // restored, so a reverted overlap candidate re-validates the inherited handoff
    // and refuses it on target identity through its logged path.
    let mut reexec = rollback_reexec_command(&b.exe, std::env::args_os().skip(1), handoff_env);
    if !handoff_fds.is_empty() {
        crate::warn(&format!(
            "the crash-loop revert of build {current_build} happened inside a seamless \
             handoff; the restored build {restored_build} will refuse the handoff and the \
             parked outgoing process keeps its sessions"
        ));
    }
    let err = exec_preserving_handoff_fds(&mut reexec, handoff_fds);
    crate::warn(&format!("re-exec of restored build failed: {err}"));
    ApplyOutcome::ReExecFailed(err.to_string())
}

/// The running build takes the install path back from an activation a newer stage
/// supersedes: the transaction behind [`crate::supersede_installed_activation`].
///
/// When build N's handoff fails, a launched-lane candidate has already boot-applied
/// it, so N is installed with its boot trial armed and unconfirmed and this
/// process's build M at the fixed rollback path. A newer verified stage N' then has
/// no way in: M cannot swap it (the installed bundle is not M, so there is no
/// rollback source), a launch of the install path is a launch of N (which refuses
/// to boot-apply past itself as the authorized target), and N's trial owns the only
/// rollback slot.
///
/// The transaction, under the apply lock:
///
/// 1. the replacement is real: `ready.toml` names exactly `newer_build`, strictly
///    newer than the activation and at or above the operator floor, and the staged
///    bundle's sealed identity (the full codesign policy in production) is the
///    marker's, before anything moves;
/// 2. the installed bundle is exactly the activation the GUI latched, sealed, and
///    newer than the running build;
/// 3. the boot sentinel is armed for it, the trial is this install's
///    ([`trial_owned_by`]) and its marker authorizes exactly those bytes
///    ([`trial_authorizes_candidate`]), and no other window's launch of it is still
///    running (another window's candidate of N could still Commit onto the bundle
///    this would delete);
/// 4. the fixed rollback path holds this process's own build, sealed build and
///    commit ([`identity_matches_running`]): the one image known to run;
/// 5. the inverse exchange ([`restore_rollback`], the crash-loop revert's own): M
///    back at the install path, N at the fixed path;
/// 6. the sentinel is disarmed (`confirm`), never observed: no launch of N is
///    counted, and this process never executes N (nor does any window's launch of
///    it still run: step 3);
/// 7. cleanup in `revert_to_rollback`'s order: the retired N at the fixed path, a
///    receipt that no longer names the installed bundle, the trial marker.
///
/// Anything short of all of 1–4 refuses and moves nothing, as a
/// [`crate::SupersedeRefusal`] saying whether the refusal was about the moment or
/// the disk's shape. A disarm that fails after the exchange is not a refusal: the
/// retire happened, and [`crate::SupersededActivation::trial_disarm_deferred`] says
/// the crash cut is left for the next boot apply.
///
/// Invariants kept:
///
/// * the only rename is the atomic inverse exchange; a crash after it and before
///   the disarm (installed = OLD, fixed = NEW, trial armed for NEW, receipt NEW's)
///   is the "crash right after inverse rollback" shape
///   [`recover_abandoned_preswap_trial_if_exact`] finishes on the next boot apply,
///   which the next candidate runs before it swaps anything;
/// * the rollback copy of M is never destroyed: it becomes the installed, running
///   image;
/// * no trial is re-armed here; N' gets a fresh one from [`prepare_trial`] when the
///   ordinary lane swaps it, with M as its rollback;
/// * N is not quarantined: its failures were handoffs, not a boot it could not
///   survive, and it is older than the stage replacing it.
pub(crate) fn supersede_installed_activation(
    current_build: u64,
    current_commit: &str,
    activation: (u64, &str),
    newer_build: u64,
) -> Result<crate::SupersededActivation, crate::SupersedeRefusal> {
    use crate::SupersedeRefusal::{ForNow, Settled};
    let installed = bundle::resolve().ok_or_else(|| {
        Settled(
            "this process does not run from an installed bundle the updater may replace"
                .to_string(),
        )
    })?;
    let staging = Staging::resolve()
        .ok_or_else(|| Settled("no private staging root is available".to_string()))?;
    install_location_check(&installed.app_root).map_err(|refusal| {
        if crate::is_passing_refusal(&refusal) {
            ForNow(refusal)
        } else {
            Settled(refusal)
        }
    })?;
    // A background caller, so the background lane's lock wait: a cold launch holding
    // the lock for its boot apply is bounded well inside it. A lock held past that is
    // a moment, not a verdict on the disk.
    let _lock = FileLock::acquire_within(&staging.apply_lock, BACKGROUND_LOCK_WAIT)
        .map_err(|error| ForNow(format!("apply lock: {error}")))?;
    // One ceiling over the three verifications, inside a launch's own lock wait
    // (`APPLY_LOCK_WAIT`), so a cold launch queued behind this still opens on time.
    let _budget = verify::ApplyBudget::start(verify::APPLY_BUDGET);
    supersede_activation_locked(
        &staging,
        &installed.app_root,
        (current_build, current_commit),
        activation,
        newer_build,
        &verified_bundle_identity,
    )
}

/// A bundle's SEALED identity reader — [`verified_bundle_identity`] in production.
type SealedIdentityReader = dyn Fn(&Path) -> Result<(u64, String), String>;

/// The body of [`supersede_installed_activation`], run under the apply lock with the
/// sealed-identity reader injected (a fixture bundle carries no signature). Every
/// exchange, sentinel transition, marker and receipt write is the shipping code.
fn supersede_activation_locked(
    staging: &Staging,
    installed: &Path,
    (current_build, current_commit): (u64, &str),
    (activation_build, activation_commit): (u64, &str),
    newer_build: u64,
    identity: &SealedIdentityReader,
) -> Result<crate::SupersededActivation, crate::SupersedeRefusal> {
    // Every refusal names the build that stays and says which kind it is: a moment
    // (`for_now`: a verification that failed or ran out of budget, a stage being
    // replaced, a rename the kernel refused) or the disk's shape (`settled`).
    let stays =
        |why: String| format!("the installed build {activation_build} stays in place: {why}");
    let for_now = |why: String| -> Result<crate::SupersededActivation, crate::SupersedeRefusal> {
        Err(crate::SupersedeRefusal::ForNow(stays(why)))
    };
    let settled = |why: String| -> Result<crate::SupersededActivation, crate::SupersedeRefusal> {
        Err(crate::SupersedeRefusal::Settled(stays(why)))
    };
    // 1. The replacement, verified like any stage before anything moves.
    if newer_build <= activation_build {
        return settled(format!("build {newer_build} is not newer than it"));
    }
    let ready = match read_ready(staging, current_build) {
        ReadyState::Newer(ready) if ready.build_number == newer_build => ready,
        // A marker naming another build, or none, is a stage being replaced or
        // retired during the lock wait: a moment. A release that has left the disk is
        // not offered again; the GUI offers only what it last observed there.
        _ => {
            return for_now(format!(
                "no verified stage of build {newer_build} is on disk"
            ));
        }
    };
    let floor = match crate::manifest::Floor::read_for_gate(&staging.floor()) {
        Ok(floor) => floor.min_build,
        Err(refusal) => return for_now(refusal),
    };
    if newer_build < floor {
        return settled(format!(
            "the staged build {newer_build} is below the operator floor {floor} (yanked)"
        ));
    }
    if !is_non_symlink_dir(&staging.staged_app) {
        return for_now(format!(
            "the staged build {newer_build} has no bundle on disk"
        ));
    }
    match identity(&staging.staged_app) {
        Ok((build, commit))
            if build == newer_build && sealed_commit_matches(ready.commit.as_deref(), &commit) => {}
        Ok((build, _)) => {
            return for_now(format!(
                "the staged bundle is sealed {build}, not the marker's build {newer_build}"
            ));
        }
        // A verification error is a moment: the full codesign policy includes a
        // Gatekeeper assessment over the network, and the budget over the three reads
        // is `verify::APPLY_BUDGET` at utility QoS.
        Err(error) => return for_now(format!("the staged bundle does not verify: {error}")),
    }
    // 2. The activation, exactly the bytes the lane latched.
    let installed_commit = match identity(installed) {
        Ok((build, commit))
            if build == activation_build
                && build > current_build
                && crate::commit_matches(&commit, activation_commit) =>
        {
            commit
        }
        Ok((build, _)) => {
            return settled(format!(
                "the bundle at the install path is sealed {build}, not the activation"
            ));
        }
        Err(error) => return for_now(format!("the installed bundle does not verify: {error}")),
    };
    // 3. Its trial: armed, this install's, and exact.
    let sentinel = boot_sentinel(staging);
    let trial_launches = match sentinel.read_state() {
        Some((armed, launches)) if armed == activation_build => launches,
        Some((armed, _)) => {
            return settled(format!(
                "the boot trial is armed for build {armed}, not for it"
            ));
        }
        None => {
            return settled(
                "no boot trial is armed for it, so nothing retained the build under it".to_string(),
            );
        }
    };
    if !trial_owned_by(staging, installed) {
        return settled("its boot trial belongs to another install".to_string());
    }
    if !trial_authorizes_candidate(staging, activation_build, &installed_commit) {
        return settled("its trial marker does not name these bytes".to_string());
    }
    // 3b. No other window's launch of it is still running. Several windows each run
    // their own candidate of the one activation, and a live one can still prove and
    // Commit: retired
    // under it, it would land on a deleted bundle whose trial nobody watches, with an
    // older build at the install path. The launcher record knows which launches of the
    // trial live; a headless one hands nothing off. A moment: the next look offers the
    // retire again.
    let this = std::process::id();
    if let Some(live) = read_trial_launchers(staging, activation_build)
        .into_iter()
        .find(|recorded| {
            recorded.witness == crate::TrialWitness::Window
                && recorded.launcher.pid != this
                && recorded.launcher.is_alive()
        })
    {
        return for_now(format!(
            "a window's launch of it (pid {}) is still running, and its switch may still \
             land on it",
            live.launcher.pid
        ));
    }
    // 4. The rollback is this process.
    let fixed = rollback_path(installed);
    if !same_volume(&fixed, installed) {
        return settled("no retained predecessor sits beside it".to_string());
    }
    let (restored_build, restored_commit) = match identity(&fixed) {
        Ok((build, commit))
            if identity_matches_running(build, &commit, current_build, Some(current_commit)) =>
        {
            (build, commit)
        }
        Ok((build, commit)) => {
            return settled(format!(
                "the retained predecessor is {build}/{commit}, not the running \
                 {current_build}/{current_commit}"
            ));
        }
        Err(error) => {
            return for_now(format!("the retained predecessor does not verify: {error}"));
        }
    };
    // 5. The inverse exchange: the running build back at the install path. One
    // atomic swap, so a refused one moved nothing.
    if let Err(error) = restore_rollback(&fixed, installed) {
        return for_now(error);
    }
    // 6. Disarmed, never observed. A failure here leaves the crash cut
    // `recover_abandoned_preswap_trial_if_exact` finishes on the next boot apply, so
    // the stage still applies next: the retire happened and is reported as one, with
    // the disarm deferred.
    if let Err(error) = sentinel.confirm() {
        let deferred = format!(
            "build {activation_build}'s boot trial could not be disarmed ({error}); the next \
             boot apply finishes it"
        );
        crate::warn(&format!(
            "retired build {activation_build}'s activation for build {newer_build}, but \
             {deferred}"
        ));
        crate::status::record(
            staging,
            current_build,
            &format!(
                "retired the installed build {activation_build} (its switch kept failing) for \
                 the staged build {newer_build}; build {restored_build} is back at the install \
                 path, and {deferred}"
            ),
        );
        return Ok(crate::SupersededActivation {
            activation_build,
            restored_build,
            newer_build,
            trial_launches,
            trial_disarm_deferred: Some(deferred),
        });
    }
    // 7. CLEANUP, in `revert_to_rollback`'s order — safe only now that the disarm
    // succeeded.
    let _ = remove_path_no_follow(&fixed);
    if !crate::manifest::InstalledReceipt::read(&staging.installed_receipt())
        .is_some_and(|receipt| receipt.matches_sealed(restored_build, &restored_commit))
    {
        crate::manifest::InstalledReceipt::clear(&staging.installed_receipt());
    }
    clear_trial_identity(staging);
    crate::status::record(
        staging,
        current_build,
        &format!(
            "retired the installed build {activation_build} (its switch kept failing) for the \
             staged build {newer_build}; build {restored_build} is back at the install path"
        ),
    );
    crate::log(&format!(
        "superseded the unconfirmed activation of build {activation_build} with the verified \
         stage of build {newer_build}: build {restored_build} is back at the install path, the \
         boot trial was disarmed at {trial_launches} counted launch(es) without counting another"
    ));
    Ok(crate::SupersededActivation {
        activation_build,
        restored_build,
        newer_build,
        trial_launches,
        trial_disarm_deferred: None,
    })
}

fn confirm_trial_health_after_proof(
    staging: &Staging,
    current_build: u64,
    prove: impl FnOnce() -> Result<(), String>,
) -> bool {
    let sentinel = boot_sentinel(staging);
    // Only disarm a sentinel for this build; never clobber one a concurrent apply
    // just armed for a different (newer) build.
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
    clear_trial_identity(staging);
    // THIS is where an apply is known to have SUCCEEDED — swapped in, re-execed,
    // and confirmed its own boot health — and therefore the only honest place to
    // clear the apply-failure streak. Clearing it at submit would keep
    // `apply_failures` from ever reaching `PERSISTENT_AFTER`.
    crate::health::Health::record_apply_success(&staging.health());
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
    let _ = std::fs::remove_file(trial_window_launch_path(staging));
    clear_window_unproven(staging);
    let _ = remove_path_no_follow(&rollback_path(app_root));
    reclaim_swapped_out_orphan(staging);
    true
}

/// A ready marker means a newer publisher now owns staged_app. Only reclaim the
/// swapped-out orphan when no published generation exists.
fn reclaim_swapped_out_orphan(staging: &Staging) {
    if !staging.ready.exists() {
        let _ = std::fs::remove_dir_all(&staging.staged_app);
    }
}

/// A HEADLESS confirmation of a trial no window is waiting on (see
/// [`window_unproven_path`]): the trial is settled for the headless path — the
/// sentinel disarms and the success is booked, exactly as a window's confirmation
/// does, so later updates are not held — but the retained predecessor stays, and
/// the record says which window launch puts the build back on trial. A record that
/// cannot be written (or a trial with no identity to key it by) settles the whole
/// trial, as before the record existed. Runs under the apply lock.
fn confirm_headless_keeping_predecessor(
    staging: &Staging,
    current_build: u64,
    app_root: &Path,
    prove: impl FnOnce() -> Result<(), String>,
) -> bool {
    // The trial's identity, read before the settle clears it.
    let digest = crate::manifest::FailedMark::read(&staging.trial())
        .filter(|trial| trial.build_number == current_build)
        .and_then(|trial| canonical_digest(&trial.sha256));
    if !confirm_trial_health_after_proof(staging, current_build, prove) {
        return false;
    }
    let _ = std::fs::remove_file(trial_window_launch_path(staging));
    let record = digest.map(|digest| WindowUnproven {
        build: current_build,
        digest,
        at: unix_now_secs(),
        install_root: app_root.to_path_buf(),
    });
    match record.as_ref().map(|record| record.record(staging)) {
        Some(Ok(())) => crate::log(&format!(
            "build {current_build}'s trial was settled by a headless launch, which proves \
             nothing about its windows: the previous build stays at {} until a window of \
             build {current_build} confirms it, and a window launch within {} h puts it \
             back on trial",
            rollback_path(app_root).display(),
            WINDOW_TRIAL_HEADLESS_WAIT.as_secs() / 3600
        )),
        failed => {
            crate::warn(&format!(
                "build {current_build}'s trial was settled by a headless launch, and its \
                 window lane could not be kept open ({}); the retained predecessor is \
                 removed as before",
                match failed {
                    Some(Err(error)) => format!("record: {error}"),
                    _ => "the trial had no identity to key it by".to_string(),
                }
            ));
            clear_window_unproven(staging);
            let _ = remove_path_no_follow(&rollback_path(app_root));
        }
    }
    reclaim_swapped_out_orphan(staging);
    true
}

/// A window's settle over the real proof, as [`confirm_boot_health_at`]'s window arm
/// runs it, for the tests that hold the apply lock themselves.
#[cfg(test)]
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
        // The hard health-commit gate: plist fields become sealed evidence only after
        // policy verification; the fixed OLD must itself be verified and strictly
        // older; the receipt's build+commit+digest must equal the active trial.
        ensure_current_trial_receipt(staging, app_root, current_build, current_commit).map(|_| ())
    })
}

/// How many launches the boot sentinel has counted for `build` (0 when it is armed
/// for another build, or not armed at all).
#[must_use]
pub(crate) fn trial_launch_count(build: u64) -> u32 {
    Staging::resolve().map_or(0, |staging| trial_launch_count_at(&staging, build))
}

/// [`trial_launch_count`] against an explicit staging root.
fn trial_launch_count_at(staging: &Staging, build: u64) -> u32 {
    boot_sentinel(staging)
        .read_state()
        .filter(|(armed, _)| *armed == build)
        .map_or(0, |(_, attempts)| attempts)
}

/// The longest a trial-launch stamp can be: `<pid> <seconds> <micros>\n` is at
/// most 10 + 1 + 20 + 1 + 20 + 1 bytes.
const TRIAL_LAUNCH_STAMP_MAX_BYTES: u64 = 64;

/// See [`crate::stamp_trial_launch`].
pub(crate) fn stamp_trial_launch(stamp: &Path) -> std::io::Result<()> {
    let this = TrialLauncher::this_process()
        .ok_or_else(|| std::io::Error::other("the kernel will not name this process"))?;
    write_trial_launch_stamp(stamp, this)
}

/// Write the name `launch` is counted under ([`TrialLauncher::name`]) to
/// `stamp`, once: created, never followed and never replaced. A stamp already
/// there is this launch's own, written before its boot apply re-exec'd it:
/// `execve` keeps the pid and the kernel's birth stamp.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateTrialGiveBack",
        action = "Launch",
        reason = "The parent's launch touches no ledger; its candidate stamps, here, the name its boot may count under, before its boot apply can count, and the parent's SettledUnnamed reads it back."
    )
)]
fn write_trial_launch_stamp(stamp: &Path, launch: TrialLauncher) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let opened = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(stamp);
    let mut file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error),
    };
    let name = launch.name();
    file.write_all(format!("{} {} {}\n", name.pid, name.started.0, name.started.1).as_bytes())
}

/// The launch `stamp` names ([`write_trial_launch_stamp`]), or `None` when there
/// is none, it is not a regular file, or it is not exactly one name: nothing is
/// given back by it, and a launch it counted stays counted.
fn read_trial_launch_stamp(stamp: &Path) -> Option<LaunchName> {
    use std::os::unix::fs::OpenOptionsExt as _;

    // Never followed, and never waited on: a FIFO at the name cannot park the
    // handoff worker.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(stamp)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(TRIAL_LAUNCH_STAMP_MAX_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    let mut fields = text.strip_suffix('\n')?.split(' ');
    let name = LaunchName {
        pid: fields.next()?.parse().ok()?,
        started: (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?),
    };
    fields.next().is_none().then_some(name)
}

/// See [`crate::forgive_stamped_trial_launch`].
pub(crate) fn forgive_stamped_trial_launch(target_build: u64, stamp: &Path) -> bool {
    let Some(staging) = Staging::resolve() else {
        return false;
    };
    let installed = bundle::resolve_layout();
    forgive_stamped_trial_launch_at(
        &staging,
        target_build,
        stamp,
        installed.as_ref().map(|b| b.app_root.as_path()),
        APPLY_LOCK_WAIT,
    )
}

/// [`forgive_stamped_trial_launch`] against an explicit staging root and
/// install: the launch the stamp names, given back by that name exactly as a
/// candidate a pid names is ([`forgive_trial_launch_of_at`]).
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "SettledUnnamed",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
fn forgive_stamped_trial_launch_at(
    staging: &Staging,
    target_build: u64,
    stamp: &Path,
    installed: Option<&Path>,
    lock_wait: std::time::Duration,
) -> bool {
    let Some(name) = read_trial_launch_stamp(stamp) else {
        crate::log(&format!(
            "the launched successor of build {target_build} left no whole launch stamp; a trial \
             launch it counted stays counted"
        ));
        return false;
    };
    forgive_trial_launch_of_at(
        staging,
        target_build,
        name.pid,
        Some(name.started),
        installed,
        lock_wait,
    )
}

/// See [`crate::forgive_trial_launch_of`].
pub(crate) fn forgive_trial_launch_of(
    target_build: u64,
    pid: u32,
    started: Option<(i64, i64)>,
) -> bool {
    let Some(staging) = Staging::resolve() else {
        return false;
    };
    let installed = bundle::resolve_layout();
    forgive_trial_launch_of_at(
        &staging,
        target_build,
        pid,
        started,
        installed.as_ref().map(|b| b.app_root.as_path()),
        APPLY_LOCK_WAIT,
    )
}

/// See [`crate::forgive_own_trial_launch`].
pub(crate) fn forgive_own_trial_launch(build: u64) -> bool {
    TrialLauncher::this_process()
        .is_some_and(|this| forgive_trial_launch_of(build, this.pid, Some(this.started)))
}

/// Gives back the one launch of `target_build` that process `pid` (born at
/// `started`, when known) counted, if it is still on the count
/// ([`Sentinel::forgive_launch_of`]), against an explicit staging root and install.
/// Both ends of a handoff may ask (the successor on its way out, the parent
/// settling the same death) and the launch is given back once; no ask can take an
/// earlier kept launch or a sibling window's launch for its own. `true` when a
/// launch was given back, or owed back to the next holder of the apply lock
/// ([`owe_trial_launch`]).
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "ExitsByRule",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "Settled",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "SiblingGivesBack",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "SiblingSettled",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateTrialGiveBack",
        action = "Killed",
        reason = "A death the candidate did not choose (its parent's post-grant kill, the machine's) runs no ledger code; its parent's settle that follows is Settled."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateTrialGiveBack",
        action = "SiblingDies",
        reason = "A sibling's death it did not choose runs no ledger code; its parent's settle by name that may follow is SiblingSettled."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateTrialGiveBack",
        action = "SiblingCrashes",
        reason = "A crash the trial keeps runs no ledger code, and nothing gives it back."
    )
)]
fn forgive_trial_launch_of_at(
    staging: &Staging,
    target_build: u64,
    pid: u32,
    started: Option<(i64, i64)>,
    installed: Option<&Path>,
    lock_wait: std::time::Duration,
) -> bool {
    let sentinel = boot_sentinel(staging);
    match trial_give_back_lock(staging, &sentinel, target_build, installed, lock_wait) {
        GiveBackLock::Held(_apply_lock) => {
            pay_owed_trial_launches(staging, &sentinel);
            give_back_launch_of(&sentinel, target_build, pid, started)
        }
        GiveBackLock::NotOurs => false,
        // ANOTHER ATERM HOLDS THE LOCK past this wait: the
        // give-back is OWED, never dropped — a launch nobody gives back moves a
        // healthy build toward its revert, and the next holder of the lock pays it
        // before it reads the count.
        GiveBackLock::Busy => owe_trial_launch(staging, &sentinel, target_build, pid, started),
    }
}

/// Where a give-back that met a held apply lock is OWED: one file per owed launch,
/// `<build> <pid> <seconds> <micros>`,
/// published by a rename so a reader never sees half of one. Written WITHOUT the
/// lock — that is the point — so each debt is its own file and no two writers
/// share one. A separate place on purpose: the sentinel's line is what every build
/// reads and writes, and an older build that never pays a debt leaves the launch
/// counted.
fn owed_give_backs_dir(staging: &Staging) -> PathBuf {
    staging.root.join("boot.sentinel.owed")
}

/// The most owed give-backs one pass reads: far above the launches the sentinel
/// names (`MAX_NAMED_LAUNCHES`), so it only bounds a directory something else
/// filled.
const MAX_OWED_GIVE_BACKS: usize = 64;

/// How long a half-written debt (its writer died between the write and the rename)
/// is left before a pass removes it.
const OWED_TEMP_STALE: std::time::Duration = std::time::Duration::from_secs(60);

/// Owe the one launch of `build` that `pid` (born at `started`, when known) has on
/// the count, for the next holder of the apply lock to give back
/// ([`pay_owed_trial_launches`]). Run by a give-back whose lock wait ran out
/// ([`forgive_trial_launch_of_at`]). The name is read off the sentinel's line now —
/// no lock is needed for a read, since every write is a rename — so a debt always
/// names exactly one launch that IS counted, and one that was never counted (a
/// burst, an overlapping handoff target) or was given back already owes nothing.
/// `true` when the debt is on disk.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "SettledOwed",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
fn owe_trial_launch(
    staging: &Staging,
    sentinel: &Sentinel,
    build: u64,
    pid: u32,
    started: Option<(i64, i64)>,
) -> bool {
    let Some(name) = sentinel
        .named_launches(build)
        .into_iter()
        .find(|name| name.pid == pid && started.is_none_or(|started| started == name.started))
    else {
        crate::log(&format!(
            "pid {pid}'s trial launch of build {build} is not on the count; nothing to give \
             back once the apply lock frees"
        ));
        return false;
    };
    let dir = owed_give_backs_dir(staging);
    let made = {
        use std::os::unix::fs::DirBuilderExt as _;
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => Ok(()),
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists
                    && is_non_symlink_dir(&dir) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    };
    let LaunchName {
        pid,
        started: (seconds, micros),
    } = name;
    let file = format!("{build}-{pid}-{seconds}-{micros}");
    let tmp = dir.join(format!(".{file}.tmp-{}", std::process::id()));
    let written = made
        .and_then(|()| {
            write_private_file(
                &tmp,
                format!("{build} {pid} {seconds} {micros}\n").as_bytes(),
            )
        })
        .and_then(|()| std::fs::rename(&tmp, dir.join(&file)));
    match written {
        Ok(()) => {
            crate::log(&format!(
                "pid {pid}'s trial launch of build {build} is owed back: the next aterm to \
                 take the apply lock gives it back before it reads the count"
            ));
            true
        }
        Err(error) => {
            let _ = std::fs::remove_file(&tmp);
            crate::warn(&format!(
                "could not record that pid {pid}'s trial launch of build {build} is owed back \
                 ({error}); the launch stays counted"
            ));
            false
        }
    }
}

/// The owed give-back `text` records ([`owe_trial_launch`]): its build and launch,
/// or `None` when it is not exactly one.
fn parse_owed_give_back(text: &str) -> Option<(u64, LaunchName)> {
    let mut fields = text.strip_suffix('\n')?.split(' ');
    let build = fields.next()?.parse().ok()?;
    let name = LaunchName {
        pid: fields.next()?.parse().ok()?,
        started: (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?),
    };
    fields.next().is_none().then_some((build, name))
}

/// PAY WHAT IS OWED: every give-back that
/// met a held apply lock ([`owe_trial_launch`]) whose launch is still on the armed
/// trial's count is given back, by its name, and every debt is then removed — paid,
/// or owed to a trial that is no longer armed, or to a launch no longer on the
/// count. Run by every holder of the apply lock that reads the count: a counted
/// launch ([`count_trial_launch`], before it counts — so the revert a boot decides
/// never stands on a launch someone gave back) and every give-back that took the
/// lock. The caller holds the apply lock.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "PaysOwed",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
fn pay_owed_trial_launches(staging: &Staging, sentinel: &Sentinel) {
    let Ok(entries) = std::fs::read_dir(owed_give_backs_dir(staging)) else {
        return;
    };
    let armed = sentinel.read_state().map(|(build, _)| build);
    for entry in entries.flatten().take(MAX_OWED_GIVE_BACKS) {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            // A debt still being written, unless its writer died long ago.
            let stale = std::fs::symlink_metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age >= OWED_TEMP_STALE);
            if stale {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        if let Some((build, name)) = crate::read_ledger_text(&path)
            .as_deref()
            .and_then(parse_owed_give_back)
            && armed == Some(build)
            && give_back_launch_of(sentinel, build, name.pid, Some(name.started))
        {
            crate::log(&format!(
                "the trial launch of build {build} by pid {} was owed back since its \
                 give-back met a held apply lock; paid",
                name.pid
            ));
        }
        let _ = remove_path_no_follow(&path);
    }
}

/// [`Sentinel::forgive_launch_of`], said in the log, for a caller that holds the
/// give-back lock ([`trial_give_back_lock`]).
fn give_back_launch_of(
    sentinel: &Sentinel,
    target_build: u64,
    pid: u32,
    started: Option<(i64, i64)>,
) -> bool {
    match sentinel.forgive_launch_of(target_build, pid, started) {
        Ok(Some(remaining)) => {
            crate::log(&format!(
                "trial launch of build {target_build} by pid {pid} given back — it did not fail \
                 the build ({remaining} unconfirmed launch(es) still counted)"
            ));
            true
        }
        // Not counted (a burst, an overlapping handoff target), or given back
        // already: nothing of this process's is on the count.
        Ok(None) => false,
        Err(error) => {
            crate::warn(&format!(
                "could not give back pid {pid}'s trial launch of build {target_build}: {error}"
            ));
            false
        }
    }
}

/// What [`trial_give_back_lock`] answers.
enum GiveBackLock {
    /// The apply lock, held for the give-back.
    Held(FileLock),
    /// No trial of the build is armed, or another install owns it: this process
    /// may give nothing back.
    NotOurs,
    /// The lock could not be taken (a sibling held it past the wait, or taking it
    /// failed): the give-back is owed ([`owe_trial_launch`]).
    Busy,
}

/// The apply lock, held to give back a counted launch of `target_build`, or why
/// not. Under the apply lock so a give-back cannot interleave with a
/// `check_boot_health` observation or a `confirm_boot_health` disarm. Bounded, so a
/// stopped sibling holding the lock cannot park the parent's handoff worker or a
/// refusing successor; a timed-out wait is `Busy`, and the give-back is OWED to the
/// next holder of the lock — never dropped: the sentinel
/// exists to revert, and a counted launch nobody gives back moves a healthy build
/// toward its revert.
fn trial_give_back_lock(
    staging: &Staging,
    sentinel: &Sentinel,
    target_build: u64,
    installed: Option<&Path>,
    lock_wait: std::time::Duration,
) -> GiveBackLock {
    if !matches!(sentinel.read_state(), Some((b, _)) if b == target_build) {
        return GiveBackLock::NotOurs;
    }
    // Only a launch this install counted may be given back: `check_boot_health` does
    // not count launches against a trial another install owns, so forgiving here
    // would decrement the owner's count for a launch it never saw.
    if let Some(app_root) = installed
        && !trial_owned_by(staging, app_root)
    {
        return GiveBackLock::NotOurs;
    }
    match FileLock::acquire_within(&staging.apply_lock, lock_wait) {
        Ok(lock) => GiveBackLock::Held(lock),
        Err(error) => {
            // The error says why (a sibling past the wait, or a lock that cannot be
            // taken at all); `owe_trial_launch` says what became of the give-back.
            crate::warn(&format!(
                "could not take the apply lock to give back a trial launch of build \
                 {target_build}: {error}"
            ));
            GiveBackLock::Busy
        }
    }
}

/// Confirms the running build reached a healthy checkpoint (window up / first
/// frame): clears the boot sentinel and GCs the retained rollback bundle and
/// orphaned post-swap copy. Idempotent and best-effort; call once from the GUI after
/// deep init, so a crash before this point is caught by [`check_boot_health`] and a
/// crash after it is an ordinary fault the sentinel ignores.
#[must_use]
pub fn confirm_boot_health(
    current_build: u64,
    current_commit: Option<&str>,
    witness: crate::TrialWitness,
) -> bool {
    let Some(staging) = Staging::resolve() else {
        return true;
    };
    confirm_boot_health_at(
        &staging,
        current_build,
        current_commit,
        witness,
        bundle::resolve_layout,
        APPLY_LOCK_WAIT,
    )
}

/// [`confirm_boot_health`] against an explicit staging root, with the install
/// resolver and the apply-lock wait named.
///
/// The wait is BOUNDED: this runs on the GUI's one updater facts worker, which a
/// stopped sibling holding the apply lock must not park. A timeout answers `false`
/// (retry later: the caller re-arms its backoff) and leaves the trial armed.
///
/// A HEADLESS confirmation settles only
/// what a headless launch proves:
/// * of a trial a WINDOW has launched (inside [`WINDOW_TRIAL_HEADLESS_WAIT`]),
///   nothing: it answers `true` — nothing for the caller to retry — and leaves the
///   trial armed and the retained predecessor in place for the window path to prove
///   or revert. `update status` says so, from the trial's own records
///   ([`window_trial_wait_at`]);
/// * of any other trial, the headless path: the trial settles, but the predecessor
///   stays for the next window launch to put the build back on trial
///   ([`confirm_headless_keeping_predecessor`]).
fn confirm_boot_health_at(
    staging: &Staging,
    current_build: u64,
    current_commit: Option<&str>,
    witness: crate::TrialWitness,
    resolve_installed: impl FnOnce() -> Option<bundle::Bundle>,
    lock_wait: std::time::Duration,
) -> bool {
    confirm_boot_health_at_with_proof(
        staging,
        current_build,
        witness,
        resolve_installed,
        lock_wait,
        |app_root| {
            // This is the hard health-commit gate. Plist fields become sealed
            // evidence only after policy verification; the fixed OLD must itself be
            // verified and strictly older; receipt build+commit+DIGEST must equal
            // the active trial.
            ensure_current_trial_receipt(staging, app_root, current_build, current_commit)
                .map(|_| ())
        },
    )
}

/// [`confirm_boot_health_at`] with the trial's recovery proof named (`prove`, over
/// the install's `app_root`), so the settle can be driven over unsealed fixtures.
fn confirm_boot_health_at_with_proof(
    staging: &Staging,
    current_build: u64,
    witness: crate::TrialWitness,
    resolve_installed: impl FnOnce() -> Option<bundle::Bundle>,
    lock_wait: std::time::Duration,
    prove: impl FnOnce(&Path) -> Result<(), String>,
) -> bool {
    // No trial for this build is already complete. A different build's marker is
    // not ours to retry or mutate.
    if !matches!(
        boot_sentinel(staging).read_state(),
        Some((build, _)) if build == current_build
    ) {
        return true;
    }
    // Publication/apply lock only: never stage_lock, so a long download cannot
    // delay this off-UI cleanup and no inverse lock ordering exists.
    let Ok(_apply_lock) = FileLock::acquire_within(&staging.apply_lock, lock_wait) else {
        crate::warn(&format!(
            "could not take the apply lock within {}s to confirm build {current_build}'s \
             boot health (another aterm holds it); the confirmation is retried later",
            lock_wait.as_secs()
        ));
        return false;
    };
    // Health confirmation of an already-applied swap — dev-mark-independent, same
    // reasoning as the trial path above.
    let Some(installed) = resolve_installed() else {
        return false;
    };
    // A trial another install of this build owns is not ours to confirm (its
    // rollback lives beside that bundle): answer "nothing to do" rather than failing
    // the proof forever from a sibling (`trial_owned_by`).
    if !trial_owned_by(staging, &installed.app_root) {
        return true;
    }
    let app_root = installed.app_root.as_path();
    match witness {
        crate::TrialWitness::Window => {
            confirm_health_under_apply_lock_with_proof(staging, current_build, app_root, || {
                prove(app_root)
            })
        }
        crate::TrialWitness::Headless => {
            match trial_window_launch(staging, current_build, unix_now_secs()) {
                WindowLaunch::Waiting { until } => {
                    // Logged, and said by `update status` from the records
                    // themselves ([`window_trial_wait_at`]): nothing is written
                    // into a slot another running build keeps its own reason in.
                    crate::log(&format!(
                        "build {current_build}'s trial was launched by a window, and a \
                         headless launch proves nothing about the window path: the trial \
                         and its predecessor are left for a window to confirm or revert, \
                         until {}",
                        aterm_types::rfc3339::format_rfc3339(until)
                    ));
                    true
                }
                WindowLaunch::None | WindowLaunch::Lapsed => {
                    confirm_headless_keeping_predecessor(staging, current_build, app_root, || {
                        prove(app_root)
                    })
                }
            }
        }
    }
}

/// The boot swap's work that must hold `apply_lock` before it reads the stage
/// (step 3 of [`apply_lock_then_ready`]): an abandoned trial recovered, the
/// just-swapped shape left to its owner, a foreign trial's wedge budgeted, and
/// orphaned swap copies cleared. `Err` is the lane's answer; `app_root` is the
/// installed bundle.
fn settle_trials_under_apply_lock(
    staging: &Staging,
    app_root: &Path,
    current_build: u64,
    current_commit: Option<&str>,
) -> Result<(), ApplyOutcome> {
    if !recover_abandoned_preswap_trial_if_exact(staging, app_root, current_build, current_commit) {
        let armed_build = boot_sentinel(staging)
            .read_state()
            .map_or(0, |(build, _)| build);
        // THE JUST-SWAPPED SHAPE: a sibling cold launch
        // that lost the apply-lock race is still the OLD image, and the bundle
        // under it now holds the NEW build mid-trial — installed == armed, both
        // newer than this process. That is a healthy apply in progress, not a
        // wedged trial: it must neither spend the foreign-trial budget (three such
        // launches would disarm a live trial) nor be booked as a refusal. This
        // image keeps running; the check lane's installed-bundle announcement
        // activates it later. The plist read is bounded and the trial owner's
        // sentinel keeps its authority either way.
        if armed_build != 0
            && armed_build > current_build
            && crate::verify::bundle_build_number(app_root).ok() == Some(armed_build)
        {
            crate::log(&format!(
                "boot apply: build {armed_build} is already installed and mid-trial; this \
                 launch is the previous build {current_build} and leaves the trial to its owner"
            ));
            return Err(ApplyOutcome::NoUpdate);
        }
        // A sentinel armed for a build that is not the running one can never be
        // cleared by the same-build lanes, so an unrecoverable one blocks EVERY
        // future apply forever. Budget it — with its own counter, never the
        // trial's. The SAME-build case that another install owns is the same
        // wedge for THIS process (`check_boot_health` rightly does not count us,
        // and the owner may never run again), so it takes the same budget.
        if armed_build != 0 && (armed_build != current_build || !trial_owned_by(staging, app_root))
        {
            escape_wedged_foreign_trial(staging, current_build, armed_build);
        }
        return Err(ApplyOutcome::Deferred(unconfirmed_trial_refusal(
            armed_build,
        )));
    }
    // The mismatched-sentinel lane is behind us for this launch, so no budget is
    // outstanding; never let one accumulate across unrelated trials.
    let _ = foreign_trial_counter(staging).confirm();
    // Under the lock no other swap is in flight, so it is safe to clear orphaned
    // transient swap copies from a previously interrupted/completed swap.
    if let ReadyState::Newer(orphaned_ready) = read_ready(staging, current_build)
        && let Err(error) =
            recover_orphaned_prepared_candidate(staging, app_root, current_build, &orphaned_ready)
    {
        return Err(ApplyOutcome::Deferred(error));
    }
    Ok(())
}

/// Tri-state read of the staging marker, distinguishing a missing marker from a
/// present but unparseable one (discarded rather than wedging updates) and folding
/// in the strict downgrade gate.
#[allow(
    clippy::large_enum_variant,
    reason = "`Newer` IS the marker (past the lint's 200-byte line since \
              `roster_confirmed_at`); a ReadyState is the \
              transient answer of one read, matched at once and never stored, so boxing \
              it would add an allocation per read to save nothing"
)]
enum ReadyState {
    Newer(Ready),
    NotNewer,
    Corrupt,
    Absent,
}

/// Steps 3–4 of the boot apply lane, as one function the lane and its Tier-1 test
/// both run.
///
/// 1. The pre-lock peek: nothing staged and no boot sentinel is the common case and
///    skips the lock (`NoUpdate`). It decides nothing else: newer, not-newer and
///    corrupt are decided under the lock, so retirement never races the stager's
///    final publication, and a sibling that swapped and retired the marker while
///    this launch waited is seen (`NativeUpdateTwoProcessApply`).
/// 2. `lock` takes `apply_lock` (in the lane, bounded by `APPLY_LOCK_WAIT`).
/// 3. `under_lock` is the work that must hold the lock before the decision (the
///    apply budget, trial and orphan recovery); its value (the budget guard) is
///    returned with the lock so both live until the swap.
/// 4. The decision: [`ready_under_apply_lock`], the marker as it reads now.
fn apply_lock_then_ready<L, U>(
    staging: &Staging,
    current_build: u64,
    lock: impl FnOnce() -> Result<L, ApplyOutcome>,
    under_lock: impl FnOnce() -> Result<U, ApplyOutcome>,
) -> Result<(L, U, Ready), ApplyOutcome> {
    if matches!(read_ready(staging, current_build), ReadyState::Absent)
        && boot_sentinel(staging).read_state().is_none()
    {
        return Err(ApplyOutcome::NoUpdate);
    }
    let held = lock()?;
    let work = under_lock()?;
    // A stage in flight may continue downloading under stage_lock, but its final
    // publication takes this same apply_lock. Apply retirement touches only
    // ready+staged_app, never that producer's scratch.
    let ready = ready_under_apply_lock(staging, current_build)?;
    Ok((held, work, ready))
}

/// The under-lock re-read the apply lane acts on once `apply_lock` is held: the
/// marker as it reads now, never as a pre-lock peek saw it. Two processes arming the
/// same build is ordinary (every window and session runs the lane); if the other's
/// successor swapped the bundle and retired the marker meanwhile, the loser finds
/// nothing to apply and swaps nothing (`NativeUpdateTwoProcessApply`; Tier-1:
/// `two_processes_arming_one_build_swap_it_once`). A marker that is not newer, or
/// not readable, is retired here too.
fn ready_under_apply_lock(staging: &Staging, current_build: u64) -> Result<Ready, ApplyOutcome> {
    match read_ready(staging, current_build) {
        ReadyState::Newer(ready) => Ok(ready),
        ReadyState::NotNewer => {
            staging.retire_published();
            Err(ApplyOutcome::NoUpdate)
        }
        ReadyState::Corrupt => {
            crate::warn("ready.toml is unreadable; discarding staged update");
            staging.retire_published();
            Err(ApplyOutcome::NoUpdate)
        }
        ReadyState::Absent => Err(ApplyOutcome::NoUpdate),
    }
}

/// Gates 4b and 4c of the swap: the operator floor's withdrawals of a staged build,
/// a yank (`min_build`) and a signer the machine roster revoked (the floor's
/// durable revocation set). `Some(NoUpdate)` once the stage is retired; `None` to go
/// on. The caller holds the apply lock and read `ready` under it.
fn floor_withdrawal_gate(
    staging: &Staging,
    current_build: u64,
    ready: &Ready,
) -> Option<ApplyOutcome> {
    // 4b. Honor an operator apply floor (yank): never apply a staged build below the
    //     persisted, monotonic min_build — even though it's genuine and strictly newer
    //     than us — so the owner can retire a bad-but-genuine release after the fact.
    //     A floor that is there but could not be read DEFERS the swap and retires
    //     nothing: the stage waits for a read that can judge it.
    let floor = match crate::manifest::Floor::read_for_gate(&staging.floor()) {
        Ok(floor) => floor,
        Err(refusal) => {
            crate::warn(&format!(
                "staged build {} not applied: {refusal}",
                ready.build_number
            ));
            crate::status::record(
                staging,
                current_build,
                "deferred: aterm could not read its update records just then",
            );
            return Some(ApplyOutcome::Deferred(refusal));
        }
    };
    if ready.build_number < floor.min_build {
        crate::warn(&format!(
            "staged build {} is below the operator floor {}; discarding (yanked)",
            ready.build_number, floor.min_build
        ));
        staging.retire_published();
        crate::status::record(
            staging,
            current_build,
            &format!(
                "held: staged build {} below floor {} (yanked)",
                ready.build_number, floor.min_build
            ),
        );
        return Some(ApplyOutcome::NoUpdate);
    }
    // 4c. The durable revocation set, and deliberately no roster-generation gate.
    //
    //     A stage is authorized at stage time; a revocation lands afterwards. The
    //     check lane that admits the revoking roster records the revoked machine ids
    //     in the floor and retires the stage under this lock, but a check that could
    //     not take the lock leaves it for later, and a quit app launches here first.
    //     So this gate reads the same set: a stage signed by a machine this client
    //     has seen revoked is retired, never swapped in. It is exact: the machine id
    //     is in the manifest's signed bytes, and the set holds only what an
    //     admitted, master-verified roster revoked.
    //
    //     It cannot see a revocation no check on this Mac has observed yet (the apply
    //     lane holds no roster, and fetching one before the boot swap would put a
    //     network wait on every launch). Gate 4d bounds that: a stage whose roster
    //     authorization no check has confirmed for a day is not swapped until one
    //     has ([`roster_freshness_gate`]), so only a revocation published inside
    //     that day is missed — and that residual is `min_build`'s to yank
    //     (docs/RELEASE-KEYS.md: revocation stops future acceptance).
    //
    //     Comparing the marker's generation with `Floor::roster_seq` is not the
    //     missing check: the floor ratchets to the roster asset's generation, the
    //     marker records the generation the manifest was attributed under, and a
    //     machine joining the roster attaches its pair to releases already shipped,
    //     so `manifest_seq < floor` is the ordinary post-join state
    //     `authorize_by_roster` admits. Gating on it would retire good stages forever.
    if floor.revokes(ready.machine_id.as_deref()) {
        let machine = ready.machine_id.as_deref().unwrap_or("?");
        crate::warn(&format!(
            "staged build {} was signed by machine {machine}, which the machine roster has \
             revoked; discarding it",
            ready.build_number
        ));
        crate::github::retire_revoked_stage(staging, ready.build_number);
        crate::status::record(
            staging,
            current_build,
            &format!(
                "held: staged build {} was signed by machine {machine}, which the machine \
                 roster has revoked",
                ready.build_number
            ),
        );
        return Some(ApplyOutcome::NoUpdate);
    }
    None
}

/// The swap's gates on the stage it read under the apply lock, in order: the floor's
/// withdrawals (4b, 4c: [`floor_withdrawal_gate`], which retire), a marker that no
/// longer names the published stage (retired), and the age of its roster
/// confirmation (4d: [`roster_freshness_gate`], which waits). `Some` is the lane's
/// answer; `None` goes on to the install location and the re-verification.
///
/// The `Swap` guard of `NativeUpdateRosterFreshness`: a stage on
/// disk whose roster confirmation is younger than the window. Tier-1:
/// `github::tests::roster_freshness_conformance`.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "NativeUpdateRosterFreshness",
        action = "Swap",
        project = "aterm_update::roster_freshness_projection"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateRosterFreshness",
        action = "Tick",
        reason = "Time passing is the environment; the Tier-1 bind maps each tick to half the window of the real clock it hands these gates."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateRosterFreshness",
        action = "Revoke",
        reason = "The owner's revocation is published elsewhere and reaches this Mac only as a roster a check admits, which is Check."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateRosterFreshness",
        action = "SwapOnTheFloorAlone",
        reason = "Buggy=1 negative control only: a lane that admits a stage on the floor's withdrawals alone; gate 4d reads the confirmation's age."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateRosterFreshness",
        action = "ConfirmWithoutTheRoster",
        reason = "Buggy=1 negative control only: a confirmation stamped without reading the roster's revocations; the check stamps only after the floor it just wrote withdraws nothing."
    )
)]
pub(crate) fn stage_admission_gates(
    staging: &Staging,
    current_build: u64,
    ready: &Ready,
    now: u64,
) -> Option<ApplyOutcome> {
    if let Some(outcome) = floor_withdrawal_gate(staging, current_build, ready) {
        return Some(outcome);
    }
    if !ready.is_publishable(staging) {
        staging.retire_published();
        return Some(ApplyOutcome::NoUpdate);
    }
    roster_freshness_gate(ready, now)
}

/// Gate 4d of the swap: the stage's roster authorization must
/// have been confirmed against a freshly fetched machine roster within
/// [`crate::manifest::ROSTER_FRESHNESS_SECS`] of `now`
/// ([`Ready::roster_authorization_stale`]). `Some(Deferred)` — a PASSING refusal,
/// the stage kept, nothing memoized — otherwise; `None` to go on.
///
/// A revocation reaches this Mac only through a check, and a launch swaps before
/// any check runs; with no bound, a stage signed by a machine revoked while the app
/// was quit would be swapped in at the next launch. Waiting costs no
/// network at launch: the check that runs right after it re-reads the roster
/// (`Ready::needs_roster_reconfirmation`) and either confirms the stage
/// ([`confirm_roster_authorization`]), after which the in-session lane or the next
/// launch applies it, or retires it as revoked. The residual — a revocation
/// published inside the window — is stated in docs/RELEASE-KEYS.md.
///
/// AND A CHANNEL NO CHECK CAN CONFIRM AGAINST HOLDS THE STAGE AS LONG AS IT LASTS.
/// Only a check that admits the channel head's roster
/// confirms anything, so the wait lasts while the head is not an app release (an
/// index cut or a source-only release at the head: no roster is read) and, for
/// good, once this build's anchor cannot verify the roster (a master rotation it
/// predates: `STALE_ANCHOR`). In the second a stage carrying the new pin is
/// stranded on disk and only a reinstall (tools/install.sh) moves the client; so a
/// rotation's adoption window must
/// cover staged-but-uninstalled clients (docs/RELEASE-KEYS.md, *Rotating the
/// master*). Failing open there is not the answer: a roster no pin verifies is
/// what a thief with the publish credential can serve too, and opening the gate on
/// it would let the stage of a machine revoked in the real roster install. The
/// check's status line says which state holds the stage
/// (`github::unconfirmable_stage_note`).
///
/// The pre-park checks ask the same predicate ([`handoff_cached_pass_refusal_at`],
/// [`preverify_staged_handoff_candidate_at`]), so the in-session lane neither swaps
/// such a stage before the check confirms it nor parks a reader for a successor
/// this gate would turn away. Its refusal carries [`crate::ROSTER_UNCONFIRMED_KEY`]
/// so the window files it as a wait for the check
/// ([`crate::is_roster_confirmation_wait`]) and not as a passing failure to retry
/// on its transient budget: it lasts until a check reads the roster, which no
/// apply attempt does.
fn roster_freshness_gate(ready: &Ready, now: u64) -> Option<ApplyOutcome> {
    ready
        .roster_authorization_stale(now, aterm_update_core::pins::roster_tier_armed())
        .map(ApplyOutcome::Deferred)
}

/// Record that a check CONFIRMED `staged` against a roster it admitted, which it
/// began reading at `confirmed_at` (RFC3339): its marker's
/// [`Ready::roster_confirmed_at`] moves to that instant, so the swap's gate 4d and
/// the pre-park checks admit it for another
/// [`crate::manifest::ROSTER_FRESHNESS_SECS`], and a standing
/// apply refusal that waited for exactly this is withdrawn from the ledger.
///
/// NEVER BACKWARDS: a stamp already on the marker that is later than
/// `confirmed_at` (a sibling's check that read a newer roster while this one ran)
/// and not ahead of this Mac's clock is kept, since both are genuine reads and the
/// later one bounds the window more tightly. A stamp ahead of the clock is
/// replaced: its age cannot be known, and it reads as stale.
///
/// UNDER THE APPLY LOCK, like every other write of the published marker, and only
/// for the stage the caller judged: a marker another process replaced or retired
/// meanwhile is left alone (its own check confirms it). Bounded like the check
/// lane's retirement; a lock not had in time confirms nothing, and the next check
/// that reads a roster tries again. The ledger is written after the lock is let go:
/// the health lock is never taken under the apply lock (see
/// [`apply_staged_if_ready`]).
///
/// A stage that was WAITING for this (its confirmation stale, or never written)
/// becomes installable only now, so its pending clock restarts here
/// ([`crate::health::Health::restart_pending_update`]): the clock counts the days
/// the app was closed, and the overdue notice would otherwise call an update stuck
/// the moment the launch after them confirmed it.
pub(crate) fn confirm_roster_authorization(
    staging: &Staging,
    staged: &Ready,
    confirmed_at: &str,
) -> Result<(), String> {
    let confirmed = aterm_types::rfc3339::parse_utc(confirmed_at)
        .and_then(|secs| u64::try_from(secs).ok())
        .ok_or_else(|| format!("the clock could not be read ({confirmed_at:?})"))?;
    let was_waiting = {
        let _apply_lock = FileLock::acquire_within(&staging.apply_lock, BACKGROUND_LOCK_WAIT)
            .map_err(|error| verify::lock_wait_refusal("confirm lock", &error))?;
        let Some(mut ready) = Ready::read_published_bundle(staging).filter(|still| {
            still.build_number == staged.build_number
                && still.dmg_sha256.eq_ignore_ascii_case(&staged.dmg_sha256)
                && still.commit == staged.commit
        }) else {
            return Ok(());
        };
        let wall = unix_now_secs().max(confirmed);
        let was_waiting = ready.roster_authorization_stale(wall, true).is_some();
        let later_on_disk = ready
            .roster_confirmation_age(wall)
            .is_some_and(|age| age < wall - confirmed);
        if !later_on_disk && ready.roster_confirmed_at.as_deref() != Some(confirmed_at) {
            ready.roster_confirmed_at = Some(confirmed_at.to_string());
            crate::manifest::write_durable(&staging.ready, &ready.to_toml()?, "ready marker")?;
        }
        was_waiting
    };
    crate::health::Health::withdraw_apply_refusal_containing(
        &staging.health(),
        crate::manifest::ROSTER_UNCONFIRMED_KEY,
    );
    if was_waiting {
        crate::health::Health::restart_pending_update(&staging.health(), staged.build_number);
    }
    Ok(())
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

/// Exec-failure rollback: puts the OLD bundle (at `rollback`) back at `installed`,
/// which holds the NEW (failed-to-exec) bundle. Both are on one volume, so this is
/// the inverse atomic exchange.
fn restore_rollback(rollback: &Path, installed: &Path) -> Result<(), String> {
    checked_bundle_exchange(rollback, installed, "atomic rollback restore")
}

/// 16 CSPRNG bytes, hex-encoded (32 chars), for the single-use re-exec nonce.
/// Unguessable, so an attacker cannot preset a matching env var. Minted through
/// `aterm_uds::rand`, the one audited entropy surface, not a hand-rolled
/// `/dev/urandom` read. `None` if the OS CSPRNG is unavailable; the caller then
/// uses the "1" marker (the guard finds no stamp and takes the normal no-op path).
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

/// Best-effort RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) for the
/// human-readable `staged_at`/status fields, computed in-process (no `/bin/date`
/// fork). Empty on a pre-epoch clock.
pub(crate) fn now_rfc3339() -> String {
    aterm_time::unix_secs().map_or_else(String::new, format_rfc3339)
}

/// Unix seconds now, or 0 on a pre-epoch clock, for the stage-failure retry
/// budget's deadlines and the check lane's window and receipt. Zero makes every
/// deadline appear passed, so a broken clock retries rather than suppresses forever.
#[must_use]
pub(crate) fn unix_now_secs() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// Seconds between two of our own RFC3339 instants (`later - earlier`), or
/// `None` when either string is not the exact `YYYY-MM-DDTHH:MM:SSZ` shape this
/// module writes (e.g. the empty pre-epoch fallback) or `later` precedes
/// `earlier`. The parse is the shared strict one, `aterm_types::rfc3339::parse_utc`.
pub(crate) fn rfc3339_delta_secs(earlier: &str, later: &str) -> Option<u64> {
    fn epoch(s: &str) -> Option<u64> {
        u64::try_from(aterm_types::rfc3339::parse_utc(s)?).ok()
    }
    epoch(later)?.checked_sub(epoch(earlier)?)
}

// The RFC 3339 UTC stamp is `aterm_types::rfc3339::format_rfc3339`, shared by the
// publisher, the updater client, the GUI and atpkg.
use aterm_types::rfc3339::format_rfc3339;

// Boot-apply tests, a child module so they reach this file's private entry points.
#[cfg(test)]
#[path = "install_boot_tests.rs"]
mod boot_tests;

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "install_copy_tests.rs"]
mod copy_tests;
