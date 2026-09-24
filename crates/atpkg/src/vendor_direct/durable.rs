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

/// [`write_durable`] through the temp `tmp`.
fn replace_via(tmp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Only a dead process that had our pid can have left this name.
    let _ = std::fs::remove_file(tmp);
    if let Err(error) = stage(tmp, bytes).and_then(|()| std::fs::rename(tmp, path)) {
        let _ = std::fs::remove_file(tmp);
        return Err(error);
    }
    crate::store::sync_dir(path.parent().unwrap_or_else(|| Path::new(".")));
    Ok(())
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
    tmp.push_str(&crate::dec_u64(u64::from(std::process::id())));
    tmp.push('-');
    tmp.push_str(&crate::dec_u64(seq));
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
    let mut f = create_temp(tmp)?;
    crate::platform::set_mode_on(&f, 0o644)?;
    f.write_all(bytes)?;
    match crate::platform::sync_file_contents(&f) {
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
    let text = crate::metadata_io::read_bounded_regular_utf8(path, limit).ok()?;
    aterm_toml::from_str(&text).ok()
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

    fn dir(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "atpkg-vendor-durable-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
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

    #[test]
    fn every_write_has_its_own_temp() {
        let dest = Path::new("/x/claude.toml");
        let a = temp_path(dest, 0).unwrap();
        assert_ne!(a, temp_path(dest, 1).unwrap());
        let name = a.file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(name, format!(".claude.toml.tmp-{}-0", std::process::id()));
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
            let reader = s.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    let got = std::fs::read(&dest).unwrap();
                    if got != a && got != b {
                        torn.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
            for w in writers {
                w.join().unwrap();
            }
            done.store(true, Ordering::Relaxed);
            reader.join().unwrap();
        });
        assert_eq!(torn.load(Ordering::Relaxed), 0, "a reader saw a torn file");
        let last = std::fs::read(&dest).unwrap();
        assert!(last == a || last == b);
    }

    /// The update lock excludes a second holder, in this process as in another.
    #[test]
    fn the_update_lock_is_exclusive() {
        let d = dir("lock");
        let dest = d.join("x.toml");
        let held = lock_for_update(&dest).unwrap();
        assert!(d.join("x.toml.lock").exists());
        std::thread::scope(|s| {
            let waiter = s.spawn(|| {
                let start = std::time::Instant::now();
                let _second = lock_for_update(&dest).unwrap();
                start.elapsed()
            });
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
            assert!(waiter.join().unwrap() >= Duration::from_millis(250));
        });
    }

    /// A dropped update lock is free at once while a copy of its descriptor lives — the
    /// copy a child another thread is spawning holds until it execs. Released by the close,
    /// the next writer waited out [`LOCK_WAIT`] and failed (the stamp's concurrent-writer
    /// test under a loaded suite).
    #[test]
    fn a_dropped_update_lock_is_free_while_a_copy_of_its_descriptor_lives() {
        let d = dir("lock-dup");
        let dest = d.join("x.toml");
        let held = lock_for_update(&dest).unwrap();
        let childs_copy = held.try_clone().unwrap();
        drop(held);
        let start = std::time::Instant::now();
        let again = lock_for_update(&dest).expect("free while its copy lives");
        assert!(start.elapsed() < LOCK_WAIT / 2, "{:?}", start.elapsed());
        drop((again, childs_copy));
    }

    /// A lock still held is refused at the bound, naming the lock file.
    #[test]
    fn a_held_update_lock_times_out_naming_its_file() {
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
        assert!(
            err.to_string().contains(&lock.display().to_string()),
            "{err}"
        );
    }
}
