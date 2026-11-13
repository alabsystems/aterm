// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_written_line_reads_back_with_what_was_counted() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join(HANDLER_FILE);
    let recorder = Recorder::at(&path).expect("a new file");
    recorder.note(Row::Post, Instant::now());
    recorder.note(Row::Post, Instant::now());
    recorder.write().expect("written");
    let first =
        HandlerTally::parse(&std::fs::read_to_string(&path).expect("read")).expect("parses");
    assert_eq!(first.pid, std::process::id());
    assert_eq!(first.row(Row::Post).count, 2);
    assert_eq!(first.row(Row::Deliver).count, 0);
    recorder.note(Row::Deliver, Instant::now());
    recorder.write().expect("written again");
    let second = HandlerTally::parse(&std::fs::read_to_string(&path).expect("read"))
        .expect("a later line replaces the first whole");
    let d = second.since(&first).expect("one process");
    assert_eq!(d.row(Row::Deliver).count, 1);
    assert_eq!(d.row(Row::Post).count, 0);
}

#[test]
fn a_second_recorder_refuses_an_occupied_name() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join(HANDLER_FILE);
    std::fs::write(&path, "another process's\n").expect("occupied");
    assert!(Recorder::at(&path).is_err());
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "another process's\n"
    );
}

/// A recorder held for the process's life, as the real one is, and its file.
fn on_file() -> (
    aterm_tempfile::TempDir,
    std::path::PathBuf,
    &'static Recorder,
) {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join(HANDLER_FILE);
    let recorder = Recorder::at(&path).expect("a new file");
    (dir, path, Box::leak(Box::new(recorder)))
}

/// Every row that counted anything, with its count.
fn counted(recorder: &Recorder) -> Vec<(Row, u64)> {
    let rows = *recorder.rows.lock().expect("not poisoned");
    Row::ALL
        .into_iter()
        .map(|row| (row, rows[row.index()].count))
        .filter(|(_, count)| *count > 0)
        .collect()
}

#[test]
fn a_wait_with_no_time_writes_every_earlier_span_and_counts_in_no_row() {
    let (_dir, path, recorder) = on_file();
    recorder.note(Row::Post, Instant::now());
    let mut spans = AwaitSpans::on(Some(recorder), 0);
    let line = HandlerTally::parse(&std::fs::read_to_string(&path).expect("the fence"))
        .expect("a whole line");
    assert_eq!(line.row(Row::Post).count, 1, "written before the answer");
    spans.parks();
    spans.answers(&mut Some(Instant::now()));
    drop(spans);
    assert_eq!(counted(recorder), [(Row::Post, 1)]);
}

#[test]
fn a_parked_wait_counts_its_arm_once_and_its_answer_as_the_wake() {
    let (_dir, _path, recorder) = on_file();
    let mut spans = AwaitSpans::on(Some(recorder), 60_000);
    assert_eq!(counted(recorder), []);
    spans.parks();
    spans.parks();
    assert_eq!(
        counted(recorder),
        [(Row::AwaitArm, 1)],
        "a park again is one"
    );
    let mut arrived = Some(Instant::now());
    spans.answers(&mut arrived);
    assert_eq!(arrived, None, "the arrival is spent");
    drop(spans);
    assert_eq!(
        counted(recorder),
        [(Row::AwaitArm, 1), (Row::AwaitWake, 1)],
        "and the drop counts no second arm"
    );
}

#[test]
fn a_wait_that_never_parked_spends_the_arrival_and_counts_only_its_arm() {
    let (_dir, _path, recorder) = on_file();
    let mut arrived = Some(Instant::now());
    let spans = AwaitSpans::on(Some(recorder), 60_000);
    spans.answers(&mut arrived);
    assert_eq!(
        arrived, None,
        "or a later wait would read this row's moment"
    );
    assert_eq!(counted(recorder), [], "its arm ends when it does");
    drop(spans);
    assert_eq!(counted(recorder), [(Row::AwaitArm, 1)]);

    let mut later = AwaitSpans::on(Some(recorder), 60_000);
    later.parks();
    later.answers(&mut arrived);
    assert_eq!(
        counted(recorder),
        [(Row::AwaitArm, 2)],
        "an answer with no arrival of its own has no wake to count"
    );
}

#[test]
fn with_the_seam_unset_nothing_is_counted() {
    assert!(stamp().is_none());
    let _timer = timer(|| panic!("the row is named only when the tally is on"));
    let mut spans = AwaitSpans::begin(1);
    spans.parks();
    spans.answers(&mut stamp());
    write();
}

/// NEGATIVE CONTROL: `Recorder::at` refuses a symlinked parent directory without creating
/// the tally file in the link target.
#[cfg(unix)]
#[test]
fn recorder_refuses_symlinked_parent_directory() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let real_dir = root.path().join("real");
    std::fs::create_dir(&real_dir).expect("real dir");
    let linked_dir = root.path().join("linked");
    std::os::unix::fs::symlink(&real_dir, &linked_dir).expect("symlink dir");

    let err =
        Recorder::at(&linked_dir.join(HANDLER_FILE)).expect_err("symlinked parent must be refused");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!real_dir.join(HANDLER_FILE).exists());
}
