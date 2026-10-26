// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Prepare an owned file before publishing it at an absent destination.

use super::{Scratch, rename_noreplace};
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

pub(super) fn create(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    finalize: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    create_with(path, bytes, mode, finalize, rename_noreplace::rename)
}

fn create_with(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    finalize: impl FnOnce(&File) -> io::Result<()>,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let scratch = Scratch::live(Some(parent), "create")?;
    let prepared = scratch.path().join("payload");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&prepared)?;
    file.write_all(bytes)?;
    file.set_permissions(Permissions::from_mode(mode))?;
    finalize(&file)?;
    drop(file);
    // Both primitives preserve an occupied destination. No fallible flush or cleanup follows
    // successful publication; Scratch only removes the private preparation path.
    match rename(&prepared, path) {
        Err(error) if rename_unsupported(&error) => std::fs::hard_link(&prepared, path),
        result => result,
    }
}

fn rename_unsupported(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Unsupported
        || error.raw_os_error().is_some_and(|code| {
            [libc::ENOSYS, libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL].contains(&code)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Live, System};
    use std::os::unix::fs::MetadataExt as _;

    fn occupy(path: &Path, kind: &str, target: &Path) {
        match kind {
            "empty" | "file" => {
                std::fs::write(
                    path,
                    if kind == "empty" {
                        b"".as_slice()
                    } else {
                        b"file owner's value".as_slice()
                    },
                )
                .unwrap();
                std::fs::set_permissions(path, Permissions::from_mode(0o640)).unwrap();
            }
            "directory" => std::fs::create_dir(path).unwrap(),
            "link" => std::os::unix::fs::symlink(target, path).unwrap(),
            _ => std::os::unix::fs::symlink("missing-owner", path).unwrap(),
        }
    }

    fn assert_owner(path: &Path, kind: &str, target: &Path) {
        match kind {
            "empty" => assert!(std::fs::read(path).unwrap().is_empty()),
            "file" => assert_eq!(std::fs::read(path).unwrap(), b"file owner's value"),
            "directory" => assert!(std::fs::read_dir(path).unwrap().next().is_none()),
            "link" => assert_eq!(std::fs::read_link(path).unwrap(), target),
            _ => assert_eq!(
                std::fs::read_link(path).unwrap(),
                Path::new("missing-owner")
            ),
        }
        assert_eq!(std::fs::read(target).unwrap(), b"link owner's value");
    }

    fn identity(metadata: &std::fs::Metadata) -> (u64, u64, u32, u32, u32, u64, u64) {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.uid(),
            metadata.gid(),
            metadata.mode(),
            metadata.nlink(),
            metadata.len(),
        )
    }

    #[test]
    fn a_failed_file_finalizer_leaves_the_destination_available_for_a_real_retry() {
        let directory = aterm_tempfile::tempdir().unwrap();
        let destination = directory.path().join("run.lock");
        let unavailable = directory.path().join("no-finalizer-input");
        let bytes = b"owned claim\n";
        let error = create(&destination, bytes, 0o600, |file| {
            assert!(!destination.try_exists().unwrap());
            assert_eq!(file.metadata().unwrap().len(), bytes.len() as u64);
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            File::open(&unavailable).map(drop)
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let absent_after_failure = !destination.try_exists().unwrap();
        let empty_after_failure = std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .is_none();
        let retry = Live::new().create_new(&destination, bytes, 0o600);
        assert_eq!(
            (absent_after_failure, empty_after_failure, retry.is_ok()),
            (true, true, true),
            "a preparation error must not occupy the claim path; retry: {retry:?}"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    }

    #[test]
    fn successful_exclusive_creation_keeps_the_exact_bytes_and_mode() {
        let directory = aterm_tempfile::tempdir().unwrap();
        let live = Live::new();
        for (name, bytes, mode) in [
            ("marker", b"".as_slice(), 0o644),
            ("claim", b"1 2\n".as_slice(), 0o600),
        ] {
            let path = directory.path().join(name);
            create(&path, bytes, mode, |file| {
                assert!(!path.try_exists().unwrap());
                assert_eq!(file.metadata().unwrap().len(), bytes.len() as u64);
                assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, mode);
                aterm_durable::ordered(file)
            })
            .unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            let metadata = std::fs::metadata(&path).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, mode);
            assert_eq!(metadata.uid(), live.euid());
            assert_eq!(metadata.nlink(), 1);
        }
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn occupied_regular_files_directories_and_links_keep_their_owners() {
        let directory = aterm_tempfile::tempdir().unwrap();
        let target = directory.path().join("link-owner");
        std::fs::write(&target, b"link owner's value").unwrap();
        for kind in ["empty", "file", "directory", "link", "dangling"] {
            let path = directory.path().join(kind);
            occupy(&path, kind, &target);
            let before = std::fs::symlink_metadata(&path).unwrap();
            let error = create(&path, b"new claim", 0o600, |_| {
                panic!("an occupied path must be refused before preparation")
            })
            .unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::AlreadyExists,
                "{kind}: {error}"
            );
            assert_owner(&path, kind, &target);
            assert_eq!(
                identity(&std::fs::symlink_metadata(&path).unwrap()),
                identity(&before)
            );
        }
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 6);
    }

    #[test]
    fn a_destination_arriving_during_preparation_survives_both_publication_paths() {
        for fallback in [false, true] {
            let directory = aterm_tempfile::tempdir().unwrap();
            let target = directory.path().join("link-owner");
            std::fs::write(&target, b"link owner's value").unwrap();
            for kind in ["empty", "file", "directory", "link", "dangling"] {
                let path = directory.path().join(kind);
                let mut before = None;
                let error = create_with(
                    &path,
                    b"new claim",
                    0o600,
                    |file| {
                        assert!(!path.try_exists().unwrap());
                        occupy(&path, kind, &target);
                        before = Some(std::fs::symlink_metadata(&path).unwrap());
                        aterm_durable::ordered(file)
                    },
                    |from, to| {
                        if fallback {
                            Err(io::ErrorKind::Unsupported.into())
                        } else {
                            rename_noreplace::rename(from, to)
                        }
                    },
                )
                .unwrap_err();
                assert_eq!(
                    error.kind(),
                    io::ErrorKind::AlreadyExists,
                    "{kind}, fallback={fallback}: {error}"
                );
                assert_owner(&path, kind, &target);
                assert_eq!(
                    identity(&std::fs::symlink_metadata(&path).unwrap()),
                    identity(&before.unwrap())
                );
            }
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 6);
        }
    }

    #[test]
    fn an_unsupported_rename_uses_absent_only_file_publication_and_cleans_its_private_link() {
        let directory = aterm_tempfile::tempdir().unwrap();
        let path = directory.path().join("claim");
        create_with(
            &path,
            b"complete claim",
            0o600,
            |file| {
                assert!(!path.try_exists().unwrap());
                aterm_durable::ordered(file)
            },
            |_, _| Err(io::Error::from_raw_os_error(libc::ENOTSUP)),
        )
        .unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"complete claim");
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), Live::new().euid());
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);

        let refused = directory.path().join("refused");
        let error = create_with(
            &refused,
            b"unpublished",
            0o600,
            aterm_durable::ordered,
            |_, _| Err(io::ErrorKind::PermissionDenied.into()),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(!refused.try_exists().unwrap());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
