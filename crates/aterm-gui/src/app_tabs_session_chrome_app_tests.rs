// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Instant;

use super::*;
use crate::menu::MenuAction;
use crate::session_chrome::TabMenuEntry;

#[test]
fn composed_chrome_reaches_presentation_and_menu_actions_are_honest() {
    let mut app = App::headless_for_test();
    let titles = app.tab_titles(WindowId(0));
    let ext = app.tab_chrome_ext(WindowId(0), &titles);
    assert_eq!(ext.len(), 1);
    assert_eq!(
        ext[0].tooltip, None,
        "a bare live session says nothing its chip does not: `alive` and a spawn \
         event are no tooltip"
    );
    assert_eq!(
        app.windows[&WindowId(0)].tab_set.tabs()[0]
            .presentation
            .tooltip,
        ext[0].tooltip
    );
    let action = |a: MenuAction| {
        ext[0].menu.iter().find_map(|e| match e {
            TabMenuEntry::Action {
                action, enabled, ..
            }
            | TabMenuEntry::SessionAction {
                action, enabled, ..
            } if *action == a => Some(*enabled),
            _ => None,
        })
    };
    assert_eq!(action(MenuAction::CopySessionId), Some(true));
    assert_eq!(action(MenuAction::CopyCwd), Some(false), "stub has no cwd");
    assert_eq!(action(MenuAction::CloseTab), Some(true));
    {
        let ctx = &app.pool.get(0).expect("session 0").ctx;
        ctx.meta
            .lock()
            .unwrap()
            .set("description", Some("purpose text".into()));
        ctx.timeline
            .lock()
            .unwrap()
            .record_raw_meta_change("field=description value=purpose%20text".into());
    }
    app.refresh_window_tabs(WindowId(0));
    let label = app.tab_titles(WindowId(0)).remove(0);
    let tip = app.windows[&WindowId(0)].tab_set.tabs()[0]
        .presentation
        .tooltip
        .clone()
        .expect("a description composes a tooltip");
    assert!(
        tip.starts_with(&label) && tip.contains("purpose text"),
        "{tip:?}"
    );
}

#[test]
fn session_chrome_gather_cuts_agent_moves_before_the_tail_cap() {
    let mut app = App::headless_for_test();
    {
        let ctx = &app.pool.get(0).expect("session 0").ctx;
        let mut tl = ctx.timeline.lock().unwrap();
        tl.record_hand_row("cwd-change", "cwd=/tmp".into());
        for _ in 0..crate::session_chrome::TIMELINE_TAIL + 2 {
            tl.record_hand_row("agent-change", "agent=busy".into());
            tl.record_hand_row("human", "bytes=1".into());
        }
    }
    let titles = app.tab_titles(WindowId(0));
    let ext = app.tab_chrome_ext(WindowId(0), &titles);
    let shown = crate::session_chrome::menu_shown_at(&ext[0].menu, crate::turn_ledger::now_ms());
    let rows: Vec<&str> = shown
        .iter()
        .filter_map(|e| match e {
            TabMenuEntry::Header(h) if h.contains(" \u{b7} ") => Some(h.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        rows.iter()
            .any(|r| r.starts_with("changed directory \u{b7} ")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .all(|r| !r.starts_with("agent-change") && !r.starts_with("human")),
        "{rows:?}"
    );
}

#[test]
fn chrome_refresh_never_waits_for_terminal_parser_lock() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.refresh_window_tabs(wid);
    let cached = app
        .session_chrome
        .get(&0)
        .expect("initial chrome cached")
        .ext
        .clone();
    let (term, live_epoch) = {
        let session = app.pool.get(0).expect("session 0");
        let term = session.term.clone();
        let live_epoch = term.lock().unwrap().title_epoch();
        (term, live_epoch)
    };
    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_title_epochs
        .insert(0, live_epoch);
    {
        let ctx = &app.pool.get(0).expect("session 0").ctx;
        ctx.meta
            .lock()
            .unwrap()
            .set("description", Some("Reviewing proxy boundary".into()));
        ctx.timeline
            .lock()
            .unwrap()
            .record_raw_meta_change("field=description value=Reviewing%20proxy%20boundary".into());
    }
    let parser_guard = term.lock().unwrap();

    app.refresh_window_tabs(wid);
    assert_eq!(
        app.session_chrome.get(&0).unwrap().ext,
        cached,
        "contention serves the last coherent chrome snapshot"
    );
    assert!(
        app.title_drift.pending.contains(&0),
        "contention must own a bounded event-loop retry"
    );
    assert!(app.session_chrome_retry.contains(&0));
    assert!(
        !app.windows[&wid].tab_title_epochs.contains_key(&0),
        "fact-only contention must force the retry past the title-epoch gate"
    );

    drop(parser_guard);
    assert!(app.flush_title_drift(0, Instant::now()));
    assert!(!app.title_drift.pending.contains(&0));
    assert!(!app.session_chrome_retry.contains(&0));
    assert!(
        app.session_chrome[&0]
            .ext
            .tooltip
            .as_deref()
            .is_some_and(|tooltip| tooltip.contains("Reviewing proxy boundary")),
        "retry recomposes the moved chrome after parser contention"
    );
}

#[test]
fn tab_menu_target_follows_its_stable_id_across_a_mid_menu_reorder() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    let popped = app.windows[&wid].tab_set.tabs()[1].id;
    let popped_sid = app
        .tab_terminal_session(wid, 1)
        .expect("clicked tab has a terminal session");
    app.move_tab(wid, 0, 2);
    let index = app
        .tab_index_for_id(wid, popped)
        .expect("the clicked tab still exists");
    assert_eq!(index, 0, "the clicked tab moved from position 1 to 0");
    assert_eq!(
        app.tab_terminal_session(wid, index),
        Some(popped_sid),
        "the action lands on the SAME session the menu was popped on"
    );
    assert_ne!(
        app.tab_terminal_session(wid, 1),
        Some(popped_sid),
        "the clicked position was inherited by another session"
    );
}

type Endpoint = (aterm_session::SessionId, std::sync::Arc<crate::SessionCtx>);

fn connected_pair_app() -> (App, Endpoint, Endpoint) {
    let mut app = App::headless_for_test();
    app.push_stub_tab(WindowId(0), crate::stub_session(app.next_session_id));
    let (a, b) = {
        let g = app.store.read().unwrap();
        let a = g.by_local(0).expect("session 0 registered");
        let b = g.by_local(1).expect("session 1 registered");
        (
            (a.sid.clone(), a.ctx.clone()),
            (b.sid.clone(), b.ctx.clone()),
        )
    };
    (app, a, b)
}

#[test]
fn connection_acts_invalidate_the_chrome_cache_without_expiry() {
    let (mut app, (a_sid, _a_ctx), (b_sid, b_ctx)) = connected_pair_app();
    let titles = app.tab_titles(WindowId(0));
    let before = app.tab_chrome_ext(WindowId(0), &titles);
    assert!(
        !before[0]
            .tooltip
            .as_deref()
            .unwrap_or_default()
            .contains("\u{21e5}"),
        "no connection line before any connection exists"
    );
    assert!(crate::connections::connect_in(
        &app.connections,
        &a_sid,
        &b_sid,
        &b_ctx.edges,
        &b_ctx.nonce,
        aterm_session::ConnectionKind::Both,
        "test"
    ));
    let fresh = app.tab_chrome_ext(WindowId(0), &titles);
    let tip_a = fresh[0].tooltip.as_deref().unwrap_or_default();
    assert!(
        tip_a.contains("\u{21e5} pushes into"),
        "A recomposed with its outbound line, no expiry needed: {tip_a:?}"
    );
    let tip_b = fresh[1].tooltip.as_deref().unwrap_or_default();
    assert!(
        tip_b.contains("\u{21e4} pushed by"),
        "B recomposed with the inbound inverse: {tip_b:?}"
    );
    assert!(
        crate::connections::disconnect_kind_in(
            &app.connections,
            &a_sid,
            &b_sid,
            &b_ctx.edges,
            None,
            "test"
        )
        .is_some()
    );
    let cleared = app.tab_chrome_ext(WindowId(0), &titles);
    for (i, ext) in cleared.iter().enumerate() {
        let tip = ext.tooltip.as_deref().unwrap_or_default();
        assert!(
            !tip.contains("\u{21e5}") && !tip.contains("\u{21e4}"),
            "tab {i} still lists a dissolved connection: {tip:?}"
        );
    }
}

#[test]
fn a_pass_whose_edge_snapshot_predates_a_mint_recomposes_on_the_next_pass() {
    let (mut app, (a_sid, _a_ctx), (b_sid, b_ctx)) = connected_pair_app();
    let titles = app.tab_titles(WindowId(0));
    let mut snapshot = Some(EdgeSnapshot {
        revision: app.connections.revision(),
        edges: crate::connections::all_edges(&app.store),
    });
    assert!(crate::connections::connect_in(
        &app.connections,
        &a_sid,
        &b_sid,
        &b_ctx.edges,
        &b_ctx.nonce,
        aterm_session::ConnectionKind::Both,
        "test"
    ));
    let names_the_mint = |ext: &crate::session_chrome::TabChromeExt| {
        ext.tooltip
            .as_deref()
            .unwrap_or_default()
            .contains("\u{21e5} pushes into")
    };
    let mid_pass = app.composed_session_chrome(WindowId(0), 0, &titles[0], &mut snapshot);
    assert!(
        !names_the_mint(&mid_pass),
        "composed from the snapshot taken before the mint"
    );
    let next = app.tab_chrome_ext(WindowId(0), &titles);
    assert!(
        names_the_mint(&next[0]),
        "the next pass recomposes the tab with the mint"
    );
}

#[test]
fn tab_menu_session_rows_show_and_never_stop_unconfirmed() {
    use crate::session_chrome::ConnVerb;
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let (mut app, (_a_sid, _a_ctx), (b_sid, b_ctx)) = connected_pair_app();
    let wid = WindowId(0);
    let tab_a = app.windows[&wid].tab_set.tabs()[0].id;
    app.dispatch_tab_menu_connection(wid, tab_a, &b_sid, ConnVerb::Show);
    assert_eq!(
        app.windows[&wid].tab_set.active_index(),
        Some(1),
        "Show raises the named session's tab"
    );
    app.switch_tab_in(wid, 0);
    app.dispatch_tab_menu_connection(
        wid,
        crate::tab_model::TabId::from_stored(u64::MAX),
        &b_sid,
        ConnVerb::Show,
    );
    assert_eq!(app.windows[&wid].tab_set.active_index(), Some(0));
    let b_local = app.store.read().unwrap().by_sid(&b_sid).unwrap().local_id;
    mark_session_task(&b_ctx, b_local, &TaskMark::new(TaskState::Working)).expect("unfenced");
    app.dispatch_tab_menu_connection(wid, tab_a, &b_sid, ConnVerb::StopWorker);
    assert_eq!(app.agents.stop_signals, 0, "nothing was signalled");
    let card = app.windows[&wid].overview().expect("the overview opened");
    assert!(
        card.controls_lines()
            .iter()
            .any(|l| l.contains(&format!("sid={} ", b_sid.as_str())) && l.contains("confirm=true")),
        "armed on B"
    );
    app.overview_exit(wid);
    app.agents.owner_sheet_for_test = Some(Some(false));
    app.dispatch_tab_menu_connection(wid, tab_a, &b_sid, ConnVerb::StopWorker);
    assert_eq!(app.agents.stop_signals, 0, "Cancel stops nothing");
    assert!(app.windows[&wid].overview().is_none());
    app.agents.owner_sheet_for_test = Some(Some(true));
    app.dispatch_tab_menu_connection(wid, tab_a, &b_sid, ConnVerb::StopWorker);
    assert_eq!(app.agents.stop_signals, 1, "Stop signals once");
}

#[test]
fn tab_menu_target_of_a_closed_tab_is_dropped_not_redirected() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    let popped = app.windows[&wid].tab_set.tabs()[1].id;
    let neighbor_sid = app.tab_terminal_session(wid, 2);
    assert!(!app.close_tab_at(wid, 1), "two tabs survive the close");
    assert_eq!(app.windows[&wid].tab_set.len(), 2, "clicked tab is gone");
    assert_eq!(
        app.tab_terminal_session(wid, 1),
        neighbor_sid,
        "the frozen index re-binds to the neighbor — the trap"
    );
    assert_eq!(
        app.tab_index_for_id(wid, popped),
        None,
        "the pop-time id refuses the re-bound slot: dispatch no-ops"
    );
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn a_controller_press_that_dismisses_the_card_swallows_its_own_release() {
    use crate::input::InputEvent;
    use aterm_types::mouse::MouseButton;

    let ev = |button, pressed| InputEvent::MouseButton {
        button,
        pressed,
        row: 8,
        col: 8,
        mods: 0,
        click_count: 1,
        side: Default::default(),
        block: false,
        suppress_copy_on_select: false,
        px_off: Default::default(),
    };

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    assert!(app.open_active_tab_context_menu(wid), "the card is up");
    assert!(
        app.tab_menu_input_event(wid, &ev(MouseButton::Left, true)),
        "a press off the card dismisses it AND is consumed"
    );
    assert!(app.windows[&wid].tab_menu.is_none(), "the card is down");
    assert!(
        app.tab_menu_input_event(wid, &ev(MouseButton::Left, false)),
        "and its RELEASE is swallowed too — never handed on alone"
    );
    assert!(
        !app.tab_menu_input_event(wid, &ev(MouseButton::Left, false)),
        "the latch is spent: a later release is ordinary input"
    );

    assert!(app.open_active_tab_context_menu(wid));
    assert!(app.tab_menu_input_event(wid, &ev(MouseButton::Left, true)));
    assert!(
        !app.tab_menu_input_event(wid, &ev(MouseButton::Right, false)),
        "a button the card never consumed passes through"
    );
}
