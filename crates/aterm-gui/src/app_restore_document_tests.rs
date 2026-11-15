// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HANG: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

fn request(path: &std::path::Path, kind: AppKind, config: bool) -> Request {
    Request {
        window: WindowId(0),
        kind,
        uri: crate::native_document_host::path_to_file_uri(path).unwrap(),
        config_path: config.then(|| path.to_path_buf()),
    }
}

#[test]
fn a_pending_restore_read_installs_nothing_until_its_owner_takes_the_result() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().canonicalize().unwrap().join("note.md");
    std::fs::write(&path, "# saved document\n").unwrap();
    let request = request(&path, AppKind::Markdown, false);
    let uri = request.uri.clone();
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (wake, wake_rx) = mpsc::channel();
    let mut ticket = RestoreDocumentAdmission::spawn(
        move |canceled| {
            entered.send(()).unwrap();
            release_rx.recv_timeout(HANG).unwrap();
            request.read(canceled)
        },
        move || wake.send(()).unwrap(),
    )
    .unwrap();
    let mut app = App::headless_for_test();
    let before = app.windows[&WindowId(0)].tab_set.len();
    entered_rx.recv_timeout(HANG).unwrap();
    assert!(ticket.poll().is_none(), "a blocked file read is pending");
    assert_eq!(app.windows[&WindowId(0)].tab_set.len(), before);
    assert!(app.document_store.id_for_uri(&uri).is_none());
    release.send(()).unwrap();
    wake_rx.recv_timeout(HANG).unwrap();
    let admitted = ticket.poll().expect("result precedes wake").unwrap();
    assert!(ticket.poll().is_none(), "a result is taken only once");
    assert_eq!(app.windows[&WindowId(0)].tab_set.len(), before);
    assert!(app.document_store.id_for_uri(&uri).is_none());
    app.restore_leaf_staging = Some(crate::app_restore::RestoreLeafStaging {
        wid: WindowId(0),
        tab: None,
    });
    app.install_restore_admitted_document(WindowId(0), admitted)
        .unwrap();
    assert_eq!(app.windows[&WindowId(0)].tab_set.len(), before);
    assert!(app.restore_leaf_staging.as_ref().unwrap().tab.is_some());
    assert!(app.document_store.id_for_uri(&uri).is_some());
}

#[test]
fn dropping_a_restore_ticket_discards_even_a_late_successful_read_without_a_wake() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().canonicalize().unwrap().join("late.txt");
    std::fs::write(&path, "late result").unwrap();
    let request = request(&path, AppKind::Editor, false);
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (wake, wake_rx) = mpsc::channel();
    let ticket = RestoreDocumentAdmission::spawn(
        move |_| {
            entered.send(()).unwrap();
            release_rx.recv_timeout(HANG).unwrap();
            // An in-progress filesystem call need not be interruptible. Return a real
            // admitted file after cancellation to exercise the worker's completion guard.
            request.read(&AtomicBool::new(false))
        },
        move || wake.send(()).unwrap(),
    )
    .unwrap();
    entered_rx.recv_timeout(HANG).unwrap();
    let canceled = Arc::clone(&ticket.canceled);
    drop(ticket);
    assert!(canceled.load(Ordering::Acquire));
    release.send(()).unwrap();
    assert_eq!(
        wake_rx.recv_timeout(HANG),
        Err(mpsc::RecvTimeoutError::Disconnected),
        "the completed worker drops its wake without publishing"
    );
}

#[test]
fn a_config_restore_ensures_the_file_in_its_worker_and_enables_the_installed_document() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().canonicalize().unwrap().join("aterm.toml");
    let request = request(&path, AppKind::Editor, true);
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (wake, wake_rx) = mpsc::channel();
    let mut ticket = RestoreDocumentAdmission::spawn(
        move |canceled| {
            entered.send(()).unwrap();
            release_rx.recv_timeout(HANG).unwrap();
            request.read(canceled)
        },
        move || wake.send(()).unwrap(),
    )
    .unwrap();
    let mut app = App::headless_for_test();
    entered_rx.recv_timeout(HANG).unwrap();
    assert!(
        !path.exists(),
        "the blocked worker has not ensured the file"
    );
    assert!(ticket.poll().is_none());
    release.send(()).unwrap();
    wake_rx.recv_timeout(HANG).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"");
    let before = app.windows[&WindowId(0)].tab_set.len();
    app.restore_leaf_staging = Some(crate::app_restore::RestoreLeafStaging {
        wid: WindowId(0),
        tab: None,
    });
    app.install_restore_admitted_document(
        WindowId(0),
        ticket.poll().expect("result precedes wake").unwrap(),
    )
    .unwrap();
    assert_eq!(app.windows[&WindowId(0)].tab_set.len(), before);
    let view = app
        .restore_leaf_staging
        .as_ref()
        .unwrap()
        .tab
        .as_ref()
        .unwrap()
        .focus;
    let Some(crate::tab_model::View::Native(native)) = app.view_store.get(view) else {
        panic!("the staged document has a native view");
    };
    let instance = native.instance;
    assert!(app.native_runtime.config_editor_enabled(instance));
    assert_eq!(
        app.native_config_service.bound_logical_path(),
        Some(path.as_path())
    );
}

#[test]
fn missing_event_loop_refuses_restore_admission_without_a_synchronous_fallback() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().canonicalize().unwrap().join("absent.md");
    let request = request(&path, AppKind::Editor, false);
    let descriptor = crate::restore::NativeLeafRestore::document("editor", request.uri);
    let app = App::headless_for_test();
    let error = app
        .begin_restore_document_admission(WindowId(17), &descriptor)
        .unwrap_err();
    assert!(error.contains("event-loop wake"), "{error}");
    assert!(!path.exists());
}

#[test]
fn detached_document_install_and_rollback_do_not_require_a_published_window() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().canonicalize().unwrap().join("staged.txt");
    std::fs::write(&path, "staged document").unwrap();
    let wid = WindowId(17);
    let mut request = request(&path, AppKind::Editor, false);
    request.window = wid;
    let admitted = request.read(&AtomicBool::new(false)).unwrap();
    let mut app = App::headless_for_test();
    let windows = app.windows.len();
    app.restore_leaf_staging = Some(crate::app_restore::RestoreLeafStaging { wid, tab: None });
    app.install_restore_admitted_document(wid, admitted)
        .unwrap();
    assert_eq!(app.windows.len(), windows);
    assert!(!app.windows.contains_key(&wid));
    let tab = app
        .restore_leaf_staging
        .as_ref()
        .unwrap()
        .tab
        .as_ref()
        .unwrap();
    let (id, view) = (tab.id, tab.focus);
    app.rollback_document_tab(
        crate::app_tabs::NativeInstallTarget::DetachedRestore(wid),
        id,
    );
    assert!(app.restore_leaf_staging.as_ref().unwrap().tab.is_none());
    assert!(app.view_store.get(view).is_none());
    assert_eq!(app.windows.len(), windows);
}

#[test]
fn a_discarded_restore_preserves_a_document_opened_and_edited_after_its_worker_read() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().canonicalize().unwrap().join("shared.txt");
    std::fs::write(&path, "original disk text").unwrap();
    let wid = WindowId(17);
    let mut request = request(&path, AppKind::Editor, false);
    request.window = wid;
    let uri = request.uri.clone();
    let admitted = request.read(&AtomicBool::new(false)).unwrap();

    // The worker's completed read predates both the ordinary open and the person's edit.
    let mut app = App::headless_for_test();
    app.open_document_tab_in_window(WindowId(0), AppKind::Editor, &uri)
        .unwrap();
    let (live_instance, live_view) = app.active_native_view(WindowId(0)).unwrap();
    let document = app.native_runtime.document_id(live_instance).unwrap();
    app.dispatch_native_event(
        WindowId(0),
        crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
            "user edit ".to_string(),
        )),
    )
    .unwrap();
    let edited = app.document_store.snapshot(document).unwrap();
    assert!(app.document_store.dirty(document).unwrap());
    assert_eq!(app.document_store.view_count(document), Some(1));
    // A main-thread reread would now fail. The admitted leaf already owns its observation.
    std::fs::remove_file(&path).unwrap();
    app.restore_leaf_staging = Some(crate::app_restore::RestoreLeafStaging { wid, tab: None });
    app.install_restore_admitted_document(wid, admitted)
        .unwrap();
    let staged = app
        .restore_leaf_staging
        .as_ref()
        .unwrap()
        .tab
        .as_ref()
        .unwrap();
    let (staged_tab, staged_view) = (staged.id, staged.focus);
    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!((after.seq, after.text), (edited.seq, edited.text.clone()));
    assert_eq!(app.document_store.view_count(document), Some(2));
    assert!(app.view_store.get(live_view).is_some());

    app.rollback_document_tab(
        crate::app_tabs::NativeInstallTarget::DetachedRestore(wid),
        staged_tab,
    );
    assert!(app.view_store.get(staged_view).is_none());
    assert!(app.view_store.get(live_view).is_some());
    assert_eq!(app.document_store.view_count(document), Some(1));
    let kept = app.document_store.snapshot(document).unwrap();
    assert_eq!((kept.seq, kept.text), (edited.seq, edited.text));
    assert!(app.document_store.dirty(document).unwrap());
}

#[test]
fn a_worker_without_a_result_is_a_completed_error_not_an_endless_pending_read() {
    let (sender, receiver) = mpsc::channel();
    let mut ticket = RestoreDocumentAdmission {
        receiver,
        canceled: Arc::new(AtomicBool::new(false)),
        finished: false,
        preparation: None,
        admitted: None,
    };
    drop(sender);
    assert!(ticket.poll().unwrap().is_err());
    assert!(ticket.poll().is_none());
}

#[test]
fn a_panicking_admission_publishes_its_error_before_waking_the_cursor() {
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (wake, wake_rx) = mpsc::channel();
    let mut ticket = RestoreDocumentAdmission::spawn(
        move |_| {
            entered.send(()).unwrap();
            release_rx.recv_timeout(HANG).unwrap();
            panic!("injected admission panic")
        },
        move || wake.send(()).unwrap(),
    )
    .unwrap();
    entered_rx.recv_timeout(HANG).unwrap();
    assert!(ticket.poll().is_none());
    release.send(()).unwrap();
    wake_rx.recv_timeout(HANG).unwrap();
    let error = ticket
        .poll()
        .expect("failure precedes the wake")
        .unwrap_err();
    assert!(error.contains("worker panicked"), "{error}");
    assert!(ticket.poll().is_none());
}

#[cfg(unix)]
#[test]
fn a_restore_ticket_uses_real_file_and_journal_workers_before_installation() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("prepared.txt");
    std::fs::write(&path, "worker-owned observation").unwrap();
    let request = request(&path, AppKind::Editor, false);
    let uri = request.uri.clone();
    let (read_done, read_done_rx) = mpsc::channel();
    let mut ticket = RestoreDocumentAdmission::spawn(
        move |canceled| request.read(canceled),
        move || read_done.send(()).unwrap(),
    )
    .unwrap();
    let (opening, opening_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let mut app = App::headless_for_test();
    app.native_documents.journals = super::super::JournalSlot::opening(move || {
        opening.send(()).unwrap();
        release_rx.recv_timeout(HANG).unwrap();
        crate::native_document_journal::DocumentJournalStore::for_test(root.join("journals"))
    });
    opening_rx.recv_timeout(HANG).unwrap();
    read_done_rx.recv_timeout(HANG).unwrap();
    let before = app.view_store.len();
    assert!(app.poll_restore_document_admission(&mut ticket).is_none());
    assert!(app.document_store.id_for_uri(&uri).is_none());
    assert_eq!(app.view_store.len(), before);
    release.send(()).unwrap();
    let deadline = std::time::Instant::now() + HANG;
    let admitted = loop {
        if let Some(result) = app.poll_restore_document_admission(&mut ticket) {
            break result.unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "prepared restore did not complete"
        );
        aterm_uds::deadline::poll_yield();
    };
    assert!(
        admitted.prepared.is_some(),
        "this exercises production adoption, not the old fixture fallback"
    );
    app.restore_leaf_staging = Some(crate::app_restore::RestoreLeafStaging {
        wid: WindowId(0),
        tab: None,
    });
    app.install_restore_admitted_document(WindowId(0), admitted)
        .unwrap();
    assert_eq!(app.view_store.len(), before + 1);
    let tab = app
        .restore_leaf_staging
        .as_ref()
        .unwrap()
        .tab
        .as_ref()
        .unwrap()
        .id;
    app.rollback_document_tab(
        crate::app_tabs::NativeInstallTarget::DetachedRestore(WindowId(0)),
        tab,
    );
    app.progress_document_preparations();
    assert_eq!(app.view_store.len(), before);
    assert!(app.document_store.id_for_uri(&uri).is_none());
}
