// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Complete Cargo-lock inventories: budgeted surveys and unbudgeted one-time retirement.

use std::path::Path;

use super::{CARGO_LOCKS, Seen};

// Bounds limit work and open descriptors, never the portion certified as clear. Reaching
// any bound is uncertainty and keeps the tree. A scan holds at most one directory per level.
const DEPTH: usize = 128;
const ENTRIES: usize = 1_000_000;
const LOCKS: usize = 1024;

pub(super) fn scan(root: &Path) -> std::io::Result<Seen> {
    #[cfg(test)]
    SCANS.with(|scans| scans.set(scans.get().saturating_add(1)));
    #[cfg(test)]
    let entries = SURVEY_ENTRIES.with(std::cell::Cell::get);
    #[cfg(not(test))]
    let entries = ENTRIES;
    scan_with(root, entries, DEPTH)
}

#[cfg(test)]
thread_local! {
    static SURVEY_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(ENTRIES) };
    static SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Ordinary inventory attempts on this test thread, including those that refuse a tree.
#[cfg(all(test, unix))]
pub(super) fn scans_here() -> usize {
    SCANS.with(std::cell::Cell::get)
}

/// Inject a small real survey budget on this test thread. Replacing retirement's scan
/// with the ordinary survey must then fail on the very same fixture, without a million files.
#[cfg(all(test, unix))]
pub(super) fn with_entry_budget<T>(entries: usize, run: impl FnOnce() -> T) -> T {
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            SURVEY_ENTRIES.with(|budget| budget.set(self.0));
        }
    }
    let _reset = Reset(SURVEY_ENTRIES.with(|budget| budget.replace(entries)));
    run()
}

/// Retirement is a one-time migration, not a cache survey. Certify the whole tree even
/// when its artifact count exceeds the survey budget; never accept a lock-free prefix.
pub(super) fn retiring(root: &Path) -> std::io::Result<Seen> {
    scan_limits(root, usize::MAX, usize::MAX, usize::MAX)
}

fn uncertain() -> std::io::Error {
    std::io::Error::other("Cargo-lock inventory is incomplete or changed")
}

fn is_lock(name: &std::ffi::OsStr) -> bool {
    CARGO_LOCKS.iter().any(|lock| {
        name.as_encoded_bytes()
            .eq_ignore_ascii_case(lock.as_bytes())
    })
}

#[cfg(unix)]
fn scan_limits(root: &Path, mut left: usize, depth: usize, locks: usize) -> std::io::Result<Seen> {
    use aterm_dirfd::{self as fd, Mode, OFlags};
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let root_fd = fd::openat(fd::CWD, root, flags, Mode::empty())?;
    let mut seen = Seen {
        newest: metadata(&root_fd)?.modified()?,
        locks: Vec::new(),
    };
    walk(root_fd, root, &mut left, depth, locks, &mut seen)?;
    seen.locks.sort();
    Ok(seen)
}

#[cfg(unix)]
fn metadata(fd: &impl std::os::fd::AsFd) -> std::io::Result<std::fs::Metadata> {
    std::fs::File::from(fd.as_fd().try_clone_to_owned()?).metadata()
}

#[cfg(unix)]
fn stamp(metadata: &std::fs::Metadata) -> (u64, u64, i64, i64, i64, i64) {
    use std::os::unix::fs::MetadataExt as _;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[cfg(unix)]
fn walk(
    directory: aterm_dirfd::OwnedFd,
    path: &Path,
    left: &mut usize,
    depth: usize,
    locks: usize,
    seen: &mut Seen,
) -> std::io::Result<()> {
    use aterm_dirfd::{self as fd, AtFlags, FileType, Mode, OFlags};
    use std::os::unix::ffi::OsStrExt as _;
    let before = metadata(&directory)?;
    seen.newest = seen.newest.max(before.modified()?);
    let mut listing = fd::Dir::new(directory)?;
    while let Some(entry) = listing.next() {
        aterm_uds::progress::tick();
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        *left = left.checked_sub(1).ok_or_else(uncertain)?;
        let name = std::ffi::OsStr::from_bytes(name.to_bytes());
        let child_path = path.join(name);
        if is_lock(name) {
            if seen.locks.len() >= locks {
                return Err(uncertain());
            }
            // Include nonregular lock names too: hold() must refuse them, not miss them.
            seen.locks.push(child_path);
            continue;
        }
        let named = fd::fstatat(&listing, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(named.st_mode).is_dir() {
            let next_depth = depth.checked_sub(1).ok_or_else(uncertain)?;
            let child = fd::openat(
                &listing,
                name,
                OFlags::RDONLY
                    | OFlags::DIRECTORY
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::empty(),
            )?;
            let held = fd::fstat(&child)?;
            if (named.st_dev, named.st_ino) != (held.st_dev, held.st_ino)
                || named.st_dev != fd::fstat(&listing)?.st_dev
            {
                return Err(uncertain());
            }
            walk(child, &child_path, left, next_depth, locks, seen)?;
            let after = fd::fstatat(&listing, name, AtFlags::SYMLINK_NOFOLLOW)?;
            if (after.st_dev, after.st_ino) != (held.st_dev, held.st_ino) {
                return Err(uncertain());
            }
        }
    }
    if stamp(&before) != stamp(&metadata(&listing)?) {
        return Err(uncertain());
    }
    Ok(())
}

#[cfg(not(unix))]
fn scan_limits(root: &Path, mut left: usize, depth: usize, locks: usize) -> std::io::Result<Seen> {
    fn walk(
        path: &Path,
        left: &mut usize,
        depth: usize,
        locks: usize,
        seen: &mut Seen,
    ) -> std::io::Result<()> {
        let before = std::fs::symlink_metadata(path)?;
        if !before.is_dir() {
            return Err(uncertain());
        }
        seen.newest = seen.newest.max(before.modified()?);
        for entry in std::fs::read_dir(path)? {
            aterm_uds::progress::tick();
            let entry = entry?;
            *left = left.checked_sub(1).ok_or_else(uncertain)?;
            if is_lock(&entry.file_name()) {
                if seen.locks.len() >= locks {
                    return Err(uncertain());
                }
                seen.locks.push(entry.path());
            } else if entry.file_type()?.is_dir() {
                walk(
                    &entry.path(),
                    left,
                    depth.checked_sub(1).ok_or_else(uncertain)?,
                    locks,
                    seen,
                )?;
            }
        }
        if std::fs::symlink_metadata(path)?.modified()? != before.modified()? {
            return Err(uncertain());
        }
        Ok(())
    }
    let mut seen = Seen {
        newest: std::fs::symlink_metadata(root)?.modified()?,
        locks: Vec::new(),
    };
    walk(root, &mut left, depth, locks, &mut seen)?;
    seen.locks.sort();
    Ok(seen)
}

pub(super) fn scan_with(root: &Path, entries: usize, depth: usize) -> std::io::Result<Seen> {
    scan_limits(root, entries, depth, LOCKS)
}

#[cfg(all(test, unix))]
#[path = "build_cache_inventory_tests.rs"]
mod tests;
