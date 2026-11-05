// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Directory metadata from one open descriptor. Device numbers are mount-local; a volume
//! UUID, where available, distinguishes permanent identity from device-number reassignment.

use std::{
    fs::OpenOptions,
    io,
    os::{
        fd::AsFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

/// The directory named by one descriptor, not a hardware or freshness attestation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The device number at this observation; it can change across mounts.
    pub dev: u64,
    /// The inode within its volume.
    pub ino: u64,
    /// The filesystem's persistent UUID, when supported.
    pub volume: Option<[u8; 16]>,
}

/// Open a directory without following its final component and read its identity.
/// Parent aliases remain allowed, as with `symlink_metadata`.
///
/// # Errors
/// Opening the directory, reading its metadata or decoding volume attributes fails.
#[cfg_attr(trust_verify, trust::skip)]
pub fn read(path: &Path) -> io::Result<Identity> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    Ok(Identity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        volume: volume(file.as_fd())?,
    })
}

#[cfg(not(target_os = "macos"))]
fn volume(_fd: std::os::fd::BorrowedFd<'_>) -> io::Result<Option<[u8; 16]>> {
    Ok(None)
}

#[cfg(target_os = "macos")]
fn volume(fd: std::os::fd::BorrowedFd<'_>) -> io::Result<Option<[u8; 16]>> {
    use std::{ffi::c_void, os::fd::AsRawFd};

    #[repr(C)]
    struct Attributes {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }
    unsafe extern "C" {
        fn fgetattrlist(
            fd: std::ffi::c_int,
            attributes: *mut c_void,
            buffer: *mut c_void,
            length: usize,
            options: std::ffi::c_uint,
        ) -> std::ffi::c_int;
    }
    let mut attributes = Attributes {
        bitmapcount: 5,
        reserved: 0,
        commonattr: 0,
        volattr: 0x8000_0000 | 0x0004_0000, // ATTR_VOL_INFO | ATTR_VOL_UUID
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buffer = [0u8; 20];
    // SAFETY: fd is borrowed for this call. Both initialized buffers remain live;
    // the 24-byte attrlist requests one 16-byte UUID after the four-byte result length.
    // On macOS LP64, the final argument is unsigned int, not unsigned long.
    let result = unsafe {
        fgetattrlist(
            fd.as_raw_fd(),
            (&raw mut attributes).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            0,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::EINVAL | libc::ENOTSUP | libc::EOPNOTSUPP)
        ) {
            return Ok(None);
        }
        return Err(error);
    }
    if u32::from_ne_bytes(buffer[..4].try_into().expect("four-byte length")) != 20 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected persistent volume attribute length",
        ));
    }
    let uuid: [u8; 16] = buffer[4..].try_into().expect("sixteen-byte UUID");
    Ok((uuid != [0; 16]).then_some(uuid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighboring_directories_share_the_volume_but_have_distinct_inodes() {
        let root = aterm_tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let first = read(&a).unwrap();
        let second = read(&b).unwrap();
        assert_eq!(first.dev, second.dev);
        assert_ne!(first.ino, second.ino);
        assert_eq!(first.volume, second.volume);
    }

    #[test]
    fn the_final_component_is_not_followed_but_parent_aliases_still_work() {
        let root = aterm_tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let dir = parent.join("directory");
        std::fs::create_dir_all(&dir).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(read(&link).is_err());
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&parent, &alias).unwrap();
        assert_eq!(read(&dir).unwrap(), read(&alias.join("directory")).unwrap());
    }
}
