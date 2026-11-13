// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
