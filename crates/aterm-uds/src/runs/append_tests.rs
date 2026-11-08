// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HEADER: &str = "run v=1 boot=b id=71-100 uniqueid=1 start=1 sid=-\n\
    group pgid=200 leader_birth=2 leader_uniqueid=2 stage=test\n";
const CLOSED: &str = "closed pgid=200";
const DEAD: &str = "dead_seen uniqueid=900 at=1000";

fn record() -> (aterm_tempfile::TempDir, PathBuf, std::fs::File) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("71-100.run");
    let file = open_regular_write(&path, false).unwrap();
    write_record_text(&file, HEADER).unwrap();
    (dir, path, file)
}

struct HeldReader(std::fs::File);

impl HeldReader {
    fn on(path: &Path) -> Self {
        let file = open_regular_write(path, true).unwrap();
        crate::keep_out_of_children(&file).unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "the native reader fixture explicitly unlocks its owned file in Drop"
        )]
        file.try_lock_shared().unwrap();
        Self(file)
    }
}

impl Drop for HeldReader {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[test]
fn a_native_reader_does_not_discard_a_complete_record_write() {
    let (dir, path, retained) = record();
    let mut held = Some(HeldReader::on(&path));
    let started = Instant::now() - Duration::from_secs(2);
    let line = format!("{CLOSED}\n");
    crate::file_lock::with_lock_wait_observer(
        move |_| {
            drop(held.take());
        },
        || {
            write_record_with(
                &retained,
                line.as_bytes(),
                record_write_deadline_at(started),
                |mut file, bytes| file.write_all(bytes),
            )
        },
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{HEADER}{CLOSED}\n")
    );
    assert!(read_records(dir.path()).unwrap()[0].1.groups[0].closed);
}

#[test]
fn an_expired_complete_record_write_retains_the_original_evidence() {
    let (_dir, path, retained) = record();
    let reader = HeldReader::on(&path);
    let mut called = false;
    let error = write_record_with(
        &retained,
        format!("{DEAD}\n").as_bytes(),
        record_write_deadline_at(Instant::now() - Duration::from_secs(61)),
        |mut file, bytes| {
            called = true;
            file.write_all(bytes)
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert!(!called);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HEADER);
    drop(reader);
}

#[test]
fn retained_writer_preserves_a_line_appended_through_another_open() {
    let (dir, path, retained) = record();
    append(&path, DEAD).unwrap();
    write_line(&retained, CLOSED).unwrap();
    let wanted = format!("{HEADER}{DEAD}\n{CLOSED}\n");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), wanted);
    let records = read_records(dir.path()).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].1.dead_seen, Some(900));
    assert!(records[0].1.groups[0].closed);
}

#[test]
fn creating_a_record_never_truncates_existing_evidence() {
    let (_dir, path, retained) = record();
    let held = RecordLock::write(&retained, Instant::now()).unwrap();
    let error = open_regular_write(&path, false).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    drop(held);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HEADER);
}

#[test]
fn a_short_line_write_excludes_an_independent_appender_until_its_newline() {
    let (dir, path, retained) = record();
    let competing = open_regular_write(&path, true).unwrap();
    let mut first = true;
    let mut blocked = false;
    write_line_with(&retained, CLOSED, |mut file, bytes| {
        if first {
            first = false;
            let split = bytes.len().min(6);
            file.write_all(&bytes[..split])?;
            #[expect(
                clippy::disallowed_methods,
                reason = "the probe explicitly unlocks before calling the independent writer"
            )]
            match competing.try_lock() {
                Ok(()) => {
                    competing.unlock()?;
                    write_line(&competing, DEAD)?;
                }
                Err(std::fs::TryLockError::WouldBlock) => blocked = true,
                Err(std::fs::TryLockError::Error(why)) => return Err(why),
            }
            file.write_all(&bytes[split..])
        } else {
            file.write_all(bytes)
        }
    })
    .unwrap();
    if blocked {
        write_line(&competing, DEAD).unwrap();
    }
    let read = read_records(dir.path());
    assert!(blocked, "the complete logical line owns the file lock");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{HEADER}{CLOSED}\n{DEAD}\n")
    );
    assert_eq!(read.unwrap().len(), 1);
}

#[test]
fn a_reader_does_not_admit_a_writer_midway_through_a_line() {
    let (dir, _, retained) = record();
    let mut observed = None;
    write_line_with(&retained, CLOSED, |mut file, bytes| {
        file.write_all(&bytes[..6])?;
        observed = Some(read_records(dir.path()).unwrap_err().kind());
        file.write_all(&bytes[6..])
    })
    .unwrap();
    assert_eq!(observed, Some(std::io::ErrorKind::WouldBlock));
    let records = read_records(dir.path()).unwrap();
    assert!(records[0].1.groups[0].closed);
}

#[test]
fn an_expired_contended_write_preserves_the_record_and_its_error() {
    let (dir, path, retained) = record();
    let competing = open_regular_write(&path, true).unwrap();
    let reading = RecordLock::read(&retained).unwrap();
    let mut called = false;
    let error = write_record_with(&competing, b"closed pgid=200\n", Instant::now(), |_, _| {
        called = true;
        Ok(())
    })
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert!(!called);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HEADER);
    drop(reading);
    write_line(&competing, CLOSED).unwrap();
    assert!(read_records(dir.path()).unwrap()[0].1.groups[0].closed);
}

#[test]
fn a_partial_write_error_releases_the_lock_without_hiding_the_error() {
    let (dir, path, retained) = record();
    let competing = open_regular_write(&path, true).unwrap();
    let error = write_line_with(&retained, CLOSED, |mut file, bytes| {
        file.write_all(&bytes[..6])?;
        Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
    })
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
    let available = RecordLock::write(&competing, Instant::now()).unwrap();
    drop(available);
    assert_eq!(
        read_records(dir.path()).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData,
        "a torn record stays unavailable after the writer releases it"
    );
}

#[test]
fn a_panicking_writer_releases_the_cross_process_lock() {
    let (_dir, path, retained) = record();
    let competing = open_regular_write(&path, true).unwrap();
    let panicked = std::panic::catch_unwind(|| {
        let _ = write_line_with(&retained, CLOSED, |_, _| panic!("injected write panic"));
    });
    assert!(panicked.is_err());
    let available = RecordLock::write(&competing, Instant::now()).unwrap();
    drop(available);
}

#[test]
fn stale_observers_receive_the_actual_first_recorded_horizon() {
    let (dir, path, _retained) = record();
    let expected = read_records(dir.path()).unwrap().remove(0).1;
    let first = append_dead_seen(&path, &expected, 300, 1).unwrap();
    let second = append_dead_seen(&path, &expected, 900, 2).unwrap();
    assert_eq!(
        first,
        DeadSeen {
            horizon: 300,
            at_ms: 1
        }
    );
    assert_eq!(second, first);
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        format!("{HEADER}dead_seen uniqueid=300 at=1\n")
    );
}

#[test]
fn dead_seen_refuses_moved_or_malformed_evidence_without_writing() {
    let (dir, path, _retained) = record();
    let expected = read_records(dir.path()).unwrap().remove(0).1;
    for text in [
        HEADER.replace("boot=b", "boot=other"),
        format!("{HEADER}root path=/different\n"),
        format!("{HEADER}dead_seen uniqueid=3\n"),
        format!("{HEADER}dead_seen uniqueid=3 at=broken\n"),
    ] {
        std::fs::write(&path, &text).unwrap();
        let error = append_dead_seen(&path, &expected, 900, 2).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
}

#[test]
fn dead_seen_contention_does_not_invent_a_persisted_horizon() {
    let (dir, path, retained) = record();
    let expected = read_records(dir.path()).unwrap().remove(0).1;
    let held = RecordLock::read(&retained).unwrap();
    let error = append_dead_seen(&path, &expected, 300, 1).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    drop(held);
    assert_eq!(std::fs::read_to_string(path).unwrap(), HEADER);
}

#[test]
fn an_already_observed_first_horizon_cannot_be_removed_or_replaced() {
    let (dir, path, _retained) = record();
    append(&path, "dead_seen uniqueid=300 at=1").unwrap();
    let expected = read_records(dir.path()).unwrap().remove(0).1;
    for text in [
        HEADER.to_owned(),
        format!("{HEADER}dead_seen uniqueid=900 at=2\n"),
        format!("{HEADER}dead_seen uniqueid=300 at=2\n"),
    ] {
        std::fs::write(&path, &text).unwrap();
        let error = append_dead_seen(&path, &expected, 1000, 3).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
}

#[test]
fn dead_seen_never_returns_success_for_an_oversized_record() {
    let line = "dead_seen uniqueid=300 at=1\n";
    for room in [line.len(), line.len() - 1] {
        let (dir, path, _retained) = record();
        let pad = RECORD_BYTES as usize - room - HEADER.len() - "root path=\n".len();
        let text = format!("{HEADER}root path={}\n", "x".repeat(pad));
        std::fs::write(&path, &text).unwrap();
        let expected = read_records(dir.path()).unwrap().remove(0).1;
        let result = append_dead_seen(&path, &expected, 300, 1);
        if room == line.len() {
            assert_eq!(
                result.unwrap(),
                DeadSeen {
                    horizon: 300,
                    at_ms: 1
                }
            );
            assert_eq!(std::fs::metadata(&path).unwrap().len(), RECORD_BYTES);
            assert_eq!(read_records(dir.path()).unwrap()[0].1.dead_seen, Some(300));
        } else {
            assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
            assert_eq!(read_records(dir.path()).unwrap()[0].1.dead_seen, None);
        }
    }
}
