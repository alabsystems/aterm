// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Creating and hardening a per-user private directory (`0700`, owned by us, never
//! group/other-writable) using [`aterm_types::fs_restricted::dir_safe_for_private_write`].

use std::io;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

#[cfg(all(test, unix))]
thread_local! {
    static CHMOD_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Our effective uid.
#[cfg(unix)]
#[inline]
fn our_uid() -> u32 {
    // SAFETY: getuid() is always-safe (no args, cannot fail).
    unsafe { libc::getuid() }
}

/// Create `dir` (and parents), force mode `0700`, then verify it is owned by us and not
/// group/other-writable, refusing a foreign-owned or shared directory.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    // Refuse a pre-existing symlink before create_dir_all or set_permissions can follow it.
    if let Ok(md) = std::fs::symlink_metadata(dir)
        && md.file_type().is_symlink()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: update directory is a symlink; refusing", dir.display()),
        ));
    }
    aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o700)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update path is not a real directory; refusing",
                dir.display()
            ),
        ));
    }
    let uid = our_uid();
    if meta.uid() == uid && meta.mode() & 0o7777 != 0o700 {
        #[cfg(all(test, unix))]
        CHMOD_CALLS.with(|c| c.set(c.get() + 1));
        set_dir_mode_nofollow(dir, 0o700)?;
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update path is not a real directory; refusing",
                dir.display()
            ),
        ));
    }
    if aterm_types::fs_restricted::dir_safe_for_private_write(uid, meta.uid(), meta.mode()) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update directory must be owned by uid {uid} and not group/other-writable",
                dir.display()
            ),
        ))
    }
}

#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn set_dir_mode_nofollow(dir: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let perms = std::fs::Permissions::from_mode(mode);
    match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir)
    {
        Ok(f) => f.set_permissions(perms),
        Err(_)
            if std::fs::symlink_metadata(dir)
                .is_ok_and(|m| m.file_type().is_dir() && m.uid() == our_uid()) =>
        {
            std::fs::set_permissions(dir, perms)
        }
        Err(e) => Err(e),
    }
}

/// Windows variant: create `dir` and refuse a final component that is a symlink or junction.
#[cfg(windows)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if let Ok(md) = std::fs::symlink_metadata(dir)
        && is_reparse_point(&md)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update directory is a symlink/junction; refusing",
                dir.display()
            ),
        ));
    }
    std::fs::create_dir_all(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() || is_reparse_point(&meta) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update path is not a real directory; refusing",
                dir.display()
            ),
        ));
    }
    Ok(())
}

/// Whether `md` is a symlink or directory junction (`FILE_ATTRIBUTE_REPARSE_POINT`).
#[cfg(windows)]
fn is_reparse_point(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    md.file_type().is_symlink() || (md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn an_already_private_dir_is_not_rewritten_and_a_drifted_one_is_hardened() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("atuc-privatedir-")
            .tempdir()
            .unwrap();
        let dir = scratch.path().join("private");
        ensure_private_dir(&dir).unwrap();
        let stamp = || {
            let m = std::fs::symlink_metadata(&dir).unwrap();
            (
                m.dev(),
                m.ino(),
                m.ctime(),
                m.ctime_nsec(),
                m.mode() & 0o7777,
            )
        };
        let before = stamp();
        assert_eq!(before.4, 0o700);
        CHMOD_CALLS.with(|c| c.set(0));
        ensure_private_dir(&dir).unwrap();
        assert_eq!(
            CHMOD_CALLS.with(std::cell::Cell::get),
            0,
            "an unchanged 0700 directory must not run chmod"
        );
        assert_eq!(stamp(), before, "an unchanged directory was written");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        CHMOD_CALLS.with(|c| c.set(0));
        ensure_private_dir(&dir).unwrap();
        assert_eq!(
            CHMOD_CALLS.with(std::cell::Cell::get),
            1,
            "a drifted mode triggers one chmod"
        );
        assert_eq!(stamp().4, 0o700, "a drifted mode is hardened");

        let link = scratch.path().join("private.link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        CHMOD_CALLS.with(|c| c.set(0));
        assert!(ensure_private_dir(&link).is_err(), "a symlink is refused");
        assert_eq!(CHMOD_CALLS.with(std::cell::Cell::get), 0);

        let file = scratch.path().join("regular.file");
        std::fs::write(&file, b"x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        CHMOD_CALLS.with(|c| c.set(0));
        assert!(
            ensure_private_dir(&file).is_err(),
            "a regular file is refused"
        );
        assert_eq!(CHMOD_CALLS.with(std::cell::Cell::get), 0);
        assert_eq!(
            std::fs::symlink_metadata(&file).unwrap().mode() & 0o7777,
            0o644
        );
    }

    const OPEN_UMASK_ENV: &str = "ATUC_PRIVATEDIR_OPEN_UMASK_DIR";
    const OPEN_UMASK_PROBE: &str =
        "privatedir::tests::missing_ancestors_are_never_shared_whatever_the_umask";

    fn probe_missing_ancestors_are_never_shared_whatever_the_umask_if_asked() -> bool {
        let Some(root) = std::env::var_os(OPEN_UMASK_ENV) else {
            return false;
        };
        let mode = |p: &Path| {
            format!(
                "{:o}",
                std::fs::symlink_metadata(p).unwrap().mode() & 0o7777
            )
        };
        let existing = Path::new(&root).join("existing");
        std::fs::create_dir(&existing).unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o775)).unwrap();
        for (umask, ancestor) in [(0o002, "755"), (0o000, "755"), (0o077, "700")] {
            // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail;
            // this process runs this one test (`--exact`), so no other create sees the mask.
            unsafe { libc::umask(umask) };
            let top = existing.join(format!("umask-{umask:03o}"));
            let leaf = top.join("aterm").join("pkg");
            ensure_private_dir(&leaf).unwrap();
            for made in [&top, &top.join("aterm")] {
                assert_eq!(
                    mode(made),
                    ancestor,
                    "{}: a missing ancestor under umask {umask:03o}",
                    made.display()
                );
            }
            assert_eq!(mode(&leaf), "700", "the leaf under umask {umask:03o}");
        }
        assert_eq!(mode(&existing), "775", "an existing ancestor was rewritten");
        true
    }

    #[test]
    fn missing_ancestors_are_never_shared_whatever_the_umask() {
        if probe_missing_ancestors_are_never_shared_whatever_the_umask_if_asked() {
            return;
        }
        let scratch = aterm_tempfile::Builder::new()
            .prefix("atuc-open-umask-")
            .tempdir()
            .unwrap();
        let out = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", OPEN_UMASK_PROBE])
            .env(OPEN_UMASK_ENV, scratch.path().as_os_str())
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the probe under open umasks: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
