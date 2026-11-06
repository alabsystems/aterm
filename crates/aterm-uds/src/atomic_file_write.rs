// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole-file publication through a sibling temporary on Unix and Windows.

use aterm_durable::Flush;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Write `bytes` to `path` whole ([`write_checked`] with no check). `mode` supplies Unix
/// permission bits; Windows has no Unix permission bits and ignores it.
/// # Errors
/// No entropy for the temporary name, a clash on every name tried, or a write's own error: `path`
/// is left as it was. Or the directory's flush after the rename failed: `path` holds `bytes`,
/// but its entry may not survive a power loss.
pub fn write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_checked(path, bytes, mode, || Ok(()))
}

/// [`write`], finished by `flush`. Every arm writes a temporary beside `path` and renames it
/// over `path`, so while the machine runs a reader finds `path` as it was or holding all of
/// `bytes`, never part of them. The arms differ in what a power loss may leave:
/// - `Some(Flush::Durable)` is [`write`]: the temporary is flushed in full before the rename,
///   and `path`'s directory after it.
/// - `Some(Flush::Ordered)` takes barriers there instead ([`aterm_durable::ordered`]): the
///   temporary's bytes are ordered ahead of the rename, and the rename ahead of later writes on
///   its device.
/// - `None` flushes neither, so after a power loss `path` may hold what it held before, or a
///   file that lacks some or all of `bytes`. It suits a file every reader treats as advisory,
///   such as a pid file, whose process no power loss leaves running.
/// # Errors
/// As [`write`]. With `None` there is no directory flush to fail.
pub fn write_flushed(path: &Path, bytes: &[u8], mode: u32, flush: Option<Flush>) -> io::Result<()> {
    write_landing(path, bytes, mode, || Ok(()), flush, Flush::dir)?
}

/// Produce a file atomically, with the same permissions and durability as [`write`].
/// The producer must finish and flush any buffering it owns before returning success.
///
/// # Errors
/// As [`write`], or the producer fails. A producer error leaves the old destination intact.
pub fn write_with(
    path: &Path,
    mode: u32,
    produce: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
) -> io::Result<()> {
    write_produced_landing(
        path,
        mode,
        produce,
        || Ok(()),
        Some(Flush::Durable),
        Flush::dir,
    )?
}

/// Produce a file atomically and return metadata captured from its completed temporary
/// before closing and renaming it. A caller identifying the path afterward must compare
/// the platform's file identity with the captured metadata; rename can change timestamps.
///
/// # Errors
/// As [`write_with`], or reading the completed temporary's metadata fails.
pub fn write_with_metadata(
    path: &Path,
    mode: u32,
    produce: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
) -> io::Result<std::fs::Metadata> {
    let (metadata, landed) = write_captured_landing(
        path,
        mode,
        produce,
        std::fs::File::metadata,
        || Ok(()),
        Some(Flush::Durable),
        Flush::dir,
    )?;
    landed?;
    Ok(metadata)
}

/// Write `bytes` to `path` with `mode`: a temporary file beside it, durably flushed
/// ([`aterm_durable::durable`]), renamed over it, and the directory's entries flushed after
/// the rename. That flush is best-effort ([`aterm_durable::Flush::dir_best_effort`]): a
/// directory whose entries cannot be flushed here is no error, and any other failure is.
/// # Errors
/// As [`fn@write`], and `check`'s error.
pub fn write_checked(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    check: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    write_flushing_dir(path, bytes, mode, check, Flush::dir)
}

/// [`write_checked`] with the directory's flush (`flush_dir`, handed the [`Flush`] it is to
/// take) handed in, so a test can make it fail; its outcome is classified here either way.
pub(super) fn write_flushing_dir(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    check: impl FnOnce() -> io::Result<()>,
    flush_dir: impl FnOnce(Flush, &Path) -> io::Result<()>,
) -> io::Result<()> {
    write_landing(path, bytes, mode, check, Some(Flush::Durable), flush_dir)?
}

/// What a careful write's rename left ([`write_landing`]): `path` holds the bytes either way.
/// `Err` is the directory's flush after the rename, failed other than as unavailable, so the
/// entry may not survive a power loss. A write finished by no flush has none to fail.
pub(super) type Landed = io::Result<()>;

/// [`write_flushed`] with a check before the rename and the directory's flush handed in,
/// telling a failure before the rename (the outer `Err`: `path` is as it was) from one after it
/// ([`Landed`]).
pub(super) fn write_landing(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    check: impl FnOnce() -> io::Result<()>,
    flush: Option<Flush>,
    flush_dir: impl FnOnce(Flush, &Path) -> io::Result<()>,
) -> io::Result<Landed> {
    write_produced_landing(
        path,
        mode,
        |writer| writer.write_all(bytes),
        check,
        flush,
        flush_dir,
    )
}

struct Temporary {
    path: PathBuf,
    armed: bool,
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(super) fn write_produced_landing(
    path: &Path,
    mode: u32,
    produce: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
    check: impl FnOnce() -> io::Result<()>,
    flush: Option<Flush>,
    flush_dir: impl FnOnce(Flush, &Path) -> io::Result<()>,
) -> io::Result<Landed> {
    write_captured_landing(path, mode, produce, |_| Ok(()), check, flush, flush_dir)
        .map(|(_, landed)| landed)
}

/// The careful write itself: `produce` writes the temporary, `flush` (when there is one)
/// finishes it, `capture` reads it, `check` may still refuse the rename, and after the rename
/// `flush_dir` takes the same flush of `path`'s directory. With no `flush` neither the
/// temporary nor the directory is flushed.
pub(super) fn write_captured_landing<T>(
    path: &Path,
    mode: u32,
    produce: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
    capture: impl FnOnce(&std::fs::File) -> io::Result<T>,
    check: impl FnOnce() -> io::Result<()>,
    flush: Option<Flush>,
    flush_dir: impl FnOnce(Flush, &Path) -> io::Result<()>,
) -> io::Result<(T, Landed)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (tmp, file) = create_temp_beside(path, mode)?;
    let mut temporary = Temporary {
        path: tmp,
        armed: true,
    };
    let mut file = file;
    produce(&mut file)?;
    #[cfg(unix)]
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))?;
    #[cfg(windows)]
    let _ = mode;
    if let Some(flush) = flush {
        flush.file(&file)?;
    }
    let captured = capture(&file)?;
    drop(file);
    check()?;
    #[cfg(any(test, debug_assertions))]
    if let Some(error) = publication_seam::failure() {
        return Err(error);
    }
    std::fs::rename(&temporary.path, path)?;
    // The old temporary name is free after the rename; do not remove a later user's file.
    temporary.armed = false;
    let Some(flush) = flush else {
        return Ok((captured, Ok(())));
    };
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    // Unavailable (a volume that cannot flush a directory, a directory this process may
    // write but not open) is accepted: the bytes are flushed on their own handle, and the
    // rename has landed. A failed flush is returned as it came, its errno kept for a caller
    // that classifies it.
    Ok((
        captured,
        aterm_durable::DirFlushed::classify(flush_dir(flush, dir))
            .map(|_flushed_or_unavailable| ()),
    ))
}

/// Deterministic publication failures for tests using the real atomic writer.
#[cfg(any(test, debug_assertions))]
pub mod publication_seam {
    use std::cell::Cell;
    use std::io;

    thread_local! {
        static FAILURE: Cell<Option<io::ErrorKind>> = const { Cell::new(None) };
    }

    /// Refuse this thread's renames after writing the temporary and taking its flush, if any.
    /// Nested guards restore the preceding failure when dropped.
    #[must_use = "the failure stops when the guard drops"]
    pub struct FailBeforeRename {
        outer: Option<io::ErrorKind>,
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl FailBeforeRename {
        /// Install a failure without changing any destination file.
        pub fn with_kind(kind: io::ErrorKind) -> Self {
            Self {
                outer: FAILURE.with(|failure| failure.replace(Some(kind))),
                _thread: std::marker::PhantomData,
            }
        }
    }

    impl Drop for FailBeforeRename {
        fn drop(&mut self) {
            FAILURE.with(|failure| failure.set(self.outer.take()));
        }
    }

    pub(super) fn failure() -> Option<io::Error> {
        FAILURE.with(|failure| failure.get().map(io::Error::from))
    }
}

/// The bytes of a temporary name's salt ([`create_temp_beside`]), written as twice as many hex
/// digits.
const SALT_BYTES: usize = 4;

/// A NEW file beside `path` to write it through.
/// # Errors
/// No entropy for the name, a clash on every try, or the create's own error.
pub fn create_temp_beside(path: &Path, mode: u32) -> io::Result<(PathBuf, std::fs::File)> {
    create_temp_salted(path, mode, crate::rand::hex_token::<SALT_BYTES>)
}

/// An age to pass [`abandoned_temps_beside`] and [`sweep_temps_beside`] as `older_than`: far past
/// any write through a file [`create_temp_beside`] made, which lasts a flush and a rename, with
/// a second or so at most of waiting for a lock or checking between.
pub const ABANDONED_AFTER: Duration = Duration::from_secs(60 * 60);

/// The files [`create_temp_beside`] made beside `path` that no write will rename, sorted, as
/// [`temp_writer`] reads their names (so none for a `path` that has no name, or one that is not
/// UTF-8, whose temporaries are named lossily): each a regular
/// file whose pid names no live process ([`crate::process::pid_alive`]) and that was last
/// written more than `older_than` ago. A writer that ends between the create and the rename
/// (killed, or its process exiting under a thread that is still writing) leaves its file for
/// good. One whose pid names a live process is not named (its writer's, or that of a later
/// process given the pid, until it ends), nor one written within `older_than` or dated ahead of
/// this clock, a link, a folder or any other name. A writer on another host that shares the
/// folder is kept by the age alone: while the time since it last wrote, with however far the
/// clock that dated the write (its own or the file server's) runs behind this one, stays within
/// `older_than`.
/// # Errors
/// The folder, or an entry of its listing, cannot be read (a missing folder names none).
pub fn abandoned_temps_beside(path: &Path, older_than: Duration) -> io::Result<Vec<PathBuf>> {
    abandoned_with(
        path,
        older_than,
        std::time::SystemTime::now(),
        crate::process::pid_alive,
    )
}

/// [`abandoned_temps_beside`] at `now`, with `alive` answering for a writer's pid.
fn abandoned_with(
    path: &Path,
    older_than: Duration,
    now: std::time::SystemTime,
    alive: impl Fn(u32) -> bool,
) -> io::Result<Vec<PathBuf>> {
    if path.file_name().and_then(|n| n.to_str()).is_none() {
        return Ok(Vec::new());
    }
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut due = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(pid) = temp_writer(path, &entry.file_name()) else {
            continue;
        };
        let old = entry.metadata().is_ok_and(|meta| {
            meta.is_file()
                && meta
                    .modified()
                    .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > older_than))
        });
        if old && !alive(pid) {
            due.push(entry.path());
        }
    }
    due.sort();
    Ok(due)
}

/// Remove each file [`abandoned_temps_beside`] names for `path` and `older_than`. Returns how
/// many went; one that went meanwhile is no error and not counted.
/// # Errors
/// As [`abandoned_temps_beside`], or one could not be removed: the first such error, once every
/// other one was tried.
pub fn sweep_temps_beside(path: &Path, older_than: Duration) -> io::Result<usize> {
    sweep_temps_with(
        path,
        older_than,
        std::time::SystemTime::now(),
        crate::process::pid_alive,
        |temp| std::fs::remove_file(temp),
    )
}

/// [`sweep_temps_beside`] at `now`, with `alive` answering for a writer's pid and `remove`
/// removing a due file.
pub(super) fn sweep_temps_with(
    path: &Path,
    older_than: Duration,
    now: std::time::SystemTime,
    alive: impl Fn(u32) -> bool,
    remove: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<usize> {
    let (mut removed, mut first) = (0, None);
    for temp in abandoned_with(path, older_than, now, alive)? {
        match remove(&temp) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    first.map_or(Ok(removed), Err)
}

/// The pid of the writer whose temporary for `path` ([`create_temp_beside`]) is named `name`:
/// `.<path's name>.tmp.<pid>.<salt>`, the pid in decimal digits and the salt as 8 lowercase hex
/// digits (`SALT_BYTES`). `None` for any other name, when either name is not UTF-8, and for a
/// `path` that has no name.
#[must_use]
pub fn temp_writer(path: &Path, name: &std::ffi::OsStr) -> Option<u32> {
    let target = path.file_name()?.to_str()?;
    let tail = name
        .to_str()?
        .strip_prefix('.')?
        .strip_prefix(target)?
        .strip_prefix(".tmp.")?;
    let (pid, salt) = tail.split_once('.')?;
    let salted = salt.len() == 2 * SALT_BYTES
        && salt.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !salted || pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

/// The name of the file a temporary named `name` stands in for ([`create_temp_beside`]):
/// `name` less its leading `.` and its `.tmp.<pid>.<salt>`, in [`temp_writer`]'s grammar, so a
/// folder's listing can say whose temporaries it holds. `None` for any other name.
#[must_use]
pub fn temp_target(name: &std::ffi::OsStr) -> Option<&str> {
    let (head, _salt) = name.to_str()?.strip_prefix('.')?.rsplit_once('.')?;
    let (head, _pid) = head.rsplit_once('.')?;
    let target = head.strip_suffix(".tmp")?;
    temp_writer(Path::new(target), name).map(|_| target)
}

pub(super) fn create_temp_salted(
    path: &Path,
    mode: u32,
    mut salt: impl FnMut() -> io::Result<String>,
) -> io::Result<(PathBuf, std::fs::File)> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    for _ in 0..8 {
        let salt = salt()?;
        let tmp = path.with_file_name(format!(".{name}.tmp.{}.{salt}", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options
            .mode(mode)
            .custom_flags(crate::atomic_file::LOCK_OPEN_FLAGS);
        #[cfg(windows)]
        let _ = mode;
        match options.open(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "every temporary name tried beside {} was already taken",
            path.display()
        ),
    ))
}

#[cfg(test)]
#[path = "atomic_file_write_tests.rs"]
mod tests;
