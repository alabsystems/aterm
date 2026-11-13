// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The vendor lane's small-file I/O.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::lock::Flock;

const LOCK_WAIT: Duration = Duration::from_secs(60);

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Replace `path` with `bytes`.
/// # Errors
/// The temp could not be created, written, flushed or renamed; the temp is removed. Or the
/// directory's flush after the rename failed other than as unavailable
/// ([`aterm_durable::Flush::dir_best_effort`]): `path` holds `bytes`, but its entry may not
/// survive a power loss.
pub(crate) fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path, TEMP_SEQ.fetch_add(1, Ordering::Relaxed))?;
    replace_via(&tmp, path, bytes)
}

/// [`write_durable`], every flush only ORDERED ([`aterm_durable::ordered`]).
/// # Errors
/// As [`write_durable`].
pub(crate) fn write_ordered(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path, TEMP_SEQ.fetch_add(1, Ordering::Relaxed))?;
    let _ = std::fs::remove_file(&tmp);
    if let Err(error) =
        stage_with(&tmp, bytes, aterm_durable::ordered).and_then(|()| replace(&tmp, path))
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    let _flushed_or_unavailable =
        aterm_durable::Flush::Ordered.dir_best_effort(parent_dir(path))?;
    Ok(())
}

/// [`write_durable`] through the temp `tmp`.
fn replace_via(tmp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let _ = std::fs::remove_file(tmp);
    if let Err(error) = stage(tmp, bytes).and_then(|()| replace(tmp, path)) {
        let _ = std::fs::remove_file(tmp);
        return Err(error);
    }
    let _flushed_or_unavailable =
        aterm_durable::Flush::Durable.dir_best_effort(parent_dir(path))?;
    Ok(())
}

/// Rename the staged `tmp` over `path` ([`again_while_refused`]). On Windows a replace is no
/// atomic step for a reader: while a rename is under way the old name can be gone and the new
/// one not there yet, and an open by name in between finds no file (measured: thousands of
/// opens in a row during one slow rename). There the rename runs under the exclusive
/// [`replace_lock`], which [`read_durable`] waits on before it takes a missing file for an
/// absent one.
fn replace(tmp: &Path, path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    let _replacing = {
        let lock = replace_lock(path)?;
        Flock::lock_within(open_lock(&lock, true)?, REPLACE_HANG, &lock)?
    };
    again_while_refused(|| std::fs::rename(tmp, path))
}

/// `.<name>.replace-lock` beside `path`: held exclusively while a writer renames over `path`
/// and shared by a reader that found no file there ([`replace`], [`read_durable`]). Windows only.
#[cfg(windows)]
fn replace_lock(path: &Path) -> io::Result<PathBuf> {
    let mut lock = String::from(".");
    lock.push_str(file_name(path)?);
    lock.push_str(".replace-lock");
    Ok(path.with_file_name(lock))
}

/// Windows: once no writer holds `path`'s [`replace_lock`], the name says whether the file is
/// there. The lock, shared, waited for at most [`REPLACE_HANG`]; `None` where no writer ever
/// made the lock, so none ever replaced `path` and a missing file is absent.
#[cfg(windows)]
fn replace_settled(path: &Path) -> io::Result<Option<Flock>> {
    let lock = replace_lock(path)?;
    match open_lock(&lock, false) {
        Ok(file) => Flock::lock_shared_within(file, REPLACE_HANG, &lock).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The directory that names `path`: its parent, or `.` for a bare name.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// A minute hang detector for Windows to release a required file operation.
#[cfg(any(windows, test))]
const REPLACE_HANG: Duration = Duration::from_secs(60);

fn again_while_refused<T>(op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    #[cfg(windows)]
    {
        again_while_refused_windows_at(op, std::time::Instant::now(), std::thread::sleep)
    }
    #[cfg(not(windows))]
    {
        let mut op = op;
        op()
    }
}

#[cfg(any(windows, test))]
fn again_while_refused_windows_at<T>(
    mut op: impl FnMut() -> io::Result<T>,
    started: std::time::Instant,
    mut pause: impl FnMut(Duration),
) -> io::Result<T> {
    let mut backoff = Duration::from_millis(1);
    loop {
        match op() {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                let left = REPLACE_HANG.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return Err(e);
                }
                pause(backoff.min(left));
                backoff = backoff
                    .saturating_add(Duration::from_millis(1))
                    .min(Duration::from_millis(30));
            }
            done => return done,
        }
    }
}

/// Hold the exclusive lock `<path>.lock` while a caller reads, changes and replaces `path`.
/// # Errors
/// The lock could not be created or was not granted within [`LOCK_WAIT`].
pub(super) fn lock_for_update(path: &Path) -> io::Result<Flock> {
    lock_for_update_after(path, Duration::ZERO)
}

fn lock_for_update_after(path: &Path, elapsed: Duration) -> io::Result<Flock> {
    lock_for_update_with(path, elapsed, Flock::lock_within)
}

fn lock_for_update_with(
    path: &Path,
    elapsed: Duration,
    acquire: impl FnOnce(std::fs::File, Duration, &Path) -> io::Result<Flock>,
) -> io::Result<Flock> {
    let name = file_name(path)?;
    let mut lock = String::from(name);
    lock.push_str(".lock");
    let lock = path.with_file_name(lock);
    let file = open_lock(&lock, true)?;
    let left = LOCK_WAIT
        .checked_sub(elapsed)
        .filter(|left| !left.is_zero())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "vendor update lock hang detector expired",
            )
        })?;
    acquire(file, left, &lock)
}

/// Open (creating `0600` when `create`, never truncating: a lock file is a rendezvous, not data)
/// the lock file at `path`.
fn open_lock(path: &Path, create: bool) -> io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(create).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = opts.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "lock path is not a regular file",
        ));
    }
    Ok(file)
}

/// `.<name>.tmp-<pid>-<seq>` beside `path`.
pub(super) fn temp_path(path: &Path, seq: u64) -> io::Result<PathBuf> {
    let mut tmp = String::from(".");
    tmp.push_str(file_name(path)?);
    tmp.push_str(".tmp-");
    tmp.push_str(&std::process::id().to_string());
    tmp.push('-');
    tmp.push_str(&seq.to_string());
    Ok(path.with_file_name(tmp))
}

fn file_name(path: &Path) -> io::Result<&str> {
    path.file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))
}

fn stage(tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    stage_with(tmp, bytes, crate::platform::sync_file_contents)
}

/// [`stage`], flushed by `flush`, a refusal accepted ([`aterm_durable::refusal_accepted`]).
fn stage_with(
    tmp: &Path,
    bytes: &[u8],
    flush: fn(&std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let mut f = create_temp(tmp)?;
    crate::platform::set_mode_on(&f, 0o644)?;
    f.write_all(bytes)?;
    aterm_durable::refusal_accepted(flush(&f))
}

/// Create `tmp` exclusively: an existing file of that name is never written through.
fn create_temp(tmp: &Path) -> io::Result<std::fs::File> {
    crate::platform::open_create_new_write(tmp, 0o644)
}

/// Read `path` as TOML of type `T`.
pub(super) fn read_toml<T: serde::de::DeserializeOwned>(path: &Path, limit: usize) -> Option<T> {
    let text = read_durable(path, limit).ok()?;
    aterm_toml::from_str(&text).ok()
}

/// Read a [`write_durable`] file whole ([`again_while_refused`]). On Windows a file found
/// missing is read once more after any replace of it under way has finished
/// ([`replace_settled`]), so a rename in flight never reads as an absent file.
fn read_durable(path: &Path, limit: usize) -> io::Result<String> {
    #[cfg(windows)]
    {
        read_settled(path, limit, replace_settled)
    }
    #[cfg(not(windows))]
    {
        read_once(path, limit)
    }
}

/// One read of `path` whole ([`again_while_refused`]).
fn read_once(path: &Path, limit: usize) -> io::Result<String> {
    again_while_refused(|| crate::metadata_io::read_bounded_regular_utf8(path, limit))
}

/// [`read_once`], and once more where `path` was missing and `settled` answers a guard: what
/// the name says while the guard is held is the answer.
#[cfg(any(windows, test))]
fn read_settled<G>(
    path: &Path,
    limit: usize,
    settled: impl FnOnce(&Path) -> io::Result<Option<G>>,
) -> io::Result<String> {
    let first = read_once(path, limit);
    if first
        .as_ref()
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        && let Some(_settled) = settled(path)?
    {
        return read_once(path, limit);
    }
    first
}

/// Serialize `value` as TOML bytes, refusing more than `limit`.
pub(super) fn to_toml<T: serde::Serialize>(value: &T, limit: usize) -> io::Result<Vec<u8>> {
    let text = aterm_toml::to_string(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    if text.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "vendor file exceeds its size limit",
        ));
    }
    Ok(text.into_bytes())
}

#[cfg(test)]
#[path = "durable_tests.rs"]
mod tests;
