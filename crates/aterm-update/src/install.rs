// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Staging (unpack → verify → publish) and application (lock → re-verify → atomic swap → re-exec) of an update.

use std::path::{Path, PathBuf};

use aterm_durable::Flush;
use aterm_update_core::{FileLock, LaunchName, Sentinel, ensure_private_dir, same_volume};

use crate::manifest::{Manifest, Ready};
use crate::paths::Staging;
use crate::sys::rename_swap;
use crate::{ApplyOutcome, bundle, verify};

/// A freshly swapped build is auto-reverted after this many consecutive launches that find the boot sentinel still unconfirmed (a crash loop).
pub(crate) const MAX_BOOT_ATTEMPTS: u32 = 3;

/// How long a launch waits for the apply lock before starting on the installed build.
pub(crate) const APPLY_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long the background lane (the checker's stage and publish, and the floor ratchet under them) waits for the stage, apply or floor lock before giving the cycle up, so a stopped holder in another aterm cannot park the checker thread.
pub(crate) const BACKGROUND_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

const CROSS_VOLUME_COPY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

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
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|error| format!("create isolated copy directory: {error}"))?;
        if let Err(error) = std::fs::write(root.join("copy-layout"), "aterm-copy-v1\n") {
            let _ = std::fs::remove_dir(&root);
            return Err(format!("record isolated copy layout: {error}"));
        }
        let _ = std::fs::write(root.join(COPY_OWNER), format!("{}\n", std::process::id()));
        Ok(Self {
            payload: root.join("payload"),
            root,
        })
    }
}

const COPY_OWNER: &str = "owner";

const STALE_COPY_ATTEMPT_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

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

fn copy_attempt_lapsed(root: &Path) -> bool {
    std::fs::symlink_metadata(root.join("copy-layout"))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|written| std::time::SystemTime::now().duration_since(written).ok())
        .is_some_and(|age| age >= STALE_COPY_ATTEMPT_AFTER)
}

/// Reclaims copy attempts no writer and no caller can still use: past [`STALE_COPY_ATTEMPT_AFTER`] and abandoned by their caller or outliving it ([`copy_owner_gone`]).
pub(crate) fn reap_abandoned_copy_attempts(parent: &Path) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    // SAFETY: geteuid has no pointer arguments or preconditions.
    let owner = unsafe { libc::geteuid() };
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
    let _ = std::fs::remove_dir_all(&attempt.root);
    result
}

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

const STAGE_UNPACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Slack the free-space check demands over the declared size: filesystem metadata, the extract's own scratch dir, and the verify step's writes.
pub(crate) const STAGE_FREE_SPACE_HEADROOM: u64 = 64 * 1024 * 1024;

/// The one action a full disk's refusal names, the same for the download, the unpack and the free-space check.
pub(crate) const FREE_SPACE_REMEDY: &str =
    "free some space and check for updates, which retries it at once";

fn ditto_failure(what: &str, status: std::process::ExitStatus, stderr: &str) -> String {
    match crate::verify::last_stderr_line(stderr) {
        Some(line) => format!("{what} ({status}): {}", crate::quote_tool_output(line)),
        None => format!("{what} ({status})"),
    }
}

/// A staging unpack's `ditto` failure ([`ditto_failure`]).
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
    crate::warn(&format!(
        "{what} ({status}): {}",
        crate::quote_tool_output(line)
    ));
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

/// Bytes still available to this user on the volume holding `path`, or `None` when the volume will not say (an unmounted or foreign path).
pub(crate) fn free_bytes_at(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` is zeroed storage the call fills; `c_path` outlives it.
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &raw mut vfs) };
    if rc != 0 {
        return None;
    }
    Some(u64::from(vfs.f_bavail) * vfs.f_frsize)
}

/// Refuse to unpack `needed` bytes into `dir` when the volume cannot hold them plus [`STAGE_FREE_SPACE_HEADROOM`].
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
pub(crate) struct ExpectedArtifact {
    build: u64,
    commit: String,
    dmg_sha256: String,
}

impl ExpectedArtifact {
    /// The exact artifact a handoff authorizes, from its three parts: a nonzero build, a 40-hex commit and a 64-hex digest, both canonicalized.
    pub(crate) fn from_parts(build: u64, commit: &str, dmg_sha256: &str) -> Result<Self, String> {
        if build == 0 {
            return Err("expected update build is malformed".to_string());
        }
        let commit = canonical_release_commit(Some(commit))
            .ok_or_else(|| "expected update commit is malformed".to_string())?;
        let dmg_sha256 = canonical_digest(dmg_sha256)
            .ok_or_else(|| "expected update digest is malformed".to_string())?;
        Ok(Self {
            build,
            commit,
            dmg_sha256,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReexecAuthority {
    Absent,
    Matched,
    Invalid,
}

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
    // Apply runs before any thread spawns. Read and clear are one critical section
    // (`aterm_log::env::take`), before any codesign/spctl helper can launch, so the
    // one-shot authority, even a malformed one, is observed once.
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
    let raw_build = aterm_log::env::take(EXPECTED_BUILD_ENV);
    let raw_commit = aterm_log::env::take(EXPECTED_COMMIT_ENV);
    let raw_digest = aterm_log::env::take(EXPECTED_DIGEST_ENV);
    match (raw_build, raw_commit, raw_digest) {
        (None, None, None) => Ok(None),
        (Some(build), Some(commit), Some(digest)) => {
            let build = build
                .to_str()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| "expected update build is malformed".to_string())?;
            let commit = commit
                .to_str()
                .ok_or_else(|| "expected update commit is malformed".to_string())?;
            let digest = digest
                .to_str()
                .ok_or_else(|| "expected update digest is malformed".to_string())?;
            ExpectedArtifact::from_parts(build, commit, digest).map(Some)
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

fn expected_artifact_refusal(ready: &Ready, expected: &ExpectedArtifact) -> Option<String> {
    (!ready_matches_expected(ready, expected)).then(|| {
        let refusal = format!(
            "staged update no longer matches authorized handoff artifact {} {} {}",
            expected.build, expected.commit, expected.dmg_sha256
        );
        if ready.build_number == expected.build {
            refusal
        } else {
            crate::verify::mark_passing(refusal)
        }
    })
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
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0 {
        // SAFETY: this branch owns the failed final-exec handoff duplicate.
        unsafe { libc::close(fd) };
    }
}

fn exec_preserving_handoff_fds(
    command: &mut aterm_uds::spawnfd::StdCommand,
    handoff_fds: &[i32],
) -> std::io::Error {
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

use crate::relaunch::window_successor_args;

fn boot_reexec_command(
    new_exe: &std::path::Path,
    args: impl Iterator<Item = std::ffi::OsString>,
    reexec_value: &str,
    updated_from: u64,
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> aterm_uds::spawnfd::StdCommand {
    #[expect(
        clippy::disallowed_types,
        reason = "the boot re-exec of this image keeps what this launch was handed (a lifeline fd, the handoff descriptors)"
    )]
    let mut reexec = std::process::Command::new(new_exe);
    reexec
        .args(window_successor_args(args))
        .env("ATERM_UPDATE_REEXEC", reexec_value)
        .env("ATERM_UPDATED_FROM", updated_from.to_string());
    for (key, value) in handoff_env {
        reexec.env(key, value);
    }
    reexec
}

fn boot_sentinel(staging: &Staging) -> Sentinel {
    Sentinel::new(staging.root.join("boot.sentinel"))
}

fn foreign_trial_counter(staging: &Staging) -> Sentinel {
    // Ordered: an older count after a power loss restarts the escape budget, which is
    // the conservative direction.
    Sentinel::ordered(foreign_trial_path(staging))
}

fn install_flush(staging: &Staging, installed: &Path) -> Flush {
    #[cfg(test)]
    if INSTALLED_ON_ANOTHER_DEVICE.with(std::cell::Cell::get) {
        return Flush::Durable;
    }
    Flush::ordered_if_same_device(&staging.root, installed)
}

#[cfg(test)]
thread_local! {
    static INSTALLED_ON_ANOTHER_DEVICE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn as_if_installed_on_another_device<R>(f: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INSTALLED_ON_ANOTHER_DEVICE.with(|flag| flag.set(false));
        }
    }
    INSTALLED_ON_ANOTHER_DEVICE.with(|flag| flag.set(true));
    let _reset = Reset;
    f()
}

#[cfg(test)]
fn on_and_off_the_install_device<T>(lane: impl Fn() -> T) -> [T; 2] {
    [lane(), as_if_installed_on_another_device(&lane)]
}

fn foreign_trial_path(staging: &Staging) -> PathBuf {
    staging.root.join("foreign-trial")
}

const FOREIGN_TRIAL_OWNER_IDLE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

const BOOT_LAUNCH_BURST_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

fn launch_is_burst(attempts: u32, sentinel_age: Option<std::time::Duration>) -> bool {
    attempts > 0 && sentinel_age.is_some_and(|age| age < BOOT_LAUNCH_BURST_WINDOW)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrialLauncher {
    pid: u32,
    started: (i64, i64),
}

impl TrialLauncher {
    fn of(pid: u32) -> Option<Self> {
        process_start(pid).map(|started| Self { pid, started })
    }

    fn this_process() -> Option<Self> {
        Self::of(std::process::id())
    }

    fn is_alive(&self) -> bool {
        Self::of(self.pid) == Some(*self)
    }

    fn has_ended(&self) -> bool {
        self.has_ended_with(Self::of, aterm_uds::holders::exe_of)
    }

    fn has_ended_with(
        &self,
        read: impl FnOnce(u32) -> Option<Self>,
        executable: impl FnOnce(u32) -> aterm_uds::holders::Exe,
    ) -> bool {
        match read(self.pid) {
            Some(observed) => observed != *self,
            None => matches!(executable(self.pid), aterm_uds::holders::Exe::Exited),
        }
    }

    const fn name(self) -> LaunchName {
        LaunchName {
            pid: self.pid,
            started: self.started,
        }
    }
}

fn process_start(pid: u32) -> Option<(i64, i64)> {
    const SZOMB: u32 = 5;
    let raw_pid = i32::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` points at `size` writable bytes of exactly the structure PROC_PIDTBSDINFO fills; libproc returns the number of bytes it wrote.
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

fn trial_launchers_path(staging: &Staging) -> PathBuf {
    staging.root.join("boot.sentinel.launchers")
}

const MAX_TRIAL_LAUNCHERS: usize = 32;

const HEADLESS_LAUNCHER_MARK: &str = "headless";

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

fn trial_window_launch_path(staging: &Staging) -> PathBuf {
    staging.root.join("boot.sentinel.windowed")
}

/// How long a trial a window has launched waits for a window to settle it (confirm at a first frame, or count launches that crash before one) before headless launches count and confirm it again (law L5: no wait without a deadline).
pub(crate) const WINDOW_TRIAL_HEADLESS_WAIT: std::time::Duration =
    std::time::Duration::from_secs(48 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowLaunch {
    None,
    Waiting { until: u64 },
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
    let armed = trial_armed_for(staging, build);
    let this_trial = crate::manifest::FailedMark::read(&staging.trial())
        .is_some_and(|trial| trial.matches(build, digest));
    if recorded != build || !armed || !this_trial {
        return WindowLaunch::None;
    }
    if now.abs_diff(at) < WINDOW_TRIAL_HEADLESS_WAIT.as_secs() {
        WindowLaunch::Waiting {
            until: at.saturating_add(WINDOW_TRIAL_HEADLESS_WAIT.as_secs()),
        }
    } else {
        WindowLaunch::Lapsed
    }
}

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

fn clear_trial_identity(staging: &Staging) {
    crate::manifest::FailedMark::clear(&staging.trial());
    let _ = std::fs::remove_file(trial_window_launch_path(staging));
}

fn window_trial_wait_notice(build: u64, until: u64) -> String {
    format!(
        "build {build} is on trial after its update and no window of it has confirmed it \
         yet: open aterm. After {}, a headless start confirms it instead",
        aterm_types::rfc3339::format_rfc3339(until)
    )
}

/// The trial of `build` waiting for a window, as `update status` says it ([`window_trial_wait_notice`]), or `None` when it is not waiting.
pub(crate) fn window_trial_wait_at(staging: &Staging, build: u64, now: u64) -> Option<String> {
    // Armed as the confirmation reads it ([`trial_armed_for`]): a trial whose
    // sentinel does not read but whose identity names the build is retried, so its
    // refusal is said too.
    if trial_armed_for(staging, build) {
        let ledger = crate::health::Health::read(&staging.health());
        if ledger.trial_proof_failure_build == build && !ledger.trial_proof_failure.is_empty() {
            return Some(format!(
                "build {build} is on trial after its update; the last confirmation was \
                 refused (first at {}): {}; a window of this build retries it",
                ledger.trial_proof_failure_at, ledger.trial_proof_failure
            ));
        }
    }
    match trial_window_launch(staging, build, now) {
        WindowLaunch::Waiting { until } => Some(window_trial_wait_notice(build, until)),
        WindowLaunch::None | WindowLaunch::Lapsed => None,
    }
}

fn window_unproven_path(staging: &Staging) -> PathBuf {
    staging.root.join("boot.sentinel.window-unproven")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WindowUnproven {
    build: u64,
    digest: String,
    at: u64,
    install_root: PathBuf,
}

impl WindowUnproven {
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

    const fn lapsed(&self, now: u64) -> bool {
        now.abs_diff(self.at) >= WINDOW_TRIAL_HEADLESS_WAIT.as_secs()
    }
}

fn clear_window_unproven(staging: &Staging) {
    let _ = std::fs::remove_file(window_unproven_path(staging));
}

fn window_unproven_names(staging: &Staging, build: u64) -> bool {
    WindowUnproven::read(staging).is_some_and(|record| record.build == build)
}

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
    let _ = std::fs::remove_file(trial_launchers_path(staging));
    let _ = std::fs::remove_file(trial_window_launch_path(staging));
    if let Err(error) = sentinel.arm_with(build, install_flush(staging, app_root)) {
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

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "Boots",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "SiblingBoots",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
    let sentinel_age = mtime(&staging.root.join("boot.sentinel")).and_then(|t| t.elapsed().ok());
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

fn overlap_exempts(target: crate::TrialWitness, live: crate::TrialWitness) -> bool {
    target == crate::TrialWitness::Headless || live == crate::TrialWitness::Window
}

fn prepare_trial(
    staging: &Staging,
    ready: &Ready,
    install_root: &Path,
) -> Result<Sentinel, String> {
    let sentinel = boot_sentinel(staging);
    if let Err(error) = crate::manifest::FailedMark::record_required(
        &staging.trial(),
        ready.build_number,
        &ready.dmg_sha256,
        Some(install_root),
    ) {
        return Err(format!("persist trial identity: {error}"));
    }
    if let Err(error) = sentinel.arm_with(ready.build_number, install_flush(staging, install_root))
    {
        clear_trial_identity(staging);
        return Err(format!("arm boot-health trial: {error}"));
    }
    let _ = foreign_trial_counter(staging).confirm();
    let _ = std::fs::remove_file(trial_launchers_path(staging));
    let _ = std::fs::remove_file(trial_window_launch_path(staging));
    let _ = remove_path_no_follow(&owed_give_backs_dir(staging));
    clear_window_unproven(staging);
    Ok(sentinel)
}

#[must_use]
fn trial_owned_by(staging: &Staging, app_root: &Path) -> bool {
    match crate::manifest::FailedMark::read(&staging.trial()).and_then(|trial| trial.install_root) {
        None => true,
        Some(recorded) => {
            let recorded = Path::new(&recorded);
            match std::fs::symlink_metadata(recorded) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => false,
                Ok(_) => same_install_root(recorded, app_root),
            }
        }
    }
}

/// Path equality for install roots, tolerant of a canonicalizable spelling: the one "is this the same copy of aterm.app?" test, shared by the trial-ownership rule and `which_copy`, so a symlinked spelling reads the same on both.
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
const fn abandoned_preswap_trial(
    process_build: u64,
    installed_build: u64,
    armed_build: u64,
) -> bool {
    installed_build == process_build && armed_build > process_build
}

#[must_use]
const fn dead_authority_trial(process_build: u64, installed_build: u64, armed_build: u64) -> bool {
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
    if dead_authority_trial(process_build, installed_build, armed_build)
        && trial_owned_by(staging, installed)
        && identity_matches_running(
            installed_build,
            &installed_commit,
            process_build,
            process_commit,
        )
    {
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

fn escape_wedged_foreign_trial(staging: &Staging, current_build: u64, armed_build: u64) -> bool {
    let counter = foreign_trial_counter(staging);
    let sentinel = boot_sentinel(staging);
    let counter_state = counter.read_state();
    let advanced = mtime(&staging.root.join("boot.sentinel"))
        .zip(mtime(&foreign_trial_path(staging)))
        .is_some_and(|(trial, ours)| trial > ours);
    if advanced {
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
    let owned_elsewhere = crate::manifest::FailedMark::read(&staging.trial())
        .and_then(|trial| trial.install_root)
        .is_some_and(|root| {
            let root = Path::new(&root);
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
    UnprovenCandidate::check(staging, &fixed, unix_now_secs())?;
    let recoverable = verified_bundle_identity(&fixed).is_ok_and(|(build, commit)| {
        same_volume(&fixed, installed)
            && build > current_build
            && ready_matches_verified_identity(ready, build, &commit)
    });
    if !recoverable {
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
        crate::warn(&format!(
            "cannot restore the orphaned fixed candidate to the staging path ({error}); \
             retiring the dangling marker so the next check re-stages"
        ));
        staging.retire_published();
    }
    Ok(())
}

#[derive(Debug)]
struct PreparedSwapCandidate {
    fixed: PathBuf,
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

struct VerifiedSwap {
    prepared: PreparedSwapCandidate,
    old: (u64, String),
}

fn verify_stage_and_prepare(
    staging: &Staging,
    installed: &Path,
    ready: &Ready,
    (current_build, current_commit): (u64, Option<&str>),
    one: impl Fn(&Path) -> Result<(u64, String), String>,
    both: impl FnOnce([&Path; 2]) -> [Result<(u64, String), String>; 2],
) -> Result<VerifiedSwap, ApplyOutcome> {
    let [staged_identity, installed_identity] = both([&staging.staged_app, installed]);
    let (staged_build, sealed_commit) = match staged_identity {
        Ok(identity) => identity,
        Err(error) if !stage_failure_is_a_verdict(&error) => {
            crate::warn(&format!(
                "staged bundle re-verification did not finish: {error}; keeping the stage \
                 for the next attempt"
            ));
            crate::status::record(
                staging,
                current_build,
                "deferred: staged bundle re-verification did not finish (kept)",
            );
            return Err(ApplyOutcome::Deferred(format!("re-verify: {error}")));
        }
        Err(error) => {
            crate::warn(&format!(
                "staged bundle re-verification failed: {error}; discarding"
            ));
            staging.retire_published();
            crate::status::record(
                staging,
                current_build,
                "deferred: staged bundle failed re-verification (discarded)",
            );
            return Err(ApplyOutcome::Deferred(format!("re-verify: {error}")));
        }
    };
    if staged_build != ready.build_number || staged_build <= current_build {
        crate::warn(&format!(
            "staged bundle CFBundleVersion {staged_build} != marker build {} or not newer than \
             running {current_build}; discarding",
            ready.build_number
        ));
        staging.retire_published();
        crate::status::record(
            staging,
            current_build,
            "deferred: staged bundle build-number rebind mismatch (discarded)",
        );
        return Err(ApplyOutcome::Deferred(format!(
            "build-number rebind: sealed {staged_build} vs marker {}",
            ready.build_number
        )));
    }

    if !sealed_commit_matches(ready.commit.as_deref(), &sealed_commit) {
        crate::warn("staged bundle commit does not match ready marker; discarding");
        staging.retire_published();
        crate::status::record(
            staging,
            current_build,
            "deferred: staged bundle commit rebind mismatch (discarded)",
        );
        return Err(ApplyOutcome::Deferred("commit rebind mismatch".to_string()));
    }

    let judge_old = |identity: Result<(u64, String), String>| match identity {
        Ok((build, commit))
            if identity_matches_running(build, &commit, current_build, current_commit) =>
        {
            Ok((build, commit))
        }
        Ok((build, commit)) => Err(ApplyOutcome::Deferred(format!(
            "current installed rollback source {build}/{commit} != running {current_build}/{:?}",
            current_commit
        ))),
        Err(error) => Err(ApplyOutcome::Deferred(format!(
            "current installed rollback source is not verified: {}",
            installed_source_refusal(installed, &error)
        ))),
    };
    let old = judge_old(installed_identity)?;
    finish_swap_preparation(
        old,
        || {
            prepare_fixed_swap_candidate(
                staging,
                installed,
                ready,
                current_build,
                (staged_build, &sealed_commit),
            )
        },
        || judge_old(one(installed)),
        |prepared| recover_prepared_candidate(prepared, staging),
    )
}

fn finish_swap_preparation(
    old: (u64, String),
    prepare: impl FnOnce() -> Result<PreparedSwapCandidate, String>,
    recheck: impl FnOnce() -> Result<(u64, String), ApplyOutcome>,
    recover: impl FnOnce(&PreparedSwapCandidate),
) -> Result<VerifiedSwap, ApplyOutcome> {
    let prepared = prepare().map_err(ApplyOutcome::Deferred)?;
    if prepared.moved_from_stage {
        return Ok(VerifiedSwap { prepared, old });
    }
    match recheck() {
        Ok(old) => Ok(VerifiedSwap { prepared, old }),
        Err(refusal) => {
            recover(&prepared);
            Err(refusal)
        }
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
    UnprovenCandidate::check(staging, &fixed, unix_now_secs())?;
    clear_kept_predecessor(staging);
    remove_path_no_follow(&fixed)
        .map_err(|error| format!("remove stale fixed rollback: {error}"))?;

    let moved_from_stage = same_volume(staged, installed);
    if moved_from_stage {
        std::fs::rename(staged, &fixed)
            .map_err(|error| format!("move verified stage to fixed swap path: {error}"))?;
    } else {
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
    flush: Flush,
) -> Result<PathBuf, String> {
    checked_bundle_exchange(&prepared.fixed, installed, "atomic fixed-path swap", flush)?;
    Ok(prepared.fixed.clone())
}

fn checked_bundle_exchange(
    a: &Path,
    b: &Path,
    operation: &str,
    flush: Flush,
) -> Result<(), String> {
    if !is_non_symlink_dir(a) || !is_non_symlink_dir(b) {
        return Err(format!(
            "{operation}: bundle changed into a symlink/non-directory"
        ));
    }
    if !same_volume(a, b) {
        return Err(format!("{operation}: bundles are not on one volume"));
    }
    rename_swap(a, b).map_err(|error| format!("{operation} failed: {error}"))?;
    sync_swap_parents(a, b, flush);
    Ok(())
}

fn sync_swap_parents(a: &Path, b: &Path, flush: Flush) {
    let sync = |dir: &Path| {
        let _ = crate::durable::sync_dir(dir, flush);
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

/// Reads a bundle's sealed identity (build + commit), refusing anything that is not a plain directory with an intact, policy-satisfying signature.
pub(crate) fn verified_bundle_identity_at(app: &Path) -> Result<(u64, String), String> {
    verified_bundle_identity(app)
}

fn verified_bundle_identity(app: &Path) -> Result<(u64, String), String> {
    let [identity] = verified_bundle_identities([app]);
    identity
}

fn verified_bundle_identities<const N: usize>(
    apps: [&Path; N],
) -> [Result<(u64, String), String>; N] {
    let shapes = apps.map(|app| {
        let metadata =
            std::fs::symlink_metadata(app).map_err(|error| format!("bundle metadata: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("bundle path is not a non-symlink directory".to_string());
        }
        Ok(())
    });
    let plain: Vec<&Path> = apps
        .iter()
        .zip(&shapes)
        .filter_map(|(app, shape)| shape.is_ok().then_some(*app))
        .collect();
    let mut checks = verify::check_bundles(&plain, crate::effective_team_id(), false).into_iter();
    shapes.map(|shape| {
        shape?;
        identity_from_checks(checks.next().ok_or("the bundle check did not run")?)
    })
}

fn identity_from_checks(checks: verify::BundleChecks) -> Result<(u64, String), String> {
    checks
        .policy
        .map_err(|error| format!("bundle policy: {error}"))?;
    let build = checks
        .build
        .map_err(|error| format!("sealed build: {error}"))?;
    let commit = checks
        .commit
        .map_err(|error| format!("sealed commit: {error}"))?;
    let clean_commit = commit.trim();
    if !(7..=40).contains(&clean_commit.len())
        || !clean_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("sealed commit is not clean hexadecimal provenance".to_string());
    }
    Ok((build, clean_commit.to_ascii_lowercase()))
}

#[derive(Debug)]
struct VerifiedRollback {
    path: PathBuf,
    build: u64,
}

#[derive(Debug)]
enum Predecessor {
    Revertible(VerifiedRollback),
    Unavailable(String),
    Unproven(String),
}

fn fixed_predecessor(
    installed: &Path,
    current_build: u64,
    identity: &IdentityReader,
) -> Predecessor {
    let fixed = rollback_path(installed);
    let metadata = match std::fs::symlink_metadata(&fixed) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Predecessor::Unavailable("fixed rollback missing".to_string());
        }
        Err(error) => return Predecessor::Unproven(format!("inspect fixed rollback: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Predecessor::Unavailable(
            "fixed rollback is invalid: bundle path is not a non-symlink directory".to_string(),
        );
    }
    if !same_volume(&fixed, installed) {
        return Predecessor::Unproven(
            "fixed rollback is invalid: rollback is not on the installed volume".to_string(),
        );
    }
    match identity(&fixed) {
        Ok((build, _)) if build != 0 && build < current_build => {
            Predecessor::Revertible(VerifiedRollback { path: fixed, build })
        }
        Ok((build, _)) => Predecessor::Unproven(format!(
            "fixed rollback is invalid: rollback sealed build {build} is not a strict \
             predecessor of {current_build}"
        )),
        Err(error) if stage_failure_is_a_verdict(&error) => {
            Predecessor::Unavailable(format!("fixed rollback is invalid: {error}"))
        }
        Err(error) => Predecessor::Unproven(format!("fixed rollback is invalid: {error}")),
    }
}

fn ensure_fixed_rollback(installed: &Path, current_build: u64) -> Result<VerifiedRollback, String> {
    match fixed_predecessor(installed, current_build, &verified_bundle_identity) {
        Predecessor::Revertible(rollback) => Ok(rollback),
        Predecessor::Unavailable(error) | Predecessor::Unproven(error) => Err(error),
    }
}

fn swap_in_and_record_receipt(
    staging: &Staging,
    prepared: &PreparedSwapCandidate,
    installed: &Path,
    ready: &Ready,
    sentinel: &Sentinel,
) -> Result<PathBuf, ApplyOutcome> {
    let flush = install_flush(staging, installed);
    let retained = match swap_fixed_candidate(prepared, installed, flush) {
        Ok(rollback) => rollback,
        Err(error) => {
            if let Err(disarm_error) = sentinel.confirm() {
                return Err(ApplyOutcome::Deferred(format!(
                    "{error}; trial disarm failed: {disarm_error}"
                )));
            }
            clear_trial_identity(staging);
            recover_prepared_candidate(prepared, staging);
            return Err(ApplyOutcome::Deferred(error));
        }
    };
    let ready_commit = ready
        .commit
        .as_deref()
        .expect("read_ready rejected a missing commit");
    if let Err(error) = crate::manifest::InstalledReceipt::record(
        &staging.installed_receipt(),
        ready.build_number,
        ready_commit,
        &ready.dmg_sha256,
        flush,
    ) {
        return Err(roll_back_unreceipted_swap(
            prepared, &retained, installed, sentinel, staging, &error,
        ));
    }
    Ok(retained)
}

fn unswap_after_failed_exec(
    staging: &Staging,
    retained: &Path,
    installed: &Path,
    sentinel: &Sentinel,
    previous_receipt: Option<&crate::manifest::InstalledReceipt>,
    exec_error: &std::io::Error,
) -> Result<Option<String>, ApplyOutcome> {
    let flush = install_flush(staging, installed);
    if let Err(error) = restore_rollback(retained, installed, flush) {
        crate::warn(&format!(
            "rollback failed: {error}; preserving sentinel, trial, receipt, and fixed rollback"
        ));
        return Err(ApplyOutcome::ReExecFailed(format!(
            "{exec_error}; rollback remains pending: {error}"
        )));
    }
    if let Err(error) = sentinel.confirm() {
        return Err(ApplyOutcome::ReExecFailed(format!(
            "{exec_error}; OLD restored but trial disarm failed: {error}"
        )));
    }
    let _ = std::fs::remove_dir_all(retained);
    Ok(restore_installed_receipt(staging, previous_receipt, flush).err())
}

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
    if let Err(rollback_error) =
        restore_rollback(retained, installed, install_flush(staging, installed))
    {
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
    flush: Flush,
) -> Result<(), String> {
    let path = staging.installed_receipt();
    let restored = if let Some(previous) = previous {
        previous.record_preserving_kind(&path, flush)
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("clear superseded installed receipt: {error}")),
        }
    };
    if let Err(error) = restored {
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

#[derive(Debug)]
enum TrialRevert {
    To(VerifiedRollback),
    Unavailable(String),
}

fn ensure_current_trial_receipt(
    staging: &Staging,
    installed: &Path,
    current_build: u64,
    current_commit: Option<&str>,
) -> Result<TrialRevert, String> {
    prove_current_trial(
        staging,
        installed,
        current_build,
        current_commit,
        &verified_bundle_identity,
    )
}

fn prove_current_trial(
    staging: &Staging,
    installed: &Path,
    current_build: u64,
    current_commit: Option<&str>,
    identity: &IdentityReader,
) -> Result<TrialRevert, String> {
    let revert = match fixed_predecessor(installed, current_build, identity) {
        Predecessor::Revertible(rollback) => TrialRevert::To(rollback),
        Predecessor::Unavailable(why) => TrialRevert::Unavailable(why),
        Predecessor::Unproven(error) => return Err(error),
    };
    let (sealed_build, sealed_commit) =
        identity(installed).map_err(|error| format!("installed trial is not verified: {error}"))?;
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
        return Ok(revert);
    }

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
            install_flush(staging, installed),
        )?;
        return Ok(revert);
    }

    Err("installed trial has no exact receipt or authorized recovery record".to_string())
}

struct Mounted {
    mountpoint: PathBuf,
}

impl Mounted {
    fn attach(dmg: &Path, mountpoint: &Path) -> Result<Self, String> {
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
        match Self::attach_at(dmg, None) {
            Ok(mounted) => Ok(mounted),
            Err(error) => {
                attempts.push(error);
                let _ = std::fs::remove_dir_all(mountpoint);
                let size = std::fs::metadata(dmg).map_or_else(
                    |_| "unreadable".to_string(),
                    |meta| format!("{} bytes", meta.len()),
                );
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
            None => {
                let parsed = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(|line| line.split('\t').nth(2))
                    .map(str::trim)
                    .find(|candidate| !candidate.is_empty())
                    .map(PathBuf::from);
                match parsed {
                    Some(found) => Ok(Self { mountpoint: found }),
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
        let _ = std::fs::remove_dir_all(&self.mountpoint);
    }
}

const HDIUTIL_ATTACH_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

const HDIUTIL_DETACH_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

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

/// Pre-park handoff verification: the full staged-bundle authenticity gate (the tiered codesign policy plus the sealed CFBundleVersion/commit rebinding the swap re-runs), run while every parent PTY reader is still live, so a failing candidate is refused before a reader stops.
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

/// The key marking the refusal of an installed bundle that cannot become the rollback source a swap installs at the fixed path (a corrupt or replaced install must never become crash-recovery authority), e.g. a local ad-hoc build swapped into `/Applications` without the `ATermDevBuild` mark.
pub const ROLLBACK_SOURCE_REFUSAL_KEY: &str = "cannot be the rollback source";

/// The one sentence that names the way out of an unverifiable install.
pub const ROLLBACK_SOURCE_REMEDY: &str = "this install cannot update itself: reinstall the \
     signed release (tools/install.sh, or drag it from the release DMG) — or, for a local \
     build, build it with `targo --unverified ship dev-app`, whose ATermDevBuild mark the \
     updater leaves alone";

/// The refusal both apply lanes mint when the installed bundle cannot be the swap's rollback source, with the remedy attached.
pub fn rollback_source_refusal(installed: &Path, error: &str) -> String {
    format!(
        "the installed bundle at {} {ROLLBACK_SOURCE_REFUSAL_KEY} the swap installs: {error}; \
         {ROLLBACK_SOURCE_REMEDY}",
        installed.display()
    )
}

/// True when a refusal string is the installed-bundle one — permanent for this process until a person changes the bundle, never a retry candidate.
pub fn is_rollback_source_refusal(reason: &str) -> bool {
    reason.contains(ROLLBACK_SOURCE_REFUSAL_KEY)
}

/// The one sentence that names the way out of a non-writable install location ([`crate::INSTALL_LOCATION_REFUSAL_KEY`]).
pub(crate) const INSTALL_LOCATION_REMEDY: &str = "this install cannot update itself: move aterm.app to \
     a folder you can write to (such as ~/Applications), or have an administrator install \
     the update";

/// The refusal for an install whose parent folder this user cannot write (an admin-installed `/Applications` run by a standard user, or an MDM-managed one).
pub(crate) fn install_location_refusal(app_root: &Path) -> String {
    format!(
        "{}: {}; {INSTALL_LOCATION_REMEDY}",
        crate::INSTALL_LOCATION_REFUSAL_KEY,
        app_root.display()
    )
}

/// The swap's write, checked on every lane that needs it (the pre-park check, the boot apply's step 5, the activation supersede): `Ok` when the parent folder took a probe entry.
pub(crate) fn install_location_check(app_root: &Path) -> Result<(), String> {
    install_location_verdict(app_root, bundle::probe_parent_write(app_root))
}

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

fn stage_failure_is_a_verdict(error: &str) -> bool {
    !crate::is_passing_refusal(error)
}

fn probe_left_unproven(error: &str) -> bool {
    !stage_failure_is_a_verdict(error) && !error.starts_with("spawn ")
}

fn unproven_candidate_path(staging: &Staging) -> PathBuf {
    staging.root.join("fixed.unproven")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UnprovenCandidate {
    bundle: PathBuf,
    killed_at: u64,
}

impl UnprovenCandidate {
    fn check(staging: &Staging, fixed: &Path, now: u64) -> Result<(), String> {
        let Some(left) = Self::holds(staging, fixed, now) else {
            return Ok(());
        };
        Err(verify::mark_passing(format!(
            "the candidate at {} was started by a start probe killed before it exited, and \
             macOS may still be assessing its first exec; it stays where it is for {left} s \
             more",
            fixed.display()
        )))
    }

    fn read(staging: &Staging) -> Option<Self> {
        Self::parse(&crate::read_ledger_text(&unproven_candidate_path(staging))?)
    }

    fn parse(text: &str) -> Option<Self> {
        use std::os::unix::ffi::OsStringExt;

        let mut lines = text.lines();
        let mut fields = lines.next()?.split_whitespace();
        if fields.next()? != "v1" {
            return None;
        }
        let killed_at = fields.next()?.parse().ok()?;
        let bytes = aterm_codec::hex::decode_vec(lines.next()?.as_bytes())?;
        if fields.next().is_some() || lines.next().is_some() || bytes.contains(&0) {
            return None;
        }
        let bundle = PathBuf::from(std::ffi::OsString::from_vec(bytes));
        bundle.is_absolute().then_some(Self { bundle, killed_at })
    }

    fn encode(&self) -> String {
        use std::os::unix::ffi::OsStrExt;

        format!(
            "v1 {}\n{}\n",
            self.killed_at,
            aterm_codec::hex::encode_lower(self.bundle.as_os_str().as_bytes())
        )
    }

    fn record(&self, staging: &Staging) -> std::io::Result<()> {
        write_private_file(&unproven_candidate_path(staging), self.encode().as_bytes())
    }

    fn holds(staging: &Staging, fixed: &Path, now: u64) -> Option<u64> {
        let hang = aterm_uds::deadline::FIRST_EXEC_HANG.as_secs();
        let Some(record) = Self::read(staging) else {
            let _ = std::fs::remove_file(unproven_candidate_path(staging));
            return None;
        };
        let since = now.saturating_sub(record.killed_at);
        if since >= hang {
            let _ = std::fs::remove_file(unproven_candidate_path(staging));
            return None;
        }
        (record.bundle == fixed).then(|| hang - since)
    }
}

fn keep_unproven_candidate(prepared: &PreparedSwapCandidate, staging: &Staging, now: u64) {
    let kept = UnprovenCandidate {
        bundle: prepared.fixed.clone(),
        killed_at: now,
    };
    if let Err(error) = kept.record(staging) {
        crate::warn(&format!(
            "could not record the unproven candidate at {}: {error}",
            prepared.fixed.display()
        ));
    }
    if prepared.moved_from_stage {
        staging.retire_published();
    }
    crate::warn(&format!(
        "the start probe of {} was killed before it exited; it stays there while macOS may \
         still be assessing its first exec",
        prepared.fixed.display()
    ));
}

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

type IdentityReader = dyn Fn(&Path) -> Result<(u64, String), String>;

fn preverify_installed_rollback_source_with(
    installed: Option<&Path>,
    current_build: u64,
    current_commit: Option<&str>,
    verify: &IdentityReader,
) -> Result<(), String> {
    let Some(installed) = installed else {
        // A handoff whose successor's swap is a no-op can only end as a wrong-build
        // refusal, so say so now.
        return Err(
            "this process does not run from an installed bundle the updater may replace, so a \
             staged build cannot become its successor"
                .to_string(),
        );
    };
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

/// What a fresh cached pass cannot vouch for when the candidate is the installed activation `build`: the operator floor ([`handoff_apply_floor_refusal_at`]), and that the install path still holds it (another window's retire, `supersede_installed_activation`, may have put the running build back since).
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

/// The operator apply floor, as a handoff to `build` must respect it: `Some(refusal)` when `build` is below the floor (a yank the swap's gate 4b would answer by retiring the stage and leaving the successor the old build), or when the staged marker for `build` was signed by a machine a roster has since revoked.
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

/// What a cached pass cannot vouch for, asked by a worker that skips the staged candidate's full pre-verification on it: `Some(refusal)` unless the stage the pass was cached for — `build` at `commit` — is still the published one, not quarantined, and admitted by the floor ([`handoff_floor_refusal`], for the signer of the marker it read).
pub(crate) fn handoff_cached_pass_refusal_at(
    staging: &Staging,
    build: u64,
    commit: &str,
) -> Option<String> {
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
    match crate::manifest::artifact_quarantined_for_gate(staging, build, &ready.dmg_sha256) {
        Err(refusal) => return Some(refusal),
        Ok(true) => {
            return Some(format!(
                "staged build {build} is quarantined: it crash-looped on this machine and was \
                 reverted"
            ));
        }
        Ok(false) => {}
    }
    if let Some(refusal) = handoff_floor_refusal(staging, build, ready.machine_id.as_deref()) {
        return Some(refusal);
    }
    if let Some(refusal) = ready.roster_authorization_stale(
        unix_now_secs(),
        aterm_update_core::pins::roster_tier_armed(),
    ) {
        return Some(refusal);
    }
    (!ready.names_published_bundle(staging)).then(|| {
        stage_moment(format!(
            "the staged bundle of build {build} is no longer the published stage; not handing \
             off to it"
        ))
    })
}

/// What a handoff pre-verification holds while it runs: the apply lock (when there is a staging root to take it in) and, under it, the verification budget.
pub(crate) struct PreverifyGuard {
    _budget: verify::ApplyBudget,
    _lock: Option<FileLock>,
}

/// Enters a handoff pre-verification: takes `apply_lock` within [`APPLY_LOCK_WAIT`], then opens [`verify::APPLY_BUDGET`] for everything the check runs.
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

fn preverify_staged_handoff_candidate_at<R>(
    staging: &Staging,
    installed: Option<&Path>,
    current_build: u64,
    current_commit: Option<&str>,
    expected_build: Option<u64>,
    expected_commit: Option<&str>,
    read_policy: &dyn Fn(&Path) -> R,
) -> Result<R, String> {
    let _preverify = enter_preverify(Some(&staging.apply_lock))?;
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
    if crate::manifest::artifact_quarantined_for_gate(
        staging,
        ready.build_number,
        &ready.dmg_sha256,
    )? {
        return Err(format!(
            "staged build {} is quarantined: it crash-looped on this machine and was reverted",
            ready.build_number
        ));
    }
    if let Some(refusal) = handoff_apply_floor_refusal_at(staging, ready.build_number) {
        return Err(refusal);
    }
    if let Some(refusal) = ready.roster_authorization_stale(
        unix_now_secs(),
        aterm_update_core::pins::roster_tier_armed(),
    ) {
        return Err(refusal);
    }
    if matches!(
        std::fs::symlink_metadata(&staging.staged_app),
        Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        return Err(stage_moment(format!(
            "the staged bundle of build {} is no longer the published stage; not handing off \
             to it",
            ready.build_number
        )));
    }
    let (staged_build, sealed_commit) = verified_bundle_identity(&staging.staged_app)?;
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
    preverify_installed_rollback_source(installed, current_build, current_commit)?;
    Ok(read_policy(&staging.staged_app))
}

const MAX_READY_CHANGELOG_CHARS: usize = 4096;

const READY_CHANGELOG_TRUNCATED: &str = "\n… (changelog truncated)";

fn clamp_ready_changelog(changelog: Option<&str>) -> Option<String> {
    let text = changelog?;
    match text.char_indices().nth(MAX_READY_CHANGELOG_CHARS) {
        None => Some(text.to_string()),
        Some((cut, _)) => Some(format!("{}{READY_CHANGELOG_TRUNCATED}", &text[..cut])),
    }
}

fn publish_verified_stage(staging: &Staging, incoming: &Path, ready: &Ready) -> Result<(), String> {
    publish_verified_stage_within(staging, incoming, ready, BACKGROUND_LOCK_WAIT)
}

fn publish_verified_stage_within(
    staging: &Staging,
    incoming: &Path,
    ready: &Ready,
    lock_wait: std::time::Duration,
) -> Result<(), String> {
    let marker = ready.to_toml()?;
    if u64::try_from(marker.len()).unwrap_or(u64::MAX) > crate::MAX_LEDGER_BYTES {
        return Err(format!(
            "refusing to commit a {}-byte ready marker: the ledger cap every reader \
             enforces is {} bytes, and a marker above it reads as absent",
            marker.len(),
            crate::MAX_LEDGER_BYTES
        ));
    }
    let _publish_lock = FileLock::acquire_within(&staging.apply_lock, lock_wait)
        .map_err(|error| verify::lock_wait_refusal("publish lock", &error))?;
    stage_withdrawn(staging, ready)?;

    let _ = std::fs::remove_file(&staging.ready);
    let _ = std::fs::remove_dir_all(&staging.staged_app);
    std::fs::rename(incoming, &staging.staged_app)
        .map_err(|error| format!("publish staged bundle: {error}"))?;

    crate::manifest::write_durable(&staging.ready, &marker, "ready marker", Flush::Ordered)
}

fn stage_withdrawn(staging: &Staging, ready: &Ready) -> Result<(), String> {
    if crate::manifest::artifact_quarantined_for_gate(
        staging,
        ready.build_number,
        &ready.dmg_sha256,
    )
    .map_err(|refusal| format!("stage refused: {refusal}"))?
    {
        return Err(format!(
            "stage refused: build {} ({}) is quarantined — it crash-looped on this machine \
             and was reverted while this check was downloading it",
            ready.build_number, ready.dmg_sha256
        ));
    }
    if let Some(why) = crate::manifest::Floor::read_for_gate(&staging.floor())
        .map_err(|refusal| format!("stage refused: {refusal}"))?
        .withdraws(ready.build_number, ready.machine_id.as_deref())
    {
        return Err(format!(
            "stage refused: {why} — withdrawn while this check was downloading it"
        ));
    }
    Ok(())
}

struct StageIncoming {
    path: PathBuf,
}

impl StageIncoming {
    fn create(staging: &Staging) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt;
        let nonce = random_nonce()
            .ok_or_else(|| verify::mark_passing("incoming bundle nonce unavailable".to_string()))?;
        let path = staging
            .staged_dir()
            .join(format!(".aterm-incoming-{nonce}.app"));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|error| verify::mark_passing(format!("reserve incoming bundle: {error}")))?;
        Ok(Self { path })
    }
}

impl Drop for StageIncoming {
    fn drop(&mut self) {
        // Only an EMPTY reservation can go. A retained candidate keeps its exact
        // path even after this stage returns; there is no assessment-end receipt.
        let _ = std::fs::remove_dir(&self.path);
    }
}

/// Stages a verified copy of the bundle from a downloaded (SHA-256-checked) DMG: mount, `ditto` the `.app` out, verify it (codesign/Team ID/spctl), publish `staged/aterm.app`, and write `ready.toml` last, atomically, so a reader never sees a half-staged bundle.
pub fn stage_from_dmg(
    staging: &Staging,
    dmg: &Path,
    manifest: &Manifest,
    expected_team: &str,
    roster_read_at: &str,
) -> Result<(), String> {
    let manifest_commit = staging_manifest_commit(manifest)?;
    ensure_private_dir(&staging.staged_dir()).map_err(|e| format!("staged dir: {e}"))?;
    sweep_stale_scratch(staging);
    let mountpoint = staging.root.join(format!("mnt-{}", std::process::id()));
    let mounted = Mounted::attach(dmg, &mountpoint)?;
    let src = mounted.mountpoint.join("aterm.app");
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

    let incoming_attempt = StageIncoming::create(staging)?;
    let incoming = incoming_attempt.path.clone();
    refuse_without_room(&staging.staged_dir(), tree_bytes(&src), "the update bundle")?;
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

use std::io::{BufReader, Read, Seek, SeekFrom};

const MAX_EXTRACT_BYTES: u64 = aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND;

const MAX_EXTRACT_ENTRIES: u64 = 50_000;

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const CENTRAL_HEADER_SIGNATURE: u32 = 0x0201_4b50;

const EOCD_FIXED_LEN: u64 = 22;
const MAX_ZIP_COMMENT: u64 = 65_535;
const ZIP64_LOCATOR_LEN: u64 = 20;
const ZIP64_EOCD_LEN: u64 = 56;
const CENTRAL_HEADER_FIXED_LEN: usize = 46;
const ZIP64_EXTRA_ID: u16 = 0x0001;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ZipExtractClaim {
    entries: u64,
    uncompressed_bytes: u64,
}

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

fn find_end_of_central_directory(
    file: &mut std::fs::File,
    file_len: u64,
) -> Result<(u64, DirectoryExtent), String> {
    let window = file_len.min(EOCD_FIXED_LEN.saturating_add(MAX_ZIP_COMMENT));
    let start = file_len - window;
    let tail = read_exact_at(file, start, window)?;
    let fixed = usize::try_from(EOCD_FIXED_LEN)
        .map_err(|_| "update zip end record length does not fit in memory".to_string())?;
    let Some(highest) = tail.len().checked_sub(fixed) else {
        return Err("update zip is smaller than an end-of-central-directory record".to_string());
    };
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
        at = data_end;
    }
    None
}

fn central_directory_claim(
    file: &mut std::fs::File,
    extent: DirectoryExtent,
) -> Result<ZipExtractClaim, String> {
    file.seek(SeekFrom::Start(extent.offset))
        .map_err(|error| format!("seek update zip central directory: {error}"))?;
    let mut reader = BufReader::new(file);
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

fn checked_zip_extraction_claim(zip: &Path) -> Result<ZipExtractClaim, String> {
    let mut file = std::fs::File::open(zip).map_err(|error| format!("open update zip: {error}"))?;
    let file_len = file
        .metadata()
        .map_err(|error| format!("stat update zip: {error}"))?
        .len();
    let (eocd_offset, classic) = find_end_of_central_directory(&mut file, file_len)?;
    let extent = match zip64_directory_extent(&mut file, eocd_offset)? {
        Some(zip64) => {
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

/// Stages a verified copy of the bundle from a downloaded (SHA-256-checked) ZIP: `ditto -x -k` extract, then the same verification and publication as [`stage_from_dmg`] ([`verify_and_publish_incoming`]).
pub fn stage_from_zip(
    staging: &Staging,
    zip: &Path,
    manifest: &Manifest,
    expected_team: &str,
    roster_read_at: &str,
) -> Result<(), String> {
    let manifest_commit = staging_manifest_commit(manifest)?;
    let claim = checked_zip_extraction_claim(zip)?;
    crate::log(&format!(
        "update zip declares {} entries / {} uncompressed bytes \
         (caps {MAX_EXTRACT_ENTRIES} / {MAX_EXTRACT_BYTES})",
        claim.entries, claim.uncompressed_bytes
    ));
    ensure_private_dir(&staging.staged_dir()).map_err(|e| format!("staged dir: {e}"))?;
    sweep_stale_scratch(staging);
    let extract = staging
        .staged_dir()
        .join(format!("zx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&extract);
    refuse_without_room(
        &staging.staged_dir(),
        claim.uncompressed_bytes,
        "the update zip",
    )?;
    std::fs::create_dir_all(&extract).map_err(|e| format!("create zip extract dir: {e}"))?;
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

    let incoming_attempt = StageIncoming::create(staging).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&extract);
    })?;
    let incoming = incoming_attempt.path.clone();
    if let Err(error) = std::fs::rename(&src, &incoming) {
        let _ = std::fs::remove_dir_all(&extract);
        return Err(format!("move extracted bundle into place: {error}"));
    }
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

fn staging_manifest_commit(manifest: &Manifest) -> Result<String, String> {
    canonical_release_commit(manifest.commit.as_deref()).ok_or_else(|| {
        "release manifest lacks a clean, valid git commit; refusing to stage".to_string()
    })
}

fn verify_and_publish_incoming(
    staging: &Staging,
    incoming: &Path,
    manifest: &Manifest,
    manifest_commit: &str,
    expected_team: &str,
    roster_read_at: &str,
) -> Result<(), String> {
    let refuse = |why: String| -> Result<(), String> {
        let _ = std::fs::remove_dir_all(incoming);
        Err(why)
    };
    let checks = verify::check_bundle(incoming, expected_team, false);
    if let Err(e) = checks.policy {
        return refuse(format!("staged bundle failed verification: {e}"));
    }
    let bundle_build = match checks.build {
        Ok(n) => n,
        Err(e) => {
            return refuse(format!("staged bundle build number unreadable: {e}"));
        }
    };
    if bundle_build != manifest.build_number {
        return refuse(format!(
            "staged bundle CFBundleVersion {bundle_build} != manifest build_number {} — \
             refusing a manifest/bundle mismatch",
            manifest.build_number
        ));
    }
    let sealed_commit = match checks.commit {
        Ok(commit) => commit,
        Err(error) => return refuse(format!("staged bundle commit unreadable: {error}")),
    };
    if !sealed_commit_matches(Some(manifest_commit), &sealed_commit) {
        return refuse(format!(
            "staged bundle ATermGitCommit {sealed_commit:?} does not match manifest commit"
        ));
    }
    let team = match checks.team {
        Some(Ok(team)) => team,
        _ => verify::team_id(incoming).unwrap_or_else(|_| expected_team.to_string()),
    };
    let ready = staged_ready_marker(
        manifest,
        manifest_commit,
        team,
        &now_rfc3339(),
        roster_read_at,
    );
    let probed = match stage_withdrawn(staging, &ready) {
        Ok(()) => verify::probe_staged_bundle_starts(incoming, bundle_build),
        Err(_) => Ok(()),
    };
    if let Err(error) = probed {
        if stage_failure_is_a_verdict(&error) {
            let _ = std::fs::remove_dir_all(incoming);
            return Err(format!("staged bundle cannot start: {error}"));
        }
        if probe_left_unproven(&error) {
            return Err(format!("staged bundle failed verification: {error}"));
        }
        crate::warn(&format!(
            "staged bundle start probe did not finish: {error}; publishing, and the \
             apply probes it again"
        ));
    }
    publish_verified_stage(staging, incoming, &ready)
}

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
        commit: Some(manifest_commit.to_string()),
        dmg_sha256: manifest.sha256.to_ascii_lowercase(),
        team_id,
        roster_confirmed_at: (!roster_read_at.is_empty()).then(|| roster_read_at.to_string()),
        staged_at: staged_at.to_string(),
        changelog: clamp_ready_changelog(manifest.changelog.as_deref()),
        machine_id: manifest.machine_id.clone(),
        roster_seq: manifest.roster_seq,
    }
}

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

fn unconfirmed_trial_refusal(armed_build: u64) -> String {
    crate::verify::mark_passing(format!(
        "update trial for build {armed_build} is still unconfirmed"
    ))
}

fn boot_apply_refusal_reason(outcome: &ApplyOutcome) -> Option<&str> {
    match outcome {
        ApplyOutcome::Deferred(reason) => Some(reason),
        ApplyOutcome::NotApplicable
        | ApplyOutcome::NoUpdate
        | ApplyOutcome::LeftForSession { .. }
        | ApplyOutcome::ReExecFailed(_)
        | ApplyOutcome::Reverted(_) => None,
    }
}

/// What a launch's boot does with a ready stage, by its image class ([`crate::ImageReplacement`]); [`boot_image_lane`] decides it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BootImageLane {
    /// Swap the stage in and exec the build it installed (steps 3–10).
    SwapAndExec,
    /// Leave the admitted stage for the window's automatic in-session lane, with this launch on the left-stage record ([`left_stage_path`]) that the escape counts: nothing verified, swapped or exec'd ([`ApplyOutcome::LeftForSession`]).
    LeaveForSession,
    /// Leave it for a person's press (`[update] auto_apply = false`): nothing verified, swapped or exec'd, and nothing recorded, since a launch that ends waiting for a press is the design and no evidence that a lane cannot land the stage.
    LeaveForPress,
    /// Swap nothing: a LaunchServices-launched successor, whose parent swapped first.
    NeverSwap,
}

/// The left-stage record ([`left_stage_path`]) as one boot of a window that keeps its image found it, for [`boot_image_lane`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LeftRecord {
    /// It names this many launches that left this same stage and have ended ([`TrialLauncher::has_ended`]'s positive evidence; a clean quit and a crash alike).
    Ended(usize),
    /// It cannot count this launch: it could not be read or written, or the kernel will not name this process.
    Unusable,
}

/// The boot's lane for a ready stage: a launched successor never swaps, and a replaceable launch swaps and execs.
pub(crate) const fn boot_image_lane(
    image: crate::ImageReplacement,
    auto_apply: bool,
    record: LeftRecord,
) -> BootImageLane {
    match image {
        crate::ImageReplacement::LaunchedSuccessor => BootImageLane::NeverSwap,
        crate::ImageReplacement::Replaceable => BootImageLane::SwapAndExec,
        crate::ImageReplacement::WindowKeepsImage if !auto_apply => BootImageLane::LeaveForPress,
        crate::ImageReplacement::WindowKeepsImage => match record {
            LeftRecord::Ended(ended) if ended < crate::LEFT_STAGE_ESCAPE => {
                BootImageLane::LeaveForSession
            }
            LeftRecord::Ended(_) | LeftRecord::Unusable => BootImageLane::SwapAndExec,
        },
    }
}

/// What a crash-loop revert does once the previous build is back at the install ([`revert_lane`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RevertLane {
    /// Re-exec the restored build as a fresh boot.
    ReExec,
    /// Return [`ApplyOutcome::Reverted`] for the caller to exit, replacing nothing.
    ExitInstead,
}

/// The crash-loop revert's lane by image class: only a LaunchServices-launched successor exits instead of re-executing the restored build.
pub(crate) const fn revert_lane(image: crate::ImageReplacement) -> RevertLane {
    match image {
        crate::ImageReplacement::LaunchedSuccessor => RevertLane::ExitInstead,
        crate::ImageReplacement::Replaceable | crate::ImageReplacement::WindowKeepsImage => {
            RevertLane::ReExec
        }
    }
}

/// Applies a staged update if it is ready and strictly newer.
pub fn apply_staged_if_ready(
    current_build: u64,
    current_commit: Option<&str>,
    handoff_fds: &[i32],
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
    handoff_target_is_this_build: bool,
    image: crate::ImageReplacement,
    witness: crate::TrialWitness,
) -> ApplyOutcome {
    let outcome = apply_staged_if_ready_inner(
        current_build,
        current_commit,
        handoff_fds,
        handoff_env,
        handoff_target_is_this_build,
        image,
        witness,
    );
    if let ApplyOutcome::LeftForSession { build, .. } = &outcome
        && let Some(staging) = Staging::resolve()
    {
        crate::health::Health::restart_pending_update(&staging.health(), *build);
    }
    if let Some(reason) = boot_apply_refusal_reason(&outcome) {
        crate::warn(&format!("boot apply refused: {reason}"));
        if let Some(staging) = Staging::resolve() {
            record_boot_apply_refusal(&staging, current_build, reason);
        }
    }
    outcome
}

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
    image: crate::ImageReplacement,
    witness: crate::TrialWitness,
) -> ApplyOutcome {
    let apply_started = std::time::Instant::now();
    let reexec_nonce = take_reexec_nonce();
    let expected_artifact = take_expected_artifact();
    let staging = Staging::resolve();
    let reexec_authority = classify_reexec_authority(staging.as_ref(), reexec_nonce);

    if let Some(s) = &staging
        && let Some(outcome) = check_boot_health(
            s,
            current_build,
            current_commit,
            RollbackHandoff {
                fds: handoff_fds,
                env: handoff_env,
                image,
            },
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
    if handoff_target_is_this_build {
        crate::log(&format!(
            "boot apply: build {current_build} is the handoff's authorized target; any newer \
             stage is left for the successor's own apply lane"
        ));
        return ApplyOutcome::NoUpdate;
    }
    if boot_image_lane(image, true, LeftRecord::Ended(0)) == BootImageLane::NeverSwap {
        crate::log(&format!(
            "boot apply: build {current_build} was launched by LaunchServices as a seamless \
             successor, which never swaps or re-execs itself; its target is not this build, \
             so the handoff will refuse it"
        ));
        return ApplyOutcome::NoUpdate;
    }
    let Some(b) = bundle::resolve() else {
        return ApplyOutcome::NotApplicable;
    };
    let Some(staging) = staging else {
        return ApplyOutcome::NotApplicable;
    };

    let admitted = match admit_stage_under_apply_lock(
        &staging,
        &b.app_root,
        current_build,
        current_commit,
        expected_artifact.as_ref(),
    ) {
        Ok(admitted) => admitted,
        Err(outcome) => return outcome,
    };
    let auto_apply = aterm_update_core::settings::update_auto_apply();
    let admitted = match boot_lane_under_apply_lock(&staging, image, auto_apply, admitted) {
        Ok(admitted) => admitted,
        Err(outcome) => return outcome,
    };

    let SwappedStage {
        lock: _lock,
        budget: _budget,
        ready,
        retained,
        sentinel,
        previous_receipt,
    } = match swap_admitted_stage(
        &staging,
        &b.app_root,
        current_build,
        current_commit,
        admitted,
        &SHIPPED_SWAP_SEAMS,
    ) {
        Ok(swapped) => swapped,
        Err(outcome) => return outcome,
    };

    let nonce = random_nonce().unwrap_or_default();
    let reexec_value = if nonce.is_empty() {
        "1".to_string()
    } else {
        let _ = write_private_file(&staging.reexec_stamp(), nonce.as_bytes());
        nonce
    };

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
    let new_exe = &b.exe;
    let mut reexec = boot_reexec_command(
        new_exe,
        std::env::args_os().skip(1),
        &reexec_value,
        current_build,
        handoff_env,
    );
    let err = exec_preserving_handoff_fds(&mut reexec, handoff_fds);
    let _ = std::fs::remove_file(staging.reexec_stamp());
    crate::warn(&format!(
        "re-exec of {} failed: {err}; rolling back to the previous build",
        new_exe.display()
    ));
    let receipt_restore_error = match unswap_after_failed_exec(
        &staging,
        &retained,
        &b.app_root,
        &sentinel,
        previous_receipt.as_ref(),
        &err,
    ) {
        Ok(receipt_restore_error) => receipt_restore_error,
        Err(outcome) => return outcome,
    };
    crate::manifest::FailedMark::record_stage_failure(
        &staging.failed(),
        ready.build_number,
        &ready.dmg_sha256,
        unix_now_secs(),
    );
    clear_trial_identity(&staging);
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

type SealedIdentity = Result<(u64, String), String>;

struct SwapSeams<'a> {
    one: &'a dyn Fn(&Path) -> SealedIdentity,
    both: &'a dyn Fn([&Path; 2]) -> [SealedIdentity; 2],
    probe: &'a dyn Fn(&Path, u64) -> Result<(), String>,
}

const SHIPPED_SWAP_SEAMS: SwapSeams<'static> = SwapSeams {
    one: &verified_bundle_identity,
    both: &verified_bundle_identities::<2>,
    probe: &crate::verify::probe_bundle_starts,
};

struct AdmittedStage {
    lock: FileLock,
    budget: crate::verify::ApplyBudget,
    ready: Ready,
}

struct SwappedStage {
    lock: FileLock,
    budget: crate::verify::ApplyBudget,
    ready: Ready,
    retained: PathBuf,
    sentinel: Sentinel,
    previous_receipt: Option<crate::manifest::InstalledReceipt>,
}

fn swap_under_apply_lock(
    staging: &Staging,
    installed: &Path,
    current_build: u64,
    current_commit: Option<&str>,
    expected: Option<&ExpectedArtifact>,
    seams: &SwapSeams<'_>,
) -> Result<SwappedStage, ApplyOutcome> {
    let admitted =
        admit_stage_under_apply_lock(staging, installed, current_build, current_commit, expected)?;
    swap_admitted_stage(
        staging,
        installed,
        current_build,
        current_commit,
        admitted,
        seams,
    )
}

fn admit_stage_under_apply_lock(
    staging: &Staging,
    installed: &Path,
    current_build: u64,
    current_commit: Option<&str>,
    expected: Option<&ExpectedArtifact>,
) -> Result<AdmittedStage, ApplyOutcome> {
    let (lock, budget, ready) = apply_lock_then_ready(
        staging,
        current_build,
        || {
            FileLock::acquire_within(&staging.apply_lock, APPLY_LOCK_WAIT)
                .map_err(|e| ApplyOutcome::Deferred(crate::verify::lock_wait_refusal("lock", &e)))
        },
        || {
            let budget = crate::verify::ApplyBudget::start(crate::verify::APPLY_BUDGET);
            settle_trials_under_apply_lock(staging, installed, current_build, current_commit)?;
            Ok(budget)
        },
    )?;
    if let Some(outcome) =
        stage_admission_gates(staging, current_build, &ready, expected, unix_now_secs())
    {
        return Err(outcome);
    }

    if let Err(refusal) = install_location_check(installed) {
        crate::status::record(
            staging,
            current_build,
            if crate::is_passing_refusal(&refusal) {
                "deferred: install location could not be written just then"
            } else {
                "deferred: install location not writable"
            },
        );
        return Err(ApplyOutcome::Deferred(refusal));
    }
    Ok(AdmittedStage {
        lock,
        budget,
        ready,
    })
}

fn swap_admitted_stage(
    staging: &Staging,
    installed: &Path,
    current_build: u64,
    current_commit: Option<&str>,
    admitted: AdmittedStage,
    seams: &SwapSeams<'_>,
) -> Result<SwappedStage, ApplyOutcome> {
    let AdmittedStage {
        lock,
        budget,
        ready,
    } = admitted;
    let VerifiedSwap {
        prepared,
        old: (old_build, old_commit),
    } = verify_stage_and_prepare(
        staging,
        installed,
        &ready,
        (current_build, current_commit),
        seams.one,
        seams.both,
    )?;
    let previous_receipt = previous_receipt_for_sealed_old(staging, old_build, &old_commit);

    if let Err(error) = (seams.probe)(&prepared.fixed, ready.build_number) {
        // A probe that started the candidate and was killed before it exited leaves
        // macOS assessing its first exec, which the kill does not end: the candidate
        // stays exactly where it ran ([`keep_unproven_candidate`]). Any other failure
        // either ran it to an exit or never started it, and is backed out as before.
        if probe_left_unproven(&error) {
            keep_unproven_candidate(&prepared, staging, unix_now_secs());
        } else {
            recover_prepared_candidate(&prepared, staging);
        }
        if stage_failure_is_a_verdict(&error) {
            crate::manifest::FailedMark::record_stage_failure(
                &staging.failed(),
                ready.build_number,
                &ready.dmg_sha256,
                unix_now_secs(),
            );
        }
        return Err(ApplyOutcome::Deferred(format!(
            "staged candidate cannot start: {error}"
        )));
    }

    let sentinel = match prepare_trial(staging, &ready, installed) {
        Ok(sentinel) => sentinel,
        Err(error) => {
            recover_prepared_candidate(&prepared, staging);
            return Err(ApplyOutcome::Deferred(error));
        }
    };
    let retained = swap_in_and_record_receipt(staging, &prepared, installed, &ready, &sentinel)?;
    staging.retire_published();
    let _ = std::fs::remove_file(left_stage_path(staging));
    record_activating_status(staging, &ready);
    Ok(SwappedStage {
        lock,
        budget,
        ready,
        retained,
        sentinel,
        previous_receipt,
    })
}

fn left_stage_path(staging: &Staging) -> PathBuf {
    staging.root.join("left-stage")
}

const MAX_LEFT_LAUNCHERS: usize = 32;

fn read_left_launchers(staging: &Staging, ready: &Ready) -> std::io::Result<Vec<TrialLauncher>> {
    let Some(text) = crate::read_ledger_text_checked(&left_stage_path(staging))? else {
        return Ok(Vec::new());
    };
    let mut lines = text.lines();
    let identity = format!("{} {}", ready.build_number, ready.dmg_sha256);
    if lines.next().map(str::trim) != Some(identity.as_str()) {
        return Ok(Vec::new());
    }
    let mut launchers = Vec::new();
    for line in lines {
        let mut fields = line.split_whitespace();
        let mut field = || fields.next().and_then(|field| field.parse::<i64>().ok());
        let (Some(pid), Some(seconds), Some(micros), None) = (field(), field(), field(), field())
        else {
            return Ok(Vec::new());
        };
        let Ok(pid) = u32::try_from(pid) else {
            return Ok(Vec::new());
        };
        launchers.push(TrialLauncher {
            pid,
            started: (seconds, micros),
        });
    }
    Ok(launchers)
}

fn write_left_launchers(
    staging: &Staging,
    ready: &Ready,
    launchers: &[TrialLauncher],
) -> std::io::Result<()> {
    let skip = launchers.len().saturating_sub(MAX_LEFT_LAUNCHERS);
    let mut text = format!("{} {}\n", ready.build_number, ready.dmg_sha256);
    for launcher in &launchers[skip..] {
        text.push_str(&format!(
            "{} {} {}\n",
            launcher.pid, launcher.started.0, launcher.started.1
        ));
    }
    write_private_file(&left_stage_path(staging), text.as_bytes())
}

fn boot_lane_under_apply_lock(
    staging: &Staging,
    image: crate::ImageReplacement,
    auto_apply: bool,
    admitted: AdmittedStage,
) -> Result<AdmittedStage, ApplyOutcome> {
    let build = admitted.ready.build_number;
    let read = read_left_launchers(staging, &admitted.ready);
    let record = match &read {
        Ok(launchers) => LeftRecord::Ended(
            launchers
                .iter()
                .filter(|launcher| launcher.has_ended())
                .count(),
        ),
        Err(_) => LeftRecord::Unusable,
    };
    let ended = match record {
        LeftRecord::Ended(ended) => ended,
        LeftRecord::Unusable => 0,
    };
    let mut unusable = read
        .as_ref()
        .err()
        .map(|error| format!("could not be read ({error})"));
    let lane = match boot_image_lane(image, auto_apply, record) {
        BootImageLane::LeaveForSession => {
            match record_left_launch(staging, &admitted.ready, read.unwrap_or_default()) {
                Ok(()) => BootImageLane::LeaveForSession,
                Err(why) => {
                    unusable = Some(why);
                    boot_image_lane(image, auto_apply, LeftRecord::Unusable)
                }
            }
        }
        lane => lane,
    };
    match lane {
        BootImageLane::SwapAndExec => {
            if image == crate::ImageReplacement::WindowKeepsImage {
                crate::warn(&match unusable {
                    Some(why) => format!(
                        "boot apply: the record of the launches that left build {build} {why}; \
                         installing it at this launch's boot (swap and exec) rather than leave \
                         a stage no escape could count"
                    ),
                    None => format!(
                        "boot apply: {ended} launches left build {build} for their windows' \
                         in-session lane and ended before it installed; installing it at this \
                         launch's boot (swap and exec)"
                    ),
                });
            }
            Ok(admitted)
        }
        BootImageLane::LeaveForSession | BootImageLane::LeaveForPress => {
            let AdmittedStage {
                lock,
                budget,
                ready,
            } = admitted;
            drop(budget);
            drop(lock);
            crate::log(&if lane == BootImageLane::LeaveForPress {
                format!(
                    "boot apply: build {build} left for this window's in-session lane, for a \
                     press: [update] auto_apply is off, so no window launch installs it \
                     without one (no swap, no exec before the window)"
                )
            } else {
                format!(
                    "boot apply: build {build} left for this window's in-session lane (no swap, \
                     no exec before the window; {ended} earlier launch(es) that left it have \
                     ended)"
                )
            });
            Err(ApplyOutcome::LeftForSession {
                build,
                dmg_sha256: ready.dmg_sha256,
            })
        }
        BootImageLane::NeverSwap => Err(ApplyOutcome::NoUpdate),
    }
}

fn record_left_launch(
    staging: &Staging,
    ready: &Ready,
    mut launchers: Vec<TrialLauncher>,
) -> Result<(), String> {
    let me = TrialLauncher::this_process()
        .ok_or_else(|| "cannot name this launch (the kernel will not)".to_string())?;
    if !launchers.contains(&me) {
        launchers.push(me);
        write_left_launchers(staging, ready, &launchers)
            .map_err(|error| format!("could not be written ({error})"))?;
    }
    Ok(())
}

/// The parent's half of a LaunchServices-launched handoff: swap the stage the attempt authorizes (`expected`) into the install before asking LaunchServices for the successor, so the process it starts is already NEW and never replaces its own image (an `execve` some hundred milliseconds after a LaunchServices launch leaves LaunchServices answering pid -1 for the app for the rest of its life).
pub(crate) fn swap_staged_for_launch(
    current_build: u64,
    current_commit: &str,
    expected: &ExpectedArtifact,
) -> Result<u64, ApplyOutcome> {
    if !crate::enabled() {
        return Err(ApplyOutcome::NotApplicable);
    }
    let Some(b) = bundle::resolve() else {
        return Err(ApplyOutcome::NotApplicable);
    };
    let Some(staging) = Staging::resolve() else {
        return Err(ApplyOutcome::NotApplicable);
    };
    swap_staged_for_launch_at(
        &staging,
        &b.app_root,
        current_build,
        Some(current_commit),
        expected,
        &SHIPPED_SWAP_SEAMS,
    )
}

fn swap_staged_for_launch_at(
    staging: &Staging,
    installed: &Path,
    current_build: u64,
    current_commit: Option<&str>,
    expected: &ExpectedArtifact,
    seams: &SwapSeams<'_>,
) -> Result<u64, ApplyOutcome> {
    let started = std::time::Instant::now();
    let SwappedStage {
        lock,
        budget,
        ready,
        ..
    } = swap_under_apply_lock(
        staging,
        installed,
        current_build,
        current_commit,
        Some(expected),
        seams,
    )?;
    drop(budget);
    drop(lock);
    crate::log(&format!(
        "installed update {} (build {}) in {} ms before launching its successor; the \
         successor starts as that build and swaps nothing",
        ready.version,
        ready.build_number,
        started.elapsed().as_millis()
    ));
    Ok(ready.build_number)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BootHealthLane {
    FullProof,
    CountOnly,
}

const fn boot_health_lane(handoff_target: bool, attempts_after_launch: u32) -> BootHealthLane {
    if handoff_target && attempts_after_launch <= 1 {
        BootHealthLane::CountOnly
    } else {
        BootHealthLane::FullProof
    }
}

#[cfg(test)]
mod boot_health_lane_tests {
    use super::{BootHealthLane, boot_health_lane};

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
    handoff: RollbackHandoff<'_>,
    handoff_target: bool,
    witness: crate::TrialWitness,
) -> Option<ApplyOutcome> {
    check_boot_health_with_lock_wait(
        staging,
        current_build,
        current_commit,
        handoff,
        handoff_target,
        witness,
        APPLY_LOCK_WAIT,
        bundle::resolve_layout,
    )
}

#[allow(clippy::too_many_arguments)]
fn check_boot_health_with_lock_wait(
    staging: &Staging,
    current_build: u64,
    current_commit: Option<&str>,
    handoff: RollbackHandoff<'_>,
    handoff_target: bool,
    witness: crate::TrialWitness,
    lock_wait: std::time::Duration,
    resolve_installed: impl FnOnce() -> Option<bundle::Bundle>,
) -> Option<ApplyOutcome> {
    check_boot_health_with_proof(
        staging,
        current_build,
        handoff,
        handoff_target,
        witness,
        lock_wait,
        resolve_installed,
        |app_root| ensure_current_trial_receipt(staging, app_root, current_build, current_commit),
    )
}

#[allow(clippy::too_many_arguments)]
fn check_boot_health_with_proof(
    staging: &Staging,
    current_build: u64,
    handoff: RollbackHandoff<'_>,
    handoff_target: bool,
    witness: crate::TrialWitness,
    lock_wait: std::time::Duration,
    resolve_installed: impl FnOnce() -> Option<bundle::Bundle>,
    prove: impl FnOnce(&Path) -> Result<TrialRevert, String>,
) -> Option<ApplyOutcome> {
    let sentinel = boot_sentinel(staging);
    let armed_for_this_build =
        |sentinel: &Sentinel| matches!(sentinel.read_state(), Some((b, _)) if b == current_build);
    let retrial = witness == crate::TrialWitness::Window
        && !armed_for_this_build(&sentinel)
        && window_unproven_names(staging, current_build);
    if !armed_for_this_build(&sentinel) && !retrial {
        return None;
    }
    let apply_lock = match FileLock::acquire_within(&staging.apply_lock, lock_wait) {
        Ok(lock) => lock,
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
    if !armed_for_this_build(&sentinel) && !retrial {
        return None;
    }
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
    if !trial_owned_by(staging, &b.app_root) {
        crate::log(&format!(
            "boot sentinel armed for build {current_build} belongs to another install of \
             this build; this launch from {} is not counted against it",
            b.app_root.display()
        ));
        return None;
    }
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
    let verified_rollback = match prove(&b.app_root) {
        Ok(TrialRevert::To(rollback)) => rollback,
        Ok(TrialRevert::Unavailable(why)) => {
            return settle_trial_without_revert(
                staging,
                &sentinel,
                current_build,
                &b.app_root,
                &why,
            );
        }
        Err(error) => {
            // Within budget: the proof may recover on a later boot (a transient
            // read failure, a receipt being rewritten), so keep the trial armed.
            if !sentinel.should_revert(current_build, MAX_BOOT_ATTEMPTS) {
                return Some(ApplyOutcome::Deferred(format!(
                    "trial recovery proof: {error}"
                )));
            }
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
                    handoff,
                    apply_lock,
                ));
            }
            let disarm = sentinel.confirm();
            crate::health::Health::record_apply_failure(
                &staging.health(),
                current_build,
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
        handoff,
        apply_lock,
    ))
}

fn settle_trial_without_revert(
    staging: &Staging,
    sentinel: &Sentinel,
    current_build: u64,
    app_root: &Path,
    why: &str,
) -> Option<ApplyOutcome> {
    if let Err(error) = sentinel.confirm() {
        return Some(ApplyOutcome::Deferred(format!(
            "trial recovery proof: no revert is available ({why}); disarm failed: {error}"
        )));
    }
    clear_trial_identity(staging);
    clear_window_unproven(staging);
    let _ = remove_path_no_follow(&rollback_path(app_root));
    crate::warn(&format!(
        "build {current_build}'s update trial has no previous build to revert to ({why}); \
         settled it rather than refusing every newer update"
    ));
    crate::status::record(
        staging,
        current_build,
        "settled an update trial with no previous build to revert to; updates re-enabled",
    );
    None
}

fn rollback_reexec_command(
    restored_exe: &Path,
    args: impl Iterator<Item = std::ffi::OsString>,
    handoff_env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> aterm_uds::spawnfd::StdCommand {
    #[expect(
        clippy::disallowed_types,
        reason = "the crash-loop revert's re-exec of the restored build, under boot_reexec_command's rule"
    )]
    let mut reexec = std::process::Command::new(restored_exe);
    for (key, value) in handoff_env {
        reexec.env(key, value);
    }
    reexec.args(window_successor_args(args));
    reexec
}

#[derive(Clone, Copy)]
struct RollbackHandoff<'a> {
    fds: &'a [i32],
    env: &'a [(std::ffi::OsString, std::ffi::OsString)],
    image: crate::ImageReplacement,
}

#[cfg(test)]
impl RollbackHandoff<'_> {
    const NONE: RollbackHandoff<'static> = RollbackHandoff {
        fds: &[],
        env: &[],
        image: crate::ImageReplacement::Replaceable,
    };
}

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
        image,
    } = handoff;
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
    if let Err(e) = restore_rollback(&rb, &b.app_root, install_flush(staging, &b.app_root)) {
        crate::warn(&format!(
            "crash-loop revert failed: {e}; preserving recovery authority"
        ));
        return ApplyOutcome::Deferred(format!("crash-loop revert: {e}"));
    }
    if let Err(error) = sentinel.confirm() {
        return ApplyOutcome::Deferred(format!(
            "rollback succeeded but trial disarm failed: {error}"
        ));
    }
    if let Some(t) = crate::manifest::FailedMark::read(&staging.trial())
        && t.build_number == current_build
    {
        crate::manifest::quarantine_artifact(
            staging,
            t.build_number,
            &t.sha256,
            install_flush(staging, &b.app_root),
        );
    }
    let _ = std::fs::remove_dir_all(&rb);
    crate::manifest::InstalledReceipt::clear(&staging.installed_receipt());
    clear_trial_identity(staging);
    staging.retire_published();
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
    if revert_lane(image) == RevertLane::ExitInstead {
        return ApplyOutcome::Reverted(format!(
            "reverted the crash-looping build {current_build} to build {restored_build}; \
             this LaunchServices-launched successor exits instead of re-executing it"
        ));
    }
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

/// The running build takes the install path back from an activation a newer stage supersedes: the transaction behind [`crate::supersede_installed_activation`].
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
    let _lock = FileLock::acquire_within(&staging.apply_lock, BACKGROUND_LOCK_WAIT)
        .map_err(|error| ForNow(format!("apply lock: {error}")))?;
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

type SealedIdentityReader = dyn Fn(&Path) -> Result<(u64, String), String>;

fn supersede_activation_locked(
    staging: &Staging,
    installed: &Path,
    (current_build, current_commit): (u64, &str),
    (activation_build, activation_commit): (u64, &str),
    newer_build: u64,
    identity: &SealedIdentityReader,
) -> Result<crate::SupersededActivation, crate::SupersedeRefusal> {
    let stays =
        |why: String| format!("the installed build {activation_build} stays in place: {why}");
    let for_now = |why: String| -> Result<crate::SupersededActivation, crate::SupersedeRefusal> {
        Err(crate::SupersedeRefusal::ForNow(stays(why)))
    };
    let settled = |why: String| -> Result<crate::SupersededActivation, crate::SupersedeRefusal> {
        Err(crate::SupersedeRefusal::Settled(stays(why)))
    };
    if newer_build <= activation_build {
        return settled(format!("build {newer_build} is not newer than it"));
    }
    let ready = match read_ready(staging, current_build) {
        ReadyState::Newer(ready) if ready.build_number == newer_build => ready,
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
        Err(error) => return for_now(format!("the staged bundle does not verify: {error}")),
    }
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
    if let Err(error) = restore_rollback(&fixed, installed, install_flush(staging, installed)) {
        return for_now(error);
    }
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

fn trial_armed_for(staging: &Staging, current_build: u64) -> bool {
    let sentinel = boot_sentinel(staging);
    match sentinel.read_state() {
        Some((build, _)) => build == current_build,
        None => {
            sentinel.unreadable()
                && crate::manifest::FailedMark::read(&staging.trial())
                    .is_some_and(|trial| trial.build_number == current_build)
        }
    }
}

fn confirm_trial_health_after_proof(
    staging: &Staging,
    current_build: u64,
    witness: crate::TrialWitness,
    prove: impl FnOnce() -> Result<(), String>,
) -> bool {
    let sentinel = boot_sentinel(staging);
    if !trial_armed_for(staging, current_build) {
        return false;
    }
    if let Err(error) = prove() {
        note_trial_proof_failure(staging, current_build, witness, &error);
        return false;
    }
    if !trial_armed_for(staging, current_build) || sentinel.confirm().is_err() {
        return false;
    }
    clear_trial_identity(staging);
    crate::health::Health::record_apply_success(&staging.health());
    true
}

fn note_trial_proof_failure(
    staging: &Staging,
    build: u64,
    witness: crate::TrialWitness,
    error: &str,
) {
    let who = match witness {
        crate::TrialWitness::Window => "a window",
        crate::TrialWitness::Headless => "a headless start",
    };
    let words = format!(
        "{who} reached its health checkpoint, but the trial's recovery proof failed: {error}"
    );
    if crate::health::Health::record_trial_proof_failure(&staging.health(), build, &words) {
        crate::warn(&format!(
            "build {build}'s boot trial stays armed: {words}; the confirmation is retried"
        ));
    }
}

fn confirm_health_under_apply_lock_with_proof(
    staging: &Staging,
    current_build: u64,
    app_root: &Path,
    prove: impl FnOnce() -> Result<(), String>,
) -> bool {
    if !confirm_trial_health_after_proof(staging, current_build, crate::TrialWitness::Window, prove)
    {
        return false;
    }
    let _ = std::fs::remove_file(trial_window_launch_path(staging));
    clear_window_unproven(staging);
    keep_settled_predecessor(staging, &rollback_path(app_root));
    reclaim_swapped_out_orphan(staging);
    true
}

fn reclaim_swapped_out_orphan(staging: &Staging) {
    if !staging.ready.exists() {
        let _ = std::fs::remove_dir_all(&staging.staged_app);
    }
}

fn kept_predecessor_path(staging: &Staging) -> PathBuf {
    staging.root.join("predecessor.kept")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KeptPredecessor {
    holder: TrialLauncher,
    bundle: PathBuf,
}

impl KeptPredecessor {
    fn read(staging: &Staging) -> Option<Self> {
        Self::parse(&crate::read_ledger_text(&kept_predecessor_path(staging))?)
    }

    fn parse(text: &str) -> Option<Self> {
        use std::os::unix::ffi::OsStringExt;

        let mut lines = text.lines();
        let mut fields = lines.next()?.split_whitespace();
        if fields.next()? != "v1" {
            return None;
        }
        let holder = TrialLauncher {
            pid: fields.next()?.parse().ok()?,
            started: (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?),
        };
        let bytes = aterm_codec::hex::decode_vec(lines.next()?.as_bytes())?;
        if fields.next().is_some() || lines.next().is_some() || bytes.contains(&0) {
            return None;
        }
        let bundle = PathBuf::from(std::ffi::OsString::from_vec(bytes));
        (bundle.is_absolute() && rollback_path(&bundle) == bundle)
            .then_some(Self { holder, bundle })
    }

    fn encode(&self) -> String {
        use std::os::unix::ffi::OsStrExt;

        format!(
            "v1 {} {} {}\n{}\n",
            self.holder.pid,
            self.holder.started.0,
            self.holder.started.1,
            aterm_codec::hex::encode_lower(self.bundle.as_os_str().as_bytes())
        )
    }

    fn record(&self, staging: &Staging) -> std::io::Result<()> {
        write_private_file(&kept_predecessor_path(staging), self.encode().as_bytes())
    }
}

fn clear_kept_predecessor(staging: &Staging) {
    let _ = std::fs::remove_file(kept_predecessor_path(staging));
}

fn keep_settled_predecessor(staging: &Staging, fixed: &Path) {
    clear_kept_predecessor(staging);
    if !is_non_symlink_dir(fixed) {
        return;
    }
    let kept = TrialLauncher::this_process().map(|holder| KeptPredecessor {
        holder,
        bundle: fixed.to_path_buf(),
    });
    if let Err(why) = kept.map_or_else(
        || Err("this process has no identity to key it by".to_string()),
        |kept| kept.record(staging).map_err(|error| error.to_string()),
    ) {
        crate::warn(&format!(
            "could not record the previous build at {} for this window ({why}); leaving it in place",
            fixed.display()
        ));
    }
}

fn release_kept_predecessor(
    staging: &Staging,
    lock_wait: std::time::Duration,
    runs_from: impl Fn(&Path) -> Option<bool>,
) {
    if KeptPredecessor::read(staging).is_none_or(|kept| !kept.holder.has_ended()) {
        return;
    }
    if let Ok(_apply_lock) = FileLock::acquire_within(&staging.apply_lock, lock_wait) {
        release_kept_predecessor_under_apply_lock(staging, runs_from);
    }
}

fn release_kept_predecessor_under_apply_lock(
    staging: &Staging,
    runs_from: impl Fn(&Path) -> Option<bool>,
) {
    let Some(kept) = KeptPredecessor::read(staging) else {
        return;
    };
    let sentinel = boot_sentinel(staging);
    if !kept.holder.has_ended() || sentinel.read_state().is_some() || sentinel.unreadable() {
        return;
    }
    if !is_non_symlink_dir(&kept.bundle)
        || (runs_from(&kept.bundle) == Some(false) && remove_path_no_follow(&kept.bundle).is_ok())
    {
        clear_kept_predecessor(staging);
    }
}

fn process_uses(dir: &Path) -> Option<bool> {
    let dir = std::fs::canonicalize(dir).ok()?;
    aterm_uds::holders::scan_path_users(std::slice::from_ref(&dir), &[])
        .ok()
        .and_then(|scan| path_scan_uses(scan, &dir))
}

fn path_scan_uses(scan: aterm_uds::holders::PathScan, dir: &Path) -> Option<bool> {
    if !scan.users.is_empty() {
        Some(true)
    } else if scan
        .unreadable
        .iter()
        .any(|(_, exe)| exe.as_ref().is_none_or(|exe| exe.starts_with(dir)))
    {
        None
    } else {
        Some(false)
    }
}

fn confirm_headless_keeping_predecessor(
    staging: &Staging,
    current_build: u64,
    app_root: &Path,
    prove: impl FnOnce() -> Result<(), String>,
) -> bool {
    let digest = crate::manifest::FailedMark::read(&staging.trial())
        .filter(|trial| trial.build_number == current_build)
        .and_then(|trial| canonical_digest(&trial.sha256));
    if !confirm_trial_health_after_proof(
        staging,
        current_build,
        crate::TrialWitness::Headless,
        prove,
    ) {
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
        // The hard health-commit gate, as `confirm_boot_health_at` runs it.
        ensure_current_trial_receipt(staging, app_root, current_build, current_commit).map(|_| ())
    })
}

/// How many launches the boot sentinel has counted for `build` (0 when it is armed for another build, or not armed at all).
#[must_use]
pub(crate) fn trial_launch_count(build: u64) -> u32 {
    Staging::resolve().map_or(0, |staging| trial_launch_count_at(&staging, build))
}

fn trial_launch_count_at(staging: &Staging, build: u64) -> u32 {
    boot_sentinel(staging)
        .read_state()
        .filter(|(armed, _)| *armed == build)
        .map_or(0, |(_, attempts)| attempts)
}

const TRIAL_LAUNCH_STAMP_MAX_BYTES: u64 = 64;

/// See [`crate::stamp_trial_launch`].
pub(crate) fn stamp_trial_launch(stamp: &Path) -> std::io::Result<()> {
    let this = TrialLauncher::this_process()
        .ok_or_else(|| std::io::Error::other("the kernel will not name this process"))?;
    write_trial_launch_stamp(stamp, this)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
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

/// The launch `stamp` names ([`write_trial_launch_stamp`]), or `None` when there is none, it is not a regular file, or it is not exactly one name: nothing is given back by it, and a launch it counted stays counted.
pub(crate) fn read_trial_launch_stamp(stamp: &Path) -> Option<LaunchName> {
    use std::os::unix::fs::OpenOptionsExt as _;

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

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "ExitsByRule",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "Settled",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "SiblingGivesBack",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateTrialGiveBack",
        action = "SiblingSettled",
        project = "aterm_update::install::boot_tests::trial_give_back_projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateTrialGiveBack",
        action = "Killed",
        reason = "A death the candidate did not choose (its parent's post-grant kill, the machine's) runs no ledger code; its parent's settle that follows is Settled."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateTrialGiveBack",
        action = "SiblingDies",
        reason = "A sibling's death it did not choose runs no ledger code; its parent's settle by name that may follow is SiblingSettled."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
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
        GiveBackLock::Busy => owe_trial_launch(staging, &sentinel, target_build, pid, started),
    }
}

fn owed_give_backs_dir(staging: &Staging) -> PathBuf {
    staging.root.join("boot.sentinel.owed")
}

const MAX_OWED_GIVE_BACKS: usize = 64;

const OWED_TEMP_STALE: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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

fn parse_owed_give_back(text: &str) -> Option<(u64, LaunchName)> {
    let mut fields = text.strip_suffix('\n')?.split(' ');
    let build = fields.next()?.parse().ok()?;
    let name = LaunchName {
        pid: fields.next()?.parse().ok()?,
        started: (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?),
    };
    fields.next().is_none().then_some((build, name))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
        Ok(None) => false,
        Err(error) => {
            crate::warn(&format!(
                "could not give back pid {pid}'s trial launch of build {target_build}: {error}"
            ));
            false
        }
    }
}

enum GiveBackLock {
    Held(FileLock),
    NotOurs,
    Busy,
}

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
    if let Some(app_root) = installed
        && !trial_owned_by(staging, app_root)
    {
        return GiveBackLock::NotOurs;
    }
    match FileLock::acquire_within(&staging.apply_lock, lock_wait) {
        Ok(lock) => GiveBackLock::Held(lock),
        Err(error) => {
            crate::warn(&format!(
                "could not take the apply lock to give back a trial launch of build \
                 {target_build}: {error}"
            ));
            GiveBackLock::Busy
        }
    }
}

/// Confirms the running build reached a healthy checkpoint (window up / first frame): clears the boot sentinel, keeps the retained rollback bundle for the window that started from it ([`kept_predecessor_path`]) and GCs the orphaned post-swap copy.
#[must_use]
pub fn confirm_boot_health(
    current_build: u64,
    current_commit: Option<&str>,
    witness: crate::TrialWitness,
) -> bool {
    let Some(staging) = Staging::resolve() else {
        return true;
    };
    release_kept_predecessor(&staging, APPLY_LOCK_WAIT, process_uses);
    confirm_boot_health_at(
        &staging,
        current_build,
        current_commit,
        witness,
        bundle::resolve_layout,
        APPLY_LOCK_WAIT,
    )
}

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
            ensure_current_trial_receipt(staging, app_root, current_build, current_commit)
                .map(|_| ())
        },
    )
}

fn confirm_boot_health_at_with_proof(
    staging: &Staging,
    current_build: u64,
    witness: crate::TrialWitness,
    resolve_installed: impl FnOnce() -> Option<bundle::Bundle>,
    lock_wait: std::time::Duration,
    prove: impl FnOnce(&Path) -> Result<(), String>,
) -> bool {
    // No trial armed for this build: already complete. A different build's marker is
    // not ours to retry or mutate. An unreadable marker of this build's trial is
    // still its trial (`trial_armed_for`).
    if !trial_armed_for(staging, current_build) {
        return true;
    }
    let Ok(_apply_lock) = FileLock::acquire_within(&staging.apply_lock, lock_wait) else {
        crate::warn(&format!(
            "could not take the apply lock within {}s to confirm build {current_build}'s \
             boot health (another aterm holds it); the confirmation is retried later",
            lock_wait.as_secs()
        ));
        return false;
    };
    let Some(installed) = resolve_installed() else {
        return false;
    };
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
        if armed_build != 0 && (armed_build != current_build || !trial_owned_by(staging, app_root))
        {
            escape_wedged_foreign_trial(staging, current_build, armed_build);
        }
        return Err(ApplyOutcome::Deferred(unconfirmed_trial_refusal(
            armed_build,
        )));
    }
    let _ = foreign_trial_counter(staging).confirm();
    if let ReadyState::Newer(orphaned_ready) = read_ready(staging, current_build)
        && let Err(error) =
            recover_orphaned_prepared_candidate(staging, app_root, current_build, &orphaned_ready)
    {
        return Err(ApplyOutcome::Deferred(error));
    }
    Ok(())
}

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
    let ready = ready_under_apply_lock(staging, current_build)?;
    Ok((held, work, ready))
}

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

fn floor_withdrawal_gate(
    staging: &Staging,
    current_build: u64,
    ready: &Ready,
) -> Option<ApplyOutcome> {
    let floor = match crate::manifest::Floor::read_for_gate(&staging.floor()) {
        Ok(floor) => floor,
        Err(refusal) => {
            return Some(records_unreadable_deferral(
                staging,
                current_build,
                ready,
                refusal,
            ));
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

fn records_unreadable_deferral(
    staging: &Staging,
    current_build: u64,
    ready: &Ready,
    refusal: String,
) -> ApplyOutcome {
    crate::warn(&format!(
        "staged build {} not applied: {refusal}",
        ready.build_number
    ));
    crate::status::record(
        staging,
        current_build,
        "deferred: aterm could not read its update records just then",
    );
    ApplyOutcome::Deferred(refusal)
}

/// The swap's gates on the stage it read under the apply lock, in order: the artifact a seamless handoff's parent authorized (`expected`, [`expected_artifact_refusal`], which refuses and retires nothing), the floor's withdrawals (4b, 4c: [`floor_withdrawal_gate`], which retire), the crash-loop quarantine (retired), a marker that no longer names the published stage (retired), and the age of its roster confirmation (4d: [`roster_freshness_gate`], which waits).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateRosterFreshness",
        action = "Swap",
        project = "aterm_update::roster_freshness_projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateRosterFreshness",
        action = "Tick",
        reason = "Time passing is the environment; the Tier-1 bind maps each tick to half the window of the real clock it hands these gates."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateRosterFreshness",
        action = "Revoke",
        reason = "The owner's revocation is published elsewhere and reaches this Mac only as a roster a check admits, which is Check."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateRosterFreshness",
        action = "SwapOnTheFloorAlone",
        reason = "Buggy=1 negative control only: a lane that admits a stage on the floor's withdrawals alone; gate 4d reads the confirmation's age."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateRosterFreshness",
        action = "ConfirmWithoutTheRoster",
        reason = "Buggy=1 negative control only: a confirmation stamped without reading the roster's revocations; the check stamps only after the floor it just wrote withdraws nothing."
    )
)]
pub(crate) fn stage_admission_gates(
    staging: &Staging,
    current_build: u64,
    ready: &Ready,
    expected: Option<&ExpectedArtifact>,
    now: u64,
) -> Option<ApplyOutcome> {
    if let Some(refusal) = expected.and_then(|expected| expected_artifact_refusal(ready, expected))
    {
        return Some(ApplyOutcome::Deferred(refusal));
    }
    if let Some(outcome) = floor_withdrawal_gate(staging, current_build, ready) {
        return Some(outcome);
    }
    match crate::manifest::artifact_quarantined_for_gate(
        staging,
        ready.build_number,
        &ready.dmg_sha256,
    ) {
        Err(refusal) => {
            return Some(records_unreadable_deferral(
                staging,
                current_build,
                ready,
                refusal,
            ));
        }
        Ok(true) => {
            staging.retire_published();
            return Some(ApplyOutcome::NoUpdate);
        }
        Ok(false) => {}
    }
    if !ready.names_published_bundle(staging) {
        staging.retire_published();
        return Some(ApplyOutcome::NoUpdate);
    }
    roster_freshness_gate(ready, now)
}

fn roster_freshness_gate(ready: &Ready, now: u64) -> Option<ApplyOutcome> {
    ready
        .roster_authorization_stale(now, aterm_update_core::pins::roster_tier_armed())
        .map(ApplyOutcome::Deferred)
}

/// Record that a check confirmed `staged` against a roster it admitted, which it began reading at `confirmed_at` (RFC3339): the marker's [`Ready::roster_confirmed_at`] moves to that instant, so gate 4d and the pre-park checks admit it for another [`crate::manifest::ROSTER_FRESHNESS_SECS`], and a standing apply refusal that waited for this is withdrawn from the ledger.
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
            crate::manifest::write_durable(
                &staging.ready,
                &ready.to_toml()?,
                "ready marker",
                Flush::Ordered,
            )?;
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

fn restore_rollback(rollback: &Path, installed: &Path, flush: Flush) -> Result<(), String> {
    checked_bundle_exchange(rollback, installed, "atomic rollback restore", flush)
}

fn random_nonce() -> Option<String> {
    aterm_uds::rand::hex_token::<16>().ok()
}

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

/// Best-effort RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) for the human-readable `staged_at`/status fields, computed in-process (no `/bin/date` fork).
pub(crate) fn now_rfc3339() -> String {
    aterm_time::unix_secs().map_or_else(String::new, format_rfc3339)
}

/// Unix seconds now, or 0 on a pre-epoch clock, for the stage-failure retry budget's deadlines and the check lane's window and receipt.
#[must_use]
pub(crate) fn unix_now_secs() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// Seconds between two of our own RFC3339 instants (`later - earlier`), or `None` when either string is not the exact `YYYY-MM-DDTHH:MM:SSZ` shape this module writes (e.g. the empty pre-epoch fallback) or `later` precedes `earlier`.
pub(crate) fn rfc3339_delta_secs(earlier: &str, later: &str) -> Option<u64> {
    fn epoch(s: &str) -> Option<u64> {
        u64::try_from(aterm_types::rfc3339::parse_utc(s)?).ok()
    }
    epoch(later)?.checked_sub(epoch(earlier)?)
}

use aterm_types::rfc3339::format_rfc3339;

#[cfg(test)]
#[path = "install_boot_tests.rs"]
mod boot_tests;

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "install_copy_tests.rs"]
mod copy_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "install_stage_probe_tests.rs"]
mod stage_probe_tests;

#[cfg(debug_assertions)]
#[path = "install_dmg_acceptance.rs"]
pub mod dmg_acceptance;
