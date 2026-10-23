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

/// The `syspolicyd` running now ([`SYSPOLICYD`], [`Look::NoneRunning`], [`Look::Unread`]).
#[cfg(target_os = "macos")]
fn daemon() -> Result<Daemon, Look> {
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
        match daemon() {
            Ok(daemon) => read_daemon(daemon, hang).map_or(Look::Unread, Look::Read),
            Err(missed) => missed,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = hang;
        Look::Unread
    }
}

/// One `ps` of `daemon`'s pool, under `hang`.
#[cfg(target_os = "macos")]
fn read_daemon(daemon: Daemon, hang: Duration) -> Option<Reading> {
    let mut cmd = crate::spawnfd::command("/bin/ps");
    cmd.args(["-o", "wq=,wql=", "-p", &daemon.pid.to_string()])
        .env("LC_ALL", "C");
    let out =
        crate::deadline::outputs_until(&mut [&mut cmd], std::time::Instant::now() + hang, PS_REAP)
            .into_iter()
            .next()?
            .ok()?;
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

/// `ps -o wq=,wql=`'s one line.
#[must_use]
pub fn parse(out: &str) -> Option<(u64, bool)> {
    let mut lines = out.lines().filter(|l| !l.trim().is_empty());
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let mut words = line.split_whitespace();
    let workers = words.next()?.parse::<u64>().ok()?;
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
mod tests {
    use super::*;

    #[test]
    fn parse_reads_ps_lines_and_refuses_anything_else() {
        assert_eq!(parse("    1   -\n"), Some((1, false)));
        assert_eq!(parse("   95   C\n"), Some((95, true)));
        assert_eq!(parse("  512   T\n"), Some((512, true)));
        assert_eq!(parse("  512   CT\n"), Some((512, true)));
        assert_eq!(parse(""), None);
        assert_eq!(parse("WQ WQL\n 1 -\n"), None);
        assert_eq!(parse(" 1 -\n 2 -\n"), None);
        assert_eq!(parse(" x -\n"), None);
        assert_eq!(parse(" 1 ?\n"), None);
        assert_eq!(parse(" 1\n"), None);
        assert_eq!(parse(" 1 - 3\n"), None);
    }

    /// The live daemon reads as a root `syspolicyd` whose pool `ps` reads
    /// unprivileged, with the kernel's limit beside it.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_live_syspolicyd_reads_through_ps() {
        let daemon = match daemon() {
            Ok(daemon) => daemon,
            Err(Look::NoneRunning) => return,
            Err(missed) => panic!("the process table reads: {missed:?}"),
        };
        assert!(daemon.pid > 1);
        let Look::Read(reading) = look(Duration::from_secs(60)) else {
            panic!("ps reads syspolicyd's pool");
        };
        assert_eq!(reading.daemon.pid, daemon.pid);
        assert!(reading.limit.is_some_and(|l| l >= 64), "{reading:?}");
    }
}
