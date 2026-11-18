// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admitted file reads (`O_NOFOLLOW | O_NONBLOCK`) and atomic 0600 writes; on Windows the same
//! reads and writes under an access list in place of a mode (`fsx_win.rs`).
//!
//! With a trace scope armed on the calling thread (`aterm_uds::trace`), every look here records
//! what its answer is taken from, so a replay of the trace sees each file a run decided by. A
//! read records the file's stamp before its open, or that nothing is there ([`read_private`] and
//! every reader through `open_admitted`); an `lstat` whose answer holds a file's times records
//! its stamp ([`stamp`], [`seen_if_present`], [`JournalSeen::of`], a [`Glance`]'s, given or
//! kept); an `lstat` that answers less records a standing of what it answers ([`kind`],
//! [`kind_and_id`], [`file_id`], [`private_dir_settled`], and [`folder_in`] for a folder judged
//! by its identity, owner and mode, whose times every file written in it moves); a listing
//! records the names it lists and the kind each names ([`tree_paths`]). A look or read
//! that fails for a reason nothing recorded decides records a retry, and a write here that does
//! not go through `aterm_uds::atomic_file` records a write. With no scope armed, each adds one
//! thread-local look. The Windows arm records the same. A look of its own ([`Meta`], the name
//! opened as itself) where Unix records a stamp (a read through `open_admitted`, [`stamp`],
//! [`seen_if_present`], [`JournalSeen::of`], a [`Glance`]'s, [`has_private_nonempty`]'s) is made
//! after the trace's own look at the path recorded its stamp; one that answers less records a
//! standing of what it answers, as on Unix ([`kind`], [`kind_and_id`], [`file_id`],
//! [`private_dir_settled`], [`folder_in`], each of which takes its look with nothing recorded of
//! it). A folder's standing holds its attributes and its owner and access list in place of an
//! owner, group and mode, and the writers that make, rename or remove a name itself record a
//! write. A stamp there holds the file's volume and id, its change time, its names and a digest
//! of its owner and access list (`aterm_uds::trace::Meta` on Windows), so a change of who may
//! use a file read, or of its names, moves the stamp a replay looks at, as a `chmod`, `chown` or
//! `link` moves a Unix one.

use crate::{Error, SecretBytes};
use std::fs::File;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

#[cfg(windows)]
#[path = "fsx_win.rs"]
mod win;
#[cfg(windows)]
pub(crate) use win::FileAt;
#[cfg(windows)]
use win::MetaLen as _;
#[cfg(windows)]
pub(crate) use win::{
    access_digest, create_private_dir_new, create_private_file_new, dir_owned_unshared, id_of,
    lock_metadata, lock_wide, meta_of, narrow_lock, open_lock, open_lock_existing,
    owned_unshared_dir_id, owner_only, private_file_settled,
};
#[cfg(windows)]
pub use win::{ensure_private_dir, has_private_nonempty};
#[cfg(windows)]
use win::{gone, kind_of, open_admitted, private_dir_settled_now, seen_of, write_all_at};

/// What one look at a path, or at the file a read opened, shows: Unix's `lstat` or `fstat`
/// (`std::fs::Metadata`); on Windows the handle's own facts ([`aterm_dirfd::Stat`]), the name
/// opened as itself.
#[cfg(unix)]
pub(crate) type Meta = std::fs::Metadata;
/// What one look at a path, or at the file a read opened, shows: Unix's `lstat` or `fstat`
/// (`std::fs::Metadata`); on Windows the handle's own facts ([`aterm_dirfd::Stat`]), the name
/// opened as itself.
#[cfg(windows)]
pub(crate) type Meta = aterm_dirfd::Stat;

/// A file's number on its device ([`FileStamp::ino`]): Unix's inode; on Windows the 128-bit file
/// id (`FILE_ID_INFO`, which ReFS fills past 64 bits).
#[cfg(unix)]
pub type Ino = u64;
/// A file's number on its device ([`FileStamp::ino`]): Unix's inode; on Windows the 128-bit file
/// id (`FILE_ID_INFO`, which ReFS fills past 64 bits).
#[cfg(windows)]
pub type Ino = u128;

/// What the handle `file` is open on is (`fstat`).
#[cfg(unix)]
pub(crate) fn meta_of(file: &File) -> io::Result<Meta> {
    file.metadata()
}

/// Whether the file `meta` shows has lost its last link since it was opened.
#[cfg(unix)]
fn gone(meta: &Meta) -> bool {
    meta.nlink() == 0
}

/// The device and inode `meta` shows.
#[cfg(unix)]
pub(crate) fn id_of(meta: &Meta) -> (u64, Ino) {
    (meta.dev(), meta.ino())
}

/// The look at `path` that follows no final link (`lstat`).
#[cfg(unix)]
fn look_at(path: &Path) -> io::Result<Meta> {
    std::fs::symlink_metadata(path)
}

/// The look at `path` that follows no final link: the name opened as itself.
#[cfg(windows)]
fn look_at(path: &Path) -> io::Result<Meta> {
    win::lstat(path)
}

/// [`look_at`], recorded: with a trace scope armed, the stamp of what it shows
/// (`aterm_uds::trace::fs::symlink_metadata`, which on Unix is this very `lstat`).
///
/// # Errors
/// As [`look_at`].
#[cfg(unix)]
fn look_traced(path: &Path) -> io::Result<Meta> {
    aterm_uds::trace::fs::symlink_metadata(path)
}

/// [`look_at`], recorded: with a trace scope armed, the stamp the trace's own look at `path` shows
/// ([`record_look`]), taken just before the name is opened as itself, so a change after it moves
/// the stamp a replay looks at.
///
/// # Errors
/// As [`look_at`].
#[cfg(windows)]
fn look_traced(path: &Path) -> io::Result<Meta> {
    record_look(path);
    win::lstat(path)
}

/// Record, with a trace scope armed on this thread, the stamp the trace's own look at `path` shows
/// (`aterm_uds::trace::fs::symlink_metadata`), for a look the Windows arm then makes its own way
/// (the name opened as itself, its handle read), as `aterm_uds::trace::fs::opening` does for a
/// read. With none armed it looks at nothing.
#[cfg(windows)]
fn record_look(path: &Path) {
    if aterm_uds::trace::armed() {
        let _stamped = aterm_uds::trace::fs::symlink_metadata(path);
    }
}

#[cfg(unix)]
fn check_and_narrow_dir_fd(dir: &Path, dir_fd: &File) -> crate::Result<()> {
    check_access_list(dir_fd, true).map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    let meta = dir_fd
        .metadata()
        .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    if !owned_dir(dir, &meta)? {
        wrote();
        dir_fd
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    }
    Ok(())
}

/// Whether the folder `meta` describes is `0700` (any other mode [`ensure_private_dir`] makes
/// `0700`), refused where it is no folder or another user's, as [`ensure_private_dir`] refuses it.
#[cfg(unix)]
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
/// this user's, of mode exactly `0700` (its `lstat`); on Windows the user's folder under a
/// protected list of the user and SYSTEM alone (`fsx_win.rs`). With a trace scope armed, the
/// verdict is recorded as a standing of `dir` ([`dir_settled_standing`]), never its times.
///
/// # Errors
/// `dir` is missing, a link or no folder, another user's, or of another mode, which
/// [`ensure_private_dir`] would make `0700` (on Windows a reparse point, or a list that is not
/// private).
pub(crate) fn private_dir_settled(dir: &Path) -> crate::Result<()> {
    let verdict = private_dir_settled_now(dir);
    if aterm_uds::trace::armed() {
        aterm_uds::trace::standing(
            dir,
            Some(dir_settled_digest(&verdict)),
            dir_settled_standing,
        );
    }
    verdict
}

/// [`private_dir_settled`]'s verdict, with nothing recorded; on Windows the arm's own.
#[cfg(unix)]
fn private_dir_settled_now(dir: &Path) -> crate::Result<()> {
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

/// A trace's standing of the folder `dir` that [`private_dir_settled`] judges: the digest of its
/// verdict now ([`dir_settled_digest`]), so a folder made another's, opened up, narrowed, swapped
/// for a link or removed moves it, and a file written in it does not.
fn dir_settled_standing(dir: &Path) -> Option<[u8; 32]> {
    Some(dir_settled_digest(&private_dir_settled_now(dir)))
}

/// The digest of a [`private_dir_settled`] verdict: one value for a settled folder, and the
/// refusal's words for each other.
fn dir_settled_digest(verdict: &crate::Result<()>) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    match verdict {
        Ok(()) => digest.update([0]),
        Err(why) => {
            digest.update([1]);
            digest.update(why.to_string().as_bytes());
        }
    }
    digest.finalize()
}

/// Record, in a trace scope armed on this thread, that a run wrote: a file, a link, a folder or
/// a mode, by a writer here that does not go through `aterm_uds::atomic_file` (whose temporary
/// files record theirs).
fn wrote() {
    aterm_uds::trace::effect(aterm_uds::trace::Effect::Write);
}

/// Makes `dir`, a new `0700` directory (`mkdir`): a taken name is `AlreadyExists`. On Windows a new
/// folder of the user's under a protected list of the user and SYSTEM alone from its creation.
/// With a trace scope armed, it records a write.
///
/// # Errors
/// The creation's.
#[cfg(unix)]
pub(crate) fn create_private_dir_new(dir: &Path) -> io::Result<()> {
    wrote();
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

/// Ensures `dir` exists as a 0700 directory owned by this user, refusing symlinks. With a trace
/// scope armed, what it decides by is recorded as [`private_dir_settled`] records it, and each
/// step that makes a folder or sets a mode as a write.
#[cfg(unix)]
pub fn ensure_private_dir(dir: &Path) -> crate::Result<()> {
    if aterm_uds::trace::armed() {
        let _settled_or_why = private_dir_settled(dir);
    }
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
            wrote();
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
    wrote();
    aterm_uds::private_file::ensure_private_dir(dir)
        .map(|_| ())
        .map_err(|e| Error::new(format!("{}: {e}", dir.display())))
}

/// Mode repair cannot remove an extended ACL. Judge the retained descriptor
/// before reads or writes; the caller supplies its own mode and owner policy.
#[cfg(unix)]
fn check_access_list(file: &File, directory: bool) -> io::Result<()> {
    match access_list_refusal(file, directory)? {
        Some(why) => Err(io::Error::new(io::ErrorKind::PermissionDenied, why)),
        None => Ok(()),
    }
}

/// [`check_access_list`]'s verdict apart from a failure to reach one: why the file's access
/// list refuses it, `None` where it does not. An error is the look failing (an `fstat`, or the
/// list's read, which can fail for memory), not a verdict on the file: looked at again, it may
/// pass.
#[cfg(unix)]
fn access_list_refusal(file: &File, directory: bool) -> io::Result<Option<&'static str>> {
    let verdict = if directory {
        astream_sys::owner_private_dir(file)?
    } else {
        astream_sys::owner_private(file)?
    };
    Ok(matches!(verdict, Err(astream_sys::NotPrivate::Acl))
        .then_some("an access list (ACL) grants access beyond the file mode"))
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
    /// another user, a mode or an access list that lets other users write it, or past the bound.
    /// Read again, it is refused again until it changes.
    Refused(Error),
    /// The open, `fstat`, look at its access list or read failed (too many open files, memory,
    /// an I/O error): read again, it may read.
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
    let failed = |e: io::Error| {
        aterm_uds::trace::fs::failed(path, &e);
        Unread::Failed(Error::new(format!("{}: {e}", path.display())))
    };
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

/// Open the file at `path` for a read and admit it under `admit`, doing `links` with a symlink
/// there: the one opening every read here goes through (on Windows the arm's own, which records
/// the same). With a trace scope armed, the file's stamp (a link's target's too), or that nothing
/// is there, is recorded before the open, so a change after it moves the stamp a replay looks at,
/// and an open or `fstat` that failed records a retry ([`aterm_uds::trace::fs::failed`]). A
/// refusal records nothing more: what it is refused by is what the stamp shows, a link, a file of
/// another kind, owner or mode, or an access list (whose change moves the inode's change time);
/// on Windows a reparse point, a file of another kind, or its owner, access list or second name
/// (the stamp's digest of the owner and list, its change time and its names,
/// `aterm_uds::trace::Meta`).
#[cfg(unix)]
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
    let failed = |e: io::Error| {
        aterm_uds::trace::fs::failed(path, &e);
        Unread::Failed(Error::new(format!("{}: {e}", path.display())))
    };
    aterm_uds::trace::fs::opening(path);
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
            // A look at the access list that failed is a failed read, which a later look may
            // pass, never a refusal of the file as it is.
            let list = access_list_refusal(&file, false);
            #[cfg(test)]
            let list = if take_access_list_failure(path) {
                Err(io::Error::from(io::ErrorKind::OutOfMemory))
            } else {
                list
            };
            if let Some(why) = list.map_err(failed)? {
                return Err(refused(why.to_owned()));
            }
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
    /// moved (renamed aside, say) are refused. On Windows a file renamed over or removed while
    /// open shows no link left or a delete pending ([`aterm_dirfd::Stat`]), taken the same way.
    pub(crate) fn check_unchanged_classified(&self) -> Result<(), Unread> {
        let metadata = meta_of(&self.file)
            .map_err(|e| Unread::Failed(Error::new(format!("{}: {e}", self.path.display()))))?;
        let now = FileStamp::of(&metadata);
        let unlinked = gone(&metadata)
            && FileStamp {
                ctime_ns: self.stamp.ctime_ns,
                ..now
            } == self.stamp;
        // NTFS leaves a file's stamp as it was through a rename (measured 2026-11-16: a file
        // renamed aside while it was read kept its change time), so on Windows a file still
        // linked must still be the one at its path.
        #[cfg(windows)]
        let moved = !unlinked && file_id(&self.path) != Some(id_of(&metadata));
        #[cfg(unix)]
        let moved = false;
        if (now != self.stamp && !unlinked) || moved {
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
#[cfg(unix)]
#[must_use]
pub fn has_private_nonempty(path: &Path, max: usize) -> bool {
    let limit = u64::try_from(max).unwrap_or(u64::MAX);
    aterm_uds::trace::fs::symlink_metadata(path).is_ok_and(|meta| {
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
    #[cfg(unix)]
    let written = aterm_uds::atomic_file::write_with(path, mode, produce);
    #[cfg(windows)]
    let written = win::write_with(path, mode, produce);
    written.map_err(|e| Error::new(format!("{}: {e}", path.display())))
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
    // What the file holds decides whether it is written: with a trace scope armed, a run that
    // leaves it as it is records its stamp, so a change to it moves what a replay looks at.
    aterm_uds::trace::fs::opening(path);
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
    let Some(mut file) = opened_as(path, bytes, mode) else {
        return false;
    };
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

/// The file at `path` opened to read ([`holds_exactly`]), where it is a regular file of this
/// user's with one link, `bytes`' length, permissions exactly `mode` and no access list that
/// widens it; `None` where it is not, or cannot be opened or examined.
#[cfg(unix)]
fn opened_as(path: &Path, bytes: &[u8], mode: u32) -> Option<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_file()
        || meta.uid() != euid()
        || meta.nlink() != 1
        || meta.mode() & 0o7777 != mode & 0o7777
        || u64::try_from(bytes.len()).ok() != Some(meta.len())
        || check_access_list(&file, false).is_err()
    {
        return None;
    }
    Some(file)
}

/// The file at `path` opened as itself to read ([`holds_exactly`]), where it is a regular file
/// of one link and `bytes`' length whose access list is of `mode`'s class (owner-only for a
/// private mode, else this user's but not owner-only); `None` where it is not, or cannot be
/// opened or examined.
#[cfg(windows)]
fn opened_as(path: &Path, bytes: &[u8], mode: u32) -> Option<File> {
    let file = win::open_read(path).ok()?;
    win::holds_as(&file, u64::try_from(bytes.len()).ok()?, mode).then_some(file)
}

/// Opens or creates a `0600` regular lock file at `path` (`O_NOFOLLOW | O_NONBLOCK`), verifying
/// ownership and a single hard link, narrowing its permissions to `0600`, and keeping its
/// descriptor out of child processes. With a trace scope armed, a lock file it makes, and a mode
/// it narrows, record a write; a lock taken in one that was there records nothing, since taking
/// it changes no file. The Windows arm's records the same.
#[cfg(unix)]
pub(crate) fn open_lock(path: &Path) -> crate::Result<File> {
    if aterm_uds::trace::armed() && aterm_uds::trace::unrecorded(|| file_id(path)).is_none() {
        wrote();
    }
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
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
pub(crate) fn narrow_lock(file: &File, path: &Path) -> crate::Result<()> {
    wrote();
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

/// What tells one file's contents from another's without reading them: the device and inode
/// of the file at a path, its length, and when its contents and its inode last changed (in
/// nanoseconds). A file renamed over the path, or one written since, has another stamp; only a
/// rewrite in place of the same length within the clock's granularity keeps it, which no
/// writer of these files does (each writes a new file and renames it, or appends). A `chmod`,
/// `chown` or `link` moves its inode-change time alone, which a filesystem whose clock ticks
/// coarsely keeps within one tick: [`Seen`] holds what those change. On Windows the device is the
/// volume's serial number and the inode the file's 128-bit id; its change time moves at a write,
/// a rename, a new link and a change of its access list (measured on NTFS, 2026-11-16: `icacls`
/// moved it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    /// The device it is on.
    pub dev: u64,
    /// Its inode ([`Ino`]).
    pub ino: Ino,
    /// Its length in bytes.
    pub len: u64,
    /// When its contents last changed.
    pub mtime_ns: i128,
    /// When its inode last changed.
    pub ctime_ns: i128,
}

#[cfg(unix)]
impl FileStamp {
    pub(crate) fn of(meta: &std::fs::Metadata) -> Self {
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
/// owner, mode and link count that a stamp shows only through its inode-change time; on Windows
/// the link count and the attributes, its owner and access list showing in the change time (NTFS
/// moves it on a change to the security descriptor, measured 2026-11-16, though not within one
/// tick of the system clock, about 16 ms, of its last move).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub(crate) kind: Kind,
    pub(crate) stamp: FileStamp,
    #[cfg(unix)]
    pub(crate) uid: u32,
    #[cfg(unix)]
    pub(crate) mode: u32,
    pub(crate) nlink: u64,
    /// The `FILE_ATTRIBUTE_*` bits: an attribute change within a clock tick of the file's last
    /// change leaves its change time where it was.
    #[cfg(windows)]
    pub(crate) attributes: u32,
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
#[cfg(unix)]
fn seen_of(meta: &std::fs::Metadata) -> Seen {
    Seen {
        kind: kind_of(meta),
        stamp: FileStamp::of(meta),
        uid: meta.uid(),
        mode: meta.mode(),
        nlink: meta.nlink(),
    }
}

/// How [`seen_set`] looks at one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// Its kind, its [`FileStamp`] (device, inode, length, both change times), owner, group,
    /// mode and link count. For a file a reader reads, and for a directory whose entries are among
    /// what it reads: making, removing or renaming an entry moves the directory's change times.
    Whole,
    /// Its kind, device, inode, owner, group and mode, without its length, change times or link
    /// count: for a directory a reader only passes through, whose own entries may come and go
    /// without moving what that reader finds. On Windows its kind, volume, file id and
    /// attributes: its owner and access list show only in its change time there.
    Identity,
    /// What an identity look takes, and the directory's verdict under the owner-only rule
    /// ([`aterm_uds::private_file::judge_private_dir`]: missing, private, open by these mode bits,
    /// or refused): for a directory a reader holds to that rule before it reads a file in it,
    /// whose entries may come and go. That reader refuses an access-list grant, which no mode bit
    /// shows, and whose change time, the only time that moves with it, moves for each entry made
    /// or removed as well.
    Private,
}

/// A directory's verdict under the owner-only rule, as a [`Look::Private`] takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Nothing there when it was judged.
    Missing,
    /// This user's, `0700`, no access-list grant.
    Private,
    /// This user's and no access-list grant, open to others by these mode bits.
    Open(u32),
    /// Refused: an access-list grant, another user's, a symlink or no directory.
    Refused,
}

impl Verdict {
    /// `dir`'s verdict now; `None` where it could not be judged.
    fn of(dir: &Path) -> Option<Self> {
        use aterm_uds::private_file::PrivateDir;
        match aterm_uds::private_file::judge_private_dir(dir) {
            Ok(PrivateDir::Missing) => Some(Self::Missing),
            Ok(PrivateDir::Private) => Some(Self::Private),
            Ok(PrivateDir::Open(mode)) => Some(Self::Open(mode)),
            Err(refused) if refused.kind() == io::ErrorKind::PermissionDenied => {
                Some(Self::Refused)
            }
            Err(_) => None,
        }
    }
}

/// What one [`Look`] of a path found ([`seen_set`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeenAs {
    /// A [`Look::Whole`]: the [`Seen`] of its `lstat`, and the group it holds, which an
    /// identity look holds too. On Windows the [`Seen`] alone: a file's owner and access list
    /// show in its change time there, which it holds.
    Whole {
        seen: Seen,
        #[cfg(unix)]
        gid: u32,
    },
    /// A [`Look::Identity`].
    Identity(Identified),
    /// A [`Look::Private`]: what an identity look takes, and the verdict.
    Private {
        identity: Identified,
        verdict: Verdict,
    },
}

/// What an identity look takes of one `lstat` ([`Look::Identity`]): its kind, device and inode,
/// and its owner, group and mode. On Windows its kind, volume, file id and attributes: its owner
/// and access list show only in its change time there, which an identity look leaves out, and a
/// [`Look::Private`] judges them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identified {
    kind: Kind,
    dev: u64,
    ino: Ino,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
    #[cfg(unix)]
    mode: u32,
    #[cfg(windows)]
    attributes: u32,
}

impl Identified {
    /// What an identity look takes of the `lstat` `meta`.
    fn of(meta: &Meta) -> Self {
        let (dev, ino) = id_of(meta);
        Self {
            kind: kind_of(meta),
            dev,
            ino,
            #[cfg(unix)]
            uid: meta.uid(),
            #[cfg(unix)]
            gid: meta.gid(),
            #[cfg(unix)]
            mode: meta.mode(),
            #[cfg(windows)]
            attributes: meta.attributes,
        }
    }
}

impl SeenAs {
    /// What `look` takes of the `lstat` `meta` of `path`; `None` where a private look's verdict
    /// could not be judged.
    fn of(path: &Path, meta: &Meta, look: Look) -> Option<Self> {
        Some(match look {
            Look::Whole => Self::Whole {
                seen: seen_of(meta),
                #[cfg(unix)]
                gid: meta.gid(),
            },
            Look::Identity => Self::Identity(Identified::of(meta)),
            Look::Private => Self::Private {
                identity: Identified::of(meta),
                verdict: Verdict::of(path)?,
            },
        })
    }
}

/// What one `lstat` each of a list of paths showed, in the list's order, each as its [`Look`]
/// takes it, and `None` for a path where nothing was ([`seen_set`]). Equal to a set taken of the
/// same list only where each path showed the same: a file renamed over, written, appended to,
/// linked, removed or made, a `chmod` or `chown`, an entry made in or removed from a directory
/// looked at whole, and an access-list grant given or taken on a directory looked at privately
/// each move it, and a read moves none of it. Of a file's times, a `chmod`, `chown`
/// or link moves only its inode-change time, which a filesystem whose clock ticks coarsely can
/// keep within one tick, so the set holds the owner, group, mode and link count they change. What
/// it cannot tell is what a [`FileStamp`] cannot: a change within one tick of such a clock that
/// moves only the times, a rewrite in place at the same length, or an entry made in a directory
/// whose length does not count its entries. A set compares with one taken of the same list only:
/// it holds no path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenSet(Vec<(Look, Option<SeenAs>)>);

/// One `lstat` of each of `paths` (never through a final symlink), each taken as its [`Look`]
/// says, and a [`Look::Private`] of what is there judged as well; `None` where an `lstat` fails
/// other than for a missing path, or a verdict cannot be judged, which is read again then. Each
/// `lstat` is a fresh look ([`fresh_looks`]). Nothing is listed, and nothing is opened but a
/// directory a private look judges where its attributes cannot decide its verdict (off macOS
/// arm64, or where it carries an access list).
#[must_use]
pub fn seen_set(paths: &[(std::path::PathBuf, Look)]) -> Option<SeenSet> {
    paths
        .iter()
        .map(|(path, look)| {
            let seen = match lstat_in(None, path).ok()? {
                Some(meta) => Some(SeenAs::of(path, &meta, *look)?),
                None => None,
            };
            Some((*look, seen))
        })
        .collect::<Option<Vec<_>>>()
        .map(SeenSet)
}

/// `root` and every directory, file, symlink and other entry under it, never through a symlink,
/// sorted (a directory before what it holds), leaving out each entry `stop` names and all it
/// holds (a successor home inside the tree, which is stamped as a home of its own). Taken
/// [`Look::Whole`] by [`seen_set`], the list moves with every change to what the tree holds
/// without being listed again: an entry made, removed or renamed moves its directory's change
/// times, and one removed shows nothing where it was. A missing `root`, or one that is a file or
/// any other entry but a directory or a symlink, lists `root` alone. `None` where `root` is a
/// symlink, which a reader of the tree may follow where this walk does not, or where `root` or
/// an entry cannot be examined or a directory cannot be listed. Each listing, and the `lstat` of
/// `root`, is a fresh look
/// ([`fresh_looks`]); a directory gone between its listing and its own (`NotFound`) is in the
/// list as an entry of its parent and lists nothing. With a trace scope armed, the `lstat` of
/// `root` records its stamp, and each directory's listing the names it holds and the kind each
/// names, or that nothing is there (`aterm_uds::trace::fs::read_dir`), so an entry made, removed
/// or renamed anywhere the walk listed moves the trace.
#[must_use]
pub fn tree_paths(root: &Path, stop: &[std::path::PathBuf]) -> Option<Vec<std::path::PathBuf>> {
    let mut paths = vec![root.to_path_buf()];
    match lstat_in(None, root) {
        Ok(Some(meta)) => match kind_of(&meta) {
            Kind::Symlink => return None,
            Kind::Dir => {}
            _ => return Some(paths),
        },
        Ok(None) => return Some(paths),
        Err(_) => return None,
    }
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        look_afresh();
        let entries = match aterm_uds::trace::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        for entry in entries {
            let path = entry.path();
            if stop.contains(&path) {
                continue;
            }
            // The entry's own type (`lstat`'s, or the directory's record of it): a symlink to a
            // directory is an entry, never walked.
            if entry.file_type().ok()?.is_dir() {
                folders.push(path.clone());
            }
            paths.push(path);
        }
    }
    paths.sort();
    Some(paths)
}

/// The `lstat` of `path`, `None` where nothing is there: `glance`'s where the caller looks
/// through one ([`Glance::symlink_metadata`]), a fresh one where it does not, which moves this
/// thread's count of fresh looks ([`fresh_looks`]).
///
/// # Errors
/// The `lstat`'s, other than a missing path.
pub(crate) fn lstat_in(glance: Option<&Glance>, path: &Path) -> io::Result<Option<Meta>> {
    if let Some(glance) = glance {
        return glance.symlink_metadata(path);
    }
    look_afresh();
    lstat_now(path)
}

/// The `lstat` of `path` taken now, `None` where nothing is there: [`lstat_in`]'s own, or a
/// glance's first ([`Glance::symlink_metadata`]). With a trace scope armed, the stamp this very
/// `lstat` shows is recorded ([`look_traced`]; on Windows the stamp of the trace's own look, taken
/// just before).
///
/// # Errors
/// The `lstat`'s, other than a missing path.
fn lstat_now(path: &Path) -> io::Result<Option<Meta>> {
    #[cfg(test)]
    count_look(path);
    match look_traced(path) {
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
/// `lstat` no glance keeps ([`lstat_in`] on its own call, [`seen_set`]'s and [`tree_paths`]' among
/// them, [`kind`], [`kind_and_id`], [`file_id`], [`kind_and_stamp_if_present`], a glance's `lstat`
/// that failed, which it does not keep, and the `lstat` a glance confirms its first look at a file
/// by, which it does not keep either ([`Glance::first_stands`])), each open of a file through
/// `open_admitted` (the one [`read_private`], [`read_public`], [`read_owned`] and the digests
/// [`digest_while_seen`] reads go through), the state lock's own open
/// (`client::StateLock::open_present`), each listing of a directory by [`tree_paths`] (the control
/// journal's, `client::vault_memo::desk_files`), a memo's miss under a look
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
    seen: std::cell::RefCell<Vec<(std::path::PathBuf, Option<Meta>)>>,
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
    /// where it took none. An error is not kept: the next ask looks again. With a trace scope
    /// armed, the first is recorded as it is taken ([`lstat_now`]), and one given again is
    /// recorded again ([`retrace`]), since the scope that heard the first may not be the one armed
    /// now.
    ///
    /// # Errors
    /// The `lstat`'s, other than a missing path.
    pub(crate) fn symlink_metadata(&self, path: &Path) -> io::Result<Option<Meta>> {
        if let Some(meta) = self.first(path) {
            if aterm_uds::trace::armed() {
                retrace(path, meta.as_ref());
            }
            return Ok(meta);
        }
        self.take_first(path, lstat_now)
    }

    /// The `lstat` of the folder `dir` this look took first, for a caller that judges a folder by
    /// its identity, owner and mode alone ([`folder_in`], which records what it judges): taken now
    /// where it took none, with nothing recorded of it, whose times every file written in the
    /// folder moves. A later [`Self::symlink_metadata`] of `dir` records its stamp then.
    ///
    /// # Errors
    /// The `lstat`'s, other than a missing path.
    fn folder(&self, dir: &Path) -> io::Result<Option<Meta>> {
        if let Some(meta) = self.first(dir) {
            return Ok(meta);
        }
        self.take_first(dir, |dir| aterm_uds::trace::unrecorded(|| lstat_now(dir)))
    }

    /// Take this look's first `lstat` of `path` by `lstat`, and keep it.
    fn take_first(
        &self,
        path: &Path,
        lstat: impl FnOnce(&Path) -> io::Result<Option<Meta>>,
    ) -> io::Result<Option<Meta>> {
        // The one look at the disk that is no fresh look ([`fresh_looks`]): what it finds is
        // this glance's for the rest of its life. One that fails is not kept, and is.
        let meta = lstat(path).inspect_err(|_| look_afresh())?;
        #[cfg_attr(
            windows,
            expect(
                clippy::clone_on_copy,
                reason = "Unix's Metadata is no Copy; Windows' Stat is"
            )
        )]
        self.seen
            .borrow_mut()
            .push((path.to_path_buf(), meta.clone()));
        Ok(meta)
    }

    /// The `lstat` of `path` this look took first, `Some(None)` where nothing was there; `None`
    /// where it took none. It looks at nothing.
    fn first(&self, path: &Path) -> Option<Option<Meta>> {
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
            .map(|(_, meta)| meta)
            .cloned()
    }

    /// What this look's first `lstat` of `path` showed, where it showed a regular file that had
    /// settled ([`settled`]) when the look began; `None` where it took none of `path`, or its
    /// first showed nothing, something other than a regular file, or a file that changed within
    /// [`SETTLED`] of when the look began. It looks at nothing.
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

/// Record, in the trace scope armed on this thread, what a glance gives again of `path`, which it
/// took first as `first` (`None` for nothing there): the stamp one `lstat` taken now shows, which
/// the glance does not keep, recorded as any read's is, and a retry where that shows other than
/// `first` did ([`Seen`]: its kind, stamp, owner, mode and link count; on Windows its kind,
/// stamp, link count and attributes), since the run then took an answer from before a change no
/// record shows. This `lstat` is the trace's, not the reader's, so it moves no count of fresh
/// looks ([`fresh_looks`]).
fn retrace(path: &Path, first: Option<&Meta>) {
    let now = look_traced(path);
    let same = match (&now, first) {
        (Ok(now), Some(first)) => seen_of(now) == seen_of(first),
        (Err(error), None) => error.kind() == io::ErrorKind::NotFound,
        _ => false,
    };
    if !same {
        aterm_uds::trace::retry(format!(
            "{}: changed after a look's first lstat of it",
            path.display()
        ));
    }
}

/// The `lstat` of the folder `dir`, `None` where nothing is there, for a caller that judges the
/// folder by its kind, device, inode, owner, group and mode alone (on Windows its kind, volume,
/// file id and attributes, and its owner and access list, [`access_digest`]), never its times,
/// which every file written in it moves (a vault folder whose `state.toml` the vault's subscriber
/// rewrites about once a minute): `glance`'s where the caller looks through one
/// ([`Glance::folder`]), a fresh one where it does not. With a trace scope armed, what the caller
/// judges is recorded as a standing of `dir` ([`folder_standing`]) and no stamp of it, and an
/// `lstat` that failed for any reason but a missing path, or an access list that could not be
/// read, records a retry.
///
/// # Errors
/// The `lstat`'s, other than a missing path.
pub(crate) fn folder_in(glance: Option<&Glance>, dir: &Path) -> io::Result<Option<Meta>> {
    if !aterm_uds::trace::armed() {
        return lstat_in(glance, dir);
    }
    let looked = match glance {
        Some(glance) => glance.folder(dir),
        None => {
            look_afresh();
            aterm_uds::trace::unrecorded(|| lstat_now(dir))
        }
    };
    match &looked {
        Ok(meta) => match folder_digest(dir, meta.as_ref()) {
            Some(digest) => aterm_uds::trace::standing(dir, Some(digest), folder_standing),
            None => aterm_uds::trace::retry(format!(
                "{}: its access list could not be read",
                dir.display()
            )),
        },
        Err(error) => aterm_uds::trace::retry(format!("{}: {error}", dir.display())),
    }
    looked
}

/// A trace's standing of a folder [`folder_in`] judged: the digest of what one `lstat` of `dir`
/// shows now ([`folder_digest`]); `None` where it fails for any reason but a missing path.
fn folder_standing(dir: &Path) -> Option<[u8; 32]> {
    match look_at(dir) {
        Ok(meta) => folder_digest(dir, Some(&meta)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => folder_digest(dir, None),
        Err(_) => None,
    }
}

/// The digest of what [`folder_in`]'s callers judge of the `lstat` `meta` of the folder `dir`: its
/// kind, device, inode, owner, group and mode, or that nothing is there. Always `Some` here: the
/// Windows arm's is `None` where it cannot read the folder's access list.
#[cfg(unix)]
fn folder_digest(_dir: &Path, meta: Option<&Meta>) -> Option<[u8; 32]> {
    let fields = meta.map_or([0; 6], |meta| {
        [
            u128::from(kind_tag(kind_of(meta))),
            u128::from(meta.dev()),
            wide(meta.ino()),
            u128::from(meta.uid()),
            u128::from(meta.gid()),
            u128::from(meta.mode()),
        ]
    });
    Some(answer_digest(b'f', &fields))
}

/// The digest of what [`folder_in`]'s callers judge of the look `meta` of the folder `dir` on
/// Windows: its kind, volume, file id and attributes, and its owner and access list
/// ([`access_digest`], read now), or that nothing is there; `None` where the access list cannot be
/// read.
#[cfg(windows)]
fn folder_digest(dir: &Path, meta: Option<&Meta>) -> Option<[u8; 32]> {
    let Some(meta) = meta else {
        return Some(answer_digest(b'f', &[0; 4]));
    };
    let (dev, ino) = id_of(meta);
    let fields = answer_digest(
        b'f',
        &[
            u128::from(kind_tag(kind_of(meta))),
            u128::from(dev),
            wide(ino),
            u128::from(meta.attributes),
        ],
    );
    let access = aterm_uds::trace::unrecorded(|| access_digest(dir)).ok()?;
    let mut digest = aterm_digest::Sha256::new();
    digest.update(fields);
    digest.update(access);
    Some(digest.finalize())
}

/// A [`Kind`] as a number a standing's digest holds.
const fn kind_tag(kind: Kind) -> u64 {
    match kind {
        Kind::Missing => 0,
        Kind::File => 1,
        Kind::Symlink => 2,
        Kind::Dir => 3,
        Kind::Other => 4,
    }
}

/// The digest of an answer of one of this module's looks that a trace's standing holds: `tag`
/// names the look, `fields` the answer, each wide enough for a Windows file id ([`Ino`]).
fn answer_digest(tag: u8, fields: &[u128]) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    digest.update([tag]);
    for field in fields {
        digest.update(field.to_le_bytes());
    }
    digest.finalize()
}

/// `ino` as a field of [`answer_digest`].
#[cfg(unix)]
fn wide(ino: Ino) -> u128 {
    u128::from(ino)
}

/// `ino` as a field of [`answer_digest`]: already a 128-bit file id.
#[cfg(windows)]
const fn wide(ino: Ino) -> u128 {
    ino
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
/// opened was the one that `lstat` saw and had stood [`SETTLED`] unchanged when that `lstat`
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
/// ([`SETTLED`]) without its waiting. Where `by` is not zero, the digests kept within it are
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
/// that took it) its file must have last changed for the digest to be kept: the settle window of
/// `aterm_uds::trace`, whose rule ([`settled`]) this crate's kept digests and a trace's reads
/// share. A measurement of quiet rounds waits it out, and a tenth of a second past it, after its
/// fixture's last write (`client::test_settled_after`).
pub(crate) const SETTLED: std::time::Duration = aterm_uds::trace::SETTLED;

/// Whether what `seen` shows last changed [`SETTLED`] or more before `now` (`aterm_uds::trace`'s
/// rule, `trace::settled`, over the later of its modification and inode-change times), so that a
/// change made after `now` moves a stamp a later `lstat` shows, on a clock that ticks this
/// coarsely or finer. Only then may a reader keep what it read while the path's [`Seen`] stands.
pub(crate) fn settled(seen: &Seen, now: std::time::SystemTime) -> bool {
    stamp_settled(&seen.stamp, now)
}

/// The time a read judges by whether the file it is about to read had settled
/// ([`stamp_settled`]): the clock, and in a test as much later as [`as_if_later`] puts it.
pub(crate) fn read_began() -> std::time::SystemTime {
    asked_now()
}

/// Whether the file whose stamp is `stamp` had stood [`SETTLED`] unchanged at `began`
/// (`aterm_uds::trace::settled`), a time taken before its bytes were read: a write after that
/// takes a later time on a clock that ticks this coarsely or finer, so no other bytes ever show
/// this stamp. One taken within its file's own tick may later show other bytes, written in place
/// at the same length.
pub(crate) fn stamp_settled(stamp: &FileStamp, began: std::time::SystemTime) -> bool {
    aterm_uds::trace::settled(stamp.mtime_ns.max(stamp.ctime_ns), began)
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
    match look_traced(path) {
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
    let appended = open_append(path, expected).and_then(|mut file| {
        wrote();
        {
            let mut exact = ExactWriter {
                writer: &mut file,
                remaining: length,
            };
            produce(&mut exact)?;
            exact.finish()?;
        }
        aterm_durable::durable_data(&file)?;
        meta_of(&file).map(|after| FileStamp::of(&after))
    });
    appended.map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

/// [`append_at_with`]'s open: `path` opened to append (never through a symlink), where it is
/// still the private regular file of one link `expected` stamped (its device, inode and length).
///
/// # Errors
/// The open, its access list or `fstat` fails, or the file is not that one.
#[cfg(unix)]
fn open_append(path: &Path, expected: &FileStamp) -> io::Result<File> {
    let file = OpenOptions::new()
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
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
    Ok(file)
}

/// [`append_at_with`]'s open on Windows ([`win::open_append`]).
///
/// # Errors
/// The open, its access list or its facts fail, or the file is not that one.
#[cfg(windows)]
fn open_append(path: &Path, expected: &FileStamp) -> io::Result<File> {
    win::open_append(path, expected)
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
    #[cfg(unix)]
    let written = aterm_uds::atomic_file::write_with_metadata(path, mode, produce)
        .map(|written| (written.dev(), written.ino()));
    #[cfg(windows)]
    let written = win::write_with_id(path, mode, produce);
    let written = written.map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    after();
    Ok(stamp(path).filter(|placed| (placed.dev, placed.ino) == written))
}

#[cfg(test)]
#[path = "fsx_stream_tests.rs"]
mod stream_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "fsx_acl_tests.rs"]
mod acl_tests;

#[cfg(all(test, unix))]
#[path = "fsx_seen_tests.rs"]
mod seen_tests;

/// [`write_atomic`] for one of many files a pass writes: the temporary file's bytes ordered
/// before its rename ([`aterm_durable::ordered`]) and no flush of the directory, which the
/// pass flushes once for all its files, so `path` holds `bytes` or what it held, never a part.
pub fn write_ordered(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<()> {
    let (tmp, mut file) = temp_beside(path, mode)?;
    let placed = file
        .write_all(bytes)
        .and_then(|()| set_mode(&file, mode))
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
    #[cfg(unix)]
    let made = aterm_uds::atomic_file::create_temp_beside(path, mode);
    #[cfg(windows)]
    let made = win::create_temp(path, mode);
    made.map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

/// Set the temporary `file` [`temp_beside`] made to `mode` (`fchmod`), past the umask.
#[cfg(unix)]
fn set_mode(file: &File, mode: u32) -> io::Result<()> {
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

/// Nothing on Windows: [`temp_beside`] made the temporary in `mode`'s class from its creation.
#[cfg(windows)]
const fn set_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Writes `bytes` to `path` at `mode` only if `path` does not exist.
///
/// The directory's flush after the link is best-effort and never reported, unlike
/// [`write_atomic`]'s: once the link has made `path`, an error would tell the caller nothing
/// was placed while its next try answers "taken", a claim no caller owns.
pub fn write_new(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<bool> {
    write_new_durable(path, bytes, mode, || Ok(()))
}

/// [`write_new`] that runs `staged` once the bytes are written whole and made durable beside
/// `path`, just before the link that places them: what `staged` does (the Windows machine key's
/// making sets aside the file DPAPI no longer opens) happens only once nothing but that link is
/// left to fail. Where `staged` fails, the temporary file is removed and nothing is placed.
///
/// # Errors
/// As [`write_new`], or `staged`'s.
#[cfg(windows)]
pub(crate) fn write_new_staged(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    staged: impl FnOnce() -> crate::Result<()>,
) -> crate::Result<bool> {
    write_new_durable(path, bytes, mode, staged)
}

/// [`write_new`] with `staged` run before its link ([`write_new_staged`]), the folder flushed
/// (best-effort) after a link that placed it.
fn write_new_durable(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    staged: impl FnOnce() -> crate::Result<()>,
) -> crate::Result<bool> {
    let placed = write_new_flushed(path, bytes, mode, sync, staged)?;
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
    write_new_flushed(path, bytes, mode, aterm_durable::ordered, || Ok(()))
}

/// [`write_new`]'s link, the temporary file's bytes finished by `flush` before it and `staged` run
/// between the two.
fn write_new_flushed(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    flush: fn(&File) -> io::Result<()>,
    staged: impl FnOnce() -> crate::Result<()>,
) -> crate::Result<bool> {
    let (tmp, mut file) = temp_beside(path, mode)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| set_mode(&file, mode))
        .and_then(|()| flush(&file));
    drop(file);
    let placed = match written {
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
        Ok(()) => match staged() {
            Err(e) => Err(e),
            Ok(()) => match link_then_unlink(&tmp, path) {
                Ok(()) => Ok(true),
                Err(Moved::NotLinked(e)) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                Err(Moved::NotLinked(e)) => Err(Error::new(format!("{}: {e}", path.display()))),
                Err(Moved::NotUnlinked(e)) => Err(Error::new(format!("{}: {e}", tmp.display()))),
            },
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

/// What is at `path` (`lstat`). With a trace scope armed, the kind is recorded as a standing of
/// `path` ([`kind_standing`]), so a file written, or one renamed over it, moves nothing a replay
/// looks at, and a failed `lstat` records a retry.
pub fn kind(path: &Path) -> crate::Result<Kind> {
    let found = kind_and_id_now(path).map(|(kind, _)| kind);
    if aterm_uds::trace::armed() {
        match &found {
            Ok(kind) => aterm_uds::trace::standing(path, Some(kind_digest(*kind)), kind_standing),
            Err(error) => aterm_uds::trace::retry(error.to_string()),
        }
    }
    found
}

/// What is at `path` (`lstat`), together with its `(device, inode)` when present (on Windows its
/// volume and 128-bit file id). With a trace scope armed, both are recorded as a standing of
/// `path` ([`kind_and_id_standing`]), and a failed `lstat` records a retry.
pub fn kind_and_id(path: &Path) -> crate::Result<(Kind, Option<(u64, Ino)>)> {
    let found = kind_and_id_now(path);
    record_kind_and_id(path, &found);
    found
}

/// [`kind_and_id`]'s answer, with nothing recorded.
fn kind_and_id_now(path: &Path) -> crate::Result<(Kind, Option<(u64, Ino)>)> {
    look_afresh();
    match look_at(path) {
        Ok(meta) => Ok((kind_of(&meta), Some(id_of(&meta)))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((Kind::Missing, None)),
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
    }
}

/// Record, in a trace scope armed on this thread, that a run took `found`, [`kind_and_id`]'s
/// answer for `path`: a standing of it, or a retry for a failed `lstat`.
fn record_kind_and_id(path: &Path, found: &crate::Result<(Kind, Option<(u64, Ino)>)>) {
    if aterm_uds::trace::armed() {
        match found {
            Ok(found) => aterm_uds::trace::standing(
                path,
                Some(kind_and_id_digest(*found)),
                kind_and_id_standing,
            ),
            Err(error) => aterm_uds::trace::retry(error.to_string()),
        }
    }
}

/// A trace's standing of `path` as [`kind`] answers it now; `None` where its `lstat` fails.
fn kind_standing(path: &Path) -> Option<[u8; 32]> {
    kind_and_id_now(path)
        .ok()
        .map(|(kind, _)| kind_digest(kind))
}

/// A trace's standing of `path` as [`kind_and_id`] answers it now; `None` where its `lstat`
/// fails.
fn kind_and_id_standing(path: &Path) -> Option<[u8; 32]> {
    kind_and_id_now(path).ok().map(kind_and_id_digest)
}

/// The digest of a [`kind`] answer.
fn kind_digest(kind: Kind) -> [u8; 32] {
    answer_digest(b'k', &[u128::from(kind_tag(kind))])
}

/// The digest of a [`kind_and_id`] answer.
fn kind_and_id_digest((kind, id): (Kind, Option<(u64, Ino)>)) -> [u8; 32] {
    let (dev, ino) = id.unwrap_or_default();
    answer_digest(
        b'i',
        &[u128::from(kind_tag(kind)), u128::from(dev), wide(ino)],
    )
}

#[cfg(unix)]
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

/// Returns `(device, inode)` of `path` via `lstat` (on Windows its volume and 128-bit file id),
/// or `None` if missing or it cannot be examined. With a trace scope armed, it records what
/// [`kind_and_id`] records.
pub fn file_id(path: &Path) -> Option<(u64, Ino)> {
    let found = kind_and_id_now(path);
    record_kind_and_id(path, &found);
    found.ok().and_then(|(_, id)| id)
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
    #[cfg_attr(
        windows,
        expect(
            dead_code,
            reason = "a Windows move is one rename that replaces nothing, with no removal to fail"
        )
    )]
    NotUnlinked(io::Error),
}

/// [`rename_noreplace`]'s steps, with the step that failed and its OS error.
#[cfg(unix)]
fn link_then_unlink(from: &Path, to: &Path) -> Result<(), Moved> {
    std::fs::hard_link(from, to).map_err(Moved::NotLinked)?;
    wrote();
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

/// [`rename_noreplace`] on Windows: one rename that refuses a taken name in the same step
/// ([`win::rename_noreplace`]), so nothing is ever linked twice and no removal can fail apart.
/// With a trace scope armed, a rename that moved the file records a write.
#[cfg(windows)]
fn link_then_unlink(from: &Path, to: &Path) -> Result<(), Moved> {
    win::rename_noreplace(from, to).inspect(|()| wrote())
}

/// Zeroes and removes a regular file at `path` (one this user cannot read stays), or unlinks a
/// symlink without following it.
pub fn wipe_and_remove(path: &Path) -> crate::Result<()> {
    if kind(path)? == Kind::File {
        return wipe(path, |_, _| Ok(true)).map(drop);
    }
    #[cfg(unix)]
    let removed = std::fs::remove_file(path);
    #[cfg(windows)]
    let removed = win::remove_link(path);
    match removed {
        Ok(()) => {
            wrote();
            Ok(())
        }
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

/// [`wipe_if`]'s steps, with `judge` reading the file it opened. With a trace scope armed, the
/// file's stamp is recorded before its open, since `judge` decides by what it holds, and its
/// removal as a write.
fn wipe(
    path: &Path,
    judge: impl FnOnce(&mut File, &Meta) -> io::Result<bool>,
) -> crate::Result<bool> {
    aterm_uds::trace::fs::opening(path);
    let at = |e: io::Error| Error::new(format!("{}: {e}", path.display()));
    #[cfg(unix)]
    let open = |write: bool| {
        OpenOptions::new()
            .read(true)
            .write(write)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
    };
    #[cfg(windows)]
    let open = |write: bool| win::open_wipe(path, write);
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
            ) || refused_link(&e) =>
        {
            return Ok(false);
        }
        Err(e) => return Err(at(e)),
    };
    let meta = meta_of(&file).map_err(at)?;
    if !meta.file_type().is_file()
        || !judge(&mut file, &meta).map_err(at)?
        || file_id(path) != Some(id_of(&meta))
    {
        return Ok(false);
    }
    wrote();
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(at(e)),
    }
    let zeros = [0u8; 4096];
    let mut done = 0;
    while done < meta.len() {
        let n = usize::try_from((meta.len() - done).min(4096)).unwrap_or(4096);
        if write_all_at(&file, &zeros[..n], done).is_err() {
            break;
        }
        done += n as u64;
    }
    let _ = sync(&file);
    Ok(true)
}

/// Whether an open's error is its refusal of a symlink at the name (`O_NOFOLLOW`'s `ELOOP`).
#[cfg(unix)]
fn refused_link(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ELOOP)
}

/// Never on Windows: a reparse point at the name opens as itself, and is judged no regular file.
#[cfg(windows)]
const fn refused_link(_: &io::Error) -> bool {
    false
}

/// Write all of `bytes` to `file` at `offset` (`pwrite`).
#[cfg(unix)]
fn write_all_at(file: &File, bytes: &[u8], offset: u64) -> io::Result<()> {
    file.write_all_at(bytes, offset)
}

fn sync(file: &File) -> io::Result<()> {
    aterm_durable::durable_data(file)
}

/// This process's effective uid.
#[cfg(unix)]
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

/// Looks at the access list of each path this test process fails ([`fail_access_lists`]).
#[cfg(test)]
static ACCESS_LIST_FAILS: std::sync::Mutex<std::collections::BTreeMap<std::path::PathBuf, usize>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Fail the next `times` looks at the access list of the file at `path` as a look that ran out
/// of memory would, after the file opened.
#[cfg(test)]
pub(crate) fn fail_access_lists(path: &Path, times: usize) {
    ACCESS_LIST_FAILS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_path_buf(), times);
}

/// Whether this look at the access list of `path` is one [`fail_access_lists`] fails, counting
/// it.
#[cfg(test)]
fn take_access_list_failure(path: &Path) -> bool {
    let mut fails = ACCESS_LIST_FAILS
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
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(all(test, unix))]
thread_local! {
    /// The uid [`euid`] answers on this thread while [`as_another_user`] runs.
    static EUID_SEAM: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with [`euid`] answering a uid other than this process's on this thread: every file this
/// user made is then another user's to the checks inside it, as a lock another account left is.
#[cfg(all(test, unix))]
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

/// The trace's laws of this module, on both arms: what a read, a look, a walk and a write record,
/// where a Unix mode and a Windows access list each stand for who may use a file.
#[cfg(test)]
#[path = "fsx_trace_tests.rs"]
mod trace_tests;
