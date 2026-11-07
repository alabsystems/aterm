// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptor-bound, bounded admission for local credentials and configuration.

use std::io;
use std::path::Path;

pub(super) fn admit(
    owner: u32,
    uid: u32,
    mode: u32,
    regular: bool,
    private: bool,
) -> io::Result<()> {
    if !regular || owner != uid || mode & 0o6022 != 0 || (private && mode & 0o077 != 0) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

pub(super) fn read(path: &Path, uid: u32, max: usize, private: bool) -> io::Result<Vec<u8>> {
    max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    read_admitted(open(path)?, uid, max, private)
}

fn open(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

fn admitted_metadata(
    file: &std::fs::File,
    uid: u32,
    max: usize,
    private: bool,
) -> io::Result<super::OwnedFileMetadata> {
    use aterm_toolchain::stat_digest::{StatDigest, StatKey};
    use std::os::unix::fs::MetadataExt as _;
    max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    let metadata = file.metadata()?;
    admit(
        metadata.uid(),
        uid,
        metadata.mode(),
        metadata.is_file(),
        private,
    )?;
    if metadata.len() > max as u64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let key = StatKey::of(&metadata);
    let mut digest = StatDigest::new(b"owned file metadata observation");
    digest.entry(b"", &key);
    Ok(super::OwnedFileMetadata {
        stat: super::Stat {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            dir: false,
            symlink: false,
            len: metadata.len(),
        },
        version: digest.finish().0,
        changed_ns: key.ctime_ns.max(key.mtime_ns),
    })
}

pub(super) fn metadata(
    path: &Path,
    uid: u32,
    max: usize,
    private: bool,
) -> io::Result<super::OwnedFileMetadata> {
    max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    admitted_metadata(&open(path)?, uid, max, private)
}

pub(super) fn observed(
    path: &Path,
    uid: u32,
    max: usize,
    private: bool,
) -> io::Result<super::OwnedFileRead> {
    use std::io::Read as _;
    max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    observed_with(open(path)?, uid, max, private, |file, bound| {
        let mut bytes = Vec::new();
        file.take(bound as u64).read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn observed_with(
    mut file: std::fs::File,
    uid: u32,
    max: usize,
    private: bool,
    read: impl FnOnce(&mut std::fs::File, usize) -> io::Result<Vec<u8>>,
) -> io::Result<super::OwnedFileRead> {
    let metadata = admitted_metadata(&file, uid, max, private)?;
    let bytes = read(
        &mut file,
        max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?,
    )?;
    if bytes.len() > max || metadata != admitted_metadata(&file, uid, max, private)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owned file changed during read",
        ));
    }
    Ok(super::OwnedFileRead { bytes, metadata })
}

pub(super) fn read_scoped(path: &Path, uid: u32, max: usize, private: bool) -> io::Result<Vec<u8>> {
    max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    #[cfg(target_os = "macos")]
    let file = {
        let (fd, _) = aterm_uds::nofollow::open_reader_with_identity(path)?;
        std::fs::File::from(fd)
    };
    #[cfg(not(target_os = "macos"))]
    let file = open_components(path)?;
    read_admitted(file, uid, max, private)
}

/// Unix platforms without macOS's no-follow-any opener walk from an owned root descriptor.
/// Each component is opened relative to the previous descriptor with O_NOFOLLOW; replacing
/// a parent name cannot redirect the next open away from the directory already held.
#[cfg(any(not(target_os = "macos"), test))]
pub(super) fn open_components(path: &Path) -> io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let tail = path
        .strip_prefix("/")
        .map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut parts = tail.components().peekable();
    if parts.peek().is_none() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open("/")?;
    while let Some(part) = parts.next() {
        let std::path::Component::Normal(name) = part else {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        let name = CString::new(name.as_bytes())?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK
            | if parts.peek().is_some() {
                libc::O_DIRECTORY
            } else {
                0
            };
        // SAFETY: directory owns this live descriptor; name is a component-only C string.
        // The returned descriptor is newly owned and no borrowed fd survives reassignment.
        let raw = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh descriptor exclusively owned by this File.
        directory = unsafe { std::fs::File::from_raw_fd(raw) };
    }
    Ok(directory)
}

fn read_admitted(file: std::fs::File, uid: u32, max: usize, private: bool) -> io::Result<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let bound = max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    let metadata = file.metadata()?;
    admit(
        metadata.uid(),
        uid,
        metadata.mode(),
        metadata.is_file(),
        private,
    )?;
    if metadata.len() > max as u64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let cap = usize::try_from(metadata.len())
        .unwrap_or(0)
        .min(max)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(cap);
    file.take(bound as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Live, Stat, System};
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    #[test]
    fn observed_owned_reads_refuse_in_place_mutation_between_descriptor_stats() {
        use std::io::Read as _;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("profile");
        std::fs::write(&path, b"AAAA").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let uid = path.metadata().unwrap().uid();
        let good = observed(&path, uid, 16, false).unwrap();
        assert_eq!(good.bytes, b"AAAA");
        assert_eq!(good.metadata, metadata(&path, uid, 16, false).unwrap());
        assert!(!format!("{good:?}").contains("AAAA"));
        let changed = observed_with(open(&path).unwrap(), uid, 16, false, |file, bound| {
            let mut prefix = [0; 2];
            file.read_exact(&mut prefix)?;
            std::fs::write(&path, b"BBBBBBBB")?;
            let mut bytes = prefix.to_vec();
            file.take(bound as u64).read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        assert_eq!(changed.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(observed(&path, uid, 16, false).unwrap().bytes, b"BBBBBBBB");
    }

    #[test]
    fn scoped_owned_reads_refuse_a_linked_parent_at_the_final_open() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let selected = root.join("selected");
        let foreign = root.join("foreign");
        std::fs::create_dir(&selected).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        let file = selected.join("control");
        std::fs::write(&file, b"owned control").unwrap();
        let uid = std::fs::metadata(&file).unwrap().uid();
        let live = Live::new();
        assert_eq!(
            live.read_owned_file_scoped(&file, uid, 13, false).unwrap(),
            b"owned control"
        );
        assert!(
            live.read_owned_file_scoped(&file, uid.wrapping_add(1), 13, false)
                .is_err()
        );
        assert!(live.read_owned_file_scoped(&file, uid, 1, false).is_err());
        std::fs::write(foreign.join("control"), b"other control").unwrap();
        std::fs::rename(&selected, root.join("saved-selected")).unwrap();
        std::os::unix::fs::symlink(&foreign, &selected).unwrap();
        assert!(live.read_owned_file_scoped(&file, uid, 13, false).is_err());
        assert_eq!(
            live.read_owned_file(&file, uid, 13, false).unwrap(),
            b"other control",
            "the existing unscoped caller policy is unchanged"
        );
    }

    #[test]
    fn descriptor_relative_scoped_reads_never_follow_an_ancestor_or_leaf_link() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let directory = root.join("selected");
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join("control");
        std::fs::write(&file, b"owned control").unwrap();
        let uid = std::fs::metadata(&file).unwrap().uid();
        assert_eq!(
            read_admitted(open_components(&file).unwrap(), uid, 13, false).unwrap(),
            b"owned control"
        );
        let leaf = directory.join("link");
        std::os::unix::fs::symlink(&file, &leaf).unwrap();
        assert!(open_components(&leaf).is_err());
        std::fs::rename(&directory, root.join("saved")).unwrap();
        std::os::unix::fs::symlink(root.join("saved"), &directory).unwrap();
        assert!(open_components(&file).is_err());
        assert!(open_components(Path::new("relative/control")).is_err());
        assert!(open_components(&root.join("selected/../saved/control")).is_err());
    }

    #[test]
    fn native_owned_reads_refuse_unsafe_modes_owner_symlink_directory_and_oversize() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let path = temp.path().join("owned");
        std::fs::write(&path, b"private fixture").unwrap();
        // A scratch file can inherit a group we do not belong to; the kernel then clears
        // SGID on chmod. Keep this owned fixture in our effective group before setting it.
        // SAFETY: getegid reads this process's effective group and has no preconditions.
        let gid = unsafe { libc::getegid() };
        std::os::unix::fs::chown(&path, None, Some(gid)).unwrap();
        assert_eq!(path.metadata().unwrap().gid(), gid);
        let uid = path.metadata().unwrap().uid();
        let live = Live::new();
        let mode = |mode| {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(
                path.metadata().unwrap().mode() & 0o7777,
                mode,
                "the fixture must retain the requested permission bits before admission"
            );
        };
        mode(0o600);
        assert_eq!(
            live.read_owned_file(&path, uid, 15, true).unwrap(),
            b"private fixture"
        );
        assert!(live.read_owned_file(&path, uid, 14, true).is_err());
        assert!(
            live.read_owned_file(&path, uid.wrapping_add(1), 15, true)
                .is_err()
        );
        for unsafe_mode in [0o620, 0o602, 0o4600, 0o2600] {
            mode(unsafe_mode);
            assert!(
                live.read_owned_file(&path, uid, 15, false).is_err(),
                "{unsafe_mode:o}"
            );
        }
        mode(0o644);
        assert!(live.read_owned_file(&path, uid, 15, true).is_err());
        assert_eq!(
            live.read_owned_file(&path, uid, 15, false).unwrap(),
            b"private fixture"
        );
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(live.read_owned_file(&link, uid, 15, false).is_err());
        assert!(live.read_owned_file(temp.path(), uid, 15, false).is_err());
        assert!(live.read_owned_file(&path, uid, usize::MAX, false).is_err());
    }

    #[test]
    fn native_owned_fifo_read_refuses_without_waiting_for_a_writer() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;
        let temp = aterm_tempfile::tempdir().unwrap();
        let path = temp.path().join("fifo");
        let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a live owned C string names an absent file in this private fixture.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let uid = path.metadata().unwrap().uid();
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            send.send(Live::new().read_owned_file(&path, uid, 64, true).is_err())
                .unwrap();
        });
        assert!(
            receive
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("FIFO admission must not wait for a writer")
        );
    }

    #[test]
    fn fake_owned_reads_admit_the_owner_and_bound_both_metadata_and_actual_bytes() {
        let sys = Fake::root();
        let path = Path::new("/owned");
        sys.file(path.to_str().unwrap(), b"private fixture");
        let safe = Stat {
            uid: 501,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 15,
        };
        sys.stat_of(path.to_str().unwrap(), safe);
        assert_eq!(
            sys.read_owned_file(path, 501, 15, true).unwrap(),
            b"private fixture"
        );
        assert_eq!(
            sys.read_owned_file_observed(path, 501, 15, true)
                .unwrap()
                .metadata
                .stat,
            safe
        );
        assert!(sys.read_owned_file(path, 501, 14, true).is_err());
        assert!(sys.read_owned_file(path, 502, 15, true).is_err());
        for changed in [
            Stat {
                mode: 0o620,
                ..safe
            },
            Stat {
                mode: 0o4600,
                ..safe
            },
            Stat {
                mode: 0o2600,
                ..safe
            },
            Stat { dir: true, ..safe },
            Stat {
                symlink: true,
                ..safe
            },
        ] {
            sys.stat_of(path.to_str().unwrap(), changed);
            assert!(sys.read_owned_file(path, 501, 15, false).is_err());
            assert!(sys.read_owned_file_observed(path, 501, 15, false).is_err());
        }
        sys.stat_of(
            path.to_str().unwrap(),
            Stat {
                mode: 0o644,
                ..safe
            },
        );
        assert!(sys.read_owned_file(path, 501, 15, true).is_err());
        assert!(sys.read_owned_file(path, 501, 15, false).is_ok());
        sys.stat_of(path.to_str().unwrap(), Stat { len: 1, ..safe });
        assert!(
            sys.read_owned_file(path, 501, 14, true).is_err(),
            "stale small metadata must not bypass the actual read bound"
        );
    }
}
