// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `task`, `result` and `await task` on an ended session, answered from its row in the machine's
//! session table (tracker ST-15).

use std::sync::Arc;
use std::time::Duration;

use super::{ActiveHandle, Scope, Selector, closed_session_reply_with, dispatch_await_task};
use crate::session_store::{ExitActor, ExitReason};
use crate::session_table::SessionTable;

/// TASK AND RESULT ANSWER FROM AN ENDED SESSION'S ROW (tracker ST-15): a done worker is closed by
/// its manager and the process ends; in a new one over the same state, `task` names its final
/// record with no agent, program or attention, `result` reads the result it left, and `await
/// task` naming it answers at once, `closed`, with news. An edge with no grant reads nothing of it
/// (a regression guard: a session nothing holds read the same before ST-15). CONTROL: the worker's
/// own `result` answered the same before its close.
#[test]
fn task_and_result_answer_from_an_ended_sessions_row() {
    let rows = aterm_tempfile::Builder::new()
        .prefix("aterm-ended-row")
        .tempdir()
        .expect("scratch dir");
    let work = aterm_tempfile::Builder::new()
        .prefix("aterm-ended-row-work")
        .tempdir()
        .expect("scratch dir");
    let (sid, snap, answer) = {
        let store = crate::session_store::new_store();
        SessionTable::at(rows.path().to_path_buf()).serve(&store);
        let worker = crate::session_store::test_handle(5);
        store.write().unwrap().register(worker.clone());
        let (_, snap, answer) = crate::task_launch::tests::done_worker(&worker, work.path());
        assert_eq!(
            crate::task_record::cmd_result(&worker.ctx, 5, ""),
            answer,
            "CONTROL: the live worker's result"
        );
        crate::session_table::close_leaving_row(
            &store,
            5,
            ExitReason::CtlClose,
            ExitActor::Sid("s-0123456789abcdef0123".to_owned()),
        )
        .expect("a close");
        assert!(store.read().unwrap().table().flush());
        (worker.sid.as_str().to_owned(), snap, answer)
    };

    let store = crate::session_store::new_store();
    SessionTable::at(rows.path().to_path_buf()).serve(&store);
    assert!(
        store
            .read()
            .unwrap()
            .table()
            .loaded_within(Duration::from_secs(60))
    );
    let selector = Selector::parse(&sid);
    let task = closed_session_reply_with(None, Some(&selector), "task", "", &store, Scope::Owner);
    assert!(
        task.starts_with("OK task rev=")
            && task.contains(" state=done ")
            && task.contains(&format!(" result_bytes={} ", snap.bytes))
            && task.ends_with(" agent=- program_state=- attention=-\n"),
        "{task}"
    );
    assert_eq!(
        closed_session_reply_with(None, Some(&selector), "result", "", &store, Scope::Owner),
        answer
    );
    let active: ActiveHandle = Arc::default();
    let wait = format!("@{sid} since=0 timeout=60000");
    let words: Vec<&str> = wait.split_whitespace().collect();
    let waited = dispatch_await_task(&words, &active, &store, Scope::Owner);
    assert!(
        waited.starts_with("OK 1\n")
            && waited.contains(&format!("@{sid} rev="))
            && waited.contains(" state=closed ")
            && waited.contains(" task_news=1"),
        "{waited}"
    );
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    assert_eq!(
        closed_session_reply_with(None, Some(&selector), "result", "", &store, edge),
        "ERR no such session\n"
    );
}

/// Which reader of an ended row [`row_disk_reads_release_the_store_guard`] drives.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum RowReader {
    /// `control::ended_record`, behind `@<sid> result` and `task`.
    Ended,
    /// `control::hosts_or_keeps_in`, the window node serve's fleet lookup.
    Hosts,
    /// `control::keeps_ended_row`, the fleet drive gate's `ended`.
    DriveGate,
}

/// A row read blocked on the state volume must not hold up the window's store writer.
#[cfg(unix)]
fn row_disk_reads_release_the_store_guard(reader_kind: RowReader) {
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    {
        let dir = aterm_tempfile::tempdir().unwrap();
        let store = crate::session_store::new_store();
        let table = SessionTable::at(dir.path().to_path_buf());
        table.serve(&store);
        assert!(table.loaded_within(Duration::from_secs(60)));
        let sid = aterm_session::SessionId::generate();
        let path = dir.path().join(format!("{}.toml", sid.as_str()));
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated pathname in this test's owned directory.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let read_store = store.clone();
        let read_sid = sid.clone();
        let (done, seen) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            match reader_kind {
                RowReader::Hosts => {
                    super::hosts_or_keeps_in(&read_store, &read_sid);
                }
                RowReader::DriveGate => {
                    super::keeps_ended_row(&read_store, &read_sid);
                }
                RowReader::Ended => {
                    super::ended_record(
                        &read_store,
                        &Selector::parse(read_sid.as_str()),
                        Scope::Owner,
                    );
                }
            }
            done.send(()).unwrap();
        });
        // Opening the writer proves the reader is in file I/O. Keep it there until after the
        // lock assertion, but release it even if that assertion will fail.
        let (connected, ready) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let mut writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            connected.send(()).unwrap();
            released
                .recv_timeout(Duration::from_secs(60))
                .expect("the lock was inspected");
            writer.write_all(b"not a row").unwrap();
        });
        ready
            .recv_timeout(Duration::from_secs(60))
            .expect("the row reader opened the FIFO");
        let free = store.try_write().is_ok();
        release.send(()).unwrap();
        seen.recv_timeout(Duration::from_secs(60))
            .expect("the read ends after the writer closes its pipe");
        writer.join().unwrap();
        reader.join().unwrap();
        assert!(free, "disk I/O holds no store read guard ({reader_kind:?})");
    }
}

#[cfg(unix)]
#[test]
fn ended_row_disk_reads_release_the_store_guard() {
    row_disk_reads_release_the_store_guard(RowReader::Ended);
}

#[cfg(unix)]
#[test]
fn ended_row_fleet_lookup_releases_the_store_guard() {
    row_disk_reads_release_the_store_guard(RowReader::Hosts);
}

#[cfg(unix)]
#[test]
fn ended_row_drive_gate_lookup_releases_the_store_guard() {
    row_disk_reads_release_the_store_guard(RowReader::DriveGate);
}
