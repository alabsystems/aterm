// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Scratch directories that live as long as the test process, not one test.
//!
//! [`process_root`] places directories under [`aterm_uds::test_binary::scratch_root`] (or
//! `<temp>/<prefix><pid>-<stamp>` when none exists or for [`short_process_root`]) and sweeps
//! directories whose owner PID is gone at first use and `exit(3)`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

static ROOTS: Mutex<Vec<(&'static str, PathBuf)>> = Mutex::new(Vec::new());

/// Process-lifetime scratch path for `prefix` (created by its first user and swept at exit).
pub(crate) fn process_root(prefix: &'static str) -> PathBuf {
    match aterm_uds::test_binary::scratch_root() {
        Some(root) => root.join(prefix),
        None => root_under(std::env::temp_dir(), prefix),
    }
}

/// [`process_root`] under `/tmp`, short enough for a Unix socket's `sun_path`.
#[cfg(test)]
pub(crate) fn short_process_root(prefix: &'static str) -> PathBuf {
    root_under(short_base(), prefix)
}

/// Per-test directory under [`process_root`] for tests with unjoined background threads.
#[cfg(test)]
pub(crate) fn outlived_dir(prefix: &'static str) -> aterm_tempfile::TempDir {
    let root = process_root(prefix);
    std::fs::create_dir_all(&root).expect("a process root");
    aterm_tempfile::TempDir::new_in(&root).expect("a scratch directory")
}

/// Write `text` as an `aterm.toml` that stands in for the person's own: owner-only (`0600`, as
/// the config rule's refusal asks), whatever the umask. A plain write under a user-private-group
/// login's umask `002` (Ubuntu's default) leaves it `664`, which the rule every reader of
/// `aterm.toml` holds (`aterm_types::dirs::aterm_config_refusal`) refuses, so the test would read
/// no config at all.
#[cfg(test)]
#[track_caller]
pub(crate) fn write_config(path: &Path, text: impl AsRef<[u8]>) {
    std::fs::write(path, text).expect("write the config");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("make the config owner-only");
    }
}

/// Per-test directory under `/tmp`, short enough for a Unix socket's `sun_path`.
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

fn root_under(temp: PathBuf, prefix: &'static str) -> PathBuf {
    let mut roots = ROOTS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, root)) = roots.iter().find(|(known, _)| *known == prefix) {
        return root.clone();
    }
    if roots.is_empty() {
        register_exit_sweep();
    }
    sweep(&temp, prefix);
    let stamp = aterm_time::since_epoch().unwrap_or_default().as_nanos();
    let root = temp.join(format!("{prefix}{}-{stamp:x}", std::process::id()));
    roots.push((prefix, root.clone()));
    root
}

/// Remove `<prefix><pid>[-…]` directories under `temp` owned by this process or a dead PID.
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

fn owner_pid(name: &str, prefix: &str) -> Option<u32> {
    let rest = name.strip_prefix(prefix)?;
    let pid = rest.split_once('-').map_or(rest, |(pid, _)| pid);
    pid.parse().ok()
}

#[cfg(unix)]
fn register_exit_sweep() {
    /// Uses `try_lock` so a forked child that inherited a held lock still exits cleanly.
    extern "C" fn sweep_at_exit() {
        use std::sync::TryLockError;
        let roots = match ROOTS.try_lock() {
            Ok(roots) => roots,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        for (prefix, root) in roots.iter() {
            if let Some(temp) = root.parent() {
                sweep(temp, prefix);
            }
        }
    }
    unsafe extern "C" {
        fn atexit(cb: extern "C" fn()) -> std::ffi::c_int;
    }
    // SAFETY: registers a plain `extern "C"` function with no captured state;
    // a failed registration only leaves the roots to the next run's sweep.
    unsafe { atexit(sweep_at_exit) };
}

/// No exit hook off unix: the next run's first-use sweep takes the roots back.
#[cfg(not(unix))]
fn register_exit_sweep() {}
