// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn the_tracker_keeps_the_normal_frame_through_a_maximize_and_restore() {
    let mut frame = super::NormalFrame {
        origin: None,
        rows: 24,
        cols: 80,
    };
    frame.note_origin(false, (300, 200));
    frame.note_grid(false, (30, 100));
    frame.note_origin(false, (320, 210));
    frame.note_origin(true, (0, 0));
    frame.note_grid(true, (62, 240));
    assert_eq!(
        super::captured_frame(Some((0, 0)), (62, 240), Some(true), frame),
        (Some((320, 210)), (30, 100)),
        "a maximized capture persists the frame the user had"
    );
    frame.note_origin(false, (320, 210));
    frame.note_grid(false, (30, 100));
    frame.note_origin(false, (500, 50));
    assert_eq!(frame.origin, Some((500, 50)));
    let (live_origin, live_grid) = (Some((-8, -8)), (62, 240));
    for shown in [Some(false), None] {
        assert_eq!(
            super::captured_frame(live_origin, live_grid, shown, frame),
            (live_origin, live_grid),
            "not maximized ({shown:?}): the frame as shown, never the tracked one"
        );
    }
    let unplaced = super::NormalFrame {
        origin: None,
        ..frame
    };
    assert_eq!(
        super::captured_frame(Some((0, 0)), (62, 240), Some(true), unplaced),
        (None, (30, 100))
    );
}

#[test]
fn a_tiled_window_persists_the_frame_it_had_before_the_tile() {
    let normal = super::NormalFrame {
        origin: Some((320, 210)),
        rows: 30,
        cols: 100,
    };
    let (tile_origin, tile_grid) = ((0, 0), (62, 118));
    let (maximized, tiled) = (false, true);
    let mut held = normal;
    held.note_origin(maximized || tiled, tile_origin);
    held.note_grid(maximized || tiled, tile_grid);
    assert_eq!(held, normal, "the tile's frame is not the normal one");
    let hold = super::captured_hold(false, Some(maximized), || maximized || tiled);
    assert_eq!(
        super::captured_frame(Some(tile_origin), tile_grid, hold, held),
        (Some((320, 210)), (30, 100)),
        "a tiled capture persists the frame from before the tile"
    );
    let mut both_flags = normal;
    both_flags.note_origin(maximized, tile_origin);
    both_flags.note_grid(maximized, tile_grid);
    assert_eq!(
        super::captured_frame(Some(tile_origin), tile_grid, Some(maximized), both_flags),
        (Some(tile_origin), tile_grid),
        "negative control: the both-flags rule persists the tiled frame"
    );
}

#[test]
fn a_full_screen_window_persists_the_frame_it_had_before_the_full_screen() {
    let normal = super::NormalFrame {
        origin: Some((320, 210)),
        rows: 30,
        cols: 100,
    };
    let (screen_origin, screen_grid) = ((0, 0), (68, 240));
    let (maximized, tiled, fullscreen) = (false, false, true);
    let mut held = normal;
    held.note_origin(maximized || tiled || fullscreen, screen_origin);
    held.note_grid(maximized || tiled || fullscreen, screen_grid);
    assert_eq!(held, normal, "the screen's frame is not the normal one");
    let shows = || maximized || tiled || fullscreen;
    assert_eq!(
        super::captured_frame(
            Some(screen_origin),
            screen_grid,
            super::captured_hold(false, Some(maximized), shows),
            held
        ),
        (Some((320, 210)), (30, 100)),
        "a quit or crash capture of a full-screen window persists the frame from before it"
    );
    assert_eq!(
        super::captured_frame(
            Some(screen_origin),
            screen_grid,
            super::captured_hold(true, Some(maximized), shows),
            held
        ),
        (Some(screen_origin), screen_grid),
        "a seamless update carries the frame the window shows"
    );
    assert_eq!(
        super::captured_hold(false, None, || unreachable!("no normal frame is tracked")),
        None,
        "where no normal frame is tracked, nothing changes"
    );
    assert_eq!(
        super::captured_hold(true, Some(true), || unreachable!(
            "a maximize is held anyway"
        )),
        Some(true),
        "a seamless update persists a maximized window's normal frame, as it did"
    );
    let mut replaced = normal;
    replaced.note_origin(maximized || tiled, screen_origin);
    replaced.note_grid(maximized || tiled, screen_grid);
    assert_eq!(
        super::captured_frame(
            Some(screen_origin),
            screen_grid,
            Some(maximized || tiled),
            replaced
        ),
        (Some(screen_origin), screen_grid),
        "negative control: the maximized-or-tiled rule persists the screen's frame"
    );
}
use crate::{App, CloseOutcome, WindowId, pane, restore};

#[test]
fn recovery_authority_comes_only_from_typed_restore_fields() {
    let valid =
        restore::NativeLeafRestore::document("markdown", "file:///tmp/Guide.md".to_string());
    assert!(matches!(
        super::recovery_capability(&valid),
        Some(crate::native_app::RecoveryCapability::Document {
            kind: crate::native_app::AppKind::Markdown,
            ref uri,
            config_editor: false,
        }) if uri == "file:///tmp/Guide.md"
    ));

    let mut manual =
        restore::NativeLeafRestore::document("editor", "file:///tmp/aterm.toml".to_string());
    manual.config_editor = true;
    assert!(matches!(
        super::recovery_capability(&manual),
        Some(crate::native_app::RecoveryCapability::Document {
            kind: crate::native_app::AppKind::Editor,
            ref uri,
            config_editor: true,
        }) if uri == "file:///tmp/aterm.toml"
    ));

    let mut diagnostics_only =
        restore::NativeLeafRestore::document("future.canvas", "file:///tmp/ignored".to_string());
    diagnostics_only.metadata = "uri=Some(\"file:///tmp/secret\")".to_string();
    assert_eq!(super::recovery_capability(&diagnostics_only), None);

    let unsafe_uri = restore::NativeLeafRestore::document(
        "editor",
        "file:///tmp/ok\nhttps://attacker.example".to_string(),
    );
    assert_eq!(super::recovery_capability(&unsafe_uri), None);
}

fn file_uri(path: &std::path::Path) -> String {
    crate::native_document_host::path_to_file_uri(path).unwrap()
}

fn add_stub(app: &mut App, id: u64) {
    let s = crate::stub_session(id);
    App::register_session(&app.store, &s, None);
    app.pool.insert(s);
    app.next_session_id = app.next_session_id.max(id + 1);
}

#[test]
fn capture_reflects_the_live_layout() {
    let mut app = App::headless_for_test();
    add_stub(&mut app, 1);
    app.view_store
        .insert_terminal(1)
        .expect("split view identity");
    {
        let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
        assert!(ws.layouts[0].split_focused(pane::SplitDir::Vertical, 1));
    }
    assert!(app.sync_tab_model_from_layout(WindowId(0), 0));
    add_stub(&mut app, 2);
    let tree = pane::PaneTree::new(2);
    let tab = crate::register_terminal_tab(&mut app.tab_ids, &mut app.view_store, &tree)
        .expect("second tab identity");
    let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
    ws.layouts.push(tree);
    ws.tabs.add();
    ws.tab_set.push(tab).expect("fresh tab id");
    app.resync_active_or_window(WindowId(0));
    assert!(app.structural_invariants_ok());

    let m = app.capture_restore_manifest();
    assert_eq!(m.windows.len(), 1);
    let w = &m.windows[0];
    assert_eq!((w.rows, w.cols), (24, 80), "headless harness grid");
    assert_eq!(w.active_tab, 1, "the appended tab was active");
    assert_eq!(w.tabs.len(), 2);
    assert_eq!(w.tabs[0].leaf_count(), 2, "tab 0 kept its split");
    assert_eq!(w.tabs[1].leaf_count(), 1);
    let leaves = w.tabs[0].leaves();
    assert!(
        matches!(leaves[1], restore::PaneLayout::Leaf { focused: true, .. }),
        "the split's new pane holds tab 0's focus",
    );
    assert!(
        matches!(leaves[0], restore::PaneLayout::Leaf { focused: false, .. }),
        "the original pane lost it",
    );
    let back =
        restore::RestoreManifest::from_toml(&m.to_toml().expect("serialize")).expect("parse");
    assert_eq!(m, back);
}

#[test]
fn a_cold_restore_brings_every_window_back_at_its_own_grid_and_size() {
    use crate::app_window::CarriedFrame;
    use aterm_window::dpi::PhysicalSize;
    let (w0, w1) = (WindowId(0), WindowId(1));
    let grid = |app: &App, wid: WindowId| (app.windows[&wid].rows, app.windows[&wid].cols);
    let size_to = |app: &mut App, wid: WindowId, size: PhysicalSize<u32>| {
        app.windows.get_mut(&wid).expect("a window").win_px = Some(size);
        app.on_resize(wid, size);
    };
    let quit = |app: &App| {
        let wire = app
            .capture_restore_manifest()
            .to_toml()
            .expect("serializes");
        restore::RestoreManifest::from_toml(&wire).expect("the next launch parses it")
    };
    let relaunch = |layout: &restore::RestoreManifest| {
        let mut next = App::headless_for_test();
        let row0 = &layout.windows[0];
        let size = next.window_frame_px_for(w0, row0.rows, row0.cols);
        size_to(&mut next, w0, size);
        let row = CarriedFrame::of(&layout.windows[1]);
        let ((rows, cols), inner) = next.new_window_frame(row);
        assert_eq!(
            next.insert_logical_window(crate::stub_session(1), rows, cols),
            w1
        );
        let ws = next.windows.get_mut(&w1).expect("window 1");
        ws.take_carried_frame(inner);
        let carried = ws.carried_inner_px.take();
        let frame = next.attach_frame_px(w1, rows, cols, carried);
        size_to(&mut next, w1, frame);
        (next, frame)
    };

    let mut desk = App::headless_for_test();
    let base = desk.font_size;
    assert_eq!(
        desk.insert_logical_window(crate::stub_session(1), 40, 120),
        w1
    );
    let first = desk.window_frame_px_for(w0, 29, 100);
    let exact = desk.window_frame_px_for(w1, 40, 120);
    let dragged = PhysicalSize::new(exact.width + 3, exact.height + 2);
    size_to(&mut desk, w0, first);
    size_to(&mut desk, w1, dragged);
    assert_eq!(
        (grid(&desk, w0), grid(&desk, w1)),
        ((29, 100), (40, 120)),
        "PRECONDITION: the drag stays inside one cell"
    );

    let layout = quit(&desk);
    let (next, frame) = relaunch(&layout);
    assert_eq!(grid(&next, w1), (40, 120), "window 1 keeps its own grid");
    assert_eq!(frame, dragged, "and the size it had");
    assert_eq!(grid(&next, w0), (29, 100));

    desk.set_font_size(base + crate::FONT_ZOOM_STEP);
    size_to(&mut desk, w0, first);
    size_to(&mut desk, w1, dragged);
    let zoomed = grid(&desk, w1);
    assert_ne!(
        zoomed,
        (40, 120),
        "PRECONDITION: the zoom re-gridded window 1"
    );
    let (next, frame) = relaunch(&quit(&desk));
    assert_eq!(grid(&next, w1), zoomed, "window 1 keeps its grid");
    assert_eq!(frame, next.window_frame_px_for(w1, zoomed.0, zoomed.1));

    let mut older = layout;
    older.windows[1].inner_px = None;
    let (next, _) = relaunch(&older);
    assert_eq!(grid(&next, w1), (29, 100), "the front window's grid");
}

fn feed_session0(app: &App, bytes: &[u8]) {
    let s = app.pool.get(0).expect("session 0");
    crate::term_lock(&s.term).process(bytes);
}

#[test]
fn tab_label_rungs_fall_back_in_order_and_move_the_strip_fingerprint() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let fingerprint = |app: &mut App| {
        let titles = app.tab_titles(wid);
        let metadata = app.tab_strip_metadata(wid);
        app.tab_strip_fingerprint_from_parts(wid, &titles, &metadata, 0)
    };
    assert_eq!(app.tab_titles(wid)[0], "aterm");
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .tab_set
        .active_mut()
        .expect("active tab")
        .presentation
        .title = "Renamed".to_string();
    assert_eq!(app.tab_titles(wid)[0], "Renamed");
    feed_session0(&app, b"\x1b]7;file://localhost/aterm-proof/live\x07");
    assert_eq!(app.tab_titles(wid)[0], "/aterm-proof/live");
    #[cfg(unix)]
    {
        if let Some(home) = crate::app_tabs::cached_home() {
            feed_session0(
                &app,
                format!("\x1b]7;file://localhost{home}/aterm-abbrev-proof\x07").as_bytes(),
            );
            assert_eq!(app.tab_titles(wid)[0], "~/aterm-abbrev-proof");
            feed_session0(
                &app,
                format!("\x1b]7;file://localhost{home}\x07").as_bytes(),
            );
            assert_eq!(app.tab_titles(wid)[0], "~", "home itself is the bare ~");
        }
    }
    feed_session0(&app, b"\x1b]7;\x07");
    assert_eq!(
        app.tab_titles(wid)[0],
        "Renamed",
        "a cleared cwd falls back"
    );

    feed_session0(&app, b"\x1b]7;file://localhost/aterm-proof/meta\x07");
    feed_session0(&app, b"\x1b]2;vim src/main.rs\x07");
    assert_eq!(
        app.tab_titles(wid)[0],
        "vim src/main.rs",
        "OSC outranks cwd"
    );
    let before = fingerprint(&mut app);
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    ctx.meta.lock().unwrap().user_title = Some("build agent".to_string());
    assert_eq!(app.tab_titles(wid)[0], "build agent");
    assert_ne!(
        fingerprint(&mut app),
        before,
        "a user title moves the strip"
    );
    ctx.meta.lock().unwrap().user_title = Some(String::new());
    assert_eq!(app.tab_titles(wid)[0], "vim src/main.rs", "empty is unset");
    ctx.meta.lock().unwrap().user_title = None;
    assert_eq!(fingerprint(&mut app), before, "clearing restores the label");
    feed_session0(&app, b"\x1b]2;\x07");
    assert_eq!(app.tab_titles(wid)[0], "/aterm-proof/meta");

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::default()));
    assert_eq!(
        app.tab_titles(wid),
        vec!["/aterm-proof/meta", "Settings"],
        "the cwd labels ONLY the terminal tab"
    );
}

#[test]
fn restore_leaf_reseeds_user_metadata_onto_the_respawned_session() {
    let bare = restore::TerminalLeafRestore {
        local_id: None,
        ..bare_leaf()
    };
    let session = crate::stub_session(9);
    let titled = restore::TerminalLeafRestore {
        user_title: Some("release builder".to_string()),
        ..bare.clone()
    };
    App::seed_restored_user_meta(&session, &titled);
    App::seed_restored_user_meta(&session, &bare);
    assert_eq!(
        session.ctx.meta.lock().unwrap().user_title.as_deref(),
        Some("release builder"),
        "a leaf with no metadata never clears live metadata"
    );

    for word in crate::session_timeline::QUESTIONS_POLICY_WORDS {
        let leaf = restore::TerminalLeafRestore {
            questions: Some(word.to_string()),
            ..bare.clone()
        };
        let session = crate::stub_session(9);
        App::seed_restored_user_meta(&session, &leaf);
        assert_eq!(
            session.ctx.meta.lock().unwrap().questions.as_deref(),
            Some(word)
        );
    }
}

#[test]
fn a_restored_worker_with_no_word_stays_land() {
    use aterm_agent::supervise::Autopilot;
    let row = "rev=3 state=working from=s-0123456789abcdef0123 since=1789000000000 \
               result_bytes=0 attention=0";
    let word = |ctx: &crate::SessionCtx| ctx.meta.lock().unwrap().autopilot_word();
    let titled = restore::TerminalLeafRestore {
        local_id: Some(0),
        user_title: Some("worker".to_string()),
        ..bare_leaf()
    };
    let session = crate::stub_session(9);
    crate::task_record::seed_carried(&session.ctx, Some(row));
    App::seed_restored_user_meta(&session, &titled);
    assert_eq!(word(&session.ctx), Some(Autopilot::Land), "reseeded");
    let cruise = restore::TerminalLeafRestore {
        autopilot: Some("cruise".to_string()),
        ..titled.clone()
    };
    App::seed_restored_user_meta(&session, &cruise);
    assert_eq!(
        word(&session.ctx),
        Some(Autopilot::Cruise),
        "NEGATIVE CONTROL: a carried word stands"
    );
    let plain = crate::stub_session(8);
    App::seed_restored_user_meta(&plain, &titled);
    assert_eq!(word(&plain.ctx), None, "NEGATIVE CONTROL: no task record");
    let mut app = App::headless_for_test();
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    crate::task_record::seed_carried(&ctx, Some(row));
    assert_eq!(word(&ctx), Some(Autopilot::Land));
    app.graft_restored_user_meta(0, &titled);
    assert_eq!(word(&ctx), Some(Autopilot::Land), "grafted");
    app.graft_restored_user_meta(0, &cruise);
    assert_eq!(
        word(&ctx),
        Some(Autopilot::Cruise),
        "NEGATIVE CONTROL: a carried word stands"
    );
}

#[test]
fn restored_and_legacy_raw_metadata_is_sanitized_before_chrome() {
    let family = "👨‍👩‍👧‍👦";
    let leaf = restore::TerminalLeafRestore {
        title: "shell".to_string(),
        local_id: None,
        user_title: Some("  release\n\u{202e}builder  ".to_string()),
        description: Some(format!("cuts\u{2029}the release{}", "x".repeat(1100))),
        icon: Some(format!("\u{2066}{family}\u{2069}")),
        role: Some("operator\u{200b}".to_string()),
        attention: Some("  needs\u{2028}human  ".to_string()),
        questions: Some("whatever claude thinks".to_string()),
        ..bare_leaf()
    };
    let session = crate::stub_session(9);
    App::seed_restored_user_meta(&session, &leaf);
    let meta = session.ctx.meta.lock().unwrap().clone();
    assert_eq!(meta.user_title.as_deref(), Some("releasebuilder"));
    assert_eq!(
        meta.icon.as_deref(),
        Some(family),
        "ZWJ cluster stays intact"
    );
    assert_eq!(
        meta.role.as_deref(),
        Some("operator"),
        "invisible formatting is stripped from a restored role"
    );
    assert_eq!(
        meta.attention.as_deref(),
        Some("needshuman"),
        "line separators are stripped from a restored attention message"
    );
    assert!(
        meta.description.as_ref().is_some_and(|value| {
            value.starts_with("cutsthe release")
                && value.len() <= crate::session_timeline::META_DESCRIPTION_MAX
                && !crate::session_timeline::metadata_has_forbidden_formatting(value)
        }),
        "restore fields are bounded and single-line: {:?}",
        meta.description
    );
    assert_eq!(
        meta.questions, None,
        "a policy outside the closed set is dropped, never stored"
    );

    let mut app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    {
        let mut raw = ctx.meta.lock().unwrap();
        raw.user_title = Some("raw\u{202e}title".to_string());
        raw.description = Some("first\nsecond".to_string());
        raw.icon = Some(format!("\u{2066}{family}\u{2069}"));
    }
    assert_eq!(
        app.tab_titles(WindowId(0))[0],
        "rawtitle",
        "the label is the sanitized title alone; the description is not joined in"
    );
    app.refresh_window_tabs(WindowId(0));
    let tooltip = app.windows[&WindowId(0)].tab_set.tabs()[0]
        .presentation
        .tooltip
        .as_deref()
        .expect("composed terminal tooltip");
    assert!(
        tooltip.contains("firstsecond")
            && tooltip
                .lines()
                .all(|line| { !crate::session_timeline::metadata_has_forbidden_formatting(line) }),
        "the description crosses the same sanitizer into the tooltip: {tooltip:?}"
    );
}

#[test]
fn last_window_close_stashes_the_quit_capture() {
    let mut app = App::headless_for_test();
    assert!(app.quit_capture.is_none());
    assert!(matches!(
        app.close_window_logical(WindowId(0)),
        CloseOutcome::Exit
    ));
    assert!(app.windows.is_empty(), "teardown drained the windows");
    let cap = app
        .quit_capture
        .take()
        .expect("stashed at close-decision time");
    assert_eq!(
        cap.windows.len(),
        1,
        "captured the window teardown destroyed"
    );
    assert_eq!(cap.windows[0].tabs.len(), 1);
    assert!(!cap.is_empty());
}

#[test]
fn capture_preserves_mixed_order_and_only_stable_native_descriptors() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-mixed-restore-capture-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("notes.md");
    std::fs::write(&path, "# Durable title\nsecret draft sentinel\n").unwrap();

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    app.open_document_tab(crate::native_app::AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (editor, _) = app.active_native_view(wid).expect("Editor active");
    let document = app
        .native_runtime
        .document_id(editor)
        .expect("Editor document");
    let canonical_uri = app
        .document_store
        .canonical_uri(document)
        .expect("canonical document URI")
        .to_string();
    app.move_tab(wid, 1, 0);
    let manifest = app.capture_restore_manifest();
    let window = &manifest.windows[0];
    assert_eq!(
        window.tab_order,
        vec![
            restore::TabOrderEntry::Native { index: 0 },
            restore::TabOrderEntry::Terminal { index: 0 },
            restore::TabOrderEntry::Native { index: 1 },
        ]
    );
    assert_eq!(window.active_item, Some(2));
    assert_eq!(
        window.native_tabs,
        vec![
            restore::NativeTabRestore::Settings {
                route: "/about".to_string(),
            },
            restore::NativeTabRestore::Editor { uri: canonical_uri },
        ]
    );
    let encoded = manifest.to_toml().unwrap();
    assert!(
        !encoded.contains("secret draft sentinel"),
        "document bytes must never enter session restore"
    );
    assert!(restore::RestoreManifest::from_toml(&encoded).is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn heterogeneous_active_tab_round_trips_by_recursive_index() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let _ = app.active_native_view(wid).expect("Settings active");
    let _ = app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    assert_eq!(app.windows[&wid].tab_set.active_index(), Some(1));

    let captured = app.capture_restore_manifest();
    let window = &captured.windows[0];
    assert_eq!(
        window.tab_order,
        vec![restore::TabOrderEntry::Terminal { index: 0 }],
        "the legacy mirror deliberately cannot encode the heterogeneous tab"
    );
    assert_eq!(window.restored_tabs.len(), 2);
    assert_eq!(
        window.active_item,
        Some(1),
        "active_item indexes the authoritative recursive tabs, not the compressed legacy mirror"
    );

    let decoded = restore::RestoreManifest::from_toml(
        &captured.to_toml().expect("serialize recursive restore"),
    )
    .expect("decode recursive restore");
    assert_eq!(decoded.windows[0].active_item, Some(1));

    let mut reopened = App::headless_for_test();
    reopened.restore_into_window(wid, decoded.windows.into_iter().next().unwrap());
    let state = &reopened.windows[&wid];
    assert_eq!(state.tab_set.active_index(), Some(1));
    let active = state.tab_set.active().expect("restored active tab");
    assert_eq!(active.root.len(), 2);
    assert!(active.root.leaves().into_iter().any(|view| matches!(
        reopened.view_store.get(view),
        Some(crate::tab_model::View::Native(_))
    )));
    assert!(active.root.leaves().into_iter().any(|view| matches!(
        reopened.view_store.get(view),
        Some(crate::tab_model::View::Terminal(_))
    )));
    assert!(reopened.structural_invariants_ok());
}

#[test]
fn failed_native_restore_staging_republishes_the_surviving_terminal() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let original_view = app.windows[&wid]
        .tab_set
        .active()
        .expect("bootstrap tab")
        .focus;
    assert_eq!(app.focused_session_id(wid), Some(0));
    assert!(app.active_handle.lock().unwrap().is_some());

    app.stage_settings_restore_view(wid, crate::native_settings::SettingsRoute::About)
        .expect("native restore staging view");
    assert_eq!(app.focused_session_id(wid), None);
    assert!(app.active_handle.lock().unwrap().is_none());

    let (staged_view, _, _) = app
        .detach_restore_staging_tab(wid)
        .expect("detach staged leaf");
    assert_eq!(
        app.windows[&wid].tab_set.active().unwrap().focus,
        original_view
    );
    assert_eq!(
        app.windows[&wid].front_terminal().map(|term| term.session),
        Some(0)
    );
    assert_eq!(app.focused_session_id(wid), Some(0));
    assert!(
        app.active_handle.lock().unwrap().is_some(),
        "detaching a candidate immediately republishes the live front capability"
    );
    assert_eq!(
        app.windows[&wid].window_focus,
        crate::front_content::WindowFocus::Content(original_view)
    );

    assert!(app.remove_view_link(staged_view).is_some());
    assert!(app.view_store.get(staged_view).is_none());
    assert_eq!(app.focused_session_id(wid), Some(0));
    assert!(app.active_handle.lock().unwrap().is_some());
    assert!(app.structural_invariants_ok());
}

#[test]
fn mixed_restore_rebuilds_canonical_order_active_item_and_derived_titles() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-mixed-restore-apply-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("restored-notes.md");
    std::fs::write(&path, "# Restored\n").unwrap();

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let layout = restore::WindowLayout {
        tabs: vec![restore::PaneLayout::leaf(
            Some("/tmp".to_string()),
            "shell title is not native identity".to_string(),
            true,
        )],
        native_tabs: vec![
            restore::NativeTabRestore::Settings {
                route: "/about".to_string(),
            },
            restore::NativeTabRestore::Editor {
                uri: file_uri(&path),
            },
        ],
        tab_order: vec![
            restore::TabOrderEntry::Native { index: 0 },
            restore::TabOrderEntry::Terminal { index: 0 },
            restore::TabOrderEntry::Native { index: 1 },
        ],
        active_item: Some(2),
        restored_tabs: Vec::new(),
        ..single_leaf_window(bare_leaf())
    };
    app.restore_into_window(wid, layout);

    let ws = &app.windows[&wid];
    let kinds = ws
        .tab_set
        .tabs()
        .iter()
        .map(|tab| match app.view_store.get(tab.focus) {
            Some(crate::tab_model::View::Terminal(_)) => "terminal",
            Some(crate::tab_model::View::Native(native)) => app
                .native_runtime
                .app(native.instance)
                .expect("live restored native instance")
                .kind()
                .as_str(),
            None => "missing",
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec!["settings", "terminal", "editor"]);
    assert_eq!(ws.tab_set.active_index(), Some(2));
    assert_eq!(app.tab_titles(wid)[0], "Settings");
    assert_eq!(app.tab_titles(wid)[2], "restored-notes.md");
    let (_, active_view) = app.active_native_view(wid).expect("restored editor active");
    assert_eq!(
        app.native_runtime
            .view_state(active_view)
            .map(|state| state.kind()),
        Some(crate::native_app::AppKind::Editor)
    );
    assert!(app.structural_invariants_ok());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn native_only_restore_never_discards_a_seamlessly_adopted_bootstrap() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.handoff_successor = true;
    adopt_as(&mut app, 0, 0);
    let original_terminal = app.windows[&wid].tab_set.active_id().unwrap();
    app.restore_into_window(wid, legacy_native_window(updates_settings()));

    let ws = &app.windows[&wid];
    assert_eq!(
        ws.tab_set.len(),
        2,
        "live handed-off shell remains reachable"
    );
    assert_eq!(
        ws.tab_set.active_index(),
        Some(0),
        "persisted native tab stays active"
    );
    assert!(ws.tab_set.get(original_terminal).is_some());
    assert_eq!(ws.layouts.len(), 1);
    assert!(
        app.pool.get(0).is_some(),
        "adopted session is never torn down"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_successors_fresh_bootstrap_is_retired_by_a_native_only_window_0() {
    use crate::native_settings::SettingsRoute::{About, SoftwareUpdate};
    for (path, successor, layout, route) in [
        (
            "cold canonical",
            false,
            window_of(vec![settings_leaf()]),
            About,
        ),
        (
            "cold legacy",
            false,
            legacy_native_window(updates_settings()),
            SoftwareUpdate,
        ),
        ("canonical", true, window_of(vec![settings_leaf()]), About),
        (
            "legacy",
            true,
            legacy_native_window(updates_settings()),
            SoftwareUpdate,
        ),
    ] {
        let mut app = App::headless_for_test();
        app.handoff_successor = successor;
        app.restore_into_window(WindowId(0), layout);
        let window = &app.windows[&WindowId(0)];
        assert_eq!(window.tab_set.len(), 1, "{path}: only the Settings tab");
        assert!(window.layouts.is_empty(), "{path}");
        assert!(window.front_terminal().is_none(), "{path}");
        assert_eq!(app.focused_session_id(WindowId(0)), None, "{path}");
        assert!(
            app.pool.get(0).is_none(),
            "{path}: the fresh bootstrap retired"
        );
        assert!(
            app.store.read().unwrap().snapshot().is_empty(),
            "{path}: no session registered in its place"
        );
        let (_, view) = app
            .active_native_view(WindowId(0))
            .expect("Settings restored");
        assert_eq!(
            app.windows[&WindowId(0)].window_focus,
            crate::front_content::WindowFocus::Content(view),
            "{path}: the restored native view owns window focus"
        );
        assert!(
            matches!(
                app.native_runtime.view_state(view),
                Some(crate::native_app::AppViewState::Settings(state)) if state.route == route
            ),
            "{path}: Settings reopens on the page it was showing"
        );
        assert!(app.structural_invariants_ok(), "{path}");
    }
}

#[test]
fn missing_native_document_skips_without_placeholder_or_id_reuse() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let old_active = app.windows[&wid].tab_set.active_id();
    let layout = legacy_native_window(restore::NativeTabRestore::Markdown {
        // Distinct branch.
        uri: "file:///definitely/missing/aterm-restore-document.md".to_string(),
    });
    app.restore_into_window(wid, layout);
    let ws = &app.windows[&wid];
    assert_eq!(ws.tab_set.len(), 1, "safe bootstrap remains usable");
    assert_eq!(ws.tab_set.active_id(), old_active);
    assert_eq!(ws.layouts.len(), 1);
    assert!(
        app.native_runtime
            .instance_by_kind(crate::native_app::AppKind::Markdown)
            .is_none()
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn recursive_mixed_restore_keeps_topology_and_substitutes_only_failed_leaf() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let restored = restore::RestoredTab {
        root: restore::RestoredSplitTree::Split {
            axis: restore::SplitKind::Horizontal,
            ratio: 0.72,
            first: Box::new(restore::RestoredSplitTree::leaf(
                restore::RestoredView::Terminal(restore::TerminalLeafRestore {
                    cwd: Some("/tmp".to_string()),
                    title: "Build shell".to_string(),
                    local_id: None,
                    ..bare_leaf()
                }),
            )),
            second: Box::new(restore::RestoredSplitTree::leaf(
                restore::RestoredView::Native(restore::NativeLeafRestore::document(
                    "markdown",
                    "file:///definitely/missing/recursive-restore.md".to_string(),
                )),
            )),
        },
        focused_path: vec![restore::RestoreBranch::Second],
        zoomed: true,
    };
    app.restore_into_window(
        wid,
        restore::WindowLayout {
            restored_tabs: vec![restored],
            ..single_leaf_window(bare_leaf())
        },
    );

    let tab = app.windows[&wid].tab_set.active().expect("restored tab");
    let crate::tab_model::SplitTree::Split {
        axis,
        ratio,
        first,
        second,
    } = &tab.root
    else {
        panic!("recursive split topology was flattened");
    };
    assert_eq!(*axis, crate::tab_model::SplitAxis::Horizontal);
    assert!((*ratio - 0.72).abs() < f32::EPSILON);
    let crate::tab_model::SplitTree::Leaf(terminal_view) = &**first else {
        panic!("healthy terminal sibling moved");
    };
    let crate::tab_model::SplitTree::Leaf(recovery_view) = &**second else {
        panic!("failed document position moved");
    };
    assert!(matches!(
        app.view_store.get(*terminal_view),
        Some(crate::tab_model::View::Terminal(_))
    ));
    let crate::tab_model::View::Native(recovery) =
        app.view_store.get(*recovery_view).copied().unwrap()
    else {
        panic!("missing document did not become a renderable native placeholder");
    };
    assert_eq!(
        app.native_runtime
            .app(recovery.instance)
            .map(|app| app.kind()),
        Some(crate::native_app::AppKind::Recovery)
    );
    assert_eq!(tab.focus, *recovery_view);
    assert!(tab.zoomed);
    assert!(tab.presentation.indicators.attention);
    assert_eq!(
        app.windows[&wid].layouts.len(),
        0,
        "mixed trees are canonical"
    );
    assert!(
        app.pool.get(0).is_some(),
        "healthy bootstrap shell was grafted"
    );
    assert!(app.structural_invariants_ok());
}

fn restored_terminal(local: u64, title: &str) -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
        restore::TerminalLeafRestore {
            cwd: Some("/tmp".to_string()),
            title: title.to_string(),
            local_id: Some(local),
            user_title: Some(title.to_string()),
            ..bare_leaf()
        },
    ))
}

#[test]
fn a_descending_leaf_run_restores_every_tab_onto_its_own_shell() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.restore_into_window(
        wid,
        restore::WindowLayout {
            restored_tabs: vec![
                restore::RestoredTab {
                    root: restored_terminal(0, "ALPHA"),
                    focused_path: Vec::new(),
                    zoomed: false,
                },
                restore::RestoredTab {
                    root: restore::RestoredSplitTree::Split {
                        axis: restore::SplitKind::Vertical,
                        ratio: 0.5,
                        first: Box::new(restored_terminal(1, "BRAVO")),
                        second: Box::new(restored_terminal(3, "DELTA")),
                    },
                    focused_path: vec![restore::RestoreBranch::Second],
                    zoomed: false,
                },
                restore::RestoredTab {
                    root: restored_terminal(2, "CHARLIE"),
                    focused_path: Vec::new(),
                    zoomed: false,
                },
            ],
            ..single_leaf_window(bare_leaf())
        },
    );

    let tabs = app.windows[&wid].tab_set.tabs().to_vec();
    assert_eq!(tabs.len(), 3, "every saved tab came back");
    let leaves = tabs
        .iter()
        .flat_map(|tab| tab.root.leaves())
        .collect::<Vec<_>>();
    assert_eq!(leaves.len(), 4, "the split tab kept both of its panes");
    let sessions = leaves
        .iter()
        .map(|view| {
            app.view_store
                .get(*view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                .expect("every restored leaf is a terminal")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sessions
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        sessions.len(),
        "two leaves share one shell: {sessions:?}"
    );
    for id in &sessions {
        assert!(app.pool.get(*id).is_some(), "restored session {id} is live");
        assert_eq!(
            app.pool.views(*id),
            Some(1),
            "restored session {id} backs exactly one leaf"
        );
    }
    assert!(app.restored_window_sessions_are_distinct(wid));

    let titles = sessions
        .iter()
        .skip(1)
        .map(|id| {
            app.pool
                .get(*id)
                .expect("restored session")
                .ctx
                .meta
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .user_title
                .clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        vec![
            Some("BRAVO".to_string()),
            Some("DELTA".to_string()),
            Some("CHARLIE".to_string()),
        ],
        "a respawned leaf must wear the metadata of the shell it stands for"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn manual_editor_identity_survives_capture_restore_and_blocks_malformed_save() {
    const ROOT: &str = "ATERM_CONFIG_RESTORE_IDENTITY_ROOT";
    if !crate::test_alone::in_child() {
        let root = aterm_tempfile::TempDir::new().unwrap();
        crate::test_alone::run_alone_with(&[
            (ROOT, root.path().as_os_str()),
            ("XDG_CONFIG_HOME", root.path().as_os_str()),
        ]);
        return;
    }

    let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
    let config = root.join("aterm/aterm.toml");
    let mut source = App::headless_for_test();
    source
        .open_config_editor_in_front()
        .expect("Open aterm.toml opens the editor");
    assert!(config.is_file(), "Manual resolves the isolated aterm.toml");
    let (_, source_view) = source.active_native_view(WindowId(0)).unwrap();
    let descriptor = match source.view_restore_descriptor(source_view).unwrap() {
        restore::RestoredView::Native(descriptor) => descriptor,
        other => panic!("expected native config Editor, got {other:?}"),
    };
    assert_eq!(descriptor.restore_tag, "editor");
    assert!(descriptor.config_editor);

    let mut retry = App::headless_for_test();
    let retried = retry.execute_recovery_request(
        WindowId(0),
        crate::native_app::RecoveryRequest::Retry(
            crate::native_app::RecoveryCapability::Document {
                kind: crate::native_app::AppKind::Editor,
                uri: descriptor.uri.clone().unwrap(),
                config_editor: true,
            },
        ),
    );
    assert!(matches!(
        retried,
        crate::native_app::RecoveryOutcome::Opened { .. }
    ));
    let (retry_instance, _) = retry.active_native_view(WindowId(0)).unwrap();
    assert!(
        retry.native_runtime.config_editor_enabled(retry_instance),
        "Recovery Retry preserves Manual's privileged editor reducer"
    );

    let mut restored = App::headless_for_test();
    restored.restore_into_window(WindowId(0), window_of(vec![native_leaf(descriptor)]));
    let (instance, view) = restored
        .active_native_view(WindowId(0))
        .expect("config Editor restored");
    assert!(restored.native_runtime.config_editor_enabled(instance));
    let document = restored.native_runtime.document_id(instance).unwrap();
    restored
        .dispatch_native_event(
            WindowId(0),
            crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
                "font_px = \"not a number\"\n".to_string(),
            )),
        )
        .unwrap();
    assert!(
        restored
            .native_runtime
            .config_editor_save_error(document)
            .is_some(),
        "restored Manual retains the config save gate"
    );
    let save = restored
        .native_runtime
        .commands(instance, view)
        .unwrap()
        .into_iter()
        .find(|command| command.id.as_str() == "editor/save")
        .unwrap();
    assert!(!save.enabled, "malformed restored Manual cannot Save");
}

#[test]
fn manual_settings_restore_is_the_manual_page_with_or_without_singleton() {
    let wid = WindowId(0);
    let descriptor = restore::NativeLeafRestore::settings("/manual".to_string());
    for preinstall_singleton in [false, true] {
        let mut app = App::headless_for_test();
        if preinstall_singleton {
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        }
        let (view, _, _) = app.restore_native_leaf(wid, &descriptor).unwrap();
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::Manual
        ));
        assert!(
            app.native_runtime.config_editor_document().is_none(),
            "restoring the page opens no editor"
        );
        app.remove_view_link(view);
    }
}

#[cfg(unix)]
#[test]
fn a_restored_harness_page_reads_the_ledgers_once() {
    let wid = WindowId(0);
    for (path, pending, asked, in_all) in [
        ("/harness", false, 1, 1),
        ("/monitor", false, 0, 0),
        ("/about", false, 0, 0),
        ("/harness", true, 0, 1),
        ("/about", true, 0, 0),
    ] {
        let context = format!("{path} (an update's successor before Commit: {pending})");
        let mut app = App::headless_for_test();
        app.incoming_handoff_pending = pending;
        let descriptor = restore::NativeLeafRestore::settings(path.to_string());
        let (view, _, _) = app.restore_native_leaf(wid, &descriptor).unwrap();
        assert_eq!(app.harness_screen.readings_asked, asked, "{context}");
        if pending {
            app.harness_screen.stats.collecting = true;
            app.incoming_handoff_pending = false;
            app.settle_harness_reading_at_commit();
            assert!(
                !app.harness_screen.stats.collecting,
                "{context}: the reading begun before the Commit let go"
            );
        }
        assert_eq!(
            app.harness_screen.readings_asked, in_all,
            "{context}: and at the Commit"
        );
        app.remove_view_link(view);
    }
}

#[test]
fn recursive_editor_restore_clamps_utf8_view_offsets() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-recursive-editor-restore-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("utf8.md");
    std::fs::write(&path, "éx").unwrap();
    let mut descriptor = restore::NativeLeafRestore::document("editor", file_uri(&path));
    descriptor.editor_selections = vec![restore::RestoreSelection {
        anchor: 1,
        head: usize::MAX,
    }];
    descriptor.viewport_anchor = 1;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.restore_into_window(wid, window_of(vec![native_leaf(descriptor)]));
    let (_, view) = app.active_native_view(wid).expect("Editor restored");
    let Some(crate::native_app::AppViewState::Editor(editor)) = app.native_runtime.view_state(view)
    else {
        panic!("restored view is not Editor");
    };
    let buffer = editor.buffer.as_ref().expect("Editor buffer attached");
    assert_eq!(buffer.selections[0].anchor, 0, "mid-codepoint clamps down");
    assert_eq!(
        buffer.selections[0].head, 3,
        "oversized offset clamps to EOF"
    );
    assert_eq!(buffer.viewport_anchor, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn remint_carried_connections_remints_kinds_and_drops_the_unmintable() {
    let carry = |src: &str, dst: &str, op: &str| crate::session_store::ConnectionCarry {
        src: src.to_string(),
        dst: dst.to_string(),
        op: op.to_string(),
    };
    let mut app = App::headless_for_test();
    add_stub(&mut app, 1);
    let (a, b) = {
        let g = app.store.read().unwrap();
        (
            g.by_local(0).expect("session 0").sid.clone(),
            g.by_local(1).expect("session 1").sid.clone(),
        )
    };
    app.pending_conn_carry = vec![
        carry(a.as_str(), b.as_str(), "read-screen"),
        carry(a.as_str(), b.as_str(), "signal"),
        carry(a.as_str(), b.as_str(), "write-input"),
        carry(b.as_str(), a.as_str(), "write-input"),
        carry(a.as_str(), b.as_str(), "config-write"),
        carry("s-gone", b.as_str(), "read-screen"),
    ];

    app.remint_carried_connections();

    assert!(
        app.pending_conn_carry.is_empty(),
        "the carry drains (one-shot)"
    );
    let rows = {
        let ctx = {
            let g = app.store.read().unwrap();
            g.by_sid(&b).unwrap().ctx.clone()
        };
        let edges = ctx.edges.lock().unwrap();
        edges.edges()
    };
    assert_eq!(rows.len(), 3, "both = 3 ops, nothing widened: {rows:?}");
    assert!(rows.iter().all(|e| e.src == a && e.dst == b));
    let a_rows = {
        let ctx = {
            let g = app.store.read().unwrap();
            g.by_sid(&a).unwrap().ctx.clone()
        };
        let edges = ctx.edges.lock().unwrap();
        edges.edges()
    };
    assert!(
        a_rows.is_empty(),
        "the lone push half must not mint: {a_rows:?}"
    );
    {
        let records = app.connections.records();
        assert_eq!(records.len(), 1);
        let rec = records
            .get(&(a.clone(), b.clone()))
            .expect("the a → b record is back");
        assert_eq!(rec.kind(), Some(aterm_session::ConnectionKind::Both));
    }
    app.remint_carried_connections();
    assert_eq!(app.connections.records().len(), 1);
}

fn single_leaf_window(leaf: restore::TerminalLeafRestore) -> restore::WindowLayout {
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
        restored_tabs: vec![restore::RestoredTab {
            root: restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(leaf)),
            focused_path: Vec::new(),
            zoomed: false,
        }],
    }
}

fn bare_leaf() -> restore::TerminalLeafRestore {
    restore::TerminalLeafRestore {
        sid: None,
        lineage: None,
        log_ep: None,
        cwd: None,
        title: String::new(),
        profile: None,
        local_id: Some(0),
        user_title: None,
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
        holds: Vec::new(),
    }
}

fn legacy_native_window(native: restore::NativeTabRestore) -> restore::WindowLayout {
    restore::WindowLayout {
        native_tabs: vec![native],
        tab_order: vec![restore::TabOrderEntry::Native { index: 0 }],
        restored_tabs: Vec::new(),
        ..single_leaf_window(bare_leaf())
    }
}

fn updates_settings() -> restore::NativeTabRestore {
    restore::NativeTabRestore::Settings {
        route: "/updates".to_string(),
    }
}

fn adopt_as(app: &mut App, session: u64, parent_id: u64) {
    app.pool
        .sessions
        .get_mut(&session)
        .expect("live session")
        .session
        .handoff_local_id = Some(parent_id);
}

fn registry_meta(app: &App, session: u64) -> crate::session_timeline::SessionMeta {
    let sid = app
        .pool
        .get(session)
        .expect("the session survived the restore")
        .ctx
        .self_id
        .clone();
    let store = app.store.read().unwrap();
    let handle = store.by_sid(&sid).expect("the sid still resolves");
    handle.ctx.meta.lock().unwrap().clone()
}

#[test]
fn a_handoff_keeps_user_metadata_on_every_adopted_window_bootstrap() {
    use crate::session_timeline::{MetaEdit, MetaField, SessionMeta, write_session_meta};

    let old = App::headless_for_test();
    let ctx = old.pool.get(0).expect("live session").ctx.clone();
    for (field, value) in [
        (MetaField::Title, "fable driver"),
        (MetaField::Description, "drives the satcomp worker"),
        (MetaField::Icon, "🦊"),
        (MetaField::Role, "agent:claude-driver-fable"),
        (MetaField::Attention, "waiting on a human review"),
        (MetaField::Questions, "recommended"),
    ] {
        assert_eq!(
            write_session_meta(&ctx, field, MetaEdit::Set(value), aterm_slog::Actor::Ctl),
            Ok(true),
            "`meta set {}` stores",
            field.wire_name()
        );
    }
    let driver = SessionMeta {
        user_title: Some("fable driver".to_string()),
        description: Some("drives the satcomp worker".to_string()),
        icon: Some("🦊".to_string()),
        role: Some("agent:claude-driver-fable".to_string()),
        attention: Some("waiting on a human review".to_string()),
        questions: Some("recommended".to_string()),
        ..SessionMeta::default()
    };
    assert_eq!(registry_meta(&old, 0), driver, "PRECONDITION: stamped");

    let captured = old.capture_restore_manifest();
    let wire = captured
        .to_toml()
        .expect("the parent serializes its layout");
    let carried = restore::RestoreManifest::from_toml(&wire)
        .filter(|layout| layout.covers_exact_seamless_ids(&[0]))
        .expect("the child accepts the sidecar");
    assert_eq!(
        carried, captured,
        "neither the wire nor the parse drops a field"
    );

    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    adopt_as(&mut new, 0, 0);
    assert!(
        !registry_meta(&new, 0).any_set(),
        "PRECONDITION: an adopted session arrives with a fresh ctx"
    );
    new.restore_into_window(
        WindowId(0),
        carried.windows.into_iter().next().expect("window 0"),
    );
    assert_eq!(
        new.focused_session_id(WindowId(0)),
        Some(0),
        "the leaf was filled by the ADOPTED session, not a respawn"
    );
    assert_eq!(
        registry_meta(&new, 0),
        driver,
        "all six fields survive onto the adopted session"
    );
    assert_eq!(
        sids_cleared(new.capture_restore_manifest()),
        sids_cleared(captured)
    );
    assert!(new.structural_invariants_ok());
}

fn sids_cleared(mut manifest: restore::RestoreManifest) -> restore::RestoreManifest {
    fn clear(tree: &mut restore::RestoredSplitTree) {
        match tree {
            restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } => {
                leaf.sid = None;
                leaf.log_ep = None;
            }
            restore::RestoredSplitTree::Leaf { .. } => {}
            restore::RestoredSplitTree::Split { first, second, .. } => {
                clear(first);
                clear(second);
            }
        }
    }
    for window in &mut manifest.windows {
        for tab in &mut window.restored_tabs {
            clear(&mut tab.root);
        }
    }
    manifest
}

#[test]
fn a_graft_leaves_every_field_the_leaf_does_not_carry_unset() {
    use crate::session_timeline::SessionMeta;

    let stale = SessionMeta {
        user_title: Some("someone else".to_string()),
        description: Some("a leaf that failed to build".to_string()),
        icon: Some("🧟".to_string()),
        role: Some("operator".to_string()),
        attention: Some("stale escalation".to_string()),
        ..SessionMeta::default()
    };
    let mut app = App::headless_for_test();
    *app.pool.get(0).unwrap().ctx.meta.lock().unwrap() = stale.clone();
    let mut leaf = bare_leaf();
    leaf.role = Some("worker:claude-satcomp".to_string());
    app.restore_into_window(WindowId(0), single_leaf_window(leaf));
    assert_eq!(app.focused_session_id(WindowId(0)), Some(0));
    assert_eq!(
        registry_meta(&app, 0),
        SessionMeta {
            role: Some("worker:claude-satcomp".to_string()),
            ..SessionMeta::default()
        },
        "only the leaf's own field is set"
    );

    let mut app = App::headless_for_test();
    *app.pool.get(0).unwrap().ctx.meta.lock().unwrap() = stale;
    app.restore_into_window(WindowId(0), single_leaf_window(bare_leaf()));
    assert!(
        !registry_meta(&app, 0).any_set(),
        "{:?}",
        registry_meta(&app, 0)
    );
}

#[test]
fn a_graft_recomposes_the_chrome_the_first_present_cached() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    feed_session0(&app, b"\x1b]2;zsh\x07");
    app.refresh_window_tabs(wid);
    let tooltip = |app: &App| {
        app.windows[&wid]
            .tab_set
            .active()
            .and_then(|tab| tab.presentation.tooltip.clone())
    };
    assert!(
        !tooltip(&app).is_some_and(|text| text.contains('🚀')),
        "PRECONDITION: {:?}",
        tooltip(&app)
    );

    let mut leaf = bare_leaf();
    leaf.title = "zsh".to_string();
    leaf.icon = Some("🚀".to_string());
    app.restore_into_window(wid, single_leaf_window(leaf));
    app.refresh_window_tabs(wid);
    assert_eq!(app.tab_titles(wid), vec!["zsh".to_string()]);
    assert!(
        tooltip(&app).is_some_and(|text| text.starts_with('🚀')),
        "the seeded icon reaches the tooltip: {:?}",
        tooltip(&app)
    );
}

fn driver_meta(
    app: &App,
    session: u64,
    field: crate::session_timeline::MetaField,
    edit: crate::session_timeline::MetaEdit<'_>,
) {
    let sid = app
        .pool
        .get(session)
        .expect("live session")
        .ctx
        .self_id
        .clone();
    let ctx = app
        .store
        .read()
        .unwrap()
        .by_sid(&sid)
        .expect("the sid resolves")
        .ctx
        .clone();
    assert!(
        crate::session_timeline::write_session_meta(&ctx, field, edit, aterm_slog::Actor::Ctl)
            .is_ok(),
        "`meta` answers OK to {field:?} {edit:?}"
    );
}

#[test]
fn a_graft_keeps_every_field_a_driver_wrote_before_the_restore_reached_it() {
    use crate::session_timeline::{MetaEdit, MetaField, SessionMeta};

    for adopted in [true, false] {
        let mut app = App::headless_for_test();
        if adopted {
            app.handoff_successor = true;
            adopt_as(&mut app, 0, 0);
        }
        driver_meta(&app, 0, MetaField::Role, MetaEdit::Set("worker:new"));
        driver_meta(
            &app,
            0,
            MetaField::Attention,
            MetaEdit::Set("fresh escalation"),
        );
        driver_meta(&app, 0, MetaField::Description, MetaEdit::Clear);
        let mut leaf = bare_leaf();
        leaf.user_title = Some("carried title".to_string());
        leaf.description = Some("carried notes".to_string());
        leaf.role = Some("worker:old".to_string());
        leaf.attention = Some("old escalation".to_string());
        app.restore_into_window(WindowId(0), single_leaf_window(leaf));
        assert_eq!(app.focused_session_id(WindowId(0)), Some(0));

        let meta = registry_meta(&app, 0);
        assert_eq!(
            (meta.role.as_deref(), meta.attention.as_deref()),
            (Some("worker:new"), Some("fresh escalation")),
            "adopted={adopted}: the two `meta set`s stand"
        );
        assert_eq!(
            meta,
            SessionMeta {
                user_title: Some("carried title".to_string()),
                role: Some("worker:new".to_string()),
                attention: Some("fresh escalation".to_string()),
                ..SessionMeta::default()
            },
            "adopted={adopted}: the `meta unset description` stands too, and \
             only the title nobody wrote is carried"
        );
    }
}

#[test]
fn a_graft_carries_a_leaf_only_onto_the_shell_that_leaf_names() {
    use crate::session_timeline::{MetaEdit, MetaField, SessionMeta};

    let mut old = App::headless_for_test();
    let mut layout = single_leaf_window(bare_leaf());
    layout.restored_tabs = vec![
        restore::RestoredTab {
            root: split_of(
                placeholder_leaf(),
                restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(bare_leaf())),
            ),
            focused_path: vec![restore::RestoreBranch::Second],
            zoomed: false,
        },
        restore::RestoredTab {
            root: restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(bare_leaf())),
            focused_path: Vec::new(),
            zoomed: false,
        },
    ];
    old.restore_into_window(WindowId(0), layout);
    driver_meta(&old, 0, MetaField::Role, MetaEdit::Set("operator"));
    driver_meta(&old, 1, MetaField::Role, MetaEdit::Set("worker"));
    let wire = old
        .capture_restore_manifest()
        .to_toml()
        .expect("the parent serializes its layout");
    let carried = restore::RestoreManifest::from_toml(&wire)
        .filter(|layout| layout.covers_exact_seamless_ids(&[0, 1]))
        .expect("the child accepts the sidecar");
    let window = carried.windows[0].clone();
    let mirror_first = window
        .tabs
        .first()
        .and_then(|tab| tab.leaves().first().and_then(|leaf| leaf.local_id()));
    assert_eq!(
        (mirror_first, window.bootstrap_local_id()),
        (Some(1), Some(0)),
        "PRECONDITION: the mirror's first leaf is tab B's shell; the graft fills \
         tab A's"
    );

    let operator = SessionMeta {
        role: Some("operator".to_string()),
        ..SessionMeta::default()
    };
    let worker = SessionMeta {
        role: Some("worker".to_string()),
        ..SessionMeta::default()
    };
    let tab_sessions = |app: &App| -> Vec<Vec<u64>> {
        app.windows[&WindowId(0)]
            .tab_set
            .tabs()
            .iter()
            .map(|tab| {
                tab.root
                    .leaves()
                    .into_iter()
                    .filter_map(|view| {
                        app.view_store
                            .get(view)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                    })
                    .collect()
            })
            .collect()
    };

    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    adopt_as(&mut new, 0, 0);
    new.seamless_adopt = vec![handed_off_shell(1)];
    new.restore_into_window(WindowId(0), window.clone());
    let placed = tab_sessions(&new);
    assert_eq!(placed[0], vec![0], "tab A's terminal pane is session 0");
    assert_eq!(registry_meta(&new, 0), operator);
    assert_eq!(
        new.pool.get(placed[1][0]).and_then(|s| s.handoff_local_id),
        Some(1),
        "tab B's pane adopted shell 1"
    );
    assert_eq!(registry_meta(&new, placed[1][0]), worker);

    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    adopt_as(&mut new, 0, 1);
    new.seamless_adopt = vec![handed_off_shell(0)];
    new.restore_into_window(WindowId(0), window.clone());
    let placed = tab_sessions(&new);
    assert_eq!(placed[0], vec![0]);
    assert_eq!(
        registry_meta(&new, 0),
        worker,
        "a leaf naming shell 0 stamped its identity on shell 1, or shell 1 lost its own"
    );
    assert_eq!(
        placed[1],
        vec![0],
        "tab B's pane shows shell 1, the shell its leaf names"
    );

    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    new.seamless_adopt = vec![handed_off_shell(1)];
    new.restore_into_window(WindowId(0), window);
    let placed = tab_sessions(&new);
    assert_eq!(placed[0], vec![0]);
    assert_eq!(registry_meta(&new, 0), SessionMeta::default());
    assert_eq!(registry_meta(&new, placed[1][0]), worker);
}

#[test]
fn a_handed_shells_frozen_flag_reaches_both_adoption_paths() {
    let state = std::env::temp_dir().join(format!(
        "aterm-restore-frozen-{}-{}",
        std::process::id(),
        aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&state).unwrap();
    let shell = |local_id: u64| {
        let mut shell = handed_off_shell(local_id);
        shell.frozen_path = true;
        shell
    };
    type Row = (Option<u64>, bool, bool);
    let rows = |app: &App| -> Vec<Row> {
        let store = app.store.read().unwrap();
        let mut rows: Vec<Row> = app
            .pool
            .sessions
            .iter()
            .map(|(&session, pooled)| {
                (
                    pooled.session.handoff_local_id,
                    pooled.session.frozen_path,
                    store.has_frozen_path(session),
                )
            })
            .collect();
        rows.sort_unstable();
        rows
    };
    let carried = |id: u64| -> Row { (Some(id), true, true) };
    let plain = |id: Option<u64>| -> Row { (id, false, false) };
    crate::test_env::scoped("ATERM_STATE_HOME", &state, || {
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        new.seamless_adopt = vec![shell(3)];
        assert_eq!(new.store.read().unwrap().frozen_path_tabs(), 0);
        assert_eq!(new.frozen_path_tabs(), 1, "the pending adoptee is counted");
        new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0), leaf_naming(3)]));
        assert!(new.seamless_adopt.is_empty(), "the leaf adopted shell 3");
        assert_eq!(rows(&new), vec![plain(None), carried(3)]);
        assert_eq!(new.store.read().unwrap().frozen_path_tabs(), 1);
        assert_eq!(new.frozen_path_tabs(), 1, "registered now, counted once");

        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0)]));
        new.seamless_adopt = vec![shell(1), handed_off_shell(2)];
        new.adopt_orphan_shells_as_tabs(&[]);
        assert!(new.seamless_adopt.is_empty(), "the net placed every orphan");
        assert_eq!(rows(&new), vec![plain(None), carried(1), plain(Some(2))]);
        assert_eq!(new.store.read().unwrap().frozen_path_tabs(), 1);
    });
    let _ = std::fs::remove_dir_all(&state);
}

fn dead_agent() -> restore::AgentRestore {
    restore::AgentRestore {
        word: String::new(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
        cwd: "/tmp".into(),
        version: Some("2.1.283".into()),
        home: None,
        login: None,
        run: None,
        cause: None,
        tab: None,
    }
}

fn leaf_hosting(shell: u64, agent: Option<restore::AgentRestore>) -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
        restore::TerminalLeafRestore {
            local_id: Some(shell),
            agent: agent.map(Box::new),
            ..bare_leaf()
        },
    ))
}

#[test]
fn a_restored_leafs_agent_is_handed_with_the_tab_it_ran_in() {
    let old = "s-00000000000000000001";
    let agent = restore::AgentRestore {
        tab: Some(old.into()),
        ..dead_agent()
    };
    let mut cold = App::headless_for_test();
    cold.restore_into_window(WindowId(0), window_of(vec![leaf_hosting(8, Some(agent))]));
    for (&id, pooled) in cold.pool.sessions.iter_mut() {
        pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
    }
    let handed = cold.take_restored_agents();
    assert_eq!(handed.len(), 1, "{handed:?}");
    let (sid, _, _, was) = &handed[0];
    assert_ne!(sid, old, "a new sid");
    assert_eq!(was.as_deref(), Some(old));
}

#[cfg(unix)]
const OWED_HOME: &str = "/Users//me/lab home/.claude";
#[cfg(unix)]
const OWED_LOGIN: &str = "/Users//me/logins/work-2";

#[cfg(unix)]
fn owed_relaunch(tab: &str) -> aterm_agent::harness::relaunch::Snapshot {
    aterm_agent::harness::relaunch::Snapshot {
        stored_login: false,
        profile: None,
        tab: tab.to_string(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        shell: 4343,
        program: std::path::PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
        cwd: "/tmp".into(),
        version: None,
        run: aterm_agent::harness::relaunch::AgentRun::Claude,
        dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
        login: Some(OWED_LOGIN.into()),
        home: Some(OWED_HOME.into()),
        claude_dir: None,
    }
}

#[cfg(unix)]
#[test]
fn a_durable_layout_names_the_conversation_the_agents_record_names_as_it_is_taken() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-restore-followed-")
        .tempdir()
        .expect("temp dir");
    let dir = tmp.path().join(".claude");
    std::fs::create_dir_all(dir.join("sessions")).expect("the records' dir");
    let mut app = App::headless_for_test();
    let sid = {
        let g = app.store.read().unwrap();
        g.by_local(0).expect("registered").sid.as_str().to_string()
    };
    let copy = aterm_agent::harness::relaunch::Snapshot {
        claude_dir: Some(dir.clone()),
        ..owed_relaunch(&sid)
    };
    let left = copy.session.clone().expect("the conversation it left");
    let cleared = "0b6f3c1e-7777-4b61-9d52-7f1e2c3a4b5c";
    let record = |session: &str, start: &str| {
        std::fs::write(
            dir.join("sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"{session}","cwd":"/tmp","version":"2.1.300","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
            ),
        )
        .expect("the record");
    };
    let captured = |app: &mut App| -> restore::AgentRestore {
        let mut layout = app.capture_restore_manifest();
        app.fill_desk_agents(&mut layout);
        let wire = layout.to_toml().expect("the wire");
        let layout = restore::RestoreManifest::from_toml(&wire).expect("read back");
        *layout
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .and_then(|leaf| leaf.agent)
            .expect("the agent rides its leaf")
    };
    let conversation = |app: &mut App| captured(app).session;
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&copy));

    record(cleared, &copy.start);
    let named = captured(&mut app);
    assert_eq!(
        named.session.as_deref(),
        Some(cleared),
        "the conversation the agent is in"
    );
    let mut cold = App::headless_for_test();
    cold.restore_into_window(WindowId(0), window_of(vec![leaf_hosting(8, Some(named))]));
    for (&id, pooled) in cold.pool.sessions.iter_mut() {
        pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
    }
    let handed = cold.take_restored_agents();
    assert_eq!(handed.len(), 1, "{handed:?}");
    assert_eq!(
        handed[0].1.session.as_deref(),
        Some(cleared),
        "the cold restore resumes it"
    );

    record(cleared, "Mon Jan  1 00:00:00 2024");
    assert_eq!(
        conversation(&mut app),
        Some(left.clone()),
        "another process's record"
    );
    std::fs::remove_file(dir.join("sessions/4242.json")).expect("removed");
    assert_eq!(conversation(&mut app), Some(left.clone()), "no record");
    record("not-a-session", &copy.start);
    assert_eq!(
        conversation(&mut app),
        Some(left.clone()),
        "no conversation named"
    );
    record(cleared, &copy.start);
    let unread = aterm_agent::harness::relaunch::Snapshot {
        claude_dir: None,
        home: Some(dir.to_string_lossy().into_owned()),
        ..copy.clone()
    };
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&unread));
    assert_eq!(
        conversation(&mut app),
        Some(left.clone()),
        "a snapshot read in no directory"
    );
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&copy));
    record(&left, &copy.start);
    assert_eq!(conversation(&mut app), Some(left), "the record names it");
    crate::harness_host::publish_snapshot_for_test(&sid, None);
}

#[test]
fn a_restored_leafs_agent_is_handed_to_its_new_tab_once() {
    let agent = dead_agent();
    let mut cold = App::headless_for_test();
    cold.restore_into_window(
        WindowId(0),
        window_of(vec![
            leaf_hosting(7, None),
            leaf_hosting(8, Some(agent.clone())),
        ]),
    );
    let mut probe = App::headless_for_test();
    probe.restore_into_window(
        WindowId(0),
        window_of(vec![leaf_hosting(8, Some(agent.clone()))]),
    );
    assert!(
        probe.take_restored_agents().is_empty(),
        "no shell, no relaunch"
    );
    for (&id, pooled) in cold.pool.sessions.iter_mut() {
        pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
    }
    let handed = cold.take_restored_agents();
    assert_eq!(handed.len(), 1, "{handed:?}");
    let (sid, snap, _, was) = &handed[0];
    assert_eq!(&snap.tab, sid);
    assert_eq!(was, &None, "a leaf naming no tab names none");
    assert_eq!(snap.session, agent.session);
    assert_eq!(
        (snap.pid, snap.start.as_str()),
        (agent.pid, agent.start.as_str())
    );
    assert_eq!(snap.argv, agent.argv);
    let session = {
        let g = cold.store.read().unwrap();
        g.snapshot()
            .into_iter()
            .find(|h| h.sid.as_str() == sid)
            .map(|h| h.local_id)
            .expect("the tab is a live session")
    };
    assert_eq!(
        i64::from(snap.shell),
        i64::from(cold.pool.sessions[&session].session.pid),
        "the new tab's own shell"
    );
    assert!(cold.take_restored_agents().is_empty(), "handed once");

    let co_viewed = vec![
        window_of(vec![leaf_hosting(8, Some(agent.clone()))]),
        window_of(vec![leaf_hosting(8, Some(agent.clone()))]),
    ];
    let reopened = crate::crash_journal::Reopened {
        manifest: restore::RestoreManifest::new(co_viewed.clone()),
        class: crate::crash_journal::DeathClass::Killed,
        sources: Vec::new(),
        writers: Vec::new(),
        handed: Vec::new(),
        programs: None,
        second_chance: false,
    };
    assert_eq!(reopened.resumed(true), vec![(0, 0)], "the row: one place");
    let mut next = App::headless_for_test();
    let mut windows = co_viewed.into_iter();
    next.restore_into_window(WindowId(0), windows.next().expect("window 0"));
    for layout in windows {
        let bootstrap = crate::stub_session(next.next_session_id);
        App::register_session(&next.store, &bootstrap, None);
        let wid = next.insert_logical_window(bootstrap, 24, 80);
        next.restore_into_window(wid, layout);
    }
    for (&id, pooled) in next.pool.sessions.iter_mut() {
        pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
    }
    let handed = next.take_restored_agents();
    assert_eq!(handed.len(), 1, "one agent, one relaunch: {handed:?}");
    let first_window_shell = {
        let g = next.store.read().unwrap();
        g.by_local(0).expect("registered").sid.as_str().to_string()
    };
    assert_eq!(
        handed[0].0, first_window_shell,
        "in the first window's tab, whose leaf took its bootstrap shell"
    );

    let mut kept = App::headless_for_test();
    kept.seamless_adopt = vec![handed_off_shell(8)];
    kept.restore_into_window(
        WindowId(0),
        window_of(vec![
            leaf_hosting(7, None),
            leaf_hosting(8, Some(agent.clone())),
        ]),
    );
    let bootstrap = crate::stub_session(kept.next_session_id);
    App::register_session(&kept.store, &bootstrap, None);
    let wid = kept.insert_logical_window(bootstrap, 24, 80);
    kept.restore_into_window(wid, window_of(vec![leaf_hosting(8, Some(agent.clone()))]));
    assert!(kept.seamless_adopt.is_empty(), "the kept shell was placed");
    for (&id, pooled) in kept.pool.sessions.iter_mut() {
        pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
    }
    let handed = kept.take_restored_agents();
    assert!(handed.is_empty(), "it still runs: {handed:?}");
}

#[cfg(unix)]
#[test]
fn a_held_tabs_holds_ride_its_leaf_back_onto_its_tab_after_every_end() {
    use crate::fabric::{Hold, apply_hold_for_test};
    let hold = |origin: &str, reason: &str| Hold {
        reason: reason.to_string(),
        origin: origin.to_string(),
    };
    let rows = |rows: &[&str]| rows.iter().map(|r| r.to_string()).collect::<Vec<_>>();
    let sid_of = |app: &App, id: u64| {
        let g = app.store.read().unwrap();
        g.by_local(id).expect("registered").sid.as_str().to_string()
    };
    let id_of = |app: &App, sid: &str| {
        let g = app.store.read().unwrap();
        g.snapshot()
            .into_iter()
            .find(|h| h.sid.as_str() == sid)
            .map(|h| h.local_id)
            .expect("the tab is a live session")
    };
    let ctx_of = |app: &App, id: u64| std::sync::Arc::clone(&app.pool.get(id).expect("pooled").ctx);
    let carried_rows = |app: &App, id: u64| {
        ctx_of(app, id)
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "hold" && e.payload.ends_with(" carried=1"))
            .count()
    };
    let captured = |app: &mut App, ids: &[u64]| {
        let mut layout = app.capture_restore_manifest();
        app.fill_desk_agents(&mut layout);
        let leaves = layout.handed_off_leaves();
        ids.iter()
            .map(|&id| {
                let leaf = leaves
                    .iter()
                    .find(|leaf| leaf.local_id == Some(id))
                    .expect("the session's leaf");
                (leaf.holds.clone(), leaf.agent.is_some())
            })
            .collect::<Vec<_>>()
    };

    let mut app = App::headless_for_test();
    let _ = app.insert_logical_window(crate::stub_session(1), 24, 80);
    let sid = sid_of(&app, 0);
    let mut snap = owed_relaunch(&sid);
    (snap.home, snap.login) = (None, None);
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&snap));
    assert_eq!(
        captured(&mut app, &[0, 1]),
        vec![(Vec::new(), true), (Vec::new(), false)],
        "no hold, none captured"
    );
    assert!(apply_hold_for_test(
        &ctx_of(&app, 0),
        Some(hold("local", "pause"))
    ));
    assert!(apply_hold_for_test(
        &ctx_of(&app, 1),
        Some(hold("local", "menu"))
    ));
    assert_eq!(
        captured(&mut app, &[0, 1]),
        vec![
            (rows(&["local pause"]), true),
            (rows(&["local menu"]), false)
        ],
        "on each tab's own leaf, its agent's or none"
    );
    assert!(apply_hold_for_test(
        &ctx_of(&app, 0),
        Some(hold("fleet", "fabric-lost"))
    ));
    assert_eq!(
        captured(&mut app, &[0])[0],
        (rows(&["fleet fabric-lost", "local pause"]), true),
        "the owner's hold beside a fleet hold"
    );
    crate::harness_host::publish_snapshot_for_test(&sid, None);

    let held_leaf = |shell: u64, pid: u32, row: Option<&str>| {
        restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
            restore::TerminalLeafRestore {
                local_id: Some(shell),
                agent: Some(Box::new(restore::AgentRestore {
                    pid,
                    ..dead_agent()
                })),
                holds: row.iter().map(|r| r.to_string()).collect(),
                ..bare_leaf()
            },
        ))
    };
    let reopened = |leaf_rows: [Option<&str>; 2], before: &dyn Fn(&App)| {
        let mut cold = App::headless_for_test();
        before(&cold);
        cold.restore_into_window(
            WindowId(0),
            window_of(vec![
                held_leaf(8, 4242, leaf_rows[0]),
                held_leaf(9, 4243, leaf_rows[1]),
            ]),
        );
        for (&id, pooled) in cold.pool.sessions.iter_mut() {
            pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
        }
        let held: Vec<(u32, u64)> = cold
            .restored_held
            .iter()
            .map(|(id, agent, ..)| (agent.pid, *id))
            .collect();
        let handed = cold.take_restored_agents();
        let ids: Vec<u64> = [4242_u32, 4243]
            .iter()
            .map(|pid| {
                held.iter()
                    .find(|(p, _)| p == pid)
                    .map(|(_, id)| *id)
                    .or_else(|| {
                        handed
                            .iter()
                            .find(|(_, snap, ..)| snap.pid == *pid)
                            .map(|(sid, ..)| id_of(&cold, sid))
                    })
                    .expect("the tab's session")
            })
            .collect();
        assert_eq!(ids[0], 0, "the first tab grafts the bootstrap");
        let tabs = [4242_u32, 4243]
            .iter()
            .zip(&ids)
            .map(|(pid, &id)| {
                (
                    held.iter().any(|(p, _)| p == pid),
                    ctx_of(&cold, id).fabric.hold(),
                    carried_rows(&cold, id),
                )
            })
            .collect::<Vec<_>>();
        (cold, ids, tabs)
    };
    let (cold, ids, tabs) = reopened([Some("local pause"), Some("local menu")], &|_| {});
    assert_eq!(
        tabs,
        vec![
            (true, Some(hold("local", "pause")), 1),
            (true, Some(hold("local", "menu")), 1)
        ],
        "both held again, recorded as carried, and neither relaunched"
    );
    let (fleet_held, _, tabs) = reopened([Some("local pause"), None], &|cold| {
        assert!(apply_hold_for_test(
            &ctx_of(cold, 0),
            Some(hold("fleet", "fabric-lost"))
        ));
    });
    assert_eq!(
        tabs[0],
        (true, Some(hold("fleet", "fabric-lost")), 1),
        "the fleet's hold stands; the agent is the holder's"
    );
    assert_eq!(
        ctx_of(&fleet_held, 0).fabric.holds().origins(),
        "fleet,local",
        "the owner's hold stands beside the fleet's"
    );
    let (fenced, _, tabs) = reopened([Some("local pause"), None], &|cold| {
        let _ = ctx_of(cold, 0).fabric.fence_hold(true);
    });
    ctx_of(&fenced, 0).fabric.lift_hold_fence();
    assert_eq!(tabs[0], (true, None, 0), "fenced: unmoved, not relaunched");
    let (_, _, tabs) = reopened([Some("fleet fabric-lost"), None], &|_| {});
    assert_eq!(
        tabs,
        vec![(true, None, 0), (false, None, 0)],
        "fleet, and no hold"
    );

    let mut cold = cold;
    cold.say_restored_held();
    assert_eq!(
        captured(&mut cold, &ids),
        vec![
            (rows(&["local pause"]), false),
            (rows(&["local menu"]), false)
        ],
        "the hold outlives its agent's attempt"
    );
    let mut layout = cold.capture_restore_manifest();
    cold.fill_desk_agents(&mut layout);
    let mut again = App::headless_for_test();
    again.restore_into_window(
        WindowId(0),
        layout.windows.into_iter().next().expect("window 0"),
    );
    let held: Vec<Option<Hold>> = (0..again.next_session_id)
        .filter_map(|id| again.pool.get(id).map(|s| s.ctx.fabric.hold()))
        .collect();
    assert!(
        held.contains(&Some(hold("local", "pause"))) && held.contains(&Some(hold("local", "menu"))),
        "both tabs held again: {held:?}"
    );
    assert!(
        again.take_restored_agents().is_empty(),
        "no agent to relaunch"
    );
}

#[test]
fn a_handoff_leaf_with_a_pending_agent_is_relaunched_by_the_successor() {
    let agent = dead_agent();
    let mut successor = App::headless_for_test();
    successor.handoff_successor = true;
    successor.restore_into_window(
        WindowId(0),
        window_of(vec![
            leaf_hosting(7, None),
            leaf_hosting(8, Some(agent.clone())),
            leaf_hosting(9, Some(agent.clone())),
        ]),
    );
    assert!(
        successor.take_restored_agents().is_empty(),
        "nothing before the Commit"
    );
    let mut ids: Vec<u64> = successor.pool.sessions.keys().copied().collect();
    ids.sort_unstable();
    let (seven, eight) = (ids[ids.len() - 2], ids[ids.len() - 1]);
    adopt_as(&mut successor, seven, 7);
    adopt_as(&mut successor, eight, 8);
    for (&id, pooled) in successor.pool.sessions.iter_mut() {
        pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
    }
    successor.queue_carried_restored_agents();
    let handed = successor.take_restored_agents();
    assert_eq!(
        handed.len(),
        1,
        "only the adopted shell 8 owes one: {handed:?}"
    );
    let (sid, snap, _, _) = &handed[0];
    let adopted_sid = {
        let g = successor.store.read().unwrap();
        g.by_local(eight)
            .expect("registered")
            .sid
            .as_str()
            .to_string()
    };
    assert_eq!(sid, &adopted_sid, "the adopted shell's own tab");
    assert_eq!(&snap.tab, sid);
    assert_eq!(
        i64::from(snap.shell),
        i64::from(successor.pool.sessions[&eight].session.pid),
        "relaunched in the shell the outgoing instance was relaunching it in"
    );
    assert_eq!(
        (snap.pid, snap.session.as_deref()),
        (agent.pid, agent.session.as_deref()),
        "the relaunch's guards are the leaf's: its dead agent and its conversation"
    );
    successor.queue_carried_restored_agents();
    assert!(successor.take_restored_agents().is_empty(), "one-shot");
}

#[cfg(unix)]
#[test]
fn the_handoff_layout_carries_the_agent_a_restored_tab_is_still_owed() {
    let tried = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let t = std::sync::Arc::clone(&tried);
    let host = crate::harness_host::HostHandle::for_restored_test(std::sync::Arc::new(
        move |_, _, _, _| {
            t.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            "wait:shell-prompt".to_string()
        },
    ));
    let mut app = App::headless_for_test();
    let _ = app.insert_logical_window(crate::stub_session(1), 24, 80);
    let sid_of = |app: &App, id: u64| {
        let g = app.store.read().unwrap();
        g.by_local(id).expect("registered").sid.as_str().to_string()
    };
    let owed_sid = sid_of(&app, 0);
    host.relaunch_restored(vec![crate::harness_host::RestoredAgent {
        sid: owed_sid.clone(),
        snap: owed_relaunch(&owed_sid),
        place: "in tab 1".into(),
        cold: true,
        asked: None,
        was: None,
    }]);
    app.harness = Some(host.clone());
    let tried_past = |n: usize| {
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the queue's next step",
            || (tried.load(std::sync::atomic::Ordering::SeqCst) > n).then_some(()),
        );
    };
    tried_past(0);
    let agents = |layout: &restore::RestoreManifest| -> Vec<(Option<u64>, Option<u32>)> {
        layout
            .handed_off_leaves()
            .iter()
            .map(|leaf| {
                let agent = leaf.agent.as_ref().inspect(|agent| {
                    assert_eq!(
                        (agent.home.as_deref(), agent.login.as_deref()),
                        (Some(OWED_HOME), Some(OWED_LOGIN)),
                        "the home and the login the agent ran under ride its leaf"
                    );
                });
                (leaf.local_id, agent.map(|agent| agent.pid))
            })
            .collect()
    };
    let journal_dir = crate::crash_journal::tests::scratch("cj-owed");
    app.crash_journal = crate::crash_journal::Lane::new(
        Some(journal_dir.path().to_path_buf()),
        crate::crash_journal::tests::reaped_pid(),
    );
    app.first_present_done = true;
    let journal = app.crash_journal.journal_id();
    app.tick_crash_journal(std::time::Instant::now());
    let (_, image) = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the journal's image",
        || {
            std::fs::read_to_string(journal_dir.path().join(journal.file_name()))
                .ok()
                .and_then(|text| crate::crash_journal::decode(&text).ok())
        },
    );
    assert_eq!(
        agents(&image),
        vec![(Some(0), Some(4242)), (Some(1), None)],
        "the crash journal names the owed agent on its tab's leaf"
    );
    app.crash_journal = crate::crash_journal::Lane::inert();
    crate::crash_journal::tests::await_released(&journal_dir.path().join(journal.lock_name()));
    assert!(app.park_all_readers(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN));
    let parked = app.capture_handoff_layout();
    assert_eq!(
        agents(&parked),
        vec![(Some(0), Some(4242)), (Some(1), None)],
        "the owed tab names its agent; the other tab names none"
    );
    assert_eq!(
        app.capture_handoff_layout(),
        parked,
        "the Commit's re-capture reads what the park froze"
    );
    assert!(
        agents(&app.capture_manifest(false))
            .iter()
            .all(|(_, agent)| agent.is_none()),
        "only the handoff layout names an owed agent"
    );
    let at_park = tried.load(std::sync::atomic::Ordering::SeqCst);
    app.rollback_overlap(None, &[]);
    tried_past(at_park);
    host.shutdown_and_join();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the queue's worker saw the shutdown",
        || host.restored_worker_ended().then_some(()),
    );
    let mut quit = app.capture_restore_manifest();
    app.fill_desk_agents(&mut quit);
    assert_eq!(
        agents(&quit),
        vec![(Some(0), Some(4242)), (Some(1), None)],
        "a restart's quit layout names the owed agent, as the journal did"
    );
}

#[cfg(unix)]
#[test]
fn a_relaunch_owed_between_the_two_captures_refuses_the_commit_by_name() {
    use crate::app_update_handoff::{commit_agent_carry, commit_layout_topology};
    let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
        std::sync::Arc::new(|_, _, _, _| "wait:shell-prompt".to_string()),
    );
    let mut app = App::headless_for_test();
    let _ = app.insert_logical_window(crate::stub_session(1), 24, 80);
    let sid = {
        let g = app.store.read().unwrap();
        g.by_local(0).expect("registered").sid.as_str().to_string()
    };
    app.harness = Some(host.clone());
    assert!(app.park_all_readers(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN));
    let pending = app.capture_handoff_layout();
    host.owe_for_test(&sid, Some(owed_relaunch(&sid)));
    let live = app.capture_handoff_layout();
    assert_eq!(
        commit_layout_topology(&live),
        commit_layout_topology(&pending),
        "the topology did not change"
    );
    assert_ne!(
        commit_agent_carry(&live),
        commit_agent_carry(&pending),
        "what the leaves carry did"
    );
    let pending = app.capture_handoff_layout();
    host.owe_for_test(&sid, None);
    let live = app.capture_handoff_layout();
    assert_eq!(
        commit_layout_topology(&live),
        commit_layout_topology(&pending)
    );
    assert_ne!(commit_agent_carry(&live), commit_agent_carry(&pending));
    assert_eq!(
        commit_agent_carry(&app.capture_handoff_layout()),
        commit_agent_carry(&live)
    );
    app.rollback_overlap(None, &[]);
}

#[cfg(unix)]
#[test]
fn an_owed_relaunch_keeps_its_home_login_and_cause_across_the_commit() {
    use aterm_agent::harness::relaunch::Snapshot;
    use aterm_agent::harness::restart_stamp::Cause;
    let handed = |asked: Option<Cause>| -> (Snapshot, Option<Cause>, bool) {
        let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
            std::sync::Arc::new(|_, _, _, _| "wait:shell-prompt".to_string()),
        );
        let mut app = App::headless_for_test();
        let _ = app.insert_logical_window(crate::stub_session(1), 24, 80);
        let sid = {
            let g = app.store.read().unwrap();
            g.by_local(0).expect("registered").sid.as_str().to_string()
        };
        app.harness = Some(host.clone());
        host.owe_asked_for_test(&sid, Some(owed_relaunch(&sid)), asked);
        assert!(app.park_all_readers(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN));
        let wire = app.capture_handoff_layout().to_toml().expect("the wire");
        app.rollback_overlap(None, &[]);
        let layout = restore::RestoreManifest::from_toml(&wire).expect("the successor reads it");
        let carried = layout
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .and_then(|leaf| leaf.agent)
            .expect("the owed tab's leaf names its agent");
        let mut successor = App::headless_for_test();
        successor.handoff_successor = true;
        successor.restore_into_window(
            WindowId(0),
            window_of(vec![leaf_hosting(8, Some(*carried))]),
        );
        let adopted = *successor.pool.sessions.keys().max().expect("a session");
        adopt_as(&mut successor, adopted, 8);
        for (&id, pooled) in successor.pool.sessions.iter_mut() {
            pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
        }
        successor.queue_carried_restored_agents();
        let mut handed = successor.take_restored_agents();
        assert_eq!(handed.len(), 1, "{handed:?}");
        let (_, snap, asked, _) = handed.remove(0);
        (snap, asked, wire.contains("cause = "))
    };
    let (snap, asked, named) = handed(None);
    assert_eq!(
        (snap.home.as_deref(), snap.login.as_deref()),
        (Some(OWED_HOME), Some(OWED_LOGIN))
    );
    assert_eq!((asked, named), (None, false), "a crash's: no cause");
    for cause in [Cause::Asked, Cause::Stall] {
        let (_, asked, named) = handed(Some(cause));
        assert_eq!((asked, named), (Some(cause), true), "{cause:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_crash_during_a_successful_commits_hold_is_relaunched_by_the_successor() {
    use aterm_agent::harness::relaunch::Snapshot;
    let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
        std::sync::Arc::new(|_, _, _, _| "refused:inert".to_string()),
    );
    let mut app = App::headless_for_test();
    let _ = app.insert_logical_window(crate::stub_session(1), 24, 80);
    let sid = {
        let g = app.store.read().unwrap();
        g.by_local(0).expect("registered").sid.as_str().to_string()
    };
    app.harness = Some(host.clone());
    let supervised = Snapshot {
        tab: sid.clone(),
        home: None,
        login: None,
        ..owed_relaunch(&sid)
    };
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&supervised));
    assert!(app.park_all_readers(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN));
    let wire = app.capture_handoff_layout().to_toml().expect("the wire");
    host.suspend();
    host.shutdown_and_join();
    crate::harness_host::publish_snapshot_for_test(&sid, None);
    app.rollback_overlap(None, &[]);
    let layout = restore::RestoreManifest::from_toml(&wire).expect("the successor reads it");
    let leaf = layout
        .handed_off_leaves()
        .into_iter()
        .find(|leaf| leaf.local_id == Some(0))
        .expect("the tab's leaf");
    let relaunched: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> =
        std::sync::Arc::default();
    let passes: std::sync::Arc<std::sync::atomic::AtomicUsize> = std::sync::Arc::default();
    let successor_host = crate::harness_host::HostHandle::for_successor_test(
        std::sync::Arc::clone(&relaunched),
        std::sync::Arc::clone(&passes),
    );
    let mut successor = App::headless_for_test();
    successor.handoff_successor = true;
    successor.restore_into_window(
        WindowId(0),
        window_of(vec![restore::RestoredSplitTree::leaf(
            restore::RestoredView::Terminal(leaf),
        )]),
    );
    let adopted = *successor.pool.sessions.keys().max().expect("a session");
    adopt_as(&mut successor, adopted, 0);
    for (&id, pooled) in successor.pool.sessions.iter_mut() {
        pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
    }
    let tab = {
        let g = successor.store.read().unwrap();
        g.by_local(adopted)
            .expect("registered")
            .sid
            .as_str()
            .to_string()
    };
    successor.harness = Some(successor_host.clone());
    successor_host.resume_after_handoff(successor.handoff_carry_on_places());
    successor.hand_carried_restored_agents();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the crash during the hold relaunched by the successor",
        || (!relaunched.lock().unwrap().is_empty()).then_some(()),
    );
    let then = passes.load(std::sync::atomic::Ordering::SeqCst);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "two whole passes since, no worker left",
        || {
            crate::harness_host::ring();
            (passes.load(std::sync::atomic::Ordering::SeqCst) >= then + 2
                && successor_host.live().is_empty())
            .then_some(())
        },
    );
    assert_eq!(
        relaunched.lock().unwrap().clone(),
        vec![(tab, "survived".to_string())],
        "(the tab, what the exit left): relaunched once"
    );
    successor_host.shutdown_and_join();
}

#[cfg(unix)]
#[test]
fn the_handoff_leaf_names_the_supervised_agent_where_nothing_is_owed() {
    use crate::app_update_handoff::{commit_agent_carry, commit_layout_topology};
    use aterm_agent::harness::relaunch::Snapshot;
    let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
        std::sync::Arc::new(|_, _, _, _| "wait:shell-prompt".to_string()),
    );
    let mut app = App::headless_for_test();
    let _ = app.insert_logical_window(crate::stub_session(1), 24, 80);
    let sid = {
        let g = app.store.read().unwrap();
        g.by_local(0).expect("registered").sid.as_str().to_string()
    };
    app.harness = Some(host.clone());
    let supervised = Snapshot {
        tab: sid.clone(),
        ..owed_relaunch(&sid)
    };
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&supervised));
    let leaf = |layout: &restore::RestoreManifest| {
        layout
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .expect("the tab's leaf")
    };
    assert!(app.park_all_readers(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN));
    let pending = app.capture_handoff_layout();
    let named = leaf(&pending);
    assert_eq!(
        (named.agent, named.supervised.map(|agent| *agent)),
        (None, Some(restore::AgentRestore::of(supervised.clone()))),
        "(owed, supervised): the handoff leaf names the agent its host supervises"
    );
    let wire = pending.to_toml().expect("the wire");
    assert_eq!(
        leaf(&restore::RestoreManifest::from_toml(&wire).expect("read back")).supervised,
        leaf(&pending).supervised,
        "through the wire"
    );
    assert_eq!(
        leaf(&app.capture_restore_manifest()).supervised,
        None,
        "a durable layout names none"
    );
    crate::harness_host::publish_snapshot_for_test(
        &sid,
        Some(&Snapshot {
            session: Some("11111111-2222-4333-8444-555555555555".to_string()),
            ..supervised.clone()
        }),
    );
    let live = app.capture_handoff_layout();
    assert_eq!(commit_agent_carry(&live), commit_agent_carry(&pending));
    crate::harness_host::publish_snapshot_for_test(
        &sid,
        Some(&Snapshot {
            pid: supervised.pid + 1,
            ..supervised.clone()
        }),
    );
    let live = app.capture_handoff_layout();
    assert_ne!(commit_agent_carry(&live), commit_agent_carry(&pending));
    assert_eq!(
        commit_layout_topology(&live),
        commit_layout_topology(&pending),
        "the topology did not change"
    );
    host.owe_for_test(&sid, Some(owed_relaunch(&sid)));
    let owed = leaf(&app.capture_handoff_layout());
    assert_eq!(
        (owed.agent.is_some(), owed.supervised),
        (true, None),
        "(owed, supervised)"
    );
    crate::harness_host::publish_snapshot_for_test(&sid, None);
    app.rollback_overlap(None, &[]);
}

#[cfg(unix)]
#[test]
fn the_handoff_leaf_carries_the_lost_word_and_the_successor_stands_it() {
    use crate::app_update_handoff::{commit_agent_carry, commit_layout_topology};
    let act = || {
        crate::harness_host::HostHandle::for_restored_test_with_open_tabs(std::sync::Arc::new(
            |_, _, _, _| "wait:shell-prompt".to_string(),
        ))
    };
    let host = act();
    let mut app = App::headless_for_test();
    let _ = app.insert_logical_window(crate::stub_session(1), 24, 80);
    let sid = {
        let g = app.store.read().unwrap();
        g.by_local(0).expect("registered").sid.as_str().to_string()
    };
    app.harness = Some(host.clone());
    let word = restore::LostRestore {
        session: "1057c0de-6e66-4f1e-9c0d-8e4f5a6b7c8d".to_string(),
        pid: 4242,
        marked: true,
        reason: "its conversation could not be resumed".to_string(),
        restarted: false,
        command: Some("claude --resume 1057c0de-6e66-4f1e-9c0d-8e4f5a6b7c8d".to_string()),
    };
    let leaf = |layout: &restore::RestoreManifest| {
        layout
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .expect("the tab's leaf")
    };
    assert!(app.park_all_readers(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN));
    assert_eq!(
        leaf(&app.capture_handoff_layout()).lost,
        None,
        "no word yet"
    );
    let carried = Some(Box::new(word.clone()));
    host.adopt_lost_words(vec![(sid.clone(), word.clone())]);
    let pending = app.capture_handoff_layout();
    assert_eq!(leaf(&pending).lost, carried, "the handoff leaf");
    let wire = pending.to_toml().expect("the wire");
    assert_eq!(
        leaf(&restore::RestoreManifest::from_toml(&wire).expect("read back")).lost,
        carried,
        "through the wire"
    );
    assert_eq!(
        leaf(&app.capture_restore_manifest()).lost,
        None,
        "a durable layout carries none"
    );
    let mut moved = app.capture_handoff_layout();
    let before = commit_agent_carry(&moved);
    assert_eq!(before, commit_agent_carry(&pending));
    for window in &mut moved.windows {
        for tab in &mut window.restored_tabs {
            if let restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(terminal),
            } = &mut tab.root
            {
                terminal.lost = None;
            }
        }
    }
    assert_ne!(commit_agent_carry(&moved), before, "compared at the Commit");
    assert_eq!(
        commit_layout_topology(&moved),
        commit_layout_topology(&pending),
        "not topology"
    );
    app.rollback_overlap(None, &[]);

    let stood = |carried: Option<restore::LostRestore>| {
        let successor_host = act();
        let mut successor = App::headless_for_test();
        successor.handoff_successor = true;
        successor.restore_into_window(
            WindowId(0),
            window_of(vec![restore::RestoredSplitTree::leaf(
                restore::RestoredView::Terminal(restore::TerminalLeafRestore {
                    local_id: Some(8),
                    lost: carried.map(Box::new),
                    ..bare_leaf()
                }),
            )]),
        );
        let adopted = *successor.pool.sessions.keys().max().expect("a session");
        adopt_as(&mut successor, adopted, 8);
        let adopted_sid = {
            let g = successor.store.read().unwrap();
            g.by_local(adopted)
                .expect("registered")
                .sid
                .as_str()
                .to_string()
        };
        successor.harness = Some(successor_host.clone());
        successor.hand_carried_restored_agents();
        successor_host.lost_word(&adopted_sid)
    };
    assert_eq!(stood(Some(word.clone())), Some(word), "stood at the Commit");
    assert_eq!(stood(None), None, "NEGATIVE CONTROL");
}

#[cfg(unix)]
#[test]
fn the_successors_commit_carries_the_stamps_and_words_before_its_host_resumes() {
    let word = restore::LostRestore {
        session: "2057c0de-6e66-4f1e-9c0d-8e4f5a6b7c8d".to_string(),
        pid: 4343,
        marked: true,
        reason: "its conversation could not be resumed".to_string(),
        restarted: false,
        command: Some("claude --resume 2057c0de-6e66-4f1e-9c0d-8e4f5a6b7c8d".to_string()),
    };
    let dir = crate::control_auth::socket_dir().expect("a private control dir");
    let commit = |carried: bool| {
        let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
            std::sync::Arc::new(|_, _, _, _| "wait:shell-prompt".to_string()),
        );
        let mut successor = App::headless_for_test();
        successor.handoff_successor = true;
        let adopted = *successor.pool.sessions.keys().max().expect("a session");
        adopt_as(&mut successor, adopted, 8);
        let sid = {
            let g = successor.store.read().unwrap();
            g.by_local(adopted)
                .expect("registered")
                .sid
                .as_str()
                .to_string()
        };
        let sidecar = dir.join(format!(
            "seamless-{}-{}.person",
            std::process::id(),
            aterm_uds::rand::hex_token::<8>().expect("a token")
        ));
        if carried {
            assert!(crate::seamless::write_person_stamps(
                &sidecar,
                &[(8, crate::seamless::PersonStamp::Keyed(90_000))]
            ));
            successor.handoff_person_stamps = Some(sidecar.clone());
            successor.handoff_lost_words = vec![(8, word.clone())];
        }
        successor.harness = Some(host.clone());
        let at_resume = std::rc::Rc::new(std::cell::RefCell::new(None));
        {
            let at_resume = std::rc::Rc::clone(&at_resume);
            let (host, sid) = (host.clone(), sid.clone());
            successor.handoff_before_host_resume = Some(Box::new(move |_| {
                *at_resume.borrow_mut() = Some(host.lost_word(&sid));
            }));
        }
        successor.take_over_supervision_at_commit();
        let stamp = successor
            .pool
            .get(adopted)
            .expect("live")
            .ctx
            .human_input
            .ms_since(crate::metrics::now_us());
        let stood = at_resume.borrow_mut().take().expect("the host resumed");
        (stamp, stood, sidecar.exists())
    };
    let (stamp, stood, left) = commit(true);
    assert!(stamp.is_some_and(|ms| ms >= 90_000), "the stamp: {stamp:?}");
    assert_eq!(
        stood.as_ref(),
        Some(&word),
        "standing when the host resumed"
    );
    assert!(!left, "the sidecar is consumed");
    assert_eq!(
        commit(false),
        (None, None, false),
        "NEGATIVE CONTROL: nothing carried"
    );
}

#[cfg(unix)]
#[test]
fn the_handoff_leaf_follows_its_agents_record_and_a_move_refuses_nothing() {
    use crate::app_update_handoff::commit_agent_carry;
    use aterm_agent::harness::relaunch::Snapshot;
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-handoff-followed-")
        .tempdir()
        .expect("temp dir");
    let dir = tmp.path().join(".claude");
    std::fs::create_dir_all(dir.join("sessions")).expect("the records' dir");
    let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
        std::sync::Arc::new(|_, _, _, _| "wait:shell-prompt".to_string()),
    );
    let mut app = App::headless_for_test();
    let _ = app.insert_logical_window(crate::stub_session(1), 24, 80);
    let sid = {
        let g = app.store.read().unwrap();
        g.by_local(0).expect("registered").sid.as_str().to_string()
    };
    app.harness = Some(host.clone());
    let copy = Snapshot {
        claude_dir: Some(dir.clone()),
        ..owed_relaunch(&sid)
    };
    let left = copy.session.clone().expect("the conversation it left");
    let cleared = "0b6f3c1e-7777-4b61-9d52-7f1e2c3a4b5c";
    let resumed = "0b6f3c1e-9999-4b61-9d52-7f1e2c3a4b5c";
    let record = |session: &str, start: &str| {
        std::fs::write(
            dir.join("sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"{session}","cwd":"/tmp","version":"2.1.300","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
            ),
        )
        .expect("the record");
    };
    let watched = |layout: &restore::RestoreManifest| {
        layout
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .and_then(|leaf| leaf.supervised)
            .and_then(|agent| agent.session)
    };
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&copy));

    record(cleared, &copy.start);
    assert!(app.park_all_readers(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN));
    let pending = app.capture_handoff_layout();
    assert_eq!(
        watched(&pending).as_deref(),
        Some(cleared),
        "the conversation the agent is in"
    );
    record(resumed, &copy.start);
    let live = app.capture_handoff_layout();
    assert_eq!(watched(&live).as_deref(), Some(resumed), "followed again");
    assert_eq!(
        commit_agent_carry(&live),
        commit_agent_carry(&pending),
        "nothing refused"
    );

    record(cleared, "Mon Jan  1 00:00:00 2024");
    assert_eq!(
        watched(&app.capture_handoff_layout()),
        Some(left),
        "another process's record"
    );
    crate::harness_host::publish_snapshot_for_test(&sid, None);
    app.rollback_overlap(None, &[]);
}

#[cfg(unix)]
#[test]
fn a_durable_layout_never_carries_the_explicit_restarts_cause() {
    use aterm_agent::harness::restart_stamp::Cause;
    let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
        std::sync::Arc::new(|_, _, _, _| "wait:shell-prompt".to_string()),
    );
    let mut app = App::headless_for_test();
    let _ = app.insert_logical_window(crate::stub_session(1), 24, 80);
    let sid = {
        let g = app.store.read().unwrap();
        g.by_local(0).expect("registered").sid.as_str().to_string()
    };
    app.harness = Some(host.clone());
    host.owe_asked_for_test(&sid, Some(owed_relaunch(&sid)), Some(Cause::Asked));
    let owed_agent = |layout: &restore::RestoreManifest| {
        layout
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .and_then(|leaf| leaf.agent)
            .map(|agent| *agent)
    };
    let mut durable = app.capture_restore_manifest();
    app.fill_desk_agents(&mut durable);
    let wire = durable.to_toml().expect("the wire");
    let named = owed_agent(&restore::RestoreManifest::from_toml(&wire).expect("read back"))
        .expect("the durable leaf names the owed agent");
    assert_eq!(
        (named.session.as_deref(), named.cause.as_deref()),
        (owed_relaunch(&sid).session.as_deref(), None),
        "(the conversation, the cause): a durable layout names the agent alone"
    );
    let mut cold = App::headless_for_test();
    cold.restore_into_window(WindowId(0), window_of(vec![leaf_hosting(8, Some(named))]));
    for (&id, pooled) in cold.pool.sessions.iter_mut() {
        pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
    }
    let handed = cold.take_restored_agents();
    assert_eq!(
        handed
            .iter()
            .map(|(_, _, asked, _)| *asked)
            .collect::<Vec<_>>(),
        vec![None],
        "a cold restore relaunches it as aterm's own end"
    );
    host.shutdown_and_join();
}

#[cfg(unix)]
#[test]
fn a_held_tabs_agent_stays_the_holders_across_a_cold_restore() {
    use crate::fabric::Hold;
    let cold_restore_of = |hold: Option<Hold>| -> (Vec<(u32, u32)>, usize, Vec<Hold>) {
        let mut app = App::headless_for_test();
        let _ = app.insert_logical_window(crate::stub_session(1), 24, 80);
        let sid = {
            let g = app.store.read().unwrap();
            g.by_local(0).expect("registered").sid.as_str().to_string()
        };
        crate::harness_host::publish_snapshot_for_test(&sid, Some(&owed_relaunch(&sid)));
        if let Some(hold) = hold {
            let ctx = &app.pool.get(0).expect("the tab's session").ctx;
            assert!(crate::fabric::apply_hold_for_test(ctx, Some(hold)));
        }
        let mut durable = app.capture_restore_manifest();
        app.fill_desk_agents(&mut durable);
        crate::harness_host::publish_snapshot_for_test(&sid, None);
        let wire = durable.to_toml().expect("the wire");
        let read = restore::RestoreManifest::from_toml(&wire).expect("read back");
        let leaf = read
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .expect("the tab's leaf");
        assert!(leaf.agent.is_some(), "the durable leaf names the agent");
        let reopened = crate::crash_journal::Reopened {
            manifest: read,
            class: crate::crash_journal::DeathClass::Killed,
            sources: Vec::new(),
            writers: Vec::new(),
            handed: Vec::new(),
            programs: None,
            second_chance: false,
        };
        let mut cold = App::headless_for_test();
        cold.restore_into_window(
            WindowId(0),
            window_of(vec![restore::RestoredSplitTree::leaf(
                restore::RestoredView::Terminal(leaf),
            )]),
        );
        for (&id, pooled) in cold.pool.sessions.iter_mut() {
            pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
        }
        let handed = cold.take_restored_agents().len();
        let holds = cold
            .pool
            .sessions
            .values()
            .filter_map(|pooled| pooled.session.ctx.fabric.hold())
            .collect();
        (reopened.resumed(true), handed, holds)
    };
    let owners = Hold {
        reason: "review".into(),
        origin: "local".into(),
    };
    let (promised, handed, holds) = cold_restore_of(Some(owners.clone()));
    assert_eq!(
        handed, 0,
        "the owner held the tab: its agent is not relaunched"
    );
    assert_eq!(
        holds,
        vec![owners],
        "the owner's hold is on the reopened tab"
    );
    assert!(
        promised.is_empty(),
        "the row promises it nowhere: {promised:?}"
    );
    let fleet = Hold {
        reason: "fleet-halt".into(),
        origin: "fleet".into(),
    };
    let (promised, handed, holds) = cold_restore_of(Some(fleet));
    assert_eq!(
        handed, 0,
        "the fleet held the tab: its agent is not relaunched"
    );
    assert!(holds.is_empty(), "a fleet hold is the process's: {holds:?}");
    assert!(
        promised.is_empty(),
        "the row promises it nowhere: {promised:?}"
    );
    let (promised, handed, holds) = cold_restore_of(None);
    assert_eq!(
        (promised, handed, holds),
        (vec![(0, 0)], 1, Vec::new()),
        "(promised, handed, holds) of an unheld tab"
    );
}

#[test]
fn a_held_tabs_agent_left_at_its_shell_is_said_on_record() {
    use crate::fabric::{Hold, render_hold};
    let held_leaf = |shell: u64, pid: u32, hold: Option<Hold>| {
        restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
            restore::TerminalLeafRestore {
                local_id: Some(shell),
                agent: Some(Box::new(restore::AgentRestore {
                    pid,
                    ..dead_agent()
                })),
                holds: hold.iter().map(render_hold).collect(),
                ..bare_leaf()
            },
        ))
    };
    let owners = Hold {
        reason: "review".into(),
        origin: "local".into(),
    };
    let fleet = Hold {
        reason: "fleet-halt".into(),
        origin: "fleet".into(),
    };
    let host =
        crate::harness_host::HostHandle::for_restored_test(std::sync::Arc::new(|_, _, _, _| {
            "wait:shell-prompt".to_string()
        }));
    let mut cold = App::headless_for_test();
    cold.harness = Some(host.clone());
    cold.restore_into_window(
        WindowId(0),
        window_of(vec![
            held_leaf(7, 4242, Some(owners.clone())),
            held_leaf(8, 4343, Some(fleet)),
            held_leaf(9, 4444, None),
            held_leaf(7, 4242, Some(owners.clone())),
        ]),
    );
    assert_eq!(
        cold.restored_held
            .iter()
            .map(|(_, agent, fleet, _)| (agent.pid, *fleet))
            .collect::<Vec<_>>(),
        vec![(4242, false), (4343, true), (4242, false)],
        "(agent, the fleet's): the held tabs' agents, not relaunched"
    );
    assert_eq!(cold.restored_agents.len(), 1, "the unheld tab's is owed");
    let holds: Vec<Hold> = cold
        .pool
        .sessions
        .values()
        .filter_map(|pooled| pooled.session.ctx.fabric.hold())
        .collect();
    assert_eq!(
        holds,
        vec![owners.clone(), owners],
        "the owner's hold alone came back, on each of its tabs"
    );
    let carried = cold.pool.sessions.values().any(|pooled| {
        pooled
            .session
            .ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .any(|event| event.kind == "hold" && event.payload.ends_with("carried=1"))
    });
    assert!(carried, "its timeline says the hold was carried");
    let records = cold.messages.log().records().count();
    cold.say_restored_held();
    let said: Vec<String> = cold
        .messages
        .log()
        .records()
        .skip(records)
        .map(|record| record.title.clone())
        .collect();
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(
        said.iter()
            .all(|title| title.contains("not restarted") && title.contains("held")),
        "{said:?}"
    );
    cold.say_restored_held();
    assert_eq!(
        cold.messages.log().records().count(),
        records + 2,
        "said once"
    );
    host.shutdown_and_join();
}

#[test]
fn a_missing_held_leaf_does_not_hide_the_same_agents_surviving_leaf() {
    let mut cold = App::headless_for_test();
    let leaf = restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
        restore::TerminalLeafRestore {
            local_id: Some(7),
            agent: Some(Box::new(dead_agent())),
            holds: vec![crate::fabric::render_hold(&crate::fabric::Hold {
                reason: "review".into(),
                origin: "local".into(),
            })],
            ..bare_leaf()
        },
    ));
    cold.restore_into_window(WindowId(0), window_of(vec![leaf]));
    let (_, agent, fleet, rows) = cold.restored_held[0].clone();
    assert!(cold.store.read().unwrap().by_local(u64::MAX).is_none());
    cold.restored_held.insert(0, (u64::MAX, agent, fleet, rows));
    let records = cold.messages.log().records().count();
    cold.say_restored_held();
    let said = held_records(&cold, records);
    assert_eq!(
        said.len(),
        1,
        "the surviving leaf's held agent is reported: {said:?}"
    );
}

#[test]
fn a_held_tabs_agent_has_its_earlier_tab_noted_as_the_restore_says_it() {
    const OLD_SID: &str = "s-00000000000000000001";
    let leaf = restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
        restore::TerminalLeafRestore {
            local_id: Some(7),
            agent: Some(Box::new(dead_agent())),
            holds: vec!["local x".to_string()],
            ..bare_leaf()
        },
    ));
    for case in ["an earlier tab", "control: no tab", "control: its own tab"] {
        type Noted = std::sync::Mutex<Vec<(String, String)>>;
        let noted: std::sync::Arc<Noted> = std::sync::Arc::default();
        let host = crate::harness_host::HostHandle::for_restored_test_noting(
            std::sync::Arc::new(|_, _, _, _| "wait:shell-prompt".to_string()),
            std::sync::Arc::clone(&noted),
        );
        let mut cold = App::headless_for_test();
        cold.harness = Some(host.clone());
        cold.restore_into_window(WindowId(0), window_of(vec![leaf.clone()]));
        assert_eq!(cold.restored_held.len(), 1, "{case}: held, not relaunched");
        let id = cold.restored_held[0].0;
        let sid = {
            let g = cold.store.read().unwrap();
            g.by_local(id).expect("registered").sid.as_str().to_string()
        };
        cold.restored_held[0].1.tab = match case {
            "an earlier tab" => Some(OLD_SID.to_string()),
            "control: its own tab" => Some(sid.clone()),
            _ => None,
        };
        cold.say_restored_held();
        let want = if case == "an earlier tab" {
            vec![(sid, OLD_SID.to_string())]
        } else {
            Vec::new()
        };
        assert_eq!(*noted.lock().unwrap(), want, "{case}");
        host.shutdown_and_join();
    }
}

fn held_records(app: &App, from: usize) -> Vec<(String, Vec<String>)> {
    app.messages
        .log()
        .records()
        .skip(from)
        .map(|record| (record.title.clone(), record.detail.clone()))
        .collect()
}

fn ctl_command_named(lines: &[String]) -> Option<String> {
    lines.iter().find_map(|line| {
        let (_, tail) = line.split_once("aterm ctl ")?;
        let mut words = Vec::new();
        for word in tail.split_whitespace() {
            let word = word.trim_matches(|c: char| matches!(c, '`' | ',' | ';' | '.' | ':'));
            words.push(word.to_string());
            if matches!(word, "off" | "on") {
                return Some(words.join(" "));
            }
        }
        None
    })
}

#[test]
fn the_lift_a_held_tabs_record_names_is_one_the_control_socket_takes() {
    use crate::fabric::{Hold, render_hold};
    let owners = Hold {
        reason: "review".into(),
        origin: "local".into(),
    };
    let mut cold = App::headless_for_test();
    cold.restore_into_window(
        WindowId(0),
        window_of(vec![restore::RestoredSplitTree::leaf(
            restore::RestoredView::Terminal(restore::TerminalLeafRestore {
                local_id: Some(7),
                agent: Some(Box::new(dead_agent())),
                holds: vec![render_hold(&owners)],
                ..bare_leaf()
            }),
        )]),
    );
    let holds = |app: &App| -> Vec<Hold> {
        app.pool
            .sessions
            .values()
            .filter_map(|pooled| pooled.session.ctx.fabric.hold())
            .collect()
    };
    assert_eq!(
        holds(&cold),
        vec![owners],
        "PRECONDITION: the hold came back"
    );
    let from = cold.messages.log().records().count();
    cold.say_restored_held();
    let lines: Vec<String> = held_records(&cold, from)
        .into_iter()
        .flat_map(|(_, lines)| lines)
        .collect();
    let lift = ctl_command_named(&lines).expect("the record names the lift");
    assert_eq!(
        crate::control::hold_line_for_test(&cold.store, &lift, crate::control::Scope::Owner),
        "OK hold=0\n",
        "the lift the record names ({lift:?}), as the Owner runs it"
    );
    assert!(holds(&cold).is_empty(), "lifted: {:?}", holds(&cold));
}

#[test]
fn a_held_tabs_record_is_its_own_agents() {
    use crate::fabric::{Hold, render_hold};
    const THREAD: &str = "019a2b3c-4d5e-7f60-8a9b-0c1d2e3f4a5b";
    const HOME: &str = "/opt/agents/claude-work";
    let owners = Hold {
        reason: "review".into(),
        origin: "local".into(),
    };
    let held = |shell: u64, agent: restore::AgentRestore| {
        restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
            restore::TerminalLeafRestore {
                local_id: Some(shell),
                agent: Some(Box::new(agent)),
                holds: vec![render_hold(&owners)],
                ..bare_leaf()
            },
        ))
    };
    let homed = restore::AgentRestore {
        home: Some(HOME.into()),
        ..dead_agent()
    };
    let codex = restore::AgentRestore {
        pid: 4343,
        program: "/opt/codex/bin/codex".into(),
        argv: vec!["/opt/codex/bin/codex".into()],
        session: Some(THREAD.into()),
        run: Some(restore::RunRestore::Codex(restore::CodexRestore {
            home: "/opt/agents/codex".into(),
            home_env: None,
            embedded: true,
            started_ms: 1,
            alab: Some(restore::AlabRestore {
                argv: vec![
                    "/opt/codex/bin/codex".into(),
                    "resume".into(),
                    THREAD.into(),
                ],
                session: THREAD.into(),
                codex_home: Some("/opt/agents/codex".into()),
                cwd: None,
            }),
            carried: None,
        })),
        ..dead_agent()
    };
    let host =
        crate::harness_host::HostHandle::for_restored_test(std::sync::Arc::new(|_, _, _, _| {
            "wait:shell-prompt".to_string()
        }));
    let mut cold = App::headless_for_test();
    cold.harness = Some(host.clone());
    assert!(
        host.relaunches_restored(),
        "PRECONDITION: a relaunching host"
    );
    cold.restore_into_window(
        WindowId(0),
        window_of(vec![held(7, homed.clone()), held(8, codex)]),
    );
    let from = cold.messages.log().records().count();
    cold.say_restored_held();
    let said = held_records(&cold, from);
    assert_eq!(said.len(), 2, "{said:?}");
    let (claude_title, claude_lines) = &said[0];
    assert!(
        claude_title.contains("Claude") && claude_title.contains("because the tab was held"),
        "{claude_title:?}"
    );
    let conversation = homed.session.as_deref().expect("a conversation");
    assert!(
        claude_lines.iter().any(|line| line.contains(&format!(
            "CLAUDE_CONFIG_DIR={HOME} claude --resume {conversation}"
        ))),
        "the line its relaunch would carry, its home on it: {claude_lines:?}"
    );
    let (codex_title, codex_lines) = &said[1];
    assert!(
        codex_title.contains("Codex") && !codex_title.contains("Claude"),
        "{codex_title:?}"
    );
    assert!(
        codex_lines.iter().any(|line| line.contains(&format!(
            "CODEX_HOME=/opt/agents/codex /opt/codex/bin/codex resume {THREAD}"
        ))) && !codex_lines.iter().any(|line| line.contains("claude")),
        "codex-alab's own validated line: {codex_lines:?}"
    );
    host.shutdown_and_join();

    let mut off = App::headless_for_test();
    off.restore_into_window(WindowId(0), window_of(vec![held(7, homed)]));
    let from = off.messages.log().records().count();
    off.say_restored_held();
    let said = held_records(&off, from);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        !said[0].0.contains("because the tab was held"),
        "no host would have relaunched it: {said:?}"
    );
    assert!(
        ctl_command_named(&said[0].1).is_some(),
        "the lift: {said:?}"
    );
}

#[test]
fn a_shell_the_keeper_gave_back_keeps_its_own_hold() {
    use crate::fabric::{Hold, render_hold, render_holds};
    let owners = Hold {
        reason: "review".into(),
        origin: "local".into(),
    };
    let fleet = Hold {
        reason: "fleet-halt".into(),
        origin: "fleet".into(),
    };
    let rows = |holds: &[&Hold]| -> Vec<String> { holds.iter().map(|h| render_hold(h)).collect() };
    let layout = |leaf: &[&Hold]| {
        window_of(vec![
            leaf_hosting(7, None),
            restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
                restore::TerminalLeafRestore {
                    local_id: Some(8),
                    agent: Some(Box::new(dead_agent())),
                    holds: rows(leaf),
                    ..bare_leaf()
                },
            )),
        ])
    };
    let restore = |leaf: &[&Hold], kept: Option<&[&Hold]>| {
        let mut app = App::headless_for_test();
        if let Some(meta) = kept {
            app.seamless_adopt = vec![crate::spawn::Adopted {
                holds: rows(meta),
                ..handed_off_shell(8)
            }];
        }
        app.restore_into_window(WindowId(0), layout(leaf));
        assert!(app.seamless_adopt.is_empty(), "the kept shell was placed");
        assert!(
            app.restored_agents.is_empty()
                && app.restored_held.is_empty() == (kept.is_some() || leaf.is_empty()),
            "nothing relaunched: a kept shell's agent runs, a held tab's is the holder's"
        );
        let mut held: Vec<(Option<u64>, Vec<String>, usize)> = app
            .pool
            .sessions
            .values()
            .map(|pooled| {
                let carried = pooled
                    .session
                    .ctx
                    .timeline
                    .lock()
                    .unwrap()
                    .since(None)
                    .filter(|event| event.kind == "hold" && event.payload.ends_with("carried=1"))
                    .count();
                (
                    pooled.session.handoff_local_id,
                    render_holds(&pooled.session.ctx.fabric.holds()),
                    carried,
                )
            })
            .filter(|(_, holds, carried)| !holds.is_empty() || *carried > 0)
            .collect();
        held.sort_by_key(|(kept, _, _)| *kept);
        held
    };
    let kept = |holds: &[&Hold], carried| vec![(Some(8), rows(holds), carried)];
    assert_eq!(
        restore(&[&fleet], Some(&[&fleet])),
        kept(&[&fleet], 1),
        "the keeper's fleet hold, carried once"
    );
    assert_eq!(
        restore(&[&owners], Some(&[&owners])),
        kept(&[&owners], 1),
        "the owner's hold, carried once"
    );
    assert_eq!(
        restore(&[&fleet], Some(&[])),
        Vec::new(),
        "lifted by the fleet after the journal's last image: the fleet's slot is the META's"
    );
    assert_eq!(
        restore(&[&owners], Some(&[])),
        kept(&[&owners], 1),
        "the two samples disagree on the owner's: it stands (fail-closed)"
    );
    assert_eq!(
        restore(&[], Some(&[&owners])),
        kept(&[&owners], 1),
        "held after the journal's last image: the keeper's carry"
    );
    assert_eq!(
        restore(&[&fleet, &owners], Some(&[&fleet])),
        kept(&[&fleet, &owners], 2),
        "both slots: the META's fleet hold and the leaf's owner's, one carry each"
    );
    assert_eq!(
        restore(&[&owners], None),
        vec![(None, rows(&[&owners]), 1)],
        "a fresh shell, the owner's hold"
    );
    assert_eq!(
        restore(&[&fleet], None),
        Vec::new(),
        "a fresh shell the fleet never halted"
    );
    assert_eq!(
        restore(&[&fleet, &owners], None),
        vec![(None, rows(&[&owners]), 1)],
        "a fresh shell, the owner's slot alone"
    );
}

#[test]
#[cfg(unix)]
fn handed_off_masters_are_adopted_or_closed_never_leaked() {
    use aterm_spec::verify::validate_transition_tiered;
    use std::io::Read;
    use std::os::fd::IntoRawFd;
    use std::os::unix::net::UnixStream;

    let model = aterm_models_session::fd_handoff_no_leak_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let closed = |peer: &UnixStream| {
        let mut byte = [0u8; 1];
        match (&*peer).read(&mut byte) {
            Ok(0) => true,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
            other => panic!("unexpected witness read: {other:?}"),
        }
    };
    let eventually_closed = |peer: &UnixStream| {
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            if closed(peer) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    for (what, window) in [("a front window", true), ("no window", false)] {
        let mut app = App::headless_for_test();
        app.handoff_successor = true;
        if !window {
            app.frontmost_window = None;
        }
        let mut peers = Vec::new();
        for local_id in 1..=2 {
            let (master, peer) = UnixStream::pair().expect("socketpair");
            peer.set_nonblocking(true).expect("non-blocking witness");
            let mut shell = handed_off_shell(local_id);
            shell.master = crate::spawn::HandedMaster::new(master.into_raw_fd());
            app.seamless_adopt.push(shell);
            peers.push((local_id, peer));
        }

        app.adopt_orphan_shells_as_tabs(&[]);
        assert!(
            app.seamless_adopt.is_empty(),
            "{what}: the net took every shell"
        );

        let label = "FdHandoffNoLeak(adopt_orphan_shells_as_tabs)";
        let advance = |state: &mut std::collections::BTreeMap<&'static str, i64>,
                       action: &str,
                       var: &'static str,
                       value: i64| {
            let mut next = state.clone();
            next.insert(var, value);
            let (ok, why) =
                validate_transition_tiered(&model, &[], state, &next, Some(action), label);
            assert!(
                ok,
                "{what}: the real handoff took `{action}` to {next:?}, which the \
                 committed model does not admit — {why}"
            );
            *state = next;
        };
        let mut state = model.init_state();
        for handed in 1..=2 {
            advance(&mut state, "Prepare", "prepared", handed);
        }
        assert!(!model.action_enabled("BuggyDrop", &state));
        assert!(buggy.action_enabled("BuggyDrop", &state));
        let (mut adopted, mut closed_count) = (0, 0);
        for (local_id, peer) in &peers {
            let in_pool = app.pool.iter().any(|session| {
                session.handoff_local_id == Some(*local_id) && session.ctx.sink.master() >= 3
            });
            let is_closed = if in_pool {
                closed(peer)
            } else {
                eventually_closed(peer)
            };
            match (in_pool, is_closed) {
                (true, false) => {
                    adopted += 1;
                    advance(&mut state, "Adopt", "adopted", adopted);
                }
                (false, true) => {
                    closed_count += 1;
                    advance(&mut state, "CloseFallback", "closed", closed_count);
                }
                (false, false) => advance(&mut state, "BuggyDrop", "leaked", 1),
                (true, true) => panic!("{what}: shell {local_id} adopted onto a closed master"),
            }
        }
        assert!(model.check_invariant("NoLeak", &state), "{what}: {state:?}");
        assert_eq!(
            (adopted, closed_count),
            if window { (2, 0) } else { (0, 2) },
            "{what}: where the shells went"
        );
    }
}

fn handed_off_shell(local_id: u64) -> crate::spawn::Adopted {
    crate::spawn::Adopted {
        repaint: false,
        local_id,
        master: crate::spawn::HandedMaster::new(-1),
        pid: -1,
        sid: aterm_session::SessionId::generate(),
        nonce: aterm_session::LaunchNonce::generate(),
        checkpoint: None,
        control: None,
        frozen_path: false,
        topics: Vec::new(),
        fg_holder: 0,
        rekey: false,
        loader: false,
        history: crate::handoff_history::AdoptedHistory::default(),
        holds: Vec::new(),
        title: String::new(),
        supervisor: None,
        attention_owners: Vec::new(),
        claim_grace: false,
        outgoing_build: None,
        keeper: None,
        render_debt: None,
        fabric: Default::default(),
        timeline_id: 0,
        log: Default::default(),
        journal_key: None,
        task: None,
        autopilot_end: None,
        wakes: None,
    }
}

fn recovered_shell(owner: u32, local_id: u64) -> crate::spawn::Adopted {
    crate::spawn::Adopted {
        keeper: Some(crate::keeper_link::Claim::new(
            u64::from(owner) * 100 + local_id,
            Some(owner),
        )),
        ..handed_off_shell(local_id)
    }
}

fn whose(shell: &crate::spawn::Adopted) -> Option<(Option<u32>, u64)> {
    shell
        .keeper
        .as_ref()
        .map(|claim| (claim.owner(), claim.rdev()))
}

#[cfg(unix)]
#[test]
fn a_taken_over_image_places_the_shells_its_successor_re_tagged() {
    use crate::crash_journal::{self, JournalHeader, JournalId, JournalOwner, Lane, tests as cj};
    use crate::keeper_link::{Claim, Tag, registration_tag};
    use aterm_keeper::core::{Holders, KeeperCore, KeeperEnv};
    use aterm_keeper::wire::{Birth, CAP_LINK, MasterHeader, PeerClass};

    struct World;
    impl KeeperEnv for World {
        fn holders(&mut self, _: u64) -> Holders {
            Holders::None
        }
        fn shell_alive(&mut self, _: u32, _: Birth) -> bool {
            true
        }
        fn pid_alive(&mut self, _: u32, _: Option<Birth>) -> bool {
            false
        }
    }
    const SHELL: u64 = 11;
    const FRESH: u64 = 12;
    let header = |rdev: u64, local_id: u64| MasterHeader {
        rdev,
        shell_pid: 900,
        shell_birth: Birth {
            seconds: 1,
            micros: 0,
        },
        local_id,
    };
    let (dir, logs) = (cj::scratch("cj-dir"), cj::scratch("cj-logs"));
    let parent = JournalId::now(cj::reaped_pid());
    let owner = JournalOwner::create(dir.path(), parent).expect("armed");
    owner
        .publish(
            &cj::layout(&[("/a", "zsh"), ("/b", "claude")]),
            &JournalHeader::now(parent.pid, false),
        )
        .expect("published");
    cj::die(owner);
    let successor = cj::reaped_pid();

    let mut app = App::headless_for_test();
    adopt_as(&mut app, 0, 1);
    let adopted = app.pool.get(0).expect("session 0");
    let retag = |handed: bool| {
        let mut tag = registration_tag(adopted, successor, Some(parent.pid));
        assert_eq!(tag.handed, Some((parent.pid, 1)), "it names the parent's");
        if !handed {
            tag.handed = None;
        }
        tag.encode()
    };
    let parent_tag = registration_tag(adopted, parent.pid, None).encode();
    let fresh_tag = Tag {
        handed: None,
        ..Tag::decode(&registration_tag(adopted, successor, None).encode()).expect("tag")
    }
    .encode();

    let keeper = |activated: bool, handed: bool| {
        let mut core = KeeperCore::new(false);
        let mut world = World;
        assert!(core.accept(1, parent.pid, None));
        let _ = core.hello(1, PeerClass::App, CAP_LINK, 0, &mut world);
        let _ = core.register(1, header(SHELL, 1), parent_tag.clone(), SHELL);
        if activated {
            assert!(core.accept(2, successor, None));
            let _ = core.hello(2, PeerClass::App, CAP_LINK, 0, &mut world);
            let _ = core.register(2, header(SHELL, 0), retag(handed), SHELL);
            let _ = core.register(2, header(FRESH, 1), fresh_tag.clone(), FRESH);
        }
        core.records()
            .iter()
            .map(|(rdev, record)| crate::spawn::Adopted {
                keeper: Some(Claim::of_tag(
                    *rdev,
                    &Tag::decode(&record.tag).expect("a tag this build reads"),
                )),
                ..handed_off_shell(record.header.local_id)
            })
            .collect::<Vec<_>>()
    };

    let mut lane = Lane::new(Some(dir.path().to_path_buf()), successor);
    lane.set_predecessor(parent.pid, Some(parent.nanos));
    let own = lane.journal_id();
    cj::marker(logs.path(), own);
    assert_eq!(lane.adopt_predecessor(true), Some(parent));
    drop(lane);
    cj::await_released(&dir.path().join(own.lock_name()));
    let claim = crash_journal::claim_at_boot(
        dir.path(),
        &crash_journal::Evidence {
            log_dir: Some(logs.path()),
            swept: &[],
        },
        std::process::id(),
        crash_journal::QuitLayout::default(),
    );
    let reopened = claim.reopened.expect("the desk comes back");
    let writers: Vec<Option<u32>> = reopened.writers.iter().copied().map(Some).collect();
    assert_eq!(writers, vec![Some(parent.pid)], "the image is the parent's");

    let rdev = |shell: &crate::spawn::Adopted| shell.keeper.as_ref().map(Claim::rdev);
    for (activated, handed) in [(true, true), (false, true), (true, false)] {
        let mut layout = reopened.manifest.clone();
        let mut shells = keeper(activated, handed);
        crate::keeper_link::place_by_owner(&mut shells, Some(&mut layout), &writers, &[]);
        let case = format!("activated={activated} handed={handed}");
        assert!(
            super::take_session0_shell(&mut shells, Some(&layout)).is_none(),
            "{case}: the parent's pane 0 lost its shell in the update"
        );
        let leaves = layout.handed_off_leaves();
        let pane1 = super::take_handed_off_shell(&mut shells, leaves[1].local_id);
        if handed {
            assert_eq!(
                pane1.as_ref().and_then(rdev),
                Some(SHELL),
                "{case}: the agent's pane takes its own live shell"
            );
            let left: Vec<_> = shells.iter().filter_map(rdev).collect();
            let expected = if activated { vec![FRESH] } else { Vec::new() };
            assert_eq!(left, expected, "{case}: the fresh session is an orphan");
        } else {
            assert!(
                pane1.is_none(),
                "{case}: the control — the re-tagged shell fills no pane"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn a_kept_shell_two_updates_on_finds_the_pane_its_grandparents_image_keys() {
    use crate::crash_journal::{self, JournalHeader, JournalId, JournalOwner, Lane, tests as cj};
    use crate::keeper_link::{Claim, Tag, registration_tag};
    const SHELL: u64 = 11;
    let (dir, logs) = (cj::scratch("cj-dir"), cj::scratch("cj-logs"));
    let grandparent = JournalId::now(cj::reaped_pid());
    let owner = JournalOwner::create(dir.path(), grandparent).expect("armed");
    owner
        .publish(
            &cj::layout(&[("/a", "zsh"), ("/b", "claude")]),
            &JournalHeader::now(grandparent.pid, false),
        )
        .expect("published");
    cj::die(owner);

    let mut parent_app = App::headless_for_test();
    adopt_as(&mut parent_app, 0, 1);
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), cj::reaped_pid());
    lane.set_predecessor(grandparent.pid, Some(grandparent.nanos));
    let parent = lane.journal_id();
    assert_eq!(lane.adopt_predecessor(true), Some(grandparent));
    parent_app.crash_journal = lane;
    let keys = parent_app.journal_keys();
    assert_eq!(
        keys,
        vec![(0, (grandparent.pid, 1))],
        "the pair G's image keys it by"
    );
    let mut manifest = crate::session_store::SessionHandoff::from_store(
        &parent_app.store.read().unwrap_or_else(|p| p.into_inner()),
    );
    for record in &mut manifest.sessions {
        record.journal_key = keys
            .iter()
            .find(|(id, _)| *id == record.local_id)
            .map(|(_, key)| crate::session_store::journal_key_row(*key));
    }
    let wire = manifest.to_toml().expect("the manifest serializes");
    let carried = crate::session_store::SessionHandoff::from_toml(&wire)
        .expect("parses")
        .sessions
        .into_iter()
        .find(|record| record.local_id == 0)
        .and_then(|record| record.journal_key)
        .as_deref()
        .and_then(crate::session_store::parse_journal_key);
    assert_eq!(carried, Some((grandparent.pid, 1)));
    drop(parent_app);
    cj::await_released(&dir.path().join(parent.lock_name()));

    let successor = cj::reaped_pid();
    let mut app = App::headless_for_test();
    adopt_as(&mut app, 0, 0);
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .journal_key = carried;
    let tag = registration_tag(
        app.pool.get(0).expect("session 0"),
        successor,
        Some(parent.pid),
    );
    assert_eq!(
        (tag.handed, tag.keyed),
        (Some((parent.pid, 0)), Some((grandparent.pid, 1))),
        "S names P's pair and the image's"
    );
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), successor);
    lane.set_predecessor(parent.pid, Some(parent.nanos));
    let own = lane.journal_id();
    cj::marker(logs.path(), own);
    assert_eq!(lane.adopt_predecessor(true), Some(parent));
    drop(lane);
    cj::await_released(&dir.path().join(own.lock_name()));

    let claim = crash_journal::claim_at_boot(
        dir.path(),
        &crash_journal::Evidence {
            log_dir: Some(logs.path()),
            swept: &[],
        },
        std::process::id(),
        crash_journal::QuitLayout::default(),
    );
    let reopened = claim.reopened.expect("the desk comes back");
    let writers: Vec<Option<u32>> = reopened.writers.iter().copied().map(Some).collect();
    assert_eq!(
        writers,
        vec![Some(grandparent.pid)],
        "the image is still G's"
    );
    let rdev = |shell: &crate::spawn::Adopted| shell.keeper.as_ref().map(Claim::rdev);
    for keyed in [true, false] {
        let tag = Tag {
            keyed: tag.keyed.filter(|_| keyed),
            ..tag.clone()
        };
        let mut shells = vec![crate::spawn::Adopted {
            keeper: Some(Claim::of_tag(
                SHELL,
                &Tag::decode(&tag.encode()).expect("a tag this build reads"),
            )),
            ..handed_off_shell(0)
        }];
        let mut layout = reopened.manifest.clone();
        crate::keeper_link::place_by_owner(
            &mut shells,
            Some(&mut layout),
            &writers,
            &reopened.handed,
        );
        let leaves = layout.handed_off_leaves();
        let pane1 = super::take_handed_off_shell(&mut shells, leaves[1].local_id);
        assert_eq!(
            pane1.as_ref().and_then(rdev),
            keyed.then_some(SHELL),
            "keyed={keyed}: the agent's pane takes its own live shell only by the image's pair"
        );
    }
}

#[cfg(unix)]
struct Successor {
    app: App,
    dir: aterm_tempfile::TempDir,
    logs: aterm_tempfile::TempDir,
    parent: crate::crash_journal::JournalId,
    own: crate::crash_journal::JournalId,
    pid: u32,
}

#[cfg(unix)]
impl Successor {
    const CONVERSATION: &'static str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";

    fn of_a_handed_off_desk() -> Self {
        use crate::crash_journal::{JournalHeader, JournalId, JournalOwner, Lane, tests as cj};
        let (dir, logs) = (cj::scratch("cj-dir"), cj::scratch("cj-logs"));
        let parent = JournalId::now(cj::reaped_pid());
        let mut desk = cj::layout(&[("/a", "zsh"), ("/b", "claude")]);
        let claude = restore::AgentRestore {
            word: String::new(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: Some(Self::CONVERSATION.into()),
            cwd: "/b".into(),
            version: Some("2.1.283".into()),
            home: None,
            login: None,
            run: None,
            cause: None,
            tab: None,
        };
        desk.fill_agents(&|id| (id == 1).then(|| claude.clone()));
        let owner = JournalOwner::create(dir.path(), parent).expect("armed");
        owner
            .publish(&desk, &JournalHeader::now(parent.pid, false))
            .expect("published");
        cj::die(owner);

        let mut app = App::headless_for_test();
        adopt_as(&mut app, 0, 1);
        let mut session = crate::stub_session(1);
        session.handoff_local_id = Some(0);
        App::register_session(&app.store, &session, None);
        app.pool.insert(session);
        app.next_session_id = app.next_session_id.max(2);
        let tree = pane::PaneTree::new(1);
        let tab = crate::register_terminal_tab(&mut app.tab_ids, &mut app.view_store, &tree)
            .expect("tab identity");
        let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
        ws.layouts.push(tree);
        ws.tabs.add();
        ws.tab_set.push(tab).expect("fresh tab id");
        app.resync_active_or_window(WindowId(0));

        let pid = cj::reaped_pid();
        app.crash_journal = Lane::new(Some(dir.path().to_path_buf()), pid);
        app.crash_journal
            .set_predecessor(parent.pid, Some(parent.nanos));
        let own = app.crash_journal.journal_id();
        cj::marker(logs.path(), own);
        app.crash_journal
            .take_over_at_commit(true)
            .expect("a successor's lane prepares its take-over")
            .run(|| {});
        assert_eq!(app.crash_journal.adopt_predecessor(true), Some(parent));
        app.first_present_done = true;
        app.incoming_handoff_pending = false;
        Self {
            app,
            dir,
            logs,
            parent,
            own,
            pid,
        }
    }

    fn first_image(&mut self) {
        self.app.tick_crash_journal(std::time::Instant::now());
        let image = self.dir.path().join(self.own.file_name());
        let pid = self.pid;
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the successor's own first image",
            || {
                std::fs::read_to_string(&image)
                    .ok()
                    .and_then(|text| crate::crash_journal::decode(&text).ok())
                    .filter(|(header, _)| header.pid == pid)
                    .map(|_| ())
            },
        );
    }

    fn killed(mut self) -> crate::crash_journal::Reopened {
        use crate::crash_journal::{self, tests as cj};
        drop(std::mem::replace(
            &mut self.app.crash_journal,
            crash_journal::Lane::inert(),
        ));
        cj::await_released(&self.dir.path().join(self.own.lock_name()));
        crash_journal::claim_at_boot(
            self.dir.path(),
            &crash_journal::Evidence {
                log_dir: Some(self.logs.path()),
                swept: &[],
            },
            std::process::id(),
            crash_journal::QuitLayout::default(),
        )
        .reopened
        .expect("the desk comes back")
    }
}

#[cfg(unix)]
#[test]
fn a_successors_first_image_keeps_the_agent_and_folders_it_took_over() {
    use crate::crash_journal::tests::cwds;
    let before = Successor::of_a_handed_off_desk().killed();
    assert_eq!(
        cwds(&before.manifest),
        vec!["/a", "/b"],
        "the parent's image"
    );
    assert_eq!(before.resumed(true), vec![(0, 1)]);

    let mut successor = Successor::of_a_handed_off_desk();
    let (parent, pid) = (successor.parent.pid, successor.pid);
    successor.first_image();
    let mut quit = successor.app.capture_restore_manifest();
    successor.app.fill_desk_agents(&mut quit);
    let conversations: Vec<_> = quit
        .handed_off_leaves()
        .iter()
        .map(|leaf| leaf.agent.as_ref().and_then(|agent| agent.session.clone()))
        .collect();
    assert_eq!(
        conversations,
        vec![Some(Successor::CONVERSATION.to_string()), None],
        "a restart's quit layout keeps the agent the parent handed over running"
    );
    let reopened = successor.killed();
    assert_eq!(reopened.writers, vec![pid], "the successor's own image");
    assert_eq!(
        cwds(&reopened.manifest),
        vec!["/b", "/a"],
        "each pane keeps its folder, in the successor's own tab order"
    );
    assert_eq!(
        reopened.resumed(true),
        vec![(0, 0)],
        "Claude comes back on its conversation, in the tab its shell is in"
    );
    assert_eq!(
        reopened.handed,
        vec![
            (
                pid,
                crate::crash_journal::Handed {
                    id: 0,
                    pid: parent,
                    was: 1
                }
            ),
            (
                pid,
                crate::crash_journal::Handed {
                    id: 1,
                    pid: parent,
                    was: 0
                }
            ),
        ],
        "each leaf names the parent's pair it was handed as"
    );

    let mut successor = Successor::of_a_handed_off_desk();
    {
        let session = successor.app.pool.get(0).expect("session 0");
        let mut timeline = session
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(4241);
        timeline.set_program(4241, Some("zsh".to_string()));
    }
    successor.first_image();
    let reopened = successor.killed();
    assert!(reopened.resumed(true).is_empty(), "no agent runs there");
    assert_eq!(cwds(&reopened.manifest), vec!["/b", "/a"]);
}

#[cfg(unix)]
#[test]
fn a_successors_own_image_places_the_shells_the_keeper_holds_under_the_parent() {
    let mut successor = Successor::of_a_handed_off_desk();
    let parent = successor.parent.pid;
    successor.first_image();
    let reopened = successor.killed();
    let writers: Vec<Option<u32>> = reopened.writers.iter().copied().map(Some).collect();
    let device = |local: u64| u64::from(parent) * 100 + local;
    for handed in [true, false] {
        let mut layout = reopened.manifest.clone();
        let mut shells = vec![recovered_shell(parent, 0), recovered_shell(parent, 1)];
        crate::keeper_link::place_by_owner(
            &mut shells,
            Some(&mut layout),
            &writers,
            if handed { &reopened.handed } else { &[] },
        );
        let shell0 = super::take_session0_shell(&mut shells, Some(&layout));
        let pane1 =
            super::take_handed_off_shell(&mut shells, layout.handed_off_leaves()[1].local_id);
        if handed {
            assert_eq!(
                shell0.as_ref().and_then(whose),
                Some((Some(parent), device(1))),
                "the agent's pane (the successor's tab 0) takes its live shell"
            );
            assert_eq!(
                pane1.as_ref().and_then(whose),
                Some((Some(parent), device(0))),
                "and pane `/a` takes the parent's shell 0"
            );
            assert!(shells.is_empty(), "no orphan");
        } else {
            assert!(shell0.is_none() && pane1.is_none(), "the control");
            assert_eq!(shells.len(), 2, "both shells are the orphan net's");
        }
    }
}

#[test]
fn a_recovered_shell_fills_only_a_pane_its_own_window_wrote() {
    const A: u32 = 4101;
    const B: u32 = 4202;
    let two_windows = || {
        restore::RestoreManifest::new(vec![
            window_of(vec![leaf_naming(0), leaf_naming(1)]),
            window_of(vec![leaf_naming(0), leaf_naming(1)]),
        ])
    };
    let offered = |first: u32, then: u32| {
        vec![
            recovered_shell(first, 0),
            recovered_shell(first, 1),
            recovered_shell(then, 0),
            recovered_shell(then, 1),
        ]
    };
    let device = |owner: u32, local: u64| u64::from(owner) * 100 + local;

    let mut layout = two_windows();
    let mut shells = offered(B, A);
    crate::keeper_link::place_by_owner(&mut shells, Some(&mut layout), &[Some(A), Some(B)], &[]);
    let shell0 = super::take_session0_shell(&mut shells, Some(&layout)).expect("session 0");
    assert_eq!(whose(&shell0), Some((Some(A), device(A, 0))));
    let leaves = layout.handed_off_leaves();
    assert_eq!(leaves.len(), 4);
    for (leaf, (owner, local)) in leaves.iter().zip([(A, 0), (A, 1), (B, 0), (B, 1)]).skip(1) {
        let shell = super::take_handed_off_shell(&mut shells, leaf.local_id)
            .unwrap_or_else(|| panic!("the pane of {owner}'s shell {local} takes one"));
        assert_eq!(
            whose(&shell),
            Some((Some(owner), device(owner, local))),
            "the pane window {owner} wrote for its shell {local}"
        );
    }
    assert!(shells.is_empty(), "every shell found its own pane");

    let mut layout =
        restore::RestoreManifest::new(vec![window_of(vec![leaf_naming(0), leaf_naming(1)])]);
    let mut shells = offered(A, B);
    crate::keeper_link::place_by_owner(&mut shells, Some(&mut layout), &[Some(B)], &[]);
    let shell0 = super::take_session0_shell(&mut shells, Some(&layout)).expect("session 0");
    assert_eq!(whose(&shell0), Some((Some(B), device(B, 0))));
    let leaf1 = layout.handed_off_leaves()[1].local_id;
    let shell1 = super::take_handed_off_shell(&mut shells, leaf1).expect("B's pane 1");
    assert_eq!(whose(&shell1), Some((Some(B), device(B, 1))));
    assert_eq!(
        shells.iter().map(whose).collect::<Vec<_>>(),
        vec![Some((Some(A), device(A, 0))), Some((Some(A), device(A, 1)))],
        "A's shells wait for the orphan net"
    );

    let mut layout = two_windows();
    let mut shells = offered(A, B);
    shells.push(crate::spawn::Adopted {
        keeper: Some(crate::keeper_link::Claim::new(77, None)),
        ..handed_off_shell(0)
    });
    crate::keeper_link::place_by_owner(&mut shells, Some(&mut layout), &[], &[]);
    assert!(super::take_session0_shell(&mut shells, Some(&layout)).is_none());
    let named: Vec<Option<u64>> = layout
        .handed_off_leaves()
        .iter()
        .map(|leaf| leaf.local_id)
        .collect();
    for id in &named {
        assert!(super::take_handed_off_shell(&mut shells, *id).is_none());
    }
    assert_eq!(shells.len(), 5, "every shell is the orphan net's");
    assert!(
        shells
            .iter()
            .all(|shell| !named.contains(&Some(shell.local_id))),
        "no orphan carries a leaf's id, so none takes a leaf's identity"
    );
}

#[test]
fn an_offered_master_that_becomes_no_session_is_released() {
    let mut new = App::headless_for_test();
    new.frontmost_window = None;
    new.seamless_adopt = vec![recovered_shell(9101, 4)];
    new.adopt_orphan_shells_as_tabs(&[]);
    assert!(new.seamless_adopt.is_empty());
    assert!(
        crate::keeper_link::released_unclaimed(9101 * 100 + 4),
        "the unplaced shell's master is released to the keeper"
    );

    let mut new = App::headless_for_test();
    new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0)]));
    new.seamless_adopt = vec![recovered_shell(9202, 5)];
    new.adopt_orphan_shells_as_tabs(&[]);
    assert!(new.seamless_adopt.is_empty(), "the net placed it");
    assert!(
        !crate::keeper_link::released_unclaimed(9202 * 100 + 5),
        "a placed shell is registered, not released"
    );
}

fn identity_of(shell: u64) -> crate::session_timeline::SessionMeta {
    let words = crate::session_timeline::QUESTIONS_POLICY_WORDS;
    let word = words[usize::try_from(shell).expect("a small index") % words.len()];
    crate::session_timeline::SessionMeta {
        user_title: Some(format!("shell {shell}")),
        description: Some(format!("the work shell {shell} was doing")),
        icon: Some(format!("🐚{shell}")),
        role: Some(format!("worker:shell-{shell}")),
        attention: Some(format!("shell {shell} waits on a review")),
        questions: Some(word.to_string()),
        ..crate::session_timeline::SessionMeta::default()
    }
}

fn leaf_naming(shell: u64) -> restore::RestoredSplitTree {
    let identity = identity_of(shell);
    restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
        restore::TerminalLeafRestore {
            cwd: Some(format!("/tmp/shell-{shell}")),
            title: format!("zsh {shell}"),
            local_id: Some(shell),
            user_title: identity.user_title,
            description: identity.description,
            icon: identity.icon,
            role: identity.role,
            attention: identity.attention,
            questions: identity.questions,
            ..bare_leaf()
        },
    ))
}

fn native_leaf(descriptor: restore::NativeLeafRestore) -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::leaf(restore::RestoredView::Native(descriptor))
}

fn settings_leaf() -> restore::RestoredSplitTree {
    native_leaf(restore::NativeLeafRestore::settings("/about".to_string()))
}

fn placeholder_leaf() -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::leaf(restore::RestoredView::Placeholder(
        restore::PlaceholderLeafRestore {
            restore_tag: "markdown".to_string(),
            reason: "Document could not be reopened".to_string(),
            metadata: String::new(),
            unhanded: None,
        },
    ))
}

fn split_of(
    first: restore::RestoredSplitTree,
    second: restore::RestoredSplitTree,
) -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::Split {
        axis: restore::SplitKind::Horizontal,
        ratio: 0.5,
        first: Box::new(first),
        second: Box::new(second),
    }
}

fn window_of(roots: Vec<restore::RestoredSplitTree>) -> restore::WindowLayout {
    let mut layout = single_leaf_window(bare_leaf());
    layout.restored_tabs = roots
        .into_iter()
        .map(|root| {
            let mut focused_path = Vec::new();
            let mut node = &root;
            while let restore::RestoredSplitTree::Split { first, .. } = node {
                focused_path.push(restore::RestoreBranch::First);
                node = first;
            }
            restore::RestoredTab {
                root,
                focused_path,
                zoomed: false,
            }
        })
        .collect();
    layout
}

fn successor_of(
    windows: Vec<restore::WindowLayout>,
    shells: u64,
    session0: fn(
        &mut Vec<crate::spawn::Adopted>,
        &restore::RestoreManifest,
    ) -> Option<crate::spawn::Adopted>,
) -> App {
    let ids = (0..shells).collect::<Vec<_>>();
    let wire = restore::RestoreManifest::new(windows)
        .to_toml()
        .expect("the parent serializes its layout");
    let carried = restore::RestoreManifest::from_toml(&wire)
        .filter(|layout| layout.covers_exact_seamless_ids(&ids))
        .expect("the child accepts the sidecar");
    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    let mut handed_off = ids.iter().copied().map(handed_off_shell).collect();
    if let Some(shell) = session0(&mut handed_off, &carried) {
        adopt_as(&mut new, 0, shell.local_id);
    }
    new.seamless_adopt = handed_off;
    let mut windows = carried.windows.into_iter();
    new.restore_into_window(WindowId(0), windows.next().expect("window 0"));
    for layout in windows {
        if new.restores_without_bootstrap(&layout) {
            let wid = new.create_native_restore_window(layout.rows, layout.cols);
            new.restore_into_window(wid, layout);
            continue;
        }
        let mut bootstrap = crate::stub_session(new.next_session_id);
        bootstrap.handoff_local_id =
            super::take_handed_off_shell(&mut new.seamless_adopt, layout.bootstrap_local_id())
                .map(|shell| shell.local_id);
        App::register_session(&new.store, &bootstrap, None);
        let wid = new.insert_logical_window(bootstrap, 24, 80);
        new.restore_into_window(wid, layout);
    }
    assert!(
        new.seamless_adopt.is_empty(),
        "PRECONDITION: the layout placed every handed-off shell"
    );
    new
}

fn shown_shells(app: &App) -> Vec<Vec<Vec<Option<Option<u64>>>>> {
    app.windows
        .values()
        .map(|window| {
            window
                .tab_set
                .tabs()
                .iter()
                .map(|tab| {
                    tab.root
                        .leaves()
                        .into_iter()
                        .map(|view| {
                            let session = app
                                .view_store
                                .get(view)
                                .copied()
                                .and_then(crate::tab_model::View::terminal_session)?;
                            Some(app.pool.get(session)?.handoff_local_id)
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

fn assert_tooltips_name_their_shells(app: &App, shape: &str) {
    for window in app.windows.values() {
        for tab in window.tab_set.tabs() {
            let Some(shell) = app
                .view_store
                .get(tab.focus)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                .and_then(|session| app.pool.get(session))
                .and_then(|session| session.handoff_local_id)
            else {
                continue;
            };
            assert!(
                tab.presentation
                    .tooltip
                    .as_deref()
                    .is_some_and(|tip| tip.starts_with(&format!("🐚{shell}"))),
                "{shape}: shell {shell}'s tab tooltip {:?}",
                tab.presentation.tooltip
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn a_co_viewed_session_keeps_the_handoff_layout_and_both_of_its_views() {
    let mut old = App::headless_for_test();
    let w1 = old.insert_logical_window(crate::stub_session(1), 24, 80);
    App::register_session(&old.store, old.pool.get(1).expect("shell 1"), None);
    old.frontmost_window = Some(w1);
    let w2 = old
        .open_active_session_in_new_window_logical()
        .expect("the co-view window");
    assert_eq!(old.window_terminal_sessions(w2), vec![1]);
    let mut handed = old
        .handoff_live_sessions()
        .iter()
        .map(|(id, _, _)| *id)
        .collect::<Vec<_>>();
    handed.sort_unstable();
    assert_eq!(handed, vec![0, 1], "each shell is handed once");
    let layout = old.capture_handoff_layout();
    let wire = layout.to_toml().expect("the capture serializes");
    let placed = restore::RestoreManifest::from_toml(&wire)
        .expect("the successor parses it")
        .placed_for_handed(&handed)
        .expect("a co-viewed session must not unplace the whole handoff layout");
    assert_eq!(placed.windows.len(), 3, "every window is kept");

    let new = successor_of(placed.windows, 2, |handed_off, layout| {
        super::take_session0_shell(handed_off, Some(layout))
    });
    assert_eq!(
        shown_shells(&new),
        vec![
            vec![vec![Some(Some(0))]],
            vec![vec![Some(Some(1))]],
            vec![vec![Some(Some(1))]],
        ],
        "each window shows the shell it showed"
    );
    assert_eq!(new.pool.iter().count(), 2, "no stand-in shell was spawned");
    let shown_in = |wid: WindowId| new.window_terminal_sessions(wid);
    let windows = new.windows.keys().copied().collect::<Vec<_>>();
    assert_eq!(
        shown_in(windows[1]),
        shown_in(windows[2]),
        "the two views show one session, as the menu command made them"
    );
}

#[cfg(unix)]
fn handed_of(old: &App) -> Vec<u64> {
    let mut handed = old
        .handoff_live_sessions()
        .iter()
        .map(|(id, _, _)| *id)
        .collect::<Vec<_>>();
    handed.sort_unstable();
    handed
}

#[cfg(unix)]
fn handoff_layout_placed(old: &App) -> (restore::RestoreManifest, restore::RestoreManifest) {
    let layout = old.capture_handoff_layout();
    let wire = layout.to_toml().expect("the capture serializes");
    let read = restore::RestoreManifest::from_toml(&wire).expect("the successor parses it");
    assert_eq!(
        read, layout,
        "the producer's round trip holds: the layout is within the caps"
    );
    let placed = read
        .placed_for_handed(&handed_of(old))
        .expect("the layout names every handed shell: nothing goes to the orphan net");
    (layout, placed)
}

#[cfg(unix)]
#[test]
fn a_desk_past_the_window_cap_folds_into_the_handoff_layout_and_places_every_shell() {
    let mut old = App::headless_for_test();
    for shell in 1..70 {
        old.insert_logical_window(crate::stub_session(shell), 24, 80);
        App::register_session(&old.store, old.pool.get(shell).expect("the shell"), None);
    }
    assert_eq!(
        handed_of(&old),
        (0..70).collect::<Vec<_>>(),
        "PRECONDITION: every shell is handed"
    );
    let (layout, placed) = handoff_layout_placed(&old);
    assert_eq!(layout.windows.len(), restore::MAX_WINDOWS);
    assert_eq!(
        layout.fold,
        restore::LayoutFold {
            windows: 6,
            tabs: 0,
            front: (0..70).collect(),
        },
        "the layout says what was folded, for the successor to say, and which \
         panes were in front before it"
    );
    assert_eq!(
        layout.windows[restore::MAX_WINDOWS - 1]
            .tabs
            .iter()
            .map(restore::PaneLayout::local_id)
            .collect::<Vec<_>>(),
        vec![Some(63)],
        "the legacy mirror names the window's own shell, never a moved one"
    );

    let new = successor_of(placed.windows, 70, |handed_off, layout| {
        super::take_session0_shell(handed_off, Some(layout))
    });
    let shown = shown_shells(&new);
    assert_eq!(shown.len(), restore::MAX_WINDOWS, "every window that fits");
    for (index, window) in shown.iter().enumerate().take(restore::MAX_WINDOWS - 1) {
        assert_eq!(
            window,
            &vec![vec![Some(Some(index as u64))]],
            "window {index} shows its own shell"
        );
    }
    assert_eq!(
        shown[restore::MAX_WINDOWS - 1],
        (63..70)
            .map(|shell| vec![Some(Some(shell))])
            .collect::<Vec<_>>(),
        "the last window holds its own tab, then a tab for each window past the cap, in order"
    );
}

#[cfg(unix)]
#[test]
fn the_successor_of_a_folded_layout_captures_a_layout_every_build_reads_whole() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-folded-capture-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("notes.md");
    std::fs::write(&path, "# Notes\n").unwrap();
    let notes = || {
        native_leaf(restore::NativeLeafRestore::document(
            "markdown",
            file_uri(&path),
        ))
    };
    let mut windows = (0..63)
        .map(|shell| window_of(vec![leaf_naming(shell)]))
        .collect::<Vec<_>>();
    windows.push(window_of(vec![leaf_naming(63), settings_leaf(), notes()]));
    windows.push(window_of(vec![settings_leaf(), notes(), leaf_naming(64)]));
    let mut layout = restore::RestoreManifest::new(windows);
    layout.fold_into_caps();
    assert!(layout.admits_whole(), "PRECONDITION: within the caps");

    let new = successor_of(layout.windows, 65, |handed_off, layout| {
        super::take_session0_shell(handed_off, Some(layout))
    });
    let leaves_of = |window: &restore::WindowLayout| {
        window
            .restored_tabs
            .iter()
            .map(|tab| match &tab.root {
                restore::RestoredSplitTree::Leaf {
                    view: restore::RestoredView::Terminal(terminal),
                } => format!("{:?}", terminal.local_id),
                restore::RestoredSplitTree::Leaf {
                    view: restore::RestoredView::Native(native),
                } => native.restore_tag.clone(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
    };
    for (label, capture) in [
        ("the next update's", new.capture_handoff_layout()),
        ("the quit's", new.capture_restore_manifest()),
    ] {
        assert_eq!(
            capture.why_not_whole(),
            None,
            "{label} capture reads back whole"
        );
        let last = &capture.windows[restore::MAX_WINDOWS - 1];
        assert_eq!(
            leaves_of(last),
            [
                "Some(63)", "settings", "markdown", "settings", "markdown", "Some(64)"
            ],
            "{label}: every tab, in the canonical tabs"
        );
        assert!(
            matches!(
                last.native_tabs.as_slice(),
                [
                    restore::NativeTabRestore::Settings { .. },
                    restore::NativeTabRestore::Markdown { .. }
                ]
            ),
            "{label}: one of each in the legacy mirror: {:?}",
            last.native_tabs
        );
        let read = restore::RestoreManifest::from_toml(&capture.to_toml().unwrap())
            .expect("the capture parses");
        assert_eq!(
            read.windows.len(),
            restore::MAX_WINDOWS,
            "{label}: every window reads back"
        );
        assert_eq!(
            leaves_of(&read.windows[restore::MAX_WINDOWS - 1]),
            leaves_of(last),
            "{label}: the folded window reads back with every tab"
        );
    }
    let next = new.capture_handoff_layout();
    assert_eq!(
        restore::RestoreManifest::from_toml(&next.to_toml().unwrap()).as_ref(),
        Some(&next),
        "the next update's round trip holds"
    );
    assert!(
        crate::app_update_handoff::commit_handoff_layout(&next).is_ok(),
        "the next update's park commits to its layout"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_window_past_the_tab_cap_spills_into_another_window_of_the_handoff_layout() {
    let mut old = App::headless_for_test();
    for shell in 1..300 {
        old.push_stub_tab(WindowId(0), crate::stub_session(shell));
        App::register_session(&old.store, old.pool.get(shell).expect("the shell"), None);
    }
    assert_eq!(handed_of(&old).len(), 300, "PRECONDITION");
    let (layout, placed) = handoff_layout_placed(&old);
    assert_eq!(
        layout
            .windows
            .iter()
            .map(|window| window.restored_tabs.len())
            .collect::<Vec<_>>(),
        vec![256, 44]
    );
    assert_eq!(
        layout.fold,
        restore::LayoutFold {
            windows: 0,
            tabs: 44,
            front: vec![299],
        }
    );
    assert_eq!(
        layout
            .windows
            .iter()
            .map(|window| window.active_item)
            .collect::<Vec<_>>(),
        vec![Some(255), Some(43)]
    );
    assert_eq!(
        (layout.windows[1].rows, layout.windows[1].cols),
        (layout.windows[0].rows, layout.windows[0].cols),
        "the moved shells keep their grid"
    );

    let new = successor_of(placed.windows, 300, |handed_off, layout| {
        super::take_session0_shell(handed_off, Some(layout))
    });
    let shown = shown_shells(&new);
    let tabs_of = |shells: std::ops::Range<u64>| {
        shells
            .map(|shell| vec![Some(Some(shell))])
            .collect::<Vec<_>>()
    };
    assert_eq!(
        shown,
        vec![tabs_of(0..256), tabs_of(256..300)],
        "the first 256 tabs stay; the rest, in order, are the next window's"
    );
}

#[cfg(unix)]
fn owe_a_proof(app: &mut App, label: &str) -> std::os::fd::OwnedFd {
    use std::os::fd::FromRawFd as _;
    let mut fds = [0i32; 2];
    // SAFETY: pipe(2) into a valid two-slot out-array.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    // SAFETY: two fresh descriptors, one owner each from here.
    let (read, write) = unsafe {
        (
            std::os::fd::OwnedFd::from_raw_fd(fds[0]),
            std::os::fd::OwnedFd::from_raw_fd(fds[1]),
        )
    };
    app.handoff_ready = Some(crate::seamless::ReadySignal::for_test(
        write, label, [0; 32], [0; 32],
    ));
    read
}

#[cfg(unix)]
#[test]
fn a_co_view_window_without_glass_degrades_the_handoff_when_it_drops_a_handed_shell() {
    for (co_view, shells, drops) in [
        (vec![leaf_naming(0)], 2, false),
        (vec![leaf_naming(0), leaf_naming(2)], 3, true),
    ] {
        let mut new = successor_of(
            vec![
                window_of(vec![leaf_naming(0)]),
                window_of(vec![leaf_naming(1)]),
                window_of(co_view),
            ],
            shells,
            |handed_off, layout| super::take_session0_shell(handed_off, Some(layout)),
        );
        let window = *new.windows.keys().last().expect("the co-view window");
        let _ready = owe_a_proof(&mut new, "co-view-glass");
        new.close_window_logical(window);
        let mut pooled = new
            .pool
            .iter()
            .filter_map(|session| session.handoff_local_id)
            .collect::<Vec<_>>();
        pooled.sort_unstable();
        assert_eq!(
            pooled,
            vec![0, 1],
            "drops = {drops}: shell 0 stays in window 0; only a shell shown in the co-view \
             window alone goes with it"
        );
        assert_eq!(
            new.handoff_degraded, drops,
            "drops = {drops}: a handed shell lost before the proof degrades the handoff, \
             and a co-view's second view does not"
        );
    }
}

#[test]
fn a_co_viewed_held_pane_comes_back_in_both_of_its_windows() {
    const HELD: u64 = 7;
    let wire = restore::RestoreManifest::new(vec![
        window_of(vec![leaf_naming(0)]),
        window_of(vec![leaf_naming(HELD)]),
        window_of(vec![leaf_naming(HELD)]),
    ])
    .to_toml()
    .expect("the parent serializes its layout");
    let placed = restore::RestoreManifest::from_toml(&wire)
        .expect("the successor parses it")
        .placed_for_handed(&[0])
        .expect("a held pane's leaves are placeholders, not a refusal");
    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    adopt_as(&mut new, 0, 0);
    let checkpoint = {
        let mut t = aterm_core::terminal::Terminal::new(24, 80);
        t.process(b"build finished\r\n");
        t.checkpoint_carry_abandoning_partial(0).0
    };
    new.seamless_held = vec![crate::seamless::HeldPane {
        local_id: HELD,
        sid: None,
        checkpoint,
        exit: Some(aterm_pty::ChildExit::Code(0)),
        history: crate::handoff_history::AdoptedHistory::default(),
        control: None,
        failure_seen: false,
    }];
    let mut windows = placed.windows.into_iter();
    new.restore_into_window(WindowId(0), windows.next().expect("window 0"));
    let mut held_windows = Vec::new();
    for layout in windows {
        assert!(
            new.restores_without_bootstrap(&layout),
            "PRECONDITION: a window of held placeholders is rebuilt with no bootstrap"
        );
        let wid = new.create_native_restore_window(layout.rows, layout.cols);
        new.restore_into_window(wid, layout);
        held_windows.push(wid);
    }
    let shown = held_windows
        .iter()
        .map(|wid| new.window_terminal_sessions(*wid))
        .collect::<Vec<_>>();
    let session = shown[0]
        .first()
        .copied()
        .expect("window 1 shows the held pane");
    assert_eq!(
        shown,
        vec![vec![session], vec![session]],
        "both windows show the one held session, as before the update"
    );
    assert_eq!(new.pool.views(session), Some(2), "one session, two views");
    let text = {
        let held = crate::term_lock(&new.pool.get(session).expect("pooled").term);
        (0..usize::from(held.rows()))
            .filter_map(|row| held.row_text(row))
            .collect::<String>()
    };
    assert!(
        text.contains("build finished"),
        "its final screen, not a placeholder: {text:?}"
    );
}

#[test]
fn await_log_on_a_restored_held_pane_answers_exited_at_once() {
    const HELD: u64 = 7;
    let wire = restore::RestoreManifest::new(vec![
        window_of(vec![leaf_naming(0)]),
        window_of(vec![leaf_naming(HELD)]),
    ])
    .to_toml()
    .expect("the parent serializes its layout");
    let placed = restore::RestoreManifest::from_toml(&wire)
        .expect("the successor parses it")
        .placed_for_handed(&[0])
        .expect("a held pane's leaf is a placeholder, not a refusal");
    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    adopt_as(&mut new, 0, 0);
    let checkpoint = {
        let mut t = aterm_core::terminal::Terminal::new(24, 80);
        t.process(b"build finished\r\n");
        t.checkpoint_carry_abandoning_partial(0).0
    };
    new.seamless_held = vec![crate::seamless::HeldPane {
        local_id: HELD,
        sid: None,
        checkpoint,
        exit: Some(aterm_pty::ChildExit::Code(0)),
        history: crate::handoff_history::AdoptedHistory::default(),
        control: None,
        failure_seen: false,
    }];
    let mut windows = placed.windows.into_iter();
    new.restore_into_window(WindowId(0), windows.next().expect("window 0"));
    let layout = windows.next().expect("the held pane's window");
    let wid = new.create_native_restore_window(layout.rows, layout.cols);
    new.restore_into_window(wid, layout);
    let session = new
        .window_terminal_sessions(wid)
        .first()
        .copied()
        .expect("the window shows the held pane");
    assert_eq!(
        new.store.read().unwrap().by_local(session).map(|h| h.state),
        Some(crate::session_store::SessionState::Exited),
        "PRECONDITION: registered and ended"
    );
    let held = new.pool.get(session).expect("pooled");
    for what in ["close=session", "need", "close=run", "kind=verdict"] {
        assert_eq!(
            crate::control::cmd_await(
                &held.term,
                &new.store,
                session,
                &held.ctx,
                &format!("log {what} timeout=0"),
            ),
            "ERR exited\n",
            "{what}: no close is coming"
        );
    }
}

#[test]
fn a_handoff_carries_each_identity_onto_the_shell_its_leaf_names_and_never_a_stand_in() {
    let shapes = [
        (
            "window 0 is one native tab; shell 0's leaf is a later pane of window 1",
            vec![
                window_of(vec![settings_leaf()]),
                window_of(vec![
                    split_of(leaf_naming(2), leaf_naming(0)),
                    leaf_naming(1),
                ]),
            ],
        ),
        (
            "window 0 is one native tab; shell 0's leaf is window 1's first pane",
            vec![
                window_of(vec![settings_leaf()]),
                window_of(vec![
                    leaf_naming(0),
                    split_of(leaf_naming(1), leaf_naming(2)),
                ]),
            ],
        ),
        (
            "window 0 opens on a native tab and has its terminal leaf later",
            vec![
                window_of(vec![
                    settings_leaf(),
                    split_of(leaf_naming(1), leaf_naming(2)),
                ]),
                window_of(vec![leaf_naming(0)]),
            ],
        ),
    ];
    for (shape, windows) in shapes {
        let named = windows
            .iter()
            .map(|window| {
                window
                    .restored_tabs
                    .iter()
                    .map(|tab| {
                        let mut leaves = Vec::new();
                        let mut stack = vec![&tab.root];
                        while let Some(node) = stack.pop() {
                            match node {
                                restore::RestoredSplitTree::Leaf {
                                    view: restore::RestoredView::Terminal(leaf),
                                } => leaves.push(Some(leaf.local_id)),
                                restore::RestoredSplitTree::Leaf { .. } => leaves.push(None),
                                restore::RestoredSplitTree::Split { first, second, .. } => {
                                    stack.push(second);
                                    stack.push(first);
                                }
                            }
                        }
                        leaves
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let new = successor_of(windows, 3, |handed_off, layout| {
            super::take_session0_shell(handed_off, Some(layout))
        });

        assert_eq!(
            shown_shells(&new),
            named,
            "{shape}: every shell is in the pane its leaf names, and no window holds a \
             pane or tab the layout did not"
        );
        let mut adopted = Vec::new();
        for (&session, pooled) in &new.pool.sessions {
            let shell = pooled.session.handoff_local_id.unwrap_or_else(|| {
                panic!("{shape}: session {session} is no handed-off shell — a stand-in")
            });
            adopted.push(shell);
            assert_eq!(
                registry_meta(&new, session),
                identity_of(shell),
                "{shape}: shell {shell} (session {session}) wears exactly the identity of \
                 the leaf that names it"
            );
        }
        adopted.sort_unstable();
        assert_eq!(adopted, vec![0, 1, 2], "{shape}: every shell is live");
        assert_tooltips_name_their_shells(&new, shape);
        assert!(new.structural_invariants_ok(), "{shape}");
    }

    let mut handed_off = (0..3).map(handed_off_shell).collect::<Vec<_>>();
    assert_eq!(
        super::take_session0_shell(&mut handed_off, None).map(|shell| shell.local_id),
        Some(0)
    );
    assert_eq!(handed_off.len(), 2);
}

#[test]
fn an_orphaned_shell_comes_back_wearing_the_identity_of_the_leaf_that_names_it() {
    let wire = restore::RestoreManifest::new(vec![
        window_of(vec![leaf_naming(0)]),
        window_of(vec![
            leaf_naming(1),
            split_of(leaf_naming(2), leaf_naming(3)),
        ]),
    ])
    .to_toml()
    .expect("the parent serializes its layout");
    let carried = restore::RestoreManifest::from_toml(&wire)
        .filter(|layout| layout.covers_exact_seamless_ids(&[0, 1, 2, 3]))
        .expect("the child accepts the sidecar");
    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    let mut handed_off = (0..4).map(handed_off_shell).collect();
    let shell0 = super::take_session0_shell(&mut handed_off, Some(&carried))
        .expect("window 0's bootstrap leaf names shell 0");
    adopt_as(&mut new, 0, shell0.local_id);
    new.seamless_adopt = handed_off;
    let leaves = carried.handed_off_leaves();
    new.restore_into_window(WindowId(0), carried.windows[0].clone());
    assert_eq!(
        new.seamless_adopt.len(),
        3,
        "PRECONDITION: window 1's shells are left over"
    );

    new.adopt_orphan_shells_as_tabs(&leaves);
    assert!(new.seamless_adopt.is_empty(), "the net placed every orphan");
    let mut adopted = Vec::new();
    for (&session, pooled) in &new.pool.sessions {
        let shell = pooled
            .session
            .handoff_local_id
            .expect("every session is a handed-off shell");
        adopted.push(shell);
        assert_eq!(
            registry_meta(&new, session),
            identity_of(shell),
            "shell {shell} (session {session}) wears the identity of the leaf that names it"
        );
    }
    adopted.sort_unstable();
    assert_eq!(adopted, vec![0, 1, 2, 3], "every shell is live");
    assert_eq!(
        shown_shells(&new),
        vec![vec![
            vec![Some(Some(0))],
            vec![Some(Some(1))],
            vec![Some(Some(2))],
            vec![Some(Some(3))],
        ]],
        "each orphan is a tab of its own in the front window"
    );
    assert_tooltips_name_their_shells(&new, "orphans");
    assert!(new.structural_invariants_ok());
}

#[test]
fn the_bootstrap_cwd_is_the_directory_of_the_leaf_the_bootstrap_fills() {
    let mut old = App::headless_for_test();
    old.restore_into_window(
        WindowId(0),
        window_of(vec![
            settings_leaf(),
            split_of(
                placeholder_leaf(),
                restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(bare_leaf())),
            ),
            restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(bare_leaf())),
        ]),
    );
    let feed = |session: u64, bytes: &[u8]| {
        crate::term_lock(&old.pool.get(session).expect("live session").term).process(bytes);
    };
    feed(1, b"\x1b]7;file://localhost/aterm-proof/other-tab\x07");
    let carried = |app: &App| {
        restore::RestoreManifest::from_toml(
            &app.capture_restore_manifest()
                .to_toml()
                .expect("the layout serializes"),
        )
        .expect("the layout parses")
    };

    let manifest = carried(&old);
    let window = &manifest.windows[0];
    let mirror_first = window
        .tabs
        .first()
        .and_then(|tab| tab.leaves().first().copied())
        .map(|leaf| (leaf.local_id(), leaf.cwd().map(str::to_owned)));
    assert_eq!(
        (mirror_first, window.bootstrap_local_id()),
        (
            Some((Some(1), Some("/aterm-proof/other-tab".to_string()))),
            Some(0)
        ),
        "PRECONDITION: the mirror's first leaf is session 1's tab; the bootstrap fills \
         session 0's pane"
    );

    feed(0, b"\x1b]7;file://localhost/aterm-proof/split-pane\x07");
    let manifest = carried(&old);
    assert_eq!(
        manifest.first_leaf_cwd().as_deref(),
        Some("/aterm-proof/split-pane"),
        "the bootstrap starts in the directory of the pane it fills"
    );
    assert_eq!(
        manifest.windows[0].bootstrap_cwd(),
        manifest.first_leaf_cwd(),
        "every window's bootstrap takes its cwd by the one pick"
    );

    let pane_in = |cwd: Option<&str>| {
        restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
            restore::TerminalLeafRestore {
                cwd: cwd.map(str::to_owned),
                ..bare_leaf()
            },
        ))
    };
    for (shape, window, expected) in [
        (
            "the bootstrap's pane recorded one",
            window_of(vec![split_of(pane_in(Some("/x")), pane_in(Some("/y")))]),
            Some("/x"),
        ),
        (
            "split first tab, its first pane recorded none",
            window_of(vec![split_of(pane_in(None), pane_in(Some("/y")))]),
            Some("/y"),
        ),
        (
            "nested split first tab, its first pane recorded none",
            window_of(vec![split_of(
                split_of(pane_in(None), pane_in(Some("/y"))),
                pane_in(Some("/z")),
            )]),
            Some("/y"),
        ),
        (
            "a native view first in the bootstrap's tab",
            window_of(vec![split_of(settings_leaf(), pane_in(Some("/y")))]),
            Some("/y"),
        ),
        (
            "no pane of the bootstrap's tab recorded one",
            window_of(vec![
                split_of(pane_in(None), pane_in(None)),
                pane_in(Some("/other-tab")),
            ]),
            None,
        ),
        (
            "behind a native-only first tab, still only the bootstrap's own tab",
            window_of(vec![
                settings_leaf(),
                split_of(settings_leaf(), pane_in(None)),
                pane_in(Some("/other-tab")),
            ]),
            None,
        ),
    ] {
        let manifest = restore::RestoreManifest::new(vec![window]);
        assert_eq!(manifest.first_leaf_cwd().as_deref(), expected, "{shape}");
    }
}

#[test]
fn a_graft_announces_every_field_it_moves_to_an_events_watcher() {
    let mut app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("the bootstrap").ctx.clone();
    let watermark = ctx.timeline.lock().unwrap().high_id();
    let watch = crate::session_watch::Subscription::single(&ctx.watchers);
    while watch.wait(std::time::Duration::ZERO) {}

    let mut leaf = bare_leaf();
    leaf.user_title = Some("fable driver".to_string());
    leaf.role = Some("agent:claude-driver-fable".to_string());
    app.restore_into_window(WindowId(0), single_leaf_window(leaf));

    let pushed: Vec<String> = ctx
        .timeline
        .lock()
        .unwrap()
        .since(watermark)
        .filter(|event| event.kind == "meta-change")
        .map(|event| event.payload.to_string())
        .collect();
    assert_eq!(
        pushed,
        vec![
            "field=title value=fable%20driver".to_string(),
            "field=role value=agent:claude-driver-fable".to_string(),
        ]
    );
    assert!(
        watch.wait(std::time::Duration::ZERO),
        "the session's `events` watcher was woken for them"
    );
}

fn hosted_app() -> (App, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    (app, host)
}

fn timed_session(app: &App, land_at: i64) -> Option<u64> {
    app.pool.iter().find_map(|session| {
        (session.ctx.meta.lock().unwrap().land_at == Some(land_at)).then_some(session.id)
    })
}

#[test]
fn a_timed_cruise_rides_the_cold_restore_the_reopen_and_the_graft() {
    use crate::session_timeline::{MetaEdit, MetaField};
    let wall = 1_792_177_200_000_i64;
    let mono = std::time::Instant::now();
    crate::autopilot_timers::set_fake_clocks(Some((mono, wall)));
    let timed = |land_at: i64| restore::TerminalLeafRestore {
        autopilot: Some("cruise".to_string()),
        land_at: Some(land_at),
        local_id: Some(1),
        ..bare_leaf()
    };

    let (mut app, host) = hosted_app();
    let passed = wall - 60_000;
    let mut layout = single_leaf_window(bare_leaf());
    layout.restored_tabs.push(restore::RestoredTab {
        root: restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(timed(passed))),
        focused_path: Vec::new(),
        zoomed: false,
    });
    app.restore_into_window(WindowId(0), layout);
    let session = timed_session(&app, passed).expect("the spawned shell carries the pair");
    assert_ne!(session, 0, "the second tab's own shell");
    assert_eq!(
        app.pool
            .get(session)
            .unwrap()
            .ctx
            .meta
            .lock()
            .unwrap()
            .autopilot
            .as_deref(),
        Some("cruise")
    );
    let entry = *app.autopilot_timers.entry(session).expect("indexed");
    assert_eq!(
        entry.passed_while,
        Some(crate::autopilot_timers::ClosedOver::Aterm)
    );
    assert_eq!(app.autopilot_timer_deadline(), Some(mono), "due at once");
    app.expire_autopilot_timers();
    let meta = app
        .pool
        .get(session)
        .unwrap()
        .ctx
        .meta
        .lock()
        .unwrap()
        .clone();
    assert_eq!(
        (meta.autopilot.as_deref(), meta.land_at),
        (Some("land"), None)
    );
    let sid = app
        .pool
        .get(session)
        .unwrap()
        .ctx
        .self_id
        .as_str()
        .to_string();
    let record = app
        .messages
        .log()
        .records()
        .rfind(|r| r.key.as_deref() == Some(format!("harness.autopilot.{sid}").as_str()))
        .map(|r| r.detail.join(" "))
        .expect("the landing's record");
    assert!(
        record.contains("landing time passed while aterm was closed; land is on"),
        "{record}"
    );
    host.shutdown_and_join();

    let (mut app, host) = hosted_app();
    let ahead = wall + 3_600_000;
    let record = restore::RestoredTab {
        root: restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(timed(ahead))),
        focused_path: Vec::new(),
        zoomed: false,
    };
    app.restore_closed_tab_into_window(WindowId(0), &record, 1)
        .expect("reopened");
    let session = timed_session(&app, ahead).expect("the reopened shell carries the pair");
    let entry = *app.autopilot_timers.entry(session).expect("indexed");
    assert_eq!(entry.passed_while, None);
    assert_eq!(
        app.autopilot_timer_deadline(),
        Some(mono + std::time::Duration::from_secs(3_600))
    );
    host.shutdown_and_join();

    let (mut app, host) = hosted_app();
    let record = restore::RestoredTab {
        root: restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(timed(passed))),
        focused_path: Vec::new(),
        zoomed: false,
    };
    app.restore_closed_tab_into_window(WindowId(0), &record, 1)
        .expect("reopened");
    let session = timed_session(&app, passed).expect("the reopened shell carries the pair");
    let entry = *app.autopilot_timers.entry(session).expect("indexed");
    assert_eq!(
        entry.passed_while,
        Some(crate::autopilot_timers::ClosedOver::Tab),
        "only the tab was closed"
    );
    assert_eq!(app.autopilot_timer_deadline(), Some(mono), "due at once");
    app.expire_autopilot_timers();
    assert_eq!(
        registry_meta(&app, session).autopilot.as_deref(),
        Some("land"),
        "landed"
    );
    let sid = app
        .pool
        .get(session)
        .unwrap()
        .ctx
        .self_id
        .as_str()
        .to_string();
    let record = app
        .messages
        .log()
        .records()
        .rfind(|r| r.key.as_deref() == Some(format!("harness.autopilot.{sid}").as_str()))
        .map(|r| r.detail.join(" "))
        .expect("the landing's record");
    assert!(
        record.contains("landing time passed while the tab was closed; land is on"),
        "{record}"
    );
    assert!(!record.contains("aterm was closed"), "{record}");
    host.shutdown_and_join();

    let (mut app, host) = hosted_app();
    app.handoff_successor = true;
    adopt_as(&mut app, 0, 0);
    let mut leaf = timed(ahead);
    leaf.local_id = Some(0);
    app.restore_into_window(WindowId(0), single_leaf_window(leaf.clone()));
    let meta = registry_meta(&app, 0);
    assert_eq!(
        (meta.autopilot.as_deref(), meta.land_at),
        (Some("cruise"), Some(ahead))
    );
    assert_eq!(
        app.autopilot_timers.entry(0).map(|e| e.land_at_ms),
        Some(ahead)
    );
    host.shutdown_and_join();
    let (mut app, host) = hosted_app();
    app.handoff_successor = true;
    adopt_as(&mut app, 0, 0);
    driver_meta(&app, 0, MetaField::Autopilot, MetaEdit::Set("off"));
    app.restore_into_window(WindowId(0), single_leaf_window(leaf));
    let meta = registry_meta(&app, 0);
    assert_eq!(
        (meta.autopilot.as_deref(), meta.land_at),
        (Some("off"), None)
    );
    assert!(app.autopilot_timers.is_empty(), "no entry");
    host.shutdown_and_join();

    let (mut app, host) = hosted_app();
    let mut odd = timed(ahead);
    odd.autopilot = Some("land".to_string());
    odd.local_id = Some(0);
    app.restore_into_window(WindowId(0), single_leaf_window(odd));
    let meta = registry_meta(&app, 0);
    assert_eq!(
        (meta.autopilot.as_deref(), meta.land_at),
        (Some("land"), None)
    );
    assert!(app.autopilot_timers.is_empty());
    host.shutdown_and_join();
    crate::autopilot_timers::set_fake_clocks(None);
}

#[cfg(unix)]
#[test]
fn the_setter_rides_the_cold_restore_the_reopen_and_the_graft() {
    use crate::session_timeline::{MetaEdit, MetaField};
    let wall = 1_792_177_200_000_i64;
    crate::autopilot_timers::set_fake_clocks(Some((std::time::Instant::now(), wall)));
    let by = |app: &App, session: u64| {
        registry_meta(app, session)
            .autopilot_set_by
            .map(|set_by| set_by.carried())
    };
    let since = wall - 3_600_000;
    let carried = format!("ctl {since}");
    let set = |by: Option<&str>| restore::TerminalLeafRestore {
        autopilot: Some("off".to_string()),
        autopilot_by: by.map(str::to_owned),
        local_id: Some(1),
        ..bare_leaf()
    };

    #[cfg(unix)]
    {
        let mut parent = App::headless_for_test();
        assert!(parent.set_session_autopilot(
            0,
            Some(aterm_agent::supervise::Autopilot::Off),
            crate::autopilot::Surface::Chip
        ));
        let (layout, _) = handoff_layout_placed(&parent);
        let leaf = match &layout.windows[0].restored_tabs[0].root {
            restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } => leaf.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(leaf.autopilot.as_deref(), Some("off"));
        assert_eq!(
            leaf.autopilot_by.as_deref(),
            Some(format!("human:chip {wall}").as_str())
        );
    }

    let (mut app, host) = hosted_app();
    let mut layout = single_leaf_window(bare_leaf());
    layout.restored_tabs.push(restore::RestoredTab {
        root: restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(set(Some(
            &carried,
        )))),
        focused_path: Vec::new(),
        zoomed: false,
    });
    app.restore_into_window(WindowId(0), layout);
    let session = app
        .pool
        .iter()
        .find_map(|s| (s.id != 0).then_some(s.id))
        .expect("the second tab's shell");
    assert_eq!(by(&app, session).as_deref(), Some(carried.as_str()));
    assert_eq!(registry_meta(&app, session).autopilot_revision, 0);
    let sid = app
        .pool
        .get(session)
        .unwrap()
        .ctx
        .self_id
        .as_str()
        .to_string();
    let recorded = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| r.key.as_deref() == Some(format!("harness.autopilot.{sid}").as_str()))
            .count()
    };
    app.on_meta_changed(session);
    assert_eq!(
        recorded(&app),
        0,
        "the old process's write, not recorded here"
    );
    host.shutdown_and_join();

    for (leaf_by, want, word) in [
        (Some(carried.as_str()), Some(carried.as_str()), "ctl"),
        (None, None, "restored"),
    ] {
        let (mut app, host) = hosted_app();
        let record = restore::RestoredTab {
            root: restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(set(leaf_by))),
            focused_path: Vec::new(),
            zoomed: false,
        };
        app.restore_closed_tab_into_window(WindowId(0), &record, 1)
            .expect("reopened");
        let session = app
            .pool
            .iter()
            .find_map(|s| (s.id != 0).then_some(s.id))
            .expect("the reopened shell");
        assert_eq!(by(&app, session).as_deref(), want);
        assert_eq!(
            crate::control::autopilot_by_fields(&registry_meta(&app, session)).0,
            word
        );
        host.shutdown_and_join();
    }

    let (mut app, host) = hosted_app();
    app.handoff_successor = true;
    adopt_as(&mut app, 0, 0);
    let mut graft = set(Some(&carried));
    graft.local_id = Some(0);
    app.restore_into_window(WindowId(0), single_leaf_window(graft.clone()));
    assert_eq!(by(&app, 0).as_deref(), Some(carried.as_str()));
    assert_eq!(registry_meta(&app, 0).autopilot_revision, 0);
    host.shutdown_and_join();
    let (mut app, host) = hosted_app();
    app.handoff_successor = true;
    adopt_as(&mut app, 0, 0);
    crate::autopilot_timers::set_fake_clocks(Some((std::time::Instant::now(), wall + 1_000)));
    driver_meta(&app, 0, MetaField::Autopilot, MetaEdit::Set("land"));
    app.restore_into_window(WindowId(0), single_leaf_window(graft));
    let meta = registry_meta(&app, 0);
    assert_eq!(meta.autopilot.as_deref(), Some("land"), "the driver's word");
    assert_eq!(
        meta.autopilot_set_by.map(|set_by| set_by.carried()),
        Some(format!("ctl {}", wall + 1_000)),
        "and the driver's setter, not the carried one"
    );
    host.shutdown_and_join();
    crate::autopilot_timers::set_fake_clocks(None);
}

#[test]
fn a_setter_beside_a_word_this_build_cannot_read_is_not_stored() {
    let since = 1_792_177_200_000_i64;
    let leaf = |word: &str| restore::TerminalLeafRestore {
        autopilot: Some(word.to_string()),
        autopilot_by: Some(format!("ctl {since}")),
        local_id: Some(1),
        ..bare_leaf()
    };
    let fields = |app: &App, session: u64| {
        let meta = registry_meta(app, session);
        (
            meta.autopilot.clone(),
            meta.autopilot_set_by.map(|set_by| set_by.carried()),
        )
    };
    for (word, want) in [
        ("fly", (None, None)),
        (
            "land",
            (Some("land".to_string()), Some(format!("ctl {since}"))),
        ),
    ] {
        let (mut app, host) = hosted_app();
        let mut layout = single_leaf_window(bare_leaf());
        layout.restored_tabs.push(restore::RestoredTab {
            root: restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(leaf(word))),
            focused_path: Vec::new(),
            zoomed: false,
        });
        app.restore_into_window(WindowId(0), layout);
        let session = app
            .pool
            .iter()
            .find_map(|s| (s.id != 0).then_some(s.id))
            .expect("the second tab's shell");
        assert_eq!(fields(&app, session), want, "cold, {word}");
        host.shutdown_and_join();

        let (mut app, host) = hosted_app();
        app.handoff_successor = true;
        adopt_as(&mut app, 0, 0);
        let mut graft = leaf(word);
        graft.local_id = Some(0);
        app.restore_into_window(WindowId(0), single_leaf_window(graft));
        assert_eq!(fields(&app, 0), want, "graft, {word}");
        host.shutdown_and_join();
    }
}

#[test]
fn a_hand_edited_setter_costs_the_setter_never_the_window() {
    let mut parent = App::headless_for_test();
    assert!(parent.set_session_autopilot(
        0,
        Some(aterm_agent::supervise::Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    let wire = parent
        .capture_restore_manifest()
        .to_toml()
        .expect("serialize");
    let line = wire
        .lines()
        .find(|line| line.trim_start().starts_with("autopilot_by = "))
        .unwrap_or_else(|| panic!("the setter is captured:\n{wire}"))
        .to_string();
    let edited = |by: &str, word: Option<&str>| {
        let mut wire = wire.replace(&line, &format!("autopilot_by = \"{by}\""));
        if word.is_none() {
            wire = wire.replace("autopilot = \"off\"\n", "");
        }
        let manifest = restore::RestoreManifest::from_toml(&wire)
            .unwrap_or_else(|| panic!("{by}: the manifest reads"));
        assert_eq!(manifest.windows.len(), 1, "{by}: the window reopens");
        let leaf = match &manifest.windows[0].restored_tabs[0].root {
            restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } => leaf.clone(),
            other => panic!("{other:?}"),
        };
        (leaf.autopilot, leaf.autopilot_by)
    };
    assert_eq!(
        edited("ctl 0005", Some("off")),
        (Some("off".to_string()), Some("ctl 5".to_string()))
    );
    assert_eq!(
        edited("human:pad 1", Some("off")),
        (Some("off".to_string()), None)
    );
    assert_eq!(edited("ctl 5", None), (None, None));
}

#[cfg(unix)]
#[test]
fn a_timed_cruise_crosses_the_handoff_and_lands_once_after_the_commit() {
    use aterm_agent::supervise::Autopilot;
    let wall = 1_792_177_200_000_i64;
    let mono = std::time::Instant::now();
    let at = |secs: u64| {
        crate::autopilot_timers::set_fake_clocks(Some((
            mono + std::time::Duration::from_secs(secs),
            wall + i64::try_from(secs).unwrap() * 1000,
        )));
    };
    at(0);
    let mut parent = App::headless_for_test();
    assert!(parent.set_session_autopilot_timed(
        0,
        Some(Autopilot::Cruise),
        Some(wall + 240_000),
        crate::autopilot::Surface::Chip
    ));
    let (layout, placed) = handoff_layout_placed(&parent);
    let carried = match &layout.windows[0].restored_tabs[0].root {
        restore::RestoredSplitTree::Leaf {
            view: restore::RestoredView::Terminal(leaf),
        } => leaf.land_at,
        _ => None,
    };
    assert_eq!(
        carried,
        Some(wall + 240_000),
        "the capture carries the pair"
    );
    let mut new = successor_of(placed.windows, 1, |handed_off, layout| {
        super::take_session0_shell(handed_off, Some(layout))
    });
    new.incoming_handoff_pending = true;
    let pair = |app: &App| {
        let meta = registry_meta(app, 0);
        (meta.autopilot, meta.land_at)
    };
    assert_eq!(
        pair(&new),
        (Some("cruise".into()), Some(wall + 240_000)),
        "grafted"
    );
    assert_eq!(
        new.autopilot_timers.entry(0).map(|e| e.land_at_ms),
        Some(wall + 240_000),
        "indexed by the graft"
    );
    let (term, ctx) = {
        let session = new.pool.get(0).unwrap();
        (session.term.clone(), session.ctx.clone())
    };
    let (reply, changed) =
        crate::control::cmd_meta(&term, &new.store, 0, &ctx, "set autopilot cruise until=+2m");
    assert!(reply.starts_with("OK land_at="), "{reply}");
    assert!(changed);
    assert!(
        new.admit_wake_before_commit(crate::Wake::MetaChanged { session: 0 })
            .is_none(),
        "the wake is dropped before the Commit"
    );
    assert_eq!(
        new.autopilot_timers.entry(0).map(|e| e.land_at_ms),
        Some(wall + 240_000),
        "the index trails the store"
    );
    at(120);
    new.expire_autopilot_timers();
    assert_eq!(new.autopilot_timer_deadline(), None, "nothing folded");
    assert_eq!(pair(&new), (Some("cruise".into()), Some(wall + 120_000)));
    new.incoming_handoff_pending = false;
    new.expire_autopilot_timers();
    assert_eq!(
        pair(&new),
        (Some("land".into()), None),
        "landed at the Commit's park"
    );
    assert_eq!(new.autopilot_timer_deadline(), None, "no entry left");
    at(240);
    new.expire_autopilot_timers();
    let lands = ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "meta-change" && e.payload.contains("field=autopilot value=land"))
        .count();
    assert_eq!(lands, 1, "landed once");
    crate::autopilot_timers::set_fake_clocks(None);
}

fn slog_lines(app: &App, session: u64) -> Vec<String> {
    let ctx = app.pool.get(session).expect("live session").ctx.clone();
    crate::control::cmd_log(&ctx, "")
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
}

fn lineage_tab(local: u64, sid: Option<&str>, lineage: Option<&str>) -> restore::RestoredTab {
    restore::RestoredTab {
        root: restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
            restore::TerminalLeafRestore {
                sid: sid.map(str::to_string),
                lineage: lineage.map(str::to_string),
                local_id: Some(local),
                ..bare_leaf()
            },
        )),
        focused_path: Vec::new(),
        zoomed: false,
    }
}

fn captured_leaf(app: &App, wid: WindowId, index: usize) -> (restore::TerminalLeafRestore, u64) {
    let tab = &app.windows[&wid].tab_set.tabs()[index];
    let session = app
        .view_store
        .get(tab.focus)
        .copied()
        .and_then(crate::tab_model::View::terminal_session)
        .expect("a terminal tab");
    let restore::RestoredSplitTree::Leaf {
        view: restore::RestoredView::Terminal(leaf),
    } = app.tab_restore_descriptor(tab).expect("captured").root
    else {
        panic!("one terminal leaf");
    };
    (leaf, session)
}

#[test]
fn a_cold_restore_opens_with_prev_and_lineage() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.restore_into_window(
        wid,
        restore::WindowLayout {
            restored_tabs: vec![
                lineage_tab(0, Some("s-a0"), None),
                lineage_tab(1, Some("s-b1"), Some("s-f0")),
                lineage_tab(2, None, None),
            ],
            ..single_leaf_window(bare_leaf())
        },
    );
    let (boot_leaf, boot) = captured_leaf(&app, wid, 0);
    let (new_leaf, new) = captured_leaf(&app, wid, 1);
    let (bare, older) = captured_leaf(&app, wid, 2);
    assert_eq!(boot, 0, "PRECONDITION: the bootstrap filled the first leaf");
    let amend = slog_lines(&app, boot);
    assert!(
        amend.iter().any(|l| l.contains(" amend - restore ")
            && l.contains(" prev=s-a0 ")
            && l.contains(" lineage=s-a0 ")),
        "{amend:?}"
    );
    let opened = slog_lines(&app, new);
    assert!(
        opened[0].contains(" open - restore ")
            && opened[0].contains(" prev=s-b1 ")
            && opened[0].contains(" lineage=s-f0 "),
        "{opened:?}"
    );
    let plain = slog_lines(&app, older);
    assert!(
        plain[0].contains(" open - restore ")
            && !plain[0].contains("prev=")
            && !plain[0].contains("lineage="),
        "NEGATIVE CONTROL: {plain:?}"
    );
    let sid_of = |s: u64| app.pool.get(s).unwrap().ctx.self_id.as_str().to_string();
    assert_eq!(boot_leaf.sid, Some(sid_of(boot)));
    assert_eq!(boot_leaf.lineage.as_deref(), Some("s-a0"));
    assert_eq!(new_leaf.sid, Some(sid_of(new)));
    assert_eq!(new_leaf.lineage.as_deref(), Some("s-f0"));
    assert_eq!(bare.sid, Some(sid_of(older)));
    assert_eq!(bare.lineage, None, "it began its chain");

    let mut again = App::headless_for_test();
    again.restore_into_window(
        wid,
        restore::WindowLayout {
            restored_tabs: vec![
                lineage_tab(0, None, None),
                restore::RestoredTab {
                    root: restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
                        new_leaf.clone(),
                    )),
                    focused_path: Vec::new(),
                    zoomed: false,
                },
            ],
            ..single_leaf_window(bare_leaf())
        },
    );
    let (_, second) = captured_leaf(&again, wid, 1);
    let opened = slog_lines(&again, second);
    assert!(
        opened[0].contains(&format!(" prev={} ", sid_of(new)))
            && opened[0].contains(" lineage=s-f0 "),
        "{opened:?}"
    );
}

#[test]
fn a_seamless_update_keeps_a_sessions_lineage_for_its_next_restore() {
    let old = App::headless_for_test();
    {
        let ctx = old.pool.get(0).expect("session 0").ctx.clone();
        let mut tl = ctx.timeline.lock().unwrap();
        let at = tl.slog().now();
        tl.slog_mut()
            .note_restored(Some("s-0abc"), Some("s-0001"), at);
    }
    let captured = old.capture_restore_manifest();
    let wire = captured
        .to_toml()
        .expect("the parent serializes its layout");
    assert!(wire.contains("lineage = \"s-0001\""), "{wire}");
    let carried = restore::RestoreManifest::from_toml(&wire).expect("the child reads it");
    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    adopt_as(&mut new, 0, 0);
    let lineage = |app: &App| {
        app.pool
            .get(0)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .lineage()
            .map(|l| l.as_str().to_string())
    };
    assert_eq!(lineage(&new), None, "PRECONDITION: none on arrival");
    let before = slog_lines(&new, 0).len();
    new.restore_into_window(
        WindowId(0),
        carried.windows.into_iter().next().expect("window 0"),
    );
    assert_eq!(new.focused_session_id(WindowId(0)), Some(0));
    assert_eq!(lineage(&new).as_deref(), Some("s-0001"));
    let after = slog_lines(&new, 0);
    assert_eq!(
        after.len(),
        before,
        "the same session: no record: {after:?}"
    );
    let (leaf, _) = captured_leaf(&new, WindowId(0), 0);
    assert_eq!(leaf.lineage.as_deref(), Some("s-0001"));
}

#[test]
fn a_reopened_closed_tab_names_the_session_it_was() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    let closing = app.next_session_id - 1;
    let sid = app.pool.get(closing).unwrap().ctx.self_id.clone();
    assert!(!app.close_tab_at(wid, 1));
    assert!(app.pool.get(closing).is_none(), "PRECONDITION: it closed");
    app.reopen_last_closed_tab().unwrap();
    let reopened = app.focused_session_id(wid).expect("the reopened tab");
    assert_ne!(reopened, closing);
    let opened = slog_lines(&app, reopened);
    assert!(
        opened[0].contains(" open - restore ")
            && opened[0].contains(&format!(" prev={} ", sid.as_str()))
            && opened[0].contains(&format!(" lineage={} ", sid.as_str())),
        "{opened:?}"
    );
    let closed = app
        .store
        .read()
        .unwrap()
        .closed_logs()
        .by_sid(&sid)
        .map(|e| e.source());
    let closed = closed
        .and_then(crate::session_log::ClosedSource::into_log)
        .expect("the session it was answers from the closed index");
    assert_eq!(
        closed.window().last().map(|r| r.op),
        Some(aterm_slog::Op::Close)
    );
}

#[test]
fn a_session_theme_rides_the_layout_and_comes_back_restored() {
    use crate::session_theme::{SessionTheme, SessionThemeOrigin};
    let mut old = App::headless_for_test();
    old.set_session_theme(
        0,
        Some(SessionTheme {
            selector: "Hearth".to_string(),
            origin: SessionThemeOrigin::Ctl,
        }),
    );
    let captured = old.capture_restore_manifest();
    let wire = captured.to_toml().expect("the layout serializes");
    assert!(wire.contains("theme = \"Hearth\""), "{wire}");
    let carried = restore::RestoreManifest::from_toml(&wire).expect("the layout parses");
    assert_eq!(carried, captured, "the wire drops nothing");
    let older = restore::RestoreManifest::from_toml(&wire.replace("theme = \"Hearth\"\n", ""))
        .expect("an older layout parses");
    let no_theme =
        |manifest: &restore::RestoreManifest| match &manifest.windows[0].restored_tabs[0].root {
            restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } => leaf.theme.is_none(),
            other => panic!("one terminal leaf: {other:?}"),
        };
    assert!(no_theme(&older));
    assert!(!no_theme(&carried));

    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    adopt_as(&mut new, 0, 0);
    let window = carried.windows.into_iter().next().expect("window 0");
    new.restore_into_window(WindowId(0), window);
    assert_eq!(new.focused_session_id(WindowId(0)), Some(0));
    assert_eq!(
        new.pool.theme(0),
        Some(&SessionTheme {
            selector: "Hearth".to_string(),
            origin: SessionThemeOrigin::Restored,
        })
    );
    assert_eq!(
        crate::term_lock(&new.pool.get(0).expect("adopted").term).default_background(),
        aterm_types::scheme::builtin("Hearth")
            .expect("built-in")
            .background
    );
}

#[test]
fn a_word_about_typing_reaches_windows_end_session_snapshot_at_once() {
    use aterm_agent::supervise::Autopilot;

    use crate::fabric::{Hold, apply_hold_for_test};
    use crate::session_timeline::{MetaField, apply_meta_value};

    let mut app = App::headless_for_test();
    assert!(
        !app.crash_journal.is_active(),
        "the lane is inert, as on Windows"
    );
    let ctx = std::sync::Arc::clone(&app.pool.get(0).expect("the window's shell").ctx);
    let sid = ctx.self_id.clone();
    let words = |snapshot: Option<Option<restore::RestoreManifest>>| {
        let manifest = snapshot.expect("due").expect("restore_session on");
        let leaf = manifest
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .expect("the tab's leaf");
        (leaf.autopilot, leaf.holds)
    };
    let ahead = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    let mut at = ahead;
    let mut inside = || {
        at += std::time::Duration::from_millis(100);
        at
    };
    assert!(app.end_session_snapshot(ahead).is_some(), "the first");
    assert!(
        app.end_session_snapshot(inside()).is_none(),
        "nothing moved"
    );

    assert!(apply_meta_value(
        &ctx,
        MetaField::Title,
        Some("renamed".into()),
        aterm_slog::Actor::Ctl
    ));
    app.on_meta_changed(0);
    assert!(
        app.end_session_snapshot(inside()).is_none(),
        "a title waits"
    );

    assert!(apply_meta_value(
        &ctx,
        MetaField::Autopilot,
        Some("land".into()),
        aterm_slog::Actor::Ctl
    ));
    app.on_meta_changed(0);
    assert_eq!(
        words(app.end_session_snapshot(inside())),
        (Some("land".to_string()), Vec::new()),
        "Land: at once"
    );
    assert!(app.end_session_snapshot(inside()).is_none(), "carried now");

    assert!(apply_hold_for_test(
        &ctx,
        Some(Hold {
            reason: "review".to_string(),
            origin: "local".to_string(),
        })
    ));
    app.on_fabric_changed(&sid);
    let held = vec!["local review".to_string()];
    assert_eq!(
        words(app.end_session_snapshot(inside())),
        (Some("land".to_string()), held.clone()),
        "the hold: at once"
    );

    assert!(app.set_session_autopilot_timed(
        0,
        Some(Autopilot::Cruise),
        Some(1_000),
        crate::autopilot::Surface::Chip
    ));
    assert_eq!(
        words(app.end_session_snapshot(inside())).0.as_deref(),
        Some("cruise"),
        "the timed Cruise: at once"
    );
    app.expire_autopilot_timers_at(ahead, 2_000);
    assert_eq!(
        words(app.end_session_snapshot(inside())).0.as_deref(),
        Some("land"),
        "the timer's Land: at once"
    );

    assert!(app.set_session_autopilot(0, Some(Autopilot::Off), crate::autopilot::Surface::Chip));
    assert_eq!(
        words(app.end_session_snapshot(inside())),
        (Some("off".to_string()), held),
        "Off: at once"
    );

    let later = inside() + crate::crash_journal::END_SESSION_SNAPSHOT_TTL;
    assert!(
        app.end_session_snapshot(later).is_some(),
        "past the interval"
    );
    app.config.restore_session = Some(false);
    let off = later + crate::crash_journal::END_SESSION_SNAPSHOT_TTL;
    assert_eq!(
        app.end_session_snapshot(off),
        Some(None),
        "restore_session off: retracted"
    );
}

#[cfg(unix)]
#[test]
fn a_word_about_typing_inside_the_write_interval_is_journaled_at_once_and_reopened() {
    use aterm_agent::supervise::Autopilot;

    use crate::fabric::{Hold, apply_hold_for_test};
    use crate::session_timeline::{MetaField, apply_meta_value};

    let mut app = App::headless_for_test();
    let ctx = std::sync::Arc::clone(&app.pool.get(0).expect("the window's shell").ctx);
    let sid = ctx.self_id.clone();
    let journal_dir = crate::crash_journal::tests::scratch("cj-word");
    app.crash_journal = crate::crash_journal::Lane::new(
        Some(journal_dir.path().to_path_buf()),
        crate::crash_journal::tests::reaped_pid(),
    );
    app.first_present_done = true;
    let journal = app.crash_journal.journal_id();
    let ahead = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    app.tick_crash_journal(ahead);
    assert_eq!(
        (app.crash_journal.captures, app.crash_journal.writes),
        (1, 1),
        "the first image"
    );
    let woken = |app: &mut App| app.crash_journal.note_activity();
    let park = |app: &mut App| app.tick_crash_journal(std::time::Instant::now());

    assert!(apply_meta_value(
        &ctx,
        MetaField::Title,
        Some("renamed".into()),
        aterm_slog::Actor::Ctl
    ));
    woken(&mut app);
    app.on_meta_changed(0);
    park(&mut app);
    assert_eq!(
        app.crash_journal.captures, 1,
        "a title waits for the write interval"
    );

    assert!(apply_meta_value(
        &ctx,
        MetaField::Autopilot,
        Some("land".into()),
        aterm_slog::Actor::Ctl
    ));
    woken(&mut app);
    app.on_meta_changed(0);
    park(&mut app);
    assert_eq!(app.crash_journal.writes, 2, "Land: handed over at once");

    assert!(apply_hold_for_test(
        &ctx,
        Some(Hold {
            reason: "review".to_string(),
            origin: "local".to_string(),
        })
    ));
    woken(&mut app);
    app.on_fabric_changed(&sid);
    park(&mut app);
    assert_eq!(app.crash_journal.writes, 3, "the hold: handed over at once");

    assert!(app.set_session_autopilot_timed(
        0,
        Some(Autopilot::Cruise),
        Some(1_000),
        crate::autopilot::Surface::Chip
    ));
    park(&mut app);
    assert_eq!(app.crash_journal.writes, 4, "the timed Cruise: at once");
    park(&mut app);
    app.expire_autopilot_timers_at(ahead, 2_000);
    assert_eq!(
        ctx.meta.lock().unwrap().autopilot.as_deref(),
        Some("land"),
        "the timer landed it"
    );
    assert_eq!(app.crash_journal.writes, 4, "nothing captured by the fire");
    app.crash_journal_wake(std::time::Instant::now());
    assert_eq!(app.crash_journal.writes, 5, "the timer's Land: at once");

    assert!(app.set_session_autopilot(0, Some(Autopilot::Off), crate::autopilot::Surface::Chip));
    park(&mut app);
    assert_eq!(app.crash_journal.writes, 6, "Off: handed over at once");

    let leaf_of = |manifest: &restore::RestoreManifest| {
        manifest
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
    };
    let image = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the journal's image naming Off and the hold",
        || {
            std::fs::read_to_string(journal_dir.path().join(journal.file_name()))
                .ok()
                .and_then(|text| crate::crash_journal::decode(&text).ok())
                .map(|(_, image)| image)
                .filter(|image| {
                    leaf_of(image).is_some_and(|leaf| {
                        leaf.autopilot.as_deref() == Some("off")
                            && leaf.holds == vec!["local review".to_string()]
                    })
                })
        },
    );
    app.crash_journal = crate::crash_journal::Lane::inert();
    crate::crash_journal::tests::await_released(&journal_dir.path().join(journal.lock_name()));

    let window = image
        .windows
        .into_iter()
        .find(|window| {
            window.restored_tabs.iter().any(|tab| {
                matches!(
                    &tab.root,
                    restore::RestoredSplitTree::Leaf {
                        view: restore::RestoredView::Terminal(leaf),
                    } if leaf.local_id == Some(0)
                )
            })
        })
        .expect("the tab's window");
    let mut cold = App::headless_for_test();
    cold.restore_into_window(WindowId(0), window);
    assert_eq!(
        registry_meta(&cold, 0).autopilot.as_deref(),
        Some("off"),
        "the reopened tab is Off"
    );
    assert!(
        cold.pool
            .get(0)
            .expect("the reopened tab's shell")
            .ctx
            .fabric
            .holds()
            .has("local"),
        "and held"
    );
}

#[cfg(unix)]
#[test]
fn a_holds_ok_comes_once_the_journal_image_carrying_it_is_on_disk() {
    let mut app = App::headless_for_test();
    let sid = app
        .pool
        .get(0)
        .expect("the window's shell")
        .ctx
        .self_id
        .clone();
    let journal_dir = crate::crash_journal::tests::scratch("cj-ack");
    app.crash_journal = crate::crash_journal::Lane::new(
        Some(journal_dir.path().to_path_buf()),
        crate::crash_journal::tests::reaped_pid(),
    );
    app.arm_word_durability(|| {});
    app.first_present_done = true;
    let journal = app.crash_journal.journal_id();
    let ahead = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    app.tick_crash_journal(ahead);
    let durable = app.crash_journal.durable();

    let store = std::sync::Arc::clone(&app.store);
    let line = format!("hold {sid} on");
    let verb = std::thread::spawn(move || {
        crate::control::hold_line_for_test(&store, &line, crate::control::Scope::Owner)
    });
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the verb's ticket, or its answer",
        || {
            (durable.covered() > 0 || verb.is_finished() || app.crash_journal.capture_due(ahead))
                .then_some(())
        },
    );
    assert!(
        !verb.is_finished(),
        "the OK waits for the journal: nothing has captured the hold yet"
    );
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the verb's answer", || {
        app.tick_crash_journal(std::time::Instant::now());
        verb.is_finished().then_some(())
    });
    assert_eq!(verb.join().expect("the verb's thread"), "OK hold=1\n");
    let text =
        std::fs::read_to_string(journal_dir.path().join(journal.file_name())).expect("the journal");
    let (_, image) = crate::crash_journal::decode(&text).expect("the image decodes");
    let holds = image
        .handed_off_leaves()
        .into_iter()
        .find(|leaf| leaf.local_id == Some(0))
        .map(|leaf| leaf.holds)
        .unwrap_or_default();
    assert_eq!(holds, vec!["local -".to_string()], "the hold is on disk");

    app.config.restore_session = Some(false);
    app.tick_crash_journal(std::time::Instant::now());
    assert_eq!(
        crate::control::hold_line_for_test(
            &app.store,
            &format!("hold {sid} off"),
            crate::control::Scope::Owner
        ),
        "OK hold=0\n",
        "restore_session off: at once"
    );
    app.crash_journal = crate::crash_journal::Lane::inert();
    app.arm_word_durability(|| {});
    assert_eq!(
        crate::control::hold_line_for_test(
            &app.store,
            &format!("hold {sid} on"),
            crate::control::Scope::Owner
        ),
        "OK hold=1\n",
        "an inert lane: at once"
    );
    crate::crash_journal::tests::await_released(&journal_dir.path().join(journal.lock_name()));
}

#[cfg(unix)]
#[test]
fn a_fleet_hold_is_answered_at_once_where_the_owners_waits_on_the_journal() {
    use crate::control::{Scope, hold_line_for_test};
    let mut app = App::headless_for_test();
    let sid = app
        .pool
        .get(0)
        .expect("the window's shell")
        .ctx
        .self_id
        .clone();
    let journal_dir = crate::crash_journal::tests::scratch("cj-fleet");
    app.crash_journal = crate::crash_journal::Lane::new(
        Some(journal_dir.path().to_path_buf()),
        crate::crash_journal::tests::reaped_pid(),
    );
    app.arm_word_durability(|| {});
    let journal = app.crash_journal.journal_id();
    app.first_present_done = false;
    app.tick_crash_journal(std::time::Instant::now());
    app.crash_journal
        .durable()
        .set_word_bound(std::time::Duration::from_millis(1));
    let hold = |line: String, scope| hold_line_for_test(&app.store, &line, scope);

    assert_eq!(
        hold(
            format!("hold {sid} on reason=stop origin=fleet"),
            Scope::Bridge
        ),
        "OK hold=1\n",
        "the fleet's halt: at once"
    );
    assert_eq!(
        hold(format!("hold {sid} off origin=fleet"), Scope::Bridge),
        "OK hold=0\n",
        "its lift: at once"
    );
    assert_eq!(
        hold(format!("hold {sid} off"), Scope::Bridge),
        "OK hold=0\n",
        "an attach's lift that changes nothing: at once"
    );
    assert_eq!(
        hold(format!("hold {sid} on"), Scope::Owner),
        "OK hold=1 journaled=0\n",
        "the owner's hold waits"
    );
    assert_eq!(
        hold(format!("hold {sid} on"), Scope::Owner),
        "OK hold=1 journaled=0\n",
        "its retry changes nothing and still waits: no image carries it"
    );
    assert_eq!(
        hold(format!("hold {sid} off origin=local"), Scope::Bridge),
        "OK hold=0 journaled=0\n",
        "a bridge's act on the owner's slot waits"
    );
    app.crash_journal = crate::crash_journal::Lane::inert();
    crate::crash_journal::tests::await_released(&journal_dir.path().join(journal.lock_name()));
}

#[cfg(unix)]
#[test]
fn a_hold_lifted_before_a_successors_first_image_comes_back_held_and_says_so() {
    use crate::control::{Scope, hold_line_for_test};
    use crate::crash_journal::{self, JournalHeader, JournalId, JournalOwner, Lane, tests as cj};
    use crate::fabric::{Hold, apply_hold_for_test};
    let reopened_held = |own_image_first: bool| -> bool {
        let (dir, logs) = (cj::scratch("cj-lift"), cj::scratch("cj-lift-logs"));
        let parent = JournalId::now(cj::reaped_pid());
        let owner = JournalOwner::create(dir.path(), parent).expect("armed");
        let held = restore::TerminalLeafRestore {
            local_id: Some(0),
            cwd: Some("/a".to_string()),
            holds: vec!["local pause".to_string()],
            ..bare_leaf()
        };
        owner
            .publish(
                &restore::RestoreManifest::new(vec![window_of(vec![
                    restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(held)),
                ])]),
                &JournalHeader::now(parent.pid, false),
            )
            .expect("published");
        cj::die(owner);
        let mut app = App::headless_for_test();
        adopt_as(&mut app, 0, 0);
        let ctx = std::sync::Arc::clone(&app.pool.get(0).expect("session 0").ctx);
        assert!(apply_hold_for_test(
            &ctx,
            Some(Hold {
                reason: "pause".to_string(),
                origin: "local".to_string(),
            })
        ));
        let sid = ctx.self_id.clone();
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), cj::reaped_pid());
        lane.set_predecessor(parent.pid, Some(parent.nanos));
        let own = lane.journal_id();
        cj::marker(logs.path(), own);
        assert_eq!(lane.adopt_predecessor(true), Some(parent));
        app.crash_journal = lane;
        app.arm_word_durability(|| {});
        let on_disk = || {
            let text =
                std::fs::read_to_string(dir.path().join(own.file_name())).expect("the journal");
            let (header, image) = crash_journal::decode(&text).expect("the image decodes");
            (header.pid == own.pid, image)
        };
        assert!(!on_disk().0, "the parent's image, taken over");
        app.first_present_done = own_image_first;
        app.tick_crash_journal(std::time::Instant::now());
        if own_image_first {
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the successor's own image on disk",
                || {
                    app.tick_crash_journal(std::time::Instant::now());
                    on_disk().0.then_some(())
                },
            );
        }
        let bound = if own_image_first {
            aterm_spec::wait::MUST_HAPPEN
        } else {
            std::time::Duration::from_millis(1)
        };
        app.crash_journal.durable().set_word_bound(bound);
        let store = std::sync::Arc::clone(&app.store);
        let line = format!("hold {sid} off");
        let verb = std::thread::spawn(move || hold_line_for_test(&store, &line, Scope::Owner));
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the lift's answer", || {
            app.tick_crash_journal(std::time::Instant::now());
            verb.is_finished().then_some(())
        });
        let answer = verb.join().expect("the verb's thread");
        let leaf_holds = on_disk()
            .1
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .map(|leaf| leaf.holds)
            .unwrap_or_default();
        if own_image_first {
            assert_eq!(answer, "OK hold=0\n", "durable: the successor's image");
            assert!(leaf_holds.is_empty(), "{leaf_holds:?}");
        } else {
            assert_eq!(
                answer, "OK hold=0 journaled=0\n",
                "the lift says no image carries it"
            );
            assert_eq!(
                leaf_holds,
                vec!["local pause".to_string()],
                "the disk still holds"
            );
        }
        app.crash_journal = Lane::inert();
        cj::await_released(&dir.path().join(own.lock_name()));
        let claim = crash_journal::claim_at_boot(
            dir.path(),
            &crash_journal::Evidence {
                log_dir: Some(logs.path()),
                swept: &[],
            },
            std::process::id(),
            crash_journal::QuitLayout::default(),
        );
        let reopened = claim.reopened.expect("the desk comes back");
        let window = reopened
            .manifest
            .windows
            .into_iter()
            .next()
            .expect("the window");
        let mut cold = App::headless_for_test();
        cold.restore_into_window(WindowId(0), window);
        assert!(
            cold.take_restored_agents().is_empty(),
            "no agent on the leaf"
        );
        cold.pool
            .get(0)
            .expect("the reopened tab's shell")
            .ctx
            .fabric
            .holds()
            .has("local")
    };
    assert!(
        reopened_held(false),
        "lifted before the successor's first image: held again"
    );
    assert!(
        !reopened_held(true),
        "NEGATIVE CONTROL: lifted after it, the lift stands"
    );
}

#[cfg(unix)]
#[test]
fn a_meta_autopilot_words_ok_comes_once_the_journal_image_carrying_it_is_on_disk() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut app = App::headless_for_test();
    let (term, ctx) = {
        let shell = app.pool.get(0).expect("the window's shell");
        (Arc::clone(&shell.term), Arc::clone(&shell.ctx))
    };
    let journal_dir = crate::crash_journal::tests::scratch("cj-meta-ack");
    app.crash_journal = crate::crash_journal::Lane::new(
        Some(journal_dir.path().to_path_buf()),
        crate::crash_journal::tests::reaped_pid(),
    );
    let rings = Arc::new(AtomicUsize::new(0));
    {
        let rings = Arc::clone(&rings);
        app.arm_word_durability(move || {
            rings.fetch_add(1, Ordering::SeqCst);
        });
    }
    app.first_present_done = true;
    let journal = app.crash_journal.journal_id();
    let ahead = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    app.tick_crash_journal(ahead);
    let on_disk = || {
        let text = std::fs::read_to_string(journal_dir.path().join(journal.file_name()))
            .expect("the journal");
        let (_, image) = crate::crash_journal::decode(&text).expect("the image decodes");
        image
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .and_then(|leaf| leaf.autopilot)
    };
    let store = Arc::clone(&app.store);
    let meta = |rest: &'static str| {
        let (store, term, ctx) = (Arc::clone(&store), Arc::clone(&term), Arc::clone(&ctx));
        std::thread::spawn(move || crate::control::meta_line_for_test(&store, &term, 0, &ctx, rest))
    };

    for (n, rest, word) in [
        (1, "set autopilot land", Some("land".to_string())),
        (2, "unset autopilot", None),
    ] {
        let verb = meta(rest);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the verb's ticket, or its answer",
            || (rings.load(Ordering::SeqCst) >= n || verb.is_finished()).then_some(()),
        );
        assert!(
            !verb.is_finished(),
            "{rest}: the OK waits for the journal, and no loop turn has run"
        );
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the verb's answer", || {
            app.tick_crash_journal(std::time::Instant::now());
            verb.is_finished().then_some(())
        });
        assert_eq!(verb.join().expect("the verb's thread"), "OK\n", "{rest}");
        assert_eq!(on_disk(), word, "{rest}: the word is on disk at the answer");
    }

    let before = rings.load(Ordering::SeqCst);
    assert_eq!(
        crate::control::meta_line_for_test(&app.store, &term, 0, &ctx, "set title x"),
        "OK\n",
        "another field: at once"
    );
    assert_eq!(rings.load(Ordering::SeqCst), before, "and no ticket");
    app.config.restore_session = Some(false);
    app.tick_crash_journal(std::time::Instant::now());
    assert_eq!(
        crate::control::meta_line_for_test(&app.store, &term, 0, &ctx, "set autopilot off"),
        "OK\n",
        "restore_session off: at once"
    );
    app.crash_journal = crate::crash_journal::Lane::inert();
    crate::crash_journal::tests::await_released(&journal_dir.path().join(journal.lock_name()));
}

#[cfg(unix)]
#[test]
fn words_about_typing_on_one_loop_turn_cost_one_capture_naming_the_last() {
    use crate::session_timeline::{MetaField, apply_meta_value};

    let mut app = App::headless_for_test();
    let ctx = std::sync::Arc::clone(&app.pool.get(0).expect("the window's shell").ctx);
    let journal_dir = crate::crash_journal::tests::scratch("cj-storm");
    app.crash_journal = crate::crash_journal::Lane::new(
        Some(journal_dir.path().to_path_buf()),
        crate::crash_journal::tests::reaped_pid(),
    );
    app.first_present_done = true;
    let journal = app.crash_journal.journal_id();
    app.tick_crash_journal(std::time::Instant::now() + std::time::Duration::from_secs(3600));
    assert_eq!(
        (app.crash_journal.captures, app.crash_journal.writes),
        (1, 1),
        "the first image"
    );

    let words = ["land", "off"];
    for n in 0..41 {
        assert!(apply_meta_value(
            &ctx,
            MetaField::Autopilot,
            Some(words[n % 2].into()),
            aterm_slog::Actor::Ctl
        ));
        app.crash_journal.note_activity();
        app.on_meta_changed(0);
    }
    assert_eq!(
        app.crash_journal.captures, 1,
        "no capture inside a wake's arm"
    );
    app.tick_crash_journal(std::time::Instant::now());
    assert_eq!(
        (app.crash_journal.captures, app.crash_journal.writes),
        (2, 2),
        "one capture for the turn, handed over at once"
    );

    let image = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the journal's image naming the last word",
        || {
            std::fs::read_to_string(journal_dir.path().join(journal.file_name()))
                .ok()
                .and_then(|text| crate::crash_journal::decode(&text).ok())
                .map(|(_, image)| image)
                .filter(|image| {
                    image.handed_off_leaves().into_iter().any(|leaf| {
                        leaf.local_id == Some(0) && leaf.autopilot.as_deref() == Some("land")
                    })
                })
        },
    );
    assert!(!image.windows.is_empty());
    app.crash_journal = crate::crash_journal::Lane::inert();
    crate::crash_journal::tests::await_released(&journal_dir.path().join(journal.lock_name()));
}

fn restored_desk(n: usize) -> (App, Vec<String>) {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let mut locals = vec![app.focused_session_id(wid).expect("the front session")];
    for _ in 1..n {
        let local = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(local));
        locals.push(local);
    }
    let sids = locals
        .into_iter()
        .map(|local| {
            if app.store.read().unwrap().by_local(local).is_none() {
                app.store
                    .write()
                    .unwrap()
                    .register(crate::session_store::test_handle(local));
            }
            let store = app.store.read().unwrap();
            store.by_local(local).unwrap().sid.as_str().to_string()
        })
        .collect();
    (app, sids)
}

fn restored_row(seq: u64, lines: &[(&str, &str, u64)]) -> crate::harness_host::RestoredRow {
    crate::harness_host::RestoredRow {
        seq,
        lines: lines
            .iter()
            .map(|(sid, place, said)| crate::harness_host::RestoredLine {
                sid: (*sid).to_string(),
                place: (*place).to_string(),
                agent: aterm_agent::harness::upgrade::Agent::Claude,
                outcome: aterm_agent::harness::relaunch::Outcome::Cannot("shell-gone".to_string()),
                said: *said,
            })
            .collect(),
        log: None,
    }
}

fn restored_up(app: &App) -> Vec<(String, Vec<String>)> {
    app.messages
        .live_rows()
        .filter(|l| {
            l.msg
                .key
                .as_deref()
                .is_some_and(|k| k.starts_with(crate::message_reporters::KEY_RESTORED_AGENTS))
                && !l
                    .msg
                    .key
                    .as_deref()
                    .is_some_and(|k| k.starts_with(crate::message_reporters::KEY_RESTORED_HELD))
        })
        .map(|l| (l.msg.title.clone(), l.msg.detail.clone()))
        .collect()
}

#[test]
fn a_restored_row_said_before_a_later_one_is_passed_over() {
    let _lane = crate::message_inbox::lane_test_guard();
    for stale_last in [true, false] {
        let _ = crate::message_inbox::take_queued();
        let (mut app, sids) = restored_desk(2);
        let older = restored_row(10, &[(&sids[0], "in tab 1", 9)]);
        let newer = restored_row(12, &[(&sids[0], "in tab 1", 9), (&sids[1], "in tab 2", 11)]);
        let rows = if stale_last {
            [newer, older]
        } else {
            [older, newer]
        };
        for row in rows {
            crate::message_inbox::queue_restored_row(crate::message_inbox::current_lane(), row);
        }
        app.drain_message_inbox();
        let up = restored_up(&app);
        assert_eq!(up.len(), 1, "stale_last={stale_last}: {up:?}");
        assert_eq!(
            up[0].0, "Couldn't resume Claude in 2 tabs",
            "stale_last={stale_last}"
        );
    }
}

#[test]
fn a_restored_row_the_owner_dismissed_does_not_come_back_inside_the_next() {
    for dismissed in [true, false] {
        let (mut app, sids) = restored_desk(2);
        let stamp = crate::messages_host::wall_stamp_now();
        app.post_restored_row(restored_row(1, &[(&sids[0], "in tab 1", 1)]), stamp);
        let first = app.restored_shown.id.expect("posted");
        if dismissed {
            assert!(app.messages.dismiss(first, std::time::Instant::now()));
        }
        app.post_restored_row(
            restored_row(3, &[(&sids[0], "in tab 1", 1), (&sids[1], "in tab 2", 2)]),
            stamp,
        );
        let titles: Vec<String> = restored_up(&app).into_iter().map(|(t, _)| t).collect();
        let want = if dismissed {
            "Couldn't resume Claude in tab 2"
        } else {
            "Couldn't resume Claude in 2 tabs"
        };
        assert_eq!(titles, [want], "dismissed={dismissed}");
        if dismissed {
            app.post_restored_row(
                restored_row(5, &[(&sids[0], "in tab 1", 4), (&sids[1], "in tab 2", 2)]),
                stamp,
            );
            let titles: Vec<String> = restored_up(&app).into_iter().map(|(t, _)| t).collect();
            assert_eq!(titles, ["Couldn't resume Claude in 2 tabs"]);
        }
    }
}

#[test]
fn a_restored_row_names_each_tab_where_it_is_now() {
    let (mut app, sids) = restored_desk(3);
    let stamp = crate::messages_host::wall_stamp_now();
    app.post_restored_row(
        restored_row(1, &[(&sids[0], "in tab 1", 1), (&sids[1], "in tab 2", 2)]),
        stamp,
    );
    assert_eq!(restored_up(&app)[0].0, "Couldn't resume Claude in 2 tabs");
    assert!(!app.close_tab_at(crate::WindowId(0), 0));
    assert_eq!(app.upgrade_place(&sids[0]), "in its tab", "closed");
    assert_eq!(app.upgrade_place(&sids[1]), "in tab 1", "renumbered");
    let row = restored_row(
        3,
        &[
            (&sids[0], "in tab 1", 1),
            (&sids[1], "in tab 2", 2),
            (&sids[2], "in tab 3", 3),
        ],
    );
    let said = row.message();
    assert_eq!(said.title, "Couldn't resume Claude in 3 tabs");
    app.post_restored_row(row, stamp);
    let up = restored_up(&app);
    assert_eq!(up.len(), 1, "{up:?}");
    assert_eq!(up[0].0, "Couldn't resume Claude in 2 tabs");
    let places: Vec<&str> = up[0]
        .1
        .iter()
        .filter_map(|l| l.split_once(", ").map(|(place, _)| place))
        .filter(|p| p.starts_with("in tab"))
        .collect();
    assert_eq!(places, ["in tab 1", "in tab 2"], "{:?}", up[0].1);
}

#[test]
fn a_successors_restored_row_stands_beside_the_one_carried_to_it() {
    for own_key in [true, false] {
        let mut parent = App::headless_for_test();
        let mut carried = restored_row(1, &[("s-parked", "in tab 4", 1)]).message();
        carried.key = Some(crate::message_reporters::KEY_RESTORED_AGENTS.to_string());
        let _ = parent.post_message(carried.clone());
        let _ = parent.messages.settle(
            std::time::Instant::now() + aterm_messages::PROGRESS_GRACE,
            true,
        );
        let carry = parent.messages.carried();
        let (mut app, sids) = restored_desk(1);
        app.messages.seed_carried(
            &carry,
            crate::messages_host::wall_stamp_now(),
            std::time::Instant::now(),
        );
        let row = restored_row(5, &[(&sids[0], "in tab 1", 4)]);
        if own_key {
            app.post_restored_row(row, crate::messages_host::wall_stamp_now());
        } else {
            let mut msg = row.message();
            msg.key.clone_from(&carried.key);
            let _ = app.post_message(msg);
        }
        let mut titles: Vec<String> = restored_up(&app).into_iter().map(|(t, _)| t).collect();
        titles.sort();
        if own_key {
            assert_eq!(
                titles,
                [
                    "Couldn't resume Claude in tab 1",
                    "Couldn't resume Claude in tab 4"
                ]
            );
        } else {
            assert_eq!(titles, ["Couldn't resume Claude in tab 1"], "CONTROL");
        }
    }
}

#[test]
fn a_restored_row_nobody_saw_through_comes_back_inside_the_next() {
    use aterm_messages::Retired;
    type Retire = fn(&mut App, aterm_messages::MessageId);
    fn now() -> std::time::Instant {
        std::time::Instant::now()
    }
    let cases: [(&str, Retire, Retired, bool); 5] = [
        (
            "unseen",
            |app, _| {
                let _ = app
                    .messages
                    .settle(now() + std::time::Duration::from_secs(3600), true);
            },
            Retired::Unseen,
            true,
        ),
        (
            "folded",
            |app, id| {
                assert!(app.messages.mark_seen(id, now()));
                let _ = app
                    .messages
                    .settle(now() + std::time::Duration::from_secs(3600), true);
            },
            Retired::Folded,
            false,
        ),
        (
            "withdrawn",
            |app, id| assert!(app.messages.withdraw(id, now())),
            Retired::Withdrawn,
            true,
        ),
        (
            "dismissed",
            |app, id| assert!(app.messages.dismiss(id, now())),
            Retired::Dismissed,
            false,
        ),
        (
            "resolved",
            |app, id| assert!(app.messages.resolve(id, aterm_messages::Outcome::Ok, now())),
            Retired::Resolved(aterm_messages::Outcome::Ok),
            false,
        ),
    ];
    for (case, retire, how, named) in cases {
        let (mut app, sids) = restored_desk(2);
        let stamp = crate::messages_host::wall_stamp_now();
        app.post_restored_row(restored_row(1, &[(&sids[0], "in tab 1", 1)]), stamp);
        let first = app.restored_shown.id.expect("posted");
        retire(&mut app, first);
        let retired = app
            .messages
            .log()
            .get(first)
            .and_then(|r| r.retired().cloned());
        assert!(app.messages.live(first).is_none(), "{case}: off the band");
        assert_eq!(retired, Some(how), "{case}: retired so");
        app.post_restored_row(
            restored_row(3, &[(&sids[0], "in tab 1", 1), (&sids[1], "in tab 2", 2)]),
            stamp,
        );
        let titles: Vec<String> = restored_up(&app).into_iter().map(|(t, _)| t).collect();
        let want = if named {
            "Couldn't resume Claude in 2 tabs"
        } else {
            "Couldn't resume Claude in tab 2"
        };
        assert_eq!(titles, [want], "{case}");
    }
}
