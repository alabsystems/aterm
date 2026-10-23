// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Copy-on-write clones of a build's files for the rustup view ([`crate::seam`])
//! and per-build exec roots ([`crate::compat`]).
//!
//! Uses `std::fs::copy` within a single volume on Unix (`fclonefileat` on APFS,
//! `copy_file_range` on Linux) and verifies clone identity by matching length,
//! permissions, and mtime on a distinct inode.

use std::io;
use std::path::Path;

/// OS device identifier (`st_dev` on Unix).
type VolumeId = u64;

/// Clones regular file `src` to `dst` on the same volume, preserving permissions and mtime.
///
/// # Errors
/// `src` is not a regular file, `dst` exists, `dst`'s directory is on another device than
/// `src` (a toolchain is never byte-copied in a clone's place), or the copy fails. Off Unix
/// it always refuses with [`io::ErrorKind::Unsupported`].
pub(crate) fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is not a regular file; only a file is cloned",
                src.display()
            ),
        ));
    }
    if std::fs::symlink_metadata(dst).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists; a clone is laid only at a free name",
                dst.display()
            ),
        ));
    }
    same_device(src, dst)?;
    std::fs::copy(src, dst).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("cannot clone {} to {} ({e})", src.display(), dst.display()),
        )
    })?;
    let modified = meta.modified()?;
    let file = std::fs::OpenOptions::new().write(true).open(dst)?;
    file.set_modified(modified)?;
    drop(file);
    std::fs::set_permissions(dst, meta.permissions())?;
    Ok(())
}

/// Refuse when `dst`'s parent directory is not on `src`'s device.
#[cfg(unix)]
fn same_device(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let (s, p) = (
        std::fs::symlink_metadata(src)?,
        std::fs::symlink_metadata(parent)?,
    );
    one_volume(src, parent, Some(s.dev()), Some(p.dev()))
}

/// Always refuses off Unix where volume identity and copy-on-write cloning are unavailable.
#[cfg(not(unix))]
fn same_device(src: &Path, dst: &Path) -> io::Result<()> {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    one_volume(src, parent, None, None)
}

/// Returns `Ok(())` only when both volume IDs are known and equal.
fn one_volume(
    src: &Path,
    parent: &Path,
    a: Option<VolumeId>,
    b: Option<VolumeId>,
) -> io::Result<()> {
    match (a, b) {
        (Some(x), Some(y)) if x == y => Ok(()),
        (Some(_), Some(_)) => Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            format!(
                "cannot clone {} into {}: they are on different volumes, and atpkg never copies \
                 a toolchain in place of a clone",
                src.display(),
                parent.display()
            ),
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "cannot clone {} into {}: atpkg cannot tell which volume either is on off Unix \
                 (no st_dev on stable Rust), and off Unix std::fs::copy is a byte copy on one \
                 volume and across two alike — so nothing laid here would be a clone, and atpkg \
                 never copies a toolchain in place of a clone",
                src.display(),
                parent.display()
            ),
        )),
    }
}

/// Whether `at` is a regular file with `src`'s length, permissions, and mtime on a distinct inode.
#[must_use]
pub(crate) fn is_clone_of(src: &Path, at: &Path) -> bool {
    let (Ok(s), Ok(a)) = (
        std::fs::symlink_metadata(src),
        std::fs::symlink_metadata(at),
    ) else {
        return false;
    };
    attributes_match(&s, &a) && distinct_files(file_identity(&s), file_identity(&a))
}

fn attributes_match(s: &std::fs::Metadata, a: &std::fs::Metadata) -> bool {
    if !s.is_file() || !a.is_file() {
        return false;
    }
    if s.len() != a.len() || s.permissions() != a.permissions() {
        return false;
    }
    matches!((s.modified(), a.modified()), (Ok(x), Ok(y)) if x == y)
}

/// Whether the regular files `a` and `b` hold the same bytes, read in bounded chunks.
///
/// # Errors
/// Either file cannot be opened or read. A length difference is `Ok(false)` without a read.
pub(crate) fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    use std::io::Read as _;
    let (ma, mb) = (std::fs::symlink_metadata(a)?, std::fs::symlink_metadata(b)?);
    if !ma.is_file() || !mb.is_file() || ma.len() != mb.len() {
        return Ok(false);
    }
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let n = fa.read(&mut ba)?;
        if n == 0 {
            return Ok(fb.read(&mut bb[..1])? == 0);
        }
        fb.read_exact(&mut bb[..n])?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
    }
}

/// File identity `(dev, ino)` used to distinguish a clone from a hard link.
type FileId = (u64, u64);

#[cfg(unix)]
fn file_identity(m: &std::fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some((m.dev(), m.ino()))
}

/// Returns `None` off Unix where stable Rust exposes no file identity.
#[cfg(not(unix))]
fn file_identity(_m: &std::fs::Metadata) -> Option<FileId> {
    None
}

/// Whether `a` and `b` are provably distinct files (`Some` and unequal).
fn distinct_files(a: Option<FileId>, b: Option<FileId>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x != y)
}

/// Platform-independent tests for [`distinct_files`] and [`one_volume`].
#[cfg(test)]
mod decisions {
    use super::*;

    #[test]
    fn an_unreadable_file_identity_is_not_a_difference() {
        let a: FileId = (66_306, 1_594_985);
        let b: FileId = (66_306, 1_594_986);
        assert!(distinct_files(Some(a), Some(b)), "two inodes are two files");
        assert!(
            !distinct_files(Some(a), Some(a)),
            "one inode under two names is a HARD LINK, not a clone"
        );
        assert!(
            !distinct_files(None, None),
            "a platform that cannot read a file's identity has not proven two files apart, \
             and `is_clone_of` must therefore answer `false` rather than collapse into \
             `attributes_match` and read a hard link into the store as a clone"
        );
        assert!(!distinct_files(Some(a), None));
        assert!(!distinct_files(None, Some(a)));
    }

    #[test]
    fn the_volume_verdict_refuses_what_it_cannot_prove() {
        let (src, parent) = (
            Path::new("/store/trust/9192/bin/trustc"),
            Path::new("/rustup/trust/bin"),
        );
        assert!(one_volume(src, parent, Some(66_306), Some(66_306)).is_ok());

        let crossed = one_volume(src, parent, Some(66_306), Some(46)).unwrap_err();
        assert_eq!(crossed.kind(), io::ErrorKind::CrossesDevices);

        let unknown = one_volume(src, parent, None, None).unwrap_err();
        assert_eq!(
            unknown.kind(),
            io::ErrorKind::Unsupported,
            "an unprovable volume is a refusal, not a pass"
        );
        assert_eq!(
            one_volume(src, parent, Some(66_306), None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }

    /// Off Unix the platform answers are the ones pinned above, and `clone_file` refuses.
    #[cfg(not(unix))]
    #[test]
    fn off_unix_the_platform_answers_are_unknown_and_a_clone_is_refused() {
        let here = std::env::current_exe().unwrap();
        let m = std::fs::symlink_metadata(&here).unwrap();
        assert!(
            file_identity(&m).is_none(),
            "no stable std API reads a Windows file id"
        );
        let refused = clone_file(&here, &here.with_extension("clone")).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::Unsupported);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-clone-{label}-"))
    }

    #[test]
    fn a_clone_keeps_length_mode_and_time_and_takes_its_own_inode() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = scratch("keeps");
        let src = dir.join("trustc");
        std::fs::write(&src, b"the Trust compiler").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let dst = dir.join("rustc");
        clone_file(&src, &dst).unwrap();
        let (s, d) = (
            std::fs::metadata(&src).unwrap(),
            std::fs::metadata(&dst).unwrap(),
        );
        assert_eq!(std::fs::read(&dst).unwrap(), b"the Trust compiler");
        assert_eq!(d.permissions().mode() & 0o7777, 0o755);
        assert_eq!(d.modified().unwrap(), old, "the time is the source's");
        assert_ne!(s.ino(), d.ino(), "a clone is its own inode");
        assert_eq!(s.nlink(), 1, "the source gained no link");
        assert!(is_clone_of(&src, &dst));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hard_link_a_symlink_and_a_changed_file_are_not_clones() {
        let dir = scratch("not");
        let src = dir.join("tippy");
        std::fs::write(&src, b"tippy").unwrap();
        let built =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(built)
            .unwrap();
        let link = dir.join("hard");
        std::fs::hard_link(&src, &link).unwrap();
        assert!(
            !is_clone_of(&src, &link),
            "a hard link is the store's own inode, which is exactly what a view must not be"
        );
        let sym = dir.join("sym");
        std::os::unix::fs::symlink(&src, &sym).unwrap();
        assert!(!is_clone_of(&src, &sym));
        let clone = dir.join("clone");
        clone_file(&src, &clone).unwrap();
        assert!(is_clone_of(&src, &clone));
        std::fs::write(&clone, b"tipsy").unwrap();
        assert!(
            !is_clone_of(&src, &clone),
            "a rewritten clone no longer is one"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_bytes_sees_a_signature_only_difference_that_the_attributes_cannot() {
        let dir = scratch("bytes");
        let trustc = dir.join("trustc");
        let rustc = dir.join("rustc");
        let mut image = vec![7u8; 200_000];
        std::fs::write(&trustc, &image).unwrap();
        image[150_000] = 8;
        std::fs::write(&rustc, &image).unwrap();
        let t = std::fs::metadata(&trustc).unwrap().modified().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&rustc)
            .unwrap()
            .set_modified(t)
            .unwrap();
        assert!(
            is_clone_of(&trustc, &rustc),
            "length, mode and time cannot tell them apart, so neither can the clone identity"
        );
        assert!(!same_bytes(&trustc, &rustc).unwrap(), "the bytes can");
        let clone = dir.join("clone");
        clone_file(&trustc, &clone).unwrap();
        assert!(same_bytes(&trustc, &clone).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clone_refuses_an_occupied_name_and_a_non_file() {
        let dir = scratch("refuse");
        let src = dir.join("targo");
        std::fs::write(&src, b"targo").unwrap();
        let dst = dir.join("cargo");
        std::fs::write(&dst, b"a copy the bundle shipped").unwrap();
        assert_eq!(
            clone_file(&src, &dst).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&dst).unwrap(), b"a copy the bundle shipped");
        assert_eq!(
            clone_file(&dir, &dir.join("d")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
