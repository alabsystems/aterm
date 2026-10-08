// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Scratch directories that live as long as the TEST PROCESS, not one test.
//!
//! A test-body scratch directory is an [`aterm_tempfile::TempDir`]: it goes when
//! the test's guard drops. Some scratch cannot have a guard in a test body,
//! because the process holds it: the update-root override every headless `App`
//! installs once and never restores ([`crate::prime_test_update_root`]), the
//! parent the draft journals of every `system_default()` store hang under, and
//! a handoff fixture's scratch, which the process-wide `HOME` and
//! `XDG_RUNTIME_DIR` name while the fixture lives — a thread that read them
//! just before the fixture put them back can re-create it after its removal
//! (`seamless::tests::ScratchEnv`). The first two used to
//! be named by pid alone and removed by nobody, so every test run left them in
//! the shared temp dir (631 update roots there on 2026-09-30).
//!
//! [`process_root`] names such a directory `<prefix><pid>-<stamp>` and takes
//! back the ones no live process owns, at two moments:
//!
//! - **At exit** (`exit(3)`, unix): this process's own roots go, and so do
//!   those of every process that has died since — the child test processes it
//!   started among them.
//! - **At first use of a prefix**: the roots of every dead process go — the
//!   runs that were killed, aborted or crashed before their exit handler ran.
//!
//! A root is taken back only when its owner is provably gone: its pid is ours
//! (ours is not created yet when the first-use sweep runs, and the exit sweep
//! is our own end), or [`crate::control_auth::pid_alive`] says no such process
//! exists. A recycled pid reads as alive and keeps a dead root one run longer;
//! nothing reads a live root as dead.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The roots this process has named, by prefix: what [`process_root`] answers
/// again for a prefix it has seen, and what the exit handler sweeps.
static ROOTS: Mutex<Vec<(&'static str, PathBuf)>> = Mutex::new(Vec::new());

/// This process's scratch root for `prefix`, under the system temp dir: the
/// same path for every call with the same prefix, NOT created (its first user
/// creates it, with whatever mode that user needs), and removed when the
/// process exits. The first call for a prefix sweeps the roots dead processes
/// left under it.
pub(crate) fn process_root(prefix: &'static str) -> PathBuf {
    let mut roots = ROOTS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, root)) = roots.iter().find(|(known, _)| *known == prefix) {
        return root.clone();
    }
    if roots.is_empty() {
        register_exit_sweep();
    }
    let temp = std::env::temp_dir();
    sweep(&temp, prefix);
    // The stamp makes the name unique per process INSTANCE: a recycled pid
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

/// The owner pid in a root name: `<prefix><pid>` followed by the end or a `-`.
/// The bare `<prefix><pid>` form is the pre-stamp name, swept the same way.
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

#[cfg(test)]
mod tests {
    use super::owner_pid;

    #[test]
    fn the_owner_pid_is_read_from_the_stamped_and_the_bare_name() {
        let prefix = "aterm-test-update-root-";
        assert_eq!(
            owner_pid("aterm-test-update-root-4242-18da52", prefix),
            Some(4242)
        );
        assert_eq!(owner_pid("aterm-test-update-root-4242", prefix), Some(4242));
        assert_eq!(owner_pid("aterm-test-update-root-4242x", prefix), None);
        assert_eq!(owner_pid("aterm-test-update-root-", prefix), None);
        assert_eq!(owner_pid("aterm-test-drafts-4242-1", prefix), None);
    }
}
