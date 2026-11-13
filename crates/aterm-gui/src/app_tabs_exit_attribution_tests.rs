// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::CloseOutcome;
use crate::app_window::CloseConfirm;
use crate::session_store::{ExitReason, RosterChange};

/// The newest close's reason and closer, from the closed index (6G-9).
fn sole_exit(app: &App) -> (ExitReason, String) {
    let g = app.store.read().unwrap_or_else(|p| p.into_inner());
    let rec = g
        .closed_logs()
        .exits_since(0)
        .next_back()
        .expect("one exit record after the close");
    (rec.reason, rec.actor.as_wire().to_string())
}

fn exit_rows(app: &App) -> usize {
    let g = app.store.read().unwrap_or_else(|p| p.into_inner());
    g.roster_since(0)
        .filter(|r| r.change == RosterChange::Exited)
        .count()
}

fn ctl_close(app: &mut App, session: u64) -> Result<CloseProgress, String> {
    let _closing = session_store::CloseAttribution::enter(
        ExitReason::CtlClose,
        session_store::ExitActor::Unknown,
    );
    app.close_confirm = CloseConfirm::WireRefuseBusy;
    let progress = app.close_session_by_id(session);
    app.close_confirm = CloseConfirm::Interactive;
    progress
}

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

use crate::feed_session_for_test as feed;

fn grid(app: &App, session: u64) -> (u16, u16) {
    let term = app.pool.get(session).expect("live session").term.clone();
    let t = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    (t.rows(), t.cols())
}

fn draw_alt_frame(app: &App, session: u64) {
    let (rows, _) = grid(app, session);
    let mut bytes = b"\x1b[?1049h\x1b[2J\x1b[H".to_vec();
    for r in 0..rows {
        if r > 0 {
            bytes.extend_from_slice(b"\r\n");
        }
        bytes.extend_from_slice(format!("ROW{r}").as_bytes());
    }
    feed(app, session, &bytes);
}

#[test]
fn a_split_closed_at_once_holds_the_siblings_take_back_for_the_quiet() {
    use crate::split_regrid_hold::SPLIT_REGRID_QUIET;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let full = grid(&app, 0);
    draw_alt_frame(&app, 0);

    let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
    let shrunk = grid(&app, 0);
    assert!(
        shrunk.0 < full.0 && shrunk.1 == full.1,
        "the split shrinks the sibling at once: {full:?} -> {shrunk:?}"
    );
    feed(&app, 0, b"\x1b[1;1Hx");

    let progress = ctl_close(&mut app, new).expect("the close proceeds");
    assert_eq!(progress, CloseProgress::Retired);
    assert_eq!(
        app.windows[&wid].layouts[0].sessions(),
        vec![0],
        "the scratch pane is gone; its sibling fills the tab"
    );
    assert_eq!(
        grid(&app, 0),
        shrunk,
        "the take-back is withheld inside the quiet: no N -> N/2 -> N pair"
    );
    app.resize_panes(wid);
    assert_eq!(grid(&app, 0), shrunk, "a re-lay inside the quiet holds");
    let due = app
        .split_regrid_hold_deadline()
        .expect("a withheld take-back owes one wake");

    app.service_split_regrid_holds(due - std::time::Duration::from_millis(1));
    assert_eq!(grid(&app, 0), shrunk);
    app.service_split_regrid_holds(due);
    assert_eq!(grid(&app, 0), full, "the take-back lands after the quiet");
    assert_eq!(app.split_regrid_hold_deadline(), None);
    assert!(due <= std::time::Instant::now() + SPLIT_REGRID_QUIET);
}

#[test]
fn a_mixed_tab_s_split_closed_at_once_holds_the_siblings_take_back_for_the_quiet() {
    for close in ["ctl close", "Cmd-W"] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (sibling, _) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let full = grid(&app, sibling);

        let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
        let shrunk = grid(&app, sibling);
        assert!(
            shrunk.0 < full.0 && shrunk.1 == full.1,
            "{close}: the split shrinks the sibling at once: {full:?} -> {shrunk:?}"
        );
        assert_eq!(app.focused_session_id(wid), Some(new));
        app.split_regrid_holds
            .prolong(std::time::Duration::from_secs(60));

        if close == "Cmd-W" {
            app.close_focused_mixed_leaf(wid)
                .expect("Cmd-W closes the focused leaf");
        } else {
            assert_eq!(ctl_close(&mut app, new), Ok(CloseProgress::Retired));
        }
        assert_eq!(
            app.windows[&wid].tab_set.active().map(|tab| tab.root.len()),
            Some(2),
            "{close}: the scratch pane is gone; Settings and its sibling stay"
        );
        assert_eq!(
            grid(&app, sibling),
            shrunk,
            "{close}: the take-back is withheld inside the quiet: no N -> N/2 -> N pair"
        );
        app.resize_panes(wid);
        assert_eq!(
            grid(&app, sibling),
            shrunk,
            "{close}: a re-lay inside the quiet holds"
        );
        let due = app
            .split_regrid_hold_deadline()
            .expect("a withheld take-back owes one wake");
        app.service_split_regrid_holds(due);
        assert_eq!(
            grid(&app, sibling),
            full,
            "{close}: the take-back lands after the quiet"
        );
        assert_eq!(app.split_regrid_hold_deadline(), None);
    }
}

#[test]
fn the_split_hold_withholds_only_the_take_back_inside_the_quiet() {
    use crate::split_regrid_hold::SPLIT_REGRID_QUIET;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let full = grid(&app, 0);

    let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
    let _ = ctl_close(&mut app, new).expect("the close proceeds");
    assert_ne!(grid(&app, 0), full, "held");
    let bigger = (full.0 + 4, full.1 + 10);
    assert!(app.apply_term_resize(wid, bigger.0, bigger.1));
    assert_eq!(
        grid(&app, 0),
        bigger,
        "a different target applies at once and ends the hold"
    );
    assert_eq!(app.split_regrid_hold_deadline(), None);

    let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Vertical);
    assert_ne!(grid(&app, 0), bigger, "the vertical split narrows it");
    app.split_regrid_holds.age(SPLIT_REGRID_QUIET);
    let _ = ctl_close(&mut app, new).expect("the close proceeds");
    assert_eq!(
        grid(&app, 0),
        bigger,
        "past the quiet the take-back is immediate"
    );
    assert_eq!(app.split_regrid_hold_deadline(), None);
    let reply = app
        .pool
        .get(0)
        .expect("session 0")
        .ctx
        .timeline
        .lock()
        .unwrap()
        .resizes()
        .reply(0, None);
    let last = reply
        .lines()
        .rfind(|line| line.starts_with("resize "))
        .unwrap_or_default();
    let close = std::path::Path::new(file!()).with_file_name("app_tabs.rs");
    assert!(
        last.contains(&format!(" site=pass at={}:", close.display())),
        "the take-back books the close in app_tabs.rs that asked for it: {reply}"
    );
}

#[test]
fn the_newborn_pane_of_a_split_is_never_held() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let full = grid(&app, 0);
    let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
    assert!(grid(&app, new).0 < full.0, "the newborn has half the rect");
    assert!(app.split_regrid_holds.get(new).is_none(), "no hold on it");

    let progress = ctl_close(&mut app, 0).expect("the close proceeds");
    assert_eq!(progress, CloseProgress::Retired);
    assert_eq!(
        grid(&app, new),
        full,
        "the newborn grows to the whole rect at once"
    );
    assert_eq!(app.split_regrid_hold_deadline(), None);
}

#[test]
fn a_zoom_inside_the_quiet_is_not_withheld() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let full = grid(&app, 0);

    let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
    let shrunk = grid(&app, 0);
    app.toggle_pane_zoom();
    assert_eq!(grid(&app, new), full, "the newborn's zoom applies at once");
    app.toggle_pane_zoom();

    assert_eq!(grid(&app, 0), shrunk);
    assert!(
        app.split_regrid_holds.get(0).is_some(),
        "the split armed it"
    );
    assert!(
        app.active_tree_mut(wid)
            .is_some_and(|tree| tree.set_focus(0))
    );
    app.toggle_pane_zoom();
    assert_eq!(grid(&app, 0), full, "the original's zoom applies at once");
    assert!(app.split_regrid_holds.get(0).is_none(), "and ends the hold");
    assert_eq!(app.split_regrid_hold_deadline(), None);
    app.toggle_pane_zoom();
    assert_eq!(grid(&app, 0), shrunk, "unzoom restores the split");
}

#[test]
fn a_ctl_close_on_one_pane_of_a_split_retires_only_that_pane() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let a = app.split_active_stub_tab(wid);
    let b = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
    assert_eq!(app.windows[&wid].layouts[0].sessions(), vec![0, a, b]);
    assert_eq!(
        app.windows[&wid].tab_set.len(),
        1,
        "one tab, split three ways"
    );

    let stable = |app: &App, local| {
        app.store
            .read()
            .unwrap()
            .by_local(local)
            .unwrap()
            .sid
            .clone()
    };
    let closed = stable(&app, a);
    let sibling = stable(&app, b);
    for sid in [&closed, &sibling] {
        app.note_harness_host(crate::harness_host::HostNote::ResumeException {
            sid: sid.as_str().to_string(),
            agent: aterm_phase::Program::Claude,
            standing: Some(
                aterm_agent::supervise::policy::exception::Exception::of_notice(
                    &aterm_agent::supervise::policy::notice::LOGIN_SIGN_IN,
                    Some("main"),
                    None,
                    "the agent working",
                ),
            ),
        });
    }
    let records: Vec<_> = [&closed, &sibling]
        .into_iter()
        .map(|sid| {
            let key = crate::message_reporters::notice_ask_key(sid.as_str(), Some("si"));
            let id = app.messages.live_by_key(&key).expect("its notice").id;
            app.messages.log().get(id).unwrap().clone()
        })
        .collect();
    start_in_shell_job(&app, a);
    let headless = std::mem::replace(&mut app.headless, false);
    assert_eq!(
        ctl_close(&mut app, a),
        Err(WIRE_PANE_CLOSE_REFUSED.to_string())
    );
    app.headless = headless;
    finish_job(&app, a);
    app.teardown_session(u64::MAX);
    app.retire_session_registration(u64::MAX);
    assert!(app.resume_stories.contains_key(closed.as_str()));
    assert!(app.resume_stories.contains_key(sibling.as_str()));

    let progress = ctl_close(&mut app, a).expect("the close proceeds");
    assert_eq!(
        progress,
        CloseProgress::Retired,
        "a pane close retires inside the call: the tab stands, nothing defers"
    );
    assert_eq!(app.close_session_verdict(a, progress), Ok(()));

    assert_eq!(app.windows[&wid].tab_set.len(), 1, "the tab is still there");
    assert!(
        !app.windows[&wid].pending_close,
        "…and its window is not closing"
    );
    assert_eq!(
        app.windows[&wid].layouts[0].sessions(),
        vec![0, b],
        "the aimed pane went; both siblings stayed"
    );
    assert!(
        !app.session_registered(a),
        "the aimed session left the registry"
    );
    assert!(
        app.session_registered(0) && app.session_registered(b),
        "the siblings' sessions still answer"
    );
    assert!(
        app.pool.get(0).is_some() && app.pool.get(b).is_some(),
        "…with their PTYs: nothing else was hung up"
    );
    assert_eq!(exit_rows(&app), 1, "one close, ONE exit row — not three");
    let (reason, _by) = sole_exit(&app);
    assert_eq!(reason, ExitReason::CtlClose);
    assert!(!app.resume_stories.contains_key(closed.as_str()));
    assert!(app.resume_stories.contains_key(sibling.as_str()));
    for record in records {
        assert_eq!(
            app.messages.log().get(record.id),
            Some(&record),
            "retirement forgets the process-local story, not its notice or outcome"
        );
    }
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_ctl_close_aims_at_the_pane_not_at_the_focus_or_the_active_tab() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let a = app.split_active_stub_tab(wid);
    assert_eq!(app.windows[&wid].layouts[0].focus(), a);
    let c = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(c));
    assert_eq!(app.windows[&wid].tab_set.active_index(), Some(1));

    let progress = ctl_close(&mut app, 0).expect("the close proceeds");
    assert_eq!(progress, CloseProgress::Retired);

    assert_eq!(
        app.windows[&wid].tab_set.len(),
        2,
        "the background tab survives its collapse"
    );
    assert_eq!(app.windows[&wid].layouts[0].sessions(), vec![a]);
    assert_eq!(
        app.windows[&wid].layouts[0].focus(),
        a,
        "focus stays on the survivor it already had"
    );
    assert_eq!(
        app.windows[&wid].tab_set.active_index(),
        Some(1),
        "the active tab did not move"
    );
    assert!(!app.session_registered(0));
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_ctl_close_of_a_busy_pane_is_refused_and_a_busy_sibling_never_refuses_one() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let wid = WindowId(0);
    let a = app.split_active_stub_tab(wid);
    start_in_shell_job(&app, a);
    assert!(app.pane_close_hangs_up_a_job(a));
    assert!(
        !app.pane_close_hangs_up_a_job(0),
        "the idle sibling is idle"
    );

    assert_eq!(
        ctl_close(&mut app, a),
        Err(WIRE_PANE_CLOSE_REFUSED.to_string()),
        "the busy pane is refused in the pane's words"
    );
    assert_eq!(
        app.windows[&wid].layouts[0].sessions(),
        vec![0, a],
        "nothing collapsed"
    );
    assert!(
        app.session_registered(a) && app.pool.get(a).is_some(),
        "the busy session was not retired"
    );
    assert_eq!(exit_rows(&app), 0);

    assert_eq!(
        ctl_close(&mut app, 0),
        Ok(CloseProgress::Retired),
        "the idle pane closes although its neighbour is busy"
    );
    assert_eq!(app.windows[&wid].layouts[0].sessions(), vec![a]);

    assert_eq!(
        ctl_close(&mut app, a),
        Err(crate::app_window::WIRE_CLOSE_REFUSED.to_string()),
        "alone in the last tab, the refusal is the last-tab confirm's"
    );
    finish_job(&app, a);
    assert!(!app.pane_close_hangs_up_a_job(a));
    assert_eq!(
        ctl_close(&mut app, a),
        Ok(CloseProgress::Deferred(wid)),
        "idle again, the last-tab close defers as it always did"
    );
    assert!(app.windows[&wid].pending_close);
}

#[test]
fn a_ctl_close_of_a_busy_tab_that_is_not_the_last_is_refused() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let wid = WindowId(0);
    let c = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(c));
    start_in_shell_job(&app, c);

    assert_eq!(
        ctl_close(&mut app, c),
        Err(WIRE_PANE_CLOSE_REFUSED.to_string())
    );
    assert_eq!(app.windows[&wid].tab_set.len(), 2, "both tabs stand");
    assert!(app.pool.get(c).is_some(), "the job's PTY was not hung up");
    assert_eq!(exit_rows(&app), 0);

    finish_job(&app, c);
    assert_eq!(ctl_close(&mut app, c), Ok(CloseProgress::Retired));
    assert_eq!(app.windows[&wid].tab_set.len(), 1);
    assert!(!app.windows[&wid].pending_close);
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_ctl_close_of_a_busy_terminal_leaf_in_a_mixed_tab_is_refused() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (_instance, native_view) = app.active_native_view(wid).unwrap();
    let (session, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let tab_id = app.windows[&wid].tab_set.active_id().unwrap();
    start_in_shell_job(&app, session);

    assert_eq!(
        ctl_close(&mut app, session),
        Err(WIRE_PANE_CLOSE_REFUSED.to_string())
    );
    assert_eq!(
        app.windows[&wid].tab_set.get(tab_id).unwrap().root.leaves(),
        vec![native_view, terminal_view],
        "nothing collapsed"
    );
    assert!(app.pool.get(session).is_some());

    finish_job(&app, session);
    assert_eq!(ctl_close(&mut app, session), Ok(CloseProgress::Retired));
    assert_eq!(
        app.windows[&wid].tab_set.get(tab_id).unwrap().root.leaves(),
        vec![native_view],
        "idle, only the terminal leaf goes; the native sibling keeps the tab"
    );
    assert!(app.pool.get(session).is_none() && !app.session_registered(session));
    assert!(!app.windows[&wid].pending_close);
    assert!(app.structural_invariants_ok());
}

/// A mixed tab's busy terminal leaf (beside a native leaf, as a restore's recovery placeholder
/// leaves it) is not closed unasked: neither its pane close (Cmd-W on the leaf) nor the tab's
/// close from the strip hangs up the job past the close policy, as a terminal tab's are not.
#[test]
fn a_busy_terminal_leaf_in_a_mixed_tab_is_not_closed_unasked() {
    let mut app = App::headless_for_test();
    app.headless = false;
    app.close_confirm = CloseConfirm::WireRefuseBusy;
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (session, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let leaves = |app: &App| {
        app.windows[&wid]
            .tab_set
            .active()
            .map_or(0, |tab| tab.root.len())
    };
    assert_eq!(leaves(&app), 2, "PRECONDITION: a mixed tab");
    assert!(
        app.windows[&wid].tab_set.len() > 1,
        "PRECONDITION: closing the mixed tab leaves the window open"
    );
    start_in_shell_job(&app, session);
    assert!(
        app.pane_close_hangs_up_a_job(session),
        "PRECONDITION: closing the leaf hangs up a job (views={:?}, busy={})",
        app.pool.views(session),
        app.session_foreground_busy(session)
    );

    assert!(app.close_active_tab().is_none());
    assert!(
        app.pool.get(session).is_some(),
        "the busy leaf's pane close is refused"
    );
    assert_eq!(leaves(&app), 2, "and the leaf stays");

    let index = app.windows[&wid]
        .tab_set
        .active_index()
        .expect("the mixed tab is active");
    assert!(!app.close_tab_at(wid, index));
    assert!(
        app.pool.get(session).is_some(),
        "the mixed tab's close is refused while its terminal is busy"
    );

    app.close_confirm = CloseConfirm::Programmatic;
    assert!(app.close_active_tab().is_none());
    assert!(
        !app.pool.get(session).is_some(),
        "control: an instructed close hangs it up"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_busy_tab_that_is_not_the_last_asks_before_it_closes() {
    use crate::close_confirm::{CloseReplay, kind};
    let mut app = App::headless_for_test();
    app.headless = false;
    app.close_confirm = CloseConfirm::Interactive;
    let wid = WindowId(0);
    let first = app.focused_session_id(wid).expect("tab 0");
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    let busy = app.focused_session_id(wid).expect("tab 2, active");
    start_in_shell_job(&app, busy);
    let tabs = |app: &App| app.windows[&wid].tab_set.len();

    assert!(app.close_active_tab().is_none());
    assert_eq!(tabs(&app), 3, "the busy tab stays while it asks");
    let pending = app.close_banner.as_ref().expect("asked in the window");
    assert!(matches!(pending.replay, CloseReplay::Tab { .. }));
    assert_eq!(
        kind(&pending.prompt),
        "close-tab",
        "its one pane is the tab"
    );
    let replay = app.answer_close_banner(true).expect("Enter replays");
    app.close_confirm = CloseConfirm::Programmatic;
    assert_eq!(app.replay_confirmed_view_close(replay), None);
    app.close_confirm = CloseConfirm::Interactive;
    assert_eq!(tabs(&app), 2, "the answer closed it");
    assert!(!app.session_registered(busy), "and its session");

    start_in_shell_job(&app, first);
    assert!(!app.close_tab_at(wid, 0));
    let pending = app.close_banner.as_ref().expect("asked again");
    assert_eq!(kind(&pending.prompt), "close-tab");
    assert!(matches!(pending.replay, CloseReplay::Tab { wid: w, .. } if w == wid));
    assert_eq!(app.answer_close_banner(false), None);
    assert_eq!(tabs(&app), 2, "Escape kept it");

    let pane = app.split_active_stub_tab(wid);
    start_in_shell_job(&app, pane);
    assert!(app.close_active_tab().is_none());
    let pending = app.close_banner.as_ref().expect("the pane asks");
    assert_eq!(kind(&pending.prompt), "close-pane");
    let replay = app.answer_close_banner(true).expect("Enter replays");
    app.close_confirm = CloseConfirm::Programmatic;
    assert_eq!(app.replay_confirmed_view_close(replay), None);
    app.close_confirm = CloseConfirm::Interactive;
    assert!(!app.session_registered(pane), "the pane closed");
    assert_eq!(tabs(&app), 2, "and its tab stayed");

    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    assert!(app.close_active_tab().is_none());
    assert!(app.close_banner.is_none());
    assert_eq!(tabs(&app), 2, "an idle tab closes at once");
}

#[cfg(not(target_os = "macos"))]
fn exit_of(app: &App, session: u64) -> (ExitReason, String) {
    let g = app.store.read().unwrap_or_else(|p| p.into_inner());
    let rec = g
        .closed_logs()
        .exits_since(0)
        .rfind(|e| e.local_id == session)
        .expect("an exit record for the session");
    (rec.reason, rec.actor.as_wire().to_string())
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_confirmed_busy_tab_close_leaves_no_attribution_behind() {
    let mut app = App::headless_for_test();
    app.headless = false;
    app.close_confirm = CloseConfirm::Interactive;
    let wid = WindowId(0);
    let first = app.focused_session_id(wid).expect("tab 0");
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    let busy = app.focused_session_id(wid).expect("tab 1, active");
    start_in_shell_job(&app, busy);

    assert!(app.close_active_tab().is_none(), "it asks");
    let replay = app.answer_close_banner(true).expect("Enter replays");
    app.close_confirm = CloseConfirm::Programmatic;
    assert_eq!(app.replay_confirmed_view_close(replay), None);
    app.close_confirm = CloseConfirm::Interactive;
    assert!(!app.session_registered(busy), "the answer closed it");
    assert_eq!(exit_of(&app, busy), (ExitReason::UiClose, "human".into()));
    assert!(
        app.windows[&wid].pending_close_attribution.is_none(),
        "the replay consumed the question's attribution"
    );

    app.store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .set_state(first, crate::session_store::SessionState::Exited);
    assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
    assert_eq!(exit_of(&app, first), (ExitReason::ShellExit, "-".into()));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_wire_confirmed_busy_tab_close_journals_ctl_close() {
    let mut app = App::headless_for_test();
    app.headless = false;
    app.close_confirm = CloseConfirm::Interactive;
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    let busy = app.focused_session_id(wid).expect("tab 1, active");
    start_in_shell_job(&app, busy);

    let raised = app.invoke_attributed(crate::session_store::ExitActor::Ctl, |app| {
        Ok(format!("{:?}", app.close_active_tab()))
    });
    assert_eq!(raised.as_deref(), Ok("None"), "the close waits");
    let replay = app
        .close_banner
        .as_ref()
        .expect("asked in the window")
        .replay;
    assert_eq!(
        app.answer_close_confirm_from_wire(true),
        Ok("answered=yes kind=close-tab".to_string())
    );
    app.close_confirm = CloseConfirm::Programmatic;
    assert_eq!(app.replay_confirmed_view_close(replay), None);
    app.close_confirm = CloseConfirm::Interactive;
    assert!(!app.session_registered(busy), "the answer closed it");
    assert_eq!(exit_of(&app, busy), (ExitReason::CtlClose, "ctl".into()));
    assert!(app.windows[&wid].pending_close_attribution.is_none());
}

#[test]
fn a_ctl_close_of_a_co_viewed_pane_collapses_it_and_names_the_other_viewer() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let wid = WindowId(0);
    let a = app.split_active_stub_tab(wid);
    let second = app
        .open_active_session_in_new_window_logical()
        .expect("Cmd-Shift-O opens a second viewer");
    assert!(app.window_contains_session(second, a));
    start_in_shell_job(&app, a);
    assert!(
        !app.pane_close_hangs_up_a_job(a),
        "a job with another viewer is not this pane's to hang up"
    );

    assert_eq!(
        ctl_close(&mut app, a),
        Err("close deferred (another window still displays this session)".to_string())
    );
    assert_eq!(
        app.windows[&wid].layouts[0].sessions(),
        vec![0],
        "the pane collapsed here"
    );
    assert!(
        app.session_registered(a),
        "…and the co-viewer keeps the session"
    );
    assert!(app.window_contains_session(second, a));
    assert_eq!(
        exit_rows(&app),
        0,
        "nothing exited: a view was detached, not a shell"
    );
}

#[test]
fn a_ctl_close_of_a_co_viewed_tab_names_the_other_viewer_not_a_refusal() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let c = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(c));
    let second = app
        .open_active_session_in_new_window_logical()
        .expect("Cmd-Shift-O opens a second viewer");
    assert!(app.window_contains_session(second, c));

    let result = ctl_close(&mut app, c);
    assert_eq!(
        result,
        Err("close deferred (another window still displays this session)".to_string())
    );
    assert_eq!(app.windows[&wid].tab_set.len(), 1, "the tab went here");
    assert!(app.window_contains_session(second, c));
    assert!(app.session_registered(c));
}

#[test]
fn a_ctl_close_of_a_pane_twinned_in_the_same_window_names_the_other_tab() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let a = app.split_active_stub_tab(wid);
    let shared = app
        .open_active_session_in_new_window_logical()
        .expect("Cmd-Shift-O shares a");
    assert!(app.window_contains_session(shared, a));
    let _ = app.migrate_active_tab_to_next_window();
    assert_eq!(app.windows.len(), 1, "the share window emptied and closed");
    assert_eq!(app.windows[&wid].tab_set.len(), 2, "tab 1 is a's twin");

    assert_eq!(
        ctl_close(&mut app, a),
        Err("close deferred (another tab still displays this session)".to_string())
    );
    assert_eq!(
        app.windows[&wid].layouts[0].sessions(),
        vec![0],
        "the split pane collapsed"
    );
    assert!(
        app.window_contains_session(wid, a),
        "its twin still shows it"
    );
    assert!(app.session_registered(a));
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_ctl_closed_pane_leaves_the_reopen_record_cmd_w_leaves() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let a = app.split_active_stub_tab(wid);
    app.focus_pane(pane::FocusDir::Left);
    assert_eq!(
        app.windows[&wid].layouts[0].focus(),
        0,
        "the aim is unfocused"
    );
    assert!(!app.can_reopen_closed_view(), "nothing closed yet");

    assert_eq!(ctl_close(&mut app, a), Ok(CloseProgress::Retired));

    assert!(
        app.can_reopen_closed_view(),
        "the unfocused pane's record was kept"
    );
}

#[test]
fn a_last_tab_ctl_close_journals_ctl_close() {
    let mut app = App::headless_for_test();
    app.close_tab_via_verb(WindowId(0), None);
    assert!(
        app.windows[&WindowId(0)].pending_close,
        "closing the only tab defers the window close"
    );
    assert_eq!(app.close_window_logical(WindowId(0)), CloseOutcome::Exit);
    let (reason, _by) = sole_exit(&app);
    assert_eq!(reason, ExitReason::CtlClose);
}

#[test]
fn a_last_tab_ctl_close_is_deferred_not_refused_and_answers_after_the_teardown() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = 0;
    let progress = {
        let _closing = session_store::CloseAttribution::enter(
            session_store::ExitReason::CtlClose,
            session_store::ExitActor::Unknown,
        );
        app.close_session_by_id(session)
            .expect("the close proceeds")
    };
    assert_eq!(
        progress,
        CloseProgress::Deferred(wid),
        "a last-tab close defers; it is not a refusal"
    );
    assert!(app.windows[&wid].pending_close, "the window is flagged");
    assert_eq!(
        app.close_session_verdict(session, progress),
        Err("close deferred (the window teardown has not run)".to_string()),
        "before the escalation the honest answer names the deferral"
    );

    assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
    assert_eq!(
        app.close_session_verdict(session, progress),
        Ok(()),
        "the session is gone: the verb answers OK closed"
    );
    let (reason, _by) = sole_exit(&app);
    assert_eq!(reason, ExitReason::CtlClose);
}

#[test]
fn a_wire_close_never_reaches_the_dialog_and_refuses_only_a_busy_job() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    for (policy, headless, busy, proceeds) in [
        (CloseConfirm::WireRefuseBusy, false, false, true),
        (CloseConfirm::WireRefuseBusy, false, true, false),
        (CloseConfirm::Programmatic, false, true, true),
        (CloseConfirm::WireRefuseBusy, true, true, true),
    ] {
        app.close_confirm = policy;
        app.headless = headless;
        let case = format!("{policy:?} headless={headless} busy={busy}");
        assert_eq!(
            app.confirm_destructive_close(wid, true, busy),
            proceeds,
            "{case}: the last-tab confirm"
        );
        assert_eq!(
            app.pane_close_allowed(busy),
            proceeds,
            "{case}: the pane guard"
        );
    }
    app.headless = false;
    app.close_confirm = CloseConfirm::Interactive;
    assert!(
        app.pane_close_allowed(true),
        "a person's close is not the wire guard's"
    );
}

#[test]
fn a_deferred_close_of_a_co_viewed_session_names_the_other_window() {
    let mut app = App::headless_for_test();
    let session = 0;
    let second = app
        .open_active_session_in_new_window_logical()
        .expect("Cmd-Shift-O opens a second viewer");
    assert!(app.window_contains_session(second, session));

    let progress = {
        let _closing = session_store::CloseAttribution::enter(
            session_store::ExitReason::CtlClose,
            session_store::ExitActor::Unknown,
        );
        app.close_session_by_id(session)
            .expect("the close proceeds")
    };
    assert_eq!(
        progress,
        CloseProgress::Deferred(WindowId(0)),
        "the close aims at the LOWEST window id holding the session"
    );
    assert_eq!(
        app.close_session_verdict(session, progress),
        Err("close deferred (another window still displays this session)".to_string()),
        "the co-viewer is the reason the close cannot take, from the start"
    );

    assert_eq!(app.close_window_logical(WindowId(0)), CloseOutcome::Stay);
    assert!(!app.windows.contains_key(&WindowId(0)), "window 0 is gone");
    assert!(
        app.session_registered(session),
        "the co-viewer's pool view keeps the session registered"
    );
    assert_eq!(
        app.close_session_verdict(session, progress),
        Err("close deferred (another window still displays this session)".to_string()),
        "the teardown DID run: the co-viewer is why the session is still here"
    );
}

#[test]
fn a_wire_close_question_keeps_its_name_through_a_settings_open() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let prompt = crate::quit_safety::confirm_prompt(true, true, true).expect("a busy quit prompts");
    let raised = app.invoke_attributed(crate::session_store::ExitActor::Ctl, |app| {
        Ok(app
            .present_close_banner(wid, prompt, crate::close_confirm::CloseReplay::Window(wid))
            .to_string())
    });
    assert_eq!(
        raised.as_deref(),
        Ok("false"),
        "the gesture waits for its answer"
    );
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(!app.windows[&wid].pending_close, "nothing was parked");
    assert_eq!(
        app.windows[&wid].pending_close_attribution,
        Some((ExitReason::CtlClose, crate::session_store::ExitActor::Ctl)),
        "the standing question's attribution survives the Settings open"
    );
    assert_eq!(
        app.answer_close_confirm_from_wire(true),
        Ok("answered=yes kind=quit".to_string())
    );
    assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
    let (reason, by) = sole_exit(&app);
    assert_eq!(reason, ExitReason::CtlClose);
    assert_eq!(by, "ctl");
}

#[test]
fn a_last_tab_ui_close_journals_ui_close_by_human() {
    let mut app = App::headless_for_test();
    let closed = app
        .close_active_tab()
        .expect("the frontmost's last tab closed");
    assert_eq!(closed, WindowId(0));
    assert_eq!(app.close_window_logical(WindowId(0)), CloseOutcome::Exit);
    let (reason, by) = sole_exit(&app);
    assert_eq!(reason, ExitReason::UiClose);
    assert_eq!(by, "human");
}
