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
//! A FOLDER IS JUDGED ON A HANDLE OF ITS OWN, AND THAT HANDLE IS HELD ([`Folder`]): every file of
//! it is then looked up, opened, made, renamed, linked and removed relative to that handle
//! (aterm-dirfd's `openat`, `fstatat`, `renameat`, `linkat`, `unlinkat`; one path component,
//! `OBJ_DONT_REPARSE`), never by a path, so a folder swapped for another, or a link put in its
//! place, after the judgement sends nothing elsewhere. A private folder is made relative to a
//! handle of its parent (`mkdirat`) and narrowed on a handle of itself (`fchmod`).
//!
//! What this module makes it makes so at its creation: a file or folder is created with the
//! descriptor `O:<user>D:P(A;;FA;;;<user>)(A;;FA;;;SY)` (a folder's entries also pass to what is
//! made inside, `OICI`), never made open and narrowed after, which would let the profile's
//! inherited entries in for a moment. Only a folder that already exists and is the user's is
//! narrowed in place ([`ensure_private_dir`]), as a 0700 `chmod` does on Unix.
//!
//! AN ELEVATED PROCESS (an administrator's full token) makes, changes and removes here too: what
//! it makes is the token user's from its creation (an owner its descriptor names, never the
//! token's default owner, Administrators), and it writes only through a held folder that was
//! judged the user's own, by names that follow nothing, so a link the user's unelevated processes
//! put anywhere sends none of its writes to a place they could not write themselves. The one
//! thing it does not do is make a missing folder above a private one, or make one in a folder
//! that is not the user's own ([`super::ELEVATED_PARENT`]): a folder made through a path, or in
//! another account's folder, is the classic way to hand an unelevated process a folder somewhere
//! it could not make one. [`super::new_file_descriptor`], whose callers make their files by
//! path, still refuses it ([`super::ELEVATED`]).

use std::fs::File;
use std::io::{self, Write as _};
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::Path;
use std::sync::OnceLock;

use aterm_dirfd::{AtFlags, Errno, FileId, Mode, OFlags};

use super::{ANOTHER_LINK, OPEN_MODE, OTHERS_WRITE, PrivateDir, PrivateFileError, dir_refused};
use crate::acl::{self, Descriptor, Inherit, NotOwnerOnly, Parts, Sid, WellKnown};

/// `READ_CONTROL`: read the object's descriptor.
const READ_CONTROL: u32 = 0x0002_0000;
/// `FILE_READ_ATTRIBUTES`.
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
/// `SYNCHRONIZE`.
const SYNCHRONIZE: u32 = 0x0010_0000;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`.
const SHARE_ALL: u32 = 0x0000_0007;
/// `INHERIT_ONLY_ACE`: the entry is only passed on to what a folder holds, and grants the folder
/// itself nothing.
const INHERIT_ONLY: u8 = 0x08;
/// The rights that only read or look: a file's data or a folder's listing (`FILE_READ_DATA`), its
/// extended attributes, its execution or traversal, its attributes, its descriptor, waiting on it,
/// and the generic read and execute rights. Any other right an entry allows writes, removes or
/// changes who may use the object.
const READ_RIGHTS: u32 = 0x0000_0001
    | 0x0000_0008
    | 0x0000_0020
    | 0x0000_0080
    | READ_CONTROL
    | SYNCHRONIZE
    | 0x2000_0000
    | 0x8000_0000;
/// The private mode a file or folder is made with ([`aterm_dirfd::Mode`]): owner-only.
const PRIVATE_FILE: Mode = Mode::from_bits(0o600);
/// The private mode of a folder.
const PRIVATE_DIR: Mode = Mode::RWXU;
/// [`OTHERS_WRITE`] of the folder.
const DIR_OTHERS_WRITE: &str = "the directory's access list lets another account write";

/// How [`open_in`] opens a private file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Opening {
    /// Read only ([`super::read_private`]).
    Read,
    /// Read and write, made with the user's protected access list when missing
    /// ([`super::open_private_rw`]).
    ReadWrite,
    /// Read only, under [`super::read_owned_into`]'s looser rule ([`owned_rule`]).
    Owned,
}

/// A folder opened as itself and held: what [`dir_is`] judged, and every file of it is looked
/// up, opened, made, renamed and removed relative to this handle, never by a path.
#[derive(Debug)]
pub(super) struct Folder(File);

impl Folder {
    /// `file`, a handle a caller opened on a folder, held as one; nothing is judged here.
    pub(super) fn held(file: File) -> Self {
        Self(file)
    }

    /// The held handle.
    pub(super) const fn file(&self) -> &File {
        &self.0
    }
}

/// A folder's verdict under this rule.
#[derive(Debug)]
pub(super) enum DirIs {
    /// There is none.
    Missing,
    /// The user's, under a protected access list of the user and SYSTEM alone.
    Private(Folder),
    /// The user's, but its access list breaks the rule as the text says: replacing the list
    /// ([`ensure_private_dir`]) makes it private.
    Open(&'static str, Folder),
    /// A reparse point (a junction, a symlink, a mount point), which the rule never follows.
    Reparse,
    /// Something other than a folder.
    NotDir,
    /// Another account's.
    Others(Folder),
}

/// [`DirIs`] without its folder: what one look at a held handle says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// [`DirIs::Private`].
    Private,
    /// [`DirIs::Open`].
    Open(&'static str),
    /// [`DirIs::Reparse`].
    Reparse,
    /// [`DirIs::NotDir`].
    NotDir,
    /// [`DirIs::Others`].
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

/// This thread's cloud placeholders shown as the reparse points they are until the guard drops
/// ([`aterm_dirfd::expose_placeholders`]): every open and look here runs under one.
///
/// # Errors
/// The system refused the mode: nothing is opened, since a placeholder would read as a plain file.
fn exposed() -> io::Result<aterm_dirfd::ExposedPlaceholders> {
    aterm_dirfd::expose_placeholders().map_err(io::Error::from)
}

/// Whether this process runs elevated; under [`super::elevated::as_elevated`], as this thread
/// says it does.
fn elevated() -> bool {
    #[cfg(any(test, debug_assertions))]
    if let Some(elevated) = super::elevated::here() {
        return elevated;
    }
    crate::elevation::is_elevated()
}

/// [`super::ELEVATED`] in an elevated process: for [`super::new_file_descriptor`], whose callers
/// make their files by path.
pub(super) fn refuse_elevated() -> io::Result<()> {
    if elevated() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            super::ELEVATED,
        ));
    }
    Ok(())
}

/// The folder at `dir` opened as itself (never a junction's target): its attributes and
/// descriptor read off this handle, and its files opened relative to it.
pub(super) fn open_folder(dir: &Path) -> io::Result<File> {
    let _exposed = exposed()?;
    std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
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
/// attributes (a reparse point, no folder), then its owner and access list. The handle is held
/// in the verdict, and every file of the folder is opened relative to it ([`open_in`]).
pub(super) fn dir_is(dir: &Path) -> io::Result<DirIs> {
    #[cfg(any(test, debug_assertions))]
    {
        super::tally::judged(dir);
        super::tally::opened(dir);
    }
    let opened = match open_folder(dir) {
        Ok(opened) => opened,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(DirIs::Missing),
        Err(e) => return Err(e),
    };
    Ok(match verdict_of(&opened)? {
        Verdict::Private => DirIs::Private(Folder(opened)),
        Verdict::Open(why) => DirIs::Open(why, Folder(opened)),
        Verdict::Others => DirIs::Others(Folder(opened)),
        Verdict::Reparse => DirIs::Reparse,
        Verdict::NotDir => DirIs::NotDir,
    })
}

/// The verdict on the folder `opened` is open on, read off that handle.
pub(super) fn verdict_of(opened: &File) -> io::Result<Verdict> {
    let _exposed = exposed()?;
    let facts = acl::by_handle(opened)?;
    if facts.attributes & acl::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(Verdict::Reparse);
    }
    if facts.attributes & acl::FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Ok(Verdict::NotDir);
    }
    Ok(match acl::owner_only(opened, user()?)? {
        Ok(()) => Verdict::Private,
        Err(NotOwnerOnly::Owner(_)) => Verdict::Others,
        Err(broken) => Verdict::Open(dir_why(&broken)),
    })
}

/// [`super::check_private_dir`]'s answer for `dir` judged `verdict`.
pub(super) fn check_dir_verdict(dir: &Path, verdict: &DirIs) -> io::Result<()> {
    let refused = |why: &str| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the directory {}: {why}", dir.display()),
        )
    };
    match verdict {
        DirIs::Private(_) => Ok(()),
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
        DirIs::Others(_) => Err(refused("it is another user's")),
        DirIs::Open(why, _) => Err(refused(why)),
    }
}

/// The held folder of `dir` where it is private, else [`check_dir_verdict`]'s refusal.
pub(super) fn private_folder(dir: &Path) -> io::Result<Folder> {
    match dir_is(dir)? {
        DirIs::Private(folder) => Ok(folder),
        other => Err(check_dir_verdict(dir, &other)
            .err()
            .unwrap_or_else(|| dir_refused(dir, "is not private"))),
    }
}

/// `name` directly in `dir`, under `dir`'s verdict `judged`, opened relative to the folder that
/// verdict holds, as `how` says, and judged on its open handle ([`judge_opened`]). A file that
/// is missing from a folder that is not private reads [`PrivateFileError::Missing`], as on
/// Unix; whether it is there is looked up relative to the folder too. Each look at the name and
/// its open is made with this thread's placeholders exposed (aterm-dirfd's own calls expose
/// them); where they cannot be, nothing is looked at and the answer is [`PrivateFileError::Io`].
pub(super) fn open_in(
    _dir: &Path,
    judged: &DirIs,
    name: &str,
    how: Opening,
) -> Result<(File, std::fs::Metadata, bool), PrivateFileError> {
    let (folder, dir_loose) = match judged {
        DirIs::Private(folder) => (folder, false),
        DirIs::Missing => return Err(PrivateFileError::Missing),
        DirIs::Reparse => return Err(PrivateFileError::Symlink),
        DirIs::NotDir => {
            return Err(PrivateFileError::Malformed("the directory is no directory"));
        }
        DirIs::Open(why, folder) => {
            let owned = if how == Opening::Owned {
                owned_rule(folder.file()).map_err(PrivateFileError::Io)?
            } else {
                Err(*why)
            };
            match owned {
                Ok(loose) => (folder, loose),
                Err(why) => return Err(refused_in(folder, name, how, refusal_of_dir(why))),
            }
        }
        DirIs::Others(folder) => {
            return Err(refused_in(
                folder,
                name,
                how,
                "the directory is another user's",
            ));
        }
    };
    let (file, meta, file_loose) = open_relative(folder, name, how)?;
    Ok((file, meta, dir_loose || file_loose))
}

/// The folder text for a clause [`owned_rule`] or [`acl::owner_only`] found broken on it.
fn refusal_of_dir(why: &'static str) -> &'static str {
    if why == OTHERS_WRITE {
        return DIR_OTHERS_WRITE;
    }
    for broken in [
        NotOwnerOnly::Owner(None),
        NotOwnerOnly::NoDacl,
        NotOwnerOnly::Unread(0),
    ] {
        if why == broken.why() {
            return dir_why(&broken);
        }
    }
    why
}

/// The refusal of `name` in a folder that is not private (`why`): [`PrivateFileError::Missing`]
/// where nothing is there (and the opening would not make it), the look's own error where it
/// failed otherwise than as missing or refused, else [`PrivateFileError::NotPrivate`].
fn refused_in(folder: &Folder, name: &str, how: Opening, why: &'static str) -> PrivateFileError {
    match aterm_dirfd::fstatat(folder.file(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(e) => {
            let e = io::Error::from(e);
            match e.kind() {
                io::ErrorKind::NotFound if how != Opening::ReadWrite => PrivateFileError::Missing,
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => {
                    PrivateFileError::NotPrivate(why)
                }
                _ => PrivateFileError::Io(e),
            }
        }
        Ok(_) => PrivateFileError::NotPrivate(why),
    }
}

/// `name` in the held `folder`, opened relative to it as `how` says (a read-write opening makes
/// it with [`file_descriptor`]'s owner and list where missing; an existing file keeps its own),
/// as itself (a reparse point there is opened, not followed, and refused), and judged on its
/// handle ([`judge_opened`]).
pub(super) fn open_relative(
    folder: &Folder,
    name: &str,
    how: Opening,
) -> Result<(File, std::fs::Metadata, bool), PrivateFileError> {
    let flags = match how {
        Opening::Read | Opening::Owned => OFlags::RDONLY,
        Opening::ReadWrite => OFlags::RDWR | OFlags::CREATE,
    };
    let file = match aterm_dirfd::openat(folder.file(), name, flags, PRIVATE_FILE) {
        Ok(handle) => File::from(handle),
        Err(e) => return Err(open_refused(folder, name, e)),
    };
    judge_opened(file, how)
}

/// What an open of `name` in `folder` refused with `e` says: a missing name, a folder opened to
/// write, else the name's own attributes decide: a reparse point there (one an open was refused
/// on) is a link, as on Unix; anything else is the open's error.
fn open_refused(folder: &Folder, name: &str, e: Errno) -> PrivateFileError {
    if e == Errno::ISDIR {
        return PrivateFileError::Malformed("not a regular file");
    }
    if e == Errno::LOOP || e == Errno::FOREIGN_REPARSE {
        return PrivateFileError::Symlink;
    }
    let e = io::Error::from(e);
    if e.kind() == io::ErrorKind::NotFound {
        return PrivateFileError::Missing;
    }
    let reparse = aterm_dirfd::fstatat(folder.file(), name, AtFlags::SYMLINK_NOFOLLOW)
        .is_ok_and(|stat| stat.file_type().is_reparse());
    if reparse {
        PrivateFileError::Symlink
    } else {
        PrivateFileError::Io(e)
    }
}

/// `file`, just opened as `how` says, judged on its handle: no reparse point, no folder, the
/// user's under a protected list of the user and SYSTEM ([`acl::owner_only`]; under
/// [`Opening::Owned`], [`owned_rule`]'s looser list), and one name. No name is a name removed
/// since the open: a read takes the file as it stood, while a read-write opening is
/// [`PrivateFileError::Missing`], as its writes would reach no name.
pub(super) fn judge_opened(
    file: File,
    how: Opening,
) -> Result<(File, std::fs::Metadata, bool), PrivateFileError> {
    let facts = {
        let _exposed = exposed().map_err(PrivateFileError::Io)?;
        acl::by_handle(&file).map_err(PrivateFileError::Io)?
    };
    if facts.attributes & acl::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PrivateFileError::Symlink);
    }
    if facts.attributes & acl::FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(PrivateFileError::Malformed("not a regular file"));
    }
    let loose = if how == Opening::Owned {
        owned_rule(&file)
            .map_err(PrivateFileError::Io)?
            .map_err(PrivateFileError::NotPrivate)?
    } else {
        let owner = user().map_err(PrivateFileError::Io)?;
        if let Err(broken) = acl::owner_only(&file, owner).map_err(PrivateFileError::Io)? {
            return Err(PrivateFileError::NotPrivate(broken.why()));
        }
        false
    };
    match facts.links {
        1 => {}
        0 if how == Opening::ReadWrite => return Err(PrivateFileError::Missing),
        0 => {}
        _ => return Err(PrivateFileError::NotPrivate(ANOTHER_LINK)),
    }
    let meta = file.metadata().map_err(PrivateFileError::Io)?;
    Ok((file, meta, loose))
}

/// [`super::read_owned_into`]'s rule on Windows, over the owner and access list of the object
/// `object` is open on, read off its handle: owned by the user; an access list (a NULL one lets
/// everyone write); and no entry that allows an account other than the user, SYSTEM and
/// Administrators (the accounts Unix's root stands for) any right but those that read or look.
/// Unlike the strict rule, its list may take entries from its folder and hold inherited ones, as
/// a Unix file's group and other read bits may be set. A deny entry takes nothing away and
/// passes, an entry that applies only to what a folder holds (`INHERIT_ONLY_ACE`) grants the
/// object nothing and passes, and an entry of any other type is refused unread. `Ok(loose)`
/// says whether another account may read it.
///
/// The outer `Err` is a read that failed; `Ok(Err(why))` is the clause broken.
fn owned_rule(object: &File) -> io::Result<Result<bool, &'static str>> {
    let user = user()?;
    let descriptor = Descriptor::of(object, Parts::OWNER | Parts::DACL)?;
    if descriptor.owner()?.as_ref() != Some(user) {
        return Ok(Err(NotOwnerOnly::Owner(None).why()));
    }
    let Some(entries) = descriptor.entries()? else {
        return Ok(Err(NotOwnerOnly::NoDacl.why()));
    };
    let mut loose = false;
    for entry in entries {
        if entry.flags & INHERIT_ONLY != 0 {
            continue;
        }
        match (entry.kind, entry.sid) {
            (acl::ACCESS_DENIED, _) => {}
            (acl::ACCESS_ALLOWED, Some(sid)) => {
                let trusted = sid == *user
                    || sid.is(WellKnown::LocalSystem)
                    || sid.is(WellKnown::Administrators);
                if trusted {
                    continue;
                }
                if entry.mask & !READ_RIGHTS != 0 {
                    return Ok(Err(OTHERS_WRITE));
                }
                loose |= entry.mask != 0;
            }
            (kind, _) => return Ok(Err(NotOwnerOnly::Unread(kind).why())),
        }
    }
    Ok(Ok(loose))
}

/// The identity of the file `file` is open on: its volume's serial number and its file id
/// ([`aterm_dirfd::file_id`]).
pub(super) fn identity(file: &File) -> Result<FileId, PrivateFileError> {
    aterm_dirfd::file_id(file).map_err(|e| PrivateFileError::Io(e.into()))
}

/// Inspects `dir` under [`ensure_private_dir`]'s rules without changing it: a folder of the
/// user's whose access list is not private is [`PrivateDir::Open`] with [`OPEN_MODE`].
/// # Errors
/// `dir` is a reparse point, no folder, or another account's; or it could not be judged.
pub fn judge_private_dir(dir: &Path) -> io::Result<PrivateDir> {
    private_dir_of(dir, &dir_is(dir)?)
}

/// [`judge_private_dir`]'s answer for `dir` judged `judged`.
fn private_dir_of(dir: &Path, judged: &DirIs) -> io::Result<PrivateDir> {
    match judged {
        DirIs::Missing => Ok(PrivateDir::Missing),
        DirIs::Private(_) => Ok(PrivateDir::Private),
        DirIs::Open(..) => Ok(PrivateDir::Open(OPEN_MODE)),
        DirIs::Reparse => Err(dir_refused(
            dir,
            "is a link or a junction (a reparse point); it must be a folder of its own",
        )),
        DirIs::NotDir => Err(dir_refused(dir, "is no directory")),
        DirIs::Others(_) => Err(dir_refused(dir, "is another user's")),
    }
}

/// Ensures `dir` exists as a private folder: made with the user's protected access list at its
/// creation, relative to a handle of its parent (unelevated, the parents made first as
/// `create_dir_all` makes them), or, where it is the user's but its list is not private, its
/// list replaced on the handle its judgement held ([`aterm_dirfd::fchmod`]), answering
/// `Some(OPEN_MODE)` then.
/// # Errors
/// `dir` fails [`judge_private_dir`], could not be made or narrowed, or is not private once made
/// or narrowed (`NotFound` where it is gone by then). In an elevated process,
/// [`super::ELEVATED_PARENT`] where `dir` is missing and its parent is missing or not the user's
/// own. Each names `dir`.
pub fn ensure_private_dir(dir: &Path) -> io::Result<Option<u32>> {
    let named = |e: io::Error| match e.raw_os_error() {
        Some(_) => io::Error::new(e.kind(), format!("the directory {}: {e}", dir.display())),
        None => e,
    };
    let removed = || {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the directory {} was removed as it was made private",
                dir.display()
            ),
        )
    };
    let open = |judged: DirIs| match judged {
        DirIs::Open(_, folder) => Ok(Some(folder)),
        DirIs::Private(_) => Ok(None),
        DirIs::Missing => Err(removed()),
        other => Err(private_dir_of(dir, &other)
            .err()
            .unwrap_or_else(|| dir_refused(dir, "could not be judged"))),
    };
    let narrowed = match dir_is(dir).map_err(named)? {
        DirIs::Private(_) => return Ok(None),
        DirIs::Missing => {
            if !elevated()
                && let Some(parent) = dir.parent()
                && !parent.as_os_str().is_empty()
            {
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
            match make_private_dir(dir) {
                Ok(()) => None,
                // Another writer made it since the judge: judged again.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    open(dir_is(dir).map_err(named)?)?
                }
                Err(e) => return Err(named(e)),
            }
        }
        judged => open(judged)?,
    };
    let was = narrowed.is_some().then_some(OPEN_MODE);
    if let Some(folder) = narrowed {
        aterm_dirfd::fchmod(folder.file(), PRIVATE_DIR)
            .map_err(io::Error::from)
            .map_err(named)?;
    }
    match judge_private_dir(dir)? {
        PrivateDir::Private => Ok(was),
        PrivateDir::Missing => Err(removed()),
        PrivateDir::Open(_) => Err(dir_refused(
            dir,
            "is not private and this user's after it was made so",
        )),
    }
}

/// Make the missing folder `dir` with the private folder's descriptor at its creation, relative
/// to a handle of its parent (`mkdirat`), its own name one component that follows nothing. An
/// elevated process makes it only where that parent is there and the user's own
/// ([`super::ELEVATED_PARENT`]).
/// # Errors
/// `dir` names no folder in a parent (a root, a `..`), or a name a path would read as another
/// file (`InvalidInput`); the parent's open, judgement or the make's own (`AlreadyExists` for a
/// name that is taken, a link included).
fn make_private_dir(dir: &Path) -> io::Result<()> {
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| super::one_component(name))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the directory {}: a private folder is made by its own name, one path \
                     component",
                    dir.display()
                ),
            )
        })?;
    let parent = match dir.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let elevated = elevated();
    let parent = match open_parent(parent) {
        Ok(parent) => parent,
        Err(e) if elevated && e.kind() == io::ErrorKind::NotFound => {
            return Err(elevated_parent(dir));
        }
        Err(e) => return Err(e),
    };
    if elevated && !is_users(&parent)? {
        return Err(elevated_parent(dir));
    }
    aterm_dirfd::mkdirat(&parent, name, PRIVATE_DIR).map_err(io::Error::from)
}

/// [`super::ELEVATED_PARENT`], naming `dir`.
fn elevated_parent(dir: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "the directory {}: {}",
            dir.display(),
            super::ELEVATED_PARENT
        ),
    )
}

/// The parent folder `parent` opened (a link on its way is followed, as a path is), to read its
/// owner and make a folder relative to it.
fn open_parent(parent: &Path) -> io::Result<File> {
    let _exposed = exposed()?;
    std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .share_mode(SHARE_ALL)
        .custom_flags(acl::FILE_FLAG_BACKUP_SEMANTICS)
        .open(parent)
}

/// Whether the folder `folder` is open on is a folder owned by this token's user.
fn is_users(folder: &File) -> io::Result<bool> {
    let facts = acl::by_handle(folder)?;
    if facts.attributes & acl::FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Ok(false);
    }
    let owner = Descriptor::of(folder, Parts::OWNER)?.owner()?;
    Ok(owner.as_ref() == Some(user()?))
}

/// Makes `dir` with the user's protected access list at its creation, relative to a handle of its
/// parent, or verifies that an existing `dir` passes [`super::check_private_dir`]. Its parent
/// must exist.
/// # Errors
/// Creation or [`super::check_private_dir`] fails; in an elevated process,
/// [`super::ELEVATED_PARENT`] where `dir` is missing and its parent is not the user's own.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    match dir_is(dir)? {
        DirIs::Private(_) => return Ok(()),
        DirIs::Missing => match make_private_dir(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        },
        other => return check_dir_verdict(dir, &other),
    }
    super::check_private_dir(dir)
}

/// Verifies that `dir` exists as a folder of the user's (never a reparse point or another
/// account's), whatever its access list says.
/// # Errors
/// `NotFound` when `dir` is missing, `NotADirectory` when it is no folder, `PermissionDenied`
/// when it is a reparse point or another account's.
pub fn check_owned_dir(dir: &Path) -> io::Result<()> {
    owned_folder(dir).map(drop)
}

/// [`check_owned_dir`], answering the folder its judgement held.
fn owned_folder(dir: &Path) -> io::Result<Folder> {
    match dir_is(dir)? {
        DirIs::Private(folder) | DirIs::Open(_, folder) => Ok(folder),
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
        DirIs::Others(_) => Err(dir_refused(dir, "is another user's")),
    }
}

/// [`super::remove_private`] on Windows: `name` removed relative to the folder `dir`'s judgement
/// held, as itself (a link goes, never its target). `false` where `dir` or `name` is missing,
/// or `dir` is no folder.
/// # Errors
/// [`check_owned_dir`]'s refusal; the look's or the removal's own error (a folder at `name`,
/// a reparse point that is no link).
pub(super) fn remove_private(dir: &Path, name: &str) -> io::Result<bool> {
    let folder = match owned_folder(dir) {
        Ok(folder) => folder,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    match remove_in(&folder, name) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// How many times a removal looks again where the name was given to another file between its
/// look and its removal ([`Errno::CHANGED`]).
const LOOKS: usize = 8;

/// Remove `name` from the held `folder` as itself: the file it names when it is looked at, and
/// none put in its place since ([`aterm_dirfd::unlinkat`] by that file's identity), looked at
/// again where it was.
/// # Errors
/// `NotFound` where nothing is there; the look's or the removal's own error.
pub(super) fn remove_in(folder: &Folder, name: &str) -> io::Result<()> {
    for _ in 0..LOOKS {
        let stat = aterm_dirfd::fstatat(folder.file(), name, AtFlags::SYMLINK_NOFOLLOW)?;
        match aterm_dirfd::unlinkat(folder.file(), name, AtFlags::empty(), stat.id()) {
            Err(e) if e == Errno::CHANGED => {}
            done => return done.map_err(io::Error::from),
        }
    }
    Err(io::Error::from(Errno::CHANGED))
}

/// A temporary [`write_in`] or [`write_new_in`] made in a held folder, removed by its identity
/// unless it was given its name.
struct Temporary<'a> {
    folder: &'a Folder,
    name: String,
    id: FileId,
    armed: bool,
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = aterm_dirfd::unlinkat(
                self.folder.file(),
                self.name.as_str(),
                AtFlags::empty(),
                self.id,
            );
        }
    }
}

/// A NEW temporary for `name` in the held `folder`, made relative to it with [`file_descriptor`]'s
/// owner and list at its creation (aterm-dirfd's `openat` with a private mode, `CREATE | EXCL`,
/// which refuses any name that is there, a link included), to read and write: eight names are
/// tried.
/// # Errors
/// No entropy for the name, a clash on every try, or the create's own error.
fn temporary_in<'a>(folder: &'a Folder, name: &str) -> io::Result<(Temporary<'a>, File)> {
    for _ in 0..8 {
        let temporary = crate::atomic_file::fresh_temp_component(name)?;
        let flags = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
        let file = match aterm_dirfd::openat(folder.file(), &temporary, flags, PRIVATE_FILE) {
            Ok(handle) => File::from(handle),
            Err(e) if e == Errno::EXIST => continue,
            Err(e) => return Err(e.into()),
        };
        let made = Temporary {
            folder,
            id: aterm_dirfd::file_id(&file)?,
            name: temporary,
            armed: true,
        };
        return Ok((made, file));
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "every private temporary name tried was already taken",
    ))
}

/// How [`write_in`] flushes: the file's flush before its rename, and whether its errors keep the
/// operation that failed ([`aterm_durable::Flush::file_diagnosed`]).
#[derive(Clone, Copy)]
pub(super) struct Flushing {
    pub(super) flush: aterm_durable::Flush,
    pub(super) diagnose: bool,
}

/// Write `bytes` to `name` in the held `folder` whole: through a NEW temporary made in it
/// ([`temporary_in`]), written, flushed as `flushing` says, `check`ed, and renamed over `name`
/// relative to the folder ([`aterm_dirfd::renameat`]), so a reader finds `name` as it was or
/// holding all of `bytes`; then the folder's own flush through its handle
/// ([`aterm_durable::Flush::dir_held`], which on Windows only notes it: NTFS commits a folder's
/// entries through its log). `path` names the file in errors.
///
/// The outer `Err` is before the rename (`name` is as it was and the temporary gone); the inner
/// one the folder's flush after it.
/// # Errors
/// As above.
pub(super) fn write_in(
    folder: &Folder,
    path: &Path,
    name: &str,
    bytes: &[u8],
    flushing: Flushing,
    check: impl FnOnce() -> io::Result<()>,
) -> io::Result<io::Result<()>> {
    use crate::atomic_file::write_error;
    let (mut temporary, mut file) =
        temporary_in(folder, name).map_err(|e| write_error("create temporary beside", path, e))?;
    let at = folder_path(path, &temporary.name);
    file.write_all(bytes)
        .map_err(|e| write_error("write", &at, e))?;
    if flushing.diagnose {
        flushing.flush.file_diagnosed(&file)
    } else {
        flushing.flush.file(&file)
    }
    .map_err(|e| write_error("flush file", &at, e))?;
    drop(file);
    check().map_err(|e| write_error("check before rename", &at, e))?;
    #[cfg(any(test, debug_assertions))]
    if let Some(error) = crate::atomic_file::publication_seam::failure() {
        return Err(write_error("rename", &at, error));
    }
    aterm_dirfd::renameat(folder.file(), temporary.name.as_str(), folder.file(), name)
        .map_err(|e| write_error("rename to", path, e.into()))?;
    temporary.armed = false;
    Ok(flushing.flush.dir_held(folder.file()))
}

/// `path`'s folder joined with `name`: where a temporary of `path` is, for an error to name.
fn folder_path(path: &Path, name: &str) -> std::path::PathBuf {
    path.parent()
        .map_or_else(|| name.into(), |dir| dir.join(name))
}

/// [`super::write_private_new`] in the held `folder`: a NEW temporary, `prepare`d, written and
/// flushed, given `name` by a hard link relative to the folder ([`aterm_dirfd::linkat`], which
/// fails on a name that is there where a rename would replace it), then removed by its identity.
/// `false` where `name` was there.
/// # Errors
/// As [`super::write_private_new`].
pub(super) fn write_new_in(
    folder: &Folder,
    name: &str,
    bytes: &[u8],
    prepare: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<bool> {
    let (mut temporary, mut file) = temporary_in(folder, name)?;
    prepare(&file)?;
    file.write_all(bytes)?;
    aterm_durable::Flush::Durable.file(&file)?;
    drop(file);
    let linked = aterm_dirfd::linkat(
        folder.file(),
        temporary.name.as_str(),
        folder.file(),
        name,
        AtFlags::empty(),
    );
    temporary.armed = false;
    let removed = aterm_dirfd::unlinkat(
        folder.file(),
        temporary.name.as_str(),
        AtFlags::empty(),
        temporary.id,
    );
    match linked {
        Err(e) if e == Errno::EXIST => return Ok(false),
        other => other?,
    }
    removed?;
    aterm_durable::Flush::Durable.dir_held(folder.file())?;
    Ok(true)
}

/// [`super::set_dir_mode_nofollow`] on Windows: the folder `dir` opened as itself (never a link's
/// target) and made owner-only on that handle ([`aterm_dirfd::fchmod`]) where it is not already.
/// # Errors
/// `Unsupported` for a mode that grants the group or others anything (Windows grants nothing in
/// place); a reparse point (`PermissionDenied`) or no folder (`NotADirectory`); the open's, or
/// `fchmod`'s (another account's folder is refused and left as it is).
pub(super) fn set_dir_mode_nofollow(dir: &Path, mode: u32) -> io::Result<()> {
    if mode & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "the directory {}: Windows keeps an access list, not a mode, and narrows a \
                 folder to its owner alone (a mode of 0700 or 0600), never widens one",
                dir.display()
            ),
        ));
    }
    let folder = open_folder(dir)?;
    match verdict_of(&folder)? {
        Verdict::Private => Ok(()),
        Verdict::Reparse => Err(dir_refused(
            dir,
            "is a link or a junction (a reparse point); it must be a folder of its own",
        )),
        Verdict::NotDir => Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("the directory {} is no directory", dir.display()),
        )),
        Verdict::Open(_) | Verdict::Others => aterm_dirfd::fchmod(&folder, PRIVATE_DIR)
            .map_err(io::Error::from)
            .map_err(|e| io::Error::new(e.kind(), format!("the directory {}: {e}", dir.display()))),
    }
}

#[cfg(test)]
#[path = "private_file_win_tests.rs"]
pub(super) mod tests;
