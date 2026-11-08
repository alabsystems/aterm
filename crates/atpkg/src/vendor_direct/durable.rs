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
    if let Err(error) = stage_with(&tmp, bytes, aterm_durable::ordered)
        .and_then(|()| again_while_refused(|| std::fs::rename(&tmp, path)))
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
    if let Err(error) =
        stage(tmp, bytes).and_then(|()| again_while_refused(|| std::fs::rename(tmp, path)))
    {
        let _ = std::fs::remove_file(tmp);
        return Err(error);
    }
    let _flushed_or_unavailable =
        aterm_durable::Flush::Durable.dir_best_effort(parent_dir(path))?;
    Ok(())
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
    let file = open_lock_file(&lock)?;
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

/// Open (creating `0600`, never truncating: a lock file is a rendezvous, not data) the lock file at
/// `path`.
fn open_lock_file(path: &Path) -> io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).read(true).write(true).truncate(false);
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

/// Read a [`write_durable`] file whole ([`again_while_refused`]).
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

    #[test]
    fn windows_replace_waits_for_a_scheduled_file_holder() {
        let d = dir("scheduled-windows-replace");
        let staged = d.join("staged");
        let dest = d.join("dest");
        std::fs::write(&staged, b"complete").unwrap();
        std::fs::write(&dest, b"before").unwrap();
        let mut calls = 0;
        let result = again_while_refused_windows_at(
            || {
                calls += 1;
                if calls <= 21 {
                    Err(io::ErrorKind::PermissionDenied.into())
                } else {
                    std::fs::rename(&staged, &dest)
                }
            },
            std::time::Instant::now() - Duration::from_secs(1),
            |_| {},
        );
        result.expect("the Windows holder releases before the hang detector");
        assert_eq!(calls, 22);
        assert_eq!(std::fs::read(&dest).unwrap(), b"complete");
        assert!(!staged.exists());
    }

    #[test]
    fn windows_refusal_expiry_keeps_the_error_and_unpublished_bytes() {
        let d = dir("windows-refusal");
        let staged = d.join("staged");
        let dest = d.join("dest");
        std::fs::write(&staged, b"complete").unwrap();
        std::fs::write(&dest, b"before").unwrap();
        for (kind, elapsed) in [
            (io::ErrorKind::PermissionDenied, Duration::from_secs(61)),
            (io::ErrorKind::NotFound, Duration::ZERO),
        ] {
            let mut calls = 0;
            let out: io::Result<()> = again_while_refused_windows_at(
                || {
                    calls += 1;
                    Err(io::Error::new(kind, std::fmt::Error))
                },
                std::time::Instant::now() - elapsed,
                |_| panic!("an expired wait or unrelated error must not wait again"),
            );
            let error = out.unwrap_err();
            assert_eq!(error.kind(), kind);
            assert!(error.get_ref().unwrap().is::<std::fmt::Error>());
            assert_eq!(calls, 1);
        }
        assert_eq!(std::fs::read(&staged).unwrap(), b"complete");
        assert_eq!(std::fs::read(&dest).unwrap(), b"before");
    }

    #[test]
    fn digest_update_waits_for_a_scheduled_native_lock_holder() {
        let d = dir("scheduled-lock");
        let dest = d.join("x.toml");
        std::fs::write(&dest, b"unchanged").unwrap();
        let mut previous = Some(lock_for_update(&dest).unwrap());
        let held = lock_for_update_with(&dest, Duration::from_secs(6), |file, left, lock| {
            assert!(
                matches!(
                    Flock::try_lock(file.try_clone().unwrap()),
                    Err(std::fs::TryLockError::WouldBlock)
                ),
                "CONTROL: the native predecessor still owns the rendezvous"
            );
            drop(previous.take());
            Flock::lock_within(file, left, lock)
        })
        .expect("a scheduled native holder may finish before the hang detector");
        assert_eq!(std::fs::read(&dest).unwrap(), b"unchanged");
        let late = lock_for_update_after(&dest, Duration::from_secs(61))
            .err()
            .expect("expired");
        assert_eq!(late.kind(), io::ErrorKind::TimedOut);
        assert!(
            matches!(
                Flock::try_lock(open_lock_file(&d.join("x.toml.lock")).unwrap()),
                Err(std::fs::TryLockError::WouldBlock)
            ),
            "the exhausted wait preserves the actual native holder"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"unchanged");
        drop(held);
    }

    #[test]
    fn a_staged_file_accepts_a_refused_flush_after_writing_its_body() {
        let d = dir("refused-flush");
        let tmp = d.join("stage");
        stage_with(&tmp, b"a = 1\n", |_| {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        })
        .unwrap();
        assert_eq!(std::fs::read(tmp).unwrap(), b"a = 1\n");
    }

    /// A raw Darwin refusal that no std names by a refusal kind, `ENODEV` (what a full flush
    /// of `/dev/null` answers), is accepted too: staging reads astream's rule, not a list of
    /// kinds. Darwin's `ENOTSUP` (45) would not show that: the Trust std reads it as
    /// `Unsupported`, the kind the test above feeds. CONTROL: `ENODEV`'s kind alone is no
    /// refusal.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_staged_file_accepts_a_darwin_raw_flush_refusal() {
        let enodev = io::Error::from_raw_os_error(libc::ENODEV);
        assert!(
            aterm_durable::refusal_accepted(Err(io::Error::from(enodev.kind()))).is_err(),
            "CONTROL: std names ENODEV by kind ({enodev:?}); the errno half is untested"
        );
        let d = dir("raw-refused-flush");
        let tmp = d.join("stage");
        stage_with(&tmp, b"a = 1\n", |_| {
            Err(io::Error::from_raw_os_error(libc::ENODEV))
        })
        .unwrap();
        assert_eq!(std::fs::read(tmp).unwrap(), b"a = 1\n");
    }

    #[test]
    fn a_staged_file_reports_a_failed_flush_after_writing_its_body() {
        let d = dir("failed-flush");
        let tmp = d.join("stage");
        let error =
            stage_with(&tmp, b"a = 1\n", |_| Err(io::Error::other(std::fmt::Error))).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.get_ref().unwrap().is::<std::fmt::Error>());
        assert_eq!(std::fs::read(tmp).unwrap(), b"a = 1\n");
    }

    #[test]
    fn accepting_a_flush_refusal_does_not_accept_a_failed_stage_open() {
        let d = dir("occupied-stage");
        let tmp = d.join("stage");
        std::fs::write(&tmp, b"keep").unwrap();
        let error = stage_with(&tmp, b"replace", |_| {
            panic!("an unsuccessful open cannot reach the flush")
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        let error = stage_with(&d.join("stage\0"), b"replace", |_| {
            panic!("an invalid path cannot reach the flush")
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(std::fs::read(tmp).unwrap(), b"keep");
    }

    #[test]
    fn each_replace_names_its_flush() {
        use aterm_durable::{Flush, seam::Recording};
        let d = dir("flush-tiers");
        let dest = d.join("x.toml");
        let recording = Recording::start();
        write_durable(&dest, b"a = 1\n").unwrap();
        // The directory's flush is recorded everywhere (on Windows `Flush::dir` only notes
        // it). The temp's durable flush is recorded on Windows (`durable_data`) and not on
        // Unix, where it is atpkg's own `fsync`.
        let durable: &[Flush] = if cfg!(windows) {
            &[Flush::Durable, Flush::Durable]
        } else {
            &[Flush::Durable]
        };
        assert_eq!(recording.take(), durable);
        write_ordered(&dest, b"a = 2\n").unwrap();
        assert_eq!(recording.take(), [Flush::Ordered, Flush::Ordered]);
        assert_eq!(std::fs::read(&dest).unwrap(), b"a = 2\n");
    }

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

    #[test]
    fn a_held_update_lock_times_out() {
        let d = dir("lock-held");
        let dest = d.join("x.toml");
        let _held = lock_for_update(&dest).unwrap();
        let err = lock_for_update_after(&dest, Duration::from_secs(61))
            .err()
            .expect("held");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[cfg(unix)]
    #[test]
    fn lock_for_update_refuses_symlink_lock_file() {
        let d = dir("lock-symlink");
        let dest = d.join("x.toml");
        let victim = d.join("victim");
        std::fs::write(&victim, b"untouched").unwrap();
        std::os::unix::fs::symlink(&victim, d.join("x.toml.lock")).unwrap();
        assert!(lock_for_update(&dest).is_err());
    }
}
