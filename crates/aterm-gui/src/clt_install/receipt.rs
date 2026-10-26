// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable custody evidence for one admitted installer child; never command authority.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

const MAX_BYTES: usize = 256;

#[derive(Debug)]
pub(super) struct Receipt {
    directory: PathBuf,
    path: PathBuf,
    text: String,
}

impl Receipt {
    pub(super) fn for_child(mark: &Path, sid: &str, pid: i32, birth: Option<u64>) -> Option<Self> {
        let birth = birth.filter(|birth| *birth > 0)?;
        if pid <= 1 || !crate::keeper_link::valid_sid(sid) {
            return None;
        }
        let mut directory = mark.file_name()?.to_os_string();
        directory.push(".children");
        let directory = mark.with_file_name(directory);
        let name = format!("{sid}-{pid}-{birth}");
        let text = format!("v1\n{sid}\n{pid}\n{birth}\n");
        (name.len() <= 255 && text.len() <= MAX_BYTES).then(|| Self {
            path: directory.join(name),
            directory,
            text,
        })
    }

    fn directory_owned(&self) -> io::Result<()> {
        let metadata = std::fs::symlink_metadata(&self.directory)?;
        if !metadata.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "installer recovery directory is not a regular directory",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if metadata.uid() != aterm_uds::peer::our_uid() || metadata.mode() & 0o022 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "installer recovery directory has another owner or permits other writers",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn write_exclusive(&self) -> io::Result<()> {
        #[cfg(unix)]
        let created = {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&self.directory)
        };
        #[cfg(not(unix))]
        let created = std::fs::create_dir(&self.directory);
        match created {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        self.directory_owned()?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = options.open(&self.path)?;
        file.write_all(self.text.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        // An error leaves the exclusive allocation in place. No installer bytes have been
        // submitted yet; a partial or unreadable receipt can never recover custody.
        aterm_durable::ordered_or_fsync(&file)
    }

    pub(super) fn matches(&self) -> bool {
        self.directory_owned().is_ok()
            && crate::config_marker::read_marker(&self.path, MAX_BYTES).as_deref()
                == Some(self.text.as_str())
    }

    pub(super) fn remove_if_matching(&self) {
        if self.matches()
            && let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            aterm_log::warn!(
                "System State: the installer's recovery receipt was not removed: {error}"
            );
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const SID: &str = "s-0123456789abcdef0123";

    #[test]
    fn immutable_child_receipts_are_exact_bounded_and_removed_only_when_matching() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        let receipt = Receipt::for_child(&mark, SID, 23, Some(45)).unwrap();
        receipt.write_exclusive().unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&receipt.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(receipt.matches());
        assert_eq!(
            receipt.write_exclusive().unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        for (sid, pid, birth) in [("s-abcdef", 23, 45), (SID, 24, 45), (SID, 23, 46)] {
            assert!(
                !Receipt::for_child(&mark, sid, pid, Some(birth))
                    .unwrap()
                    .matches()
            );
        }
        std::fs::write(&receipt.path, b"partial\n").unwrap();
        assert!(!receipt.matches());
        receipt.remove_if_matching();
        assert_eq!(std::fs::read(&receipt.path).unwrap(), b"partial\n");
        std::fs::write(&receipt.path, &receipt.text).unwrap();
        receipt.remove_if_matching();
        assert!(!receipt.path.exists());
        for (sid, pid, birth) in [
            ("../escaped", 23, Some(45)),
            (SID, 1, Some(45)),
            (SID, 23, Some(0)),
            (SID, 23, None),
        ] {
            assert!(Receipt::for_child(&mark, sid, pid, birth).is_none());
        }
    }

    #[test]
    fn malformed_and_nonregular_receipts_neither_match_nor_get_replaced() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        let receipt = Receipt::for_child(&mark, SID, 23, Some(45)).unwrap();
        std::fs::create_dir(&receipt.directory).unwrap();
        let owner = root.path().join("owner");
        std::fs::write(&owner, &receipt.text).unwrap();
        for kind in ["partial", "oversized", "link", "directory"] {
            match kind {
                "partial" => std::fs::write(&receipt.path, b"v1\n").unwrap(),
                "oversized" => std::fs::write(&receipt.path, vec![b'x'; MAX_BYTES + 1]).unwrap(),
                "link" => std::os::unix::fs::symlink(&owner, &receipt.path).unwrap(),
                _ => std::fs::create_dir(&receipt.path).unwrap(),
            }
            assert!(!receipt.matches(), "{kind}");
            assert!(receipt.write_exclusive().is_err(), "{kind}");
            receipt.remove_if_matching();
            assert!(std::fs::symlink_metadata(&receipt.path).is_ok(), "{kind}");
            if kind == "directory" {
                std::fs::remove_dir(&receipt.path).unwrap();
            } else {
                std::fs::remove_file(&receipt.path).unwrap();
            }
        }
        assert_eq!(std::fs::read_to_string(&owner).unwrap(), receipt.text);
    }

    #[test]
    fn other_writers_or_a_link_at_the_recovery_directory_give_no_custody() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        let receipt = Receipt::for_child(&mark, SID, 23, Some(45)).unwrap();
        receipt.write_exclusive().unwrap();
        for mode in [0o770, 0o777] {
            std::fs::set_permissions(&receipt.directory, std::fs::Permissions::from_mode(mode))
                .unwrap();
            assert!(!receipt.matches());
            assert_eq!(
                receipt.write_exclusive().unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            receipt.remove_if_matching();
            assert_eq!(
                std::fs::read_to_string(&receipt.path).unwrap(),
                receipt.text
            );
        }
        std::fs::set_permissions(&receipt.directory, std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(
            receipt.matches(),
            "a non-writable owned directory is sufficient"
        );
        receipt.remove_if_matching();
        std::fs::remove_dir(&receipt.directory).unwrap();
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &receipt.directory).unwrap();
        assert!(receipt.write_exclusive().is_err());
        assert!(!receipt.matches());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }
}
