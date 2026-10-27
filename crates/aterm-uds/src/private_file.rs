// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Owner-only private file and directory operations backed by `astream_sys` checks.

use std::fmt;
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, Instant};

/// Error returned when reading or opening a private state file ([`read_private`]).
#[derive(Debug)]
pub enum PrivateFileError {
    /// File or state directory does not exist.
    Missing,
    /// File or directory is not owned exclusively by the current user or is accessible to others.
    NotPrivate(&'static str),
    /// File or directory is a symbolic link.
    Symlink,
    /// Target is not a regular file, name spans multiple path components, or size exceeds the cap.
    Malformed(&'static str),
    /// Underlying I/O error.
    Io(io::Error),
}

impl fmt::Display for PrivateFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("there is no such file"),
            Self::NotPrivate(why) => write!(f, "not private: {why}"),
            Self::Symlink => f.write_str("it or its directory is a symlink"),
            Self::Malformed(why) => f.write_str(why),
            Self::Io(e) => write!(f, "it could not be opened or read: {e}"),
        }
    }
}

impl std::error::Error for PrivateFileError {}

/// Why a file or directory with an access list for other users is not private.
pub const ACL_OPEN: &str = "an access list (ACL) grants other users access; `chmod -N` removes it";
/// Why a file others may read is not private.
pub const FILE_OPEN: &str = "it is open to other users (want 0600)";
/// Why a directory others may enter is not private.
pub const DIR_OPEN: &str = "the directory is open to other users (want 0700)";
/// Why a file with a second hard link is not private: that link is another copy of it.
pub const ANOTHER_LINK: &str = "it has another link";
/// Why a file [`read_owned_into`] reads is refused: others may write it.
const FILE_WRITABLE: &str = "it is open to other users' writes (want no group or other write bit)";
/// Why a directory [`read_owned_into`] reads in is refused: others may write it.
const DIR_WRITABLE: &str =
    "the directory is open to other users' writes (want no group or other write bit)";

/// How [`open_judged`] opens and judges a private file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Opening {
    /// Read only, under the strict rules ([`read_private`]).
    Read,
    /// Read and write, created 0600 when missing, under the strict rules
    /// ([`open_private_rw`]).
    ReadWrite,
    /// Read only, with group and other read and search bits allowed, and no write bit
    /// ([`read_owned_into`]).
    Owned,
}

/// Whether permission bits `mode` (`& 0o777`) that astream-sys's owner-only rule refused
/// pass [`Opening::Owned`]'s looser one: `Some(loose)` when no group or other WRITE bit is
/// set, `loose` saying a group or other READ bit is (a search bit alone lets nobody read,
/// and is neither refused nor loose); `None` when one is, or the opening is a strict one.
/// astream-sys judges the mode last, so every other clause (kind, owner, access list)
/// held.
fn owned_mode(how: Opening, mode: u32) -> Option<bool> {
    (how == Opening::Owned && mode & 0o022 == 0).then_some(mode & 0o044 != 0)
}

/// `file` held to astream-sys's owner-only rule, loosened as `how` says: its metadata, and
/// whether a group or other read bit let it through (only [`Opening::Owned`] lets one).
fn file_verdict(
    file: &std::fs::File,
    how: Opening,
) -> Result<(std::fs::Metadata, bool), PrivateFileError> {
    use astream_sys::NotPrivate;
    match astream_sys::owner_private(file).map_err(PrivateFileError::Io)? {
        Ok(meta) => Ok((meta, false)),
        Err(NotPrivate::Kind(_)) => Err(PrivateFileError::Malformed("not a regular file")),
        Err(NotPrivate::Owner(_)) => Err(PrivateFileError::NotPrivate("it is another user's")),
        Err(NotPrivate::Acl) => Err(PrivateFileError::NotPrivate(ACL_OPEN)),
        Err(NotPrivate::Mode(mode)) => match owned_mode(how, mode) {
            Some(loose) => Ok((file.metadata().map_err(PrivateFileError::Io)?, loose)),
            None if how == Opening::Owned => Err(PrivateFileError::NotPrivate(FILE_WRITABLE)),
            None => Err(PrivateFileError::NotPrivate(FILE_OPEN)),
        },
    }
}

/// Reads `name` inside private directory `dir` up to `max` bytes.
/// # Errors
/// Returns [`PrivateFileError`] if `dir` or `name` is missing, symlinked, not private, or too big.
pub fn read_private(dir: &Path, name: &str, max: usize) -> Result<Vec<u8>, PrivateFileError> {
    let (file, _meta) = open_private(dir, name)?;
    let mut bytes = Vec::with_capacity(max.min(64 * 1024) + 1);
    file.take((max as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(PrivateFileError::Io)?;
    if bytes.len() > max {
        return Err(PrivateFileError::Malformed(
            "longer than such a file can be",
        ));
    }
    Ok(bytes)
}

const ONE_COMPONENT: &str = "a private file's name is one path component";

fn one_component(name: &str) -> bool {
    !(name.is_empty() || name == "." || name == ".." || name.contains('/'))
}

fn one_component_io(name: &str) -> io::Result<()> {
    if one_component(name) {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::InvalidInput, ONE_COMPONENT))
    }
}

enum DirIs {
    /// There is none.
    Missing,
    /// This user's, no access-list grant, no group or other mode bit.
    Private,
    /// This user's and no access-list grant, but these mode bits (`& 0o777`) open
    /// it to others: narrowing them makes it private.
    Open(u32),
    /// A symlink, which the rule never follows.
    Symlink,
    /// Something other than a directory.
    NotDir,
    /// Another user's.
    Others,
    /// An access-list entry allows someone something.
    Acl,
}

fn dir_is(dir: &Path) -> io::Result<DirIs> {
    use astream_sys::NotPrivate;
    use std::os::unix::fs::MetadataExt as _;
    let opened = match astream_sys::open_nonblocking(dir, astream_sys::Links::Refuse) {
        Ok(opened) => opened,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(DirIs::Missing),
        Err(e) => {
            return match std::fs::symlink_metadata(dir) {
                Ok(meta) if meta.file_type().is_symlink() => Ok(DirIs::Symlink),
                Ok(meta) if !meta.is_dir() => Ok(DirIs::NotDir),
                Ok(meta) if meta.uid() != crate::peer::our_uid() => Ok(DirIs::Others),
                _ => Err(e),
            };
        }
    };
    Ok(match astream_sys::owner_private_dir(&opened)? {
        Ok(_meta) => DirIs::Private,
        Err(NotPrivate::Mode(mode)) => DirIs::Open(mode),
        Err(NotPrivate::Kind(_)) => DirIs::NotDir,
        Err(NotPrivate::Owner(_)) => DirIs::Others,
        Err(NotPrivate::Acl) => DirIs::Acl,
    })
}

/// Opens `name` in `dir` under [`read_private`] rules and returns the file and metadata.
/// # Errors
/// Returns [`PrivateFileError`] if `dir` or `name` is missing, symlinked, or not owner-private.
pub fn open_private(
    dir: &Path,
    name: &str,
) -> Result<(std::fs::File, std::fs::Metadata), PrivateFileError> {
    open_judged(dir, name, Opening::Read).map(|(file, meta, _loose)| (file, meta))
}

/// `name` directly in `dir`, opened as `how` says and judged on the open descriptor
/// (astream-sys's owner-only rule, never a second `stat` by path), with whether a group or
/// other read bit was set on the file or its directory (only [`Opening::Owned`] lets one
/// through).
fn open_judged(
    dir: &Path,
    name: &str,
    how: Opening,
) -> Result<(std::fs::File, std::fs::Metadata, bool), PrivateFileError> {
    use std::os::unix::fs::MetadataExt as _;
    if !one_component(name) {
        return Err(PrivateFileError::Malformed(ONE_COMPONENT));
    }
    let judged = dir_is(dir).map_err(PrivateFileError::Io)?;
    match judged {
        DirIs::Missing => return Err(PrivateFileError::Missing),
        DirIs::Symlink => return Err(PrivateFileError::Symlink),
        DirIs::NotDir => {
            return Err(PrivateFileError::Malformed("the directory is no directory"));
        }
        _ => {}
    }
    match std::fs::symlink_metadata(dir.join(name)) {
        // A missing file is created by a read-write opening, judged like any other.
        Err(e) if e.kind() == io::ErrorKind::NotFound && how != Opening::ReadWrite => {
            return Err(PrivateFileError::Missing);
        }
        Err(e)
            if e.kind() != io::ErrorKind::PermissionDenied
                && e.kind() != io::ErrorKind::NotFound =>
        {
            return Err(PrivateFileError::Io(e));
        }
        _ => {}
    }
    let dir_loose = match judged {
        DirIs::Others => {
            return Err(PrivateFileError::NotPrivate(
                "the directory is another user's",
            ));
        }
        DirIs::Acl => return Err(PrivateFileError::NotPrivate(ACL_OPEN)),
        DirIs::Open(mode) => match owned_mode(how, mode) {
            Some(loose) => loose,
            None if how == Opening::Owned => {
                return Err(PrivateFileError::NotPrivate(DIR_WRITABLE));
            }
            None => return Err(PrivateFileError::NotPrivate(DIR_OPEN)),
        },
        _ => false,
    };
    let opened = match how {
        Opening::Read | Opening::Owned => {
            astream_sys::open_nonblocking(&dir.join(name), astream_sys::Links::Refuse)
        }
        Opening::ReadWrite => open_rw_created_0600(&dir.join(name), libc::O_NOFOLLOW),
    };
    let file = match opened {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(PrivateFileError::Missing),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(PrivateFileError::Symlink);
        }
        Err(e) => return Err(PrivateFileError::Io(e)),
    };
    let (meta, file_loose) = file_verdict(&file, how)?;
    if meta.nlink() != 1 {
        return Err(PrivateFileError::NotPrivate(ANOTHER_LINK));
    }
    Ok((file, meta, dir_loose || file_loose))
}

/// `path` opened read and write with the extra open `flags`, created when missing and then
/// made exactly 0600. `create` with a mode is masked by the umask, so the creating open
/// (`O_EXCL`, so it is this open that made the file) says the mode again on the
/// DESCRIPTOR: under a umask that clears an owner bit the file would otherwise come out
/// 0400, and the next read-write open of it would be refused. An existing file is opened
/// as it stands, and its mode is never changed. `O_EXCL` never follows a link, so a
/// symlink at `path` (a dangling one included) falls to the plain open, where
/// `O_NOFOLLOW` in `flags` refuses it.
fn open_rw_created_0600(path: &Path, flags: libc::c_int) -> io::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let mut existing = std::fs::OpenOptions::new();
    existing.read(true).write(true).custom_flags(flags);
    let mut creating = existing.clone();
    creating.create_new(true).mode(0o600);
    match creating.open(path) {
        Ok(file) => {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            Ok(file)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => existing.open(path),
        Err(e) => Err(e),
    }
}

/// `<dir>/<name>` opened read and write, created 0600 when missing, under [`read_private`]'s
/// rules: `dir` the user's own 0700 directory, the file the user's own regular file with no
/// second link, no symlink anywhere and no group or other bit. The cursor is at 0. The write
/// journal's file (agh, design §6.8): scanned, truncated past a torn tail, appended, and made
/// durable through the one descriptor.
///
/// # Errors
///
/// As [`read_private`]; a missing file is created exactly 0600 whatever the umask, not
/// `Missing` (that only for one removed between the creating open and the plain one).
pub fn open_private_rw(dir: &Path, name: &str) -> Result<std::fs::File, PrivateFileError> {
    open_judged(dir, name, Opening::ReadWrite).map(|(file, _meta, _loose)| file)
}

/// Byte count and `(dev, ino)` identity read by [`read_private_into`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrivateRead {
    /// Number of bytes read into the caller's buffer.
    pub len: usize,
    /// Device ID from `fstat`.
    pub dev: u64,
    /// Inode number from `fstat`.
    pub ino: u64,
}

/// Reads `name` in `dir` into `buf` (up to `max` bytes) and returns [`PrivateRead`].
/// # Errors
/// Returns [`PrivateFileError`] if privacy checks fail or the file exceeds `max`/`buf.len()`.
pub fn read_private_into(
    dir: &Path,
    name: &str,
    max: usize,
    buf: &mut [u8],
) -> Result<PrivateRead, PrivateFileError> {
    use std::os::unix::fs::MetadataExt as _;
    let (mut file, meta) = open_private(dir, name)?;
    let len = read_capped_into(&mut file, max, buf)?;
    Ok(PrivateRead {
        len,
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// What [`read_owned_into`] read: the length, the file's identity, and whether the file or
/// its directory had a group or other read bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnedRead {
    /// Bytes read into the caller's buffer.
    pub len: usize,
    /// The file's device.
    pub dev: u64,
    /// The file's inode.
    pub ino: u64,
    /// The file or its directory is group- or world-readable.
    pub loose: bool,
}

/// [`read_private_into`]'s rules, loosened for one legacy case only: the file and its
/// directory may carry group and other **read** bits (`loose` says so). Still refused:
/// - a symlink, file or directory;
/// - anything but a regular file;
/// - another user's file or directory;
/// - an access list that grants other users access;
/// - a second link;
/// - a group or other **write** bit on either.
///
/// agh's `legacy_loose` profiles alone call it (design §6.6, M2 only).
///
/// # Errors
///
/// As [`read_private_into`].
pub fn read_owned_into(
    dir: &Path,
    name: &str,
    max: usize,
    buf: &mut [u8],
) -> Result<OwnedRead, PrivateFileError> {
    use std::os::unix::fs::MetadataExt as _;
    let (mut file, meta, loose) = open_judged(dir, name, Opening::Owned)?;
    let len = read_capped_into(&mut file, max, buf)?;
    Ok(OwnedRead {
        len,
        dev: meta.dev(),
        ino: meta.ino(),
        loose,
    })
}

/// The read [`read_private_into`] and [`read_owned_into`] share: at most `min(max,
/// buf.len())` bytes into `buf`, a file past that cap `Malformed`, and on `Err` none of the
/// file left in `buf`.
fn read_capped_into(
    file: &mut std::fs::File,
    max: usize,
    buf: &mut [u8],
) -> Result<usize, PrivateFileError> {
    let cap = max.min(buf.len());
    let len = match read_full(file, &mut buf[..cap]) {
        Ok(len) => len,
        Err(e) => {
            crate::secret::scrub(&mut buf[..cap]);
            return Err(PrivateFileError::Io(e));
        }
    };
    if len == cap {
        let mut more = [0u8; 1];
        let past = read_full(file, &mut more);
        crate::secret::scrub(&mut more);
        let refused = match past {
            Ok(0) => None,
            Ok(_) => Some(PrivateFileError::Malformed(
                "longer than such a file can be",
            )),
            Err(e) => Some(PrivateFileError::Io(e)),
        };
        if let Some(refused) = refused {
            crate::secret::scrub(&mut buf[..cap]);
            return Err(refused);
        }
    }
    Ok(len)
}

fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Current state of a directory under [`ensure_private_dir`] rules ([`judge_private_dir`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateDir {
    /// Directory does not exist (`ensure_private_dir` would create it with mode `0700`).
    Missing,
    /// Directory is already owner-private with mode `0700` and no ACL grants.
    Private,
    /// Directory is owned by this user with no ACL grants, but has group/other mode bits set.
    Open(u32),
}

fn dir_refused(dir: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("the directory {} {why}", dir.display()),
    )
}

fn dir_acl_refused(dir: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "the directory {}: {}",
            dir.display(),
            ACL_OPEN.replace("`chmod -N`", &format!("`chmod -N {}`", dir.display()))
        ),
    )
}

/// Inspects `dir` under [`ensure_private_dir`] rules without modifying it.
/// # Errors
/// Returns an error if `dir` is a symlink, not a directory, not owned, or has ACL grants.
pub fn judge_private_dir(dir: &Path) -> io::Result<PrivateDir> {
    match dir_is(dir)? {
        DirIs::Missing => Ok(PrivateDir::Missing),
        DirIs::Private => Ok(PrivateDir::Private),
        DirIs::Open(mode) => Ok(PrivateDir::Open(mode)),
        DirIs::Symlink => Err(dir_refused(
            dir,
            "is a symlink; it must be a directory of its own",
        )),
        DirIs::NotDir => Err(dir_refused(dir, "is no directory")),
        DirIs::Others => Err(dir_refused(dir, "is another user's")),
        DirIs::Acl => Err(dir_acl_refused(dir)),
    }
}

/// Ensures `dir` exists as a mode-`0700` private directory, returning prior mode bits if narrowed.
/// # Errors
/// Returns an error naming `dir` if it cannot be created or fails owner/symlink/ACL checks, or is not private once made or narrowed (a volume that ignores modes; `NotFound` where it is gone by then).
pub fn ensure_private_dir(dir: &Path) -> io::Result<Option<u32>> {
    ensure_private_dir_by(dir, make_0700, narrow_0700)
}

fn make_0700(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

fn narrow_0700(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// [`ensure_private_dir`], with `make` making `dir` and `narrow` making it 0700.
fn ensure_private_dir_by(
    dir: &Path,
    make: impl FnOnce(&Path) -> io::Result<()>,
    narrow: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<Option<u32>> {
    let named = |e: io::Error| match e.raw_os_error() {
        Some(_) => io::Error::new(e.kind(), format!("the directory {}: {e}", dir.display())),
        None => e,
    };
    let judge = || judge_private_dir(dir).map_err(named);
    let removed = || {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the directory {} was removed as it was made 0700",
                dir.display()
            ),
        )
    };
    let was = match judge()? {
        PrivateDir::Private => return Ok(None),
        PrivateDir::Missing => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!(
                            "the directory {}: {} could not be made: {e}",
                            dir.display(),
                            parent.display()
                        ),
                    )
                })?;
            }
            match make(dir) {
                Ok(()) => None,
                // Another writer made it since the judge: judged again before a chmod, which follows a symlink.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match judge()? {
                    PrivateDir::Private => return Ok(None),
                    PrivateDir::Missing => return Err(removed()),
                    PrivateDir::Open(mode) => Some(mode),
                },
                Err(e) => return Err(named(e)),
            }
        }
        PrivateDir::Open(mode) => Some(mode),
    };
    narrow(dir).map_err(named)?;
    match judge()? {
        PrivateDir::Private => Ok(was),
        PrivateDir::Missing => Err(removed()),
        PrivateDir::Open(_) => Err(dir_refused(
            dir,
            "is not 0700 and this user's after it was made so",
        )),
    }
}

/// Verifies that `dir` already exists as an owner-private `0700` directory with no ACL grants.
/// # Errors
/// Returns an error if `dir` is missing, loose, symlinked, not owned, or has ACL grants.
pub fn check_private_dir(dir: &Path) -> io::Result<()> {
    let refused = |why: &dyn fmt::Display| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the directory {}: {why}", dir.display()),
        )
    };
    match dir_is(dir)? {
        DirIs::Private => Ok(()),
        DirIs::Missing => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the directory {}: there is no such directory",
                dir.display()
            ),
        )),
        DirIs::Symlink => Err(refused(&PrivateFileError::Symlink)),
        DirIs::NotDir => Err(refused(&"no directory")),
        DirIs::Others => Err(refused(&"it is another user's")),
        DirIs::Acl => Err(refused(&ACL_OPEN)),
        DirIs::Open(_) => Err(refused(&"it is open to other users (want 0700)")),
    }
}

/// Atomically writes `bytes` to `name` in private directory `dir` with mode `0600`.
/// # Errors
/// Returns an error if `name` spans multiple components, `dir` is not private, or write fails.
pub fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    one_component_io(name)?;
    check_private_dir(dir)?;
    crate::atomic_file::write(&dir.join(name), bytes, 0o600)
}

/// Removes single-component file `name` from `dir`, returning `Ok(true)` if it existed (none does where `dir` is no directory).
/// # Errors
/// Returns an error if `name` spans multiple components or unlinking fails.
pub fn remove_private(dir: &Path, name: &str) -> io::Result<bool> {
    one_component_io(name)?;
    match std::fs::remove_file(dir.join(name)) {
        Ok(()) => Ok(true),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Creates `dir` with mode `0700` or verifies that an existing `dir` passes [`check_private_dir`].
/// # Errors
/// Returns an error if creation or [`check_private_dir`] fails.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            match std::fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    check_private_dir(dir)
}

/// An exclusive lock on `<dir>/<name>`, lifted from aterm-net's `fleet_member` (`BookLock`).
///
/// - **How it is opened.** `name` must be one path component, and `dir` must pass
///   [`check_private_dir`]. The file is opened read-write, created exactly 0600 whatever the
///   umask, with `O_NOFOLLOW | O_NONBLOCK`, so it is never a link and the open never waits
///   on a FIFO. Anything but a regular file is refused.
/// - **How it is taken.** An exclusive `flock`, tried again with a backoff that starts at
///   1 ms and doubles to 50 ms until `within` has passed.
/// - **How it is released.** `LOCK_UN` in `Drop`, before the close. That is `clippy.toml`'s
///   flock release rule: a lock let go by dropping its `File` stays held in every child
///   forked meanwhile.
pub struct PrivateLock(std::fs::File);

impl PrivateLock {
    /// Take the lock on `<dir>/<name>`, waiting at most `within`.
    ///
    /// # Errors
    ///
    /// - `ErrorKind::InvalidInput` when `name` is not one path component (empty, `.`, `..`
    ///   or a `/` in it), before `dir` is judged or anything is opened (only `dir` is judged,
    ///   so a deeper name would escape the rules);
    /// - `ErrorKind::WouldBlock` with the text `busy` when another holder kept it for `within`
    ///   (a `within` past what a clock can represent waits with no deadline);
    /// - `ErrorKind::InvalidData`, `<dir>/<name> is no regular file`;
    /// - [`check_private_dir`]'s refusal, which names `dir`;
    /// - the open's own error.
    pub fn take(
        dir: &Path,
        name: &str,
        within: Duration,
        busy: &'static str,
    ) -> io::Result<PrivateLock> {
        Self::take_with(dir, name, within, busy, || {})
    }

    /// [`take`](Self::take), with a `busy` text made at run time (one that names `dir`, say)
    /// and `held` called each time the lock is found held, before the backoff's sleep (a
    /// test's hook to see a writer wait).
    ///
    /// # Errors
    ///
    /// As [`take`](Self::take).
    pub fn take_with(
        dir: &Path,
        name: &str,
        within: Duration,
        busy: &str,
        mut held: impl FnMut(),
    ) -> io::Result<PrivateLock> {
        one_component_io(name)?;
        check_private_dir(dir)?;
        let path = dir.join(name);
        let file = open_rw_created_0600(&path, libc::O_NOFOLLOW | libc::O_NONBLOCK)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is no regular file", path.display()),
            ));
        }
        // `None`: a wait no clock can represent, which has no deadline.
        let deadline = Instant::now().checked_add(within);
        let mut backoff = Duration::from_millis(1);
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
                Err(std::fs::TryLockError::WouldBlock)
                    if deadline.is_none_or(|d| Instant::now() < d) =>
                {
                    held();
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(50));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(io::ErrorKind::WouldBlock, busy.to_owned()));
                }
            }
        }
    }
}

impl Drop for PrivateLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};

    use super::{
        OwnedRead, PrivateDir, PrivateFileError, PrivateLock, PrivateRead, create_private_dir,
        ensure_private_dir, judge_private_dir, open_private_rw, read_owned_into, read_private,
        read_private_into, remove_private, write_private,
    };

    /// The busy text the lock tests hand [`PrivateLock::take`].
    const BUSY: &str = "another holder has it";

    /// With one lock held, a second take that does not wait is `WouldBlock` with the busy
    /// text; once the first is dropped, a second take succeeds.
    #[test]
    fn private_lock_excludes_a_second_holder() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        let held = PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY).unwrap();
        let refused = PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY)
            .err()
            .expect("a second holder is refused");
        assert_eq!(refused.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(refused.to_string(), BUSY);
        drop(held);
        let _again = PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY).unwrap();
    }

    /// A symlink at the lock's name is refused, and so is a FIFO, whose open returns at
    /// once (`O_NONBLOCK`).
    #[test]
    fn private_lock_refuses_a_link_and_a_fifo() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        file_in(&dir, "real", b"", 0o600);
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link.lock")).unwrap();
        assert!(PrivateLock::take(&dir, "link.lock", std::time::Duration::ZERO, BUSY).is_err());
        // A dangling link is refused too, and its target is never created.
        let outside = root.path().join("outside.lock");
        std::os::unix::fs::symlink(&outside, dir.join("dangling.lock")).unwrap();
        assert!(PrivateLock::take(&dir, "dangling.lock", std::time::Duration::ZERO, BUSY).is_err());
        assert!(!outside.exists());
        let fifo = dir.join("fifo.lock");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0, "mkfifo");
        let refused = PrivateLock::take(&dir, "fifo.lock", std::time::Duration::ZERO, BUSY)
            .err()
            .expect("a FIFO is no lock");
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidData);
    }

    /// A name of more than one path component is `InvalidInput` before anything is opened:
    /// no lock file is made below `dir`, beside it, or at an absolute path.
    #[test]
    fn private_lock_refuses_a_name_of_more_than_one_component() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        let absolute = root.path().join("absolute.lock");
        let absolute_name = absolute.to_str().unwrap();
        for name in ["sub/x.lock", "../beside.lock", "..", ".", "", absolute_name] {
            assert_eq!(
                PrivateLock::take(&dir, name, std::time::Duration::ZERO, BUSY)
                    .err()
                    .expect("refused")
                    .kind(),
                std::io::ErrorKind::InvalidInput,
                "{name:?}"
            );
        }
        assert!(!dir.join("sub").exists());
        assert!(!root.path().join("beside.lock").exists());
        assert!(!absolute.exists());
    }

    /// `take_with` tells its hook each time it finds the lock held, and takes it once the
    /// holder lets go: here the hook itself drops the first holder. Its busy text is the
    /// caller's, made at run time.
    #[test]
    fn private_lock_take_with_tells_its_hook_and_takes_a_freed_lock() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        let mut first =
            Some(PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY).unwrap());
        let mut told = 0;
        let _second = PrivateLock::take_with(
            &dir,
            ".lock",
            std::time::Duration::from_secs(60),
            "unused",
            || {
                told += 1;
                drop(first.take());
            },
        )
        .unwrap();
        assert_eq!(told, 1);
        let busy = format!("busy in {}", dir.display());
        let refused =
            PrivateLock::take_with(&dir, ".lock", std::time::Duration::ZERO, &busy, || {})
                .err()
                .expect("held by the second");
        assert_eq!(refused.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(refused.to_string(), busy);
    }

    /// A free lock is taken whatever the wait, one no clock can represent included.
    #[test]
    fn private_lock_takes_a_free_lock_with_any_wait() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        let _held = PrivateLock::take(&dir, ".lock", std::time::Duration::MAX, BUSY).unwrap();
    }

    /// A missing file is created 0600; a 0644 file, or a file in a 0755 directory, is not
    /// private.
    #[test]
    fn open_private_rw_creates_0600_and_refuses_loose() {
        use std::io::{Seek as _, Write as _};
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        let mut file = open_private_rw(&dir, "journal").unwrap();
        assert_eq!(mode_of(&dir.join("journal")), 0o600);
        assert_eq!(file.stream_position().unwrap(), 0);
        file.write_all(b"step").unwrap();
        drop(file);
        let again = open_private_rw(&dir, "journal").unwrap();
        assert_eq!(again.metadata().unwrap().len(), 4);
        file_in(&dir, "loose", b"x", 0o644);
        assert!(matches!(
            open_private_rw(&dir, "loose"),
            Err(PrivateFileError::NotPrivate(_))
        ));
        let open = dir_in(root.path(), "open", 0o755);
        assert!(matches!(
            open_private_rw(&open, "journal"),
            Err(PrivateFileError::NotPrivate(_))
        ));
        assert!(!open.join("journal").exists());
        // An existing private file in a directory loosened later is refused too.
        file_in(&open, "existing", b"x", 0o600);
        assert!(matches!(
            open_private_rw(&open, "existing"),
            Err(PrivateFileError::NotPrivate(_))
        ));
        // A link in the file's place, a dangling one included, is refused, and the
        // creating open never makes its target.
        let outside = root.path().join("outside");
        std::os::unix::fs::symlink(&outside, dir.join("linked")).unwrap();
        assert!(matches!(
            open_private_rw(&dir, "linked"),
            Err(PrivateFileError::Symlink)
        ));
        assert!(!outside.exists());
    }

    /// Under a umask that clears an owner bit (`0o277`), the journal file and the lock file
    /// are still made exactly 0600, so a second open of each, read-write, succeeds; an
    /// existing file's mode is left as it was. The umask is process-wide, so the body runs
    /// alone in a child of this test binary, which is waited on by a hang detector.
    #[test]
    fn files_made_under_a_narrow_umask_are_exactly_0600() {
        const NAME: &str = "private_file::tests::files_made_under_a_narrow_umask_are_exactly_0600";
        const STAGE_ENV: &str = "ATERM_UDS_TEST_NARROW_UMASK";
        if std::env::var_os(STAGE_ENV).is_some() {
            // The directory first: under this umask a new one would be 0500.
            let root = aterm_tempfile::tempdir().unwrap();
            let dir = dir_in(root.path(), "state", 0o700);
            // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail;
            // this child runs this one test, so no other create sees the mask.
            unsafe { libc::umask(0o277) };
            drop(open_private_rw(&dir, "journal").unwrap());
            assert_eq!(mode_of(&dir.join("journal")), 0o600);
            drop(open_private_rw(&dir, "journal").expect("the second read-write open"));
            drop(PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY).unwrap());
            assert_eq!(mode_of(&dir.join(".lock")), 0o600);
            drop(
                PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY)
                    .expect("the second take"),
            );
            // An existing file is opened as it stands: never chmodded.
            file_in(&dir, "narrow", b"x", 0o400);
            assert!(open_private_rw(&dir, "narrow").is_err());
            assert_eq!(mode_of(&dir.join("narrow")), 0o400);
            println!("STAGED narrow umask ok");
            return;
        }
        let out = crate::deadline::output_within(
            crate::spawnfd::command(std::env::current_exe().expect("the test binary"))
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(STAGE_ENV, "1"),
            std::time::Duration::from_secs(60),
        )
        .expect("the child ran within the hang detector");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("STAGED narrow umask ok"),
            "{:?}\nstdout: {stdout}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Group and other read bits read, and say so; a write bit on the file or the
    /// directory, or a symlink, is refused.
    #[test]
    fn read_owned_into_accepts_read_bits_only() {
        use std::os::unix::fs::MetadataExt as _;
        let root = aterm_tempfile::tempdir().unwrap();
        let mut buf = [0u8; 64];
        let shared = dir_in(root.path(), "shared", 0o755);
        file_in(&shared, "token", b"legacy", 0o644);
        let meta = std::fs::metadata(shared.join("token")).unwrap();
        assert_eq!(
            read_owned_into(&shared, "token", 64, &mut buf).unwrap(),
            OwnedRead {
                len: 6,
                dev: meta.dev(),
                ino: meta.ino(),
                loose: true,
            }
        );
        assert_eq!(&buf[..6], b"legacy");
        let private = dir_in(root.path(), "private", 0o700);
        file_in(&private, "token", b"tight", 0o600);
        let read = read_owned_into(&private, "token", 64, &mut buf).unwrap();
        assert_eq!((read.len, read.loose), (5, false));
        // A search bit lets nobody read: neither refused nor loose. A group read bit is.
        let searchable = dir_in(root.path(), "searchable", 0o711);
        file_in(&searchable, "token", b"tight", 0o600);
        let read = read_owned_into(&searchable, "token", 64, &mut buf).unwrap();
        assert_eq!((read.len, read.loose), (5, false));
        file_in(&private, "group-read", b"group", 0o640);
        let read = read_owned_into(&private, "group-read", 64, &mut buf).unwrap();
        assert_eq!((read.len, read.loose), (5, true));
        file_in(&shared, "group-write", b"x", 0o664);
        assert!(matches!(
            read_owned_into(&shared, "group-write", 64, &mut buf),
            Err(PrivateFileError::NotPrivate(_))
        ));
        let world = dir_in(root.path(), "world", 0o700);
        file_in(&world, "token", b"x", 0o600);
        std::fs::set_permissions(&world, std::fs::Permissions::from_mode(0o757)).unwrap();
        assert!(matches!(
            read_owned_into(&world, "token", 64, &mut buf),
            Err(PrivateFileError::NotPrivate(_))
        ));
        std::os::unix::fs::symlink(private.join("token"), shared.join("link")).unwrap();
        assert!(matches!(
            read_owned_into(&shared, "link", 64, &mut buf),
            Err(PrivateFileError::Symlink)
        ));
        // astream-sys's rule judges an access list before the mode: a loose file that one
        // opens to others is refused for the list, not read as loose.
        #[cfg(target_os = "macos")]
        {
            let token = shared.join("token");
            chmod(&[
                "+a".as_ref(),
                "everyone allow write".as_ref(),
                token.as_os_str(),
            ]);
            match read_owned_into(&shared, "token", 64, &mut buf) {
                Err(PrivateFileError::NotPrivate(why)) => {
                    assert!(why.contains("chmod -N"), "{why}");
                }
                other => panic!("an access list that allows others: {other:?}"),
            }
        }
    }

    fn dir_in(root: &Path, name: &str, mode: u32) -> PathBuf {
        let dir = root.join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        dir
    }

    fn file_in(dir: &Path, name: &str, bytes: &[u8], mode: u32) {
        use std::io::Write as _;
        let path = dir.join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn a_private_file_reads_into_the_buffer_with_its_identity() {
        use std::os::unix::fs::MetadataExt as _;
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        file_in(&dir, "token", b"eleven byte", 0o600);
        let mut buf = [0u8; 64];
        let read = read_private_into(&dir, "token", 32, &mut buf).unwrap();
        let meta = std::fs::metadata(dir.join("token")).unwrap();
        assert_eq!(
            read,
            PrivateRead {
                len: 11,
                dev: meta.dev(),
                ino: meta.ino(),
            }
        );
        assert_eq!(&buf[..read.len], b"eleven byte");
    }

    #[test]
    fn a_file_past_the_cap_is_malformed() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        file_in(&dir, "token", &[7; 9], 0o600);
        let mut buf = [0u8; 64];
        assert!(matches!(
            read_private_into(&dir, "token", 8, &mut buf),
            Err(PrivateFileError::Malformed(
                "longer than such a file can be"
            ))
        ));
        assert!(buf.iter().all(|&b| b == 0), "{buf:?}");
        let mut short = [0u8; 8];
        assert!(matches!(
            read_private_into(&dir, "token", 64, &mut short),
            Err(PrivateFileError::Malformed(
                "longer than such a file can be"
            ))
        ));
        assert_eq!(short, [0; 8]);
        assert_eq!(
            read_private_into(&dir, "token", 9, &mut buf).unwrap().len,
            9
        );
    }

    #[test]
    fn a_loose_file_or_directory_is_not_private() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut buf = [0u8; 64];
        let dir = dir_in(root.path(), "state", 0o700);
        file_in(&dir, "token", b"x", 0o644);
        assert!(matches!(
            read_private_into(&dir, "token", 64, &mut buf),
            Err(PrivateFileError::NotPrivate(_))
        ));
        let loose = dir_in(root.path(), "loose", 0o755);
        file_in(&loose, "token", b"x", 0o600);
        assert!(matches!(
            read_private_into(&loose, "token", 64, &mut buf),
            Err(PrivateFileError::NotPrivate(_))
        ));
    }

    #[test]
    fn a_name_of_more_than_one_component_is_malformed() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        let sub = dir_in(&dir, "sub", 0o700);
        file_in(&sub, "token", b"x", 0o600);
        let mut buf = [0u8; 64];
        for name in ["sub/token", "../state/sub/token", "..", ".", ""] {
            assert!(
                matches!(
                    read_private_into(&dir, name, 64, &mut buf),
                    Err(PrivateFileError::Malformed(_))
                ),
                "{name:?}"
            );
        }
        assert_eq!(
            read_private_into(&sub, "token", 64, &mut buf).unwrap().len,
            1
        );
    }

    #[test]
    fn a_name_of_more_than_one_component_is_neither_written_nor_removed() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        file_in(root.path(), "beside", b"x", 0o600);
        for name in ["sub/token", "../token", "..", ".", ""] {
            assert_eq!(
                write_private(&dir, name, b"t").unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput,
                "{name:?}"
            );
        }
        assert!(!dir.join("sub").exists());
        assert!(!root.path().join("token").exists());
        assert_eq!(
            remove_private(&dir, "../beside").unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(root.path().join("beside").exists());
        write_private(&dir, "token", b"t").unwrap();
        assert!(remove_private(&dir, "token").unwrap());
    }

    #[test]
    fn a_file_beneath_no_directory_was_never_there() {
        let root = aterm_tempfile::tempdir().unwrap();
        file_in(root.path(), "state", b"x", 0o600);
        let file = root.path().join("state");
        assert!(!remove_private(&file, "token").unwrap());
        assert!(!remove_private(&file.join("sub"), "token").unwrap());
        assert!(file.exists());
    }

    #[test]
    fn read_private_with_no_cap_reads_the_file() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        file_in(&dir, "token", b"whole", 0o600);
        assert_eq!(read_private(&dir, "token", usize::MAX).unwrap(), b"whole");
    }

    /// On macOS a new directory that inherits its parent's access list is no private one:
    /// `create_private_dir` refuses it, and accepts it once `chmod -N` removes the list.
    #[cfg(target_os = "macos")]
    #[test]
    fn create_private_dir_refuses_an_inherited_access_list() {
        let root = aterm_tempfile::tempdir().unwrap();
        let parent = dir_in(root.path(), "parent", 0o700);
        let child = parent.join("child");
        chmod(&[
            "+a".as_ref(),
            "everyone allow list,search,directory_inherit".as_ref(),
            parent.as_os_str(),
        ]);
        assert_eq!(
            create_private_dir(&child).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        chmod(&["-N".as_ref(), parent.as_os_str(), child.as_os_str()]);
        create_private_dir(&child).unwrap();
        assert_eq!(mode_of(&child), 0o700);
    }

    /// `chmod <args>`, which must succeed.
    #[cfg(target_os = "macos")]
    fn chmod(args: &[&std::ffi::OsStr]) {
        let ran = crate::spawnfd::command("/bin/chmod")
            .args(args)
            .status()
            .expect("run chmod");
        assert!(ran.success(), "chmod {args:?}");
    }

    /// On macOS an access list that allows anyone anything makes a 0700 directory no
    /// private one: `judge_private_dir` and `ensure_private_dir` refuse it, naming
    /// `chmod -N`, and leave it as it was.
    #[cfg(target_os = "macos")]
    #[test]
    fn ensure_private_dir_refuses_an_access_list_that_allows_others() {
        let root = aterm_tempfile::tempdir().unwrap();
        let granted = dir_in(root.path(), "granted", 0o700);
        chmod(&[
            "+a".as_ref(),
            "everyone allow read".as_ref(),
            granted.as_os_str(),
        ]);
        for refusal in [
            judge_private_dir(&granted).unwrap_err(),
            ensure_private_dir(&granted).unwrap_err(),
        ] {
            assert_eq!(refusal.kind(), std::io::ErrorKind::PermissionDenied);
            assert!(refusal.raw_os_error().is_none(), "{refusal}");
            assert!(refusal.to_string().contains("chmod -N"), "{refusal}");
        }
        assert_eq!(mode_of(&granted), 0o700);

        // Open by its mode bits AND by an access list: refused for the list, its mode
        // left as it was. astream-sys judges the list before the mode. Were the mode
        // judged first, `judge_private_dir` would answer `Open(0o755)`, so
        // `aterm fabric on --dry-run` would promise only a `chmod 700`, and
        // `ensure_private_dir` would narrow the owner's directory to 0700 before its
        // second look found the list and refused it: the mode changed for nothing.
        // Both are caught here, by the `unwrap_err` and by the mode check below.
        let wide = dir_in(root.path(), "wide", 0o755);
        chmod(&[
            "+a".as_ref(),
            "everyone allow read".as_ref(),
            wide.as_os_str(),
        ]);
        for refusal in [
            judge_private_dir(&wide).unwrap_err(),
            ensure_private_dir(&wide).unwrap_err(),
        ] {
            assert_eq!(refusal.kind(), std::io::ErrorKind::PermissionDenied);
            assert!(refusal.to_string().contains("chmod -N"), "{refusal}");
        }
        assert_eq!(mode_of(&wide), 0o755, "a refused directory is not narrowed");

        let parent = dir_in(root.path(), "parent", 0o700);
        chmod(&[
            "+a".as_ref(),
            "everyone allow list,search,directory_inherit".as_ref(),
            parent.as_os_str(),
        ]);
        let child = parent.join("child");
        assert_eq!(
            ensure_private_dir(&child).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );

        let denied = dir_in(root.path(), "denied", 0o700);
        chmod(&[
            "+a".as_ref(),
            "everyone deny add_file".as_ref(),
            denied.as_os_str(),
        ]);
        assert_eq!(judge_private_dir(&denied).unwrap(), PrivateDir::Private);
        assert_eq!(ensure_private_dir(&denied).unwrap(), None);
    }

    #[test]
    fn judge_private_dir_says_what_ensure_would_do_and_changes_nothing() {
        let root = aterm_tempfile::tempdir().unwrap();
        let fresh = root.path().join("fresh");
        assert_eq!(judge_private_dir(&fresh).unwrap(), PrivateDir::Missing);
        assert!(!fresh.exists());
        assert_eq!(ensure_private_dir(&fresh).unwrap(), None);
        assert_eq!(judge_private_dir(&fresh).unwrap(), PrivateDir::Private);

        let open = dir_in(root.path(), "open", 0o755);
        assert_eq!(judge_private_dir(&open).unwrap(), PrivateDir::Open(0o755));
        assert_eq!(mode_of(&open), 0o755);
        assert_eq!(ensure_private_dir(&open).unwrap(), Some(0o755));
        assert_eq!(mode_of(&open), 0o700);

        let link = root.path().join("link");
        std::os::unix::fs::symlink(&fresh, &link).unwrap();
        let refusal = judge_private_dir(&link).unwrap_err();
        assert_eq!(refusal.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(refusal.raw_os_error().is_none(), "{refusal}");
    }

    #[test]
    fn ensure_private_dir_refuses_one_its_narrowing_left_open() {
        let root = aterm_tempfile::tempdir().unwrap();
        let open = dir_in(root.path(), "open", 0o755);
        // A volume that ignores modes: the chmod succeeds and changes nothing.
        let refusal =
            super::ensure_private_dir_by(&open, super::make_0700, |_| Ok(())).unwrap_err();
        assert_eq!(refusal.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(refusal.to_string().contains("is not 0700"), "{refusal}");
        assert_eq!(judge_private_dir(&open).unwrap(), PrivateDir::Open(0o755));
        assert_eq!(ensure_private_dir(&open).unwrap(), Some(0o755));
    }

    #[test]
    fn ensure_private_dir_finds_one_removed_as_it_was_made_private_gone() {
        let root = aterm_tempfile::tempdir().unwrap();
        let open = dir_in(root.path(), "open", 0o755);
        for dir in [open, root.path().join("missing")] {
            let gone = super::ensure_private_dir_by(&dir, super::make_0700, |dir| {
                std::fs::remove_dir(dir)
            })
            .unwrap_err();
            assert_eq!(gone.kind(), std::io::ErrorKind::NotFound, "{gone}");
            assert!(gone.to_string().contains("was removed"), "{gone}");
            assert_eq!(judge_private_dir(&dir).unwrap(), PrivateDir::Missing);
        }
    }

    #[test]
    fn ensure_private_dir_judges_one_another_writer_made_first() {
        let root = aterm_tempfile::tempdir().unwrap();
        for (name, mode, was) in [("private", 0o700, None), ("open", 0o755, Some(0o755))] {
            let dir = root.path().join(name);
            let made = super::ensure_private_dir_by(
                &dir,
                |dir| {
                    dir_in(root.path(), name, mode);
                    super::make_0700(dir)
                },
                super::narrow_0700,
            );
            assert_eq!(made.unwrap(), was, "{name}");
            assert_eq!(mode_of(&dir), 0o700, "{name}");
        }

        let gone = root.path().join("gone");
        let refusal = super::ensure_private_dir_by(
            &gone,
            |_| Err(std::io::ErrorKind::AlreadyExists.into()),
            super::narrow_0700,
        )
        .unwrap_err();
        assert_eq!(refusal.kind(), std::io::ErrorKind::NotFound, "{refusal}");
        assert!(refusal.to_string().contains("was removed"), "{refusal}");

        let target = dir_in(root.path(), "target", 0o755);
        let link = root.path().join("link");
        let refusal = super::ensure_private_dir_by(
            &link,
            |dir| {
                std::os::unix::fs::symlink(&target, dir).unwrap();
                super::make_0700(dir)
            },
            super::narrow_0700,
        )
        .unwrap_err();
        assert_eq!(
            refusal.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{refusal}"
        );
        assert!(refusal.to_string().contains("is a symlink"), "{refusal}");
        assert_eq!(mode_of(&target), 0o755);
    }

    #[test]
    fn ensure_private_dir_names_the_directory_once_in_a_bare_refusal() {
        use std::io::ErrorKind;
        let root = aterm_tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a-file"), b"x").unwrap();
        std::os::unix::fs::symlink(root.path().join("nowhere"), root.path().join("dangling"))
            .unwrap();
        let unwritable = dir_in(root.path(), "unwritable", 0o500);
        for (odd, kind) in [
            ("a-file/state", ErrorKind::NotADirectory),
            ("dangling/state", ErrorKind::AlreadyExists),
            ("unwritable/state", ErrorKind::PermissionDenied),
            // `.` hides `gone` from `Path::parent`: the make finds no parent.
            ("gone/.", ErrorKind::NotFound),
        ] {
            let dir = root.path().join(odd);
            let refusal = ensure_private_dir(&dir).unwrap_err();
            assert_eq!(refusal.kind(), kind, "{odd}: {refusal}");
            let shown = dir.display().to_string();
            assert_eq!(refusal.to_string().matches(&shown).count(), 1, "{refusal}");
        }
        std::fs::set_permissions(&unwritable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn a_fifo_or_an_access_list_in_the_files_place_is_refused() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        file_in(&dir, "token", b"x", 0o600);
        let made = crate::spawnfd::command("/usr/bin/mkfifo")
            .arg("-m")
            .arg("600")
            .arg(dir.join("fifo"))
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo");
        let (tx, rx) = std::sync::mpsc::channel();
        let at = dir.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_private(&at, "fifo", 64).map(|_| ()));
        });
        let refused = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("a FIFO is judged without waiting for a writer");
        assert!(
            matches!(
                refused,
                Err(PrivateFileError::Malformed("not a regular file"))
            ),
            "{refused:?}"
        );
        assert_eq!(read_private(&dir, "token", 64).unwrap(), b"x");

        #[cfg(target_os = "macos")]
        {
            let token = dir.join("token");
            chmod(&[
                "+a".as_ref(),
                "everyone allow read".as_ref(),
                token.as_os_str(),
            ]);
            match read_private(&dir, "token", 64) {
                Err(PrivateFileError::NotPrivate(why)) => {
                    assert!(why.contains("chmod -N"), "{why}")
                }
                other => panic!("an access list that allows others: {other:?}"),
            }
            chmod(&["-N".as_ref(), token.as_os_str()]);
            assert_eq!(read_private(&dir, "token", 64).unwrap(), b"x");
        }
    }

    #[test]
    fn a_symlink_in_the_files_place_is_refused() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        file_in(&dir, "real", b"x", 0o600);
        std::os::unix::fs::symlink(dir.join("real"), dir.join("token")).unwrap();
        let mut buf = [0u8; 64];
        assert!(matches!(
            read_private_into(&dir, "token", 64, &mut buf),
            Err(PrivateFileError::Symlink)
        ));
    }

    #[test]
    fn create_private_dir_makes_or_accepts_and_never_narrows() {
        let root = aterm_tempfile::tempdir().unwrap();
        let fresh = root.path().join("fresh");
        create_private_dir(&fresh).unwrap();
        assert!(std::fs::symlink_metadata(&fresh).unwrap().is_dir());
        assert_eq!(mode_of(&fresh), 0o700);
        create_private_dir(&fresh).unwrap();

        let loose = dir_in(root.path(), "loose", 0o755);
        assert!(create_private_dir(&loose).is_err());
        assert_eq!(mode_of(&loose), 0o755);

        let link = root.path().join("link");
        std::os::unix::fs::symlink(&fresh, &link).unwrap();
        assert!(create_private_dir(&link).is_err());

        let orphan = root.path().join("absent").join("child");
        assert_eq!(
            create_private_dir(&orphan).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
