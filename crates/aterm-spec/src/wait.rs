// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Bounded polls: the one loop behind every "wait until it holds" in the tests.
//!
//! A wait for something that must happen is a hang detector, never a latency budget: a correct run
//! leaves the moment the probe answers, and the budget only decides how long a wedged run takes to
//! be named. The default is [`MUST_HAPPEN`].
//!
//! [`within`] is the loop and never panics; [`until_within`] panics naming what it waited for.
//! [`first_exec`] pays the system's first-exec wait on a just-built binary once, unclocked, before
//! the launch a hang detector times. [`past_text_busy`] execs a just-written file past the
//! `ETXTBSY` a sibling test's fork can hold it in.
//!
//! Compiled out on `wasm32-unknown-unknown`, where `std::thread::sleep` and `Instant::now` panic.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The budget for a wait on something that must happen. A hang detector: a loaded host only slows
/// what is awaited, and a defect that never delivers still fails.
pub const MUST_HAPPEN: Duration = Duration::from_secs(60);

/// The gap [`until_within`] leaves between observations. Not a synchronisation
/// primitive: the observation is the assertion, and this only keeps the poll
/// from spinning a core.
const UNTIL_GAP: Duration = Duration::from_millis(10);

/// Probe until it answers `Some`, or `None` once `budget` has passed.
///
/// The probe runs at once, then again after every `gap`; the deadline is read
/// after each probe, so the last probe comes after the budget ran out and a
/// state that lands during the final gap still counts. A zero `gap` yields the
/// core between probes instead of sleeping.
pub fn within<T>(
    budget: Duration,
    gap: Duration,
    mut probe: impl FnMut() -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        if gap.is_zero() {
            std::thread::yield_now();
        } else {
            std::thread::sleep(gap);
        }
    }
}

/// [`within`], every 10 ms, failing the test when the budget runs out.
///
/// # Panics
///
/// After `budget`, naming `what` was being waited for.
#[track_caller]
pub fn until_within<T>(budget: Duration, what: &str, probe: impl FnMut() -> Option<T>) -> T {
    match within(budget, UNTIL_GAP, probe) {
        Some(value) => value,
        None => panic!("timed out after {budget:?} waiting for: {what}"),
    }
}

/// `spawn` (a `spawn`, `output` or `status` of a file this process JUST wrote or copied),
/// again for as long as the kernel refuses the exec as `ETXTBSY`, up to [`MUST_HAPPEN`].
///
/// A sibling test thread that forks while this one still holds the file open for writing
/// hands its child that descriptor, held until the child's own exec closes it, and Linux
/// refuses to exec a file open for writing anywhere. Writing to a temporary name and
/// renaming does not help: the child holds the inode, not the name. The window is only
/// that other child's fork-to-exec, and a refused exec ran nothing, so trying again is
/// safe; any other answer, and the last one, is returned as it came.
///
/// # Errors
///
/// What `spawn` returned last.
pub fn past_text_busy<T>(mut spawn: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let deadline = Instant::now() + MUST_HAPPEN;
    loop {
        match spawn() {
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            done => return done,
        }
    }
}

/// How long [`first_exec`]'s run may take before it is killed: a hang detector of its own. The
/// system's first-exec wait is long under load: a fresh 167 MB debug `aterm-gui` took 64 s to its
/// first exec (0.13 s to its next), and a fresh two-line script up to 600 s while syspolicyd was
/// backed up.
pub const FIRST_EXEC: Duration = Duration::from_secs(10 * 60);

/// Runs the executable at `path` once, unclocked, right before a caller launches it under a hang
/// detector.
///
/// macOS assesses an executable the first time it runs and caches the verdict per inode, and that
/// first exec can outlast [`MUST_HAPPEN`]. It cannot be paid at build time: cargo copies each
/// binary into `<target>/debug` on every invocation, rebuilt or not, and the new inode is assessed
/// again. So this runs the file at `path` now, the one the caller starts next.
///
/// Once per file per process, keyed by (device, inode), off Unix by path, length and mtime. A
/// caller that finds the file being run waits for that run; a file replaced since is run again.
///
/// `args` must make the program print and exit before any setup (a `--help` or `--version` read
/// off its `main`). Stdio is null and the environment is `PATH` plus `HOME` and `TMPDIR` in a
/// scratch directory (the temp dir when one cannot be made) removed afterwards, so no `ATERM_*`
/// authority and nothing of the person's reaches it. A run still going at [`FIRST_EXEC`] is
/// killed.
///
/// It never panics: a missing path is a no-op, and a failed spawn or nonzero exit is the launch's
/// to meet.
pub fn first_exec(path: &Path, args: &[&str]) {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    static RUN: Mutex<BTreeMap<FileId, Arc<OnceLock<()>>>> = Mutex::new(BTreeMap::new());

    let Some(id) = file_id(path) else {
        return;
    };
    let once = Arc::clone(
        RUN.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(id)
            .or_default(),
    );
    once.get_or_init(|| run_once(path, args, FIRST_EXEC));
}

/// What [`first_exec`] keys a file by: the identity macOS caches its verdict
/// under.
#[cfg(unix)]
type FileId = (u64, u64);
#[cfg(not(unix))]
type FileId = (PathBuf, u64, Option<std::time::SystemTime>);

/// The file at `path` as [`first_exec`] keys it; `None` when there is no file.
#[cfg(unix)]
fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).ok()?;
    meta.is_file().then(|| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(path: &Path) -> Option<FileId> {
    let meta = std::fs::metadata(path).ok()?;
    let at = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    meta.is_file()
        .then(|| (at, meta.len(), meta.modified().ok()))
}

/// [`first_exec`]'s one run: `path` with `args` in a scratch environment,
/// killed once `bound` has passed. Built by aterm-uds's `spawnfd::command`, so
/// the child holds its (null) stdio and nothing else of this process.
// Skip: spawns a subprocess. Test tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn run_once(path: &Path, args: &[&str], bound: Duration) {
    use aterm_uds::spawnfd::Stdio;
    let scratch = scratch_dir();
    let home = scratch.clone().unwrap_or_else(std::env::temp_dir);
    let mut cmd = aterm_uds::spawnfd::command(path);
    cmd.args(args).env_clear();
    if let Some(search) = std::env::var_os("PATH") {
        cmd.env("PATH", search);
    }
    cmd.env("HOME", &home)
        .env("TMPDIR", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        let ended = within(bound, UNTIL_GAP, || match child.try_wait() {
            Ok(None) => None,
            Ok(Some(_)) | Err(_) => Some(()),
        });
        if ended.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    if let Some(dir) = scratch {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A new directory under the temp dir for one [`first_exec`] run, or `None`
/// when it cannot be made.
fn scratch_dir() -> Option<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "aterm-first-exec-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).ok().map(|()| dir)
}
