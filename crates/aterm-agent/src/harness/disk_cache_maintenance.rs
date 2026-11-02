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
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn a_borrowed_lifetime_guard_stamps_without_releasing_or_reacquiring_it() {
        let home = aterm_tempfile::tempdir().unwrap();
        let path = super::super::reserve::pass_lock_for(home.path());
        let held = super::super::reserve::take_pass(&path)
            .unwrap()
            .expect("lifetime guard");
        let dir = path.parent().unwrap();
        assert!(claim_held(dir, 1_790_000_000, &held).unwrap());
        assert!(
            !claim_held(dir, 1_790_000_001, &held).unwrap(),
            "one daily stamp"
        );
        assert!(
            super::super::reserve::take_pass(&path).unwrap().is_none(),
            "borrowed claim kept actual exclusion"
        );
        let stamp_before = read_stamp(dir).unwrap();
        let other = aterm_tempfile::tempdir().unwrap();
        let other_path = super::super::reserve::pass_lock_for(other.path());
        let other_guard = super::super::reserve::take_pass(&other_path)
            .unwrap()
            .unwrap();
        assert!(
            claim_held(dir, 1_790_086_400, &other_guard).is_err(),
            "a real guard for another path is no authority here"
        );
        assert_eq!(read_stamp(dir).unwrap(), stamp_before);
        std::fs::rename(&path, dir.join("old-pass.lock")).unwrap();
        let replacement = super::super::reserve::take_pass(&path).unwrap().unwrap();
        assert!(
            held.validate_for(&path).is_err(),
            "a replaced name is not the held inode"
        );
        assert!(claim_held(dir, 1_790_086_400, &held).is_err());
        assert_eq!(
            read_stamp(dir).unwrap(),
            stamp_before,
            "replacement refused before a new stamp"
        );
        drop(replacement);
        drop(held);
    }

    #[test]
    fn daily_and_pressure_passes_exclude_each_other_on_the_same_real_lock() {
        let home = aterm_tempfile::tempdir().unwrap();
        let path = super::super::reserve::pass_lock_for(home.path());
        let pressure = super::super::reserve::take_pass(&path)
            .unwrap()
            .expect("pressure guard");
        let dir = path.parent().expect("pass directory");
        assert!(
            claim(dir, 1_790_000_000).unwrap().is_none(),
            "daily cannot overlap pressure"
        );
        assert!(
            !dir.join(STAMP).exists(),
            "a contended daily attempt writes no stamp"
        );
        drop(pressure);
        let daily = claim(dir, 1_790_000_000).unwrap().expect("daily guard");
        assert!(
            super::super::reserve::take_pass(&path).unwrap().is_none(),
            "pressure cannot overlap daily"
        );
        drop(daily);
        assert!(
            super::super::reserve::take_pass(&path).unwrap().is_some(),
            "both guards release normally"
        );
    }

    #[test]
    fn a_long_lived_ample_watch_claims_one_cache_sweep_each_day_across_instances() {
        let root = aterm_tempfile::tempdir().unwrap();
        let cache = root.path().join("aterm");
        let dir = cache.join("disk-reserve.noindex");
        assert!(!cache.exists());
        assert!(!dir.exists());
        let first = Mutex::new(Schedule::new());
        let second = Mutex::new(Schedule::new());
        let t0 = Instant::now();
        let unix = 1_790_000_000;
        let sweeps = Cell::new(0);
        let allowed = eligible(&Config::default(), Level::Ok, true, true, false);
        let look = |schedule, at, seconds| {
            run(
                schedule,
                at,
                allowed,
                || claim(&dir, seconds),
                || {
                    assert!(PrivateLock::take(&dir, "pass.lock", Duration::ZERO, "busy").is_err());
                    sweeps.set(sweeps.get() + 1);
                },
            )
            .unwrap()
        };
        assert!(look(&first, t0, unix));
        private_file::check_private_dir(&cache).unwrap();
        private_file::check_private_dir(&dir).unwrap();
        assert_eq!(
            private_file::read_private(&dir, STAMP, 12).unwrap().len(),
            12
        );
        assert!(!look(
            &first,
            t0 + DAY - Duration::from_secs(1),
            unix + 86_399
        ));
        assert!(!look(&second, t0, unix));
        assert_eq!(sweeps.get(), 1);
        assert!(look(&first, t0 + DAY, unix + 86_400));
        assert!(!look(&second, t0 + DAY, unix + 86_400));
        assert!(!look(&first, t0 + DAY + RETRY, unix + 87_000));
        assert_eq!(sweeps.get(), 2);
    }

    #[test]
    fn a_failed_or_busy_claim_never_sweeps_and_backs_off() {
        let root = aterm_tempfile::tempdir().unwrap();
        let cache = root.path().join("aterm");
        let dir = cache.join("disk-reserve.noindex");
        private_file::create_private_dir(&cache).unwrap();
        private_file::create_private_dir(&dir).unwrap();
        let schedule = Mutex::new(Schedule::new());
        let t0 = Instant::now();
        let held = PrivateLock::take(&dir, "pass.lock", Duration::ZERO, "busy").unwrap();
        assert!(
            !run(
                &schedule,
                t0,
                true,
                || claim(&dir, 1_790_000_000),
                || panic!("busy")
            )
            .unwrap()
        );
        drop(held);
        assert!(
            !run::<()>(
                &schedule,
                t0 + Duration::from_secs(1),
                true,
                || panic!("backoff"),
                || panic!("backoff")
            )
            .unwrap()
        );
        aterm_uds::atomic_file::write(&dir.join(STAMP), b"damaged", 0o600).unwrap();
        assert!(
            run(
                &schedule,
                t0 + RETRY,
                true,
                || claim(&dir, 1_790_000_600),
                || panic!("damaged stamp")
            )
            .is_err()
        );
        assert_eq!(std::fs::read(dir.join(STAMP)).unwrap(), b"damaged");
        assert!(
            !run::<()>(
                &schedule,
                t0 + RETRY + Duration::from_secs(1),
                true,
                || panic!("failed stamp backoff"),
                || panic!("failed stamp")
            )
            .unwrap()
        );
    }

    #[test]
    fn an_owned_searchable_cache_parent_keeps_its_mode_and_gets_a_private_pass_leaf() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = aterm_tempfile::tempdir().unwrap();
        let cache = root.path().join("aterm");
        private_file::create_private_dir(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755)).unwrap();
        let dir = cache.join("disk-reserve.noindex");
        assert!(!dir.exists());
        let held = claim(&dir, 1_790_000_000).unwrap().unwrap();
        private_file::check_private_dir(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            private_file::read_private(&dir, STAMP, 12).unwrap().len(),
            12
        );
        assert!(claim(&dir, 1_790_000_001).unwrap().is_none());
        drop(held);
        assert!(claim(&dir, 1_790_000_001).unwrap().is_none());
    }

    #[test]
    fn existing_nonprivate_or_linked_cache_directories_are_not_changed() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        for leaf in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let cache = root.path().join("aterm");
            let dir = cache.join("disk-reserve.noindex");
            private_file::create_private_dir(&cache).unwrap();
            private_file::create_private_dir(&dir).unwrap();
            let unsafe_dir = if leaf { &dir } else { &cache };
            let mode = if leaf { 0o755 } else { 0o775 };
            std::fs::set_permissions(unsafe_dir, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(claim(&dir, 1_790_000_000).is_err());
            assert_eq!(
                std::fs::metadata(unsafe_dir).unwrap().permissions().mode() & 0o777,
                mode
            );
            assert!(!dir.join(STAMP).exists());
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let target = root.path().join("other");
        private_file::create_private_dir(&target).unwrap();
        let cache = root.path().join("aterm");
        symlink(&target, &cache).unwrap();
        assert!(claim(&cache.join("disk-reserve.noindex"), 1_790_000_000).is_err());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    }

    #[test]
    fn cache_upkeep_leaves_opt_outs_private_unknown_and_pressure_looks_alone() {
        let config = Config::default();
        assert!(eligible(&config, Level::Ok, true, true, false));
        for off in [
            Config {
                auto_free_gib: 0,
                ..config
            },
            Config {
                dead_build_dirs: false,
                ..config
            },
        ] {
            assert!(!eligible(&off, Level::Ok, true, true, false));
        }
        for (level, owner, placed, requested) in [
            (Level::Unknown, true, true, false),
            (Level::Low, true, true, false),
            (Level::Short, true, true, false),
            (Level::Emergency, true, true, false),
            (Level::Ok, false, true, false),
            (Level::Ok, true, false, false),
            (Level::Ok, true, true, true),
        ] {
            let allowed = eligible(&config, level, owner, placed, requested);
            assert!(!allowed);
            assert!(
                !run::<()>(
                    &Mutex::new(Schedule::new()),
                    Instant::now(),
                    allowed,
                    || panic!("no claim"),
                    || panic!("no sweep")
                )
                .unwrap()
            );
        }
    }
}
