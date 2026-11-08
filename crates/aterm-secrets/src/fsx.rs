// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admitted file reads (`O_NOFOLLOW | O_NONBLOCK`) and atomic 0600 writes.

use crate::{Error, SecretBytes};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

fn check_and_narrow_dir_fd(dir: &Path, dir_fd: &File) -> crate::Result<()> {
    let meta = dir_fd
        .metadata()
        .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    if !meta.is_dir() {
        return Err(Error::new(format!("{}: not a directory", dir.display())));
    }
    if meta.uid() != euid() {
        return Err(Error::new(format!(
            "{}: owned by uid {}, not this user",
            dir.display(),
            meta.uid()
        )));
    }
    if meta.mode() & 0o7777 != 0o700 {
        dir_fd
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    }
    Ok(())
}

/// Ensures `dir` exists as a 0700 directory owned by this user, refusing symlinks.
pub fn ensure_private_dir(dir: &Path) -> crate::Result<()> {
    let open_dir = || {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)
    };
    if let Ok(dir_fd) = open_dir() {
        return check_and_narrow_dir_fd(dir, &dir_fd);
    }
    match kind(dir)? {
        Kind::Dir => {}
        Kind::Missing => {
            if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)
                    .map_err(|e| Error::new(format!("{}: {e}", parent.display())))?;
            }
            match std::fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(Error::new(format!("{}: {e}", dir.display()))),
            }
        }
        Kind::Symlink => {
            return Err(Error::new(format!(
                "{}: is a symlink; refusing to keep secrets through it",
                dir.display()
            )));
        }
        Kind::File | Kind::Other => {
            return Err(Error::new(format!("{}: not a directory", dir.display())));
        }
    }
    if let Ok(dir_fd) = open_dir() {
        return check_and_narrow_dir_fd(dir, &dir_fd);
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    if !meta.is_dir() {
        return Err(Error::new(format!("{}: not a directory", dir.display())));
    }
    if meta.uid() != euid() {
        return Err(Error::new(format!(
            "{}: owned by uid {}, not this user",
            dir.display(),
            meta.uid()
        )));
    }
    if meta.mode() & 0o7777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    }
    Ok(())
}

/// Who may own and what mode a read admits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Admit {
    /// Owned by this user, no group/other bits.
    Private,
    /// Owned by this user, no group/other write.
    Owned,
    /// `aterm.toml`, by the one rule every reader of it holds
    /// ([`aterm_types::dirs::aterm_config_refusal`]): owned by this user or by root, no
    /// group/other write.
    AtermConfig,
    /// Anyone's.
    Any,
}

/// What a read does with a symlink at its path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Links {
    /// Refuse it (`O_NOFOLLOW`).
    Refuse,
    /// Follow it to the file it names, which the read then admits.
    Follow,
}

/// What a read does with a file longer than its cap.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Past {
    /// Refuse it.
    Refuse,
    /// Keep its first `max` bytes.
    Head,
}

/// Open and read a regular file under `admit`, at most `max` bytes.
fn read_admitted(path: &Path, max: usize, admit: Admit) -> crate::Result<Option<Vec<u8>>> {
    read_admitted_past(path, max, admit, Past::Refuse)
}

/// [`read_admitted`], doing `past` with a file longer than `max` bytes.
fn read_admitted_past(
    path: &Path,
    max: usize,
    admit: Admit,
    past: Past,
) -> crate::Result<Option<Vec<u8>>> {
    read_opened(path, max, admit, past, Links::Refuse)
}

/// [`read_admitted_past`], doing `links` with a symlink at `path`: the file it opens, followed
/// or not, is the one admitted and read.
fn read_opened(
    path: &Path,
    max: usize,
    admit: Admit,
    past: Past,
    links: Links,
) -> crate::Result<Option<Vec<u8>>> {
    Ok(read_opened_stamped(path, max, admit, past, links)
        .map_err(Unread::into_error)?
        .map(|(_, bytes)| bytes))
}

/// Why a stamped read ([`read_owned_stamped`]) read no bytes from a file at its path.
#[derive(Debug)]
pub enum Unread {
    /// The file is one the reader refuses as it is: a symlink, not a regular file, owned by
    /// another user, a mode that lets other users write it, or past the bound. Read again, it is
    /// refused again until it changes.
    Refused(Error),
    /// The open, `fstat` or read failed (too many open files, an I/O error): read again, it may
    /// read.
    Failed(Error),
}

impl Unread {
    /// The error either way.
    pub fn into_error(self) -> Error {
        match self {
            Self::Refused(e) | Self::Failed(e) => e,
        }
    }
}

/// [`read_opened`], with the [`FileStamp`] of the file it opened, taken (`fstat`) before it reads
/// a byte: a change during the read leaves the file's stamp unlike this one.
fn read_opened_stamped(
    path: &Path,
    max: usize,
    admit: Admit,
    past: Past,
    links: Links,
) -> Result<Option<(FileStamp, Vec<u8>)>, Unread> {
    let Some((file, meta)) = open_admitted(path, admit, links)? else {
        return Ok(None);
    };
    let refused = |why: String| Unread::Refused(Error::new(format!("{}: {why}", path.display())));
    let failed = |e: io::Error| Unread::Failed(Error::new(format!("{}: {e}", path.display())));
    let limit = u64::try_from(max).unwrap_or(u64::MAX);
    if past == Past::Refuse && meta.len() > limit {
        return Err(refused(format!("larger than {max} bytes")));
    }
    let mut bytes = read_buffer(meta.len(), max);
    let limit = if past == Past::Head {
        limit
    } else {
        limit.saturating_add(1)
    };
    crate::secret::read_to_end_wiping_on_error(file.take(limit), &mut bytes).map_err(failed)?;
    if bytes.len() > max {
        zeroize::Zeroize::zeroize(&mut bytes);
        return Err(refused(format!("larger than {max} bytes")));
    }
    Ok(Some((FileStamp::of(&meta), bytes)))
}

fn open_admitted(
    path: &Path,
    admit: Admit,
    links: Links,
) -> Result<Option<(File, std::fs::Metadata)>, Unread> {
    let nofollow = if links == Links::Refuse {
        libc::O_NOFOLLOW
    } else {
        0
    };
    let refused = |why: String| Unread::Refused(Error::new(format!("{}: {why}", path.display())));
    let failed = |e: io::Error| Unread::Failed(Error::new(format!("{}: {e}", path.display())));
    #[cfg(test)]
    count_read(path);
    #[cfg(test)]
    if take_read_failure(path) {
        return Err(failed(io::Error::other("the test's disk failed the read")));
    }
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(nofollow | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) if links == Links::Refuse && e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(refused("is a symlink; refused".to_owned()));
        }
        Err(e) => return Err(failed(e)),
    };
    let meta = file.metadata().map_err(failed)?;
    if !meta.file_type().is_file() {
        return Err(refused("not a regular file".to_owned()));
    }
    match admit {
        Admit::Any => {}
        Admit::AtermConfig => {
            if let Some(why) = aterm_types::dirs::aterm_config_file_refusal(&meta) {
                return Err(refused(why));
            }
        }
        Admit::Private | Admit::Owned => {
            if meta.uid() != euid() {
                return Err(refused(format!(
                    "owned by uid {}, not this user",
                    meta.uid()
                )));
            }
            let forbidden = if admit == Admit::Private {
                0o077
            } else {
                0o022
            };
            if meta.mode() & forbidden != 0 || meta.mode() & 0o6000 != 0 {
                return Err(refused(format!(
                    "mode {:o} lets other users {}; run chmod 600 on it",
                    meta.mode() & 0o7777,
                    if admit == Admit::Private {
                        "read it"
                    } else {
                        "write it"
                    }
                )));
            }
        }
    }
    Ok(Some((file, meta)))
}

/// One admitted owned file. Its descriptor does not make in-place writes immutable.
pub(crate) struct OwnedReader {
    file: File,
    path: std::path::PathBuf,
    stamp: FileStamp,
}

impl OwnedReader {
    pub(crate) fn file(&mut self) -> &mut File {
        &mut self.file
    }

    pub(crate) const fn len(&self) -> u64 {
        self.stamp.len
    }

    pub(crate) const fn stamp(&self) -> FileStamp {
        self.stamp
    }

    /// Refuse an observation when the opened file changed while it was being read.
    pub(crate) fn check_unchanged(&self) -> crate::Result<()> {
        self.check_unchanged_classified()
            .map_err(Unread::into_error)
    }

    /// A changed descriptor is an interrupted observation, not a stable refusal. A file that left
    /// its path while it was read (its last link gone: a writer renamed a new file over it, as a
    /// writer replacing the file whole does ([`write_atomic`]), or removed it) still holds the
    /// bytes read, and that unlink is its one change: the system stamps the inode's change time
    /// when its link count drops (measured on APFS, 2026-11-06: `mv` over a file held open moved
    /// its `st_ctime` and left its inode, length and `st_mtime`), so it is taken when every other
    /// field is unchanged. An append, a write in place and a file still linked whose change time
    /// moved (renamed aside, say) are refused.
    pub(crate) fn check_unchanged_classified(&self) -> Result<(), Unread> {
        let metadata = self
            .file
            .metadata()
            .map_err(|e| Unread::Failed(Error::new(format!("{}: {e}", self.path.display()))))?;
        let now = FileStamp::of(&metadata);
        let unlinked = metadata.nlink() == 0
            && FileStamp {
                ctime_ns: self.stamp.ctime_ns,
                ..now
            } == self.stamp;
        if now != self.stamp && !unlinked {
            return Err(Unread::Failed(Error::new(format!(
                "{}: changed while reading membership records",
                self.path.display()
            ))));
        }
        Ok(())
    }
}

/// Open the same descriptor that passes [`read_owned`]'s admission checks.
pub(crate) fn open_owned_reader(path: &Path) -> crate::Result<Option<OwnedReader>> {
    open_owned_reader_classified(path).map_err(Unread::into_error)
}

/// Admit the same owned descriptor while preserving stable refusal versus retryable failure.
pub(crate) fn open_owned_reader_classified(path: &Path) -> Result<Option<OwnedReader>, Unread> {
    Ok(
        open_admitted(path, Admit::Owned, Links::Refuse)?.map(|(file, metadata)| OwnedReader {
            file,
            path: path.to_path_buf(),
            stamp: FileStamp::of(&metadata),
        }),
    )
}

/// A buffer sized once for reading a file of `len` bytes (its `fstat` size) under a cap of
/// `max`. `read_to_end` through a `Take` has no size hint, so into an empty buffer it grows
/// from 32 bytes, freeing each block it outgrew, with what it had read, unwiped. One byte past
/// the file lets it see the end without growing; only a file that grows between the `fstat`
/// and the read still grows it.
pub(crate) fn read_buffer(len: u64, max: usize) -> Vec<u8> {
    let len = usize::try_from(len).unwrap_or(usize::MAX).min(max);
    Vec::with_capacity(len.saturating_add(1))
}

/// Metadata-only check whether `path` is a non-empty regular file [`read_private`] admits
/// (not a symlink, owned by this user, no group/other or setuid/setgid bits, at most `max` bytes).
#[must_use]
pub fn has_private_nonempty(path: &Path, max: usize) -> bool {
    let limit = u64::try_from(max).unwrap_or(u64::MAX);
    std::fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.file_type().is_file()
            && meta.uid() == euid()
            && meta.mode() & 0o6077 == 0
            && meta.len() > 0
            && meta.len() <= limit
    })
}

/// Reads a regular file owned by this user with no group/other bits, up to `max` bytes.
pub fn read_private(path: &Path, max: usize) -> crate::Result<Option<SecretBytes>> {
    Ok(read_admitted(path, max, Admit::Private)?.map(SecretBytes::new))
}

/// Reads a regular file owned by this user with no group/other write bits, up to `max` bytes.
pub fn read_owned(path: &Path, max: usize) -> crate::Result<Option<Vec<u8>>> {
    read_admitted(path, max, Admit::Owned)
}

/// `aterm.toml` at `path`, up to `max` bytes, as every reader of it reads it, the window's
/// settings included: a symlink there (a dotfiles checkout, home-manager) is followed, and the
/// file it ends at must be a regular file owned by this user or by root with no group/other
/// write bits ([`aterm_types::dirs::aterm_config_refusal`]), such as the read-only file
/// home-manager links from its store.
pub fn read_aterm_config(path: &Path, max: usize) -> crate::Result<Option<Vec<u8>>> {
    read_opened(path, max, Admit::AtermConfig, Past::Refuse, Links::Follow)
}

/// [`read_owned`], with the [`FileStamp`] of the file it read, taken from the open file before
/// the read: a file that changes during the read shows a new stamp at the next [`stamp`].
///
/// # Errors
/// [`Unread::Refused`]: the file is one [`read_owned`] refuses as it is. [`Unread::Failed`]: the
/// open, `fstat` or read failed.
pub fn read_owned_stamped(path: &Path, max: usize) -> Result<Option<(FileStamp, Vec<u8>)>, Unread> {
    read_opened_stamped(path, max, Admit::Owned, Past::Refuse, Links::Refuse)
}

/// [`read_owned`] that keeps the first `max` bytes of a longer file instead of refusing it.
pub fn read_owned_head(path: &Path, max: usize) -> crate::Result<Option<Vec<u8>>> {
    read_admitted_past(path, max, Admit::Owned, Past::Head)
}

/// Reads any regular file up to `max` bytes, refusing symlinks and non-regular files.
pub fn read_public(path: &Path, max: usize) -> crate::Result<Option<Vec<u8>>> {
    read_admitted(path, max, Admit::Any)
}

/// Writes `bytes` atomically to `path` with permissions `mode`.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<()> {
    write_atomic_with(path, mode, |writer| writer.write_all(bytes))
}

/// Produce a file atomically with [`write_atomic`]'s parent and durability rules.
/// The producer must flush any buffer it owns before returning success.
///
/// # Errors
/// As [`write_atomic`], or the producer fails before the temporary is committed.
pub fn write_atomic_with(
    path: &Path,
    mode: u32,
    produce: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> crate::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && kind(parent)? != Kind::Dir
    {
        return Err(Error::new(format!(
            "{}: no such directory",
            parent.display()
        )));
    }
    aterm_uds::atomic_file::write_with(path, mode, produce)
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

/// Opens or creates a `0600` regular lock file at `path` (`O_NOFOLLOW | O_NONBLOCK`), verifying
/// ownership and a single hard link, narrowing its permissions to `0600`, and keeping its
/// descriptor out of child processes.
pub(crate) fn open_lock(path: &Path) -> crate::Result<File> {
    let file = aterm_uds::kept_out(|| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
    })
    .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    let metadata = lock_metadata(&file, path)?;
    if metadata.mode() & 0o7777 != 0o600 {
        narrow_lock(&file, path)?;
    }
    Ok(file)
}

/// The metadata of the lock file `file` opened at `path` (`O_NOFOLLOW`, so never through a
/// symlink), once it passed the checks every taker and observer of a vault lock makes: a regular
/// file, owned by this user, with one hard link. Its mode is left to the caller to judge.
///
/// # Errors
/// The file cannot be examined, or it fails one of those checks.
pub(crate) fn lock_metadata(file: &File, path: &Path) -> crate::Result<std::fs::Metadata> {
    let metadata = file
        .metadata()
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    if !metadata.file_type().is_file() {
        return Err(Error::new(format!(
            "{}: not a regular file",
            path.display()
        )));
    }
    if metadata.uid() != euid() {
        return Err(Error::new(format!(
            "{}: owned by uid {}, not this user",
            path.display(),
            metadata.uid()
        )));
    }
    if metadata.nlink() != 1 {
        return Err(Error::new(format!(
            "{}: has {} hard links",
            path.display(),
            metadata.nlink()
        )));
    }
    Ok(metadata)
}

/// Set the lock file `file` opened at `path`, which passed [`lock_metadata`], to `0600` through
/// its descriptor (`fchmod`), so the inode those checks judged is the one changed, never whatever
/// the name points at by then.
///
/// # Errors
/// The `fchmod` fails (a read-only filesystem, say).
pub(crate) fn narrow_lock(file: &File, path: &Path) -> crate::Result<()> {
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

/// What tells one file's contents from another's without reading them: the device and inode
/// of the file at a path, its length, and when its contents and its inode last changed (in
/// nanoseconds). A file renamed over the path, or one written since, has another stamp; only a
/// rewrite in place of the same length within the clock's granularity keeps it, which no
/// writer of these files does (each writes a new file and renames it, or appends). A `chmod`,
/// `chown` or `link` moves its inode-change time alone, which a filesystem whose clock ticks
/// coarsely keeps within one tick: [`Seen`] holds what those change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    /// The device it is on.
    pub dev: u64,
    /// Its inode.
    pub ino: u64,
    /// Its length in bytes.
    pub len: u64,
    /// When its contents last changed.
    pub mtime_ns: i128,
    /// When its inode last changed.
    pub ctime_ns: i128,
}

impl FileStamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        let ns = |secs: i64, nsec: i64| i128::from(secs) * 1_000_000_000 + i128::from(nsec);
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime_ns: ns(meta.mtime(), meta.mtime_nsec()),
            ctime_ns: ns(meta.ctime(), meta.ctime_nsec()),
        }
    }
}

/// The [`FileStamp`] of what is at `path` (`lstat`, so a symlink's own, never its target's), or
/// `None` when nothing is there or it cannot be examined.
pub fn stamp(path: &Path) -> Option<FileStamp> {
    stamp_if_present(path).ok().flatten()
}

/// Read a file's stamp without treating an observation failure as an absent file.
///
/// # Errors
/// The metadata read fails for any reason other than a missing path.
pub(crate) fn stamp_if_present(path: &Path) -> crate::Result<Option<FileStamp>> {
    Ok(kind_and_stamp_if_present(path)?.map(|(_, stamp)| stamp))
}

/// What one `lstat` of a path shows ([`seen_if_present`]): its [`Kind`] and [`FileStamp`], and the
/// owner, mode and link count that a stamp shows only through its inode-change time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub(crate) kind: Kind,
    pub(crate) stamp: FileStamp,
    pub(crate) uid: u32,
    pub(crate) mode: u32,
    pub(crate) nlink: u64,
}

/// [`kind_and_stamp_if_present`], with the owner, mode and link count the same `lstat` found.
///
/// # Errors
/// As [`stamp_if_present`].
pub(crate) fn seen_if_present(path: &Path) -> crate::Result<Option<Seen>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(Seen {
            kind: kind_of(&meta),
            stamp: FileStamp::of(&meta),
            uid: meta.uid(),
            mode: meta.mode(),
            nlink: meta.nlink(),
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::new(format!("{}: {error}", path.display()))),
    }
}

/// [`stamp_if_present`], with the [`Kind`] the same `lstat` found.
///
/// # Errors
/// As [`stamp_if_present`].
pub(crate) fn kind_and_stamp_if_present(path: &Path) -> crate::Result<Option<(Kind, FileStamp)>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some((kind_of(&meta), FileStamp::of(&meta)))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::new(format!("{}: {error}", path.display()))),
    }
}

/// Append `bytes` to the regular file at `path` (never through a symlink) while it is still
/// the file `expected` stamped (its device, inode and length), and make them durable
/// ([`aterm_durable::durable_data`]). Another file there, or one of another length, is left as
/// it is: its writer was not the one that last read or wrote it. A file renamed over the path
/// with the same length is another inode, so the check is of the file's identity, not its
/// length alone.
///
/// # Errors
/// The file is missing, not the one `expected` names, or the open, write or flush fails. A
/// write cut short leaves part of `bytes` on the end of the file; its writer replaces the file
/// whole then.
pub fn append_at(path: &Path, expected: &FileStamp, bytes: &[u8]) -> crate::Result<FileStamp> {
    append_at_with(path, expected, bytes.len(), |writer| {
        writer.write_all(bytes)
    })
}

/// Append exactly `length` produced bytes under [`append_at`]'s file-identity rules.
/// The producer must flush its buffers before returning. An oversized write is refused
/// before any byte of that write reaches the file; a short or failed producer is not durable.
///
/// # Errors
/// As [`append_at`], or production is short, exceeds `length`, or overflows the file length.
pub fn append_at_with(
    path: &Path,
    expected: &FileStamp,
    length: usize,
    produce: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> crate::Result<FileStamp> {
    let added = u64::try_from(length).map_err(|_| Error::new("append length is out of range"))?;
    let _end = expected
        .len
        .checked_add(added)
        .ok_or_else(|| Error::new("append length is out of range"))?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && kind(parent)? != Kind::Dir
    {
        return Err(Error::new(format!(
            "{}: no such directory",
            parent.display()
        )));
    }
    let appended = OpenOptions::new()
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .and_then(|mut file| {
            let metadata = file.metadata()?;
            if !metadata.file_type().is_file()
                || metadata.uid() != euid()
                || metadata.nlink() != 1
                || metadata.mode() & 0o077 != 0
                || metadata.mode() & 0o6000 != 0
                || metadata.dev() != expected.dev
                || metadata.ino() != expected.ino
                || metadata.len() != expected.len
            {
                return Err(io::Error::other("not the file this writer last left"));
            }
            {
                let mut exact = ExactWriter {
                    writer: &mut file,
                    remaining: length,
                };
                produce(&mut exact)?;
                exact.finish()?;
            }
            aterm_durable::durable_data(&file)?;
            file.metadata().map(|after| FileStamp::of(&after))
        });
    appended.map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

struct ExactWriter<'a> {
    writer: &'a mut dyn Write,
    remaining: usize,
}

impl ExactWriter<'_> {
    fn finish(&self) -> io::Result<()> {
        if self.remaining == 0 {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "append producer wrote too few bytes",
            ))
        }
    }
}

impl Write for ExactWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "append producer exceeded its length",
            ));
        }
        let written = self.writer.write(bytes)?;
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// [`write_atomic`], returning the [`FileStamp`] of the file it placed: the one an `lstat` of
/// `path` reads after the rename, when its device and inode are the temporary file's. `None`
/// when they are not (another writer renamed a file over it in between) or the `lstat` fails:
/// its caller then knows no stamp of what `path` holds, and reads it again.
///
/// # Errors
/// As [`write_atomic`]: the temporary file cannot be made, written, flushed or renamed (`path`
/// is as it was), or the directory's flush after the rename failed (`path` holds `bytes`, but
/// its entry may not survive a power loss).
pub fn write_atomic_stamped(
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> crate::Result<Option<FileStamp>> {
    write_atomic_stamped_with(path, mode, |writer| writer.write_all(bytes))
}

/// [`write_atomic_stamped`] with a producer instead of a complete byte buffer.
/// The producer must flush any buffering before it returns.
///
/// # Errors
/// As [`write_atomic_stamped`], or the producer fails before the temporary is committed.
pub fn write_atomic_stamped_with(
    path: &Path,
    mode: u32,
    produce: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> crate::Result<Option<FileStamp>> {
    write_atomic_stamped_after(path, mode, produce, || {})
}

fn write_atomic_stamped_after(
    path: &Path,
    mode: u32,
    produce: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    after: impl FnOnce(),
) -> crate::Result<Option<FileStamp>> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && kind(parent)? != Kind::Dir
    {
        return Err(Error::new(format!(
            "{}: no such directory",
            parent.display()
        )));
    }
    let written = aterm_uds::atomic_file::write_with_metadata(path, mode, produce)
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    after();
    Ok(stamp(path).filter(|placed| placed.dev == written.dev() && placed.ino == written.ino()))
}

#[cfg(test)]
#[path = "fsx_stream_tests.rs"]
mod stream_tests;

/// [`write_atomic`] for one of many files a pass writes: the temporary file's bytes ordered
/// before its rename ([`aterm_durable::ordered`]) and no flush of the directory, which the
/// pass flushes once for all its files, so `path` holds `bytes` or what it held, never a part.
pub fn write_ordered(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<()> {
    let (tmp, mut file) = temp_beside(path, mode)?;
    let placed = file
        .write_all(bytes)
        .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(mode)))
        .and_then(|()| aterm_durable::ordered(&file))
        .and_then(|()| {
            drop(file);
            std::fs::rename(&tmp, path)
        });
    placed.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::new(format!("{}: {e}", path.display()))
    })
}

/// A new temporary file beside `path`, in a directory that is there.
fn temp_beside(path: &Path, mode: u32) -> crate::Result<(std::path::PathBuf, File)> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && kind(parent)? != Kind::Dir
    {
        return Err(Error::new(format!(
            "{}: no such directory",
            parent.display()
        )));
    }
    aterm_uds::atomic_file::create_temp_beside(path, mode)
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

/// Writes `bytes` to `path` at `mode` only if `path` does not exist.
///
/// The directory's flush after the link is best-effort and never reported, unlike
/// [`write_atomic`]'s: once the link has made `path`, an error would tell the caller nothing
/// was placed while its next try answers "taken", a claim no caller owns.
pub fn write_new(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<bool> {
    let placed = write_new_flushed(path, bytes, mode, sync)?;
    if placed {
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        let _never_reported = aterm_durable::Flush::Durable.dir_best_effort(dir);
    }
    Ok(placed)
}

/// [`write_new`] for one of many files a pass writes, as [`write_ordered`]: the bytes ordered
/// before the link, and no flush of the directory.
pub fn write_new_ordered(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<bool> {
    write_new_flushed(path, bytes, mode, aterm_durable::ordered)
}

/// [`write_new`]'s link, the temporary file's bytes finished by `flush` before it.
fn write_new_flushed(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    flush: fn(&File) -> io::Result<()>,
) -> crate::Result<bool> {
    let (tmp, mut file) = temp_beside(path, mode)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(mode)))
        .and_then(|()| flush(&file));
    drop(file);
    let placed = match written {
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
        Ok(()) => match link_then_unlink(&tmp, path) {
            Ok(()) => Ok(true),
            Err(Moved::NotLinked(e)) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(Moved::NotLinked(e)) => Err(Error::new(format!("{}: {e}", path.display()))),
            Err(Moved::NotUnlinked(e)) => Err(Error::new(format!("{}: {e}", tmp.display()))),
        },
    };
    if !matches!(placed, Ok(true)) {
        let _ = std::fs::remove_file(&tmp);
    }
    placed
}

/// What is at a path, without following a final symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Nothing.
    Missing,
    /// A regular file.
    File,
    /// A symlink.
    Symlink,
    /// A directory.
    Dir,
    /// Something else (FIFO, socket, device).
    Other,
}

/// What is at `path` (`lstat`).
pub fn kind(path: &Path) -> crate::Result<Kind> {
    kind_and_id(path).map(|(kind, _)| kind)
}

/// What is at `path` (`lstat`), together with its `(device, inode)` when present.
pub fn kind_and_id(path: &Path) -> crate::Result<(Kind, Option<(u64, u64)>)> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok((kind_of(&meta), Some((meta.dev(), meta.ino())))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((Kind::Missing, None)),
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
    }
}

fn kind_of(meta: &std::fs::Metadata) -> Kind {
    let kind = meta.file_type();
    if kind.is_symlink() {
        Kind::Symlink
    } else if kind.is_dir() {
        Kind::Dir
    } else if kind.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

/// Returns `(device, inode)` of `path` via `lstat`, or `None` if missing.
pub fn file_id(path: &Path) -> Option<(u64, u64)> {
    std::fs::symlink_metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

/// Renames `from` to `to` without replacing an existing `to`.
pub fn rename_noreplace(from: &Path, to: &Path) -> crate::Result<()> {
    link_then_unlink(from, to).map_err(|moved| match moved {
        Moved::NotLinked(e) => Error::new(format!("{} -> {}: {e}", from.display(), to.display())),
        Moved::NotUnlinked(e) => Error::new(format!("{}: {e}", from.display())),
    })
}

/// Why [`link_then_unlink`] did not move a file.
#[derive(Debug)]
enum Moved {
    /// The link failed (`AlreadyExists` when `to` was taken): nothing changed.
    NotLinked(io::Error),
    /// The removal of `from` failed; the link was undone.
    NotUnlinked(io::Error),
}

/// [`rename_noreplace`]'s steps, with the step that failed and its OS error.
fn link_then_unlink(from: &Path, to: &Path) -> Result<(), Moved> {
    std::fs::hard_link(from, to).map_err(Moved::NotLinked)?;
    let Err(e) = std::fs::remove_file(from) else {
        return Ok(());
    };
    let linked = file_id(to);
    if linked.is_some() && file_id(from) == linked {
        let _ = std::fs::remove_file(to);
    } else if e.kind() == io::ErrorKind::NotFound {
        return Ok(());
    }
    Err(Moved::NotUnlinked(e))
}

/// Zeroes and removes a regular file at `path` (one this user cannot read stays), or unlinks a
/// symlink without following it.
pub fn wipe_and_remove(path: &Path) -> crate::Result<()> {
    if kind(path)? == Kind::File {
        return wipe(path, |_, _| Ok(true)).map(drop);
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
    }
}

/// Zeroes and removes the regular file at `path` if its contents (up to `max` bytes) pass `ours`.
pub fn wipe_if(path: &Path, max: usize, ours: impl FnOnce(&[u8]) -> bool) -> crate::Result<bool> {
    wipe(path, |file, meta| {
        let limit = u64::try_from(max).unwrap_or(u64::MAX);
        if meta.len() > limit {
            return Ok(false);
        }
        let mut bytes = read_buffer(meta.len(), max);
        let limit = limit.saturating_add(1);
        if crate::secret::read_to_end_wiping_on_error(file.take(limit), &mut bytes).is_err() {
            return Ok(false);
        }
        let bytes = SecretBytes::new(bytes);
        Ok(bytes.len() <= max && ours(bytes.as_slice()))
    })
}

/// [`wipe_if`]'s steps, with `judge` reading the file it opened.
fn wipe(
    path: &Path,
    judge: impl FnOnce(&mut File, &std::fs::Metadata) -> io::Result<bool>,
) -> crate::Result<bool> {
    let at = |e: io::Error| Error::new(format!("{}: {e}", path.display()));
    let open = |write: bool| {
        OpenOptions::new()
            .read(true)
            .write(write)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
    };
    let opened = match open(true) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => open(false),
        opened => opened,
    };
    let mut file = match opened {
        Ok(file) => file,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) || e.raw_os_error() == Some(libc::ELOOP) =>
        {
            return Ok(false);
        }
        Err(e) => return Err(at(e)),
    };
    let meta = file.metadata().map_err(at)?;
    if !meta.file_type().is_file()
        || !judge(&mut file, &meta).map_err(at)?
        || file_id(path) != Some((meta.dev(), meta.ino()))
    {
        return Ok(false);
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(at(e)),
    }
    let zeros = [0u8; 4096];
    let mut done = 0;
    while done < meta.len() {
        let n = usize::try_from((meta.len() - done).min(4096)).unwrap_or(4096);
        if file.write_all_at(&zeros[..n], done).is_err() {
            break;
        }
        done += n as u64;
    }
    let _ = sync(&file);
    Ok(true)
}

fn sync(file: &File) -> io::Result<()> {
    aterm_durable::durable_data(file)
}

/// This process's effective uid.
pub fn euid() -> u32 {
    #[cfg(test)]
    if let Some(uid) = EUID_SEAM.with(std::cell::Cell::get) {
        return uid;
    }
    aterm_uds::peer::our_uid()
}

/// How many reads each path took since this test process started ([`reads_of`]).
#[cfg(test)]
static READS: std::sync::Mutex<std::collections::BTreeMap<std::path::PathBuf, usize>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Count one read of `path` ([`reads_of`]).
#[cfg(test)]
pub(crate) fn count_read(path: &Path) {
    *READS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(path.to_path_buf())
        .or_default() += 1;
}

/// Reads of each path this test process fails before opening it ([`fail_reads`]).
#[cfg(test)]
static READ_FAILS: std::sync::Mutex<std::collections::BTreeMap<std::path::PathBuf, usize>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Fail the next `times` reads of `path` as an I/O error would ([`Unread::Failed`]), opening
/// nothing.
#[cfg(test)]
pub(crate) fn fail_reads(path: &Path, times: usize) {
    READ_FAILS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_path_buf(), times);
}

/// Whether this read of `path` is one [`fail_reads`] fails, counting it.
#[cfg(test)]
fn take_read_failure(path: &Path) -> bool {
    let mut fails = READ_FAILS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match fails.get_mut(path) {
        Some(left) if *left > 0 => {
            *left -= 1;
            true
        }
        _ => false,
    }
}

/// How many times this test process opened `path` to read it (each tempdir path is one test's).
#[cfg(test)]
pub(crate) fn reads_of(path: &Path) -> usize {
    READS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .copied()
        .unwrap_or(0)
}

/// Every path under `dir` this test process opened to read, with how many times ([`reads_of`]).
#[cfg(test)]
pub(crate) fn reads_under(dir: &Path) -> std::collections::BTreeMap<std::path::PathBuf, usize> {
    READS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|(path, _)| path.starts_with(dir))
        .map(|(path, reads)| (path.clone(), *reads))
        .collect()
}

/// Write `bytes` to `path` owner-only, as aterm writes its own files and a person keeps theirs, so
/// the reads that refuse what group or others may write ([`read_owned`], [`read_aterm_config`])
/// admit it under any umask, Ubuntu's user-private-group 002 among them. A rewrite keeps the
/// file's inode, as an edit in place does; [`write_atomic`] is the replace.
#[cfg(test)]
pub(crate) fn write_owned_in_place(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) {
    let path = path.as_ref();
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(test)]
thread_local! {
    /// The uid [`euid`] answers on this thread while [`as_another_user`] runs.
    static EUID_SEAM: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with [`euid`] answering a uid other than this process's on this thread: every file this
/// user made is then another user's to the checks inside it, as a lock another account left is.
#[cfg(test)]
pub(crate) fn as_another_user<T>(f: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            EUID_SEAM.with(|seam| seam.set(None));
        }
    }
    let other = aterm_uds::peer::our_uid().wrapping_add(1);
    EUID_SEAM.with(|seam| seam.set(Some(other)));
    let _reset = Reset;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn a_stamp_distinguishes_missing_paths_from_failed_observations() {
        let root = scratch();
        let path = root.path().join("file");
        assert_eq!(stamp_if_present(&path).unwrap(), None);
        write_atomic(&path, b"present", 0o600).unwrap();
        assert!(stamp_if_present(&path).unwrap().is_some());
        assert!(stamp_if_present(&path.join("cannot-be-a-child")).is_err());
    }

    /// F23 (rule 5): an append refuses a file renamed over the one its writer last left, though
    /// it has the same length: `append_at` checks the file's device and inode with its length.
    /// NEGATIVE CONTROL: the length-only check of main before F23 appends onto it, and the test
    /// fails "the append went onto a file renamed over with the same length".
    #[test]
    fn an_append_refuses_a_file_renamed_over_with_the_same_length() {
        let root = scratch();
        let path = root.path().join("team.records");
        let left = write_atomic_stamped(&path, b"ours", 0o600)
            .expect("write")
            .expect("its stamp");
        assert_eq!(stamp(&path), Some(left));
        let beside = root.path().join("team.records.new");
        write_atomic(&beside, b"some", 0o600).expect("write");
        std::fs::rename(&beside, &path).expect("renamed over");
        assert_eq!(stamp(&path).map(|now| now.len), Some(left.len));
        assert!(
            append_at(&path, &left, b"tail").is_err(),
            "the append went onto a file renamed over with the same length"
        );
        assert_eq!(std::fs::read(&path).expect("read"), b"some");

        // The file it stamped takes the append, which returns the stamp it leaves.
        let now = stamp(&path).expect("stamped");
        let after = append_at(&path, &now, b"tail").expect("appended");
        assert_eq!(std::fs::read(&path).expect("read"), b"sometail");
        assert_eq!(stamp(&path), Some(after));
        assert_eq!(after.len, 8);
        assert_eq!(
            read_owned_stamped(&path, 64).expect("read"),
            Some((after, b"sometail".to_vec()))
        );
    }

    #[test]
    fn a_private_dir_is_made_and_narrowed_to_0700() {
        let root = scratch();
        let dir = root.path().join("a").join("secrets");
        ensure_private_dir(&dir).expect("made");
        assert_eq!(
            std::fs::metadata(&dir).expect("meta").mode() & 0o7777,
            0o700
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("widen");
        ensure_private_dir(&dir).expect("narrowed");
        assert_eq!(
            std::fs::metadata(&dir).expect("meta").mode() & 0o7777,
            0o700
        );
    }

    #[test]
    fn a_symlinked_dir_is_refused() {
        let root = scratch();
        let real = root.path().join("real");
        std::fs::create_dir(&real).expect("dir");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        let error = ensure_private_dir(&link).expect_err("refused");
        assert!(error.0.contains("symlink"), "{error}");
    }

    #[test]
    fn private_reads_refuse_group_or_other_bits_and_symlinks() {
        let root = scratch();
        let path = root.path().join("token");
        write_atomic(&path, b"value", 0o600).expect("write");
        assert_eq!(
            std::fs::metadata(&path).expect("meta").mode() & 0o777,
            0o600
        );
        assert_eq!(
            read_private(&path, 64)
                .expect("read")
                .expect("present")
                .as_slice(),
            b"value"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");
        let error = read_private(&path, 64).expect_err("refused");
        assert!(!error.0.contains("value"), "{error}");
        assert!(
            read_owned(&path, 64)
                .expect("owned admits group read")
                .is_some()
        );
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&path, &link).expect("link");
        assert!(read_public(&link, 64).is_err());
        assert!(
            read_private(&root.path().join("missing"), 64)
                .expect("ok")
                .is_none()
        );

        let lock_path = root.path().join("state.lock");
        std::fs::write(&lock_path, b"").expect("seed lock");
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o644))
            .expect("widen lock");
        let _lock = open_lock(&lock_path).expect("open_lock");
        assert_eq!(
            std::fs::symlink_metadata(&lock_path).expect("meta").mode() & 0o7777,
            0o600
        );
        let lock_hard = root.path().join("state.lock.hard");
        std::fs::hard_link(&lock_path, &lock_hard).expect("hard link lock");
        assert!(open_lock(&lock_path).is_err());
        std::fs::remove_file(&lock_hard).expect("rm hard link lock");

        let append_file = root.path().join("append.bin");
        write_atomic(&append_file, b"head", 0o600).expect("write append");
        let stamped = |path: &Path| stamp(path).expect("stamped");
        append_at(&append_file, &stamped(&append_file), b"-tail").expect("append private");
        assert_eq!(std::fs::read(&append_file).expect("read"), b"head-tail");
        std::fs::set_permissions(&append_file, std::fs::Permissions::from_mode(0o644))
            .expect("widen append");
        assert!(append_at(&append_file, &stamped(&append_file), b"!").is_err());
        std::fs::set_permissions(&append_file, std::fs::Permissions::from_mode(0o600))
            .expect("restore append");
        let append_hard = root.path().join("append.hard");
        std::fs::hard_link(&append_file, &append_hard).expect("hard link append");
        assert!(append_at(&append_file, &stamped(&append_file), b"!").is_err());
        std::fs::remove_file(&append_hard).expect("rm hard link append");
        let real_dir = root.path().join("real-dir");
        std::fs::create_dir(&real_dir).expect("real dir");
        write_atomic(&real_dir.join("in.bin"), b"abcd", 0o600).expect("seed in");
        let link_dir = root.path().join("link-dir");
        std::os::unix::fs::symlink(&real_dir, &link_dir).expect("symlink dir");
        let real = stamped(&real_dir.join("in.bin"));
        assert!(append_at(&link_dir.join("in.bin"), &real, b"ef").is_err());
    }

    /// A read fills one buffer sized from the file's length, a byte over so it sees the end
    /// without growing: a buffer that grew freed each block it outgrew, unwiped.
    #[test]
    fn a_read_fills_one_buffer_sized_from_the_files_length() {
        let root = scratch();
        let path = root.path().join("value");
        write_atomic(&path, &[7u8; 1000], 0o600).expect("write");
        let bytes = read_owned(&path, 4096).expect("read").expect("there");
        assert_eq!((bytes.len(), bytes.capacity()), (1000, 1001));
    }

    #[test]
    fn reads_are_bounded_and_a_fifo_never_blocks() {
        let root = scratch();
        let path = root.path().join("big");
        write_atomic(&path, &[7u8; 65], 0o600).expect("write");
        assert!(read_private(&path, 64).is_err());
        assert!(read_private(&path, 65).expect("ok").is_some());
        let fifo = root.path().join("fifo");
        let made = aterm_uds::spawnfd::command("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(made.success());
        assert!(
            read_public(&fifo, 64).is_err(),
            "a FIFO is not a regular file"
        );
    }

    #[test]
    fn rename_noreplace_refuses_an_existing_target() {
        let root = scratch();
        let a = root.path().join("a");
        let b = root.path().join("b");
        write_atomic(&a, b"a", 0o600).expect("a");
        write_atomic(&b, b"b", 0o600).expect("b");
        assert!(rename_noreplace(&a, &b).is_err());
        assert_eq!(std::fs::read(&b).expect("b"), b"b");
        std::fs::remove_file(&b).expect("rm");
        rename_noreplace(&a, &b).expect("moved");
        assert_eq!(kind(&a).expect("kind"), Kind::Missing);
        assert_eq!(std::fs::read(&b).expect("b"), b"a");
    }

    #[test]
    fn a_rename_whose_unlink_fails_undoes_its_link() {
        let root = scratch();
        let locked = root.path().join("locked");
        std::fs::create_dir(&locked).expect("dir");
        let from = locked.join("f");
        write_atomic(&from, b"kept", 0o600).expect("write");
        let to = root.path().join("to");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("lock");
        let result = rename_noreplace(&from, &to);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("unlock");
        if euid() == 0 {
            result.expect("root renames");
            assert_eq!(kind(&from).expect("kind"), Kind::Missing);
            return;
        }
        let error = result.expect_err("the unlink is refused");
        assert!(error.0.contains("locked/f"), "{error}");
        assert_eq!(
            kind(&to).expect("kind"),
            Kind::Missing,
            "the link is undone"
        );
        assert_eq!(std::fs::read(&from).expect("from"), b"kept");
        assert_eq!(
            std::fs::symlink_metadata(&from).expect("meta").nlink(),
            1,
            "one name again"
        );
    }

    #[test]
    fn file_ids_tell_one_file_from_two() {
        let root = scratch();
        let a = root.path().join("a");
        write_atomic(&a, b"x", 0o600).expect("a");
        let link = root.path().join("hard");
        std::fs::hard_link(&a, &link).expect("link");
        let other = root.path().join("other");
        write_atomic(&other, b"x", 0o600).expect("other");
        assert!(file_id(&a).is_some());
        assert_eq!(file_id(&a), file_id(&link));
        assert_ne!(file_id(&a), file_id(&other));
        assert_eq!(file_id(&root.path().join("missing")), None);
        let soft = root.path().join("soft");
        std::os::unix::fs::symlink(&a, &soft).expect("symlink");
        assert_ne!(file_id(&soft), file_id(&a));
    }

    #[test]
    fn wipe_removes_files_and_only_the_link_of_a_link() {
        let root = scratch();
        let file = root.path().join("f");
        write_atomic(&file, b"secret", 0o600).expect("write");
        let link = root.path().join("l");
        std::os::unix::fs::symlink(&file, &link).expect("link");
        wipe_and_remove(&link).expect("link removed");
        assert_eq!(std::fs::read(&file).expect("intact"), b"secret");
        wipe_and_remove(&file).expect("wiped");
        assert_eq!(kind(&file).expect("kind"), Kind::Missing);
        wipe_and_remove(&file).expect("missing is fine");
    }

    #[test]
    fn wipe_zeroes_the_bytes_under_every_name() {
        let root = scratch();
        let file = root.path().join("f");
        write_atomic(&file, b"secret", 0o600).expect("write");
        let kept = root.path().join("kept");
        std::fs::hard_link(&file, &kept).expect("link");
        let mut open = File::open(&file).expect("open");
        wipe_and_remove(&file).expect("wiped");
        assert_eq!(kind(&file).expect("kind"), Kind::Missing);
        assert_eq!(std::fs::read(&kept).expect("the other name"), [0u8; 6]);
        let mut left = Vec::new();
        open.read_to_end(&mut left).expect("read");
        assert_eq!(
            left, [0u8; 6],
            "a descriptor open before the wipe reads zeros"
        );
    }

    #[test]
    fn a_judged_wipe_never_touches_a_file_renamed_over_the_one_it_judged() {
        let root = scratch();
        let file = root.path().join("config");
        write_atomic(&file, b"the vault's", 0o600).expect("write");
        assert!(!wipe_if(&file, 64, |bytes| bytes == b"other").expect("judged"));
        assert!(
            !wipe_if(&file, 4, |_| true).expect("judged"),
            "past the bound"
        );
        assert_eq!(std::fs::read(&file).expect("kept"), b"the vault's");

        let tools = root.path().join("config.lock");
        write_atomic(&tools, b"the tool's", 0o600).expect("write");
        let removed = wipe_if(&file, 64, |bytes| {
            std::fs::rename(&tools, &file).expect("renamed over");
            bytes == b"the vault's"
        })
        .expect("judged");
        assert!(!removed);
        assert_eq!(
            std::fs::read(&file).expect("the tool's file"),
            b"the tool's"
        );

        let kept = root.path().join("kept");
        std::fs::hard_link(&file, &kept).expect("link");
        assert!(wipe_if(&file, 64, |bytes| bytes == b"the tool's").expect("wiped"));
        assert_eq!(kind(&file).expect("kind"), Kind::Missing);
        assert_eq!(std::fs::read(&kept).expect("the other name"), [0u8; 10]);
    }

    /// The names in `dir`, sorted.
    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn write_new_fills_only_an_empty_name() {
        let root = scratch();
        let path = root.path().join("gap");
        assert!(write_new(&path, b"filled", 0o600).expect("written"));
        assert_eq!(std::fs::read(&path).expect("read"), b"filled");
        assert_eq!(
            std::fs::symlink_metadata(&path).expect("meta").mode() & 0o7777,
            0o600
        );
        assert_eq!(
            std::fs::symlink_metadata(&path).expect("meta").nlink(),
            1,
            "the temp's name is gone"
        );

        assert!(!write_new(&path, b"again", 0o600).expect("taken"));
        assert_eq!(std::fs::read(&path).expect("read"), b"filled");
        let dangling = root.path().join("dangling");
        let nowhere = root.path().join("nowhere");
        std::os::unix::fs::symlink(&nowhere, &dangling).expect("link");
        assert!(!write_new(&dangling, b"v", 0o600).expect("taken"));
        assert_eq!(kind(&dangling).expect("kind"), Kind::Symlink);
        assert_eq!(kind(&nowhere).expect("kind"), Kind::Missing);
        assert_eq!(names_in(root.path()), ["dangling", "gap"], "no temp left");

        let missing = root.path().join("no-dir").join("x");
        let error = write_new(&missing, b"v", 0o600).expect_err("no parent");
        assert!(error.0.contains("no-dir"), "{error}");
    }
}
