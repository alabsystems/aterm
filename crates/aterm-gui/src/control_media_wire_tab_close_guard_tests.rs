// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::app_window::{CloseConfirm, WIRE_CLOSE_REFUSED, WIRE_TAB_CLOSE_REFUSED};
use crate::session_store::{ExitActor, ExitReason};
use crate::{App, TabAction, WindowId};

use crate::feed_session_for_test as feed;

fn start_in_shell_job(app: &App, session: u64) {
    feed(
        app,
        session,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07Start-Sleep 60\n\x1b]133;C\x07",
    );
}

fn finish_job(app: &App, session: u64) {
    feed(app, session, b"\x1b]133;D;0\x07");
}

fn wire_tab_close(app: &mut App, which: Option<usize>) -> Result<(usize, usize), String> {
    wire_tab_close_by(app, which, ExitActor::Ctl)
}

fn wire_tab_close_by(
    app: &mut App,
    which: Option<usize>,
    by: ExitActor,
) -> Result<(usize, usize), String> {
    let state = app.tab_cmd_applied(by, |app| app.apply_tab_cmd(TabAction::Close(which)));
    app.close_confirm = CloseConfirm::Interactive;
    state
}

/// The newest close's reason and closer, from the closed index.
fn last_exit(app: &App) -> (ExitReason, String) {
    let g = app.store.read().unwrap_or_else(|p| p.into_inner());
    let row = g
        .closed_logs()
        .exits_since(0)
        .next_back()
        .expect("an exit row after the close");
    (row.reason, row.actor.as_wire().to_string())
}

#[test]
fn a_wire_tab_close_journals_its_caller() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(1));

    wire_tab_close(&mut app, Some(1)).expect("a non-last tab closes");
    assert_eq!(last_exit(&app), (ExitReason::CtlClose, "ctl".to_string()));

    wire_tab_close_by(&mut app, None, ExitActor::Sid("s-edge-caller".to_string()))
        .expect("an idle last-tab close proceeds");
    assert!(app.windows[&wid].pending_close);
    assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Exit);
    assert_eq!(
        last_exit(&app),
        (ExitReason::CtlClose, "s-edge-caller".to_string())
    );
}

#[test]
fn an_invoked_close_tab_journals_its_caller_not_a_human() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(1));
    app.push_stub_tab(wid, crate::stub_session(2));

    assert_eq!(app.close_active_tab(), None, "a non-last tab closes");
    assert_eq!(last_exit(&app), (ExitReason::UiClose, "human".to_string()));

    let invoked = app.invoke_attributed(ExitActor::Ctl, |app| {
        assert_eq!(app.close_active_tab(), None, "a non-last tab closes");
        Ok("invoked CloseTab".to_string())
    });
    assert_eq!(invoked, Ok("invoked CloseTab".to_string()));
    assert_eq!(last_exit(&app), (ExitReason::CtlClose, "ctl".to_string()));

    let _ = app.invoke_attributed(ExitActor::Sid("s-edge-caller".to_string()), |app| {
        assert_eq!(app.close_active_tab(), Some(wid), "the last tab closes");
        Ok("invoked CloseTab".to_string())
    });
    assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Exit);
    assert_eq!(
        last_exit(&app),
        (ExitReason::CtlClose, "s-edge-caller".to_string())
    );
}

#[test]
fn a_busy_last_tab_tab_close_is_refused_and_proceeds_once_idle() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let wid = WindowId(0);
    start_in_shell_job(&app, 0);

    assert_eq!(
        wire_tab_close(&mut app, None),
        Err(WIRE_CLOSE_REFUSED.to_string()),
        "a running job refuses the last-tab close on the wire, in `close`'s words"
    );
    assert_eq!(app.windows[&wid].tab_set.len(), 1, "the tab is still there");
    assert!(
        !app.windows[&wid].pending_close,
        "nothing was flagged for teardown"
    );
    assert!(app.pool.get(0).is_some(), "the session was not retired");
    assert!(
        app.pending_action_refusal.is_none(),
        "the refusal was consumed by the reply, not left for the next verb"
    );

    finish_job(&app, 0);
    wire_tab_close(&mut app, None).expect("an idle last-tab close proceeds");
    assert!(
        app.windows[&wid].pending_close,
        "the idle close flagged the window for the deferred teardown"
    );
}

#[test]
fn a_busy_non_last_tab_close_is_refused_as_close_refuses_its_pane() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(1));
    assert_eq!(app.windows[&wid].tab_set.len(), 2);
    start_in_shell_job(&app, 0);

    assert_eq!(
        wire_tab_close(&mut app, Some(0)),
        Err(WIRE_TAB_CLOSE_REFUSED.to_string()),
        "a running job refuses the close of its tab, last or not"
    );
    assert_eq!(app.windows[&wid].tab_set.len(), 2, "both tabs stand");
    assert!(app.pool.get(0).is_some(), "the session was not retired");
    assert!(!app.windows[&wid].pending_close);
    assert!(
        app.pending_action_refusal.is_none(),
        "the refusal was consumed by the reply, not left for the next verb"
    );

    app.close_confirm = CloseConfirm::WireRefuseBusy;
    let close_verb = app.close_session_by_id(0);
    app.close_confirm = CloseConfirm::Interactive;
    assert_eq!(
        close_verb,
        Err(crate::app_tabs::WIRE_PANE_CLOSE_REFUSED.to_string())
    );

    finish_job(&app, 0);
    let state = wire_tab_close(&mut app, Some(0)).expect("an idle non-last tab closes");
    assert_eq!(state.1, 1, "the tab is gone once its job ended");
    assert!(app.pool.get(0).is_none(), "and its session with it");
    assert!(!app.windows[&wid].pending_close, "the window stays open");
}

#[test]
fn a_non_last_tab_close_reads_only_the_jobs_it_would_hang_up() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(1));
    app.push_stub_tab(wid, crate::stub_session(2));
    start_in_shell_job(&app, 0);

    let state = wire_tab_close(&mut app, Some(2)).expect("an idle tab beside a busy one");
    assert_eq!(state.1, 2, "the idle tab closed");
    assert!(app.pool.get(0).is_some(), "the busy sibling's job runs on");

    start_in_shell_job(&app, 1);
    app.pool.attach(1);
    let state = wire_tab_close(&mut app, Some(1)).expect("a co-viewed busy tab closes");
    assert_eq!(state.1, 1, "the co-viewed tab closed");
    assert!(
        app.pool.get(1).is_some(),
        "its session lives on in the other view"
    );
}

#[test]
fn a_headless_busy_non_last_tab_close_proceeds() {
    let mut app = App::headless_for_test();
    assert!(app.headless);
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(1));
    start_in_shell_job(&app, 0);

    let state = wire_tab_close(&mut app, Some(0)).expect("headless never refuses");
    assert_eq!(state.1, 1);
}
