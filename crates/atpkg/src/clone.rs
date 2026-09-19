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
//! * On LINUX the same call is `copy_file_range(2)`: a reflink on a copy-on-write
//!   filesystem (btrfs, XFS), a real copy otherwise. No Linux bundle ever needs an exec
//!   root ([`crate::compat::needs_root`] — its copies are byte-identical), so what that
//!   costs is the rustup view alone.
//! * [`is_clone_of`] is the identity: a regular file, not a symlink, with the source's
//!   length, permission bits and modification time — and NOT the source's inode. The last
//!   clause is what retires the hard-link views: one laid before this module reads as a
//!   mismatch and is rebuilt once, as clones, and the store's link counts come back.
//!
//! # OFF UNIX THIS MODULE REFUSES, AND SAYS SO (2026-09-18)
//!
//! Both rules above rest on `st_dev`/`st_ino`, and neither number is reachable from
//! stable Rust off Unix: `std::os::windows::fs::MetadataExt::file_index` and its
//! `volume_serial_number` sibling are unstable (`windows_by_handle`), and this crate's
//! dependency graph is argued edge by edge and carries no `windows-sys` to call
//! `GetFileInformationByHandle` with. The third leg is worse: on Windows `std::fs::copy`
//! is `CopyFileExW`, a byte copy on one volume and across two alike, so a "clone" laid
//! there is a COPY OF A TOOLCHAIN — the one thing Rule 1 exists to forbid.
//!
//! Until 2026-09-18 the two `not(unix)` arms answered `true` and `Ok(())`: the hard-link
//! rejection collapsed into [`attributes_match`], so a hard link into the store read as a
//! clone (the defect dd3808bc9 had just fixed on the Unix side), and the cross-volume
//! refusal could not fire. Both now FAIL CLOSED and name what is not checked — a guard
//! that cannot fail is worse than no guard, because it reads as coverage. The decisions
//! themselves live in [`distinct_files`] and [`one_volume`], which take the platform's
//! answer as an argument and so are asserted on EVERY target, Unix or not.

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
/// place), or the copy itself fails. OFF UNIX it always refuses, with
/// [`io::ErrorKind::Unsupported`] and the reason — see [`same_device`] and the module doc.
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

/// OFF UNIX THE VOLUME IS NOT CHECKED — AND THERE IS NOTHING TO CHECK IT FOR.
///
/// The invariant this guard carries on Unix is "`std::fs::copy` falls back to a byte copy
/// across volumes, so never let it". Off Unix that fallback is not the exception, it is
/// the whole implementation: Windows' `std::fs::copy` is `CopyFileExW`, which byte-copies
/// on one volume and across two alike, and there is no `st_dev` on stable Rust to compare
/// anyway (see the module doc). So a "clone" laid here is a copy of a toolchain no matter
/// what the two paths are, and the answer is a NAMED REFUSAL rather than the `Ok(())`
/// this arm returned until 2026-09-18 — which let `refresh_view_in_process`, the one
/// caller that carries no `cfg` of its own, byte-copy the whole store into the view on
/// every pass (off Unix `first_mismatch` always reports a mismatch, so every pass rebuilt)
/// while the module doc promised that could not happen.
///
/// The rest of the seam already treats laying a view as a Unix operation
/// ([`crate::seam::lay_view`], `lay_view_dirs` and `route_for_shim` are all `cfg(unix)`),
/// so this refusal names a lane that was never implemented instead of removing one that
/// worked.
#[cfg(not(unix))]
fn same_device(src: &Path, dst: &Path) -> io::Result<()> {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    one_volume(src, parent, None, None)
}

/// The verdict `same_device` returns once the platform has said which volume each side is
/// on: `Ok(())` ONLY when both are known and equal.
///
/// `None` is a refusal, not a pass. A guard whose unknown case answers "fine" is exactly
/// the shape this module carried off Unix, and it reads as coverage while checking
/// nothing. Taking the two answers as arguments is what lets the `not(unix)` verdict be
/// asserted from a Unix box — see `the_volume_verdict_refuses_what_it_cannot_prove`.
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
/// inode, so a hard link to `src` is NOT a clone of it (see the module doc).
///
/// Attributes, not bytes: this runs over every file of a view on every update pass. It
/// cannot tell a clone of `src` from another file that happens to share all three — and
/// bundle 8595 ships exactly such a file, a separately signed `bin/rustc` with `trustc`'s
/// length, mode and time and 2,428 different bytes. Where that matters, the three stock
/// names, the deep check reads the bytes too ([`same_bytes`]).
///
/// OFF UNIX IT ANSWERS `false` FOR EVERY PAIR, because [`file_identity`] cannot tell a
/// hard link from a clone there and this predicate would rather say "not proven" than
/// "clone" (the seam's own `not(unix)` arms already report every view as mismatched, so
/// the re-lay this provokes is the behaviour that side was already written for).
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

/// The OS's own identity for a file: what tells a HARD LINK (the store's inode under a
/// second name) from a clone (a new inode carrying the same attributes). `(dev, ino)` on
/// Unix.
type FileId = (u64, u64);

#[cfg(unix)]
fn file_identity(m: &std::fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some((m.dev(), m.ino()))
}

/// OFF UNIX ATPKG CANNOT READ A FILE'S IDENTITY, and says `None` rather than guessing.
///
/// Windows HAS one — `(dwVolumeSerialNumber, nFileIndexHigh:nFileIndexLow)` from
/// `GetFileInformationByHandle` — but no stable std API reaches it
/// (`std::os::windows::fs::MetadataExt::file_index` is unstable, behind
/// `windows_by_handle`) and this crate carries no `windows-sys` edge to call it directly.
/// NTFS has hard links and `std::fs::hard_link` makes them, so the question is live here;
/// only the answer is missing. `None` is what [`distinct_files`] turns into "not a clone".
#[cfg(not(unix))]
fn file_identity(_m: &std::fs::Metadata) -> Option<FileId> {
    None
}

/// Whether `a` and `b` are PROVABLY different files.
///
/// An unreadable identity is not a difference. Where [`file_identity`] cannot answer this
/// answers `false`, so [`is_clone_of`] says "not a clone" and its callers re-lay rather
/// than trust a file they cannot tell from a hard link into the store. Until 2026-09-18
/// the `not(unix)` arm answered `true` unconditionally, which collapsed [`is_clone_of`]
/// into [`attributes_match`] off Unix and made a hard link read as a clone — the defect
/// dd3808bc9 fixed on the Unix side the same week.
fn distinct_files(a: Option<FileId>, b: Option<FileId>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x != y)
}

/// THE TWO PLATFORM DECISIONS, ASSERTED ON EVERY TARGET.
///
/// The module's `unix` arms are covered by the filesystem tests below, which need
/// `hard_link` and a mode bit and so cannot run everywhere. The `not(unix)` arms used to
/// be covered by NOTHING — the only test module in this file was `cfg(all(test, unix))`,
/// so neither stub had a single assertion anywhere, on any box. These do not touch the
/// filesystem: they feed [`distinct_files`] and [`one_volume`] the answer each platform
/// gives (`Some(..)` on Unix, `None` off it) and pin the verdict, so a Unix box asserts
/// the Windows decision and a Windows box asserts the Unix one.
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

        // Off Unix both answers are `None`. THIS is the arm that returned `Ok(())` until
        // 2026-09-18 and so could never refuse anything.
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
    /// `clone_file` really does refuse. Compiled (and run) only where that is the case;
    /// type-checked for `x86_64-pc-windows-msvc` with
    /// `rustc --test --target x86_64-pc-windows-msvc` over this file.
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
