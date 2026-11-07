// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Filesystem custody for an app image: descriptor-pinned cloning and a confined seal.
//! These operations supply no code-signing authority; callers check the sealed candidate.

use std::collections::VecDeque;
use std::io;
#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd as _;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

/// A definite unsafe shape, or an operation that could not complete.
#[derive(Debug)]
pub enum Error {
    /// The outer image is not a real directory, or an internal link escapes it.
    Refused(String),
    /// A filesystem operation failed; its original errno is preserved.
    Io(io::Error),
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) => out.write_str(why),
            Self::Io(error) => error.fmt(out),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Refused(_) => None,
        }
    }
}

fn directory(path: &Path) -> Result<OwnedFd, Error> {
    if !std::fs::symlink_metadata(path)?.is_dir() {
        return Err(Error::Refused(format!(
            "{} is not a real app directory",
            path.display()
        )));
    }
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    Ok(file.into())
}

#[cfg(target_os = "macos")]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path holds a NUL"))
}

/// Clone the real directory opened at `from`, never a replaced pathname or a source link.
/// The caller owns the clone. Internal links are copied as links and must be sealed before use.
///
/// # Errors
/// Unsafe outer shape, an open/clone error (including unchanged `EXDEV`), or unsupported OS.
pub fn clone(from: &Path, to: &Path) -> Result<(), Error> {
    #[cfg(target_os = "macos")]
    {
        let source = directory(from)?;
        let to = c_path(to)?;
        unsafe extern "C" {
            /// `<sys/clonefile.h>`.
            fn fclonefileat(
                srcfd: i32,
                dst_dirfd: i32,
                dst: *const std::ffi::c_char,
                flags: u32,
            ) -> i32;
        }
        // CLONE_NOFOLLOW | CLONE_NOOWNERCOPY: links stay links, and the clone is the caller's.
        // SAFETY: source is a live directory descriptor, and to is NUL-terminated for the call.
        if unsafe { fclonefileat(source.as_raw_fd(), libc::AT_FDCWD, to.as_ptr(), 0x1 | 0x2) } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (from, to);
        Err(io::Error::from(io::ErrorKind::Unsupported).into())
    }
}

fn confined(root: &Path, link: &Path) -> Result<(), Error> {
    let mut pending: VecDeque<_> = link
        .strip_prefix(root)
        .map_err(|_| Error::Refused("a link is outside the sealed image".into()))?
        .components()
        .map(|part| part.as_os_str().to_owned())
        .collect();
    let mut resolved = PathBuf::new();
    let mut followed = 0_u8;
    while let Some(part) = pending.pop_front() {
        match Path::new(&part).components().next() {
            Some(Component::CurDir) | None => continue,
            Some(Component::ParentDir) => {
                if !resolved.pop() {
                    return Err(Error::Refused(format!(
                        "{} leads out of the app image",
                        link.display()
                    )));
                }
                continue;
            }
            Some(Component::RootDir | Component::Prefix(_)) => {
                return Err(Error::Refused(format!(
                    "{} has an absolute link target",
                    link.display()
                )));
            }
            Some(Component::Normal(_)) => {}
        }
        // Only normalized paths beneath root are observed. In particular, never
        // canonicalize an attacker-controlled target through an outside directory.
        let at = root.join(&resolved).join(&part);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => {
                followed += 1;
                if followed > 40 {
                    return Err(Error::Refused(
                        "an app image link chain does not terminate".into(),
                    ));
                }
                let target = std::fs::read_link(&at)?;
                for component in target.components().rev() {
                    pending.push_front(component.as_os_str().to_owned());
                }
            }
            Ok(_) => resolved.push(&part),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                // Missing, but still confined: existence is the code checker's
                // concern, and a dangling internal link is no outside authority.
                resolved.push(&part);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Seal a real outer directory for `uid:gid`, clearing macOS ACLs, set-id/sticky and shared-write
/// bits. Recursive seals refuse every escaping link without following it; each directory's
/// grants are removed before visiting its children. A nonrecursive seal changes only the root.
///
/// # Errors
/// Unsafe outer shape or escaping link, or the first filesystem/ACL operation that fails.
pub fn seal(root: &Path, uid: u32, gid: u32, recursive: bool) -> Result<(), Error> {
    let held = std::fs::File::from(directory(root)?);
    // The descriptor pins the outer directory through the walk. The caller keeps its staging
    // parent private; a replaced outer name is never accepted as the opened image.
    let opened = held.metadata()?;
    let named = std::fs::symlink_metadata(root)?;
    if !named.is_dir() || named.dev() != opened.dev() || named.ino() != opened.ino() {
        return Err(Error::Refused(
            "the opened app directory was replaced before sealing".into(),
        ));
    }
    seal_at(root, uid, gid, recursive, root)
}

fn seal_at(path: &Path, uid: u32, gid: u32, recursive: bool, root: &Path) -> Result<(), Error> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        confined(root, path)?;
    }
    std::os::unix::fs::lchown(path, Some(uid), Some(gid))?;
    strip_acl(path, meta.file_type().is_symlink())?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(meta.mode() & 0o755))?;
    if recursive && meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            seal_at(&entry?.path(), uid, gid, true, root)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
const ACL_TYPE_EXTENDED: u32 = 0x0000_0100;
#[cfg(target_os = "macos")]
const O_SYMLINK: i32 = 0x0020_0000;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn acl_init(count: i32) -> *mut std::ffi::c_void;
    fn acl_set_link_np(path: *const std::ffi::c_char, kind: u32, acl: *mut std::ffi::c_void)
    -> i32;
    fn acl_set_fd_np(fd: i32, acl: *mut std::ffi::c_void, kind: u32) -> i32;
    fn acl_free(acl: *mut std::ffi::c_void) -> i32;
}

fn strip_acl(path: &Path, link: bool) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let held = if link {
            Some(
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(O_SYMLINK | libc::O_NONBLOCK | libc::O_CLOEXEC)
                    .open(path)?,
            )
        } else {
            None
        };
        let path = c_path(path)?;
        // SAFETY: acl_init returns an owned empty ACL or NULL with errno set.
        let empty = unsafe { acl_init(0) };
        if empty.is_null() {
            return Err(io::Error::last_os_error());
        }
        let set = match &held {
            // SAFETY: the descriptor and ACL are live; the link itself is open, not its target.
            Some(file) => unsafe { acl_set_fd_np(file.as_raw_fd(), empty, ACL_TYPE_EXTENDED) },
            // SAFETY: the path and ACL are live, and acl_set_link_np never follows a final link.
            None => unsafe { acl_set_link_np(path.as_ptr(), ACL_TYPE_EXTENDED, empty) },
        };
        let error = io::Error::last_os_error();
        // SAFETY: empty came from acl_init and is freed exactly once after its last use.
        unsafe { acl_free(empty) };
        if set == 0 { Ok(()) } else { Err(error) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, link);
        Ok(())
    }
}
