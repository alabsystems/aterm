// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! THE WINDOWS ARM OF [`super`]: owner-only files and folders under a protected access list.
//!
//! Windows has no mode bits; a private file or folder here is one that:
//! - is OWNED by this process token's user (`TokenUser`, the same account elevated or not);
//! - has a PROTECTED access list (it takes no entry from its folder), holding no inherited
//!   entry, whose every entry that allows anything allows the user or SYSTEM, and nobody else
//!   ([`crate::acl::owner_only`], read off the open handle, never by path);
//! - is NO REPARSE POINT (a symlink, a junction, a mount point, a cloud placeholder): every open
//!   here opens the name itself (`FILE_FLAG_OPEN_REPARSE_POINT`) and refuses one, never following
//!   it, and every open and look runs with this thread's placeholders exposed
//!   ([`aterm_dirfd::expose_placeholders`]), so a placeholder a sync client (OneDrive) left reads
//!   as the reparse point it is, as `aterm-secrets`' `fsx` reads it, never as a plain file or
//!   folder, which the process's default placeholder mode would show;
//! - and, for a file, has ONE NAME: a second hard link is another copy of it.
//!
//! What this module makes it makes so at its creation: a file or folder is created with the
//! descriptor `O:<user>D:P(A;;FA;;;<user>)(A;;FA;;;SY)` (a folder's entries also pass to what is
//! made inside, `OICI`) through `SECURITY_ATTRIBUTES`, never made open and narrowed after, which
//! would let the profile's inherited entries in for a moment. Only a folder that already exists
//! and is the user's is narrowed in place ([`ensure_private_dir`]), as a 0700 `chmod` does on
//! Unix.
//!
//! An elevated process (an administrator's full token) makes and changes nothing here
//! ([`super::ELEVATED`]): it may read what passes the rule.

use std::fs::File;
use std::io;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::Path;
use std::sync::OnceLock;

use super::{ANOTHER_LINK, OPEN_MODE, PrivateDir, PrivateFileError, dir_refused};
use crate::acl::{self, Descriptor, Inherit, NotOwnerOnly, Parts, Sid};

/// `READ_CONTROL`: read the object's descriptor.
const READ_CONTROL: u32 = 0x0002_0000;
/// `WRITE_DAC`: write the object's access list.
const WRITE_DAC: u32 = 0x0004_0000;
/// `FILE_READ_ATTRIBUTES`.
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
/// `SYNCHRONIZE`.
const SYNCHRONIZE: u32 = 0x0010_0000;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`.
const SHARE_ALL: u32 = 0x0000_0007;

/// How [`open_in`] opens a private file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Opening {
    /// Read only ([`super::read_private`]).
    Read,
    /// Read and write, made with the user's protected access list when missing
    /// ([`super::open_private_rw`]).
    ReadWrite,
}

/// A folder's verdict under this rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DirIs {
    /// There is none.
    Missing,
    /// The user's, under a protected access list of the user and SYSTEM alone.
    Private,
    /// The user's, but its access list breaks the rule as the text says: replacing the list
    /// ([`ensure_private_dir`]) makes it private.
    Open(&'static str),
    /// A reparse point (a junction, a symlink, a mount point), which the rule never follows.
    Reparse,
    /// Something other than a folder.
    NotDir,
    /// Another account's.
    Others,
}

/// This process token's user, the owner every private file and folder has: read once.
fn user() -> io::Result<&'static Sid> {
    static USER: OnceLock<Sid> = OnceLock::new();
    if let Some(user) = USER.get() {
        return Ok(user);
    }
    let user = Sid::token_user()?;
    Ok(USER.get_or_init(|| user))
}

/// The descriptor a private FILE is made with: owned by the user, a protected list of the user
/// and SYSTEM, nothing passed on.
pub(super) fn file_descriptor() -> io::Result<Descriptor> {
    acl::owner_only_descriptor(user()?, Inherit::No)
}

/// The descriptor a private FOLDER is made with: [`file_descriptor`]'s entries, each also passed
/// on to what is made inside.
fn dir_descriptor() -> io::Result<Descriptor> {
    acl::owner_only_descriptor(user()?, Inherit::ToChildren)
}

/// This thread's cloud placeholders shown as the reparse points they are until the guard drops
/// ([`aterm_dirfd::expose_placeholders`]): every open and look here runs under one.
///
/// # Errors
/// The system refused the mode: nothing is opened, since a placeholder would read as a plain file.
fn exposed() -> io::Result<aterm_dirfd::ExposedPlaceholders> {
    aterm_dirfd::expose_placeholders().map_err(io::Error::from)
}

/// Whether this process runs elevated; in a test, as this thread says it does.
fn elevated() -> bool {
    #[cfg(test)]
    if let Some(elevated) = tests::ELEVATED_HERE.get() {
        return elevated;
    }
    crate::elevation::is_elevated()
}

/// [`super::ELEVATED`] in an elevated process, which makes and changes nothing here.
pub(super) fn refuse_elevated() -> io::Result<()> {
    if elevated() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            super::ELEVATED,
        ));
    }
    Ok(())
}

/// The folder at `dir` opened as itself (never a junction's target), to read its attributes and
/// descriptor and, with `write_dac`, to replace its access list.
fn open_dir(dir: &Path, write_dac: bool) -> io::Result<File> {
    let write = if write_dac { WRITE_DAC } else { 0 };
    std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE | write)
        .share_mode(SHARE_ALL)
        .custom_flags(acl::FILE_FLAG_BACKUP_SEMANTICS | acl::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir)
}

/// The folder text for an access-list clause the folder breaks.
const fn dir_why(broken: &NotOwnerOnly) -> &'static str {
    match broken {
        NotOwnerOnly::Owner(_) => "the directory is another user's",
        NotOwnerOnly::NoDacl => "the directory has no access list, which lets everyone in",
        NotOwnerOnly::Unprotected => "the directory's access list takes entries from its folder",
        NotOwnerOnly::Inherited => {
            "the directory's access list holds an entry inherited from its folder"
        }
        NotOwnerOnly::Grants(_) => "the directory's access list lets another account in",
        NotOwnerOnly::Unread(_) => {
            "the directory's access list holds an entry of a kind this rule does not read"
        }
    }
}

/// `dir`'s verdict now, from the folder opened as itself and judged on that handle: its
/// attributes (a reparse point, no folder), then its owner and access list. Nothing binds what
/// follows: each file is opened again by path ([`open_in`]).
pub(super) fn dir_is(dir: &Path) -> io::Result<DirIs> {
    #[cfg(any(test, debug_assertions))]
    {
        super::tally::judged(dir);
        super::tally::opened(dir);
    }
    let _exposed = exposed()?;
    let opened = match open_dir(dir, false) {
        Ok(opened) => opened,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(DirIs::Missing),
        Err(e) => return Err(e),
    };
    judge_dir(&opened)
}

/// [`dir_is`] of the folder `opened` is open on.
fn judge_dir(opened: &File) -> io::Result<DirIs> {
    let facts = acl::by_handle(opened)?;
    if facts.attributes & acl::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(DirIs::Reparse);
    }
    if facts.attributes & acl::FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Ok(DirIs::NotDir);
    }
    Ok(match acl::owner_only(opened, user()?)? {
        Ok(()) => DirIs::Private,
        Err(NotOwnerOnly::Owner(_)) => DirIs::Others,
        Err(broken) => DirIs::Open(dir_why(&broken)),
    })
}

/// [`super::check_private_dir`]'s answer for `dir` judged `verdict`.
pub(super) fn check_dir_verdict(dir: &Path, verdict: DirIs) -> io::Result<()> {
    let refused = |why: &str| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the directory {}: {why}", dir.display()),
        )
    };
    match verdict {
        DirIs::Private => Ok(()),
        DirIs::Missing => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the directory {}: there is no such directory",
                dir.display()
            ),
        )),
        DirIs::Reparse => Err(refused(
            "it is a link or a junction (a reparse point); it must be a folder of its own",
        )),
        DirIs::NotDir => Err(refused("no directory")),
        DirIs::Others => Err(refused("it is another user's")),
        DirIs::Open(why) => Err(refused(why)),
    }
}

/// `name` directly in `dir`, under `dir`'s verdict `judged`, opened as `how` says and judged
/// on its open handle ([`judge_opened`]). A file that is missing from a folder that is not
/// private reads [`PrivateFileError::Missing`], as on Unix. Each look at the name and its open
/// is made with this thread's placeholders exposed ([`exposed`]); where they cannot be, nothing
/// is looked at and the answer is [`PrivateFileError::Io`].
pub(super) fn open_in(
    dir: &Path,
    judged: DirIs,
    name: &str,
    how: Opening,
) -> Result<(File, std::fs::Metadata, bool), PrivateFileError> {
    match judged {
        DirIs::Private => {}
        DirIs::Missing => return Err(PrivateFileError::Missing),
        DirIs::Reparse => return Err(PrivateFileError::Symlink),
        DirIs::NotDir => {
            return Err(PrivateFileError::Malformed("the directory is no directory"));
        }
        DirIs::Others | DirIs::Open(_) => {
            // Whether the name is there is looked at as every look here is, placeholders exposed.
            let _exposed = exposed().map_err(PrivateFileError::Io)?;
            match std::fs::symlink_metadata(dir.join(name)) {
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
            return Err(PrivateFileError::NotPrivate(match judged {
                DirIs::Open(why) => why,
                _ => "the directory is another user's",
            }));
        }
    }
    let path = dir.join(name);
    let _exposed = exposed().map_err(PrivateFileError::Io)?;
    let opened = match how {
        Opening::Read => open_file(&path, false),
        Opening::ReadWrite => refuse_elevated().and_then(|()| open_rw_created(&path)),
    };
    let file = match opened {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(PrivateFileError::Missing),
        // An open refused otherwise is judged from the name's own attributes: a reparse point
        // there (one a write open of a folder link is refused on) is a link, as on Unix.
        Err(e) => {
            use std::os::windows::fs::MetadataExt as _;
            let reparse = std::fs::symlink_metadata(&path)
                .is_ok_and(|meta| meta.file_attributes() & acl::FILE_ATTRIBUTE_REPARSE_POINT != 0);
            return Err(if reparse {
                PrivateFileError::Symlink
            } else {
                PrivateFileError::Io(e)
            });
        }
    };
    judge_opened(file, how)
}

/// `path` opened as itself (a reparse point is opened, not followed, and refused by the judge),
/// to read and, with `write`, to write. A folder opens too, so the judge names it.
fn open_file(path: &Path, write: bool) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(write)
        .share_mode(SHARE_ALL)
        .custom_flags(acl::FILE_FLAG_OPEN_REPARSE_POINT | acl::FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// `path` opened to read and write: made with [`file_descriptor`] where missing (`CREATE_NEW`,
/// which refuses any name that exists, a link included), else the file there, opened as itself.
/// An existing file's descriptor is never changed.
pub(super) fn open_rw_created(path: &Path) -> io::Result<File> {
    let _exposed = exposed()?;
    match acl::create_file(path, &file_descriptor()?) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => open_file(path, true),
        Err(e) => Err(e),
    }
}

/// Whether the file `file` is open on is a plain file: no folder and no reparse point.
pub(super) fn plain_file(file: &File) -> io::Result<bool> {
    let _exposed = exposed()?;
    let attributes = acl::by_handle(file)?.attributes;
    Ok(attributes & (acl::FILE_ATTRIBUTE_DIRECTORY | acl::FILE_ATTRIBUTE_REPARSE_POINT) == 0)
}

/// `file`, just opened as `how` says, judged on its handle: no reparse point, no folder, the
/// user's under a protected list of the user and SYSTEM ([`acl::owner_only`]), and one name. No
/// name is a name removed since the open: a read takes the file as it stood, while a read-write
/// opening is [`PrivateFileError::Missing`], as its writes would reach no name.
fn judge_opened(
    file: File,
    how: Opening,
) -> Result<(File, std::fs::Metadata, bool), PrivateFileError> {
    let facts = acl::by_handle(&file).map_err(PrivateFileError::Io)?;
    if facts.attributes & acl::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PrivateFileError::Symlink);
    }
    if facts.attributes & acl::FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(PrivateFileError::Malformed("not a regular file"));
    }
    let owner = user().map_err(PrivateFileError::Io)?;
    if let Err(broken) = acl::owner_only(&file, owner).map_err(PrivateFileError::Io)? {
        return Err(PrivateFileError::NotPrivate(broken.why()));
    }
    match facts.links {
        1 => {}
        0 if how == Opening::ReadWrite => return Err(PrivateFileError::Missing),
        0 => {}
        _ => return Err(PrivateFileError::NotPrivate(ANOTHER_LINK)),
    }
    let meta = file.metadata().map_err(PrivateFileError::Io)?;
    Ok((file, meta, false))
}

/// Inspects `dir` under [`ensure_private_dir`]'s rules without changing it: a folder of the
/// user's whose access list is not private is [`PrivateDir::Open`] with [`OPEN_MODE`].
/// # Errors
/// `dir` is a reparse point, no folder, or another account's; or it could not be judged.
pub fn judge_private_dir(dir: &Path) -> io::Result<PrivateDir> {
    match dir_is(dir)? {
        DirIs::Missing => Ok(PrivateDir::Missing),
        DirIs::Private => Ok(PrivateDir::Private),
        DirIs::Open(_) => Ok(PrivateDir::Open(OPEN_MODE)),
        DirIs::Reparse => Err(dir_refused(
            dir,
            "is a link or a junction (a reparse point); it must be a folder of its own",
        )),
        DirIs::NotDir => Err(dir_refused(dir, "is no directory")),
        DirIs::Others => Err(dir_refused(dir, "is another user's")),
    }
}

/// Ensures `dir` exists as a private folder: made with the user's protected access list at its
/// creation (its parents as `create_dir_all` makes them), or, where it is the user's but its
/// list is not private, its list replaced on a handle of the folder itself, answering
/// `Some(OPEN_MODE)` then.
/// # Errors
/// An elevated process ([`super::ELEVATED`]) where anything is to be made or changed; `dir`
/// fails [`judge_private_dir`], could not be made or narrowed, or is not private once made or
/// narrowed (`NotFound` where it is gone by then). Each names `dir`.
pub fn ensure_private_dir(dir: &Path) -> io::Result<Option<u32>> {
    let named = |e: io::Error| match e.raw_os_error() {
        Some(_) => io::Error::new(e.kind(), format!("the directory {}: {e}", dir.display())),
        None => e,
    };
    let judge = || judge_private_dir(dir).map_err(named);
    let removed = || {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the directory {} was removed as it was made private",
                dir.display()
            ),
        )
    };
    let was = match judge()? {
        PrivateDir::Private => return Ok(None),
        PrivateDir::Missing => {
            refuse_elevated()?;
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
            match acl::create_dir(dir, &dir_descriptor()?) {
                Ok(()) => None,
                // Another writer made it since the judge: judged again.
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
    if was.is_some() {
        narrow(dir).map_err(named)?;
    }
    match judge()? {
        PrivateDir::Private => Ok(was),
        PrivateDir::Missing => Err(removed()),
        PrivateDir::Open(_) => Err(dir_refused(
            dir,
            "is not private and this user's after it was made so",
        )),
    }
}

/// Replace the access list of the user's folder `dir` with the private folder's, protected, on
/// a handle of the folder itself: refused where it is a reparse point, no folder or another
/// account's by the time it is open.
fn narrow(dir: &Path) -> io::Result<()> {
    refuse_elevated()?;
    let _exposed = exposed()?;
    let opened = open_dir(dir, true)?;
    match judge_dir(&opened)? {
        DirIs::Private | DirIs::Open(_) => {}
        other => return check_dir_verdict(dir, other),
    }
    dir_descriptor()?.apply(&opened, Parts::DACL | Parts::PROTECTED_DACL)
}

/// Makes `dir` with the user's protected access list at its creation, or verifies that an
/// existing `dir` passes [`super::check_private_dir`]. Its parent must exist.
/// # Errors
/// An elevated process ([`super::ELEVATED`]) where `dir` is missing; creation or
/// [`super::check_private_dir`] fails.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    match dir_is(dir)? {
        DirIs::Private => return Ok(()),
        DirIs::Missing => {
            refuse_elevated()?;
            match acl::create_dir(dir, &dir_descriptor()?) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        other => return check_dir_verdict(dir, other),
    }
    super::check_private_dir(dir)
}

/// Verifies that `dir` exists as a folder of the user's (never a reparse point or another
/// account's), whatever its access list says.
/// # Errors
/// `NotFound` when `dir` is missing, `NotADirectory` when it is no folder, `PermissionDenied`
/// when it is a reparse point or another account's.
pub fn check_owned_dir(dir: &Path) -> io::Result<()> {
    match dir_is(dir)? {
        DirIs::Private | DirIs::Open(_) => Ok(()),
        DirIs::Missing => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the directory {}: there is no such directory",
                dir.display()
            ),
        )),
        DirIs::NotDir => Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("the directory {} is no directory", dir.display()),
        )),
        DirIs::Reparse => Err(dir_refused(
            dir,
            "is a link or a junction (a reparse point); it must be a folder of its own",
        )),
        DirIs::Others => Err(dir_refused(dir, "is another user's")),
    }
}

#[cfg(test)]
#[path = "private_file_win_tests.rs"]
mod tests;
