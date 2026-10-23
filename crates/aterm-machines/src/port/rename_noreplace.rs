// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Atomic absent-only directory rename.

use std::io;
use std::path::Path;

pub(super) fn rename(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;
        let from = CString::new(from.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let to = CString::new(to.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        #[cfg(target_os = "macos")]
        // SAFETY: both owned C strings remain valid for this call; AT_FDCWD uses
        // their paths, and RENAME_EXCL makes destination absence part of rename.
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        // SAFETY: the generated cell supplies the syscall number/flags for this
        // architecture; the two live NUL-terminated path pointers are input only.
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (from, to);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Live, Stat, System};

    #[test]
    fn native_publication_preserves_empty_and_occupied_directories_and_dangling_links() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let live = Live::new();
        for kind in ["empty", "occupied", "link"] {
            let from = temp.path().join(format!("source-{kind}"));
            let to = temp.path().join(format!("destination-{kind}"));
            std::fs::create_dir(&from).unwrap();
            std::fs::write(from.join("payload"), b"source payload").unwrap();
            match kind {
                "empty" => std::fs::create_dir(&to).unwrap(),
                "occupied" => {
                    std::fs::create_dir(&to).unwrap();
                    std::fs::write(to.join("keep"), b"destination payload").unwrap();
                }
                _ => std::os::unix::fs::symlink("missing-destination", &to).unwrap(),
            }
            assert_eq!(
                live.rename_noreplace(&from, &to).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(
                std::fs::read(from.join("payload")).unwrap(),
                b"source payload"
            );
            match kind {
                "empty" => assert_eq!(std::fs::read_dir(&to).unwrap().count(), 0),
                "occupied" => assert_eq!(
                    std::fs::read(to.join("keep")).unwrap(),
                    b"destination payload"
                ),
                _ => assert_eq!(
                    std::fs::read_link(&to).unwrap(),
                    Path::new("missing-destination")
                ),
            }
        }
        let from = temp.path().join("source-success");
        let to = temp.path().join("published");
        std::fs::create_dir(&from).unwrap();
        std::fs::write(from.join("payload"), b"complete tree").unwrap();
        live.rename_noreplace(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(std::fs::read(to.join("payload")).unwrap(), b"complete tree");
    }

    #[test]
    fn fake_publication_preserves_collisions_and_moves_owned_tree_metadata_on_success() {
        let sys = Fake::root();
        let from = Path::new("/stage/repo");
        let to = Path::new("/home/repo");
        sys.create_dir_all(from, 0o700).unwrap();
        sys.file("/stage/repo/payload", b"source payload");
        sys.link("/stage/repo/link", "payload");
        let before = sys.stat(from).unwrap();
        for kind in ["empty", "occupied", "link"] {
            match kind {
                "empty" => {
                    sys.stat_of(
                        to.to_str().unwrap(),
                        Stat {
                            mode: 0o755,
                            ..before
                        },
                    );
                }
                "occupied" => {
                    sys.file(to.to_str().unwrap(), b"destination payload");
                }
                _ => {
                    sys.link(to.to_str().unwrap(), "missing-destination");
                }
            }
            assert_eq!(
                sys.rename_noreplace(from, to).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(
                sys.read_file(&from.join("payload")).unwrap(),
                b"source payload"
            );
            assert_eq!(sys.stat(from).unwrap(), before);
            match kind {
                "empty" => assert!(sys.stat(to).unwrap().dir),
                "occupied" => assert_eq!(sys.read_file(to).unwrap(), b"destination payload"),
                _ => assert_eq!(sys.read_link(to).unwrap(), Path::new("missing-destination")),
            }
            sys.remove(to).unwrap();
        }
        sys.rename_noreplace(from, to).unwrap();
        assert_eq!(sys.stat(from).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(sys.stat(to).unwrap(), before);
        assert_eq!(
            sys.read_file(&to.join("payload")).unwrap(),
            b"source payload"
        );
        assert_eq!(
            sys.read_link(&to.join("link")).unwrap(),
            Path::new("payload")
        );
        assert_eq!(
            sys.read_file(&from.join("payload")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
