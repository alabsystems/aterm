// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness_ab::AbArm;

const DAY: i64 = 20_002;
const HOUR: u64 = 3_600_000;

/// A day whose Cruise arm was busy `busy_ms` and saw `boxes` boxes in
/// bucket 3, and `boxes` landings.
fn day_of(day: i64, first_unix_ms: i64, busy_ms: u64, boxes: u32) -> AbDay {
    let mut kept = AbDay::new(day, first_unix_ms);
    kept.arms[AbArm::Cruise.index()].busy_ms = busy_ms;
    kept.arms[AbArm::Off.index()].at_prompt_ms = busy_ms / 2;
    kept.arms[AbArm::Land.index()].landed_ms = 7;
    kept.holds[0][0][3] = boxes;
    kept.holds[2][1][19] = 1;
    kept.ends.landed = boxes;
    kept.ends.landing[10] = boxes;
    kept
}

/// A ring holding `days`.
fn ring_of(days: &[AbDay]) -> AbRing {
    let mut ring: AbRing = [None; DAYS];
    for day in days {
        ring[usize::try_from(day.day.rem_euclid(DAYS as i64)).unwrap()] = Some(*day);
    }
    ring
}

/// A FILE ROUND TRIP: what one process writes another reads back day for
/// day — every arm's six times and every histogram — and the file is the
/// owner's alone (0600), named for its process. NEGATIVE CONTROL: the
/// writer's own file is skipped by its own reading (the live book holds it).
#[test]
fn a_book_written_is_read_back_whole() {
    let state = aterm_tempfile::tempdir().expect("scratch state");
    let days = [day_of(DAY - 1, 100, HOUR, 4), day_of(DAY, 200, 2 * HOUR, 1)];
    write_own(state.path(), "1000-7.json", &ring_of(&days), DAY, 5).expect("written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir_of(state.path()).join("1000-7.json");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert_eq!(read_others(state.path(), "2000-8.json", DAY), days);
    assert_eq!(
        read_others(state.path(), "1000-7.json", DAY),
        Vec::<AbDay>::new(),
        "its own file is the live book's"
    );
    assert!(is_book_name(&own_name()), "{}", own_name());
    assert_eq!(own_name(), own_name(), "one name per process");
}

/// TWO PROCESSES' FILES ARE SUMMED BY DAY, AND NOTHING TWICE: a seamless
/// update's outgoing book and another instance's are added day by day,
/// the earlier first stretch kept. NEGATIVE CONTROL: the reader's own file
/// (a third, same days) adds nothing.
#[test]
fn two_processes_books_are_summed_by_day_once() {
    let state = aterm_tempfile::tempdir().expect("scratch state");
    let a = [day_of(DAY, 300, HOUR, 2)];
    let b = [day_of(DAY - 2, 50, HOUR, 1), day_of(DAY, 100, 3 * HOUR, 5)];
    write_own(state.path(), "1-10.json", &ring_of(&a), DAY, 0).unwrap();
    write_own(state.path(), "2-10.json", &ring_of(&b), DAY, 0).unwrap();
    write_own(state.path(), "3-11.json", &ring_of(&b), DAY, 0).unwrap();
    let others = read_others(state.path(), "3-11.json", DAY);
    let mut today = a[0];
    today.plus(&b[1]);
    assert_eq!(others, [b[0], today]);
    assert_eq!(others[1].first_unix_ms, 100);
    assert_eq!(others[1].arms[AbArm::Cruise.index()].busy_ms, 4 * HOUR);
    assert_eq!(others[1].holds[0][0][3], 7);
}

/// A FILE WITH NO DAY IN THE LAST 8 IS REMOVED, and counts nothing; a
/// newer schema and an unreadable file are passed over and kept; a name
/// that is no book's, or a directory, is left alone. NEGATIVE CONTROL: a
/// file whose oldest day is 8 days back but holds one 7 back is kept, and
/// only that day is counted.
#[test]
fn an_old_file_is_removed_and_a_strange_one_passed_over() {
    let state = aterm_tempfile::tempdir().expect("scratch state");
    let dir = dir_of(state.path());
    let old = [day_of(DAY - 8, 1, HOUR, 1)];
    write_own(state.path(), "1-1.json", &ring_of(&old), DAY - 8, 0).unwrap();
    let edge = [day_of(DAY - 8, 1, HOUR, 1), day_of(DAY - 7, 2, HOUR, 1)];
    write_own(state.path(), "2-2.json", &ring_of(&edge), DAY - 7, 0).unwrap();
    std::fs::write(
        dir.join("3-3.json"),
        "{\"schema\":2,\"pid\":3,\"days\":\"another shape\"}",
    )
    .unwrap();
    std::fs::write(dir.join("4-4.json"), "{not json").unwrap();
    std::fs::write(dir.join("notes.json"), "{\"schema\":1}").unwrap();
    std::fs::create_dir(dir.join("5-5.json")).unwrap();
    let others = read_others(state.path(), "9-9.json", DAY);
    assert_eq!(others, [edge[1]], "the day 7 back alone");
    assert!(
        !dir.join("1-1.json").exists(),
        "no day in the last 8: removed"
    );
    for kept in ["2-2.json", "3-3.json", "4-4.json", "notes.json", "5-5.json"] {
        assert!(dir.join(kept).exists(), "{kept} kept");
    }
    assert_eq!(
        read_others(&state.path().join("absent"), "9-9.json", DAY),
        Vec::<AbDay>::new()
    );
}

/// A BOOK WITH NOTHING TO KEEP WRITES NO FILE and removes its old one: a
/// process whose days all left the last 8. NEGATIVE CONTROL: with a day in
/// them it writes.
#[test]
fn a_book_with_nothing_to_keep_leaves_no_file() {
    let state = aterm_tempfile::tempdir().expect("scratch state");
    let path = dir_of(state.path()).join("1-1.json");
    let ring = ring_of(&[day_of(DAY, 1, HOUR, 1)]);
    write_own(state.path(), "1-1.json", &ring, DAY, 0).unwrap();
    assert!(path.exists());
    write_own(state.path(), "1-1.json", &ring, DAY + 8, 0).unwrap();
    assert!(!path.exists());
    write_own(state.path(), "1-1.json", &ring, DAY + 8, 0).expect("nothing to remove");
}

/// Snapshot order, not worker arrival order, decides this book's file. Both
/// arrival orders use actual atomic replacement and another book's real reader.
#[test]
fn a_late_snapshot_cannot_replace_the_newer_book_written_at_exit() {
    for old_first in [true, false] {
        let state = aterm_tempfile::tempdir().expect("scratch state");
        let writes = std::sync::Arc::new(AbWrites::default());
        let old = writes
            .snapshot((ring_of(&[day_of(DAY, 100, HOUR, 1)]), DAY))
            .unwrap();
        let newer = writes
            .snapshot((ring_of(&[day_of(DAY, 100, 2 * HOUR, 2)]), DAY))
            .unwrap();
        if old_first {
            old.write_if_newer(state.path(), "1-1.json", 10).unwrap();
            assert_eq!(
                read_others(state.path(), "2-2.json", DAY),
                [day_of(DAY, 100, HOUR, 1)]
            );
        }
        newer.write_if_newer(state.path(), "1-1.json", 20).unwrap();
        assert_eq!(
            read_others(state.path(), "2-2.json", DAY),
            [day_of(DAY, 100, 2 * HOUR, 2)]
        );
        if !old_first {
            old.write_if_newer(state.path(), "1-1.json", 10).unwrap();
        }
        assert_eq!(
            read_others(state.path(), "2-2.json", DAY),
            [day_of(DAY, 100, 2 * HOUR, 2)],
            "AB_SNAPSHOT_MUST_NOT_REGRESS: a delayed worker must not replace the exit book"
        );
    }
}

/// One day on disk as a build before the ends wrote it: no `ends` field.
#[derive(Debug, Serialize, Deserialize)]
struct OldDayFile {
    day: i64,
    first_unix_ms: i64,
    arms: [[u64; 6]; 4],
    holds: [[[u32; BUCKETS]; 2]; 3],
}

/// One book on disk as that build read it.
#[derive(Debug, Deserialize)]
struct OldBookFile {
    schema: u32,
    days: Vec<OldDayFile>,
}

/// The same, as it would read with `deny_unknown_fields`: the NEGATIVE
/// CONTROL, which a newer file must break.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(dead_code, reason = "parsed, never read")]
struct StrictDayFile {
    day: i64,
    first_unix_ms: i64,
    arms: [[u64; 6]; 4],
    holds: [[[u32; BUCKETS]; 2]; 3],
}

/// The strict copy's book.
#[derive(Debug, Deserialize)]
#[expect(dead_code, reason = "parsed, never read")]
struct StrictBookFile {
    days: Vec<StrictDayFile>,
}

/// THE ENDS ARE AN ADDITIVE FIELD OF SCHEMA 1 (§11.6.3 item 4, §11.9
/// "Schema-1 additive fields"): a book written now round-trips its ends; a
/// file written before them reads with none; a file written now parses in
/// the older build's `DayFile`, which skips them, its other numbers whole;
/// and a day with no end writes no `ends` at all. An older build's `done`
/// and `quiet` counts (Cruise's dones and Land's quiet ends, neither of
/// which FOREVER CRUISE / FOREVER LAND has) are passed over, its landings
/// read whole. NEGATIVE CONTROL: the older struct with `deny_unknown_fields`
/// refuses the newer file.
#[test]
fn the_ends_read_across_builds() {
    let state = aterm_tempfile::tempdir().expect("scratch state");
    let day = day_of(DAY, 100, HOUR, 4);
    write_own(state.path(), "1-1.json", &ring_of(&[day]), DAY, 0).unwrap();
    assert_eq!(read_others(state.path(), "9-9.json", DAY), [day]);
    let text = std::fs::read_to_string(dir_of(state.path()).join("1-1.json")).unwrap();
    assert!(text.contains("\"ends\""), "{text}");
    // The older build reads the newer file, and skips the ends.
    let old: OldBookFile = aterm_json::from_str(&text).expect("an older build reads it");
    assert_eq!(old.schema, SCHEMA);
    assert_eq!(old.days[0].arms[AbArm::Cruise.index()][0], HOUR);
    assert_eq!(old.days[0].holds[0][0][3], 4);
    assert!(
        aterm_json::from_str::<StrictBookFile>(&text).is_err(),
        "CONTROL: a reader that denies unknown fields would not"
    );
    // The newer build reads an older file, with no end.
    let older = BookFile {
        schema: SCHEMA,
        pid: 2,
        written_unix_ms: 0,
        days: Vec::new(),
    };
    let mut older = aterm_json::to_string(&older).unwrap();
    let old_day = aterm_json::to_string(&OldDayFile {
        day: DAY,
        first_unix_ms: 100,
        arms: old.days[0].arms,
        holds: old.days[0].holds,
    })
    .unwrap();
    older = older.replace("\"days\":[]", &format!("\"days\":[{old_day}]"));
    assert!(older.contains(&old_day), "{older}");
    std::fs::write(dir_of(state.path()).join("2-2.json"), &older).unwrap();
    let read = read_others(state.path(), "1-1.json", DAY);
    let mut want = day;
    want.ends = AbEnds::default();
    assert_eq!(read, [want], "an older file reads as no end");
    // A day with no end writes none.
    let mut quiet = day_of(DAY, 100, HOUR, 0);
    quiet.ends = AbEnds::default();
    write_own(state.path(), "3-3.json", &ring_of(&[quiet]), DAY, 0).unwrap();
    let text = std::fs::read_to_string(dir_of(state.path()).join("3-3.json")).unwrap();
    assert!(!text.contains("\"ends\""), "{text}");
    // An older build's done and quiet counts are passed over.
    let older_ends = aterm_tempfile::tempdir().expect("scratch state");
    let two = day_of(DAY, 100, HOUR, 2);
    write_own(older_ends.path(), "4-4.json", &ring_of(&[two]), DAY, 0).unwrap();
    let path = dir_of(older_ends.path()).join("4-4.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let with_old = text.replace(
        "\"ends\":{\"landed\":2",
        "\"ends\":{\"done\":3,\"quiet\":4,\"landed\":2",
    );
    assert_ne!(with_old, text, "the older counts are in: {text}");
    std::fs::write(&path, &with_old).unwrap();
    let read = read_others(older_ends.path(), "9-9.json", DAY);
    assert_eq!(read, [two], "the landings stand, the rest passed over");
    assert_eq!(read[0].ends.landed, 2);
}
