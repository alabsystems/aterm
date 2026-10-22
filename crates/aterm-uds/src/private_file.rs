// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Strict private files: the rules aterm's fleet state and agh's token files share.

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

/// Whether `meta` is this user's and nobody else's: `whose` and `open` name
/// the two ways it is not. The mode bits only; [`granted_to_others`] judges
/// an access list, which mode bits do not show.
fn private(
    meta: &std::fs::Metadata,
    whose: &'static str,
    open: &'static str,
) -> Result<(), PrivateFileError> {
    use std::os::unix::fs::MetadataExt as _;
    if meta.uid() != crate::peer::our_uid() {
        Err(PrivateFileError::NotPrivate(whose))
    } else if meta.mode() & 0o077 != 0 {
        Err(PrivateFileError::NotPrivate(open))
    } else {
        Ok(())
    }
}

/// What a file or directory an extended access list opens to others is
/// refused as ([`granted_to_others`]).
const ACL_OPEN: &str = "an access list (ACL) grants other users access; `chmod -N` removes it";

/// Whether `target` — a path, read without following a link, or an open
/// descriptor — carries an access list that ALLOWS anyone anything: on macOS
/// an extended ACL is checked before the mode bits, so a 0600 file in a 0700
/// directory with an `everyone allow read` entry (one inherited from a
/// parent's `file_inherit` entry, say) is open to every user. A deny entry
/// (the `group:everyone deny delete` macOS puts on `~/Library`) takes nothing
/// away from privacy, and is no reason to refuse. Elsewhere mode bits are the
/// rule, and this is `false`.
#[cfg(target_os = "macos")]
fn granted_to_others(target: AclOf<'_>) -> io::Result<bool> {
    use std::os::raw::{c_char, c_int, c_void};
    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: c_int = 0;
    const ACL_NEXT_ENTRY: c_int = -1;
    const ACL_EXTENDED_ALLOW: c_int = 1;
    // SAFETY (the declarations): `<sys/acl.h>`'s, `acl_t` and `acl_entry_t`
    // opaque pointers and `acl_type_t`/`acl_tag_t` C enums (`int`).
    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
        fn acl_get_link_np(path: *const c_char, kind: c_int) -> *mut c_void;
        fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_get_tag_type(entry: *mut c_void, tag: *mut c_int) -> c_int;
        fn acl_free(object: *mut c_void) -> c_int;
    }
    let acl = match target {
        // SAFETY: `fd` is open for the call (the caller holds its file).
        AclOf::Fd(fd) => unsafe { acl_get_fd_np(fd, ACL_TYPE_EXTENDED) },
        AclOf::Path(path) => {
            use std::os::unix::ffi::OsStrExt as _;
            let path = std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in a path"))?;
            // SAFETY: `path` is a NUL-terminated string that outlives the call.
            unsafe { acl_get_link_np(path.as_ptr(), ACL_TYPE_EXTENDED) }
        }
    };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        // No extended ACL at all: ENOENT.
        return match error.raw_os_error() {
            Some(libc::ENOENT) => Ok(false),
            _ => Err(error),
        };
    }
    let mut allows = false;
    let mut entry: *mut c_void = std::ptr::null_mut();
    let mut which = ACL_FIRST_ENTRY;
    // SAFETY: `acl` is the list just returned, freed once below; each entry
    // and its tag are read only while it lives.
    while unsafe { acl_get_entry(acl, which, &mut entry) } == 0 {
        which = ACL_NEXT_ENTRY;
        let mut tag: c_int = 0;
        // SAFETY: `entry` is an entry of the live `acl`; `tag` is this frame's.
        if unsafe { acl_get_tag_type(entry, &mut tag) } == 0 && tag == ACL_EXTENDED_ALLOW {
            allows = true;
            break;
        }
    }
    // SAFETY: `acl` came from `acl_get_*_np` and is freed exactly once.
    unsafe { acl_free(acl) };
    Ok(allows)
}

#[cfg(not(target_os = "macos"))]
fn granted_to_others(target: AclOf<'_>) -> io::Result<bool> {
    match target {
        AclOf::Fd(_fd) => Ok(false),
        AclOf::Path(_path) => Ok(false),
    }
}

/// What [`granted_to_others`] reads the access list of.
#[derive(Clone, Copy)]
enum AclOf<'a> {
    /// An open descriptor.
    Fd(std::os::raw::c_int),
    /// A path, not followed if it is a link.
    Path(&'a Path),
}

/// [`granted_to_others`] as a privacy refusal.
fn no_acl(target: AclOf<'_>) -> Result<(), PrivateFileError> {
    match granted_to_others(target) {
        Ok(false) => Ok(()),
        Ok(true) => Err(PrivateFileError::NotPrivate(ACL_OPEN)),
        Err(e) => Err(PrivateFileError::Io(e)),
    }
}

/// The bytes of `name` directly in `dir`, read under the key file's rules:
/// the directory and the file are this user's and nobody else's — by their
/// mode bits and by any access list ([`granted_to_others`]) — neither is a
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
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    if !one_component(name) {
        return Err(PrivateFileError::Malformed(ONE_COMPONENT));
    }
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(PrivateFileError::Missing),
        Err(e) => return Err(PrivateFileError::Io(e)),
    };
    if meta.file_type().is_symlink() {
        return Err(PrivateFileError::Symlink);
    }
    if !meta.is_dir() {
        return Err(PrivateFileError::Malformed("the directory is no directory"));
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
    private(
        &meta,
        "the directory is another user's",
        "the directory is open to other users (want 0700)",
    )?;
    no_acl(AclOf::Path(dir))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(name));
    let file = match file {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(PrivateFileError::Missing),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(PrivateFileError::Symlink);
        }
        Err(e) => return Err(PrivateFileError::Io(e)),
    };
    let meta = file.metadata().map_err(PrivateFileError::Io)?;
    if !meta.file_type().is_file() {
        return Err(PrivateFileError::Malformed("not a regular file"));
    }
    if meta.nlink() != 1 {
        return Err(PrivateFileError::NotPrivate("it has another link"));
    }
    private(
        &meta,
        "it is another user's",
        "it is open to other users (want 0600)",
    )?;
    no_acl(AclOf::Fd(file.as_raw_fd()))?;
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

/// Make `dir` a private state directory, or say why it cannot be one: made
/// 0700 when it is missing (its parents as the platform makes them),
/// narrowed to 0700 when it is this user's but open to others — the mode it
/// had is returned, so the caller can say so — and refused when it is a
/// symlink, no directory, or another user's. Nothing in it is touched.
///
/// # Errors
/// The refusal, worded for a person, or the filesystem's own error.
pub fn ensure_private_dir(dir: &Path) -> io::Result<Option<u32>> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    let refused = |why: &str| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the directory {} {why}", dir.display()),
        )
    };
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            Ok(None)
        }
        Err(e) => Err(e),
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(refused("is a symlink; it must be a directory of its own"))
        }
        Ok(meta) if !meta.is_dir() => Err(refused("is no directory")),
        Ok(meta) if meta.uid() != crate::peer::our_uid() => Err(refused("is another user's")),
        Ok(_) if granted_to_others(AclOf::Path(dir))? => Err(refused(&format!(
            "has {}",
            ACL_OPEN.replace("`chmod -N`", &format!("`chmod -N {}`", dir.display()))
        ))),
        Ok(meta) if meta.mode() & 0o077 != 0 => {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            Ok(Some(meta.mode() & 0o777))
        }
        Ok(_) => Ok(None),
    }
}

/// Whether `dir` is a private directory as it stands: this user's, mode 0700
/// (no group or other bit), no symlink and no ACL grant. Nothing is changed.
pub fn check_private_dir(dir: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    let refused = |why: &dyn fmt::Display| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the directory {}: {why}", dir.display()),
        )
    };
    if meta.file_type().is_symlink() {
        return Err(refused(&PrivateFileError::Symlink));
    }
    if !meta.is_dir() {
        return Err(refused(&"no directory"));
    }
    private(
        &meta,
        "it is another user's",
        "it is open to other users (want 0700)",
    )
    .and_then(|()| no_acl(AclOf::Path(dir)))
    .map_err(|e| refused(&e))
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
        PrivateFileError, PrivateRead, create_private_dir, read_private, read_private_into,
        remove_private, write_private,
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
        let chmod = |args: &[&std::ffi::OsStr]| {
            let ran = crate::spawnfd::command("/bin/chmod")
                .args(args)
                .status()
                .expect("run chmod");
            assert!(ran.success(), "chmod {args:?}");
        };
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
