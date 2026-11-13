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
const STAMP: &str = "cache-maintenance.stamp";
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

fn read_stamp(dir: &Path) -> io::Result<Option<Vec<u8>>> {
    match private_file::read_private(dir, STAMP, 12) {
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
    Ok(stamp(dir, now)?.then_some(held))
}

/// Stamp daily admission while borrowing the standalone watch's genuine lifetime guard.
/// It neither substitutes a fake guard nor reacquires the same flock on another descriptor.
/// # Errors
/// The guard no longer names this pass file, or private directory/stamp validation failed.
pub fn claim_held(dir: &Path, now: i64, held: &super::reserve::PassLock) -> io::Result<bool> {
    held.validate_for(&dir.join("pass.lock"))?;
    prepare_dir(dir)?;
    let _ = private_file::open_private(dir, "pass.lock").map_err(io::Error::other)?;
    let due = stamp(dir, now)?;
    held.validate_for(&dir.join("pass.lock"))?;
    Ok(due)
}

fn stamp(dir: &Path, now: i64) -> io::Result<bool> {
    if now <= 0 {
        return Err(io::Error::other("cache maintenance clock unavailable"));
    }
    let before = read_stamp(dir)?;
    if let Some(bytes) = &before {
        let record: &[u8; 12] = bytes
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
        if now.saturating_sub(previous) < DAY.as_secs() as i64 {
            return Ok(false);
        }
    }
    let mut bytes = [0u8; 12];
    bytes[..4].copy_from_slice(MAGIC);
    bytes[4..].copy_from_slice(&now.to_le_bytes());
    aterm_uds::atomic_file::write_checked(&dir.join(STAMP), &bytes, 0o600, || {
        if read_stamp(dir)? == before {
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
