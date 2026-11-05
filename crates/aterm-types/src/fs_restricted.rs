// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Filesystem helpers with restricted permissions (mode 0o700; CWE-276).

use std::io;
use std::path::Path;

/// Create a directory (and parents) with mode 0o700 on Unix; elsewhere plain `create_dir_all`.
pub fn create_dir_restricted(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::DirBuilder;
        use std::os::unix::fs::DirBuilderExt;
        DirBuilder::new().recursive(true).mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// The mode a missing ancestor of one of aterm's own directories is born with ([`create_dir_all_unshared`]): what a `022` umask gives it, and never the group or other write bit a plain `create_dir_all` leaves under `002` (a user-private-group login's default) or `000`.
pub const ANCESTOR_DIR_MODE: u32 = 0o755;

/// Create `dir` and its missing ancestors so that no directory this call makes is group- or other-writable, whatever the umask: each missing ancestor is born [`ANCESTOR_DIR_MODE`] and `dir` itself `leaf_mode`, both narrowed further by the umask (a tighter umask stays tighter).
pub fn create_dir_all_unshared(dir: &Path, leaf_mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::DirBuilder;
        use std::os::unix::fs::DirBuilderExt;
        if dir.as_os_str().is_empty() {
            return Ok(());
        }
        let is_real_dir =
            |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir());
        match DirBuilder::new().mode(leaf_mode).create(dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) if is_real_dir(dir) => return Ok(()),
            Err(e) => return Err(no_real_dir(dir, e)),
        }
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            DirBuilder::new()
                .recursive(true)
                .mode(ANCESTOR_DIR_MODE)
                .create(parent)?;
        }
        match DirBuilder::new().mode(leaf_mode).create(dir) {
            Err(_) if is_real_dir(dir) => Ok(()),
            Err(e) => Err(no_real_dir(dir, e)),
            made => made,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = leaf_mode;
        std::fs::create_dir_all(dir)
    }
}

/// `e`, the refusal to make `dir`, said plainly where `dir` is there but is no real directory: a
/// symbolic link (which a follow would take somewhere else) or another file. mkdir's own word
/// for both is `File exists`.
#[cfg(unix)]
fn no_real_dir(dir: &Path, e: io::Error) -> io::Error {
    let what = match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() => "a symbolic link, not a real directory",
        Ok(m) if !m.is_dir() => "a file, not a directory",
        _ => return e,
    };
    io::Error::new(e.kind(), format!("{}: {what}", dir.display()))
}

/// Create `dir` (and parents) if absent, force mode `0700`, and verify it is ours and not group/other-writable: the control socket's private-dir rule, shared by the session-identity tree.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if std::fs::symlink_metadata(dir).is_ok_and(|m| !m.file_type().is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: must be a real directory, not a link", dir.display()),
        ));
    }
    create_dir_restricted(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let before = std::fs::symlink_metadata(dir)?;
        if !before.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{}: must be a real directory, not a link", dir.display()),
            ));
        }
        let uid = current_uid();
        if before.uid() == uid && before.mode() & 0o7777 != 0o700 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let meta = std::fs::symlink_metadata(dir)?;
        if !meta.file_type().is_dir() || !dir_safe_for_private_write(uid, meta.uid(), meta.mode()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{}: must be owned by uid {uid} and not group/other-writable",
                    dir.display()
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid() is always safe — no failure mode, no args.
    unsafe { libc_getuid() }
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// Whether the caller (`uid`) may write private files into a directory owned by `owner_uid` with `st_mode` bits `mode`: owned by the caller and not group/other-writable.
#[must_use]
pub const fn dir_safe_for_private_write(uid: u32, owner_uid: u32, mode: u32) -> bool {
    owner_uid == uid && mode & 0o022 == 0
}

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "riscv64",
        target_arch = "aarch64",
        target_arch = "arm",
    ),
))]
const NONBLOCK_OPEN_FLAG: i32 = 0x0000_0800 | 0x0008_0000;

#[cfg(any(target_os = "macos", target_os = "ios"))]
const NONBLOCK_OPEN_FLAG: i32 = 0x0000_0004 | 0x0100_0000;

#[cfg(any(
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
))]
const NONBLOCK_OPEN_FLAG: i32 = 0x0000_0004;

/// Flags passed to `OpenOptionsExt::custom_flags` for exclusive file creation (`O_NONBLOCK | O_CLOEXEC`).
#[cfg(unix)]
pub(crate) const CREATE_NEW_OPEN_FLAG: i32 = NONBLOCK_OPEN_FLAG;

/// Open `path` without blocking on FIFOs and verify the opened descriptor is a regular file.
pub(crate) fn open_regular_nonblock_with_meta(
    path: &Path,
) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    #[cfg(any(
        all(
            any(target_os = "linux", target_os = "android"),
            any(
                target_arch = "x86",
                target_arch = "x86_64",
                target_arch = "riscv64",
                target_arch = "aarch64",
                target_arch = "arm",
            ),
        ),
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ))]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(NONBLOCK_OPEN_FLAG)
            .open(path)?
    };
    #[cfg(not(any(
        all(
            any(target_os = "linux", target_os = "android"),
            any(
                target_arch = "x86",
                target_arch = "x86_64",
                target_arch = "riscv64",
                target_arch = "aarch64",
                target_arch = "arm",
            ),
        ),
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    )))]
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((file, meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a directory we own that nobody else can write is safe; group or other READ is the caller's choice.
    #[test]
    fn private_write_needs_our_ownership_and_no_foreign_write_bit() {
        for (uid, owner, mode, safe) in [
            (501, 501, 0o040700, true),
            (501, 501, 0o040755, true),
            (0, 0, 0o040700, true),
            (501, 0, 0o041777, false),
            (501, 502, 0o040700, false),
            (501, 501, 0o040775, false),
            (501, 501, 0o040757, false),
            (501, 501, 0o040722, false),
        ] {
            assert_eq!(
                dir_safe_for_private_write(uid, owner, mode),
                safe,
                "uid {uid} owner {owner} mode {mode:o}"
            );
        }
    }

    /// `ensure_private_dir` creates 0700 (parents included), tightens a looser dir of ours, and is idempotent.
    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_creates_0700_tightens_and_is_idempotent() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "aterm-fs-restricted-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
        ));
        let dir = root.join("identities").join("worker");
        ensure_private_dir(&dir).expect("created");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(
            mode(&root.join("identities")),
            0o700,
            "parents are 0700 too"
        );
        ensure_private_dir(&dir).expect("idempotent");
        assert_eq!(mode(&dir), 0o700);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).expect("an existing dir of ours is tightened, not refused");
        assert_eq!(mode(&dir), 0o700);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An already-`0700` directory of ours gets no `chmod(2)`, so its `st_ctime` holds; a drifted mode is still forced back to `0700`.
    #[cfg(unix)]
    #[test]
    fn an_already_private_dir_keeps_its_ctime_and_a_drifted_one_is_hardened() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("atypes-privdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let stamp = || {
            let m = std::fs::metadata(&dir).unwrap();
            (m.ctime(), m.ctime_nsec(), m.mode() & 0o7777)
        };
        let before = stamp();
        assert_eq!(before.2, 0o700);
        std::thread::sleep(std::time::Duration::from_millis(20));
        ensure_private_dir(&dir).unwrap();
        assert_eq!(stamp(), before, "an unchanged directory was written");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(stamp().2, 0o700, "a drifted mode is hardened");
        let outside = std::env::temp_dir().join(format!("atypes-outside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = std::env::temp_dir().join(format!("atypes-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());
        assert_eq!(
            std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A leaf that is a symbolic link or a file is refused in words that name it, with mkdir's
    /// kind, and the link's target is neither made private nor written.
    #[cfg(unix)]
    #[test]
    fn a_leaf_that_is_no_real_directory_is_refused_by_name() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("atypes-noreal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let target = root.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let e = create_dir_all_unshared(&link, 0o700).expect_err("a link is no directory of ours");
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            e.to_string()
                .contains("a symbolic link, not a real directory"),
            "{e}"
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let file = root.join("file");
        std::fs::write(&file, b"").unwrap();
        let e = create_dir_all_unshared(&file, 0o700).expect_err("a file is no directory");
        assert!(e.to_string().contains("a file, not a directory"), "{e}");
        // CONTROL: a real directory is taken as it is.
        create_dir_all_unshared(&target, 0o700).expect("a real directory");
        let _ = std::fs::remove_dir_all(&root);
    }
}
