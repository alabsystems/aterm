// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The vendor lane's small-file I/O: a durable replace, a bounded read, and the lock a
//! read-modify-write holds.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::lock::Flock;

/// How long a read-modify-write waits for the file's lock. The holder keeps it for one
/// small read and write; a wedged holder costs this one update, never a hung thread.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// Per-process sequence for temp names, so no two writes share a temp.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Replace `path` with `bytes`: a fresh `0644` temp sibling, fsync, rename, directory
/// fsync. A reader sees the old file or the new one, never a torn one. Every call stages
/// through its own exclusively created temp (pid plus a per-process sequence), so two
/// writers — threads or processes — never write through one inode; a crash leaves at most
/// a `.<name>.tmp-<pid>-<seq>` that no reader opens.
///
/// # Errors
/// The temp could not be created, written, flushed or renamed; the temp is removed.
pub(crate) fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path, TEMP_SEQ.fetch_add(1, Ordering::Relaxed))?;
    replace_via(&tmp, path, bytes)
}

/// [`write_durable`], every flush only ORDERED ([`aterm_durable::ordered`]): the temp's bytes
/// land before the rename names them, and the name before any later write, so a reader
/// after a power loss finds the old file or the whole new one, never a torn one, but
/// perhaps the old one. For a record the next pass writes again (the vendor stamp).
///
/// # Errors
/// As [`write_durable`].
pub(crate) fn write_ordered(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path, TEMP_SEQ.fetch_add(1, Ordering::Relaxed))?;
    let _ = std::fs::remove_file(&tmp);
    if let Err(error) = stage_with(&tmp, bytes, aterm_durable::ordered)
        .and_then(|()| again_while_refused(|| std::fs::rename(&tmp, path)))
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    crate::store::sync_dir_ordered(path.parent().unwrap_or_else(|| Path::new(".")));
    Ok(())
}

/// [`write_durable`] through the temp `tmp`.
fn replace_via(tmp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Only a dead process that had our pid can have left this name.
    let _ = std::fs::remove_file(tmp);
    if let Err(error) =
        stage(tmp, bytes).and_then(|()| again_while_refused(|| std::fs::rename(tmp, path)))
    {
        let _ = std::fs::remove_file(tmp);
        return Err(error);
    }
    crate::store::sync_dir(path.parent().unwrap_or_else(|| Path::new(".")));
    Ok(())
}

/// How many times [`again_while_refused`] tries an operation again; the waits between
/// the tries grow by a millisecond each, about 0.2 s in all.
#[cfg(windows)]
const REFUSED_TRIES: u64 = 20;

/// `op`, tried again while Windows refuses it with `ERROR_ACCESS_DENIED`; elsewhere, `op`
/// once.
///
/// POSIX `rename(2)` replaces a name atomically whoever else is replacing it, and hands
/// a reader the old file or the new one. Windows refuses both for as long as another
/// replace of that name is in flight: `MoveFileExW` answers `ERROR_ACCESS_DENIED` to a
/// second writer the contract above admits, and an open answers it while the old file is
/// delete-pending. The snapshot of 2026-09-29 (6ad1a274d8) measured both on Windows in
/// `concurrent_writers_never_tear_the_file`: a writer failed within the 800 writes, and a
/// reader met the refusal, which [`read_toml`] would have answered as "absent". The other
/// replace finishes in microseconds, so a refusal is tried again, briefly; one that
/// outlasts the tries (a real denial, a scanner holding the file without delete sharing)
/// is the caller's error as before.
fn again_while_refused<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    #[cfg(windows)]
    {
        let mut tries = 0;
        loop {
            match op() {
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied && tries < REFUSED_TRIES => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(tries));
                }
                done => return done,
            }
        }
    }
    #[cfg(not(windows))]
    {
        op()
    }
}

/// Hold the exclusive lock `<path>.lock` (created `0600`) while a caller reads, changes
/// and replaces `path`. `flock` excludes other open descriptions, so this serializes
/// threads of one process as well as processes. Released by `LOCK_UN` on drop
/// ([`Flock`]): a lock released only by the close stays held while any child another
/// thread is spawning still holds a copy of the descriptor, and every writer queued behind
/// it waits that spawn out of its [`LOCK_WAIT`] too.
///
/// # Errors
/// The lock could not be created or was not granted within [`LOCK_WAIT`].
pub(super) fn lock_for_update(path: &Path) -> io::Result<Flock> {
    let name = file_name(path)?;
    let mut lock = String::from(name);
    lock.push_str(".lock");
    let lock = path.with_file_name(lock);
    Flock::lock_within(open_lock_file(&lock)?, LOCK_WAIT, &lock)
}

/// Open (creating `0600`, never truncating: a lock file is a rendezvous, not data) the
/// lock file at `path`; std opens it close-on-exec.
fn open_lock_file(path: &Path) -> io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    opts.open(path)
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

/// `path`'s final component as UTF-8.
fn file_name(path: &Path) -> io::Result<&str> {
    path.file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))
}

/// Create, write and flush the temp. `0644` on the handle as well as at creation: the
/// umask may clear bits, and under a system prefix a non-root reader must still read what
/// root wrote.
fn stage(tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    stage_with(tmp, bytes, crate::platform::sync_file_contents)
}

/// [`stage`], flushed by `flush`.
fn stage_with(
    tmp: &Path,
    bytes: &[u8],
    flush: fn(&std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let mut f = create_temp(tmp)?;
    crate::platform::set_mode_on(&f, 0o644)?;
    f.write_all(bytes)?;
    match flush(&f) {
        // A volume that cannot flush (some network or FUSE mounts) is not a failure.
        Err(e)
            if !matches!(
                e.kind(),
                io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
            ) =>
        {
            Err(e)
        }
        _ => Ok(()),
    }
}

/// Create `tmp` exclusively: an existing file of that name is never written through.
#[cfg(unix)]
fn create_temp(tmp: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(tmp)
}

#[cfg(not(unix))]
fn create_temp(tmp: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
}

/// Read `path` as TOML of type `T`: `None` when it is absent, not a bounded regular file,
/// not UTF-8, or does not parse.
pub(super) fn read_toml<T: serde::de::DeserializeOwned>(path: &Path, limit: usize) -> Option<T> {
    let text = read_durable(path, limit).ok()?;
    aterm_toml::from_str(&text).ok()
}

/// Read a [`write_durable`] file whole, bounded to `limit` bytes of UTF-8: the old file or
/// the new one, a refusal while a replace is in flight tried again
/// ([`again_while_refused`]).
fn read_durable(path: &Path, limit: usize) -> io::Result<String> {
    again_while_refused(|| crate::metadata_io::read_bounded_regular_utf8(path, limit))
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
mod tests {
    use super::*;

    fn dir(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-vendor-durable-{label}-"))
    }

    /// Each replace names its flush (aterm-durable's counting seam): the durable one's
    /// directory flush is durable (its temp takes a plain `fsync(2)`, which the seam does
    /// not see); the ordered one orders the temp and the directory, and the vendor stamp
    /// takes it.
    #[test]
    fn each_replace_names_its_flush() {
        use aterm_durable::{Flush, seam::Recording};
        let d = dir("flush-tiers");
        let dest = d.join("x.toml");
        let recording = Recording::start();
        write_durable(&dest, b"a = 1\n").unwrap();
        assert_eq!(recording.take(), [Flush::Durable]);
        write_ordered(&dest, b"a = 2\n").unwrap();
        assert_eq!(recording.take(), [Flush::Ordered, Flush::Ordered]);
        assert_eq!(std::fs::read(&dest).unwrap(), b"a = 2\n");
    }

    /// A leftover temp a dead process of this pid left under the next name is replaced;
    /// another pid's is ignored.
    #[test]
    fn a_leftover_temp_is_replaced_or_ignored() {
        let d = dir("leftover");
        let dest = d.join("x.toml");
        let stale = temp_path(&dest, u64::MAX).unwrap();
        std::fs::write(&stale, b"garbage garbage garbage garbage").unwrap();
        let foreign = d.join(".x.toml.tmp-1-0");
        std::fs::write(&foreign, b"garbage").unwrap();
        replace_via(&stale, &dest, b"a = 1\n").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"a = 1\n");
        assert!(
            !stale.exists(),
            "the stale temp was replaced and renamed into place"
        );
        write_durable(&dest, b"a = 2\n").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"a = 2\n");
        assert!(foreign.exists(), "another pid's temp is left alone");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o644);
        }
    }

    /// A failed rename leaves the destination untouched and no temp behind.
    #[test]
    fn a_failed_replace_leaves_no_temp() {
        let d = dir("fail");
        let dest = d.join("x.toml");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("keep"), b"").unwrap();
        assert!(write_durable(&dest, b"a = 1\n").is_err());
        let temps = std::fs::read_dir(&d)
            .unwrap()
            .filter(|e| {
                let name = e.as_ref().unwrap().file_name();
                name.to_str().unwrap().starts_with(".x.toml.tmp-")
            })
            .count();
        assert_eq!(temps, 0);
        assert!(dest.join("keep").exists());
    }

    /// Concurrent writers of one file never expose a torn file: each stages through its
    /// own temp.
    #[test]
    fn concurrent_writers_never_tear_the_file() {
        use std::sync::atomic::{AtomicBool, AtomicUsize};
        let d = dir("race");
        let dest = d.join("x.toml");
        let a = vec![b'A'; 256 * 1024];
        let b = vec![b'B'; 128 * 1024];
        write_durable(&dest, &a).unwrap();
        let done = AtomicBool::new(false);
        let torn = AtomicUsize::new(0);
        std::thread::scope(|s| {
            let writers: Vec<_> = (0..8)
                .map(|i| {
                    let (dest, body) = (&dest, if i % 2 == 0 { &a } else { &b });
                    s.spawn(move || {
                        for _ in 0..100 {
                            write_durable(dest, body).unwrap();
                        }
                    })
                })
                .collect();
            // Through the lane's own reader, as every reader of these files reads them.
            let reader = s.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    let got = read_durable(&dest, 1024 * 1024).unwrap().into_bytes();
                    if got != a && got != b {
                        torn.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
            let written: Vec<_> = writers.into_iter().map(|w| w.join()).collect();
            done.store(true, Ordering::Relaxed);
            reader.join().unwrap();
            for result in written {
                result.unwrap();
            }
        });
        assert_eq!(torn.load(Ordering::Relaxed), 0, "a reader saw a torn file");
        let last = std::fs::read(&dest).unwrap();
        assert!(last == a || last == b);
    }

    /// A dropped update lock is free at once while a copy of its descriptor lives — the
    /// copy a child another thread is spawning holds until it execs.
    #[test]
    fn a_dropped_update_lock_is_free_while_a_copy_of_its_descriptor_lives() {
        let d = dir("lock-dup");
        let dest = d.join("x.toml");
        let held = lock_for_update(&dest).unwrap();
        let childs_copy = held.try_clone().unwrap();
        drop(held);
        let again = lock_for_update(&dest).expect("free while its copy lives");
        drop((again, childs_copy));
    }

    /// A lock still held is refused at the bound.
    #[test]
    fn a_held_update_lock_times_out() {
        let d = dir("lock-held");
        let dest = d.join("x.toml");
        let _held = lock_for_update(&dest).unwrap();
        let lock = d.join("x.toml.lock");
        let err = Flock::lock_within(
            open_lock_file(&lock).unwrap(),
            Duration::from_millis(120),
            &lock,
        )
        .err()
        .expect("held");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }
}
