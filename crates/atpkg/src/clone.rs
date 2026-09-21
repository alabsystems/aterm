// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Copy-on-write clones of a build's files — the construction behind the rustup view
//! ([`crate::seam`]) and the per-build exec roots ([`crate::compat`]).
//!
//! # Why not hard links
//!
//! A hard-link mirror makes the store's own inodes part of every view's lifecycle: a link
//! or unlink writes the store inode's link count and ctime, which aborts every tippy
//! running from the store (tippy pins its own executable's and its siblings'); a link from
//! a provenance-tracked process tags the store file; and identity has to be proven by
//! `(dev, ino)`. A clone has none of that — on APFS `std::fs::copy` is `fclonefileat(2)`, a
//! new inode sharing the source's blocks copy-on-write with its mode, length and mtime,
//! costing only metadata and touching nothing about the store.
//!
//! # Rules
//!
//! * A clone never silently becomes a copy of a toolchain: `std::fs::copy` falls back to a
//!   byte copy across volumes, so [`clone_file`] refuses when the destination directory is
//!   not on the source's device.
//! * On Linux the same call is `copy_file_range(2)` — a reflink on btrfs or XFS, a real
//!   copy otherwise. No Linux bundle needs an exec root ([`crate::compat::needs_root`]), so
//!   that cost falls on the rustup view alone.
//! * [`is_clone_of`] is the identity: a regular file, not a symlink, with the source's
//!   length, permission bits and modification time — and not the source's inode. That last
//!   clause retires the hard-link views, which read as a mismatch and are rebuilt once as
//!   clones.
//! * Off Unix both `not(unix)` arms fail closed: `st_dev`/`st_ino` are unreachable from
//!   stable Rust and Windows' `std::fs::copy` byte-copies within one volume too, so nothing
//!   laid there would be a clone. [`distinct_files`] and [`one_volume`] take the platform's
//!   answer as an argument, so both decisions are asserted on every target.

use std::io;
use std::path::Path;

/// The volume a path lives on, as the OS numbers it: `st_dev` on Unix. `Option<VolumeId>`
/// is the answer type because off Unix there is no answer — see [`one_volume`].
type VolumeId = u64;

/// Clone the regular file `src` to `dst`, which must not exist: its data shared
/// copy-on-write where the filesystem can, its permission bits and modification time
/// kept.
///
/// # Errors
/// `src` is not a regular file, `dst` exists, `dst`'s directory is on another device than
/// `src` (a clone cannot cross a volume, and a toolchain is never byte-copied in its
/// place), or the copy itself fails. Off Unix it always refuses with
/// [`io::ErrorKind::Unsupported`] — see [`same_device`] and the module doc.
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
    one_volume(src, parent, Some(s.dev()), Some(p.dev()))
}

/// Off Unix the volume is not checked — and there is nothing to check it for: Windows'
/// `std::fs::copy` byte-copies on one volume and across two alike, and stable Rust exposes
/// no `st_dev` to compare, so a "clone" laid here would be a copy of a toolchain whatever
/// the paths are. A named refusal, not an `Ok(())` that would read as coverage.
#[cfg(not(unix))]
fn same_device(src: &Path, dst: &Path) -> io::Result<()> {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    one_volume(src, parent, None, None)
}

/// The verdict `same_device` returns once the platform has said which volume each side is
/// on: `Ok(())` only when both are known and equal — `None` is a refusal, not a pass.
/// Taking the two answers as arguments is what lets the `not(unix)` verdict be asserted
/// from a Unix box — see `the_volume_verdict_refuses_what_it_cannot_prove`.
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

/// Whether `at` is a clone of the regular file `src`: by `lstat`, a regular file (never a
/// symlink) with `src`'s length, permission bits and modification time — and a different
/// inode, so a hard link to `src` is not a clone of it (see the module doc).
///
/// Attributes, not bytes, because this runs over every file of a view on every update pass.
/// It cannot tell a clone from another file sharing all three — bundle 8595 ships one, a
/// separately signed `bin/rustc` with `trustc`'s length, mode and time — so for the three
/// stock names the deep check reads the bytes too ([`same_bytes`]). Off Unix it answers
/// `false` for every pair: [`file_identity`] cannot tell a hard link from a clone there.
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

/// The OS's own identity for a file: what tells a hard link (the store's inode under a
/// second name) from a clone (a new inode with the same attributes). `(dev, ino)` on Unix.
type FileId = (u64, u64);

#[cfg(unix)]
fn file_identity(m: &std::fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some((m.dev(), m.ino()))
}

/// Off Unix atpkg cannot read a file's identity, and says `None` rather than guessing.
/// Windows has one (`GetFileInformationByHandle`), but no stable std API reaches it and
/// this crate carries no `windows-sys` edge. NTFS has hard links, so the question is live
/// here; only the answer is missing. [`distinct_files`] turns `None` into "not a clone".
#[cfg(not(unix))]
fn file_identity(_m: &std::fs::Metadata) -> Option<FileId> {
    None
}

/// Whether `a` and `b` are provably different files.
///
/// An unreadable identity is not a difference: where [`file_identity`] cannot answer, this
/// answers `false`, so [`is_clone_of`] says "not a clone" and its callers re-lay rather
/// than trust a file they cannot tell from a hard link into the store.
fn distinct_files(a: Option<FileId>, b: Option<FileId>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x != y)
}

/// The two platform decisions, asserted on every target. The filesystem tests below need
/// `hard_link` and a mode bit, so they cannot run everywhere and left the `not(unix)` arms
/// with no assertion at all. These touch no filesystem: they feed [`distinct_files`] and
/// [`one_volume`] the answer each platform gives (`Some(..)` on Unix, `None` off it) and
/// pin the verdict, so a Unix box asserts the Windows decision and a Windows box the Unix.
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
        // The three shapes off Unix, where `file_identity` answers `None`.
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
        assert!(crossed.to_string().contains("different volumes"));

        // Off Unix both answers are `None` — the arm that must refuse rather than pass.
        let unknown = one_volume(src, parent, None, None).unwrap_err();
        assert_eq!(
            unknown.kind(),
            io::ErrorKind::Unsupported,
            "an unprovable volume is a refusal, not a pass"
        );
        assert!(unknown.to_string().contains("never copies a toolchain"));
        // A half-known pair is unprovable too.
        assert_eq!(
            one_volume(src, parent, Some(66_306), None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }

    /// Off Unix the two platform answers really are the ones the tests above pin, and
    /// `clone_file` really does refuse. Compiled only where that is the case; type-checked
    /// with `rustc --test --target x86_64-pc-windows-msvc` over this file.
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
