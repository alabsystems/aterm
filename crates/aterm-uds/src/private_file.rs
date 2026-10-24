// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Owner-only private file and directory operations backed by `astream_sys` checks.

use std::fmt;
use std::io::{self, Read};
use std::path::Path;

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
            Self::Io(e) => write!(f, "it could not be read: {e}"),
        }
    }
}

impl std::error::Error for PrivateFileError {}

const ACL_OPEN: &str = "an access list (ACL) grants other users access; `chmod -N` removes it";

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
/// Returns an error if `dir` cannot be created or fails owner/symlink/ACL checks, or is not private once made or narrowed (a volume that ignores modes; `NotFound` where it is gone by then).
pub fn ensure_private_dir(dir: &Path) -> io::Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt as _;
    ensure_private_dir_by(dir, |dir| {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    })
}

/// [`ensure_private_dir`], with `narrow` making `dir` 0700.
fn ensure_private_dir_by(
    dir: &Path,
    narrow: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<Option<u32>> {
    use std::os::unix::fs::DirBuilderExt as _;
    let was = match judge_private_dir(dir)? {
        PrivateDir::Private => return Ok(None),
        PrivateDir::Missing => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
            None
        }
        PrivateDir::Open(mode) => Some(mode),
    };
    narrow(dir)?;
    match judge_private_dir(dir)? {
        PrivateDir::Private => Ok(was),
        PrivateDir::Missing => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the directory {} was removed as it was made 0700",
                dir.display()
            ),
        )),
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};

    use super::{
        PrivateDir, PrivateFileError, PrivateRead, create_private_dir, ensure_private_dir,
        judge_private_dir, read_private, read_private_into, remove_private, write_private,
    };

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
        let refusal = super::ensure_private_dir_by(&open, |_| Ok(())).unwrap_err();
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
            let gone =
                super::ensure_private_dir_by(&dir, |dir| std::fs::remove_dir(dir)).unwrap_err();
            assert_eq!(gone.kind(), std::io::ErrorKind::NotFound, "{gone}");
            assert!(gone.to_string().contains("was removed"), "{gone}");
            assert_eq!(judge_private_dir(&dir).unwrap(), PrivateDir::Missing);
        }
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
