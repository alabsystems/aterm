// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The navigator's host (tracker ST-04, `docs/DESIGN-streams-2026-11-12.md` §3): what it lists
//! and in what order, how it opens, what stays out of the restore manifest, and that a list
//! nothing changed in builds no row and repaints nothing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_types::app_inspection::InspectionProjection;
use aterm_types::keyboard::NamedKey;

use crate::app_control::InspectRequest;
use crate::app_workers_tests::{land_worker, person_tab};
use crate::native_app::DamageRegion;
use crate::navigator::Word;
use crate::tab_model::{View, ViewId};
use crate::task_record::{TaskMark, TaskState, mark_session_task};
use crate::{App, WindowId, stub_session};

const WID: WindowId = WindowId(0);

fn active_session(app: &App, wid: WindowId) -> Option<u64> {
    let tab = app.windows[&wid].tab_set.active()?;
    app.view_store
        .get(tab.focus)
        .copied()
        .and_then(View::terminal_session)
}

/// The sid tasks name `session` by in `from=`.
fn sid_of(app: &App, session: u64) -> String {
    app.pool
        .get(session)
        .expect("a pooled session")
        .ctx
        .self_id
        .as_str()
        .to_owned()
}

/// Mark `session`'s task `state`, created by `from` (`None`: a `ctl` call from no session), and
/// tell the host as `Wake::TaskChanged` does.
fn mark(app: &mut App, session: u64, state: TaskState, from: Option<String>) {
    {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let handle = store.by_local(session).expect("a live session");
        let mut mark = TaskMark::new(state);
        mark.from = from;
        mark_session_task(&handle.ctx, handle.local_id, &mark).expect("unfenced");
    }
    app.on_task_changed(session);
}

fn press(key: NamedKey) -> crate::input::InputEvent {
    crate::input::InputEvent::Key {
        key: aterm_types::keyboard::Key::Named(key),
        mods: aterm_types::keyboard::Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    }
}

/// Press the page's control `key` as a click does.
fn press_control(app: &mut App, key: &str) {
    app.dispatch_native_event(
        WID,
        crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new(key.to_owned()),
            value: None,
        }),
    )
    .expect("the page's control");
}

/// The navigator view window `wid` holds.
fn view_in(app: &App, wid: WindowId) -> ViewId {
    app.navigator_views()
        .into_iter()
        .find_map(|(window, view)| (window == wid).then_some(view))
        .expect("a navigator view in the window")
}

/// The sessions `view` lists, in order.
fn listed(app: &App, view: ViewId) -> Vec<u64> {
    app.navigator_listing(view)
        .into_iter()
        .map(|row| row.session)
        .collect()
}

fn inspect_text(app: &mut App, view: ViewId) -> Vec<String> {
    app.inspect_app(InspectRequest::View {
        view,
        projection: InspectionProjection::Text,
    })
    .expect("the view answers inspect")
}

/// The canonical index of the tab holding `view`.
fn tab_index_of(app: &App, wid: WindowId, view: ViewId) -> usize {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .position(|tab| tab.root.contains(view))
        .expect("a tab holds the view")
}

fn tab_index_of_session(app: &App, wid: WindowId, session: u64) -> usize {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .position(|tab| app.tab_holds_session(tab, session))
        .expect("a tab holds the session")
}

/// Paint window `wid`'s front as `image` does, so its native leaf has a raster and a layout.
fn paint(app: &mut App) {
    let (reply, answer) = std::sync::mpsc::channel();
    app.render_image(crate::control::ImageReq {
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: crate::control_auth::ConfinedImage::in_memory(),
        clean: false,
        session: None,
        want_bytes: true,
        want_metadata: false,
        frame_metadata: Arc::new(std::sync::OnceLock::new()),
        cancel: crate::control::CaptureCancellation::new(),
        reply,
    });
    answer
        .recv_timeout(Duration::from_secs(60))
        .expect("an image reply")
        .expect("an image of the navigator");
}

/// EVERY LIVE SESSION OF EVERY WINDOW IS ONE ROW: the person's tabs first (windows in their
/// order, each window's tabs in strip order, each tab's panes in order), each with the sessions
/// created under it; then, under the machine's name, every session no tab of theirs leads to. A
/// row with sessions under it starts closed and says how many, a count and nothing else: its
/// word is its own, whatever a session under it reads. Each row reads one of five words.
#[test]
fn the_navigator_lists_every_session_of_every_window_pinned_first_then_the_machine() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let a = active_session(&app, WID).expect("the first tab");
    let b1 = person_tab(&mut app);
    let b2 = app.split_active_stub_tab(WID);
    app.sync_window(WID);
    let w1 = land_worker(&mut app);
    let w2 = land_worker(&mut app);
    let w3 = land_worker(&mut app);
    let w4 = land_worker(&mut app);
    let (from_a, from_w1) = (sid_of(&app, a), sid_of(&app, w1));
    mark(&mut app, w1, TaskState::Working, Some(from_a));
    mark(&mut app, w2, TaskState::Failed, Some(from_w1));
    mark(&mut app, w3, TaskState::Done, None);
    let gone = crate::tab_ids::mint().as_str().to_owned();
    mark(&mut app, w4, TaskState::Exited, Some(gone));

    let other = WindowId(1);
    let c = app.next_session_id;
    app.next_session_id += 1;
    let session = stub_session(c);
    // A stub has no PTY, so the window's install does not register it.
    App::register_session(&app.store, &session, None);
    app.install_window_state(other, session, 38, 132);
    app.sync_window(other);
    app.frontmost_window = Some(WID);
    let person = active_session(&app, WID).expect("the person's pane");
    assert!(person == b1 || person == b2);

    assert_eq!(app.open_navigator(false), Ok(false), "it opens as a tab");
    app.publish_navigators(Instant::now());
    let view = view_in(&app, WID);
    let tree = app.navigator_tree().expect("a published list");

    // The heading is the machine's name.
    assert_eq!(tree.machine, app.machine_title());
    assert!(!tree.machine.is_empty() && tree.machine != "unnumbered");

    // Each live session once, and nothing else: the navigator's own tab and the Workers list
    // parking opened are not rows.
    let mut all = tree.sessions();
    all.sort_unstable();
    let mut live = vec![a, b1, b2, c, w1, w2, w3, w4];
    live.sort_unstable();
    assert_eq!(all, live);

    // The person's tabs in window, strip and pane order; then the machine's.
    let rows = app.navigator_listing(view);
    assert_eq!(
        rows.iter().map(|row| row.session).collect::<Vec<_>>(),
        [a, b1, b2, c, w3, w4]
    );
    assert_eq!(
        rows.iter().map(|row| row.pinned).collect::<Vec<_>>(),
        [true, true, true, true, false, false]
    );

    // `a` is closed over w1 and w2: a count, and its own word, though w2 under it is stuck.
    assert!(!rows[0].open);
    assert_eq!(rows[0].under, 2);
    let word = |session: u64| {
        tree.entries[tree.index_of(session).expect("listed")]
            .row
            .word
    };
    assert_eq!(word(a), Word::Idle);
    assert_eq!(word(w2), Word::Stuck);
    let text = inspect_text(&mut app, view);
    let line_of = |session: u64| {
        text.iter()
            .find(|line| line.contains(&format!("\"navigator/row/{session}\"")))
            .unwrap_or_else(|| panic!("a row for session {session}: {text:#?}"))
            .clone()
    };
    assert!(line_of(a).contains("2 under"), "{}", line_of(a));
    assert!(line_of(a).contains("idle"), "{}", line_of(a));
    assert!(!line_of(a).contains("stuck"), "{}", line_of(a));

    // One of five words each, each earned here, and no need words anywhere.
    assert_eq!(word(w1), Word::Working);
    assert_eq!(word(w3), Word::Quiet);
    assert_eq!(word(w4), Word::Ended);
    for session in [b1, b2, c] {
        assert_eq!(word(session), Word::Idle);
    }
    for line in &text {
        assert!(
            !line.contains("Needs you") && !line.contains("need"),
            "{line}"
        );
    }

    // The selection starts on the session the person is in. Step In opens a row; the sessions
    // under it are listed under it.
    assert_eq!(app.navigator_selection(view), Some(person));
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowUp)));
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowUp)));
    assert_eq!(app.navigator_selection(view), Some(a));
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowRight)));
    assert_eq!(listed(&app, view), [a, w1, b1, b2, c, w3, w4]);
    let rows = app.navigator_listing(view);
    assert_eq!((rows[1].depth, rows[1].open, rows[1].under), (1, false, 1));
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowRight)));
    assert_eq!(app.navigator_selection(view), Some(w1), "into the open row");
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowRight)));
    assert_eq!(listed(&app, view), [a, w1, w2, b1, b2, c, w3, w4]);
    // Step Out closes it again, then moves to the row above.
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowLeft)));
    assert_eq!(listed(&app, view), [a, w1, b1, b2, c, w3, w4]);
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowLeft)));
    assert_eq!(app.navigator_selection(view), Some(a));
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowLeft)));
    assert_eq!(listed(&app, view), [a, b1, b2, c, w3, w4]);

    // A fresh view starts open on the way to the session the person is in, and only there.
    let nav_tab = tab_index_of(&app, WID, view);
    app.close_tab_via_verb(WID, Some(nav_tab));
    app.sync_window(WID);
    assert!(app.focus_session_preferring(Some(WID), w1));
    app.sync_window(WID);
    assert_eq!(app.open_navigator(false), Ok(false));
    app.publish_navigators(Instant::now());
    let fresh = view_in(&app, WID);
    assert_ne!(fresh, view);
    assert_eq!(listed(&app, fresh), [a, w1, w2, b1, b2, c, w3, w4]);
}

/// ONE VIEW, TWO HOSTS: the navigator opens as an ordinary closable tab titled with the
/// machine's name, and as the left pane of a split about 30 cells wide beside a terminal that
/// keeps its session; a pane too narrow for both gets a tab. Both views show one list. It answers
/// `inspect` and image capture, and neither host is written to the restore manifest: the split
/// tab is written with its terminal alone.
#[test]
fn the_navigator_opens_as_a_tab_and_a_split_leaf_from_one_view() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    let second = person_tab(&mut app);
    app.switch_tab_in(WID, 0);
    app.sync_window(WID);

    // A tab.
    assert_eq!(app.open_navigator(false), Ok(false));
    app.publish_navigators(Instant::now());
    let (instance, tab_view) = app.active_native_view(WID).expect("the navigator in front");
    assert_eq!(
        app.native_runtime
            .app(instance)
            .expect("its app")
            .kind()
            .as_str(),
        "navigator"
    );
    let title = app.machine_title();
    assert!(!title.is_empty());
    {
        let tab = app.windows[&WID].tab_set.active().expect("its tab");
        assert!(tab.presentation.closable);
        assert_eq!(tab.presentation.title, title);
    }
    let text = inspect_text(&mut app, tab_view).join("\n");
    for session in [first, second] {
        let label = app.tab_label_of_session(session);
        assert!(text.contains(&label), "{label} is in:\n{text}");
    }
    paint(&mut app);
    assert!(
        app.windows[&WID].leaf_render_cache[&tab_view]
            .native
            .is_some(),
        "the capture painted the navigator's leaf"
    );
    assert_eq!(app.open_navigator(false), Ok(false));
    assert_eq!(
        app.navigator_views().len(),
        1,
        "a second ask reuses its tab"
    );

    // A split, on the left, beside a lone terminal.
    app.switch_tab_in(WID, tab_index_of_session(&app, WID, second));
    app.sync_window(WID);
    let next = app.next_session_id;
    let split_tab = app.windows[&WID].tab_set.active_id();
    assert_eq!(app.open_navigator(true), Ok(true));
    app.publish_navigators(Instant::now());
    assert_eq!(app.next_session_id, next, "a native split spawns no PTY");
    assert_eq!(app.windows[&WID].tab_set.active_id(), split_tab);
    let plan = app.active_visible_leaf_plan(WID).expect("a plan");
    assert_eq!(plan.leaves.len(), 2);
    let (left, right) = (&plan.leaves[0], &plan.leaves[1]);
    assert!(left.rect.origin.x < right.rect.origin.x);
    let split_view = left.view;
    assert_ne!(split_view, tab_view);
    assert!(
        matches!(app.view_store.get(split_view), Some(View::Native(native)) if native.instance == instance),
        "the left pane is a view of the same navigator"
    );
    assert_eq!(
        left.rect.size.width,
        f32::from(crate::navigator::SPLIT_COLS),
        "about 30 cells"
    );
    assert_eq!(
        app.view_store
            .get(right.view)
            .copied()
            .and_then(View::terminal_session),
        Some(second)
    );
    {
        let session = app.pool.get(second).expect("its session");
        let term = crate::term_lock(&session.term);
        assert_eq!(
            (term.rows(), term.cols()),
            (
                (right.rect.size.height.round() as u16).max(1),
                (right.rect.size.width.round() as u16).max(1)
            ),
            "the terminal has its pane's grid"
        );
    }
    assert_eq!(app.navigator_instance(), Some(instance), "one navigator");
    assert_eq!(app.navigator_views().len(), 2);
    assert_eq!(listed(&app, split_view), listed(&app, tab_view), "one list");
    assert_eq!(app.open_navigator(true), Ok(true));
    assert_eq!(
        app.navigator_views().len(),
        2,
        "a tab that has the navigator is not split again"
    );

    // The restore manifest holds neither: the navigator's tab is not written, and the split is
    // written with its terminal alone.
    let nav_tab = tab_index_of(&app, WID, tab_view);
    {
        let tabs = app.windows[&WID].tab_set.tabs();
        assert!(app.tab_restore_descriptor(&tabs[nav_tab]).is_none());
        let split = tabs
            .iter()
            .find(|tab| tab.root.contains(split_view))
            .expect("the split tab");
        let restored = app
            .tab_restore_descriptor(split)
            .expect("the split tab is written");
        assert!(
            matches!(
                restored.root,
                crate::restore::RestoredSplitTree::Leaf {
                    view: crate::restore::RestoredView::Terminal(_)
                }
            ),
            "its terminal alone: {:?}",
            restored.root
        );
    }
    let manifest = app.capture_restore_manifest();
    assert_eq!(manifest.windows.len(), 1);
    assert_eq!(
        manifest.windows[0].restored_tabs.len(),
        2,
        "both of the person's tabs and nothing else"
    );
    assert!(
        !format!("{manifest:?}").to_lowercase().contains("navigator"),
        "{manifest:?}"
    );

    // Closing the navigator's tab is not refused.
    app.close_tab_via_verb(WID, Some(nav_tab));
    app.sync_window(WID);
    assert_eq!(app.navigator_views().len(), 1);
    assert!(app.structural_invariants_ok());

    // A pane too narrow for a 30-cell list beside a terminal gets a tab.
    let mut narrow = App::headless_for_test();
    narrow.windows.get_mut(&WID).expect("its window").cols = 40;
    let lone = narrow.windows[&WID].tab_set.active_id().expect("a tab");
    assert_eq!(narrow.open_navigator(true), Ok(false));
    assert_eq!(
        narrow.windows[&WID]
            .tab_set
            .get(lone)
            .expect("the terminal's tab")
            .root
            .leaves()
            .len(),
        1
    );
    assert_ne!(narrow.windows[&WID].tab_set.active_id(), Some(lone));
    // Asked again from the terminal's tab, the window's one navigator tab comes forward.
    let lone_at = narrow.windows[&WID]
        .tab_set
        .tabs()
        .iter()
        .position(|tab| tab.id == lone)
        .expect("the terminal's tab");
    narrow.switch_tab_in(WID, lone_at);
    narrow.sync_window(WID);
    assert_eq!(narrow.open_navigator(true), Ok(false));
    assert_eq!(
        narrow.navigator_views().len(),
        1,
        "a split with no room goes to the navigator tab the window has"
    );
    assert_ne!(narrow.windows[&WID].tab_set.active_id(), Some(lone));
    assert!(narrow.structural_invariants_ok());
}

/// ROWS DO NOT REORDER WHILE THE LIST HAS THE KEYS: a tab moved, opened or closed behind it
/// leaves each row where it was, newcomers after them, and the selection on its session. Once
/// the keys go elsewhere the list takes the strip's order. Nothing the list does resizes a
/// session: a publish, the arrows, the filter, Step In and Step Out, or Return on a parked
/// session, which shows it and raises its window.
#[test]
fn rows_do_not_reorder_while_the_navigator_has_the_keys() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let x = active_session(&app, WID).expect("the first tab");
    let y = person_tab(&mut app);
    let z = person_tab(&mut app);
    let w = land_worker(&mut app);
    let from_y = sid_of(&app, y);
    mark(&mut app, w, TaskState::Working, Some(from_y));

    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    assert_eq!(listed(&app, view), [x, y, z]);
    let resizes = |app: &App, session: u64| {
        let session = app.pool.get(session).expect("a pooled session");
        crate::resize_ledger::cmd_resizes(&session.ctx, &session.term, "")
    };
    let ledgers = |app: &App| [y, z, w].map(|session| resizes(app, session));
    let before = ledgers(&app);

    assert_eq!(
        app.navigator_selection(view),
        Some(z),
        "the selection starts on the session the person is in"
    );
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowUp)));
    assert_eq!(app.navigator_selection(view), Some(y));

    // With the keys: z moves to the strip's front and a tab opens behind the list.
    let z_at = tab_index_of_session(&app, WID, z);
    app.move_tab(WID, z_at, 0);
    let n = app.next_session_id;
    app.next_session_id += 1;
    let parent = app.new_tab_parent(WID);
    let kept = app.windows[&WID]
        .tab_set
        .active_index()
        .expect("the navigator's tab");
    app.install_tab_session_behind(WID, stub_session(n), parent, kept);
    app.sync_window(WID);
    app.publish_navigators(now);
    assert_eq!(view_in(&app, WID), view, "the list kept the front");
    assert_eq!(listed(&app, view), [x, y, z, n]);
    assert_eq!(app.navigator_selection(view), Some(y));

    assert!(app.close_session_tab_for_test(x));
    app.sync_window(WID);
    app.publish_navigators(now);
    assert_eq!(listed(&app, view), [y, z, n]);
    assert_eq!(
        app.navigator_selection(view),
        Some(y),
        "the selection follows its session, not its place"
    );

    // The arrows, the filter and the folds move nothing but the list.
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowRight)));
    assert_eq!(listed(&app, view), [y, w, z, n], "Step In opened y");
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowLeft)));
    assert_eq!(listed(&app, view), [y, z, n]);
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowDown)));
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowUp)));
    press_control(&mut app, crate::navigator::FILTER);
    app.dispatch_native_event(
        WID,
        crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
            app.tab_label_of_session(z),
        )),
    )
    .expect("typing in the filter");
    assert!(listed(&app, view).contains(&z));
    assert!(app.native_input_event(WID, &press(NamedKey::Escape)));
    assert_eq!(listed(&app, view), [y, z, n], "Escape clears the filter");

    // Without the keys the list takes the strip's order, and keeps it when they come back.
    app.switch_tab_in(WID, tab_index_of_session(&app, WID, y));
    app.sync_window(WID);
    app.publish_navigators(now);
    assert_eq!(listed(&app, view), [z, y, n]);
    app.switch_tab_in(WID, tab_index_of(&app, WID, view));
    app.sync_window(WID);
    app.publish_navigators(now);
    assert_eq!(listed(&app, view), [z, y, n]);
    assert_eq!(ledgers(&app), before, "nothing above resized a session");

    // Return on a parked session shows it in the window and raises the window.
    let _ = crate::app_tabs::take_raised_windows();
    press_control(&mut app, &format!("navigator/row/{w}"));
    assert_eq!(active_session(&app, WID), Some(w));
    assert!(app.session_is_parked(w), "showing it does not unpark it");
    assert_eq!(crate::app_tabs::take_raised_windows(), [WID]);
    assert_eq!(ledgers(&app), before, "and resized nobody");
}

/// AN UNCHANGED LIST COSTS NOTHING: a publish with nothing marked, or with only the broad status
/// bit and an equal layout, builds no row, reads no words, keeps the published tree and asks for
/// no paint; and the list keeps no polling timer. NEGATIVE CONTROL: one task edge builds exactly
/// that row and repaints.
#[test]
fn an_unchanged_list_rebuilds_no_row_and_repaints_nothing() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    let _second = person_tab(&mut app);
    let worker = land_worker(&mut app);
    mark(&mut app, worker, TaskState::Working, None);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    paint(&mut app);

    let serial = |app: &App| app.windows[&WID].leaf_render_cache[&view].native_paint_serial;
    let revision = |app: &App| {
        app.native_runtime
            .view_state(view)
            .expect("the view")
            .common()
            .presentation_revision
    };
    let tree = app.navigator_tree().expect("a list");
    let (work, painted, shown) = (app.navigator_work(), serial(&app), revision(&app));
    let compiles = crate::app_native::native_ui_compile_count();

    // Nothing marked.
    app.publish_navigators(now + Duration::from_secs(5));
    // The broad bit every strip sync sets, with the layout as it was.
    app.navigator_mark_structure();
    app.publish_navigators(now + Duration::from_secs(6));
    app.sync_window(WID);
    app.publish_navigators(now + Duration::from_secs(7));
    assert!(Arc::ptr_eq(&tree, &app.navigator_tree().expect("the list")));
    assert_eq!(app.navigator_work(), work, "no row built, no words read");
    assert_eq!(serial(&app), painted, "no paint asked for");
    assert_eq!(revision(&app), shown);
    assert_eq!(crate::app_native::native_ui_compile_count(), compiles);
    assert_eq!(
        app.navigator_words_wake(),
        None,
        "no words are owed, so no deadline is armed"
    );

    // One task edge: that row and no other.
    mark(&mut app, worker, TaskState::Done, None);
    app.publish_navigators(now + Duration::from_secs(8));
    let after = app.navigator_work();
    assert_eq!(after.rows_built, work.rows_built + 1);
    let moved = app.navigator_tree().expect("the list");
    assert!(!Arc::ptr_eq(&tree, &moved));
    assert_eq!(
        moved.entries[moved.index_of(worker).expect("listed")]
            .row
            .word,
        Word::Quiet
    );
    assert!(Arc::ptr_eq(
        &tree.entries[tree.index_of(first).expect("listed")].row,
        &moved.entries[moved.index_of(first).expect("listed")].row
    ));
    assert_ne!(serial(&app), painted, "the change asks for a paint");
}

/// ONE ROW'S CHANGE REPAINTS ONE ROW: a word that changed on a row already listed asks for a
/// rectangle inside that row and leaves the view's presentation revision alone, so the page
/// rasters that rectangle and not itself. NEGATIVE CONTROL: a session that opens changes the
/// arrangement and repaints the page whole.
#[test]
fn one_rows_change_repaints_one_rect() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let _first = active_session(&app, WID).expect("the first tab");
    let second = person_tab(&mut app);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    paint(&mut app);
    assert!(
        app.windows[&WID].leaf_render_cache[&view]
            .native_damage
            .is_none(),
        "PRECONDITION: the leaf is painted"
    );
    let revision = |app: &App| {
        app.native_runtime
            .view_state(view)
            .expect("the view")
            .common()
            .presentation_revision
    };
    let shown = revision(&app);

    mark(&mut app, second, TaskState::Working, None);
    app.publish_navigators(now + Duration::from_secs(1));
    let (top, bottom) = app
        .navigator_row_span(view, second)
        .expect("the row is on screen");
    match app.windows[&WID].leaf_render_cache[&view].native_damage {
        Some(DamageRegion::Rect { y, height, .. }) => {
            assert!(
                y as f32 >= top.floor() && (y + height) as f32 <= bottom.ceil(),
                "the rectangle {y}+{height} is inside the row {top}..{bottom}"
            );
        }
        other => panic!("one row's rectangle, not {other:?}"),
    }
    assert_eq!(revision(&app), shown);
    paint(&mut app);
    // What the paint did, not what was asked of it: a region inside the row.
    let scale = app.windows[&WID].scale as f32;
    let work = |app: &App| {
        app.windows[&WID].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("a retained raster")
            .last_work
    };
    match work(&app) {
        crate::NativeRasterWork::Region { rect, .. } => assert!(
            rect.y as f32 >= (top * scale).floor()
                && (rect.y + rect.height) as f32 <= (bottom * scale).ceil(),
            "the raster {rect:?} is inside the row {top}..{bottom} at scale {scale}"
        ),
        other => panic!("one row's region rastered, not {other:?}"),
    }

    let _third = person_tab(&mut app);
    app.switch_tab_in(WID, tab_index_of(&app, WID, view));
    app.sync_window(WID);
    app.publish_navigators(now + Duration::from_secs(2));
    assert_ne!(revision(&app), shown, "a new row lays the page out again");
    paint(&mut app);
    assert!(
        matches!(work(&app), crate::NativeRasterWork::Full { .. }),
        "NEGATIVE CONTROL: the page whole, not {:?}",
        work(&app)
    );
}

/// LATEST WORDS ARE READ FOR ROWS ON SCREEN ONLY, AT MOST ONCE A SECOND: output on a session
/// whose row is scrolled away reads nothing and paints nothing; output on one that shows is read
/// once its second is up, and not before. A NEW VIEW OPENS WITH THE SELECTED ROW IN ITS ROOM:
/// the session the person is in is the last of 41 here, so the first row is the one off screen.
#[test]
fn a_row_off_screen_is_not_read() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    let mut last = first;
    for _ in 0..40 {
        last = person_tab(&mut app);
    }
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    assert_eq!(app.navigator_selection(view), Some(last));
    assert!(
        app.navigator_row_span(view, last).is_some() && app.navigator_reads_words(last),
        "the selected row is in the room before the first frame"
    );
    paint(&mut app);
    assert!(app.navigator_row_span(view, last).is_some());
    assert!(
        app.navigator_row_span(view, first).is_none() && !app.navigator_reads_words(first),
        "PRECONDITION: the first row is off the screen"
    );
    // Settle what the first read owes (its trailing read).
    app.publish_navigators(now + Duration::from_secs(2));
    app.publish_navigators(now + Duration::from_secs(4));
    paint(&mut app);
    let serial = |app: &App| app.windows[&WID].leaf_render_cache[&view].native_paint_serial;
    let (work, painted) = (app.navigator_work(), serial(&app));

    app.navigator_mark_words(first);
    app.publish_navigators(now + Duration::from_secs(6));
    app.publish_navigators(now + Duration::from_secs(8));
    assert_eq!(app.navigator_work(), work, "an off-screen row is not read");
    assert_eq!(serial(&app), painted);
    assert_eq!(app.navigator_words_wake(), None);

    app.navigator_mark_words(last);
    assert!(
        app.navigator_words_wake().is_some(),
        "words are owed: one deadline"
    );
    app.publish_navigators(now + Duration::from_secs(10));
    assert_eq!(
        app.navigator_work().words_read,
        work.words_read + 1,
        "the row on screen is read"
    );
    assert_eq!(app.navigator_work().rows_built, work.rows_built);
    // Marked again inside the same second: coalesced, not read.
    let read = app.navigator_work().words_read;
    app.navigator_mark_words(last);
    app.publish_navigators(now + Duration::from_millis(10_200));
    assert_eq!(app.navigator_work().words_read, read);
}

/// The row the list holds for `session`.
fn row_of(app: &App, session: u64) -> Option<Arc<crate::navigator::NavRow>> {
    let tree = app.navigator_tree()?;
    Some(Arc::clone(&tree.entries[tree.index_of(session)?].row))
}

fn presentation_revision(app: &App, view: ViewId) -> u64 {
    app.native_runtime
        .view_state(view)
        .expect("the view")
        .common()
        .presentation_revision
}

fn paint_serial(app: &App, view: ViewId) -> u64 {
    app.windows[&WID].leaf_render_cache[&view].native_paint_serial
}

/// What the loop does with a session whose shell status moved.
fn observe_statuses(app: &mut App, at: Instant) {
    for changed in app.observe_session_statuses(at) {
        app.navigator_mark_row(changed);
    }
}

/// A RENAME IN THE WINDOW RENAMES THE ROW: a person's own rename (the strip's field, Rename
/// Session) writes the session's title on the loop thread and posts no wake, so the write
/// itself marks the row. NEGATIVE CONTROL: before the rename the row reads its tab's name.
#[test]
fn a_rename_in_the_window_renames_its_row() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    let _second = person_tab(&mut app);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let title = |app: &App| row_of(app, first).expect("listed").title.clone();
    assert_ne!(title(&app), "ledger");
    assert_eq!(
        app.write_session_title_pin(first, "ledger").ok(),
        Some(true)
    );
    app.publish_navigators(now + Duration::from_secs(1));
    assert_eq!(title(&app), "ledger");
}

/// A VIEW THAT GROWS READS THE ROWS IT NOW SHOWS: the frame that lays the page out at a new
/// size (a window resize, a divider drag, a text size) is what tells the list, so the rows that
/// came onto its screen are read and kept current.
#[test]
fn a_view_that_grows_reads_the_rows_it_now_shows() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let mut tabs = vec![active_session(&app, WID).expect("the first tab")];
    for _ in 0..40 {
        tabs.push(person_tab(&mut app));
    }
    app.switch_tab_in(WID, 0);
    app.sync_window(WID);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    paint(&mut app);
    app.publish_navigators(now + Duration::from_secs(1));
    assert!(app.navigator_reads_words(tabs[0]));
    let below = *tabs
        .iter()
        .find(|session| !app.navigator_reads_words(**session))
        .expect("PRECONDITION: a row under the fold");

    app.windows.get_mut(&WID).expect("the window").rows += 30;
    paint(&mut app);
    app.publish_navigators(now + Duration::from_secs(2));
    assert!(
        app.navigator_reads_words(below),
        "the row the taller view shows is read"
    );
}

/// A WINDOW THAT IS COVERED READS NOTHING AND ARMS NO DEADLINE; uncovered, it does again.
#[test]
fn a_covered_window_owes_no_wake_and_an_uncovered_one_does() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    assert!(
        app.navigator_wake().is_some() && app.navigator_reads_words(first),
        "PRECONDITION: the ages' minute is armed and the row is read"
    );
    app.on_occlusion_changed(WID, true, std::time::Instant::now());
    app.publish_navigators(now + Duration::from_secs(1));
    assert_eq!(app.navigator_wake(), None, "a covered list owes no wake");
    app.on_occlusion_changed(WID, false, std::time::Instant::now());
    app.publish_navigators(now + Duration::from_secs(2));
    assert!(app.navigator_wake().is_some());
    assert!(app.navigator_reads_words(first));
}

/// A ROW'S WORD IS ITS CONVERSATION'S, NOT A JOB'S LAST STATE: a person's shell with a command
/// running reads `working` and `idle` again at its prompt; a worker a person took off Autopilot
/// (`why=off`) reads its own word, never the error. NEGATIVE CONTROL: the same task failed with
/// no such reason reads `stuck`.
#[test]
fn a_shells_command_is_working_and_a_take_over_is_no_error() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let shell = active_session(&app, WID).expect("the first tab");
    let worker = land_worker(&mut app);
    mark(&mut app, worker, TaskState::Failed, None);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let word = |app: &App, session: u64| row_of(app, session).expect("listed").word;
    assert_eq!(word(&app, shell), Word::Idle);
    assert_eq!(word(&app, worker), Word::Stuck, "NEGATIVE CONTROL");

    let term = app.pool.get(shell).expect("the shell").term.clone();
    crate::term_lock(&term).process(b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\make\r\n\x1b]133;C\x1b\\");
    observe_statuses(&mut app, now);
    observe_statuses(&mut app, now + Duration::from_secs(1));
    app.publish_navigators(now + Duration::from_secs(1));
    assert_eq!(word(&app, shell), Word::Working, "a command is running");
    crate::term_lock(&term).process(b"\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\");
    observe_statuses(&mut app, now + Duration::from_secs(2));
    observe_statuses(&mut app, now + Duration::from_secs(3));
    app.publish_navigators(now + Duration::from_secs(3));
    assert_eq!(word(&app, shell), Word::Idle, "back at its prompt");

    {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let handle = store.by_local(worker).expect("a live session");
        let mut off = TaskMark::new(TaskState::Failed);
        off.why = Some(crate::task_record::WHY_OFF);
        mark_session_task(&handle.ctx, handle.local_id, &off).expect("unfenced");
    }
    app.on_task_changed(worker);
    app.publish_navigators(now + Duration::from_secs(4));
    assert_eq!(
        word(&app, worker),
        Word::Quiet,
        "taken off Autopilot: the conversation at rest, not an error"
    );
}

/// THE SIDEBAR'S EDGE IS A HAIRLINE: the column between the list and the pane beside it takes
/// the page's ground and a one-eighth rule in the separator's ink, in place of the pane
/// divider's slab and the active pane's mark. NEGATIVE CONTROL: the cell is not the divider's.
#[test]
fn the_sidebars_edge_is_a_hairline_in_the_separators_ink() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    assert_eq!(app.open_navigator(true), Ok(true));
    app.publish_navigators(Instant::now());
    assert!(app.prepare_heterogeneous_input_scratch(WID).is_some());
    let page = app.chrome_palette(crate::app_render::ChromeSurface::NativePage);
    let roles = crate::native_appearance::default_roles(page);
    let divider =
        crate::app_render::divider_cell(app.chrome_palette(crate::app_render::ChromeSurface::Grid));
    let plan = app.active_visible_leaf_plan(WID).expect("a plan");
    let list = plan.leaves[0].rect;
    let gap = (list.origin.x + list.size.width).round() as usize;
    assert_eq!(gap, usize::from(crate::navigator::SPLIT_COLS));
    let top = list.origin.y.round() as usize;
    let rows = top..top + list.size.height.round() as usize;
    let scratch = &app.windows[&WID].input_scratch;
    assert!(rows.len() > 1);
    for row in rows {
        let cell = scratch.cells[row][gap];
        assert_eq!(
            (cell.ch, cell.fg, cell.bg, cell.overline),
            ('\u{2595}', roles.separator, roles.surface, false),
            "row {row}: a one-eighth rule on the page's ground"
        );
        assert_ne!(
            (cell.ch, cell.bg),
            (divider.ch, divider.bg),
            "NEGATIVE CONTROL: not the pane divider's slab"
        );
    }
    // The rule runs on across the list's header row, on the header's own ground.
    let header = plan.leaves[0]
        .header
        .expect("a pane of a split has a header");
    let band = crate::pane_header::header_colors(
        app.chrome_palette(crate::app_render::ChromeSurface::Header),
    )
    .bar_bg;
    let cell = scratch.cells[header.origin.y.round() as usize][gap];
    assert_eq!(
        (cell.ch, cell.fg, cell.bg),
        ('\u{2595}', roles.separator, band)
    );
}

/// A FILTERED LIST IS ITS MATCHES: a row whose word stops matching the filter leaves the page,
/// which is drawn whole at a new revision and asks for a paint.
#[test]
fn a_row_that_leaves_the_filter_redraws_the_list() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    let second = person_tab(&mut app);
    mark(&mut app, first, TaskState::Working, None);
    mark(&mut app, second, TaskState::Working, None);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    press_control(&mut app, crate::navigator::FILTER);
    app.dispatch_native_event(
        WID,
        crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
            "working".to_owned(),
        )),
    )
    .expect("typing in the filter");
    app.publish_navigators(now + Duration::from_secs(1));
    assert_eq!(listed(&app, view), [first, second]);
    paint(&mut app);
    app.publish_navigators(now + Duration::from_secs(2));
    app.publish_navigators(now + Duration::from_secs(4));
    paint(&mut app);
    let (shown, painted) = (presentation_revision(&app, view), paint_serial(&app, view));

    mark(&mut app, second, TaskState::Done, None);
    app.publish_navigators(now + Duration::from_secs(5));
    assert_eq!(listed(&app, view), [first]);
    assert_ne!(
        presentation_revision(&app, view),
        shown,
        "the page is drawn whole"
    );
    assert_ne!(
        paint_serial(&app, view),
        painted,
        "and a paint is asked for"
    );
}

/// A FRAME OF THE TERMINAL BESIDE AN UNCHANGED LIST COPIES NOTHING OF THE LIST: with the keys
/// on the sidebar, the window keeps the compiled page it holds across frames. NEGATIVE CONTROL:
/// a change to the list hands the window a new one.
#[test]
fn a_frame_beside_an_unchanged_sidebar_keeps_its_compiled_page() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let beside = active_session(&app, WID).expect("the first tab");
    assert_eq!(app.open_navigator(true), Ok(true));
    let now = Instant::now();
    app.publish_navigators(now);
    fn held(app: &App) -> &crate::native_ui::CompiledUi {
        &app.windows[&WID]
            .native_ui_compiled
            .as_ref()
            .expect("the focused page's compiled frame")
            .compiled
    }
    assert!(app.prepare_heterogeneous_input_scratch(WID).is_some());
    // The first page is alive through the second frame, so the two addresses compare.
    let first = held(&app).paint.as_ptr();
    assert!(app.prepare_heterogeneous_input_scratch(WID).is_some());
    assert_eq!(
        held(&app).paint.as_ptr(),
        first,
        "the second frame cloned no page"
    );

    // A new page is proved by what it holds: the page it replaces is freed before it is made,
    // and its buffer may take that address.
    let before = held(&app).clone();
    mark(&mut app, beside, TaskState::Working, None);
    app.publish_navigators(now + Duration::from_secs(1));
    assert!(app.prepare_heterogeneous_input_scratch(WID).is_some());
    assert!(
        *held(&app) != before,
        "NEGATIVE CONTROL: a changed list is a new page"
    );
}

/// A KEY THAT MOVES NOTHING REPAINTS NOTHING: an arrow at the list's end, or a page step on a
/// list that fits, leaves the paint serial and the revision where they were. NEGATIVE CONTROL:
/// an arrow that moves the selection asks for a paint, of its rows' band and not the page.
#[test]
fn a_key_that_moves_nothing_repaints_nothing() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let _first = active_session(&app, WID).expect("the first tab");
    let second = person_tab(&mut app);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    paint(&mut app);
    assert_eq!(app.navigator_selection(view), Some(second), "the last row");
    // The first key shows the keyboard's ring, which is a change.
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowDown)));
    app.publish_navigators(now);
    paint(&mut app);
    let (shown, painted) = (presentation_revision(&app, view), paint_serial(&app, view));
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowDown)));
    assert!(app.native_input_event(WID, &press(NamedKey::PageDown)));
    assert_eq!(app.navigator_selection(view), Some(second));
    assert_eq!(paint_serial(&app, view), painted, "nothing moved: no paint");
    assert_eq!(presentation_revision(&app, view), shown);

    assert!(app.native_input_event(WID, &press(NamedKey::ArrowUp)));
    assert_ne!(paint_serial(&app, view), painted, "NEGATIVE CONTROL");
    assert_eq!(
        presentation_revision(&app, view),
        shown,
        "a selection between rows in place repaints their band, not the page"
    );
}

/// THE KEYS REACH THE LIST AS KEYS, through the window's key route: Command-] opens the selected
/// row and Command-[ closes it, Down moves onto the session under it, Find puts the keyboard in
/// the filter and Escape gives it back, and Return goes to the selected session.
#[test]
fn step_in_the_arrows_find_and_return_reach_the_list_as_keys() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let x = active_session(&app, WID).expect("the first tab");
    let y = person_tab(&mut app);
    let w = land_worker(&mut app);
    let from_y = sid_of(&app, y);
    mark(&mut app, w, TaskState::Working, Some(from_y));
    assert_eq!(app.open_navigator(false), Ok(false));
    app.publish_navigators(Instant::now());
    let view = view_in(&app, WID);
    assert_eq!(
        listed(&app, view),
        [x, y, w],
        "the row of the session the person is in starts open"
    );
    assert_eq!(app.navigator_selection(view), Some(y));
    let command = |ch: char| crate::input::InputEvent::Key {
        key: aterm_types::keyboard::Key::Character(ch),
        mods: aterm_types::keyboard::Modifiers::SUPER,
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    };
    let focus = |app: &App| {
        app.native_runtime
            .view_state(view)
            .expect("the view")
            .common()
            .last_focus
            .as_ref()
            .map(|key| key.as_str().to_owned())
    };
    assert!(app.native_input_event(WID, &command('[')));
    assert_eq!(listed(&app, view), [x, y], "Command-[ closed y");
    assert!(app.native_input_event(WID, &command(']')));
    assert_eq!(listed(&app, view), [x, y, w], "Command-] opened it");
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowDown)));
    assert_eq!(app.navigator_selection(view), Some(w));

    app.find_requested_in(WID);
    assert_eq!(focus(&app).as_deref(), Some(crate::navigator::FILTER));
    assert!(app.native_input_event(WID, &press(NamedKey::Escape)));
    assert_eq!(focus(&app), Some(format!("navigator/row/{w}")));

    let _ = crate::app_tabs::take_raised_windows();
    assert!(app.native_input_event(WID, &press(NamedKey::Enter)));
    assert_eq!(
        active_session(&app, WID),
        Some(w),
        "Return goes to the session"
    );
    assert_eq!(crate::app_tabs::take_raised_windows(), [WID]);
}

/// The retained raster of `view`.
fn raster(app: &App, view: ViewId) -> &crate::NativeLeafRaster {
    app.windows[&WID].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("a retained raster")
}

/// The middle of control `key` on the frame the pointer is routed by, in window pixels.
fn middle_of(app: &App, view: ViewId, key: &str) -> (f64, f64) {
    let artifact = app
        .retained_native_pointer_artifact(WID, view)
        .expect("a presented frame the pointer is routed by");
    let hit = artifact
        .compiled
        .hits
        .iter()
        .find(|hit| hit.key.as_str() == key)
        .unwrap_or_else(|| panic!("{key} is a target of the frame"));
    (
        artifact.device_x as f64 + f64::from(hit.rect.x + hit.rect.width * 0.5) * artifact.scale,
        artifact.device_y as f64 + f64::from(hit.rect.y + hit.rect.height * 0.5) * artifact.scale,
    )
}

/// The frames window [`WID`] asked its OS window for.
fn redraws(app: &App) -> u32 {
    app.windows[&WID].redraw_requests_for_test.get()
}

/// What the compile `view`'s raster was painted from says of `session`'s row: the page on glass.
fn drawn_label(app: &App, view: ViewId, session: u64) -> String {
    raster(app, view)
        .compiled
        .semantic(&crate::native_ui::UiKey::new(format!(
            "navigator/row/{session}"
        )))
        .expect("the row's node")
        .label
        .clone()
}

/// A navigator tab over 41 sessions, framed, its reads settled, with the selected row (the
/// last) in its room and the first row scrolled off its screen: the app, the view, the first and
/// last sessions, and the clock it was published at.
fn a_list_past_its_first_row() -> (App, ViewId, u64, u64, Instant) {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    let mut last = first;
    for _ in 0..40 {
        last = person_tab(&mut app);
    }
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    assert!(app.commit_glassless_native_frame(WID));
    app.publish_navigators(now + Duration::from_secs(2));
    app.publish_navigators(now + Duration::from_secs(4));
    assert!(app.commit_glassless_native_frame(WID));
    assert!(
        app.navigator_row_span(view, first).is_none(),
        "PRECONDITION: the first row is off the screen"
    );
    assert!(
        listed(&app, view).contains(&first),
        "PRECONDITION: and the view lists it"
    );
    assert!(
        !inspect_text(&mut app, view)
            .iter()
            .any(|line| line.contains("ledger"))
    );
    assert!(
        app.retained_native_pointer_artifact(WID, view).is_some(),
        "PRECONDITION: the frame on glass routes the pointer"
    );
    (app, view, first, last, now)
}

/// A CHANGE A VIEW LISTS OFF ITS SCREEN ASKS FOR NO FRAME: no pixel or target of the row is on
/// the glass, so the view keeps its revision and its paint, asks its window for nothing, and a
/// frame that comes for another cause compiles nothing for it. The list took the change:
/// `inspect` reads the row as it is now, the frame on glass still routes the pointer, and the row
/// reads the change once it is scrolled onto the screen. CONTROL: the same change to a row on
/// the screen asks for exactly one frame, which compiles the page once.
#[test]
fn a_change_off_screen_asks_for_no_frame() {
    let (mut app, view, first, last, now) = a_list_past_its_first_row();
    let at = |s: u64| now + Duration::from_secs(s);
    assert_eq!(
        app.write_session_title_pin(first, "ledger").ok(),
        Some(true)
    );
    app.navigator_mark_row(first);
    let (shown, painted, asked) = (
        presentation_revision(&app, view),
        paint_serial(&app, view),
        redraws(&app),
    );
    let compiles = crate::app_native::native_ui_compile_count();
    app.publish_navigators(at(5));
    assert_eq!(
        row_of(&app, first).expect("listed").title,
        "ledger",
        "NEGATIVE CONTROL: the list took the change"
    );
    assert_eq!(redraws(&app), asked, "no frame asked for");
    assert_eq!(paint_serial(&app, view), painted, "no paint asked for");
    assert_eq!(
        app.windows[&WID].leaf_render_cache[&view].native_damage,
        None
    );
    assert_eq!(
        presentation_revision(&app, view),
        shown,
        "the view's revision stays: the raster on glass is still its page"
    );
    assert!(app.commit_glassless_native_frame(WID));
    assert_eq!(
        crate::app_native::native_ui_compile_count(),
        compiles,
        "a frame that comes for another cause compiles nothing for it"
    );
    assert!(
        !drawn_label(&app, view, first).contains("ledger"),
        "the page on glass is the one it was"
    );
    assert!(
        app.retained_native_pointer_artifact(WID, view).is_some(),
        "the frame on glass routes the pointer"
    );
    let _ = middle_of(&app, view, &format!("navigator/row/{last}"));
    assert!(
        inspect_text(&mut app, view)
            .iter()
            .any(|line| line.contains("ledger")),
        "inspect reads the row as it is now"
    );

    // Scrolled onto the screen, the row is drawn from the list as published.
    app.dispatch_native_event(WID, crate::native_app::AppEvent::ScrollLines(-1000))
        .expect("a wheel on the list");
    app.navigator_view_moved();
    app.publish_navigators(at(6));
    assert!(app.commit_glassless_native_frame(WID));
    assert!(
        app.navigator_row_span(view, first).is_some(),
        "PRECONDITION: the first row is on the screen"
    );
    assert!(
        drawn_label(&app, view, first).contains("ledger"),
        "the row reads the change: {}",
        drawn_label(&app, view, first)
    );

    // CONTROL: the same change to a row on the screen.
    assert_eq!(
        app.write_session_title_pin(first, "journal").ok(),
        Some(true)
    );
    app.navigator_mark_row(first);
    let (painted, asked) = (paint_serial(&app, view), redraws(&app));
    let compiles = crate::app_native::native_ui_compile_count();
    app.publish_navigators(at(7));
    assert_eq!(redraws(&app), asked + 1, "CONTROL: one frame asked for");
    assert_ne!(paint_serial(&app, view), painted, "CONTROL: and a paint");
    assert!(app.commit_glassless_native_frame(WID));
    assert_eq!(
        crate::app_native::native_ui_compile_count(),
        compiles + 1,
        "CONTROL: which compiles the page once"
    );
    assert!(drawn_label(&app, view, first).contains("journal"));
}

/// A CHANGE A VIEW LISTS OFF ITS SCREEN, WITH NO CLIENT READING THE WINDOW, KEEPS THE FRAME THE
/// POINTER PRESSES: it asks for no frame, and while the page's nodes stand behind its model a
/// press is dispatched through the page compiled as it stands (`App::note_native_nodes_behind`).
/// The frame on glass still routes the pointer: hover lights the row under it, and a press and
/// release on the row goes to its session.
#[test]
fn a_change_off_screen_keeps_the_frame_the_pointer_presses() {
    use aterm_window::event::{ElementState, MouseButton};

    let (mut app, view, first, last, now) = a_list_past_its_first_row();
    assert_eq!(
        app.write_session_title_pin(first, "ledger").ok(),
        Some(true)
    );
    app.navigator_mark_row(first);
    let asked = redraws(&app);
    app.publish_navigators(now + Duration::from_secs(5));
    assert_eq!(redraws(&app), asked, "PRECONDITION: no frame asked for");

    let key = format!("navigator/row/{last}");
    let (x, y) = middle_of(&app, view, &key);
    app.on_cursor_moved(WID, x, y);
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.as_ref())
            .map(crate::native_ui::UiKey::as_str),
        Some(key.as_str()),
        "the pointer lights the row it is over"
    );
    assert!(
        app.windows[&WID].leaf_render_cache[&view].native_nodes_behind,
        "PRECONDITION: the page's nodes stand behind its model at the press"
    );
    assert_eq!(active_session(&app, WID), None, "PRECONDITION");
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        active_session(&app, WID),
        Some(last),
        "the first click on a row goes to its session"
    );
}

/// A SNAPSHOT'S TEXT READS A ROW CHANGED OFF THE LIST'S SCREEN AS IT IS NOW, as `inspect` does:
/// the change asked for no frame, so the compile glass shows still says the row's old words, and
/// a sidecar read from it would contradict `inspect` until the list's next raster. NEGATIVE
/// CONTROL: the compile glass shows does not read the change.
#[test]
fn a_snapshots_text_reads_a_row_changed_off_screen() {
    let (mut app, view, first, _last, now) = a_list_past_its_first_row();
    let text = |app: &App| {
        app.snapshot_visible_text(
            WID,
            app.active_visible_content_route(WID).expect("a route"),
            &app.active_visible_leaf_plan(WID).expect("a plan"),
            &app.windows[&WID].input_scratch,
            usize::from(app.tab_strip_rows),
            usize::from(app.windows[&WID].cols),
        )
    };
    let row = format!("text key=\"navigator/row/{first}\"");
    let before = text(&app);
    assert!(
        before.contains(&row) && !before.contains("ledger"),
        "PRECONDITION: the snapshot lists the row off the screen: {before}"
    );

    assert_eq!(
        app.write_session_title_pin(first, "ledger").ok(),
        Some(true)
    );
    app.navigator_mark_row(first);
    app.publish_navigators(now + Duration::from_secs(5));
    assert!(
        !drawn_label(&app, view, first).contains("ledger"),
        "NEGATIVE CONTROL: the page on glass is the one it was"
    );
    let after = text(&app);
    assert!(
        after
            .lines()
            .any(|line| line.contains(&row) && line.contains("ledger")),
        "the snapshot reads the row as it is now: {after}"
    );
}

/// A CLIENT THAT BEGINS READING A WINDOW HEARS ITS ROWS AS THEY ARE: a change off the list's
/// screen made while no client read the tree asked for no frame, so the client's arrival owes
/// the least frame, and the tree it is sent reads the change. NEGATIVE CONTROL: the frame on
/// glass did not.
#[cfg(a11y_tree)]
#[test]
fn a_client_that_begins_reading_hears_a_row_changed_off_screen() {
    let (mut app, view, first, _last, now) = a_list_past_its_first_row();
    assert_eq!(
        app.write_session_title_pin(first, "ledger").ok(),
        Some(true)
    );
    app.navigator_mark_row(first);
    let asked = redraws(&app);
    app.publish_navigators(now + Duration::from_secs(5));
    assert_eq!(
        redraws(&app),
        asked,
        "PRECONDITION: no frame asked for while no client reads"
    );
    assert!(
        !drawn_label(&app, view, first).contains("ledger"),
        "NEGATIVE CONTROL: the page on glass is the one it was"
    );

    let winit_id = aterm_window::window::WindowId::from(1u64);
    app.winit_to_window.insert(winit_id, WID);
    app.windows
        .get_mut(&WID)
        .expect("the window")
        .a11y_sent
        .clear();
    app.on_accessibility_event(crate::a11y_adapter::Event {
        window_id: winit_id,
        window_event: crate::a11y_adapter::WindowEvent::InitialTreeRequested,
    });
    assert_eq!(
        app.windows[&WID].leaf_render_cache[&view].native_damage,
        Some(crate::app_native::TREE_ONLY),
        "the least frame is owed"
    );
    assert_eq!(redraws(&app), asked + 1, "and asked for");
    let sent = &app.windows[&WID].a11y_sent;
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0]
            .nodes
            .iter()
            .any(|(_, node)| node.label().is_some_and(|label| label.contains("ledger"))),
        "the tree the client is sent reads the change"
    );
}

/// A CHANGE A VIEW LISTS OFF ITS SCREEN, WITH A CLIENT READING THE WINDOW'S TREE, IS TOLD BY THE
/// LEAST FRAME AND LEAVES THE FRAME THE POINTER PRESSES: the row's pixels are not on the glass, so
/// the view keeps its revision, `inspect` reads the new title before any frame, and the one frame
/// it asks for rasters one pixel and moves none. After it the frame on glass still routes the
/// pointer: a press and release on a row goes to its session.
#[cfg(a11y_tree)]
#[test]
fn a_change_off_screen_tells_a_reading_client_and_keeps_the_frame_the_pointer_presses() {
    use aterm_window::event::{ElementState, MouseButton};

    let (mut app, view, first, last, now) = a_list_past_its_first_row();
    app.windows.get_mut(&WID).expect("the window").a11y_active = true;
    let shown = presentation_revision(&app, view);
    let (wholes, pixels) = (
        raster(&app, view).full_rasters,
        raster(&app, view).rgba.clone(),
    );

    assert_eq!(
        app.write_session_title_pin(first, "ledger").ok(),
        Some(true)
    );
    app.navigator_mark_row(first);
    let asked = redraws(&app);
    app.publish_navigators(now + Duration::from_secs(5));
    assert_eq!(redraws(&app), asked + 1, "one frame asked for");
    assert_eq!(
        presentation_revision(&app, view),
        shown,
        "the view's revision stays: the raster on glass is still its page"
    );
    assert!(
        inspect_text(&mut app, view)
            .iter()
            .any(|line| line.contains("ledger")),
        "inspect reads the row as it is now"
    );
    assert_eq!(
        app.windows[&WID].leaf_render_cache[&view].native_damage,
        Some(DamageRegion::Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1
        }),
        "the page's tree moved and no pixel did: the least frame is asked for"
    );
    assert!(app.commit_glassless_native_frame(WID));
    let scale = app.windows[&WID].scale.ceil() as u64;
    match raster(&app, view).last_work {
        crate::NativeRasterWork::Region { pixels, .. } => assert!(
            pixels <= scale * scale,
            "one logical pixel rastered, not {pixels}"
        ),
        other => panic!("one pixel rastered, not {other:?}"),
    }
    assert_eq!(raster(&app, view).full_rasters, wholes, "no whole raster");
    assert!(raster(&app, view).rgba == pixels, "and no pixel moved");

    // The pointer on a row of the frame: hover lights it, a press and release goes to it.
    let key = format!("navigator/row/{last}");
    let (x, y) = middle_of(&app, view, &key);
    app.on_cursor_moved(WID, x, y);
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.as_ref())
            .map(crate::native_ui::UiKey::as_str),
        Some(key.as_str()),
        "the pointer lights the row it is over"
    );
    assert_eq!(active_session(&app, WID), None, "PRECONDITION");
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        active_session(&app, WID),
        Some(last),
        "the first click on a row goes to its session"
    );
}

/// A CHANGE A VIEW DOES NOT LIST ASKS IT FOR NOTHING: a session under a closed row is no node of
/// the page, which compiles as it did. No frame asked for, no revision, no paint, and a frame that
/// comes for another cause compiles nothing. NEGATIVE CONTROL: the list did take the change.
#[test]
fn a_change_under_a_closed_row_asks_the_view_for_nothing() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let lead = active_session(&app, WID).expect("the first tab");
    let person = person_tab(&mut app);
    let worker = land_worker(&mut app);
    let from_lead = sid_of(&app, lead);
    mark(
        &mut app,
        worker,
        TaskState::Working,
        Some(from_lead.clone()),
    );
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    assert_eq!(
        listed(&app, view),
        [lead, person],
        "PRECONDITION: the worker is under its lead's closed row"
    );
    paint(&mut app);
    app.publish_navigators(now + Duration::from_secs(2));
    app.publish_navigators(now + Duration::from_secs(4));
    paint(&mut app);
    let (shown, painted) = (presentation_revision(&app, view), paint_serial(&app, view));

    mark(&mut app, worker, TaskState::Done, Some(from_lead));
    let asked = redraws(&app);
    let compiles = crate::app_native::native_ui_compile_count();
    app.publish_navigators(now + Duration::from_secs(5));
    let asked_after = redraws(&app);
    assert_eq!(
        row_of(&app, worker).expect("listed").word,
        Word::Quiet,
        "NEGATIVE CONTROL: the list took the change"
    );
    // The count is read across a frame: with none between, a change that owed the page a
    // compile would leave it where it was.
    paint(&mut app);
    assert_eq!(
        crate::app_native::native_ui_compile_count(),
        compiles,
        "the frame after it compiles nothing"
    );
    assert_eq!(asked_after, asked, "no frame asked for");
    assert_eq!(presentation_revision(&app, view), shown);
    assert_eq!(paint_serial(&app, view), painted, "no paint asked for");
}

/// A MARK FOR A SESSION NO TAB HOLDS BUILDS NOTHING: no row, no arrangement, no new list.
#[test]
fn a_mark_for_a_session_no_tab_holds_builds_nothing() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let _second = person_tab(&mut app);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let spare = app.next_session_id;
    app.next_session_id += 1;
    let session = stub_session(spare);
    App::register_session(&app.store, &session, None);
    let (work, tree) = (app.navigator_work(), app.navigator_tree().expect("a list"));
    app.navigator_mark_row(spare);
    app.publish_navigators(now + Duration::from_secs(1));
    assert_eq!(app.navigator_work(), work, "no row is built for it");
    assert!(Arc::ptr_eq(&tree, &app.navigator_tree().expect("the list")));
}

/// A TAB THAT OPENS REBUILDS ITS OWN ROW AND NO OTHER: at the strip's end every other tab keeps
/// its place, and a row reads nothing else of the walk. NEGATIVE CONTROL: a tab moved to the
/// strip's front moves every place, and each row is read again.
#[test]
fn a_tab_that_opens_builds_its_own_row_and_no_other() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let _second = person_tab(&mut app);
    let third = person_tab(&mut app);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    let work = app.navigator_work();
    let fourth = person_tab(&mut app);
    app.publish_navigators(now + Duration::from_secs(1));
    assert!(listed(&app, view).contains(&fourth));
    assert_eq!(
        app.navigator_work().rows_built,
        work.rows_built + 1,
        "the new session's row and no other"
    );
    let work = app.navigator_work();
    let third_at = tab_index_of_session(&app, WID, third);
    app.move_tab(WID, third_at, 0);
    app.sync_window(WID);
    app.publish_navigators(now + Duration::from_secs(2));
    assert!(
        app.navigator_work().rows_built >= work.rows_built + 3,
        "NEGATIVE CONTROL: the tabs whose place moved are read again"
    );
}

/// THE LIST IS OF LIVE SESSIONS: a pane kept open on a program that exited (`--hold`) is no row.
#[test]
fn an_exit_in_a_kept_pane_leaves_the_list() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    let second = person_tab(&mut app);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    app.hold = true;
    assert!(listed(&app, view).contains(&second));
    let _ = app.exit_session_logical(second);
    app.publish_navigators(now + Duration::from_secs(1));
    assert!(
        app.window_contains_session(WID, second),
        "PRECONDITION: its pane is kept"
    );
    assert!(
        !listed(&app, view).contains(&second),
        "an ended program is no row"
    );
    assert!(listed(&app, view).contains(&first));
}

/// A SCREEN THAT CANNOT BE READ NOW SAYS NOTHING OF WHAT IT HOLDS: while a session's terminal
/// is held (its reader is feeding it), the row keeps the line it has; the next step reads the
/// new one.
#[test]
fn a_screen_that_cannot_be_read_keeps_the_rows_line() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    let term = app.pool.get(first).expect("the shell").term.clone();
    let feed = |app: &App, text: &[u8]| {
        let mut term = crate::term_lock(&term);
        term.process(text);
        let screen = crate::control::screen_gen(&term);
        drop(term);
        let _ = app
            .pool
            .get(first)
            .expect("the shell")
            .ctx
            .screen_moves
            .fed(screen);
    };
    feed(&app, b"build ok\r\n");
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let line = |app: &App| row_of(app, first).expect("listed").line.clone();
    assert!(
        line(&app).is_some_and(|line| line.contains("build ok")),
        "PRECONDITION: the row reads the screen's last line, not {:?}",
        line(&app)
    );

    feed(&app, b"tests ok\r\n");
    let held = crate::term_lock(&term);
    app.navigator_mark_words(first);
    app.publish_navigators(now + Duration::from_secs(2));
    assert!(
        line(&app).is_some_and(|line| line.contains("build ok")),
        "the line is kept while the screen cannot be read, not {:?}",
        line(&app)
    );
    drop(held);
    assert!(app.navigator_words_wake().is_some(), "and the read is owed");
    app.publish_navigators(now + Duration::from_secs(4));
    assert!(
        line(&app).is_some_and(|line| line.contains("tests ok")),
        "{:?}",
        line(&app)
    );
}

/// THE SELECTION IS THE KEYBOARD'S ONLY IN THE KEY WINDOW: a list in a window the keys left
/// takes the strip's order, as one whose tab lost them does. NEGATIVE CONTROL: with the keys it
/// held its order through the same move.
#[test]
fn a_list_in_a_window_that_is_not_key_does_not_hold_its_order() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let x = active_session(&app, WID).expect("the first tab");
    let y = person_tab(&mut app);
    let z = person_tab(&mut app);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    let z_at = tab_index_of_session(&app, WID, z);
    app.move_tab(WID, z_at, 0);
    app.sync_window(WID);
    app.publish_navigators(now);
    assert_eq!(listed(&app, view), [x, y, z], "NEGATIVE CONTROL: held");
    app.on_focus(WID, false);
    app.publish_navigators(now);
    assert_eq!(listed(&app, view), [z, x, y]);
}

/// A SPLIT NEVER RE-GRIDS A SESSION ANOTHER VIEW SHOWS, whichever pane of the tab holds it: the
/// navigator opens as a tab then.
#[test]
fn a_tab_with_a_shared_session_in_any_pane_gets_a_tab_not_a_split() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let shared = active_session(&app, WID).expect("the first tab");
    app.frontmost_window = Some(WID);
    let _second = app
        .open_active_session_in_new_window_logical()
        .expect("the session shown in a second window");
    app.frontmost_window = Some(WID);
    let own = app.split_active_stub_tab(WID);
    app.sync_window(WID);
    assert_eq!(
        active_session(&app, WID),
        Some(own),
        "PRECONDITION: the keys are in the pane nobody else shows"
    );
    assert!(app.pool.views(shared).is_some_and(|views| views > 1));
    assert_eq!(app.open_navigator(true), Ok(false));
}

/// The title the strip and every other surface give `session` now.
fn label(app: &App, session: u64) -> String {
    app.tab_label_of_session(session)
}

/// A ROW IS MARKED WHEN ITS TAB'S PLACE MOVED, whichever tab moved it: a page's tab that moves
/// across the terminals' leaves their order as it was and gives each a new place, which names a
/// session nothing has titled. Each is read again and reads as its tab is labelled.
#[test]
fn a_place_that_moved_under_an_equal_order_reads_its_row_again() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    let second = person_tab(&mut app);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    let places = |app: &App| [first, second].map(|session| tab_index_of_session(app, WID, session));
    let order = app.navigator_tree().expect("a list").sessions();
    let (was, work) = (places(&app), app.navigator_work());

    // The navigator's own tab, moved to the strip's front.
    let page_at = tab_index_of(&app, WID, view);
    app.move_tab(WID, page_at, 0);
    app.sync_window(WID);
    app.publish_navigators(now + Duration::from_secs(1));
    assert_ne!(
        places(&app),
        was,
        "PRECONDITION: each terminal's place moved"
    );
    assert_eq!(
        app.navigator_tree().expect("the list").sessions(),
        order,
        "PRECONDITION: and their order did not"
    );
    assert_eq!(
        app.navigator_work().rows_built,
        work.rows_built + 2,
        "each row whose place moved is read again"
    );
    for session in [first, second] {
        assert_eq!(
            row_of(&app, session).expect("listed").title,
            label(&app, session)
        );
    }
}

/// A PARKED SESSION'S ROW READS NO PLACE: a tab that opens ahead of the parked ones moves each
/// of them along the strip and builds its own row alone.
#[test]
fn a_tab_that_opens_ahead_of_parked_ones_builds_no_parked_row() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let _first = active_session(&app, WID).expect("the first tab");
    let workers = [land_worker(&mut app), land_worker(&mut app)];
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let places = |app: &App| workers.map(|session| tab_index_of_session(app, WID, session));
    let (was, work) = (places(&app), app.navigator_work());

    let opened = person_tab(&mut app);
    app.publish_navigators(now + Duration::from_secs(1));
    assert_ne!(
        places(&app),
        was,
        "PRECONDITION: the parked tabs moved along"
    );
    assert!(row_of(&app, opened).is_some());
    assert_eq!(
        app.navigator_work().rows_built,
        work.rows_built + 1,
        "the tab that opened, and no parked row"
    );
}

/// A ROW IS MARKED WHEN ITS TAB IS KEPT: a worker kept as a tab is titled as a person's tab is,
/// so its row is read again though its tab stayed where it stood, and the worker still parked
/// is not.
#[test]
fn a_worker_kept_as_a_tab_reads_its_row_again() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let _first = active_session(&app, WID).expect("the first tab");
    let kept = land_worker(&mut app);
    let parked = land_worker(&mut app);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let (at, work) = (tab_index_of_session(&app, WID, kept), app.navigator_work());

    assert!(app.keep_as_tab(kept));
    app.publish_navigators(now + Duration::from_secs(1));
    assert!(!app.session_is_parked(kept) && app.session_is_parked(parked));
    assert_eq!(
        tab_index_of_session(&app, WID, kept),
        at,
        "PRECONDITION: the first parked tab is kept where it stands"
    );
    assert_eq!(
        app.navigator_work().rows_built,
        work.rows_built + 1,
        "the kept worker's row, and not the parked one's"
    );
    assert_eq!(row_of(&app, kept).expect("listed").title, label(&app, kept));
}

/// Feed `bytes` to `session`'s terminal as its reader does.
fn fed(app: &App, session: u64, bytes: &[u8]) {
    let live = app.pool.get(session).expect("a pooled session");
    let mut term = crate::term_lock(&live.term);
    term.process(bytes);
    let moved = live.ctx.screen_moves.fed(crate::control::screen_gen(&term));
    live.ctx.job_feed.fed(&term, bytes, moved, Instant::now);
}

/// [`fed`], and wake the loop for the output.
fn feed(app: &mut App, session: u64, bytes: &[u8], at: Instant) {
    fed(app, session, bytes);
    app.on_output_wake(session, at);
}

/// What `session`'s reader's wake for a chunk it just fed does with the session's output latch
/// (`spawn::gated_output_wake`), a post counted as sent.
fn reader_wake(app: &App, session: u64) -> crate::spawn::WakeGate {
    let live = app.pool.get(session).expect("a pooled session");
    crate::spawn::gated_output_wake_for_test(&live.output_wake_pending, 1, || true)
}

/// Run `session`'s output pass at `at` for a wake its reader posted for `first`, with `late`
/// fed after the pass began and before it stored the output latch. The reader's wake for
/// `late` finds the latch still armed and posts nothing, so no other pass follows for it.
fn pass_with_a_chunk_fed_before_the_latch_store(
    app: &mut App,
    session: u64,
    first: &[u8],
    late: &'static [u8],
    at: Instant,
) {
    use crate::spawn::WakeGate;
    fed(app, session, first);
    assert_eq!(
        reader_wake(app, session),
        WakeGate::Posted,
        "PRECONDITION: the first chunk posts the wake this pass answers"
    );
    let gate = std::rc::Rc::new(std::cell::Cell::new(None));
    crate::vitals_host::BEFORE_LATCH_CLEAR.with_borrow_mut(|slot| {
        let gate = std::rc::Rc::clone(&gate);
        *slot = Some(Box::new(move |app, session| {
            fed(app, session, late);
            gate.set(Some(reader_wake(app, session)));
        }));
    });
    app.on_output_wake(session, at);
    crate::vitals_host::BEFORE_LATCH_CLEAR.with_borrow_mut(|slot| *slot = None);
    assert_eq!(
        gate.get(),
        Some(WakeGate::Coalesced),
        "PRECONDITION: the late chunk rides the wake already posted"
    );
    assert_eq!(
        app.pool
            .get(session)
            .expect("a pooled session")
            .output_wake_pending
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "PRECONDITION: the pass left the latch clear, with no hold to release for the late chunk"
    );
}

/// A headless app with Tab Status off and the navigator published, and its first tab's session:
/// a shell at its prompt, whose row reads `idle`. The prompt's own output pass has run, so a
/// later pass finds the session's title as the list read it and marks no row for it.
fn shell_at_its_prompt_by_its_own_word(now: Instant) -> (App, u64) {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let shell = active_session(&app, WID).expect("the first tab");
    app.config.tab_status = Some(false);
    app.reconfigure_session_status();
    assert_eq!(app.open_navigator(false), Ok(false));
    feed(&mut app, shell, b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\", now);
    app.publish_navigators(now);
    assert_eq!(row_of(&app, shell).expect("listed").word, Word::Idle);
    (app, shell)
}

/// A COMMAND'S START FED AS THE OUTPUT PASS BEGINS IS READ BY THAT PASS: with Tab Status off the
/// row reads the shell's own word, and the reader's wake for a chunk fed before the pass stores
/// the output latch posts nothing, so that pass is the only one that can read it.
#[test]
fn a_command_start_fed_before_the_latch_store_is_read_by_that_pass() {
    let now = Instant::now();
    let (mut app, shell) = shell_at_its_prompt_by_its_own_word(now);
    let work = app.navigator_work();

    let started = now + Duration::from_secs(1);
    pass_with_a_chunk_fed_before_the_latch_store(
        &mut app,
        shell,
        b"sleep 100\r\n",
        b"\x1b]133;C\x1b\\",
        started,
    );
    app.publish_navigators(started);
    assert_eq!(
        row_of(&app, shell).expect("listed").word,
        Word::Working,
        "the command started"
    );
    assert_eq!(
        app.navigator_work().rows_built,
        work.rows_built + 1,
        "the shell's row, read once"
    );
}

/// A COMMAND'S END FED AS THE OUTPUT PASS BEGINS IS READ BY THAT PASS: the mirror of the start.
/// Unread, the row would keep `working` on a shell at its prompt.
#[test]
fn a_command_end_fed_before_the_latch_store_is_read_by_that_pass() {
    let now = Instant::now();
    let (mut app, shell) = shell_at_its_prompt_by_its_own_word(now);
    let started = now + Duration::from_secs(1);
    feed(&mut app, shell, b"sleep 100\r\n\x1b]133;C\x1b\\", started);
    app.publish_navigators(started);
    assert_eq!(
        row_of(&app, shell).expect("listed").word,
        Word::Working,
        "PRECONDITION: a command is running"
    );

    let ended = now + Duration::from_secs(2);
    pass_with_a_chunk_fed_before_the_latch_store(
        &mut app,
        shell,
        b"\r\n",
        b"\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\",
        ended,
    );
    app.publish_navigators(ended);
    assert_eq!(
        row_of(&app, shell).expect("listed").word,
        Word::Idle,
        "back at its prompt"
    );
}

/// WITH TAB STATUS OFF A SHELL'S COMMAND STILL READS `working`: nothing observes the session
/// then, and the row reads the shell's own word, which arrives as output and marks the row.
#[test]
fn with_tab_status_off_a_shells_command_reads_working_by_its_own_word() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let shell = active_session(&app, WID).expect("the first tab");
    app.config.tab_status = Some(false);
    app.reconfigure_session_status();
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let word = |app: &App| row_of(app, shell).expect("listed").word;
    assert_eq!(word(&app), Word::Idle);

    feed(
        &mut app,
        shell,
        b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\make\r\n\x1b]133;C\x1b\\",
        now + Duration::from_secs(1),
    );
    assert!(
        app.session_status.status(shell).is_none(),
        "PRECONDITION: nothing observes the session"
    );
    app.publish_navigators(now + Duration::from_secs(1));
    assert_eq!(word(&app), Word::Working, "a command is running");
    feed(
        &mut app,
        shell,
        b"\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\",
        now + Duration::from_secs(2),
    );
    app.publish_navigators(now + Duration::from_secs(2));
    assert_eq!(word(&app), Word::Idle, "back at its prompt");
}

/// WITH TAB STATUS OFF AN AGENT BETWEEN TURNS READS `quiet` BY ITS OWN WORD: its shell says a
/// command runs for as long as the agent lives, and the agent's own report (OSC 7501) says its
/// turn ended, which arrives as output and marks the row. NEGATIVE CONTROL: its next turn reads
/// `working`, and once it leaves (`state=clear`) the row reads its shell's word again.
#[test]
fn with_tab_status_off_an_agent_waiting_reads_quiet_by_its_own_word() {
    let now = Instant::now();
    let (mut app, shell) = shell_at_its_prompt_by_its_own_word(now);
    let word = |app: &App| row_of(app, shell).expect("listed").word;
    let at = |s: u64| now + Duration::from_secs(s);
    feed(&mut app, shell, b"claude\r\n\x1b]133;C\x1b\\", at(1));
    feed(
        &mut app,
        shell,
        b"\x1b]7501;state=working:app=claude-code\x1b\\",
        at(2),
    );
    app.publish_navigators(at(2));
    assert_eq!(
        word(&app),
        Word::Working,
        "PRECONDITION: a turn in progress"
    );

    feed(
        &mut app,
        shell,
        b"\x1b]7501;state=idle:app=claude-code\x1b\\",
        at(3),
    );
    assert!(
        app.pool.get(shell).expect("the shell").ctx.job_feed.runs(),
        "PRECONDITION: its shell still says the agent runs"
    );
    assert!(
        app.session_status.status(shell).is_none(),
        "PRECONDITION: nothing observes the session"
    );
    app.publish_navigators(at(3));
    assert_eq!(
        word(&app),
        Word::Quiet,
        "its turn ended: at rest, though its process lives"
    );

    feed(
        &mut app,
        shell,
        b"\x1b]7501;state=working:app=claude-code\x1b\\",
        at(4),
    );
    app.publish_navigators(at(4));
    assert_eq!(word(&app), Word::Working, "NEGATIVE CONTROL: its next turn");
    feed(
        &mut app,
        shell,
        b"\x1b]7501;state=clear\x1b\\\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\",
        at(5),
    );
    app.publish_navigators(at(5));
    assert_eq!(
        word(&app),
        Word::Idle,
        "the agent left: its shell at its prompt"
    );
}

/// WITH TAB STATUS OFF AN AGENT MID-TURN READS `working` BY ITS OWN WORD, in a shell that says
/// nothing of itself (no OSC 133 marks): the agent's report is the fact, and it marks the row.
/// NEGATIVE CONTROL: the same session before the agent reports reads `idle`.
#[test]
fn with_tab_status_off_an_agent_mid_turn_reads_working_by_its_own_word() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let shell = active_session(&app, WID).expect("the first tab");
    app.config.tab_status = Some(false);
    app.reconfigure_session_status();
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    feed(&mut app, shell, b"$ claude\r\n", now);
    app.publish_navigators(now);
    let word = |app: &App| row_of(app, shell).expect("listed").word;
    assert_eq!(
        word(&app),
        Word::Idle,
        "NEGATIVE CONTROL: no word of its own"
    );
    assert!(
        !app.pool.get(shell).expect("the shell").ctx.job_feed.runs(),
        "PRECONDITION: its shell says nothing of a command"
    );

    let turn = now + Duration::from_secs(1);
    feed(
        &mut app,
        shell,
        b"\x1b]7501;state=working:app=claude-code\x1b\\",
        turn,
    );
    app.publish_navigators(turn);
    assert_eq!(
        word(&app),
        Word::Working,
        "a turn in progress, by the agent's own word"
    );
}

/// WITH TAB STATUS OFF AN AGENT THAT REPORTS UNDER ITS OWN APP WORD READS BY IT: Kimi Code and
/// Muse Code name themselves (`app=kimi-code`, `app=muse`), and the engine keeps such a report
/// apart from the known programs' (`ProgramStateReport::other`). Between its turns the row reads
/// `quiet`, though its shell says it runs. NEGATIVE CONTROL: its turn reads `working`, and once it
/// leaves with no `clear`, its shell's prompt takes the report down and the row reads `idle`.
#[test]
fn with_tab_status_off_an_agent_under_its_own_app_word_reads_quiet_by_it() {
    let now = Instant::now();
    let (mut app, shell) = shell_at_its_prompt_by_its_own_word(now);
    let word = |app: &App| row_of(app, shell).expect("listed").word;
    let at = |s: u64| now + Duration::from_secs(s);
    feed(&mut app, shell, b"kimi\r\n\x1b]133;C\x1b\\", at(1));
    feed(
        &mut app,
        shell,
        b"\x1b]7501;state=working:app=kimi-code\x1b\\",
        at(2),
    );
    app.publish_navigators(at(2));
    assert_eq!(
        word(&app),
        Word::Working,
        "NEGATIVE CONTROL: a turn in progress"
    );

    feed(
        &mut app,
        shell,
        b"\x1b]7501;state=idle:app=kimi-code\x1b\\",
        at(3),
    );
    let live = app.pool.get(shell).expect("the shell");
    let report = crate::term_lock(&live.term).program_state();
    assert!(
        report.state.is_none() && report.other.state.is_some(),
        "PRECONDITION: the engine holds the report apart from the known programs': {report:?}"
    );
    assert!(
        live.ctx.job_feed.runs(),
        "PRECONDITION: its shell still says the agent runs"
    );
    assert!(
        app.session_status.status(shell).is_none(),
        "PRECONDITION: nothing observes the session"
    );
    app.publish_navigators(at(3));
    assert_eq!(
        word(&app),
        Word::Quiet,
        "its turn ended: at rest, though its process lives"
    );

    feed(
        &mut app,
        shell,
        b"\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\",
        at(4),
    );
    app.publish_navigators(at(4));
    assert_eq!(
        word(&app),
        Word::Idle,
        "NEGATIVE CONTROL: the agent left: its shell at its prompt"
    );
}

/// WITH TAB STATUS OFF A REPORT AN AGENT LEFT STANDING IS NO WORD AT ITS SHELL'S PROMPT: an agent
/// killed mid-turn writes no `clear`, and the engine keeps a known program's report standing
/// under the shell. Where the host names no front group (Windows, and this headless reader) or a
/// handoff carried the report, the engine cannot tell the shell from the agent; the shell's own
/// marks can: at its prompt no program is in front of it, and the row reads `idle`. NEGATIVE
/// CONTROL: once its shell runs a command again (a stopped agent's `fg`), the report is that
/// program's word again.
#[test]
fn with_tab_status_off_a_report_left_standing_is_no_word_at_the_shells_prompt() {
    let now = Instant::now();
    let (mut app, shell) = shell_at_its_prompt_by_its_own_word(now);
    let word = |app: &App| row_of(app, shell).expect("listed").word;
    let at = |s: u64| now + Duration::from_secs(s);
    feed(&mut app, shell, b"claude\r\n\x1b]133;C\x1b\\", at(1));
    feed(
        &mut app,
        shell,
        b"\x1b]7501;state=working:app=claude-code\x1b\\",
        at(2),
    );
    app.publish_navigators(at(2));
    assert_eq!(
        word(&app),
        Word::Working,
        "PRECONDITION: a turn in progress"
    );

    feed(
        &mut app,
        shell,
        b"\x1b]133;D;137\x1b\\\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\",
        at(3),
    );
    let live = app.pool.get(shell).expect("the shell");
    assert!(
        crate::term_lock(&live.term)
            .program_state()
            .sign()
            .is_some(),
        "PRECONDITION: the killed agent's report stands as the engine's run sign"
    );
    assert!(
        !live.ctx.job_feed.runs(),
        "PRECONDITION: its shell says nothing runs"
    );
    app.publish_navigators(at(3));
    assert_eq!(
        word(&app),
        Word::Idle,
        "the shell at its prompt: no program in front of it"
    );

    feed(&mut app, shell, b"fg\r\n\x1b]133;C\x1b\\", at(4));
    app.publish_navigators(at(4));
    assert_eq!(
        word(&app),
        Word::Working,
        "NEGATIVE CONTROL: its shell runs a program again, whose report stands"
    );
}

/// TURNING TAB STATUS ON OR OFF MARKS THE ROWS: what it observed of a shell is not kept once it
/// is off, where each row reads the shell's own word. NEGATIVE CONTROL: on, the row read
/// `working` for a command the shell's reader never fed.
#[test]
fn turning_tab_status_off_reads_each_row_by_the_shells_own_word() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let shell = active_session(&app, WID).expect("the first tab");
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let word = |app: &App| row_of(app, shell).expect("listed").word;
    let term = app.pool.get(shell).expect("the shell").term.clone();
    crate::term_lock(&term).process(b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\make\r\n\x1b]133;C\x1b\\");
    observe_statuses(&mut app, now);
    observe_statuses(&mut app, now + Duration::from_secs(1));
    app.publish_navigators(now + Duration::from_secs(1));
    assert_eq!(word(&app), Word::Working, "NEGATIVE CONTROL: as observed");

    app.config.tab_status = Some(false);
    app.reconfigure_session_status();
    app.publish_navigators(now + Duration::from_secs(2));
    assert_eq!(
        word(&app),
        Word::Idle,
        "off, the row is read again, by the shell's own word"
    );
}

/// A KEY DRIVEN INTO A WINDOW THAT IS NOT THE KEY ONE KEEPS ITS RING AND COSTS ITS BAND: the
/// window's keys go to the list whether or not a person's keyboard is on it, so the pass after
/// the key takes nothing back. NEGATIVE CONTROL: the window's keys going to another tab does.
#[test]
fn a_key_driven_into_a_window_that_is_not_key_keeps_its_ring() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    let second = person_tab(&mut app);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    app.on_focus(WID, false);
    app.publish_navigators(now);
    paint(&mut app);
    let ring = |app: &App| {
        app.native_runtime
            .view_state(view)
            .expect("the view")
            .common()
            .focus_visible
    };
    assert!(!ring(&app), "PRECONDITION: no key has reached the list");
    let (shown, wholes) = (
        presentation_revision(&app, view),
        raster(&app, view).full_rasters,
    );

    assert_eq!(app.navigator_selection(view), Some(second));
    assert!(app.native_input_event(WID, &press(NamedKey::ArrowUp)));
    assert_eq!(app.navigator_selection(view), Some(first));
    // The loop turn the key ends in: its dispatch marked the strip, so the structure is read.
    app.navigator_mark_structure();
    app.publish_navigators(now);
    assert!(ring(&app), "the ring the key drew stays");
    assert_eq!(presentation_revision(&app, view), shown);
    paint(&mut app);
    assert!(
        matches!(
            raster(&app, view).last_work,
            crate::NativeRasterWork::Region { .. }
        ),
        "the key's band, not {:?}",
        raster(&app, view).last_work
    );
    assert_eq!(raster(&app, view).full_rasters, wholes, "no whole raster");

    app.switch_tab_in(WID, tab_index_of_session(&app, WID, second));
    app.sync_window(WID);
    app.publish_navigators(now);
    assert!(
        !ring(&app),
        "NEGATIVE CONTROL: the window's keys left the list"
    );
}

/// A VIEW UNCOVERED BESIDE ONE THAT STAYED IN SIGHT IS DRAWN AGAIN: the ages were worded for
/// the view that showed while this one was covered, so its raster holds the ages of the minute it
/// was covered at. Uncovered, it is drawn whole and asked for a frame. NEGATIVE CONTROL: covered
/// and uncovered inside one minute, it keeps the frame it has.
#[test]
fn a_view_uncovered_beside_one_in_sight_is_drawn_with_the_ages_of_now() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    assert_eq!(app.open_navigator(false), Ok(false));
    let other = WindowId(1);
    let c = app.next_session_id;
    app.next_session_id += 1;
    let session = stub_session(c);
    App::register_session(&app.store, &session, None);
    app.install_window_state(other, session, 38, 132);
    app.sync_window(other);
    app.frontmost_window = Some(other);
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let covered = view_in(&app, other);
    assert_ne!(covered, view_in(&app, WID), "one view in each window");
    let asked = |app: &App| {
        app.windows[&other]
            .leaf_render_cache
            .get(&covered)
            .map_or(0, |cache| cache.native_paint_serial)
    };

    app.on_occlusion_changed(other, true, std::time::Instant::now());
    app.publish_navigators(now + Duration::from_secs(1));
    let (shown, painted) = (presentation_revision(&app, covered), asked(&app));
    app.on_occlusion_changed(other, false, std::time::Instant::now());
    app.publish_navigators(now + Duration::from_secs(2));
    assert_eq!(
        (presentation_revision(&app, covered), asked(&app)),
        (shown, painted),
        "NEGATIVE CONTROL: no minute passed, and the view keeps its frame"
    );

    app.on_occlusion_changed(other, true, std::time::Instant::now());
    app.publish_navigators(now + Duration::from_secs(3));
    assert!(
        app.navigator_wake().is_some(),
        "PRECONDITION: the view in sight keeps the ages' minute armed"
    );
    app.publish_navigators(now + Duration::from_secs(70));
    app.on_occlusion_changed(other, false, std::time::Instant::now());
    app.publish_navigators(now + Duration::from_secs(71));
    assert_ne!(
        presentation_revision(&app, covered),
        shown,
        "the page is drawn whole"
    );
    assert_eq!(
        app.windows[&other].leaf_render_cache[&covered].native_damage,
        Some(DamageRegion::All)
    );
    assert_ne!(asked(&app), painted, "and a frame is asked for");
}

/// A COVERED VIEW'S PAGE IS THE LIST AS IT STANDS, AFTER EACH CHANGE: a covered window still
/// draws for a client that waits on a frame (`image`), and that frame keeps the page it compiled.
/// A row that changes after it moves the view's revision again, so `inspect` compiles the list
/// as it is now and does not answer with the page of that frame.
#[test]
fn a_covered_view_that_drew_a_frame_reads_each_later_change() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = active_session(&app, WID).expect("the first tab");
    assert_eq!(app.open_navigator(false), Ok(false));
    let now = Instant::now();
    app.publish_navigators(now);
    let view = view_in(&app, WID);
    paint(&mut app);
    app.on_occlusion_changed(WID, true, std::time::Instant::now());
    app.publish_navigators(now + Duration::from_secs(1));
    let says = |app: &mut App, title: &str| {
        inspect_text(app, view)
            .iter()
            .any(|line| line.contains(title))
    };

    assert_eq!(
        app.write_session_title_pin(first, "ledger").ok(),
        Some(true)
    );
    app.navigator_mark_row(first);
    app.publish_navigators(now + Duration::from_secs(2));
    paint(&mut app);
    assert!(
        says(&mut app, "ledger"),
        "PRECONDITION: the covered window's frame compiled the first change"
    );

    assert_eq!(
        app.write_session_title_pin(first, "journal").ok(),
        Some(true)
    );
    app.navigator_mark_row(first);
    app.publish_navigators(now + Duration::from_secs(3));
    assert_eq!(
        row_of(&app, first).expect("listed").title,
        "journal",
        "PRECONDITION: the list took the second change"
    );
    assert!(
        says(&mut app, "journal"),
        "inspect reads the row as it is now, not the page of the covered frame"
    );
}

#[test]
fn a_delivered_machine_title_updates_the_page_once_without_a_poll() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    app.open_navigator(false).expect("the list");
    let now = Instant::now();
    app.publish_navigators(now);
    app.navigator_title_ready("m42 Lark".into());
    app.publish_navigators(now + Duration::from_secs(1));
    assert_eq!(app.navigator_tree().unwrap().machine, "m42 Lark");
    let tree = app.navigator_tree().unwrap();
    let work = app.navigator_work();
    let wake = app.navigator_wake();
    app.navigator_title_ready("m42 Lark".into());
    app.publish_navigators(now + Duration::from_secs(1));
    assert!(Arc::ptr_eq(&tree, &app.navigator_tree().unwrap()));
    assert_eq!(
        app.navigator_work(),
        work,
        "an unchanged title publishes nothing"
    );
    assert_eq!(
        app.navigator_wake(),
        wake,
        "title delivery adds no polling deadline"
    );
}
