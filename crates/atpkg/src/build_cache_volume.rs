// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE PERSISTENT IDENTITY OF THE VOLUME A FOLDER IS ON (`ATTR_VOL_UUID`), which custody records
//! bind a folder's volume by across mounts ([`super::Identity::names`]).
//!
//! A device number (`st_dev`) is the mount's, not the volume's: macOS numbers a volume as it
//! mounts it, so after a restart one volume can carry another number, and another volume the
//! number it had. On the Mac this was measured on (2026-11-10), 8 of 88 receipts recorded the data
//! volume as device 16777233 or 16777229, the same inodes now reading as 16777232; and the sealed
//! system volume and the data volume of one volume group both read as 16777232 in the same boot,
//! so a device number does not tell two volumes apart even within one. The UUID is the volume's own
//! (`diskutil info`'s Volume UUID) and is the same whatever number a mount gives it.
//!
//! A volume attribute is read for any object on the volume, not only its root: both calls here
//! return the data volume's UUID for a folder under `~`.

#[cfg(unix)]
use aterm_dirfd::BorrowedFd;

/// A volume's persistent UUID.
pub(in crate::build_cache) type Uuid = [u8; 16];

/// The UUID of the volume the entry `name` of the folder held as `folder` is on, a link at `name`
/// read as itself (`FSOPT_NOFOLLOW`), as `fstatat` with `AT_SYMLINK_NOFOLLOW` reads it;
/// [`aterm_dirfd::CWD`] and a path read it by the path. `Ok(None)` where the volume keeps no UUID.
///
/// # Errors
/// The entry cannot be read (gone, unreachable, a name with a NUL in it).
#[cfg(unix)]
pub(in crate::build_cache) fn at(
    folder: BorrowedFd<'_>,
    name: impl AsRef<std::ffi::OsStr>,
) -> std::io::Result<Option<Uuid>> {
    sys::at(folder, name.as_ref())
}

/// The UUID of the volume the folder or file held open as `fd` is on. `Ok(None)` where the
/// volume keeps no UUID.
///
/// # Errors
/// The descriptor cannot be read.
#[cfg(unix)]
pub(in crate::build_cache) fn held(fd: BorrowedFd<'_>) -> std::io::Result<Option<Uuid>> {
    sys::held(fd)
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::OsStr;
    use std::os::fd::AsRawFd as _;

    use aterm_dirfd::{Arg as _, BorrowedFd};

    use super::Uuid;

    /// `<sys/attr.h>` `struct attrlist`.
    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_VOL_INFO: u32 = 0x8000_0000;
    const ATTR_VOL_UUID: u32 = 0x0004_0000;
    const FSOPT_NOFOLLOW: u32 = 0x0000_0001;
    /// The returned length (a `u_int32_t`) and the one 16-byte UUID asked for after it.
    const LEN: usize = 4 + 16;

    // SAFETY (declaration): libSystem exports both with these C signatures on every supported
    // macOS (`getattrlistat` since 10.10, whose options are an `unsigned long`; `fgetattrlist`'s
    // an `unsigned int` on LP64); each returns 0, or -1 and errno.
    unsafe extern "C" {
        fn getattrlistat(
            dirfd: libc::c_int,
            path: *const libc::c_char,
            attr_list: *mut libc::c_void,
            attr_buf: *mut libc::c_void,
            attr_buf_size: usize,
            options: libc::c_ulong,
        ) -> libc::c_int;
        fn fgetattrlist(
            fd: libc::c_int,
            attr_list: *mut libc::c_void,
            attr_buf: *mut libc::c_void,
            attr_buf_size: usize,
            options: libc::c_uint,
        ) -> libc::c_int;
    }

    fn request() -> AttrList {
        AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: 0,
            volattr: ATTR_VOL_INFO | ATTR_VOL_UUID,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        }
    }

    /// The UUID a call that returned `rc` wrote into `buf`: none where the volume keeps none (a
    /// file system that does not answer the attribute, or an all-zero UUID, as one with none
    /// answers).
    fn decoded(rc: libc::c_int, buf: &[u8; LEN]) -> std::io::Result<Option<Uuid>> {
        if rc != 0 {
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::EINVAL | libc::ENOTSUP | libc::EOPNOTSUPP) => Ok(None),
                _ => Err(error),
            };
        }
        let mut length = [0; 4];
        length.copy_from_slice(&buf[..4]);
        if u32::from_ne_bytes(length) as usize != LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the volume's UUID came back at an unexpected length",
            ));
        }
        let mut uuid = [0; 16];
        uuid.copy_from_slice(&buf[4..]);
        Ok((uuid != [0; 16]).then_some(uuid))
    }

    pub(super) fn at(folder: BorrowedFd<'_>, name: &OsStr) -> std::io::Result<Option<Uuid>> {
        let mut list = request();
        let mut buf = [0u8; LEN];
        let rc = name
            .with_c_str(|c| {
                // SAFETY: `folder` is live, `c` is NUL-terminated, and `list` and `buf` are
                // writable C layouts of the sizes passed, which the call writes and nothing else.
                Ok(unsafe {
                    getattrlistat(
                        folder.as_raw_fd(),
                        c.as_ptr(),
                        (&raw mut list).cast(),
                        buf.as_mut_ptr().cast(),
                        LEN,
                        libc::c_ulong::from(FSOPT_NOFOLLOW),
                    )
                })
            })
            .map_err(std::io::Error::from)?;
        decoded(rc, &buf)
    }

    pub(super) fn held(fd: BorrowedFd<'_>) -> std::io::Result<Option<Uuid>> {
        let mut list = request();
        let mut buf = [0u8; LEN];
        // SAFETY: `fd` is live, and `list` and `buf` are writable C layouts of the sizes passed,
        // which the call writes and nothing else.
        let rc = unsafe {
            fgetattrlist(
                fd.as_raw_fd(),
                (&raw mut list).cast(),
                buf.as_mut_ptr().cast(),
                LEN,
                0,
            )
        };
        decoded(rc, &buf)
    }
}

/// Off macOS no volume UUID is read: every identity keeps the device number alone, as before.
#[cfg(all(unix, not(target_os = "macos")))]
mod sys {
    use std::ffi::OsStr;

    use aterm_dirfd::BorrowedFd;

    use super::Uuid;

    pub(super) const fn at(_: BorrowedFd<'_>, _: &OsStr) -> std::io::Result<Option<Uuid>> {
        Ok(None)
    }

    pub(super) const fn held(_: BorrowedFd<'_>) -> std::io::Result<Option<Uuid>> {
        Ok(None)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::fd::AsFd as _;

    /// A folder and the folder holding it are on one volume, read by name and held alike; on
    /// macOS that volume has a UUID (the test scratch is on APFS).
    #[test]
    fn a_folder_reads_its_volume_by_name_and_held_alike() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let folder = scratch.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let by_name = super::at(aterm_dirfd::CWD, &folder).unwrap();
        let open = std::fs::File::open(&folder).unwrap();
        let held = super::held(open.as_fd()).unwrap();
        let holder = super::at(aterm_dirfd::CWD, scratch.path()).unwrap();
        assert_eq!(by_name, held);
        assert_eq!(by_name, holder);
        assert_eq!(by_name.is_some(), cfg!(target_os = "macos"));
    }

    /// A link at the name is read as itself, never followed: a dangling one still reads (its own
    /// volume), and a missing name is an error, never a volume.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_link_is_read_as_itself_and_a_missing_name_is_an_error() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let link = scratch.path().join("link");
        std::os::unix::fs::symlink(scratch.path().join("nothing"), &link).unwrap();
        assert_eq!(
            super::at(aterm_dirfd::CWD, &link).unwrap(),
            super::at(aterm_dirfd::CWD, scratch.path()).unwrap()
        );
        assert!(super::at(aterm_dirfd::CWD, scratch.path().join("missing")).is_err());
    }
}
