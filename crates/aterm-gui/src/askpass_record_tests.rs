// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the password sheet's spacing record: its line, and how it is written and read.

use super::*;

/// The wall clock these tests read the record at.
const NOW_MS: u64 = 1_800_000_000_000;

/// B1. THE LINE ROUND-TRIPS, AND EVERY OTHER SHAPE READS AS NONE: another version, a streak of 0
/// with an end, a leading zero or a sign, a trailing space, no LF, two lines, bytes that are no
/// UTF-8, a sixth field and a file past the cap.
/// NEGATIVE CONTROLS: a parse without the version check takes version 2; one without the field
/// count takes a sixth field.
#[test]
fn the_record_line_round_trips_and_any_other_shape_reads_as_none() {
    for (streak, ended, standing) in [
        (1, Some(NOW_MS), None),
        (7, Some(0), Some(NOW_MS)),
        (u32::MAX, Some(u64::MAX), Some(u64::MAX)),
        (0, None, Some(NOW_MS)),
        (0, None, None),
    ] {
        let written = line(streak, ended, standing);
        assert!(written.len() <= MAX_BYTES, "{written:?}");
        assert_eq!(
            parse(written.as_bytes()),
            Some((streak, ended, standing)),
            "{written:?}"
        );
    }
    assert_eq!(
        line(0, Some(5), None),
        "askpass-quiet 1 0 - -\n",
        "a streak of 0 names no end"
    );
    assert_eq!(
        line(3, None, None),
        "askpass-quiet 1 0 - -\n",
        "a streak names an end"
    );
    let long = format!("askpass-quiet 1 1 1 {}\n", "1".repeat(MAX_BYTES));
    for bad in [
        "askpass-quiet 2 1 5 -\n",
        "askpass-quiet 1 0 5 -\n",
        "askpass-quiet 1 1 - -\n",
        "askpass-quiet 1 03 5 -\n",
        "askpass-quiet 1 +3 5 -\n",
        "askpass-quiet 1 1 05 -\n",
        "askpass-quiet 1 1 5 - \n",
        "askpass-quiet 1 1 5 -",
        "askpass-quiet 1 1 5 -\naskpass-quiet 1 1 5 -\n",
        "askpass-quiet 1 1 5 - -\n",
        "askpass-quiet 1 1 5\n",
        "askpass-quiet 1  1 5 -\n",
        "askpass-off 1 1 5 -\n",
        "askpass-quiet 1 4294967296 5 -\n",
        "askpass-quiet 1 1 18446744073709551616 -\n",
        "",
        "\n",
        long.as_str(),
    ] {
        assert_eq!(parse(bad.as_bytes()), None, "{bad:?}");
    }
    assert_eq!(parse(b"askpass-quiet 1 1 5 \xff\n"), None, "no UTF-8");
}

/// The record `dir` holds, as bytes.
fn on_disk(dir: &std::path::Path) -> Option<Vec<u8>> {
    std::fs::read(dir.join(NAME)).ok()
}

/// B2. THE RECORD IS WRITTEN 0600 THROUGH A TEMPORARY, so a link planted at its name is replaced
/// and its target never written; a clear run twice is fine; and the temporary a writer that died
/// mid-write left beside it is swept, while a live process's is kept.
/// NEGATIVE CONTROL: an in-place open of the name writes through the link.
#[cfg(unix)]
#[test]
fn the_record_is_written_private_never_through_a_link_and_its_dead_temporaries_swept() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::TempDir::new().expect("a scratch dir");
    let dir = scratch.path();
    let target = dir.join("unrelated-owned-file");
    std::fs::write(&target, b"private fixture sentinel\n").expect("plant the target");
    std::os::unix::fs::symlink(&target, dir.join(NAME)).expect("plant a link");
    let record = line(2, Some(NOW_MS - 1_000), None);
    write_in(dir, &record).expect("written");
    assert_eq!(
        std::fs::read(&target).expect("the target"),
        b"private fixture sentinel\n",
        "the link's target is untouched"
    );
    let meta = std::fs::symlink_metadata(dir.join(NAME)).expect("the record");
    assert!(meta.file_type().is_file(), "the link was replaced");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(on_disk(dir).as_deref(), Some(record.as_bytes()));
    assert_eq!(read_in(dir), Some((2, Some(NOW_MS - 1_000), None)));

    clear_in(dir).expect("cleared");
    clear_in(dir).expect("cleared again");
    assert_eq!(on_disk(dir), None);
    assert!(target.exists());

    let dead = dir.join(format!(".{NAME}.tmp.4294967295.0123abcd"));
    let alive = dir.join(format!(".{NAME}.tmp.{}.0123abcd", std::process::id()));
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    for temp in [&dead, &alive] {
        std::fs::File::create(temp)
            .and_then(|file| file.set_modified(two_hours_ago))
            .expect("plant an old temporary");
    }
    sweep(dir);
    assert!(!dead.exists(), "a writer no process has");
    assert!(alive.exists(), "this process's own");
}

/// B3. ONLY THIS USER'S OWN REGULAR FILE, WITH ONE LINK, IS READ: a link to a valid record, a
/// folder, a file past the cap, a FIFO, a group-writable record and one with a second hard link
/// each read as none. NEGATIVE CONTROL: a reader that follows links reads the linked record.
#[cfg(unix)]
#[test]
fn only_an_owned_regular_record_is_read() {
    use std::os::unix::fs::PermissionsExt as _;
    let valid = line(1, Some(NOW_MS - 1_000), None);
    let fresh = || {
        let scratch = aterm_tempfile::TempDir::new().expect("a scratch dir");
        let record = scratch.path().join(NAME);
        (scratch, record)
    };
    let (scratch, record) = fresh();
    write_in(scratch.path(), &valid).expect("written");
    assert!(
        read_in(scratch.path()).is_some(),
        "PRECONDITION: a valid record reads"
    );

    let (scratch, record_link) = fresh();
    let elsewhere = scratch.path().join("elsewhere");
    std::fs::write(&elsewhere, &valid).expect("a valid record elsewhere");
    std::os::unix::fs::symlink(&elsewhere, &record_link).expect("a link to it");
    assert_eq!(read_in(scratch.path()), None, "a link");

    let (folder, at) = fresh();
    std::fs::create_dir(&at).expect("a folder");
    assert_eq!(read_in(folder.path()), None, "a folder");

    let (big, at) = fresh();
    std::fs::write(&at, vec![b'1'; 4096]).expect("a big file");
    assert_eq!(read_in(big.path()), None, "past the cap");

    let (fifo, at) = fresh();
    let path = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()).expect("no NUL");
    // SAFETY: `path` is a NUL-terminated path in a scratch dir this test owns.
    let made = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(made, 0, "a FIFO");
    assert_eq!(read_in(fifo.path()), None, "a FIFO");

    let (writable, at) = fresh();
    write_in(writable.path(), &valid).expect("written");
    std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o620)).expect("chmod");
    assert_eq!(read_in(writable.path()), None, "group-writable");

    let (linked, at) = fresh();
    write_in(linked.path(), &valid).expect("written");
    std::fs::hard_link(&at, linked.path().join("second-name")).expect("a second link");
    assert_eq!(read_in(linked.path()), None, "a second link");

    drop(record);
}
