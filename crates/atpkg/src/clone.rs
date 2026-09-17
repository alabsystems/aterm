// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! COPY-ON-WRITE CLONES of a build's files — the construction behind the rustup view
//! ([`crate::seam`]) and the per-build exec roots ([`crate::compat`]).
//!
//! # Why not hard links (the owner, 2026-09-16)
//!
//! *"Trust and rustc are different … hard linking? that sounds stupid … doing that with
//! hardlinks sounds like bugs and indeed: bugs."* Both views used to be HARD-LINK MIRRORS:
//! every file of a view was one of the STORE's own inodes under a second name. That made
//! the store's inodes part of every view's lifecycle, and each consequence was a measured
//! defect:
//!
//! * a link or an unlink is a write to the store inode's link count and ctime, and tippy
//!   pins its own executable's and its siblings' — so laying, rebuilding or retiring any
//!   view aborted every tippy running from the store (measured 2026-09-15);
//! * a link made by a provenance-tracked process is a provenance write on the store file
//!   itself, so the view had to be laid through an untracked launchd lane or it tagged
//!   the toolchain;
//! * identity had to be proven by `(dev, ino)` — in Rust, and in every routed shim with a
//!   `-ef` test — and a view could never outlive, or differ from, the store it shared.
//!
//! A CLONE has none of that. On APFS, `std::fs::copy` is `fclonefileat(2)`: a new inode
//! whose data blocks are shared copy-on-write with the source, with the source's mode,
//! length and modification time, costing only metadata (bundle 8595, 4,114 files and
//! 2.6 GB, cloned for about 2 MB, measured 2026-09-16). Nothing about the store changes:
//! not a link count, not a ctime, not an extended attribute. The clone runs exactly as the
//! hard link did — the Trust frontends choose their behaviour by the name they are invoked
//! under and find their siblings and sysroot by their own path, and a clone is a real file
//! at a real path (8595's tippy lints from a cloned view, and `trustc --print sysroot`
//! answers the view).
//!
//! # Rules
//!
//! * A clone never silently becomes a copy of a toolchain. `std::fs::copy` falls back to a
//!   byte copy across volumes, so [`clone_file`] refuses first when the destination
//!   directory is not on the source's device.
//! * Elsewhere than macOS the same call is `copy_file_range(2)`: a reflink on a
//!   copy-on-write filesystem (btrfs, XFS), a real copy otherwise. No Linux bundle ever
//!   needs an exec root ([`crate::compat::needs_root`] — its copies are byte-identical),
//!   so what that costs is the rustup view alone.
//! * [`is_clone_of`] is the identity: a regular file, not a symlink, with the source's
//!   length, permission bits and modification time — and NOT the source's inode. The last
//!   clause is what retires the hard-link views: one laid before this module reads as a
//!   mismatch and is rebuilt once, as clones, and the store's link counts come back.

use std::io;
use std::path::Path;

/// Clone the regular file `src` to `dst`, which must not exist: its data shared
/// copy-on-write where the filesystem can, its permission bits and modification time
/// kept.
///
/// # Errors
/// `src` is not a regular file, `dst` exists, `dst`'s directory is on another device than
/// `src` (a clone cannot cross a volume, and a toolchain is never byte-copied in its
/// place), or the copy itself fails.
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
    // `fclonefileat` keeps the time already; a reflink or a byte copy may not, and the
    // identity reads it.
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
    if s.dev() == p.dev() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::CrossesDevices,
        format!(
            "cannot clone {} into {}: they are on different volumes, and atpkg never copies \
             a toolchain in place of a clone",
            src.display(),
            parent.display()
        ),
    ))
}

#[cfg(not(unix))]
fn same_device(_src: &Path, _dst: &Path) -> io::Result<()> {
    Ok(())
}

/// Whether `at` is a clone of the regular file `src`: by `lstat`, a regular file (never a
/// symlink) with `src`'s length, permission bits and modification time — and a different
/// inode, so a hard link to `src` is NOT a clone of it (see the module doc).
///
/// Attributes, not bytes: this runs over every file of a view on every update pass. It
/// cannot tell a clone of `src` from another file that happens to share all three — and
/// bundle 8595 ships exactly such a file, a separately signed `bin/rustc` with `trustc`'s
/// length, mode and time and 2,428 different bytes. Where that matters, the three stock
/// names, the deep check reads the bytes too ([`same_bytes`]).
#[must_use]
pub(crate) fn is_clone_of(src: &Path, at: &Path) -> bool {
    let (Ok(s), Ok(a)) = (
        std::fs::symlink_metadata(src),
        std::fs::symlink_metadata(at),
    ) else {
        return false;
    };
    attributes_match(&s, &a) && distinct_inodes(&s, &a)
}

/// Whether `a` and `b` are regular files (never symlinks) with the same length, permission
/// bits and modification time, whatever their inodes — a clone of the other, or the same
/// file. What identifies the build a view was laid from, including a view laid as hard
/// links before clones.
#[must_use]
pub(crate) fn same_attributes(a: &Path, b: &Path) -> bool {
    let (Ok(x), Ok(y)) = (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) else {
        return false;
    };
    attributes_match(&x, &y)
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
            // `a` ended: equal exactly when `b` ends too.
            return Ok(fb.read(&mut bb[..1])? == 0);
        }
        fb.read_exact(&mut bb[..n])?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
    }
}

#[cfg(unix)]
fn distinct_inodes(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    (a.dev(), a.ino()) != (b.dev(), b.ino())
}

#[cfg(not(unix))]
fn distinct_inodes(_a: &std::fs::Metadata, _b: &std::fs::Metadata) -> bool {
    true
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atpkg-clone-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        // The bundle's shape: the same length, one different byte deep inside.
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
            same_attributes(&trustc, &rustc),
            "length, mode and time cannot tell them apart"
        );
        assert!(is_clone_of(&trustc, &rustc), "nor can the clone identity");
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
