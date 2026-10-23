// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Destination resolution and private writing for `SIGUSR1` screen snapshots.

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use crate::control_auth;

#[cfg(unix)]
fn default_snapshot_file_name(pid: u32) -> String {
    format!("aterm_snapshot-{pid}.png")
}

/// Resolve the path the snapshot PNG may be written to, or `None` when unavailable.
#[must_use]
#[cfg(unix)]
pub(crate) fn resolve() -> Option<String> {
    match control_auth::socket_dir() {
        Some(dir) => Some(
            dir.join(default_snapshot_file_name(std::process::id()))
                .to_string_lossy()
                .into_owned(),
        ),
        None => {
            crate::logging::stderr_line!(
                "aterm-gui: no per-user runtime dir (set XDG_RUNTIME_DIR or HOME); \
                 snapshot skipped"
            );
            None
        }
    }
}

#[cfg(test)]
fn absolute_lexical(path: &Path) -> std::io::Result<PathBuf> {
    let source = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in source.components() {
        match component {
            std::path::Component::Prefix(prefix) => result.push(prefix.as_os_str()),
            std::path::Component::RootDir => result.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !result.pop() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "private artifact path escapes its filesystem root",
                    ));
                }
            }
            std::path::Component::Normal(name) => result.push(name),
        }
    }
    Ok(result)
}

/// Write `path` through the retained-handle writer.
#[cfg(test)]
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private artifact path has no filename",
        )
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = absolute_lexical(parent)?;
    let dir = crate::pinned_dir::PinnedDir::open_resolved(&parent)?;
    let file = dir.write_private(name, bytes)?;
    dir.sync()?;
    dir.validate_path_identity()?;
    file.validate_path_identity()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_auth::ensure_private_dir;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    #[test]
    fn write_private_refuses_symlinked_final_component() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("aterm-snap-at-sym-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();
        symlink(&victim, dir.join("evil.png")).unwrap();
        assert!(
            write_private(&dir.join("evil.png"), b"attack").is_err(),
            "writing through a symlinked final component must be refused",
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn write_private_refuses_symlinked_target() {
        let dir = std::env::temp_dir().join(format!("aterm-snap-win-sym-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();
        let link = dir.join("evil.png");
        if std::os::windows::fs::symlink_file(&victim, &link).is_err() {
            let _ = std::fs::remove_dir_all(&dir);
            aterm_spec::host::could_not_run("no symlink privilege here (Developer Mode is off)");
        }
        assert!(
            write_private(&link, b"attack").is_err(),
            "writing through a reparse point must be refused",
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_private_creates_0600_and_forces_mode_on_overwrite() {
        let dir = std::env::temp_dir().join(format!("aterm-snap-wr-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("snap.bin");
        write_private(&path, b"first").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, b"x").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
