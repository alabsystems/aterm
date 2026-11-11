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
mod atomic_write_tests {
    use super::*;

    #[test]
    fn an_atomic_write_leaves_an_unowned_old_stage_untouched() {
        let dir = aterm_tempfile::tempdir().expect("private directory");
        let dest = dir.path().join("record.json");
        let unrelated = dest.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&dest, "old").expect("destination");
        std::fs::write(&unrelated, "irreplaceable fixture").expect("unowned former stage");
        write_atomic(&dest, "new").expect("actual public replacement");
        assert_eq!(std::fs::read_to_string(&dest).expect("destination"), "new");
        assert_eq!(
            std::fs::read_to_string(&unrelated).expect("unrelated"),
            "irreplaceable fixture"
        );
    }

    /// A destination's name leaves its temporary room: [`write_atomic`] writes the longest one
    /// and refuses the next, with what was there kept and nothing else left in the folder.
    ///
    /// NEGATIVE CONTROLS (2026-11-05): with two bytes more in the temporary's name, the longest
    /// name fails (`the longest name: ... "File name too long"`); with no `.tmp.<pid>` in it, the
    /// name past the room is written (`past the room: ()`).
    #[cfg(unix)]
    #[test]
    fn a_destination_name_is_limited_by_the_room_its_temporary_leaves() {
        let dir = aterm_tempfile::tempdir().expect("private directory");
        // A name's `.`, `.tmp.`, the `.` before its salt and the salt's 8 digits, and the pid.
        let room = 255 - 15 - std::process::id().to_string().len();
        let longest = dir.path().join("n".repeat(room));
        write_atomic(&longest, "fits").expect("the longest name");
        let past = dir.path().join("p".repeat(room + 1));
        std::fs::write(&past, "as it was").expect("the folder takes the name");
        let refused = write_atomic(&past, "no room").expect_err("past the room");
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidFilename);
        assert_eq!(std::fs::read_to_string(&past).expect("kept"), "as it was");
        let left = std::fs::read_dir(dir.path()).expect("folder").count();
        assert_eq!(left, 2, "a temporary stayed behind");
    }

    /// A destination's final mode reaches only a complete replacement: its bytes are written
    /// under 0600, in a temporary whose name any reader of the folder can see.
    ///
    /// NEGATIVE CONTROL (2026-11-05): with the mode set before the bytes are written, this fails at
    /// "the bytes went into a wider file", `Some(420)` against `Some(384)`.
    #[cfg(unix)]
    #[test]
    fn a_wider_mode_reaches_only_a_complete_replacement() {
        use std::{io::Write as _, os::unix::fs::PermissionsExt as _};
        let dir = aterm_tempfile::tempdir().expect("private directory");
        let dest = dir.path().join("record");
        let mut written_under = None;
        write_atomic_using(
            &dest,
            "public signature",
            Some(0o644),
            aterm_durable::Flush::Durable,
            |file, bytes| {
                written_under = Some(file.metadata()?.permissions().mode() & 0o777);
                file.write_all(bytes)
            },
            || Ok(()),
        )
        .expect("public replacement");
        assert_eq!(
            written_under,
            Some(0o600),
            "the bytes went into a wider file"
        );
        let meta = std::fs::metadata(&dest).expect("metadata");
        assert_eq!(meta.permissions().mode() & 0o777, 0o644);
    }

    #[test]
    fn the_atomic_writes_name_their_flush() {
        use aterm_durable::{Flush, seam::Recording};
        let dir = aterm_tempfile::tempdir().expect("private directory");
        let dest = dir.path().join("record.json");
        let recording = Recording::start();
        write_atomic(&dest, "durable").expect("durable write");
        assert_eq!(recording.take(), [Flush::Durable]);
        write_atomic_ordered(&dest, "ordered").expect("ordered write");
        assert_eq!(recording.take(), [Flush::Ordered]);
        assert_eq!(std::fs::read_to_string(&dest).expect("body"), "ordered");
    }

    #[test]
    fn a_failed_atomic_write_preserves_the_destination_and_cleans_only_its_stage() {
        use std::io::Write as _;
        let dir = aterm_tempfile::tempdir().expect("private directory");
        let dest = dir.path().join("record.json");
        let unrelated = dir.path().join("notes");
        std::fs::write(&dest, "old good snapshot").expect("destination");
        std::fs::write(&unrelated, "keep me").expect("unrelated");
        let mut published = false;
        let result = write_atomic_using(
            &dest,
            "replacement",
            None,
            Flush::Durable,
            |file, bytes| {
                file.write_all(&bytes[..1])?;
                Err(std::io::Error::other("injected owned-stage write failure"))
            },
            || {
                published = true;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!published, "a failed preparation cannot publish evidence");
        assert_eq!(
            std::fs::read_to_string(&dest).expect("destination"),
            "old good snapshot"
        );
        assert_eq!(
            std::fs::read_to_string(&unrelated).expect("unrelated"),
            "keep me"
        );
        let mut kept: Vec<_> = std::fs::read_dir(dir.path())
            .expect("directory")
            .map(|e| e.expect("entry").file_name())
            .collect();
        kept.sort();
        assert_eq!(
            kept,
            [
                std::ffi::OsString::from("notes"),
                std::ffi::OsString::from("record.json")
            ]
        );
    }

    #[test]
    fn the_publication_barrier_sees_a_flushed_stage_and_keeps_the_old_destination_on_error() {
        use aterm_durable::{Flush, seam::Recording};
        let dir = aterm_tempfile::tempdir().unwrap();
        let dest = dir.path().join("state.json");
        std::fs::write(&dest, "old state").unwrap();
        let recording = Recording::start();
        let result = write_atomic_before_publish(&dest, "replacement", || {
            assert_eq!(recording.take(), [Flush::Durable]);
            assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old state");
            Err(std::io::Error::other("injected barrier refusal"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old state");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn an_atomic_replacement_publishes_its_requested_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::tempdir().expect("private directory");
        let dest = dir.path().join("record");
        write_atomic_mode(&dest, "public signature", Some(0o644)).expect("public replacement");
        assert_eq!(
            std::fs::metadata(&dest)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        write_atomic(&dest, "private replacement").expect("private replacement");
        assert_eq!(
            std::fs::read_to_string(&dest).expect("body"),
            "private replacement"
        );
        assert_eq!(
            std::fs::metadata(&dest)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
