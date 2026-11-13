// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Atomic replacement of a file: its bytes go into a sibling temporary, which is flushed and then
//! renamed over the destination.

use std::path::{Path, PathBuf};

use aterm_durable::Flush;

/// Write `text` to `path` through a sibling temp file and a rename.
/// # Errors
/// The directory could not be made or the write/rename failed. Among those, with
/// [`std::io::ErrorKind::InvalidFilename`], a name the folder takes but its temporary does not:
/// the temporary's name is that name, 15 bytes and the writer's pid in decimal, so a name past 233
/// bytes on a filesystem of 255-byte names under a 7-digit pid (235 under a 5-digit one). Nothing
/// is left behind, and what was at `path` stays.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    write_atomic_mode(path, text, None)
}

/// [`write_atomic`], with the replacement's permission bits (unix) where `mode` names them, for a file its owner keeps as they made it.
/// # Errors
/// The directory could not be made, the name has no room for its temporary ([`write_atomic`]), or
/// the write, the mode or the rename failed.
pub fn write_atomic_mode(path: &Path, text: &str, mode: Option<u32>) -> std::io::Result<()> {
    use std::io::Write as _;
    write_atomic_using(
        path,
        text,
        mode,
        Flush::Durable,
        |file, bytes| file.write_all(bytes),
        || Ok(()),
    )
}

/// Prepares and durably flushes a private replacement, then calls `before_publish` before
/// its rename. The caller can retain evidence without doing so for a failed preparation.
/// # Errors
/// As [`write_atomic`], or the callback failed. A callback error leaves the destination intact.
pub(super) fn write_atomic_before_publish(
    path: &Path,
    text: &str,
    before_publish: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::Write as _;
    write_atomic_using(
        path,
        text,
        None,
        Flush::Durable,
        |file, bytes| file.write_all(bytes),
        before_publish,
    )
}

/// [`write_atomic`] with the stage finished by [`aterm_durable::ordered`] instead of a full flush.
/// # Errors
/// As [`write_atomic`].
pub fn write_atomic_ordered(path: &Path, text: &str) -> std::io::Result<()> {
    write_atomic_ordered_mode(path, text, None)
}

/// [`write_atomic_ordered`], with the replacement's permission bits (unix) where `mode` names them.
/// # Errors
/// As [`write_atomic_mode`].
pub fn write_atomic_ordered_mode(
    path: &Path,
    text: &str,
    mode: Option<u32>,
) -> std::io::Result<()> {
    use std::io::Write as _;
    write_atomic_using(
        path,
        text,
        mode,
        Flush::Ordered,
        |file, bytes| file.write_all(bytes),
        || Ok(()),
    )
}

fn write_atomic_using(
    path: &Path,
    text: &str,
    mode: Option<u32>,
    flush: Flush,
    write: impl FnOnce(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
    before_publish: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let (staged, mut file) = match aterm_uds::atomic_file::create_temp_beside(path, 0o600) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let dir = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            std::fs::create_dir_all(dir)?;
            aterm_uds::atomic_file::create_temp_beside(path, 0o600)?
        }
        made => made?,
    };
    let mut stage = OwnedStage {
        path: staged,
        unpublished: true,
    };
    // The bytes are written under 0600: `mode` reaches only a complete replacement.
    write(&mut file, text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(mode.unwrap_or(0o600)))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    crate::durable::finish(&file, flush)?;
    drop(file);
    before_publish()?;
    std::fs::rename(&stage.path, path)?;
    // Its name is free after the rename, and may be a later writer's by then.
    stage.unpublished = false;
    Ok(())
}

/// The temporary this write alone created ([`aterm_uds::atomic_file::create_temp_beside`]), removed
/// on any way out that did not publish it.
struct OwnedStage {
    path: PathBuf,
    unpublished: bool,
}

impl Drop for OwnedStage {
    fn drop(&mut self) {
        if self.unpublished {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
#[path = "atomic_write_atomic_write_tests.rs"]
mod atomic_write_tests;
