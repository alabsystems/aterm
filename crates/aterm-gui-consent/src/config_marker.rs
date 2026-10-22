// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Config-directory marker files: the small "the person already answered this" records beside
//! `aterm.toml`, read and written here by the macOS access card (`consent_card::MARKER`) and
//! the debugger-access change (`debugger_access::CHANGE_MARKER`, `OBSERVED_MARKER`). A planted
//! link, a directory, or an oversized file at a marker path must be neither read through,
//! written through, nor mistaken for a record:
//!
//! * **Reading** opens the path without following a final-component link and checks the handle
//!   (not a separate stat something could swap under), the boundary the theme files use
//!   (`aterm_effects::file_feed::read_bounded_regular_utf8`). Anything but a regular file of at
//!   most `max_bytes` valid UTF-8 is `None`, so the surface errs toward disclosure and shows
//!   again.
//! * **Writing** never goes through anything already at the path: the text lands in a fresh
//!   sibling created exclusively (`create_new`, owner-only on unix) and is renamed over the
//!   marker (a rename replaces a planted link rather than following it). A non-regular occupant
//!   fails closed (`AlreadyExists`) and is left as found. A text past `max_bytes` is refused
//!   (`InvalidInput`), never truncated: a truncated record would read as a different answer.

use crate::durable::Flush;
use std::io::Write as _;
use std::path::Path;

/// The marker's text, when a regular, non-symlink file of at most `max_bytes` sits at `path`;
/// `None` for everything else.
#[inline]
pub fn read_marker(path: &Path, max_bytes: usize) -> Option<String> {
    aterm_effects::file_feed::read_bounded_regular_utf8(path, max_bytes).ok()
}

/// Record `text` at `path`, exclusively and atomically (see the module doc), its
/// sibling finished with `flush` before the rename: [`Flush::Ordered`] where a lost or
/// older marker only brings its question back, [`Flush::Durable`] where something
/// beyond this device's write order depends on it. Each caller says which.
/// Creates the parent directory best-effort; an unwritable one surfaces as the
/// `create_new` error, which callers treat as "the card comes back".
pub fn write_marker(
    path: &Path,
    text: &str,
    max_bytes: usize,
    flush: Flush,
) -> std::io::Result<()> {
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
    // Only a successful exclusive create owns this sibling. In particular,
    // AlreadyExists must not remove the entry another writer created.
    let mut file = options.open(&tmp)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| crate::durable::finish(&file, flush));
    drop(file);
    let written = written.and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Remove the marker at `path`, if a regular file is there. A link or a directory at the path
/// is not a marker and is left as found (`AlreadyExists`, the refusal [`write_marker`] gives);
/// an absent marker is not an error.
pub fn remove_marker(path: &Path) -> std::io::Result<()> {
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

/// Return `true` when `len` fits within `max_bytes`.
#[must_use]
#[inline]
pub const fn is_within_marker_cap(len: usize, max_bytes: usize) -> bool {
    len <= max_bytes
}

/// Encode the four FDA record fields (`asked_at`, `opened_at`, `verified_at`, `verified_build`)
/// into 16 little-endian bytes (`u32` each).
#[must_use]
#[inline]
pub const fn encode_fda_timestamps_le(
    asked_at: u32,
    opened_at: u32,
    verified_at: u32,
    verified_build: u32,
) -> [u8; 16] {
    [
        asked_at as u8,
        (asked_at >> 8) as u8,
        (asked_at >> 16) as u8,
        (asked_at >> 24) as u8,
        opened_at as u8,
        (opened_at >> 8) as u8,
        (opened_at >> 16) as u8,
        (opened_at >> 24) as u8,
        verified_at as u8,
        (verified_at >> 8) as u8,
        (verified_at >> 16) as u8,
        (verified_at >> 24) as u8,
        verified_build as u8,
        (verified_build >> 8) as u8,
        (verified_build >> 16) as u8,
        (verified_build >> 24) as u8,
    ]
}

/// Decode 16 little-endian bytes into the four FDA record fields.
#[must_use]
#[inline]
pub const fn decode_fda_timestamps_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
    let asked_at = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let opened_at = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let verified_at = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let verified_build = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (asked_at, opened_at, verified_at, verified_build)
}

#[cfg(test)]
mod tests {
    use super::{Flush, read_marker, remove_marker, write_marker};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-marker-{tag}-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().unwrap().as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A record round-trips and re-records in place with no sibling left; an oversized text is
    /// refused, an oversized file is not a record, a directory fails the write closed, and on
    /// unix a planted link is neither read nor written through.
    #[test]
    fn markers_are_exclusive_bounded_and_never_written_through() {
        let dir = scratch("roundtrip");
        let marker = dir.join("answered");
        assert_eq!(read_marker(&marker, 64), None, "absent: not a record");
        write_marker(&marker, "first", 64, Flush::Durable).unwrap();
        assert_eq!(read_marker(&marker, 64).as_deref(), Some("first"));
        write_marker(&marker, "second", 64, Flush::Durable).unwrap();
        assert_eq!(read_marker(&marker, 64).as_deref(), Some("second"));
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "the exclusive sibling is renamed away, never left beside the marker"
        );
        let refused = write_marker(&marker, &"x".repeat(65), 64, Flush::Durable).unwrap_err();
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
            let refused = write_marker(&marker, "through?", 64, Flush::Durable).unwrap_err();
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
        assert!(write_marker(&marker, "x", 64, Flush::Durable).is_err());
        assert_eq!(
            remove_marker(&marker).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists,
            "a directory is not a marker and is not removed"
        );
        assert!(std::fs::symlink_metadata(&marker).unwrap().is_dir());
        std::fs::remove_dir(&marker).unwrap();
        // Removal: a regular marker goes, an absent one is not an error.
        write_marker(&marker, "gone soon", 64, Flush::Durable).unwrap();
        remove_marker(&marker).unwrap();
        assert!(!marker.exists());
        remove_marker(&marker).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The parent directory is created on demand: a fresh install has no config directory yet.
    #[test]
    fn the_parent_directory_is_created_on_demand() {
        let dir = scratch("mkdir");
        let marker = dir.join("nested").join("answered");
        write_marker(&marker, "ok", 64, Flush::Durable).unwrap();
        assert_eq!(read_marker(&marker, 64).as_deref(), Some("ok"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed exclusive create owns no temporary file to remove. CONTROL:
    /// an ordinary private marker still lands and replaces its own record.
    #[test]
    fn a_marker_temporary_collision_is_left_with_its_creator() {
        let root = aterm_tempfile::TempDir::new().expect("a private marker fixture");
        let marker = root.path().join("answered");
        write_marker(&marker, "before", 64, Flush::Durable).expect("ordinary marker");
        write_marker(&marker, "standing", 64, Flush::Durable).expect("ordinary replacement");
        assert_eq!(read_marker(&marker, 64).as_deref(), Some("standing"));

        let temporary = root
            .path()
            .join(format!("answered.tmp-{}", std::process::id()));
        std::fs::write(&temporary, b"another creator's bytes").expect("a collision");
        let error = write_marker(&marker, "replacement", 64, Flush::Durable)
            .expect_err("exclusive create refuses");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(read_marker(&marker, 64).as_deref(), Some("standing"));
        assert_eq!(
            std::fs::read(&temporary).ok(),
            Some(b"another creator's bytes".to_vec()),
            "a refused exclusive create has no cleanup authority over the collision"
        );
    }
}
