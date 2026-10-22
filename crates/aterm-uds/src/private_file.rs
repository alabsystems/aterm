// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Strict private files: the rules aterm's fleet state and agh's token files share.
//!
//! What makes one file or directory private is astream's one owner-only rule, judged
//! on the open descriptor ([`astream_sys::owner_private`],
//! [`astream_sys::owner_private_dir`]): the kind asked for, this user's (the effective
//! uid), no access-list entry that allows anyone anything (macOS checks an extended
//! ACL before the mode bits), and no group or other mode bit. This module keeps the
//! rest of the rules: a directory is made 0700 or narrowed to it
//! ([`ensure_private_dir`]), a file is one path component directly in its directory,
//! opened without following a link or waiting on a FIFO, with one link, and is
//! written whole ([`write_private`]).

use std::fmt;
use std::io::{self, Read};
use std::path::Path;

/// Why a private state file gives nothing ([`read_private`]). None carries a
/// byte of the file.
#[derive(Debug)]
pub enum PrivateFileError {
    /// No such file (or no state directory).
    Missing,
    /// The file or its directory is another user's, or another user could
    /// reach it (a group or other mode bit, a second link to the file).
    NotPrivate(&'static str),
    /// The file or its directory is a symbolic link.
    Symlink,
    /// No regular file, a name of more than one path component, or longer
    /// than such a file can be.
    Malformed(&'static str),
    /// The file could not be read.
    Io(io::Error),
}

impl fmt::Display for PrivateFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("there is no such file"),
            Self::NotPrivate(why) => write!(f, "not private: {why}"),
            Self::Symlink => f.write_str("it or its directory is a symlink"),
            Self::Malformed(why) => f.write_str(why),
            Self::Io(e) => write!(f, "it could not be read: {e}"),
        }
    }
}

impl std::error::Error for PrivateFileError {}

/// What a file or directory an extended access list opens to others is
/// refused as.
const ACL_OPEN: &str = "an access list (ACL) grants other users access; `chmod -N` removes it";

/// astream-sys's verdict on an open FILE ([`astream_sys::owner_private`]) in
/// this module's words. The rule is astream's one owner-only rule, read off the
/// descriptor: a regular file, this user's (the effective uid), no access-list
/// entry that allows anyone anything (macOS checks an extended ACL before the
/// mode bits, so a 0600 file with an inherited `everyone allow read` entry is
/// open to every user; a deny entry takes nothing away), and no group or other
/// mode bit.
fn file_verdict(file: &std::fs::File) -> Result<std::fs::Metadata, PrivateFileError> {
    use astream_sys::NotPrivate;
    match astream_sys::owner_private(file).map_err(PrivateFileError::Io)? {
        Ok(meta) => Ok(meta),
        Err(NotPrivate::Kind(_)) => Err(PrivateFileError::Malformed("not a regular file")),
        Err(NotPrivate::Owner(_)) => Err(PrivateFileError::NotPrivate("it is another user's")),
        Err(NotPrivate::Acl) => Err(PrivateFileError::NotPrivate(ACL_OPEN)),
        Err(NotPrivate::Mode(_)) => Err(PrivateFileError::NotPrivate(
            "it is open to other users (want 0600)",
        )),
    }
}

/// The bytes of `name` directly in `dir`, read under the key file's rules:
/// the directory and the file are this user's and nobody else's — by their
/// mode bits and by any access list, astream's one owner-only rule read off
/// each open descriptor ([`astream_sys::owner_private`]) — neither is a
/// symlink, and the file is one link to a regular file of at most `max`
/// bytes. Read without following a link and without waiting on a FIFO. No
/// such file is [`PrivateFileError::Missing`] whatever the directory is: a
/// machine in no fleet has a state directory as a launch left it, and its
/// mode is judged only when there is a secret in it to keep.
///
/// # Errors
/// A [`PrivateFileError`] naming the first rule the file or directory breaks.
pub fn read_private(dir: &Path, name: &str, max: usize) -> Result<Vec<u8>, PrivateFileError> {
    let (file, _meta) = open_private(dir, name)?;
    let mut bytes = Vec::with_capacity(max.min(64 * 1024) + 1);
    // Saturating: `usize::MAX` asks for no cap, and must not wrap to a cap of nothing.
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

/// What a name of more than one path component is refused as.
const ONE_COMPONENT: &str = "a private file's name is one path component";

/// Whether `name` is one path component: not empty, `.` or `..`, and no `/`. Only `dir`
/// is judged, and `O_NOFOLLOW` covers only the last component, so a name that reaches
/// through another directory would escape the rules.
fn one_component(name: &str) -> bool {
    !(name.is_empty() || name == "." || name == ".." || name.contains('/'))
}

/// [`one_component`] as the refusal [`write_private`] and [`remove_private`] give.
fn one_component_io(name: &str) -> io::Result<()> {
    if one_component(name) {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::InvalidInput, ONE_COMPONENT))
    }
}

/// What a directory is under the private-directory rule.
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

/// `dir` judged by astream's one owner-only rule on the OPEN directory
/// ([`astream_sys::owner_private_dir`]): opened read-only, non-blocking and
/// refusing a symlink at its last component, then judged off the descriptor —
/// a directory, this user's (the effective uid), no access-list entry that
/// allows anyone anything (macOS), and no group or other mode bit, in that
/// order, so [`DirIs::Open`] means every other clause held. A directory that
/// cannot be opened is refused either way; its `lstat` only words why when it
/// names a rule broken (a symlink, no directory, another user's), and any other
/// failure is the open's own error.
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

/// `name` directly in `dir`, opened under [`read_private`]'s rules, with the
/// metadata of the open descriptor (`fstat`, never a second `stat` by path).
/// The caller keeps this handle when a later operation (such as an advisory lock)
/// must act on the file that passed admission, rather than opening its name again.
///
/// # Errors
/// As [`read_private`], except that no bytes are read and no length cap is applied.
pub fn open_private(
    dir: &Path,
    name: &str,
) -> Result<(std::fs::File, std::fs::Metadata), PrivateFileError> {
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
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(PrivateFileError::Missing),
        Err(e) if e.kind() != io::ErrorKind::PermissionDenied => {
            return Err(PrivateFileError::Io(e));
        }
        // There is a file (or one this user cannot see past its directory's
        // mode, which the directory's judgement names): judge both.
        _ => {}
    }
    match judged {
        DirIs::Others => {
            return Err(PrivateFileError::NotPrivate(
                "the directory is another user's",
            ));
        }
        DirIs::Acl => return Err(PrivateFileError::NotPrivate(ACL_OPEN)),
        DirIs::Open(_) => {
            return Err(PrivateFileError::NotPrivate(
                "the directory is open to other users (want 0700)",
            ));
        }
        _ => {}
    }
    let file = match astream_sys::open_nonblocking(&dir.join(name), astream_sys::Links::Refuse) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(PrivateFileError::Missing),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(PrivateFileError::Symlink);
        }
        Err(e) => return Err(PrivateFileError::Io(e)),
    };
    let meta = file_verdict(&file)?;
    if meta.nlink() != 1 {
        return Err(PrivateFileError::NotPrivate("it has another link"));
    }
    Ok((file, meta))
}

/// What [`read_private_into`] read: the length, and the identity of the file it opened
/// (from `fstat` of the open descriptor, never a second `stat` by path).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrivateRead {
    /// Bytes read into the caller's buffer.
    pub len: usize,
    /// The file's device.
    pub dev: u64,
    /// The file's inode.
    pub ino: u64,
}

/// [`read_private`]'s rules, with the bytes read into the caller's buffer so that no copy
/// is left in a vector the caller cannot scrub. The cap is `min(max, buf.len())`. A file
/// longer than the cap is `Malformed("longer than such a file can be")`: after filling the
/// cap, one more byte is read into a one-byte local, which is zeroed before returning.
/// On `Err` the buffer holds none of the file: what a refused or failed read put in
/// `buf[..cap]` is scrubbed before returning.
///
/// # Errors
///
/// As [`read_private`].
pub fn read_private_into(
    dir: &Path,
    name: &str,
    max: usize,
    buf: &mut [u8],
) -> Result<PrivateRead, PrivateFileError> {
    use std::os::unix::fs::MetadataExt as _;
    let (mut file, meta) = open_private(dir, name)?;
    let cap = max.min(buf.len());
    let len = match read_full(&mut file, &mut buf[..cap]) {
        Ok(len) => len,
        Err(e) => {
            crate::secret::scrub(&mut buf[..cap]);
            return Err(PrivateFileError::Io(e));
        }
    };
    if len == cap {
        let mut more = [0u8; 1];
        let past = read_full(&mut file, &mut more);
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
    Ok(PrivateRead {
        len,
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// Read from `file` until `buf` is full or the file ends: how many bytes were read.
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

/// What a directory is under [`ensure_private_dir`]'s rules, as it stands
/// ([`judge_private_dir`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateDir {
    /// There is none: `ensure_private_dir` would make it 0700.
    Missing,
    /// It is private already: this user's, no group or other mode bit, no
    /// symlink and no access-list grant.
    Private,
    /// It is this user's, but its mode bits (these, `& 0o777`) open it to
    /// others: `ensure_private_dir` would narrow it to 0700.
    Open(u32),
}

/// [`ensure_private_dir`]'s refusal of `dir`, `why` worded for a person.
fn dir_refused(dir: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("the directory {} {why}", dir.display()),
    )
}

/// [`ensure_private_dir`]'s refusal of a directory an access list opens to others.
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

/// `dir` judged by [`ensure_private_dir`]'s rules with nothing changed: what
/// it would do (make it, narrow it, or nothing), so a dry run can say so.
///
/// # Errors
/// What `ensure_private_dir` refuses — a symlink, no directory, another
/// user's, an access list that grants others anything — as
/// `PermissionDenied` naming the directory, with no OS error code; or the
/// filesystem's own error, which carries one.
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

/// Make `dir` a private state directory, or say why it cannot be one: made
/// 0700 when it is missing (its parents as the platform makes them),
/// narrowed to 0700 when it is this user's but open to others — the mode it
/// had is returned, so the caller can say so — and refused when it is a
/// symlink, no directory, another user's, or open to others by an access
/// list ([`judge_private_dir`]). A directory it makes is judged too: an
/// access list a parent passes on to a new directory makes it no private
/// one. Nothing in it is touched.
///
/// # Errors
/// The refusal, worded for a person, or the filesystem's own error.
pub fn ensure_private_dir(dir: &Path) -> io::Result<Option<u32>> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    match judge_private_dir(dir)? {
        PrivateDir::Missing => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            match judge_private_dir(dir)? {
                PrivateDir::Private => Ok(None),
                PrivateDir::Missing | PrivateDir::Open(_) => Err(dir_refused(
                    dir,
                    "is not 0700 and this user's after it was made so",
                )),
            }
        }
        PrivateDir::Open(mode) => {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            Ok(Some(mode))
        }
        PrivateDir::Private => Ok(None),
    }
}

/// Whether `dir` is a private directory as it stands: this user's, mode 0700
/// (no group or other bit), no symlink and no ACL grant, judged on the open
/// directory by astream's one owner-only rule ([`astream_sys::owner_private_dir`]).
/// Nothing is changed.
///
/// # Errors
/// `NotFound` when there is no such directory; `PermissionDenied` naming the
/// directory and the rule it breaks; or the open's own error.
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

/// Write `name` directly in the private state directory `dir`, 0600, whole:
/// through a new temporary file beside it and a rename
/// ([`crate::atomic_file::write`]), so a reader sees the old bytes or the
/// new, never a part, and nothing planted at a temporary name is written
/// through.
///
/// # Errors
/// `dir` is not private ([`ensure_private_dir`] makes it so), or a write's own
/// error. A name of more than one path component is `InvalidInput`, and nothing is
/// written.
pub fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    one_component_io(name)?;
    check_private_dir(dir)?;
    crate::atomic_file::write(&dir.join(name), bytes, 0o600)
}

/// Remove `name` from `dir`: `true` when there was one.
///
/// # Errors
/// The removal's own error. A name of more than one path component is `InvalidInput`,
/// and nothing is removed.
pub fn remove_private(dir: &Path, name: &str) -> io::Result<bool> {
    one_component_io(name)?;
    match std::fs::remove_file(dir.join(name)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Make `dir` a new private directory (mode 0700), or accept one that already is private
/// ([`check_private_dir`]). It never narrows: an existing directory that is loose, another
/// user's, a symlink or ACL-granted is an error, and its mode is left as it was. The parent
/// must exist (`NotFound` otherwise).
///
/// # Errors
///
/// The `mkdir` failure, or `PermissionDenied` naming why the existing directory is not private.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            match std::fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {
                    // The umask can only narrow the mode `mkdir` gave; this sets it exactly.
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
                }
                // Made by another hand meanwhile: judged below like any existing one.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    // Judged as it stands, a new one too: an access list the parent passes on to a new
    // directory makes it no private one.
    check_private_dir(dir)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};

    use super::{
        PrivateDir, PrivateFileError, PrivateRead, create_private_dir, ensure_private_dir,
        judge_private_dir, read_private, read_private_into, remove_private, write_private,
    };

    /// A directory `name` in `root`, mode `mode`.
    fn dir_in(root: &Path, name: &str, mode: u32) -> PathBuf {
        let dir = root.join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        dir
    }

    /// A file `name` in `dir` holding `bytes`, mode `mode`.
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

    /// A 0600 file in a 0700 directory reads whole, and the identity it names is the
    /// file's own.
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

    /// A file one byte past the cap is malformed, whether the cap is `max` or the buffer.
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
        // A refusal leaves none of the file in the buffer.
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

    /// A file or a directory open to others is not private.
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

    /// A name of more than one path component is refused before anything is opened, even
    /// when the file it reaches is private: only `dir` would be judged.
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

    /// `write_private` and `remove_private` refuse a name of more than one path component
    /// as `InvalidInput`: nothing is made below `dir` or written or removed beside it.
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

    /// `usize::MAX` as `read_private`'s cap is no cap: the file reads whole.
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
    /// `chmod -N`, and leave it as it was. A new directory that inherits such a list from
    /// its parent is refused too, though `ensure_private_dir` made it. A deny entry (macOS puts
    /// one on `~/Library`) takes nothing away, and is accepted: the negative control.
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

    /// `judge_private_dir` says what `ensure_private_dir` would do and changes nothing: a
    /// missing directory stays missing, an open one keeps its mode until
    /// `ensure_private_dir` narrows it (returning the mode it had), and a symlink is
    /// refused with no OS error code.
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

    /// The file's clauses are astream's owner-only rule, worded here: a FIFO in the file's
    /// place is refused as no regular file AT ONCE (the open does not wait for a writer;
    /// the read runs behind the one-minute hang detector), and on macOS a 0600 file with
    /// an `everyone allow read` entry is not private, naming `chmod -N`. Controls: the same
    /// file reads once `chmod -N` clears the entry, and a plain 0600 file beside the FIFO
    /// reads.
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

    /// A symlink in the file's place is refused as one.
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

    /// `create_private_dir` makes a missing directory 0700, accepts a private one, and
    /// never narrows: a loose one is refused and left as it was, a symlink is refused, and
    /// a missing parent is `NotFound`.
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
