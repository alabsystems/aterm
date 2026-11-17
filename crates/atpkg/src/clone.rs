// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Copy-on-write clones of a build's files for the rustup view ([`crate::seam`])
//! and per-build exec roots ([`crate::compat`]).
//!
//! [`clone_file`] uses `std::fs::copy` within a single volume on Unix (`fclonefileat` on APFS,
//! `copy_file_range` on Linux), which falls back to a byte copy where the filesystem cannot
//! clone (HFS+, exFAT, ext4). [`cow_clone_file`] never does: it asks the kernel for a
//! copy-on-write clone (`clonefile` on macOS, `FICLONE` on Linux) and refuses where none can be
//! made. [`is_clone_of`] verifies clone identity by matching length, permissions, and mtime on a
//! distinct inode.

use std::io;
use std::path::Path;

/// OS device identifier (`st_dev` on Unix).
type VolumeId = u64;

/// Clones regular file `src` to `dst` on the same volume, preserving permissions and mtime.
/// Where the volume cannot clone, `std::fs::copy` lays a byte copy instead; a caller that must
/// never byte-copy uses [`cow_clone_file`].
///
/// # Errors
/// `src` is not a regular file, `dst` exists, `dst`'s directory is on another device than
/// `src` (a toolchain is never copied across volumes in a clone's place), or the copy fails.
/// Off Unix it always refuses with [`io::ErrorKind::Unsupported`].
pub(crate) fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    lay(src, dst, |src, dst| std::fs::copy(src, dst).map(drop))
}

/// Clones regular file `src` to `dst` as a copy-on-write clone the kernel makes (`clonefile` on
/// macOS, `FICLONE` on Linux), preserving permissions and mtime: `dst` shares `src`'s blocks
/// until one of them is written, and no byte is ever copied.
///
/// # Errors
/// As [`clone_file`]; and [`io::ErrorKind::Unsupported`] where the volume cannot clone (HFS+,
/// exFAT, ext4, any other OS), with nothing left at `dst`.
pub(crate) fn cow_clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    lay(src, dst, kernel_clone)
}

/// [`clone_file`]'s steps around `copy`, the one that lays `dst`'s bytes.
fn lay(
    src: &Path,
    dst: &Path,
    copy: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
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
    same_device(src, &meta, dst)?;
    copy(src, dst).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("cannot clone {} to {} ({e})", src.display(), dst.display()),
        )
    })?;
    let modified = meta.modified()?;
    // Read-only: the owner sets a file's time and mode through any descriptor, and a clone of a
    // read-only file opens for writing to nobody.
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = opts.open(dst)?;
    let dst_meta = file.metadata()?;
    if !dst_meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", dst.display()),
        ));
    }
    file.set_modified(modified)?;
    if dst_meta.permissions() != meta.permissions() {
        file.set_permissions(meta.permissions())?;
    }
    Ok(())
}

/// Ask the kernel for a copy-on-write clone of `src` at the free name `dst`
/// (`clonefile(2)`, `CLONE_NOFOLLOW`): no byte copy where the volume cannot clone.
#[cfg(target_os = "macos")]
fn kernel_clone(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    /// `<sys/clonefile.h>`: do not follow a symbolic link at `src`.
    const CLONE_NOFOLLOW: u32 = 0x0001;

    unsafe extern "C" {
        /// `<sys/clonefile.h>`: `int clonefile(const char *src, const char *dst, uint32_t flags)`.
        fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32)
        -> libc::c_int;
    }

    let c = |p: &Path| {
        std::ffi::CString::new(p.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    };
    let (from, to) = (c(src)?, c(dst)?);
    // SAFETY: both arguments are NUL-terminated paths alive across the call; `clonefile` reads
    // them and writes no memory of ours.
    let rc = unsafe { clonefile(from.as_ptr(), to.as_ptr(), CLONE_NOFOLLOW) };
    if rc == 0 {
        return Ok(());
    }
    Err(unclonable(io::Error::last_os_error()))
}

/// Ask the kernel for a copy-on-write clone of `src` at the free name `dst` (`FICLONE`): no
/// byte copy where the volume cannot clone, and nothing left at `dst` after a refusal.
#[cfg(target_os = "linux")]
fn kernel_clone(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    /// `<linux/fs.h>`: `FICLONE = _IOW(0x94, 9, int)`.
    const FICLONE: libc::c_ulong = 0x4004_9409;

    let from = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(src)?;
    let to = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dst)?;
    // SAFETY: both descriptors are open for the call; `FICLONE` takes the source descriptor as
    // its argument and writes no memory of ours.
    let rc = unsafe { libc::ioctl(to.as_raw_fd(), FICLONE, from.as_raw_fd()) };
    if rc == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    drop(to);
    let _ = std::fs::remove_file(dst);
    Err(unclonable(e))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn kernel_clone(src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "{}: atpkg makes a copy-on-write clone only on macOS and Linux",
            src.display()
        ),
    ))
}

/// A volume that cannot clone reads as [`io::ErrorKind::Unsupported`]; any other error stands.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn unclonable(e: io::Error) -> io::Error {
    // `ENOTSUP` and `EOPNOTSUPP` are one number on Linux and two on macOS.
    let cannot = e.raw_os_error().is_some_and(|code| {
        [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EXDEV, libc::EINVAL].contains(&code)
    });
    if cannot {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "the volume cannot make a copy-on-write clone ({e}), and atpkg never byte-copies one"
            ),
        )
    } else {
        e
    }
}

/// Refuse when `dst`'s parent directory is not on `src`'s device.
#[cfg(unix)]
fn same_device(src: &Path, src_meta: &std::fs::Metadata, dst: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let p = std::fs::symlink_metadata(parent)?;
    one_volume(src, parent, Some(src_meta.dev()), Some(p.dev()))
}

/// Always refuses off Unix where volume identity and copy-on-write cloning are unavailable.
#[cfg(not(unix))]
fn same_device(src: &Path, _src_meta: &std::fs::Metadata, dst: &Path) -> io::Result<()> {
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

fn open_if_regular(path: &Path) -> io::Result<Option<(std::fs::File, std::fs::Metadata)>> {
    match crate::metadata_io::open_regular_with_meta(path) {
        Ok(opened) => Ok(Some(opened)),
        Err(err) => {
            let meta = std::fs::symlink_metadata(path)?;
            if !meta.is_file() { Ok(None) } else { Err(err) }
        }
    }
}

/// Whether the regular files `a` and `b` hold the same bytes, read in bounded chunks.
///
/// # Errors
/// Either file cannot be opened or read. A length difference is `Ok(false)` without a read.
pub(crate) fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    use std::io::Read as _;
    let (Some((mut fa, oma)), Some((mut fb, omb))) = (open_if_regular(a)?, open_if_regular(b)?)
    else {
        return Ok(false);
    };
    if oma.len() != omb.len() {
        return Ok(false);
    }
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
#[path = "clone_decisions_tests.rs"]
mod decisions;

#[cfg(all(test, unix))]
#[path = "clone_tests.rs"]
mod tests;
