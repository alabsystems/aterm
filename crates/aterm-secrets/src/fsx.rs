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
    look_afresh();
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

/// [`write_atomic`], unless `path` already holds exactly `bytes`: a regular file of this user's
/// with one link, permissions exactly `mode`, no access list that widens it, that length and
/// those bytes, which is then left as it is (`false`): no temporary file, no rename, and the
/// same inode and times, so a reader that keeps what it read by the file's [`FileStamp`] keeps
/// it. Anything else there (nothing, other bytes, another mode, a symlink, a file that cannot be
/// read) is replaced as [`write_atomic`] replaces it (`true`). For text a caller derives again
/// from state that often has not changed.
///
/// A file left as it is has the durability its last writer gave it, and nothing is flushed
/// again: where that writer's flush of the directory failed, it was told so, and a caller that
/// then writes the same bytes with this function does not repeat that flush.
///
/// # Errors
/// As [`write_atomic`], where it writes.
pub fn write_atomic_if_changed(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<bool> {
    if holds_exactly(path, bytes, mode) {
        return Ok(false);
    }
    write_atomic(path, bytes, mode)?;
    Ok(true)
}

/// Whether the file at `path` is one [`write_atomic`] of `bytes` under `mode` would leave no
/// different ([`write_atomic_if_changed`]). Any doubt (it cannot be opened, examined or read
/// through) is `false`. The opened file is examined and read, so a file renamed over the path
/// meanwhile is not the one judged, and the bytes read are wiped.
fn holds_exactly(path: &Path, bytes: &[u8], mode: u32) -> bool {
    let Ok(mut file) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    if !meta.file_type().is_file()
        || meta.uid() != euid()
        || meta.nlink() != 1
        || meta.mode() & 0o7777 != mode & 0o7777
        || u64::try_from(bytes.len()).ok() != Some(meta.len())
        || check_access_list(&file, false).is_err()
    {
        return false;
    }
    let mut held = zeroize::Zeroizing::new([0u8; 64 * 1024]);
    let mut rest = bytes;
    loop {
        match file.read(&mut held[..]) {
            Ok(0) => return rest.is_empty(),
            Ok(n) if n <= rest.len() && held[..n] == rest[..n] => rest = &rest[n..],
            Ok(_) => return false,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
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
        Ok(Some(meta)) => Ok(Some(seen_of(&meta))),
        Ok(None) => Ok(None),
        Err(error) => Err(Error::new(format!("{}: {error}", path.display()))),
    }
}

/// What the `lstat` `meta` shows ([`Seen`]).
fn seen_of(meta: &std::fs::Metadata) -> Seen {
    Seen {
        kind: kind_of(meta),
        stamp: FileStamp::of(meta),
        uid: meta.uid(),
        mode: meta.mode(),
        nlink: meta.nlink(),
    }
}

/// The `lstat` of `path`, `None` where nothing is there: `glance`'s where the caller looks
/// through one ([`Glance::symlink_metadata`]), a fresh one where it does not, which moves this
/// thread's count of fresh looks ([`fresh_looks`]).
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
    look_afresh();
    lstat_now(path)
}

/// The `lstat` of `path` taken now, `None` where nothing is there: [`lstat_in`]'s own, or a
/// glance's first ([`Glance::symlink_metadata`]).
///
/// # Errors
/// The `lstat`'s, other than a missing path.
fn lstat_now(path: &Path) -> io::Result<Option<std::fs::Metadata>> {
    #[cfg(test)]
    count_look(path);
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

std::thread_local! {
    /// How many times this thread looked at the disk afresh ([`fresh_looks`]).
    static FRESH_LOOKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many times this thread looked at the disk afresh, since it began. These looks move it: each
/// `lstat` no glance keeps ([`lstat_in`] on its own call, [`kind`], [`kind_and_id`], [`file_id`],
/// [`kind_and_stamp_if_present`], a glance's `lstat` that failed, which it does not keep, and the
/// `lstat` a glance confirms its first look at a file by, which it does not keep either
/// ([`Glance::first_stands`])), each open of a file through `open_admitted` (the one [`read_private`], [`read_public`],
/// [`read_owned`] and the digests [`digest_while_seen`] reads go through), the state lock's own
/// open (`client::StateLock::open_present`), each listing of the control journal
/// (`client::vault_memo::desk_files`), a memo's miss under a look
/// (`client::vault_memo::Memo::read_under`), and each other mark of a reader that reads past these
/// ([`look_afresh`]). A glance's first `lstat` of a path, which the glance keeps and gives for the
/// rest of its life ([`Glance::symlink_metadata`]), does not move it. Nor do some other looks a
/// derivation through a vault look takes, but each follows, within the same derivation, one that
/// moved it: `records_storage`'s `Context` (its `lstat` and `canonicalize` of the records' folder,
/// its `openat`s and the reads of its layout files) after `data_path`'s read of the pin or its
/// [`kind`], the state lock's `lstat` of the vault's folder as it narrows a lock this user owns
/// (`client::StateLock::narrow_owned_then`) after its open, and an opened file's `fstat` or ACL
/// read after its open. So a value a look at the vault derives while this stands still was derived
/// from its glance's `lstat`s alone and from caches whose entries those `lstat`s name
/// (`client::Moment`), and the look may keep it; one derived while it moved read something afresh,
/// and is derived again at the look's next ask, as each ask derived it before.
pub(crate) fn fresh_looks() -> u64 {
    FRESH_LOOKS.with(std::cell::Cell::get)
}

/// Count one fresh look at the disk on this thread ([`fresh_looks`]).
pub(crate) fn look_afresh() {
    FRESH_LOOKS.with(|looks| looks.set(looks.get().wrapping_add(1)));
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
/// ask it answers. A glance's first `lstat` of a path is the one look at the disk that does not
/// move this thread's count of fresh looks ([`fresh_looks`]): it stands for the rest of the
/// glance's life, so what a look at the vault derives from such `lstat`s alone may be kept for
/// that look (`client::Moment`). One that fails is not kept, and moves it. A glance's first look at
/// a file stands for it as of a later moment, past a read its caller made since, only where one
/// `lstat` taken then, afresh, shows the same, and the first showed the file settled when the
/// glance began ([`Self::first_stands`]); the glance keeps that `lstat` no more than a failed one,
/// and whether its first look showed a settled file it says without looking again
/// ([`Self::first_settled`]).
pub(crate) struct Glance {
    /// When this look began, before any `lstat` it took ([`asked_now`]).
    began: std::time::SystemTime,
    seen: std::cell::RefCell<Vec<(std::path::PathBuf, Option<std::fs::Metadata>)>>,
}

impl Glance {
    /// A look that has taken nothing yet, begun now.
    pub(crate) fn new() -> Self {
        Self::begun_at(asked_now())
    }

    /// A look that has taken nothing yet, begun at `began`: [`Self::new`]'s, or a test's own
    /// time, whose digests judge a file settled as of then ([`digest_while_seen`]).
    pub(crate) fn begun_at(began: std::time::SystemTime) -> Self {
        Self {
            began,
            seen: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The `lstat` of `path` this look took first, `None` where nothing was there; taken now
    /// where it took none. An error is not kept: the next ask looks again.
    ///
    /// # Errors
    /// The `lstat`'s, other than a missing path.
    pub(crate) fn symlink_metadata(&self, path: &Path) -> io::Result<Option<std::fs::Metadata>> {
        if let Some(meta) = self.first(path) {
            return Ok(meta);
        }
        // The one look at the disk that is no fresh look ([`fresh_looks`]): what it finds is
        // this glance's for the rest of its life. One that fails is not kept, and is.
        let meta = lstat_now(path).inspect_err(|_| look_afresh())?;
        self.seen
            .borrow_mut()
            .push((path.to_path_buf(), meta.clone()));
        Ok(meta)
    }

    /// The `lstat` of `path` this look took first, `Some(None)` where nothing was there; `None`
    /// where it took none. It looks at nothing.
    fn first(&self, path: &Path) -> Option<Option<std::fs::Metadata>> {
        let bytes = path.as_os_str().as_encoded_bytes();
        // The paths one check names share their folder: they differ at their ends.
        let same = |at: &std::path::PathBuf| {
            let at = at.as_os_str().as_encoded_bytes();
            at.len() == bytes.len() && at.iter().rev().eq(bytes.iter().rev())
        };
        self.seen
            .borrow()
            .iter()
            .find(|(at, _)| same(at))
            .map(|(_, meta)| meta.clone())
    }

    /// What this look's first `lstat` of `path` showed, where it showed a regular file that had
    /// settled ([`settled`]) when the look began; `None` where it took none of `path`, or its
    /// first showed nothing, something other than a regular file, or a file that changed within
    /// [`SETTLED_NS`] of when the look began. It looks at nothing.
    fn first_seen_settled(&self, path: &Path) -> Option<Seen> {
        self.first(path)
            .flatten()
            .map(|meta| seen_of(&meta))
            .filter(|first| first.kind == Kind::File && settled(first, self.began))
    }

    /// Whether the `lstat` of `path` this look took first showed a regular file that had settled
    /// ([`settled`]) when the look began, as [`Self::first_stands`] asks before it looks again:
    /// `false` where the look took none of `path`, or its first showed nothing or something else.
    /// It looks at nothing, so it moves no count of fresh looks ([`fresh_looks`]): for a caller
    /// that would hand the look on only where its first look at a file can be confirmed later.
    pub(crate) fn first_settled(&self, path: &Path) -> bool {
        self.first_seen_settled(path).is_some()
    }

    /// Whether the `lstat` of `path` this look took first still stands: it showed a regular file
    /// that had settled ([`settled`]) when the look began, and an `lstat` taken now shows the same
    /// ([`Seen`]: its kind, device, inode, length, both change times, owner, mode and link count).
    /// `false` where the look took none of `path`, or its first showed nothing or something else,
    /// or one taken now shows another file, a change or nothing, or fails. The `lstat` taken now
    /// is a fresh look ([`fresh_looks`]), and the look does not keep it: its first stands as it
    /// was. For a caller that judged a file through a look begun before a read of its own, and
    /// must judge it as of after that read: a change made after the look's first `lstat` moves
    /// what one taken now shows, as the settled rule has it for a digest kept of what a glance
    /// saw ([`digest_while_seen`]).
    pub(crate) fn first_stands(&self, path: &Path) -> bool {
        let Some(first) = self.first_seen_settled(path) else {
            return false;
        };
        look_afresh();
        matches!(lstat_now(path), Ok(Some(now)) if seen_of(&now) == first)
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
    static DIGESTS: KeptDigests = std::sync::Mutex::new(std::collections::VecDeque::new());
    let now = asked_now();
    #[cfg(any(test, debug_assertions))]
    if cfg!(test) || !ASKED_LATER.get().is_zero() {
        return THREAD_DIGESTS.with(|kept| {
            digest_while_seen_in(kept, glance, now, path, reader, |path| reader.digest(path))
        });
    }
    digest_while_seen_in(&DIGESTS, glance, now, path, reader, |path| {
        reader.digest(path)
    })
}

/// The time a kept digest's settled rule judges an ask, or the start of a glance, by: the clock,
/// and in a test (this crate's own, or another crate's in a build with debug assertions) as much
/// later as this thread's [`as_if_later`] puts it.
fn asked_now() -> std::time::SystemTime {
    let now = std::time::SystemTime::now();
    #[cfg(any(test, debug_assertions))]
    let now = now + ASKED_LATER.get();
    now
}

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    /// A test thread's own [`digest_while_seen`] digests, so one test's asks never let go of
    /// another's: every ask in this crate's own tests, and, in a build with debug assertions
    /// (another crate's tests'), each ask within an [`as_if_later`] that puts it later than the
    /// clock.
    static THREAD_DIGESTS: KeptDigests =
        const { std::sync::Mutex::new(std::collections::VecDeque::new()) };
}

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    /// How much later than the clock this thread's asks of [`digest_while_seen`], and the glances
    /// it begins, are ([`as_if_later`]).
    static ASKED_LATER: std::cell::Cell<std::time::Duration> =
        const { std::cell::Cell::new(std::time::Duration::ZERO) };
}

/// Run `f` with each digest this thread asks ([`digest_while_seen`]), and each glance it begins
/// ([`Glance::new`]), asked `by` later than the clock: a test's files have stood settled
/// ([`SETTLED_NS`]) without its waiting. Where `by` is not zero, the digests kept within it are
/// this thread's own ([`THREAD_DIGESTS`]), which no other thread's asks let go of. Another
/// crate's tests ask it as `client::test_as_if_later`.
#[cfg(any(test, debug_assertions))]
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

/// What one `lstat` each showed of a journal's folder and of the files a read of it reads
/// ([`aterm_journal::committed_files`]): the anchor, the checkpoint and the astream file. A reader
/// that keeps what a read of the journal found keeps it while a later one shows the same, where it
/// stands for that read ([`Self::stands_for`]): an append, a checkpoint renamed over, a new link
/// and a mode, owner or access-list change each move what one shows, which nothing moves back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalSeen([Seen; 4]);

impl JournalSeen {
    /// The journal `name` in `dir` as `lstat` shows it now; `None` where the folder or a file is
    /// missing or cannot be examined, or is not what a read admits (the folder a directory, each
    /// file a regular file).
    #[must_use]
    pub fn of(dir: &Path, name: &str) -> Option<Self> {
        let files = aterm_journal::committed_files(dir, name);
        let seen = |path: &Path, kind| {
            seen_if_present(path)
                .ok()
                .flatten()
                .filter(|seen| seen.kind == kind)
        };
        Some(Self([
            seen(dir, Kind::Dir)?,
            seen(&files.anchor, Kind::File)?,
            seen(&files.checkpoint, Kind::File)?,
            seen(&files.astream, Kind::File)?,
        ]))
    }

    /// Whether this, taken before a read that opened `opened`, may stand for what that read found:
    /// it shows the very files the read opened, and none of what it shows changed within
    /// [`settled`]'s window of `now`. A copy of the journal swapped in for the read, through a
    /// parent renamed aside and back, moves none of the times a witness holds, but each of its
    /// files is another inode, or one moved out of this folder, which moves the folder's times.
    /// The folder's own verdict is the read's: a folder swapped in for that verdict alone, and out
    /// again before the files are opened, is taken as a fresh read made then takes it.
    #[must_use]
    pub fn stands_for(&self, opened: &aterm_journal::Opened, now: std::time::SystemTime) -> bool {
        let [_, anchor, checkpoint, astream] = &self.0;
        let id = |seen: &Seen| (seen.stamp.dev, seen.stamp.ino);
        (id(anchor), id(checkpoint), id(astream))
            == (opened.anchor, opened.checkpoint, opened.astream)
            && self.0.iter().all(|seen| settled(seen, now))
    }

    /// The earliest and the latest change (each path's later of its content and inode change
    /// times, in nanoseconds) this shows, for a law that asks within or past [`settled`]'s window.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    #[must_use]
    pub fn test_changed_ns(&self) -> (i128, i128) {
        let changed = self
            .0
            .map(|seen| seen.stamp.mtime_ns.max(seen.stamp.ctime_ns));
        (
            changed.into_iter().min().unwrap_or_default(),
            changed.into_iter().max().unwrap_or_default(),
        )
    }
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
    look_afresh();
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
    look_afresh();
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
    look_afresh();
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
#[path = "fsx_tests.rs"]
mod tests;
