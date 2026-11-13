// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS `syspolicyd` thread-pool reader for [`aterm_vitals::program_check`].

use std::time::Duration;

pub use aterm_vitals::program_check::{Daemon, Reading};

/// `syspolicyd`'s image.
pub const SYSPOLICYD: &str = "/usr/libexec/syspolicyd";

/// How long one `ps` may take before it is killed.
pub const PS_HANG: Duration = Duration::from_secs(10);

/// Reaping reserve after killing our timed-out `ps`. A child the kernel
/// still holds goes to the reaper, so it cannot stop later pool readings.
#[cfg(target_os = "macos")]
const PS_REAP: Duration = Duration::from_millis(250);

/// The `syspolicyd` running now: the first live root process named `syspolicyd` whose image is
/// [`SYSPOLICYD`], with its pid and start (the kernel's `p_start`, the clock a held process's
/// fork is read on, [`aterm_sysprobe::jobs::StartFacts::start_us`]). [`Look::NoneRunning`] when
/// none is; [`Look::Unread`] when the process table cannot be read, or none was found but a
/// candidate's image could not be read. No argument of any process is read.
#[cfg(target_os = "macos")]
pub fn running() -> Result<Daemon, Look> {
    let records = aterm_sysprobe::kproc::records().ok_or(Look::Unread)?;
    let mut unread = false;
    for r in records
        .into_iter()
        .filter(|r| r.uid == 0 && r.comm == "syspolicyd" && !r.zombie)
    {
        match aterm_sysprobe::process_path(r.pid) {
            Some(path) if path == std::path::Path::new(SYSPOLICYD) => {
                return Ok(Daemon {
                    pid: r.pid,
                    start_s: r.start_s,
                    start_us: r.start_us,
                });
            }
            Some(_) => {}
            None => unread = true,
        }
    }
    Err(if unread {
        Look::Unread
    } else {
        Look::NoneRunning
    })
}

/// See the macOS twin: there is no program check to read here, so [`Look::Unread`].
#[cfg(not(target_os = "macos"))]
pub fn running() -> Result<Daemon, Look> {
    Err(Look::Unread)
}

/// What one look at the program check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// The pool, read.
    Read(Reading),
    /// No `syspolicyd` runs (launchd starts one at the next request).
    NoneRunning,
    /// Nothing could be read: off macOS, the process table or a candidate's image could not be
    /// read, or `ps` failed, printed no pool line, or outlived its hang bound.
    Unread,
}

/// One look at `syspolicyd`'s pool.
#[must_use]
pub fn look(hang: Duration) -> Look {
    #[cfg(target_os = "macos")]
    {
        look_and_ps(hang).0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = hang;
        Look::Unread
    }
}

/// The `ps` one look ran: the daemon it ran on, and its [`pool_output`] (`None` when that `ps`
/// did not start or outlived the look's hang bound).
#[cfg(target_os = "macos")]
type PoolRun = (Daemon, Option<std::process::Output>);

/// [`look`]'s one look, with the `ps` it ran: the daemon now running, one [`pool_output`] of it
/// under `hang`, and what that output reads as ([`reading_of`]). The run is `None` when no daemon
/// read, and then the look is what [`running`] found instead.
#[cfg(target_os = "macos")]
fn look_and_ps(hang: Duration) -> (Look, Option<PoolRun>) {
    let daemon = match running() {
        Ok(daemon) => daemon,
        Err(missed) => return (missed, None),
    };
    let out = pool_output(daemon.pid, hang);
    let look = out
        .as_ref()
        .and_then(|out| reading_of(daemon, out))
        .map_or(Look::Unread, Look::Read);
    (look, Some((daemon, out)))
}

/// One [`pool_ps`] of `pid` under `hang`: what it printed and how it ended, or `None` when it did
/// not start or outlived `hang`.
#[cfg(target_os = "macos")]
fn pool_output(pid: u32, hang: Duration) -> Option<std::process::Output> {
    crate::deadline::outputs_until(
        &mut [&mut pool_ps(pid)],
        crate::deadline::saturating_after(std::time::Instant::now(), hang),
        PS_REAP,
    )
    .into_iter()
    .next()?
    .ok()
}

/// What `out`, one [`pool_ps`] of `daemon`, reads as: its one pool line, with the kernel's
/// constrained limit beside it, when `ps` exited 0; `None` for a `ps` that failed or a line
/// [`parse`] refuses, the kernel's refusal `- -` among them.
#[cfg(target_os = "macos")]
fn reading_of(daemon: Daemon, out: &std::process::Output) -> Option<Reading> {
    if !out.status.success() {
        return None;
    }
    let (workers, at_limit) = parse(&String::from_utf8_lossy(&out.stdout))?;
    Some(Reading {
        daemon,
        workers,
        at_limit,
        limit: aterm_sysprobe::workqueue_limits().constrained,
    })
}

/// The `ps` that reads `pid`'s pool: exit 0 and one line, `<workers> <limit flags>`, or `- -` when
/// the kernel refuses the read. A keyword this `ps` does not know, or a pid that runs no process,
/// exits 1. A program, not an in-process read: `/bin/ps` is setuid root, and libproc answers an
/// unprivileged read of a root process's workqueue `EPERM` (M29 of
/// `docs/DESIGN-leftover-processes-2026-10-16.md`); `-p` with no argument column does not hang in
/// the wedge it reads.
#[cfg(target_os = "macos")]
fn pool_ps(pid: u32) -> crate::spawnfd::Command {
    let mut cmd = crate::spawnfd::command("/bin/ps");
    cmd.args(["-o", "wq=,wql=", "-p", &pid.to_string()])
        .env("LC_ALL", "C");
    cmd
}

/// `ps -o wq=,wql=`'s one line.
#[must_use]
pub fn parse(out: &str) -> Option<(u64, bool)> {
    let mut lines = out.lines().filter(|l| !l.trim().is_empty());
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let mut words = line.split_whitespace();
    let workers = words
        .next()
        .filter(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()))?
        .parse::<u64>()
        .ok()?;
    let limit = words.next()?;
    if words.next().is_some() {
        return None;
    }
    let at_limit = match limit {
        "-" => false,
        word if !word.is_empty() && word.chars().all(|c| c == 'C' || c == 'T') => true,
        _ => return None,
    };
    Some((workers, at_limit))
}

#[cfg(test)]
#[path = "program_check_tests.rs"]
mod tests;
