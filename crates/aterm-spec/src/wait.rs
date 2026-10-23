// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Bounded polling helpers and hang detectors for tests.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Default hang-detector timeout for conditions that must hold.
pub const MUST_HAPPEN: Duration = Duration::from_secs(60);

/// Lifetime in seconds for a parked test stand-in (`2 * MUST_HAPPEN`).
pub const PARKED_SECS: u64 = 2 * MUST_HAPPEN.as_secs();

/// The gap [`until_within`] leaves between observations. Not a synchronisation
/// primitive: the observation is the assertion, and this only keeps the poll
/// from spinning a core.
const UNTIL_GAP: Duration = Duration::from_millis(10);

/// Polls `probe` every `interval` until it returns `Some` or `budget` elapses.
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

/// Polls `probe` every 10 ms until it returns `Some`, or panics after `budget`.
///
/// # Panics
/// Panics if `budget` elapses before `probe` returns `Some`, naming `what`.
#[track_caller]
pub fn until_within<T>(budget: Duration, what: &str, probe: impl FnMut() -> Option<T>) -> T {
    match within(budget, UNTIL_GAP, probe) {
        Some(value) => value,
        None => panic!("timed out after {budget:?} waiting for: {what}"),
    }
}

/// Retries `spawn` while the kernel returns `ETXTBSY`, up to [`MUST_HAPPEN`].
///
/// # Errors
/// Returns the last error produced by `spawn`.
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

/// Timeout for [`first_exec`]'s unclocked warm-up run.
pub const FIRST_EXEC: Duration = Duration::from_secs(10 * 60);

/// Runs `path` once outside timed test budgets to warm macOS first-exec policy checks.
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
/// killed once `bound` has passed, and one line on stderr when macOS's
/// program check still held it then.
#[cfg_attr(trust_verify, trust::skip)]
fn run_once(path: &Path, args: &[&str], bound: Duration) {
    run_once_judged(
        path,
        args,
        bound,
        &|pid| aterm_uds::process::at_program_check(i32::try_from(pid).unwrap_or(0)),
        &mut |line| eprintln!("{line}"),
    );
}

/// [`run_once`], with `held` asked whether the run is held at macOS's
/// program check before it is killed at `bound`, and `say` given the one
/// line that says so. It never panics: a program held before its `main`
/// cannot be the code's hang.
#[cfg_attr(trust_verify, trust::skip)]
fn run_once_judged(
    path: &Path,
    args: &[&str],
    bound: Duration,
    held: &dyn Fn(u32) -> Option<bool>,
    say: &mut dyn FnMut(String),
) {
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
            let pid = child.id();
            let was_held = held(pid) == Some(true);
            aterm_uds::deadline::kill_within(&mut child, MUST_HAPPEN);
            if was_held {
                say(format!(
                    "first_exec: macOS's program check held {} (pid {pid}) for the whole \
                     {bound:?} warm-up; it was ended unrun",
                    path.display()
                ));
            }
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A warm-up killed at its bound while the program check holds it says so in one line
    /// naming its pid, and one that is not held says nothing. (`sleep 30` outlives the 50 ms
    /// bound by construction.) The reader is asked while the run still runs: the fake answers
    /// only while the program check's own reader still reads the pid, which on macOS a killed
    /// and reaped child no longer does.
    #[test]
    fn a_warm_up_killed_while_held_says_so_once() {
        let sleep = Path::new("/bin/sleep");
        let bound = Duration::from_millis(50);
        let asked = std::cell::Cell::new(None);
        let mut lines = Vec::new();
        run_once_judged(
            sleep,
            &["30"],
            bound,
            &|pid| {
                asked.set(Some(pid));
                aterm_uds::process::at_program_check(i32::try_from(pid).unwrap_or(0)).map(|_| true)
            },
            &mut |line| lines.push(line),
        );
        let pid = asked.get().expect("the killed run was asked about");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("first_exec: macOS's program check held /bin/sleep")
                && lines[0].contains(&format!("(pid {pid})")),
            "{lines:?}"
        );
        let mut quiet = Vec::new();
        run_once_judged(sleep, &["30"], bound, &|_| Some(false), &mut |line| {
            quiet.push(line);
        });
        assert_eq!(quiet, Vec::<String>::new());
    }
}
