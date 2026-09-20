// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Staging (unpack → verify → publish) and application (lock → re-verify →
//! atomic swap → re-exec) of an update. The ordering here is the
//! security-critical part; see the per-step comments and the crate-level trust
//! model.
//!
//! Unpacking has two shapes for the same signed bundle: [`stage_from_zip`]
//! (`ditto -x -k`, preferred) and [`stage_from_dmg`] (`hdiutil attach`, for
//! releases published before the zip existed). They share
//! `verify_and_publish_incoming`, so the container never changes what is proved.
//! What IS container-specific is the cost of unpacking: the zip path reads the
//! archive's central directory and refuses one that would fill the volume before
//! `ditto` is spawned — see [`checked_zip_extraction_claim`].

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
pub(crate) const MAX_BOOT_ATTEMPTS: u32 = 3;

/// How long a LAUNCH waits for the apply lock before giving up and starting on
/// the installed build.
///
/// Deliberately short. Every legitimate holder either returns in milliseconds or
/// is itself bounded, so reaching this means the holder is wedged — and a wedged
/// holder must not be able to stop a terminal from opening. Deferring costs one
/// launch's update; hanging costs the application.
const APPLY_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Ceiling on the cross-volume `ditto` of the candidate bundle.
///
/// Generous, because unlike the verification helpers this one's honest duration
/// is a property of the hardware: a several-hundred-megabyte bundle onto a slow
/// external disk. It exists to bound the pathological case — a volume that has
/// gone away mid-copy — not to hurry a real one.
const CROSS_VOLUME_COPY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Unpacking from a provenance-TRACKED app (2026-09-14).
//
// macOS stamps `com.apple.provenance` on every file a tracked process writes, and a
// process is tracked when its executable carries the tag or its parent is tracked
// (`crates/atpkg/src/provenance.rs` holds the measured law). This updater writes the
// SUCCESSOR bundle — so a tracked aterm hands the tag to the app that replaces it, and
// to every app after that, forever: measured on m16, the 0.85.0 laid down by a tracked
// 0.84.0 carried the tag on `Contents/MacOS/aterm` and `Info.plist`, which is exactly
// what makes atpkg's untracked lane take its copy plan for the life of the install.
//
// The one thing that escapes is a job LAUNCHD spawns from an untagged executable:
// launchd is the job's parent, not us, and `/usr/bin/ditto` is a base-OS binary
// (measured the same day: ditto of a clean tree by a launchd job → clean; by this
// process → tagged). So when this process measures itself tracked, the unpacks run as
// one-shot launchd jobs. The renames that follow are safe as they are: a tracked
// rename tags the DIRECTORY it moves, not the files inside it (measured).
// ---------------------------------------------------------------------------

/// Whether THIS process is provenance-tracked — MEASURED, by writing a probe file into
/// `scratch` and reading the attribute back, never inferred from the binary's own
/// attributes (a clean binary under a tracked parent is tracked, which is the whole
/// point). `false` when the probe cannot be written or inspected: a lane that cannot
/// measure must not route an unpack through machinery it has no evidence it needs. The
/// probe is removed before returning.
#[cfg(target_os = "macos")]
fn process_is_tracked(scratch: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let probe = scratch.join(format!(".provenance-probe-{}", std::process::id()));
    if std::fs::write(&probe, b"probe\n").is_err() {
        return false;
    }
    let verdict = (|| {
        let c_path = std::ffi::CString::new(probe.as_os_str().as_bytes()).ok()?;
        // SAFETY: `c_path` is NUL-terminated and outlives both calls; a null buffer with
        // size 0 is the documented size query; the second call passes `buf`'s own length.
        let needed = unsafe { libc::listxattr(c_path.as_ptr(), std::ptr::null_mut(), 0, 0) };
        if needed <= 0 {
            return Some(false);
        }
        let mut buf = vec![0u8; needed as usize];
        let got = unsafe {
            libc::listxattr(
                c_path.as_ptr(),
                buf.as_mut_ptr().cast::<libc::c_char>(),
                buf.len(),
                0,
            )
        };
        if got < 0 {
            return None;
        }
        buf.truncate(got as usize);
        Some(
            buf.split(|b| *b == 0)
                .any(|name| name == b"com.apple.provenance"),
        )
    })();
    let _ = std::fs::remove_file(&probe);
    verdict.unwrap_or(false)
}

/// Each copy owns a fresh private directory on the destination volume. A helper
/// that outlives its deadline can write only here, never into a later attempt,
/// the published stage, or the fixed rollback path. Unconfirmed attempts retain
/// their directory; its random name is never handed to another copy or swept as
/// legacy `zx-*` scratch.
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
        Ok(Self {
            payload: root.join("payload"),
            root,
        })
    }
}

/// Reclaim only explicitly abandoned attempts whose fenced writer published a
/// terminal status. A live caller may have finished copying but not promoted yet;
/// completion alone is therefore not permission to remove its directory. The
/// background checker owns this recursive maintenance. Copy allocation creates
/// only its own directory and marker, regardless of abandoned payloads nearby.
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
        if nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
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
            || crate::read_ledger_text(&root.join("abandoned")).as_deref() != Some("1\n")
        {
            continue;
        }
        let Some(label) = crate::read_ledger_text(&root.join("launchd.job")) else {
            continue;
        };
        let Some(nonce) = label.strip_prefix("systems.alab.aterm-update.unpack-") else {
            continue;
        };
        if nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let status = root.join(format!(".{label}.status"));
        if !is_non_symlink_dir(&root.join(format!(".{label}.status.once"))) {
            continue;
        }
        if crate::read_ledger_text(&status).is_some_and(|text| text.trim().parse::<u8>().is_ok()) {
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

/// The shipping promotion boundary, shared by DMG extraction, ZIP extraction,
/// and cross-volume apply. `run` must report success only after the copy exits;
/// the launchd wrapper additionally fences its own automatic restarts.
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
            // Even a successful launchctl remove is asynchronous. Keep the
            // one-shot claim and completion evidence for a future conservative
            // sweep. A late completion licenses cleanup only, never promotion.
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
    // A terminal copy owns no live writer. A restarted wrapper cannot recreate
    // its claim after this never-reused parent disappears (plain mkdir, no -p).
    let _ = std::fs::remove_dir_all(&attempt.root);
    result
}

/// Run `/usr/bin/ditto` into an isolated attempt, bounded by `limit`, then promote
/// only its completed output. A provenance-tracked process delegates the copy to
/// launchd so the successor's files do not inherit its provenance tag.
fn ditto_bounded_clean(
    args: &[std::ffi::OsString],
    what: &str,
    limit: std::time::Duration,
    scratch: &Path,
) -> Result<(std::process::ExitStatus, String), String> {
    copy_into_isolated_destination(args, what, |args, attempt_root| {
        #[cfg(target_os = "macos")]
        if process_is_tracked(scratch) {
            return ditto_via_launchd(args, what, limit, attempt_root).map_err(|message| {
                crate::verify::HelperFailure {
                    message,
                    writer_stopped: false,
                }
            });
        }
        crate::verify::status_bounded_with_stderr_observed(
            Command::new("/usr/bin/ditto").args(args),
            what,
            limit,
        )
    })
}

/// The tracked half of [`ditto_bounded_clean`]. Submit, job completion, and
/// timeout cleanup share one deadline. A short reserve allows bounded removal
/// after a stuck submit or copy; neither launchctl invocation can wait forever.
#[cfg(target_os = "macos")]
fn ditto_via_launchd(
    args: &[std::ffi::OsString],
    what: &str,
    limit: std::time::Duration,
    scratch: &Path,
) -> Result<(std::process::ExitStatus, String), String> {
    ditto_via_launchd_using(args, what, limit, scratch, Path::new("/bin/launchctl"))
}

#[cfg(target_os = "macos")]
struct LaunchdCopyBudget {
    work: std::time::Instant,
    finish: std::time::Instant,
}

#[cfg(target_os = "macos")]
impl LaunchdCopyBudget {
    fn new(now: std::time::Instant, limit: std::time::Duration) -> Self {
        let finish = now + limit;
        let reserve = std::time::Duration::from_secs(2).min(limit / 4);
        Self {
            work: finish - reserve,
            finish,
        }
    }

    fn deadline(&self, cleanup: bool) -> std::time::Instant {
        if cleanup { self.finish } else { self.work }
    }
}

// launchctl submit can restart a failed wrapper, including after it published a
// successful ditto status. The exclusive claim survives that gap: no incarnation
// may copy twice, and a removed attempt parent cannot be recreated by this claim.
#[cfg(target_os = "macos")]
const LAUNCHD_COPY_WRAPPER: &str = r#"st="$1"; lab="$2"; ctl="$3"; shift 3; /bin/mkdir "$st.once" 2>/dev/null || exit 0; "$@"; s=$?; printf '%s
' "$s" > "$st.tmp" && mv -f "$st.tmp" "$st"; "$ctl" remove "$lab"; exit 0"#;

#[cfg(target_os = "macos")]
fn ditto_via_launchd_using(
    args: &[std::ffi::OsString],
    what: &str,
    limit: std::time::Duration,
    scratch: &Path,
    launchctl: &Path,
) -> Result<(std::process::ExitStatus, String), String> {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let budget = LaunchdCopyBudget::new(Instant::now(), limit);
    let work_deadline = budget.deadline(false);
    let nonce = random_nonce().ok_or_else(|| "launchd copy nonce unavailable".to_string())?;
    let label = format!("systems.alab.aterm-update.unpack-{nonce}");
    std::fs::write(scratch.join("launchd.job"), &label)
        .map_err(|error| format!("record launchd copy identity: {error}"))?;
    let status_file = scratch.join(format!(".{label}.status"));
    let status_tmp = scratch.join(format!(".{label}.status.tmp"));
    let err_log = scratch.join(format!(".{label}.err"));
    aterm_log::info!(
        "aterm-update: this process is provenance-tracked — running {what} as a launchd job \
         so the bundle it lays carries no com.apple.provenance"
    );
    let run_until = |cmd: &mut Command, operation: &str, until: Instant| {
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("{operation}: unpack deadline exhausted"));
        }
        crate::verify::status_bounded_with_stderr(cmd.stdout(Stdio::null()), operation, remaining)
    };
    let result = (|| {
        let (status, stderr) = run_until(
            Command::new(launchctl)
                .arg("submit")
                .args(["-l", &label])
                .arg("-e")
                .arg(&err_log)
                .arg("--")
                .arg("/bin/sh")
                .arg("-c")
                .arg(LAUNCHD_COPY_WRAPPER)
                .arg("aterm-update-unpack")
                .arg(&status_file)
                .arg(&label)
                .arg(launchctl)
                .arg("/usr/bin/ditto")
                .args(args),
            &format!("launchctl submit for {what}"),
            work_deadline,
        )?;
        if !status.success() {
            return Err(ditto_failure(
                &format!("launchctl submit for {what}"),
                status,
                &stderr,
            ));
        }
        loop {
            let remaining = work_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "{what} did not finish within its {}s launchd job budget; treating as a failure",
                    limit.as_secs()
                ));
            }
            // The same bounded regular-file reader used for updater ledgers:
            // a FIFO, oversized file or incomplete publication is not a result.
            if let Some(text) = crate::read_ledger_text(&status_file) {
                let code = text
                    .trim()
                    .parse::<u8>()
                    .map_err(|_| format!("{what}: launchd job returned a malformed exit status"))?;
                let stderr = launchd_stderr_tail(&err_log);
                // This is the wrapper's shell exit code (signals are 128+n),
                // not the unavailable raw wait status of launchd's child.
                return Ok((
                    std::process::ExitStatus::from_raw(i32::from(code) << 8),
                    stderr,
                ));
            }
            std::thread::sleep(Duration::from_millis(50).min(remaining));
        }
    })();
    let result = match result {
        Ok(completed) => Ok(completed),
        Err(error) => {
            // A timed-out submit may already have created the job. Try removal
            // even on that path, using only the original budget's remaining time.
            match run_until(
                Command::new(launchctl).args(["remove", &label]),
                &format!("launchctl remove for {what}"),
                budget.deadline(true),
            ) {
                Ok((status, _)) if status.success() => Err(error),
                Ok((status, stderr)) => Err(format!(
                    "{error}; cleanup not confirmed: {}",
                    ditto_failure("launchctl remove", status, &stderr)
                )),
                Err(cleanup) => Err(format!("{error}; cleanup not confirmed: {cleanup}")),
            }
        }
    };
    if result.is_ok() {
        for path in [&status_file, &status_tmp, &err_log] {
            let _ = std::fs::remove_file(path);
        }
    }
    result
}

/// Match the direct helper's bounded diagnostic tail without loading a job's
/// entire stderr log. Opening nonblocking and requiring a regular file avoids
/// mistaking a FIFO for a log and waiting on a writer that may never arrive.
#[cfg(target_os = "macos")]
fn launchd_stderr_tail(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::OpenOptionsExt;
    const KEEP: u64 = 512;
    let read = || -> Option<String> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        file.seek(SeekFrom::Start(metadata.len().saturating_sub(KEEP)))
            .ok()?;
        let mut bytes = Vec::new();
        file.take(KEEP).read_to_end(&mut bytes).ok()?;
        Some(String::from_utf8_lossy(&bytes).trim().to_string())
    };
    read().unwrap_or_default()
}

/// Ceiling on one staging unpack (the DMG copy or the zip extract). A release is
/// ~100 MB; minutes is a wedged volume, not a slow one. Before 2026-09-14 these
/// two `ditto`s had no ceiling at all.
const STAGE_UNPACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Slack the free-space check demands over the declared size: filesystem
/// metadata, the extract's own scratch dir, and the verify step's writes.
const STAGE_FREE_SPACE_HEADROOM: u64 = 64 * 1024 * 1024;

/// A `ditto` failure that says WHY, from the tail of its stderr (2026-09-14).
fn ditto_failure(what: &str, status: std::process::ExitStatus, stderr: &str) -> String {
    match crate::verify::last_stderr_line(stderr) {
        Some(line) => format!("{what} ({status}): {line}"),
        None => format!("{what} ({status})"),
    }
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
/// [`STAGE_FREE_SPACE_HEADROOM`] (2026-09-14). The message names both numbers and
/// the volume, so a full disk reads as a full disk in the ledger and the pull-down
/// instead of "ditto zip extract failed (exit status: 1)" — and no container is
/// re-unpacked (or, on the zip lane, extracted at all) to learn what `statvfs`
/// already knows. Not a size ceiling: `checked_zip_extraction_claim` bounds the
/// fleet-wide cost; this bounds THIS machine's. A volume that will not report its
/// free space is let through — `ditto` then decides, with its stderr kept.
fn refuse_without_room(dir: &Path, needed: u64, what: &str) -> Result<(), String> {
    let Some(free) = free_bytes_at(dir) else {
        return Ok(());
    };
    let wanted = needed.saturating_add(STAGE_FREE_SPACE_HEADROOM);
    if free >= wanted {
        return Ok(());
    }
    let mib = |bytes: u64| bytes.div_ceil(1024 * 1024);
    Err(format!(
        "not enough free space to unpack {what}: {} MiB free on the volume holding {}, \
         {} MiB needed ({} MiB declared plus {} MiB headroom); free some space and the \
         next check retries",
        mib(free),
        dir.display(),
        mib(wanted),
        mib(needed),
        mib(STAGE_FREE_SPACE_HEADROOM)
    ))
}

/// Bytes a copy of the tree at `root` writes: every regular file's length, links
/// and directories counted as nothing. Symlinks are NOT followed — the mounted
/// image controls them.
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

// The relaunch argv filter lives in `crate::relaunch` — every platform
// relaunches aterm, so it cannot live inside this macOS-only module.
use crate::relaunch::reexec_forwarded_args;

/// The post-swap re-exec command: the NEW binary at the canonical path, the
/// forwarded argv, the single-use re-exec nonce — and the caller's handoff
/// authority variables restored onto the exec image ONLY. The GUI's prearm
/// consumed those variables out of the ambient environment (so no
/// codesign/PlistBuddy/spctl helper this process spawned could observe them),
/// but the successor image re-runs prearm and must re-validate the inherited
/// handoff: without the restored pairs it classifies the handoff malformed and
/// exits before writing the readiness proof, the parked parent reads EOF
/// (`ChildDied`), and the seamless lane can never succeed.
///
/// `ATERM_UPDATED_FROM` names the build this image was (2026-09-14, audit BA-6):
/// the successor's `main` reads it for the quiet post-update "leveled-up" notice
/// and clears it. Only the in-session lane used to set it, so a Finder-launch
/// fallback apply came up with no notice at all.
fn boot_reexec_command(
    new_exe: &std::path::Path,
    reexec_value: &str,
    updated_from: u64,
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
        // apply runs before the entry consumes it). The forwarded args are
        // stripped of the leading pins earlier swaps prepended, so exactly ONE
        // `--window` survives however many updates this process has ridden.
        .args(reexec_forwarded_args(std::env::args_os().skip(1)))
        .env("ATERM_UPDATE_REEXEC", reexec_value)
        .env("ATERM_UPDATED_FROM", updated_from.to_string());
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

/// Consecutive launches that observed a boot sentinel armed for a build this
/// process is NOT running and could not recover.
///
/// Deliberately its OWN file rather than [`Sentinel::observe_launch`] on the boot
/// sentinel: that attempt count belongs to the TRIALED build, and incrementing it
/// from another build's launches would push a healthy build toward
/// [`Sentinel::should_revert`] on its next launch — two installs (say
/// `/Applications` and `~/Applications`) share one staging root, so a spurious
/// crash-loop revert of a build that never crashed is reachable, not theoretical.
/// Reusing the `Sentinel` TYPE is free and gives the atomic write shape: `arm`
/// records `"<build> 0"`, `observe_launch` counts only while the recorded build
/// still matches, `confirm` deletes it.
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

/// Last-modified time of `path`, or `None` when it does not exist / cannot be
/// stat'd. Used to compare two files in the SAME directory against each other (a
/// system clock jump moves both or neither), and — for the present-owner idle test
/// in `escape_wedged_foreign_trial` only — against the wall clock, where a future
/// stamp (skew, a restored backup) reads as "not idle", the fail-safe direction.
fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Launches of the trialed build closer together than this count as ONE
/// (2026-09-14, audit BA-3). The sentinel counted every launch of the trial from
/// the owning install, so three processes of the new build started inside the
/// trial window — parallel headless engines from agent tooling, `open -n -a aterm`
/// three times, a launcher script — were three counted launches with zero
/// crashes, the third reverted, and the quarantine poisoned a healthy release for
/// good. A real crash loop is user- or launcher-driven and every relaunch passes
/// dyld and GPU init, so its launches are seconds apart and still reach the budget.
const BOOT_LAUNCH_BURST_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether this launch is part of a burst the sentinel already counted: an
/// OBSERVED trial (attempts > 0 — the arm itself is never a burst, so the first
/// launch always counts) whose file was written inside
/// [`BOOT_LAUNCH_BURST_WINDOW`]. Pure, so the law is testable without a bundle.
fn launch_is_burst(attempts: u32, sentinel_age: Option<std::time::Duration>) -> bool {
    attempts > 0 && sentinel_age.is_some_and(|age| age < BOOT_LAUNCH_BURST_WINDOW)
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
    // Sentinel is the transaction commit marker and is published LAST. A crash
    // after trial.toml but before arm leaves no active authority and is harmless;
    // the reverse order could wedge forever with an armed build but no exact digest.
    if let Err(error) = sentinel.arm(ready.build_number) {
        crate::manifest::FailedMark::clear(&staging.trial());
        return Err(format!("arm boot-health trial: {error}"));
    }
    // A fresh trial owns the sentinel now, so any launches counted against the
    // PREVIOUS armed build are answered; the budget must never accumulate across
    // unrelated trials.
    let _ = foreign_trial_counter(staging).confirm();
    Ok(sentinel)
}

/// Is the armed trial (if any) OWNED by the install at `app_root`? The sentinel and
/// `trial.toml` are per user, but a build can be installed at several paths at once
/// (a dev machine's `dist/aterm.app` beside `/Applications/aterm.app`, a duplicate
/// copy of a release). A same-build process launched from a bundle the trial did
/// not swap must neither count launches against it (three sibling launches used to
/// revert and poison a build that never crashed), nor confirm it, nor disarm it as
/// dead authority. A marker without a recorded root (written before the field
/// existed) is treated as owned, which is the historical behaviour.
#[must_use]
fn trial_owned_by(staging: &Staging, app_root: &Path) -> bool {
    match crate::manifest::FailedMark::read(&staging.trial()).and_then(|trial| trial.install_root) {
        None => true,
        Some(recorded) => {
            let recorded = Path::new(&recorded);
            // An install that is GONE (moved or deleted mid-trial) owns nothing: if
            // only its ghost could count, confirm or budget the sentinel, every
            // launch of this build from anywhere else would defer forever. Whoever
            // runs this build now inherits the trial and its escapes. GONE means
            // `NotFound` precisely — an unmounted volume or a TCC-protected folder
            // answers another error and still owns its trial (fail-safe).
            match std::fs::symlink_metadata(recorded) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => false,
                Ok(_) => same_install_root(recorded, app_root),
            }
        }
    }
}

/// Path equality for install roots, tolerant of a canonicalizable spelling. The one
/// "is this the same copy of aterm.app?" test — the trial-ownership rule above and
/// the S12 `which_copy` report both ask it, so a symlinked spelling of the running
/// bundle can never read as a second copy on one surface and not the other.
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

/// DEAD AUTHORITY: the sentinel names a build strictly OLDER than the bundle we
/// are canonically installed as and running. Only an out-of-band install (a
/// `tools/install.sh` run, a DMG drag) landing inside an armed trial window can
/// produce it — and `MAX_BOOT_ATTEMPTS` is 3, so a trial legitimately survives two
/// launches that crash before the health checkpoint, which is exactly when a user
/// hand-installs a different build.
///
/// Disjoint from [`abandoned_preswap_trial`] by construction: the armed build sits
/// above the installed one there and below it here.
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
/// Leaving it armed is not the conservative choice, it is the permanent one. The
/// trial can no longer be confirmed (its build is not installed) and must not be
/// reverted (that would install a build older than the one already running), while
/// `check_boot_health` / `confirm_boot_health` / `confirm_trial_health_after_proof`
/// all early-out unless the armed build equals the running one — so nothing else
/// in the tree can ever clear it, and every future apply is refused forever. Same
/// reasoning as the wedged-trial escape hatch in `check_boot_health`.
///
/// The caller must have proved the shape first; `check_boot_health` already
/// returned for the `armed == process_build` case, so this cannot race a live
/// trial of ours.
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
    crate::manifest::FailedMark::clear(&staging.trial());
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

/// Recover the sole safe mismatched-sentinel crash cut under apply_lock: this
/// process's OLD build is still canonically installed while the armed build is
/// newer. Because apply_lock is held, a live concurrent swap must finish before
/// this observation; canonical OLD proves NEW is not installed. This covers both a
/// pre-swap crash and a crash immediately after inverse rollback.
///
/// Also the one place a [`dead_authority_trial`] — a sentinel armed for a build
/// OLDER than the running one, which no other transition can reach — is retired.
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
    // Placed AFTER the identity resolution on purpose: an installed bundle we
    // cannot verify still fails closed, disarming nothing. And ONLY the install the
    // trial swapped may retire it: a newer sibling bundle at another path sees
    // "armed for an older build than me" too, and disarming from there stripped
    // the live install's crash-loop protection and cleared its receipt.
    if dead_authority_trial(process_build, installed_build, armed_build)
        && trial_owned_by(staging, installed)
        && identity_matches_running(
            installed_build,
            &installed_commit,
            process_build,
            process_commit,
        )
    {
        // Do NOT `retire_published()` here (unlike the check_boot_health escape
        // hatch): returning true lets the caller go on to apply a legitimately
        // staged newer build in this same launch, and retiring would throw it away.
        return disarm_dead_authority_trial(
            staging,
            installed,
            armed_build,
            installed_build,
            &installed_commit,
            process_build,
        );
    }
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
        // Cross-volume staging COPIES staged_app -> fixed and leaves the staged
        // copy intact, so when the staged bundle already carries the exact
        // candidate identity the fixed twin is the redundant copy — reclaim it
        // and keep the publication. Deleting the staged copy first just to fail
        // the rename back (EXDEV whenever the install volume differs from HOME,
        // i.e. every external-volume install) destroyed the one copy the
        // published authority still pointed at.
        if verified_bundle_identity(&staging.staged_app)
            .is_ok_and(|(build, commit)| build == candidate_build && commit == candidate_commit)
        {
            let _ = remove_path_no_follow(&fixed);
        } else {
            let _ = remove_path_no_follow(&staging.staged_app);
            if std::fs::rename(&fixed, &staging.staged_app).is_err() {
                // EXDEV (or any restore failure): the bytes cannot reach the
                // staging path from here. Retire the publication and reclaim the
                // fixed candidate so THIS launch settles at NoUpdate and the next
                // check re-stages cleanly — instead of returning an unrecovered
                // trial that defers every launch behind a marker naming bytes
                // nothing can restore. Pre-swap the fixed path is NEW, never a
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
    crate::manifest::FailedMark::clear(&staging.trial());
    let receipt_matches = crate::manifest::InstalledReceipt::read(&staging.installed_receipt())
        .is_some_and(|receipt| receipt.matches_sealed(installed_build, &installed_commit));
    if !receipt_matches {
        crate::manifest::InstalledReceipt::clear(&staging.installed_receipt());
    }
    true
}

/// Budgeted escape for a boot sentinel armed for a build that is NOT the running
/// one and that [`recover_abandoned_preswap_trial_if_exact`] could not resolve.
///
/// Such a sentinel is otherwise permanent. Every lane that clears one — the
/// launch counting and revert in `check_boot_health`, `confirm_boot_health`,
/// `confirm_trial_health_after_proof` — early-outs unless the armed build equals
/// the running build, and [`prepare_trial`], the only writer, is gated behind the
/// very check that is failing, so the stuck sentinel blocks its own replacement.
/// The observable result is the "staged and ready but never applies" signature:
/// every launch returns `Deferred` while the background stager keeps downloading
/// and publishing, with no client-side repair short of deleting the file by hand.
/// The shapes that reach here are the ones the exact recovery deliberately
/// refuses (a rollback sibling that is not the armed build, an unverifiable
/// install, an armed build below a running build we are not canonically installed
/// as), so refusing forever is not evidence of anything except that this launch
/// cannot prove the trial's story.
///
/// So budget it exactly the way the same-build lane is budgeted: after
/// [`MAX_BOOT_ATTEMPTS`] launches that observed the SAME unrecoverable armed
/// build, disarm. The count lives in [`foreign_trial_counter`], never in the boot
/// sentinel itself.
///
/// Returns whether the sentinel was disarmed. The caller still defers on the
/// disarming launch: recovery state was just mutated, and the next launch takes
/// the normal path (`recover_orphaned_prepared_candidate` requires an absent
/// sentinel), so nothing is applied on the same pass that repaired it. That
/// deferral is itself recorded by [`apply_staged_if_ready`], whose refusal line
/// supersedes the status line written here for this launch — the durable record
/// of the repair is the health ledger entry and the log warning below.
fn escape_wedged_foreign_trial(staging: &Staging, current_build: u64, armed_build: u64) -> bool {
    let counter = foreign_trial_counter(staging);
    // A LIVE TRIAL IS NOT A WEDGED ONE, and the staging root is shared by every copy
    // this user runs (`~/Library/Application Support/aterm/Updates` — per USER, not per
    // install). So a second installation — a locally built .app beside the released one
    // is the ordinary case — can be mid-trial on a build this process does not run, and
    // three launches of THIS copy would otherwise disarm it. That would take the
    // crash-loop protection away from the very build that is crash-looping: its own
    // `check_boot_health` would find no sentinel and never revert.
    //
    // The trial's owner is the only party that advances the sentinel (`observe_launch`
    // counts only when the armed build is the RUNNING one), and it rewrites the file to
    // do so. So "did anyone touch this trial while we were counting" is answerable
    // without a clock, a pid, or new state: compare the sentinel's mtime against our own
    // counter's. Advanced ⇒ the owner is alive ⇒ restart the budget and defer. Frozen
    // across our whole budget ⇒ nobody is left to confirm or revert it, which is the
    // wedge this escape exists for.
    let sentinel = boot_sentinel(staging);
    let counter_state = counter.read_state();
    let advanced = mtime(&staging.root.join("boot.sentinel"))
        .zip(mtime(&foreign_trial_path(staging)))
        .is_some_and(|(trial, ours)| trial > ours);
    if advanced {
        // The owner moved since we last counted. Restart the budget, and do NOT count
        // this launch: a trial that is being actively observed is not abandoned.
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
    // A trial OWNED BY ANOTHER INSTALL THAT STILL EXISTS keeps its protection unless
    // that owner has been idle for a long time: three launches of a sibling copy in
    // one afternoon must not strip the crash-loop revert from the install that is
    // actually mid-trial (2026-08-19 round-3 audit). Idle = the sentinel untouched
    // for `FOREIGN_TRIAL_OWNER_IDLE` — the owner's every launch rewrites it.
    let owned_elsewhere = crate::manifest::FailedMark::read(&staging.trial())
        .and_then(|trial| trial.install_root)
        .is_some_and(|root| {
            let root = Path::new(&root);
            // Present unless the path is demonstrably GONE (`NotFound`): an
            // unmounted volume or a TCC-protected folder still owns its trial,
            // exactly as `trial_owned_by` judges it.
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
    crate::manifest::FailedMark::clear(&staging.trial());
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
/// put the bytes back; a mismatched/corrupt fixed path is preserved and never
/// adopted, so an unproven candidate is still refused.
///
/// When there is nothing to adopt, though, the marker itself is DANGLING and this
/// function retires it — see the comment on that branch for why deferring instead
/// wedged every future launch.
fn recover_orphaned_prepared_candidate(
    staging: &Staging,
    installed: &Path,
    current_build: u64,
    ready: &Ready,
) -> Result<(), String> {
    if is_non_symlink_dir(&staging.staged_app) || boot_sentinel(staging).read_state().is_some() {
        return Ok(());
    }
    // Past this point `ready.toml` advertises bytes that are NOT at `staged_app`, and
    // the fixed path is the only place they could still be.
    let fixed = rollback_path(installed);
    let recoverable = verified_bundle_identity(&fixed).is_ok_and(|(build, commit)| {
        same_volume(&fixed, installed)
            && build > current_build
            && ready_matches_verified_identity(ready, build, &commit)
    });
    if !recoverable {
        // The marker names bytes that exist NOWHERE — the ordinary shape being a fixed
        // path that is just the previous build's retained rollback, or no fixed path at
        // all. Returning Err here made every launch a `Deferred` whose message was about
        // an "orphaned fixed candidate", i.e. a rollback path with nothing to do with the
        // operator's actual problem; and because this runs BEFORE the `is_publishable`
        // gate further down, the retirement that would have cleared the dangling marker
        // was unreachable. Nothing on disk could ever satisfy it, so the deferral was
        // permanent with no client-side repair short of deleting the file by hand.
        //
        // Retire it and report success instead: retirement touches only `ready` and the
        // already-absent `staged_app`, the caller re-reads and sees `Absent` → `NoUpdate`,
        // and the next check re-stages. The fixed path is deliberately LEFT ALONE — it may
        // be the genuine rollback source crash recovery depends on, and declining to adopt
        // an unproven candidate is exactly the fail-closed behavior kept from before.
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
        // EXDEV on an external-volume install (fixed sits beside the bundle on
        // the install volume; staging lives under HOME), or any other restore
        // failure: an Err here made every launch defer behind a marker whose
        // bytes can never reach the staging path. Same remedy as the
        // nothing-to-adopt branch above — retire the marker, report success, and
        // let the next check re-stage; the fixed path is deliberately LEFT ALONE
        // (it may double as the rollback-shaped source a later recovery reads).
        crate::warn(&format!(
            "cannot restore the orphaned fixed candidate to the staging path ({error}); \
             retiring the dangling marker so the next check re-stages"
        ));
        staging.retire_published();
    }
    Ok(())
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
    staged_identity: (u64, &str),
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
        // The one genuinely slow step on this path: a several-hundred-megabyte
        // copy between volumes, run while the apply lock is held and before the
        // window exists. It was the ONLY child on the apply path with no ceiling
        // at all — an unplugged or wedged external volume meant a launch that
        // never finished and a lock every other launch queued behind.
        let copy_started = std::time::Instant::now();
        let (status, stderr) = ditto_bounded_clean(
            &[staged.into(), fixed.as_os_str().to_os_string()],
            "ditto to fixed swap path",
            CROSS_VOLUME_COPY_TIMEOUT,
            staged.parent().unwrap_or(staged),
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
    // identity the CALLER just read, because `rename(2)` moved the very inode it
    // read it from: same bytes, same signature, same plist. Re-running the five
    // codesign/spctl/PlistBuddy helpers over it would re-answer a question
    // nothing could have changed the answer to — the apply lock is held, the
    // parent is a private directory, and `checked_bundle_exchange` revalidates
    // the no-follow shape again at the point of no return. That redundant pass
    // was five of the sixteen helper spawns on the launch path.
    //
    // The cross-volume path DOES re-verify: `ditto` produced new bytes at a new
    // path, so its identity is genuinely unestablished.
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
/// Crate-visible door onto [`verified_bundle_identity`] for the installed-bundle
/// activation pre-verify (`crate::preverify_installed_for_handoff`): the same policy
/// gate + sealed identity read the staged-candidate path uses, aimed at the bundle
/// under the running executable.
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
    /// The sealed build the validation proved (2026-09-14): the revert names it,
    /// and compares it with the operator floor — a crash loop is strictly worse
    /// than a yank, so the revert still happens, but a restored build below the
    /// floor is said out loud instead of the machine silently running a yanked
    /// build the activation lane would have refused.
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
        return Ok(verified_rollback);
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
        // must not fail on the first refusal. Both mitigations below are cheap and
        // no-ops on the happy path, and they are right for a genuinely transient
        // refusal (memory pressure, a device attaching slowly).
        //
        // WHAT THEY DO NOT FIX. 2026-07-31 (v0.10.0): every machine reported
        //   hdiutil attach failed: hdiutil: attach failed - Device not configured
        // (ENXIO) from inside the app, while the IDENTICAL command — same DMG,
        // same `-mountpoint` under the same 0700 dir, same environment copied
        // from the running process — succeeded every time from a shell. The cause
        // is the attaching process's BOOTSTRAP CONTEXT, not the image and not a
        // race: after a seamless overlap update the surviving aterm is a
        // fork-child whose parent (the launchd job's process) has exited, so it
        // is an orphan holding a bootstrap context for a dead job. DiskImages
        // registers with the `com.apple.hdiejectd` XPC service through that
        // context, and a lookup in a dead domain can never succeed — so no number
        // of retries and no choice of mount point helps. The only fix is to not
        // use hdiutil at all, which is what `stage_from_zip` does; this path
        // survives for releases whose manifest carries no zip.
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
            None => {
                let parsed = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(|line| line.split('\t').nth(2))
                    .map(str::trim)
                    .find(|candidate| !candidate.is_empty())
                    .map(PathBuf::from);
                match parsed {
                    Some(found) => Ok(Self { mountpoint: found }),
                    // The image DID attach; only naming it failed. Returning a
                    // bare Err here would drop the `Mounted` guard that has not
                    // been built yet and leave the device attached for the life
                    // of the process, so detach by DEVICE NODE — column 0 of the
                    // same table, which is present even when the mount column is
                    // not — before reporting the failure.
                    None => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        for dev in stdout
                            .lines()
                            .filter_map(|line| line.split('\t').next())
                            .map(str::trim)
                            .filter(|dev| dev.starts_with("/dev/"))
                        {
                            let _ = Command::new("/usr/bin/hdiutil")
                                .args(["detach", "-force"])
                                .arg(dev)
                                .output();
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

/// The zip path's counterpart to [`sweep_stale_mounts`]: reclaim leftover
/// `zx-*` extract dirs from a run that was killed mid-stage. Nothing needs
/// detaching (that is the entire point of the zip container) — an abandoned
/// extract is just a directory, but it can be a whole app bundle's worth of
/// bytes inside the user's Application Support, so it must not accumulate.
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

/// BOTH LANES SWEEP BOTH KINDS OF SCRATCH (2026-09-14): every current release
/// ships a zip, so a `mnt-<pid>` a DMG-era stage left behind (a process
/// killed mid-stage; on the owner's machine `Updates/mnt-21441` from
/// 2026-08-31 survived every stage and an apply) was never reclaimed by the
/// lane that now runs, and an image still attached behind it stayed attached
/// until reboot. Each lane runs this before it unpacks, so a container
/// `ditto` cannot even open still reclaims the other lane's leftover.
fn sweep_stale_scratch(staging: &Staging) {
    sweep_stale_mounts(staging);
    sweep_stale_extracts(staging);
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
///
/// `current_commit` is the RUNNING image's provenance, and it is here for the
/// second half of the gate — see [`preverify_installed_rollback_source`]. A
/// staged apply is a SWAP, and a swap has two preconditions, not one: the
/// incoming bundle must be authentic, AND the outgoing bundle must be able to
/// become the rollback source the swap installs at the fixed path. Only the
/// first was hoisted here originally, and the second is the one that fails on a
/// hand-installed bundle — silently, inside the successor, five seconds after
/// the terminal parked.
pub fn preverify_staged_handoff_candidate(
    current_build: u64,
    current_commit: Option<&str>,
    expected_build: Option<u64>,
    expected_commit: Option<&str>,
) -> Result<(), String> {
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
    )
}

/// THE OTHER HALF OF A SWAP'S PRECONDITION: can the bundle we are running from
/// become the rollback source the swap is about to install at the fixed path?
///
/// `apply_staged_if_ready` asks exactly this at step 7, immediately before it
/// arms the crash-loop sentinel — "a corrupt/replaced install must never become
/// crash-recovery authority" — and refuses with `Deferred` when the answer is
/// no. That refusal is CORRECT and stays. What was wrong is WHERE the question
/// was first asked: inside the SUCCESSOR's boot, after the outgoing process had
/// already parked every reader, duplicated every PTY master, written a manifest
/// and asked LaunchServices for a whole new application job.
///
/// A successor that cannot swap stays the OLD build, so it fails
/// `seamless::take_target_identity` ("the outgoing process authorized build X,
/// but this binary is build Y"), drops the readiness pipe it was handed, and
/// exits. The parked parent reads EOF and books the one outcome that means the
/// new bytes crashed — `ChildDied` — against a build that was never even
/// executed. Observed in the field: five consecutive identical failures,
/// `failing_applies=5 persistent=true`, five frozen terminals, and not one line
/// anywhere naming the cause.
///
/// So the question moves here, where it is asked while every reader is still
/// live and the answer costs the user nothing but a log line. This is strictly
/// ADDITIVE, exactly like the staged half above: it can only ever refuse an
/// attempt the swap was going to refuse anyway, and the swap re-runs it under
/// `apply_lock` regardless.
/// **THE REMEDY, said where the refusal is minted** (2026-09-14, the owner's
/// desk): an installed bundle that fails the team-pinned policy can never be
/// updated by ANY lane — the seamless apply refuses it as the rollback source
/// here, the cold-launch apply refuses it at step 7 for the same reason — and
/// until now neither refusal said what to do. A local build that was swapped
/// into `/Applications` by hand (ad-hoc signed, no `ATermDevBuild` mark) is a
/// state the owner's own workflow produces routinely; it sat on v0.85.0 for
/// eight hours with six refusals and a red bar, and the only fix was never
/// written anywhere. Both lanes now append this sentence; the trouble
/// classifier keys on its first clause (`ROLLBACK_SOURCE_REFUSAL_KEY`) to
/// render the cause and the remedy on the Software Update page and to stop
/// scheduling retries that cannot succeed.
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

fn preverify_installed_rollback_source(
    installed: Option<&Path>,
    current_build: u64,
    current_commit: Option<&str>,
) -> Result<(), String> {
    let Some(installed) = installed else {
        // `bundle::resolve` is what the apply itself calls, and `None` there is
        // `ApplyOutcome::NotApplicable` — the updater is inert for this process
        // (no `.app` layout, a translocated or DMG launch, or a bundle carrying
        // the `ATermDevBuild` mark). A handoff whose successor's swap will be a
        // no-op can only ever end as a wrong-build refusal, so say so now.
        return Err(
            "this process does not run from an installed bundle the updater may replace, so a \
             staged build cannot become its successor"
                .to_string(),
        );
    };
    match verified_bundle_identity(installed) {
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
        Err(error) => Err(rollback_source_refusal(installed, &error)),
    }
}

/// Injectable core of [`preverify_staged_handoff_candidate`]; the split exists
/// so the refusal ladder is provable against a temp staging root without a
/// signed fixture bundle (a missing/unsigned candidate must refuse BEFORE any
/// caller could park a reader on its behalf). `installed` is the resolved
/// bundle root the swap would replace, injected for the same reason.
fn preverify_staged_handoff_candidate_at(
    staging: &Staging,
    installed: Option<&Path>,
    current_build: u64,
    current_commit: Option<&str>,
    expected_build: Option<u64>,
    expected_commit: Option<&str>,
) -> Result<(), String> {
    // Serialize against a concurrent publication/apply exactly like the swap
    // path: verifying a half-published candidate proves nothing.
    //
    // BOUNDED, like the launch path's taker at `apply_staged_if_ready` and for
    // a harsher reason: the seamless worker calls this with EVERY PTY reader
    // already parked — the whole terminal frozen. An unbounded `acquire`
    // against a wedged holder (SIGSTOPped, debugger, dead volume) froze the
    // screen indefinitely, deferred Cmd+Q behind the stuck attempt, and made
    // every later apply answer "an update handoff is already in flight"
    // (2026-09-01 audit). A timeout is an ordinary refusal: overlap rolls
    // back, readers resume, the stage stays armed for the next attempt.
    let _lock = FileLock::acquire_within(&staging.apply_lock, APPLY_LOCK_WAIT)
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
    // LAST, deliberately. Every check above is about the INCOMING bytes and is
    // the more common refusal, so it keeps naming itself first; this one is
    // about the machine's install and, when it fires, it fires forever.
    preverify_installed_rollback_source(installed, current_build, current_commit)
}

/// Upper bound on the changelog text a `ready.toml` may carry.
///
/// The marker is a SMALL ledger: every reader reaches it through
/// `read_ledger_text`, which returns `None` for a file above `MAX_LEDGER_BYTES`. So
/// an oversized changelog does not make the marker BIG, it makes it INVISIBLE — and
/// nothing upstream bounded it: the publisher's changelog extraction runs to EOF when
/// a heading is malformed, and CHANGELOG.md is ~382 KiB. The result was the worst
/// shape this crate can produce: the stage reported success and the status file said
/// "verified and ready to apply", while `publishable_stage_covers` read the marker as
/// absent and re-downloaded the whole container every cadence tick forever, and every
/// launch read the marker as `Corrupt` so the build never applied.
///
/// A few thousand characters is far more release note than any surface shows, and the
/// authoritative changelog lives in the release itself.
const MAX_READY_CHANGELOG_CHARS: usize = 4096;

/// Appended in place of the discarded tail so a clamped changelog reads as truncated
/// rather than as a release note that stops mid-sentence.
const READY_CHANGELOG_TRUNCATED: &str = "\n… (changelog truncated)";

/// Clamp the manifest's changelog to what a marker may carry.
///
/// The cut is by CHARACTERS, never bytes: release notes routinely contain em dashes
/// and emoji, and slicing a `str` at a byte offset inside a multi-byte UTF-8 sequence
/// panics. `char_indices().nth(N)` yields an offset that is a char boundary by
/// construction, so the slice below cannot split a sequence.
fn clamp_ready_changelog(changelog: Option<&str>) -> Option<String> {
    let text = changelog?;
    match text.char_indices().nth(MAX_READY_CHANGELOG_CHARS) {
        None => Some(text.to_string()),
        Some((cut, _)) => Some(format!("{}{READY_CHANGELOG_TRUNCATED}", &text[..cut])),
    }
}

/// Publish one already-verified incoming bundle as a short transaction shared
/// with the apply path. The long download/extract/verification work remains under
/// `stage_lock` only; compliant callers therefore acquire locks in the sole nested
/// order `stage_lock -> apply_lock`. Apply never acquires `stage_lock`, so startup
/// cannot wait on a download and the two lanes cannot deadlock.
fn publish_verified_stage(staging: &Staging, incoming: &Path, ready: &Ready) -> Result<(), String> {
    let marker = ready.to_toml()?;
    // A marker above the shared ledger cap is not a big marker, it is an ABSENT one:
    // `read_ledger_text` refuses it, so the status surface, the stage-coverage check
    // and the apply gate all see "nothing staged" while a fully verified bundle sits on
    // disk — the container then re-downloads forever and the build never applies.
    // `clamp_ready_changelog` bounds today's only unbounded field; THIS guard is what
    // stops a future field from silently reintroducing an unreadable marker.
    //
    // It runs before the lock and before the old generation is invalidated, so a
    // rejected publish leaves the previously staged (readable) generation exactly as it
    // was rather than trading a working stage for an invisible one.
    if u64::try_from(marker.len()).unwrap_or(u64::MAX) > crate::MAX_LEDGER_BYTES {
        return Err(format!(
            "refusing to commit a {}-byte ready marker: the ledger cap every reader \
             enforces is {} bytes, and a marker above it reads as absent",
            marker.len(),
            crate::MAX_LEDGER_BYTES
        ));
    }
    let _publish_lock =
        FileLock::acquire(&staging.apply_lock).map_err(|error| format!("publish lock: {error}"))?;

    // Invalidate the old generation first. Lock-free status readers may briefly
    // observe "absent", but never an old marker paired with the new bundle.
    let _ = std::fs::remove_file(&staging.ready);
    let _ = std::fs::remove_dir_all(&staging.staged_app);
    std::fs::rename(incoming, &staging.staged_app)
        .map_err(|error| format!("publish staged bundle: {error}"))?;

    // The marker remains the commit point and is written last — DURABLY: it is the
    // fallback link of the same recovery-proof chain as the trial identity and the
    // installed receipt (`ensure_current_trial_receipt` falls back to it when the
    // receipt is absent), so a zero-length one after a kernel panic would strand the
    // very repair those two were made durable for (2026-08-19 round-4 skeptics).
    crate::manifest::write_durable(&staging.ready, &marker, "ready marker")
}

/// Stage a verified copy of the bundle from a downloaded (sha256-checked) DMG:
/// mount, `ditto`-extract the `.app`, verify it (codesign/Team-ID/spctl), then
/// publish `staged/aterm.app` + write `ready.toml` LAST. The ready marker's
/// presence is the sole "ready" signal, so writing it last (atomic rename) means
/// a reader never sees a half-staged bundle.
///
/// [`stage_from_zip`] is the preferred path when the release carries a zip; both
/// share [`verify_and_publish_incoming`], so the two differ ONLY in how the
/// bundle is unpacked.
pub fn stage_from_dmg(
    staging: &Staging,
    dmg: &Path,
    manifest: &Manifest,
    expected_team: &str,
) -> Result<(), String> {
    let manifest_commit = staging_manifest_commit(manifest)?;
    ensure_private_dir(&staging.staged_dir()).map_err(|e| format!("staged dir: {e}"))?;
    // Clean up any mount a previously-killed run leaked, then mount at a fresh private
    // mountpoint under our 0700 dir (never /Volumes).
    sweep_stale_scratch(staging);
    let mountpoint = staging.root.join(format!("mnt-{}", std::process::id()));
    let mounted = Mounted::attach(dmg, &mountpoint)?;
    let src = mounted.mountpoint.join("aterm.app");
    // `symlink_metadata` for the same reason the zip path uses it: the container
    // controls this entry, and a symlink here would be copied into `staged/` as
    // a link that can never be applied but IS published as ready.
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
    // cannot hold it before a byte lands (2026-09-14, see `refuse_without_room`).
    refuse_without_room(&staging.staged_dir(), tree_bytes(&src), "the update bundle")?;
    // `ditto` (not `cp -R`) preserves extended attributes + the _CodeSignature
    // layout, so the copied bundle's signature stays valid.
    let (status, stderr) = ditto_bounded_clean(
        &[
            src.as_os_str().to_os_string(),
            incoming.as_os_str().to_os_string(),
        ],
        "ditto",
        STAGE_UNPACK_TIMEOUT,
        &staging.staged_dir(),
    )
    .inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&incoming);
    })?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(ditto_failure("ditto extract failed", status, &stderr));
    }
    // detach the DMG now; everything we need is in `incoming`.
    drop(mounted);

    verify_and_publish_incoming(
        staging,
        &incoming,
        manifest,
        &manifest_commit,
        expected_team,
    )
}

// ---------------------------------------------------------------------------
// Bounding the zip extract — what unpacking this archive would cost, read out
// of the archive's own central directory BEFORE `ditto` is allowed to run.
// ---------------------------------------------------------------------------

// Trait/type imports scoped to the zip reader below; the rest of this file
// spells its occasional `std::io` use out in full, but four repetitions of
// `std::io::SeekFrom::Start` earn the import.
use std::io::{BufReader, Read, Seek, SeekFrom};

/// Hard ceiling on the UNCOMPRESSED bytes an update archive may DECLARE before
/// [`stage_from_zip`] will hand it to `ditto`.
///
/// WHY A CEILING EXISTS AT ALL. The download is capped (`github.rs` passes
/// [`aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND`] to `download_to`) and the
/// archive's sha256 is checked against the release manifest before extraction,
/// so nothing reaches this point that the publisher did not sign for. But
/// deflate EXPANDS, `ditto` has no size limit of its own, and one bad release is
/// unpacked by every machine on the channel at once. A volume with no free space
/// cannot stage the fix that would undo it, so the failure has no in-band
/// recovery — it is a fleet-wide availability event, not a per-machine
/// annoyance. Refusing on the archive's own declared size turns "every disk is
/// full" into "the fleet stayed on the previous build and said why", which the
/// caller's stage-failure path already knows how to sit on.
///
/// WHY THIS NUMBER, AND WHY IT IS NOT ITS OWN. It is the download bound itself,
/// and it must never sit BELOW it. The cutter validates every published asset
/// against that one constant (`UPDATER_MAX_DMG_BYTES` in `aterm-release`) and has
/// no extraction gate of its own, so a stricter cap here is a hole the publisher
/// cannot see: a container the cutter happily ships stages nowhere, on every
/// machine on the channel, deterministically, and the stage-failure backoff just
/// re-downloads it once a day forever.
///
/// The retired 1 GiB was picked when "the signed bundle is well under 100 MB
/// uncompressed" was true of every cut. It is not: a batteries-included cut seals
/// `Contents/Resources/toolchain-seed` into the SAME signed `.app` this archive
/// carries, and one has already been observed at ~775 MB — three quarters of that
/// cap, with a payload of already-compressed `.tar.zst` artifacts whose declared
/// uncompressed size is roughly their packed size. Tying the two together removes
/// the drift rather than re-guessing the headroom. The cap still does its job at
/// the bound: it refuses any archive DECLARING more than the largest asset the
/// publisher could ever have shipped, so an honestly-enormous zip is still
/// refused before one byte is written.
const MAX_EXTRACT_BYTES: u64 = aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND;

/// Companion ceiling on the archive's entry COUNT, because bytes alone do not
/// bound the damage: a million empty files declare zero uncompressed bytes and
/// still cost a filesystem block and an inode apiece. MEASURED, not estimated:
/// the shipped `aterm-0.12.0-mac.zip` declares 48 entries / 51,203,501
/// uncompressed bytes, and a fresh `ditto -c -k --sequesterRsrc --keepParent`
/// of the installed bundle gives 48 / 51,154,253 — so
/// this leaves three orders of magnitude of headroom while holding the
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

/// Read exactly `len` bytes at an absolute offset. A short read is an error, not
/// a short buffer: every caller here is reading a FIXED-layout record, so "the
/// file ended early" means the record is not there.
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

/// Locate the End Of Central Directory record and read its classic 32-bit view
/// of the directory. Returns the record's absolute offset alongside it, because
/// everything the directory describes must lie strictly before it.
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
    // A four-byte signature also occurs inside compressed data and inside the
    // archive comment, so the signature alone identifies nothing. What makes a
    // candidate real is that its declared comment length runs EXACTLY to end of
    // file.
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
        // TWO valid candidates means the archive has a decoy trailer, and
        // nothing here can know which one `ditto` will follow — so the bound
        // would be measured off a record that is not the one being extracted.
        // Refuse. An honest archive cannot hit this by accident: it needs the
        // four signature bytes AND a comment length that lands exactly on EOF,
        // which is ~2^-48 per position over a ~64 KiB scan.
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

/// The ZIP64 view of the directory, when the archive carries one. `Ok(None)`
/// means there is no ZIP64 locator at all (an ordinary small archive); once a
/// locator IS present every further problem is a refusal, because the archive
/// has declared that the 32-bit record cannot describe it.
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

/// The uncompressed size out of a central directory entry's ZIP64 extended
/// information field. That field carries ONLY the values that overflowed 32
/// bits, in a fixed order with the uncompressed ("original") size first — so an
/// entry that reaches here (its 32-bit size saturated) has it at data offset 0.
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

/// Walk the central directory, summing uncompressed sizes and counting entries.
///
/// The caps are enforced INSIDE the walk, not after it: a directory claiming a
/// billion entries has to cost a bounded read, not a billion iterations.
fn central_directory_claim(
    file: &mut std::fs::File,
    extent: DirectoryExtent,
) -> Result<ZipExtractClaim, String> {
    file.seek(SeekFrom::Start(extent.offset))
        .map_err(|error| format!("seek update zip central directory: {error}"))?;
    let mut reader = BufReader::new(file);
    // The caller proved the extent lies inside a file the download cap held
    // under 512 MiB, so this cannot fail on any host aterm runs on; written as a
    // conversion rather than a cast so a narrower target refuses instead of
    // wrapping.
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
            // 0xFFFFFFFF is the ZIP64 escape. It is also a legal literal size,
            // but a ~4 GiB member already blows MAX_EXTRACT_BYTES on its own, so
            // reading it as the escape can only make this stricter, never laxer.
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

/// Read the archive's CENTRAL DIRECTORY and refuse anything claiming to unpack
/// to more than [`MAX_EXTRACT_BYTES`] or [`MAX_EXTRACT_ENTRIES`].
///
/// HAND-ROLLED ON PURPOSE. This reads four fixed-layout records and never
/// inflates a byte. The update path is the one place in aterm where a new
/// dependency is also a new way to lose the whole fleet, so a page of
/// little-endian field reads beats pulling a zip crate in behind it.
///
/// FAIL CLOSED. Every malformed, truncated, ambiguous or unreadable directory is
/// a refusal, never an "assume fine" — a bound the archive can opt out of by
/// being broken is not a bound. In particular the entry count the walk observes
/// must equal the count the end record declares, so a directory that describes a
/// different archive than its own trailer cannot slip past on either reading.
///
/// WHAT THIS DOES AND DOES NOT PROVE. It bounds what the archive DECLARES, read
/// out of the record any extractor must consult to know what the archive holds.
/// It is not a proof about `ditto`: an extractor that streamed local file headers
/// and ignored the directory could still be fed members the directory never
/// mentions, and that residue is bounded only by the download cap times deflate's
/// expansion. Closing it needs a watchdog that kills `ditto` when the extract
/// directory crosses the cap, which is a separate change. What this DOES close is
/// every archive that honestly says it is enormous plus every malformed shape in
/// between — the realistic failure being a release built wrong, not a signer gone
/// hostile (a hostile signer already ships the binary).
fn checked_zip_extraction_claim(zip: &Path) -> Result<ZipExtractClaim, String> {
    let mut file = std::fs::File::open(zip).map_err(|error| format!("open update zip: {error}"))?;
    let file_len = file
        .metadata()
        .map_err(|error| format!("stat update zip: {error}"))?
        .len();
    let (eocd_offset, classic) = find_end_of_central_directory(&mut file, file_len)?;
    let extent = match zip64_directory_extent(&mut file, eocd_offset)? {
        Some(zip64) => {
            // Where the 32-bit record is NOT saturated it must agree with the
            // 64-bit one. A disagreement means the two records describe
            // different archives, and nothing here can know which one `ditto`
            // will follow — so neither is trusted.
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
            // A sentinel with no ZIP64 record behind it is an archive shape this
            // reader cannot describe. Unhandled is a REFUSAL, never "assume
            // fine": the saturated field is exactly the one the cap is measured
            // against.
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

/// Stage a verified copy of the bundle from a downloaded (sha256-checked) ZIP:
/// `ditto -x -k` extract, then the IDENTICAL verification + publication
/// [`stage_from_dmg`] performs ([`verify_and_publish_incoming`]).
///
/// WHY THIS EXISTS — and why it is preferred over the DMG path: `hdiutil attach`
/// fails with ENXIO ("Device not configured") in the one process that most needs
/// to stage. After a seamless overlap update the surviving aterm is a fork-child
/// whose parent (the launchd job's process) has exited, so it is an orphan
/// holding a bootstrap context for a dead job. DiskImages registers with the
/// `com.apple.hdiejectd` XPC service through that context, and the lookup in a
/// dead domain cannot succeed — which is why the identical command runs fine from
/// a shell and never from the app. `ditto` needs no XPC service, so unpacking a
/// zip works from ANY process context.
///
/// Nothing about the trust model changes: the zip's bytes are checked against the
/// manifest digest before this is called, and the extracted bundle then goes
/// through the same codesign/Team-ID/sealed-identity gate. What the digest does
/// NOT bound is how far those bytes EXPAND, so the unpack is gated on
/// [`checked_zip_extraction_claim`] first — see [`MAX_EXTRACT_BYTES`] for why an
/// authentic-but-malformed archive is still a fleet-wide availability problem.
pub fn stage_from_zip(
    staging: &Staging,
    zip: &Path,
    manifest: &Manifest,
    expected_team: &str,
) -> Result<(), String> {
    let manifest_commit = staging_manifest_commit(manifest)?;
    // Bound the unpack BEFORE any filesystem work: `ditto` has no size limit of
    // its own, so the archive's own central directory is the only place to learn
    // what extracting it would cost — and refusing here costs nothing, because
    // not one byte has been written yet.
    let claim = checked_zip_extraction_claim(zip)?;
    crate::log(&format!(
        "update zip declares {} entries / {} uncompressed bytes \
         (caps {MAX_EXTRACT_ENTRIES} / {MAX_EXTRACT_BYTES})",
        claim.entries, claim.uncompressed_bytes
    ));
    ensure_private_dir(&staging.staged_dir()).map_err(|e| format!("staged dir: {e}"))?;
    // Reclaim any extract dir a previously-killed run leaked (same intent as
    // `sweep_stale_mounts`: nothing under our 0700 dir may accumulate — and the
    // DMG lane's own leftovers too (`sweep_stale_scratch`, 2026-09-14).
    sweep_stale_scratch(staging);
    // Extract into a fresh scratch dir rather than straight into `staged/`: the
    // archive's root entry is `aterm.app` (`--keepParent`), and unpacking that
    // name next to the PUBLISHED `staged/aterm.app` would collide with it.
    let extract = staging
        .staged_dir()
        .join(format!("zx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&extract);
    // The archive's own declared size, against the volume's free space: a full disk
    // used to surface as the bare "ditto zip extract failed (exit status: 1)" and a
    // re-download per backoff step (2026-09-14, see `refuse_without_room`).
    refuse_without_room(
        &staging.staged_dir(),
        claim.uncompressed_bytes,
        "the update zip",
    )?;
    std::fs::create_dir_all(&extract).map_err(|e| format!("create zip extract dir: {e}"))?;
    // `ditto -x -k` (not `unzip`) restores extended attributes and the sequestered
    // resource forks, so the extracted bundle's codesign seal stays intact.
    let (status, stderr) = ditto_bounded_clean(
        &[
            "-x".into(),
            "-k".into(),
            zip.as_os_str().to_os_string(),
            extract.as_os_str().to_os_string(),
        ],
        "ditto zip extract",
        STAGE_UNPACK_TIMEOUT,
        &staging.staged_dir(),
    )
    .inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&extract);
    })?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&extract);
        return Err(ditto_failure("ditto zip extract failed", status, &stderr));
    }
    let src = extract.join("aterm.app");
    // `symlink_metadata`, not `is_dir`: the archive controls this entry, and a
    // SYMLINK named `aterm.app` pointing at some other bundle would follow
    // through `is_dir` and then be renamed into `staged/` as a link. The apply
    // path and the status reader both refuse a symlinked staged bundle
    // (`install::…symlink_metadata`, `manifest.rs`), so it could never be
    // installed — but it would be published as "ready", which is a staging
    // success the DMG path would not have produced and a retry loop nobody can
    // clear. Refuse it here, where the container's own claim is still local.
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
    )
}

/// The manifest's git commit in canonical form — the one identity every stage
/// binds the signed bundle back to. Shared so neither container path can drift
/// into accepting a manifest the other would refuse.
fn staging_manifest_commit(manifest: &Manifest) -> Result<String, String> {
    canonical_release_commit(manifest.commit.as_deref()).ok_or_else(|| {
        "release manifest lacks a clean, valid git commit; refusing to stage".to_string()
    })
}

/// Verify an already-extracted incoming bundle and publish it as the staged
/// update. This is the WHOLE security gate of staging — codesign policy, the
/// manifest↔sealed-CFBundleVersion rebind, the sealed-commit rebind, then the
/// ready marker written LAST — and it is deliberately the single copy shared by
/// [`stage_from_dmg`] and [`stage_from_zip`]: which container the bytes arrived
/// in must never be able to change what is proved about them.
///
/// Every failure path removes `incoming`, so a refused candidate leaves no
/// half-staged bundle behind.
fn verify_and_publish_incoming(
    staging: &Staging,
    incoming: &Path,
    manifest: &Manifest,
    manifest_commit: &str,
    expected_team: &str,
) -> Result<(), String> {
    // Verify the extracted bundle before publishing it (tiered: full Developer-ID
    // check when a Team ID is pinned, else structural-only — see the crate trust model).
    if let Err(e) = verify::verify_bundle_policy(incoming, expected_team) {
        let _ = std::fs::remove_dir_all(incoming);
        return Err(format!("staged bundle failed verification: {e}"));
    }
    // Bind the (unauthenticated) manifest build_number to the number actually inside
    // the signed bundle — otherwise a manifest could claim a high build_number while
    // pointing at an OLD genuine signed bundle (a downgrade/replay via repo-write). The
    // CFBundleVersion is codesign-sealed, so reading it AFTER verify_bundle is sound.
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

    let ready = Ready {
        build_number: manifest.build_number,
        version: manifest.version.clone(),
        // Store canonical lowercase hex (like `dmg_sha256`) so `commit_matches` and any
        // display get a clean value; `None` stays `None`.
        commit: Some(manifest_commit.to_string()),
        // The manifest's `sha256` — the DMG digest — stays the staged artifact's
        // identity even when the bytes arrived as a zip: it is what the release
        // manifest, the ready marker, the failed-candidate memo and the apply-time
        // expected-artifact handoff all key on. Recording the container-specific
        // digest here would silently break that one identity.
        dmg_sha256: manifest.sha256.to_ascii_lowercase(),
        team_id: team,
        staged_at: now_rfc3339(),
        // Clamped, never copied verbatim. The manifest's changelog is publisher-side
        // text with no length bound anywhere in the pipeline, and an oversized one
        // pushes the whole marker past `MAX_LEDGER_BYTES` — which does not make the
        // marker large, it makes it unreadable to every consumer. See
        // `clamp_ready_changelog` for the failure this prevents.
        changelog: clamp_ready_changelog(manifest.changelog.as_deref()),
        // The pair the apply gate re-checks against the ratcheted floor. Both sit
        // inside the manifest's SIGNED bytes, so recording them here carries the
        // stage-time authorization forward without trusting anything new.
        machine_id: manifest.machine_id.clone(),
        roster_seq: manifest.roster_seq,
    };
    publish_verified_stage(staging, incoming, &ready)
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

/// The one outcome shape that means "a newer build is staged and this launch
/// REFUSED to apply it" — the entire answer to an operator's "the build is
/// staged, so why is it not running?".
///
/// `NotApplicable`/`NoUpdate` are the every-launch common case, so recording them
/// would rewrite `status.toml` on every single launch and neither is a refusal.
/// `ReExecFailed` is excluded too: it is a genuine FAILURE that already recorded
/// its own status line, and the refusal slot is the wrong ledger for it —
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

/// Apply a staged update if it is ready and strictly newer. On success this
/// re-execs and never returns. See module + crate docs for the full contract.
///
/// The body is `apply_staged_if_ready_inner`; this wrapper exists so that every
/// refusal the boot lane can return is recorded in ONE place. Most of the inner
/// deferrals used to be silent — no `status.toml` line, no health entry, nothing
/// in the app log — so the lane that runs on every cold launch could refuse the
/// same staged build forever while `status.toml` kept the stager's "verified and
/// ready to apply" line and `aterm ctl update status` reported a healthy updater.
/// The Deferred reason only ever reached stderr, which a Finder/launchd launch
/// throws away. A refusal nobody can observe reads exactly like an updater that is
/// not running — the same reasoning that put [`crate::record_apply_refusal`] on
/// the in-session lane, which this one never called.
///
/// Recording happens OUT here, after the inner call returned: the apply lock (and
/// `check_boot_health`'s) are both released by then, so the health ledger lock is
/// never taken underneath them. Do not "simplify" this by writing at each return.
pub fn apply_staged_if_ready(
    current_build: u64,
    current_commit: Option<&str>,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target_is_this_build: bool,
) -> ApplyOutcome {
    let outcome = apply_staged_if_ready_inner(
        current_build,
        current_commit,
        handoff_fds,
        handoff_env,
        handoff_target_is_this_build,
    );
    if let Some(reason) = boot_apply_refusal_reason(&outcome) {
        crate::warn(&format!("boot apply refused: {reason}"));
        if let Some(staging) = Staging::resolve() {
            crate::health::Health::record_apply_refusal(&staging.health(), current_build, reason);
            crate::status::record(
                &staging,
                current_build,
                &format!("boot apply refused: {reason}"),
            );
        }
    }
    outcome
}

fn apply_staged_if_ready_inner(
    current_build: u64,
    current_commit: Option<&str>,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target_is_this_build: bool,
) -> ApplyOutcome {
    // THE SWAP'S OWN COST, for the image that actually pays it. On a download
    // lane the parent launches the CURRENT binary, so this image is the one that
    // dittos the bundle and `execve`s — and `exec_preserving_handoff_fds` never
    // returns, so a timer in the GUI's `main_entry` below the call can never see
    // it. If a handoff is in flight, every millisecond spent here is spent with
    // the OUTGOING process's readers parked.
    let apply_started = std::time::Instant::now();
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
        && let Some(outcome) = check_boot_health(
            s,
            current_build,
            current_commit,
            handoff_fds,
            handoff_env,
            handoff_target_is_this_build,
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
    // WE ARE THE AUTHORIZED CANDIDATE of a seamless handoff (the outgoing process's
    // target names THIS build): an activation successor, or a download successor
    // already swapped and re-exec'd into its target. Everything above still ran —
    // the re-exec nonce and stamp are consumed, the boot-trial launch is counted,
    // startup authority is settled — but nothing below may SWAP: a newer stage on
    // disk re-exec'd from here would be a build the parent did not authorize; it
    // would refuse the target, drop the adopted PTYs and be booked as a structural
    // failure against a healthy candidate (2026-08-19 audit). The newer stage keeps
    // for the successor's own apply lane (or the next launch). `NoUpdate` is not a
    // refusal, so the ledger stays quiet.
    if handoff_target_is_this_build {
        crate::log(&format!(
            "boot apply: build {current_build} is the handoff's authorized target; any newer \
             stage is left for the successor's own apply lane (or the next launch)"
        ));
        return ApplyOutcome::NoUpdate;
    }
    // NEVER SWAP THE BUNDLE OUT FROM UNDER A TOOLCHAIN INSTALL. `atpkg seed` reads
    // the sealed payload inside this bundle by path, lazily, for minutes; the swap
    // below would leave its next read resolving into the replacement, which came
    // from the lean zip with the seal stripped. Deliberately AFTER the boot-health
    // lane above, so a crash-looping build can still revert (2026-08-20 round-8
    // audit) — and, since 2026-09-14, after startup authority is settled and the
    // handoff target has answered: the deferral guards the SWAP, and firing it
    // first booked "boot apply refused: a toolchain install is reading …" against
    // a just-swapped re-exec (a Matched nonce) and against an authorized handoff
    // successor — launches that ARE the successful apply — for as long as `atpkg
    // seed` held the marker after a launch.
    if crate::is_toolchain_install_active() {
        return ApplyOutcome::Deferred(
            "a toolchain install is reading this bundle's sealed payload".to_string(),
        );
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
    //
    // BOUNDED, unlike every other taker of this lock: we are before the window,
    // so waiting here is a terminal that has not appeared. Every legitimate
    // holder returns in millis (publish is remove+rename+write) or is itself
    // bounded (verification by the apply budget, a cross-volume copy by its own
    // ceiling). A holder that outlasts this one is wedged — SIGSTOPped, paused
    // under a debugger, stuck on a dead volume — and the right answer then is to
    // start on the build we already have, not to hang with nothing on screen.
    let _lock = match FileLock::acquire_within(&staging.apply_lock, APPLY_LOCK_WAIT) {
        Ok(l) => l,
        Err(e) => return ApplyOutcome::Deferred(format!("lock: {e}")),
    };
    // Everything from here to the swap is verification work on the launch path.
    // One ceiling over all of it (the guard restores any outer budget on drop).
    let _budget = crate::verify::ApplyBudget::start(crate::verify::APPLY_BUDGET);
    if !recover_abandoned_preswap_trial_if_exact(
        &staging,
        &b.app_root,
        current_build,
        current_commit,
    ) {
        let armed_build = boot_sentinel(&staging)
            .read_state()
            .map_or(0, |(build, _)| build);
        // THE JUST-SWAPPED SHAPE (2026-09-14, audit BA-7): a sibling cold launch
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
            && crate::verify::bundle_build_number(&b.app_root).ok() == Some(armed_build)
        {
            crate::log(&format!(
                "boot apply: build {armed_build} is already installed and mid-trial; this \
                 launch is the previous build {current_build} and leaves the trial to its owner"
            ));
            return ApplyOutcome::NoUpdate;
        }
        // A sentinel armed for a build that is not the running one can never be
        // cleared by the same-build lanes, so an unrecoverable one blocks EVERY
        // future apply forever. Budget it — with its own counter, never the
        // trial's. The SAME-build case that another install owns is the same
        // wedge for THIS process (`check_boot_health` rightly does not count us,
        // and the owner may never run again), so it takes the same budget.
        if armed_build != 0
            && (armed_build != current_build || !trial_owned_by(&staging, &b.app_root))
        {
            escape_wedged_foreign_trial(&staging, current_build, armed_build);
        }
        return ApplyOutcome::Deferred(format!(
            "update trial for build {armed_build} is still unconfirmed"
        ));
    }
    // The mismatched-sentinel lane is behind us for this launch, so no budget is
    // outstanding; never let one accumulate across unrelated trials.
    let _ = foreign_trial_counter(&staging).confirm();
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
    // 4c. NO ROSTER-GENERATION GATE HERE, and the reason is worth recording.
    //
    //     A stage is an authorization made at stage time, and a revocation lands
    //     afterwards, so "retract an already-staged build" is a real gap. But the
    //     apply lane cannot answer it: revocation is a LIST inside the roster
    //     document, and all this lane holds is `Floor::roster_seq`, a number.
    //
    //     Comparing the marker's recorded generation against that floor LOOKS like
    //     the missing check and is not. `Floor::roster_seq` ratchets to the
    //     generation of the roster ASSET the client observed, while the marker
    //     records the generation the MANIFEST was attributed under — and those two
    //     legitimately differ. A machine joining the roster attaches the new pair to
    //     releases that already shipped, so `manifest_seq < floor` is the ordinary
    //     POST-JOIN STEADY STATE, which `authorize_by_roster` deliberately admits
    //     ("a newer roster paired with an older release"). Gating on it would retire
    //     a perfectly good stage on every launch after any join: the update never
    //     applies and the container is downloaded again forever — the exact
    //     never-updates shape this file already carries two other scars from.
    //
    //     The check lane is where the answer lives, because that is where the roster
    //     document (and its `revoked` list) is in hand. `Ready` records `machine_id`
    //     and `roster_seq` so that retraction can be written there without a second
    //     marker migration; it is deliberately NOT enforced from here.
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

    // 7-pre. ON A CROSS-VOLUME INSTALL ONLY, ask the cheap-to-answer question
    // first (2026-09-14, audit SV-5): can the installed bundle be the rollback
    // source at all? The authoritative answer is re-asked below, AFTER the
    // candidate is prepared (TOCTOU hygiene — the swap must trust nothing older
    // than the exchange). But preparing a candidate across volumes is a ≤120 s
    // `ditto` plus a five-helper re-verify, and an install that cannot verify
    // (a hand-installed or ad-hoc bundle) did all of that on EVERY cold launch
    // only to undo it. On the common same-volume path the preparation is two
    // renames and the early pass would put five helper spawns back on every
    // successful launch (the law recorded at `prepare_fixed_swap_candidate`), so
    // it is gated on the volume.
    if !same_volume(&staging.staged_app, &b.app_root)
        && let Err(error) = verified_bundle_identity(&b.app_root)
    {
        return ApplyOutcome::Deferred(format!(
            "current installed rollback source is not verified: {}",
            rollback_source_refusal(&b.app_root, &error)
        ));
    }
    // 7. Prepare/verify NEW at the fixed destination-volume recovery path BEFORE
    // the point of no return. One atomic exchange then puts NEW at installed and
    // OLD directly at that fixed path; every process-crash cut is discoverable.
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
            // The same refusal the seamless lane mints, remedy included: the
            // cold launch is the "fallback" the stager promises, and on this
            // bundle it refuses for the same reason.
            return ApplyOutcome::Deferred(format!(
                "current installed rollback source is not verified: {}",
                rollback_source_refusal(&b.app_root, &error)
            ));
        }
    };
    // Preserve only a receipt that is authority for the exact sealed OLD bundle
    // just rechecked above. A well-formed but stale receipt is unsigned local
    // state and must not be resurrected after an inverse swap.
    let previous_receipt = previous_receipt_for_sealed_old(&staging, old_build, &old_commit);

    // 7b. THE CANDIDATE MUST BE ABLE TO START. Every check so far is cryptographic;
    // none of them proves the binary LOADS. The crash-loop sentinel cannot cover that
    // case — it runs inside the new build's own `main` — so a bundle that dyld or
    // Gatekeeper rejects before `main` would be swapped in and never reverted, leaving
    // a bricked app beside the verified predecessor it should have rolled back to.
    // Start it once, here, while OLD is still installed and backing out is free.
    if let Err(error) = crate::verify::probe_bundle_starts(&prepared.fixed, ready.build_number) {
        recover_prepared_candidate(&prepared, &staging);
        crate::manifest::FailedMark::record_stage_failure(
            &staging.failed(),
            ready.build_number,
            &ready.dmg_sha256,
            unix_now_secs(),
        );
        return ApplyOutcome::Deferred(format!("staged candidate cannot start: {error}"));
    }

    // 8. Arm exact crash-loop authority only after fixed NEW is fully verified and
    // immediately before the atomic swap. A crash after this boundary has one
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
    // Booked in the failure ledger too (2026-09-14): a swap that was rolled
    // back because the new binary would not exec is an apply that failed, and
    // the wrapper deliberately leaves `ReExecFailed` out of the refusal slot
    // ("a re-exec FAILURE belongs in the failure ledger") — which nothing
    // wrote until now.
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

/// On every launch: if the boot sentinel is armed for `current_build`, count this
/// launch and, if the build has failed to confirm across [`MAX_BOOT_ATTEMPTS`]
/// launches (a crash loop), revert to the retained OLD bundle and re-exec it.
/// Returns `Some` only in the revert path (which normally re-exec's away and does
/// not return); `None` to continue booting the current build.
/// Which boot-health lane a counted launch takes ([`boot_health_lane`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BootHealthLane {
    /// Count the launch and prove the trial's rollback now (`ensure_current_trial_receipt`:
    /// the installed bundle's identity, the retained predecessor, the receipt).
    FullProof,
    /// Count the launch only; the rollback proof runs at the health checkpoint.
    CountOnly,
}

/// THE FIRST COUNTED LAUNCH OF A HANDOFF TARGET IS COUNT-ONLY (2026-09-19). The
/// image that swapped the bundle proved both identities and recorded the receipt
/// before it stamped the re-exec, moments ago; re-proving them here cost the
/// 0.87.0→0.88.0 apply 430 ms of codesign inside the parked window ("update apply
/// (boot): NotApplicable in 430 ms — this ran with the outgoing process's readers
/// still parked"). So a launch that is the live parent's authorized handoff target
/// (`handoff_target_is_this_build`) and the trial's first counted launch skips the
/// proof: the launch is still counted (a crash before the checkpoint still climbs
/// toward the revert), and the SAME proof runs at `confirm_boot_health` — after
/// Commit, off the park — which is where a healthy trial is disarmed anyway. A
/// second counted launch (a relaunch after a crash, a sibling cold launch inside
/// the window) proves in full as before: degraded, never unsafe.
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

fn check_boot_health(
    staging: &Staging,
    current_build: u64,
    current_commit: Option<&str>,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target: bool,
) -> Option<ApplyOutcome> {
    check_boot_health_with_lock_wait(
        staging,
        current_build,
        current_commit,
        handoff_fds,
        handoff_env,
        handoff_target,
        APPLY_LOCK_WAIT,
    )
}

#[allow(clippy::too_many_arguments)]
fn check_boot_health_with_lock_wait(
    staging: &Staging,
    current_build: u64,
    current_commit: Option<&str>,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target: bool,
    lock_wait: std::time::Duration,
) -> Option<ApplyOutcome> {
    let sentinel = boot_sentinel(staging);
    // Cheap non-mutating early-out: nothing armed for us. A sentinel for another
    // build belongs to that build and is never "stale" authority for this process.
    if !matches!(sentinel.read_state(), Some((b, _)) if b == current_build) {
        return None;
    }
    let apply_lock = match FileLock::acquire_within(&staging.apply_lock, lock_wait) {
        Ok(lock) => lock,
        Err(error) => return Some(ApplyOutcome::Deferred(format!("health lock: {error}"))),
    };
    // Revalidate after acquiring the transaction lock. Another process may have
    // armed a newer build between the cheap peek and this point.
    if !matches!(sentinel.read_state(), Some((b, _)) if b == current_build) {
        return None;
    }
    // Finalizing a swap a previous run already made — installs nothing — so the dev
    // mark must not strand an armed trial. See `bundle::resolve_layout`.
    let b = bundle::resolve_layout()?;
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
    // COUNT THE LAUNCH FIRST. This used to run after the recovery proof below, so a
    // trial whose proof failed every boot never advanced its attempt counter, never
    // reached MAX_BOOT_ATTEMPTS, and therefore never reverted OR confirmed: the
    // sentinel stayed armed forever and this function returned `Deferred` on every
    // subsequent launch, which makes `apply_staged_if_ready` return early forever.
    // The machine kept running (and kept downloading and staging updates) but could
    // never apply another one, with nothing in the UI to say why. The attempt count
    // must measure LAUNCHES OBSERVED, which is a fact about this boot, not a
    // conclusion that depends on a proof that may itself be what is broken.
    //
    // A BURST IS ONE LAUNCH (2026-09-14): a second process of the trial build
    // starting within `BOOT_LAUNCH_BURST_WINDOW` of the last counted one is the
    // same launch event, not a relaunch after a crash — see `launch_is_burst`.
    let attempts_now = sentinel.read_state().map_or(0, |(_, attempts)| attempts);
    let sentinel_age = mtime(&staging.root.join("boot.sentinel")).and_then(|t| t.elapsed().ok());
    if launch_is_burst(attempts_now, sentinel_age) {
        crate::log(&format!(
            "boot sentinel for build {current_build}: a launch {} ms after the last counted \
             one is the same burst and is not counted (attempt {attempts_now} stands)",
            sentinel_age.map_or(0, |age| age.as_millis())
        ));
    } else if let Err(error) = sentinel.observe_launch(current_build) {
        return Some(ApplyOutcome::Deferred(format!(
            "trial launch observation: {error}"
        )));
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
                // BUDGET EXHAUSTED. Reverting is the right answer whenever it is
                // AVAILABLE — a verified predecessor sitting at the fixed rollback
                // path — because this trial can no longer be confirmed and the build
                // is, by the sentinel's own count, failing to reach its checkpoint.
                // The proof that just failed is a CONJUNCTION (rollback ∧ sealed
                // identity ∧ trial marker ∧ receipt), and only its first term speaks
                // to whether a revert can be performed; treating any failure of the
                // later terms as "the rollback is unprovable" left a crash-looping
                // build installed beside a perfectly good predecessor, which is the
                // outcome the whole sentinel exists to prevent (2026-08-19 round-4
                // skeptics). Re-derive that first term alone and revert on it.
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
                // No verified predecessor: reverting is genuinely unavailable.
                // Staying armed is the strictly worse option: this build demonstrably
                // BOOTS — we are executing its code, this many times in a row — and
                // remaining armed only guarantees that no future update can ever
                // apply. Disarm, keep running, and make the reason loud and durable
                // instead of silently bricking the updater.
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

/// Captured descriptors and the authority environment restored together when
/// rollback re-execs the predecessor during an inherited handoff.
struct RollbackHandoff<'a> {
    fds: &'a [i32],
    env: &'a [(std::ffi::OsString, std::ffi::OsString)],
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
        // QUARANTINE, not `record`. The old call wrote the memo with `retry_after = 0`
        // meaning "forever" — but `FailedMark::suppresses`, the only reader, treats a
        // zero deadline as "already elapsed". The poison was written and then ignored,
        // so the next background check (75 s later) re-downloaded and re-applied the
        // build that had just crash-looped, and the machine went straight back around
        // the crash/revert loop this code exists to break.
        crate::manifest::FailedMark::record_quarantine(
            &staging.failed(),
            t.build_number,
            &t.sha256,
        );
    }
    // OLD is restored at the install; failed NEW sits at rb. Disarm succeeded, so
    // transaction cleanup cannot leave an armed trial without recovery metadata.
    let _ = std::fs::remove_dir_all(&rb);
    crate::manifest::InstalledReceipt::clear(&staging.installed_receipt());
    crate::manifest::FailedMark::clear(&staging.trial());
    staging.retire_published(); // the staged build is bad — never re-apply it
    // BOOKED IN THE FAILURE LEDGER (2026-09-14): the one outcome that means the
    // update landed and was taken back used to leave `health.toml` at
    // `apply_failures = 0`, so `update status` said failing=0, the pull-down
    // never showed the failing row, the persistent notice could not fire, and
    // the only trace — one status line — was overwritten by the next check.
    // The user experienced an app that silently went back a version.
    // THE FLOOR, SAID OUT LOUD (2026-09-14, audit BA-4). The activation lane
    // refuses an installed bundle below the operator floor ("a yanked build found
    // under our own path is still a yanked build"); the revert restores whatever
    // verified predecessor the fixed path holds, and a crash loop is strictly
    // worse than a yank, so it must. But the two lanes must not disagree in
    // silence: the ledger, the status line and the notice name the breach and the
    // remedy, so the machine does not sit on a known-bad build with a status
    // line that reads like a repair.
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
    // Re-exec the restored OLD binary as a FRESH boot: no re-exec env, no sentinel
    // (already cleared), and nothing staged, so it comes up clean on the old build.
    //
    // THE HANDOFF AUTHORITY RIDES ALONG (2026-09-14, audit BA-6). The handoff
    // descriptors were already kept open across this exec, but the authority
    // variables the GUI's prearm consumed were not restored, so a reverted
    // overlap candidate came up with the fds in its environment and no parent
    // identity — `rejected malformed inherited handoff`, a stderr-only line — and
    // the parked parent read EOF with nothing in the log joining the two facts.
    // With the pairs restored the OLD image re-validates the inherited handoff
    // and refuses it on target identity through its loud, logged path; the
    // parent's verdict is the same (the candidate did not commit) and the record
    // says why.
    let mut reexec = Command::new(&b.exe);
    for (key, value) in handoff_env {
        reexec.env(key, value);
    }
    if !handoff_fds.is_empty() {
        crate::warn(&format!(
            "the crash-loop revert of build {current_build} happened inside a seamless \
             handoff; the restored build {restored_build} will refuse the handoff and the \
             parked outgoing process keeps its sessions"
        ));
    }
    reexec
        .args(reexec_forwarded_args(std::env::args_os().skip(1)))
        // NO --window here: the restored ROLLBACK build may predate the
        // one-binary router entirely (its gui parser exits 2 on the unknown
        // flag — a dead relaunch is worse than a mode-imperfect one). A
        // pre-collapse rollback IS the window binary and needs no flag; a
        // one-binary rollback launched flag-less from a TTY degrades to a
        // session but stays alive. That is also why the LEADING `--window`
        // pins are stripped from the forwarded args: this process's argv
        // carries one per swap it has ridden, and forwarding any of them to a
        // pre-collapse rollback is the exit-2 dead relaunch this comment
        // exists to prevent.
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
    // THIS is where an apply is known to have SUCCEEDED, and therefore the only
    // honest place to clear the apply-failure streak.
    //
    // The GUI used to call `record_apply_success` the moment it SUBMITTED an apply
    // (`UpdateOutcome::Accepted`), which cleared the streak before the swap had even
    // been attempted — so `apply_failures` could never reach `PERSISTENT_AFTER` and
    // the self-healing ledger was blind to a lane that failed every single time. That
    // call is gone; without this one the streak would instead be uncleanable, which is
    // the opposite error. A new build swapped in, re-execed, and confirmed its own boot
    // health is the whole apply lane working end to end, which is exactly the claim
    // `record_apply_success` makes.
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

/// See [`crate::forgive_trial_launch`]. Under the apply lock so it cannot interleave
/// with a `check_boot_health` observation or a `confirm_boot_health` disarm.
/// How many launches the boot sentinel has counted for `build` (0 when it is armed
/// for another build, or not armed at all). The parent snapshots this before it
/// launches a candidate so it can tell whether that candidate actually observed a
/// launch before it was killed — see [`crate::forgive_trial_launch_if_advanced`].
///
/// A LOCK-FREE READ, unlike the forgiveness it feeds: it is a hint compared against
/// a later read, and both a stale and a fresh answer are handled by the "moved and
/// non-zero" rule.
#[must_use]
pub(crate) fn trial_launch_count(build: u64) -> u32 {
    Staging::resolve()
        .and_then(|staging| boot_sentinel(&staging).read_state())
        .filter(|(armed, _)| *armed == build)
        .map_or(0, |(_, attempts)| attempts)
}

pub(crate) fn forgive_trial_launch(target_build: u64) {
    let Some(staging) = Staging::resolve() else {
        return;
    };
    let sentinel = boot_sentinel(&staging);
    if !matches!(sentinel.read_state(), Some((b, _)) if b == target_build) {
        return;
    }
    // Only a launch this install COUNTED may be given back: `check_boot_health`
    // does not count launches against a trial another install owns, so forgiving
    // from here would decrement the owner's count for a launch it never saw.
    if let Some(b) = bundle::resolve_layout()
        && !trial_owned_by(&staging, &b.app_root)
    {
        return;
    }
    let Ok(_apply_lock) = FileLock::acquire(&staging.apply_lock) else {
        crate::warn(&format!(
            "could not take the apply lock to forgive a killed trial launch of build \
             {target_build}; the launch stays counted"
        ));
        return;
    };
    match sentinel.forgive_launch(target_build) {
        Ok(remaining) => crate::log(&format!(
            "trial launch of build {target_build} forgiven — the outgoing process ended \
             that candidate itself ({remaining} unconfirmed launch(es) still counted)"
        )),
        Err(error) => crate::warn(&format!(
            "could not forgive a killed trial launch of build {target_build}: {error}"
        )),
    }
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
    // Health confirmation of an already-applied swap — dev-mark-independent, same
    // reasoning as the trial path above.
    let Some(installed) = bundle::resolve_layout() else {
        return false;
    };
    // A trial another install of this build owns is not ours to confirm (nor could
    // we: its rollback lives beside THAT bundle). Answer "nothing to do" instead of
    // failing the proof forever from a sibling process — see `trial_owned_by`.
    if !trial_owned_by(&staging, &installed.app_root) {
        return true;
    }
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

// Boot-apply audit tests (2026-09-14): red until their findings are fixed; see the
// file header. A child module so they reach this file's private entry points.
#[cfg(test)]
#[path = "install_boot_tests.rs"]
mod boot_tests;

#[cfg(test)]
mod tests {

    /// THE ChildDied FIELD FAILURE (2026-08-25), pinned at the seam that now
    /// answers it. Five consecutive in-session updates on one desk ended
    /// `handoff proof ended ChildDied` with `failing_applies=5 persistent=true`,
    /// and nothing on the machine named a cause. The chain was:
    /// `/Applications/aterm.app` had been replaced in place by a hand-built,
    /// ad-hoc-signed bundle → the successor's boot apply refused to swap
    /// ("current installed rollback source is not verified") → the successor
    /// stayed the OLD build → it refused the authorized target identity and
    /// dropped the readiness pipe → the parent read EOF and booked the crash
    /// outcome. Five frozen terminals for a verdict that was decidable before
    /// the first reader parked.
    ///
    /// `None` is `bundle::resolve()` finding nothing the updater may replace,
    /// which is `ApplyOutcome::NotApplicable` at swap time: a successor whose
    /// swap is a no-op can only ever refuse the target, so this must refuse now.
    #[test]
    fn a_process_with_no_replaceable_bundle_refuses_before_anything_parks() {
        let error = super::preverify_installed_rollback_source(None, 100, Some("abcdef0"))
            .expect_err("no replaceable bundle cannot host a swap");
        assert!(
            error.contains("does not run from an installed bundle"),
            "the refusal names the missing bundle rather than blaming the candidate: {error}"
        );
    }

    /// A bundle path that is not a verifiable bundle refuses, and the refusal
    /// NAMES THE PATH — the one fact an operator needs to act on, and the one
    /// the `ChildDied` verdict could never carry.
    #[test]
    fn an_unverifiable_installed_bundle_refuses_and_names_itself() {
        let root = std::env::temp_dir().join(format!(
            "aterm-rollback-source-{}-{}",
            std::process::id(),
            super::unix_now_secs()
        ));
        let error = super::preverify_installed_rollback_source(Some(&root), 100, Some("abcdef0"))
            .expect_err("a path that is not a bundle cannot be a rollback source");
        assert!(
            error.contains(&root.display().to_string()),
            "the refusal names the bundle it examined: {error}"
        );
        assert!(
            error.contains("rollback source"),
            "the refusal names WHAT the bundle failed to be: {error}"
        );
    }

    /// The identity half of the same gate, pinned pure. The owner's desk failed
    /// this one too — the bundle on disk was sealed 1787634715 while the process
    /// running out of it was 1787614521, because the `.app` had been replaced
    /// underneath a live instance. A rollback source that is not the running
    /// image is not a rollback source.
    #[test]
    fn only_the_running_image_can_be_the_rollback_source() {
        assert!(
            super::identity_matches_running(1787614521, "abcdef0123", 1787614521, Some("abcdef0")),
            "the sealed identity of the image we are executing is the rollback source"
        );
        assert!(
            !super::identity_matches_running(1787634715, "abcdef0123", 1787614521, Some("abcdef0")),
            "a bundle swapped under a live instance is a different build, not our rollback source"
        );
        assert!(
            !super::identity_matches_running(1787614521, "abcdef0123", 1787614521, Some("9999999")),
            "same build number, different provenance, is still not our image"
        );
    }

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
            1785910394,
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
                std::ffi::OsStr::new("ATERM_UPDATED_FROM"),
                Some(std::ffi::OsStr::new("1785910394")),
            )),
            "the cold-launch apply names the build it came from, so the successor shows \
             the post-update notice (2026-09-14)"
        );
        assert!(
            envs.contains(&(
                std::ffi::OsStr::new("ATERM_HANDOFF_PARENT_PID"),
                Some(std::ffi::OsStr::new("4242")),
            )),
            "handoff authority restored onto the exec image"
        );
    }

    /// THE `--window` accumulation regression (2026-08-24): the re-exec
    /// forwards this process's own argv, which already carries the mode pin
    /// the PREVIOUS swap prepended — forwarded verbatim, a long-lived install
    /// grows one `--window` per update it rides (observed at six). The strip
    /// makes the forwarded argv a fixed point while never reaching past the
    /// first non-pin token, so `-e`/`--command` payloads are untouchable.
    #[test]
    fn reexec_forwarded_args_is_a_fixed_point_and_never_reads_past_the_lead() {
        let args = |list: &[&str]| -> Vec<std::ffi::OsString> {
            list.iter().map(std::ffi::OsString::from).collect()
        };
        // The field case: six accumulated pins collapse to none forwarded
        // (boot_reexec_command re-pins exactly one).
        assert_eq!(
            super::reexec_forwarded_args(args(&["--window"; 6]).into_iter()),
            args(&[]),
        );
        // Fixed point: a once-stripped argv re-strips to itself.
        let once = super::reexec_forwarded_args(
            args(&["--window", "--font-px", "30", "--window"]).into_iter(),
        );
        assert_eq!(once, args(&["--font-px", "30", "--window"]));
        assert_eq!(
            super::reexec_forwarded_args(once.clone().into_iter()),
            once,
            "stripping must converge after one pass"
        );
        // A `--window` past the first other token is payload-adjacent and
        // survives verbatim — including inside an -e command.
        assert_eq!(
            super::reexec_forwarded_args(args(&["-e", "sh", "-c", "--window"]).into_iter()),
            args(&["-e", "sh", "-c", "--window"]),
        );
        // Empty argv (Finder/launchd launch) stays empty.
        assert_eq!(
            super::reexec_forwarded_args(args(&[]).into_iter()),
            args(&[])
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
        let s = Staging::scratch("rr");
        std::fs::create_dir_all(s.staged_dir()).unwrap();
        let root = s.root.clone();
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
            machine_id: None,
            roster_seq: None,
        };
        std::fs::write(&s.ready, r.to_toml().unwrap()).unwrap();
    }

    /// A `ready.toml` whose staged bundle is GONE must clear itself. This recovery runs
    /// before the `is_publishable` retirement, so returning Err made that retirement
    /// unreachable and turned every launch into a permanent `Deferred` naming a rollback
    /// path the operator has no reason to care about.
    #[test]
    fn a_ready_marker_whose_staged_bundle_vanished_retires_instead_of_deferring_forever() {
        let (s, root) = temp_staging();
        write_ready(&s, 7);
        let installed = root.join("Applications").join("aterm.app");
        std::fs::create_dir_all(&installed).unwrap();
        let ready = Ready::read(&s.ready).unwrap();
        assert!(
            !s.staged_app.exists(),
            "the staged bundle is the missing half"
        );
        assert!(
            !rollback_path(&installed).exists(),
            "and nothing at the fixed path could ever satisfy the marker"
        );

        assert!(
            recover_orphaned_prepared_candidate(&s, &installed, 6, &ready).is_ok(),
            "a dangling marker is a self-healing condition, not a permanent refusal"
        );
        assert!(!s.ready.exists(), "the dangling marker cleared itself");
        assert!(matches!(read_ready(&s, 6), ReadyState::Absent));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The new attribution fields must be OPTIONAL on the wire: a `ready.toml` written
    /// before they existed has to keep parsing, or an upgrade would strand every
    /// already-staged build behind a `Corrupt` marker.
    #[test]
    fn a_ready_marker_without_attribution_still_parses() {
        let legacy = r#"
build_number = 42
version = "0.42.0"
dmg_sha256 = "abcd"
team_id = "T"
staged_at = "2026-08-17T00:00:00Z"
"#;
        let ready: Ready = aterm_toml::from_str(legacy).expect("pre-attribution marker parses");
        assert_eq!(ready.build_number, 42);
        assert_eq!(ready.machine_id, None);
        assert_eq!(ready.roster_seq, None);
    }

    /// AN OVERSIZED CHANGELOG MAKES THE MARKER INVISIBLE, NOT LARGE: every reader goes
    /// through `read_ledger_text`, which returns `None` above `MAX_LEDGER_BYTES`. The
    /// clamp must count CHARACTERS — a byte-offset cut inside a multi-byte sequence
    /// panics, and release notes are full of em dashes.
    #[test]
    fn an_oversized_manifest_changelog_is_clamped_by_characters_not_bytes() {
        assert_eq!(clamp_ready_changelog(None), None);
        assert_eq!(
            clamp_ready_changelog(Some("a short release note")).as_deref(),
            Some("a short release note")
        );

        // Three bytes per char, so any byte-indexed cut would land mid-sequence.
        let wide = "—".repeat(MAX_READY_CHANGELOG_CHARS * 4);
        let clamped = clamp_ready_changelog(Some(&wide)).unwrap();
        assert!(clamped.starts_with('—'), "the head survives verbatim");
        assert!(clamped.ends_with(READY_CHANGELOG_TRUNCATED));
        assert_eq!(
            clamped.chars().count(),
            MAX_READY_CHANGELOG_CHARS + READY_CHANGELOG_TRUNCATED.chars().count()
        );

        // The defect's real scale: the whole ~382 KiB CHANGELOG.md in one manifest
        // field. Clamped, the committed marker stays inside the cap every reader
        // enforces, so the stage stays visible instead of reading as absent/corrupt.
        let whole_changelog_md = "x".repeat(382 * 1024);
        let ready = Ready {
            build_number: 9,
            version: "0.9.0".into(),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            dmg_sha256: "ab".repeat(32),
            team_id: "T".into(),
            staged_at: String::new(),
            changelog: clamp_ready_changelog(Some(&whole_changelog_md)),
            machine_id: None,
            roster_seq: None,
        };
        let marker = ready.to_toml().unwrap();
        assert!(
            u64::try_from(marker.len()).unwrap() <= crate::MAX_LEDGER_BYTES,
            "clamped marker is {} bytes",
            marker.len()
        );
        assert!(
            crate::read_ledger_text(&{
                let (s, _root) = temp_staging();
                std::fs::write(&s.ready, &marker).unwrap();
                s.ready
            })
            .is_some()
        );
    }

    /// A marker that would read as ABSENT must never be committed — and refusing it
    /// must not cost the readable generation that is already staged, which is why the
    /// size guard runs before the lock and before the old marker/bundle are removed.
    #[test]
    fn publish_refuses_a_marker_that_would_exceed_the_ledger_cap() {
        let (s, root) = temp_staging();
        write_ready(&s, 5);
        make_app(&s.staged_app, "OLD-STAGE");
        let incoming = s.staged_dir().join("aterm.app.incoming");
        make_app(&incoming, "NEW-STAGE");

        let mut ready = Ready::read(&s.ready).unwrap();
        ready.build_number = 6;
        ready.changelog = Some("y".repeat(usize::try_from(crate::MAX_LEDGER_BYTES).unwrap() + 1));
        let error = publish_verified_stage(&s, &incoming, &ready).unwrap_err();
        assert!(error.contains("ready marker"), "{error}");

        assert!(
            matches!(
                read_ready(&s, 4),
                ReadyState::Newer(Ready {
                    build_number: 5,
                    ..
                })
            ),
            "a rejected publish leaves the previous readable generation staged"
        );
        assert_eq!(read_id(&s.staged_app), "OLD-STAGE");
        assert_eq!(read_id(&incoming), "NEW-STAGE");
        let _ = std::fs::remove_dir_all(root);
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
        let absent = preverify_staged_handoff_candidate_at(&s, None, 10, None, None, None);
        assert!(
            absent.clone().unwrap_err().contains("no verified update"),
            "{absent:?}"
        );

        // Staged but not strictly newer than the running build.
        write_ready(&s, 20);
        let stale = preverify_staged_handoff_candidate_at(&s, None, 20, None, None, None);
        assert!(
            stale.clone().unwrap_err().contains("strictly newer"),
            "{stale:?}"
        );

        // Staged build is not the artifact the updater reducer authorized.
        let wrong = preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(21), None);
        assert!(
            wrong
                .clone()
                .unwrap_err()
                .contains("not the authorized build"),
            "{wrong:?}"
        );

        // Right identity on the marker, but no verifiable bundle exists at the
        // staged path: the sealed-identity gate must fail closed.
        let unverifiable =
            preverify_staged_handoff_candidate_at(&s, None, 10, None, Some(20), None);
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
        let value: aterm_toml::Value =
            aterm_toml::from_str(&text).expect("activation status parses");
        assert_eq!(
            value
                .get("current_build")
                .and_then(aterm_toml::Value::as_integer),
            Some(54)
        );
        assert_eq!(
            value.get("outcome").and_then(aterm_toml::Value::as_str),
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
            machine_id: None,
            roster_seq: None,
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
                machine_id: None,
                roster_seq: None,
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

        assert!(prepare_trial(&s, &ready, Path::new("/Applications/aterm.app")).is_err());
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

    /// The two mismatched-sentinel shapes are disjoint, and neither admits a
    /// process that is not running the installed build: an overlapping process
    /// from another build has no authority to disarm anything.
    #[test]
    fn dead_authority_and_abandoned_preswap_shapes_are_disjoint() {
        // Out-of-band replacement: armed BELOW the build that is installed and
        // running. Nothing in the tree could ever clear this one before.
        assert!(dead_authority_trial(1001, 1001, 1000));
        assert!(!abandoned_preswap_trial(1001, 1001, 1000));
        // The pre-swap crash cut the exact recovery was written for: armed ABOVE.
        assert!(abandoned_preswap_trial(1000, 1000, 1001));
        assert!(!dead_authority_trial(1000, 1000, 1001));
        // Neither fires for a process that is not the installed build.
        assert!(!dead_authority_trial(999, 1001, 1000));
        assert!(!abandoned_preswap_trial(999, 1000, 1001));
    }

    /// An out-of-band install (install.sh, a DMG drag) landing inside an armed
    /// trial window leaves a sentinel for a build that is no longer installed.
    /// It can never be confirmed and must never be reverted, so the terminal
    /// transition is to disarm it — otherwise every future apply is refused
    /// forever while the stager keeps publishing "ready to apply".
    #[test]
    fn a_trial_armed_below_the_running_build_is_retired_not_kept_forever() {
        let (s, root) = temp_staging();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let sentinel = boot_sentinel(&s);
        sentinel.arm(1000).unwrap();
        crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));
        // A receipt for the DEAD trial's build — not proof for what is installed.
        crate::manifest::InstalledReceipt::record(
            &s.installed_receipt(),
            1000,
            commit,
            &"ab".repeat(32),
        )
        .unwrap();
        let installed = root.join("Applications").join("aterm.app");
        make_app(&installed, "HAND-INSTALLED-1001");
        let rollback = rollback_path(&installed);
        make_app(&rollback, "OLDER-THAN-RUNNING");

        assert!(disarm_dead_authority_trial(
            &s, &installed, 1000, 1001, commit, 1001
        ));
        assert_eq!(sentinel.read_state(), None, "the wedge is cleared");
        assert!(!s.trial().exists(), "the dead trial's identity goes too");
        assert!(
            !rollback.exists(),
            "its rollback source is older than the running build and has no sentinel behind it"
        );
        assert!(
            !s.installed_receipt().exists(),
            "a receipt for the dead build is not proof for the installed one"
        );
        assert!(
            std::fs::read_to_string(&s.status)
                .unwrap()
                .contains("updates re-enabled"),
            "the repair is durable and explains itself"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A trial belongs to the install it swapped. A same-build process launched
    /// from ANOTHER bundle (a dev machine's `dist/aterm.app` beside
    /// `/Applications/aterm.app`, a duplicate copy) is not its owner: it must not
    /// count launches against it, confirm it, or disarm it as dead authority.
    /// Legacy markers without a recorded root stay owned by whoever asks.
    #[test]
    fn a_trial_is_owned_by_the_install_it_swapped_and_no_sibling_bundle() {
        let (s, root) = temp_staging();
        let owner = root.join("Applications").join("aterm.app");
        let sibling = root.join("dist").join("aterm.app");
        make_app(&owner, "OWNER");
        make_app(&sibling, "SIBLING");
        write_ready(&s, 6);
        let ready = Ready::read(&s.ready).unwrap();
        let sentinel = prepare_trial(&s, &ready, &owner).unwrap();
        assert!(trial_owned_by(&s, &owner));
        assert!(!trial_owned_by(&s, &sibling));
        // A sibling launch of the trialed build does not count against it…
        // (`check_boot_health` early-outs before `observe_launch` for a non-owner —
        // exercised through the pure predicate here, and through
        // `recover_abandoned_preswap_trial_if_exact` below).
        assert_eq!(sentinel.read_state(), Some((6, 0)));
        // …and a sibling that is NEWER than the armed build may not retire it as
        // dead authority either: the pure gate is the ownership test.
        assert!(dead_authority_trial(7, 7, 6) && !trial_owned_by(&s, &sibling));
        // The owner install GONE (moved/deleted mid-trial): whoever runs the build
        // now inherits the trial, so it can still be counted, confirmed or escaped.
        std::fs::remove_dir_all(&owner).unwrap();
        assert!(trial_owned_by(&s, &sibling), "a ghost owns nothing");
        // A legacy marker (no recorded root) is owned by whoever asks.
        crate::manifest::FailedMark::record(&s.trial(), 6, &"ab".repeat(32));
        assert!(trial_owned_by(&s, &sibling) && trial_owned_by(&s, &owner));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The unrecoverable mismatched-sentinel shapes get a budgeted escape of
    /// their own. The budget must be counted in a SEPARATE file: incrementing the
    /// boot sentinel would advance the trialed build's attempt count from
    /// launches of a different build and could revert a build that never crashed.
    #[test]
    fn an_unrecoverable_foreign_trial_is_disarmed_on_a_budget_not_on_sight() {
        let (s, root) = temp_staging();
        let sentinel = boot_sentinel(&s);
        sentinel.arm(1000).unwrap();
        crate::manifest::FailedMark::record(&s.trial(), 1000, &"ab".repeat(32));

        for launch in 1..MAX_BOOT_ATTEMPTS {
            assert!(
                !escape_wedged_foreign_trial(&s, 1001, 1000),
                "launch {launch} is inside the budget"
            );
            assert_eq!(
                sentinel.read_state(),
                Some((1000, 0)),
                "the trialed build's OWN attempt count is never touched"
            );
        }

        assert!(
            escape_wedged_foreign_trial(&s, 1001, 1000),
            "the budget is exhausted"
        );
        assert_eq!(sentinel.read_state(), None, "the wedge is cleared");
        assert!(!s.trial().exists());
        assert!(
            !s.root.join("foreign-trial").exists(),
            "the counter is retired with the sentinel it was counting"
        );
        assert!(
            std::fs::read_to_string(&s.status)
                .unwrap()
                .contains("updates re-enabled")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A trial OWNED BY ANOTHER INSTALL THAT STILL EXISTS is not disarmed by a
    /// sibling's exhausted budget unless that owner has been idle for a day; an owner
    /// that is GONE cedes at the budget as before.
    #[test]
    fn a_present_owners_trial_survives_a_siblings_budget_until_the_owner_is_long_idle() {
        let (s, root) = temp_staging();
        let owner = root.join("Applications").join("aterm.app");
        make_app(&owner, "OWNER");
        let sentinel = boot_sentinel(&s);
        sentinel.arm(1000).unwrap();
        crate::manifest::FailedMark::record_required(
            &s.trial(),
            1000,
            &"ab".repeat(32),
            Some(&owner),
        )
        .unwrap();
        for _ in 0..MAX_BOOT_ATTEMPTS + 2 {
            assert!(
                !escape_wedged_foreign_trial(&s, 1001, 1000),
                "a present, recently-touched owner keeps its trial past the budget"
            );
        }
        assert_eq!(
            sentinel.read_state(),
            Some((1000, 0)),
            "still armed, still untouched"
        );
        // The owner install GONE: the next exhausted budget disarms (ghost cedes).
        std::fs::remove_dir_all(&owner).unwrap();
        let mut disarmed = false;
        for _ in 0..MAX_BOOT_ATTEMPTS + 1 {
            disarmed |= escape_wedged_foreign_trial(&s, 1001, 1000);
        }
        assert!(disarmed, "a ghost owner cedes at the budget");
        assert_eq!(sentinel.read_state(), None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A LIVE TRIAL BELONGING TO ANOTHER INSTALLATION IS NEVER DISARMED.
    ///
    /// The staging root is per USER, not per install, so a locally built .app beside
    /// the released one shares this sentinel. If launches of THIS copy could exhaust
    /// the budget while the other copy is still trialing, the escape would strip the
    /// crash-loop protection off the very build that is crash-looping: that build's
    /// own `check_boot_health` would then find no sentinel and never revert.
    ///
    /// The owner advancing its own attempt count is what proves it alive, so a budget
    /// that spans such an advance must restart rather than complete.
    #[test]
    fn a_foreign_trial_whose_owner_is_still_launching_is_never_disarmed() {
        let (s, root) = temp_staging();
        let sentinel = boot_sentinel(&s);
        sentinel.arm(1000).unwrap();

        // Run the budget to one launch short of the escape, over and over, with the
        // trial's OWNER observing a launch each time — exactly what a crash-looping
        // second installation does. The escape must never fire.
        for round in 0..4 {
            for _ in 1..MAX_BOOT_ATTEMPTS {
                assert!(
                    !escape_wedged_foreign_trial(&s, 1001, 1000),
                    "round {round}: inside the budget"
                );
            }
            // The other copy launches: `observe_launch` counts because IT is build 1000.
            std::thread::sleep(std::time::Duration::from_millis(10));
            let attempts = sentinel.observe_launch(1000).unwrap();
            assert!(attempts >= 1, "the owner really did advance its own trial");
            std::thread::sleep(std::time::Duration::from_millis(10));
            assert!(
                !escape_wedged_foreign_trial(&s, 1001, 1000),
                "round {round}: an advance restarts the budget instead of completing it"
            );
            assert!(
                sentinel.read_state().is_some(),
                "round {round}: the live trial survives"
            );
        }

        // NEGATIVE CONTROL: the same number of launches with the owner GONE (nothing
        // advances the sentinel) does reach the escape — so the guard above is what is
        // holding, not a budget that never completes.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut fired = false;
        for _ in 0..MAX_BOOT_ATTEMPTS + 1 {
            fired |= escape_wedged_foreign_trial(&s, 1001, 1000);
        }
        assert!(fired, "an abandoned trial is still escaped");
        assert_eq!(sentinel.read_state(), None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A new armed build restarts the budget: launches counted against a trial
    /// that has since been replaced must not carry over and disarm the new one
    /// early.
    #[test]
    fn the_foreign_trial_budget_does_not_accumulate_across_trials() {
        let (s, root) = temp_staging();
        let sentinel = boot_sentinel(&s);
        sentinel.arm(1000).unwrap();
        for _ in 1..MAX_BOOT_ATTEMPTS {
            assert!(!escape_wedged_foreign_trial(&s, 1001, 1000));
        }
        // A different build is armed now (the previous one was resolved).
        sentinel.arm(1002).unwrap();
        assert!(
            !escape_wedged_foreign_trial(&s, 1001, 1002),
            "the new trial gets the full budget, not the tail of the old one"
        );
        assert_eq!(sentinel.read_state(), Some((1002, 0)));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Only a refusal is recorded as one. The every-launch outcomes would rewrite
    /// `status.toml` on every boot, and a re-exec FAILURE belongs in the failure
    /// ledger — `record_apply_failure` clears refusals, never the reverse.
    #[test]
    fn only_a_deferred_boot_apply_is_recorded_as_a_refusal() {
        let deferred = ApplyOutcome::Deferred("trial 1000 unconfirmed".to_string());
        assert_eq!(
            boot_apply_refusal_reason(&deferred),
            Some("trial 1000 unconfirmed")
        );
        for quiet in [
            ApplyOutcome::NotApplicable,
            ApplyOutcome::NoUpdate,
            ApplyOutcome::ReExecFailed("exec: ENOMEM".to_string()),
        ] {
            assert_eq!(
                boot_apply_refusal_reason(&quiet),
                None,
                "{quiet:?} is not an apply refusal"
            );
        }
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
            machine_id: None,
            roster_seq: None,
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
        let failed_sentinel =
            prepare_trial(&failed_staging, &failed_ready, &failed_installed).unwrap();
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
        let _sentinel = prepare_trial(&staging, &ready, &installed).unwrap();
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
        let rollback_sentinel =
            prepare_trial(&rollback_staging, &rollback_ready, &rollback_installed).unwrap();
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

    // -----------------------------------------------------------------------
    // The zip extraction bound. These build central directories BY HAND rather
    // than shelling out to `ditto -c -k`, because the shapes that matter are
    // exactly the ones a real archiver will not produce: a saturated ZIP64
    // sentinel with nothing behind it, a directory cut mid-header, a size field
    // no filesystem could satisfy.
    // -----------------------------------------------------------------------

    /// One central directory file header for `name`, declaring `uncompressed`
    /// bytes and carrying `extra` as its extra field.
    fn central_header(name: &str, uncompressed: u32, extra: &[u8]) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend_from_slice(&CENTRAL_HEADER_SIGNATURE.to_le_bytes());
        header.extend_from_slice(&[0u8; 4]); // version made by + version needed
        header.extend_from_slice(&[0u8; 2]); // general purpose flags
        header.extend_from_slice(&[0u8; 2]); // method (stored)
        header.extend_from_slice(&[0u8; 4]); // mod time + mod date
        header.extend_from_slice(&[0u8; 4]); // crc32
        header.extend_from_slice(&0u32.to_le_bytes()); // compressed size
        header.extend_from_slice(&uncompressed.to_le_bytes());
        header.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        header.extend_from_slice(&u16::try_from(extra.len()).unwrap().to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // file comment length
        header.extend_from_slice(&[0u8; 2]); // disk number start
        header.extend_from_slice(&[0u8; 2]); // internal attributes
        header.extend_from_slice(&[0u8; 4]); // external attributes
        header.extend_from_slice(&0u32.to_le_bytes()); // local header offset
        assert_eq!(
            header.len(),
            CENTRAL_HEADER_FIXED_LEN,
            "fixed header layout"
        );
        header.extend_from_slice(name.as_bytes());
        header.extend_from_slice(extra);
        header
    }

    /// An End Of Central Directory record with the given (already-encoded)
    /// fields, so a test can declare a sentinel or a deliberate mismatch.
    fn eocd(entries: u16, size: u32, offset: u32, comment: &[u8]) -> Vec<u8> {
        let mut record = Vec::new();
        record.extend_from_slice(&EOCD_SIGNATURE.to_le_bytes());
        record.extend_from_slice(&0u16.to_le_bytes()); // this disk
        record.extend_from_slice(&0u16.to_le_bytes()); // disk with the directory
        record.extend_from_slice(&entries.to_le_bytes()); // entries on this disk
        record.extend_from_slice(&entries.to_le_bytes()); // total entries
        record.extend_from_slice(&size.to_le_bytes());
        record.extend_from_slice(&offset.to_le_bytes());
        record.extend_from_slice(&u16::try_from(comment.len()).unwrap().to_le_bytes());
        record.extend_from_slice(comment);
        record
    }

    /// A stand-in for the local-file-header area, so the directory offset under
    /// test is never the degenerate zero.
    const ZIP_PREFIX: &[u8] = b"PK\x03\x04stub!!";

    /// Write `bytes` to a fresh temp path and hand back a guard that removes it.
    struct TempZip(std::path::PathBuf);

    impl TempZip {
        fn new(label: &str, bytes: &[u8]) -> Self {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aterm-zipbound-{label}-{}-{n}.zip",
                std::process::id()
            ));
            std::fs::write(&path, bytes).unwrap();
            Self(path)
        }
    }

    impl Drop for TempZip {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Assemble prefix + headers + an EOCD whose extent and count match them —
    /// the shape every refusal test then perturbs in exactly one place.
    fn well_formed_zip(entries: &[(&str, u32)]) -> Vec<u8> {
        let mut bytes = ZIP_PREFIX.to_vec();
        let offset = u32::try_from(bytes.len()).unwrap();
        for &(name, uncompressed) in entries {
            bytes.extend_from_slice(&central_header(name, uncompressed, &[]));
        }
        let size = u32::try_from(bytes.len()).unwrap() - offset;
        let count = u16::try_from(entries.len()).unwrap();
        bytes.extend_from_slice(&eocd(count, size, offset, b""));
        bytes
    }

    /// The happy path: a directory `ditto -c -k` could plausibly have written is
    /// read exactly — entry count and summed uncompressed bytes both.
    #[test]
    fn zip_claim_reads_a_well_formed_central_directory() {
        let zip = TempZip::new(
            "ok",
            &well_formed_zip(&[
                ("aterm.app/", 0),
                ("aterm.app/Contents/Info.plist", 1_024),
                ("aterm.app/Contents/MacOS/aterm", 40_000_000),
            ]),
        );
        let claim = checked_zip_extraction_claim(&zip.0).expect("well-formed directory is read");
        assert_eq!(
            claim,
            ZipExtractClaim {
                entries: 3,
                uncompressed_bytes: 40_001_024,
            }
        );
    }

    /// An archive comment does not hide the end record: the scan must mind the
    /// variable-length tail rather than assume the record sits at EOF - 22.
    #[test]
    fn zip_claim_finds_the_end_record_behind_an_archive_comment() {
        let mut bytes = ZIP_PREFIX.to_vec();
        let offset = u32::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(&central_header("only", 7, &[]));
        let size = u32::try_from(bytes.len()).unwrap() - offset;
        // A 4 KiB comment of bytes that are NOT a signature, so the only valid
        // candidate is the real record.
        let comment = [b'c'; 4096];
        bytes.extend_from_slice(&eocd(1, size, offset, &comment));
        let zip = TempZip::new("comment", &bytes);
        assert_eq!(
            checked_zip_extraction_claim(&zip.0).expect("comment does not hide the end record"),
            ZipExtractClaim {
                entries: 1,
                uncompressed_bytes: 7,
            }
        );
    }

    /// A batteries-included container — the toolchain seed sealed into the same
    /// signed `.app` — declares far more than the retired 1 GiB cap while staying
    /// inside the ONE bound the cutter validates every published asset against.
    /// The client must be able to unpack anything the publisher was allowed to
    /// publish, or a legitimate release strands the whole channel at unpack time
    /// with nothing on the publishing side able to see it coming.
    #[test]
    fn zip_claim_admits_a_batteries_sized_container_the_cutter_would_publish() {
        assert_eq!(
            MAX_EXTRACT_BYTES,
            aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND,
            "the extract cap must never sit below the bound the cutter enforces"
        );
        let declared: u32 = 1_610_612_736; // 1.5 GiB
        let zip = TempZip::new(
            "batteries",
            &well_formed_zip(&[(
                "aterm.app/Contents/Resources/toolchain-seed/trust.tar.zst",
                declared,
            )]),
        );
        assert_eq!(
            checked_zip_extraction_claim(&zip.0).expect("a publishable container is accepted"),
            ZipExtractClaim {
                entries: 1,
                uncompressed_bytes: u64::from(declared),
            }
        );
    }

    /// An absurd declared size is refused. `u32::MAX - 1` is the largest size a
    /// classic entry can state without tripping the ZIP64 escape, and it is
    /// already twice the cap, so this is refused on BYTES — no
    /// filesystem, no `ditto`, nothing written.
    #[test]
    fn zip_claim_refuses_an_absurd_declared_size() {
        let zip = TempZip::new(
            "huge",
            &well_formed_zip(&[("aterm.app/huge", u32::MAX - 1)]),
        );
        let error = checked_zip_extraction_claim(&zip.0).expect_err("absurd size is refused");
        assert!(
            error.contains("uncompressed bytes") && error.contains("extraction cap"),
            "refusal names the byte cap: {error}"
        );
    }

    /// THIS MACHINE's free space is asked before `ditto` is spawned (2026-09-14,
    /// audit SV-3). `checked_zip_extraction_claim` bounds the fleet-wide cost; a
    /// declared size the volume cannot hold used to be learned by extracting into
    /// it and reading "ditto zip extract failed (exit status: 1)" — then re-downloading
    /// the container on every backoff step. The refusal names free, needed and the
    /// volume; a size the volume holds passes; a volume that will not report passes
    /// too (ditto decides, with its stderr kept).
    #[test]
    fn the_stage_refuses_an_unpack_the_volume_cannot_hold_and_names_the_numbers() {
        let (_s, root) = temp_staging();
        let free = free_bytes_at(&root).expect("the scratch volume reports its free space");
        // More than the volume has, by a margin no headroom explains.
        let error = refuse_without_room(&root, free.saturating_add(1 << 40), "the update zip")
            .expect_err("a declared size past the volume is refused before any byte lands");
        assert!(
            error.contains("not enough free space")
                && error.contains("MiB free")
                && error.contains("MiB needed")
                && error.contains(&root.display().to_string()),
            "the refusal names the numbers and the volume: {error}"
        );
        // A modest size the volume holds is let through.
        refuse_without_room(&root, 1024, "the update zip").expect("a small unpack fits");
        // A path no volume answers for is let through, not refused: ditto decides.
        refuse_without_room(
            Path::new("/nonexistent/volume/for/this/test"),
            u64::MAX / 2,
            "the update zip",
        )
        .expect("an unanswerable volume defers to ditto");
        assert!(free_bytes_at(Path::new("/nonexistent/volume/for/this/test")).is_none());
    }

    /// `tree_bytes` is what the DMG lane's copy will write: regular files summed,
    /// directories and symlinks counted as nothing and not followed.
    #[test]
    fn tree_bytes_sums_regular_files_and_never_follows_links() {
        let (_s, root) = temp_staging();
        let tree = root.join("tree.app");
        std::fs::create_dir_all(tree.join("Contents/MacOS")).unwrap();
        std::fs::write(tree.join("Contents/MacOS/aterm"), vec![7u8; 1000]).unwrap();
        std::fs::write(tree.join("Contents/Info.plist"), vec![1u8; 24]).unwrap();
        // A link to a big file outside the tree must not be counted through.
        let big = root.join("big.bin");
        std::fs::write(&big, vec![0u8; 100_000]).unwrap();
        std::os::unix::fs::symlink(&big, tree.join("Contents/MacOS/link")).unwrap();
        assert_eq!(tree_bytes(&tree), 1024);
        assert_eq!(tree_bytes(&root.join("absent")), 0);
    }

    /// Many small entries are refused on COUNT even though their declared bytes
    /// are zero — the inode/block cost bytes alone cannot see.
    #[test]
    fn zip_claim_refuses_an_absurd_entry_count() {
        let over = usize::try_from(MAX_EXTRACT_ENTRIES).unwrap() + 1;
        let mut bytes = ZIP_PREFIX.to_vec();
        let offset = u32::try_from(bytes.len()).unwrap();
        for _ in 0..over {
            bytes.extend_from_slice(&central_header("e", 0, &[]));
        }
        let size = u32::try_from(bytes.len()).unwrap() - offset;
        bytes.extend_from_slice(&eocd(u16::try_from(over).unwrap(), size, offset, b""));
        let zip = TempZip::new("many", &bytes);
        let error = checked_zip_extraction_claim(&zip.0).expect_err("absurd count is refused");
        assert!(
            error.contains("entries"),
            "refusal names the entry cap: {error}"
        );
    }

    /// A directory cut mid-header is refused. The end record still declares the
    /// full extent, so the walk runs out of header before it runs out of extent
    /// — the shape a truncated upload produces.
    #[test]
    fn zip_claim_refuses_a_truncated_central_directory() {
        let mut bytes = ZIP_PREFIX.to_vec();
        let offset = u32::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(&central_header("first", 10, &[]));
        let full = central_header("second", 20, &[]);
        let present = u32::try_from(bytes.len()).unwrap() - offset;
        // Half of the second header survives; the extent still claims both.
        let size = present + u32::try_from(full.len()).unwrap();
        bytes.extend_from_slice(&full[..full.len() / 2]);
        bytes.extend_from_slice(&eocd(2, size, offset, b""));
        let zip = TempZip::new("cut", &bytes);
        let error = checked_zip_extraction_claim(&zip.0).expect_err("truncation is refused");
        assert!(
            error.contains("does not lie before its end record")
                || error.contains("ends inside a file header"),
            "refusal names the truncation: {error}"
        );
    }

    /// A directory whose walked entry count disagrees with the trailer is
    /// refused: the two records must describe the SAME archive, or the bound was
    /// measured off something other than what is about to be unpacked.
    #[test]
    fn zip_claim_refuses_a_directory_that_contradicts_its_trailer() {
        let mut bytes = ZIP_PREFIX.to_vec();
        let offset = u32::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(&central_header("one", 1, &[]));
        bytes.extend_from_slice(&central_header("two", 2, &[]));
        let size = u32::try_from(bytes.len()).unwrap() - offset;
        // Two headers present, seven declared.
        bytes.extend_from_slice(&eocd(7, size, offset, b""));
        let zip = TempZip::new("miscount", &bytes);
        let error = checked_zip_extraction_claim(&zip.0).expect_err("miscount is refused");
        assert!(
            error.contains("end record declares"),
            "refusal names the disagreement: {error}"
        );
    }

    /// A ZIP64 sentinel with no locator behind it is a shape this reader cannot
    /// describe, and the saturated field is exactly the one the cap is measured
    /// against — so it is a REFUSAL, never "assume fine".
    #[test]
    fn zip_claim_refuses_a_zip64_sentinel_without_a_locator() {
        for (label, entries, size, offset) in [
            ("count", u16::MAX, 46u32, 12u32),
            ("size", 1u16, u32::MAX, 12u32),
            ("offset", 1u16, 46u32, u32::MAX),
        ] {
            let mut bytes = ZIP_PREFIX.to_vec();
            bytes.extend_from_slice(&central_header("one", 1, &[]));
            bytes.extend_from_slice(&eocd(entries, size, offset, b""));
            let zip = TempZip::new(label, &bytes);
            let error = checked_zip_extraction_claim(&zip.0)
                .expect_err("a sentinel with no ZIP64 record is refused");
            assert!(
                error.contains("carries no ZIP64 one"),
                "{label}: refusal names the missing ZIP64 record: {error}"
            );
        }
    }

    /// An entry escaping to a ZIP64 size with no ZIP64 extra field to hold it is
    /// refused rather than read as a literal ~4 GiB (which would also fail, but
    /// for the wrong reason — the point is that the escape is UNRESOLVED).
    #[test]
    fn zip_claim_refuses_a_zip64_escape_with_no_extra_field() {
        let zip = TempZip::new("escape", &well_formed_zip(&[("aterm.app/x", u32::MAX)]));
        let error = checked_zip_extraction_claim(&zip.0).expect_err("bare escape is refused");
        assert!(
            error.contains("carries no ZIP64 extra field"),
            "refusal names the missing extra field: {error}"
        );
    }

    /// Two trailers that both run exactly to EOF are ambiguous: the bound would
    /// be measured off a record that may not be the one `ditto` follows.
    #[test]
    fn zip_claim_refuses_an_ambiguous_pair_of_end_records() {
        let mut bytes = ZIP_PREFIX.to_vec();
        let offset = u32::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(&central_header("one", 1, &[]));
        let size = u32::try_from(bytes.len()).unwrap() - offset;
        // The decoy's comment swallows the real record, so BOTH end exactly at
        // EOF and both parse.
        let real = eocd(1, size, offset, b"");
        let filler = vec![0u8; real.len()];
        bytes.extend_from_slice(&eocd(1, size, offset, &filler));
        let decoy_comment_start = bytes.len() - real.len();
        bytes[decoy_comment_start..].copy_from_slice(&real);
        let zip = TempZip::new("ambiguous", &bytes);
        let error = checked_zip_extraction_claim(&zip.0).expect_err("ambiguity is refused");
        assert!(
            error.contains("more than one end-of-central-directory record"),
            "refusal names the ambiguity: {error}"
        );
    }

    /// A file too short to hold an end record at all is refused, not read as an
    /// empty archive.
    #[test]
    fn zip_claim_refuses_a_file_with_no_end_record() {
        let zip = TempZip::new("stub", b"PK\x03\x04not-a-zip");
        let error = checked_zip_extraction_claim(&zip.0).expect_err("a non-archive is refused");
        assert!(
            error.contains("end-of-central-directory"),
            "refusal names the missing end record: {error}"
        );
    }

    // -----------------------------------------------------------------------
    // 2026-09-14 stage-verify audit.
    // -----------------------------------------------------------------------

    /// THE OWNER'S DESK, 2026-09-14 (aterm.log 1789364570 … 1789390973): six automatic
    /// applies of v0.85.0 refused with exactly this error, escalating stand-downs up
    /// to ~6 h — and the cold-launch fallback refuses for the SAME reason at step 7 of
    /// `apply_staged_if_ready_inner` ("current installed rollback source is not
    /// verified"), so a hand-installed, ad-hoc-signed bundle in `/Applications` can
    /// never be updated by ANY lane. The refusal names the cause and the path; it must
    /// also name the way out, because nothing else on the machine does: put the signed
    /// release back (`tools/install.sh`, or drag it from the release DMG), or mark a
    /// local build `ATermDevBuild` so the updater leaves it alone instead of failing
    /// every half hour.
    #[test]
    fn a_rollback_source_refusal_names_the_remedy() {
        let root = std::env::temp_dir().join(format!(
            "aterm-rollback-remedy-{}-{}",
            std::process::id(),
            super::unix_now_secs()
        ));
        let error = super::preverify_installed_rollback_source(Some(&root), 100, Some("abcdef0"))
            .expect_err("a path that is not a bundle cannot be a rollback source");
        assert!(
            error.contains("tools/install.sh"),
            "the refusal must say how to put a signed release back: {error}"
        );
        assert!(
            error.contains("ATermDevBuild"),
            "…and how to take a local build out of the channel on purpose: {error}"
        );
    }

    /// `sweep_stale_mounts` runs only on the DMG lane and `sweep_stale_extracts` only
    /// on the zip lane. Every current release carries a zip, so a `mnt-<pid>` that a
    /// DMG-era stage left behind (a process killed mid-stage; on this machine
    /// `Updates/mnt-21441`, 2026-08-31, still present on 2026-09-14) is never
    /// reclaimed — and if the image behind it was still attached, it stays attached
    /// until reboot. Both lanes must sweep both kinds of scratch. The sweep runs
    /// BEFORE the unpack, so a zip `ditto` cannot even open still reclaims the
    /// leftover.
    #[test]
    fn the_zip_lane_reclaims_a_dmg_era_mountpoint() {
        let (s, root) = temp_staging();
        let stale_mount = s.root.join("mnt-424242");
        std::fs::create_dir_all(&stale_mount).unwrap();
        let stale_extract = s.staged_dir().join("zx-424242");
        std::fs::create_dir_all(&stale_extract).unwrap();
        let zip = TempZip::new("sweep", &well_formed_zip(&[("aterm.app/", 0)]));
        let manifest = Manifest {
            schema: 1,
            version: "0.0.9".into(),
            build_number: 9,
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            sha256: "ab".repeat(32),
            dmg: "aterm-0.0.9.dmg".into(),
            url: None,
            zip: Some("aterm-0.0.9-mac.zip".into()),
            zip_sha256: Some("cd".repeat(32)),
            min_build: None,
            machine_id: None,
            roster_seq: None,
            changelog: None,
        };
        let outcome = stage_from_zip(&s, &zip.0, &manifest, "");
        assert!(
            outcome.is_err(),
            "a synthetic central directory has nothing ditto can unpack: {outcome:?}"
        );
        assert!(
            !stale_extract.exists(),
            "the zip lane reclaims its own leftover extract dir"
        );
        assert!(
            !stale_mount.exists(),
            "the DMG lane's leftover mountpoint must be reclaimed by the zip lane too"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod launchd_copy_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    fn budget_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            LaunchdCopyDeadline {
                const Buggy = 0;
                var now = 0;
                var deadline = 3;
                var cleanup = 0;
                action Tick when (now <= 3) { now = now + 1; }
                action Cleanup when (cleanup == 0) {
                    cleanup = 1;
                    deadline = if Buggy == 1 { now + 4 } else { 4 };
                }
                invariant NoRenewedDeadline: deadline <= 4;
            }
        }
    }

    #[test]
    fn launchd_copy_deadline_proves_and_catches_renewed_cleanup_budget() {
        let model = budget_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, "launchd copy deadline");
        let started = Instant::now();
        let limit = Duration::from_secs(4);
        let budget = LaunchdCopyBudget::new(started, limit);
        for seconds in 0..=4 {
            let now = started + Duration::from_secs(seconds);
            for cleanup in [false, true] {
                let mut state = model.init_state();
                state.insert("now", i64::try_from(seconds).unwrap());
                state.insert("cleanup", i64::from(cleanup));
                let until = budget.deadline(cleanup);
                state.insert(
                    "deadline",
                    i64::try_from(until.duration_since(started).as_secs()).unwrap(),
                );
                assert!(model.check_invariant("NoRenewedDeadline", &state));
                assert!(
                    until.saturating_duration_since(now)
                        <= limit.saturating_sub(now.duration_since(started))
                );
                if cleanup && seconds > 0 {
                    // Historical per-operation timeout: a fresh deadline after
                    // submit has already consumed part of the caller's budget.
                    state.insert(
                        "deadline",
                        i64::try_from((now + limit).duration_since(started).as_secs()).unwrap(),
                    );
                    assert!(!model.check_invariant("NoRenewedDeadline", &state));
                }
            }
        }
    }

    fn publication_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            IsolatedCopyPublication {
                const Buggy = 0;
                var phase = 0;
                var current_target = 1;
                var old_target = 1;
                var opened_target = 0;
                var published_target = 0;
                var corrupted = 0;
                action Timeout when (phase == 0) { phase = 1; }
                action Retry when (phase == 1) {
                    phase = 2;
                    current_target = if Buggy == 1 { old_target } else { 2 };
                }
                action OpenOld when (phase == 2) {
                    phase = 3;
                    opened_target = old_target;
                }
                action Publish when (phase == 3) {
                    phase = 4;
                    published_target = current_target;
                }
                action LateWrite when (phase == 4) {
                    phase = 5;
                    corrupted = if opened_target == published_target { 1 } else { 0 };
                }
                invariant NoLateMutation: corrupted == 0;
            }
        }
    }

    fn wrapper_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            LaunchdCopyOnce {
                const Buggy = 0;
                var claimed = 0;
                var complete = 0;
                var copies = 0;
                var replayed = 0;
                var removed = 0;
                action Start when (claimed == 0 && removed == 0) {
                    claimed = 1;
                    copies = 1;
                }
                action Complete when (claimed == 1 && complete == 0) { complete = 1; }
                action Replay when (complete == 1 && replayed == 0) {
                    replayed = 1;
                    copies = if Buggy == 1 { 2 } else { copies };
                }
                action Remove when (complete == 1 && removed == 0) { removed = 1; }
                invariant OneCopy: copies <= 1;
            }
        }
    }

    fn copy_model_step(
        model: &aterm_spec::derive::Model,
        state: &mut aterm_spec::interp::State,
        action: &str,
    ) {
        let next = model.successors(action, state);
        assert_eq!(next.len(), 1, "{action}: {state:?}");
        *state = next[0].clone();
    }

    #[test]
    fn isolated_copy_and_one_shot_wrapper_prove_and_catch() {
        for model in [publication_model(), wrapper_model()] {
            aterm_spec::verify::prove_and_catch_scalar(&model, "isolated update copy");
        }
    }

    // The negative control replays the old shipping policy: ditto receives the
    // caller's reusable path directly, and returning success needs no promotion.
    fn copy_with_policy(
        isolated: bool,
        args: &[std::ffi::OsString],
        run: impl FnOnce(
            &[std::ffi::OsString],
            &Path,
        )
            -> Result<(std::process::ExitStatus, String), crate::verify::HelperFailure>,
    ) -> Result<(std::process::ExitStatus, String), String> {
        if isolated {
            copy_into_isolated_destination(args, "copy fixture", run)
        } else {
            run(args, Path::new(args.last().unwrap()).parent().unwrap())
                .map_err(|failure| failure.message)
        }
    }

    #[test]
    fn a_timed_out_writer_cannot_mutate_a_retry_after_stage_or_swap_publication() {
        use std::io::Write;
        use std::os::unix::process::ExitStatusExt;
        use std::sync::mpsc;
        for shape in ["dmg", "zip", "cross-volume"] {
            for isolated in [true, false] {
                let staging = Staging::scratch(&format!("copy-late-{shape}-{isolated}"));
                std::fs::create_dir_all(staging.staged_dir()).unwrap();
                let installed = staging.root.join("aterm.app");
                let destination = match shape {
                    "dmg" => staging.staged_dir().join("aterm.app.incoming"),
                    "zip" => staging.staged_dir().join("zx-fixture"),
                    _ => rollback_path(&installed),
                };
                let bundle_in = |payload: &Path| {
                    if shape == "zip" {
                        payload.join("aterm.app")
                    } else {
                        payload.to_path_buf()
                    }
                };
                let args = ["source".into(), destination.as_os_str().to_os_string()];
                let (open_tx, open_rx) = mpsc::sync_channel(1);
                let (opened_tx, opened_rx) = mpsc::sync_channel(1);
                let (write_tx, write_rx) = mpsc::sync_channel(1);
                let mut old_payload = None;
                let mut writer = None;
                let error = copy_with_policy(isolated, &args, |actual, _| {
                    let payload = PathBuf::from(actual.last().unwrap());
                    let bundle = bundle_in(&payload);
                    std::fs::create_dir_all(&bundle).unwrap();
                    std::fs::write(bundle.join("id"), "OLD-COPY").unwrap();
                    old_payload = Some(payload);
                    // Open the old job's absolute destination only AFTER the
                    // retry has materialized, then keep that actual file open
                    // across the retry's publication/atomic exchange.
                    writer = Some(std::thread::spawn(move || {
                        open_rx.recv_timeout(Duration::from_secs(30)).unwrap();
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .open(bundle.join("id"))
                            .unwrap();
                        opened_tx.send(()).unwrap();
                        write_rx.recv_timeout(Duration::from_secs(30)).unwrap();
                        file.set_len(0).unwrap();
                        file.write_all(b"LATE-WRITE").unwrap();
                        file.sync_all().unwrap();
                    }));
                    Err(crate::verify::HelperFailure {
                        message: "copy deadline elapsed; cleanup not confirmed".to_string(),
                        writer_stopped: false,
                    })
                })
                .unwrap_err();
                assert!(error.contains("cleanup not confirmed"), "{error}");
                let old_payload = old_payload.unwrap();
                let mut model = publication_model();
                model
                    .consts
                    .iter_mut()
                    .find(|(name, _)| *name == "Buggy")
                    .unwrap()
                    .1 = i64::from(!isolated);
                let mut state = model.init_state();
                copy_model_step(&model, &mut state, "Timeout");
                // The real callers reclaim their requested destination after
                // failure. The isolated attempt must remain outside that cleanup.
                remove_path_no_follow(&destination).unwrap();
                if shape == "zip" {
                    std::fs::create_dir(&destination).unwrap();
                }
                copy_with_policy(isolated, &args, |actual, _| {
                    let payload = PathBuf::from(actual.last().unwrap());
                    let bundle = bundle_in(&payload);
                    std::fs::create_dir_all(&bundle).unwrap();
                    std::fs::write(bundle.join("id"), "NEW").unwrap();
                    copy_model_step(&model, &mut state, "Retry");
                    assert_eq!(
                        state["current_target"],
                        if payload == old_payload { 1 } else { 2 },
                        "the model must bind actual destination identity"
                    );
                    if isolated {
                        assert_eq!(payload.parent().unwrap().parent(), destination.parent());
                        assert!(
                            old_payload.exists(),
                            "caller cleanup cannot reclaim a live attempt"
                        );
                    }
                    open_tx.send(()).unwrap();
                    opened_rx.recv_timeout(Duration::from_secs(30)).unwrap();
                    copy_model_step(&model, &mut state, "OpenOld");
                    Ok((std::process::ExitStatus::from_raw(0), String::new()))
                })
                .unwrap();

                let published = if shape == "cross-volume" {
                    std::fs::create_dir(&installed).unwrap();
                    std::fs::write(installed.join("id"), "ROLLBACK").unwrap();
                    checked_bundle_exchange(&destination, &installed, "copy fixture swap").unwrap();
                    installed
                } else {
                    let ready = Ready {
                        build_number: 42,
                        version: "0.42.0".to_string(),
                        commit: Some("ab".repeat(20)),
                        dmg_sha256: "cd".repeat(32),
                        team_id: String::new(),
                        staged_at: String::new(),
                        changelog: None,
                        machine_id: None,
                        roster_seq: None,
                    };
                    // The fixture models already-verified bytes; publication and
                    // the apply lock below are the genuine shipping transaction.
                    publish_verified_stage(&staging, &bundle_in(&destination), &ready).unwrap();
                    staging.staged_app.clone()
                };
                copy_model_step(&model, &mut state, "Publish");
                write_tx.send(()).unwrap();
                writer.unwrap().join().unwrap();
                copy_model_step(&model, &mut state, "LateWrite");
                let actual = std::fs::read_to_string(published.join("id")).unwrap();
                assert_eq!(state["corrupted"], i64::from(actual != "NEW"));
                assert_eq!(model.check_invariant("NoLateMutation", &state), isolated);
                assert_eq!(actual, if isolated { "NEW" } else { "LATE-WRITE" });
                if shape == "cross-volume" {
                    assert_eq!(
                        std::fs::read_to_string(destination.join("id")).unwrap(),
                        "ROLLBACK"
                    );
                }
                let _ = std::fs::remove_dir_all(staging.root);
            }
        }
    }

    #[test]
    fn the_real_launchd_wrapper_cannot_copy_again_after_publishing_status() {
        use std::os::unix::process::ExitStatusExt;
        for guarded in [true, false] {
            let staging = Staging::scratch(&format!("copy-wrapper-replay-{guarded}"));
            let cleanup = staging.root.join("cleanup-fixture");
            std::fs::write(&cleanup, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&cleanup, std::fs::Permissions::from_mode(0o700)).unwrap();
            let destination = staging.root.join("result");
            let calls = staging.root.join("copies");
            let script = if guarded {
                LAUNCHD_COPY_WRAPPER.to_string()
            } else {
                LAUNCHD_COPY_WRAPPER.replace("/bin/mkdir \"$st.once\" 2>/dev/null || exit 0; ", "")
            };
            assert_eq!(script == LAUNCHD_COPY_WRAPPER, guarded);
            let run_wrapper = |root: &Path, payload: &Path| {
                let (exit, stderr) = crate::verify::status_bounded_with_stderr(
                    Command::new("/bin/sh")
                        .arg("-c")
                        .arg(&script)
                        .arg("fixture-wrapper")
                        .arg(root.join("status"))
                        .arg("fixture-label")
                        .arg(&cleanup)
                        .arg("/bin/sh")
                        .arg("-c")
                        .arg(r#"/bin/mkdir -p "$1"; printf NEW > "$1/id"; printf 'copy\n' >> "$2""#)
                        .arg("fixture-copy")
                        .arg(payload)
                        .arg(&calls),
                    "one-shot wrapper fixture",
                    Duration::from_secs(5),
                )
                .unwrap();
                assert!(exit.success(), "{stderr}");
            };
            let mut model = wrapper_model();
            model
                .consts
                .iter_mut()
                .find(|(name, _)| *name == "Buggy")
                .unwrap()
                .1 = i64::from(!guarded);
            let mut state = model.init_state();
            let mut old_paths = None;
            copy_into_isolated_destination(
                &[destination.clone().into_os_string()],
                "wrapper copy",
                |args, root| {
                    let payload = Path::new(args.last().unwrap());
                    run_wrapper(root, payload);
                    copy_model_step(&model, &mut state, "Start");
                    assert_eq!(std::fs::read_to_string(root.join("status")).unwrap(), "0\n");
                    copy_model_step(&model, &mut state, "Complete");
                    // Replay the exact shipping shell after its terminal status is
                    // visible, before the parent gets to rename the completed tree.
                    run_wrapper(root, payload);
                    copy_model_step(&model, &mut state, "Replay");
                    assert_eq!(
                        state["copies"],
                        i64::try_from(std::fs::read_to_string(&calls).unwrap().lines().count())
                            .unwrap()
                    );
                    assert_eq!(model.check_invariant("OneCopy", &state), guarded);
                    old_paths = Some((root.to_path_buf(), payload.to_path_buf()));
                    Ok((std::process::ExitStatus::from_raw(0), String::new()))
                },
            )
            .unwrap();
            copy_model_step(&model, &mut state, "Remove");
            if guarded {
                let (root, payload) = old_paths.unwrap();
                assert!(!root.exists());
                run_wrapper(&root, &payload);
                assert_eq!(std::fs::read_to_string(&calls).unwrap(), "copy\n");
                assert!(!root.exists(), "a replay cannot recreate a retired attempt");
            }
            assert_eq!(
                std::fs::read_to_string(destination.join("id")).unwrap(),
                "NEW"
            );
            let _ = std::fs::remove_dir_all(staging.root);
        }
    }

    fn reaper_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            AbandonedCopyReclamation {
                const Buggy = 0;
                var abandoned = 0;
                var complete = 0;
                var fenced = 1;
                var reaped = 0;
                action Abandon when (abandoned == 0 && reaped == 0) { abandoned = 1; }
                action Complete when (complete == 0 && reaped == 0) { complete = 1; }
                action LoseFence when (fenced == 1 && reaped == 0) { fenced = 0; }
                action Reap when (reaped == 0 && complete == 1 && fenced == 1 && (abandoned == 1 || Buggy == 1)) {
                    reaped = 1;
                }
                invariant ReclaimOnlyAbandoned: reaped == 0 || abandoned == 1;
                invariant ReclaimOnlyCompleted: reaped == 0 || complete == 1;
                invariant ReclaimOnlyFenced: reaped == 0 || fenced == 1;
            }
        }
    }

    #[test]
    fn abandoned_copy_reaping_requires_terminal_status_and_the_one_shot_fence() {
        let model = reaper_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, "abandoned copy reclamation");
        for abandoned in [false, true] {
            for completed in ["absent", "malformed", "complete"] {
                for fenced in [false, true] {
                    let staging = Staging::scratch("copy-reaper");
                    let attempt = IsolatedCopy::create(&staging.root.join("destination")).unwrap();
                    let label = format!("systems.alab.aterm-update.unpack-{}", "ab".repeat(16));
                    std::fs::write(attempt.root.join("launchd.job"), &label).unwrap();
                    if abandoned {
                        std::fs::write(attempt.root.join("abandoned"), "1\n").unwrap();
                    }
                    if fenced {
                        std::fs::create_dir(attempt.root.join(format!(".{label}.status.once")))
                            .unwrap();
                    }
                    match completed {
                        "complete" => {
                            std::fs::write(attempt.root.join(format!(".{label}.status")), "0\n")
                                .unwrap()
                        }
                        "malformed" => std::fs::write(
                            attempt.root.join(format!(".{label}.status")),
                            "unfinished",
                        )
                        .unwrap(),
                        _ => {}
                    }
                    let mut state = model.init_state();
                    state.insert("abandoned", i64::from(abandoned));
                    state.insert("complete", i64::from(completed == "complete"));
                    state.insert("fenced", i64::from(fenced));
                    let admitted = model.action_enabled("Reap", &state);
                    // Creating an apply/copy attempt must not recursively sweep
                    // predecessors, even when their cleanup is already authorized.
                    // The historical allocation-time sweep fails this assertion.
                    let next =
                        IsolatedCopy::create(&staging.root.join("next-destination")).unwrap();
                    assert!(
                        attempt.root.exists(),
                        "allocation must leave bulk cleanup to the checker"
                    );
                    reap_abandoned_copy_attempts(&staging.root);
                    assert!(
                        next.root.exists(),
                        "maintenance cannot reclaim the live new attempt"
                    );
                    assert_eq!(!attempt.root.exists(), admitted);
                    if admitted {
                        copy_model_step(&model, &mut state, "Reap");
                        assert!(model.check_invariant("ReclaimOnlyAbandoned", &state));
                        assert!(
                            !staging.root.join("destination").exists(),
                            "late completion cannot promote abandoned bytes"
                        );
                    }
                    if !abandoned && completed == "complete" && fenced {
                        // Negative control: completion alone is not authority to
                        // delete the current caller's not-yet-promoted output.
                        std::fs::remove_dir_all(&attempt.root).unwrap();
                        state.insert("reaped", 1);
                        assert!(!model.check_invariant("ReclaimOnlyAbandoned", &state));
                    }
                    let _ = std::fs::remove_dir_all(staging.root);
                }
            }
        }
    }

    #[test]
    fn an_observed_stopped_writer_reclaims_its_failed_attempt_immediately() {
        let staging = Staging::scratch("copy-confirmed-failure");
        let destination = staging.root.join("destination");
        let mut root = None;
        let error = copy_into_isolated_destination(
            &[destination.clone().into_os_string()],
            "failed copy",
            |_, attempt| {
                root = Some(attempt.to_path_buf());
                Err(crate::verify::HelperFailure {
                    message: "the owned child was reaped".to_string(),
                    writer_stopped: true,
                })
            },
        )
        .unwrap_err();
        assert_eq!(error, "the owned child was reaped");
        assert!(!root.unwrap().exists());
        assert!(!destination.exists());
        let _ = std::fs::remove_dir_all(staging.root);
    }

    fn fake_launchctl(staging: &Staging, mode: &str) -> std::path::PathBuf {
        let script = staging.root.join("launchctl-fixture");
        std::fs::write(staging.root.join("mode"), mode).unwrap();
        // No launchd mutation and no inherited background child: a hung helper
        // replaces itself with sleep, so the real bounded runner can kill/reap it.
        std::fs::write(
            &script,
            r##"#!/bin/sh
dir="${0%/*}"
printf '%s\n' "$1" >> "$dir/calls"
IFS= read -r mode < "$dir/mode"
if [ "$1" = remove ]; then
    if [ "$mode" = remove-hangs ] || [ "$mode" = both-hang ]; then exec /bin/sleep 30; fi
    exit 0
fi
if [ "$mode" = submit-hangs ] || [ "$mode" = both-hang ]; then exec /bin/sleep 30; fi
# submit -l LABEL -e STDERR -- /bin/sh -c WRAPPER argv0 STATUS LABEL launchctl ditto ...
shift 3
err="$2"
shift 3
st="$5"
case "$mode" in
    complete)
        /usr/bin/head -c 65536 /dev/zero > "$err"
        printf '\ncopy failed: fixture disk full\n' >> "$err"
        printf '1\n' > "$st"
        ;;
    malformed) printf 'not-an-exit-status\n' > "$st" ;;
esac
exit 0
"##,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        script
    }

    #[test]
    fn launchd_submit_and_remove_share_the_real_callers_deadline() {
        for mode in ["submit-hangs", "remove-hangs", "both-hang"] {
            let staging = Staging::scratch(&format!("launchd-bound-{mode}"));
            let launchctl = fake_launchctl(&staging, mode);
            let started = Instant::now();
            let error = ditto_via_launchd_using(
                &[],
                "fixture unpack",
                // Leave room for cold helper startup during parallel tests;
                // the fake helper's 30 s hang still exceeds this by a wide margin.
                Duration::from_secs(2),
                &staging.root,
                &launchctl,
            )
            .unwrap_err();
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{mode} bypassed the deadline: {error}"
            );
            assert!(error.contains("did not finish"), "{mode}: {error}");
            assert_eq!(
                std::fs::read_to_string(staging.root.join("calls")).unwrap_or_else(|receipt| {
                    panic!(
                        "{mode}: missing helper receipt at {}: {receipt}; runner: {error}",
                        staging.root.display()
                    )
                }),
                "submit\nremove\n"
            );
            if mode != "submit-hangs" {
                assert!(
                    error.contains("cleanup not confirmed"),
                    "a timed-out removal must be explicit: {error}"
                );
            }
            let _ = std::fs::remove_dir_all(staging.root);
        }
    }

    #[test]
    fn launchd_completion_keeps_only_the_error_tail_and_rejects_malformed_status() {
        let staging = Staging::scratch("launchd-result-tail");
        let launchctl = fake_launchctl(&staging, "complete");
        let (status, stderr) = ditto_via_launchd_using(
            &[],
            "fixture unpack",
            Duration::from_secs(5),
            &staging.root,
            &launchctl,
        )
        .unwrap();
        assert!(!status.success());
        assert!(stderr.len() <= 512);
        assert_eq!(
            crate::verify::last_stderr_line(&stderr),
            Some("copy failed: fixture disk full")
        );
        assert!(std::fs::read_dir(&staging.root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".systems.alab.")
        }));
        std::fs::write(staging.root.join("mode"), "malformed").unwrap();
        let error = ditto_via_launchd_using(
            &[],
            "fixture unpack",
            Duration::from_secs(5),
            &staging.root,
            &launchctl,
        )
        .unwrap_err();
        assert!(error.contains("malformed exit status"), "{error}");
        assert_eq!(
            std::fs::read_to_string(staging.root.join("calls")).unwrap(),
            "submit\nsubmit\nremove\n"
        );
        let _ = std::fs::remove_dir_all(staging.root);
    }
}
