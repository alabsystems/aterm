// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared daily managed-cache upkeep, independent of pressure.

use std::io;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::{Config, Level};
use aterm_uds::private_file::{self, PrivateFileError, PrivateLock};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const RETRY: Duration = Duration::from_secs(10 * 60);
/// The least time from any stamped sweep to one taken early ([`claim_held_early`]): the
/// entry-bound pass's own hour.
const EARLY: Duration = Duration::from_secs(60 * 60);
/// The daily turn's stamp: its sweep, the holders' measure and the branch-name step.
const STAMP: &str = "cache-maintenance.stamp";
/// The early sweep's own stamp ([`claim_held_early`]), which the daily claim never reads: an
/// early sweep that ran every hour would otherwise hold the daily turn off for good.
const EARLY_STAMP: &str = "cache-maintenance-early.stamp";
const MAGIC: &[u8; 4] = b"ACM1";

/// Whether ordinary daily upkeep is admitted by the current automatic policy and volume facts.
pub fn eligible(config: &Config, level: Level, owner: bool, placed: bool, requested: bool) -> bool {
    config.auto_free_gib > 0
        && config.dead_build_dirs
        && owner
        && placed
        && level == Level::Ok
        && !requested
}

/// The next local storage check, not another worker or a pressure-pass deadline.
pub struct Schedule {
    next: Option<Instant>,
}

impl Default for Schedule {
    fn default() -> Self {
        Self::new()
    }
}

impl Schedule {
    #[must_use]
    pub const fn new() -> Self {
        Self { next: None }
    }
}

/// Run only after a successful shared claim; hold its guard across the whole sweep. A failed
/// or busy claim backs off locally. The shared attempt is stamped before work, so a failed or
/// interrupted sweep waits until the next day; ordinary pressure passes remain independent.
/// # Errors
/// The shared claim could not be validated or acquired.
pub fn run<G>(
    schedule: &Mutex<Schedule>,
    now: Instant,
    eligible: bool,
    claim: impl FnOnce() -> io::Result<Option<G>>,
    sweep: impl FnOnce(),
) -> io::Result<bool> {
    {
        let mut schedule = schedule.lock().unwrap_or_else(PoisonError::into_inner);
        if !eligible || schedule.next.is_some_and(|next| now < next) {
            return Ok(false);
        }
        schedule.next = now.checked_add(RETRY);
    }
    let Some(held) = claim()? else {
        return Ok(false);
    };
    schedule.lock().unwrap_or_else(PoisonError::into_inner).next = now.checked_add(DAY);
    sweep();
    drop(held);
    Ok(true)
}

fn read_stamp(dir: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    match private_file::read_private(dir, name, 12) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(PrivateFileError::Missing) => Ok(None),
        Err(error) => Err(io::Error::other(error)),
    }
}

/// Validate the existing cache parent without narrowing its permissions, and establish the
/// private reserve directory. This creates no removal authority.
fn prepare_dir(dir: &Path) -> io::Result<()> {
    let cache = dir
        .parent()
        .ok_or_else(|| io::Error::other("cache maintenance has no cache directory"))?;
    // Ample space does not imply the ballast reserve or its cache parent was created. An
    // existing owner-only-writable cache parent may be searchable (0755); never narrow it.
    if matches!(
        private_file::judge_private_dir(cache)?,
        private_file::PrivateDir::Missing
    ) {
        private_file::create_private_dir(cache)?;
    }
    match private_file::judge_private_dir(cache)? {
        private_file::PrivateDir::Private => {}
        private_file::PrivateDir::Open(mode) if mode & 0o022 == 0 => {}
        _ => {
            return Err(io::Error::other(
                "cache maintenance parent is not owner-only writable",
            ));
        }
    }
    private_file::create_private_dir(dir)?;
    Ok(())
}

/// Acquire the ordinary shared daily claim.
/// # Errors
/// Clock, directory, lock or stamp admission failed.
pub fn claim(dir: &Path, now: i64) -> io::Result<Option<PrivateLock>> {
    if now <= 0 {
        return Err(io::Error::other("cache maintenance clock unavailable"));
    }
    prepare_dir(dir)?;
    let held = match PrivateLock::take(dir, "pass.lock", Duration::ZERO, "disk pass busy") {
        Ok(held) => held,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
        Err(error) => return Err(error),
    };
    // PrivateLock supplies exclusion; the existing private-file reader also checks the
    // file's ownership, link count and permissions before this new proactive path uses it.
    let _ = private_file::open_private(dir, "pass.lock").map_err(io::Error::other)?;
    Ok(stamp(dir, now, (STAMP, DAY), &[])?.then_some(held))
}

/// Stamp daily admission while borrowing the standalone watch's genuine lifetime guard.
/// It neither substitutes a fake guard nor reacquires the same flock on another descriptor.
/// # Errors
/// The guard no longer names this pass file, or private directory/stamp validation failed.
pub fn claim_held(dir: &Path, now: i64, held: &super::reserve::PassLock) -> io::Result<bool> {
    claim_held_after(dir, now, held, (STAMP, DAY), &[])
}

/// Take a sweep early under the pass lock `held`, for a build cache over the entry bound:
/// admitted where no sweep, daily or early, was stamped within the last hour (one that was
/// already took what this one would), and stamped under its own name, so the daily turn, which
/// also measures the holders and prunes stale branch names, keeps its own day however often a
/// growing cache brings this one back.
/// # Errors
/// As [`claim_held`].
pub fn claim_held_early(dir: &Path, now: i64, held: &super::reserve::PassLock) -> io::Result<bool> {
    claim_held_after(dir, now, held, (EARLY_STAMP, EARLY), &[STAMP])
}

fn claim_held_after(
    dir: &Path,
    now: i64,
    held: &super::reserve::PassLock,
    own: (&str, Duration),
    also: &[&str],
) -> io::Result<bool> {
    held.validate_for(&dir.join("pass.lock"))?;
    prepare_dir(dir)?;
    let _ = private_file::open_private(dir, "pass.lock").map_err(io::Error::other)?;
    let due = stamp(dir, now, own, also)?;
    held.validate_for(&dir.join("pass.lock"))?;
    Ok(due)
}

/// The unix second the stamp `name` in `dir` holds, `None` where there is none.
fn stamped_at(dir: &Path, name: &str) -> io::Result<(Option<Vec<u8>>, Option<i64>)> {
    let bytes = read_stamp(dir, name)?;
    let Some(record) = &bytes else {
        return Ok((None, None));
    };
    let record: &[u8; 12] = record
        .as_slice()
        .try_into()
        .map_err(|_| io::Error::other("invalid cache maintenance stamp"))?;
    if &record[..4] != MAGIC {
        return Err(io::Error::other("invalid cache maintenance stamp"));
    }
    let previous = i64::from_le_bytes(record[4..].try_into().expect("eight-byte stamp"));
    if previous <= 0 {
        return Err(io::Error::other("invalid cache maintenance stamp"));
    }
    Ok((bytes, Some(previous)))
}

/// Stamp `now` under `own`'s name where neither it nor any stamp of `also` was written within
/// `own`'s span of it; `false` where one was.
fn stamp(dir: &Path, now: i64, own: (&str, Duration), also: &[&str]) -> io::Result<bool> {
    if now <= 0 {
        return Err(io::Error::other("cache maintenance clock unavailable"));
    }
    let (name, after) = own;
    let within =
        |at: Option<i64>| at.is_some_and(|at| now.saturating_sub(at) < after.as_secs() as i64);
    let (before, at) = stamped_at(dir, name)?;
    if within(at) {
        return Ok(false);
    }
    for other in also {
        if within(stamped_at(dir, other)?.1) {
            return Ok(false);
        }
    }
    let mut bytes = [0u8; 12];
    bytes[..4].copy_from_slice(MAGIC);
    bytes[4..].copy_from_slice(&now.to_le_bytes());
    aterm_uds::atomic_file::write_checked(&dir.join(name), &bytes, 0o600, || {
        if read_stamp(dir, name)? == before {
            Ok(())
        } else {
            Err(io::Error::other("cache maintenance stamp changed"))
        }
    })?;
    Ok(true)
}

#[cfg(test)]
#[path = "disk_cache_maintenance_tests.rs"]
mod tests;
