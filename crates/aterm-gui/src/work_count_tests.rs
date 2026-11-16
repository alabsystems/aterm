// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Count tests for the many-tab / many-session paths and the subscribe idle wake.

use super::*;
use crate::work_counts;
use std::time::{Duration, Instant};

/// The PTY reader's batch cadence under a flood — the rate the `Wake::Output` gates run at.
const BURST_DT: Duration = Duration::from_micros(333);

/// Window 0 split to `panes` panes, then `tabs - 1` more tabs appended, each split as it becomes active.
fn workspace(tabs: usize, panes: usize) -> App {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    for _ in 1..panes {
        app.split_active_stub_tab(wid);
    }
    for _ in 1..tabs {
        let sid = app.next_session_id;
        app.push_stub_tab(wid, stub_session(sid));
        for _ in 1..panes {
            app.split_active_stub_tab(wid);
        }
    }
    assert_eq!(app.windows[&wid].tab_set.len(), tabs, "fixture tab count");
    assert_eq!(
        app.pool.iter().count(),
        tabs * panes,
        "fixture session count"
    );
    app
}

fn set_grid(app: &mut App, rows: u16, cols: u16) {
    let mut ws = app.windows.get_mut(&WindowId(0)).expect("fixture window");
    ws.rows = rows;
    ws.cols = cols;
}

/// The live-drag tick (`resize_panes_scoped(wid, true)`) lays out the active tab only.
#[test]
fn a_live_drag_tick_lays_out_only_the_active_tab() {
    const TABS: usize = 8;
    let mut app = workspace(TABS, 4);
    let wid = WindowId(0);
    app.resize_panes_scoped(wid, false);
    let _ = work_counts::take_plans_built();

    set_grid(&mut app, 30, 100);
    app.resize_panes_scoped(wid, false);
    assert_eq!(
        work_counts::take_plans_built(),
        TABS,
        "a settle plans every tab"
    );

    set_grid(&mut app, 32, 120);
    for _ in 0..16 {
        app.resize_panes_scoped(wid, true);
    }
    assert!(
        app.windows[&wid].panes_stale,
        "background tabs are deferred"
    );
    assert_eq!(
        work_counts::take_plans_built(),
        16,
        "a live-drag tick must lay out only the active tab — a background tab's \
         discarded plan is the work MPT-2 removed"
    );
}

/// A successful present's latency walk touches the visible panes' stamps only.
#[test]
fn the_present_walk_touches_only_the_visible_panes_stamps() {
    const PANES: usize = 4;
    let mut app = workspace(30, PANES);
    let wid = WindowId(0);
    let arm = |app: &App| {
        for s in app.pool.iter() {
            s.last_output_ns
                .store(1, std::sync::atomic::Ordering::Relaxed);
        }
    };
    arm(&app);
    let _ = work_counts::take_latency_stamps_touched();
    let _ = app.present_latency_ns(wid);
    let first = work_counts::take_latency_stamps_touched();
    assert!(
        (PANES..=2 * PANES).contains(&first),
        "the first walk clears the newly revealed panes and books them: {first}"
    );
    let _ = work_counts::take_plans_built();
    for _ in 0..8 {
        arm(&app);
        let _ = app.present_latency_ns(wid);
    }
    assert_eq!(
        work_counts::take_latency_stamps_touched(),
        8 * PANES,
        "a steady present touches exactly the visible panes' stamps"
    );
    assert_eq!(
        work_counts::take_plans_built(),
        8,
        "a present lays out the active tab only"
    );
}

/// Inside the observation interval the status sweep probes no session.
#[test]
fn a_rate_limited_status_sweep_probes_no_session() {
    const SESSIONS: usize = 30;
    let mut app = workspace(1, SESSIONS);
    assert!(
        app.config.tab_status_or_default(),
        "tab_status must be ON, or the sweep is one early return and proves nothing"
    );
    let t0 = Instant::now();
    let _ = work_counts::take_status_probes();
    let _ = app.observe_session_statuses(t0);
    assert_eq!(
        work_counts::take_status_probes(),
        SESSIONS,
        "CONTROL: the first sweep probes every session"
    );
    let mut now = t0;
    for _ in 0..100 {
        now += BURST_DT;
        let _ = app.observe_session_statuses(now);
    }
    assert_eq!(
        work_counts::take_status_probes(),
        0,
        "100 flood wakes inside the interval must probe nothing"
    );
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(2));
    assert_eq!(
        work_counts::take_status_probes(),
        SESSIONS,
        "CONTROL: past the interval the sweep probes again"
    );
}

fn take_title_focus_visits(app: &App) -> usize {
    app.windows
        .values()
        .map(|window| window.tab_set.take_focused_view_visits_for_test())
        .sum()
}

fn title_workspace(tabs: usize) -> (App, [WindowId; 2]) {
    let mut app = workspace(tabs, 1);
    let unrelated = std::array::from_fn(|_| {
        app.frontmost_window = Some(WindowId(0));
        let session = app.next_session_id;
        app.push_stub_tab(WindowId(0), stub_session(session));
        let other = app.detach_active_tab_logical().expect("unrelated window");
        for _ in 1..30 {
            let session = app.next_session_id;
            app.push_stub_tab(other, stub_session(session));
        }
        assert_eq!(app.windows[&other].tab_set.len(), 30);
        other
    });
    app.frontmost_window = Some(WindowId(0));
    app.sync_active_session();
    assert_eq!(app.windows[&WindowId(0)].tab_set.len(), tabs);
    assert!(app.structural_invariants_ok());
    (app, unrelated)
}

type TitleSnapshot = Vec<(
    tab_model::TabId,
    tab_model::TabPresentation,
    Option<(Option<u64>, String)>,
)>;

fn title_snapshot(app: &App, wid: WindowId) -> TitleSnapshot {
    let window = &app.windows[&wid];
    window
        .tab_set
        .tabs()
        .iter()
        .map(|tab| {
            (
                tab.id,
                tab.presentation.clone(),
                window.tab_chrome_titles_by_tab.get(&tab.id).cloned(),
            )
        })
        .collect()
}

fn assert_unrelated_titles_untouched(
    app: &App,
    unrelated: [WindowId; 2],
    before: &[TitleSnapshot; 2],
) {
    for (wid, titles) in unrelated.into_iter().zip(before) {
        assert_eq!(
            app.windows[&wid]
                .tab_set
                .take_focused_view_visits_for_test(),
            0,
            "unrelated window {wid:?} must not walk its 30 tabs"
        );
        assert_eq!(&title_snapshot(app, wid), titles);
    }
}

fn report_title(app: &mut App, session: u64, title: &str, now: &mut Instant) {
    app.pool
        .get(session)
        .expect("live terminal")
        .term
        .lock()
        .unwrap()
        .process(format!("\x1b]2;{title}\x07").as_bytes());
    *now += DRIFT_REFRESH_MIN_INTERVAL + Duration::from_millis(1);
    app.observe_title_drift(session, *now);
}

fn assert_published_title(
    app: &App,
    wid: WindowId,
    tab: tab_model::TabId,
    session: Option<u64>,
    title: &str,
) {
    assert_eq!(
        app.windows[&wid].tab_chrome_titles_by_tab.get(&tab),
        Some(&(session, title.to_string())),
        "read the last published strip label without refreshing it"
    );
}

fn begin_cold_title_membership_control(app: &mut App) {
    // Fixture construction now publishes titles. Start this measured cold phase after setup,
    // invalidating through the real mutable topology API rather than clearing a private index.
    for mut window in app.windows.values_mut() {
        let _ = window.tab_set.take_focused_view_visits_for_test();
        let _ = window.tab_set.active_mut();
    }
}

fn assert_title_caches_warmed(app: &App) {
    for (wid, window) in &app.windows {
        assert_eq!(
            window.tab_set.take_focused_view_visits_for_test(),
            window.tab_set.len(),
            "cold control: window {wid:?} builds its membership once"
        );
    }
}

fn assert_steady_title_gate(app: &mut App, session: u64, now: &mut Instant) {
    let _ = take_title_focus_visits(app);
    for _ in 0..100 {
        *now += BURST_DT;
        app.observe_title_drift(session, *now);
    }
    assert_eq!(take_title_focus_visits(app), 0);
}

/// Neither the labeling window nor unrelated many-tab windows walk tabs for steady output.
#[test]
fn a_steady_title_gate_walks_no_tab() {
    const TABS: usize = 30;
    let (mut app, unrelated) = title_workspace(TABS);
    let wid = WindowId(0);
    let active = app.focused_session_id(wid).expect("an active session");
    begin_cold_title_membership_control(&mut app);
    let mut now = Instant::now();
    app.observe_title_drift(active, now);
    for _ in 0..16 {
        now += BURST_DT;
        app.observe_title_drift(active, now);
    }
    assert_title_caches_warmed(&app);
    let before = unrelated.map(|wid| title_snapshot(&app, wid));
    for _ in 0..100 {
        now += BURST_DT;
        app.observe_title_drift(active, now);
    }
    assert_eq!(
        take_title_focus_visits(&app),
        0,
        "a steady flood must not walk any window's tab list"
    );
    assert_unrelated_titles_untouched(&app, unrelated, &before);

    let mut window = app.windows.get_mut(&wid).expect("fixture window");
    window.tab_title_epochs.remove(&active);
    let _ = window.tab_set.tab_at_mut(0).expect("mutable topology loan");
    drop(window);
    now += Duration::from_millis(500);
    app.observe_title_drift(active, now);
    assert_eq!(
        take_title_focus_visits(&app),
        TABS,
        "a mutable topology loan rebuilds one window's focused-view membership"
    );
    assert_unrelated_titles_untouched(&app, unrelated, &before);
}

#[test]
fn title_labels_follow_detach_migrate_attach_and_close_without_unrelated_walks() {
    let (mut app, unrelated) = title_workspace(2);
    let source = WindowId(0);
    let tab = app.windows[&source].tab_set.active().unwrap();
    let (tab_id, view) = (tab.id, tab.focus);
    let session = app.focused_session_id(source).unwrap();
    begin_cold_title_membership_control(&mut app);
    let mut now = Instant::now();
    report_title(&mut app, session, "before move", &mut now);
    assert_published_title(&app, source, tab_id, Some(session), "before move");
    assert_title_caches_warmed(&app);
    let before = unrelated.map(|wid| title_snapshot(&app, wid));

    let detached = app.detach_active_tab_logical().unwrap();
    assert_eq!(app.windows[&detached].tab_set.active().unwrap().id, tab_id);
    assert_eq!(app.windows[&detached].tab_set.active().unwrap().focus, view);
    report_title(&mut app, session, "detached", &mut now);
    assert_published_title(&app, detached, tab_id, Some(session), "detached");
    assert!(
        !app.windows[&source]
            .tab_chrome_titles_by_tab
            .contains_key(&tab_id)
    );
    assert!(!app.windows[&source].tab_title_epochs.contains_key(&session));
    assert_unrelated_titles_untouched(&app, unrelated, &before);
    assert!(
        take_title_focus_visits(&app) > 0,
        "detaching invalidates membership"
    );
    assert_steady_title_gate(&mut app, session, &mut now);

    app.migrate_active_tab_from_window(detached).unwrap();
    assert!(!app.windows.contains_key(&detached));
    assert_eq!(app.windows[&source].tab_set.active().unwrap().id, tab_id);
    report_title(&mut app, session, "migrated", &mut now);
    assert_published_title(&app, source, tab_id, Some(session), "migrated");
    assert_unrelated_titles_untouched(&app, unrelated, &before);
    assert!(
        take_title_focus_visits(&app) > 0,
        "migration invalidates membership"
    );

    let attached = app.open_active_session_in_new_window_logical().unwrap();
    let attached_tab = app.windows[&attached].tab_set.active().unwrap();
    let attached_id = attached_tab.id;
    assert_ne!(attached_tab.focus, view);
    assert_eq!(app.view_store.terminal_views(session).count(), 2);
    report_title(&mut app, session, "shared", &mut now);
    assert_published_title(&app, source, tab_id, Some(session), "shared");
    assert_published_title(&app, attached, attached_id, Some(session), "shared");
    assert_unrelated_titles_untouched(&app, unrelated, &before);
    assert!(
        take_title_focus_visits(&app) > 0,
        "a new view joins the gate"
    );

    app.close_window_logical(attached);
    assert!(!app.windows.contains_key(&attached));
    assert_eq!(app.view_store.terminal_views(session).count(), 1);
    report_title(&mut app, session, "surviving view", &mut now);
    assert_published_title(&app, source, tab_id, Some(session), "surviving view");
    assert_unrelated_titles_untouched(&app, unrelated, &before);
    assert_steady_title_gate(&mut app, session, &mut now);
    assert!(app.structural_invariants_ok());
}

#[test]
fn title_labels_follow_native_split_close_and_reopen_without_unrelated_walks() {
    let (mut app, unrelated) = title_workspace(2);
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, settings) = app.active_native_view(wid).unwrap();
    let (session, terminal) =
        app.split_active_with_stub_terminal(wid, tab_model::SplitAxis::Horizontal);
    let tab = app.windows[&wid].tab_set.active().unwrap().id;
    begin_cold_title_membership_control(&mut app);
    let mut now = Instant::now();
    report_title(&mut app, session, "mixed terminal", &mut now);
    assert_published_title(&app, wid, tab, Some(session), "mixed terminal");
    assert_title_caches_warmed(&app);
    let before = unrelated.map(|wid| title_snapshot(&app, wid));

    assert!(app.focus_pane_in(wid, pane::FocusDir::Left));
    assert_eq!(app.windows[&wid].tab_set.active().unwrap().focus, settings);
    report_title(&mut app, session, "hidden terminal", &mut now);
    assert_published_title(&app, wid, tab, None, "Settings");
    assert!(!app.windows[&wid].tab_title_epochs.contains_key(&session));
    assert_unrelated_titles_untouched(&app, unrelated, &before);
    assert!(
        take_title_focus_visits(&app) > 0,
        "native focus invalidates membership"
    );

    assert_eq!(app.close_active_tab(), None);
    assert_eq!(app.windows[&wid].tab_set.active().unwrap().focus, terminal);
    report_title(&mut app, session, "promoted terminal", &mut now);
    assert_published_title(&app, wid, tab, Some(session), "promoted terminal");
    assert_unrelated_titles_untouched(&app, unrelated, &before);
    assert!(
        take_title_focus_visits(&app) > 0,
        "native close invalidates membership"
    );

    app.reopen_closed_view_and_wait_for_test().unwrap();
    assert!(app.active_native_view(wid).is_some());
    report_title(&mut app, session, "hidden after reopen", &mut now);
    assert_published_title(&app, wid, tab, None, "Settings");
    assert!(!app.windows[&wid].tab_title_epochs.contains_key(&session));
    assert_unrelated_titles_untouched(&app, unrelated, &before);
    assert!(
        take_title_focus_visits(&app) > 0,
        "native reopen invalidates membership"
    );
    assert_steady_title_gate(&mut app, session, &mut now);
    assert!(app.structural_invariants_ok());
}

fn title_restore_window(record: restore::RestoredTab) -> restore::WindowLayout {
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
        restored_tabs: vec![record],
    }
}

#[test]
fn title_labels_follow_recursive_native_terminal_replacement_without_unrelated_walks() {
    let (mut app, unrelated) = title_workspace(1);
    let wid = WindowId(0);
    let mut session = app.focused_session_id(wid).unwrap();
    let terminal = app
        .tab_restore_descriptor(app.windows[&wid].tab_set.active().unwrap())
        .unwrap();
    let mut native = terminal.clone();
    native.root = restore::RestoredSplitTree::leaf(restore::RestoredView::Native(
        restore::NativeLeafRestore::about(),
    ));
    let probe = app.focused_session_id(unrelated[0]).unwrap();
    begin_cold_title_membership_control(&mut app);
    let mut now = Instant::now();
    report_title(&mut app, session, "before restore", &mut now);
    assert_title_caches_warmed(&app);

    for round in 0..2 {
        let before = unrelated.map(|wid| title_snapshot(&app, wid));
        let old_tab = app.windows[&wid].tab_set.active().unwrap().id;
        app.restore_into_window(wid, title_restore_window(native.clone()));
        let native_tab = app.windows[&wid].tab_set.active().unwrap().id;
        assert_ne!(native_tab, old_tab);
        assert!(app.active_native_view(wid).is_some());
        assert!(app.pool.get(session).is_none());
        assert_eq!(app.view_store.terminal_views(session).count(), 0);
        assert_published_title(&app, wid, native_tab, None, "About aterm");
        assert!(
            !app.windows[&wid]
                .tab_chrome_titles_by_tab
                .contains_key(&old_tab)
        );
        assert_unrelated_titles_untouched(&app, unrelated, &before);

        // An actual output observation warms the new native-only tab set as a non-labeler.
        report_title(&mut app, probe, &format!("probe {round}"), &mut now);
        assert!(
            take_title_focus_visits(&app) > 0,
            "replacement cold control"
        );
        let before = unrelated.map(|wid| title_snapshot(&app, wid));
        app.restore_into_window(wid, title_restore_window(terminal.clone()));
        let new_tab = app.windows[&wid].tab_set.active().unwrap().id;
        assert_ne!(new_tab, native_tab);
        let previous = session;
        session = app.focused_session_id(wid).unwrap();
        assert_ne!(session, previous);
        let title = format!("restored {round}");
        report_title(&mut app, session, &title, &mut now);
        assert_published_title(&app, wid, new_tab, Some(session), &title);
        assert!(
            !app.windows[&wid]
                .tab_chrome_titles_by_tab
                .contains_key(&native_tab)
        );
        assert_unrelated_titles_untouched(&app, unrelated, &before);
        assert!(
            take_title_focus_visits(&app) > 0,
            "terminal restore invalidates membership"
        );
        assert_steady_title_gate(&mut app, session, &mut now);
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn an_unfocused_split_stays_cheap_and_becomes_a_label_when_focused() {
    const TABS: usize = 30;
    let mut app = workspace(TABS, 2);
    let wid = WindowId(0);
    let tab = app.windows[&wid].tab_set.tab_at(0).unwrap();
    let hidden = tab
        .root
        .leaves()
        .into_iter()
        .find(|view| *view != tab.focus)
        .unwrap();
    let session = app
        .view_store
        .get(hidden)
        .copied()
        .and_then(tab_model::View::terminal_session)
        .unwrap();
    let mut now = Instant::now();
    app.observe_title_drift(session, now);
    let _ = take_title_focus_visits(&app);
    for _ in 0..100 {
        now += BURST_DT;
        app.observe_title_drift(session, now);
    }
    assert_eq!(take_title_focus_visits(&app), 0);
    assert!(!app.windows[&wid].tab_title_epochs.contains_key(&session));
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .tab_at_mut(0)
            .unwrap()
            .set_focus(hidden)
    );
    now += Duration::from_millis(500);
    app.observe_title_drift(session, now);
    assert!(app.windows[&wid].tab_title_epochs.contains_key(&session));
    assert_eq!(take_title_focus_visits(&app), TABS);
}

/// Exercise the accepted OSC path with every cache warm, rather than measuring only
/// the epoch gate. The counters cover drift walks, identity reads, title reads, retain membership
/// calls and chrome-cache writes; they do not claim CPU time or total copied bytes.
#[test]
fn complete_title_refresh_has_bounded_cache_maintenance() {
    for tabs in [1, 30, 200] {
        let mut app = workspace(tabs, 1);
        let wid = WindowId(0);
        let sessions: Vec<_> = app.pool.iter().map(|s| s.id).collect();
        let t0 = Instant::now();
        for &sid in &sessions {
            term_lock(&app.pool.get(sid).unwrap().term)
                .process(format!("\x1b]2;label-{sid}\x07").as_bytes());
            app.observe_title_drift(sid, t0);
        }
        let active = app.focused_session_id(wid).unwrap();
        let ids: Vec<_> = app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|t| t.id)
            .collect();
        let ws = &app.windows[&wid];
        assert_eq!(ws.tab_title_cache.len(), tabs);
        assert_eq!(ws.tab_title_epochs.len(), tabs);
        assert_eq!(ws.tab_chrome_titles.len(), tabs);
        assert_eq!(ws.tab_chrome_titles_by_tab.len(), tabs);
        let _ = work_counts::take_title_refresh();
        term_lock(&app.pool.get(active).unwrap().term).process(b"\x1b]2;changed-label\x07");
        let later = t0 + Duration::from_secs(1);
        app.observe_title_drift(active, later);
        let work = work_counts::take_title_refresh();
        eprintln!("complete-title tabs={tabs} changed={work:?}");
        assert!(work.drift_tabs > 0 && work.drift_tabs <= 3 * tabs);
        assert!(work.drift_cache_probes > 0 && work.drift_cache_probes <= 4 * tabs);
        assert!(work.eligibility_reads > 0 && work.eligibility_reads <= tabs);
        assert!(work.identities > 0 && work.identities <= tabs);
        assert!(work.reads > 0 && work.reads <= tabs);
        assert!(work.cache_probes > 0 && work.cache_probes <= 4 * tabs);
        assert!(work.cache_writes > 0 && work.cache_writes <= 2 * tabs);
        let ws = &app.windows[&wid];
        assert_eq!(ws.tab_title_cache[&active].title, "changed-label");
        for (id, tab) in ids.iter().zip(ws.tab_set.tabs()) {
            assert_eq!(*id, tab.id, "refresh preserves ordering and identity");
            let sid = app
                .view_store
                .get(tab.focus)
                .copied()
                .unwrap()
                .terminal_session()
                .unwrap();
            let expected = if sid == active {
                "changed-label".to_owned()
            } else {
                format!("label-{sid}")
            };
            assert_eq!(ws.tab_chrome_titles[&sid], expected);
            assert_eq!(ws.tab_chrome_titles_by_tab[id].0, Some(sid));
            assert_eq!(ws.tab_chrome_titles_by_tab[id].1, expected);
            assert_eq!(
                ws.tab_title_epochs[&sid],
                term_lock(&app.pool.get(sid).unwrap().term).title_epoch()
            );
        }
        assert_eq!(
            work.chrome_bytes,
            2 * ws
                .tab_chrome_titles
                .values()
                .map(String::len)
                .sum::<usize>()
        );
        for tick in 1..=100 {
            term_lock(&app.pool.get(active).unwrap().term).process(b"\x1b]2;changed-label\x07");
            app.observe_title_drift(active, later + Duration::from_millis(tick));
        }
        let unchanged = work_counts::take_title_refresh();
        eprintln!("complete-title tabs={tabs} unchanged-flood={unchanged:?}");
        assert_eq!(unchanged, work_counts::TitleRefreshWork::default());
        assert!(!app.title_drift.pending.contains(&active));
    }
}

#[test]
fn complete_title_refresh_preserves_shared_and_detached_identity() {
    let mut app = workspace(3, 1);
    let original = WindowId(0);
    let sid = app.focused_session_id(original).unwrap();
    term_lock(&app.pool.get(sid).unwrap().term).process(b"\x1b]2;before-detach\x07");
    app.observe_title_drift(sid, Instant::now());
    let tab = app.windows[&original].tab_set.active_id().unwrap();
    let detached = app.detach_active_tab_logical().unwrap();
    let shared = app.open_active_session_in_new_window_logical().unwrap();
    for wid in [original, detached, shared] {
        app.refresh_window_tabs(wid);
    }
    let unrelated = app.windows[&original].tab_chrome_titles_by_tab.clone();
    let _ = work_counts::take_title_refresh();
    term_lock(&app.pool.get(sid).unwrap().term).process(b"\x1b]2;after-detach\x07");
    app.observe_title_drift(sid, Instant::now() + Duration::from_secs(1));
    let work = work_counts::take_title_refresh();
    eprintln!("complete-title shared-detached={work:?}");
    assert_eq!(work.reads, 2, "only the two labeling windows refresh");
    assert_eq!(app.windows[&original].tab_chrome_titles_by_tab, unrelated);
    assert!(!app.windows[&original].tab_title_cache.contains_key(&sid));
    assert!(
        !app.windows[&original]
            .tab_chrome_titles_by_tab
            .contains_key(&tab)
    );
    for wid in [detached, shared] {
        assert_eq!(app.windows[&wid].tab_chrome_titles[&sid], "after-detach");
    }
    app.pool
        .get(sid)
        .unwrap()
        .ctx
        .meta
        .lock()
        .unwrap()
        .set("title", Some("pinned-label".into()));
    app.refresh_meta_dependent_chrome(sid);
    for wid in [detached, shared] {
        assert_eq!(app.windows[&wid].tab_chrome_titles[&sid], "pinned-label");
    }
    app.close_window_logical(shared);
    app.detach_rollback_logical(Some(original), detached);
    app.refresh_window_tabs(original);
    assert_eq!(
        app.windows[&original].tab_chrome_titles_by_tab[&tab].0,
        Some(sid)
    );
    assert_eq!(
        app.windows[&original].tab_chrome_titles_by_tab[&tab].1,
        "pinned-label"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn complete_title_refresh_keeps_parked_and_native_fallbacks() {
    let mut app = workspace(2, 1);
    let wid = WindowId(0);
    let hidden = app.focused_session_id(wid).unwrap();
    term_lock(&app.pool.get(hidden).unwrap().term).process(b"\x1b]2;hidden-terminal\x07");
    app.refresh_window_tabs(wid);
    let instance = app
        .native_runtime
        .insert_instance(crate::native_app::NativeApp::Allocation(Box::default()))
        .unwrap();
    let native = app
        .split_active_with_native(
            wid,
            crate::tab_model::SplitAxis::Horizontal,
            instance,
            crate::native_app::AppViewState::Allocation(Box::default()),
        )
        .unwrap();
    let parked = crate::app_workers_tests::land_worker(&mut app);
    term_lock(&app.pool.get(parked).unwrap().term).process(b"\x1b]2;parked-label\x07");
    app.refresh_window_tabs(wid);
    assert!(!app.windows[&wid].tab_title_cache.contains_key(&hidden));
    assert!(!app.windows[&wid].tab_chrome_titles.contains_key(&hidden));
    let native_title = app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .find(|t| t.focus == native)
        .map(|t| app.windows[&wid].tab_chrome_titles_by_tab[&t.id].clone())
        .unwrap();
    assert_eq!(native_title.0, None);
    app.observe_title_drift(parked, Instant::now() + Duration::from_secs(1));
    let ws = &app.windows[&wid];
    assert_eq!(ws.tab_chrome_titles[&parked], "parked-label");
    assert_eq!(ws.tooltips_pushed.len(), ws.tab_set.shown_len());
    assert!(ws.tab_set.tabs().len() > ws.tab_set.shown_len());
    assert!(
        ws.tab_chrome_titles_by_tab
            .values()
            .any(|v| v == &native_title)
    );
    assert!(app.structural_invariants_ok());
}

/// An idle `events` digest wake with saturated ledgers touches no retained record.
#[test]
fn an_idle_events_wake_touches_no_retained_record() {
    use crate::subscribe::digest_fixture::DigestFixture;
    const TARGETS: usize = 4;
    let floor = aterm_slog::row_floor(crate::session_log::ring_limits()).records;
    let mut f = DigestFixture::build(TARGETS, 2 * 1000, 2 * crate::turn_ledger::LEDGER_CAP, {
        aterm_slog::MAX_RECORDS + floor
    });
    let (blocks, turns, rows, lost) = f.retained();
    assert!(
        blocks >= 1000 && turns == crate::turn_ledger::LEDGER_CAP && rows >= floor && lost > 0,
        "the fixture must saturate every ledger and the view: {blocks}/{turns}/{rows}/{lost}"
    );
    let _ = work_counts::take_retained_records_touched();
    for woke in [false, true, false, false] {
        assert_eq!(f.wake(woke), 0, "an idle wake emits nothing");
    }
    assert_eq!(
        work_counts::take_retained_records_touched(),
        0,
        "an idle wake must not touch the retained ledgers"
    );

    f.land_turn();
    assert!(f.wake(true) > 0, "a wake after a real append emits");
    let touched = work_counts::take_retained_records_touched();
    let per_target_bound = 2 + crate::turn_ledger::LEDGER_CAP.ilog2() as usize + 1;
    assert!(
        (TARGETS..=TARGETS * per_target_bound).contains(&touched),
        "a wake with one new turn per target touched {touched} records (bound \
         {per_target_bound} per target)"
    );

    f.land_row();
    assert!(f.wake(true) > 0, "a wake after a new row emits");
    let touched = work_counts::take_retained_records_touched();
    assert!(
        (TARGETS..=TARGETS * per_target_bound).contains(&touched),
        "a wake with one new row per target touched {touched} records"
    );
}

/// A headless window drawn through the real redraw path on a scripted CPU surface.
#[cfg(debug_assertions)]
fn presented_window() -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    app.windows.get_mut(&wid).expect("fixture window").present = Some(crate::PresentTarget::Cpu {
        surface: crate::present::CpuSurface::Scripted(Default::default()),
    });
    let ws = &app.windows[&wid];
    assert!(
        matches!(&ws.present, Some(crate::PresentTarget::Cpu { surface }) if surface.scripted().is_some()),
        "a scripted surface"
    );
    (app, wid, session)
}

/// Successful presents on `wid`'s scripted surface so far.
#[cfg(debug_assertions)]
fn presents(app: &App, wid: WindowId) -> usize {
    match &app.windows[&wid].present {
        Some(crate::PresentTarget::Cpu { surface }) => {
            surface.scripted().map_or(0, |s| s.commits())
        }
        _ => 0,
    }
}

/// Cell geometries the renderer derived over `frames` presented frames of `wid`.
#[cfg(debug_assertions)]
fn derivations_over(
    app: &mut App,
    wid: WindowId,
    frames: usize,
    mut before: impl FnMut(&mut App),
) -> usize {
    let _ = work_counts::take_cell_geometry_derivations();
    for _ in 0..frames {
        before(app);
        app.redraw_window(wid);
    }
    work_counts::take_cell_geometry_derivations()
}

/// The cell box is derived once per change, never per frame.
#[cfg(debug_assertions)]
#[test]
fn a_frame_derives_the_cell_box_only_when_an_input_changed() {
    const FRAMES: usize = 20;
    let (mut app, wid, session) = presented_window();
    let committed = presents(&app, wid);
    let _ = derivations_over(&mut app, wid, 2, |_| {});
    assert!(
        presents(&app, wid) > committed,
        "the frames reach the glass"
    );
    let rest = derivations_over(&mut app, wid, FRAMES, |_| {});
    assert_eq!(rest, 0, "frames at rest derive no cell box");
    let term = app.pool.get(session).expect("session").term.clone();
    let keys = derivations_over(&mut app, wid, FRAMES, |_| {
        crate::term_lock(&term).process(b"k");
    });
    assert_eq!(keys, 0, "keystroke frames derive no cell box");

    let px = app.windows[&wid].metrics.font_px;
    let before = app.win_cell_size(wid);
    app.windows
        .get_mut(&wid)
        .expect("fixture window")
        .metrics
        .font_px = px * 2.0;
    let _ = work_counts::take_cell_geometry_derivations();
    app.redraw_window(wid);
    assert_eq!(
        work_counts::take_cell_geometry_derivations(),
        1,
        "CONTROL: a new scale derives once"
    );
    let doubled = app.win_cell_size(wid);
    assert!(
        doubled.0.abs_diff(2 * before.0) <= 1 && doubled.1.abs_diff(2 * before.1) <= 1,
        "the new scale's cell box ({before:?} -> {doubled:?})"
    );
    assert_eq!(
        derivations_over(&mut app, wid, FRAMES, |_| {}),
        0,
        "kept at 2x"
    );
    app.windows
        .get_mut(&wid)
        .expect("fixture window")
        .metrics
        .font_px = px;
    assert_eq!(
        derivations_over(&mut app, wid, FRAMES, |_| {}),
        0,
        "kept at 1x"
    );
    assert_eq!(app.win_cell_size(wid), before, "the first display's box");

    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-cell-box-reload")
        .tempdir()
        .expect("scratch dir");
    let dir = scratch.path();
    let geometry = app.backend.cell_geometry(px);
    let _ = work_counts::take_cell_geometry_derivations();
    apply_config_text(&mut app, dir, "adjust_baseline = 3\n");
    app.redraw_window(wid);
    assert_eq!(
        work_counts::take_cell_geometry_derivations(),
        1,
        "CONTROL: a reloaded baseline derives once"
    );
    assert_eq!(
        app.backend.cell_geometry(px),
        (geometry.0, geometry.1, geometry.2 + 3),
        "the same box, its baseline 3 px down"
    );
    assert_eq!(
        derivations_over(&mut app, wid, FRAMES, |_| {}),
        0,
        "kept after the baseline's reload"
    );

    // The reload re-grids every window inside the size it holds, a headless one too, so the
    // one derivation may fall in the reload or in the frame after it.
    let _ = work_counts::take_cell_geometry_derivations();
    apply_config_text(&mut app, dir, "line_height = 1.5\n");
    app.redraw_window(wid);
    assert_eq!(
        work_counts::take_cell_geometry_derivations(),
        1,
        "CONTROL: a reloaded line height derives once"
    );
    let taller = app.win_cell_size(wid);
    assert_eq!(taller.0, before.0, "the same advance");
    assert!(
        taller.1 > before.1,
        "a taller cell ({before:?} -> {taller:?})"
    );
    assert_eq!(app.cell_size(), taller, "the renderer's own box at this px");
    assert_eq!(
        derivations_over(&mut app, wid, FRAMES, |_| {}),
        0,
        "kept after the reload"
    );

    let _ = work_counts::take_cell_geometry_derivations();
    app.set_font_size(app.font_size + 4.0);
    app.redraw_window(wid);
    assert_eq!(
        work_counts::take_cell_geometry_derivations(),
        2,
        "CONTROL: a zoom derives once on each renderer"
    );
    let zoomed = app.win_cell_size(wid);
    assert!(
        (app.windows[&wid].metrics.font_px - (px + 4.0)).abs() < 0.5,
        "the window took the zoom"
    );
    assert_eq!(app.cell_size(), zoomed, "the zoomed renderer's own box");
    assert!(
        zoomed.1 > taller.1,
        "a bigger cell ({taller:?} -> {zoomed:?})"
    );
    assert_eq!(
        derivations_over(&mut app, wid, FRAMES, |_| {}),
        0,
        "kept after the zoom"
    );
}

/// Apply `text` as `aterm.toml` through the real admission path.
#[cfg(debug_assertions)]
fn apply_config_text(app: &mut App, dir: &std::path::Path, text: &str) {
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, text);
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false)
            .expect("observe");
    let config: Config = aterm_toml::from_str(text).expect("valid toml");
    let assets = config.resolve_asset_catalog();
    let path_feed_fps = config.path_feed_fingerprints();
    let sparkle = config.prepare_sparkle_runtime();
    app.apply_prepared_config_generation(crate::native_font_catalog::PreparedConfigGeneration {
        observation,
        config,
        values: std::collections::BTreeMap::new(),
        disk_faults: std::sync::Arc::default(),
        assets,
        path_feed_fps,
        sparkle,
        fonts: None,
        warnings: Vec::new(),
        font_refusals: Vec::new(),
        reason: crate::app_colorscheme::ThemeChange::ConfigReload,
    });
}

fn title_output_authority_case(windows: usize, hidden_output: bool, omit_completion: bool) {
    let mut app = workspace(30, if hidden_output { 2 } else { 1 });
    for _ in 1..windows {
        app.frontmost_window = Some(WindowId(0));
        let sid = app.next_session_id;
        app.push_stub_tab(WindowId(0), stub_session(sid));
        let wid = app.detach_active_tab_logical().unwrap();
        for _ in 1..30 {
            let sid = app.next_session_id;
            app.push_stub_tab(wid, stub_session(sid));
        }
    }
    app.frontmost_window = Some(WindowId(0));
    app.sync_active_session();
    let tab = app.windows[&WindowId(0)].tab_set.active().unwrap();
    let view = if hidden_output {
        tab.root
            .leaves()
            .into_iter()
            .find(|view| *view != tab.focus)
            .unwrap()
    } else {
        tab.focus
    };
    let sid = app
        .view_store
        .get(view)
        .copied()
        .and_then(tab_model::View::terminal_session)
        .unwrap();
    let now = Instant::now();
    for _ in 0..12 {
        app.on_output_wake(sid, now);
    }
    let _ = take_title_focus_visits(&app);
    let _ = work_counts::take_title_refresh();
    let mut run = || {
        for _ in 0..100 {
            app.on_output_wake(sid, now);
        }
        let work = work_counts::take_title_refresh();
        assert_eq!(
            work.window_gates, 0,
            "unchanged output must not visit title windows"
        );
        assert_eq!(
            work.membership_probes, 0,
            "unchanged output must not join reverse views"
        );
        assert_eq!(
            work.drift_tabs + work.drift_cache_probes + work.cache_probes,
            0
        );
        assert_eq!(take_title_focus_visits(&app), 0);
    };
    if omit_completion {
        crate::title_authority::with_fault(crate::title_authority::Fault::Completion, run);
    } else {
        run();
    }
}

#[test]
fn title_authority_output_hits_visit_no_window_membership_or_cache() {
    for windows in [1, 2, 5] {
        for hidden in [false, true] {
            title_output_authority_case(windows, hidden, false);
        }
    }
}

#[test]
fn title_authority_missing_completion_is_detected_by_actual_window_work() {
    let rejected = std::panic::catch_unwind(|| title_output_authority_case(2, false, true));
    assert!(
        rejected.is_err(),
        "the window-work oracle must reject the production slow gate"
    );
}

fn geometry_title_authority_case(tabs: usize, snapshot: bool) {
    let mut app = workspace(tabs, 1);
    let now = Instant::now();
    for _ in 0..4 {
        app.observe_title_drift(0, now);
    }
    let revision = app.windows.title_authority().revision();
    let _ = take_title_focus_visits(&app);
    let _ = work_counts::take_title_refresh();
    let mut run = || {
        for _ in 0..8 {
            let mut ws = app.windows.get_mut(&WindowId(0)).unwrap();
            ws.rows += 1;
        }
        assert!(revision.is_current(app.windows.title_authority()));
        let work = work_counts::take_title_refresh();
        assert_eq!(
            work.drift_tabs, 0,
            "a geometry loan cannot snapshot tab membership"
        );
        assert_eq!(take_title_focus_visits(&app), 0);
    };
    if snapshot {
        crate::title_authority::with_fault(crate::title_authority::Fault::GeneralSnapshot, run);
    } else {
        run();
    }
}

#[test]
fn title_authority_geometry_loans_do_not_scan_or_invalidate_at_any_tab_count() {
    for tabs in [1, 30, 200] {
        geometry_title_authority_case(tabs, false);
    }
    assert!(std::panic::catch_unwind(|| geometry_title_authority_case(30, true)).is_err());
}

#[test]
fn title_authority_cold_delivery_prunes_each_owner_once_not_once_per_session() {
    let run = |rescan: bool| {
        const TABS: usize = 30;
        let mut app = workspace(TABS, 1);
        let sessions: Vec<_> = app.pool.iter().map(|session| session.id).collect();
        for &sid in &sessions {
            app.pool
                .get(sid)
                .unwrap()
                .term
                .lock()
                .unwrap()
                .process(format!("\x1b]2;topology-{sid}\x07").as_bytes());
        }
        let now = Instant::now();
        for &sid in &sessions {
            app.observe_title_drift(sid, now);
        }
        let pacing: Vec<_> = sessions
            .iter()
            .map(|sid| (*sid, app.title_drift.last_flush[sid]))
            .collect();
        app.windows.get_mut(&WindowId(0)).unwrap().tab_title_epochs =
            crate::title_authority::TitleEpochs::default();
        let _ = work_counts::take_title_refresh();
        let mut publish = || {
            app.inspect_app(crate::app_control::InspectRequest::Tabs)
                .unwrap();
            let work = work_counts::take_title_refresh();
            assert_eq!(
                work.identities, TABS,
                "one complete publication for this dirty owner"
            );
            assert_eq!(
                work.drift_tabs, 0,
                "topology already owns canonical membership pruning"
            );
            assert_eq!(
                work.drift_cache_probes, 0,
                "topology cannot repeat ordinary drift pruning per session"
            );
            for &(sid, paced) in &pacing {
                let epoch = app
                    .pool
                    .get(sid)
                    .unwrap()
                    .term
                    .lock()
                    .unwrap()
                    .title_epoch();
                assert_eq!(
                    app.windows[&WindowId(0)].tab_title_epochs.get(&sid),
                    Some(&epoch)
                );
                assert_eq!(app.title_drift.last_flush[&sid], paced);
                assert!(
                    app.windows[&WindowId(0)].tab_chrome_titles[&sid]
                        .contains(&format!("topology-{sid}"))
                );
            }
        };
        if rescan {
            crate::title_authority::with_fault(
                crate::title_authority::Fault::TopologyRescan,
                publish,
            );
        } else {
            publish();
        }
    };
    run(false);
    assert!(std::panic::catch_unwind(|| run(true)).is_err());
}
