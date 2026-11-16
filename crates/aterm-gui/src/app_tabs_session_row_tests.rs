// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rows a window's closes leave in the machine's session table (tracker ST-15).

use std::time::Duration;

use super::*;
use crate::session_store::ExitReason;
use crate::session_table::SessionTable;

/// A scratch directory of the test's own for its table's rows.
fn scratch(prefix: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("scratch dir")
}

/// The headless app with its table's rows in `dir`.
fn app_at(dir: &std::path::Path) -> App {
    let app = App::headless_for_test();
    SessionTable::at(dir.to_path_buf()).serve(&app.store);
    assert!(
        app.store
            .read()
            .unwrap()
            .table()
            .loaded_within(Duration::from_secs(60))
    );
    app
}

/// A new tab in window 0 whose session is a done worker ([`crate::task_launch::tests::done_worker`]):
/// its session, its handle and its result snapshot.
fn worker_tab(
    app: &mut App,
    scratch: &std::path::Path,
) -> (
    u64,
    crate::session_store::SessionHandle,
    aterm_agent::harness::result::Snapshot,
) {
    let id = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(id));
    let handle = app
        .store
        .read()
        .unwrap()
        .by_local(id)
        .cloned()
        .expect("the worker's session");
    let (_, snap, _) = crate::task_launch::tests::done_worker(&handle, scratch);
    (id, handle, snap)
}

/// A PERSON'S CLOSE KEEPS THE RESULT WITH ITS ROW (tracker ST-15): a person closes a done
/// worker's tab; its result and the sidecar beside it stay, and its row names that result and
/// says the person closed it; when the row goes, so do they. CONTROL: the result was there before
/// the close.
#[test]
fn a_persons_close_keeps_the_result_with_its_row() {
    let rows = scratch("aterm-row-person");
    let work = scratch("aterm-row-person-work");
    let mut app = app_at(rows.path());
    let (_, worker, snap) = worker_tab(&mut app, work.path());
    let sidecar = aterm_agent::harness::result::sidecar_of(&snap.path);
    assert!(snap.path.exists() && sidecar.exists(), "CONTROL");

    app.close_active_tab_in(WindowId(0));
    assert!(
        app.store.read().unwrap().by_sid(&worker.sid).is_none(),
        "the session ended"
    );
    let table = app.store.read().unwrap().table();
    assert!(table.flush());
    assert!(
        snap.path.exists() && sidecar.exists(),
        "the close kept the result"
    );
    let row = table.row(&worker.sid).expect("its row");
    assert_eq!((row.out, row.by.as_str()), (ExitReason::UiClose, "human"));
    assert_eq!(
        row.task.as_ref().and_then(|t| t.result_path.as_deref()),
        Some(snap.path.as_path()),
        "the row names the result"
    );

    assert!(table.remove(&worker.sid), "a person removes the row");
    assert!(table.flush());
    assert!(
        !snap.path.exists() && !sidecar.exists(),
        "the result went with its row"
    );
    assert!(!table.holds(&worker.sid));
    assert!(
        table.row(&worker.sid).is_none(),
        "the row is gone from disk"
    );
}

/// A QUIT LEAVES A ROW FOR EACH SESSION IT ENDS FOR GOOD, by whoever asked for the quit, with the
/// exit code of a program that had exited, and none for a session the layout it wrote brings back
/// while it still writes the rest; the rows are on disk when the run's end goes on (read from their
/// files, with no flush of the test's own), and the result a row names outlives the quit. CONTROL:
/// before the quit, no row.
#[test]
fn a_quit_leaves_a_row_for_what_it_does_not_bring_back() {
    let rows = scratch("aterm-row-quit");
    let work = scratch("aterm-row-quit-work");

    // A quit that wrote no layout (restore off): every session ends.
    let mut app = app_at(rows.path());
    let (_, worker, snap) = worker_tab(&mut app, work.path());
    let first = app
        .store
        .read()
        .unwrap()
        .by_local(0)
        .cloned()
        .expect("tab 0");
    app.quit_by = Some(crate::session_store::ExitActor::Human);
    // Tab 0's program had exited with status 3, its tab left open.
    app.store.write().unwrap().note_exit_code(0, Some(3));
    let table = app.store.read().unwrap().table();
    assert!(!table.holds(&worker.sid), "CONTROL: no row before the quit");
    app.close_sessions_at_quit(&[]);
    app.settle_quit_desk_and_tasks(|_| crate::run_end::QuitDesk::default(), |_, _| {});
    for (sid, exit_code) in [(&worker.sid, None), (&first.sid, Some(3))] {
        let text = std::fs::read_to_string(rows.path().join(format!("{}.toml", sid.as_str())))
            .expect("the quit's row of each session it ended is on disk");
        let row =
            crate::session_table::Row::decode(&text, crate::task_record::tasks_dir().as_deref())
                .expect("it reads");
        assert_eq!((row.out, row.by.as_str()), (ExitReason::AppQuit, "human"));
        assert_eq!(row.exit_code, exit_code, "{sid}'s exit code");
    }
    assert_eq!(
        table
            .row(&worker.sid)
            .and_then(|row| row.task)
            .and_then(|task| task.result_path),
        Some(snap.path.clone()),
        "the worker's row names its result"
    );
    assert!(
        snap.path.exists(),
        "the result its row names outlives the quit"
    );
    drop(app);

    // A quit whose layout brings the worker back: its session goes on, under its layout, and tab 0,
    // which the layout does not name, still leaves its row.
    let rows = scratch("aterm-row-quit-kept");
    let mut app = app_at(rows.path());
    let (_, worker, _) = worker_tab(&mut app, work.path());
    let first = app
        .store
        .read()
        .unwrap()
        .by_local(0)
        .cloned()
        .expect("tab 0");
    app.quit_by = Some(crate::session_store::ExitActor::Human);
    let mut layout = crate::restore::RestoreManifest::one_terminal_for_test(None);
    for window in &mut layout.windows {
        for tab in &mut window.restored_tabs {
            tab.root.visit_terminals_mut(&mut |leaf| {
                leaf.sid = Some(worker.sid.as_str().to_owned());
            });
        }
    }
    let table = app.store.read().unwrap().table();
    app.close_sessions_at_quit(&[]);
    app.settle_quit_desk_and_tasks(
        |_| crate::run_end::QuitDesk {
            wrote: true,
            published: Some(layout),
        },
        |_, _| {},
    );
    assert!(
        !table.holds(&worker.sid),
        "no row for a session the layout brings back"
    );
    let row = table
        .row(&first.sid)
        .expect("a row for the session the layout does not name");
    assert_eq!((row.out, row.by.as_str()), (ExitReason::AppQuit, "human"));
}
