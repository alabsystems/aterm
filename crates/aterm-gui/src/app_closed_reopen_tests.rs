// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::native_app::{AppEvent, AppKind, TextInputEvent};
use crate::restore::{NativeLeafRestore, RestoredSplitTree, RestoredTab, RestoredView};
use std::sync::mpsc;

const HANG: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
const WID: WindowId = WindowId(0);

fn record(root: RestoredSplitTree) -> RestoredTab {
    RestoredTab {
        root,
        focused_path: Vec::new(),
        zoomed: false,
        parked: false,
        ephemeral: false,
        fleet: false,
    }
}

fn document(uri: &str) -> RestoredSplitTree {
    RestoredSplitTree::leaf(RestoredView::Native(NativeLeafRestore::document(
        "editor",
        uri.into(),
    )))
}

fn push(app: &mut App, tab: RestoredTab) {
    app.closed_recovery.tabs.push(
        ClosedTab {
            original_window: WID,
            original_index: 1,
            tab,
        },
        app.lat_epoch.elapsed().as_millis() as u64,
    );
}

fn drive(app: &mut App, mut done: impl FnMut(&App) -> bool) {
    let until = std::time::Instant::now() + HANG;
    while !done(app) {
        app.progress_closed_reopens();
        assert!(std::time::Instant::now() < until, "reopen did not settle");
        aterm_uds::deadline::poll_yield();
    }
}

struct Release(Option<mpsc::Sender<()>>);
impl Release {
    fn now(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.now();
    }
}

#[test]
fn actual_read_and_journal_hold_return_to_caller_and_coalesce_without_a_prefix() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("closed.txt");
    std::fs::write(&path, "saved text").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut app = App::headless_for_test();
    let (journal, journal_rx) = mpsc::channel();
    let (journal_release, journal_release_rx) = mpsc::channel();
    let mut journal_release = Release(Some(journal_release));
    app.open_document_journals_for_test(move || {
        journal.send(()).unwrap();
        journal_release_rx.recv_timeout(HANG).unwrap();
        crate::native_document_journal::DocumentJournalStore::for_test(root.join("journals"))
    });
    journal_rx.recv_timeout(HANG).unwrap();
    push(&mut app, record(document(&uri)));
    let before = app.view_store.len();
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let mut release = Release(Some(release));
    crate::app_documents::with_headless_restore_admission_for_test(|| {
        crate::app_documents::with_restore_read_gate_for_test(
            move || {
                entered.send(()).unwrap();
                release_rx.recv_timeout(HANG).unwrap();
            },
            || {
                app.reopen_last_closed_tab().unwrap();
                // The actual reader is still held after the UI caller returned.
                entered_rx.recv_timeout(HANG).unwrap();
                app.reopen_last_closed_tab().unwrap();
                assert_eq!(app.closed_reopens.len(), 1);
                assert_eq!(app.view_store.len(), before);
                assert_eq!(app.closed_recovery.tabs.len(), 1);
                release.now();
                drive(&mut app, |app| app.document_preparation_pending(&uri));
                assert_eq!(app.view_store.len(), before);
                assert_eq!(app.closed_recovery.tabs.len(), 1);
                assert!(!app.closed_reopens.is_empty());
                journal_release.now();
                drive(&mut app, |app| app.closed_reopens.is_empty());
            },
        );
    });
    assert_eq!(app.closed_recovery.tabs.len(), 0);
    assert_eq!(app.view_store.len(), before + 1);
    assert!(app.structural_invariants_ok());
    assert!(app.unpublished_attachment_count_for_test() == 0);
}

#[test]
fn stale_candidate_window_death_and_explicit_cancel_keep_history_after_late_read() {
    for fault in 0..4 {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let path = root.join("late.txt");
        std::fs::write(&path, "late").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(root.join("journals"));
        push(&mut app, record(document(&uri)));
        if fault == 1 {
            app.open_active_session_in_new_window_logical().unwrap();
        }
        let before = app.view_store.len();
        let token = app
            .closed_recovery
            .tabs
            .candidate_snapshot(0)
            .unwrap()
            .token;
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let mut release = Release(Some(release));
        crate::app_documents::with_headless_restore_admission_for_test(|| {
            crate::app_documents::with_restore_read_gate_for_test(
                move || {
                    entered.send(()).unwrap();
                    release_rx.recv_timeout(HANG).unwrap();
                },
                || {
                    app.reopen_last_closed_tab().unwrap();
                    entered_rx.recv_timeout(HANG).unwrap();
                    match fault {
                        0 => push(&mut app, record(document(&uri))),
                        1 => {
                            assert_eq!(app.close_window_logical(WID), crate::CloseOutcome::Stay);
                            assert!(!app.windows.contains_key(&WID));
                        }
                        2 => app.cancel_closed_reopen(true).unwrap(),
                        _ => app.closed_recovery.tabs.prune(CLOSED_EXPIRY),
                    }
                    app.progress_closed_reopens();
                    assert!(app.closed_reopens.is_empty());
                    release.now();
                },
            );
        });
        assert_eq!(app.view_store.len(), before - usize::from(fault == 1));
        assert!(app.structural_invariants_ok());
        assert!(app.unpublished_attachment_count_for_test() == 0);
        if fault != 3 {
            assert_eq!(
                app.closed_recovery.tabs.len(),
                if fault == 0 { 2 } else { 1 }
            );
            let current = app
                .closed_recovery
                .tabs
                .candidate_snapshot(0)
                .unwrap()
                .token;
            assert_eq!(current == token, fault != 0);
        }
    }
}
const CLOSED_EXPIRY: u64 = u64::MAX;

#[test]
fn later_leaf_failure_rolls_back_exact_prefix_and_keeps_shared_edits() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("shared.txt");
    std::fs::write(&path, "disk").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(root.join("journals"));
    app.open_document_tab_in_window(WID, AppKind::Editor, &uri)
        .unwrap();
    let (instance, live_view) = app.active_native_view(WID).unwrap();
    let id = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_event(
        WID,
        AppEvent::TextInput(TextInputEvent::Commit("current edit ".into())),
    )
    .unwrap();
    let current = app.document_store.snapshot(id).unwrap();
    let before = app.view_store.len();
    let missing = crate::native_document_host::path_to_file_uri(&root.join("missing.txt")).unwrap();
    push(
        &mut app,
        record(RestoredSplitTree::Split {
            axis: restore::SplitKind::Vertical,
            ratio: 0.37,
            first: Box::new(document(&uri)),
            second: Box::new(document(&missing)),
        }),
    );
    crate::app_documents::with_headless_restore_admission_for_test(|| {
        app.reopen_last_closed_tab().unwrap();
        assert_eq!(
            app.view_store.len(),
            before + 1,
            "first leaf remains unpublished"
        );
        drive(&mut app, |app| app.closed_reopens.is_empty());
    });
    assert_eq!(app.closed_recovery.tabs.len(), 1);
    assert_eq!(app.view_store.len(), before);
    assert!(app.view_store.get(live_view).is_some());
    let after = app.document_store.snapshot(id).unwrap();
    assert_eq!((after.seq, after.text), (current.seq, current.text));
    assert_eq!(app.document_store.view_count(id), Some(1));
    assert!(app.unpublished_attachment_count_for_test() == 0);
    assert!(app.structural_invariants_ok());
}

#[test]
fn repeated_document_leaves_join_one_preparation_and_publish_together() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("twice.md");
    std::fs::write(&path, "# shared").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(root.join("journals"));
    let terminal = app
        .view_restore_descriptor(app.windows[&WID].tab_set.active().unwrap().focus)
        .unwrap();
    let mut tab = record(RestoredSplitTree::Split {
        axis: restore::SplitKind::Horizontal,
        ratio: 0.31,
        first: Box::new(document(&uri)),
        second: Box::new(RestoredSplitTree::Split {
            axis: restore::SplitKind::Vertical,
            ratio: 0.62,
            first: Box::new(RestoredSplitTree::leaf(RestoredView::Native(
                NativeLeafRestore::document("markdown", uri.clone()),
            ))),
            second: Box::new(RestoredSplitTree::Split {
                axis: restore::SplitKind::Horizontal,
                ratio: 0.45,
                first: Box::new(RestoredSplitTree::leaf(RestoredView::Native(
                    NativeLeafRestore::settings(
                        crate::native_settings::SettingsRoute::About.path().into(),
                    ),
                ))),
                second: Box::new(RestoredSplitTree::leaf(terminal)),
            }),
        }),
    });
    tab.focused_path = vec![
        restore::RestoreBranch::Second,
        restore::RestoreBranch::First,
    ];
    tab.zoomed = true;
    push(&mut app, tab);
    crate::app_documents::with_headless_restore_admission_for_test(|| {
        app.reopen_last_closed_tab().unwrap();
        drive(&mut app, |app| app.closed_reopens.is_empty());
    });
    assert_eq!(app.closed_recovery.tabs.len(), 0);
    let document = app.document_store.id_for_uri(&uri).unwrap();
    assert_eq!(app.document_store.view_count(document), Some(2));
    let tab = app.windows[&WID].tab_set.active().unwrap();
    assert!(tab.zoomed);
    assert_eq!(tab.root.len(), 4);
    let kinds = tab.root.map(&mut |view| match app.view_store.get(*view) {
        Some(crate::tab_model::View::Terminal(_)) => "terminal",
        Some(crate::tab_model::View::Native(native)) => {
            match app.native_runtime.app(native.instance).unwrap().kind() {
                AppKind::Editor => "document",
                AppKind::Settings => "settings",
                _ => "other",
            }
        }
        None => "missing",
    });
    assert_eq!(
        kinds,
        SplitTree::Split {
            axis: crate::tab_model::SplitAxis::Horizontal,
            ratio: 0.31,
            first: Box::new(SplitTree::Leaf("document")),
            second: Box::new(SplitTree::Split {
                axis: crate::tab_model::SplitAxis::Vertical,
                ratio: 0.62,
                first: Box::new(SplitTree::Leaf("document")),
                second: Box::new(SplitTree::Split {
                    axis: crate::tab_model::SplitAxis::Horizontal,
                    ratio: 0.45,
                    first: Box::new(SplitTree::Leaf("settings")),
                    second: Box::new(SplitTree::Leaf("terminal")),
                }),
            }),
        }
    );
    let crate::native_app::AppViewState::Editor(state) =
        app.native_runtime.view_state(tab.focus).unwrap()
    else {
        panic!("focused Markdown")
    };
    assert_eq!(
        state.markdown.as_ref().unwrap().mode,
        crate::native_app::MarkdownWorkspaceMode::Read
    );
    assert_eq!(
        tab.focus,
        App::restored_focus(
            &tab.root,
            &[
                restore::RestoreBranch::Second,
                restore::RestoreBranch::First
            ]
        )
        .unwrap()
    );
    assert!(app.unpublished_attachment_count_for_test() == 0);
    assert!(app.structural_invariants_ok());
}

fn project(
    model: &aterm_spec::derive::Model,
    app: &App,
    started: bool,
    id: crate::document_store::DocumentId,
    original: crate::closed_recovery::RecoveryToken,
    baseline: &str,
) -> aterm_spec::interp::State {
    let mut state = model.init_state();
    let prefix = app
        .closed_reopens
        .first()
        .map_or(0, |pending| pending.build.owned_leaf_count());
    let phase = if app.closed_reopens.is_empty() {
        if started { 3 } else { 0 }
    } else if prefix == 2 {
        2
    } else {
        1
    };
    let published = app
        .document_store
        .document_ids()
        .into_iter()
        .map(|document| app.document_store.view_count(document).unwrap_or(0) as i64)
        .sum::<i64>()
        - 1
        - app.unpublished_attachment_count_for_test() as i64;
    let now = app.lat_epoch.elapsed().as_millis() as u64;
    let candidate = app.closed_recovery.tabs.candidate_snapshot(now);
    state.insert("phase", phase);
    state.insert(
        "candidate",
        candidate.map_or(
            if now > crate::closed_recovery::CLOSED_TAB_MAX_AGE_MS {
                0
            } else {
                1
            },
            |candidate| if candidate.token == original { 1 } else { 2 },
        ),
    );
    state.insert(
        "window",
        i64::from(
            app.windows
                .get(&WID)
                .is_some_and(|window| !window.pending_close),
        ),
    );
    state.insert(
        "prefix",
        app.closed_reopens
            .first()
            .map_or(0, |pending| pending.build.owned_leaf_count()) as i64,
    );
    state.insert(
        "custody",
        app.unpublished_attachment_count_for_test() as i64,
    );
    state.insert("published", published);
    state.insert(
        "consumed",
        i64::from(
            !app.closed_recovery.tabs.retains_token_for_test(original)
                && now <= crate::closed_recovery::CLOSED_TAB_MAX_AGE_MS,
        ),
    );
    let edited = i64::from(app.document_store.snapshot(id).unwrap().text.as_ref() != baseline);
    state.insert("canonical", edited);
    state.insert("edited", edited);
    state
}

#[test]
fn production_publication_conforms_and_rejects_async_identity_and_custody_mutants() {
    use aterm_spec::interp::{self, State};
    let model = aterm_models_native::closed_reopen_publication_model();
    for scenario in 0..5 {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let path = root.join("canonical.txt");
        std::fs::write(&path, "disk").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(root.join("journals"));
        app.open_document_tab_in_window(WID, AppKind::Editor, &uri)
            .unwrap();
        let (instance, _) = app.active_native_view(WID).unwrap();
        let id = app.native_runtime.document_id(instance).unwrap();
        let initial = app.document_store.snapshot(id).unwrap();
        push(
            &mut app,
            record(RestoredSplitTree::Split {
                axis: restore::SplitKind::Vertical,
                ratio: 0.4,
                first: Box::new(document(&uri)),
                second: Box::new(document(&uri)),
            }),
        );
        let original = app
            .closed_recovery
            .tabs
            .candidate_snapshot(0)
            .unwrap()
            .token;
        let started = std::cell::Cell::new(false);
        let project = |app: &App| -> State {
            project(
                &model,
                app,
                started.get(),
                id,
                original,
                initial.text.as_ref(),
            )
        };
        let assert_step = |before: &State, after: &State, action| {
            assert!(
                model.successors(action, before).contains(after),
                "{action}: {before:?} -> {after:?}"
            );
        };
        let initial = project(&app);
        app.reopen_last_closed_tab().unwrap();
        started.set(true);
        let began = project(&app);
        assert_step(&initial, &began, "Begin");
        app.dispatch_native_event(
            WID,
            AppEvent::TextInput(TextInputEvent::Commit("edit ".into())),
        )
        .unwrap();
        let edited = project(&app);
        assert_step(&began, &edited, "Edit");
        let mut operation = app.closed_reopens.pop().unwrap();
        let cold::Step::Complete(built) = operation.build.step(&mut app, WID) else {
            panic!("canonical second leaf completes without IO");
        };
        app.closed_reopens.push(operation);
        let staged = project(&app);
        assert_step(&edited, &staged, "Stage");
        let mut before = staged.clone();
        if scenario == 1 {
            push(&mut app, record(document(&uri)));
            let replaced = project(&app);
            assert_step(&before, &replaced, "Replace");
            before = replaced;
        }
        if scenario == 2 {
            app.windows.get_mut(&WID).unwrap().pending_close = true;
            let dead = project(&app);
            assert_step(&before, &dead, "Kill");
            before = dead;
        }
        if scenario == 0 {
            let operation = app.closed_reopens.pop().unwrap();
            app.publish_closed_reopen(&operation, built).unwrap();
            let published = project(&app);
            assert_step(&before, &published, "Publish");
            // The owner is gone and its consumed history cannot publish or spend custody again.
            assert!(app.reopen_last_closed_tab().is_err());
            assert_step(&published, &project(&app), "Repeat");
        } else {
            if scenario <= 2 {
                let operation = &app.closed_reopens[0];
                // Invoke the actual publication boundary, including exact candidate/window checks.
                // Moving the owner out avoids aliasing App during the exclusive transition.
                assert!(operation.build.validate_cancel(&app).is_ok());
            }
            if scenario == 1 || scenario == 2 {
                let operation = app.closed_reopens.pop().unwrap();
                assert!(app.publish_closed_reopen(&operation, built).is_err());
                app.closed_reopens.push(operation);
            } else if scenario == 3 {
                app.with_missing_restore_attachment_for_test(|app| {
                    let operation = app.closed_reopens.pop().unwrap();
                    assert!(app.publish_closed_reopen(&operation, built).is_err());
                    app.closed_reopens.push(operation);
                    assert!(
                        app.cancel_closed_reopen(true).is_err(),
                        "lost custody retains its owner"
                    );
                    assert_eq!(app.closed_reopens.len(), 1);
                });
            }
            app.cancel_closed_reopen(true).unwrap();
            let canceled = project(&app);
            assert_step(&before, &canceled, "Cancel");
            assert_eq!(app.document_store.view_count(id), Some(1));
        }
        assert!(app.structural_invariants_ok());
    }
    // Reachable mutated transitions, each rejected by the actual protocol's model relation.
    let begin = model.successors("Begin", &model.init_state()).remove(0);
    let staged = model.successors("Stage", &begin).remove(0);
    let replaced = model.successors("Replace", &staged).remove(0);
    let dead = model.successors("Kill", &staged).remove(0);
    let edited = model.successors("Edit", &staged).remove(0);
    let published = model.successors("Publish", &staged).remove(0);
    for (dial, law, action, before) in [
        (
            1,
            "HistoryOnlyAfterPublication",
            "Begin",
            model.init_state(),
        ),
        (2, "CompleteTopology", "Stage", begin),
        (3, "ExactCandidate", "Publish", replaced),
        (4, "LiveDestination", "Publish", dead),
        (5, "SharedEditsSurvive", "Cancel", edited),
        (6, "OneShotCustody", "Repeat", published),
    ] {
        let mutant = interp::with_buggy(&model, dial);
        let bad = mutant.successors(action, &before).remove(0);
        assert!(
            !model.check_invariant(law, &bad),
            "{law} needs a negative witness"
        );
        assert!(
            !model.successors(action, &before).contains(&bad),
            "{action} admitted mutant {dial}"
        );
    }
}

#[test]
fn closed_view_reuses_its_leaf_for_graft_or_fallback_and_preserves_current_branches() {
    use crate::native_settings::SettingsRoute;
    use crate::tab_model::SplitAxis;
    for fallback in [false, true] {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(SettingsRoute::About));
        let tab = app.windows[&WID].tab_set.active_id().unwrap();
        let (_, native) = app.active_native_view(WID).unwrap();
        app.split_active_with_stub_terminal(WID, SplitAxis::Horizontal);
        let survivor = app.windows[&WID].tab_set.active().unwrap().focus;
        // Focus and close the native leaf, retaining its actual placement.
        app.windows
            .get_mut(&WID)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .focus = native;
        app.close_focused_mixed_leaf(WID).unwrap();
        let candidate = app.closed_recovery.views.candidate_snapshot(0).unwrap();
        let source = Source::View(candidate.clone());
        // Construct one detached leaf manually so a host edit can intervene at readiness.
        let mut operation = Pending {
            source,
            window: WID,
            survivor: Some(SplitTree::Leaf(survivor)),
            build: cold::BuildingTab::reopen(
                2,
                record(RestoredSplitTree::leaf(candidate.value.view)),
            ),
            canceling: false,
        };
        let before = app.view_store.len();
        let cold::Step::Complete(built) = operation.build.step(&mut app, WID) else {
            panic!("one native leaf should stage once");
        };
        let reopened = built.0.focus;
        assert_eq!(app.view_store.len(), before + 1);
        if fallback {
            app.split_active_stub_tab_dir(WID, crate::pane::SplitDir::Vertical);
        }
        let current = app.windows[&WID].tab_set.get(tab).unwrap().root.clone();
        app.publish_closed_reopen(&operation, built).unwrap();
        assert_eq!(app.closed_recovery.views.len(), 0);
        assert!(app.view_store.get(reopened).is_some());
        assert_eq!(app.view_store.len(), before + 1 + usize::from(fallback));
        if fallback {
            assert_eq!(app.windows[&WID].tab_set.get(tab).unwrap().root, current);
            assert_eq!(
                app.windows[&WID].tab_set.active().unwrap().root,
                SplitTree::Leaf(reopened)
            );
        } else {
            let host = app.windows[&WID].tab_set.get(tab).unwrap();
            assert!(host.root.contains(survivor) && host.root.contains(reopened));
            assert_eq!(host.focus, reopened);
        }
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn real_journal_initialization_holds_publication_and_cancellation_preserves_recovery_entry() {
    for cancel in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let path = root.join("initialized.txt");
        std::fs::write(&path, "journal initialization").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(root.join("journals"));
        push(&mut app, record(document(&uri)));
        let before = app.view_store.len();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let mut release = Release(Some(release));
        crate::app_documents::set_initialize_gate_for_test(move || {
            entered.send(()).unwrap();
            release_rx.recv_timeout(HANG).unwrap();
        });
        crate::app_documents::with_headless_restore_admission_for_test(|| {
            app.reopen_last_closed_tab().unwrap();
            drive(&mut app, |_| entered_rx.try_recv().is_ok());
            assert_eq!(app.view_store.len(), before);
            assert_eq!(app.closed_recovery.tabs.len(), 1);
            if cancel {
                app.cancel_closed_reopen(true).unwrap();
            }
            release.now();
            if !cancel {
                drive(&mut app, |app| app.closed_reopens.is_empty());
            }
        });
        assert_eq!(app.closed_recovery.tabs.len(), usize::from(cancel));
        assert_eq!(app.view_store.len(), before + usize::from(!cancel));
        assert!(app.unpublished_attachment_count_for_test() == 0);
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn reader_join_and_canceled_restart_preserve_canonical_edits_and_view_state() {
    for cancel in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let path = root.join("joined.txt");
        std::fs::write(&path, "abcdef").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(root.join("journals"));
        let mut descriptor = NativeLeafRestore::document("editor", uri.clone());
        descriptor.editor_selections = vec![
            restore::RestoreSelection { anchor: 1, head: 6 },
            restore::RestoreSelection { anchor: 0, head: 2 },
        ];
        descriptor.primary_selection = 1;
        descriptor.viewport_anchor = 6;
        push(
            &mut app,
            record(RestoredSplitTree::leaf(RestoredView::Native(descriptor))),
        );
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let mut release = Release(Some(release));
        crate::app_documents::with_headless_restore_admission_for_test(|| {
            crate::app_documents::with_restore_read_gate_for_test(
                move || {
                    entered.send(()).unwrap();
                    release_rx.recv_timeout(HANG).unwrap();
                },
                || {
                    app.reopen_last_closed_tab().unwrap();
                    entered_rx.recv_timeout(HANG).unwrap();
                    if cancel {
                        app.cancel_closed_reopen(true).unwrap();
                    }
                    app.open_document_tab_in_window(WID, AppKind::Editor, &uri)
                        .unwrap();
                    let (instance, live_view) = app.active_native_view(WID).unwrap();
                    let id = app.native_runtime.document_id(instance).unwrap();
                    let initial = app.document_store.snapshot(id).unwrap();
                    app.editor_workspace
                        .replace_text(
                            &mut app.document_store,
                            id,
                            initial.seq,
                            "é",
                            crate::native_editor::Origin::Keyboard {
                                view: crate::document_store::DocumentViewId(live_view.get()),
                            },
                        )
                        .unwrap();
                    let edited = app.document_store.snapshot(id).unwrap();
                    if cancel {
                        // The new operation may finish before the abandoned reader returns.
                        app.reopen_last_closed_tab().unwrap();
                    } else {
                        // The original read joins a canonical head edited after it began.
                        release.now();
                    }
                    drive(&mut app, |app| app.closed_reopens.is_empty());
                    release.now();
                    app.progress_closed_reopens();
                    assert_eq!(app.closed_recovery.tabs.len(), 0);
                    assert_eq!(app.document_store.view_count(id), Some(2));
                    let (reopened_instance, reopened) = app.active_native_view(WID).unwrap();
                    assert_eq!(app.native_runtime.document_id(reopened_instance), Some(id));
                    assert_ne!(reopened, live_view);
                    let after = app.document_store.snapshot(id).unwrap();
                    assert_eq!((after.seq, after.text), (edited.seq, edited.text));
                    let crate::native_app::AppViewState::Editor(state) =
                        app.native_runtime.view_state(reopened).unwrap()
                    else {
                        panic!("editor")
                    };
                    let buffer = state.buffer.as_ref().unwrap();
                    assert_eq!(
                        buffer.selections,
                        vec![
                            crate::native_editor::Selection { anchor: 0, head: 2 },
                            crate::native_editor::Selection { anchor: 0, head: 2 },
                        ]
                    );
                    assert_eq!(buffer.primary, 1);
                    assert_eq!(buffer.viewport_anchor, 2);
                    assert!(app.structural_invariants_ok());
                },
            );
        });
    }
}

#[test]
fn zero_prefix_real_reader_wait_conforms_through_replacement_expiry_death_cancel_and_success() {
    let model = aterm_models_native::closed_reopen_publication_model();
    for fault in 0..5 {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let path = root.join("pending.txt");
        let baseline_path = root.join("baseline.txt");
        std::fs::write(&path, "pending").unwrap();
        std::fs::write(&baseline_path, "baseline").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let baseline_uri = crate::native_document_host::path_to_file_uri(&baseline_path).unwrap();
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(root.join("journals"));
        app.open_document_tab_in_window(WID, AppKind::Editor, &baseline_uri)
            .unwrap();
        let (instance, _) = app.active_native_view(WID).unwrap();
        let id = app.native_runtime.document_id(instance).unwrap();
        let baseline = app.document_store.snapshot(id).unwrap().text;
        push(
            &mut app,
            record(RestoredSplitTree::Split {
                axis: restore::SplitKind::Vertical,
                ratio: 0.4,
                first: Box::new(document(&uri)),
                second: Box::new(document(&uri)),
            }),
        );
        let token = app
            .closed_recovery
            .tabs
            .candidate_snapshot(0)
            .unwrap()
            .token;
        let state = |app: &App, started| project(&model, app, started, id, token, &baseline);
        let step =
            |before: &aterm_spec::interp::State, after: &aterm_spec::interp::State, action| {
                assert!(
                    model.successors(action, before).contains(after),
                    "{action}: {before:?} -> {after:?}"
                );
            };
        let initial = state(&app, false);
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let mut release = Release(Some(release));
        crate::app_documents::with_headless_restore_admission_for_test(|| {
            crate::app_documents::with_restore_read_gate_for_test(
                move || {
                    entered.send(()).unwrap();
                    release_rx.recv_timeout(HANG).unwrap();
                },
                || {
                    app.reopen_last_closed_tab().unwrap();
                    entered_rx.recv_timeout(HANG).unwrap();
                    assert!(app.document_store.id_for_uri(&uri).is_none());
                    let waiting = state(&app, true);
                    assert_eq!(waiting["prefix"], 0);
                    assert_eq!(waiting["custody"], 0);
                    step(&initial, &waiting, "BeginWaiting");
                    app.progress_closed_reopens();
                    step(&waiting, &state(&app, true), "Wait");
                    app.reopen_last_closed_tab().unwrap();
                    step(&waiting, &state(&app, true), "Wait");
                    let mut before = waiting;
                    match fault {
                        1 => {
                            push(&mut app, record(document(&uri)));
                            let replaced = state(&app, true);
                            step(&before, &replaced, "Replace");
                            before = replaced;
                        }
                        2 => {
                            app.lat_epoch = std::time::Instant::now()
                                - std::time::Duration::from_millis(
                                    crate::closed_recovery::CLOSED_TAB_MAX_AGE_MS + 60_000,
                                );
                            let expired = state(&app, true);
                            step(&before, &expired, "Expire");
                            before = expired;
                        }
                        3 => {
                            app.windows.get_mut(&WID).unwrap().pending_close = true;
                            let dead = state(&app, true);
                            step(&before, &dead, "Kill");
                            before = dead;
                        }
                        _ => {}
                    }
                    if fault == 4 {
                        release.now();
                        drive(&mut app, |app| {
                            app.closed_reopens
                                .first()
                                .is_some_and(|pending| pending.build.owned_leaf_count() == 1)
                        });
                        let first = state(&app, true);
                        step(&before, &first, "FirstReady");
                        let mut operation = app.closed_reopens.pop().unwrap();
                        let cold::Step::Complete(built) = operation.build.step(&mut app, WID)
                        else {
                            panic!("shared prepared second leaf");
                        };
                        app.closed_reopens.push(operation);
                        let staged = state(&app, true);
                        step(&first, &staged, "Stage");
                        let operation = app.closed_reopens.pop().unwrap();
                        app.publish_closed_reopen(&operation, built).unwrap();
                        step(&staged, &state(&app, true), "Publish");
                    } else {
                        if fault == 0 {
                            app.cancel_closed_reopen(true).unwrap();
                        } else {
                            app.progress_closed_reopens();
                        }
                        step(&before, &state(&app, true), "Cancel");
                        assert_eq!(
                            app.closed_recovery.tabs.len(),
                            if fault == 1 { 2 } else { 1 }
                        );
                        release.now();
                    }
                },
            );
        });
        assert!(app.closed_reopens.is_empty());
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn failed_reopen_detaches_only_its_borrowed_terminal_edge_and_keeps_the_survivors_hold() {
    let mut app = App::headless_for_test();
    let live = app.windows[&WID].tab_set.active().unwrap().clone();
    let session = app.focused_session_id(WID).unwrap();
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    let descriptor = app.view_restore_descriptor(live.focus).unwrap();
    let before_views = app.view_store.len();
    app.split_regrid_holds
        .arm(session, (24, 80), std::time::Instant::now());
    let hold = app.split_regrid_holds.get(session);
    push(
        &mut app,
        record(RestoredSplitTree::Split {
            axis: restore::SplitKind::Horizontal,
            ratio: 0.4,
            first: Box::new(RestoredSplitTree::leaf(descriptor)),
            second: Box::new(RestoredSplitTree::leaf(RestoredView::Native(
                NativeLeafRestore::settings("/unavailable-route".into()),
            ))),
        }),
    );
    app.reopen_last_closed_tab().unwrap();
    assert_eq!(app.pool.views(session), Some(2), "one borrowed staged edge");
    app.progress_closed_reopens();
    assert!(app.closed_reopens.is_empty());
    assert_eq!(app.closed_recovery.tabs.len(), 1);
    assert_eq!(app.view_store.len(), before_views);
    assert_eq!(app.pool.views(session), Some(1));
    assert!(std::sync::Arc::ptr_eq(
        &ctx,
        &app.pool.get(session).unwrap().ctx
    ));
    assert_eq!(
        app.split_regrid_holds.get(session),
        hold,
        "a failed unpublished view cannot cancel the surviving pane's take-back"
    );
    assert_eq!(app.windows[&WID].tab_set.active(), Some(&live));
    assert!(app.structural_invariants_ok());
}

#[test]
fn publication_applies_saved_offsets_to_the_head_edited_while_a_later_leaf_waited() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("current.txt");
    let later = root.join("later.txt");
    std::fs::write(&path, "abcdef").unwrap();
    std::fs::write(&later, "later").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let later_uri = crate::native_document_host::path_to_file_uri(&later).unwrap();
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(root.join("journals"));
    app.open_document_tab_in_window(WID, AppKind::Editor, &uri)
        .unwrap();
    let (instance, live_view) = app.active_native_view(WID).unwrap();
    let id = app.native_runtime.document_id(instance).unwrap();
    let mut descriptor = NativeLeafRestore::document("editor", uri.clone());
    descriptor.editor_selections = vec![restore::RestoreSelection { anchor: 1, head: 3 }];
    descriptor.viewport_anchor = 6;
    push(
        &mut app,
        record(RestoredSplitTree::Split {
            axis: restore::SplitKind::Vertical,
            ratio: 0.4,
            first: Box::new(RestoredSplitTree::leaf(RestoredView::Native(descriptor))),
            second: Box::new(document(&later_uri)),
        }),
    );
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let mut release = Release(Some(release));
    crate::app_documents::with_headless_restore_admission_for_test(|| {
        crate::app_documents::with_restore_read_gate_for_test(
            move || {
                entered.send(()).unwrap();
                release_rx.recv_timeout(HANG).unwrap();
            },
            || {
                app.reopen_last_closed_tab().unwrap();
                app.progress_closed_reopens();
                entered_rx.recv_timeout(HANG).unwrap();
                let staged = app
                    .view_store
                    .iter()
                    .find_map(|(view, linked)| {
                        (view != live_view
                            && matches!(linked,
                    crate::tab_model::View::Native(native) if native.instance == instance))
                        .then_some(view)
                    })
                    .unwrap();
                let mut view_guard = app.native_runtime.view_state_mut(live_view).unwrap();
                let crate::native_app::AppViewState::Editor(state) = &mut *view_guard else {
                    panic!("live Editor")
                };
                state.buffer.as_mut().unwrap().selections =
                    vec![crate::native_editor::Selection { anchor: 0, head: 6 }];
                drop(view_guard);
                app.dispatch_native_event(
                    WID,
                    AppEvent::TextInput(TextInputEvent::Commit("é".into())),
                )
                .unwrap();
                let edited = app.document_store.snapshot(id).unwrap();
                assert_eq!(edited.text.as_ref(), "é");
                release.now();
                drive(&mut app, |app| app.closed_reopens.is_empty());
                let crate::native_app::AppViewState::Editor(state) =
                    app.native_runtime.view_state(staged).unwrap()
                else {
                    panic!("reopened Editor")
                };
                let buffer = state.buffer.as_ref().unwrap();
                assert_eq!(
                    buffer.selections,
                    vec![crate::native_editor::Selection { anchor: 0, head: 2 }]
                );
                assert_eq!(buffer.viewport_anchor, 2);
                assert_eq!(app.document_store.snapshot(id).unwrap().text, edited.text);
                assert!(app.view_store.get(live_view).is_some());
                assert!(app.structural_invariants_ok());
            },
        );
    });
}

#[test]
fn directory_candidate_publication_refusal_keeps_exact_history_and_owned_prefix() {
    for missing_custody in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("directory");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("entry.txt"), "").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(scratch.path().join("journals"));
        app.open_document_tab_in_window(WID, AppKind::Editor, &uri)
            .unwrap();
        let (instance, original_view) = app.active_native_view(WID).unwrap();
        let id = app.native_runtime.document_id(instance).unwrap();
        let before = app.document_store.snapshot(id).unwrap();
        let original = app.windows[&WID]
            .tab_set
            .tabs()
            .iter()
            .map(|tab| (tab.id, tab.root.clone(), tab.focus))
            .collect::<Vec<_>>();
        let views = app.view_store.len();
        push(
            &mut app,
            record(RestoredSplitTree::Split {
                axis: restore::SplitKind::Horizontal,
                ratio: 0.37,
                first: Box::new(document(&uri)),
                second: Box::new(RestoredSplitTree::Split {
                    axis: restore::SplitKind::Vertical,
                    ratio: 0.61,
                    first: Box::new(document(&uri)),
                    second: Box::new(RestoredSplitTree::leaf(RestoredView::Native(
                        NativeLeafRestore::settings(
                            crate::native_settings::SettingsRoute::About.path().into(),
                        ),
                    ))),
                }),
            }),
        );
        let token = app
            .closed_recovery
            .tabs
            .candidate_snapshot(0)
            .unwrap()
            .token;
        crate::app_documents::with_headless_restore_admission_for_test(|| {
            app.reopen_last_closed_tab().unwrap();
            // Stop at the actual full builder result, before its exclusive publication.
            let mut operation = app.closed_reopens.pop().unwrap();
            let until = std::time::Instant::now() + HANG;
            let built = loop {
                match operation.build.step(&mut app, WID) {
                    cold::Step::Complete(built) => break built,
                    cold::Step::Waiting | cold::Step::Advanced => {}
                    cold::Step::Failed(error) => panic!("directory candidate failed: {error}"),
                }
                app.progress_document_preparations();
                assert!(
                    std::time::Instant::now() < until,
                    "directory candidate did not build"
                );
                aterm_uds::deadline::poll_yield();
            };
            assert_eq!(app.unpublished_attachment_count_for_test(), 2);
            assert_eq!(app.document_store.view_count(id), Some(3));
            app.closed_reopens.push(operation);
            if missing_custody {
                app.with_missing_restore_attachment_for_test(|app| {
                    let operation = app.closed_reopens.pop().unwrap();
                    assert!(app.publish_closed_reopen(&operation, built).is_err());
                    app.closed_reopens.push(operation);
                    assert!(app.cancel_closed_reopen(true).is_err());
                    assert_eq!(app.closed_reopens.len(), 1);
                    assert!(app.closed_recovery.tabs.retains_token_for_test(token));
                });
            } else {
                // The actual topology preflight must refuse a colliding live TabId
                // without spending any document attachment or history custody.
                let mut collided = built;
                collided.0.id = original[0].0;
                let operation = app.closed_reopens.pop().unwrap();
                assert!(app.publish_closed_reopen(&operation, collided).is_err());
                app.closed_reopens.push(operation);
            }
            assert!(app.closed_recovery.tabs.retains_token_for_test(token));
            assert_eq!(app.closed_recovery.tabs.len(), 1);
            assert_eq!(app.unpublished_attachment_count_for_test(), 2);
            assert_eq!(
                app.windows[&WID]
                    .tab_set
                    .tabs()
                    .iter()
                    .map(|tab| (tab.id, tab.root.clone(), tab.focus))
                    .collect::<Vec<_>>(),
                original
            );
            app.cancel_closed_reopen(true).unwrap();
        });
        assert_eq!(app.view_store.len(), views);
        assert_eq!(app.document_store.view_count(id), Some(1));
        assert_eq!(app.unpublished_attachment_count_for_test(), 0);
        assert!(app.view_store.get(original_view).is_some());
        let after = app.document_store.snapshot(id).unwrap();
        assert_eq!(after.seq, before.seq);
        assert!(std::sync::Arc::ptr_eq(&after.text, &before.text));
        assert!(app.closed_recovery.tabs.retains_token_for_test(token));
        assert!(app.structural_invariants_ok());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn absent_document_closed_view_holds_real_read_and_journal_then_grafts_or_falls_back() {
    for fallback in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let path = root.join("closed-view.txt");
        std::fs::write(&path, "Closed view\n").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(root.join("journals"));
        let host = app.windows[&WID].tab_set.active().unwrap().clone();
        assert!(app.seed_closed_reopen_fixture(uri.clone(), true));
        let candidate = app.closed_recovery.views.candidate_snapshot(0).unwrap();
        assert_eq!(candidate.value.original_tab, host.id);
        let (read, read_rx) = mpsc::channel();
        let (release_read, release_read_rx) = mpsc::channel();
        let mut release_read = Release(Some(release_read));
        let (journal, journal_rx) = mpsc::channel();
        let (release_journal, release_journal_rx) = mpsc::channel();
        let mut release_journal = Release(Some(release_journal));
        crate::app_documents::set_initialize_gate_for_test(move || {
            journal.send(()).unwrap();
            release_journal_rx.recv_timeout(HANG).unwrap();
        });
        crate::app_documents::with_headless_restore_admission_for_test(|| {
            crate::app_documents::with_restore_read_gate_for_test(
                move || {
                    read.send(()).unwrap();
                    release_read_rx.recv_timeout(HANG).unwrap();
                },
                || {
                    app.reopen_last_closed_view().unwrap();
                    read_rx.recv_timeout(HANG).unwrap();
                    app.reopen_last_closed_view().unwrap();
                    assert_eq!(app.closed_reopens.len(), 1);
                    assert!(app.document_store.id_for_uri(&uri).is_none());
                    assert_eq!(app.unpublished_attachment_count_for_test(), 0);
                    assert!(
                        app.closed_recovery
                            .views
                            .matches_candidate(candidate.token, 0)
                    );
                    if fallback {
                        app.split_active_stub_tab_dir(WID, crate::pane::SplitDir::Vertical);
                    }
                    let current = app.windows[&WID].tab_set.get(host.id).unwrap().root.clone();
                    let before = app.view_store.len();
                    release_read.now();
                    drive(&mut app, |_| journal_rx.try_recv().is_ok());
                    assert_eq!(app.view_store.len(), before);
                    assert_eq!(app.unpublished_attachment_count_for_test(), 0);
                    assert!(
                        app.closed_recovery
                            .views
                            .matches_candidate(candidate.token, 0)
                    );
                    assert_eq!(
                        app.windows[&WID].tab_set.get(host.id).unwrap().root,
                        current
                    );
                    release_journal.now();
                    drive(&mut app, |app| app.closed_reopens.is_empty());
                    assert!(
                        !app.closed_recovery
                            .views
                            .retains_token_for_test(candidate.token)
                    );
                    assert_eq!(app.unpublished_attachment_count_for_test(), 0);
                    assert_eq!(app.view_store.len(), before + 1);
                    let id = app.document_store.id_for_uri(&uri).unwrap();
                    assert_eq!(app.document_store.view_count(id), Some(1));
                    let opened = app.windows[&WID].tab_set.active().unwrap();
                    if fallback {
                        assert_ne!(opened.id, host.id);
                        assert_eq!(opened.root.len(), 1);
                        assert_eq!(
                            app.windows[&WID].tab_set.get(host.id).unwrap().root,
                            current
                        );
                    } else {
                        assert_eq!(opened.id, host.id);
                        assert!(opened.root.contains(host.focus));
                        assert_eq!(opened.root.len(), 2);
                    }
                    assert!(app.structural_invariants_ok());
                    assert!(
                        !app.seed_closed_reopen_fixture(uri.clone(), true),
                        "fixture cannot hide a live canonical document to force admission"
                    );
                },
            );
        });
    }
}
