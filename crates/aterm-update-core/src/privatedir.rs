// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Creating and hardening a per-user private directory (`0700`, owned by us, never
//! group/other-writable) using [`aterm_types::fs_restricted::dir_safe_for_private_write`].

use std::io;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

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
// Skip: set_permissions/format on the 0700 dir; the mode and owner checks are what this fn
// enforces.
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
    // Create missing ancestors as `0755 & ~umask` so open umasks (`002`) never leave them group-writable.
    aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o700)?;
    // Skip `chmod(2)` when already `0700` to avoid bumping `st_ctime` on APFS.
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.mode() & 0o7777 == 0o700) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
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
    if aterm_types::fs_restricted::dir_safe_for_private_write(our_uid(), meta.uid(), meta.mode()) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update directory must be owned by uid {} and not group/other-writable",
                dir.display(),
                our_uid()
            ),
        ))
    }
}

/// Windows variant: create `dir` and refuse a final component that is a symlink or junction.
#[cfg(windows)]
// Skip: format over the refusal messages; the reparse-point checks are what this fn enforces.
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
        let dir = std::env::temp_dir().join(format!("atuc-privatedir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let stamp = || {
            let m = std::fs::symlink_metadata(&dir).unwrap();
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

        let link = dir.with_extension("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err(), "a symlink is refused");
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&dir);
    }

    const OPEN_UMASK_ENV: &str = "ATUC_PRIVATEDIR_OPEN_UMASK_DIR";
    const OPEN_UMASK_PROBE: &str =
        "privatedir::tests::probe_missing_ancestors_are_never_shared_whatever_the_umask";

    #[test]
    fn probe_missing_ancestors_are_never_shared_whatever_the_umask() {
        let Some(root) = std::env::var_os(OPEN_UMASK_ENV) else {
            return;
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
    }

    #[test]
    fn missing_ancestors_are_never_shared_whatever_the_umask() {
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
