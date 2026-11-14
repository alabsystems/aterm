// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::fleet_access::{DriveKind, FleetAccess};
use aterm_types::fleet_activity::{Outcome, Plain, Way};

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SID: &str = "s-0123456789abcdef0123";

const BLOCKED: Outcome = Outcome::Blocked {
    level: FleetAccess::ReadOnly,
    needs: FleetAccess::Full,
};

fn row(at_ms: u64, way: Way, node: &str, kind: DriveKind, outcome: Outcome) -> Row {
    Row::of(at_ms, way, (node, Some("m86")), kind, Some(SID), outcome).expect("an agent request")
}

fn scratch() -> (aterm_tempfile::TempDir, Record) {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let record = Record::in_state(dir.path());
    (dir, record)
}

/// Write `rows` one write each, as one request after another does.
fn write_each(record: &Record, rows: &[Row]) {
    for row in rows {
        record.append(std::slice::from_ref(row)).expect("written");
    }
}

/// How many requests `rows` stand for.
fn requests(rows: &[Row]) -> u64 {
    rows.iter().map(|row| u64::from(row.n)).sum()
}

#[test]
fn rows_written_are_read_back_oldest_first() {
    let (_dir, record) = scratch();
    assert_eq!(record.read(10_000), Kept::default(), "no record yet");
    let rows = [
        row(3_000, Way::Out, B, DriveKind::Spawn, BLOCKED),
        row(1_000, Way::In, A, DriveKind::Turn, Outcome::Admitted),
        row(2_000, Way::In, A, DriveKind::Post, Outcome::Admitted),
    ];
    write_each(&record, &rows);
    let read = record.read(10_000);
    assert_eq!(
        read.rows,
        [rows[1].clone(), rows[2].clone(), rows[0].clone()]
    );
    assert_eq!(read.cut_ms, None, "nothing was dropped");
    // A line that is no row is passed over, and the rows beside it still read.
    aterm_uds::line_log::append(record.path(), "not a row").expect("appended");
    assert_eq!(record.read(10_000), read);
}

/// Rows handed over together are one write, in the order given.
#[test]
fn rows_of_one_request_are_written_together() {
    let (_dir, record) = scratch();
    record.append(&[]).expect("nothing to write");
    assert!(!record.path().exists(), "no rows, no record");
    let rows = [
        row(1_000, Way::Out, A, DriveKind::Read, Outcome::Admitted),
        row(1_001, Way::Out, B, DriveKind::Read, Outcome::Admitted),
    ];
    record.append(&rows).expect("written");
    let text = std::fs::read_to_string(record.path()).expect("the file");
    assert_eq!(text, format!("{}\n{}\n", rows[0].line(), rows[1].line()));
}

#[test]
fn a_block_is_written_every_time_and_a_minutes_reads_fold_into_one_row() {
    let (_dir, record) = scratch();
    let minute = 600_000;
    for at in 0..5 {
        record.note(&row(minute + at, Way::In, A, DriveKind::Spawn, BLOCKED));
    }
    // Reads of one Mac, with another Mac's read and a block between them.
    for at in 0..40 {
        record.note(&row(
            minute + 100 + at,
            Way::In,
            A,
            DriveKind::Read,
            Outcome::Admitted,
        ));
        if at == 10 {
            record.note(&row(
                minute + 150,
                Way::In,
                B,
                DriveKind::Read,
                Outcome::Admitted,
            ));
            record.note(&row(minute + 151, Way::In, A, DriveKind::Turn, BLOCKED));
        }
    }
    // The next minute's read starts a row of its own.
    record.note(&row(
        minute + 60_000,
        Way::In,
        A,
        DriveKind::Read,
        Outcome::Admitted,
    ));
    let rows = record.read(minute + 60_000).rows;
    let blocked = rows.iter().filter(|row| row.blocked()).count();
    assert_eq!(blocked, 6, "every block has its row");
    let reads: Vec<(&str, u32, u64)> = rows
        .iter()
        .filter(|row| row.what == Plain::Read)
        .map(|row| (row.node.as_str(), row.n, row.at_ms))
        .collect();
    assert_eq!(
        reads,
        [
            (A, 40, minute + 139),
            (B, 1, minute + 150),
            (A, 1, minute + 60_000)
        ]
    );
    assert_eq!(rows.len(), 6 + 3);
}

/// The size bound: a write never leaves the record past it. Requests that no minute's count
/// can stand for (each of another Mac) go oldest first, and the record says up to when.
#[test]
fn the_record_stays_within_its_size_and_says_what_it_dropped() {
    let (_dir, record) = scratch();
    let from = |at: u64| {
        row(
            1_000_000 + at,
            Way::In,
            &format!("{at:032x}"),
            DriveKind::Turn,
            Outcome::Admitted,
        )
    };
    let one = from(0).line().len() as u64 + 1;
    let writes = 2 * MAX_BYTES / one;
    for at in 0..writes {
        record.note(&from(at));
        let len = std::fs::metadata(record.path()).expect("the file").len();
        assert!(len <= MAX_BYTES, "{len} bytes after write {at}");
    }
    let read = record.read(1_000_000 + writes);
    let rows = read.rows;
    assert!(
        rows.len() as u64 >= KEPT_BYTES / one - 1,
        "{} rows kept",
        rows.len()
    );
    assert!((rows.len() as u64) < writes, "the oldest rows went");
    let newest: Vec<u64> = rows.iter().map(|row| row.at_ms).collect();
    let first = 1_000_000 + writes - rows.len() as u64;
    assert_eq!(newest, (first..1_000_000 + writes).collect::<Vec<u64>>());
    assert_eq!(
        read.cut_ms,
        Some(first - 1),
        "the newest row dropped is named"
    );
}

/// A flood of one Mac's blocked requests never pushes a request out of the record: past its
/// size the record keeps each minute's like requests as one counted row, so every block is
/// still counted and nothing is cut.
#[test]
fn a_flood_of_blocks_is_counted_by_the_minute_and_drops_nothing() {
    let (_dir, record) = scratch();
    let quiet = row(1_000, Way::In, B, DriveKind::Turn, Outcome::Admitted);
    record.note(&quiet);
    let minute = 600_000;
    let one = row(minute, Way::In, A, DriveKind::Spawn, BLOCKED)
        .line()
        .len() as u64
        + 1;
    let writes = 2 * MAX_BYTES / one;
    for at in 0..writes {
        record.note(&row(
            minute + at % 60_000,
            Way::In,
            A,
            DriveKind::Spawn,
            BLOCKED,
        ));
        let len = std::fs::metadata(record.path()).expect("the file").len();
        assert!(len <= MAX_BYTES, "{len} bytes after write {at}");
    }
    let read = record.read(minute + 60_000);
    assert_eq!(read.cut_ms, None, "no request was dropped");
    assert_eq!(read.rows.first(), Some(&quiet), "the oldest row is kept");
    let blocked: Vec<Row> = read.rows.into_iter().filter(Row::blocked).collect();
    assert_eq!(requests(&blocked), writes, "every block is counted");
    assert!(
        (blocked.len() as u64) < writes,
        "{} rows for {writes} blocks",
        blocked.len()
    );
}

/// The age bound: a read passes over a row older than a week, and a rewrite drops it.
#[test]
fn a_row_past_its_age_is_not_read_and_does_not_survive_a_rewrite() {
    let (_dir, record) = scratch();
    let old = row(1_000, Way::In, A, DriveKind::Turn, Outcome::Admitted);
    let new = row(
        1_000 + MAX_AGE_MS + 1,
        Way::In,
        A,
        DriveKind::Turn,
        Outcome::Admitted,
    );
    record.note(&old);
    assert_eq!(
        record.read(1_000 + MAX_AGE_MS).rows,
        std::slice::from_ref(&old),
        "a week old is still read"
    );
    record.note(&new);
    assert_eq!(record.read(new.at_ms).rows, std::slice::from_ref(&new));
    let text = format!("{}\n{}\n", old.line(), new.line());
    assert_eq!(replaced(&text, new.at_ms), format!("{}\n", new.line()));
}

/// The clock was set back: a row stamped after now reads as written now, the next write
/// restamps it on disk, and it then leaves the record at its age like any other row. Left as
/// stamped it would stay for as long as the clock had been ahead.
#[test]
fn a_row_stamped_after_the_clock_is_restamped_and_ages_out() {
    let (_dir, record) = scratch();
    let now = 100 * MAX_AGE_MS;
    let ahead = now + 30 * MAX_AGE_MS;
    let block = row(ahead, Way::In, A, DriveKind::Spawn, BLOCKED);
    record.note(&block);
    // A reader settles it without writing anything.
    let read = record.read(now).rows;
    assert_eq!(read.len(), 1);
    assert_eq!((read[0].at_ms, read[0].blocked()), (now, true));
    // The next write, on the clock as it is now, restamps it.
    let turn = row(now + 5, Way::In, B, DriveKind::Turn, Outcome::Admitted);
    record.note(&turn);
    let text = std::fs::read_to_string(record.path()).expect("the file");
    let stamps: Vec<u64> = text
        .lines()
        .filter_map(Row::parse)
        .map(|row| row.at_ms)
        .collect();
    assert_eq!(stamps, [now + 5, now + 5], "{text}");
    assert_eq!(requests(&record.read(now + 5).rows), 2);
    assert_eq!(record.read(now + 5 + MAX_AGE_MS + 1).rows, Vec::new());
    // A stamp within the clocks' slack is no clock set back: the row is appended.
    let near = row(
        now + 6 + AHEAD_MS,
        Way::In,
        A,
        DriveKind::Turn,
        Outcome::Admitted,
    );
    record.note(&near);
    record.note(&row(now + 6, Way::In, A, DriveKind::Key, Outcome::Admitted));
    let text = std::fs::read_to_string(record.path()).expect("the file");
    assert!(text.contains(&near.line()), "{text}");
}

/// Writing is best effort: where the record cannot be written, `note` says nothing and fails
/// nothing, and the read finds no rows.
#[cfg(unix)]
#[test]
fn a_record_that_cannot_be_written_drops_the_row_and_fails_nothing() {
    use std::os::unix::fs::PermissionsExt as _;
    let turn = row(1_000, Way::In, A, DriveKind::Turn, Outcome::Admitted);
    let one = std::slice::from_ref(&turn);
    // The state folder takes no new entry: the record's folder cannot be made.
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let closed = dir.path().join("closed");
    std::fs::create_dir(&closed).expect("a folder");
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o500)).expect("read only");
    let record = Record::in_state(&closed);
    assert!(record.append(one).is_err());
    record.note(&turn);
    assert_eq!(record.read(2_000), Kept::default());
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o700)).expect("restored");
    // Something that is no folder stands where the record's folder goes.
    let dir = aterm_tempfile::tempdir().expect("scratch");
    std::fs::write(dir.path().join(DIR), b"in the way").expect("a file");
    let record = Record::in_state(dir.path());
    assert!(record.append(one).is_err());
    record.note(&turn);
    assert_eq!(record.read(2_000), Kept::default());
    // The rows' own path is a folder.
    let dir = aterm_tempfile::tempdir().expect("scratch");
    std::fs::create_dir_all(dir.path().join(DIR).join(ROWS)).expect("a folder");
    let record = Record::in_state(dir.path());
    record.note(&turn);
    assert_eq!(record.read(2_000), Kept::default());
}

/// A WRITE THAT FAILS LOSES NO ROW ALREADY WRITTEN. The record is at its size, so the next
/// write replaces it whole, and the replacement cannot be made (its folder takes no new file):
/// the write fails, the record is byte for byte what it was, and once the folder takes files
/// again the next write keeps every request counted.
#[cfg(unix)]
#[test]
fn a_replacement_that_fails_leaves_the_record_as_it_was() {
    use std::os::unix::fs::PermissionsExt as _;
    let (dir, record) = scratch();
    let minute = 600_000;
    let first = row(minute, Way::In, A, DriveKind::Spawn, BLOCKED);
    record.note(&first);
    let one = first.line().len() as u64 + 1;
    let fill = (MAX_BYTES / one) as usize - 1;
    let rows = vec![first; fill];
    record.append(&rows).expect("filled, within the size");
    let before = std::fs::read(record.path()).expect("the file");
    assert!(
        before.len() as u64 + one > MAX_BYTES,
        "the next write replaces"
    );
    let folder = dir.path().join(DIR);
    let turn = row(minute + 5, Way::In, B, DriveKind::Turn, Outcome::Admitted);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500)).expect("read only");
    let failed = record.append(std::slice::from_ref(&turn));
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).expect("restored");
    assert!(failed.is_err(), "the replacement could not be made");
    assert_eq!(
        std::fs::read(record.path()).expect("the file"),
        before,
        "nothing written before it is gone"
    );
    record.append(std::slice::from_ref(&turn)).expect("written");
    let read = record.read(minute + 5);
    assert_eq!(read.cut_ms, None);
    assert_eq!(requests(&read.rows), fill as u64 + 2);
    let len = std::fs::metadata(record.path()).expect("the file").len();
    assert!(len < KEPT_BYTES, "{len} bytes: counted by the minute");
    let left: Vec<_> = std::fs::read_dir(&folder)
        .expect("the folder")
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .filter(|name| name != ROWS && name != "rows.lock")
        .collect();
    assert_eq!(left, Vec::<std::ffi::OsString>::new(), "no temporary file");
}

/// A writer that finds the record's lock held gives the row up within the lock's bound, and the
/// row written once the lock is free lands.
#[test]
fn a_locked_record_drops_the_row_and_takes_the_next() {
    let (_dir, record) = scratch();
    let first = row(1_000, Way::In, A, DriveKind::Turn, Outcome::Admitted);
    record.note(&first);
    let lock = aterm_uds::line_log::lock(record.path()).expect("the record's lock");
    let second = row(2_000, Way::In, A, DriveKind::Key, Outcome::Admitted);
    assert!(
        record.append(std::slice::from_ref(&second)).is_err(),
        "the lock is held"
    );
    record.note(&second);
    // A reader takes no lock: it reads what is there while a writer holds it.
    assert_eq!(record.read(5_000).rows, std::slice::from_ref(&first));
    drop(lock);
    let third = row(3_000, Way::In, A, DriveKind::Send, Outcome::Admitted);
    record.note(&third);
    assert_eq!(record.read(5_000).rows, [first, third]);
}

#[test]
fn a_reader_that_polls_reads_again_only_what_was_written_since() {
    let (_dir, record) = scratch();
    let mut seen = None;
    let mut read_new = |at| record.read_new(&mut seen, at).map(|kept| kept.rows);
    assert_eq!(read_new(5_000), None, "no record, as before");
    let first = row(1_000, Way::In, A, DriveKind::Turn, Outcome::Admitted);
    record.note(&first);
    assert_eq!(read_new(5_000), Some(vec![first.clone()]));
    assert_eq!(read_new(5_000), None, "nothing written since");
    let second = row(2_000, Way::Out, B, DriveKind::Spawn, BLOCKED);
    record.note(&second);
    assert_eq!(read_new(5_000), Some(vec![first, second]));
}

#[test]
fn the_menus_last_open_is_kept_beside_the_rows() {
    let (dir, record) = scratch();
    assert_eq!(record.menu_seen(), None, "never opened");
    record.note_menu_seen(1_700_000_000_123).expect("written");
    assert_eq!(record.menu_seen(), Some(1_700_000_000_123));
    record
        .note_menu_seen(1_700_000_009_000)
        .expect("written again");
    assert_eq!(record.menu_seen(), Some(1_700_000_009_000));
    // It is no row: the record reads none, and another reader of the folder finds the time.
    assert_eq!(record.read(1_700_000_010_000), Kept::default());
    assert_eq!(
        Record::in_state(dir.path()).menu_seen(),
        Some(1_700_000_009_000)
    );
    // What is not a time is read as none.
    std::fs::write(dir.path().join(DIR).join("menu-seen"), "soon\n").expect("written");
    assert_eq!(record.menu_seen(), None);
}

/// A serving process's rows reach the record through its writer thread, in the order handed.
#[test]
fn rows_handed_to_the_writer_thread_are_written_in_order() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let rows: Vec<Row> = (0..20)
        .map(|at| row(1_000 + at, Way::In, A, DriveKind::Turn, Outcome::Admitted))
        .collect();
    for pair in rows.chunks(2) {
        note_later(dir.path(), pair.to_vec());
    }
    note_later(dir.path(), Vec::new());
    settle();
    assert_eq!(Record::in_state(dir.path()).read(5_000).rows, rows);
}
