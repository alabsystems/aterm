// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Instant;

use super::*;
use crate::WindowId;
use crate::app_edit::Placement;
use crate::native_app::{AppEvent, AppKind, TextInputEvent};

fn drive_until<T>(app: &mut App, mut observe: impl FnMut(&App) -> Option<T>) -> T {
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        app.progress_document_preparations();
        if let Some(found) = observe(app) {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "document preparation did not complete"
        );
        aterm_uds::deadline::poll_yield();
    }
}

fn ready(app: &mut App, lease: &PreparationLease) -> PreparedDocument {
    drive_until(app, |_| lease.poll()).unwrap()
}

fn edit(app: &mut App, text: &str) {
    app.dispatch_native_event(
        WindowId(0),
        AppEvent::TextInput(TextInputEvent::Commit(text.into())),
    )
    .unwrap();
}

fn recovered_fixture() -> (
    aterm_tempfile::TempDir,
    std::path::PathBuf,
    String,
    std::path::PathBuf,
) {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("recover.txt");
    let journals = root.join("journals");
    std::fs::write(&path, "disk\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let journal;
    {
        let mut crashed = App::headless_for_test();
        crashed.use_document_journal_root_for_test(journals.clone());
        crashed.open_document_tab(AppKind::Editor, &uri).unwrap();
        edit(&mut crashed, "recovered ");
        let (instance, _) = crashed.active_native_view(WindowId(0)).unwrap();
        let document = crashed.native_runtime.document_id(instance).unwrap();
        assert_eq!(
            crashed
                .document_store
                .snapshot(document)
                .unwrap()
                .text
                .as_ref(),
            "recovered disk\n"
        );
        journal = crashed.document_journal_path_for_test(document).unwrap();
        crashed.exit_for_test();
    }
    (scratch, journals, uri, journal)
}

#[test]
fn a_gated_journal_opener_never_blocks_the_preparation_caller() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("file.txt");
    std::fs::write(&path, "text").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let admitted = App::admit_document_now(&uri, false).unwrap();
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let mut app = App::headless_for_test();
    app.native_documents.journals = super::super::JournalSlot::opening(move || {
        entered.send(()).unwrap();
        release_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        crate::native_document_journal::DocumentJournalStore::for_test(root.join("journals"))
    });
    entered_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    let views = app.view_store.len();
    let lease = app.prepare_document_open(admitted, GrantAccess::ReadWrite, &uri);
    app.progress_document_preparations();
    assert!(lease.poll().is_none());
    assert!(app.document_store.id_for_uri(&uri).is_none());
    assert!(app.native_documents.journals.try_settle().is_none());
    assert_eq!(app.view_store.len(), views);
    release.send(()).unwrap();
    let prepared = ready(&mut app, &lease);
    assert_eq!(app.document_store.view_count(prepared.document), Some(0));
    assert_eq!(app.view_store.len(), views);
    drop(prepared);
    drop(lease);
    app.progress_document_preparations();
    assert!(app.document_store.id_for_uri(&uri).is_none());
}

#[test]
fn cancelling_an_adopted_preparation_only_enqueues_custody_and_preserves_a_later_open() {
    use crate::native_document_journal::custody_test;

    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("cancel.txt");
    std::fs::write(&path, "clean text").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(root.join("journals"));
    let lease = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    let prepared = ready(&mut app, &lease);
    let document = prepared.document;
    let disk = app.document_store.snapshot(document).unwrap();
    let journal = app.document_journal_path_for_test(document).unwrap();
    let host = app
        .native_documents
        .journals
        .as_ref()
        .unwrap()
        .admission_host();
    let decision = host.inspect_open(&uri, disk.text.as_bytes()).unwrap();
    let later = host.initialize_on_worker(decision, &disk, &disk).unwrap();
    assert_eq!(later.path, journal);
    let gate = custody_test::Gate::hold(&host);
    custody_test::without_fs(|| {
        drop(prepared);
        drop(lease);
        app.progress_document_preparations();
        assert!(app.document_store.id_for_uri(&uri).is_none());
        assert!(!app.own_document_work_pending());
    });
    drop(gate);
    app.native_documents
        .journals
        .as_ref()
        .unwrap()
        .await_worker_for_test();
    assert!(
        journal.exists(),
        "the later registration survives the old cancellation"
    );
    drop(later);
    app.native_documents
        .journals
        .as_ref()
        .unwrap()
        .await_worker_for_test();
    assert!(
        !journal.exists(),
        "the final clean registration can be reclaimed"
    );
}

#[test]
fn a_private_preparation_waits_for_worker_purge_and_old_receipt_drops_cannot_recreate_it() {
    use crate::native_document_journal::custody_test;

    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let private = root.join("document.private.noindex");
    std::fs::create_dir(&private).unwrap();
    let path = private.join("edit.txt");
    std::fs::write(&path, "private disk text").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    assert!(crate::native_document_journal::private_uri(&uri));
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(root.join("journals"));
    let host = app
        .native_documents
        .journals
        .as_ref()
        .unwrap()
        .admission_host();
    // An older writer's private draft, still held by a delayed result when the
    // current open makes its privacy decision. No current App path initializes it.
    let mut older = crate::test_document_store();
    let old_document = older.open(uri.clone(), "private disk text".into());
    let disk = older.snapshot(old_document).unwrap();
    assert!(matches!(
        older.transact(
            old_document,
            disk.seq,
            vec![crate::document_store::TextEdit {
                range: 0..0,
                insert: "older private draft ".into(),
            }]
        ),
        crate::document_store::DocumentTxnOutcome::Committed { .. }
    ));
    let old_head = older.snapshot(old_document).unwrap();
    let decision = host.inspect_open(&uri, disk.text.as_bytes()).unwrap();
    let receipt = host
        .initialize_on_worker(decision, &disk, &old_head)
        .unwrap();
    let journal = receipt.path.clone();
    assert!(journal.exists());
    let admitted = App::admit_document_now(&uri, false).unwrap();
    let gate = custody_test::Gate::hold(&host);
    let lease = custody_test::without_fs(|| {
        let lease = app.prepare_document_open(admitted, GrantAccess::ReadWrite, &uri);
        app.progress_document_preparations();
        assert!(lease.poll().is_none());
        assert!(app.document_store.id_for_uri(&uri).is_none());
        lease
    });
    drop(gate);
    let prepared = ready(&mut app, &lease);
    assert_eq!(
        app.document_store.snapshot(prepared.document).unwrap().text,
        disk.text
    );
    assert!(
        app.native_documents
            .journals
            .as_ref()
            .unwrap()
            .declined(prepared.document)
    );
    assert!(!journal.exists(), "privacy cleanup precedes readiness");
    drop(receipt);
    app.native_documents
        .journals
        .as_ref()
        .unwrap()
        .await_worker_for_test();
    assert!(
        !journal.exists(),
        "a delayed old result cannot recreate the private journal"
    );
    drop(prepared);
    drop(lease);
    app.progress_document_preparations();
    assert!(app.document_store.id_for_uri(&uri).is_none());
    let reopened = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    let prepared = ready(&mut app, &reopened);
    assert_eq!(
        app.document_store.snapshot(prepared.document).unwrap().text,
        disk.text
    );
    assert!(
        app.native_documents
            .journals
            .as_ref()
            .unwrap()
            .declined(prepared.document)
    );
    assert!(!journal.exists());
}

#[test]
fn a_second_open_waits_for_recovery_initialization_and_keeps_the_current_shared_edits() {
    let (_scratch, journals, uri, _) = recovered_fixture();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals);
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    INITIALIZE_GATE.set(Some(Box::new(move || {
        entered.send(()).unwrap();
        release_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
    })));
    let first = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    drive_until(&mut app, |_| entered_rx.try_recv().ok());
    let document = app.document_store.id_for_uri(&uri).unwrap();
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "recovered disk\n"
    );
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert_eq!(app.document_store.view_count(document), Some(0));
    assert!(
        app.native_runtime
            .instance_for_document(AppKind::Editor, document)
            .is_none()
    );

    let admitted = App::admit_document_now(&uri, false).unwrap();
    let second = app.prepare_document_open(admitted.clone(), GrantAccess::ReadWrite, &uri);
    assert!(second.poll().is_none());
    assert!(
        app.install_admitted_document(
            WindowId(0),
            AppKind::Editor,
            &uri,
            admitted,
            Placement::NewTab
        )
        .is_err(),
        "CONTROL: an unprepared open must not attach the provisional buffer"
    );
    assert_eq!(app.document_store.document_ids(), vec![document]);
    assert_eq!(app.document_store.view_count(document), Some(0));

    release.send(()).unwrap();
    let prepared = ready(&mut app, &first);
    let installed = app
        .install_prepared_document(
            WindowId(0),
            AppKind::Editor,
            &uri,
            prepared,
            Placement::NewTab,
        )
        .unwrap();
    assert_eq!(installed.document, document);
    app.hold_journal_appends_for_test();
    edit(&mut app, "newer ");
    assert_eq!(app.held_journal_appends_for_test().len(), 1);
    let newest = app.document_store.snapshot(document).unwrap();
    let prepared = ready(&mut app, &second);
    assert_eq!(
        app.held_journal_appends_for_test().len(),
        1,
        "the published owner's autosave is still held"
    );
    let installed = app
        .install_prepared_document(
            WindowId(0),
            AppKind::Editor,
            &uri,
            prepared,
            Placement::NewTab,
        )
        .unwrap();
    assert_eq!(installed.document, document);
    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!((after.seq, after.text), (newest.seq, newest.text));
    assert_eq!(app.document_store.view_count(document), Some(2));
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert!(app.run_held_journal_append_for_test());
    assert!(
        !app.own_document_work_pending(),
        "the published lease retains no recovery work"
    );
    drop(first);
}

#[test]
fn abandoning_a_recovered_unpublished_view_retires_its_edges_but_retains_the_journal() {
    let (_scratch, journals, uri, journal) = recovered_fixture();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals);
    let views = app.view_store.len();
    let lease = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    let prepared = ready(&mut app, &lease);
    let document = prepared.document;
    app.restore_leaf_staging = Some(crate::app_restore::RestoreLeafStaging {
        wid: WindowId(17),
        tab: None,
    });
    let installed = app
        .install_prepared_document(
            crate::app_tabs::NativeInstallTarget::DetachedRestore(WindowId(17)),
            AppKind::Editor,
            &uri,
            prepared,
            Placement::NewTab,
        )
        .unwrap();
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert_eq!(app.document_store.view_count(document), Some(1));
    let image = std::fs::read(&journal).unwrap();
    app.rollback_document_tab(
        crate::app_tabs::NativeInstallTarget::DetachedRestore(WindowId(17)),
        installed.tab,
    );
    assert!(app.view_store.get(installed.view).is_none());
    assert_eq!(app.view_store.len(), views);
    assert!(
        app.native_runtime
            .instance_for_document(AppKind::Editor, document)
            .is_none()
    );
    assert_eq!(app.document_store.view_count(document), Some(0));
    drop(lease);
    app.progress_document_preparations();
    assert!(app.document_store.id_for_uri(&uri).is_none());
    assert_eq!(
        std::fs::read(journal).unwrap(),
        image,
        "cancellation never purges the durable recovered draft"
    );
}

#[test]
fn two_unpublished_leaves_share_recovery_without_waiting_for_the_first_to_publish() {
    for publish_second in [false, true] {
        let (_scratch, journals, uri, journal) = recovered_fixture();
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(journals);
        let first = app.prepare_document_open(
            App::admit_document_now(&uri, false).unwrap(),
            GrantAccess::ReadWrite,
            &uri,
        );
        let prepared = ready(&mut app, &first);
        let document = prepared.document;
        app.restore_leaf_staging = Some(crate::app_restore::RestoreLeafStaging {
            wid: WindowId(17),
            tab: None,
        });
        let one = app
            .install_prepared_document(
                crate::app_tabs::NativeInstallTarget::DetachedRestore(WindowId(17)),
                AppKind::Editor,
                &uri,
                prepared,
                Placement::NewTab,
            )
            .unwrap();
        let first_tab = app
            .restore_leaf_staging
            .as_mut()
            .unwrap()
            .tab
            .take()
            .unwrap();
        let second = app.prepare_document_open(
            App::admit_document_now(&uri, false).unwrap(),
            GrantAccess::ReadWrite,
            &uri,
        );
        let prepared = ready(&mut app, &second);
        assert_eq!(prepared.document, document);
        let two = app
            .install_prepared_document(
                crate::app_tabs::NativeInstallTarget::DetachedRestore(WindowId(17)),
                AppKind::Editor,
                &uri,
                prepared,
                Placement::NewTab,
            )
            .unwrap();
        let second_tab = app
            .restore_leaf_staging
            .as_mut()
            .unwrap()
            .tab
            .take()
            .unwrap();
        assert_eq!(app.document_store.view_count(document), Some(2));
        assert_eq!(app.document_store.document_ids(), vec![document]);
        let recovered = app.document_store.snapshot(document).unwrap();
        let image = std::fs::read(&journal).unwrap();

        assert!(app.rollback_restore_document_view(one.view).unwrap());
        app.remove_tab_views(&first_tab);
        drop(first);
        app.progress_document_preparations();
        assert_eq!(app.document_store.view_count(document), Some(1));
        assert!(app.view_store.get(two.view).is_some());
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text,
            recovered.text
        );
        if publish_second {
            app.publish_restore_document_views(&second_tab).unwrap();
            app.windows
                .get_mut(&WindowId(0))
                .unwrap()
                .tab_set
                .push(second_tab)
                .unwrap();
            assert!(
                !app.rollback_restore_document_view(two.view).unwrap(),
                "publication consumed rollback authority"
            );
            drop(second);
            app.progress_document_preparations();
            assert_eq!(app.document_store.view_count(document), Some(1));
            assert!(app.view_store.get(two.view).is_some());
            assert_eq!(
                app.document_store.snapshot(document).unwrap().text,
                recovered.text
            );
        } else {
            assert!(app.rollback_restore_document_view(two.view).unwrap());
            app.remove_tab_views(&second_tab);
            drop(second);
            app.progress_document_preparations();
            assert!(app.document_store.id_for_uri(&uri).is_none());
            assert!(app.view_store.get(two.view).is_none());
        }
        assert_eq!(std::fs::read(journal).unwrap(), image);
    }
}

#[test]
fn a_recreated_uri_does_not_inherit_an_old_documents_ready_or_publication_state() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("again.txt");
    std::fs::write(&path, "same clean file").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(root.join("journals"));
    let first = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    let prepared = ready(&mut app, &first);
    let old = prepared.document;
    let installed = app
        .install_prepared_document(
            WindowId(0),
            AppKind::Editor,
            &uri,
            prepared,
            Placement::NewTab,
        )
        .unwrap();
    app.close_view_keeping_front(WindowId(0), installed.tab, installed.view)
        .unwrap();
    app.release_unheld_documents(u64::MAX);
    assert!(app.document_store.id_for_uri(&uri).is_none());

    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    INITIALIZE_GATE.set(Some(Box::new(move || {
        entered.send(()).unwrap();
        release_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
    })));
    let second = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    drive_until(&mut app, |_| entered_rx.try_recv().ok());
    let new = app.document_store.id_for_uri(&uri).unwrap();
    assert_ne!(new, old);
    assert!(!app.document_preparation_allows_attach(new));
    let group = &app.native_documents.preparations[&uri];
    assert!(!group.provisional_ready);
    assert!(!group.published.load(Ordering::Acquire));
    assert!(group.recovery_notice.is_none());
    assert!(second.poll().is_none());
    drop(second);
    release.send(()).unwrap();
    drive_until(&mut app, |app| {
        app.document_store.id_for_uri(&uri).is_none().then_some(())
    });
    assert!(
        first.poll().is_some(),
        "an old-generation lease may remain without owning the new document"
    );
    drop(first);
    app.progress_document_preparations();
    assert!(!app.document_preparation_pending(&uri));
}

#[test]
fn an_edit_during_initialization_survives_abandonment_and_reopen() {
    let (_scratch, journals, uri, _) = recovered_fixture();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals.clone());
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    INITIALIZE_GATE.set(Some(Box::new(move || {
        entered.send(()).unwrap();
        release_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
    })));
    let first = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    drive_until(&mut app, |_| entered_rx.try_recv().ok());
    let document = app.document_store.id_for_uri(&uri).unwrap();
    let initialized = app.document_store.snapshot(document).unwrap();
    app.editor_workspace
        .replace_text(
            &mut app.document_store,
            document,
            initialized.seq,
            "newer provisional edit\n",
            crate::native_editor::Origin::Recovered,
        )
        .unwrap();
    let newest = app.document_store.snapshot(document).unwrap();
    assert!(newest.seq > initialized.seq);
    let second = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    drop(first);
    release.send(()).unwrap();
    let prepared = ready(&mut app, &second);
    assert_eq!(
        prepared.document, document,
        "the successor retains the edited buffer"
    );
    let retained = app.document_store.snapshot(document).unwrap();
    assert_eq!((retained.seq, &retained.text), (newest.seq, &newest.text));
    assert!(
        app.native_documents
            .journals
            .as_ref()
            .unwrap()
            .carry_candidate(&retained)
            .is_ok(),
        "preparation cannot finish before its current head is journal-durable"
    );
    drop(prepared);
    drop(second);
    drive_until(&mut app, |app| {
        (!app.document_preparation_pending(&uri)).then_some(())
    });
    assert!(app.document_store.id_for_uri(&uri).is_none());
    app.exit_for_test();
    let mut reopened = App::headless_for_test();
    reopened.use_document_journal_root_for_test(journals);
    reopened.open_document_tab(AppKind::Editor, &uri).unwrap();
    let document = reopened.document_store.id_for_uri(&uri).unwrap();
    assert_eq!(
        reopened.document_store.snapshot(document).unwrap().text,
        newest.text,
        "the newer edit must survive a real journal reopen"
    );
}

#[test]
fn a_failed_initializer_without_requests_keeps_recovery_and_holds_exit_boundaries() {
    let (_scratch, journals, uri, _) = recovered_fixture();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals.clone());
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    INITIALIZE_GATE.set(Some(Box::new(move || {
        entered.send(()).unwrap();
        release_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        panic!("injected initialization worker failure before publication");
    })));
    let lease = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    drive_until(&mut app, |_| entered_rx.try_recv().ok());
    let document = app.document_store.id_for_uri(&uri).unwrap();
    let old = app.document_store.snapshot(document).unwrap();
    app.editor_workspace
        .replace_text(
            &mut app.document_store,
            document,
            old.seq,
            "retained after worker failure\n",
            crate::native_editor::Origin::Recovered,
        )
        .unwrap();
    drop(lease);
    release.send(()).unwrap();
    drive_until(&mut app, |app| {
        app.native_documents.recovery_status.get(&document).cloned()
    });
    assert!(app.document_preparation_pending(&uri));
    assert!(app.document_preparation_owns(document));
    assert!(app.own_document_work_pending());
    assert!(app.open_native_documents().is_empty());
    assert!(app.native_documents.grants.grant_for_uri(&uri).is_some());
    assert!(
        app.native_documents
            .journals
            .as_ref()
            .unwrap()
            .carry_candidate(&app.document_store.snapshot(document).unwrap())
            .is_err()
    );
    assert!(app.document_preparation_wake().is_some());
    app.backdate_own_document_work_for_test(
        crate::app_native::OWN_WORK_BACKSTOP + Duration::from_secs(1),
    );
    assert!(
        app.revalidate_native_update_safety().is_err(),
        "the update timeout cannot lose an unattached draft"
    );
    assert!(!app.prepare_quit_document_shutdown().unwrap());
    assert!(!app.take_ready_document_shutdowns().unwrap().0);
    let retained = app.document_store.snapshot(document).unwrap();
    assert_eq!(retained.text.as_ref(), "retained after worker failure\n");
    drive_until(&mut app, |app| {
        (!app.document_preparation_pending(&uri)).then_some(())
    });
    assert!(app.document_store.snapshot(document).is_none());
    assert!(app.take_ready_document_shutdowns().unwrap().0);
    app.exit_for_test();
    let mut reopened = App::headless_for_test();
    reopened.use_document_journal_root_for_test(journals);
    reopened.open_document_tab(AppKind::Editor, &uri).unwrap();
    let document = reopened.document_store.id_for_uri(&uri).unwrap();
    assert_eq!(
        reopened.document_store.snapshot(document).unwrap().text,
        retained.text
    );
}

#[test]
fn a_canceled_preparation_keeps_every_edge_until_the_newest_append_lands() {
    let (_scratch, journals, uri, _) = recovered_fixture();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals);
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    INITIALIZE_GATE.set(Some(Box::new(move || {
        entered.send(()).unwrap();
        release_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
    })));
    let lease = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    drive_until(&mut app, |_| entered_rx.try_recv().ok());
    let document = app.document_store.id_for_uri(&uri).unwrap();
    let old = app.document_store.snapshot(document).unwrap();
    app.hold_journal_appends_for_test();
    app.editor_workspace
        .replace_text(
            &mut app.document_store,
            document,
            old.seq,
            "held newest draft\n",
            crate::native_editor::Origin::Recovered,
        )
        .unwrap();
    drop(lease);
    release.send(()).unwrap();
    drive_until(&mut app, |app| {
        (!app.held_journal_appends_for_test().is_empty()).then_some(())
    });
    assert_eq!(app.held_journal_appends_for_test().len(), 1);
    assert!(app.document_preparation_pending(&uri));
    assert!(app.document_store.snapshot(document).is_some());
    assert!(app.native_documents.grants.grant_for_uri(&uri).is_some());
    assert!(app.native_documents.unvouched.contains_key(&document));
    assert!(
        !app.native_documents
            .journals
            .as_mut()
            .unwrap()
            .forget(&[document])
    );
    assert!(
        app.native_documents
            .journals
            .as_ref()
            .unwrap()
            .journal_path(document)
            .is_some()
    );
    // A second edit invalidates the inflight generation as a retirement goal.
    let head = app.document_store.snapshot(document).unwrap();
    app.editor_workspace
        .replace_text(
            &mut app.document_store,
            document,
            head.seq,
            "even newer held draft\n",
            crate::native_editor::Origin::Recovered,
        )
        .unwrap();
    assert!(app.run_held_journal_append_for_test());
    assert!(app.document_store.snapshot(document).is_some());
    assert!(app.document_preparation_pending(&uri));
    assert_eq!(app.held_journal_appends_for_test().len(), 1);
    assert!(app.run_held_journal_append_for_test());
    assert!(
        !app.document_preparation_pending(&uri),
        "completion wakes the requestless owner"
    );
    assert!(app.document_store.snapshot(document).is_none());
    assert!(app.native_documents.grants.grant_for_uri(&uri).is_none());
    assert!(!app.native_documents.unvouched.contains_key(&document));
    assert!(!app.run_held_journal_append_for_test());
}

#[test]
fn an_intermediate_published_initialization_is_a_known_predecessor_after_delivery_failure() {
    let (_scratch, journals, uri, _) = recovered_fixture();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals.clone());
    INITIALIZE_GATE.set(Some(Box::new(|| {
        panic!("first attempt fails before publication")
    })));
    let lease = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    drive_until(&mut app, |app| app.document_store.id_for_uri(&uri));
    let document = app.document_store.id_for_uri(&uri).unwrap();
    drive_until(&mut app, |app| {
        app.native_documents.recovery_status.get(&document).cloned()
    });
    let head = app.document_store.snapshot(document).unwrap();
    app.editor_workspace
        .replace_text(
            &mut app.document_store,
            document,
            head.seq,
            "intermediate own draft\n",
            crate::native_editor::Origin::Recovered,
        )
        .unwrap();
    let (published, published_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    INITIALIZE_DELIVERY_GATE.set(Some(Box::new(move || {
        published.send(()).unwrap();
        release_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        Err("injected failure after publication before adoption".into())
    })));
    drop(lease);
    drive_until(&mut app, |_| published_rx.try_recv().ok());
    let head = app.document_store.snapshot(document).unwrap();
    app.editor_workspace
        .replace_text(
            &mut app.document_store,
            document,
            head.seq,
            "newest after undelivered publication\n",
            crate::native_editor::Origin::Recovered,
        )
        .unwrap();
    release.send(()).unwrap();
    drive_until(&mut app, |app| {
        (!app.document_preparation_pending(&uri)).then_some(())
    });
    assert!(app.document_store.snapshot(document).is_none());
    app.exit_for_test();
    let mut reopened = App::headless_for_test();
    reopened.use_document_journal_root_for_test(journals);
    reopened.open_document_tab(AppKind::Editor, &uri).unwrap();
    let document = reopened.document_store.id_for_uri(&uri).unwrap();
    assert_eq!(
        reopened
            .document_store
            .snapshot(document)
            .unwrap()
            .text
            .as_ref(),
        "newest after undelivered publication\n"
    );
}

#[test]
fn unchanged_preparation_drives_owed_legacy_cleanup_and_recovers_from_its_failure() {
    use crate::native_document_io::{JournalDocumentKey, JournalRecord, encode_journal_v1};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("legacy.txt");
    std::fs::write(&path, "unchanged disk").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let journals = root.join("journals");
    std::fs::create_dir(&journals).unwrap();
    let mut store = crate::test_document_store();
    let id = store.open(uri.clone(), "unchanged disk".into());
    let key = JournalDocumentKey::legacy_unkeyed(&uri);
    let bytes = encode_journal_v1(&[JournalRecord::snapshot_for(
        key,
        &store.snapshot(id).unwrap(),
    )])
    .unwrap();
    let legacy = journals.join(format!("{:016x}.atdj", key.0));
    std::fs::write(&legacy, &bytes).unwrap();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals);
    app.hold_journal_reseats_for_test();
    let gate_legacy = legacy.clone();
    INITIALIZE_GATE.set(Some(Box::new(move || {
        std::fs::remove_file(&gate_legacy).unwrap();
        std::fs::create_dir(&gate_legacy).unwrap();
    })));
    let lease = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    drive_until(&mut app, |app| {
        (!app.held_journal_reseats_for_test().is_empty()).then_some(())
    });
    let document = app.document_store.id_for_uri(&uri).unwrap();
    let head = app.document_store.snapshot(document).unwrap();
    assert_eq!(head.text.as_ref(), "unchanged disk");
    assert_eq!(app.document_store.dirty(document), Some(false));
    assert!(
        lease.poll().is_none(),
        "a durable head alone does not discharge legacy custody"
    );
    assert!(app.run_held_journal_reseat_for_test());
    assert!(lease.poll().is_none());
    assert!(app.document_preparation_wake().is_some());
    std::fs::remove_dir(&legacy).unwrap();
    std::fs::write(&legacy, bytes).unwrap();
    drive_until(&mut app, |app| {
        (!app.held_journal_reseats_for_test().is_empty()).then_some(())
    });
    assert!(app.run_held_journal_reseat_for_test());
    let prepared = ready(&mut app, &lease);
    assert_eq!(prepared.document, document);
    assert_eq!(app.document_store.snapshot(document).unwrap().seq, head.seq);
    assert!(!legacy.exists());
    drop(prepared);
    drop(lease);
    app.progress_document_preparations();
    assert!(!app.document_preparation_pending(&uri));
}

#[test]
fn a_durable_unattached_preparation_still_holds_update_until_retirement() {
    let (_scratch, journals, uri, _) = recovered_fixture();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals);
    let lease = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    let prepared = ready(&mut app, &lease);
    let head = app.document_store.snapshot(prepared.document).unwrap();
    assert!(
        app.native_documents
            .journals
            .as_ref()
            .unwrap()
            .carry_candidate(&head)
            .is_ok()
    );
    assert!(app.open_native_documents().is_empty());
    assert!(!app.document_journal_writes_in_flight());
    app.backdate_own_document_work_for_test(
        crate::app_native::OWN_WORK_BACKSTOP + Duration::from_secs(1),
    );
    assert!(app.revalidate_native_update_safety().is_err());
    drop(prepared);
    drop(lease);
    let _ = app.document_preparation_wake();
    assert!(
        app.document_store.snapshot(head.id).is_some(),
        "a deadline query does not retire a document"
    );
    app.progress_document_preparations();
    assert!(app.revalidate_native_update_safety().is_ok());
}

#[test]
fn recovery_retry_keeps_an_unrelated_recovered_image_and_its_newer_buffer() {
    let (_scratch, journals, uri, journal) = recovered_fixture();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals.clone());
    INITIALIZE_GATE.set(Some(Box::new(|| panic!("before publication"))));
    let lease = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    drive_until(&mut app, |app| app.document_store.id_for_uri(&uri));
    let document = app.document_store.id_for_uri(&uri).unwrap();
    drive_until(&mut app, |app| {
        app.native_documents.recovery_status.get(&document).cloned()
    });
    let head = app.document_store.snapshot(document).unwrap();
    app.editor_workspace
        .replace_text(
            &mut app.document_store,
            document,
            head.seq,
            "our newer buffer\n",
            crate::native_editor::Origin::Recovered,
        )
        .unwrap();
    let known_image = std::fs::read(&journal).unwrap();
    {
        let mut other = App::headless_for_test();
        other.use_document_journal_root_for_test(journals);
        other.open_document_tab(AppKind::Editor, &uri).unwrap();
        edit(&mut other, "unrelated writer ");
        other.exit_for_test();
    }
    let foreign_image = std::fs::read(&journal).unwrap();
    assert_ne!(foreign_image, known_image);
    drop(lease);
    drive_until(&mut app, |app| {
        app.native_documents
            .recovery_status
            .get(&document)
            .filter(|message| message.contains("unrelated draft"))
            .cloned()
    });
    assert_eq!(std::fs::read(&journal).unwrap(), foreign_image);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "our newer buffer\n"
    );
    assert!(app.native_documents.grants.grant_for_uri(&uri).is_some());
    assert!(app.document_preparation_pending(&uri));
    assert!(app.own_document_work_pending());
    // Restoring the known predecessor permits the same retained owner to recover.
    std::fs::write(&journal, known_image).unwrap();
    drive_until(&mut app, |app| {
        (!app.document_preparation_pending(&uri)).then_some(())
    });
}

#[test]
fn canceled_unchanged_legacy_recovery_keeps_the_image_an_older_process_can_read() {
    use crate::native_document_io::{
        JournalDocumentKey, JournalEdit, JournalRecord, encode_journal_v1,
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("carried-legacy.txt");
    std::fs::write(&path, "disk").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let journals = root.join("journals");
    std::fs::create_dir(&journals).unwrap();
    let mut store = crate::test_document_store();
    let document = store.open(uri.clone(), "disk".into());
    let disk = store.snapshot(document).unwrap();
    let key = JournalDocumentKey::legacy_unkeyed(&uri);
    let records = [
        JournalRecord::snapshot_for(key, &disk),
        JournalRecord::delta_for(
            key,
            disk.seq,
            aterm_buffer::Seq(disk.seq.0 + 1),
            vec![JournalEdit {
                range: 4..4,
                insert: " and its carried draft".into(),
            }],
        )
        .unwrap(),
    ];
    let bytes = encode_journal_v1(&records).unwrap();
    let legacy = journals.join(format!("{:016x}.atdj", key.0));
    std::fs::write(&legacy, &bytes).unwrap();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals.clone());
    let lease = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    let prepared = ready(&mut app, &lease);
    let document = prepared.document;
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "disk and its carried draft"
    );
    let journal = app.document_journal_path_for_test(document).unwrap();
    let keyed = std::fs::read(&journal).unwrap();
    assert_eq!(std::fs::read(&legacy).unwrap(), bytes);
    drop(prepared);
    drop(lease);
    app.progress_document_preparations();
    assert!(!app.document_preparation_pending(&uri));
    assert!(app.document_store.snapshot(document).is_none());
    assert_eq!(std::fs::read(&legacy).unwrap(), bytes);
    assert_eq!(std::fs::read(journal).unwrap(), keyed);
    app.exit_for_test();
    let mut reopened = App::headless_for_test();
    reopened.use_document_journal_root_for_test(journals);
    reopened.open_document_tab(AppKind::Editor, &uri).unwrap();
    let document = reopened.document_store.id_for_uri(&uri).unwrap();
    assert_eq!(
        reopened
            .document_store
            .snapshot(document)
            .unwrap()
            .text
            .as_ref(),
        "disk and its carried draft"
    );
}

std::thread_local! {
    static CLOCK: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

pub(super) fn clock_now() -> Option<Instant> {
    CLOCK.get()
}

struct PreparationClock(Option<Instant>);

impl PreparationClock {
    fn freeze() -> Self {
        Self(CLOCK.replace(Some(Instant::now())))
    }

    fn advance_to(&self, at: Instant) {
        assert!(CLOCK.get().is_some_and(|now| at >= now));
        CLOCK.set(Some(at));
    }
}

impl Drop for PreparationClock {
    fn drop(&mut self) {
        CLOCK.set(self.0);
    }
}

/// A test's stand-in for a real window's event loop: every admission's read runs on its worker
/// and reports back here, as it would through the loop's proxy, so the open is prepared on
/// workers (the route every window with an event loop takes) and never installed inline. The
/// preparation clock is frozen while this lives.
pub(crate) struct WorkerOpens {
    deliveries: mpsc::Receiver<super::super::DocumentAdmissionOutcome>,
    clock: PreparationClock,
}

impl WorkerOpens {
    pub(crate) fn start() -> Self {
        let (delivered, deliveries) = mpsc::channel();
        super::super::DOCUMENT_ADMISSION_DELIVERY.set(Some(Arc::new(move |outcome| {
            // A test that ended leaves a late worker nobody to report to.
            let _ = delivered.send(outcome);
        })));
        Self {
            deliveries,
            clock: PreparationClock::freeze(),
        }
    }

    /// One pass of the loop: complete every admission that reported, then advance preparation.
    fn turn(&self, app: &mut App) {
        while let Ok(outcome) = self.deliveries.try_recv() {
            // What an admission came to is what its window shows, which the test reads.
            let _ = app.complete_document_admission(outcome);
        }
        app.progress_document_preparations();
    }

    /// Run the loop until `done` answers, letting the clock reach each retry as it falls due.
    pub(crate) fn until<T>(&self, app: &mut App, done: impl FnMut(&App) -> Option<T>) -> T {
        self.run(app, true, done)
    }

    /// As [`Self::until`] on a clock that never moves: only work that is already due happens.
    pub(crate) fn until_without_time<T>(
        &self,
        app: &mut App,
        done: impl FnMut(&App) -> Option<T>,
    ) -> T {
        self.run(app, false, done)
    }

    fn run<T>(&self, app: &mut App, time: bool, mut done: impl FnMut(&App) -> Option<T>) -> T {
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            self.turn(app);
            if let Some(found) = done(app) {
                return found;
            }
            assert!(Instant::now() < deadline, "the open did not get there");
            if time
                && let Some(at) = app.document_preparation_wake()
                && clock_now().is_some_and(|now| at > now)
            {
                self.clock.advance_to(at);
            }
            aterm_uds::deadline::poll_yield();
        }
    }

    /// How many attempts on the draft of the file `canonical_uri` names have failed while its
    /// preparation owner holds it.
    pub(crate) fn failed_attempts(app: &App, canonical_uri: &str) -> u32 {
        app.native_documents
            .preparations
            .get(canonical_uri)
            .and_then(|group| group.recovery.as_ref())
            .map_or(0, |owner| owner.failures)
    }
}

impl Drop for WorkerOpens {
    fn drop(&mut self) {
        super::super::DOCUMENT_ADMISSION_DELIVERY.set(None);
    }
}

enum JournalWorkerReply {
    Append {
        document: DocumentId,
        generation: crate::native_document_io::JournalGeneration,
        result: crate::native_document_io::JournalAppendResult,
    },
    Reseat {
        document: DocumentId,
        generation: crate::native_document_journal::JournalRewriteGeneration,
        result: crate::native_document_journal::JournalReseatResult,
    },
}

impl JournalWorkerReply {
    fn finish(self, app: &mut App) {
        match self {
            Self::Append {
                document,
                generation,
                result,
            } => app.finish_native_document_journal(document, generation, result),
            Self::Reseat {
                document,
                generation,
                result,
            } => app.finish_native_document_journal_reseat(document, generation, result),
        }
    }
}

// Capture normal App effects with the existing holds, but execute their real disk
// primitives on a worker and deliver the production completion to the UI owner.
fn dispatch_held_journal(
    app: &mut App,
    wake: &Arc<Shared>,
) -> mpsc::Receiver<Result<JournalWorkerReply, String>> {
    if let Some(held) = app
        .native_documents
        .held_journal_appends
        .as_mut()
        .filter(|held| !held.is_empty())
        .map(|held| held.remove(0))
    {
        return worker("aterm-test-preparation-append", wake, move || {
            let result = crate::native_document_journal::execute_journal_append(
                &held.path,
                held.key,
                &held.plan,
                crate::native_document_journal::JournalLockPatience::Worker,
            );
            Ok(JournalWorkerReply::Append {
                document: held.document,
                generation: held.plan.generation,
                result,
            })
        });
    }
    let held = app
        .native_documents
        .held_journal_reseats
        .as_mut()
        .filter(|held| !held.is_empty())
        .expect("a recovery effect is queued")
        .remove(0);
    worker("aterm-test-preparation-reseat", wake, move || {
        let result = crate::native_document_journal::execute_journal_reseat(
            &held.path,
            &held.plan,
            crate::native_document_journal::JournalLockPatience::Worker,
        );
        Ok(JournalWorkerReply::Reseat {
            document: held.document,
            generation: held.plan.generation,
            result,
        })
    })
}

#[test]
fn a_post_rename_append_failure_reconciles_the_newer_canceled_preparation() {
    post_rename_append_recovery(false);
}

#[test]
fn a_post_rename_append_failure_preserves_a_foreign_draft_during_reconciliation() {
    post_rename_append_recovery(true);
}

fn post_rename_append_recovery(replace_with_foreign: bool) {
    use crate::native_document_io::{JournalRecord, encode_journal_v1, recover_journal_for};
    use crate::native_document_journal::DocumentJournalStore;
    let (_scratch, journals, uri, journal) = recovered_fixture();
    let clock = PreparationClock::freeze();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals.clone());
    let key = app
        .native_documents
        .journals
        .as_ref()
        .unwrap()
        .key_for(&uri);
    let mut legacy_space = crate::test_document_store();
    let initial = legacy_space.open(uri.clone(), "disk\n".into());
    let disk = legacy_space.snapshot(initial).unwrap();
    let current = DocumentSnapshot {
        id: initial,
        seq: aterm_buffer::Seq(disk.seq.0 + 1),
        text: Arc::from("recovered disk\n"),
    };
    std::fs::write(
        &journal,
        encode_journal_v1(&[
            JournalRecord::snapshot_for(key, &disk),
            JournalRecord::snapshot_for(key, &current),
        ])
        .unwrap(),
    )
    .unwrap();
    let lease = app.prepare_document_open(
        App::admit_document_now(&uri, false).unwrap(),
        GrantAccess::ReadWrite,
        &uri,
    );
    let wake = Arc::clone(&lease.0);
    let prepared = ready(&mut app, &lease);
    let document = prepared.document;
    let h0 = app.document_store.snapshot(document).unwrap();
    assert!(crate::native_document_io::journal_is_format_1(
        &std::fs::read(&journal).unwrap()
    ));
    app.hold_journal_appends_for_test();
    app.hold_journal_reseats_for_test();
    app.editor_workspace
        .replace_text(
            &mut app.document_store,
            document,
            h0.seq,
            "H1 published without acknowledgement\n",
            crate::native_editor::Origin::Recovered,
        )
        .unwrap();
    app.progress_document_preparations();
    let (published, published_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let _gate = DocumentJournalStore::after_replacement_for_test(journal.clone(), move || {
        published.send(()).unwrap();
        release_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        Err(std::io::Error::other(
            "one-shot directory acknowledgement failure",
        ))
    });
    let first = dispatch_held_journal(&mut app, &wake);
    drive_until(&mut app, |_| published_rx.try_recv().ok());
    let published_image = std::fs::read(&journal).unwrap();
    assert_eq!(
        recover_journal_for(key, &published_image).unwrap().text,
        "H1 published without acknowledgement\n"
    );
    let h1 = app.document_store.snapshot(document).unwrap();
    app.editor_workspace
        .replace_text(
            &mut app.document_store,
            document,
            h1.seq,
            "H2 newest canceled draft\n",
            crate::native_editor::Origin::Recovered,
        )
        .unwrap();
    let h2 = app.document_store.snapshot(document).unwrap();
    drop(prepared);
    drop(lease);
    app.progress_document_preparations();
    release.send(()).unwrap();
    let failed = drive_until(&mut app, |_| completed(&first)).unwrap();
    assert!(matches!(
        &failed,
        JournalWorkerReply::Append {
            result: crate::native_document_io::JournalAppendResult::Failed { .. },
            ..
        }
    ));
    failed.finish(&mut app);
    assert_eq!(app.document_store.snapshot(document).unwrap().text, h2.text);
    assert_eq!(app.document_store.dirty(document), Some(true));
    assert!(app.native_documents.grants.grant_for_uri(&uri).is_some());
    assert!(app.document_preparation_pending(&uri));
    assert!(app.own_document_work_pending());
    assert!(
        app.native_documents
            .journals
            .as_ref()
            .unwrap()
            .carry_candidate(&h2)
            .is_err(),
        "the failed reply is never durability evidence"
    );
    let preserved_image = if replace_with_foreign {
        let foreign = DocumentSnapshot {
            id: initial,
            seq: aterm_buffer::Seq(h2.seq.0 + 10),
            text: Arc::from("another writer's unrelated draft\n"),
        };
        let image = encode_journal_v1(&[
            JournalRecord::snapshot_for(key, &disk),
            JournalRecord::snapshot_for(key, &foreign),
        ])
        .unwrap();
        std::fs::write(&journal, &image).unwrap();
        image
    } else {
        published_image
    };
    clock.advance_to(
        app.document_preparation_wake()
            .expect("failure retains a retry deadline"),
    );
    app.service_due_document_preparations();
    let healthy = dispatch_held_journal(&mut app, &wake);
    let healthy = drive_until(&mut app, |_| completed(&healthy)).unwrap();
    healthy.finish(&mut app);
    let recovered = recover_journal_for(key, &std::fs::read(&journal).unwrap()).unwrap();
    assert_eq!(
        recovered.text,
        h2.text.as_ref(),
        "a healthy recovery must persist H2 rather than repeatedly reject the obsolete H0 tail"
    );
    assert!(!app.document_preparation_pending(&uri));
    assert!(app.document_store.snapshot(document).is_none());
    assert!(app.native_documents.grants.grant_for_uri(&uri).is_none());
    assert!(
        std::fs::read_dir(journal.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .any(
                |entry| entry.file_name().to_string_lossy().contains(".preserved-")
                    && std::fs::read(entry.path()).is_ok_and(|bytes| bytes == preserved_image)
            ),
        "an unacknowledged or foreign image must stay preserved"
    );
    app.exit_for_test();
    let mut reopened = App::headless_for_test();
    reopened.use_document_journal_root_for_test(journals);
    reopened.open_document_tab(AppKind::Editor, &uri).unwrap();
    let document = reopened.document_store.id_for_uri(&uri).unwrap();
    assert_eq!(
        reopened.document_store.snapshot(document).unwrap().text,
        h2.text
    );
}

#[test]
fn actual_preparation_keeps_content_work_on_workers_for_clean_and_recovered_text() {
    use amacs::store::{PreparationWork, take_preparation_work};
    for repeats in [1, 65_536] {
        for recovered in [false, true] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let root = scratch.path().canonicalize().unwrap();
            let path = root.join("prepared.txt");
            let disk = "é line\n".repeat(repeats);
            std::fs::write(&path, format!("\u{feff}{}", disk.replace('\n', "\r\n"))).unwrap();
            let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
            let journals = root.join("journals");
            if recovered {
                let mut crashed = App::headless_for_test();
                crashed.use_document_journal_root_for_test(journals.clone());
                crashed.open_document_tab(AppKind::Editor, &uri).unwrap();
                edit(&mut crashed, "draft ");
                crashed.exit_for_test();
            }
            let admission_uri = uri.clone();
            let (admitted, admission_work, admission_thread) = std::thread::spawn(move || {
                take_preparation_work();
                let admitted = App::admit_document_for_worker(&admission_uri, false).unwrap();
                (
                    admitted,
                    take_preparation_work(),
                    std::thread::current().id(),
                )
            })
            .join()
            .unwrap();
            assert_ne!(admission_thread, std::thread::current().id());
            assert_eq!(admission_work.projection_input_bytes, disk.len());
            assert_eq!(admission_work.rope_input_bytes, disk.len());
            assert_eq!(admission_work.baseline_input_bytes, disk.len());
            let mut app = App::headless_for_test();
            app.use_document_journal_root_for_test(journals);
            let (send, receive) = mpsc::channel();
            WORK_OBSERVER.set(Some(Arc::new(move |name, work| {
                send.send((name.to_owned(), std::thread::current().id(), work))
                    .unwrap();
            })));
            take_preparation_work();
            let lease = app.prepare_document_open(admitted, GrantAccess::ReadWrite, &uri);
            let prepared = ready(&mut app, &lease);
            for _ in 0..8 {
                drop(lease.poll().unwrap().unwrap().clone());
            }
            WORK_OBSERVER.set(None);
            let ui = take_preparation_work();
            assert_eq!(
                ui,
                PreparationWork {
                    adoptions: if recovered { 2 } else { 1 },
                    retirement_transfers: usize::from(recovered),
                    ..Default::default()
                }
            );
            eprintln!(
                "actual-preparation repeats={repeats} recovered={recovered} admission={admission_work:?} ui={ui:?}"
            );
            let events: Vec<_> = receive.try_iter().collect();
            assert!(
                events
                    .iter()
                    .all(|(_, thread, _)| *thread != std::thread::current().id())
            );
            let inspection = events
                .iter()
                .find(|(name, _, _)| name == "aterm-document-prepare")
                .unwrap()
                .2;
            eprintln!("actual-preparation inspection={inspection:?}");
            if recovered {
                assert!(inspection.diff_input_bytes >= disk.len());
                assert!(inspection.rope_input_bytes > disk.len());
            } else {
                assert_eq!(inspection, PreparationWork::default());
            }
            assert_eq!(
                app.document_store
                    .snapshot(prepared.document)
                    .unwrap()
                    .text
                    .as_ref(),
                if recovered {
                    format!("draft {disk}")
                } else {
                    disk.clone()
                }
            );
            let installed = app
                .install_prepared_document(
                    WindowId(0),
                    AppKind::Editor,
                    &uri,
                    prepared,
                    Placement::NewTab,
                )
                .unwrap();
            assert!(
                app.native_documents
                    .persistence
                    .observed_text_matches(installed.document, &disk)
            );
            if recovered {
                assert!(!app.editor_workspace.can_undo(installed.document));
            }
        }
    }
}

#[test]
fn prepared_canonical_reuse_keeps_live_edits_without_a_ui_reread_and_retires_unused_input() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("canonical.txt");
    std::fs::write(&path, "disk\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(root.join("journals"));
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    edit(&mut app, "live ");
    let document = app.document_store.id_for_uri(&uri).unwrap();
    let live = app.document_store.snapshot(document).unwrap();
    std::fs::write(&path, "new disk 新\n".repeat(65_536)).unwrap();
    let read_uri = uri.clone();
    let admitted =
        std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
            .join()
            .unwrap();
    let receipt = admitted.prepared_text.retirement_receipt();
    let lease = app.prepare_document_open(admitted, GrantAccess::ReadWrite, &uri);
    let prepared = ready(&mut app, &lease);
    amacs::store::take_preparation_work();
    let installed = app
        .install_prepared_document(
            WindowId(0),
            AppKind::Editor,
            &uri,
            prepared,
            Placement::NewTab,
        )
        .unwrap();
    let ui = amacs::store::take_preparation_work();
    assert_eq!(ui.decoded_input_bytes, 0);
    assert_eq!(ui.baseline_input_bytes, 0);
    assert_eq!(installed.document, document);
    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!(after.seq, live.seq);
    assert!(Arc::ptr_eq(&after.text, &live.text));
    drop(lease);
    app.progress_document_preparations();
    let observed = drive_until(&mut app, |_| receipt.poll());
    use std::hash::{Hash, Hasher};
    let mut ui_identity = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut ui_identity);
    assert_ne!(observed.executor, ui_identity.finish());
    assert!(observed.work.retired_rope_nodes > 0);
}

#[test]
fn real_directory_admission_clones_and_cancellation_keep_content_work_off_ui() {
    for entries in [1, 512] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let directory = root.join("directory");
        std::fs::create_dir(&directory).unwrap();
        for entry in 0..entries {
            std::fs::write(directory.join(format!("file-{entry}")), "").unwrap();
        }
        let uri = crate::native_document_host::path_to_file_uri(&directory).unwrap();
        let read_uri = uri.clone();
        let (admitted, worker_work) = std::thread::spawn(move || {
            amacs::store::take_preparation_work();
            let admitted = App::admit_document_for_worker(&read_uri, false).unwrap();
            (admitted, amacs::store::take_preparation_work())
        })
        .join()
        .unwrap();
        assert!(worker_work.projection_input_bytes > 0);
        assert!(worker_work.rope_leaf_input_bytes > 0);
        let receipt = admitted.prepared_text.retirement_receipt();
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(root.join("journals"));
        amacs::store::take_preparation_work();
        let lease = app.prepare_document_open(admitted.clone(), GrantAccess::ReadWrite, &uri);
        drop(admitted);
        let prepared = ready(&mut app, &lease);
        for _ in 0..8 {
            drop(lease.poll().unwrap().unwrap().clone());
        }
        let ui = amacs::store::take_preparation_work();
        assert_eq!(
            ui,
            amacs::store::PreparationWork {
                adoptions: 1,
                ..Default::default()
            }
        );
        let document = prepared.document;
        assert!(app.document_store.read_only(document));
        assert_eq!(
            app.document_store
                .directory(document)
                .unwrap()
                .entries
                .len(),
            entries + 1
        );
        amacs::store::take_preparation_work();
        drop(prepared);
        drop(lease);
        drive_until(&mut app, |app| {
            app.document_store.id_for_uri(&uri).is_none().then_some(())
        });
        let ui = amacs::store::take_preparation_work();
        assert_eq!(ui.retired_rope_nodes, 0);
        assert_eq!(ui.retired_directory_entries, 0);
        let observed = drive_until(&mut app, |_| receipt.poll());
        use std::hash::{Hash, Hasher};
        let mut ui_identity = std::collections::hash_map::DefaultHasher::new();
        std::thread::current().id().hash(&mut ui_identity);
        assert_ne!(observed.executor, ui_identity.finish());
        assert_eq!(observed.work.retired_directory_entries, entries + 1);
    }
}

fn directory_reopen_fixture() -> (
    aterm_tempfile::TempDir,
    App,
    std::path::PathBuf,
    String,
    DocumentId,
    crate::tab_model::ViewId,
) {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("directory");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("b.txt"), "").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let target = crate::native_document_host::path_to_file_uri(&path.join("b.txt")).unwrap();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(root.join("journals"));
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    let document = app.document_store.id_for_uri(&uri).unwrap();
    let (_, view) = app.active_native_view(WindowId(0)).unwrap();
    let point = app
        .document_store
        .directory(document)
        .unwrap()
        .point_for(Some(&target));
    let origin = app.document_store.snapshot(document).unwrap().text.len();
    let Some(crate::native_app::AppViewState::Editor(state)) =
        app.native_runtime.view_state_mut(view)
    else {
        panic!("editor");
    };
    let buffer = state.buffer.as_mut().unwrap();
    buffer.selections = vec![crate::native_editor::Selection::caret(point)];
    buffer.viewport_anchor = point;
    buffer.minibuffer = crate::native_editor::Minibuffer::Search {
        query: "b.txt".into(),
        origin,
    };
    (scratch, app, path, uri, document, view)
}

#[test]
fn normal_async_directory_reopen_adopts_fresh_targets_and_coordinates_with_bounded_ui_work() {
    for entries in [1, 512] {
        let (_scratch, mut app, path, uri, document, view) = directory_reopen_fixture();
        for entry in 0..entries {
            std::fs::write(path.join(format!("a-{entry}.txt")), "").unwrap();
        }
        let read_uri = uri.clone();
        let admitted =
            std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
                .join()
                .unwrap();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        WORK_OBSERVER.set(Some(Arc::new(move |name, work| {
            if name == "aterm-document-prepare" {
                entered.send(work).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            }
        })));
        amacs::store::take_preparation_work();
        let lease = app.prepare_document_open(admitted, GrantAccess::ReadWrite, &uri);
        let worker = drive_until(&mut app, |_| entered_rx.try_recv().ok());
        assert!(worker.diff_input_bytes > 0);
        assert!(worker.directory_entry_visits >= entries);
        WORK_OBSERVER.set(None);
        release.send(()).unwrap();
        let prepared = ready(&mut app, &lease);
        let ui = amacs::store::take_preparation_work();
        eprintln!("directory-reopen entries={entries} worker={worker:?} ui={ui:?}");
        assert_eq!(
            ui,
            amacs::store::PreparationWork {
                adoptions: 1,
                retirement_transfers: 1,
                ..Default::default()
            }
        );
        assert_eq!(prepared.document, document);
        let snapshot = app.document_store.snapshot(document).unwrap();
        assert!(snapshot.text.contains("a-0.txt"));
        assert_eq!(app.document_store.dirty(document), Some(false));
        let target = crate::native_document_host::path_to_file_uri(&path.join("b.txt")).unwrap();
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("editor");
        };
        let buffer = state.buffer.as_ref().unwrap();
        assert_eq!(
            app.document_store
                .directory(document)
                .unwrap()
                .target_at(buffer.primary_selection().head),
            Some(target.as_str())
        );
        let crate::native_editor::Minibuffer::Search { origin, .. } = buffer.minibuffer else {
            panic!("search");
        };
        assert!(origin <= snapshot.text.len() && snapshot.text.is_char_boundary(origin));
        let installed = app
            .install_prepared_document(
                WindowId(0),
                AppKind::Editor,
                &uri,
                prepared,
                Placement::FocusOrNewTab,
            )
            .unwrap();
        assert_eq!(installed.document, document);
        assert_eq!(app.active_native_view(WindowId(0)).unwrap().1, view);
    }
}

#[test]
fn directory_reopen_refuses_stale_base_or_selection_and_cancellation_keeps_the_live_listing() {
    for mode in [0, 1, 2] {
        let (_scratch, mut app, path, uri, document, view) = directory_reopen_fixture();
        std::fs::write(path.join("a.txt"), "").unwrap();
        let read_uri = uri.clone();
        let admitted =
            std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
                .join()
                .unwrap();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        WORK_OBSERVER.set(Some(Arc::new(move |name, _| {
            if name == "aterm-document-prepare" {
                entered.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            }
        })));
        let lease = app.prepare_document_open(admitted, GrantAccess::ReadWrite, &uri);
        drive_until(&mut app, |_| entered_rx.try_recv().ok());
        WORK_OBSERVER.set(None);
        if mode == 0 {
            let Some(crate::native_app::AppViewState::Editor(state)) =
                app.native_runtime.view_state_mut(view)
            else {
                panic!("editor");
            };
            state.buffer.as_mut().unwrap().selections =
                vec![crate::native_editor::Selection::caret(0)];
        } else if mode == 1 {
            std::fs::write(path.join("latest.txt"), "").unwrap();
            let listing = crate::native_document_host::read_directory(
                &path,
                crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
            )
            .unwrap();
            app.document_store.set_directory(document, listing).unwrap();
        }
        let current = app.document_store.snapshot(document).unwrap();
        amacs::store::take_preparation_work();
        release.send(()).unwrap();
        if mode == 2 {
            drop(lease);
            drive_until(&mut app, |app| {
                (!app.document_preparation_pending(&uri)).then_some(())
            });
        } else {
            assert!(drive_until(&mut app, |_| lease.poll()).is_err());
            drop(lease);
            app.progress_document_preparations();
        }
        let ui = amacs::store::take_preparation_work();
        assert_eq!(ui.diff_input_bytes, 0);
        assert_eq!(ui.rope_leaf_input_bytes, 0);
        assert_eq!(ui.retired_rope_nodes, 0);
        assert_eq!(ui.retired_directory_entries, 0);
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!(after.seq, current.seq);
        assert!(Arc::ptr_eq(&after.text, &current.text));
        if mode == 0 {
            let Some(crate::native_app::AppViewState::Editor(state)) =
                app.native_runtime.view_state(view)
            else {
                panic!("editor");
            };
            assert_eq!(state.buffer.as_ref().unwrap().primary_selection().head, 0);
        }
    }
}

#[test]
fn detached_restore_directory_preparation_keeps_the_current_canonical_listing() {
    let (_scratch, mut app, path, uri, document, _) = directory_reopen_fixture();
    let current = app.document_store.snapshot(document).unwrap();
    std::fs::write(path.join("new.txt"), "").unwrap();
    let read_uri = uri.clone();
    let admitted =
        std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
            .join()
            .unwrap();
    amacs::store::take_preparation_work();
    let lease = app.prepare_restore_document_open(admitted, &uri);
    let prepared = ready(&mut app, &lease);
    let ui = amacs::store::take_preparation_work();
    assert_eq!(ui, amacs::store::PreparationWork::default());
    assert_eq!(prepared.document, document);
    let after = app.document_store.snapshot(document).unwrap();
    assert_eq!(after.seq, current.seq);
    assert!(Arc::ptr_eq(&after.text, &current.text));
}

#[test]
fn directory_reopen_rejects_changed_navigation_even_with_identical_text_and_sequence() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("directory");
    std::fs::create_dir(&path).unwrap();
    let old_path = path.join("line\nname");
    let new_path = path.join("line\\nname");
    std::fs::write(&old_path, "").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(root.join("journals"));
    app.open_document_tab(AppKind::Editor, &uri).unwrap();
    let document = app.document_store.id_for_uri(&uri).unwrap();
    let original = app.document_store.snapshot(document).unwrap();
    let read_uri = uri.clone();
    let admitted =
        std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
            .join()
            .unwrap();
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    WORK_OBSERVER.set(Some(Arc::new(move |name, _| {
        if name == "aterm-document-prepare" {
            entered.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
        }
    })));
    let lease = app.prepare_document_open(admitted, GrantAccess::ReadWrite, &uri);
    drive_until(&mut app, |_| entered_rx.try_recv().ok());
    WORK_OBSERVER.set(None);
    std::fs::rename(&old_path, &new_path).unwrap();
    let listing = crate::native_document_host::read_directory(
        &path,
        crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
    )
    .unwrap();
    assert_eq!(
        listing.text.as_str(),
        original.text.as_ref(),
        "escaped labels collide while exact navigation changes"
    );
    app.document_store.set_directory(document, listing).unwrap();
    let current = app.document_store.snapshot(document).unwrap();
    assert_eq!(current.seq, original.seq);
    assert!(Arc::ptr_eq(&current.text, &original.text));
    release.send(()).unwrap();
    assert!(drive_until(&mut app, |_| lease.poll()).is_err());
    let expected = crate::native_document_host::path_to_file_uri(&new_path).unwrap();
    assert!(
        app.document_store
            .directory(document)
            .unwrap()
            .entries
            .iter()
            .any(|(_, uri)| uri == &expected)
    );
}

fn start_real_explicit_directory_refresh(
    app: &mut App,
    document: DocumentId,
    uri: &str,
    admitted: AdmittedDocument,
) -> super::super::DocumentAdmissionKey {
    use super::super::{DocumentAdmissionKey, DocumentAdmissionOutcome, DocumentAdmissionPurpose};
    assert!(admitted.prepared_text.is_worker_prepared());
    let key = DocumentAdmissionKey {
        uri: uri.into(),
        kind: AppKind::Editor,
        edit: None,
        config: false,
    };
    let ticket = app.mint_document_admission(WindowId(0), key.clone());
    app.native_documents
        .admissions
        .get_mut(&(WindowId(0), key.clone()))
        .unwrap()
        .purpose = DocumentAdmissionPurpose::DirectoryRefresh(document);
    app.complete_document_admission(DocumentAdmissionOutcome {
        ticket,
        wid: WindowId(0),
        key: key.clone(),
        result: Ok(Box::new(admitted)),
    })
    .unwrap();
    assert!(
        app.native_documents.admissions[&(WindowId(0), key.clone())]
            .preparation
            .is_some(),
        "the actual async refresh enters preparation rather than the synchronous refresh tail"
    );
    key
}

#[test]
fn explicit_async_directory_refresh_transfers_prepared_state_without_focus_or_ui_content_work() {
    for entries in [1, 512] {
        let (_scratch, mut app, path, uri, document, view) = directory_reopen_fixture();
        for entry in 0..entries {
            std::fs::write(path.join(format!("a-{entry}.txt")), "").unwrap();
        }
        let other = path.parent().unwrap().join("other.txt");
        std::fs::write(&other, "other").unwrap();
        app.open_document_tab(
            AppKind::Editor,
            &crate::native_document_host::path_to_file_uri(&other).unwrap(),
        )
        .unwrap();
        let focus = app.active_native_view(WindowId(0)).unwrap();
        let before = app.document_store.snapshot(document).unwrap();
        let read_uri = uri.clone();
        let admitted =
            std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
                .join()
                .unwrap();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        WORK_OBSERVER.set(Some(Arc::new(move |name, work| {
            if name == "aterm-document-prepare" {
                entered.send(work).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            }
        })));
        amacs::store::take_preparation_work();
        let key = start_real_explicit_directory_refresh(&mut app, document, &uri, admitted);
        let worker = drive_until(&mut app, |_| entered_rx.try_recv().ok());
        assert!(Arc::ptr_eq(
            &app.document_store.snapshot(document).unwrap().text,
            &before.text
        ));
        WORK_OBSERVER.set(None);
        release.send(()).unwrap();
        drive_until(&mut app, |app| {
            (!app
                .native_documents
                .admissions
                .contains_key(&(WindowId(0), key.clone())))
            .then_some(())
        });
        let ui = amacs::store::take_preparation_work();
        eprintln!("explicit-directory-refresh entries={entries} worker={worker:?} ui={ui:?}");
        assert!(worker.diff_input_bytes > 0 && worker.directory_entry_visits >= entries);
        assert_eq!(
            ui,
            amacs::store::PreparationWork {
                adoptions: 1,
                retirement_transfers: 1,
                ..Default::default()
            }
        );
        assert_eq!(app.active_native_view(WindowId(0)).unwrap(), focus);
        assert_eq!(app.document_store.id_for_uri(&uri), Some(document));
        let after = app.document_store.snapshot(document).unwrap();
        assert!(after.seq > before.seq && after.text.contains("a-0.txt"));
        assert_eq!(app.document_store.dirty(document), Some(false));
        let target = crate::native_document_host::path_to_file_uri(&path.join("b.txt")).unwrap();
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("editor");
        };
        let buffer = state.buffer.as_ref().unwrap();
        assert_eq!(
            app.document_store
                .directory(document)
                .unwrap()
                .target_at(buffer.primary_selection().head),
            Some(target.as_str())
        );
        assert!(state.status.is_some());
        let crate::native_editor::Minibuffer::Search { origin, .. } = buffer.minibuffer else {
            panic!("search");
        };
        assert!(origin <= after.text.len() && after.text.is_char_boundary(origin));
    }
}

#[test]
fn explicit_async_directory_refresh_rejects_stale_state_and_cancellation_without_clobbering() {
    for mode in [0, 1, 2, 3] {
        let (_scratch, mut app, path, uri, document, view) = directory_reopen_fixture();
        std::fs::write(path.join("a.txt"), "").unwrap();
        let read_uri = uri.clone();
        let admitted =
            std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
                .join()
                .unwrap();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        WORK_OBSERVER.set(Some(Arc::new(move |name, _| {
            if name == "aterm-document-prepare" {
                entered.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            }
        })));
        let key = start_real_explicit_directory_refresh(&mut app, document, &uri, admitted);
        drive_until(&mut app, |_| entered_rx.try_recv().ok());
        WORK_OBSERVER.set(None);
        if mode == 0 {
            let Some(crate::native_app::AppViewState::Editor(state)) =
                app.native_runtime.view_state_mut(view)
            else {
                panic!("editor");
            };
            state.buffer.as_mut().unwrap().selections =
                vec![crate::native_editor::Selection::caret(0)];
        } else if mode == 1 {
            std::fs::write(path.join("latest.txt"), "").unwrap();
            let listing = crate::native_document_host::read_directory(
                &path,
                crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
            )
            .unwrap();
            app.document_store.set_directory(document, listing).unwrap();
        } else if mode == 2 {
            app.native_documents
                .admissions
                .remove(&(WindowId(0), key.clone()));
        } else {
            app.close_active_native_tab(WindowId(0)).unwrap();
        }
        let current = app.document_store.snapshot(document);
        amacs::store::take_preparation_work();
        release.send(()).unwrap();
        drive_until(&mut app, |app| {
            (!app
                .native_documents
                .admissions
                .contains_key(&(WindowId(0), key.clone()))
                && !app.document_preparation_pending(&uri))
            .then_some(())
        });
        let ui = amacs::store::take_preparation_work();
        assert_eq!(ui.diff_input_bytes, 0);
        assert_eq!(ui.projection_input_bytes, 0);
        assert_eq!(ui.rope_leaf_input_bytes, 0);
        assert_eq!(ui.directory_entry_visits, 0);
        assert_eq!(ui.retired_rope_nodes, 0);
        assert_eq!(ui.retired_directory_entries, 0);
        let after = app.document_store.snapshot(document);
        assert_eq!(
            after.as_ref().map(|value| value.seq),
            current.as_ref().map(|value| value.seq)
        );
        if let (Some(after), Some(current)) = (after, current) {
            assert!(Arc::ptr_eq(&after.text, &current.text));
        }
        if mode == 3 {
            assert_eq!(app.document_store.view_count(document).unwrap_or(0), 0);
        }
        if mode == 0 {
            let Some(crate::native_app::AppViewState::Editor(state)) =
                app.native_runtime.view_state(view)
            else {
                panic!("editor");
            };
            assert_eq!(state.buffer.as_ref().unwrap().primary_selection().head, 0);
        }
    }
}

#[test]
fn explicit_async_directory_refresh_cannot_recreate_a_closed_document() {
    use super::super::{
        DocumentAdmissionKey, DocumentAdmissionOutcome, DocumentAdmissionPurpose,
        DocumentAdmissionRejected,
    };
    let (_scratch, mut app, path, uri, document, _) = directory_reopen_fixture();
    std::fs::write(path.join("a.txt"), "").unwrap();
    let read_uri = uri.clone();
    let admitted =
        std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
            .join()
            .unwrap();
    let key = DocumentAdmissionKey {
        uri: uri.clone(),
        kind: AppKind::Editor,
        edit: None,
        config: false,
    };
    let ticket = app.mint_document_admission(WindowId(0), key.clone());
    app.native_documents
        .admissions
        .get_mut(&(WindowId(0), key.clone()))
        .unwrap()
        .purpose = DocumentAdmissionPurpose::DirectoryRefresh(document);
    app.close_active_native_tab(WindowId(0)).unwrap();
    let focus = app.active_native_view(WindowId(0));
    amacs::store::take_preparation_work();
    let result = app.complete_document_admission(DocumentAdmissionOutcome {
        ticket,
        wid: WindowId(0),
        key,
        result: Ok(Box::new(admitted)),
    });
    assert!(matches!(result, Err(DocumentAdmissionRejected::Dropped(_))));
    assert_eq!(app.active_native_view(WindowId(0)), focus);
    assert_eq!(app.document_store.view_count(document).unwrap_or(0), 0);
    let ui = amacs::store::take_preparation_work();
    assert_eq!(ui.diff_input_bytes, 0);
    assert_eq!(ui.rope_leaf_input_bytes, 0);
    assert_eq!(ui.adoptions, 0);
    assert_eq!(ui.retired_rope_nodes, 0);
}

#[test]
fn directory_refresh_joins_coordinates_by_view_identity_across_actual_tab_reorder() {
    for explicit in [false, true] {
        let (_scratch, mut app, path, uri, document, first) = directory_reopen_fixture();
        let second = app
            .install_admitted_document(
                WindowId(0),
                AppKind::Editor,
                &uri,
                App::admit_document_now(&uri, false).unwrap(),
                Placement::NewTab,
            )
            .unwrap();
        assert_eq!(second.document, document);
        assert_ne!(second.view, first);
        let b_uri = crate::native_document_host::path_to_file_uri(&path.join("b.txt")).unwrap();
        let listing = app.document_store.directory(document).unwrap();
        let (second_point, second_uri) = listing
            .entries
            .iter()
            .find(|(_, target)| target != &b_uri)
            .map(|(range, uri)| (range.start, uri.clone()))
            .expect("a distinct directory entry");
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state_mut(second.view)
        else {
            panic!("editor");
        };
        let buffer = state.buffer.as_mut().unwrap();
        buffer.selections = vec![crate::native_editor::Selection::caret(second_point)];
        buffer.viewport_anchor = second_point;
        let before_order = app.document_native_views(document);
        let before = app.document_store.snapshot(document).unwrap();
        std::fs::write(path.join("a.txt"), "").unwrap();
        let read_uri = uri.clone();
        let admitted =
            std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
                .join()
                .unwrap();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        WORK_OBSERVER.set(Some(Arc::new(move |name, _| {
            if name == "aterm-document-prepare" {
                entered.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            }
        })));
        let (key, lease) = if explicit {
            (
                Some(start_real_explicit_directory_refresh(
                    &mut app, document, &uri, admitted,
                )),
                None,
            )
        } else {
            (
                None,
                Some(app.prepare_document_open(admitted, GrantAccess::ReadWrite, &uri)),
            )
        };
        drive_until(&mut app, |_| entered_rx.try_recv().ok());
        WORK_OBSERVER.set(None);
        assert!(
            app.windows
                .get_mut(&WindowId(0))
                .unwrap()
                .tab_set
                .reorder(second.tab, 0)
        );
        assert_ne!(app.document_native_views(document), before_order);
        amacs::store::take_preparation_work();
        release.send(()).unwrap();
        if let Some(lease) = lease {
            assert_eq!(ready(&mut app, &lease).document, document);
        } else {
            let key = key.unwrap();
            drive_until(&mut app, |app| {
                (!app
                    .native_documents
                    .admissions
                    .contains_key(&(WindowId(0), key.clone())))
                .then_some(())
            });
        }
        let ui = amacs::store::take_preparation_work();
        assert_eq!(ui.diff_input_bytes, 0);
        assert_eq!(ui.directory_entry_visits, 0);
        assert_eq!(ui.rope_leaf_input_bytes, 0);
        assert_eq!(ui.retired_directory_entries, 0);
        assert_eq!(app.document_store.id_for_uri(&uri), Some(document));
        let after = app.document_store.snapshot(document).unwrap();
        assert!(after.seq > before.seq && after.text.contains("a.txt"));
        for (view, target) in [(first, b_uri), (second.view, second_uri)] {
            let Some(crate::native_app::AppViewState::Editor(state)) =
                app.native_runtime.view_state(view)
            else {
                panic!("editor");
            };
            let point = state.buffer.as_ref().unwrap().primary_selection().head;
            assert_eq!(
                app.document_store
                    .directory(document)
                    .unwrap()
                    .target_at(point),
                Some(target.as_str())
            );
        }
    }
}

fn directory_read_command(
    app: &mut App,
    view: crate::tab_model::ViewId,
    command: crate::native_editor::EditorCommand,
) {
    let Some(crate::native_app::AppViewState::Editor(state)) =
        app.native_runtime.view_state_mut(view)
    else {
        panic!("editor");
    };
    app.editor_workspace
        .execute(
            &mut app.document_store,
            state.buffer.as_mut().unwrap(),
            command,
        )
        .unwrap();
}

#[test]
fn directory_refresh_refuses_complete_selection_changes_even_when_head_and_viewport_return() {
    use crate::native_editor::{EditorCommand, Minibuffer, Selection};
    for mode in 0..4 {
        let (_scratch, mut app, path, uri, document, view) = directory_reopen_fixture();
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("editor");
        };
        state.buffer.as_mut().unwrap().minibuffer = Minibuffer::Inactive;
        // Prime vertical navigation so the actual mark/movement counterexample
        // also returns to its captured desired column, not just head/viewport.
        directory_read_command(&mut app, view, EditorCommand::MoveLineUp);
        directory_read_command(&mut app, view, EditorCommand::MoveLineDown);
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("editor");
        };
        let buffer = state.buffer.as_mut().unwrap();
        let head = buffer.primary_selection().head;
        let viewport = buffer.viewport_anchor;
        if mode >= 2 {
            buffer.selections.push(Selection { anchor: 0, head });
        }
        let source = buffer.directory_coordinates();
        let before = app.document_store.snapshot(document).unwrap();
        std::fs::write(path.join("a.txt"), "").unwrap();
        let read_uri = uri.clone();
        let admitted =
            std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
                .join()
                .unwrap();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        WORK_OBSERVER.set(Some(Arc::new(move |name, _| {
            if name == "aterm-document-prepare" {
                entered.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            }
        })));
        let lease = app.prepare_document_open(admitted, GrantAccess::ReadWrite, &uri);
        drive_until(&mut app, |_| entered_rx.try_recv().ok());
        WORK_OBSERVER.set(None);
        if mode == 0 {
            directory_read_command(&mut app, view, EditorCommand::MoveLineUp);
            directory_read_command(&mut app, view, EditorCommand::SetMark);
            directory_read_command(&mut app, view, EditorCommand::MoveLineDown);
        } else {
            let Some(crate::native_app::AppViewState::Editor(state)) =
                app.native_runtime.view_state_mut(view)
            else {
                panic!("editor");
            };
            let buffer = state.buffer.as_mut().unwrap();
            match mode {
                1 => buffer.selections.push(Selection { anchor: 0, head }),
                2 => buffer.primary = 1,
                3 => buffer.selections[1].anchor = head,
                _ => unreachable!(),
            }
        }
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("editor");
        };
        let buffer = state.buffer.as_ref().unwrap();
        assert_eq!(buffer.primary_selection().head, head);
        assert_eq!(buffer.viewport_anchor, viewport);
        assert_ne!(buffer.directory_coordinates(), source);
        let fresh = buffer.directory_coordinates();
        let current = app.document_store.snapshot(document).unwrap();
        assert_eq!(current.seq, before.seq);
        assert!(Arc::ptr_eq(&current.text, &before.text));
        amacs::store::take_preparation_work();
        release.send(()).unwrap();
        assert!(drive_until(&mut app, |_| lease.poll()).is_err());
        let ui = amacs::store::take_preparation_work();
        assert_eq!(ui.adoptions, 0);
        assert_eq!(ui.diff_input_bytes, 0);
        assert_eq!(ui.directory_entry_visits, 0);
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!(after.seq, before.seq);
        assert!(Arc::ptr_eq(&after.text, &before.text));
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("editor");
        };
        assert_eq!(
            state.buffer.as_ref().unwrap().directory_coordinates(),
            fresh
        );
    }
}

fn ordinary_directory_admission(
    app: &mut App,
    wid: WindowId,
    uri: &str,
) -> (
    super::super::DocumentAdmissionKey,
    super::super::DocumentAdmissionTicket,
) {
    let key = super::super::DocumentAdmissionKey {
        uri: uri.into(),
        kind: AppKind::Editor,
        edit: None,
        config: false,
    };
    let ticket = app.mint_document_admission(wid, key.clone());
    (key, ticket)
}

fn start_surviving_directory_admission(app: &mut App, uri: &str) {
    let (key, ticket) = ordinary_directory_admission(app, WindowId(0), uri);
    let read_uri = uri.to_owned();
    let admitted =
        std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
            .join()
            .unwrap();
    app.complete_document_admission(super::super::DocumentAdmissionOutcome {
        ticket,
        wid: WindowId(0),
        key,
        result: Ok(Box::new(admitted)),
    })
    .unwrap();
}

#[test]
fn closing_an_ordinary_requester_cancels_before_refresh_and_preserves_independent_live_requests() {
    use super::super::{DocumentAdmissionOutcome, DocumentAdmissionRejected};
    for during_inspection in [false, true] {
        for independent in [false, true] {
            let (_scratch, mut app, path, uri, document, view) = directory_reopen_fixture();
            let requester =
                app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
            let before = app.document_store.snapshot(document).unwrap();
            let Some(crate::native_app::AppViewState::Editor(state)) =
                app.native_runtime.view_state(view)
            else {
                panic!("editor");
            };
            let coordinates = state.buffer.as_ref().unwrap().directory_coordinates();
            std::fs::write(path.join("a.txt"), "").unwrap();
            let (key, ticket) = ordinary_directory_admission(&mut app, requester, &uri);
            let read_uri = uri.clone();
            let (read, read_rx) = mpsc::channel();
            let (release_read, release_read_rx) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                let admitted = App::admit_document_for_worker(&read_uri, false).unwrap();
                read.send(()).unwrap();
                release_read_rx
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
                admitted
            });
            read_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
            let (entered, entered_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            if during_inspection {
                let release_rx = Mutex::new(release_rx);
                WORK_OBSERVER.set(Some(Arc::new(move |name, _| {
                    if name == "aterm-document-prepare" {
                        entered.send(()).unwrap();
                        release_rx
                            .lock()
                            .unwrap()
                            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                            .unwrap();
                    }
                })));
                release_read.send(()).unwrap();
                app.complete_document_admission(DocumentAdmissionOutcome {
                    ticket,
                    wid: requester,
                    key: key.clone(),
                    result: Ok(Box::new(reader.join().unwrap())),
                })
                .unwrap();
                drive_until(&mut app, |_| entered_rx.try_recv().ok());
                WORK_OBSERVER.set(None);
                if independent {
                    start_surviving_directory_admission(&mut app, &uri);
                }
                app.close_window_logical(requester);
                if independent {
                    assert!(
                        app.native_documents
                            .admissions
                            .keys()
                            .any(|(owner, _)| *owner == WindowId(0))
                    );
                }
            } else {
                app.close_window_logical(requester);
                release_read.send(()).unwrap();
                let outcome = app.complete_document_admission(DocumentAdmissionOutcome {
                    ticket,
                    wid: requester,
                    key: key.clone(),
                    result: Ok(Box::new(reader.join().unwrap())),
                });
                assert!(matches!(
                    outcome,
                    Err(DocumentAdmissionRejected::Dropped(_))
                ));
            }
            assert!(!app.windows.contains_key(&requester));
            assert!(
                !app.native_documents
                    .admissions
                    .contains_key(&(requester, key))
            );
            let closed = app.document_store.snapshot(document).unwrap();
            assert_eq!(closed.seq, before.seq);
            assert!(Arc::ptr_eq(&closed.text, &before.text));
            let Some(crate::native_app::AppViewState::Editor(state)) =
                app.native_runtime.view_state(view)
            else {
                panic!("editor");
            };
            assert_eq!(
                state.buffer.as_ref().unwrap().directory_coordinates(),
                coordinates
            );
            if independent && !during_inspection {
                start_surviving_directory_admission(&mut app, &uri);
            }
            amacs::store::take_preparation_work();
            if during_inspection {
                release.send(()).unwrap();
            }
            drive_until(&mut app, |app| {
                (!app.document_preparation_pending(&uri)).then_some(())
            });
            let ui = amacs::store::take_preparation_work();
            assert_eq!(ui.diff_input_bytes, 0);
            assert_eq!(ui.directory_entry_visits, 0);
            assert_eq!(ui.retired_rope_nodes, 0);
            let after = app.document_store.snapshot(document).unwrap();
            if independent {
                assert!(after.seq > before.seq && after.text.contains("a.txt"));
            } else {
                assert_eq!(after.seq, before.seq);
                assert!(Arc::ptr_eq(&after.text, &before.text));
                let Some(crate::native_app::AppViewState::Editor(state)) =
                    app.native_runtime.view_state(view)
                else {
                    panic!("editor");
                };
                assert_eq!(
                    state.buffer.as_ref().unwrap().directory_coordinates(),
                    coordinates
                );
            }
        }
    }
}

#[test]
fn borrowed_readiness_cannot_extend_a_closed_requesters_directory_refresh_authority() {
    use super::super::DocumentAdmissionOutcome;
    for during_inspection in [false, true] {
        let (_scratch, mut app, path, uri, document, view) = directory_reopen_fixture();
        let requester = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
        let before = app.document_store.snapshot(document).unwrap();
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("editor");
        };
        let coordinates = state.buffer.as_ref().unwrap().directory_coordinates();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        WORK_OBSERVER.set(Some(Arc::new(move |name, _| {
            if name == "aterm-document-prepare" {
                entered.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            }
        })));
        // A real unfinished inspection keeps the target queued. Ready owners
        // intentionally do not block later requests or detached restore leaves.
        let blocker = if during_inspection {
            None
        } else {
            let read_uri = uri.clone();
            let admitted = std::thread::spawn(move || {
                App::admit_document_for_worker(&read_uri, false).unwrap()
            })
            .join()
            .unwrap();
            let lease = app.prepare_restore_document_open(admitted, &uri);
            drive_until(&mut app, |_| entered_rx.try_recv().ok());
            WORK_OBSERVER.set(None);
            Some(lease)
        };
        std::fs::write(path.join("a.txt"), "").unwrap();
        let read_uri = uri.clone();
        let admitted =
            std::thread::spawn(move || App::admit_document_for_worker(&read_uri, false).unwrap())
                .join()
                .unwrap();
        let (key, ticket) = ordinary_directory_admission(&mut app, requester, &uri);
        app.complete_document_admission(DocumentAdmissionOutcome {
            ticket,
            wid: requester,
            key: key.clone(),
            result: Ok(Box::new(admitted)),
        })
        .unwrap();
        if during_inspection {
            drive_until(&mut app, |_| entered_rx.try_recv().ok());
            WORK_OBSERVER.set(None);
        } else {
            assert!(matches!(
                app.native_documents.preparations[&uri]
                    .jobs
                    .back()
                    .unwrap()
                    .stage,
                Stage::Queued
            ));
        }
        // Legitimate shared readiness custody does not acquire refresh authority.
        let borrowed = app.native_documents.admissions[&(requester, key.clone())]
            .preparation
            .as_ref()
            .unwrap()
            .clone();
        start_surviving_directory_admission(&mut app, &uri);
        app.close_window_logical(requester);
        assert!(
            !app.native_documents
                .admissions
                .contains_key(&(requester, key))
        );
        assert!(
            app.native_documents
                .admissions
                .keys()
                .any(|(owner, _)| *owner == WindowId(0))
        );
        drop(blocker);

        // The independent live requester is allowed to progress past Ready B.
        // Hold its own real inspection result so B's readiness can be observed
        // before A legitimately changes the canonical listing.
        let (live_entered, live_entered_rx) = mpsc::channel();
        let (live_release, live_release_rx) = mpsc::channel();
        let live_release_rx = Mutex::new(live_release_rx);
        // In the genuinely queued branch B still has its inspection ahead of
        // A. In the in-flight branch B already captured the earlier gate.
        let skip_queued_requester = AtomicBool::new(!during_inspection);
        WORK_OBSERVER.set(Some(Arc::new(move |name, _| {
            if name == "aterm-document-prepare"
                && !skip_queued_requester.swap(false, Ordering::AcqRel)
            {
                live_entered.send(()).unwrap();
                live_release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            }
        })));
        amacs::store::take_preparation_work();
        release.send(()).unwrap();
        let prepared = ready(&mut app, &borrowed);
        drive_until(&mut app, |_| live_entered_rx.try_recv().ok());
        WORK_OBSERVER.set(None);
        assert_eq!(prepared.document, document);
        let ui = amacs::store::take_preparation_work();
        assert_eq!(ui.adoptions, 0);
        assert_eq!(ui.diff_input_bytes, 0);
        assert_eq!(ui.directory_entry_visits, 0);
        let unchanged = app.document_store.snapshot(document).unwrap();
        assert_eq!(unchanged.seq, before.seq);
        assert!(Arc::ptr_eq(&unchanged.text, &before.text));
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("editor");
        };
        assert_eq!(
            state.buffer.as_ref().unwrap().directory_coordinates(),
            coordinates
        );
        drop(prepared);
        drop(borrowed);
        live_release.send(()).unwrap();
        drive_until(&mut app, |app| {
            (!app.document_preparation_pending(&uri)).then_some(())
        });
        let after = app.document_store.snapshot(document).unwrap();
        assert!(after.seq > before.seq && after.text.contains("a.txt"));
        assert_eq!(app.document_store.id_for_uri(&uri), Some(document));
    }
}

fn directory_recovery_action(
    app: &mut App,
    wid: WindowId,
    uri: &str,
) -> (crate::tab_model::TabId, crate::tab_model::ViewId) {
    use crate::native_app::{
        ActionInvocation, AppViewState, NativeApp, RecoveryApp, RecoveryCapability,
    };
    let (_, tab, view) = app
        .install_new_native_tab(
            wid,
            NativeApp::Recovery(RecoveryApp {
                restore_tag: "editor".into(),
                reason: "Directory storage recovered".into(),
                metadata: String::new(),
                capability: Some(RecoveryCapability::Document {
                    kind: AppKind::Editor,
                    uri: uri.into(),
                    config_editor: false,
                }),
                origin: crate::native_app::RecoveryOrigin::Restore,
            }),
            AppViewState::Recovery(Default::default()),
            crate::tab_model::TabPresentation {
                title: "Recovery".into(),
                icon: Some(crate::tab_model::TabIconKind::Recovery),
                indicators: Default::default(),
                conn: None,
                closable: true,
                tooltip: None,
            },
        )
        .unwrap();
    app.dispatch_native_event(
        wid,
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new("recovery/retry"),
            value: None,
        }),
    )
    .unwrap();
    (tab, view)
}

#[test]
fn actual_recovery_action_guard_precedes_directory_refresh_after_source_tab_close() {
    use super::super::DOCUMENT_ADMISSION_DELIVERY;
    for during_inspection in [false, true] {
        for coalesced in 0..3 {
            let (_scratch, mut app, path, uri, document, editor) = directory_reopen_fixture();
            let requester =
                app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
            let before = app.document_store.snapshot(document).unwrap();
            let Some(crate::native_app::AppViewState::Editor(state)) =
                app.native_runtime.view_state(editor)
            else {
                panic!("editor");
            };
            let coordinates = state.buffer.as_ref().unwrap().directory_coordinates();
            std::fs::write(path.join("a.txt"), "").unwrap();
            let (file_entered, file_entered_rx) = mpsc::channel();
            let (file_release, file_release_rx) = mpsc::channel();
            let file_release_rx = Mutex::new(file_release_rx);
            let (delivered, delivery_rx) = mpsc::channel();
            DOCUMENT_ADMISSION_DELIVERY.set(Some(Arc::new(move |outcome| {
                file_entered.send(()).unwrap();
                file_release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
                delivered.send(outcome).unwrap();
            })));
            let (tab, source) = directory_recovery_action(&mut app, requester, &uri);
            file_entered_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
            let (entered, entered_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            let mut live_source = None;
            if during_inspection {
                let release_rx = Mutex::new(release_rx);
                WORK_OBSERVER.set(Some(Arc::new(move |name, _| {
                    if name == "aterm-document-prepare" {
                        entered.send(()).unwrap();
                        release_rx
                            .lock()
                            .unwrap()
                            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                            .unwrap();
                    }
                })));
                file_release.send(()).unwrap();
                app.complete_document_admission(
                    delivery_rx
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .unwrap(),
                )
                .unwrap();
                drive_until(&mut app, |_| entered_rx.try_recv().ok());
                WORK_OBSERVER.set(None);
            }
            if coalesced == 1 {
                // Add this guard after preparation starts in the in-flight case.
                // The canonical boundary must read the live coalesced owner set.
                live_source = Some(directory_recovery_action(&mut app, requester, &uri).1);
            }
            if coalesced == 2 {
                assert!(matches!(
                    app.open_document_tab_revealing(
                        requester,
                        AppKind::Editor,
                        &uri,
                        None,
                        super::super::DocumentAdmissionPurpose::Open
                    )
                    .unwrap(),
                    super::super::DocumentAdmission::Coalesced(_)
                ));
            }
            app.close_view_keeping_front(requester, tab, source)
                .unwrap();
            assert!(app.windows.contains_key(&requester));
            assert!(app.locate_native_view(source).is_err());
            let unchanged = app.document_store.snapshot(document).unwrap();
            assert_eq!(unchanged.seq, before.seq);
            assert!(Arc::ptr_eq(&unchanged.text, &before.text));
            amacs::store::take_preparation_work();
            if during_inspection {
                release.send(()).unwrap();
            } else {
                file_release.send(()).unwrap();
                app.complete_document_admission(
                    delivery_rx
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .unwrap(),
                )
                .unwrap();
            }
            DOCUMENT_ADMISSION_DELIVERY.set(None);
            drive_until(&mut app, |app| {
                (!app
                    .native_documents
                    .admissions
                    .keys()
                    .any(|(owner, _)| *owner == requester)
                    && !app.document_preparation_pending(&uri))
                .then_some(())
            });
            let ui = amacs::store::take_preparation_work();
            assert_eq!(ui.diff_input_bytes, 0);
            assert_eq!(ui.directory_entry_visits, 0);
            assert_eq!(ui.retired_rope_nodes, 0);
            let after = app.document_store.snapshot(document).unwrap();
            if coalesced != 0 {
                assert!(after.seq > before.seq && after.text.contains("a.txt"));
            }
            if let Some(live_source) = live_source {
                let Some(crate::native_app::AppViewState::Recovery(state)) =
                    app.native_runtime.view_state(live_source)
                else {
                    panic!("live recovery");
                };
                assert!(state.pending.is_none() && state.notice.is_some());
            } else if coalesced == 2 {
                let (instance, _) = app.active_native_view(requester).unwrap();
                assert_eq!(app.native_runtime.document_id(instance), Some(document));
            } else {
                assert_eq!(ui.adoptions, 0);
                assert_eq!(after.seq, before.seq);
                assert!(Arc::ptr_eq(&after.text, &before.text));
                let Some(crate::native_app::AppViewState::Editor(state)) =
                    app.native_runtime.view_state(editor)
                else {
                    panic!("editor");
                };
                assert_eq!(
                    state.buffer.as_ref().unwrap().directory_coordinates(),
                    coordinates
                );
            }
            assert!(app.structural_invariants_ok());
        }
    }
}

#[test]
fn actual_recovery_migration_preserves_refresh_intent_and_replies_at_the_live_origin() {
    use super::super::DOCUMENT_ADMISSION_DELIVERY;
    // Return after false capture; remain away; leave after true capture;
    // acquire a later coalesced live action after false capture.
    for mode in 0..4 {
        let (_scratch, mut app, path, uri, document, editor) = directory_reopen_fixture();
        let original = WindowId(0);
        let other = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
        let before = app.document_store.snapshot(document).unwrap();
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(editor)
        else {
            panic!("editor");
        };
        let coordinates = state.buffer.as_ref().unwrap().directory_coordinates();
        let (prefix_entered, prefix_entered_rx) = mpsc::channel();
        let (prefix_release, prefix_release_rx) = mpsc::channel();
        let prefix = if mode == 2 {
            None
        } else {
            let prefix_release_rx = Mutex::new(prefix_release_rx);
            WORK_OBSERVER.set(Some(Arc::new(move |name, _| {
                if name == "aterm-document-prepare" {
                    prefix_entered.send(()).unwrap();
                    prefix_release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .unwrap();
                }
            })));
            let read_uri = uri.clone();
            let admitted = std::thread::spawn(move || {
                App::admit_document_for_worker(&read_uri, false).unwrap()
            })
            .join()
            .unwrap();
            let lease = app.prepare_restore_document_open(admitted, &uri);
            drive_until(&mut app, |_| prefix_entered_rx.try_recv().ok());
            WORK_OBSERVER.set(None);
            Some(lease)
        };
        std::fs::write(path.join("a.txt"), "").unwrap();
        let (delivery, delivery_rx) = mpsc::channel();
        DOCUMENT_ADMISSION_DELIVERY.set(Some(Arc::new(move |outcome| {
            delivery.send(outcome).unwrap();
        })));
        let (tab, source) = directory_recovery_action(&mut app, original, &uri);
        let instance = app.locate_native_view(source).unwrap().1;
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        WORK_OBSERVER.set(Some(Arc::new(move |name, work| {
            if name == "aterm-document-prepare" {
                entered.send(work).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            }
        })));
        app.complete_document_admission(
            delivery_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap(),
        )
        .unwrap();
        if mode != 2 {
            assert!(matches!(
                app.native_documents.preparations[&uri]
                    .jobs
                    .back()
                    .unwrap()
                    .stage,
                Stage::Queued
            ));
            app.windows
                .get_mut(&original)
                .unwrap()
                .tab_set
                .switch_to(tab);
            app.migrate_active_tab_from_window(original).unwrap();
            assert_eq!(app.locate_native_view(source), Ok((other, instance)));
            drop(prefix);
            prefix_release.send(()).unwrap();
        }
        let worker = drive_until(&mut app, |_| entered_rx.try_recv().ok());
        WORK_OBSERVER.set(None);
        assert!(
            worker.diff_input_bytes > 0,
            "temporary guard failure must not erase preparation intent"
        );
        if mode == 0 {
            app.migrate_active_tab_from_window(other).unwrap();
            assert_eq!(app.locate_native_view(source), Ok((original, instance)));
        } else if mode == 2 {
            app.windows
                .get_mut(&original)
                .unwrap()
                .tab_set
                .switch_to(tab);
            app.migrate_active_tab_from_window(original).unwrap();
            assert_eq!(app.locate_native_view(source), Ok((other, instance)));
        }
        let coalesced = if mode == 3 {
            Some(directory_recovery_action(&mut app, original, &uri).1)
        } else {
            None
        };
        let Some(crate::native_app::AppViewState::Recovery(state)) =
            app.native_runtime.view_state(source)
        else {
            panic!("recovery");
        };
        assert!(
            state.pending.is_some(),
            "migration preserves the actual pending operation"
        );
        let unchanged = app.document_store.snapshot(document).unwrap();
        assert_eq!(unchanged.seq, before.seq);
        assert!(Arc::ptr_eq(&unchanged.text, &before.text));
        DOCUMENT_ADMISSION_DELIVERY.set(None);
        amacs::store::take_preparation_work();
        release.send(()).unwrap();
        drive_until(&mut app, |app| {
            (!app
                .native_documents
                .admissions
                .keys()
                .any(|(owner, _)| *owner == original)
                && !app.document_preparation_pending(&uri))
            .then_some(())
        });
        let ui = amacs::store::take_preparation_work();
        assert_eq!(ui.diff_input_bytes, 0);
        assert_eq!(ui.directory_entry_visits, 0);
        assert_eq!(ui.retired_directory_entries, 0);
        let after = app.document_store.snapshot(document).unwrap();
        if mode == 0 || mode == 3 {
            assert!(after.seq > before.seq && after.text.contains("a.txt"));
        } else {
            assert_eq!(ui.adoptions, 0);
            assert_eq!(after.seq, before.seq);
            assert!(Arc::ptr_eq(&after.text, &before.text));
            let Some(crate::native_app::AppViewState::Editor(state)) =
                app.native_runtime.view_state(editor)
            else {
                panic!("editor");
            };
            assert_eq!(
                state.buffer.as_ref().unwrap().directory_coordinates(),
                coordinates
            );
        }
        for view in std::iter::once(source).chain(coalesced) {
            let Some(crate::native_app::AppViewState::Recovery(state)) =
                app.native_runtime.view_state(view)
            else {
                panic!("live recovery");
            };
            assert!(
                state.pending.is_none() && state.notice.is_some(),
                "the valid origin receives its one-shot reply at its actual location"
            );
        }
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn posted_refusal_cannot_revive_and_late_requests_keep_their_own_effect_epoch() {
    use super::super::{DOCUMENT_ADMISSION_DELIVERY, DocumentAdmission, DocumentAdmissionRejected};
    for late in 0..3 {
        let (_scratch, mut app, path, uri, document, editor) = directory_reopen_fixture();
        let original = WindowId(0);
        let other = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
        let before = app.document_store.snapshot(document).unwrap();
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(editor)
        else {
            panic!("editor");
        };
        let coordinates = state.buffer.as_ref().unwrap().directory_coordinates();
        std::fs::write(path.join("a.txt"), "").unwrap();
        let (delivery, delivery_rx) = mpsc::channel();
        // Both file and prepared outcomes follow the production posted path.
        // This transport never blocks the UI sender.
        DOCUMENT_ADMISSION_DELIVERY.set(Some(Arc::new(move |outcome| {
            delivery.send(outcome).unwrap();
        })));
        let (_, source) = directory_recovery_action(&mut app, original, &uri);
        let read = delivery_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        let old_ticket = read.ticket;
        let key = read.key.clone();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        WORK_OBSERVER.set(Some(Arc::new(move |name, _| {
            if name == "aterm-document-prepare" {
                entered.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            }
        })));
        app.complete_document_admission(read).unwrap();
        let borrowed = app.native_documents.admissions[&(original, key.clone())]
            .preparation
            .as_ref()
            .unwrap()
            .clone();
        drive_until(&mut app, |_| entered_rx.try_recv().ok());
        WORK_OBSERVER.set(None);
        app.migrate_active_tab_from_window(original).unwrap();
        assert_eq!(app.locate_native_view(source).unwrap().0, other);
        amacs::store::take_preparation_work();
        release.send(()).unwrap();
        let posted = drive_until(&mut app, |_| delivery_rx.try_recv().ok());
        assert_eq!(posted.ticket, old_ticket);
        let refusal = app.native_documents.admissions[&(original, key.clone())]
            .prepared
            .as_ref()
            .unwrap()
            .as_ref()
            .expect("the original request was prepared")
            .refusal()
            .expect("the original canonical decision refused refresh");
        let unchanged = app.document_store.snapshot(document).unwrap();
        assert_eq!(unchanged.seq, before.seq);
        assert!(Arc::ptr_eq(&unchanged.text, &before.text));
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(editor)
        else {
            panic!("editor");
        };
        assert_eq!(
            state.buffer.as_ref().unwrap().directory_coordinates(),
            coordinates
        );

        // The real locator is true again before delivery of the posted result.
        app.migrate_active_tab_from_window(other).unwrap();
        assert_eq!(app.locate_native_view(source).unwrap().0, original);
        let readiness = borrowed.poll().unwrap().unwrap();
        let installed = app
            .install_prepared_document(other, AppKind::Editor, &uri, readiness, Placement::NewTab)
            .unwrap();
        assert_eq!(installed.document, document);
        assert_eq!(
            app.document_store.snapshot(document).unwrap().seq,
            before.seq,
            "a legitimate readiness borrower does not acquire the refused refresh effect"
        );

        let mut live_sources = Vec::new();
        if late == 0 {
            app.complete_document_admission(posted).unwrap();
            assert_eq!(
                app.active_native_view(original).unwrap().1,
                source,
                "a sealed refusal must not install/focus the directory after the guard returns true"
            );
        } else {
            if late == 1 {
                live_sources.push(directory_recovery_action(&mut app, original, &uri).1);
                let fresh_ticket = app.native_documents.admissions[&(original, key.clone())].ticket;
                assert_ne!(fresh_ticket, old_ticket);
                live_sources.push(directory_recovery_action(&mut app, original, &uri).1);
                assert_eq!(
                    app.native_documents.admissions[&(original, key.clone())].ticket,
                    fresh_ticket
                );
                assert_eq!(
                    app.native_documents.admissions[&(original, key.clone())]
                        .continuations
                        .len(),
                    2
                );
            } else {
                assert!(matches!(
                    app.open_document_tab_revealing(
                        original,
                        AppKind::Editor,
                        &uri,
                        None,
                        super::super::DocumentAdmissionPurpose::Open
                    )
                    .unwrap(),
                    DocumentAdmission::InFlight(_)
                ));
            }
            let read = delivery_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
            assert_ne!(read.ticket, old_ticket);
            let new_ticket = read.ticket;
            app.complete_document_admission(read).unwrap();
            let fresh_posted = drive_until(&mut app, |_| delivery_rx.try_recv().ok());
            assert_eq!(fresh_posted.ticket, new_ticket);
            assert!(
                app.native_documents.admissions[&(original, key.clone())]
                    .prepared
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .expect("the fresh request was prepared")
                    .refusal()
                    .is_none()
            );
            assert!(matches!(
                app.complete_document_admission(posted),
                Err(DocumentAdmissionRejected::Dropped(_))
            ));
            assert!(
                app.native_documents.admissions[&(original, key.clone())]
                    .prepared
                    .is_some(),
                "an old ticket cannot consume a new epoch's posted readiness/outcome"
            );
            app.complete_document_admission(fresh_posted).unwrap();
            let (instance, _) = app.active_native_view(original).unwrap();
            assert_eq!(app.native_runtime.document_id(instance), Some(document));
        }
        let ui = amacs::store::take_preparation_work();
        assert_eq!(ui.diff_input_bytes, 0);
        assert_eq!(ui.directory_entry_visits, 0);
        assert_eq!(ui.retired_directory_entries, 0);
        assert_eq!(ui.adoptions, usize::from(late != 0));
        let Some(crate::native_app::AppViewState::Recovery(state)) =
            app.native_runtime.view_state(source)
        else {
            panic!("original recovery");
        };
        assert!(state.pending.is_none());
        assert!(
            state
                .notice
                .as_ref()
                .is_some_and(|notice| notice.contains(refusal)),
            "the original request receives its immutable refused effect exactly once"
        );
        for view in live_sources {
            let Some(crate::native_app::AppViewState::Recovery(state)) =
                app.native_runtime.view_state(view)
            else {
                panic!("live recovery");
            };
            assert!(state.pending.is_none() && state.notice.is_some());
        }
        let after = app.document_store.snapshot(document).unwrap();
        if late == 0 {
            assert_eq!(after.seq, before.seq);
            assert!(Arc::ptr_eq(&after.text, &before.text));
        } else {
            assert!(after.seq > before.seq && after.text.contains("a.txt"));
        }
        drop(borrowed);
        DOCUMENT_ADMISSION_DELIVERY.set(None);
        drive_until(&mut app, |app| {
            (!app.document_preparation_pending(&uri)).then_some(())
        });
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn closed_reopen_does_not_borrow_read_only_preparation_authority_or_widen_a_hard_read_only_buffer()
{
    for (pending, hard_read_only) in [(true, false), (false, false), (false, true)] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let path = root.join("access.txt");
        std::fs::write(&path, "access").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(root.join("journals"));
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        if pending {
            INITIALIZE_GATE.set(Some(Box::new(move || {
                entered.send(()).unwrap();
                release_rx
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            })));
        }
        let read_only = app.prepare_document_open(
            App::admit_document_now(&uri, false).unwrap(),
            GrantAccess::ReadOnly,
            &uri,
        );
        let mut retained = None;
        if pending {
            drive_until(&mut app, |_| entered_rx.try_recv().ok());
        } else {
            let prepared = ready(&mut app, &read_only);
            if hard_read_only {
                app.document_store.set_read_only(prepared.document).unwrap();
            }
            retained = Some(prepared);
            assert!(
                app.ready_restore_document(&uri).is_none(),
                "ReadOnly is not a ready ReadWrite lease"
            );
        }
        app.closed_recovery.tabs.push(
            crate::closed_recovery::ClosedTab {
                original_window: WindowId(0),
                original_index: 1,
                tab: crate::restore::RestoredTab {
                    root: crate::restore::RestoredSplitTree::leaf(
                        crate::restore::RestoredView::Native(
                            crate::restore::NativeLeafRestore::document("editor", uri.clone()),
                        ),
                    ),
                    focused_path: Vec::new(),
                    zoomed: false,
                    parked: false,
                    ephemeral: false,
                    fleet: false,
                },
            },
            0,
        );
        crate::app_documents::with_headless_restore_admission_for_test(|| {
            app.reopen_last_closed_tab().unwrap();
            if pending {
                assert_eq!(app.closed_recovery.tabs.len(), 1);
                assert_eq!(app.unpublished_attachment_count_for_test(), 0);
                release.send(()).unwrap();
            }
            let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while !app.closed_reopens.is_empty() {
                app.progress_closed_reopens();
                assert!(Instant::now() < until);
                aterm_uds::deadline::poll_yield();
            }
        });
        assert!(app.closed_recovery.tabs.is_empty());
        let (instance, _) = app.active_native_view(WindowId(0)).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        assert_eq!(app.document_store.read_only(document), hard_read_only);
        let before = app.document_store.snapshot(document).unwrap();
        let _ = app.dispatch_native_event(
            WindowId(0),
            AppEvent::TextInput(TextInputEvent::Commit("new ".into())),
        );
        if hard_read_only {
            assert_eq!(
                app.document_store.snapshot(document).unwrap().text,
                before.text
            );
            assert!(!app.can_save_document(WindowId(0)));
        } else {
            assert!(app.can_save_document(WindowId(0)));
            app.save_front_document().unwrap();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "new access",
                "reopen obtained its own write authority"
            );
        }
        drop(retained);
        drop(read_only);
    }
}

#[track_caller]
fn drive_combined_directory_policy<T>(
    app: &mut App,
    case: &str,
    phase: &str,
    mut observe: impl FnMut(&mut App) -> Option<T>,
) -> T {
    eprintln!("combined directory policy: {case} phase={phase}");
    let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        app.progress_closed_reopens();
        app.progress_document_preparations();
        if let Some(value) = observe(app) {
            return value;
        }
        assert!(
            Instant::now() < until,
            "combined directory policy did not settle: {case} phase={phase} closed={} history={} custody={} admissions={} preparations={}",
            app.closed_reopens.len(),
            app.closed_recovery.tabs.len(),
            app.unpublished_attachment_count_for_test(),
            app.native_documents.admissions.len(),
            app.native_documents.preparations.len()
        );
        aterm_uds::deadline::poll_yield();
    }
}

#[test]
fn actual_closed_directory_reopen_and_posted_ordinary_refresh_keep_independent_policies() {
    use super::super::{DOCUMENT_ADMISSION_DELIVERY, DocumentAdmission};
    for (restore_first, cancel, no_refresh_worker_first) in [
        (false, false, false),
        (true, false, false),
        (true, false, true),
        (false, true, false),
    ] {
        for entries in [1, 256] {
            let case = format!(
                "restore_first={restore_first} cancel={cancel} no_refresh_worker_first={no_refresh_worker_first} entries={entries}"
            );
            eprintln!("combined directory policy: {case} phase=fixture");
            let (_scratch, mut app, path, uri, document, original_view) =
                directory_reopen_fixture();
            let requester =
                app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
            let before = app.document_store.snapshot(document).unwrap();
            let Some(crate::native_app::AppViewState::Editor(state)) =
                app.native_runtime.view_state(original_view)
            else {
                panic!("original directory");
            };
            let coordinates = state.buffer.as_ref().unwrap().directory_coordinates();
            for entry in 0..entries {
                std::fs::write(path.join(format!("a-{entry}.txt")), "").unwrap();
            }
            let (delivery, delivery_rx) = mpsc::channel();
            DOCUMENT_ADMISSION_DELIVERY.set(Some(Arc::new(move |outcome| {
                delivery.send(outcome).unwrap();
            })));
            let (entered, entered_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            let release_rx = Mutex::new(release_rx);
            WORK_OBSERVER.set(Some(Arc::new(move |name, work| {
                if name == "aterm-document-prepare" {
                    entered.send(work).unwrap();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .unwrap();
                }
            })));
            app.closed_recovery.tabs.push(
                crate::closed_recovery::ClosedTab {
                    original_window: WindowId(0),
                    original_index: 1,
                    tab: crate::restore::RestoredTab {
                        root: crate::restore::RestoredSplitTree::leaf(
                            crate::restore::RestoredView::Native(
                                crate::restore::NativeLeafRestore::document("editor", uri.clone()),
                            ),
                        ),
                        focused_path: Vec::new(),
                        zoomed: false,
                        parked: false,
                        ephemeral: false,
                        fleet: false,
                    },
                },
                0,
            );
            crate::app_documents::with_headless_restore_admission_for_test(|| {
                if restore_first && !no_refresh_worker_first {
                    // Preserve the warm canonical fast path: closed reopen needs
                    // neither a read nor a preparation job to reuse this live head.
                    app.reopen_last_closed_tab().unwrap();
                    drive_combined_directory_policy(
                        &mut app,
                        &case,
                        "warm canonical reuse published",
                        |app| app.closed_reopens.is_empty().then_some(()),
                    );
                    assert!(
                        matches!(entered_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
                        "{case}: warm reuse must not start preparation"
                    );
                    assert!(!app.document_preparation_pending(&uri));
                    assert!(app.closed_recovery.tabs.is_empty());
                    assert_eq!(app.unpublished_attachment_count_for_test(), 0);
                    let unchanged = app.document_store.snapshot(document).unwrap();
                    assert_eq!(unchanged.seq, before.seq);
                    assert!(Arc::ptr_eq(&unchanged.text, &before.text));
                    let Some(crate::native_app::AppViewState::Editor(state)) =
                        app.native_runtime.view_state(original_view)
                    else {
                        panic!("original directory");
                    };
                    assert_eq!(
                        state.buffer.as_ref().unwrap().directory_coordinates(),
                        coordinates
                    );
                    assert_eq!(app.document_store.view_count(document), Some(2));
                    // The independently requested ordinary open still performs
                    // its real posted read and refresh after that no-refresh reuse.
                    assert!(matches!(
                        app.open_document_tab_revealing(
                            requester,
                            AppKind::Editor,
                            &uri,
                            None,
                            super::super::DocumentAdmissionPurpose::Open
                        )
                        .unwrap(),
                        DocumentAdmission::InFlight(_)
                    ));
                    app.complete_document_admission(
                        delivery_rx
                            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                            .unwrap(),
                    )
                    .unwrap();
                    let work = drive_combined_directory_policy(
                        &mut app,
                        &case,
                        "ordinary worker after warm reuse held",
                        |_| entered_rx.try_recv().ok(),
                    );
                    assert!(work.diff_input_bytes > 0);
                    WORK_OBSERVER.set(None);
                    amacs::store::take_preparation_work();
                    release.send(()).unwrap();
                    let posted = drive_combined_directory_policy(
                        &mut app,
                        &case,
                        "ordinary outcome after warm reuse posted",
                        |_| delivery_rx.try_recv().ok(),
                    );
                    app.complete_document_admission(posted).unwrap();
                    let after = app.document_store.snapshot(document).unwrap();
                    assert!(after.seq > before.seq && after.text.contains("a-0.txt"));
                    let (instance, _) = app.active_native_view(WindowId(0)).unwrap();
                    assert_eq!(app.native_runtime.document_id(instance), Some(document));
                    assert_eq!(app.document_store.id_for_uri(&uri), Some(document));
                    let ui = amacs::store::take_preparation_work();
                    assert_eq!(ui.adoptions, 1);
                    assert_eq!(ui.diff_input_bytes, 0);
                    assert_eq!(ui.directory_entry_visits, 0);
                    assert_eq!(ui.retired_directory_entries, 0);
                    assert!(app.structural_invariants_ok());
                    return;
                }
                // A separate real no-refresh admission fixture exercises worker
                // ordering without forcing I/O onto the warm canonical fast path.
                // With no pending owner, closed reopen correctly reuses the live
                // canonical document without a worker; this extra case deliberately
                // starts an independently owned restore admission first.
                let mut restore_admission = if restore_first {
                    Some(
                        app.begin_restore_document_admission(
                            WindowId(0),
                            &crate::restore::NativeLeafRestore::document("editor", uri.clone()),
                        )
                        .unwrap(),
                    )
                } else {
                    None
                };
                if !restore_first {
                    assert!(matches!(
                        app.open_document_tab_revealing(
                            requester,
                            AppKind::Editor,
                            &uri,
                            None,
                            super::super::DocumentAdmissionPurpose::Open
                        )
                        .unwrap(),
                        DocumentAdmission::InFlight(_)
                    ));
                    app.complete_document_admission(
                        delivery_rx
                            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                            .unwrap(),
                    )
                    .unwrap();
                }
                let first_work = drive_combined_directory_policy(
                    &mut app,
                    &case,
                    "first preparation worker held",
                    |app| {
                        if let Some(ticket) = restore_admission.as_mut() {
                            assert!(
                                app.poll_restore_document_admission(ticket).is_none(),
                                "{case}: held no-refresh admission cannot complete yet"
                            );
                        }
                        entered_rx.try_recv().ok()
                    },
                );
                WORK_OBSERVER.set(None);
                assert_eq!(first_work.diff_input_bytes > 0, !restore_first);
                app.reopen_last_closed_tab().unwrap();
                drive_combined_directory_policy(
                    &mut app,
                    &case,
                    "closed ticket borrows held preparation",
                    |app| {
                        (app.native_documents.preparations[&uri]
                            .jobs
                            .front()
                            .unwrap()
                            .shared
                            .leases
                            .load(Ordering::Acquire)
                            > 1)
                        .then_some(())
                    },
                );
                let (next_entered, next_entered_rx) = mpsc::channel();
                let (next_release, next_release_rx) = mpsc::channel();
                if restore_first {
                    let next_release_rx = Mutex::new(next_release_rx);
                    WORK_OBSERVER.set(Some(Arc::new(move |name, work| {
                        if name == "aterm-document-prepare" {
                            next_entered.send(work).unwrap();
                            next_release_rx
                                .lock()
                                .unwrap()
                                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                                .unwrap();
                        }
                    })));
                    assert!(matches!(
                        app.open_document_tab_revealing(
                            requester,
                            AppKind::Editor,
                            &uri,
                            None,
                            super::super::DocumentAdmissionPurpose::Open
                        )
                        .unwrap(),
                        DocumentAdmission::InFlight(_)
                    ));
                    app.complete_document_admission(
                        delivery_rx
                            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                            .unwrap(),
                    )
                    .unwrap();
                }
                assert_eq!(app.closed_recovery.tabs.len(), 1);
                assert_eq!(app.unpublished_attachment_count_for_test(), 0);
                if cancel {
                    app.close_window_logical(requester);
                }
                amacs::store::take_preparation_work();
                release.send(()).unwrap();
                if restore_first {
                    // Advance preparation alone so the ordinary worker captures
                    // the original view before the closed candidate adds its view.
                    let second_work = drive_until(&mut app, |_| next_entered_rx.try_recv().ok());
                    eprintln!(
                        "combined directory policy: {case} phase=ordinary refresh captured before publication"
                    );
                    assert_eq!(app.document_store.view_count(document), Some(1));
                    assert!(!app.closed_reopens.is_empty());
                    WORK_OBSERVER.set(None);
                    assert!(second_work.diff_input_bytes > 0);
                    let unchanged = app.document_store.snapshot(document).unwrap();
                    assert_eq!(unchanged.seq, before.seq);
                    assert!(Arc::ptr_eq(&unchanged.text, &before.text));
                    let Some(crate::native_app::AppViewState::Editor(state)) =
                        app.native_runtime.view_state(original_view)
                    else {
                        panic!("original directory");
                    };
                    assert_eq!(
                        state.buffer.as_ref().unwrap().directory_coordinates(),
                        coordinates
                    );
                    let ticket = restore_admission.as_mut().unwrap();
                    assert!(
                        app.poll_restore_document_admission(ticket).unwrap().is_ok(),
                        "{case}: no-refresh admission must retain independent readiness"
                    );
                    drop(restore_admission.take());
                }
                drive_combined_directory_policy(
                    &mut app,
                    &case,
                    "closed candidate publication",
                    |app| app.closed_reopens.is_empty().then_some(()),
                );
                let published_coordinates = app
                    .document_native_views(document)
                    .into_iter()
                    .map(|(_, _, view)| {
                        let Some(crate::native_app::AppViewState::Editor(state)) =
                            app.native_runtime.view_state(view)
                        else {
                            panic!("published directory view");
                        };
                        (view, state.buffer.as_ref().unwrap().directory_coordinates())
                    })
                    .collect::<Vec<_>>();
                if restore_first {
                    assert_eq!(app.document_store.view_count(document), Some(2));
                    next_release.send(()).unwrap();
                }
                if !cancel {
                    let posted = drive_combined_directory_policy(
                        &mut app,
                        &case,
                        "ordinary prepared outcome posted",
                        |_| delivery_rx.try_recv().ok(),
                    );
                    let result = app.complete_document_admission(posted);
                    if restore_first {
                        assert!(
                            matches!(
                                result,
                                Err(super::super::DocumentAdmissionRejected::Failed(_))
                            ),
                            "{case}: a newly published view invalidates the captured refresh"
                        );
                        for (view, coordinates) in &published_coordinates {
                            let Some(crate::native_app::AppViewState::Editor(state)) =
                                app.native_runtime.view_state(*view)
                            else {
                                panic!("published directory view");
                            };
                            assert_eq!(
                                state.buffer.as_ref().unwrap().directory_coordinates(),
                                *coordinates
                            );
                        }
                    } else {
                        result.unwrap();
                    }
                }
                assert!(app.closed_recovery.tabs.is_empty());
                assert_eq!(app.unpublished_attachment_count_for_test(), 0);
                let (instance, _) = app.active_native_view(WindowId(0)).unwrap();
                assert_eq!(app.native_runtime.document_id(instance), Some(document));
                assert_eq!(app.document_store.id_for_uri(&uri), Some(document));
                if cancel || restore_first {
                    let unchanged = app.document_store.snapshot(document).unwrap();
                    assert_eq!(unchanged.seq, before.seq);
                    assert!(Arc::ptr_eq(&unchanged.text, &before.text));
                    let Some(crate::native_app::AppViewState::Editor(state)) =
                        app.native_runtime.view_state(original_view)
                    else {
                        panic!("original directory");
                    };
                    assert_eq!(
                        state.buffer.as_ref().unwrap().directory_coordinates(),
                        coordinates
                    );
                    // A later ordinary epoch owns a fresh view snapshot after either
                    // cancellation or the stale-view refusal of the earlier request.
                    assert!(matches!(
                        app.open_document_tab_revealing(
                            WindowId(0),
                            AppKind::Editor,
                            &uri,
                            None,
                            super::super::DocumentAdmissionPurpose::Open
                        )
                        .unwrap(),
                        DocumentAdmission::InFlight(_)
                    ));
                    let read = delivery_rx
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .unwrap();
                    app.complete_document_admission(read).unwrap();
                    let posted = drive_combined_directory_policy(
                        &mut app,
                        &case,
                        "late independent prepared outcome posted",
                        |_| delivery_rx.try_recv().ok(),
                    );
                    app.complete_document_admission(posted).unwrap();
                }
                let after = app.document_store.snapshot(document).unwrap();
                assert!(after.seq > before.seq && after.text.contains("a-0.txt"));
                let ui = amacs::store::take_preparation_work();
                assert_eq!(ui.adoptions, 1);
                assert_eq!(ui.diff_input_bytes, 0);
                assert_eq!(ui.directory_entry_visits, 0);
                assert_eq!(ui.retired_directory_entries, 0);
                assert!(app.structural_invariants_ok());
            });
            WORK_OBSERVER.set(None);
            DOCUMENT_ADMISSION_DELIVERY.set(None);
            drive_combined_directory_policy(
                &mut app,
                &case,
                "all preparation custody retired",
                |app| (!app.document_preparation_pending(&uri)).then_some(()),
            );
        }
    }
}
