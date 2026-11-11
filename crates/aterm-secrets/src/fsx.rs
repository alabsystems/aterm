// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admitted file reads (`O_NOFOLLOW | O_NONBLOCK`) and atomic 0600 writes.

use crate::{Error, SecretBytes};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

fn check_and_narrow_dir_fd(dir: &Path, dir_fd: &File) -> crate::Result<()> {
    check_access_list(dir_fd, true).map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    let meta = dir_fd
        .metadata()
        .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    if !owned_dir(dir, &meta)? {
        dir_fd
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    }
    Ok(())
}

/// Whether the folder `meta` describes is `0700` (any other mode [`ensure_private_dir`] makes
/// `0700`), refused where it is no folder or another user's, as [`ensure_private_dir`] refuses it.
fn owned_dir(dir: &Path, meta: &std::fs::Metadata) -> crate::Result<bool> {
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
    Ok(meta.mode() & 0o7777 == 0o700)
}

/// Whether [`ensure_private_dir`] would leave the existing `dir` as it is: a folder, no link,
/// this user's, of mode exactly `0700` (its `lstat`).
///
/// # Errors
/// `dir` is missing, a link or no folder, another user's, or of another mode, which
/// [`ensure_private_dir`] would make `0700`.
pub(crate) fn private_dir_settled(dir: &Path) -> crate::Result<()> {
    aterm_uds::private_file::check_private_dir(dir)
        .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    if meta.file_type().is_symlink() {
        return Err(Error::new(format!(
            "{}: is a symlink; refusing to keep secrets through it",
            dir.display()
        )));
    }
    if !owned_dir(dir, &meta)? {
        return Err(Error::new(format!(
            "{}: mode {:o}, which is made 0700",
            dir.display(),
            meta.mode() & 0o7777
        )));
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
    aterm_uds::private_file::ensure_private_dir(dir)
        .map(|_| ())
        .map_err(|e| Error::new(format!("{}: {e}", dir.display())))
}

/// Mode repair cannot remove an extended ACL. Judge the retained descriptor
/// before reads or writes; the caller supplies its own mode and owner policy.
fn check_access_list(file: &File, directory: bool) -> io::Result<()> {
    let verdict = if directory {
        astream_sys::owner_private_dir(file)?
    } else {
        astream_sys::owner_private(file)?
    };
    if matches!(verdict, Err(astream_sys::NotPrivate::Acl)) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "an access list (ACL) grants access beyond the file mode",
        ));
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
    let bytes = read_bounded(&file, meta.len(), max, past, path)?;
    Ok(Some((FileStamp::of(&meta), bytes)))
}

/// Read the opened `file` (whose `fstat` gave `len`), at most `max` bytes, doing `past` with a
/// longer one; `path` names it in an error.
fn read_bounded(
    mut file: &File,
    len: u64,
    max: usize,
    past: Past,
    path: &Path,
) -> Result<Vec<u8>, Unread> {
    let refused = |why: String| Unread::Refused(Error::new(format!("{}: {why}", path.display())));
    let failed = |e: io::Error| Unread::Failed(Error::new(format!("{}: {e}", path.display())));
    let limit = u64::try_from(max).unwrap_or(u64::MAX);
    if past == Past::Refuse && len > limit {
        return Err(refused(format!("larger than {max} bytes")));
    }
    let mut bytes = read_buffer(len, max);
    let limit = if past == Past::Head {
        limit
    } else {
        limit.saturating_add(1)
    };
    crate::secret::read_to_end_wiping_on_error((&mut file).take(limit), &mut bytes)
        .map_err(failed)?;
    if bytes.len() > max {
        zeroize::Zeroize::zeroize(&mut bytes);
        return Err(refused(format!("larger than {max} bytes")));
    }
    Ok(bytes)
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
            check_access_list(&file, false).map_err(|e| refused(e.to_string()))?;
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

    /// The opened file's bytes, at most `max`, as [`read_private`] reads them: a longer one is
    /// refused.
    pub(crate) fn read_private(&self, max: usize) -> crate::Result<SecretBytes> {
        read_bounded(&self.file, self.stamp.len, max, Past::Refuse, &self.path)
            .map(SecretBytes::new)
            .map_err(Unread::into_error)
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
    open_reader(path, Admit::Owned)
}

/// Open the same descriptor that passes [`read_private`]'s admission checks, stamped before a
/// byte is read ([`OwnedReader::read_private`]): a reader that holds the bytes of a file of
/// that stamp need not read them again.
pub(crate) fn open_private_reader(path: &Path) -> crate::Result<Option<OwnedReader>> {
    open_reader(path, Admit::Private).map_err(Unread::into_error)
}

fn open_reader(path: &Path, admit: Admit) -> Result<Option<OwnedReader>, Unread> {
    Ok(
        open_admitted(path, admit, Links::Refuse)?.map(|(file, metadata)| OwnedReader {
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
    check_access_list(file, false).map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    let metadata = file
        .metadata()
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    judge_lock(metadata, path)
}

/// [`lock_metadata`] of `metadata`, which one `fstat` of the lock file at `path` found.
///
/// # Errors
/// It fails one of [`lock_metadata`]'s checks.
pub(crate) fn judge_lock(
    metadata: std::fs::Metadata,
    path: &Path,
) -> crate::Result<std::fs::Metadata> {
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
    seen_if_present_in(None, path)
}

/// [`seen_if_present`] from `glance`'s `lstat` of `path` where the caller looks through one
/// ([`Glance::symlink_metadata`]), and from a fresh one where it does not.
///
/// # Errors
/// As [`stamp_if_present`].
pub(crate) fn seen_if_present_in(
    glance: Option<&Glance>,
    path: &Path,
) -> crate::Result<Option<Seen>> {
    match lstat_in(glance, path) {
        Ok(Some(meta)) => Ok(Some(Seen {
            kind: kind_of(&meta),
            stamp: FileStamp::of(&meta),
            uid: meta.uid(),
            mode: meta.mode(),
            nlink: meta.nlink(),
        })),
        Ok(None) => Ok(None),
        Err(error) => Err(Error::new(format!("{}: {error}", path.display()))),
    }
}

/// The `lstat` of `path`, `None` where nothing is there: `glance`'s where the caller looks
/// through one ([`Glance::symlink_metadata`]), a fresh one where it does not.
///
/// # Errors
/// The `lstat`'s, other than a missing path.
pub(crate) fn lstat_in(
    glance: Option<&Glance>,
    path: &Path,
) -> io::Result<Option<std::fs::Metadata>> {
    if let Some(glance) = glance {
        return glance.symlink_metadata(path);
    }
    #[cfg(test)]
    count_look(path);
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The `lstat`s one check of a vault takes, or the checks one look at it serves, each path's once.
/// A Commons check asks several reads
/// of the same files under one shared observation of the vault's state lock (a table session's
/// access: this Mac's own denial, the membership log and the state behind it; a roster access's
/// currency: the identity, the log and the state), and each stamped them again: a quiet repair
/// round over a long log took about 8,600 `lstat`s, both peers, of a few dozen files, a check's
/// own stamps naming many of its files two or three times (2026-11-08,
/// `docs/measured/commons-long-log-rounds-2026-11-07.md`). A check that looks through one glance
/// stamps each file once, at its first ask of it, and a kept answer stands for the files as that
/// look saw them; a miss reads and stamps afresh. A file that moved meanwhile moves the next
/// glance's stamps, which read it again. Each check takes its own, or shares the one of a look at
/// the vault it is judged through ([`crate::client::VaultLook`]: checks that stand for one moment
/// of the vault, a table session's open and the checks its local batch makes at once after it, and
/// the checks a read batch whose session keeps the look makes through it);
/// none outlives its check or its look, and a look that confirms a file stood still across a read
/// is never taken from one. A digest kept of what a glance saw is kept only where the file had
/// settled when the glance began ([`digest_while_seen`]): a glance's `lstat` can be older than the
/// ask it answers.
pub(crate) struct Glance {
    /// When this look began, before any `lstat` it took ([`asked_now`]).
    began: std::time::SystemTime,
    seen: std::cell::RefCell<Vec<(std::path::PathBuf, Option<std::fs::Metadata>)>>,
}

impl Glance {
    /// A look that has taken nothing yet, begun now.
    pub(crate) fn new() -> Self {
        Self {
            began: asked_now(),
            seen: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The `lstat` of `path` this look took first, `None` where nothing was there; taken now
    /// where it took none. An error is not kept: the next ask looks again.
    ///
    /// # Errors
    /// The `lstat`'s, other than a missing path.
    pub(crate) fn symlink_metadata(&self, path: &Path) -> io::Result<Option<std::fs::Metadata>> {
        let bytes = path.as_os_str().as_encoded_bytes();
        // The paths one check names share their folder: they differ at their ends.
        let same = |at: &std::path::PathBuf| {
            let at = at.as_os_str().as_encoded_bytes();
            at.len() == bytes.len() && at.iter().rev().eq(bytes.iter().rev())
        };
        if let Some((_, meta)) = self.seen.borrow().iter().find(|(at, _)| same(at)) {
            return Ok(meta.clone());
        }
        let meta = lstat_in(None, path)?;
        self.seen
            .borrow_mut()
            .push((path.to_path_buf(), meta.clone()));
        Ok(meta)
    }
}

/// The SHA-256 of the file at `path` as [`read_private`] reads it, `None` where there is none,
/// kept while the file stands ([`digest_while_seen`]), its first `lstat` of `path` `glance`'s
/// where the caller's check looks through one ([`seen_if_present_in`]).
///
/// # Errors
/// As [`read_private`], or the `lstat` of `path`.
pub(crate) fn private_digest(
    path: &Path,
    max: usize,
    glance: Option<&Glance>,
) -> crate::Result<Option<[u8; 32]>> {
    digest_while_seen(
        path,
        Reader {
            admit: Admit::Private,
            max,
        },
        glance,
    )
}

/// [`private_digest`] as [`read_public`] reads the file.
///
/// # Errors
/// As [`read_public`], or the `lstat` of `path`.
pub(crate) fn public_digest(
    path: &Path,
    max: usize,
    glance: Option<&Glance>,
) -> crate::Result<Option<[u8; 32]>> {
    digest_while_seen(
        path,
        Reader {
            admit: Admit::Any,
            max,
        },
        glance,
    )
}

/// A reader whose digests [`digest_while_seen`] keeps: its admission and its cap. A digest one
/// reader kept answers only that reader, since another may refuse the file it admitted.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Reader {
    admit: Admit,
    max: usize,
}

impl Reader {
    /// The [`FileStamp`] of the file this reader opens at `path`, taken before it reads a byte,
    /// and the SHA-256 of the bytes it reads, wiped once hashed; `None` where there is no file.
    fn digest(self, path: &Path) -> crate::Result<Option<(FileStamp, [u8; 32])>> {
        Ok(
            read_opened_stamped(path, self.max, self.admit, Past::Refuse, Links::Refuse)
                .map_err(Unread::into_error)?
                .map(|(stamp, bytes)| {
                    let bytes = SecretBytes::new(bytes);
                    (stamp, aterm_digest::Sha256::digest(bytes.as_slice()))
                }),
        )
    }
}

/// The digest `reader` gives of the file at `path`, kept for that reader while one `lstat` of
/// `path` ([`Seen`]) shows what it showed before the read that gave it, when the file the read
/// opened was the one that `lstat` saw and had stood [`SETTLED_NS`] unchanged when that `lstat`
/// was taken: at the ask, or, where the caller's check looks through a glance, when the glance
/// began, before it took any `lstat` ([`Glance`]), which may be well before the ask.
///
/// The opened file's `fstat` ([`FileStamp`]) shows the `lstat`'s device, inode, length and change
/// times, so its owner, mode and link count are the ones seen: a `chmod`, `chown` or link moves
/// the change time past a settled one. A file swapped in for the read, at the path or through a
/// parent directory renamed aside and back (which moves none of the times of the file it stood in
/// for: measured on APFS, 2026-11-08), has another stamp, and its digest is not kept. Each later
/// change to the file seen shows in its [`Seen`]: a write, `chmod`, link or rename moves its
/// change time past the settled one, and another file at the path has another inode or a later
/// change time. A reader's admission judges the opened file by its type, owner and mode, and its
/// bytes by their length, so an unchanged witness stands for the same verdict and the same bytes.
///
/// A quiet repair round's custody and log-state observations opened, read and hashed the same
/// unchanged files at each of their checks (2026-11-08). A file that changed too recently, is
/// absent, or that the reader refused is read again at the next ask. Only digests are kept,
/// never a file's bytes.
fn digest_while_seen(
    path: &Path,
    reader: Reader,
    glance: Option<&Glance>,
) -> crate::Result<Option<[u8; 32]>> {
    let now = asked_now();
    #[cfg(not(test))]
    {
        static DIGESTS: KeptDigests = std::sync::Mutex::new(std::collections::VecDeque::new());
        digest_while_seen_in(&DIGESTS, glance, now, path, reader, |path| {
            reader.digest(path)
        })
    }
    #[cfg(test)]
    THREAD_DIGESTS.with(|kept| {
        digest_while_seen_in(kept, glance, now, path, reader, |path| reader.digest(path))
    })
}

/// The time a kept digest's settled rule judges an ask, or the start of a glance, by: the clock,
/// and in a test as much later as this thread's [`as_if_later`] puts it.
fn asked_now() -> std::time::SystemTime {
    let now = std::time::SystemTime::now();
    #[cfg(test)]
    let now = now + ASKED_LATER.get();
    now
}

#[cfg(test)]
std::thread_local! {
    /// A test thread's own [`digest_while_seen`] digests, so one test's asks never let go of
    /// another's.
    static THREAD_DIGESTS: KeptDigests =
        const { std::sync::Mutex::new(std::collections::VecDeque::new()) };
    /// How much later than the clock this thread's asks of [`digest_while_seen`], and the glances
    /// it begins, are ([`as_if_later`]).
    static ASKED_LATER: std::cell::Cell<std::time::Duration> =
        const { std::cell::Cell::new(std::time::Duration::ZERO) };
}

/// Run `f` with each digest this thread asks ([`digest_while_seen`]), and each glance it begins
/// ([`Glance::new`]), asked `by` later than the clock: a test's files have stood settled
/// ([`SETTLED_NS`]) without its waiting.
#[cfg(test)]
pub(crate) fn as_if_later<T>(by: std::time::Duration, f: impl FnOnce() -> T) -> T {
    let was = ASKED_LATER.replace(by);
    let out = f();
    ASKED_LATER.set(was);
    out
}

/// The digests [`digest_while_seen`] keeps, each with the path, the reader and the [`Seen`] it
/// stands for.
type KeptDigests =
    std::sync::Mutex<std::collections::VecDeque<(std::path::PathBuf, Reader, Seen, [u8; 32])>>;

/// How long before a digest's witness was taken (its ask's `lstat`, or the start of the glance
/// that took it) its file must have last changed for the digest to be kept. A change made after
/// the witness then takes a later time on a clock that ticks this coarsely or finer
/// (FAT's two seconds), so the file's [`Seen`] shows it, as git's index treats a file it stamped
/// within its own tick as racily clean. Without it, a rewrite in place at the same length, or a
/// new file given a freed inode, in one tick of a coarse clock would keep the stamp of the file
/// it replaced.
const SETTLED_NS: i128 = SETTLED.as_nanos() as i128;

/// `SETTLED_NS` as a time to wait: a measurement of quiet rounds waits it out, and a tenth of a
/// second past it, after its fixture's last write (`client::test_settled_after`).
pub(crate) const SETTLED: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether what `seen` shows last changed [`SETTLED_NS`] or more before `now`, so that a change
/// made after `now` moves a stamp a later `lstat` shows, on a clock that ticks this coarsely or
/// finer. Only then may a reader keep what it read while the path's [`Seen`] stands.
pub(crate) fn settled(seen: &Seen, now: std::time::SystemTime) -> bool {
    now.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| i128::try_from(since.as_nanos()).ok())
        .is_some_and(|now| seen.stamp.mtime_ns.max(seen.stamp.ctime_ns) <= now - SETTLED_NS)
}

/// [`digest_while_seen`] with its digests in `kept`, asked at `now`, reading with `read` what
/// `reader` reads: a fresh witness is judged settled as of `now`, taken before its `lstat`, and
/// one `glance` took as of when the glance began.
fn digest_while_seen_in(
    kept: &KeptDigests,
    glance: Option<&Glance>,
    now: std::time::SystemTime,
    path: &Path,
    reader: Reader,
    read: impl FnOnce(&Path) -> crate::Result<Option<(FileStamp, [u8; 32])>>,
) -> crate::Result<Option<[u8; 32]>> {
    /// The files whose digests are kept, the oldest let go first.
    const KEPT: usize = 32;
    // A glance's look stands before the read: what is kept is kept only where the file the read
    // opened is the one it saw.
    let before = seen_if_present_in(glance, path)?;
    let witnessed = glance.map_or(now, |glance| glance.began.min(now));
    if let Some(seen) = before
        && let Ok(kept) = kept.lock()
        && let Some((.., digest)) = kept
            .iter()
            .find(|(at, by, was, _)| *was == seen && *by == reader && at == path)
    {
        return Ok(Some(*digest));
    }
    let Some((opened, digest)) = read(path)? else {
        return Ok(None);
    };
    if let Some(seen) = before
        && opened == seen.stamp
        && settled(&seen, witnessed)
        && let Ok(mut kept) = kept.lock()
    {
        kept.retain(|(at, by, ..)| !(at == path && *by == reader));
        if kept.len() >= KEPT {
            kept.pop_front();
        }
        kept.push_back((path.to_path_buf(), reader, seen, digest));
    }
    Ok(Some(digest))
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
            check_access_list(&file, false)?;
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

#[cfg(all(test, target_os = "macos"))]
#[path = "fsx_acl_tests.rs"]
mod acl_tests;

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

/// How many `lstat`s [`lstat_in`] took of each path since this test process started, a
/// [`Glance`]'s included ([`looks_under`]).
#[cfg(test)]
static LOOKS: std::sync::Mutex<std::collections::BTreeMap<std::path::PathBuf, usize>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Count one `lstat` of `path` ([`looks_under`]).
#[cfg(test)]
fn count_look(path: &Path) {
    *LOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(path.to_path_buf())
        .or_default() += 1;
}

/// Every path under `dir` [`lstat_in`] took an `lstat` of in this test process, with how many
/// times (each tempdir path is one test's).
#[cfg(test)]
pub(crate) fn looks_under(dir: &Path) -> std::collections::BTreeMap<std::path::PathBuf, usize> {
    LOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|(path, _)| path.starts_with(dir))
        .map(|(path, looks)| (path.clone(), *looks))
        .collect()
}

/// Reads of each path this test process lets through and then fails before opening it
/// ([`fail_reads_after`]).
#[cfg(test)]
static READ_FAILS: std::sync::Mutex<
    std::collections::BTreeMap<std::path::PathBuf, (usize, usize)>,
> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Fail the next `times` reads of `path` as an I/O error would ([`Unread::Failed`]), opening
/// nothing.
#[cfg(test)]
pub(crate) fn fail_reads(path: &Path, times: usize) {
    fail_reads_after(path, 0, times);
}

/// [`fail_reads`] once `skip` more reads of `path` went through: a fault at one read among those
/// a caller makes of the same file.
#[cfg(test)]
pub(crate) fn fail_reads_after(path: &Path, skip: usize, times: usize) {
    READ_FAILS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_path_buf(), (skip, times));
}

/// Whether this read of `path` is one [`fail_reads_after`] fails, counting it.
#[cfg(test)]
fn take_read_failure(path: &Path) -> bool {
    let mut fails = READ_FAILS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match fails.get_mut(path) {
        Some((skip, _)) if *skip > 0 => {
            *skip -= 1;
            false
        }
        Some((_, left)) if *left > 0 => {
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

    /// A glance gives each path the `lstat` it took of it first ([`Glance`]): a file rewritten,
    /// removed or made since stands as the glance saw it, while a new glance sees each as it is;
    /// each path has its own, and an error is not kept, so the next ask looks again.
    /// NEGATIVE CONTROLS (2026-11-08): a glance that looks afresh at each ask sees the rewrite
    /// (`a rewrite stands as first seen`); one that tells paths apart by their length alone gives
    /// the second file the first one's `lstat` (`each path its own`); one that keeps a failed look
    /// as an absence answers the file made at that path since with none (`an error is looked at
    /// again`).
    #[test]
    fn a_glance_gives_each_path_the_lstat_it_took_first() {
        let root = scratch();
        let (a, b, made) = (
            root.path().join("a"),
            root.path().join("b"),
            root.path().join("c"),
        );
        write_atomic(&a, b"one", 0o600).unwrap();
        write_atomic(&b, b"second", 0o600).unwrap();
        let len = |meta: Option<std::fs::Metadata>| meta.map(|meta| meta.len());
        let glance = Glance::new();
        assert_eq!(len(glance.symlink_metadata(&a).unwrap()), Some(3));
        assert_eq!(
            len(glance.symlink_metadata(&b).unwrap()),
            Some(6),
            "each path its own"
        );
        assert_eq!(len(glance.symlink_metadata(&made).unwrap()), None);

        write_atomic(&a, b"rewritten", 0o600).unwrap();
        std::fs::remove_file(&b).unwrap();
        write_atomic(&made, b"made", 0o600).unwrap();
        assert_eq!(
            len(glance.symlink_metadata(&a).unwrap()),
            Some(3),
            "a rewrite stands as first seen"
        );
        assert_eq!(
            len(glance.symlink_metadata(&b).unwrap()),
            Some(6),
            "a removal stands as first seen"
        );
        assert_eq!(
            len(glance.symlink_metadata(&made).unwrap()),
            None,
            "a file made since stands absent"
        );
        let now = Glance::new();
        assert_eq!(len(now.symlink_metadata(&a).unwrap()), Some(9));
        assert_eq!(len(now.symlink_metadata(&b).unwrap()), None);
        assert_eq!(len(now.symlink_metadata(&made).unwrap()), Some(4));

        let child = a.join("child");
        assert!(
            glance.symlink_metadata(&child).is_err(),
            "a file has no child"
        );
        std::fs::remove_file(&a).unwrap();
        std::fs::create_dir(&a).unwrap();
        write_atomic(&child, b"child", 0o600).unwrap();
        assert_eq!(
            len(glance.symlink_metadata(&child).unwrap()),
            Some(5),
            "an error is looked at again"
        );
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

    /// A digest of what a glance saw is kept only where its file had settled ([`SETTLED_NS`]) when
    /// the glance began, not merely by the ask: a glance's `lstat` can be older than the ask, and a
    /// change between them in one tick of a coarse clock would leave its witness standing. A file
    /// a glance began to look at a second after its last change is read again at each ask through
    /// it, though the asks come two hours after the change; one a glance begun an hour after the
    /// change saw is read once. Each time is taken from the file's own stamp, so neither depends
    /// on how soon after the write the law runs.
    /// NEGATIVE CONTROL (2026-11-10): judged settled as of the ask, the file a glance began to
    /// look at a second after its change is read once (`a digest of what a glance saw before its
    /// file settled`).
    #[test]
    fn a_glance_fed_digest_is_kept_only_where_its_file_had_settled_when_the_glance_began() {
        const PRIVATE: Reader = Reader {
            admit: Admit::Private,
            max: 64,
        };
        let root = scratch();
        let path = root.path().join("custody");
        write_atomic(&path, b"custody", 0o600).unwrap();
        let seen = seen_if_present(&path).unwrap().unwrap();
        let changed = std::time::UNIX_EPOCH
            + std::time::Duration::from_nanos(
                u64::try_from(seen.stamp.mtime_ns.max(seen.stamp.ctime_ns)).unwrap(),
            );
        let after_it = |secs| changed + std::time::Duration::from_secs(secs);
        let begun_after_it = |secs| Glance {
            began: after_it(secs),
            seen: std::cell::RefCell::new(Vec::new()),
        };
        let reads_through = |glance: &Glance| {
            let kept = std::sync::Mutex::new(std::collections::VecDeque::new());
            let reads = std::cell::Cell::new(0);
            for _ in 0..2 {
                digest_while_seen_in(
                    &kept,
                    Some(glance),
                    after_it(7200),
                    &path,
                    PRIVATE,
                    |path| {
                        reads.set(reads.get() + 1);
                        PRIVATE.digest(path)
                    },
                )
                .unwrap()
                .unwrap();
            }
            reads.get()
        };
        assert_eq!(
            reads_through(&begun_after_it(1)),
            2,
            "a digest of what a glance saw before its file settled"
        );
        assert_eq!(
            reads_through(&begun_after_it(3600)),
            1,
            "CONTROL: a digest of what a glance begun after its file settled saw"
        );
    }

    /// A digest is kept while its file stands ([`digest_while_seen`]): a settled file is read once
    /// however often its digest is asked, and each change a reader would see is read again (its
    /// bytes rewritten in place at the same length, the file opened to others, another file renamed
    /// over it at the same length, the file removed), as is a file swapped in for the read that
    /// gave the digest, at its path or through its parent directory, and a file changed within
    /// [`SETTLED_NS`] of the ask. A digest one reader kept never answers a reader that refuses the
    /// file.
    /// NEGATIVE CONTROLS (2026-11-08): a digest kept by its path alone, and one kept by the file's
    /// device, inode and length, each answer the rewrite in place with the digest they kept; one
    /// kept without the opened file's stamp answers the parent's swap with the swapped-in file's;
    /// one kept for any reader answers the stricter reader; one kept without the settled rule
    /// reads a file changed a second before the ask once. A digest kept without its mode answers
    /// no change here wrongly, since APFS moves a file's change time at each `chmod`, which
    /// [`Seen`] holds for a coarser clock.
    #[test]
    fn a_digest_is_kept_while_its_file_stands_and_read_again_after_each_change() {
        use std::os::unix::fs::PermissionsExt as _;
        /// A change to a file whose digest is kept, and what makes it.
        struct Change {
            what: &'static str,
            make: fn(&Path),
        }
        const PRIVATE: Reader = Reader {
            admit: Admit::Private,
            max: 64,
        };
        fn read(path: &Path) -> crate::Result<Option<(FileStamp, [u8; 32])>> {
            PRIVATE.digest(path)
        }
        /// What a fresh read by `reader` answers: its digest, `None` where there is no file, or
        /// `None` for its refusal.
        fn fresh(reader: Reader, path: &Path) -> Option<Option<[u8; 32]>> {
            reader
                .digest(path)
                .ok()
                .map(|read| read.map(|(_, digest)| digest))
        }
        fn store() -> KeptDigests {
            std::sync::Mutex::new(std::collections::VecDeque::new())
        }
        /// When the file at `path` last changed, `secs` seconds on.
        fn after_its_change(path: &Path, secs: u64) -> std::time::SystemTime {
            let seen = seen_if_present(path).unwrap().unwrap();
            let changed = seen.stamp.mtime_ns.max(seen.stamp.ctime_ns);
            std::time::UNIX_EPOCH
                + std::time::Duration::from_nanos(u64::try_from(changed).unwrap())
                + std::time::Duration::from_secs(secs)
        }
        let sha = aterm_digest::Sha256::digest;
        // The file stood an hour unchanged at each ask but the settled rule's.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        let changes = [
            Change {
                what: "its bytes rewritten in place at the same length",
                make: |path| {
                    let file = File::options().write(true).open(path).unwrap();
                    file.write_all_at(b"CUSTODY", 0).unwrap();
                    // Its mtime moved away too, so the case holds where a coarse clock gives the
                    // rewrite the time the file had, which the settled rule answers at a real ask.
                    file.set_modified(std::time::UNIX_EPOCH).unwrap();
                },
            },
            Change {
                what: "the file opened to others",
                make: |path| {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
                },
            },
            Change {
                what: "another file renamed over it at the same length",
                make: |path| {
                    let beside = path.with_extension("new");
                    write_atomic(&beside, b"CUSTODY", 0o600).unwrap();
                    std::fs::rename(&beside, path).unwrap();
                },
            },
            Change {
                what: "the file removed",
                make: |path| std::fs::remove_file(path).unwrap(),
            },
        ];
        for Change { what, make } in changes {
            let root = scratch();
            let path = root.path().join("custody");
            write_atomic(&path, b"custody", 0o600).unwrap();
            let kept = store();
            let first = digest_while_seen_in(&kept, None, later, &path, PRIVATE, read).unwrap();
            assert_eq!(first, Some(sha(b"custody")), "{what}");
            let reads = reads_of(&path);
            assert_eq!(
                digest_while_seen_in(&kept, None, later, &path, PRIVATE, read).unwrap(),
                first
            );
            assert_eq!(reads_of(&path), reads, "{what}: read again while it stood");
            make(&path);
            let again = digest_while_seen_in(&kept, None, later, &path, PRIVATE, read).ok();
            assert_eq!(again, fresh(PRIVATE, &path), "{what}");
            assert_ne!(again, Some(first), "{what}: answered with the kept digest");
        }

        // Another file swapped in for the read, at the path or through its parent directory,
        // gives that read its digest, which is not kept: the next ask reads the file put back.
        let root = scratch();
        let dir = root.path().join("vault");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("custody");
        write_atomic(&path, b"custody", 0o600).unwrap();
        /// A swap of another file in for the read: it puts the other file there, reads it and
        /// puts the file back.
        type Swap = fn(&Path) -> crate::Result<Option<(FileStamp, [u8; 32])>>;
        let swaps: [(&str, Swap); 2] = [
            ("its path", |path| {
                let aside = path.with_extension("aside");
                std::fs::rename(path, &aside).unwrap();
                write_atomic(path, b"CUSTODY", 0o600).unwrap();
                let swapped_in = read(path);
                std::fs::rename(&aside, path).unwrap();
                swapped_in
            }),
            ("its parent directory", |path| {
                let dir = path.parent().unwrap();
                let aside = dir.with_extension("aside");
                std::fs::rename(dir, &aside).unwrap();
                std::fs::create_dir(dir).unwrap();
                write_atomic(path, b"CUSTODY", 0o600).unwrap();
                let swapped_in = read(path);
                std::fs::remove_file(path).unwrap();
                std::fs::remove_dir(dir).unwrap();
                std::fs::rename(&aside, dir).unwrap();
                swapped_in
            }),
        ];
        for (through, swap) in swaps {
            let kept = store();
            let seen = seen_if_present(&path).unwrap();
            let swapped = digest_while_seen_in(&kept, None, later, &path, PRIVATE, swap);
            assert_eq!(swapped.unwrap(), Some(sha(b"CUSTODY")), "through {through}");
            if through == "its parent directory" {
                assert_eq!(
                    seen_if_present(&path).unwrap(),
                    seen,
                    "the parent's swap left the file's witness as it was"
                );
            }
            assert_eq!(
                digest_while_seen_in(&kept, None, later, &path, PRIVATE, read).unwrap(),
                Some(sha(b"custody")),
                "the file put back through {through}"
            );
        }

        // A digest one reader kept answers no reader that refuses the file.
        let stricter = [
            (
                "a reader refusing its mode",
                Reader {
                    admit: Admit::Any,
                    max: 64,
                },
                PRIVATE,
                0o640,
            ),
            (
                "a reader with a smaller cap",
                PRIVATE,
                Reader {
                    admit: Admit::Private,
                    max: 4,
                },
                0o600,
            ),
        ];
        for (what, laxer, stricter, mode) in stricter {
            let root = scratch();
            let path = root.path().join("custody");
            write_atomic(&path, b"custody", mode).unwrap();
            let kept = store();
            let admitted =
                digest_while_seen_in(&kept, None, later, &path, laxer, |path| laxer.digest(path));
            assert_eq!(admitted.unwrap(), Some(sha(b"custody")), "{what}");
            let refused = digest_while_seen_in(&kept, None, later, &path, stricter, |path| {
                stricter.digest(path)
            });
            assert!(refused.is_err(), "{what}: {refused:?}");
        }

        // A file that changed a second before the ask is read at each ask, and one that changed
        // three seconds before it once.
        let root = scratch();
        let path = root.path().join("custody");
        write_atomic(&path, b"changed", 0o600).unwrap();
        for (secs, read_at) in [(1, 2), (3, 1)] {
            let kept = store();
            let at = after_its_change(&path, secs);
            let reads = reads_of(&path);
            for _ in 0..2 {
                let changed = digest_while_seen_in(&kept, None, at, &path, PRIVATE, read).unwrap();
                assert_eq!(changed, Some(sha(b"changed")));
            }
            assert_eq!(
                reads_of(&path) - reads,
                read_at,
                "a file changed {secs} s before the ask"
            );
        }
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
