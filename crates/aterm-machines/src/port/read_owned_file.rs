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
    #[cfg(unix)]
    {
        use std::io::Read as _;
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        let bound = max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
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
        let mut bytes = Vec::new();
        file.take(bound as u64).read_to_end(&mut bytes)?;
        if bytes.len() > max {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, uid, max, private);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Live, Stat, System};
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    #[test]
    fn native_owned_reads_refuse_unsafe_modes_owner_symlink_directory_and_oversize() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let path = temp.path().join("owned");
        std::fs::write(&path, b"private fixture").unwrap();
        let uid = path.metadata().unwrap().uid();
        let live = Live::new();
        let mode =
            |mode| std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
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
            Stat { dir: true, ..safe },
            Stat {
                symlink: true,
                ..safe
            },
        ] {
            sys.stat_of(path.to_str().unwrap(), changed);
            assert!(sys.read_owned_file(path, 501, 15, false).is_err());
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
