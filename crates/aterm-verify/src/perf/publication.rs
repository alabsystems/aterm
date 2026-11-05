// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publication of performance baselines and retained reports.

use std::io::{self, Write};
use std::path::Path;

pub(super) type Publisher = fn(&Path, &str) -> io::Result<()>;

pub(super) fn publish(path: &Path, text: &str) -> io::Result<()> {
    publish_with(path, |writer| writer.write_all(text.as_bytes()))
}

pub(super) fn publish_with(
    path: &Path,
    produce: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    #[cfg(any(unix, windows))]
    {
        let target = match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)?,
            Ok(_) => path.to_path_buf(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
            Err(error) => return Err(error),
        };
        let parent = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !std::fs::metadata(parent)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "publication parent is not a directory",
            ));
        }
        let metadata = match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.is_file() => Some(meta),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "publication target is not a regular file",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            metadata.map_or(0o644, |meta| meta.permissions().mode() & 0o777)
        };
        #[cfg(windows)]
        let mode = {
            let _ = metadata;
            0o644
        };
        aterm_uds::atomic_file::write_with(&target, mode, produce)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, produce);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic performance publication is unavailable",
        ))
    }
}

#[cfg(test)]
pub(super) fn interrupted(path: &Path, _: &str) -> io::Result<()> {
    publish_with(path, |writer| {
        writer.write_all(b"partial")?;
        Err(io::Error::other("injected producer failure"))
    })
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    #[test]
    fn publication_retains_existing_paths_and_modes() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let target = dir.path().join("report");
        publish(&target, "first").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o644
            );
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        publish_with(&target, |writer| writer.write_all(b"replacement")).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "replacement");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o640
            );
            let link = dir.path().join("linked");
            std::os::unix::fs::symlink("report", &link).unwrap();
            publish(&link, "linked update").unwrap();
            assert!(
                std::fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "linked update");
            assert!(interrupted(&link, "unused").is_err());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "linked update");
        }
        let missing = dir.path().join("absent").join("report");
        assert_eq!(
            publish(&missing, "no").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(!missing.parent().unwrap().exists());
    }
}
