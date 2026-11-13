// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Laying executables (the `bin/` shims, their `agents/` twins and `alab-` aliases, the pending
//! stubs, the reroute stubs, the tombstones) through one writer: [`write_in_process`](crate::lay::write_in_process), and
//! [`write_regenerable`](crate::lay::write_regenerable) for a file every seed lays again.

use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;

/// One executable to lay: where, and what bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executable {
    /// The destination path.
    pub path: PathBuf,
    /// The file's bytes.
    pub body: Vec<u8>,
}

impl Executable {
    /// An executable at `path` with `body`.
    pub fn new(path: impl Into<PathBuf>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            body: body.into(),
        }
    }
}

/// Write one executable.
pub fn write_in_process(file: &Executable) -> io::Result<()> {
    write_with(file, aterm_durable::ordered)
}

/// [`write_in_process`] for a file the next seed lays again when it is missing or its bytes differ
/// (a pending stub, its `agents/` twin, a reroute stub, the reroute directory's marker): the same
/// temp, mode and `rename(2)`, flushed with a plain `fsync(2)`
/// ([`crate::platform::sync_file_contents`]) instead of the drive-cache barrier.
pub fn write_regenerable(file: &Executable) -> io::Result<()> {
    write_with(file, crate::platform::sync_file_contents)
}

/// Per-process sequence for the writer's temp names ([`write_in_process`]).
static LAY_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The temp a lay of `dest` stages in: `.<name>.lay-<pid>-<seq>` beside it.
pub(crate) fn staged_at(dest: &std::path::Path, name: &str, seq: u64) -> PathBuf {
    dest.with_file_name(format!(".{name}.lay-{}-{seq}", std::process::id()))
}

/// Whether a directory entry is the temp of a lay THIS process has in flight.
pub(crate) fn in_flight_here(entry: &OsStr) -> bool {
    entry
        .to_str()
        .and_then(|name| name.strip_prefix('.'))
        .and_then(|name| name.rsplit_once(".lay-"))
        .and_then(|(_, tail)| tail.split_once('-'))
        .is_some_and(|(pid, seq)| {
            !pid.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit())
                && pid.parse() == Ok(std::process::id())
                && !seq.is_empty()
                && seq.bytes().all(|b| b.is_ascii_digit())
        })
}

/// Serializes every executable publication in the process (a lay's rename, or a removal) with a
/// reconcile's read of a primary and its publish of what that read selected.
pub(crate) static PUBLICATION_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The one writer: stages the bytes with the caller's flush `sync` (a sibling temp only
/// this writer names), then takes [`PUBLICATION_TURN`] for the publication alone: the
/// `rename(2)` that is the only step a reader of the destination can see.
fn write_with(file: &Executable, sync: fn(&std::fs::File) -> io::Result<()>) -> io::Result<()> {
    let tmp = stage(file, sync)?;
    #[cfg(all(test, unix))]
    if matches!(
        PUBLICATION_TURN.try_lock(),
        Err(std::sync::TryLockError::WouldBlock)
    ) {
        crate::activate::agents_checkpoint::waiting(&file.path);
    }
    let _turn = PUBLICATION_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    publish(file, &tmp)
}

/// The ordered writer while the caller holds [`PUBLICATION_TURN`] across the read that selected
/// these bytes.
pub(crate) fn write_in_publication_turn(
    file: &Executable,
    _turn: &std::sync::MutexGuard<'_, ()>,
) -> io::Result<()> {
    let tmp = stage(file, aterm_durable::ordered)?;
    publish(file, &tmp)
}

/// Remove a primary under [`PUBLICATION_TURN`].
pub(crate) fn remove_in_process(path: &std::path::Path) -> io::Result<()> {
    let _turn = PUBLICATION_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::fs::remove_file(path)
}

/// Write `file`'s bytes to a fresh sibling temp ([`staged_at`]), mode `0755`, flushed with
/// `sync`; a temp this call creates is removed again on a later error. An occupied candidate
/// is left untouched.
fn stage(file: &Executable, sync: fn(&std::fs::File) -> io::Result<()>) -> io::Result<PathBuf> {
    stage_numbered(
        file,
        sync,
        LAY_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
}

fn stage_numbered(
    file: &Executable,
    sync: fn(&std::fs::File) -> io::Result<()>,
    seq: u64,
) -> io::Result<PathBuf> {
    let name = file
        .path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "executable has no file name")
        })?;
    let tmp = staged_at(&file.path, name, seq);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o755);
        opts.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let mut f = opts.open(&tmp)?;
    let written = (|| -> io::Result<()> {
        use std::io::Write as _;
        f.write_all(&file.body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            f.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        sync(&f)?;
        Ok(())
    })();
    drop(f);
    match written {
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Rename a [`stage`]d temp over `file`'s destination; the temp is removed if that fails.
fn publish(file: &Executable, tmp: &std::path::Path) -> io::Result<()> {
    #[cfg(windows)]
    let _ = std::fs::remove_file(&file.path);
    if let Err(e) = std::fs::rename(tmp, &file.path) {
        let _ = std::fs::remove_file(tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "lay_tests.rs"]
mod tests;
