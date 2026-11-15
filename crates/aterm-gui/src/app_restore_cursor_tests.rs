// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cold restore advances by leaf while its unpublished work remains owned and capturable.

use std::ops::ControlFlow;
use std::time::Duration;

use super::{App, RestoreDrain, WindowId, cold, restore};
use crate::tab_model::{SplitAxis, SplitTree, View, ViewId};

fn terminal(title: &str) -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
        restore::TerminalLeafRestore {
            sid: None,
            lineage: None,
            log_ep: None,
            cwd: Some("/tmp".into()),
            title: title.into(),
            profile: None,
            local_id: None,
            user_title: Some(title.into()),
            description: None,
            icon: None,
            role: None,
            attention: None,
            questions: None,
            autopilot: None,
            land_at: None,
            autopilot_by: None,
            theme: None,
            agent: None,
            supervised: None,
            lost: None,
            held: false,
            pinned: false,
            holds: Vec::new(),
            claim_ended: false,
            not_claim_worker: false,
            claim_worker: None,
            task: None,
        },
    ))
}

fn about() -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::leaf(restore::RestoredView::Native(
        restore::NativeLeafRestore::about(),
    ))
}

fn tree(mut leaves: Vec<restore::RestoredSplitTree>) -> restore::RestoredSplitTree {
    assert!(!leaves.is_empty());
    if leaves.len() == 1 {
        return leaves.pop().unwrap();
    }
    let right = leaves.split_off(leaves.len() / 2);
    restore::RestoredSplitTree::Split {
        axis: restore::SplitKind::Horizontal,
        ratio: 0.375,
        first: Box::new(tree(leaves)),
        second: Box::new(tree(right)),
    }
}

fn record(root: restore::RestoredSplitTree) -> restore::RestoredTab {
    let mut focused_path = Vec::new();
    let mut cursor = &root;
    while let restore::RestoredSplitTree::Split { second, .. } = cursor {
        focused_path.push(restore::RestoreBranch::Second);
        cursor = second;
    }
    restore::RestoredTab {
        root,
        focused_path,
        zoomed: false,
        parked: false,
        ephemeral: false,
        fleet: false,
    }
}

fn window(tabs: Vec<restore::RestoredTab>) -> restore::WindowLayout {
    restore::WindowLayout {
        rows: 24,
        cols: 80,
        active_tab: 0,
        outer_x: None,
        outer_y: None,
        maximized: None,
        inner_px: None,
        presence_row: false,
        show: restore::WindowShow::UNKNOWN,
        tabs: Vec::new(),
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: Some(0),
        workers_in_front: false,
        restored_tabs: tabs,
    }
}

fn views(app: &App) -> Vec<ViewId> {
    let mut views: Vec<_> = app.view_store.iter().map(|(id, _)| id).collect();
    views.sort();
    views
}

fn sessions(app: &App) -> Vec<u64> {
    let mut sessions: Vec<_> = app.pool.iter().map(|session| session.id).collect();
    sessions.sort_unstable();
    sessions
}

fn terminal_names(app: &App, root: &SplitTree<ViewId>) -> Vec<String> {
    let mut names = Vec::new();
    root.visit(&mut |view| {
        if let Some(session) = app
            .view_store
            .get(*view)
            .copied()
            .and_then(View::terminal_session)
        {
            names.push(
                app.pool
                    .get(session)
                    .unwrap()
                    .ctx
                    .meta
                    .lock()
                    .unwrap()
                    .user_title
                    .clone()
                    .unwrap(),
            );
        }
    });
    names
}

fn assert_shape(source: &restore::RestoredSplitTree, built: &SplitTree<ViewId>) {
    match (source, built) {
        (restore::RestoredSplitTree::Leaf { .. }, SplitTree::Leaf(_)) => {}
        (
            restore::RestoredSplitTree::Split {
                axis,
                ratio,
                first,
                second,
            },
            SplitTree::Split {
                axis: actual_axis,
                ratio: actual_ratio,
                first: actual_first,
                second: actual_second,
            },
        ) => {
            assert_eq!(*axis, restore::SplitKind::Horizontal);
            assert_eq!(*actual_axis, SplitAxis::Horizontal);
            assert_eq!(ratio, actual_ratio);
            assert_shape(first, actual_first);
            assert_shape(second, actual_second);
        }
        other => panic!("restore changed a split or a leaf: {other:?}"),
    }
}

fn native_slice(app: &mut App) -> bool {
    app.drain_restore_slice(
        Duration::ZERO,
        |app, _, source, _, filling| {
            *filling = app.begin_native_sliced_fill(source);
            ControlFlow::Continue(())
        },
        |_, _| true,
    )
}

fn bootstrap_slice(app: &mut App) -> bool {
    app.drain_restore_slice(
        Duration::ZERO,
        |app, first, source, placed, filling| {
            assert!(first, "this fixture has one restored window");
            app.restore_bootstrap_window(source, placed, Some(filling));
            ControlFlow::Continue(())
        },
        |_, _| true,
    )
}

fn carried_terminals(manifest: &restore::RestoreManifest) -> Vec<restore::TerminalLeafRestore> {
    fn walk(root: &restore::RestoredSplitTree, found: &mut Vec<restore::TerminalLeafRestore>) {
        match root {
            restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(terminal),
            } => found.push(terminal.clone()),
            restore::RestoredSplitTree::Leaf { .. } => {}
            restore::RestoredSplitTree::Split { first, second, .. } => {
                walk(first, found);
                walk(second, found);
            }
        }
    }
    let mut found = Vec::new();
    for window in &manifest.windows {
        for tab in &window.restored_tabs {
            walk(&tab.root, &mut found);
        }
    }
    found
}

#[test]
fn a_large_mixed_tab_allocates_exactly_one_leaf_per_step_before_publication() {
    const LEAVES: usize = 32;
    let source = record(tree(
        (0..LEAVES)
            .map(|index| {
                if index % 3 == 0 {
                    about()
                } else {
                    terminal(&format!("shell-{index}"))
                }
            })
            .collect(),
    ));
    let mut app = App::headless_for_test();
    let initial_views = app.view_store.len();
    let initial_sessions = sessions(&app).len();
    let original_tab = app.windows[&WindowId(0)].tab_set.active().unwrap().clone();
    let mut building = cold::BuildingTab::new(0, source.clone(), None, None);
    let mut terminals = 0;
    for index in 0..LEAVES {
        terminals += usize::from(index % 3 != 0);
        let step = building.step(&mut app, WindowId(0));
        assert_eq!(
            app.view_store.len(),
            initial_views + index + 1,
            "step {index} must allocate one leaf"
        );
        assert_eq!(sessions(&app).len(), initial_sessions + terminals);
        assert_eq!(
            app.windows[&WindowId(0)].tab_set.tabs(),
            std::slice::from_ref(&original_tab)
        );
        assert!(
            app.restore_leaf_staging.is_none(),
            "a slice never leaves an active install context"
        );
        match step {
            cold::Step::Advanced if index + 1 < LEAVES => {}
            cold::Step::Complete((tab, projection)) if index + 1 == LEAVES => {
                assert_eq!(tab.root.len(), LEAVES);
                assert_shape(&source.root, &tab.root);
                let mut leaves = Vec::new();
                tab.root.visit(&mut |view| leaves.push(*view));
                assert_eq!(
                    tab.focus,
                    *leaves.last().unwrap(),
                    "the saved focus names the last leaf"
                );
                assert!(
                    projection.is_none(),
                    "a mixed tab has no all-terminal projection"
                );
                assert_eq!(
                    terminal_names(&app, &tab.root),
                    (0..LEAVES)
                        .filter(|index| index % 3 != 0)
                        .map(|index| format!("shell-{index}"))
                        .collect::<Vec<_>>()
                );
            }
            cold::Step::Failed(error) => panic!("step {index}: {error}"),
            _ => panic!("step {index} did not stop after its one leaf"),
        }
    }
    building.cancel(&mut app);
    assert_eq!(app.view_store.len(), initial_views);
    assert_eq!(sessions(&app).len(), initial_sessions);
}

#[test]
fn a_legacy_active_terminal_index_is_normalized_before_the_shared_builder_runs() {
    let mut app = App::headless_for_test();
    let mut legacy = window(Vec::new());
    legacy.active_item = None;
    legacy.active_tab = 1;
    legacy.tabs = vec![
        restore::PaneLayout::leaf(Some("/tmp".into()), "first".into(), true),
        restore::PaneLayout::leaf(Some("/tmp".into()), "second".into(), true),
    ];
    app.pending_restore = Some(restore::RestoreManifest::new(vec![legacy]));
    while !bootstrap_slice(&mut app) {}
    let active = app.windows[&WindowId(0)].tab_set.active().unwrap();
    let session = app
        .view_store
        .get(active.focus)
        .copied()
        .and_then(View::terminal_session)
        .unwrap();
    assert_eq!(
        session, 1,
        "legacy active_tab=1 selects the second reconstructed shell"
    );
}

#[cfg(unix)]
#[test]
fn an_editor_first_restore_keeps_its_later_bootstraps_input_and_current_state() {
    use std::io::Read;
    use std::sync::{Arc, mpsc};

    use crate::session_timeline::{MetaEdit, MetaField};

    struct Release(Option<mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    for mixed in [false, true] {
        for select_new_tab in [false, true] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let root = scratch.path().canonicalize().unwrap();
            let path = root.join("first.txt");
            std::fs::write(&path, "saved editor\n").unwrap();
            let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
            let editor = record(restore::RestoredSplitTree::leaf(
                restore::RestoredView::Native(restore::NativeLeafRestore::document(
                    "editor",
                    uri.clone(),
                )),
            ));
            let later = record(if mixed {
                tree(vec![about(), terminal("saved terminal")])
            } else {
                terminal("saved terminal")
            });
            let (writer, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
            reader
                .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .unwrap();
            let sink = Arc::new(aterm_session::sink::SinkWriter::new_owned(writer.into()));
            let mut app = App::headless_for_test_with_sink(sink);
            app.use_document_journal_root_for_test(root.join("drafts"));
            let wid = WindowId(0);
            let original = app.windows[&wid].tab_set.active().unwrap().clone();
            let bootstrap = app.focused_session_id(wid).unwrap();
            let ctx = Arc::clone(&app.pool.get(bootstrap).unwrap().ctx);
            app.pending_restore = Some(restore::RestoreManifest::new(vec![window(vec![
                editor,
                later.clone(),
            ])]));
            let (entered, entered_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            let release = Release(Some(release));
            crate::app_documents::with_headless_restore_admission_for_test(|| {
                crate::app_documents::with_restore_read_gate_for_test(
                    move || {
                        entered.send(()).unwrap();
                        release_rx
                            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                            .unwrap();
                    },
                    || {
                        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                        loop {
                            if let Some(drain) = app.restore_drain.as_mut() {
                                drain.document_ready();
                            }
                            assert!(!bootstrap_slice(&mut app));
                            if entered_rx.try_recv().is_ok() {
                                break;
                            }
                            assert!(std::time::Instant::now() < deadline);
                            aterm_uds::deadline::poll_yield();
                        }
                        let filling = app
                            .restore_drain
                            .as_ref()
                            .unwrap()
                            .filling
                            .as_ref()
                            .unwrap();
                        assert_eq!(
                            filling.building.as_ref().unwrap().index,
                            0,
                            "the blocked reader belongs to the first saved Editor"
                        );
                        assert_eq!(
                            filling.placed,
                            [(1, original.id)],
                            "the later terminal-bearing tab owns the existing bootstrap"
                        );
                        assert!(app.document_store.id_for_uri(&uri).is_none());
                        assert_eq!(app.windows[&wid].tab_set.active_id(), Some(original.id));
                        assert_eq!(app.focused_session_id(wid), Some(bootstrap));
                        let tab = app.windows[&wid].tab_set.get(original.id).unwrap();
                        assert_shape(&later.root, &tab.root);

                        for (text, title, hold) in [
                            ("first input", "changed once", "local first"),
                            ("second input", "changed twice", "local second"),
                        ] {
                            app.on_ime_commit(wid, text.into());
                            let mut received = vec![0; text.len()];
                            reader.read_exact(&mut received).unwrap();
                            assert_eq!(received, text.as_bytes(), "visible input reaches its sink");
                            crate::feed_session_for_test(&app, bootstrap, text.as_bytes());
                            crate::feed_session_for_test(&app, bootstrap, b"\r\n");
                            assert!(
                                crate::session_timeline::write_session_meta(
                                    &ctx,
                                    MetaField::Title,
                                    MetaEdit::Set(title),
                                    aterm_slog::Actor::Ctl,
                                )
                                .unwrap()
                            );
                            assert!(crate::fabric::apply_hold_for_test(
                                ctx.as_ref(),
                                Some(crate::fabric::parse_hold(hold)),
                            ));
                            let before = views(&app);
                            let mut captured = app.capture_restore_manifest();
                            app.quit_agents(&mut captured, true);
                            let captured = &captured.windows[0];
                            assert_eq!(captured.restored_tabs.len(), 2);
                            assert_eq!(captured.active_item, Some(1));
                            assert!(matches!(
                                &captured.restored_tabs[0].root,
                                restore::RestoredSplitTree::Leaf {
                                    view: restore::RestoredView::Native(native)
                                } if native.uri.as_deref() == Some(uri.as_str())
                            ));
                            assert_shape(
                                &later.root,
                                &app.windows[&wid].tab_set.get(original.id).unwrap().root,
                            );
                            let current = captured.restored_tabs[1]
                                .root
                                .first_terminal_leaf()
                                .unwrap();
                            assert_eq!(current.user_title.as_deref(), Some(title));
                            assert_eq!(current.holds, [hold]);
                            assert_eq!(views(&app), before, "capture consumes no pending work");
                            assert!(app.restore_drain.as_ref().unwrap().waiting_document());
                        }
                        let mut canceled_quit = app.capture_restore_manifest();
                        app.quit_agents(&mut canceled_quit, false);
                        assert_eq!(app.desk_holds(bootstrap), ["local second"]);
                        let selected = if select_new_tab {
                            let session = app.next_session_id;
                            app.push_stub_tab(wid, crate::stub_session(session));
                            app.windows[&wid].tab_set.active_id()
                        } else {
                            None
                        };
                        drop(release);
                        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                        loop {
                            if let Some(drain) = app.restore_drain.as_mut() {
                                drain.document_ready();
                            }
                            if bootstrap_slice(&mut app) {
                                break;
                            }
                            assert!(std::time::Instant::now() < deadline);
                            aterm_uds::deadline::poll_yield();
                        }
                        let tabs = app.windows[&wid].tab_set.tabs();
                        assert_eq!(tabs.len(), if select_new_tab { 3 } else { 2 });
                        assert_eq!(tabs[1].id, original.id, "saved order is restored");
                        let mut restored_sessions = Vec::new();
                        tabs[1].root.visit(&mut |view| {
                            if let Some(session) = app
                                .view_store
                                .get(*view)
                                .copied()
                                .and_then(View::terminal_session)
                            {
                                restored_sessions.push(session);
                            }
                        });
                        assert_eq!(restored_sessions, [bootstrap]);
                        assert!(matches!(
                            app.view_store.get(tabs[0].focus),
                            Some(View::Native(_))
                        ));
                        assert_eq!(
                            app.windows[&wid].tab_set.active_id(),
                            selected.or(Some(tabs[0].id)),
                            "saved active intent yields to a later user selection"
                        );
                        let live = app
                            .pool
                            .get(bootstrap)
                            .expect("the original bootstrap survives");
                        assert!(Arc::ptr_eq(&ctx, &live.ctx));
                        assert_eq!(
                            live.ctx.meta.lock().unwrap().user_title.as_deref(),
                            Some("changed twice")
                        );
                        assert_eq!(app.desk_holds(bootstrap), ["local second"]);
                        let term = crate::term_lock(&live.term);
                        assert_eq!(term.row_text(0).as_deref(), Some("first input"));
                        assert_eq!(term.row_text(1).as_deref(), Some("second input"));
                        drop(term);
                        assert!(app.structural_invariants_ok());
                    },
                );
            });
        }
    }
}

#[cfg(unix)]
#[test]
fn an_all_native_restore_keeps_only_a_bootstrap_the_person_used() {
    use std::io::Read;
    use std::sync::{Arc, mpsc};

    use crate::session_timeline::{MetaEdit, MetaField};

    struct Release(Option<mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    // No input, input before the first restore slice, input while the actual reader is
    // blocked, and later explicit selection with and without input into the bootstrap.
    for scenario in 0..7 {
        let posted = scenario >= 5;
        let used = (1..4).contains(&scenario) || posted;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let path = root.join("native.txt");
        std::fs::write(&path, "saved editor\n").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let saved = vec![
            record(restore::RestoredSplitTree::leaf(
                restore::RestoredView::Native(restore::NativeLeafRestore::document(
                    "editor",
                    uri.clone(),
                )),
            )),
            record(about()),
        ];
        let (writer, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let sink = Arc::new(aterm_session::sink::SinkWriter::new_owned(writer.into()));
        let mut app = App::headless_for_test_with_sink(sink);
        app.use_document_journal_root_for_test(root.join("drafts"));
        let wid = WindowId(0);
        let original = app.windows[&wid].tab_set.active().unwrap().clone();
        let bootstrap = app.focused_session_id(wid).unwrap();
        let ctx = Arc::clone(&app.pool.get(bootstrap).unwrap().ctx);
        crate::feed_session_for_test(&app, bootstrap, b"\x1b]0;startup title\x07prompt> ");
        assert_eq!(ctx.human_input.seq(), 0);
        assert!(!ctx.typed.driver_admitted());
        let type_text = |app: &mut App, reader: &mut std::os::unix::net::UnixStream, text: &str| {
            if posted {
                app.dispatch_ime_event(
                    wid,
                    aterm_window::event::Ime::Commit(text.into()),
                    aterm_window::event::InputOrigin::Posted,
                );
            } else {
                app.on_ime_commit(wid, text.into());
            }
            let mut received = vec![0; text.len()];
            reader.read_exact(&mut received).unwrap();
            assert_eq!(received, text.as_bytes());
            crate::feed_session_for_test(app, bootstrap, text.as_bytes());
        };
        if scenario == 1 || scenario == 5 {
            type_text(&mut app, &mut reader, "before begin");
        }
        app.pending_restore = Some(restore::RestoreManifest::new(vec![window(saved.clone())]));
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release = Release(Some(release));
        crate::app_documents::with_headless_restore_admission_for_test(|| {
            crate::app_documents::with_restore_read_gate_for_test(
                move || {
                    entered.send(()).unwrap();
                    release_rx
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .unwrap();
                },
                || {
                    assert!(!bootstrap_slice(&mut app));
                    entered_rx
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .unwrap();
                    assert!(app.document_store.id_for_uri(&uri).is_none());
                    assert_eq!(
                        app.windows[&wid].tab_set.tabs(),
                        std::slice::from_ref(&original)
                    );
                    if scenario == 2 || scenario == 3 || scenario == 6 {
                        type_text(&mut app, &mut reader, "during read");
                    }
                    if used {
                        assert_eq!(ctx.human_input.seq() > 0, !posted);
                        if posted {
                            assert!(!ctx.typed.driver_admitted());
                            assert!(ctx.output_echo.retains_input());
                        }
                    }
                    assert!(app.desk_holds(bootstrap).is_empty());
                    assert!(!app.pool.theme_decided(bootstrap));
                    let input_only = app.capture_restore_manifest();
                    assert_eq!(
                        carried_terminals(&input_only).len(),
                        usize::from(used),
                        "input alone preserves the bootstrap before metadata or holds change"
                    );
                    assert_eq!(
                        input_only.windows[0].restored_tabs.len(),
                        2 + usize::from(used)
                    );
                    for revision in 0..2 {
                        if used {
                            let title = if revision == 0 {
                                "first title"
                            } else {
                                "latest title"
                            };
                            crate::session_timeline::write_session_meta(
                                &ctx,
                                MetaField::Title,
                                MetaEdit::Set(title),
                                aterm_slog::Actor::Ctl,
                            )
                            .unwrap();
                            let hold = if revision == 0 {
                                "local first"
                            } else {
                                "local latest"
                            };
                            assert!(crate::fabric::apply_hold_for_test(
                                &ctx,
                                Some(crate::fabric::parse_hold(hold)),
                            ));
                        }
                        let before = views(&app);
                        let mut captured = app.capture_restore_manifest();
                        app.quit_agents(&mut captured, true);
                        let window = &captured.windows[0];
                        assert_eq!(window.restored_tabs.len(), if used { 3 } else { 2 });
                        let offset = usize::from(used);
                        assert_eq!(window.restored_tabs[offset..], saved);
                        assert_eq!(window.active_item, Some(0));
                        let terminals = carried_terminals(&captured);
                        assert_eq!(terminals.len(), usize::from(used));
                        if used {
                            assert_eq!(
                                terminals[0].user_title.as_deref(),
                                Some(if revision == 0 {
                                    "first title"
                                } else {
                                    "latest title"
                                })
                            );
                            assert_eq!(
                                terminals[0].holds,
                                [if revision == 0 {
                                    "local first"
                                } else {
                                    "local latest"
                                }]
                            );
                        }
                        assert_eq!(
                            views(&app),
                            before,
                            "repeated capture consumes no ownership"
                        );
                        assert!(app.restore_drain.as_ref().unwrap().waiting_document());
                    }
                    let selected = if scenario == 3 || scenario == 4 {
                        let session = app.next_session_id;
                        app.push_stub_tab(wid, crate::stub_session(session));
                        app.windows[&wid].tab_set.active_id()
                    } else {
                        None
                    };
                    for _ in 0..2 {
                        let before = views(&app);
                        let captured = app.capture_restore_manifest();
                        assert_eq!(
                            captured.windows[0].restored_tabs.len(),
                            2 + usize::from(used) + usize::from(selected.is_some())
                        );
                        assert_eq!(
                            carried_terminals(&captured).len(),
                            usize::from(used) + usize::from(selected.is_some())
                        );
                        if selected.is_some() {
                            assert_eq!(captured.windows[0].active_item, Some(1));
                        }
                        assert_eq!(views(&app), before);
                    }
                    let mut cancelled_quit = app.capture_restore_manifest();
                    app.quit_agents(&mut cancelled_quit, false);
                    if used {
                        assert_eq!(app.desk_holds(bootstrap), ["local latest"]);
                    }
                    drop(release);
                    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                    loop {
                        if let Some(drain) = app.restore_drain.as_mut() {
                            drain.document_ready();
                        }
                        if bootstrap_slice(&mut app) {
                            break;
                        }
                        assert!(std::time::Instant::now() < deadline);
                        aterm_uds::deadline::poll_yield();
                    }
                    let tabs = app.windows[&wid].tab_set.tabs();
                    assert_eq!(
                        tabs.len(),
                        2 + usize::from(used) + usize::from(selected.is_some())
                    );
                    let native: Vec<_> = tabs
                        .iter()
                        .filter(|tab| {
                            matches!(app.view_store.get(tab.focus), Some(View::Native(_)))
                        })
                        .collect();
                    assert_eq!(native.len(), 2);
                    assert!(
                        matches!(app.view_restore_descriptor(native[0].focus).unwrap(), restore::RestoredView::Native(ref descriptor) if descriptor.restore_tag == "editor" && descriptor.uri.as_deref() == Some(uri.as_str()))
                    );
                    assert_eq!(
                        app.view_restore_descriptor(native[1].focus).unwrap(),
                        restore::RestoredView::Native(restore::NativeLeafRestore::about())
                    );
                    if used {
                        assert_eq!(tabs.iter().filter(|tab| tab.id == original.id).count(), 1);
                        assert_eq!(
                            app.windows[&wid].tab_set.active_id(),
                            selected.or(Some(original.id)),
                            "publishing native tabs must not choose the preserved bootstrap over a later user selection"
                        );
                        let live = app.pool.get(bootstrap).unwrap();
                        assert!(Arc::ptr_eq(&ctx, &live.ctx));
                        assert_eq!(
                            live.ctx.meta.lock().unwrap().user_title.as_deref(),
                            Some("latest title")
                        );
                        assert_eq!(app.desk_holds(bootstrap), ["local latest"]);
                        let line = crate::term_lock(&live.term).row_text(0).unwrap();
                        assert!(line.contains(if scenario == 1 || scenario == 5 {
                            "before begin"
                        } else {
                            "during read"
                        }));
                    } else {
                        assert!(app.pool.get(bootstrap).is_none());
                        assert!(app.view_store.get(original.focus).is_none());
                        assert_eq!(
                            tabs[0].id, original.id,
                            "the untouched anchor becomes the first native tab"
                        );
                        assert_eq!(
                            app.windows[&wid].tab_set.active_id(),
                            selected.or(Some(tabs[0].id))
                        );
                    }
                    let captured = app.capture_restore_manifest();
                    assert_eq!(
                        carried_terminals(&captured).len(),
                        usize::from(used) + usize::from(selected.is_some())
                    );
                    assert!(app.structural_invariants_ok());
                },
            );
        });
    }
}

#[cfg(unix)]
#[test]
fn a_native_restore_retains_the_bootstrap_while_posted_input_still_owns_its_fifo_job() {
    use std::io::Read as _;
    use std::sync::{Arc, mpsc};
    struct Release(Option<mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let (writer, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    let sink = Arc::new(aterm_session::sink::SinkWriter::new_owned(writer.into()));
    let mut app = App::headless_for_test_with_sink(sink);
    let wid = WindowId(0);
    let original = app.windows[&wid].tab_set.active().unwrap().clone();
    let session = app.focused_session_id(wid).unwrap();
    let ctx = Arc::clone(&app.pool.get(session).unwrap().ctx);
    let _queue = crate::app_input::paste_order::force_conpty_route_for_test();
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let release = Release(Some(release));
    crate::app_input::paste_order::with_write_gate_for_test(
        move || {
            entered.send(()).unwrap();
            release_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
        },
        || {
            app.dispatch_ime_event(
                wid,
                aterm_window::event::Ime::Commit("queued".into()),
                aterm_window::event::InputOrigin::Posted,
            );
            entered_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
            assert_eq!(ctx.human_input.seq(), 0);
            assert!(!ctx.typed.driver_admitted());
            app.pending_restore = Some(restore::RestoreManifest::new(vec![window(vec![
                record(about()),
                record(about()),
            ])]));
            let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while !bootstrap_slice(&mut app) {
                assert!(std::time::Instant::now() < deadline);
                let captured = app.capture_restore_manifest();
                assert_eq!(carried_terminals(&captured).len(), 1);
            }
            assert_eq!(app.windows[&wid].tab_set.len(), 3);
            assert_eq!(app.windows[&wid].tab_set.active_id(), Some(original.id));
            assert!(Arc::ptr_eq(&ctx, &app.pool.get(session).unwrap().ctx));
            assert_eq!(carried_terminals(&app.capture_restore_manifest()).len(), 1);
            drop(release);
            let mut bytes = [0; 6];
            reader.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"queued");
            crate::app_input::paste_order::await_drain_for_test(&ctx.sink);
            assert!(ctx.output_echo.retains_input());
            assert!(app.structural_invariants_ok());
        },
    );
}

#[test]
fn native_bootstrap_preservation_reads_explicit_provenance_not_output_or_inherited_theme() {
    use crate::session_theme::{SessionTheme, SessionThemeOrigin};
    use crate::session_timeline::{MetaEdit, MetaField};

    for change in 0..7 {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let original = app.windows[&wid].tab_set.active().unwrap().clone();
        let session = app.focused_session_id(wid).unwrap();
        let ctx = std::sync::Arc::clone(&app.pool.get(session).unwrap().ctx);
        let building = cold::BuildingTab::new(
            0,
            record(about()),
            Some(cold::Anchor {
                tab: original.id,
                view: original.focus,
                original,
            }),
            Some((app.windows[&wid].tab_set.active().unwrap().focus, session)),
        );
        crate::feed_session_for_test(&app, session, b"\x1b]0;prompt title\x07hello");
        app.set_session_theme(
            session,
            Some(SessionTheme {
                selector: "Linen".into(),
                origin: SessionThemeOrigin::Split,
            }),
        );
        assert!(!building.preserves_bootstrap(&app, wid));
        match change {
            0 => {
                let caller =
                    crate::typed_by::Caller::new(crate::typed_by::Who::Word("driver".into()));
                ctx.typed.admit(&caller);
            }
            1 | 6 => {
                crate::session_timeline::write_session_meta(
                    &ctx,
                    MetaField::Title,
                    MetaEdit::Set("explicit"),
                    if change == 1 {
                        aterm_slog::Actor::Ctl
                    } else {
                        aterm_slog::Actor::Human
                    },
                )
                .unwrap();
            }
            2 => {
                assert!(crate::fabric::apply_hold_for_test(
                    &ctx,
                    Some(crate::fabric::parse_hold("local kept"))
                ));
            }
            3 => {
                app.set_session_theme_checked(session, "Linen").unwrap();
            }
            4 => {
                app.split_active_stub_tab(wid);
            }
            5 => app.clear_front_tab_theme(),
            _ => unreachable!(),
        }
        assert!(building.preserves_bootstrap(&app, wid));
        assert!(
            building.preserves_bootstrap(&app, wid),
            "the predicate is non-consuming"
        );
        building.cancel(&mut app);
        assert!(app.pool.get(session).is_some());
    }
}

#[test]
fn a_native_host_stays_off_the_window_map_until_its_first_tab_is_complete() {
    let mut app = App::headless_for_test();
    let initial_windows = app.windows.len();
    let initial_views = app.view_store.len();
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(vec![record(
        tree(vec![about(), about(), about()]),
    )])]));
    assert!(!native_slice(&mut app));
    let wid = app
        .restore_drain
        .as_ref()
        .unwrap()
        .filling_window()
        .unwrap();
    assert!(!app.windows.contains_key(&wid));
    assert_eq!(
        app.view_store.len(),
        initial_views,
        "beginning a host builds no leaf"
    );
    for built in 1..=3 {
        assert!(!native_slice(&mut app));
        assert_eq!(app.view_store.len(), initial_views + built);
        if built < 3 {
            assert_eq!(app.windows.len(), initial_windows);
            assert!(
                !app.windows.contains_key(&wid),
                "a partial native tab is not published"
            );
        }
    }
    assert_eq!(app.windows.len(), initial_windows + 1);
    assert_eq!(app.windows[&wid].tab_set.len(), 1);
    assert_eq!(app.windows[&wid].tab_set.active().unwrap().root.len(), 3);
    assert!(
        app.structural_invariants_ok(),
        "{}",
        app.structural_invariant_violation().unwrap_or_default()
    );
}

#[test]
fn cancellation_retires_fresh_leaves_but_keeps_the_borrowed_bootstrap() {
    for borrow_bootstrap in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let original = app.windows[&wid].tab_set.active().unwrap().clone();
        let before_views = views(&app);
        let before_sessions = sessions(&app);
        let session = app
            .view_store
            .get(original.focus)
            .copied()
            .and_then(View::terminal_session)
            .unwrap();
        let anchor = borrow_bootstrap.then(|| cold::Anchor {
            tab: original.id,
            view: original.focus,
            original: original.clone(),
        });
        let reusable = borrow_bootstrap.then_some((original.focus, session));
        let mut building = cold::BuildingTab::new(
            0,
            record(tree(vec![
                terminal("first"),
                about(),
                terminal("unstarted"),
            ])),
            anchor,
            reusable,
        );
        assert!(matches!(building.step(&mut app, wid), cold::Step::Advanced));
        assert!(matches!(building.step(&mut app, wid), cold::Step::Advanced));
        let new_views: Vec<_> = views(&app)
            .into_iter()
            .filter(|view| !before_views.contains(view))
            .collect();
        assert_eq!(new_views.len(), if borrow_bootstrap { 1 } else { 2 });
        let native = new_views
            .iter()
            .find_map(|view| match app.view_store.get(*view) {
                Some(View::Native(native)) => Some(native.instance),
                _ => None,
            })
            .unwrap();
        assert!(app.native_runtime.app(native).is_some());
        building.cancel(&mut app);
        assert_eq!(views(&app), before_views);
        assert_eq!(sessions(&app), before_sessions);
        assert!(
            app.native_runtime.app(native).is_none(),
            "the unpublished native instance has no remaining owner"
        );
        assert_eq!(
            app.windows[&wid].tab_set.active().unwrap().root,
            original.root
        );
        assert_eq!(
            app.windows[&wid].tab_set.active().unwrap().focus,
            original.focus
        );
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn allocation_failure_after_a_completed_prefix_rolls_back_only_owned_leaves() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let original = app.windows[&wid].tab_set.active().unwrap().clone();
    let session = app
        .view_store
        .get(original.focus)
        .copied()
        .and_then(View::terminal_session)
        .unwrap();
    let before_views = views(&app);
    let before_sessions = sessions(&app);
    let anchor = cold::Anchor {
        tab: original.id,
        view: original.focus,
        original: original.clone(),
    };
    let mut building = cold::BuildingTab::new(
        0,
        record(tree(vec![
            terminal("borrowed"),
            terminal("new shell"),
            about(),
        ])),
        Some(anchor),
        Some((original.focus, session)),
    );
    assert!(matches!(building.step(&mut app, wid), cold::Step::Advanced));
    assert!(matches!(building.step(&mut app, wid), cold::Step::Advanced));
    assert_eq!(sessions(&app).len(), before_sessions.len() + 1);
    app.tab_ids.exhaust_for_test();
    assert!(matches!(
        building.step(&mut app, wid),
        cold::Step::Failed(_)
    ));
    building.cancel(&mut app);
    assert_eq!(views(&app), before_views);
    assert_eq!(sessions(&app), before_sessions);
    assert_eq!(
        app.windows[&wid].tab_set.active().unwrap().root,
        original.root
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_bootstrap_keeps_committed_theme_set_or_clear_before_its_first_restore_step() {
    use crate::session_theme::{SessionTheme, SessionThemeOrigin};

    for choice in 0..5 {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let original = app.windows[&wid].tab_set.active().unwrap().clone();
        let session = app.focused_session_id(wid).unwrap();
        match choice {
            1 | 2 => app.set_session_theme(
                session,
                Some(SessionTheme {
                    selector: "Linen".into(),
                    origin: if choice == 1 {
                        SessionThemeOrigin::Split
                    } else {
                        SessionThemeOrigin::Restored
                    },
                }),
            ),
            3 => {
                app.set_session_theme_checked(session, "Linen").unwrap();
            }
            4 => app.clear_front_tab_theme(),
            _ => {}
        }
        let mut saved = terminal("saved");
        let restore::RestoredSplitTree::Leaf {
            view: restore::RestoredView::Terminal(leaf),
        } = &mut saved
        else {
            unreachable!()
        };
        leaf.theme = Some("Hearth".into());
        let mut building = cold::BuildingTab::new(
            0,
            record(tree(vec![about(), saved])),
            Some(cold::Anchor {
                tab: original.id,
                view: original.focus,
                original: original.clone(),
            }),
            Some((original.focus, session)),
        );
        assert!(matches!(building.step(&mut app, wid), cold::Step::Advanced));
        let expected = match choice {
            3 => Some("Linen"),
            4 => None,
            _ => Some("Hearth"),
        };
        assert_eq!(
            app.kept_session_theme(session)
                .map(|theme| theme.selector.as_str()),
            expected
        );
        assert_eq!(app.pool.theme_decided(session), choice >= 3);
        assert!(matches!(
            building.step(&mut app, wid),
            cold::Step::Complete(_)
        ));
        assert_eq!(
            app.kept_session_theme(session)
                .map(|theme| theme.selector.as_str()),
            expected
        );
        building.cancel(&mut app);
    }
}

#[test]
fn a_restored_theme_updates_the_trial_underlay_without_committing_its_preview() {
    use crate::palette::PaletteTarget;

    for commit in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let original = app.windows[&wid].tab_set.active().unwrap().clone();
        let session = app.focused_session_id(wid).unwrap();
        app.open_tab_theme_palette();
        for ch in "Linen".chars() {
            app.palette_filter_push(ch);
        }
        assert_eq!(
            app.windows[&wid].palette().unwrap().selected_target(),
            Some(PaletteTarget::Theme("Linen".into()))
        );
        app.sync_palette_theme_preview();
        assert_eq!(app.pool.theme(session).unwrap().selector, "Linen");
        assert!(!app.pool.theme_decided(session));
        let mut saved = terminal("saved");
        let restore::RestoredSplitTree::Leaf {
            view: restore::RestoredView::Terminal(leaf),
        } = &mut saved
        else {
            unreachable!()
        };
        leaf.theme = Some("Hearth".into());
        let mut building = cold::BuildingTab::new(
            0,
            record(tree(vec![about(), saved])),
            Some(cold::Anchor {
                tab: original.id,
                view: original.focus,
                original: original.clone(),
            }),
            Some((original.focus, session)),
        );
        assert!(matches!(building.step(&mut app, wid), cold::Step::Advanced));
        assert_eq!(
            app.pool.theme(session).unwrap().selector,
            "Linen",
            "the preview still draws"
        );
        assert_eq!(
            app.kept_session_theme(session).unwrap().selector,
            "Hearth",
            "capture reads the restored underlay"
        );
        assert!(!app.pool.theme_decided(session));
        if commit {
            app.palette_activate();
        } else {
            app.palette_exit();
        }
        app.sync_palette_theme_preview();
        assert!(app.palette_tab_theme.is_none());
        assert_eq!(
            app.pool.theme(session).unwrap().selector,
            if commit { "Linen" } else { "Hearth" }
        );
        assert_eq!(app.pool.theme_decided(session), commit);
        assert!(matches!(
            building.step(&mut app, wid),
            cold::Step::Complete(_)
        ));
        building.cancel(&mut app);
    }
}

#[test]
fn a_native_prefix_does_not_reapply_the_bootstraps_saved_theme_after_user_edit() {
    use crate::session_theme::{SessionTheme, SessionThemeOrigin};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let original = app.windows[&wid].tab_set.active().unwrap().clone();
    let session = app
        .view_store
        .get(original.focus)
        .copied()
        .and_then(View::terminal_session)
        .unwrap();
    let mut saved = terminal("later terminal");
    let restore::RestoredSplitTree::Leaf {
        view: restore::RestoredView::Terminal(leaf),
    } = &mut saved
    else {
        unreachable!()
    };
    leaf.theme = Some("Hearth".into());
    let view = original.focus;
    let anchor = cold::Anchor {
        tab: original.id,
        view,
        original,
    };
    let mut building = cold::BuildingTab::new(
        0,
        record(tree(vec![about(), saved])),
        Some(anchor),
        Some((view, session)),
    );
    assert!(matches!(building.step(&mut app, wid), cold::Step::Advanced));
    assert_eq!(app.kept_session_theme(session).unwrap().selector, "Hearth");
    app.set_session_theme(
        session,
        Some(SessionTheme {
            selector: "Linen".into(),
            origin: SessionThemeOrigin::Ctl,
        }),
    );
    assert!(matches!(
        building.step(&mut app, wid),
        cold::Step::Complete(_)
    ));
    assert_eq!(app.kept_session_theme(session).unwrap().selector, "Linen");
    building.cancel(&mut app);
}

#[test]
fn repeated_snapshots_refresh_built_leaves_and_carry_every_unstarted_leaf_once() {
    let mut app = App::headless_for_test();
    let source = window(vec![
        record(tree(vec![
            terminal("built"),
            terminal("owed-leaf"),
            about(),
        ])),
        record(terminal("owed-tab")),
    ]);
    let unstarted = window(vec![record(terminal("owed-window"))]);
    app.restore_drain = Some(Box::new(RestoreDrain::new(Some(
        restore::RestoreManifest::new(vec![source, unstarted]),
    ))));
    assert!(!native_slice(&mut app));
    assert!(!native_slice(&mut app));
    let session = app
        .pool
        .iter()
        .find(|session| session.ctx.meta.lock().unwrap().user_title.as_deref() == Some("built"))
        .unwrap()
        .id;
    let before_views = views(&app);
    let before_sessions = sessions(&app);
    for (title, hold) in [
        ("changed once", "local first"),
        ("changed twice", "local second"),
    ] {
        let live = app.pool.get(session).unwrap();
        live.ctx.meta.lock().unwrap().user_title = Some(title.into());
        assert!(crate::fabric::apply_hold_for_test(
            live.ctx.as_ref(),
            Some(crate::fabric::parse_hold(hold)),
        ));
        assert_eq!(
            crate::fabric::render_holds(&live.ctx.fabric.holds()),
            [hold]
        );
        let manifest = app.capture_restore_manifest();
        let leaves = carried_terminals(&manifest);
        for expected in [title, "owed-leaf", "owed-tab", "owed-window"] {
            assert_eq!(
                leaves
                    .iter()
                    .filter(|leaf| leaf.user_title.as_deref() == Some(expected))
                    .count(),
                1,
                "{expected} is carried once"
            );
        }
        let built = leaves
            .iter()
            .find(|leaf| leaf.user_title.as_deref() == Some(title))
            .unwrap();
        assert_eq!(built.holds, [hold]);
        assert!(
            built.local_id.is_none(),
            "an off-map source never claims a live pool id in the next process"
        );
        assert!(
            leaves
                .iter()
                .all(|leaf| leaf.user_title.as_deref() != Some("built"))
        );
        assert_eq!(views(&app), before_views, "capture consumes no staged view");
        assert_eq!(
            sessions(&app),
            before_sessions,
            "capture starts no owed shell"
        );
    }
    let drain = app.restore_drain.take().unwrap();
    drain.filling.unwrap().building.unwrap().cancel(&mut app);
}

#[test]
fn completing_the_anchor_graft_keeps_a_users_new_split_focus_and_geometry() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let original = app.windows[&wid].tab_set.active().unwrap().clone();
    let source = record(tree(vec![
        terminal("one"),
        terminal("two"),
        terminal("three"),
    ]));
    let mut filling = app
        .begin_sliced_fill(wid, window(vec![source.clone()]))
        .unwrap();
    assert!(
        filling.building.is_some(),
        "the first leaf borrowed the bootstrap and yielded"
    );
    let user_session = app.split_active_stub_tab(wid);
    let user_view = app.windows[&wid].tab_set.active().unwrap().focus;
    assert_eq!(
        app.view_store
            .get(user_view)
            .copied()
            .and_then(View::terminal_session),
        Some(user_session)
    );
    let (user_axis, user_ratio) = match &app.windows[&wid].tab_set.active().unwrap().root {
        SplitTree::Split {
            axis,
            ratio,
            second,
            ..
        } => {
            assert_eq!(**second, SplitTree::Leaf(user_view));
            (*axis, *ratio)
        }
        _ => panic!("the user's split must exist before restore resumes"),
    };
    assert!(app.land_next_drained_tab(&mut filling));
    assert!(!app.land_next_drained_tab(&mut filling));
    let tab = app.windows[&wid].tab_set.active().unwrap();
    assert_eq!(tab.id, original.id);
    assert_eq!(
        tab.focus, user_view,
        "restore must not retake focus from a new sibling"
    );
    let SplitTree::Split {
        axis,
        ratio,
        first,
        second,
    } = &tab.root
    else {
        panic!("the user's outer split was lost")
    };
    assert_eq!((*axis, *ratio), (user_axis, user_ratio));
    assert_eq!(**second, SplitTree::Leaf(user_view));
    assert_shape(&source.root, first);
    assert_eq!(terminal_names(&app, first), ["one", "two", "three"]);
    assert_eq!(tab.root.len(), 4);
    assert!(app.pool.get(user_session).is_some());
    assert!(
        app.structural_invariants_ok(),
        "{}",
        app.structural_invariant_violation().unwrap_or_default()
    );
}

#[test]
fn an_anchored_snapshot_preserves_current_state_without_consuming_the_builder() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let anchor = app.windows[&wid].tab_set.active().unwrap().focus;
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(vec![record(
        tree(vec![terminal("saved"), terminal("unstarted"), about()]),
    )])]));
    assert!(!bootstrap_slice(&mut app));
    let session = app
        .view_store
        .get(anchor)
        .copied()
        .and_then(View::terminal_session)
        .unwrap();
    let live = app.pool.get(session).unwrap();
    live.ctx.meta.lock().unwrap().user_title = Some("current live leaf".into());
    live.ctx.fabric.seed_holds(
        crate::fabric::parse_holds(["local paused"]),
        &live.ctx.timeline,
    );
    let before_views = views(&app);
    for _ in 0..2 {
        let mut manifest = app.capture_restore_manifest();
        app.quit_agents(&mut manifest, true);
        let leaves = carried_terminals(&manifest);
        assert_eq!(leaves.len(), 2);
        assert_eq!(leaves[0].user_title.as_deref(), Some("current live leaf"));
        assert_eq!(leaves[0].holds, ["local paused"]);
        assert_eq!(leaves[1].user_title.as_deref(), Some("unstarted"));
        assert!(leaves.iter().all(|leaf| leaf.local_id.is_none()));
        assert_eq!(views(&app), before_views);
        assert!(
            app.restore_drain
                .as_ref()
                .unwrap()
                .filling
                .as_ref()
                .unwrap()
                .building
                .is_some()
        );
    }
    let mut discarded = app.capture_restore_manifest();
    app.quit_agents(&mut discarded, false);
    assert!(
        carried_terminals(&discarded)
            .iter()
            .all(|leaf| leaf.holds.is_empty() && leaf.agent.is_none())
    );
    assert_eq!(
        app.desk_holds(session),
        ["local paused"],
        "cancelled quit never changes the live hold"
    );
    while !bootstrap_slice(&mut app) {}
    assert_eq!(
        terminal_names(&app, &app.windows[&wid].tab_set.active().unwrap().root),
        ["current live leaf", "unstarted"]
    );
}

#[test]
fn closing_the_anchor_immediately_retires_its_unpublished_siblings() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let bootstrap = app.focused_session_id(wid).unwrap();
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(vec![record(
        tree(vec![terminal("anchor"), about(), terminal("never started")]),
    )])]));
    assert!(!bootstrap_slice(&mut app));
    assert!(!bootstrap_slice(&mut app));
    let fresh = views(&app)
        .into_iter()
        .find(|view| matches!(app.view_store.get(*view), Some(View::Native(_))))
        .unwrap();
    let next = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(next));
    assert!(app.close_session_tab_for_test(bootstrap));
    assert!(
        app.view_store.get(fresh).is_none(),
        "close releases unpublished native resources immediately"
    );
    assert!(
        app.restore_drain
            .as_ref()
            .unwrap()
            .filling
            .as_ref()
            .unwrap()
            .building
            .is_none()
    );
    assert!(app.pool.get(bootstrap).is_none());
    while !bootstrap_slice(&mut app) {}
    assert_eq!(app.windows[&wid].tab_set.len(), 1);
    assert_eq!(sessions(&app), [next]);
    assert!(app.structural_invariants_ok());
}

fn finish_regrid(app: &mut App, regrid: &mut super::regrid::Finalizing) {
    for _ in 0..4096 {
        match regrid.step(app) {
            super::regrid::Step::Complete => return,
            super::regrid::Step::Advanced => {}
            super::regrid::Step::Wait(_) => panic!("headless geometry is already settled"),
        }
    }
    panic!("the finite re-grid did not finish");
}

#[test]
fn the_final_regrid_plans_each_stable_tab_once_and_resizes_each_leaf_once() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.restore_into_window(
        wid,
        window(vec![record(tree(
            (0..32).map(|n| terminal(&format!("pane-{n}"))).collect(),
        ))]),
    );
    let session_ids = sessions(&app);
    let expected = {
        let state = &app.windows[&wid];
        app.plan_tab(state.tab_set.active().unwrap(), state.rows, state.cols)
            .leaves
            .into_iter()
            .map(|leaf| {
                let session = app
                    .view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(View::terminal_session)
                    .unwrap();
                (session, App::leaf_grid_cells(&leaf.rect))
            })
            .collect::<Vec<_>>()
    };
    crate::work_counts::take_plans_built();
    let mut regrid = super::regrid::Finalizing::new([wid].into_iter());
    finish_regrid(&mut app, &mut regrid);
    assert_eq!(
        crate::work_counts::take_plans_built(),
        1,
        "one plan for 32 independent leaf applications"
    );
    for (id, grid) in expected {
        assert_eq!(super::tests::grid_of(&app, id), grid);
        assert_eq!(super::tests::resize_sites(&app, id), ["site=restore"]);
    }
    let mut repeated = super::regrid::Finalizing::new([wid].into_iter());
    finish_regrid(&mut app, &mut repeated);
    assert!(
        session_ids
            .iter()
            .all(|id| super::tests::resize_sites(&app, *id) == ["site=restore"]),
        "unchanged dimensions never make another resize wave"
    );
}

#[test]
fn geometry_changed_after_an_early_leaf_is_resized_invalidates_the_remaining_plan() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.restore_into_window(
        wid,
        window(vec![record(tree(vec![
            terminal("first"),
            terminal("second"),
            terminal("third"),
        ]))]),
    );
    let mut regrid = super::regrid::Finalizing::new([wid].into_iter());
    assert!(
        matches!(regrid.step(&mut app), super::regrid::Step::Advanced),
        "first unit plans the tab"
    );
    assert!(
        matches!(regrid.step(&mut app), super::regrid::Step::Advanced),
        "second unit resizes its first leaf"
    );
    let first = sessions(&app)[0];
    assert_eq!(super::tests::resize_sites(&app, first), ["site=restore"]);
    app.windows.get_mut(&wid).unwrap().cols = 120;
    finish_regrid(&mut app, &mut regrid);
    let state = &app.windows[&wid];
    for leaf in app
        .plan_tab(state.tab_set.active().unwrap(), state.rows, state.cols)
        .leaves
    {
        let id = app
            .view_store
            .get(leaf.view)
            .copied()
            .and_then(View::terminal_session)
            .unwrap();
        assert_eq!(
            super::tests::grid_of(&app, id),
            App::leaf_grid_cells(&leaf.rect)
        );
        let count = super::tests::resize_sites(&app, id).len();
        assert_eq!(
            count,
            if id == first { 2 } else { 1 },
            "only the previously applied old size requires correction"
        );
    }
}

#[test]
fn replacing_the_view_store_never_resizes_the_session_a_reused_view_id_used_to_name() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().rows = 30;
    let original = app.windows[&wid].tab_set.active().unwrap().focus;
    let mut regrid = super::regrid::Finalizing::new([wid].into_iter());
    assert!(matches!(
        regrid.step(&mut app),
        super::regrid::Step::Advanced
    ));
    let mut replacement = crate::tab_model::ViewStore::default();
    assert_eq!(replacement.insert_terminal(99).unwrap(), original);
    app.pool.insert(crate::stub_session(99));
    app.view_store = replacement;
    finish_regrid(&mut app, &mut regrid);
    assert_eq!(super::tests::grid_of(&app, 99), (30, 80));
    assert_eq!(super::tests::grid_of(&app, 0), (24, 80));
    assert!(super::tests::resize_sites(&app, 0).is_empty());
}

#[test]
fn presentation_updates_between_every_regrid_step_do_not_restart_geometry_work() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.restore_into_window(
        wid,
        window(vec![record(tree(
            (0..16).map(|n| terminal(&format!("pane-{n}"))).collect(),
        ))]),
    );
    let tab = app.windows[&wid].tab_set.active_id().unwrap();
    let mut regrid = super::regrid::Finalizing::new([wid].into_iter());
    crate::work_counts::take_plans_built();
    let mut complete = false;
    for _ in 0..32 {
        app.refresh_aggregate_tab_presentation(wid, tab);
        app.refresh_window_tabs(wid);
        match regrid.step(&mut app) {
            super::regrid::Step::Complete => {
                complete = true;
                break;
            }
            super::regrid::Step::Advanced => {}
            super::regrid::Step::Wait(_) => panic!("headless geometry does not wait"),
        }
    }
    assert!(
        complete,
        "presentation activity must not starve the finite re-grid"
    );
    assert_eq!(crate::work_counts::take_plans_built(), 1);
}

#[test]
fn the_drain_stays_pending_through_every_final_resize_unit() {
    let mut app = App::headless_for_test();
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(vec![record(
        tree(vec![
            terminal("first"),
            terminal("second"),
            terminal("third"),
        ]),
    )])]));
    for _ in 0..3 {
        assert!(!bootstrap_slice(&mut app));
    }
    let ids = sessions(&app);
    assert_eq!(ids.len(), 3);
    let resized = |app: &App| {
        ids.iter()
            .map(|id| super::tests::resize_sites(app, *id).len())
            .sum::<usize>()
    };
    assert_eq!(
        resized(&app),
        0,
        "construction does not emit intermediate resize waves"
    );
    let mut previous = 0;
    let mut complete = false;
    for _ in 0..16 {
        complete = bootstrap_slice(&mut app);
        let now = resized(&app);
        assert!(
            now - previous <= 1,
            "a zero-budget callback applies at most one final leaf resize"
        );
        if now < ids.len() {
            assert!(
                !complete,
                "the restore cannot finish before its last regrid unit"
            );
        }
        previous = now;
        if complete {
            break;
        }
    }
    assert!(complete);
    assert_eq!(resized(&app), 3);
}

#[test]
fn a_coviews_active_tab_change_during_finalization_keeps_shared_sizing_semantics() {
    let mut app = App::headless_for_test();
    let first = WindowId(0);
    let second = app.open_active_session_in_new_window_logical().unwrap();
    app.windows.get_mut(&first).unwrap().rows = 30;
    app.windows.get_mut(&first).unwrap().cols = 100;
    app.windows.get_mut(&second).unwrap().rows = 60;
    app.windows.get_mut(&second).unwrap().cols = 200;
    let extra = app.next_session_id;
    app.push_stub_tab(first, crate::stub_session(extra));
    app.switch_tab_in(first, 0);
    assert_eq!(app.shared_target_geometry(0), (30, 100));
    let mut regrid = super::regrid::Finalizing::new([first, second].into_iter());
    assert!(matches!(
        regrid.step(&mut app),
        super::regrid::Step::Advanced
    ));
    assert!(matches!(
        regrid.step(&mut app),
        super::regrid::Step::Advanced
    ));
    assert_eq!(super::tests::grid_of(&app, 0), (30, 100));
    app.switch_tab_in(first, 1);
    assert_eq!(app.shared_target_geometry(0), (60, 200));
    finish_regrid(&mut app, &mut regrid);
    assert_eq!(super::tests::grid_of(&app, 0), (60, 200));
    assert_eq!(super::tests::grid_of(&app, extra), (30, 100));
    assert!(app.structural_invariants_ok());
}

#[cfg(unix)]
#[test]
fn failed_native_attach_rolls_back_a_recovered_dirty_document_without_losing_its_journal() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let path = root.join("recovered.txt");
    let journals = root.join("drafts");
    std::fs::write(&path, "disk\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let journal_path = {
        let mut crashed = App::headless_for_test();
        crashed.use_document_journal_root_for_test(journals.clone());
        crashed
            .open_document_tab_in_window_sync(WindowId(0), crate::native_app::AppKind::Editor, &uri)
            .unwrap();
        crashed
            .dispatch_native_event(
                WindowId(0),
                crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
                    "unsaved ".into(),
                )),
            )
            .unwrap();
        let (instance, _) = crashed.active_native_view(WindowId(0)).unwrap();
        let document = crashed.native_runtime.document_id(instance).unwrap();
        assert_eq!(crashed.document_store.dirty(document), Some(true));
        let journal_path = crashed.document_journal_path_for_test(document).unwrap();
        crashed.exit_for_test();
        journal_path
    };
    let mut app = App::headless_for_test();
    app.use_document_journal_root_for_test(journals.clone());
    let before_views = views(&app);
    let before_windows = app.windows.len();
    let mut refused = None;
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(vec![record(
        restore::RestoredSplitTree::leaf(restore::RestoredView::Native(
            restore::NativeLeafRestore::document("editor", uri.clone()),
        )),
    )])]));
    crate::app_documents::with_headless_restore_admission_for_test(|| {
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            if let Some(drain) = app.restore_drain.as_mut() {
                drain.document_ready();
            }
            let complete = app.drain_restore_slice(
                Duration::ZERO,
                |app, _, source, _, filling| {
                    *filling = app.begin_native_sliced_fill(source);
                    ControlFlow::Continue(())
                },
                |app, filling| {
                    let view = app.windows[&filling.wid].tab_set.active().unwrap().focus;
                    let View::Native(native) = *app.view_store.get(view).unwrap() else {
                        panic!("recovered native view")
                    };
                    let document = app.native_runtime.document_id(native.instance).unwrap();
                    assert_eq!(app.document_store.dirty(document), Some(true));
                    assert_eq!(
                        app.document_store.snapshot(document).unwrap().text.as_ref(),
                        "unsaved disk\n"
                    );
                    refused = Some((document, native.instance));
                    false
                },
            );
            if complete {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the actual document worker did not finish"
            );
            aterm_uds::deadline::poll_yield();
        }
    });
    let (document, instance) = refused.expect("the attach refusal was exercised");
    assert_eq!(app.windows.len(), before_windows);
    assert_eq!(views(&app), before_views);
    assert!(app.native_runtime.app(instance).is_none());
    assert!(
        app.document_store
            .view_count(document)
            .is_none_or(|count| count == 0)
    );
    assert!(journal_path.exists());
    app.exit_for_test();
    let mut reopened = App::headless_for_test();
    reopened.use_document_journal_root_for_test(journals);
    reopened
        .open_document_tab_in_window_sync(WindowId(0), crate::native_app::AppKind::Editor, &uri)
        .unwrap();
    let (instance, _) = reopened.active_native_view(WindowId(0)).unwrap();
    let document = reopened.native_runtime.document_id(instance).unwrap();
    assert_eq!(
        reopened
            .document_store
            .snapshot(document)
            .unwrap()
            .text
            .as_ref(),
        "unsaved disk\n"
    );
}
