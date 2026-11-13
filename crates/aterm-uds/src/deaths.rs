// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A debug build's journal of the children it reaps that a signal ended. With
//! `ATERM_DEBUG_DEATH_JOURNAL` naming a file, every reap that hands its wait status to [`note`]
//! (a `spawnfd` child's wait, with or without resource accounting, the process-wide reaper's,
//! aterm-pty's shell reaps and the PTY keeper's) appends one line to it for a child a signal
//! ended. A release build never reads that variable or writes that journal.
//!
//! It is the lab's witness of a helper a jail killed (`aterm-lab local-signins`): the kernel
//! hands each child's status to its parent's wait however briefly the child lived, so a reap
//! that notes it cannot miss a short-lived one. macOS offers no kernel-wide witness without
//! privileges: kqueue's `NOTE_TRACK` (follow a process's forks) answers `ENOTSUP`, there is no
//! subreaper, and a scan of the process table misses a child that lived between two looks. What
//! it does not see is a death no reap here takes: a child some other program (a shell) reaps,
//! or an orphan launchd reaps. When signal provenance is active, each observed signal death
//! also goes to that recorder, independently of the debug journal variable.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// The variable naming the journal, read by a debug build alone.
pub const SEAM: &str = "ATERM_DEBUG_DEATH_JOURNAL";

/// The signal that ended a process whose wait status is `raw`; `None` for an exit, a stop or a
/// continue.
#[must_use]
pub const fn signal_of(raw: i32) -> Option<i32> {
    if libc::WIFSIGNALED(raw) {
        Some(libc::WTERMSIG(raw))
    } else {
        None
    }
}

/// Note that this process reaped `pid` with wait status `raw`: one journal line when a signal
/// ended it and the journal is named. An active signal-origin recorder also receives it.
/// A journal it cannot write is no reap's concern, so it never fails.
pub fn note(pid: i32, raw: i32) {
    let Some(signal) = signal_of(raw) else {
        return;
    };
    crate::signal_origin::child_death(pid, signal);
    if let Some(path) = journal() {
        let _ = append(&path, pid, signal);
    }
}

/// The journal's line for `pid`, which this process reaped dead by `signal`: the reaper's pid
/// and executable name, then the child's pid and the signal.
#[must_use]
pub fn line(reaper: u32, reaper_name: &str, pid: i32, signal: i32) -> String {
    format!("{reaper} {reaper_name} reaped {pid} signal {signal}\n")
}

/// Append [`line`] to `path` in one write (`O_APPEND`, so lines from many processes never
/// interleave), creating it `0600`, never through a final symlink.
fn append(path: &Path, pid: i32, signal: i32) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let name = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "?".into());
    let text = line(std::process::id(), &name, pid, signal);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?
        .write_all(text.as_bytes())
}

#[cfg(test)]
thread_local! {
    static FORCED: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// `f` with the journal at `path` on this thread; this crate's own tests read no environment, so
/// no journal is named in them elsewhere.
#[cfg(test)]
pub(crate) fn forced<R>(path: Option<PathBuf>, f: impl FnOnce() -> R) -> R {
    let was = FORCED.with(|forced| forced.replace(path));
    let ran = f();
    FORCED.with(|forced| *forced.borrow_mut() = was);
    ran
}

fn journal() -> Option<PathBuf> {
    #[cfg(test)]
    {
        FORCED.with(|forced| forced.borrow().clone())
    }
    #[cfg(all(debug_assertions, not(test)))]
    {
        std::env::var_os(SEAM)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        None
    }
}

#[cfg(test)]
#[path = "deaths_tests.rs"]
mod tests;
