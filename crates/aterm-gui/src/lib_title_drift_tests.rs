// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Feed a real OSC 7 cwd report through the engine — the genuine
/// `title_epoch` bump path, not a test back door.
fn report_cwd(app: &App, session: u64, dir: &str) {
    let term = app.pool.get(session).expect("session").term.clone();
    term.lock()
        .unwrap()
        .process(format!("\x1b]7;file://localhost{dir}\x07").as_bytes());
}

fn timeline_events(app: &App, session: u64) -> Vec<(&'static str, String)> {
    let tl = app.pool.get(session).expect("session").ctx.timeline.clone();
    let tl = tl.lock().unwrap();
    tl.since(None)
        .map(|e| (e.kind, e.payload.clone()))
        .collect()
}

/// A single `cd` flushes on the observing wake, and the refreshed tooltip already shows
/// the new cwd (the record precedes `refresh_window_tabs`; the label is pinned by a user
/// title).
#[test]
fn quiet_cwd_change_flushes_instantly_and_records_before_the_refresh() {
    let mut app = App::headless_for_test();
    let t0 = Instant::now();
    // Pin the label so a cwd change can never move the cache's label key.
    app.pool
        .get(0)
        .unwrap()
        .ctx
        .meta
        .lock()
        .unwrap()
        .set("title", Some("pinned".into()));
    // Consume the initial epoch, then warm the chrome cache (the tab-switch
    // analog): composed at the current high id, fresh, label "pinned".
    app.observe_title_drift(0, t0);
    app.refresh_window_tabs(WindowId(0));
    report_cwd(&app, 0, "/tmp/one");
    // Past the debounce window ⇒ the onset flushes immediately.
    app.observe_title_drift(
        0,
        t0 + DRIFT_REFRESH_MIN_INTERVAL + Duration::from_millis(1),
    );
    let events = timeline_events(&app, 0);
    assert!(
        events
            .iter()
            .any(|(k, p)| *k == "cwd-change" && p.contains("/tmp/one")),
        "the cd was recorded: {events:?}"
    );
    let tooltip = app.windows[&WindowId(0)].tab_set.tabs()[0]
        .presentation
        .tooltip
        .clone()
        .expect("terminal tab has a composed tooltip");
    assert!(
        tooltip.contains("/tmp/one"),
        "record-before-refresh: the SAME flush's strip refresh composes \
         against the moved high id, so the tooltip is already fresh: {tooltip:?}"
    );
}

/// A flood of title/cwd changes flushes the first instantly, marks the rest pending, and
/// the next due wake absorbs them in one flush of the final value. A no-drift wake then
/// does nothing.
#[test]
fn title_cwd_flood_coalesces_to_the_debounce_interval() {
    let mut app = App::headless_for_test();
    let t0 = Instant::now();
    app.observe_title_drift(0, t0); // consume the initial epoch
    let base = timeline_events(&app, 0).len();
    // Onset (quiet ⇒ instant).
    let t1 = t0 + DRIFT_REFRESH_MIN_INTERVAL + Duration::from_millis(1);
    report_cwd(&app, 0, "/flood/0");
    app.observe_title_drift(0, t1);
    assert_eq!(
        timeline_events(&app, 0).len(),
        base + 1,
        "the FIRST change of a flood flushes instantly"
    );
    // Flood: 50 distinct changes, all observed inside one debounce window.
    for i in 1..=50u64 {
        report_cwd(&app, 0, &format!("/flood/{i}"));
        app.observe_title_drift(0, t1 + Duration::from_millis(i));
    }
    assert_eq!(
        timeline_events(&app, 0).len(),
        base + 1,
        "wakes inside the debounce window flush nothing — they mark pending"
    );
    assert!(
        app.title_drift.pending.contains(&0),
        "the tail flush is scheduled (about_to_wait owns its deadline)"
    );
    // The next due wake: one flush absorbs the backlog with the final cwd.
    let t2 = t1 + DRIFT_REFRESH_MIN_INTERVAL;
    app.observe_title_drift(0, t2);
    let events = timeline_events(&app, 0);
    assert_eq!(events.len(), base + 2, "one coalesced flush for 50 changes");
    assert!(
        events.last().unwrap().1.contains("/flood/50"),
        "the coalesced flush records the LIVE value: {events:?}"
    );
    assert!(!app.title_drift.pending.contains(&0), "backlog consumed");
    // Steady state (epoch unmoved): a further wake records/flushes nothing.
    app.observe_title_drift(0, t2 + Duration::from_millis(1));
    assert_eq!(timeline_events(&app, 0).len(), base + 2);
    assert!(!app.title_drift.pending.contains(&0));
}

/// A session labelling no tab still records `cwd-change`, and a session closing with
/// drift pending is forgotten.
#[test]
fn unlabeled_sessions_record_cwd_changes_and_closed_sessions_are_forgotten() {
    let mut app = App::headless_for_test();
    let sid = 7;
    app.pool.insert(stub_session(sid));
    report_cwd(&app, sid, "/pane/b");
    let t0 = Instant::now();
    app.observe_title_drift(sid, t0);
    let events = timeline_events(&app, sid);
    assert!(
        events
            .iter()
            .any(|(k, p)| *k == "cwd-change" && p.contains("/pane/b")),
        "a session labeling no tab still records its cd: {events:?}"
    );
    // Close it with a drift pending: the flush drops every trace.
    app.title_drift.pending.insert(sid);
    assert!(app.pool.detach(sid), "sole view ⇒ session dropped");
    assert!(
        app.flush_title_drift(sid, t0 + DRIFT_REFRESH_MIN_INTERVAL),
        "a dead session flushes vacuously"
    );
    assert!(!app.title_drift.pending.contains(&sid));
    assert!(!app.title_drift.seen.contains_key(&sid));
    assert!(!app.title_drift.last_flush.contains_key(&sid));
}

/// Every production close path uses `detach_session_view`: one of multiple
/// views must preserve both the PTY and its per-session chrome state, while
/// the final detach retires it (presented labels, chrome, drift) before the id
/// can be reused.
#[test]
fn final_view_detach_retires_per_session_chrome_state_exactly_once() {
    let mut app = App::headless_for_test();
    // A title the clean fast path declines (a spinner glyph), so the refresh
    // below fills the per-session label cache.
    let term = app.pool.get(0).expect("session 0").term.clone();
    term.lock()
        .unwrap()
        .process("\x1b]2;✳ private title\x07".as_bytes());
    app.refresh_window_tabs(WindowId(0));
    assert!(
        app.chrome_labels.tracks(0),
        "the test warms the presented-label cache"
    );
    assert!(
        app.session_chrome.contains_key(&0),
        "the test warms the authored chrome cache"
    );
    let drift_time = Instant::now();
    app.title_drift.seen.insert(0, 7);
    app.title_drift.last_flush.insert(0, drift_time);
    app.title_drift.pending.insert(0);
    app.session_chrome_expiry.session = Some(0);
    app.session_chrome_expiry.after_window = Some(WindowId(0));
    let window = app.windows.get_mut(&WindowId(0)).expect("test window");
    window.tab_title_epochs.insert(0, 7);
    window
        .tab_title_cache
        .insert(0, "private title".to_string());
    window
        .tab_chrome_titles
        .insert(0, "private title".to_string());
    let tab_id = window.tab_set.active_id().expect("terminal tab");
    window
        .tab_chrome_titles_by_tab
        .insert(tab_id, (Some(0), "private title".to_string()));

    app.pool.attach(0);
    assert!(
        !app.detach_session_view(0),
        "the first of two views keeps the shared session alive"
    );
    assert!(
        app.chrome_labels.tracks(0),
        "auxiliary state belongs to the live shared session"
    );
    assert!(app.session_chrome.contains_key(&0));
    assert!(app.session_chrome_expiry.pending());
    assert!(app.title_drift.pending.contains(&0));
    assert!(app.windows[&WindowId(0)].tab_title_cache.contains_key(&0));
    assert!(
        app.windows[&WindowId(0)]
            .tab_chrome_titles_by_tab
            .contains_key(&tab_id)
    );

    assert!(
        app.detach_session_view(0),
        "the final view owns session teardown"
    );
    assert!(!app.chrome_labels.tracks(0));
    assert!(!app.session_chrome.contains_key(&0));
    assert!(!app.session_chrome_expiry.pending());
    assert!(!app.title_drift.pending.contains(&0));
    assert!(!app.title_drift.seen.contains_key(&0));
    assert!(!app.title_drift.last_flush.contains_key(&0));
    let window = &app.windows[&WindowId(0)];
    assert!(!window.tab_title_epochs.contains_key(&0));
    assert!(!window.tab_title_cache.contains_key(&0));
    assert!(!window.tab_chrome_titles.contains_key(&0));
    assert!(!window.tab_chrome_titles_by_tab.contains_key(&tab_id));
    assert!(!app.detach_session_view(0), "a repeated detach is a no-op");
}
