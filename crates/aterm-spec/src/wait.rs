// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Bounded polls: the one loop behind every "wait until it holds" in the tests.
//!
//! A wait for something that MUST happen is a hang detector, never a latency
//! budget (`AGENTS.md`): a correct run leaves the moment the probe answers, and
//! the budget only decides how long a wedged run takes to be named. So the
//! default budget is [`MUST_HAPPEN`], a minute, and a caller that needs a
//! different one states it.
//!
//! [`within`] is the loop and never panics; [`until_within`] is its panicking
//! form, naming what was being waited for. A caller with a failure message of
//! its own (the last reading, a context dump) builds it on [`within`].
//!
//! A hang detector on a just-built binary's launch also meets the system's
//! first-exec wait, which is no part of what it detects: [`first_exec`] pays
//! that wait once, unclocked, right before the launch.
//!
//! Compiled out on `wasm32-unknown-unknown`, where `std::thread::sleep` and
//! `std::time::Instant::now` panic.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The budget for a wait on something that must happen: a minute, a hang
/// detector (`AGENTS.md`). A loaded host only slows what is awaited; a defect
/// that never delivers still fails, at the minute.
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

/// How long [`first_exec`]'s one run may take before it is killed: ten
/// minutes, a hang detector of its own, never a latency budget. The wait it
/// absorbs is the system's, and long under load: measured 2026-10-07/08, a
/// freshly built 167 MB debug `aterm-gui` took 64 s to its first exec and
/// 0.13 s to its next, and a fresh two-line script's first run took 103 s to
/// 600 s while syspolicyd was backed up.
pub const FIRST_EXEC: Duration = Duration::from_secs(10 * 60);

/// RUN THE EXECUTABLE AT `path` ONCE, UNCLOCKED, RIGHT BEFORE A CALLER
/// LAUNCHES IT UNDER A HANG DETECTOR.
///
/// macOS assesses an executable file the first time it runs and caches the
/// verdict per inode, and that first exec can take a minute ([`FIRST_EXEC`]
/// has the measurements). A test that launches a just-built binary and then
/// waits a minute for it ([`MUST_HAPPEN`]) went red on that wait, not on a
/// defect. The wait cannot be paid at build time: on this Mac cargo COPIES
/// each binary into `<target>/debug` on every invocation, rebuilt or not
/// (measured 2026-10-08: two no-op builds, two inodes; a re-copied binary
/// that had already run took 4.3 s and 13 s again, its same inode 20 ms), so
/// a run before the last cargo invocation warms a file that is gone by the
/// launch. This runs at the launch: the file at `path` now, which is the one
/// the caller starts next.
///
/// ONCE PER FILE PER PROCESS, keyed by (device, inode) — off Unix by the
/// path, length and modification time. A caller that finds the file being
/// run waits for that run instead of starting its own; a file cargo replaced
/// since is a new key and is run again.
///
/// `args` must make the program print and exit before any setup: a `--help`
/// or a `--version` read off its `main`. Every stdio is null, and the
/// environment is `PATH` alone plus `HOME` and `TMPDIR` in a scratch
/// directory of its own under the temp dir (the temp dir itself when one
/// cannot be made), removed afterwards: no `ATERM_*` session, capability,
/// handoff or update authority, and nothing of the person's, reaches it. A
/// run still going at [`FIRST_EXEC`] is killed.
///
/// It decides nothing and never panics: a path that does not exist is no
/// call at all, and a spawn that fails or an exit that is not 0 is the
/// launch's to meet. The caller keeps its hang detector.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_answers_the_first_some_and_none_after_the_budget() {
        let mut probes = 0;
        let got = within(MUST_HAPPEN, Duration::ZERO, || {
            probes += 1;
            (probes == 3).then_some(probes)
        });
        assert_eq!(got, Some(3));

        // A zero budget still probes once, and gives up after it.
        let mut probes = 0;
        let got: Option<()> = within(Duration::ZERO, Duration::from_millis(1), || {
            probes += 1;
            None
        });
        assert_eq!((got, probes), (None, 1));
    }

    #[test]
    #[should_panic(expected = "timed out after 0ns waiting for: the never")]
    fn until_within_names_what_it_waited_for() {
        until_within::<()>(Duration::ZERO, "the never", || None);
    }

    /// ONE RUN PER FILE, IN A SCRATCH ENVIRONMENT: a second call for the same
    /// file runs nothing; the run saw `PATH`, a scratch `HOME` and `TMPDIR`
    /// and its shell's own variables, nothing else, and the scratch is gone
    /// after it; a path that does not exist is no call at all. The program is
    /// `/bin/sh`, not a fresh stand-in, so the test waits on no first-exec
    /// assessment ([`FIRST_EXEC`]: a fresh script's took up to 600 s). No
    /// other test here hands `/bin/sh` to [`first_exec`], whose memory is the
    /// process's.
    #[cfg(unix)]
    #[test]
    fn first_exec_runs_a_file_once_in_a_scratch_environment() {
        use std::collections::BTreeSet;
        let dir = aterm_tempfile::tempdir().expect("a scratch dir");
        let ran = dir.path().join("ran");
        let ran = ran.to_str().expect("a UTF-8 scratch path");
        let record = r#"{ echo run; env; } >> "$0""#;
        first_exec(Path::new("/bin/sh"), &["-c", record, ran]);
        first_exec(Path::new("/bin/sh"), &["-c", record, ran]);

        let text = std::fs::read_to_string(ran).expect("the first call ran it");
        let runs = text.lines().filter(|l| *l == "run").count();
        assert_eq!(runs, 1, "one run for one file:\n{text}");
        let value = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                .unwrap_or_else(|| panic!("no {key} in:\n{text}"))
        };
        // What `sh` itself exports to the `env` it runs.
        let shell_own = ["PWD", "OLDPWD", "SHLVL", "_"];
        let names: BTreeSet<&str> = text
            .lines()
            .filter_map(|l| l.split_once('=').map(|(name, _)| name))
            .filter(|name| !shell_own.contains(name))
            .collect();
        assert_eq!(
            names,
            BTreeSet::from(["HOME", "PATH", "TMPDIR"]),
            "the whole environment:\n{text}"
        );
        let home = Path::new(value("HOME"));
        assert_eq!(value("TMPDIR"), value("HOME"));
        assert!(
            home.starts_with(std::env::temp_dir()) && home != std::env::temp_dir(),
            "a scratch HOME of its own: {}",
            home.display()
        );
        assert!(!home.exists(), "removed after: {}", home.display());

        let absent = dir.path().join("absent");
        first_exec(&absent, &["--help"]);
        assert_eq!(file_id(&absent), None);

        // A file replaced by rename (cargo re-copies a binary on every
        // invocation) is a new key, so it is warmed again.
        let binary = dir.path().join("binary");
        std::fs::write(&binary, "same bytes").unwrap();
        let before = file_id(&binary).expect("a file");
        let next = dir.path().join("next");
        std::fs::write(&next, "same bytes").unwrap();
        std::fs::rename(&next, &binary).unwrap();
        assert_ne!(
            file_id(&binary),
            Some(before),
            "a replaced file is a new key"
        );
        assert!(!absent.exists());
    }
}
