// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOWS ARM OF [`super`]: Cargo's own two cache locks, taken as Cargo takes them here.
//!
//! Cargo on Windows locks `.package-cache-mutate` and `.package-cache` with `LockFileEx` over the
//! whole range (offset 0, length `u64::MAX`) on every volume (only on Linux NFS does it skip
//! locking), the range std's `File::try_lock` and `File::try_lock_shared` take, so a lock taken
//! here excludes and is excluded by Cargo's as `flock` does on unix. A Windows lock belongs to its
//! handle: a second handle in this process conflicts as another process's does, as a second open
//! file description's `flock` does on unix. No process inherits it (Windows has no fork, and these
//! handles are not inheritable), so a guard is never shared with another process, and its drop
//! releases by `UnlockFile` before the handle closes, as the unix arm releases by `LOCK_UN`.
//!
//! The rest is the unix arm's, in Windows terms: the Cargo home is opened by its resolved path as
//! itself (a reparse point there is refused), each lock is opened relative to that retained handle
//! and never through a reparse point (`aterm_dirfd::openat`), and both the held and the named lock
//! must be a regular file (no reparse point, no delete pending) with one name, owned by this token's
//! user or by the owner this token gives what it makes (`TokenOwner`: the user, or Administrators
//! when elevated, as a unix file is its maker's euid's), whose access list lets no account but that
//! user, SYSTEM and Administrators write, delete, re-own or re-permit it (unix's no group or other
//! write bits; SYSTEM and Administrators are Windows' root, and the profile gives them every file
//! in it). Cargo's own lock files in a profile pass as Cargo leaves them.

use std::{
    fs::{File, OpenOptions, TryLockError},
    io,
    os::windows::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use aterm_dirfd::{self as dirfd, Mode, OFlags, acl};

const LOCK: &str = ".package-cache-mutate";
const DOWNLOAD: &str = ".package-cache";

/// `FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL`: the retained Cargo
/// home's rights, which a handle-relative open and the checks read through.
const HOME_ACCESS: u32 = 0x0001 | 0x0020 | 0x0080 | 0x0002_0000;
/// `FILE_READ_ATTRIBUTES | READ_CONTROL`: a name observed for its identity, kind and owner only.
const OBSERVE_ACCESS: u32 = 0x0080 | 0x0002_0000;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`, as Cargo's and std's opens share.
const SHARE_ALL: u32 = 0x0000_0007;
/// `INHERIT_ONLY_ACE`: the entry passes to what is made inside and allows nothing on this object.
const INHERIT_ONLY: u8 = 0x08;
/// The rights an account other than the user, SYSTEM and Administrators may not hold on a lock:
/// `FILE_WRITE_DATA`, `FILE_APPEND_DATA`, `FILE_WRITE_EA`, `FILE_WRITE_ATTRIBUTES`, `DELETE`,
/// `WRITE_DAC`, `WRITE_OWNER`, `GENERIC_ALL` and `GENERIC_WRITE`.
const CHANGE_RIGHTS: u32 = 0x0002
    | 0x0004
    | 0x0010
    | 0x0100
    | 0x0001_0000
    | 0x0004_0000
    | 0x0008_0000
    | 0x1000_0000
    | 0x4000_0000;

#[derive(Debug)]
pub(super) struct Held {
    file: File,
    directory: File,
    original: PathBuf,
    resolved: PathBuf,
}

#[derive(Debug)]
pub(super) struct ReclaimHeld {
    mutate: Held,
    download: File,
}

impl ReclaimHeld {
    pub(super) fn check_current(&self) -> io::Result<()> {
        check_download(&self.mutate, &self.download)
    }
}

fn check_download(mutate: &Held, download: &File) -> io::Result<()> {
    mutate.check_current()?;
    let held = valid_lock(download)?;
    let named = valid_lock(&observe(&mutate.resolved.join(DOWNLOAD))?)?;
    if held.id() != named.id() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "original Cargo download lock identity changed",
        ));
    }
    Ok(())
}

impl Drop for ReclaimHeld {
    /// Release in reverse protocol order: the download lock here, then `mutate`'s own drop.
    fn drop(&mut self) {
        let _ = self.download.unlock();
    }
}

impl Drop for Held {
    /// Release by `UnlockFile` before the handle closes: Windows frees a closed handle's locks
    /// only as its resources allow.
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Open `path`'s last component as itself, never following a reparse point there, for its
/// identity, kind and owner (`FILE_FLAG_OPEN_REPARSE_POINT`; a folder needs backup semantics).
fn open_as_itself(path: &Path, access: u32) -> io::Result<File> {
    let _exposed = dirfd::expose_placeholders()?;
    OpenOptions::new()
        .access_mode(access)
        .share_mode(SHARE_ALL)
        .custom_flags(acl::FILE_FLAG_BACKUP_SEMANTICS | acl::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// What the name `path` is now, observed without being followed.
fn observe(path: &Path) -> io::Result<File> {
    open_as_itself(path, OBSERVE_ACCESS)
}

/// The directory a handle is open on, refused unless it is a plain directory.
fn directory(file: &File) -> io::Result<dirfd::Stat> {
    let stat = dirfd::fstat(file)?;
    if stat.file_type().is_dir() && !stat.delete_pending {
        Ok(stat)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "Cargo home is not a plain directory",
        ))
    }
}

/// This token's user and the owner it gives what it makes (`TokenOwner`), read once.
fn token() -> io::Result<&'static (acl::Sid, acl::Sid)> {
    static TOKEN: OnceLock<(acl::Sid, acl::Sid)> = OnceLock::new();
    if let Some(token) = TOKEN.get() {
        return Ok(token);
    }
    let token = (acl::Sid::token_user()?, acl::Sid::token_owner()?);
    Ok(TOKEN.get_or_init(|| token))
}

/// Owned by this token's user or its default owner, with an access list (a NULL list lets
/// everyone in) whose every allow entry that may change the file names the user, SYSTEM or
/// Administrators. A deny entry takes nothing away; an entry of a type not read is refused.
pub(super) fn closed_to_others(descriptor: &acl::Descriptor) -> io::Result<bool> {
    let (user, made) = token()?;
    if !descriptor
        .owner()?
        .is_some_and(|owner| owner == *user || owner == *made)
    {
        return Ok(false);
    }
    let Some(entries) = descriptor.entries()? else {
        return Ok(false);
    };
    Ok(entries.iter().all(|entry| {
        entry.flags & INHERIT_ONLY != 0
            || match (entry.kind, &entry.sid) {
                (acl::ACCESS_DENIED, _) => true,
                (acl::ACCESS_ALLOWED, Some(sid)) => {
                    entry.mask & CHANGE_RIGHTS == 0
                        || sid == user
                        || sid.is(acl::WellKnown::LocalSystem)
                        || sid.is(acl::WellKnown::Administrators)
                }
                _ => false,
            }
    }))
}

/// The lock `file` is open on, refused unless it is one this module may hold ([`super`]'s rule).
fn valid_lock(file: &File) -> io::Result<dirfd::Stat> {
    let stat = dirfd::fstat(file)?;
    let descriptor = acl::Descriptor::of(file, acl::Parts::OWNER | acl::Parts::DACL)?;
    if stat.file_type().is_file()
        && !stat.delete_pending
        && stat.st_nlink == 1
        && closed_to_others(&descriptor)?
    {
        Ok(stat)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Cargo lock is not a regular single-name file of this user's that no other account but SYSTEM and Administrators may change",
        ))
    }
}

impl Held {
    pub(super) fn check_current(&self) -> io::Result<()> {
        let named = std::fs::canonicalize(&self.original)?;
        let held_home = directory(&self.directory)?;
        let named_home = directory(&observe(&named)?)?;
        let held_lock = valid_lock(&self.file)?;
        let named_lock = valid_lock(&observe(&self.resolved.join(LOCK))?)?;
        if named != self.resolved
            || held_home.id() != named_home.id()
            || held_lock.id() != named_lock.id()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "original Cargo home or mutate lock identity changed",
            ));
        }
        Ok(())
    }
}

fn shared_lock(file: &File) -> io::Result<()> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the strict owned Cargo shared source-cache guard's actual LockFileEx acquisition, released by `Held`'s Drop (`UnlockFile`); no best-effort fallback"
    )]
    let result = file.try_lock_shared();
    result.map_err(|error| match error {
        TryLockError::WouldBlock => io::ErrorKind::WouldBlock.into(),
        TryLockError::Error(error) => error,
    })
}

fn exclusive_lock(file: &File) -> io::Result<()> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the strict Cargo reclamation guard's nonblocking original protocol acquisition; its Drop releases by UnlockFile"
    )]
    let result = file.try_lock();
    result.map_err(|error| match error {
        TryLockError::WouldBlock => io::ErrorKind::WouldBlock.into(),
        TryLockError::Error(error) => error,
    })
}

pub(super) fn acquire(cargo_home: &Path) -> io::Result<Held> {
    acquire_mode(cargo_home, shared_lock, true)
}

pub(super) fn acquire_reclaim(cargo_home: &Path) -> io::Result<ReclaimHeld> {
    let mutate = acquire_mode(cargo_home, exclusive_lock, false)?;
    // Relative to the retained home, never through a reparse point; no file is created.
    let download = File::from(dirfd::openat(
        &mutate.directory,
        DOWNLOAD,
        OFlags::RDWR | OFlags::NOFOLLOW,
        Mode::empty(),
    )?);
    check_download(&mutate, &download)?;
    exclusive_lock(&download)?;
    let held = ReclaimHeld { mutate, download };
    held.check_current()?;
    Ok(held)
}

/// Open the mutate lock of the absolute `cargo_home` relative to its retained directory (made
/// owner-only when `create` and missing, as unix makes it 0600), check it, take it by `lock`
/// and check it again.
pub(super) fn acquire_mode(
    cargo_home: &Path,
    lock: impl FnOnce(&File) -> io::Result<()>,
    create: bool,
) -> io::Result<Held> {
    if !cargo_home.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Cargo home must be explicit and absolute",
        ));
    }
    let resolved = std::fs::canonicalize(cargo_home)?;
    let directory = open_as_itself(&resolved, HOME_ACCESS)?;
    self::directory(&directory)?;
    let flags = OFlags::RDWR
        | OFlags::NOFOLLOW
        | if create {
            OFlags::CREATE
        } else {
            OFlags::empty()
        };
    let file = File::from(dirfd::openat(
        &directory,
        LOCK,
        flags,
        Mode::RUSR | Mode::WUSR,
    )?);
    let held = Held {
        file,
        directory,
        original: cargo_home.into(),
        resolved,
    };
    held.check_current()?;
    lock(&held.file)?;
    held.check_current()?;
    Ok(held)
}
