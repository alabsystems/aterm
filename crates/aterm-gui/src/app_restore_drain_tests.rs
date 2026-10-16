// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE COLD DRAIN ([`super::RestoreDrain`]): a cold launch's layout rebuild,
//! a slice per park. Every test drives the real slice driver with a zero
//! budget, so each slice lands exactly one window or tab: the order and the
//! interleaving with what a person does are the code's, never the clock's.

use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use crate::{App, WindowId, restore};

/// A one-pane terminal leaf named `title` by its operator title, which rides
/// onto whichever shell fills it (a fresh one or the grafted bootstrap) and
/// back into a capture — what these tests tell tabs apart by.
fn leaf(title: &str) -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
        restore::TerminalLeafRestore {
            cwd: Some("/tmp".to_string()),
            title: title.to_string(),
            profile: None,
            local_id: None,
            user_title: Some(title.to_string()),
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
        },
    ))
}

/// A window of one-pane tabs titled `titles`, `front` in front, as a quit
/// writes it.
fn window(titles: &[&str], front: usize) -> restore::WindowLayout {
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
        active_item: Some(front),
        restored_tabs: titles
            .iter()
            .map(|title| restore::RestoredTab {
                root: leaf(title),
                focused_path: Vec::new(),
                zoomed: false,
            })
            .collect(),
    }
}

/// One slice of a desk whose windows after the first are logical windows
/// over a stub shell (a unit test has no event loop to create glass with).
fn slice(app: &mut App) -> bool {
    app.drain_restore_slice(Duration::ZERO, |app, first, wl, placed, filling| {
        if first {
            app.restore_bootstrap_window(wl, placed, Some(filling));
        } else {
            let wid = app.insert_logical_window(
                crate::stub_session(app.next_session_id),
                wl.rows,
                wl.cols,
            );
            placed.shown.push((wid, wl.show));
            app.fill_restored_window(wid, wl, Some(filling));
        }
        ControlFlow::Continue(())
    })
}

fn drain(app: &mut App) {
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !slice(app) {
        assert!(Instant::now() < deadline, "the drain never ended");
    }
}

/// The operator title of the shell a tab shows (empty for one a person
/// opened).
fn named(app: &App, tab: &crate::tab_model::Tab) -> String {
    let session = app
        .view_store
        .get(tab.focus)
        .copied()
        .and_then(crate::tab_model::View::terminal_session)
        .expect("a terminal tab");
    app.pool
        .get(session)
        .expect("a live shell")
        .ctx
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .user_title
        .clone()
        .unwrap_or_default()
}

fn titles(app: &App, wid: WindowId) -> Vec<String> {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .map(|tab| named(app, tab))
        .collect()
}

fn front(app: &App, wid: WindowId) -> String {
    named(
        app,
        app.windows[&wid].tab_set.active().expect("a front tab"),
    )
}

fn layout_titles(wl: &restore::WindowLayout) -> Vec<String> {
    wl.restored_tabs
        .iter()
        .map(|tab| match &tab.root {
            restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } => leaf.user_title.clone().unwrap_or_default(),
            other => panic!("a one-pane terminal tab: {other:?}"),
        })
        .collect()
}

/// A person opens a tab in `wid`, as the tests of a live window do.
fn open_tab(app: &mut App, wid: WindowId) -> u64 {
    let id = app.next_session_id;
    let session = crate::stub_session(id);
    App::register_session(&app.store, &session, None);
    app.pool.insert(session);
    app.next_session_id += 1;
    let tree = crate::pane::PaneTree::new(id);
    let tab = crate::register_terminal_tab(&mut app.tab_ids, &mut app.view_store, &tree)
        .expect("tab identity");
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.layouts.push(tree);
    ws.tabs.add();
    ws.tab_set.push(tab).expect("fresh tab id");
    app.resync_active_or_window(wid);
    id
}

/// The window that has painted gets its bootstrap tab back at once, then the
/// tab it had in front, each in its place; one tab per slice after that, and
/// the desk ends exactly as it was written, with the rebuild owed until then.
#[test]
fn a_cold_drain_lands_the_front_tab_second_and_ends_as_recorded() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(
        &["A", "B", "C", "D", "E"],
        3,
    )]));

    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A"], "the bootstrap's own tab, at once");
    assert!(app.restore_pass_queued(), "the rest is owed");
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A", "D"]);
    assert_eq!(front(&app, wid), "D", "the tab on glass lands second");
    let mut slices = 2;
    while !slice(&mut app) {
        slices += 1;
        assert!(app.restore_pass_queued());
    }
    assert_eq!(slices + 1, 5, "one tab per slice");
    assert_eq!(titles(&app, wid), ["A", "B", "C", "D", "E"]);
    assert_eq!(front(&app, wid), "D");
    assert!(app.restore_drain.is_none() && !app.restore_pass_queued());
    assert!(app.restored_window_sessions_are_distinct(wid));
    assert!(
        app.structural_invariants_ok(),
        "{}",
        app.structural_invariant_violation().unwrap_or_default()
    );
}

/// A tab the person closes while the drain runs stays closed, and one they
/// open is kept, after the restored tabs it was opened beside; their choice
/// of front tab stands.
#[test]
fn a_person_s_tabs_outlive_the_drain_and_a_closed_tab_stays_closed() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(
        &["A", "B", "C", "D", "E"],
        3,
    )]));
    assert!(!slice(&mut app));
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A", "D"]);

    // The person closes D (not the window's last tab, so the window stays).
    assert!(!app.close_tab_at(wid, 1));
    assert_eq!(titles(&app, wid), ["A"]);
    let mine = open_tab(&mut app, wid);
    let opened = front(&app, wid);
    drain(&mut app);

    let after = titles(&app, wid);
    assert_eq!(
        &after[..4],
        ["A", "B", "C", "E"],
        "D is not rebuilt: {after:?}"
    );
    assert_eq!(after.len(), 5, "{after:?}");
    assert_eq!(front(&app, wid), opened, "the person's front tab stands");
    assert!(app.pool.get(mine).is_some(), "the person's shell lives");
    assert!(
        app.structural_invariants_ok(),
        "{}",
        app.structural_invariant_violation().unwrap_or_default()
    );
}

/// Each window lands in turn, its own tabs a slice at a time; a window the
/// person closes while its tabs land owes nothing more, and is not made
/// again.
#[test]
fn the_drain_takes_windows_in_turn_and_a_closed_window_owes_nothing() {
    let mut app = App::headless_for_test();
    app.pending_restore = Some(restore::RestoreManifest::new(vec![
        window(&["A", "B"], 0),
        window(&["P", "Q", "R"], 2),
        window(&["X", "Y"], 1),
    ]));
    assert!(!slice(&mut app));
    assert_eq!(app.windows.len(), 1, "window 0 first, alone");
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, WindowId(0)), ["A", "B"]);
    assert!(!slice(&mut app));
    let second = *app.windows.keys().last().expect("the second window");
    assert_eq!(titles(&app, second), ["P"]);
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, second), ["P", "R"]);
    let _ = app.close_window_logical(second);
    drain(&mut app);

    assert!(!app.windows.contains_key(&second), "not made again");
    assert_eq!(app.windows.len(), 2, "window 0 and the third");
    let third = *app.windows.keys().last().expect("the third window");
    assert_eq!(titles(&app, third), ["X", "Y"]);
    assert_eq!(front(&app, third), "Y");
}

/// Half a desk is never the crash journal's image: while the drain runs the
/// journal captures nothing, however much activity it is told of, and its
/// first image after the drain is the whole layout.
#[cfg(unix)]
#[test]
fn the_crash_journal_captures_nothing_until_the_drain_is_done() {
    let mut app = App::headless_for_test();
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(
        &["A", "B", "C"],
        0,
    )]));
    let journal_dir = crate::crash_journal::tests::scratch("cj-drain");
    app.crash_journal = crate::crash_journal::Lane::new(
        Some(journal_dir.path().to_path_buf()),
        crate::crash_journal::tests::reaped_pid(),
    );
    app.first_present_done = true;
    let journal = app.crash_journal.journal_id();

    assert!(!slice(&mut app));
    app.crash_journal.note_activity();
    app.tick_crash_journal(Instant::now());
    assert_eq!(app.crash_journal.captures, 0, "nothing captured mid-drain");
    drain(&mut app);
    app.crash_journal.note_activity();
    app.tick_crash_journal(Instant::now());
    assert_eq!(
        app.crash_journal.captures, 1,
        "the drained desk is captured"
    );
    let (_, image) = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the journal's image",
        || {
            std::fs::read_to_string(journal_dir.path().join(journal.file_name()))
                .ok()
                .and_then(|text| crate::crash_journal::decode(&text).ok())
        },
    );
    assert_eq!(layout_titles(&image.windows[0]), ["A", "B", "C"]);
    app.crash_journal = crate::crash_journal::Lane::inert();
    crate::crash_journal::tests::await_released(&journal_dir.path().join(journal.lock_name()));
}

/// A quit in the drain's first instants keeps every tab and window it still
/// owed: the quit's layout is the rebuilt part with the owed part after it,
/// and it parses back.
#[test]
fn a_quit_inside_the_drain_keeps_every_tab_and_window_it_owed() {
    let mut app = App::headless_for_test();
    app.pending_restore = Some(restore::RestoreManifest::new(vec![
        window(&["A", "B", "C", "D"], 2),
        window(&["X", "Y"], 0),
    ]));
    assert!(!slice(&mut app));
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, WindowId(0)), ["A", "C"]);

    let layout = app.capture_restore_manifest();
    assert_eq!(layout.windows.len(), 2, "the window not begun is carried");
    assert_eq!(layout_titles(&layout.windows[0]), ["A", "B", "C", "D"]);
    assert_eq!(layout.windows[0].active_item, Some(2), "C stays in front");
    assert_eq!(layout_titles(&layout.windows[1]), ["X", "Y"]);
    let parsed = restore::RestoreManifest::from_toml(&layout.to_toml().expect("the layout writes"))
        .expect("the layout parses back");
    assert_eq!(parsed.windows.len(), 2);
    assert_eq!(layout_titles(&parsed.windows[0]), ["A", "B", "C", "D"]);
}

/// A mid-drain quit has the same tab order as finishing the drain: the
/// restored tabs retain their manifest positions around the live tabs, a
/// closed restored tab stays closed, and the person's selected tab stays
/// selected after owed tabs are inserted before it.
#[test]
fn a_mid_drain_quit_preserves_live_tabs_and_the_persons_selection() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(
        &["A", "B", "C", "D", "E"],
        3,
    )]));
    assert!(!slice(&mut app));
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A", "D"]);
    assert!(!app.close_tab_at(wid, 1));
    let mine = open_tab(&mut app, wid);
    let selected = app.windows[&wid].tab_set.active_id();

    let layout = app.capture_restore_manifest();
    assert_eq!(layout_titles(&layout.windows[0]), ["A", "B", "C", "E", ""]);
    assert_eq!(layout.windows[0].active_item, Some(4));
    let parsed = restore::RestoreManifest::from_toml(&layout.to_toml().expect("the layout writes"))
        .expect("the layout parses back");
    assert_eq!(
        layout_titles(&parsed.windows[0]),
        layout_titles(&layout.windows[0])
    );
    assert_eq!(parsed.windows[0].active_item, Some(4));

    drain(&mut app);
    assert_eq!(titles(&app, wid), layout_titles(&layout.windows[0]));
    assert_eq!(app.windows[&wid].tab_set.active_id(), selected);
    assert!(app.pool.get(mine).is_some(), "the person's shell lives");
}

/// A tab that lands behind the one in front leaves that tab as the person
/// has it: a find bar they open while the drain runs stays open as the rest
/// of the desk lands.
#[test]
fn tabs_landing_behind_the_front_one_leave_its_find_bar_open() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(
        &["A", "B", "C"],
        0,
    )]));
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A"]);
    app.search_enter();
    assert!(app.windows[&wid].search.is_some(), "find opens mid-drain");

    drain(&mut app);
    assert_eq!(titles(&app, wid), ["A", "B", "C"]);
    assert_eq!(front(&app, wid), "A");
    assert!(
        app.windows[&wid].search.is_some(),
        "find stays open while B and C land"
    );
}
