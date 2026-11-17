// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Completed wait reports: automatic observations discard their raw files; explicit ones
//! retain at most 128 reports for seven days. In-flight reports live outside that set.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

const KEEP: usize = 128;
const AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[cfg(unix)]
const OWNER: &str = ".sample-owner";
#[cfg(unix)]
const OWNED: &[u8] = b"aterm wait sample staging v1\n";

#[cfg(unix)]
fn claim(directory: &Path) -> io::Result<aterm_uds::private_file::PrivateLock> {
    use std::io::Write as _;
    let lock = aterm_uds::private_file::PrivateLock::take_private(
        directory,
        OWNER,
        Duration::ZERO,
        "sample staging is busy",
    )?;
    lock.file().write_all(OWNED)?;
    Ok(lock)
}

#[cfg(unix)]
fn scavenge(staging: &Path) -> io::Result<()> {
    use std::io::Read as _;
    for entry in std::fs::read_dir(staging)? {
        let entry = entry?;
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !kind.is_dir()
            || !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("wait-") && name.ends_with(".noindex"))
        {
            continue;
        }
        // The marker is written only under this lock, before sampling. A just-created
        // directory without a completed marker is not yet ours to collect.
        let Ok(Some(lock)) = aterm_uds::private_file::PrivateLock::take_existing(
            &entry.path(),
            OWNER,
            Duration::ZERO,
            "sample staging is busy",
        ) else {
            continue;
        };
        let mut marker = Vec::new();
        lock.file()
            .take(OWNED.len() as u64 + 1)
            .read_to_end(&mut marker)?;
        if marker == OWNED {
            match std::fs::remove_dir_all(entry.path()) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                result => result?,
            }
        }
    }
    Ok(())
}

fn report_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_prefix("wait-")?.strip_suffix(".sample.txt"))
        .is_some_and(|id| {
            id.len() == 32
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn sweep(directory: &Path, now: SystemTime) -> io::Result<()> {
    let mut reports = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if !report_name(&entry.file_name()) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified()?;
        if now.duration_since(modified).is_ok_and(|age| age >= AGE) {
            remove(&entry.path())?;
        } else {
            reports.push((modified, entry.path()));
        }
    }
    reports.sort();
    let excess = reports.len().saturating_sub(KEEP);
    for (_, path) in reports.into_iter().take(excess) {
        remove(&path)?;
    }
    Ok(())
}

/// The sampler owns a private temporary directory until it returns, on success or failure.
/// Only completed explicit reports enter the retained set; age/count sweeps ignore staging.
/// Preserve a collection error if retaining its partial report also fails.
pub(super) fn observe<T>(
    directory: &Path,
    keep: bool,
    now: SystemTime,
    collect: impl FnOnce(&Path) -> io::Result<T>,
) -> io::Result<T> {
    sweep(directory, now)?;
    let staging = directory.join(".ubd");
    std::fs::create_dir_all(&staging)?;
    #[cfg(unix)]
    scavenge(&staging)?;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("wait-")
        .suffix(".noindex")
        .tempdir_in(&staging)?;
    #[cfg(unix)]
    let _owner = claim(scratch.path())?;
    let result = collect(scratch.path());
    let maintenance = (|| {
        if keep {
            for entry in std::fs::read_dir(scratch.path())? {
                let entry = entry?;
                if report_name(&entry.file_name()) && entry.file_type()?.is_file() {
                    // A sample reserves a random 128-bit name with create_new. Hard linking
                    // publishes without replacing any retained path, on this same volume.
                    std::fs::hard_link(entry.path(), directory.join(entry.file_name()))?;
                }
            }
        }
        sweep(directory, now)
    })();
    drop(scratch);
    result.and_then(|value| maintenance.map(|()| value))
}

#[cfg(test)]
#[path = "sys_naming_reports_tests.rs"]
mod tests;
