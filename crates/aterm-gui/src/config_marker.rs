// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Config-directory MARKER files — the small "the person already answered
//! this" records that sit beside `aterm.toml`.
//!
//! Three surfaces keep one: the first-launch admin card
//! (`packages_screen::ADMIN_STEP_MARKER`), the session-connections first-use
//! notice (`connections`), and the macOS access card
//! (`consent_card::MARKER`). They share a threat model — a planted link, a
//! directory, or an oversized file at the marker path must be neither read
//! through, written through, nor mistaken for a record — and until this module
//! they shared it by copy. The rules, once:
//!
//! * **Reading** opens the path without following a final-component link and
//!   checks the HANDLE (not a separate stat something could swap under) — the
//!   same boundary the theme files use (`app_config::open_regular_theme_file`).
//!   Anything that is not a regular file of at most `max_bytes` valid UTF-8 is
//!   `None`: not a record this machine wrote, so the surface errs toward
//!   disclosure and shows again.
//! * **Writing** never goes THROUGH anything already at the path: the text
//!   lands in a fresh sibling created exclusively (`create_new`, owner-only on
//!   unix) and is renamed over the marker — a rename replaces a planted link
//!   rather than following it — and a non-regular occupant fails closed
//!   (`AlreadyExists`) and is left exactly as found, the way atpkg's own prefix
//!   markers refuse an occupied path. A text past `max_bytes` is refused
//!   (`InvalidInput`), never truncated: a truncated record would read as a
//!   different answer.

use std::io::{Read as _, Write as _};
use std::path::Path;

/// The marker's text, when a REGULAR, non-symlink file of at most `max_bytes`
/// sits at `path`; `None` for everything else.
pub(crate) fn read_marker(path: &Path, max_bytes: usize) -> Option<String> {
    let file = crate::app_config::open_regular_theme_file(path).ok()?;
    let mut bytes = Vec::with_capacity(256);
    file.take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Record `text` at `path`, exclusively and atomically (see the module doc).
/// Creates the parent directory best-effort; an unwritable one surfaces as the
/// `create_new` error, which callers treat as "the card comes back".
pub(crate) fn write_marker(path: &Path, text: &str, max_bytes: usize) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => {}
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "marker path is occupied by something that is not a marker",
            ));
        }
        Err(_) => {}
    }
    if text.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "record is larger than a marker may hold",
        ));
    }
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "marker has no name")
    })?;
    let tmp = path.with_file_name(format!("{name}.tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let written = options
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Remove the marker at `path`, if a REGULAR file is there. A link or a
/// directory at the path is not a marker and is left exactly as found
/// (`AlreadyExists`, the same refusal [`write_marker`] gives); an absent
/// marker is not an error.
pub(crate) fn remove_marker(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => std::fs::remove_file(path),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "marker path is occupied by something that is not a marker",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::{read_marker, remove_marker, write_marker};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-marker-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A record round-trips; a re-record replaces in place with no sibling
    /// left behind; an oversized text is refused rather than truncated; an
    /// oversized FILE is not a record; a directory at the path fails the write
    /// closed and is left alone; on unix a planted link is neither read nor
    /// written through.
    #[test]
    fn markers_are_exclusive_bounded_and_never_written_through() {
        let dir = scratch("roundtrip");
        let marker = dir.join("answered");
        assert_eq!(read_marker(&marker, 64), None, "absent: not a record");
        write_marker(&marker, "first", 64).unwrap();
        assert_eq!(read_marker(&marker, 64).as_deref(), Some("first"));
        write_marker(&marker, "second", 64).unwrap();
        assert_eq!(read_marker(&marker, 64).as_deref(), Some("second"));
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "the exclusive sibling is renamed away, never left beside the marker"
        );
        let refused = write_marker(&marker, &"x".repeat(65), 64).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            read_marker(&marker, 64).as_deref(),
            Some("second"),
            "a refused write changes nothing"
        );
        std::fs::write(&marker, "x".repeat(65)).unwrap();
        assert_eq!(read_marker(&marker, 64), None, "oversized: not a record");
        std::fs::remove_file(&marker).unwrap();
        #[cfg(unix)]
        {
            let target = dir.join("planted-target");
            std::fs::write(&target, "planted").unwrap();
            std::os::unix::fs::symlink(&target, &marker).unwrap();
            assert_eq!(read_marker(&marker, 64), None, "a link is not a record");
            let refused = write_marker(&marker, "through?", 64).unwrap_err();
            assert_eq!(refused.kind(), std::io::ErrorKind::AlreadyExists);
            assert!(
                std::fs::symlink_metadata(&marker)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "the planted link is left alone"
            );
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "planted");
            std::fs::remove_file(&marker).unwrap();
            std::fs::remove_file(&target).unwrap();
        }
        std::fs::create_dir(&marker).unwrap();
        assert_eq!(read_marker(&marker, 64), None);
        assert!(write_marker(&marker, "x", 64).is_err());
        assert_eq!(
            remove_marker(&marker).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists,
            "a directory is not a marker and is not removed"
        );
        assert!(std::fs::symlink_metadata(&marker).unwrap().is_dir());
        std::fs::remove_dir(&marker).unwrap();
        // Removal: a regular marker goes, an absent one is not an error.
        write_marker(&marker, "gone soon", 64).unwrap();
        remove_marker(&marker).unwrap();
        assert!(!marker.exists());
        remove_marker(&marker).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The parent directory is created on demand — a fresh install has no
    /// config directory yet, and the first answer must still be recordable.
    #[test]
    fn the_parent_directory_is_created_on_demand() {
        let dir = scratch("mkdir");
        let marker = dir.join("nested").join("answered");
        write_marker(&marker, "ok", 64).unwrap();
        assert_eq!(read_marker(&marker, 64).as_deref(), Some("ok"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
