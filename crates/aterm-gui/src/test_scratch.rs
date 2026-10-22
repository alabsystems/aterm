// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Scratch directories that live as long as the test process, not one test.
//!
//! A test-body scratch directory is an [`aterm_tempfile::TempDir`], gone when its
//! guard drops. Some scratch the process itself holds: the update-root override
//! every headless `App` installs once and never restores
//! ([`crate::prime_test_update_root`]), the parent of every `system_default()`
//! store's draft journals, and a handoff fixture's scratch, which the
//! process-wide `HOME` and `XDG_RUNTIME_DIR` name while the fixture lives (a
//! thread that read them just before the fixture put them back can re-create it
//! after its removal, `seamless::tests::ScratchEnv`).
//!
//! [`process_root`] puts such a directory in the test binary's own root
//! ([`aterm_uds::test_binary::scratch_root`]), which goes with it. Where there is
//! none (a `bench-support` build), and for [`short_process_root`]'s under `/tmp`,
//! it names `<base>/<prefix><pid>-<stamp>` and takes back the ones no live process
//! owns:
//!
//! - At exit (`exit(3)`, unix): this process's own roots, and those of every
//!   process that has died since, its child test processes among them.
//! - At first use of a prefix: the roots of every dead process, the runs killed
//!   or crashed before their exit handler ran.
//!
//! A root is taken back only when its owner is provably gone: its pid is ours
//! (ours is not created yet when the first-use sweep runs, and the exit sweep
//! is our own end), or [`crate::control_auth::pid_alive`] says no such process
//! exists. A recycled pid reads as alive and keeps a dead root one run longer;
//! nothing reads a live root as dead.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The roots this process has named under a shared base, by prefix: what
/// [`root_under`] answers again for a prefix it has seen, and what the exit handler
/// sweeps.
static ROOTS: Mutex<Vec<(&'static str, PathBuf)>> = Mutex::new(Vec::new());

/// This process's scratch root for `prefix`: the same path for every call with the
/// same prefix, not created (its first user creates it, with the mode it needs), and
/// removed when the process exits (module docs).
pub(crate) fn process_root(prefix: &'static str) -> PathBuf {
    match aterm_uds::test_binary::scratch_root() {
        Some(root) => root.join(prefix),
        None => root_under(std::env::temp_dir(), prefix),
    }
}

/// [`process_root`] under `/tmp`: for a directory the process holds that a unix
/// socket is bound in, short whatever `$TMPDIR` is.
#[cfg(test)]
pub(crate) fn short_process_root(prefix: &'static str) -> PathBuf {
    root_under(short_base(), prefix)
}

/// A directory for ONE test under [`process_root`], removed when the guard drops: a
/// thread the test cannot join may re-create it after that, and the root goes at exit.
#[cfg(test)]
pub(crate) fn outlived_dir(prefix: &'static str) -> aterm_tempfile::TempDir {
    let root = process_root(prefix);
    std::fs::create_dir_all(&root).expect("a process root");
    aterm_tempfile::TempDir::new_in(&root).expect("a scratch directory")
}

/// A fresh private directory for ONE test under `/tmp`, removed when the guard drops:
/// short enough for a unix socket's `sun_path` whatever `$TMPDIR` is.
#[cfg(test)]
#[track_caller]
pub(crate) fn short_dir(for_what: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::TempDir::new_in(short_base()).unwrap_or_else(|error| {
        aterm_spec::host::could_not_run(format!(
            "no directory for {for_what} could be made under {}: {error}",
            short_base().display()
        ))
    })
}

/// Where [`short_dir`] and [`short_process_root`] make their directories.
#[cfg(test)]
fn short_base() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/tmp")
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir()
    }
}

/// `<temp>/<prefix><pid>-<stamp>`, after taking back the ones dead processes left
/// there; the exit handler takes it back too.
fn root_under(temp: PathBuf, prefix: &'static str) -> PathBuf {
    let mut roots = ROOTS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, root)) = roots.iter().find(|(known, _)| *known == prefix) {
        return root.clone();
    }
    if roots.is_empty() {
        register_exit_sweep();
    }
    sweep(&temp, prefix);
    // The stamp makes the name unique per process instance: a recycled pid
    // never resolves to a dead run's root even if its sweep failed part-way.
    let stamp = aterm_time::since_epoch().unwrap_or_default().as_nanos();
    let root = temp.join(format!("{prefix}{}-{stamp:x}", std::process::id()));
    roots.push((prefix, root.clone()));
    root
}

/// Remove every `<prefix><pid>[-…]` directory directly under `temp` whose pid
/// is this process's or names no live process. Best-effort: a test run never
/// fails over housekeeping, so every error is discarded.
fn sweep(temp: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| owner_pid(name, prefix)) else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if pid == me || !crate::control_auth::pid_alive(pid) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The owner pid in a root name: `<prefix><pid>` followed by the end or a `-`
/// (an unstamped name is swept the same way).
fn owner_pid(name: &str, prefix: &str) -> Option<u32> {
    let rest = name.strip_prefix(prefix)?;
    let pid = rest.split_once('-').map_or(rest, |(pid, _)| pid);
    pid.parse().ok()
}

#[cfg(unix)]
fn register_exit_sweep() {
    /// Runs inside `exit(3)`. `try_lock`, never `lock`: a child forked while
    /// another thread held the lock inherits it held, and must exit anyway.
    extern "C" fn sweep_at_exit() {
        use std::sync::TryLockError;
        let roots = match ROOTS.try_lock() {
            Ok(roots) => roots,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        // Each root's own parent, not `$TMPDIR` now: a test may have moved it.
        for (prefix, root) in roots.iter() {
            if let Some(temp) = root.parent() {
                sweep(temp, prefix);
            }
        }
    }
    unsafe extern "C" {
        /// `stdlib.h`, declared inline as `crash_signal` declares it (the
        /// workspace's `libc` is `aterm-libc`, which does not carry it).
        fn atexit(cb: extern "C" fn()) -> std::ffi::c_int;
    }
    // SAFETY: registers a plain `extern "C"` function with no captured state;
    // a failed registration only leaves the roots to the next run's sweep.
    unsafe { atexit(sweep_at_exit) };
}

/// No exit hook off unix: the next run's first-use sweep takes the roots back.
#[cfg(not(unix))]
fn register_exit_sweep() {}
