// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole-folder steps for a root-owned copy of an app bundle ([`super::System::clone_tree`],
//! [`super::System::root_own`], [`super::System::exchange`]): APFS `clonefile(2)`, an owner and
//! mode walk that never follows a link, and `renamex_np`'s atomic swap.

use std::io;
use std::path::Path;

#[cfg(target_os = "macos")]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path holds a NUL"))
}

/// `clonefile(2)` of the folder (or file) `from` to the new name `to`, never following a link at
/// `from`, and owned by the caller (`CLONE_NOOWNERCOPY`: a root caller's clone is root's, never
/// the source owner's). Off macOS it refuses (`Unsupported`).
///
/// # Errors
/// The call's error: `EXDEV` when the two are on different volumes, `EEXIST` when `to` exists.
pub(super) fn clone(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        /// `<sys/clonefile.h>`: do not follow a link at the source.
        const CLONE_NOFOLLOW: u32 = 0x0001;
        /// `<sys/clonefile.h>`: do not copy the source's owner when the caller is root.
        const CLONE_NOOWNERCOPY: u32 = 0x0002;
        unsafe extern "C" {
            /// `<sys/clonefile.h>`: `int clonefile(const char *src, const char *dst, uint32_t)`.
            fn clonefile(
                src: *const std::ffi::c_char,
                dst: *const std::ffi::c_char,
                flags: u32,
            ) -> i32;
        }
        let (src, dst) = (c_path(from)?, c_path(to)?);
        // SAFETY: both pointers are NUL-terminated strings that outlive the call, which reads
        // them and writes nothing of ours.
        let done = unsafe {
            clonefile(
                src.as_ptr(),
                dst.as_ptr(),
                CLONE_NOFOLLOW | CLONE_NOOWNERCOPY,
            )
        };
        if done == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (from, to);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Exchange the names `a` and `b` atomically (`renamex_np` with `RENAME_SWAP`): each then names
/// what the other did. Off macOS it refuses (`Unsupported`).
///
/// # Errors
/// The call's error, `ENOENT` when either is missing.
pub(super) fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let (a, b) = (c_path(a)?, c_path(b)?);
        // SAFETY: both pointers are NUL-terminated strings that outlive the call; RENAME_SWAP
        // makes it an exchange of the two existing names.
        if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (a, b);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Make `path` (and, with `tree`, everything below it) `uid:gid` with mode bits no wider than
/// `0755`, never following a link: the group and other write bits, setuid, setgid and sticky are
/// cleared, and any extended ACL is removed ([`strip_acl`]), so a bundle the person's account
/// made can carry no grant of its own into a root-owned clone (`ditto`'s copy keeps ACLs and set-id
/// bits). A link itself is re-owned (`lchown`), stripped of any ACL and never entered, and its mode
/// (which macOS ignores) is left.
///
/// # Errors
/// The first failing `lstat`, `lchown`, ACL removal, `chmod` or listing.
pub(super) fn own(path: &Path, uid: u32, gid: u32, tree: bool) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let meta = std::fs::symlink_metadata(path)?;
    std::os::unix::fs::lchown(path, Some(uid), Some(gid))?;
    strip_acl(path, meta.file_type().is_symlink())?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let mode = meta.mode() & 0o755;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    if tree && meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            own(&entry?.path(), uid, gid, true)?;
        }
    }
    Ok(())
}

/// `<sys/acl.h>`: macOS's NFSv4-style ACL, the only kind it keeps.
#[cfg(target_os = "macos")]
const ACL_TYPE_EXTENDED: u32 = 0x0000_0100;

/// `<sys/fcntl.h>`: open a link at the end of the path itself, not what it names.
#[cfg(target_os = "macos")]
const O_SYMLINK: i32 = 0x0020_0000;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// `<sys/acl.h>`: `acl_t acl_init(int count)`, an empty ACL.
    fn acl_init(count: i32) -> *mut std::ffi::c_void;
    /// `<sys/acl.h>`: `int acl_set_link_np(const char *, acl_type_t, acl_t)`, never following a
    /// link at the end of the path (and refusing a link itself, `ENOTSUP`).
    fn acl_set_link_np(path: *const std::ffi::c_char, kind: u32, acl: *mut std::ffi::c_void)
    -> i32;
    /// `<sys/acl.h>`: `int acl_set_fd_np(int fd, acl_t, acl_type_t)`.
    fn acl_set_fd_np(fd: i32, acl: *mut std::ffi::c_void, kind: u32) -> i32;
    /// `<sys/acl.h>`: `int acl_free(void *)`.
    fn acl_free(obj: *mut std::ffi::c_void) -> i32;
}

/// Replace `path`'s extended ACL with an empty one, never following a link at its end: a `link`
/// is opened itself (`O_SYMLINK`), since `acl_set_link_np` refuses one. Off macOS there is
/// nothing to remove (the clone is made only on macOS).
///
/// # Errors
/// The `acl_init`, `open` or `acl_set_*` error.
fn strip_acl(path: &Path, link: bool) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd as _;
        let held = if link {
            use std::os::unix::fs::OpenOptionsExt as _;
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
        // SAFETY: `acl_init` takes a count and returns an owned ACL or NULL with errno set.
        let empty = unsafe { acl_init(0) };
        if empty.is_null() {
            return Err(io::Error::last_os_error());
        }
        let set = match &held {
            // SAFETY: the descriptor is open for the call, and `empty` is the live ACL
            // `acl_init` returned, which the call only reads.
            Some(file) => unsafe { acl_set_fd_np(file.as_raw_fd(), empty, ACL_TYPE_EXTENDED) },
            // SAFETY: `path` is a NUL-terminated string that outlives the call and `empty` is
            // the live ACL `acl_init` returned, which the call only reads.
            None => unsafe { acl_set_link_np(path.as_ptr(), ACL_TYPE_EXTENDED, empty) },
        };
        let error = io::Error::last_os_error();
        // SAFETY: `empty` came from `acl_init` and is freed once, here, after its last use.
        unsafe { acl_free(empty) };
        if set == 0 { Ok(()) } else { Err(error) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, link);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The walk clears group and other write below the folder and leaves a link's target alone.
    /// NEGATIVE CONTROL: without `tree`, only the folder itself changes.
    #[test]
    fn the_walk_clears_shared_write_and_never_follows_a_link() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = aterm_tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o666)).unwrap();
        let app = dir.path().join("a.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        let exe = app.join("Contents/MacOS/aterm");
        std::fs::write(&exe, b"").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::os::unix::fs::symlink(&outside, app.join("Contents/link")).unwrap();
        let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().mode() & 0o7777;
        let (uid, gid) = {
            let meta = std::fs::metadata(dir.path()).unwrap();
            (meta.uid(), meta.gid())
        };

        own(&app, uid, gid, false).unwrap();
        assert_eq!((mode(&app), mode(&exe)), (0o755, 0o777));
        own(&app, uid, gid, true).unwrap();
        assert_eq!((mode(&app), mode(&exe)), (0o755, 0o755));
        assert_eq!(mode(&outside), 0o666);
    }

    /// The seal removes an ACL the person's account put on a file, a folder or a link, and clears
    /// setuid and setgid: what `ditto` carries into a copy. NEGATIVE CONTROL: before the seal,
    /// `ls -le` shows each ACL and the set-id bits are there.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_seal_removes_acls_and_set_id_bits() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = aterm_tempfile::tempdir().unwrap();
        let app = dir.path().join("a.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let exe = macos.join("aterm");
        let helper = macos.join("helper");
        std::fs::write(&exe, b"").unwrap();
        std::fs::write(&helper, b"").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o4755)).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o2755)).unwrap();
        let link = app.join("Contents/link");
        std::os::unix::fs::symlink("MacOS/aterm", &link).unwrap();
        let chmod = |args: &[&std::ffi::OsStr]| {
            let ran = aterm_uds::spawnfd::command("/bin/chmod")
                .args(args)
                .output()
                .unwrap();
            assert!(ran.status.success(), "{ran:?}");
        };
        let grant = std::ffi::OsStr::new("everyone allow write,delete,append");
        chmod(&["+a".as_ref(), grant, exe.as_os_str()]);
        chmod(&["+a".as_ref(), grant, macos.as_os_str()]);
        chmod(&["-h".as_ref(), "+a".as_ref(), grant, link.as_os_str()]);
        let acls = |p: &Path| {
            let ran = aterm_uds::spawnfd::command("/bin/ls")
                .arg("-led")
                .arg(p)
                .output()
                .unwrap();
            assert!(ran.status.success(), "{ran:?}");
            String::from_utf8_lossy(&ran.stdout)
                .lines()
                .filter(|line| line.trim_start().starts_with("0:"))
                .count()
        };
        let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().mode() & 0o7777;
        assert_eq!((acls(&exe), acls(&macos), acls(&link)), (1, 1, 1));
        assert_eq!((mode(&exe), mode(&helper)), (0o4755, 0o2755));
        let (uid, gid) = {
            let meta = std::fs::metadata(dir.path()).unwrap();
            (meta.uid(), meta.gid())
        };

        own(&app, uid, gid, true).unwrap();
        assert_eq!((acls(&exe), acls(&macos), acls(&link)), (0, 0, 0));
        assert_eq!((mode(&exe), mode(&helper)), (0o755, 0o755));
    }

    /// A clone of a folder holds its files, and an exchange swaps two names whole.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_folder_clones_and_two_folders_swap() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a.app"),
            dir.path().join("b.app"),
            dir.path().join("c.app"),
        );
        std::fs::create_dir_all(a.join("Contents")).unwrap();
        std::fs::write(a.join("Contents/x"), b"a").unwrap();
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(c.join("y"), b"c").unwrap();
        clone(&a, &b).unwrap();
        assert_eq!(std::fs::read(b.join("Contents/x")).unwrap(), b"a");
        assert_eq!(
            clone(&a, &b).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        exchange(&b, &c).unwrap();
        assert_eq!(std::fs::read(b.join("y")).unwrap(), b"c");
        assert_eq!(std::fs::read(c.join("Contents/x")).unwrap(), b"a");
    }
}
