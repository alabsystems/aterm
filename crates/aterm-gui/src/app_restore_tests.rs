// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

/// The tracker through a maximize and a restore-down, as the `Moved` and
/// grid-commit events drive it: the normal frame follows every move and
/// re-grid of the NOT-maximized window, ignores the maximize's corner and
/// grown grid, and the capture of the maximized window persists the frame
/// the user had — so the reopened window is placed there and re-maximized,
/// and restore-down lands where the user had it. After restore-down the
/// window is normal again and the tracker follows it once more; a window
/// that is not maximized (or whose show state is unknown) persists exactly
/// what it is showing. The negative control: a tracker that took the
/// maximized events too would persist the monitor corner.
#[test]
fn the_tracker_keeps_the_normal_frame_through_a_maximize_and_restore() {
    let mut frame = super::NormalFrame {
        origin: None,
        rows: 24,
        cols: 80,
    };
    // Placed and resized by the user, not maximized.
    frame.note_origin(false, (300, 200));
    frame.note_grid(false, (30, 100));
    frame.note_origin(false, (320, 210));
    // Maximized: the window manager moves it to the corner and grows the grid.
    frame.note_origin(true, (0, 0));
    frame.note_grid(true, (62, 240));
    assert_eq!(
        super::captured_frame(Some((0, 0)), (62, 240), Some(true), frame),
        (Some((320, 210)), (30, 100)),
        "a maximized capture persists the frame the user had"
    );
    // Restored down: normal again, and the tracker follows.
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
    // A maximized window whose normal origin was never observed (Wayland
    // reports none) persists NO origin — the window manager places it —
    // rather than the maximized frame's corner.
    let unplaced = super::NormalFrame {
        origin: None,
        ..frame
    };
    assert_eq!(
        super::captured_frame(Some((0, 0)), (62, 240), Some(true), unplaced),
        (None, (30, 100))
    );
    // Negative control: fed the maximized events as normal ones, the capture
    // would persist the corner and the grown grid.
    let mut naive = frame;
    naive.note_origin(false, (0, 0));
    naive.note_grid(false, (62, 240));
    assert_ne!(
        super::captured_frame(Some((0, 0)), (62, 240), Some(true), naive),
        (Some((500, 50)), (30, 100))
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

/// Address a temp-dir file the way the SHIPPING seams do. The hand-rolled
/// `format!("file://{}")` this replaced is malformed on Windows — `C:\…`
/// lands where the authority goes, so the document host refuses it with
/// "malformed file URI" before a byte is read (a fixture defect, not a
/// product one: `app_tabs::mixed_tab_tests::file_uri` has the account).
fn file_uri(path: &std::path::Path) -> String {
    crate::native_document_host::path_to_file_uri(path).unwrap()
}

/// Install a stub session `id` in the pool + registry (the headless analogue of a
/// real spawn — see `stub_session`).
fn add_stub(app: &mut App, id: u64) {
    let s = crate::stub_session(id);
    App::register_session(&app.store, &s, None);
    app.pool.insert(s);
    app.next_session_id = app.next_session_id.max(id + 1);
}

/// Quit-side capture: a live App with a split first tab + a second tab yields a
/// manifest with the same shape (tab count, split structure, focus tagging, grid
/// size, active tab) — and it round-trips through the persisted TOML byte-for-byte.
#[test]
fn capture_reflects_the_live_layout() {
    let mut app = App::headless_for_test();
    // Split tab 0 (stub session 1 joins session 0)…
    add_stub(&mut app, 1);
    app.view_store
        .insert_terminal(1)
        .expect("split view identity");
    {
        let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
        assert!(ws.layouts[0].split_focused(pane::SplitDir::Vertical, 1));
    }
    assert!(app.sync_tab_model_from_layout(WindowId(0), 0));
    // …and append a second tab (stub session 2), which becomes active.
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
    // Focus tagging: `split_focused` moved focus to the NEW pane (session 1), the
    // second tree-order leaf of tab 0.
    let leaves = w.tabs[0].leaves();
    assert!(
        matches!(leaves[1], restore::PaneLayout::Leaf { focused: true, .. }),
        "the split's new pane holds tab 0's focus",
    );
    assert!(
        matches!(leaves[0], restore::PaneLayout::Leaf { focused: false, .. }),
        "the original pane lost it",
    );
    // And the capture survives the on-disk format round trip.
    let back =
        restore::RestoreManifest::from_toml(&m.to_toml().expect("serialize")).expect("parse");
    assert_eq!(m, back);
}

/// A COLD RESTORE BRINGS EVERY WINDOW BACK AT ITS OWN FRAME. A desk of two
/// windows of different sizes quits: window 0 at 29×100, window 1 at
/// 40×120 dragged a few px inside one cell. The quit's layout crosses its
/// file's TOML, and the next launch — window 0 at its row's grid
/// (`main_entry`), window 1 created from its row
/// (`create_window_internal_connected`), which takes the row's frame, then
/// first attached (`attach_os_window`) — brings window 1 back at its own
/// grid and at the size it had. After a zoomed quit the launch's
/// configured font no longer fits that size to the saved grid, so window 1
/// keeps its grid at the grid's exact frame. A row an older build wrote
/// (no size) still inherits the front window's grid.
///
/// FAILS WITHOUT THE FIX: the durable capture wrote no size, the row gave
/// no frame, and window 1 came back at the front window's grid (29×100,
/// not 40×120).
#[test]
fn a_cold_restore_brings_every_window_back_at_its_own_grid_and_size() {
    use crate::app_window::CarriedFrame;
    use winit::dpi::PhysicalSize;
    let (w0, w1) = (WindowId(0), WindowId(1));
    let grid = |app: &App, wid: WindowId| (app.windows[&wid].rows, app.windows[&wid].cols);
    // A real window's re-grid inside the size it has.
    let size_to = |app: &mut App, wid: WindowId, size: PhysicalSize<u32>| {
        app.windows.get_mut(&wid).expect("a window").win_px = Some(size);
        app.on_resize(wid, size);
    };
    // The quit's layout, across its file's wire.
    let quit = |app: &App| {
        let wire = app
            .capture_restore_manifest()
            .to_toml()
            .expect("serializes");
        restore::RestoreManifest::from_toml(&wire).expect("the next launch parses it")
    };
    // The next launch, at the configured font: window 0 at its row's grid,
    // window 1 from its row. Answers the launch and window 1's first size.
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
        ws.take_carried_frame(inner, row.is_some_and(|row| row.presence_row));
        let carried = ws.carried_inner_px.take();
        let frame = next.attach_frame_px(w1, rows, cols, carried);
        size_to(&mut next, w1, frame);
        (next, frame)
    };

    // THE DESK.
    let mut desk = App::headless_for_test();
    let base = desk.font_px;
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

    // A ZOOMED QUIT: the saved size holds the zoomed grid only at the zoom.
    desk.set_font_px(base + crate::FONT_ZOOM_STEP);
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

    // An older build's row, which names no size, restores as it always has.
    let mut older = layout;
    older.windows[1].inner_px = None;
    let (next, _) = relaunch(&older);
    assert_eq!(grid(&next, w1), (29, 100), "the front window's grid");
}

/// Drive session 0's engine with raw escape bytes, exactly as a PTY would.
/// The tab-label tests below all report titles/cwds this way so they prove
/// the REAL OSC 0/2/7 ingestion path, not a state poke.
fn feed_session0(app: &App, bytes: &[u8]) {
    let s = app.pool.get(0).expect("session 0");
    crate::term_lock(&s.term).process(bytes);
}

/// TAB-LABEL DEFAULT (cwd-as-title): a cwd under `$HOME` reads in the `~…`
/// form — byte-matching what the zsh integration's precmd puts in OSC 0
/// titles (`~` for home itself, `~/sub` below it) — so the no-integration
/// default label is indistinguishable from the integrated one.
///
/// Unix-only: the OSC 7 fed here is `file://localhost<$HOME>/…`, a Unix
/// cwd spelling (`$HOME` is `C:\Users\…` on Windows, and
/// `home_relative_suffix` reads a `/`-rooted remainder), so what the
/// Windows integration reports, and how it abbreviates, is a separate
/// question this fixture cannot ask.
#[cfg(unix)]
#[test]
fn tab_title_abbreviates_a_home_cwd_like_the_shell_integration() {
    let Some(home) = crate::app_tabs::cached_home() else {
        return; // no $HOME in this environment -> nothing to abbreviate
    };
    let mut app = App::headless_for_test();
    feed_session0(
        &app,
        format!("\x1b]7;file://localhost{home}/aterm-abbrev-proof\x07").as_bytes(),
    );
    assert_eq!(app.tab_titles(WindowId(0))[0], "~/aterm-abbrev-proof");
    feed_session0(
        &app,
        format!("\x1b]7;file://localhost{home}\x07").as_bytes(),
    );
    assert_eq!(
        app.tab_titles(WindowId(0))[0],
        "~",
        "home itself reads as the bare ~"
    );
}

/// TAB-LABEL DEFAULT (cwd-as-title): the cwd outranks the tab PRESENTATION
/// title, and an unset/cleared cwd falls through to it (and finally to the
/// literal "aterm") — the pre-feature fallback rungs survive verbatim
/// below the new cwd rung, so a session with no shell integration at all
/// labels exactly as before.
#[test]
fn tab_title_cwd_outranks_presentation_and_clearing_falls_back() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // Titleless, cwd-less baseline: a plain terminal tab's presentation
    // title is empty, so the label is the literal "aterm".
    assert_eq!(app.tab_titles(wid)[0], "aterm");
    // A presentation title takes the slot while nothing livelier exists.
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .tab_set
        .active_mut()
        .expect("active tab")
        .presentation
        .title = "Renamed".to_string();
    assert_eq!(app.tab_titles(wid)[0], "Renamed");
    // A reported cwd outranks the presentation…
    feed_session0(&app, b"\x1b]7;file://localhost/aterm-proof/live\x07");
    assert_eq!(app.tab_titles(wid)[0], "/aterm-proof/live");
    // …and CLEARING it (empty OSC 7 — integration torn down) falls back to
    // the presentation again, never to a stale path.
    feed_session0(&app, b"\x1b]7;\x07");
    assert_eq!(app.tab_titles(wid)[0], "Renamed");
}

/// TAB-LABEL DEFAULT (cwd-as-title): with no program-set title a terminal
/// tab is labeled by its session's cwd, verbatim outside `$HOME`, while
/// NON-terminal tabs are untouched by the cwd rung — a native tab keeps its
/// presentation title even while a terminal sibling in the SAME window is
/// labeled by its session's cwd (native views own no session, so no cwd
/// can ever apply to them).
#[test]
fn native_tab_titles_ignore_the_terminal_cwd_default() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(
        app.open_settings_tab(crate::native_settings::SettingsRoute::default()),
        "settings tab installs in the headless fixture"
    );
    feed_session0(
        &app,
        b"\x1b]7;file://localhost/aterm-proof/terminal-only\x07",
    );
    assert_eq!(
        app.tab_titles(wid),
        vec!["/aterm-proof/terminal-only", "Settings"],
        "the cwd labels ONLY the terminal tab; the native tab keeps its \
         presentation title"
    );
}

/// SESSION-METADATA stage 1 — the USER title (`meta set title`) is the TOP
/// tab-label rung: it outranks a live OSC 0/2 title AND the reported cwd,
/// and CLEARING it (`meta unset title`) falls back to the live title — the
/// pre-existing chain survives verbatim beneath the new rung, whose baseline
/// is a live OSC title outranking the reported cwd (the cwd is the DEFAULT
/// label, never a displacement of an explicit title). Driven through the
/// real ctx the label chain reads (the same object the `meta` verb
/// mutates), against real OSC ingestion.
#[test]
fn user_title_outranks_the_live_osc_title_and_clearing_falls_back() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    feed_session0(&app, b"\x1b]2;vim src/main.rs\x07");
    feed_session0(&app, b"\x1b]7;file://localhost/aterm-proof/meta\x07");
    assert_eq!(app.tab_titles(wid)[0], "vim src/main.rs", "baseline: OSC");

    // Clone the ctx handle out so the pool borrow doesn't pin `app`
    // (`tab_titles` needs `&mut self` for its keep-stale cache).
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    ctx.meta.lock().unwrap().user_title = Some("build agent".to_string());
    assert_eq!(
        app.tab_titles(wid)[0],
        "build agent",
        "the user title outranks the live OSC title"
    );
    // An EMPTY user title is treated as unset (never a blank tab label).
    ctx.meta.lock().unwrap().user_title = Some(String::new());
    assert_eq!(app.tab_titles(wid)[0], "vim src/main.rs");
    // Clearing falls back to the live title, then (title cleared too) the cwd.
    ctx.meta.lock().unwrap().user_title = None;
    assert_eq!(app.tab_titles(wid)[0], "vim src/main.rs");
    feed_session0(&app, b"\x1b]2;\x07");
    assert_eq!(
        app.tab_titles(wid)[0],
        "/aterm-proof/meta",
        "beneath the user rung the old chain survives verbatim"
    );
}

/// SESSION-METADATA stage 1 — a user-title change moves the tab-strip
/// fingerprint (the fingerprint hashes the exact `tab_titles` bytes), so the
/// `Wake::MetaChanged` refresh repaints the strip through the ordinary
/// RepaintKey path. Whatever rung changed the label — a titleless `cd`'s
/// cwd included — moves the same bytes.
#[test]
fn user_title_change_moves_the_tab_strip_fingerprint() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1; // enable the strip (fingerprint pinned 0 when disabled)
    let fingerprint = |app: &mut App| {
        let titles = app.tab_titles(wid);
        let metadata = app.tab_strip_metadata(wid);
        app.tab_strip_fingerprint_from_parts(wid, &titles, &metadata, 0)
    };
    let before = fingerprint(&mut app);
    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .meta
        .lock()
        .unwrap()
        .user_title = Some("build agent".to_string());
    let with_user_title = fingerprint(&mut app);
    assert_ne!(
        before, with_user_title,
        "a user-title change must move the strip fingerprint"
    );
    // And unsetting moves it back to the original label bytes.
    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .meta
        .lock()
        .unwrap()
        .user_title = None;
    assert_eq!(fingerprint(&mut app), before, "clearing restores the label");
}

/// SESSION-METADATA stage 1 — a restore leaf carrying user metadata re-seeds
/// the respawned session's ctx, and a leaf without any is a no-op (the meta
/// mutex is untouched on the common path).
#[test]
fn restore_leaf_reseeds_user_metadata_onto_the_respawned_session() {
    let bare = restore::TerminalLeafRestore {
        local_id: None,
        ..bare_leaf()
    };
    let leaf = restore::TerminalLeafRestore {
        cwd: Some("/tmp".to_string()),
        title: "shell".to_string(),
        user_title: Some("release builder".to_string()),
        description: Some("cuts the v0.56 release".to_string()),
        icon: Some("🚀".to_string()),
        role: Some("operator".to_string()),
        attention: Some("⚠ waiting on approval".to_string()),
        questions: Some("recommended".to_string()),
        ..bare.clone()
    };
    let session = crate::stub_session(9);
    App::seed_restored_user_meta(&session, &leaf);
    let meta = session.ctx.meta.lock().unwrap().clone();
    assert_eq!(meta.questions.as_deref(), Some("recommended"));
    assert_eq!(meta.user_title.as_deref(), Some("release builder"));
    assert_eq!(meta.description.as_deref(), Some("cuts the v0.56 release"));
    assert_eq!(meta.icon.as_deref(), Some("🚀"));
    assert_eq!(meta.role.as_deref(), Some("operator"));
    assert_eq!(meta.attention.as_deref(), Some("⚠ waiting on approval"));

    // A metadata-less leaf leaves an already-seeded ctx untouched (no wipe).
    App::seed_restored_user_meta(&session, &bare);
    assert_eq!(
        session.ctx.meta.lock().unwrap().user_title.as_deref(),
        Some("release builder"),
        "a leaf with no metadata never clears live metadata"
    );

    // Every question-policy word reseeds as written — the priority words
    // `long-term` and `short-term` too (an older build's sanitizer drops
    // them; this one keeps them).
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
        // A hand-edited policy outside the four words is dropped.
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

    // Defense at the presentation seam also covers an old/internal caller
    // that assigned the public compatibility fields directly.
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
        tooltip
            .lines()
            .all(|line| { !crate::session_timeline::metadata_has_forbidden_formatting(line) })
    );
    assert!(tooltip.contains("description: firstsecond"), "{tooltip:?}");
    assert!(tooltip.starts_with(family), "{tooltip:?}");
}

/// The last-window close DECIDES to exit and stashes the manifest BEFORE the
/// teardown drains `windows`/`pool` — the seam the post-loop writer depends on
/// (after `close_window_logical` there is nothing left to capture).
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
    // [terminal, Settings, Editor] -> [Settings, terminal, Editor]. Relative
    // terminal/native projection order remains monotonic while canonical chrome mixes.
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
fn native_only_restore_retires_bootstrap_without_fabricating_a_session() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.restore_into_window(wid, legacy_native_window(updates_settings()));
    let ws = &app.windows[&wid];
    assert_eq!(ws.tab_set.len(), 1);
    assert!(ws.layouts.is_empty());
    assert_eq!(ws.tabs, crate::TabIndex::new(0, 0));
    assert!(ws.active_terminal.is_none());
    assert!(ws.front_terminal().is_none());
    assert_eq!(app.focused_session_id(wid), None);
    assert!(app.pool.get(0).is_none(), "bootstrap session retired");
    assert!(app.store.read().unwrap().snapshot().is_empty());
    let (_, view) = app.active_native_view(wid).expect("native Settings");
    assert_eq!(
        ws.window_focus,
        crate::front_content::WindowFocus::Content(view),
        "the first restored native view owns window focus"
    );
    assert!(matches!(
        app.native_runtime.view_state(view),
        Some(crate::native_app::AppViewState::Settings(state))
            if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
    ));
    assert!(app.structural_invariants_ok());
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

    // Negative restore outcome: the unmounted candidate is discarded after
    // validation fails. The already-republished front must remain unchanged.
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

/// A handoff successor whose window 0 has no terminal leaf spawns session 0
/// FRESH (`take_session0_shell`): no handed-off shell belongs in window 0.
/// That bootstrap is as throwaway as a cold one, so a native-only rebuild
/// retires it — on the canonical path a handoff layout takes and on the
/// legacy one — instead of keeping it as an extra tab the layout never had.
#[test]
fn a_successors_fresh_bootstrap_is_retired_by_a_native_only_window_0() {
    for (path, layout) in [
        ("canonical", window_of(vec![settings_leaf()])),
        ("legacy", legacy_native_window(updates_settings())),
    ] {
        let mut app = App::headless_for_test();
        app.handoff_successor = true;
        app.restore_into_window(WindowId(0), layout);
        let window = &app.windows[&WindowId(0)];
        assert_eq!(window.tab_set.len(), 1, "{path}: only the Settings tab");
        assert!(window.layouts.is_empty(), "{path}");
        assert!(
            app.pool.get(0).is_none(),
            "{path}: the fresh bootstrap retired"
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

/// One terminal leaf, exactly as `capture_restore_manifest` writes it.
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

/// THE ALIASING REGRESSION. Reproduced from a cold launch on 2026-08-26: open
/// three tabs, split the MIDDLE one, quit, relaunch. Capture walks the tabs in
/// tree order, so the four shells persist as the DESCENDING run `0, 1, 3, 2`
/// — and the old `restore_terminal_leaf` treated "the pool already holds id 2"
/// as recognition, even though that id belonged to the fresh shell this same
/// pass had just forked for the split. The third tab grafted itself onto the
/// split's new pane: three tabs came back over three shells instead of four,
/// CHARLIE's saved shell was never started, and typing in the third tab
/// appeared in the second. The manifest's ids are a DEAD process's namespace;
/// nothing here may consult them for a live handle.
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

    // WRITE ISOLATION — the symptom a user actually meets. Bytes fed to one
    // leaf's engine, exactly as its PTY would, must land on that leaf's screen
    // and on NO other's.
    for (index, id) in sessions.iter().enumerate() {
        let session = app.pool.get(*id).expect("restored session");
        crate::term_lock(&session.term).process(format!("MARKER{index}").as_bytes());
    }
    for (index, id) in sessions.iter().enumerate() {
        let session = app.pool.get(*id).expect("restored session");
        let terminal = crate::term_lock(&session.term);
        let screen = (0..i32::from(terminal.rows()))
            .filter_map(|row| terminal.get_line_text(row, None))
            .collect::<String>();
        assert!(
            screen.contains(&format!("MARKER{index}")),
            "leaf {index} lost its own typing"
        );
        for other in (0..sessions.len()).filter(|other| *other != index) {
            assert!(
                !screen.contains(&format!("MARKER{other}")),
                "leaf {index} is showing leaf {other}'s typing — they share a PTY"
            );
        }
    }

    // Each respawned leaf also carries ITS OWN operator title, so the saved
    // work contexts stayed separate all the way to the tab strip. (Tab 0's
    // shell is the grafted bootstrap, which keeps the live session it already
    // had rather than being re-seeded.)
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
fn recursive_native_only_restore_retires_the_cold_bootstrap() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.restore_into_window(wid, window_of(vec![settings_leaf()]));
    let window = &app.windows[&wid];
    assert_eq!(window.tab_set.len(), 1);
    assert!(window.layouts.is_empty());
    assert!(app.pool.get(0).is_none());
    assert!(app.store.read().unwrap().snapshot().is_empty());
    let (_, view) = app.active_native_view(wid).expect("Settings restored");
    assert_eq!(
        window.window_focus,
        crate::front_content::WindowFocus::Content(view),
        "recursive native-only restore mounts its selected content"
    );
    assert!(matches!(
        app.native_runtime.view_state(view),
        Some(crate::native_app::AppViewState::Settings(state))
            if state.route == crate::native_settings::SettingsRoute::About
    ));
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
    assert!(source.open_settings_tab(crate::native_settings::SettingsRoute::Manual));
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
fn legacy_manual_settings_restore_normalizes_with_or_without_singleton() {
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
                if state.route == crate::native_settings::SettingsRoute::Home
        ));
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

/// The ADOPTION-SIDE RE-MINT (design §1.4#6): carried tokenless triples
/// become live rows + a rebuilt `ConnectionRecord` through the one
/// kind-bounded helper — while a lone push half, an op the vocabulary
/// cannot spell, and a triple whose endpoint did not survive the swap are
/// all dropped fail-soft (authority only ever shrinks). One-shot: the
/// carry drains, so a second pass is a no-op.
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
        // A full `both` set a → b: re-mints as ONE Both connection.
        carry(a.as_str(), b.as_str(), "read-screen"),
        carry(a.as_str(), b.as_str(), "signal"),
        carry(a.as_str(), b.as_str(), "write-input"),
        // A lone push half b → a: NEVER rounded up to a full seat — drops.
        carry(b.as_str(), a.as_str(), "write-input"),
        // An op no connection can spell (§1.4#2): drops.
        carry(a.as_str(), b.as_str(), "config-write"),
        // An endpoint that did not survive the swap: drops.
        carry("s-gone", b.as_str(), "read-screen"),
    ];

    app.remint_carried_connections();

    assert!(
        app.pending_conn_carry.is_empty(),
        "the carry drains (one-shot)"
    );
    // b's table holds exactly the re-minted `both` rows, all src = a.
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
    // a's table stays EMPTY: the lone half was dropped, not widened.
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
    // The record store was REBUILT (dissolution stays pair-precise).
    {
        let records = app.connections.records();
        assert_eq!(records.len(), 1);
        let rec = records
            .get(&(a.clone(), b.clone()))
            .expect("the a → b record is back");
        assert_eq!(rec.kind(), Some(aterm_session::ConnectionKind::Both));
    }
    // And the ❯ count agrees with the map's arrow fold: one connection.
    assert_eq!(crate::connections::connection_count(&app.store), 1);

    // Second pass: nothing pending, nothing changes.
    app.remint_carried_connections();
    assert_eq!(crate::connections::connection_count(&app.store), 1);
}

/// A one-window, one-terminal-leaf layout whose leaf carries `leaf`'s USER
/// fields — the shape `capture_restore_manifest` writes for a window that
/// holds a single session.
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
        identity: None,
        agent: None,
        supervised: None,
        held: false,
        holds: Vec::new(),
    }
}

/// A LEGACY (mirror-only) layout whose one tab is `native`: no terminal
/// tab and no recursive tree.
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

/// Mark pool session `session` as the shell a seamless handoff ADOPTED
/// under the parent's id `parent_id` — what `spawn_session` records from
/// the `Adopted` handle.
fn adopt_as(app: &mut App, session: u64, parent_id: u64) {
    app.pool
        .sessions
        .get_mut(&session)
        .expect("live session")
        .session
        .handoff_local_id = Some(parent_id);
}

/// The live USER metadata of pool session `session`, read the way `@<sid>
/// meta` reads it: resolve the sid through the registry, then lock the
/// handle's ctx.
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

/// THE SELF-UPDATE HANDOFF DROPPED EVERY SESSION'S USER METADATA. Measured
/// 2026-09-12 on the 0.82.0 -> 0.83.0 handoff, on a machine with two windows
/// and one terminal in each: before it `meta` read `role=agent:claude-driver-
/// fable` on one session and `role=worker:claude-satcomp` plus an
/// `attention=` on the other; after it both — the same sids, `attribution=
/// adopted` — read `user_title=- description=- icon=- role=- attention=-`.
///
/// This drives every stage that shape crosses, in order: `meta set`'s own
/// write ladder, the post-park capture (`capture_restore_manifest`, the call
/// `start_unix_update_handoff` makes), the exact sidecar bytes the parent
/// writes (`to_toml`) and the child's acceptance of them (`from_toml` plus
/// `covers_exact_seamless_ids`), then the successor's per-window apply onto
/// the sessions it ADOPTED — each window's bootstrap, which is what fills a
/// window's first terminal leaf. Capture, wire and parse all carried the
/// fields; the graft in `restore_terminal_leaf` dropped them.
#[test]
fn a_handoff_keeps_user_metadata_on_every_adopted_window_bootstrap() {
    use crate::session_timeline::{MetaEdit, MetaField, SessionMeta, write_session_meta};

    let stamp = |app: &App, session: u64, field: MetaField, value: &str| {
        let ctx = app.pool.get(session).expect("live session").ctx.clone();
        assert_eq!(
            write_session_meta(&ctx, field, MetaEdit::Set(value)),
            Ok(true),
            "`meta set {}` stores",
            field.wire_name()
        );
    };

    // PREDECESSOR: two windows, one terminal each — the measured layout.
    let mut old = App::headless_for_test();
    let second = crate::stub_session(1);
    App::register_session(&old.store, &second, None);
    let old_second_window = old.insert_logical_window(second, 24, 80);
    // Session 0 carries all five fields; session 1 carries two, and its
    // title, description and icon are never set.
    stamp(&old, 0, MetaField::Title, "fable driver");
    stamp(&old, 0, MetaField::Description, "drives the satcomp worker");
    stamp(&old, 0, MetaField::Icon, "🦊");
    stamp(&old, 0, MetaField::Role, "agent:claude-driver-fable");
    stamp(&old, 0, MetaField::Attention, "waiting on a human review");
    stamp(&old, 1, MetaField::Role, "worker:claude-satcomp");
    stamp(
        &old,
        1,
        MetaField::Attention,
        "SAT-COMP campaign docs handoff",
    );
    // The worker's questions are answered with their recommended option
    // (2026-09-24): the sixth field rides the same capture, wire and graft.
    stamp(&old, 1, MetaField::Questions, "recommended");
    let driver = SessionMeta {
        user_title: Some("fable driver".to_string()),
        description: Some("drives the satcomp worker".to_string()),
        icon: Some("🦊".to_string()),
        role: Some("agent:claude-driver-fable".to_string()),
        attention: Some("waiting on a human review".to_string()),
        ..SessionMeta::default()
    };
    let worker = SessionMeta {
        role: Some("worker:claude-satcomp".to_string()),
        attention: Some("SAT-COMP campaign docs handoff".to_string()),
        questions: Some("recommended".to_string()),
        ..SessionMeta::default()
    };
    assert_eq!(registry_meta(&old, 0), driver, "PRECONDITION: stamped");
    assert_eq!(registry_meta(&old, 1), worker, "PRECONDITION: stamped");

    // CAPTURE, then the WIRE: the parent writes exactly `to_toml()` to the
    // layout sidecar (and hashes those bytes); the child parses the bytes it
    // read and refuses a layout that does not name every handed-off id once.
    let captured = old.capture_restore_manifest();
    let wire = captured
        .to_toml()
        .expect("the parent serializes its layout");
    for spelled in [
        "user_title = \"fable driver\"",
        "role = \"agent:claude-driver-fable\"",
        "role = \"worker:claude-satcomp\"",
        "attention = \"SAT-COMP campaign docs handoff\"",
        "questions = \"recommended\"",
    ] {
        assert!(
            wire.contains(spelled),
            "the sidecar spells {spelled}: {wire}"
        );
    }
    let carried = restore::RestoreManifest::from_toml(&wire)
        .filter(|layout| layout.covers_exact_seamless_ids(&[0, 1]))
        .expect("the child accepts the sidecar");
    assert_eq!(
        carried, captured,
        "neither the wire nor the parse drops a field"
    );

    // SUCCESSOR: window 0's bootstrap is the shell adopted in `main_entry`, window
    // 1's the one adopted in `create_window_internal_connected` — each picked by
    // `bootstrap_local_id`, and each keeping the id the parent knew it by.
    // Adoption mints a fresh ctx, so both start with no USER metadata.
    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    let picked: Vec<Option<u64>> = carried
        .windows
        .iter()
        .map(restore::WindowLayout::bootstrap_local_id)
        .collect();
    assert_eq!(picked, vec![Some(0), Some(1)]);
    adopt_as(&mut new, 0, 0);
    let adopted = crate::stub_session(1);
    App::register_session(&new.store, &adopted, None);
    let new_second_window = new.insert_logical_window(adopted, 24, 80);
    adopt_as(&mut new, 1, 1);
    assert_eq!(new_second_window, old_second_window);
    assert!(
        !registry_meta(&new, 0).any_set() && !registry_meta(&new, 1).any_set(),
        "PRECONDITION: an adopted session arrives with a fresh ctx"
    );

    // APPLY: what `apply_restore_manifest` runs for each window once its
    // host (and so its bootstrap session) exists.
    let mut windows = carried.windows.into_iter();
    new.restore_into_window(WindowId(0), windows.next().expect("window 0"));
    new.restore_into_window(new_second_window, windows.next().expect("window 1"));

    for (window, session) in [(WindowId(0), 0), (new_second_window, 1)] {
        assert_eq!(
            new.focused_session_id(window),
            Some(session),
            "the leaf was filled by the ADOPTED session, not a respawn"
        );
    }
    assert_eq!(
        registry_meta(&new, 0),
        driver,
        "all five fields survive onto the adopted session"
    );
    assert_eq!(
        registry_meta(&new, 1),
        worker,
        "the two set fields survive and the three unset ones stay unset"
    );
    // And the successor now carries exactly the layout the parent's Commit
    // compared — USER metadata included, which `commit_layout_topology`
    // deliberately keeps in that comparison.
    assert_eq!(new.capture_restore_manifest(), captured);
    assert!(new.structural_invariants_ok());
}

/// A parent built before the carry dropped it still writes the retired
/// Claude Code hooks' bare badge into the sidecar (v0.95.0 carried one for
/// two days on a busy agent's tab, 2026-09-27). The successor's parse drops
/// it, so the adopted session arrives without it: the handoff INTO a fixed
/// build is what clears the one standing. NEGATIVE CONTROL: the other
/// session's attention arrives intact.
#[test]
fn a_handoff_from_an_older_parent_drops_the_retired_hook_badge() {
    use crate::session_timeline::{MetaEdit, MetaField, write_session_meta};

    let legacy = "claude needs approval: Claude needs your permission - permission_prompt";
    let mut old = App::headless_for_test();
    let second = crate::stub_session(1);
    App::register_session(&old.store, &second, None);
    let old_second_window = old.insert_logical_window(second, 24, 80);
    for (session, text) in [(0, "PLACEHOLDER"), (1, "SAT-COMP campaign docs handoff")] {
        let ctx = old.pool.get(session).expect("live session").ctx.clone();
        assert_eq!(
            write_session_meta(&ctx, MetaField::Attention, MetaEdit::Set(text)),
            Ok(true)
        );
    }
    // This build's capture would drop the badge, so it is put in the wire
    // where the placeholder stands — what the older parent writes.
    let wire = old
        .capture_restore_manifest()
        .to_toml()
        .expect("the parent serializes its layout")
        .replace(
            "attention = \"PLACEHOLDER\"",
            &format!("attention = \"{legacy}\""),
        );
    assert!(
        wire.contains(legacy),
        "the sidecar spells the badge: {wire}"
    );
    let carried = restore::RestoreManifest::from_toml(&wire)
        .filter(|layout| layout.covers_exact_seamless_ids(&[0, 1]))
        .expect("the child accepts the sidecar");

    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    adopt_as(&mut new, 0, 0);
    let adopted = crate::stub_session(1);
    App::register_session(&new.store, &adopted, None);
    let new_second_window = new.insert_logical_window(adopted, 24, 80);
    adopt_as(&mut new, 1, 1);
    assert_eq!(new_second_window, old_second_window);
    let mut windows = carried.windows.into_iter();
    new.restore_into_window(WindowId(0), windows.next().expect("window 0"));
    new.restore_into_window(new_second_window, windows.next().expect("window 1"));

    let first = registry_meta(&new, 0);
    assert_eq!(first.attention, None, "the badge did not arrive");
    assert!(first.attention_owners.is_empty());
    assert_eq!(
        registry_meta(&new, 1).attention.as_deref(),
        Some("SAT-COMP campaign docs handoff")
    );
}

/// The graft stores the leaf's USER fields EXACTLY over anything a driver
/// did not write: an unset leaf field is unset on the session afterwards
/// even when the session held a value — as it does on a cold restore when
/// an earlier graft's tab failed to build and handed the bootstrap on to
/// the next terminal leaf. (`stale` is assigned raw, the way that earlier
/// graft stored it: no driver wrote it.)
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

    // A leaf carrying nothing leaves a grafted session with nothing.
    let mut app = App::headless_for_test();
    *app.pool.get(0).unwrap().ctx.meta.lock().unwrap() = stale;
    app.restore_into_window(WindowId(0), single_leaf_window(bare_leaf()));
    assert!(
        !registry_meta(&app, 0).any_set(),
        "{:?}",
        registry_meta(&app, 0)
    );
}

/// The bootstrap window presents BEFORE the deferred restore runs, so its
/// session's tooltip is already composed and cached by the time the graft
/// seeds it. An icon-only seed moves no label, activity or connection
/// revision the cache is keyed on; it recomposes only because the graft
/// records its `meta-change`, which moves the timeline high-water mark.
/// (Seeded without that record, the tooltip stayed without the icon.)
#[test]
fn a_graft_recomposes_the_chrome_the_first_present_cached() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // A live OSC title outranks the tab's fallback, so the label — one of
    // the cache's keys — is the same before and after the rebuild.
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

/// A driver's `aterm ctl @<sid> meta set|unset` on pool session `session`:
/// resolve the sid through the registry the way the control socket does,
/// then take the write door the verb's handler takes (`cmd_meta` calls
/// `write_session_meta` for a set and `apply_meta_value` for an unset,
/// which is what `write_session_meta` does with a `Clear`). `Ok` is the
/// `OK` the verb answers.
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
        crate::session_timeline::write_session_meta(&ctx, field, edit).is_ok(),
        "`meta` answers OK to {field:?} {edit:?}"
    );
}

/// A WRITE THE SUCCESSOR ANSWERED `OK` IS NEVER UNDONE BY ITS OWN RESTORE.
/// A handoff child binds its control socket in `main_entry` — repointing
/// `aterm.sock` and the adopted session's graph entry at itself — but runs
/// the deferred restore only after its first present, so a `meta set` can
/// land on the adopted bootstrap before the graft reaches it. On a cold
/// restore the bootstrap shell's own startup can do the same. The graft
/// keeps every field such a write touched — set, or cleared even where the
/// clear moved nothing — and fills only the others from the leaf.
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

/// THE LEAF MUST NAME THE SHELL IT IS GRAFTED ONTO. A tab split between a
/// native view and a terminal has no entry in the legacy `tabs` mirror (the
/// capture writes only all-terminal tabs there), so when such a tab comes
/// first, the mirror's first leaf and the recursive tree's first terminal
/// leaf — the one the graft fills — name different shells. `main_entry` adopted
/// window 0's bootstrap by the mirror; with the graft carrying identity,
/// that stamped one shell's role on another shell's live sid.
#[test]
fn a_graft_carries_a_leaf_only_onto_the_shell_that_leaf_names() {
    use crate::session_timeline::{MetaEdit, MetaField, SessionMeta};

    // PREDECESSOR: tab A splits a recovery placeholder with shell 0 (role
    // `operator`); tab B holds shell 1 alone (role `worker`). Built by the
    // real restore, stamped by the real verb, captured by the real capture.
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
    // The session each of window 0's tabs shows, in tab order.
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

    // `main_entry` picks by `bootstrap_local_id`: session 0 IS shell 0, fills tab
    // A's terminal pane, and takes shell 0's identity; tab B adopts shell 1.
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

    // The mirror's pick: session 0 is shell 1. The graft still fills tab
    // A's pane with it, but tab A's leaf names shell 0, so none of that
    // leaf's identity may land on this one. Tab B's leaf names shell 1,
    // which this restore already adopted, so tab B shows that shell again
    // (round six, finding 10: no stand-in shell is forked during the
    // overlap for a shell that is here), and shell 1's identity goes to
    // session 0, the shell it describes.
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

    // A handoff child's bootstrap it did NOT adopt stands in for a shell
    // running elsewhere; it is not the one the leaf names either.
    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    new.seamless_adopt = vec![handed_off_shell(1)];
    new.restore_into_window(WindowId(0), window);
    let placed = tab_sessions(&new);
    assert_eq!(placed[0], vec![0]);
    assert_eq!(registry_meta(&new, 0), SessionMeta::default());
    assert_eq!(registry_meta(&new, placed[1][0]), worker);
}

/// A FROZEN SHELL'S ADOPTION REACHES THE REGISTRY'S COUNT (2026-09-16), end to
/// end through the spawn seam: `seamless::take_incoming` decides
/// `Adopted::frozen_path` (the manifest's `outgoing_build` absent, or the
/// record's own flag), `spawn_session` copies it onto `Session::frozen_path`,
/// and `App::register_session` marks it on the store — where
/// `SessionStore::frozen_path_tabs` counts it for the managed row's "N tabs
/// from before this update" note and the seed pill's narrowing. Both adoption
/// paths are driven: the restore of a leaf naming the shell, and the orphan
/// net. Dropping `Session::frozen_path` (or the copy in either path) fails
/// this, and with it the note that keeps the "every aterm tab" claim honest.
#[test]
fn a_frozen_shells_adoption_reaches_the_registrys_frozen_tab_count() {
    let frozen_shell = |local_id: u64| {
        let mut shell = handed_off_shell(local_id);
        shell.frozen_path = true;
        shell
    };
    let frozen_sessions = |app: &App| -> Vec<(Option<u64>, bool, bool)> {
        let store = app.store.read().unwrap();
        let mut rows: Vec<_> = app
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

    // The restore path: tab B's leaf names shell 3, handed across frozen; tab A
    // is a stand-in (no shell adopted, never frozen).
    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    new.seamless_adopt = vec![frozen_shell(3)];
    new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0), leaf_naming(3)]));
    assert!(new.seamless_adopt.is_empty(), "the leaf adopted shell 3");
    assert_eq!(
        frozen_sessions(&new),
        vec![(None, false, false), (Some(3), true, true)],
        "the session adopted from shell 3 is frozen on the session AND the registry"
    );
    assert_eq!(new.store.read().unwrap().frozen_path_tabs(), 1);

    // The orphan net: shell 1 frozen, shell 2 not — each counted as it came.
    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0)]));
    new.seamless_adopt = vec![frozen_shell(1), handed_off_shell(2)];
    new.adopt_orphan_shells_as_tabs(&[]);
    assert!(new.seamless_adopt.is_empty(), "the net placed every orphan");
    assert_eq!(
        frozen_sessions(&new),
        vec![
            (None, false, false),
            (Some(1), true, true),
            (Some(2), false, false)
        ]
    );
    assert_eq!(new.store.read().unwrap().frozen_path_tabs(), 1);
}

/// THE IDENTITY CARRY (session identities, 2026-09-17), end to end. Across
/// a seamless update the record's label reaches the session and the
/// registry handle through both adoption paths (the leaf naming the shell,
/// and the orphan net) — the shell keeps the identity's env, the label is
/// what rides. Across a quit, a leaf naming an identity respawns under it
/// only if it still exists: `create = false` — a leaf naming a forgotten
/// identity comes back as a default shell, and nothing is created for it.
#[test]
fn an_identitys_label_rides_the_adoption_and_a_cold_restore_never_creates_one() {
    let state = std::env::temp_dir().join(format!(
        "aterm-restore-identity-{}-{}",
        std::process::id(),
        aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&state).unwrap();
    let labels = |app: &App| -> Vec<(Option<u64>, Option<String>, Option<String>)> {
        let store = app.store.read().unwrap();
        let mut rows: Vec<_> = app
            .pool
            .sessions
            .iter()
            .map(|(&session, pooled)| {
                (
                    pooled.session.handoff_local_id,
                    pooled.session.identity.clone(),
                    store
                        .by_local(session)
                        .and_then(|h| h.identity.as_deref().map(str::to_owned)),
                )
            })
            .collect();
        rows.sort_unstable();
        rows
    };
    crate::test_env::scoped("ATERM_STATE_HOME", &state, || {
        // Seamless, the leaf path: the record says `worker`, the session and
        // its handle say `worker`; the stand-in for leaf 0 says nothing.
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        let mut shell = handed_off_shell(3);
        shell.identity = Some("worker".to_string());
        new.seamless_adopt = vec![shell];
        new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0), leaf_naming(3)]));
        assert!(new.seamless_adopt.is_empty(), "the leaf adopted shell 3");
        assert_eq!(
            labels(&new),
            vec![
                (None, None, None),
                (
                    Some(3),
                    Some("worker".to_string()),
                    Some("worker".to_string())
                ),
            ]
        );
        // Seamless, the orphan net: the same carry.
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0)]));
        let mut shell = handed_off_shell(1);
        shell.identity = Some("reviewer".to_string());
        new.seamless_adopt = vec![shell, handed_off_shell(2)];
        new.adopt_orphan_shells_as_tabs(&[]);
        assert_eq!(
            labels(&new),
            vec![
                (None, None, None),
                (
                    Some(1),
                    Some("reviewer".to_string()),
                    Some("reviewer".to_string())
                ),
                (Some(2), None, None),
            ]
        );
        // Cold: `worker` exists (the verb created it); `ghost` never did.
        crate::agent_identity::ensure("worker", true).expect("the verb's create");
        let mut cold = App::headless_for_test();
        cold.restore_into_window(
            WindowId(0),
            window_of(vec![
                leaf_naming(0),
                leaf_under(7, Some("worker")),
                leaf_under(8, Some("ghost")),
            ]),
        );
        assert_eq!(
            labels(&cold),
            vec![
                (None, None, None),
                (None, None, None),
                (None, Some("worker".to_string()), Some("worker".to_string())),
            ],
            "the leaf under `worker` respawns under it; the one under `ghost` is a default shell"
        );
        assert!(
            state.join("identities").join("worker").is_dir()
                && !state.join("identities").join("ghost").exists(),
            "restore never creates an identity"
        );
    });
    let _ = std::fs::remove_dir_all(&state);
}

/// The Claude Code agent a crash took from a tab, as its leaf records it.
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
        codex: None,
        cause: None,
    }
}

/// A terminal leaf naming shell `shell`, whose tab hosted `agent`.
fn leaf_hosting(shell: u64, agent: Option<restore::AgentRestore>) -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
        restore::TerminalLeafRestore {
            local_id: Some(shell),
            agent: agent.map(Box::new),
            ..bare_leaf()
        },
    ))
}

/// The Claude home and the login [`owed_relaunch`]'s agent ran under, as
/// the host read them off its process.
#[cfg(unix)]
const OWED_HOME: &str = "/Users//me/lab home/.claude";
#[cfg(unix)]
const OWED_LOGIN: &str = "/Users//me/logins/work-2";

/// The relaunch the supervisor host owes tab `tab`: its dead Claude Code
/// agent, that agent's conversation, and the Claude home and the login it
/// ran under.
#[cfg(unix)]
fn owed_relaunch(tab: &str) -> aterm_agent::harness::relaunch::Snapshot {
    aterm_agent::harness::relaunch::Snapshot {
        stored_login: false,
        tab: tab.to_string(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        shell: 4343,
        program: std::path::PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
        cwd: "/tmp".into(),
        version: None,
        codex: None,
        dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
        login: Some(OWED_LOGIN.into()),
        home: Some(OWED_HOME.into()),
        claude_dir: None,
    }
}

/// THE AGENT A CRASH TOOK GOES BACK TO ITS TAB (2026-09-27): a cold
/// restore of the live layout whose leaf carries an agent hands the host
/// that agent with the sid of the tab that now stands for the leaf and
/// that tab's own shell — once: a second take finds nothing. NEGATIVE
/// CONTROL: a leaf with no agent hands nothing. (A seamless successor
/// hands nothing at its restore pass: a handoff leaf's agent is owed only
/// at the Commit, to the adopted shell the leaf names —
/// `a_handoff_leaf_with_a_pending_agent_is_relaunched_by_the_successor`.)
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
    // A headless stub has no shell (pid -1): a tab with none takes no
    // relaunch. Give each one a pid, as a real spawn has.
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
    let (sid, snap, _) = &handed[0];
    assert_eq!(&snap.tab, sid);
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

    // ONE SESSION SHOWN IN TWO WINDOWS (`Open Session in New Window`) IS
    // RELAUNCHED ONCE (restore audit, 2026-10-02). The image names its
    // agent on both leaves (`fill_agents` fills by session), and a cold
    // restore gives each leaf a shell of its own: relaunched once per
    // leaf, one conversation was resumed in two tabs. The first leaf
    // takes it, and the reopened row names that one place.
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

    // …AND NOT AT ALL WHERE THE PTY KEEPER GAVE ITS SESSION'S LIVE SHELL
    // BACK: the leaf rebuilt first takes the kept shell, whose agent
    // still runs, and the other leaf's fresh shell must not resume the
    // same conversation beside it. RED before: relaunched in the second
    // window's tab.
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

/// ROUND FOUR, PLAN ITEM 7 (c): A HANDOFF LEAF THAT NAMES AN AGENT — one
/// the outgoing instance's cold-restore queue still owed at its park — is
/// relaunched by the successor, at its Commit, in the ADOPTED shell the
/// leaf names (where the outgoing instance was relaunching it), under that
/// tab's sid. NEGATIVE CONTROLS: nothing before the Commit; a handoff leaf
/// with no agent (what every older producer writes) owes nothing; a leaf
/// naming a shell this process did not adopt is dropped, never given to
/// a stand-in.
///
/// RED before the change: `carry_restored_identity` skipped every agent
/// on a seamless successor (`!self.handoff_successor`), so the leaf's
/// agent was dropped and nothing was ever handed.
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
    // The shells the handoff adopted: 7 and 8 (9 was not), each with a
    // pid as a real adoption has.
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
    let (sid, snap, _) = &handed[0];
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

/// ROUND FOUR, PLAN ITEM 7 (b)+(c), THE OUTGOING SIDE: a park pauses the
/// supervisor host's cold-restore queue (`App::park_all_readers`), the
/// handoff layout names the agent a tab is still owed on that tab's leaf
/// — the same as the Commit's re-capture, whatever the step in flight
/// ends in — and a rollback resumes the queue. NEGATIVE CONTROLS: a tab
/// owed nothing names nothing, and a person's quit layout never names an
/// owed agent.
///
/// THE CRASH JOURNAL NAMES IT TOO (restore audit, 2026-10-02): until the
/// relaunched agent runs, nothing is published for its tab, so the
/// journal's images of a launch that reopened a crashed desk named no
/// agent there: stopped again before the relaunch ran (its probation's
/// second chance), the next launch reopened that tab bare.
///
/// AND SO DOES A RESTART'S QUIT LAYOUT (restore audit round two,
/// 2026-10-02), filled after the host's shutdown as `main_entry` fills
/// it ([`App::fill_desk_agents`]).
///
/// EACH WITH THE CLAUDE HOME AND THE LOGIN ITS AGENT RAN UNDER (2026-10-09):
/// the owed snapshot's, which every one of the three used to drop, so the
/// next launch, or the successor, relaunched it in the default home on the
/// default account.
///
/// RED before the change: every handoff leaf said `agent: None`; before
/// the journal's change, so did every leaf of its image; before round
/// two, so did every leaf of the restart's quit layout; before
/// 2026-10-09, each named the agent with no home and no login.
#[cfg(unix)]
#[test]
fn the_handoff_layout_carries_the_agent_a_restored_tab_is_still_owed() {
    let tried = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let t = std::sync::Arc::clone(&tried);
    let host =
        crate::harness_host::HostHandle::for_restored_test(std::sync::Arc::new(move |_, _, _| {
            t.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            "wait:shell-prompt".to_string()
        }));
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
    }]);
    app.harness = Some(host.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while tried.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(std::time::Instant::now() < deadline, "the first step");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // Each leaf's agent: its pid, and that it names the home and the login
    // its agent ran under.
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
    // The journal's real capture, through its writer thread.
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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while tried.load(std::sync::atomic::Ordering::SeqCst) == at_park {
        assert!(
            std::time::Instant::now() < deadline,
            "the rollback resumes the queue"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // A RESTART'S QUIT NAMES IT TOO (restore audit round two,
    // 2026-10-02): the quit retires the journal that named it, so its
    // layout is the one record left. `main_entry`'s order: the host
    // shuts down first, its queue's worker sees that, then the fill.
    // RED before: the quit asked for hosted agents only, and the
    // shutdown emptied the queue — `[(Some(0), None), (Some(1), None)]`
    // with either half of the fix alone.
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

/// A RELAUNCH OWED BETWEEN THE PARK'S CAPTURE AND THE COMMIT'S RE-CAPTURE
/// (round seven, finding 111). The agent a worker's back-off owes a tab is
/// read live into the handoff layout, so an agent that crashes while the
/// successor boots is named by the Commit's re-capture and not by the
/// pending layout the successor restores. The Commit is refused — the
/// successor would not relaunch it, and this process, once it exited,
/// could not — for a reason of its own, as activity: the rollback resumes
/// the worker, which relaunches the agent here, and the next attempt
/// carries whatever is still owed. The reverse (an owed relaunch settled in
/// between) is refused the same way. NEGATIVE CONTROL: the window/tab/pane
/// topology is unchanged in both.
///
/// RED before the fix: the leaf's agent was compared as topology, and the
/// Commit was refused as "window/tab/pane topology changed".
///
/// The host knows the tab is open (`for_restored_test_with_open_tabs`), as
/// a live App's harness does: an inert one forgot the owed relaunch at
/// whatever pass another test's bell started between the captures.
#[cfg(unix)]
#[test]
fn a_relaunch_owed_between_the_two_captures_refuses_the_commit_by_name() {
    use crate::app_update_handoff::{commit_agent_carry, commit_layout_topology};
    let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
        std::sync::Arc::new(|_, _, _| "wait:shell-prompt".to_string()),
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
    // The agent crashes while the successor boots: owed after the capture.
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
    // The reverse: owed at the capture, settled before the re-capture.
    let pending = app.capture_handoff_layout();
    host.owe_for_test(&sid, None);
    let live = app.capture_handoff_layout();
    assert_eq!(
        commit_layout_topology(&live),
        commit_layout_topology(&pending)
    );
    assert_ne!(commit_agent_carry(&live), commit_agent_carry(&pending));
    // NEGATIVE CONTROL: nothing moved, nothing differs.
    assert_eq!(
        commit_agent_carry(&app.capture_handoff_layout()),
        commit_agent_carry(&live)
    );
    app.rollback_overlap(None, &[]);
}

/// A RELAUNCH ON EXIT OWED ACROSS A SUCCESSFUL COMMIT KEEPS THE CLAUDE HOME
/// AND THE LOGIN ITS AGENT RAN UNDER (2026-10-09): the agent a worker's
/// back-off owes a tab at the park (`Kept::owed`: a crash, a stall's
/// remedy, the explicit restart — its snapshot read off the live process,
/// home and login with it) is named on that tab's handoff leaf with both,
/// through the layout's wire, and the successor's relaunch of it at its
/// Commit (the same step a cold restore takes) plans its line with both. It
/// came back in the default home on the default account before.
///
/// An owed relaunch the explicit restart's act made carries its cause too
/// since round eighteen (ruling 20's (iv): carried through a hand across the
/// Commit, its relaunch is the act's, never a counted try):
/// `an_owed_relaunchs_cause_rides_the_handoff_leaf`. Of every other — a
/// crash's — none rides, and the successor relaunches it as aterm's own end
/// (`host`), as before.
///
/// NEGATIVE CONTROL: owed with neither, the successor's line is today's.
#[cfg(unix)]
#[test]
fn an_owed_relaunch_keeps_its_home_and_login_across_the_commit() {
    use aterm_agent::harness::relaunch::{Snapshot, exit_line};
    let successor_line = |owed: Snapshot| -> (Snapshot, Option<String>) {
        let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
            std::sync::Arc::new(|_, _, _| "wait:shell-prompt".to_string()),
        );
        let mut app = App::headless_for_test();
        let _ = app.insert_logical_window(crate::stub_session(1), 24, 80);
        let sid = {
            let g = app.store.read().unwrap();
            g.by_local(0).expect("registered").sid.as_str().to_string()
        };
        app.harness = Some(host.clone());
        host.owe_for_test(
            &sid,
            Some(Snapshot {
                tab: sid.clone(),
                ..owed
            }),
        );
        assert!(app.park_all_readers(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN));
        // What the successor reads: the handoff layout's wire.
        let wire = app.capture_handoff_layout().to_toml().expect("the wire");
        app.rollback_overlap(None, &[]);
        let layout = restore::RestoreManifest::from_toml(&wire).expect("the successor reads it");
        let carried = layout
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .and_then(|leaf| leaf.agent)
            .expect("the owed tab's leaf names its agent");
        // The successor: the leaf's shell adopted, its Commit's carry.
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
        let (_, snap, _) = handed.remove(0);
        // In a zsh, as the relaunch's plan reads the adopted shell's dialect.
        let line = exit_line(&Snapshot {
            dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
            ..snap.clone()
        });
        (snap, line)
    };
    let program = "'/opt/claude/bin/claude' '--resume' '0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c'";
    let (snap, line) = successor_line(owed_relaunch("s-old"));
    assert_eq!(
        (snap.home.as_deref(), snap.login.as_deref()),
        (Some(OWED_HOME), Some(OWED_LOGIN))
    );
    assert_eq!(
        line,
        Some(format!(
            " CLAUDE_CONFIG_DIR='{OWED_HOME}' CLAUDE_SECURESTORAGE_CONFIG_DIR={OWED_LOGIN} \
             CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 {program}"
        ))
    );
    // NEGATIVE CONTROL: an agent that had neither — today's line.
    let (snap, line) = successor_line(Snapshot {
        home: None,
        login: None,
        ..owed_relaunch("s-old")
    });
    assert_eq!((snap.home, snap.login), (None, None));
    assert_eq!(
        line,
        Some(format!(" CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 {program}"))
    );
}

/// A CRASH DURING A SUCCESSFUL COMMIT'S HOLD REACHES THE SUCCESSOR (round
/// eighteen; `HostHandle::pending_restored_for`'s stated residual: a crash
/// read after the Commit's re-capture was on neither layout). The parent's
/// host supervises the tab's Claude Code, published as every supervised
/// agent is; a seamless update parks, its layout is captured, and its
/// Commit goes ahead — and the agent crashes during the Commit's hold, read
/// by the parent's exit watch in a process about to exit, typed into by
/// nobody there (parked, then stopped). The successor's Commit hands it the
/// tab: the agent its parent supervised there is watched from that Commit,
/// its exit read as the successor sees it — the agent gone, a crash's record
/// left — and it is relaunched once, by the successor alone.
///
/// FAILS ON 745d63b36: a handoff leaf carried an agent only where a
/// relaunch was owed at the park (`TerminalLeafRestore::agent`), and nothing
/// told the successor which agent ran in the tab: its first pass found no
/// agent named there and started nothing, and the crash was relaunched by
/// nobody and said to nobody (the wait for its relaunch runs out).
#[cfg(unix)]
#[test]
fn a_crash_during_a_successful_commits_hold_is_relaunched_by_the_successor() {
    use aterm_agent::harness::relaunch::Snapshot;
    // The parent: its host supervises the tab's agent, and published it.
    let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
        std::sync::Arc::new(|_, _, _| "refused:inert".to_string()),
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
    // The park's layout, which the successor restores.
    assert!(app.park_all_readers(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN));
    let wire = app.capture_handoff_layout().to_toml().expect("the wire");
    // The Commit goes ahead; the agent crashes during its hold, and the
    // parent exits with what it read.
    host.suspend();
    host.shutdown_and_join();
    crate::harness_host::publish_snapshot_for_test(&sid, None);
    app.rollback_overlap(None, &[]);
    // The successor: the leaf's shell adopted, its Commit.
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
    // Its Commit, in the run loop's order.
    successor_host.resume_after_handoff(successor.handoff_carry_on_places());
    successor.hand_carried_restored_agents();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the crash during the hold relaunched by the successor",
        || (!relaunched.lock().unwrap().is_empty()).then_some(()),
    );
    // Once, on the crash's record: two whole passes since with no worker
    // left — the one handed the exit ended — so no second worker, and no
    // second relaunch, can follow.
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

/// AN OWED RELAUNCH'S CAUSE RIDES THE HANDOFF LEAF (round eighteen; ruling
/// 20's (iv)): a relaunch the explicit restart's act owes a tab (the
/// worker's `OwedRelaunch::asked`) is named on that tab's handoff leaf with
/// its cause, through the layout's wire, and the successor's Commit hands
/// its restored queue the agent with it (`RestoredAgent::asked`), so the
/// queue carries it through a hand and relaunches it as the act's.
/// NEGATIVE CONTROL: a crash's owed relaunch writes no cause, and is handed
/// with none.
#[cfg(unix)]
#[test]
fn an_owed_relaunchs_cause_rides_the_handoff_leaf() {
    use aterm_agent::harness::restart_stamp::Cause;
    let carried = |asked: Option<Cause>| -> (bool, Option<Cause>) {
        let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
            std::sync::Arc::new(|_, _, _| "wait:shell-prompt".to_string()),
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
        let agent = layout
            .handed_off_leaves()
            .into_iter()
            .find(|leaf| leaf.local_id == Some(0))
            .and_then(|leaf| leaf.agent)
            .expect("the owed tab's leaf names its agent");
        let mut successor = App::headless_for_test();
        successor.handoff_successor = true;
        successor.restore_into_window(WindowId(0), window_of(vec![leaf_hosting(8, Some(*agent))]));
        let adopted = *successor.pool.sessions.keys().max().expect("a session");
        adopt_as(&mut successor, adopted, 8);
        for (&id, pooled) in successor.pool.sessions.iter_mut() {
            pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
        }
        successor.queue_carried_restored_agents();
        let mut handed = successor.take_restored_agents();
        assert_eq!(handed.len(), 1, "{handed:?}");
        let (_, _, asked) = handed.remove(0);
        (wire.contains("cause = "), asked)
    };
    assert_eq!(
        carried(Some(Cause::Asked)),
        (true, Some(Cause::Asked)),
        "(the wire names it, handed with it)"
    );
    assert_eq!(carried(Some(Cause::Stall)), (true, Some(Cause::Stall)));
    // NEGATIVE CONTROL: a crash's.
    assert_eq!(carried(None), (false, None));
}

/// THE HANDOFF LEAF NAMES THE AGENT ITS HOST SUPERVISES, WHERE NOTHING IS
/// OWED (round eighteen): the agent the host last published for a tab rides
/// that tab's handoff leaf (`TerminalLeafRestore::supervised`) — never a
/// durable layout's, whose `agent` names what runs, and never a tab owed a
/// relaunch, whose `agent` names that. The Commit compares the process it
/// names: re-read with its conversation followed it is no change, and
/// another process in its place refuses the Commit as activity, its exit
/// decided by the outgoing instance. The topology does not change either
/// way.
#[cfg(unix)]
#[test]
fn the_handoff_leaf_names_the_supervised_agent_where_nothing_is_owed() {
    use crate::app_update_handoff::{commit_agent_carry, commit_layout_topology};
    use aterm_agent::harness::relaunch::Snapshot;
    let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
        std::sync::Arc::new(|_, _, _| "wait:shell-prompt".to_string()),
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
    // Its conversation followed: the same process, no change at the Commit.
    crate::harness_host::publish_snapshot_for_test(
        &sid,
        Some(&Snapshot {
            session: Some("11111111-2222-4333-8444-555555555555".to_string()),
            ..supervised.clone()
        }),
    );
    let live = app.capture_handoff_layout();
    assert_eq!(commit_agent_carry(&live), commit_agent_carry(&pending));
    // Another process in its place: refused as activity.
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
    // A tab owed a relaunch names that, and no supervised agent.
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

/// A DURABLE LAYOUT NEVER CARRIES THE EXPLICIT RESTART'S CAUSE (round
/// eighteen, the review of its first cut): the act's cause on a relaunch a
/// tab is owed rides the seamless update's handoff layout alone, whose
/// successor adopts the very tab (its sid kept) and relaunches it as the
/// act's. The layouts an end nobody chose leaves the next launch — the
/// crash journal's image and the quit layout (`App::fill_desk_agents`) —
/// name the owed agent with none, and a cold restore of one hands its queue
/// the agent as aterm's own end. Carried there, the next launch relaunched
/// it on the act's cause (`relaunch::after_exit`) in a REOPENED tab, whose
/// sid is new: a relaunch the act left in flight for the old tab then read
/// as the conversation's in another tab (`wait:conversation-in-other-tab`),
/// a counted wait — eight, and the tab said not to have come back — where
/// aterm's own end (`relaunch::after_host_ended`) closes that record as
/// `host-ended` and relaunches the agent (aterm-agent's
/// `an_agent_whose_host_ended_is_relaunched_in_its_reopened_tab` shows the
/// two steps apart). POSITIVE CONTROL: the handoff layout's leaf names it.
///
/// FAILS ON 95d62f656: the durable leaf read `cause = "asked"` (the one
/// reader, `App::pending_restored_agent`, served both layouts), and the cold
/// restore handed its queue `Some(Asked)`.
#[cfg(unix)]
#[test]
fn a_durable_layout_never_carries_the_explicit_restarts_cause() {
    use aterm_agent::harness::restart_stamp::Cause;
    let host = crate::harness_host::HostHandle::for_restored_test_with_open_tabs(
        std::sync::Arc::new(|_, _, _| "wait:shell-prompt".to_string()),
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
    // The crash journal's image and the quit layout: the owed agent named,
    // through the wire, with no cause.
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
    // Its cold restore hands the queue aterm's own end.
    let mut cold = App::headless_for_test();
    cold.restore_into_window(WindowId(0), window_of(vec![leaf_hosting(8, Some(named))]));
    for (&id, pooled) in cold.pool.sessions.iter_mut() {
        pooled.session.pid = 40_000 + i32::try_from(id).unwrap();
    }
    let handed = cold.take_restored_agents();
    assert_eq!(
        handed
            .iter()
            .map(|(_, _, asked)| *asked)
            .collect::<Vec<_>>(),
        vec![None],
        "a cold restore relaunches it as aterm's own end"
    );
    // POSITIVE CONTROL: the handoff layout's leaf names the cause.
    assert!(app.park_all_readers(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN));
    let handoff = app.capture_handoff_layout();
    app.rollback_overlap(None, &[]);
    assert_eq!(
        owed_agent(&handoff).and_then(|agent| agent.asked()),
        Some(Cause::Asked),
        "the handoff layout carries it"
    );
    host.shutdown_and_join();
}

/// LC-8 (ATERM-KI-1015-1, the 2026-10-15 sweep against Claude Code 2.1.300):
/// A TAB ITS OWNER HELD STAYS THE HOLDER'S ACROSS ATERM'S END. `hold` is a
/// stop: nothing is typed into the tab while it stands, and an exit read
/// under it is the holder's (`relaunch::on_exit`'s `Held`). The layouts an
/// end nobody chose leaves the next launch — the crash journal's image and a
/// restart's quit layout (`App::fill_desk_agents`) — carried the tab's agent
/// and no hold, so the next launch reopened the tab unheld, relaunched the
/// agent and typed the continuation into it. Through the wire and a cold
/// restore: the agent of a tab held by its OWNER (`origin=local`) is not
/// handed to the host, the reopened row does not promise it, and the owner's
/// hold is on the reopened tab; a FLEET hold is not carried (holds of that
/// origin are the process's, and quitting is the one recovery from a stuck
/// one: `fabric.rs`'s header), but its agent is not handed either.
/// NEGATIVE CONTROL: an unheld tab's agent is handed, promised, and its tab
/// is not held.
#[cfg(unix)]
#[test]
fn a_held_tabs_agent_stays_the_holders_across_a_cold_restore() {
    use crate::fabric::Hold;
    // What a cold restore of the durable layout of a tab running an agent,
    // under `hold`, does: the places the reopened row promises the agent
    // back, how many agents it hands the host, and the holds on its tabs.
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
    // NEGATIVE CONTROL: an unheld tab's agent comes back as ever.
    let (promised, handed, holds) = cold_restore_of(None);
    assert_eq!(
        (promised, handed, holds),
        (vec![(0, 0)], 1, Vec::new()),
        "(promised, handed, holds) of an unheld tab"
    );
}

/// LC-8, SAID (ruling 23): the restore pass records, for each tab that was
/// held when aterm ended, that its agent was not relaunched — a RECORD, the
/// person held the tab — naming the conversation to pick up and whose hold
/// it was; the owner's hold is on the reopened tab, `carried=1` on its
/// timeline, a fleet's is not. Once, and once for an agent two leaves name
/// (a session shown in two tabs). NEGATIVE CONTROL: an unheld tab's agent is
/// owed its relaunch and nothing is said of it.
#[test]
fn a_held_tabs_agent_left_at_its_shell_is_said_on_record() {
    use crate::fabric::{Hold, render_hold};
    // A leaf naming shell `shell` whose tab ran agent `pid`, under `hold`.
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
    // A host that relaunches restored agents: the hold is what kept each
    // held tab's down.
    let host =
        crate::harness_host::HostHandle::for_restored_test(std::sync::Arc::new(|_, _, _| {
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
            // The owner's held session shown in a second tab: said once.
            held_leaf(7, 4242, Some(owners.clone())),
        ]),
    );
    assert_eq!(
        cold.restored_held
            .iter()
            .map(|(_, agent, fleet)| (agent.pid, *fleet))
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
    let (_, agent, fleet) = cold.restored_held[0].clone();
    assert!(cold.store.read().unwrap().by_local(u64::MAX).is_none());
    cold.restored_held.insert(0, (u64::MAX, agent, fleet));
    let records = cold.messages.log().records().count();
    cold.say_restored_held();
    let said = held_records(&cold, records);
    assert_eq!(
        said.len(),
        1,
        "the surviving leaf's held agent is reported: {said:?}"
    );
}

/// The records the restore pass said of held tabs ([`App::say_restored_held`])
/// since the log held `from`: each one's title and detail lines.
fn held_records(app: &App, from: usize) -> Vec<(String, Vec<String>)> {
    app.messages
        .log()
        .records()
        .skip(from)
        .map(|record| (record.title.clone(), record.detail.clone()))
        .collect()
}

/// The `aterm ctl` command a record's line names, through its `off` or
/// `on` (`@s-1 hold off`, `hold s-1 off`), its quoting stripped.
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

/// THE REVIEW OF ROUND TWENTY'S LC-8 (its major): THE LIFT A HELD TAB'S
/// RECORD NAMES IS ONE THE CONTROL SOCKET TAKES. The first cut's record (and
/// ruling 23, and the KI entry) said `aterm ctl @<sid> hold off`; `hold`
/// names its session as its argument and refuses a selector (`ERR denied`,
/// `control::dispatch_hold_verb`), so a person who followed the record kept
/// the tab held (live: `ERR denied`, `hold=1` after it). The command is read
/// off the record the restore pass says and run as the Owner — the scope
/// every in-session client holds — through control's `hold` path: it lifts
/// the owner's carried hold.
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

/// THE REVIEW OF ROUND TWENTY'S LC-8: A HELD TAB'S RECORD IS ITS OWN
/// AGENT'S. The first cut said "Claude was not restarted" and `type claude
/// --resume <id>` whatever had run: a Codex's record named Claude and put the
/// Codex thread under Claude's resume, and a Claude Code that ran under
/// another home (`CLAUDE_CONFIG_DIR`, which its relaunch carries) was told a
/// bare line that does not find its conversation. And it gave the hold as
/// the reason where the cold restore would not have relaunched the agent
/// anyway (no host: the harness off). Now: under a host that relaunches
/// restored agents, each record names its program and the line that
/// resumes it, its home on it; with none, a held tab's record says the hold
/// came back and no reason the agent stayed down.
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
        codex: Some(restore::CodexRestore {
            home: "/opt/agents/codex".into(),
            home_env: None,
            embedded: true,
            started_ms: 1,
        }),
        ..dead_agent()
    };
    let host =
        crate::harness_host::HostHandle::for_restored_test(std::sync::Arc::new(|_, _, _| {
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
        codex_lines
            .iter()
            .any(|line| line.contains(&format!("codex resume {THREAD}")))
            && !codex_lines.iter().any(|line| line.contains("claude")),
        "Codex's own resume: {codex_lines:?}"
    );
    host.shutdown_and_join();

    // NO HOST (the harness off): nothing would have relaunched the agent, so
    // the hold is no reason given for it; the owner's hold came back, said.
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

/// A SHELL THE PTY KEEPER GAVE BACK KEEPS ITS OWN HOLDS; THE LAYOUT ADDS
/// THE OWNER'S WHERE THEY HAVE NONE. Since `5287b96eb` (and both slots since
/// `6b3367748`) the keeper's META carries the session's holds, both origins,
/// and the recovered shell is held again before it is registered
/// (`spawn::seed_adopted_fabric`). The crash journal's leaf names the holds
/// too, and this branch's first cut seeded the leaf's hold on that shell
/// again: an owner's or a fleet's hold carried twice (two `carried=1` rows),
/// and a fleet hold the bridge lifted after the journal's last image put
/// back — a fleet slot the owner may not lift. Now the fleet's slot is the
/// META's alone, and the owner's from the leaf is added where the META's
/// carry left it empty, fail-closed: neither sample can be ordered before the
/// other (the journal captures at most every 2 s; the META is the last one
/// delivered to the keeper, which contention or a failed link leaves older).
/// A slot that stands takes nothing and records nothing. NEGATIVE CONTROLS:
/// a fresh shell takes the leaf's owner's hold alone. (The headless restore's
/// stub adoption seeds its handle's fabric as `spawn_session` does, so the
/// keeper's carry is the production one.)
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
    // A desk whose shell 8 ran an agent under the leaf's holds.
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
    // Restore `layout(leaf)`, shell 8 given back by the keeper with the holds
    // its META carried (`Some`), or fresh (`None`): each held session's
    // outgoing id (`Some(8)`: the kept shell), its holds, and how many holds
    // its timeline says were carried.
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
    // NEGATIVE CONTROLS: a fresh shell takes the leaf's owner's hold alone.
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

/// REVIEW (separation lens, 2026-09-17): ON A COLD RESTORE, THE WINDOW'S
/// FIRST LEAF KEEPS ITS IDENTITY. The window's bootstrap shell is grafted
/// onto its first terminal leaf, and the graft carried only the USER meta:
/// a tab that was `identity=worker` in window 0's first leaf came back as
/// `identity=-`, its bootstrap forked under the human's login — measured
/// `[(None, None), (Some("worker"), Some("worker"))]` for two leaves both
/// naming `worker`. The agent identity is the SHELL's: the graft now
/// takes the bootstrap only when it wears what the pane names
/// (`main_entry` forks it under `first_leaf_identity` so it does), a
/// bootstrap wearing something else fills a later pane that names its
/// identity or is retired, and the pane forks its own shell under the
/// identity — in every case each pane's shell wears what its leaf names.
#[test]
fn a_cold_restore_keeps_the_identity_of_the_windows_first_leaf() {
    let state = std::env::temp_dir().join(format!(
        "aterm-restore-first-leaf-identity-{}-{}",
        std::process::id(),
        aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&state).unwrap();
    // `(local id, the session's label, the registry handle's label)`, by id.
    let rows = |app: &App| -> Vec<(u64, Option<String>, Option<String>)> {
        let store = app.store.read().unwrap();
        let mut rows: Vec<_> = app
            .pool
            .sessions
            .iter()
            .map(|(&session, pooled)| {
                (
                    session,
                    pooled.session.identity.clone(),
                    store
                        .by_local(session)
                        .and_then(|h| h.identity.as_deref().map(str::to_owned)),
                )
            })
            .collect();
        rows.sort_unstable();
        rows
    };
    let worker = || Some("worker".to_string());
    crate::test_env::scoped("ATERM_STATE_HOME", &state, || {
        crate::agent_identity::ensure("worker", true).expect("the verb's create");
        // The reviewer's shape: both leaves name `worker`; the bootstrap (a
        // stub, the human's own) wears none. Neither pane takes it — each
        // forks its own shell under `worker` — and it is retired unconsumed.
        let mut cold = App::headless_for_test();
        cold.restore_into_window(
            WindowId(0),
            window_of(vec![
                leaf_under(7, Some("worker")),
                leaf_under(8, Some("worker")),
            ]),
        );
        assert_eq!(
            rows(&cold),
            vec![(1, worker(), worker()), (2, worker(), worker())],
            "both panes under `worker`; the bootstrap that wore none is gone"
        );
        // A later pane names what the bootstrap wears: it fills THAT pane.
        let mut cold = App::headless_for_test();
        cold.restore_into_window(
            WindowId(0),
            window_of(vec![leaf_under(7, Some("worker")), leaf_under(8, None)]),
        );
        assert_eq!(
            rows(&cold),
            vec![(0, None, None), (1, worker(), worker())],
            "the bootstrap (session 0) fills the second pane; the first forked its own"
        );
        // A bootstrap already under `worker` — what `main_entry` forks for
        // this layout, by `first_leaf_identity` — is grafted: no extra fork.
        let mut cold = App::headless_for_test();
        cold.pool
            .sessions
            .get_mut(&0)
            .expect("the headless bootstrap")
            .session
            .identity = worker();
        cold.restore_into_window(
            WindowId(0),
            window_of(vec![
                leaf_under(7, Some("worker")),
                leaf_under(8, Some("worker")),
            ]),
        );
        let mut ids: Vec<u64> = cold.pool.sessions.keys().copied().collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![0, 1],
            "session 0 was grafted, one fork for the second pane"
        );
        assert!(
            cold.pool
                .sessions
                .values()
                .all(|p| p.session.identity == worker()),
            "every pane's shell wears `worker`"
        );
        // A forgotten identity is a default shell on both sides: the leaf
        // naming `ghost` takes the bootstrap that wears none.
        let mut cold = App::headless_for_test();
        cold.restore_into_window(
            WindowId(0),
            window_of(vec![
                leaf_under(7, Some("ghost")),
                leaf_under(8, Some("worker")),
            ]),
        );
        assert_eq!(rows(&cold), vec![(0, None, None), (1, worker(), worker())]);
        assert!(!state.join("identities").join("ghost").exists());
    });
    let _ = std::fs::remove_dir_all(&state);
}

/// THE BOOTSTRAP IS FORKED UNDER THE IDENTITY OF THE PANE IT FILLS. The
/// pick is `bootstrap_local_id`'s — the canonical tree's first terminal
/// leaf in rebuild order, behind native and placeholder leaves — so
/// `main_entry` (`first_leaf_identity`) and `apply_restore_manifest`
/// (`bootstrap_identity`) fork each window's bootstrap under exactly the
/// identity the graft will look for. The legacy mirror names none.
#[test]
fn the_bootstrap_identity_is_the_identity_of_the_leaf_the_bootstrap_fills() {
    let layout = window_of(vec![
        settings_leaf(),
        split_of(placeholder_leaf(), leaf_under(4, Some("worker"))),
        leaf_under(5, Some("reviewer")),
    ]);
    assert_eq!(
        (layout.bootstrap_local_id(), layout.bootstrap_identity()),
        (Some(4), Some("worker")),
        "the same leaf, behind the settings tab and the placeholder"
    );
    let mut manifest = App::headless_for_test().capture_restore_manifest();
    manifest.windows = vec![layout];
    assert_eq!(manifest.first_leaf_identity(), Some("worker"));
    // A pane naming no identity, and a legacy (mirror-only) layout — a
    // capture with its canonical tabs dropped, so only the `tabs` mirror
    // names the bootstrap's pane: none.
    let plain = window_of(vec![leaf_naming(0)]);
    assert_eq!(plain.bootstrap_identity(), None);
    let mut legacy = App::headless_for_test()
        .capture_restore_manifest()
        .windows
        .remove(0);
    legacy.restored_tabs.clear();
    assert_eq!(
        (legacy.bootstrap_local_id(), legacy.bootstrap_identity()),
        (Some(0), None)
    );
}

/// THE ENTRY BUILT BEFORE THE ADOPTION NAMES THE FROZEN TAB (review,
/// 2026-09-16). The atpkg launch pass starts from `main_entry` before the
/// event loop, and on a warm machine prints `managed-current:` within tens
/// of milliseconds — before the first park after first paint adopts the
/// handed-off shells (`apply_pending_restore`). Counted from the registry
/// alone, those words claimed every tab, the very one the owner had
/// complained about. `App::frozen_path_tabs` counts the pending adoptees, so
/// the marker that arrives first is worded right. Since 2026-09-22 the words
/// are a `Hold::LogOnly` record and never a row, and with a frozen tab the
/// record's detail is the remedy alone ("1 tab from before this update
/// picks them up with …"): the restore's refresh that re-posted the row
/// when the count moved (`refresh_managed_current`) is gone with it, and
/// the count simply reads the registry once the shells are adopted.
#[test]
fn a_managed_entry_built_before_the_adoption_counts_the_frozen_shell_and_takes_no_row() {
    let wire = "claude 2.1.273 (build 2026091601); codex 0.154.0 (build 2026091001)";
    let managed = |app: &App| {
        app.messages
            .log()
            .records()
            .rfind(|r| r.key.as_deref() == Some(crate::toolchain_words::KEY_MANAGED))
            .map(|r| r.detail[0].clone())
    };
    let mut new = App::headless_for_test();
    new.handoff_successor = true;
    let mut frozen = handed_off_shell(3);
    frozen.frozen_path = true;
    new.seamless_adopt = vec![frozen, handed_off_shell(4)];
    // Before the adoption: the registry knows nothing, the count is 1.
    assert_eq!(new.store.read().unwrap().frozen_path_tabs(), 0);
    assert_eq!(new.frozen_path_tabs(), 1, "the pending adoptee is counted");
    // The marker lands now — what `Wake::PkgManagedCurrent` does.
    new.post_managed_current(wire);
    assert_eq!(new.messages.live_rows().count(), 0, "no row");
    let detail = managed(&new).expect("the record");
    assert_eq!(
        detail,
        "1 tab from before this update picks them up with \
         `. ~/.aterm/shell.d/00-atpkg.zsh`",
        "the remedy, never a claim on every tab"
    );
    // The adoption: the leaf places shell 3, the net places shell 4.
    new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0), leaf_naming(3)]));
    new.adopt_orphan_shells_as_tabs(&[]);
    assert!(new.seamless_adopt.is_empty());
    assert_eq!(new.store.read().unwrap().frozen_path_tabs(), 1);
    assert_eq!(new.frozen_path_tabs(), 1, "registered now, counted once");
}

/// CONFORMANCE (Tier-1) of the REAL incoming side to
/// `aterm_spec::derive::fd_handoff_no_leak_model` (`FdHandoffNoLeak`,
/// proof-carrying DSU Rung 1b): every PTY master handed across a seamless
/// re-exec ends ADOPTED — owned by a session — or CLOSED, never dropped while
/// still open (a live, readerless channel whose shell never sees its hangup).
///
/// Driven through the orphan net, `App::adopt_orphan_shells_as_tabs` — the
/// placement every shell no restored leaf claimed goes through — once with a
/// front window to adopt into and once with none. The headless harness has
/// no event loop for `spawn_session`, so its stand-in builds the session, but
/// the master reaches that session's sink through the shipping
/// `HandedMaster::into_sink`, the one adoption both share. Each handed master
/// is one end of a socketpair and its peer is the witness: EOF there means
/// every copy of the master is closed; a pooled session carrying the shell's
/// `local_id` whose sink owns a master means it was adopted. Per master that
/// is the model's `Adopt`, its `CloseFallback`, or — neither — its
/// `BuggyDrop`, and every step is validated against the committed model
/// after the `Prepare`s that hand the masters across.
///
/// The other ways a handed master can end: a refused handoff —
/// `seamless::tests::a_refused_handoff_closes_every_master_it_was_handed` —
/// and a spawn that fails after taking the `Adopted`, which needs the event
/// loop no headless test has and is closed by ownership instead: the error
/// drops either the `HandedMaster` not yet adopted or the sink that adopted
/// it, and both close the master.
///
/// NEGATIVE CONTROLS: a master left open with no owner is the `BuggyDrop`
/// step, which only `Buggy = 1` admits. Until `spawn::HandedMaster` owned the
/// descriptor that is exactly what the windowless branch did ("their raw fds
/// close with the process"); make `HandedMaster`'s `Drop` a no-op again and
/// this test fails there. And a master the adoption closes as it hands it on
/// is a pooled session over a closed master, which this test refuses: let
/// `into_sink` drop its handle (`self.0` for the `ManuallyDrop`) and the
/// front-window case fails — in a debug build as an abort ("IO Safety
/// violation"), when that session's sink closes the descriptor a second time.
#[test]
#[cfg(unix)]
fn handed_off_masters_are_adopted_or_closed_never_leaked() {
    use aterm_spec::verify::validate_transition_tiered;
    use std::io::Read;
    use std::os::fd::IntoRawFd;
    use std::os::unix::net::UnixStream;

    let model = aterm_spec::derive::fd_handoff_no_leak_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let closed = |peer: &UnixStream| {
        let mut byte = [0u8; 1];
        match (&*peer).read(&mut byte) {
            Ok(0) => true,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
            other => panic!("unexpected witness read: {other:?}"),
        }
    };
    // A master the net closed reads EOF at its peer only once EVERY copy is
    // closed, and a child another test in this binary is forking holds one
    // until it execs (a socketpair end is made close-on-exec non-atomically
    // on macOS, so it can even ride through the exec). So a master that is
    // not in the pool is given a hang detector's minute to read closed before
    // it is classified; a leak — `HandedMaster`'s `Drop` a no-op, the negative
    // control above — never closes and still reads as the `BuggyDrop` step, a
    // minute later (the fd-copy sweep of 2026-09-27). An adopted master is
    // read at once: a live master must never be waited into reading closed.
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
        // A leak from here is a step only the mutant can take.
        assert!(!model.action_enabled("BuggyDrop", &state));
        assert!(buggy.action_enabled("BuggyDrop", &state));
        let (mut adopted, mut closed_count) = (0, 0);
        for (local_id, peer) in &peers {
            // Adopted = a pooled session carries the shell AND its sink owns
            // a master, as a real adoption's does.
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

/// A shell the outgoing process handed across as `local_id`, the way
/// `seamless::take_incoming` gives it to `main_entry`. A headless restore
/// reads only the id: the `-1` sentinel master is no descriptor, so the stub
/// spawn owns none, and the pid is never signalled.
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
        identity: None,
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
        log_carry: None,
        journal_key: None,
    }
}

/// A shell the PTY KEEPER kept for window `owner`, which knew it as
/// `local_id`: [`handed_off_shell`] with the keeper's release obligation,
/// its device numbered `owner * 100 + local_id` so a test can say whose
/// shell a pane took.
fn recovered_shell(owner: u32, local_id: u64) -> crate::spawn::Adopted {
    crate::spawn::Adopted {
        keeper: Some(crate::keeper_link::Claim::new(
            u64::from(owner) * 100 + local_id,
            Some(owner),
        )),
        ..handed_off_shell(local_id)
    }
}

/// Whose recovered shell `shell` is: its window and its device.
fn whose(shell: &crate::spawn::Adopted) -> Option<(Option<u32>, u64)> {
    shell
        .keeper
        .as_ref()
        .map(|claim| (claim.owner(), claim.rdev()))
}

/// ROUND SEVEN, FINDING 5'S SECOND REVIEW — A TAKEN-OVER IMAGE'S PANES GET
/// THE SHELLS THE SUCCESSOR RE-TAGGED. The parent's desk: pane 0 (`/a`) and
/// pane 1 (`/b`, where an agent ran), the parent's pool ids 0 and 1. Its
/// shell 0 ended during the update; the successor adopted shell 1 as ITS
/// session 0 (its counter starts over), and after Commit started a fresh
/// session 1. At Commit it took the parent's journal image over, and
/// `keeper_link::activate` sent its registrations — through the real keeper
/// core, which keeps only the latest tag: shell 1 is now (successor, 0).
/// Then the successor died before its first image. The next launch's claim
/// reopens the parent's image (leaves keyed by the parent), and the
/// placement puts the parent's shell back in pane 1 — the agent's pane is
/// filled by its live shell, so the agent is not relaunched beside itself —
/// while the successor's fresh session goes to the orphan net, never into
/// the parent's pane 1 (the collision the first review fixed). And with
/// the crash BEFORE the link delivered the registrations (the keeper still
/// holding the parent's tag), the same pane takes the same shell.
/// NEGATIVE CONTROL: without the registration's `handed` pair, the re-tagged
/// shell matches no pane and pane 1 starts fresh (the review's failure).
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
    const SHELL: u64 = 11; // the parent's shell 1, adopted
    const FRESH: u64 = 12; // the successor's own session 1
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

    // The successor's session 0 is the parent's shell 1.
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

    // The keeper as the next launch finds it: the parent registered its
    // shell 1; then (`activated`) the successor's Commit re-tagged it and
    // registered its fresh session.
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

    // The take-over at Commit, and the successor's death before its first
    // image; the next launch's claim reopens the parent's image.
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

/// ROUND SEVEN'S LEFTOVER I2b — TWO GENERATIONS ON, A KEPT SHELL STILL
/// FINDS ITS PANE. The chain G → P → S: G's image keys pane 1 (`/b`) by
/// G's shell 1. P adopted that shell as its session 0 and took G's image
/// over at its Commit, but never replaced it (its first image failed to
/// write and its desk did not change). S adopted P's session 0 as its own
/// session 0, took P's journal — still G's image — over, registered the
/// shell with the keeper, and died before its own first image. S's
/// registration names S's pair and P's (`handed`); neither is on any of
/// G's leaves. So P's handoff record says the pair G's image keys the
/// shell by (`App::journal_keys` → `SessionRecord::journal_key`), S's
/// registration names it too (`Tag::keyed`), and the placement puts the
/// shell back in pane 1 instead of a bare orphan tab beside a relaunched
/// agent. NEGATIVE CONTROL: without the key, pane 1 takes no shell — the
/// one-generation limit the round-seven fixer disclosed.
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

    // P: G's shell 1 is its session 0; G's image is its journal, unreplaced.
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
    // Across the manifest's own wire.
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
    drop(parent_app); // P's `_exit` after its Commit
    cj::await_released(&dir.path().join(parent.lock_name()));

    // S: P's session 0 is its session 0, adopted with the carried key.
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
    drop(lane); // S dies before its own first image
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

/// An update's successor that took its parent's desk over at Commit
/// (round eight of the update audit, findings 46 and 76). The parent's
/// image: pane 0 (`/a`) and pane 1 (`/b`, Claude on its conversation),
/// the parent's pool ids 0 and 1. The successor adopted the parent's
/// shell 1 as ITS session 0 and shell 0 as its session 1 (so a carry keyed
/// by the wrong id space shows), its Commit waiter took the image over,
/// and its wake installed it. Its host has published no agent yet (no
/// worker attached: 45 s after an update from a build that writes no
/// claim) and its engines answer no folder (busy, or no OSC 7 yet).
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
            codex: None,
            cause: None,
        };
        desk.fill_agents(&|id| (id == 1).then(|| claude.clone()));
        let owner = JournalOwner::create(dir.path(), parent).expect("armed");
        owner
            .publish(&desk, &JournalHeader::now(parent.pid, false))
            .expect("published");
        cj::die(owner); // the hand-off: a clean end that leaves its image

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
        cj::marker(logs.path(), own); // the successor's run, armed at its start
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

    /// The successor's first capture after Commit, written.
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

    /// The successor is killed; the next launch claims what it left.
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

/// ROUND EIGHT OF THE UPDATE AUDIT, FINDING 76 — THE SUCCESSOR'S FIRST
/// IMAGE KEEPS WHAT IT TOOK OVER UNTIL IT KNOWS BETTER. Its first capture
/// after Commit replaced the taken-over image with one built only of its
/// own state: no agent (its host had published none) and no folder or
/// title (its engines answered none). Killed then, the next launch reopened
/// the desk in `$HOME` and relaunched no conversation. The image now
/// carries the parent's record and folders forward, keyed by the parent's
/// ids the sessions were handed under. NEGATIVE CONTROLS: the same desk
/// killed before the first image (the take-over alone) reopens the same;
/// and once the successor's own status read says the shell holds the
/// agent's tab, its image carries no agent there.
///
/// AND SO DOES A RESTART'S QUIT LAYOUT (the review of restore audit round
/// two, 2026-10-03), filled as `main_entry` fills it
/// ([`App::fill_desk_agents`]): that quit retires the journal that
/// carried the agent, so its layout is the one record left.
///
/// RED before the fix: `resumed` was empty and every folder was `None`;
/// before the review, the restart's quit layout named no agent.
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
    // A restart now, before the successor's host published the agent:
    // the quit's layout names it as the image does, and tab 1 nothing.
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

    // NEGATIVE CONTROL: this run's own status read says the shell holds
    // the agent's tab now — the agent is gone, and so is its record.
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

/// ROUND EIGHT OF THE UPDATE AUDIT, FINDING 46 — A KEPT SHELL FILLS ITS
/// PANE IN THE SUCCESSOR'S OWN IMAGE WHILE THE KEEPER STILL HOLDS IT UNDER
/// THE PARENT. The successor published its own image (its pool ids) and
/// died before its re-registration reached the keeper (or its link never
/// started), so the keeper offers each shell under the parent's pair,
/// which no leaf of that image carries: every kept shell became an
/// unnamed orphan tab, and the agent was relaunched beside its
/// still-running original. The image names the pairs its leaves were
/// handed as, and each shell takes its own pane. NEGATIVE CONTROL: placed
/// without those pairs (the image before the fix), both shells are the
/// orphan net's.
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
        // The keeper's records, as the parent registered them.
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

/// P3 REVIEW — WHOSE SHELL FILLS WHICH PANE. Two windows registered with
/// one keeper both died; each minted its pool ids from 0, so the next
/// launch is offered shells `{0, 1}` of window A and `{0, 1}` of window
/// B, and the reopened layout is both crash journals' windows. Every pane
/// takes ITS OWN window's shell — session 0 included — through the same
/// picks `main_entry` and the restore make. A layout whose writer is
/// unknown (a clean quit's `session.toml`, written by a window that left
/// the keeper nothing) takes none, and neither does a journal of another
/// window: those shells go to the orphan net, carrying no leaf's id.
/// NEGATIVE CONTROL: without the placement, window A's first pane takes
/// B's shell 0 when the keeper offers B's first.
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
    // The keeper offers in its own order; each case lists the window
    // whose shells a pane must NOT take first.
    let offered = |first: u32, then: u32| {
        vec![
            recovered_shell(first, 0),
            recovered_shell(first, 1),
            recovered_shell(then, 0),
            recovered_shell(then, 1),
        ]
    };
    let device = |owner: u32, local: u64| u64::from(owner) * 100 + local;

    // Both crash journals: every leaf takes its own window's shell.
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

    // Only B's journal was reopened (A's was set aside): B's panes take
    // B's shells, and A's are left for the orphan net.
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

    // A clean quit's `session.toml` (no writer named): no pane takes a
    // recovered shell, one with no owner included, and none of them
    // carries an id a leaf names, so the orphan net places them bare.
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

/// P3 REVIEW — AN OFFERED MASTER THAT BECOMES NO SESSION IS RELEASED. The
/// keeper holds its own copy of every master it offered; when this window
/// drops its copy without registering a session — the orphan net with no
/// window to land in, here — the keeper must be told, or it keeps the
/// master (the shell never hung up, running unseen) and offers it again
/// to the next launch. A shell the net DOES place is registered, and is
/// not released (its release is its session's close).
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

/// The six USER fields handed-off shell `shell` wore in the predecessor.
/// Every free-text value names its shell, so a value on the wrong session
/// says whose it was; `questions` is a closed set of words, so it cycles
/// with the shell id and the other five carry the name.
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

/// The terminal leaf a capture writes for handed-off shell `shell`,
/// carrying the identity [`identity_of`] gives that shell.
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

/// A terminal leaf naming shell `shell`, spawned under agent identity
/// `identity`.
fn leaf_under(shell: u64, identity: Option<&str>) -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
        restore::TerminalLeafRestore {
            local_id: Some(shell),
            identity: identity.map(str::to_owned),
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

/// The recovery placeholder a markdown document that could not be
/// reopened leaves in its pane.
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

/// A window of one tab per root, each focused on its first pane (a
/// focus path must name a leaf, or the parse refuses the tab).
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

/// `main_entry`'s pick for session 0 before `take_session0_shell`: the shell
/// window 0's bootstrap leaf names or, with no such leaf, the first shell the
/// handoff lists (`SessionHandoff::from_store` lists them in local-id order).
fn first_listed_session0_shell(
    handed_off: &mut Vec<crate::spawn::Adopted>,
    layout: &restore::RestoreManifest,
) -> Option<crate::spawn::Adopted> {
    super::take_handed_off_shell(handed_off, layout.windows[0].bootstrap_local_id())
        .or_else(|| Some(handed_off.remove(0)))
}

/// A handoff SUCCESSOR rebuilt from `windows` the way the child runs. The
/// layout crosses the wire (`to_toml`, then `from_toml` plus
/// `covers_exact_seamless_ids`, as `take_incoming` accepts it) with shells
/// `0..shells` handed off; session 0 adopts by `session0`'s pick; window 0
/// is rebuilt around it, and every further window is created around the
/// shell its bootstrap leaf names — `apply_restore_manifest`'s pick — or a
/// fresh one when that shell is taken. The restore itself runs unchanged,
/// and must place every shell: this harness leaves the orphan net nothing.
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

/// What every leaf of every window shows, tab by tab: the handed-off
/// shell a terminal pane runs (`Some(None)` for a shell nobody handed
/// off), `None` for a native view.
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

/// Every tab focused on a handed-off shell shows that shell's icon
/// ([`identity_of`]) at the head of its tooltip.
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

/// EVERY SHELL COMES BACK IN THE PANE ITS LEAF NAMES, WEARING THAT LEAF'S
/// IDENTITY, AND NO STAND-IN IS SPAWNED. When window 0 has no terminal leaf,
/// `main_entry` once adopted the first shell the handoff listed as session 0
/// anyway. That shell's own leaf lies in a later window, whose rebuild found
/// it taken and spawned a FRESH shell in its pane — a stand-in, with a new
/// sid and no history — while the shell itself stayed in window 0 as an
/// extra tab. The stand-in was also seeded with the leaf's identity
/// (`role=` included, which peers read before typing) while the shell that
/// identity describes came back bare; when the leaf was its window's first
/// pane, the graft refused the stand-in and the identity reached nobody.
/// `take_session0_shell` now adopts nothing for such a window 0: session 0
/// is a fresh bootstrap its native-only rebuild retires, and each shell is
/// adopted by its own leaf. The third shape — a native first tab with the
/// terminal leaf later in the same window — was always right; it is the
/// control.
///
/// HOW IT FAILS WITHOUT THE FIXES, as measured. With the old pick
/// ([`first_listed_session0_shell`]) in place of `take_session0_shell`, the
/// first two shapes fail the placement check: shell 0 is an extra tab of
/// window 0 and a stand-in fills its pane
/// (`a_handoff_keeps_each_identity_on_its_shell_when_a_leaf_names_a_shell_running_elsewhere`
/// keeps those shapes, to hold the identity rule there). Further back —
/// against the code before the identity fix, whose headless stub never
/// adopted from `seamless_adopt` — this test's first form fails at its
/// PRECONDITION that the layout placed every handed-off shell, not at an
/// identity check, since every stub stands in for its shell (as the review
/// of that fix measured). The identity failures described above were
/// measured with the stub adopting and the old identity rule.
/// ROUND SIX, FINDING 10: A SESSION OPEN IN TWO WINDOWS KEEPS THE WHOLE
/// LAYOUT, AND BOTH VIEWS. `Open Session in New Window` (Cmd-Shift-O)
/// shows one pooled session in a second window, and the handoff capture
/// stamps its id on both leaves. The successor refused the repeat as a
/// malformed layout, so every update taken while the co-view was open
/// folded every window into tabs of the first. Now the layout is placed,
/// the shell is adopted once, and the co-viewing window shows it again —
/// no stand-in shell is spawned for it.
#[cfg(unix)]
#[test]
fn a_co_viewed_session_keeps_the_handoff_layout_and_both_of_its_views() {
    // The outgoing process: window 0 runs shell 0, window 1 runs shell 1,
    // and window 1's shell is opened in a new window (window 2).
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

    // The successor places it: shell 1 is adopted once, shown twice.
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

/// The handed set of `old`, sorted: what its handoff carries.
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

/// `old`'s handoff layout as the successor takes it: the capture, its
/// wire, the producer's round trip (the update worker's own check,
/// `from_toml(to_toml(layout)) == layout`), and the placement against
/// the handed set (`seamless::take_incoming`'s).
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

/// ROUND EIGHT OF THE UPDATE AUDIT, D5 (#505, and the lead U33): A DESK
/// PAST THE WINDOW CAP FOLDS INTO THE HANDOFF LAYOUT. Nothing at runtime
/// limits how many windows a desk has, but a layout holds at most
/// `MAX_WINDOWS` (64) of them, and the producer applied no cap: the
/// capture of a 70-window desk wrote 70 windows, its own round trip read
/// back 64, and the worker filed every attempt as a transient
/// `ProducerFailed` that froze every tab again on each retry — the desk
/// never updated. Now the windows past the cap are tabs of the last
/// window that fits: the layout round-trips, names every handed shell,
/// and the successor places each one.
///
/// RED before the fold: the round trip read 64 of the 70 windows back.
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
    assert!(
        layout.to_toml().expect("serializes").contains("[fold]"),
        "on the wire"
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

/// D5: TABS FOLD WHOLE, NATIVE ONES TOO. A Settings tab of a window past
/// the cap joins the last window, beside that window's own Settings tab:
/// the successor reopens both views (a window may present the Settings
/// controller twice, as two windows do) and every shell around them.
#[cfg(unix)]
#[test]
fn a_folded_window_reopens_every_native_tab_it_received() {
    let mut windows = (0..63)
        .map(|shell| window_of(vec![leaf_naming(shell)]))
        .collect::<Vec<_>>();
    windows.push(window_of(vec![leaf_naming(63), settings_leaf()]));
    windows.push(window_of(vec![settings_leaf(), leaf_naming(64)]));
    let mut layout = restore::RestoreManifest::new(windows);
    layout.fold_into_caps();
    assert!(layout.admits_whole(), "PRECONDITION: within the caps");

    let new = successor_of(layout.windows, 65, |handed_off, layout| {
        super::take_session0_shell(handed_off, Some(layout))
    });
    let shown = shown_shells(&new);
    assert_eq!(shown.len(), restore::MAX_WINDOWS);
    assert_eq!(
        shown[restore::MAX_WINDOWS - 1],
        vec![
            vec![Some(Some(63))],
            vec![None],
            vec![None],
            vec![Some(Some(64))]
        ],
        "its own shell and Settings, then the folded window's Settings and shell"
    );
}

/// THE SUCCESSOR OF A FOLDED LAYOUT CAPTURES ONE EVERY BUILD READS WHOLE
/// (the D5 review). The fold put a second Settings tab and a second view
/// of the same document into the last window, and the successor reopens
/// them all. Its next captures — the next update's and the quit's —
/// wrote both copies into that window's legacy mirror, which every build's
/// parse refuses: the next update refused its own layout at the park, and
/// the quit layout and every crash journal left the window out, with every
/// tab the fold had put into it. Now the mirror leaves the second copy out
/// (`restore::NativeTabRestore::joins_mirror`), as it does a mixed split
/// tab, and the canonical tabs keep every tab.
///
/// RED before the fix: the next update's capture did not read back whole
/// ("window 64 of the handoff layout is in a shape this build does not
/// read back"), and the parse leaves that window out.
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
    // Each tab here is one pane: what it is.
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

/// D5's tab half: A WINDOW PAST THE TAB CAP SPILLS INTO ANOTHER WINDOW.
/// A window holds at most 256 tabs; a 300-tab window wrote a layout no
/// build reads whole. Its tabs past the cap move to the next window with
/// room — here a window made for them, since the desk has no other — and
/// every shell is placed.
///
/// RED before the fold: the round trip dropped the 300-tab window.
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
    // The front tab (the last one pushed) moved: the window made for the
    // spill shows it, and the donor its last remaining tab.
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

/// D5's record: the successor says ONCE, calmly, what the outgoing build
/// folded (`App::say_layout_folded`, which `apply_pending_restore` runs
/// once the layout's windows are up), and says nothing for a layout that
/// folded nothing — every desk within the caps.
#[test]
fn the_successor_says_once_what_the_update_folded() {
    let folded = |app: &App| {
        app.messages
            .live_rows()
            .filter(|row| row.msg.key.as_deref() == Some(crate::update_words::KEY_LAYOUT_FOLDED))
            .map(|row| row.msg.clone())
            .collect::<Vec<_>>()
    };
    let mut app = App::headless_for_test();
    app.say_layout_folded(&restore::LayoutFold::default());
    assert!(folded(&app).is_empty(), "nothing folded, nothing said");

    app.say_layout_folded(&restore::LayoutFold {
        windows: 6,
        ..restore::LayoutFold::default()
    });
    let rows = folded(&app);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].title,
        "6 windows were merged into tabs during the update"
    );
    assert_eq!(
        rows[0].severity,
        aterm_messages::Severity::Info,
        "a record, not a warning: every tab and shell came across"
    );
}

/// An owed adoption proof on `app`, as a successor holds one until it
/// writes it: the readiness pipe's write end. The read end comes back.
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

/// ROUND EIGHT, FINDING 58: A CO-VIEW WINDOW THAT LOSES ITS GLASS DROPS
/// ONLY ITS VIEWS — AND A SHELL IT ALONE SHOWED DEGRADES THE HANDOFF.
/// Since round six's finding 10 a co-view window is rebuilt by the native
/// arm, and its other tabs adopt handed shells there. When its glass will
/// not attach the arm closes the window, which drops a shell shown only in
/// it before the proof: the proof then named a subset, the parent filed
/// `AdoptionMismatch` — a STRUCTURAL verdict that latches the new build —
/// for what was this machine's GPU or surface trouble, which the terminal
/// arm answers by degrading. Here the arm's own response
/// (`close_window_logical`) runs on the co-view window of W0[shell 0],
/// W1[shell 1], W2[shell 0 again | shell 2]: the handoff is degraded, so
/// the proof is withheld and the parent's rollback resumes every shell.
/// NEGATIVE CONTROL: a co-view window showing only shell 0 again drops no
/// shell, and the handoff stands.
///
/// RED before the fix: `handoff_degraded` stayed false with shell 2 gone.
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

/// ROUND EIGHT, FINDING 59: A HELD PANE OPEN IN TWO WINDOWS COMES BACK IN
/// BOTH. A pane kept by `--hold` and opened in a new window is one session
/// in two views; the capture names it at both leaves, the successor turns
/// both into placeholders carrying it, and the held screen crosses once.
/// The first placeholder took the screen and the second, finding it gone,
/// said the pane was 'not carried' — and the next update carried that
/// placeholder on, so the second view was lost for good. Now the second
/// placeholder shows the session the first one restored, as a co-viewed
/// live shell's later leaf shows the adopted shell again.
///
/// RED before the fix: window 2 held a recovery placeholder, no terminal.
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
        // Where the layout puts each shell: the pane its leaf names.
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

    // With no layout to place by, session 0 still takes a shell.
    let mut handed_off = (0..3).map(handed_off_shell).collect::<Vec<_>>();
    assert_eq!(
        super::take_session0_shell(&mut handed_off, None).map(|shell| shell.local_id),
        Some(0)
    );
    assert_eq!(handed_off.len(), 2);
}

/// A LEAF NAMING A SHELL THAT RUNS ELSEWHERE GIVES ITS IDENTITY TO THAT
/// SHELL, NOT TO THE STAND-IN FILLING ITS PANE. An honored handoff layout
/// never reaches this: each shell is adopted by its own leaf or as its own
/// window's bootstrap. The old session-0 pick reached it
/// ([`first_listed_session0_shell`]), and a tab that fails to build hands its
/// window's bootstrap on to another shell's pane; `carry_restored_identity`
/// must stay right there. Session 0 is shell 0, kept as an extra tab in
/// native-only window 0 because it IS a handed-off shell; its own pane shows
/// it again (round six, finding 10: a shell this restore already adopted is
/// shown, never stood in for by a shell forked during the overlap), and it
/// wears its leaf's identity, which window 0's tab chrome repaints to show.
#[test]
fn a_handoff_keeps_each_identity_on_its_shell_when_a_leaf_names_a_shell_running_elsewhere() {
    use crate::session_timeline::SessionMeta;

    for (shape, windows) in [
        (
            "shell 0's leaf is a later pane of window 1",
            vec![
                window_of(vec![settings_leaf()]),
                window_of(vec![
                    split_of(leaf_naming(2), leaf_naming(0)),
                    leaf_naming(1),
                ]),
            ],
        ),
        (
            "shell 0's leaf is window 1's first pane",
            vec![
                window_of(vec![settings_leaf()]),
                window_of(vec![
                    leaf_naming(0),
                    split_of(leaf_naming(1), leaf_naming(2)),
                ]),
            ],
        ),
    ] {
        let new = successor_of(windows, 3, first_listed_session0_shell);
        assert_eq!(
            new.pool.get(0).and_then(|session| session.handoff_local_id),
            Some(0),
            "PRECONDITION {shape}: session 0 is shell 0"
        );
        let mut stand_ins = Vec::new();
        for (&session, pooled) in &new.pool.sessions {
            match pooled.session.handoff_local_id {
                Some(shell) => assert_eq!(
                    registry_meta(&new, session),
                    identity_of(shell),
                    "{shape}: shell {shell} (session {session}) wears exactly the identity \
                     of the leaf that names it"
                ),
                None => {
                    stand_ins.push(session);
                    assert_eq!(
                        registry_meta(&new, session),
                        SessionMeta::default(),
                        "{shape}: stand-in session {session} is no handed-off shell, so \
                         it wears no identity"
                    );
                }
            }
        }
        assert_eq!(
            stand_ins,
            Vec::<u64>::new(),
            "{shape}: no stand-in is forked"
        );
        assert!(
            shown_shells(&new)
                .iter()
                .skip(1)
                .flatten()
                .flatten()
                .any(|shown| *shown == Some(Some(0))),
            "{shape}: shell 0's own pane shows it"
        );
        // Window 0 was rebuilt before shell 0's identity arrived; its tab
        // shows it all the same.
        assert_tooltips_name_their_shells(&new, shape);
        assert!(new.structural_invariants_ok(), "{shape}");
    }
}

/// A SHELL THE REBUILD LEAVES OVER STILL COMES BACK WEARING ITS IDENTITY.
/// The orphan net re-homes every handed-off shell no leaf took as a tab of
/// the front window, so none is lost. Such a shell is left over because the
/// leaf naming it never rebuilt — its tab failed to build, or, as here, its
/// window could not be created and `apply_restore_manifest` stopped — or
/// was filled by another shell. Its identity reached nobody during the
/// rebuild, and the net used to place it bare: the manifest was consumed by
/// then. The leaves naming handed-off shells now outlive it
/// (`handed_off_leaves`), and the net puts each one's identity on its
/// shell before registering it.
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
    // Window 0 rebuilds; window 1 is never created.
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

/// THE BOOTSTRAP STARTS IN THE DIRECTORY OF THE PANE IT FILLS. A cold
/// restore spawns session 0 in `first_leaf_cwd` before the deferred pass
/// grafts it onto the canonical tree's first terminal leaf — the leaf
/// `bootstrap_local_id` names. `first_leaf_cwd` read the first TAB, and on a
/// native-only first tab fell back to the legacy `tabs` mirror, which lists
/// all-terminal tabs only: behind a tab splitting a native view with a
/// terminal, it named another tab's pane. A pane that recorded no directory
/// starts where another pane of its OWN tab was — what that first-tab read
/// did for a first tab — but never where another tab's was.
#[test]
fn the_bootstrap_cwd_is_the_directory_of_the_leaf_the_bootstrap_fills() {
    // PREDECESSOR: a Settings tab, a tab splitting a recovery placeholder
    // with session 0, then session 1's all-terminal tab.
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
    assert_eq!(
        manifest.first_leaf_cwd(),
        None,
        "no pane of session 0's tab recorded a directory, so the bootstrap starts in \
         the default one, not in session 1's tab's"
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

    // A pane that recorded no directory — its program sent no OSC 7, or
    // the capture found its terminal locked — takes the first directory
    // another terminal pane of its own tab recorded, in rebuild order.
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

/// THE GRAFT IS A CHANGE A WATCHER CAN SEE. The grafted session is already
/// registered and served under its sid when the deferred restore reaches
/// it, so a `meta` reader or a `subscribe … events` watcher may already have
/// seen it without its identity. Each field the graft moves lands on the
/// session's timeline as the ordinary `meta-change` — what the digest pushes
/// as `EVENT <sid> meta …` — and the watcher is woken for it.
#[test]
fn a_graft_announces_every_field_it_moves_to_an_events_watcher() {
    let mut app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("the bootstrap").ctx.clone();
    // Where a `subscribe … events` digest seeds its watermark.
    let watermark = ctx.timeline.lock().unwrap().high_id();
    let watch = crate::subscribe::SubscriberSet::register(&app.subscribers, &[0]);
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
        .map(|event| event.payload.clone())
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
