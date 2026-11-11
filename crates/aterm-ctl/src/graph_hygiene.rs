// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Discovery graph hygiene: dead-writer pruning and foreign-socket filtering
//! ([`crate::fleet_sessions`], [`prune_dead_entry`], [`writer_gone`], [`foreign_token_miss`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aterm_uds::process::ProcessBirth;

/// How far a writer's recorded birth may trail its entry's mtime and still be its writer.
#[cfg(any(not(any(target_os = "linux", target_os = "android")), test))]
pub(crate) const WRITE_SLACK: Duration = Duration::from_secs(2);

/// `WRITE_SLACK` for a boot-clock birth placed on the wall clock ([`read_birth`] on Linux).
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) const PLACED_SLACK: Duration = Duration::from_secs(60);

/// A live process's start on the wall clock ([`read_birth`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Birth {
    pub(crate) at: ProcessBirth,
    pub(crate) slack: Duration,
}

/// The clocks [`read_birth`] places a Linux boot-clock start on.
#[derive(Debug, Default)]
pub(crate) struct Clocks {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    paired: std::cell::OnceCell<Option<(SystemTime, aterm_uds::process::BootClock)>>,
}

/// When the live process `pid` of this uid started, on the wall clock.
pub(crate) fn read_birth(pid: i32, clocks: &Clocks) -> Option<Birth> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let start = aterm_uds::process::read_process_start(pid)?;
        let (wall, boot) = (*clocks.paired.get_or_init(|| {
            let boot = aterm_uds::process::boot_clock_now()?;
            Some((SystemTime::now(), boot))
        }))?;
        place_on_wall(start.boot, wall, boot).map(|at| Birth {
            at,
            slack: PLACED_SLACK,
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let Clocks {} = clocks;
        aterm_uds::process::read_process_birth(pid).map(|at| Birth {
            at,
            slack: WRITE_SLACK,
        })
    }
}

/// A boot-clock start (Linux nanoseconds) on the wall clock.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
pub(crate) fn place_on_wall(
    start: aterm_uds::process::BootClock,
    wall: SystemTime,
    boot: aterm_uds::process::BootClock,
) -> Option<ProcessBirth> {
    let age = Duration::from_nanos(boot.0.checked_sub(start.0)?);
    let born = wall.checked_sub(age)?.duration_since(UNIX_EPOCH).ok()?;
    Some(ProcessBirth {
        seconds: born.as_secs(),
        microseconds: u64::from(born.subsec_micros()),
    })
}

/// Whether the live process born at `birth` wrote a graph entry naming its pid.
pub(crate) fn is_entry_writer(
    birth: ProcessBirth,
    slack: Duration,
    written: SystemTime,
) -> Option<bool> {
    let born = UNIX_EPOCH
        .checked_add(Duration::from_secs(birth.seconds))?
        .checked_add(Duration::from_micros(birth.microseconds))?;
    Some(born <= written.checked_add(slack)?)
}

/// Whether the instance that wrote a graph entry naming `pid` (non-zero) is gone.
pub(crate) fn writer_gone(pid: u32, written: Option<SystemTime>) -> bool {
    if !aterm_uds::process::pid_alive(pid) {
        return true;
    }
    let birth = i32::try_from(pid)
        .ok()
        .and_then(|pid| read_birth(pid, &Clocks::default()));
    matches!(
        (birth, written),
        (Some(birth), Some(written))
            if is_entry_writer(birth.at, birth.slack, written) == Some(false)
    )
}

/// A graph entry's body and when it was last written, from one open.
pub(crate) fn read_entry(path: &Path) -> std::io::Result<(String, Option<SystemTime>)> {
    crate::read_bounded_regular_utf8(path, crate::MAX_GRAPH_ENTRY_BYTES)
}

/// Remove the graph entry at `entry`.
pub(crate) fn prune_dead_entry(entry: &Path, seen: &str) -> bool {
    let (Some(dir), Some(name)) = (entry.parent(), entry.file_name()) else {
        return false;
    };
    if !std::fs::symlink_metadata(entry).is_ok_and(|m| m.file_type().is_file()) {
        return false;
    }
    let mut aside_name = std::ffi::OsString::from(".prune-");
    aside_name.push(name);
    aside_name.push(format!("-{}", std::process::id()));
    let aside = dir.join(aside_name);
    if std::fs::rename(entry, &aside).is_err() {
        return false;
    }
    let unchanged = crate::read_bounded_regular_utf8(&aside, crate::MAX_GRAPH_ENTRY_BYTES)
        .is_ok_and(|(now, _)| now == seen);
    if !unchanged {
        // Rewritten under us: restore it, unless a newer entry already holds
        // the name (then that one is the live answer and ours is older).
        let _ = std::fs::hard_link(&aside, entry);
    }
    let _ = std::fs::remove_file(&aside);
    unchanged
}

/// `Some(token_path)` when the instance at `sock` is foreign to the rendezvous directory.
pub(crate) fn foreign_token_miss(dir: &Path, sock: &str) -> Option<PathBuf> {
    if inside(dir, Path::new(sock)) {
        return None;
    }
    crate::read_token_at(sock).err().map(|(path, _)| path)
}

/// Whether `sock`'s parent directory is `dir`, through any symlinked ancestor
/// (macOS `/var` → `/private/var`).
fn inside(dir: &Path, sock: &Path) -> bool {
    let Some(parent) = sock.parent() else {
        return false;
    };
    match (std::fs::canonicalize(parent), std::fs::canonicalize(dir)) {
        (Ok(a), Ok(b)) => a == b,
        _ => parent == dir,
    }
}

#[cfg(all(test, unix))]
#[path = "graph_hygiene_tests.rs"]
mod tests;
