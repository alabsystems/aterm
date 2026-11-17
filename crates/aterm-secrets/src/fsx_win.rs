// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! THE WINDOWS ARM OF [`super`]: the vault's looks, admitted reads and writes under an access list
//! instead of a mode.
//!
//! - A LOOK at a path ([`lstat`], what Unix's `lstat` is to the rest of `fsx`) opens the name
//!   itself (`FILE_FLAG_OPEN_REPARSE_POINT`, for its attributes and descriptor alone, sharing
//!   everything) and reads the handle ([`aterm_dirfd::fstat`]): the volume's serial number and the
//!   file's 128-bit id, its kind, length and link count, its last write and its change time
//!   ([`super::FileStamp`]). A reparse point (a symlink, a junction, an exposed cloud placeholder)
//!   is seen as itself, never followed.
//! - An admitted READ ([`open_admitted`]) opens the name as itself too and judges the open handle:
//!   a reparse point and a folder are refused; `Private` holds the file to the owner-only rule
//!   ([`aterm_uds::acl::owner_only`]: owned by this token's user under a protected list of the
//!   user and SYSTEM alone, W1's verdict) and to one link; `Owned` to an owner who is the user and
//!   an access list that lets no other account change it (an allow entry that may write, append,
//!   delete, or change the list or the owner names the user, SYSTEM or Administrators, which on
//!   Windows may take any file as root may write any on Unix); `AtermConfig` and `Any` to a
//!   regular file, as the window reads its settings on Windows (`AtermConfig` follows a link).
//! - A MODE maps to two classes: private (no group or other bit, so 0600, 0400, 0700) is
//!   owner-only from the file's creation, its temporary made with the owner-only descriptor
//!   ([`aterm_uds::private_file::new_file_descriptor`]) and never narrowed after; any other mode
//!   takes its folder's entries. An elevated process makes no private file (W1's `ELEVATED`).
//! - A name is taken without replacing ([`rename_noreplace`]) by one rename with POSIX semantics
//!   and no replace ([`aterm_dirfd::renameat_with`] `NOREPLACE`): the file system refuses a name
//!   that is taken in the same step, so a new file never has two links on the way.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aterm_uds::acl::{self, Descriptor, NotOwnerOnly, Parts, Sid, WellKnown};

use super::{Admit, Error, FileStamp, Kind, Links, Meta, Moved, Seen, Unread};

/// `READ_CONTROL`: read the object's descriptor.
const READ_CONTROL: u32 = 0x0002_0000;
/// `FILE_READ_ATTRIBUTES`.
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
/// `FILE_APPEND_DATA`: writes go to the end, whatever the file pointer.
const FILE_APPEND_DATA: u32 = 0x0000_0004;
/// `FILE_LIST_DIRECTORY | FILE_TRAVERSE`: a folder's handle to name entries relative to.
const FILE_LIST_AND_TRAVERSE: u32 = 0x0000_0001 | 0x0000_0020;
/// `SYNCHRONIZE`.
const SYNCHRONIZE: u32 = 0x0010_0000;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`: a writer may rename over, or remove,
/// a file this side holds open.
const SHARE_ALL: u32 = 0x0000_0007;
/// `ERROR_DELETE_PENDING`: a name whose file is being removed, which a look reads as gone.
const ERROR_DELETE_PENDING: i32 = 303;
/// `ERROR_PATH_NOT_FOUND`: a folder of the path is missing, or is a file.
const ERROR_PATH_NOT_FOUND: i32 = 3;
/// The FILETIME tick (100 ns since 1601) of the Unix epoch.
const FILETIME_UNIX_EPOCH: i128 = 116_444_736_000_000_000;

/// The rights an access-list entry may hold that let its account change a file: write or append
/// data, write its extended attributes or attributes, delete it, change its list or its owner,
/// remove a child (a folder's), and the generic and maximal rights that hold them.
const WRITE_RIGHTS: u32 = 0x0000_0002 // FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | 0x0000_0010 // FILE_WRITE_EA
    | 0x0000_0040 // FILE_DELETE_CHILD
    | 0x0000_0100 // FILE_WRITE_ATTRIBUTES
    | 0x0001_0000 // DELETE
    | 0x0004_0000 // WRITE_DAC
    | 0x0008_0000 // WRITE_OWNER
    | 0x0200_0000 // MAXIMUM_ALLOWED
    | 0x1000_0000 // GENERIC_ALL
    | 0x4000_0000; // GENERIC_WRITE
/// `INHERIT_ONLY_ACE`: an entry for what is made inside a folder, which grants the folder nothing.
const INHERIT_ONLY: u8 = 0x08;

/// The length a [`Meta`] gives, by the name a Unix `Metadata` gives it.
pub(crate) trait MetaLen {
    /// `EndOfFile`.
    fn len(&self) -> u64;
}

impl MetaLen for aterm_dirfd::Stat {
    fn len(&self) -> u64 {
        self.size
    }
}

/// Whether a mode is private: it grants the group and others nothing.
pub(crate) const fn private_mode(mode: u32) -> bool {
    mode & 0o077 == 0
}

/// This process token's user, the owner every file the vault keeps has: read once. In a test
/// within [`as_another_user`], SYSTEM, so every file this user made is another's to the checks.
fn user() -> io::Result<Sid> {
    #[cfg(test)]
    if ANOTHER_USER.with(std::cell::Cell::get) {
        return Sid::parse("S-1-5-18");
    }
    static USER: OnceLock<Sid> = OnceLock::new();
    if let Some(user) = USER.get() {
        return Ok(user.clone());
    }
    let user = Sid::token_user()?;
    Ok(USER.get_or_init(|| user).clone())
}

/// `path` opened as itself (a reparse point is opened, never followed), to read its attributes and,
/// with `access` [`READ_CONTROL`], its descriptor; a folder opens too.
fn open_look(path: &Path, access: u32) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(access | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .share_mode(SHARE_ALL)
        .custom_flags(acl::FILE_FLAG_OPEN_REPARSE_POINT | acl::FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// An open's error as a look reads it: a name being removed is gone.
fn gone_as_missing(error: io::Error) -> io::Error {
    if error.raw_os_error() == Some(ERROR_DELETE_PENDING) {
        io::Error::new(io::ErrorKind::NotFound, error)
    } else {
        error
    }
}

/// What the handle `file` is open on is ([`aterm_dirfd::fstat`]).
///
/// # Errors
/// The queries' error.
pub(crate) fn meta_of(file: &File) -> io::Result<Meta> {
    aterm_dirfd::fstat(file).map_err(io::Error::from)
}

/// The look at `path` ([`super::lstat_now`]'s on Windows): the name opened as itself and its
/// handle read. A path under a file is no missing path but an error (`NotADirectory`), as Unix's
/// `ENOTDIR` is: Windows answers it as it answers a missing folder (`ERROR_PATH_NOT_FOUND`), so
/// that answer is told apart by the nearest of its folders that is there.
///
/// # Errors
/// `NotFound` where nothing is there (a name being removed included); `NotADirectory` where a
/// folder of the path is a file; the open's or the query's error otherwise.
pub(crate) fn lstat(path: &Path) -> io::Result<Meta> {
    match open_look(path, 0) {
        Ok(file) => meta_of(&file),
        Err(error) if error.raw_os_error() == Some(ERROR_PATH_NOT_FOUND) => {
            Err(under_a_file(path).unwrap_or(error))
        }
        Err(error) => Err(gone_as_missing(error)),
    }
}

/// `NotADirectory` where the nearest folder of `path` that is there is no folder, `None` where it
/// is one (or none is there, or a look fails).
fn under_a_file(path: &Path) -> Option<io::Error> {
    for folder in path.ancestors().skip(1) {
        if folder.as_os_str().is_empty() {
            return None;
        }
        match open_look(folder, 0) {
            Ok(file) => {
                let meta = meta_of(&file).ok()?;
                return (!meta.file_type().is_dir()).then(|| {
                    io::Error::new(
                        io::ErrorKind::NotADirectory,
                        format!("{} is not a directory", folder.display()),
                    )
                });
            }
            Err(error) if error.raw_os_error() == Some(ERROR_PATH_NOT_FOUND) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Remove the link at `path` as itself ([`super::wipe_and_remove`]): a file's name, or a link to
/// a folder (a junction or a directory symlink, which only a folder's removal takes away).
///
/// # Errors
/// The removal's.
pub(crate) fn remove_link(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            let folder_link = lstat(path).is_ok_and(|meta| {
                meta.file_type().is_link() && meta.attributes & acl::FILE_ATTRIBUTE_DIRECTORY != 0
            });
            if folder_link {
                std::fs::remove_dir(path)
            } else {
                Err(error)
            }
        }
        removed => removed,
    }
}

/// The [`Kind`] `meta` shows: a link (a symlink or a junction) is [`Kind::Symlink`], any other
/// reparse point (a cloud placeholder) [`Kind::Other`].
pub(crate) const fn kind_of(meta: &Meta) -> Kind {
    let kind = meta.file_type();
    if kind.is_link() {
        Kind::Symlink
    } else if kind.is_reparse() {
        Kind::Other
    } else if kind.is_dir() {
        Kind::Dir
    } else {
        Kind::File
    }
}

/// The volume and file id `meta` shows.
pub(crate) const fn id_of(meta: &Meta) -> (u64, super::Ino) {
    (meta.st_dev, meta.st_ino)
}

/// Whether the file `meta` shows has lost its last name since it was opened (renamed over, or
/// removed): no link left, or a delete pending.
pub(crate) const fn gone(meta: &Meta) -> bool {
    meta.st_nlink == 0 || meta.delete_pending
}

/// A FILETIME tick count as nanoseconds since the Unix epoch.
fn unix_ns(ticks: i64) -> i128 {
    (i128::from(ticks) - FILETIME_UNIX_EPOCH) * 100
}

impl FileStamp {
    /// The stamp of the file `meta` shows: its volume serial and 128-bit file id, its length, its
    /// last write and its change time (which a write, an attribute change and an access-list
    /// change move). A rename moves nothing of it: NTFS leaves a file's change time as it was
    /// when the file is renamed (measured 2026-11-16: a file renamed aside while it was read kept
    /// it), and the stamp of a file another is renamed over as it was too, so a stamp alone never
    /// shows that a file left its path; a reader that must know it compares the path's file id
    /// too (`OwnedReader::check_unchanged`, ATERM-KI-1116-40951).
    pub(crate) fn of(meta: &Meta) -> Self {
        Self {
            dev: meta.st_dev,
            ino: meta.st_ino,
            len: meta.size,
            mtime_ns: unix_ns(meta.last_write),
            ctime_ns: unix_ns(meta.change_time),
        }
    }
}

/// What `meta` shows ([`Seen`]).
pub(crate) fn seen_of(meta: &Meta) -> Seen {
    Seen {
        kind: kind_of(meta),
        stamp: FileStamp::of(meta),
        nlink: meta.st_nlink,
        attributes: meta.attributes,
    }
}

/// Why the file `file` is open on is not the user's alone ([`acl::owner_only`] against [`user`]),
/// `None` where it is.
///
/// # Errors
/// The token's user or the descriptor could not be read.
fn not_owner_only(file: &File) -> io::Result<Option<NotOwnerOnly>> {
    Ok(acl::owner_only(file, &user()?)?.err())
}

/// Why the file `file` is open on is not one the user owns and nobody else may change, `None`
/// where it is ([`Admit::Owned`]).
///
/// # Errors
/// The token's user or the descriptor could not be read.
fn not_owned(file: &File) -> io::Result<Option<String>> {
    let user = user()?;
    let descriptor = Descriptor::of(file, Parts::OWNER | Parts::DACL)?;
    match descriptor.owner()? {
        Some(owner) if owner == user => {}
        Some(owner) => return Ok(Some(format!("owned by {owner}, not this user"))),
        None => return Ok(Some("its descriptor names no owner".to_owned())),
    }
    foreign_writer(&descriptor, &user)
}

/// Why `descriptor`'s access list lets an account other than `user`, SYSTEM or Administrators
/// change what it guards, `None` where it does not.
///
/// # Errors
/// The list could not be read.
fn foreign_writer(descriptor: &Descriptor, user: &Sid) -> io::Result<Option<String>> {
    let Some(entries) = descriptor.entries()? else {
        return Ok(Some(
            "it has no access list, which lets everyone change it".to_owned(),
        ));
    };
    for entry in entries {
        if entry.flags & INHERIT_ONLY != 0 || entry.kind == acl::ACCESS_DENIED {
            continue;
        }
        match (entry.kind, entry.sid) {
            (acl::ACCESS_ALLOWED, Some(sid)) => {
                let trusted = sid == *user
                    || sid.is(WellKnown::LocalSystem)
                    || sid.is(WellKnown::Administrators);
                if !trusted && entry.mask & WRITE_RIGHTS != 0 {
                    return Ok(Some(format!(
                        "its access list lets {sid} change it (rights {:#x})",
                        entry.mask
                    )));
                }
            }
            (kind, _) => {
                return Ok(Some(format!(
                    "its access list holds an entry of a kind this rule does not read (type {kind})"
                )));
            }
        }
    }
    Ok(None)
}

/// [`super::open_admitted`] on Windows: `path` opened to read as itself (`links` [`Links::Follow`]:
/// through a link), judged on the open handle under `admit`.
pub(crate) fn open_admitted(
    path: &Path,
    admit: Admit,
    links: Links,
) -> Result<Option<(File, Meta)>, Unread> {
    let refused = |why: String| Unread::Refused(Error::new(format!("{}: {why}", path.display())));
    let failed = |e: io::Error| Unread::Failed(Error::new(format!("{}: {e}", path.display())));
    super::look_afresh();
    #[cfg(test)]
    super::count_read(path);
    #[cfg(test)]
    if super::take_read_failure(path) {
        return Err(failed(io::Error::other("the test's disk failed the read")));
    }
    let no_follow = if links == Links::Refuse {
        acl::FILE_FLAG_OPEN_REPARSE_POINT
    } else {
        0
    };
    let file = match OpenOptions::new()
        .read(true)
        .share_mode(SHARE_ALL)
        .custom_flags(no_follow | acl::FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(gone_as_missing)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(failed(e)),
    };
    let meta = meta_of(&file).map_err(failed)?;
    if links == Links::Refuse && meta.file_type().is_reparse() {
        return Err(refused(
            "is a symlink, a junction or another reparse point; refused".to_owned(),
        ));
    }
    if !meta.file_type().is_file() {
        return Err(refused("not a regular file".to_owned()));
    }
    match admit {
        Admit::Any | Admit::AtermConfig => {}
        Admit::Private | Admit::Owned => {
            // A look at the access list that failed is a failed read, which a later look may
            // pass, never a refusal of the file as it is.
            #[cfg(test)]
            if super::take_access_list_failure(path) {
                return Err(failed(io::Error::from(io::ErrorKind::OutOfMemory)));
            }
            let why = if admit == Admit::Private {
                not_owner_only(&file).map(|broken| broken.map(|broken| broken.to_string()))
            } else {
                not_owned(&file)
            };
            if let Some(why) = why.map_err(failed)? {
                return Err(refused(why));
            }
            if admit == Admit::Private && meta.st_nlink > 1 {
                return Err(refused(format!(
                    "has {} hard links; a private file has one",
                    meta.st_nlink
                )));
            }
        }
    }
    Ok(Some((file, meta)))
}

/// [`super::has_private_nonempty`] on Windows: a plain file of at most `max` bytes, not empty,
/// owner-only and of one link, judged on the name opened as itself.
#[must_use]
pub fn has_private_nonempty(path: &Path, max: usize) -> bool {
    let limit = u64::try_from(max).unwrap_or(u64::MAX);
    let Ok(file) = open_look(path, READ_CONTROL) else {
        return false;
    };
    meta_of(&file).is_ok_and(|meta| {
        meta.file_type().is_file()
            && meta.st_nlink == 1
            && meta.size > 0
            && meta.size <= limit
            && matches!(not_owner_only(&file), Ok(None))
    })
}

/// Whether `path` names a plain file of one link, the user's under the owner-only list
/// ([`acl::owner_only`]), judged on the name opened as itself: what a Unix `0600` file of this
/// user's with one link is.
#[must_use]
pub(crate) fn private_file_settled(path: &Path) -> bool {
    let Ok(file) = open_look(path, READ_CONTROL) else {
        return false;
    };
    meta_of(&file).is_ok_and(|meta| meta.file_type().is_file() && meta.st_nlink == 1)
        && matches!(not_owner_only(&file), Ok(None))
}

/// The `ELEVATED` refusal aterm-uds' private files make (an elevated process makes and changes no
/// private file), for a change this module makes itself.
fn refuse_elevated() -> io::Result<()> {
    if aterm_uds::elevation::is_elevated() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            aterm_uds::private_file::ELEVATED,
        ));
    }
    Ok(())
}

/// The descriptor a write under `mode` makes its temporary with: the owner-only one for a private
/// mode, none (its folder's entries) for another.
fn descriptor_for(mode: u32) -> io::Result<Option<Descriptor>> {
    private_mode(mode)
        .then(aterm_uds::private_file::new_file_descriptor)
        .transpose()
}

/// [`super::write_atomic_with`] on Windows: a temporary made as `mode` says, renamed over `path`.
///
/// # Errors
/// As [`aterm_uds::atomic_file::write_secured_with`].
pub(crate) fn write_with(
    path: &Path,
    mode: u32,
    produce: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    aterm_uds::atomic_file::write_secured_with(path, descriptor_for(mode)?, produce, |_| Ok(()))
}

/// [`write_with`], answering the volume and file id of the temporary it renamed over `path`.
///
/// # Errors
/// As [`write_with`], or the temporary's id could not be read.
pub(crate) fn write_with_id(
    path: &Path,
    mode: u32,
    produce: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<(u64, super::Ino)> {
    aterm_uds::atomic_file::write_secured_with(path, descriptor_for(mode)?, produce, |file| {
        meta_of(file).map(|meta| id_of(&meta))
    })
}

/// A new temporary beside `path`, made as `mode` says ([`super::temp_beside`]).
///
/// # Errors
/// As [`aterm_uds::atomic_file::create_temp_beside`], or the descriptor could not be made.
pub(crate) fn create_temp(path: &Path, mode: u32) -> io::Result<(PathBuf, File)> {
    match descriptor_for(mode)? {
        Some(descriptor) => aterm_uds::atomic_file::create_temp_secured(path, &descriptor),
        None => aterm_uds::atomic_file::create_temp_beside(path, mode),
    }
}

/// [`super::holds_exactly`]'s judgement of the opened `file` on Windows: a plain file of one link
/// and `len` bytes whose access list is of `mode`'s class (owner-only for a private mode; the
/// user's but not owner-only for another, as a write under it leaves it).
pub(crate) fn holds_as(file: &File, len: u64, mode: u32) -> bool {
    let Ok(meta) = meta_of(file) else {
        return false;
    };
    if !meta.file_type().is_file() || meta.st_nlink != 1 || meta.size != len {
        return false;
    }
    match not_owner_only(file) {
        Ok(None) => private_mode(mode),
        Ok(Some(NotOwnerOnly::Owner(_))) | Err(_) => false,
        Ok(Some(_)) => !private_mode(mode),
    }
}

/// `path` opened to read as itself, for [`super::holds_exactly`].
pub(crate) fn open_read(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(SHARE_ALL)
        .custom_flags(acl::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// [`super::open_lock`] on Windows: the lock file at `path` made owner-only at its creation, or
/// the one there opened as itself, judged ([`lock_metadata`]) and made owner-only in place where
/// its list is not ([`aterm_dirfd::fchmod`] through its handle), its handle kept out of children.
///
/// # Errors
/// The make, open or narrowing fails (an elevated process makes and narrows none), or the file
/// fails [`lock_metadata`].
pub(crate) fn open_lock(path: &Path) -> crate::Result<File> {
    let at = |e: io::Error| Error::new(format!("{}: {e}", path.display()));
    let open = || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(SHARE_ALL)
            .custom_flags(acl::FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    };
    // The lock there is opened as it is (an elevated process may take one), and only a missing
    // one is made, owner-only from its creation.
    let file = aterm_uds::kept_out(|| match open() {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let made = aterm_uds::private_file::new_file_descriptor()
                .and_then(|descriptor| acl::create_file(path, &descriptor));
            match made {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => open(),
                made => made,
            }
        }
        opened => opened,
    })
    .map_err(at)?;
    lock_metadata(&file, path)?;
    if lock_wide(&file, path)?.is_some() {
        narrow_lock(&file, path)?;
    }
    Ok(file)
}

/// The lock file at `path` opened as it is, never made: as itself (a reparse point is opened,
/// never followed), to read and, with `writable`, to write, its handle kept out of children. A
/// missing lock is `NotFound`.
///
/// # Errors
/// The open's.
pub(crate) fn open_lock_existing(path: &Path, writable: bool) -> io::Result<File> {
    aterm_uds::kept_out(|| {
        OpenOptions::new()
            .read(true)
            .write(writable)
            .share_mode(SHARE_ALL)
            .custom_flags(acl::FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(gone_as_missing)
    })
}

/// Why the lock file `file` opened at `path` lets another account use it, `None` where its list
/// is the owner-only one ([`acl::owner_only`]): what a mode with a group, other, setuid or setgid
/// bit is on Unix to the lock's takers and observers.
///
/// # Errors
/// The token's user or the descriptor could not be read.
pub(crate) fn lock_wide(file: &File, path: &Path) -> crate::Result<Option<String>> {
    not_owner_only(file)
        .map(|why| why.map(|why| why.to_string()))
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

/// [`super::narrow_lock`] on Windows: the lock file `file` opened at `path`, which passed
/// [`lock_metadata`], made owner-only through its handle ([`aterm_dirfd::fchmod`]), so the file
/// those checks judged is the one changed. An elevated process narrows none.
///
/// # Errors
/// The process is elevated, or the change fails.
pub(crate) fn narrow_lock(file: &File, path: &Path) -> crate::Result<()> {
    let at = |e: io::Error| Error::new(format!("{}: {e}", path.display()));
    refuse_elevated().map_err(at)?;
    aterm_dirfd::fchmod(file, aterm_dirfd::Mode::RUSR | aterm_dirfd::Mode::WUSR)
        .map_err(|e| at(e.into()))
}

/// Whether `dir` is a folder of the user's (never a reparse point) whose list lets no account but
/// the user, SYSTEM and Administrators change it: what a Unix folder of this user's that no group
/// or other may write is, where no one else can rename a file to a name in it.
pub(crate) fn dir_owned_unshared(dir: &Path) -> bool {
    owned_unshared_dir_id(dir).is_some()
}

/// The volume and file id of `dir` where it is [`dir_owned_unshared`], judged on one handle: what a
/// Unix `lstat` that shows this user's folder no group or other may write gives as its device and
/// inode. `None` where it is not, or cannot be judged.
pub(crate) fn owned_unshared_dir_id(dir: &Path) -> Option<(u64, super::Ino)> {
    let handle = open_look(dir, READ_CONTROL).ok()?;
    let meta = meta_of(&handle).ok()?;
    (meta.file_type().is_dir() && matches!(not_owned(&handle), Ok(None))).then(|| id_of(&meta))
}

/// Whether the file or folder `file` is open on is the user's alone ([`acl::owner_only`] against
/// the token's user): what a Unix `0600` file or `0700` folder of this user's is. The handle needs
/// `READ_CONTROL` (a read or write open has it).
///
/// # Errors
/// The token's user or the descriptor could not be read.
pub(crate) fn owner_only(file: &File) -> io::Result<bool> {
    Ok(not_owner_only(file)?.is_none())
}

/// A new private file at `path`, open to read and write: the user's under a protected list of the
/// user and SYSTEM alone from its creation ([`aterm_uds::private_file::new_file_descriptor`]),
/// what a Unix `0600` file made with `O_CREAT | O_EXCL | O_NOFOLLOW` is. A name that is there, a
/// link included, is `AlreadyExists`.
///
/// # Errors
/// The process is elevated, the descriptor cannot be made, or the creation fails.
pub(crate) fn create_private_file_new(path: &Path) -> io::Result<File> {
    let descriptor = aterm_uds::private_file::new_file_descriptor()?;
    acl::create_file(path, &descriptor)
}

/// The digest of the owner and access list of what `path` names, the name opened as itself: what
/// a Unix `lstat`'s owner, group and mode bits are to who may use it, so a stamp that holds it
/// moves when who may use the name changes.
///
/// # Errors
/// `path` cannot be opened, or its descriptor cannot be read.
pub(crate) fn access_digest(path: &Path) -> io::Result<[u8; 32]> {
    let handle = open_look(path, READ_CONTROL).map_err(gone_as_missing)?;
    let parts = Parts::OWNER | Parts::DACL;
    let text = Descriptor::of(&handle, parts)?.to_sddl(parts)?;
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    let mut access = [0; 32];
    access.copy_from_slice(digest.as_ref());
    Ok(access)
}

/// [`super::lock_metadata`] on Windows: what the lock file `file` opened at `path` is, once it
/// passed the checks every taker of a vault lock makes: a plain file (no folder, no reparse
/// point), owned by this user, with one link. Its access list is left to the caller to judge.
///
/// # Errors
/// The file cannot be examined, or it fails one of those checks.
pub(crate) fn lock_metadata(file: &File, path: &Path) -> crate::Result<Meta> {
    let at = |e: io::Error| Error::new(format!("{}: {e}", path.display()));
    let meta = meta_of(file).map_err(at)?;
    if !meta.file_type().is_file() {
        return Err(Error::new(format!(
            "{}: not a regular file",
            path.display()
        )));
    }
    if let Some(NotOwnerOnly::Owner(owner)) = not_owner_only(file).map_err(at)? {
        let owner = owner.map_or_else(|| "nobody".to_owned(), |owner| owner.to_string());
        return Err(Error::new(format!(
            "{}: owned by {owner}, not this user",
            path.display()
        )));
    }
    if meta.st_nlink != 1 {
        return Err(Error::new(format!(
            "{}: has {} hard links",
            path.display(),
            meta.st_nlink
        )));
    }
    Ok(meta)
}

/// [`super::append_at_with`]'s open on Windows: `path` opened as itself to append (and to read its
/// facts and descriptor), where it is still the owner-only plain file of one link `expected`
/// stamped (its volume, id and length).
///
/// # Errors
/// The open fails, or the file is not that one.
pub(crate) fn open_append(path: &Path, expected: &FileStamp) -> io::Result<File> {
    let file = OpenOptions::new()
        .access_mode(FILE_APPEND_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE)
        .share_mode(SHARE_ALL)
        .custom_flags(acl::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let meta = meta_of(&file)?;
    if !meta.file_type().is_file()
        || meta.st_nlink != 1
        || not_owner_only(&file)?.is_some()
        || id_of(&meta) != (expected.dev, expected.ino)
        || meta.size != expected.len
    {
        return Err(io::Error::other("not the file this writer last left"));
    }
    Ok(file)
}

/// [`super::wipe`]'s open on Windows: `path` opened as itself, to read and, with `write`, to write.
pub(crate) fn open_wipe(path: &Path, write: bool) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(write)
        .share_mode(SHARE_ALL)
        .custom_flags(acl::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Write all of `bytes` to `file` at `offset`.
///
/// # Errors
/// The write's.
pub(crate) fn write_all_at(file: &File, mut bytes: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt as _;
    while !bytes.is_empty() {
        match file.seek_write(bytes, offset) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(n) => {
                bytes = &bytes[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Unix's positioned reads (`std::os::unix::fs::FileExt`) by the same names, for code that reads
/// a file at an offset on both. A Windows positioned read (`seek_read`) moves the handle's
/// position, so each puts back the position it found: a later read from the position starts where
/// it would on Unix.
pub(crate) trait FileAt {
    /// One read of up to `buf.len()` bytes at `offset`: the count read, 0 at the end.
    ///
    /// # Errors
    /// The read's, or the position's read or restore.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;

    /// `buf.len()` bytes at `offset`, or `UnexpectedEof` where the file ends first.
    ///
    /// # Errors
    /// A read's, or the file ends before `buf` is full.
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
}

impl FileAt for File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        use std::io::Seek as _;
        use std::os::windows::fs::FileExt as _;
        let mut file = self;
        let position = file.stream_position()?;
        let read = self.seek_read(buf, offset);
        file.seek(io::SeekFrom::Start(position))?;
        read
    }

    fn read_exact_at(&self, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
        while !buf.is_empty() {
            match self.read_at(buf, offset) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                Ok(n) => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// The folder `path` names, opened to name entries relative to it: `.` for a bare name.
fn open_parent(path: &Path) -> io::Result<(File, &std::ffi::OsStr)> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let handle = OpenOptions::new()
        .access_mode(FILE_LIST_AND_TRAVERSE | SYNCHRONIZE)
        .share_mode(SHARE_ALL)
        .custom_flags(acl::FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)?;
    Ok((handle, name))
}

/// [`super::link_then_unlink`] on Windows: `from` renamed to `to` in one step that refuses a
/// taken `to` ([`aterm_dirfd::renameat_with`] with `NOREPLACE`), each name relative to its own
/// folder's handle. Nothing is ever linked twice, so the removal never fails apart.
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> Result<(), Moved> {
    let (from_dir, from_name) = open_parent(from).map_err(Moved::NotLinked)?;
    let (to_dir, to_name) = open_parent(to).map_err(Moved::NotLinked)?;
    aterm_dirfd::renameat_with(
        &from_dir,
        from_name,
        &to_dir,
        to_name,
        aterm_dirfd::RenameFlags::NOREPLACE,
    )
    .map_err(|e| Moved::NotLinked(e.into()))
}

/// [`super::ensure_private_dir`] on Windows: aterm-uds' owner-only folder, made with its protected
/// list at its creation (its parents as `create_dir_all` makes them) or narrowed in place where
/// it is the user's and its list is not.
///
/// # Errors
/// As [`aterm_uds::private_file::ensure_private_dir`].
pub fn ensure_private_dir(dir: &Path) -> crate::Result<()> {
    aterm_uds::private_file::ensure_private_dir(dir)
        .map(drop)
        .map_err(|e| naming(dir, &e))
}

/// [`super::private_dir_settled`] on Windows: `dir` is the user's folder under a protected list of
/// the user and SYSTEM alone, as [`ensure_private_dir`] leaves it.
///
/// # Errors
/// `dir` is missing, a reparse point, no folder, another account's, or its list is not private.
pub(crate) fn private_dir_settled(dir: &Path) -> crate::Result<()> {
    aterm_uds::private_file::check_private_dir(dir).map_err(|e| naming(dir, &e))
}

/// [`super::create_private_dir_new`] on Windows: a new folder at `dir`, the user's under a
/// protected list of the user and SYSTEM alone from its creation (what is made in it inherits
/// that list). A taken name is `AlreadyExists`; an elevated process makes none.
///
/// # Errors
/// The process is elevated, the token's user cannot be read, or the creation fails.
pub(crate) fn create_private_dir_new(dir: &Path) -> io::Result<()> {
    refuse_elevated()?;
    let descriptor = acl::owner_only_descriptor(&user()?, acl::Inherit::ToChildren)?;
    acl::create_dir(dir, &descriptor)
}

/// `e`, naming `dir` unless its text already does.
fn naming(dir: &Path, e: &io::Error) -> Error {
    let text = e.to_string();
    if text.contains(&dir.display().to_string()) {
        Error::new(text)
    } else {
        Error::new(format!("{}: {text}", dir.display()))
    }
}

#[cfg(test)]
std::thread_local! {
    /// Whether [`user`] answers another account on this thread ([`as_another_user`]).
    static ANOTHER_USER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with [`user`] answering SYSTEM on this thread: every file this user made is then
/// another's to the checks inside it, as a lock another account left is.
#[cfg(test)]
pub(crate) fn as_another_user<T>(f: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ANOTHER_USER.with(|seam| seam.set(false));
        }
    }
    ANOTHER_USER.with(|seam| seam.set(true));
    let _reset = Reset;
    f()
}

#[cfg(test)]
#[path = "fsx_win_tests.rs"]
mod tests;
