// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! APFS clones of the disk layers (`docs/DESIGN-vz-guest-2026-10-23.md` §4, fact 36).
//!
//! A clone shares every block with its source until one side writes it, so a 2 GB base
//! clones in milliseconds and a 60 GiB sparse disk costs what the guest writes. A volume
//! that cannot clone is refused, never byte-copied: a copy of a disk is no clone, and a
//! copy of a 60 GiB sparse disk could fill the volume.

use std::io;
use std::path::Path;

/// Clone the regular file `src` to the new name `dst` with `clonefile(2)`, never following
/// a link at `src`. The clone carries `src`'s mode and times.
///
/// # Errors
/// `dst` exists (`AlreadyExists`), `src` is a link or no regular file, the two are on
/// different volumes, or the volume cannot clone (APFS can). Off macOS it always refuses
/// (`Unsupported`): the lane's VM runs on Apple silicon only.
pub fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is no regular file, and only a file is cloned",
                src.display()
            ),
        ));
    }
    clone(src, dst).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "could not clone {} to {}: {e} (a clone needs one APFS volume)",
                src.display(),
                dst.display()
            ),
        )
    })
}

#[cfg(target_os = "macos")]
fn clone(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    /// `<sys/clonefile.h>`: do not follow a link at the source.
    const CLONE_NOFOLLOW: u32 = 0x0001;

    unsafe extern "C" {
        /// `<sys/clonefile.h>`: `int clonefile(const char *src, const char *dst, uint32_t flags)`.
        fn clonefile(src: *const std::ffi::c_char, dst: *const std::ffi::c_char, flags: u32)
        -> i32;
    }

    let c = |p: &Path| {
        std::ffi::CString::new(p.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path holds a NUL"))
    };
    let (from, to) = (c(src)?, c(dst)?);
    // SAFETY: both pointers are NUL-terminated strings that outlive the call, which reads
    // them and writes nothing of ours.
    if unsafe { clonefile(from.as_ptr(), to.as_ptr(), CLONE_NOFOLLOW) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn clone(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "clonefile is APFS's, and the lane's VM runs on macOS only",
    ))
}

#[cfg(all(test, target_os = "macos"))]
#[path = "clone_tests.rs"]
mod tests;
