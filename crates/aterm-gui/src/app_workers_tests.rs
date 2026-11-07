// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Workers tab's host (`docs/DESIGN-workers-tab-2026-11-02.md`): how a worker lands, what the
//! strip's consumers make of it, and how it leaves.

use crate::native_workers::{RowState, WorkerRow};
use crate::task_record::{TaskMark, TaskState, mark_session_task};
use crate::{App, WindowId, stub_session};

const WID: WindowId = WindowId(0);

/// An app with `shown` (at least one) person's tabs and a worker for each of `workers`, front on the first tab.
fn app_with(shown: usize, workers: usize) -> (App, Vec<u64>, Vec<u64>) {
    let mut app = App::headless_for_test();
    // The headless app opens with the person's first tab.
    let mut tabs = vec![active_session(&app).expect("the first tab")];
    for _ in 1..shown {
        tabs.push(person_tab(&mut app));
    }
    app.switch_tab_in(WID, 0);
    let mut parked = Vec::new();
    for _ in 0..workers {
        parked.push(land_worker(&mut app));
    }
    (app, tabs, parked)
}

pub(crate) fn person_tab(app: &mut App) -> u64 {
    let id = app.next_session_id;
    app.push_stub_tab(WID, stub_session(id));
    app.sync_window(WID);
    id
}

/// A worker landed parked behind the front, its session's local id.
pub(crate) fn land_worker(app: &mut App) -> u64 {
    let id = app.next_session_id;
    app.next_session_id += 1;
    let parent = app.new_tab_parent(WID);
    app.install_tab_session_parked(WID, stub_session(id), parent);
    app.sync_window(WID);
    id
}

/// An app whose one window holds only its empty Workers list: the person's tab closed, then its
/// last worker.
#[cfg(unix)]
pub(crate) fn app_with_a_lone_list() -> App {
    let (mut app, tabs, workers) = app_with(1, 1);
    assert!(app.close_session_tab_for_test(tabs[0]));
    app.sync_window(WID);
    assert!(app.close_session_tab_for_test(workers[0]));
    app.sync_window(WID);
    assert_eq!(
        set(&app).len(),
        1,
        "PRECONDITION: the list stays with no worker"
    );
    assert_eq!(set(&app).home(), set(&app).active_id());
    app
}

fn set(app: &App) -> &crate::tab_model::TabSet {
    &app.windows[&WID].tab_set
}

fn home_index(app: &App) -> Option<usize> {
    let home = set(app).home()?;
    set(app).tabs().iter().position(|tab| tab.id == home)
}

fn active_session(app: &App) -> Option<u64> {
    let tab = set(app).active()?;
    app.view_store
        .get(tab.focus)
        .copied()
        .and_then(crate::tab_model::View::terminal_session)
}

fn rows(app: &App) -> Vec<WorkerRow> {
    app.worker_rows(WID)
}

fn mark(app: &App, session: u64, state: TaskState) {
    let store = app.store.read().unwrap_or_else(|p| p.into_inner());
    let handle = store.by_local(session).expect("a live session");
    mark_session_task(&handle.ctx, handle.local_id, &TaskMark::new(state)).expect("unfenced");
}

#[test]
fn a_worker_lands_parked_behind_the_front_and_the_list_opens_without_taking_it() {
    let (app, tabs, workers) = app_with(2, 2);
    assert_eq!(set(&app).parked().len(), 2);
    assert_eq!(active_session(&app), Some(tabs[0]), "the front stays");
    assert_eq!(
        set(&app).shown_len(),
        3,
        "the person's two tabs and the one Workers tab"
    );
    assert_eq!(home_index(&app), Some(2));
    assert_eq!(set(&app).strip_active(), Some(0));
    assert_eq!(
        rows(&app).iter().map(|row| row.session).collect::<Vec<_>>(),
        workers
    );
    assert!(set(&app).invariant_holds(&app.view_store));
}

#[test]
fn a_persons_new_tab_never_parks_and_stays_ahead_of_the_workers() {
    let (mut app, _, workers) = app_with(1, 1);
    let tab = person_tab(&mut app);
    assert_eq!(set(&app).parked().len(), 1);
    assert!(!app.session_is_parked(tab));
    assert!(app.session_is_parked(workers[0]));
    assert_eq!(
        active_session(&app),
        Some(tab),
        "a person's tab takes the front"
    );
    assert!(set(&app).invariant_holds(&app.view_store));
}

#[test]
fn the_terminal_projection_stays_in_canonical_order_around_the_workers() {
    let (mut app, _, _) = app_with(1, 2);
    person_tab(&mut app);
    let ws = &app.windows[&WID];
    let terminals = ws
        .tab_set
        .tabs()
        .iter()
        .filter(|tab| {
            matches!(
                app.view_store.get(tab.focus).copied(),
                Some(crate::tab_model::View::Terminal(_))
            )
        })
        .count();
    assert_eq!(ws.layouts.len(), terminals);
    // The projection names the front's terminal, so a key goes where the strip says.
    assert_eq!(
        app.focused_session_id(WID),
        active_session(&app),
        "the focused pane is the active tab's"
    );
}

#[test]
fn the_last_worker_leaving_removes_the_list() {
    let (mut app, tabs, workers) = app_with(1, 2);
    assert!(app.close_session_tab_for_test(workers[0]));
    app.sync_window(WID);
    assert!(home_index(&app).is_some(), "one worker remains");
    assert!(app.close_session_tab_for_test(workers[1]));
    app.sync_window(WID);
    assert_eq!(set(&app).home(), None);
    assert_eq!(set(&app).len(), 1);
    assert_eq!(active_session(&app), Some(tabs[0]));
}

#[test]
fn a_list_alone_in_its_window_stays_closable_when_its_last_worker_ends() {
    let (mut app, tabs, workers) = app_with(1, 1);
    assert!(app.close_session_tab_for_test(tabs[0]));
    app.sync_window(WID);
    assert_eq!(set(&app).shown_len(), 1);
    assert!(app.close_session_tab_for_test(workers[0]));
    app.sync_window(WID);
    let home = set(&app).home().expect("the window keeps its list");
    assert_eq!(set(&app).active_id(), Some(home));
    assert!(!app.workers_tab_holds(WID, home), "the person may close it");
}

#[test]
fn the_list_cannot_be_closed_while_workers_remain() {
    let (mut app, _, _) = app_with(1, 1);
    let at = home_index(&app).expect("a list");
    app.switch_tab_in(WID, at);
    assert_eq!(
        app.close_active_native_tab_keeping(WID, None).unwrap_err(),
        crate::app_workers::WORKERS_TAB_STAYS
    );
    assert_eq!(set(&app).parked().len(), 1);
}

#[test]
fn opening_a_worker_swaps_in_place_and_the_chip_names_it() {
    let (mut app, _, workers) = app_with(1, 2);
    assert!(app.focus_session_preferring(Some(WID), workers[1]));
    assert_eq!(active_session(&app), Some(workers[1]));
    assert_eq!(
        set(&app).strip_active(),
        home_index(&app),
        "the chip stands for the worker it shows"
    );
    let title = {
        let ws = &app.windows[&WID];
        let home = ws.tab_set.get(ws.tab_set.home().unwrap()).unwrap();
        app.workers_chip_title(ws, home).expect("a worker shows")
    };
    assert!(title.starts_with("Workers \u{25b8} "), "{title}");

    // The chip's press is `switch_tab_in` at the home, back to the list.
    app.switch_tab_in(WID, home_index(&app).unwrap());
    assert_eq!(set(&app).active_id(), set(&app).home());
    let ws = &app.windows[&WID];
    let home = ws.tab_set.get(ws.tab_set.home().unwrap()).unwrap();
    assert_eq!(app.workers_chip_title(ws, home), None);
}

#[test]
fn closing_the_worker_on_show_returns_to_the_list() {
    let (mut app, _, workers) = app_with(1, 2);
    assert!(app.focus_session_preferring(Some(WID), workers[0]));
    assert!(app.close_session_tab_for_test(workers[0]));
    app.sync_window(WID);
    assert_eq!(set(&app).active_id(), set(&app).home());
}

#[test]
fn next_previous_and_the_number_keys_reach_only_what_the_strip_draws() {
    let (mut app, tabs, workers) = app_with(1, 1);
    app.cycle_tab_in(WID, true);
    assert_eq!(set(&app).active_id(), set(&app).home());
    app.cycle_tab_in(WID, true);
    assert_eq!(active_session(&app), Some(tabs[0]), "past the list wraps");

    app.switch_shown_tab_in(WID, 2);
    assert_eq!(
        active_session(&app),
        Some(tabs[0]),
        "Cmd-3 draws no chip here, and the worker at canonical 2 stays parked"
    );
    assert!(app.session_is_parked(workers[0]));
    app.switch_shown_tab_in(WID, 1);
    assert_eq!(set(&app).active_id(), set(&app).home());

    // While a worker shows, next walks from its chip.
    assert!(app.focus_session_preferring(Some(WID), workers[0]));
    app.cycle_tab_in(WID, true);
    assert_eq!(active_session(&app), Some(tabs[0]));
}

/// KEEP AS TAB IS THE ONE WAY OUT, AND IT MAKES THE TAB A PERSON'S: the worker leaves the parked
/// tabs with its mark, keeping the front. CONTROL: kept twice, or a person's own tab, nothing moves.
#[test]
fn keep_as_tab_is_the_way_out_and_keeps_the_front() {
    let (mut app, tabs, workers) = app_with(1, 2);
    assert!(app.session_is_ephemeral(workers[0]));
    assert!(app.keep_as_tab(workers[0]));
    assert!(!app.session_is_parked(workers[0]));
    assert!(!app.session_is_ephemeral(workers[0]), "a person's tab now");
    assert_eq!(active_session(&app), Some(tabs[0]));
    assert_eq!(set(&app).parked().len(), 1);
    assert!(set(&app).invariant_holds(&app.view_store));
    assert!(!app.keep_as_tab(workers[0]), "already a tab");
    assert!(!app.keep_as_tab(tabs[0]), "a person's tab has no way in");

    assert!(app.keep_as_tab(workers[1]));
    app.sync_window(WID);
    assert_eq!(set(&app).home(), None, "no worker is left for the list");
    assert_eq!(set(&app).len(), 3);
    assert!(set(&app).invariant_holds(&app.view_store));
}

#[test]
fn a_worker_on_show_kept_as_a_tab_stays_on_show() {
    let (mut app, _, workers) = app_with(1, 1);
    assert!(app.focus_session_preferring(Some(WID), workers[0]));
    assert!(app.keep_as_tab(workers[0]));
    app.sync_window(WID);
    assert_eq!(active_session(&app), Some(workers[0]));
    assert_eq!(set(&app).strip_active(), set(&app).active_index());
    assert_eq!(app.focused_session_id(WID), Some(workers[0]));
}

#[test]
fn a_parked_tab_and_the_list_stay_in_their_window() {
    let (mut app, _, workers) = app_with(1, 1);
    assert!(app.focus_session_preferring(Some(WID), workers[0]));
    assert_eq!(
        app.detach_active_tab_logical(),
        None,
        "a worker moves to a tab first"
    );
    app.switch_tab_in(WID, home_index(&app).unwrap());
    assert_eq!(
        app.detach_active_tab_logical(),
        None,
        "the list stands for them"
    );
    assert_eq!(set(&app).parked().len(), 1);
}

/// AN EMPTY LIST STAYS IN ITS WINDOW TOO: the other window would hold a list nothing publishes to,
/// beside the one its own next worker opens. NEGATIVE CONTROL: a person's tab moves.
#[test]
fn an_empty_list_does_not_move_to_another_window() {
    let (mut app, tabs, workers) = app_with(1, 1);
    let other = app.next_session_id;
    let wid_b = app.insert_logical_window(stub_session(other), 40, 120);
    assert!(app.close_session_tab_for_test(tabs[0]));
    assert!(app.close_session_tab_for_test(workers[0]));
    app.sync_window(WID);
    let home = set(&app).home().expect("the window keeps its empty list");
    assert_eq!(set(&app).active_id(), Some(home));
    assert_eq!(
        app.migrate_active_tab_from_window(WID),
        Err("the Workers tab stays in its window: it lists only that window's workers")
    );
    assert_eq!(set(&app).home(), Some(home));
    assert_eq!(app.windows[&wid_b].tab_set.len(), 1, "nothing moved");
    let mine = app.next_session_id;
    app.push_stub_tab(wid_b, stub_session(mine));
    app.sync_window(wid_b);
    assert!(
        app.migrate_active_tab_from_window(wid_b).is_ok(),
        "CONTROL: a person's tab moves"
    );
}

#[test]
fn a_rows_state_follows_its_task_and_running_comes_first() {
    let (app, _, workers) = app_with(1, 2);
    mark(&app, workers[0], TaskState::Done);
    mark(&app, workers[1], TaskState::Working);
    let rows = rows(&app);
    assert_eq!(rows[0].state, RowState::Finished);
    assert_eq!(rows[1].state, RowState::Working);
    let groups = crate::native_workers::grouped(&rows);
    assert_eq!(groups.len(), 1, "both are the harness's own");
    assert_eq!(groups[0].0, crate::native_workers::ATERM_GROUP);
    assert_eq!(
        groups[0].1[0].session, workers[1],
        "running above concluded"
    );
}

#[test]
fn inspect_says_which_tabs_are_parked() {
    let (app, _, _) = app_with(1, 1);
    let body = app.inspect_tabs_body().join("\n");
    assert_eq!(body.matches(" parked=true").count(), 1, "{body}");
    assert_eq!(body.matches(" parked=false").count(), 2, "{body}");
}

#[test]
fn the_workers_chrome_line_gives_each_its_canonical_index() {
    let (mut app, _, workers) = app_with(1, 2);
    app.frontmost_window = Some(WID);
    let line = app.workers_chrome_line().expect("two workers");
    assert!(
        line.starts_with("toolbar-workers first=2 selected=none "),
        "{line}"
    );
    assert!(app.focus_session_preferring(Some(WID), workers[1]));
    let line = app.workers_chrome_line().expect("two workers");
    assert!(
        line.starts_with("toolbar-workers first=2 selected=3 "),
        "{line}"
    );
    app.keep_as_tab(workers[0]);
    app.keep_as_tab(workers[1]);
    assert_eq!(app.workers_chrome_line(), None, "no parked tab, no line");
}

#[test]
fn a_session_on_a_parked_tab_still_answers_the_control_verbs() {
    let (mut app, _, workers) = app_with(1, 1);
    let sid = app.session_id_text(workers[0]).expect("a sid");
    assert!(app.raise_session_by_id(workers[0]).is_ok());
    assert_eq!(active_session(&app), Some(workers[0]));
    assert!(!sid.is_empty());
    assert!(
        app.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(&aterm_session::SessionId::new(&sid))
            .is_some(),
        "the control plane addresses it by sid whatever the bit says"
    );
}

#[test]
fn opening_the_list_with_no_workers_is_refused() {
    let (mut app, _, _) = app_with(1, 0);
    assert_eq!(
        app.open_workers_tab().unwrap_err(),
        crate::app_workers::NO_WORKERS
    );
}

#[test]
fn opening_the_list_brings_it_to_the_front() {
    let (mut app, _, _) = app_with(1, 1);
    app.frontmost_window = Some(WID);
    assert!(app.open_workers_tab().is_ok());
    assert_eq!(set(&app).active_id(), set(&app).home());
}

#[test]
fn the_native_strip_mirrors_the_shown_tabs_while_workers_come_and_go() {
    let lockstep = |app: &App| {
        let [count, active, seg_count, selected] = crate::tab_strip_conformance::project(app, WID);
        assert_eq!((count, active), (seg_count, selected), "the strip is stale");
        (count, active)
    };
    let (mut app, _, workers) = app_with(2, 1);
    assert_eq!(
        lockstep(&app),
        (3, 0),
        "two tabs and the list; a worker has no chip"
    );

    let parked = land_worker(&mut app);
    assert_eq!(
        lockstep(&app),
        (3, 0),
        "another worker moves nothing on the strip"
    );

    assert!(app.focus_session_preferring(Some(WID), workers[0]));
    app.sync_window(WID);
    assert_eq!(lockstep(&app), (3, 2), "opening a worker selects the chip");

    assert!(app.close_session_tab_for_test(workers[0]));
    app.sync_window(WID);
    assert_eq!(lockstep(&app), (3, 2), "closing it lands on the chip");

    assert!(app.close_session_tab_for_test(parked));
    app.sync_window(WID);
    let (count, _) = lockstep(&app);
    assert_eq!(count, 2, "the list went with its last worker");
}

/// The list on screen with `workers` rows on it.
fn listing(workers: usize) -> (App, Vec<u64>, Vec<u64>) {
    let (mut app, tabs, parked) = app_with(1, workers);
    app.switch_tab_in(WID, home_index(&app).expect("a list"));
    app.sync_window(WID);
    (app, tabs, parked)
}

fn press(key: aterm_types::keyboard::NamedKey) -> crate::input::InputEvent {
    crate::input::InputEvent::Key {
        key: aterm_types::keyboard::Key::Named(key),
        mods: aterm_types::keyboard::Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    }
}

#[test]
fn the_arrows_move_between_rows_and_return_opens_the_one_they_reach() {
    use aterm_types::keyboard::NamedKey::{ArrowDown, ArrowUp, Enter};
    let (mut app, _, workers) = listing(2);
    assert!(app.native_input_event(WID, &press(ArrowDown)));
    assert!(app.native_input_event(WID, &press(ArrowDown)));
    assert!(
        app.native_input_event(WID, &press(ArrowDown)),
        "it stops at the end"
    );
    assert!(app.native_input_event(WID, &press(ArrowUp)));
    assert!(app.native_input_event(WID, &press(Enter)));
    assert_eq!(
        active_session(&app),
        Some(workers[0]),
        "the row the arrows reached"
    );
    assert!(
        app.session_is_parked(workers[0]),
        "opening never unparks it"
    );
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

#[test]
fn a_rows_keep_as_tab_button_keeps_it_and_the_rows_follow() {
    let (mut app, _, workers) = listing(2);
    press_control(&mut app, &format!("workers/keep/{}", workers[0]));
    assert!(!app.session_is_parked(workers[0]));
    assert!(!app.session_is_ephemeral(workers[0]));
    assert!(app.session_is_parked(workers[1]));
    assert_eq!(
        set(&app).active_id(),
        set(&app).home(),
        "the list stays on screen"
    );
    assert_eq!(
        rows(&app).iter().map(|row| row.session).collect::<Vec<_>>(),
        [workers[1]]
    );
}

#[test]
fn a_rows_press_opens_its_worker_in_place() {
    let (mut app, _, workers) = listing(2);
    press_control(&mut app, &format!("workers/open/{}", workers[1]));
    assert_eq!(active_session(&app), Some(workers[1]));
    assert_eq!(set(&app).strip_active(), home_index(&app));
}

#[test]
fn a_worker_has_keep_as_tab_on_its_menu_and_a_persons_tab_does_not() {
    use crate::session_chrome::{ConnVerb, TabMenuEntry};
    let (mut app, _, _) = app_with(1, 1);
    let offered = |entries: &[TabMenuEntry]| {
        entries.iter().any(|entry| {
            matches!(
                entry,
                TabMenuEntry::ConnectionAction {
                    verb: ConnVerb::KeepAsTab,
                    ..
                }
            )
        })
    };
    let worker_at = set(&app).shown_len();
    assert!(offered(&app.tab_context_menu_model(WID, worker_at)));
    assert!(!offered(&app.tab_context_menu_model(WID, 0)));
}

/// A CLOSED WORKER IS NOT ON THE PERSON'S REOPEN STACK, whoever closed it: Reopen Closed Tab brings
/// back the person's own tab closed before it. NEGATIVE CONTROL: that tab is on it.
#[test]
fn a_closed_worker_is_not_on_the_persons_reopen_stack() {
    let (mut app, _, workers) = app_with(2, 1);
    let mine = person_tab(&mut app);
    assert!(app.close_session_tab_for_test(mine));
    app.sync_window(WID);
    let stacked = app.closed_recovery.tabs.len();
    assert!(stacked >= 1, "CONTROL: the person's tab is kept");
    assert!(app.close_session_tab_for_test(workers[0]));
    app.sync_window(WID);
    assert_eq!(app.closed_recovery.tabs.len(), stacked, "the worker is not");
}

#[test]
fn a_worker_split_by_hand_keeps_its_row_and_gains_one_for_the_pane() {
    let (mut app, _, workers) = app_with(1, 1);
    mark(&app, workers[0], TaskState::Working);
    assert!(app.focus_session_preferring(Some(WID), workers[0]));
    let shell = app.next_session_id;
    app.install_split_pane(WID, crate::pane::SplitDir::Vertical, stub_session(shell))
        .expect("a pane beside the worker");
    assert_eq!(
        app.focused_session_id(WID),
        Some(shell),
        "the new shell has focus"
    );
    let rows = rows(&app);
    assert_eq!(
        rows.iter()
            .map(|row| (row.session, row.state))
            .collect::<Vec<_>>(),
        [(workers[0], RowState::Working), (shell, RowState::Idle)],
        "the running worker stays listed whichever pane has focus"
    );
    assert_eq!(
        crate::native_workers::summary_words(&rows),
        "1 working, 1 idle"
    );
}

#[test]
fn spawn_split_is_refused_in_words_while_a_worker_is_in_front() {
    let (mut app, tabs, workers) = app_with(1, 1);
    app.frontmost_window = Some(WID);
    assert!(app.focus_session_preferring(Some(WID), workers[0]));
    let (reply, answer) = std::sync::mpsc::channel();
    app.spawn_session_aimed(
        crate::control::SpawnAim::Front,
        None,
        Some(crate::pane::SplitDir::Vertical),
        false,
        None,
        None,
        None,
        reply,
    );
    assert_eq!(
        answer.recv().expect("answered"),
        Err(crate::tab_spawn::WORKER_SPLIT_REFUSED.to_owned())
    );
    assert_eq!(set(&app).parked()[0].root.leaves().len(), 1);

    // CONTROL: from a person's tab it goes on to the split's own checks, which name no worker.
    app.switch_tab_in(WID, 0);
    assert_eq!(active_session(&app), Some(tabs[0]));
    let (reply, answer) = std::sync::mpsc::channel();
    app.spawn_session_aimed(
        crate::control::SpawnAim::Front,
        None,
        Some(crate::pane::SplitDir::Vertical),
        false,
        None,
        None,
        None,
        reply,
    );
    assert_ne!(
        answer.try_recv().ok(),
        Some(Err(crate::tab_spawn::WORKER_SPLIT_REFUSED.to_owned()))
    );
}

/// The worker's tab id and the chip's menu as the native strip takes them, with `workers[0]` shown.
fn chip_menu(app: &mut App) -> (crate::session_chrome::TabChromeExt, crate::tab_model::TabId) {
    let home = home_index(app).expect("a chip");
    let titles = app.tab_titles(WID);
    let ext = app.tab_chrome_ext(WID, &titles);
    let shown = set(app).active().expect("a worker").id;
    (ext[home].clone(), shown)
}

#[test]
fn every_row_of_the_chips_menu_acts_on_the_worker_it_shows() {
    use crate::menu::MenuAction;
    use crate::session_chrome::TabMenuEntry;
    let (mut app, _, workers) = app_with(1, 2);
    let home = set(&app).home().expect("a chip");
    assert!(app.focus_session_preferring(Some(WID), workers[0]));
    let (ext, shown) = chip_menu(&mut app);
    assert_eq!(ext.menu_tab, Some(shown), "the rows go to the worker's tab");
    let origin = ext
        .menu
        .iter()
        .find_map(|entry| match entry {
            TabMenuEntry::SessionAction { origin, .. } => *origin,
            _ => None,
        })
        .expect("the worker's session rows, bound to its pane");
    assert_eq!(origin.session, workers[0]);
    assert_eq!(
        app.terminal_menu_origin_index(WID, home, origin),
        None,
        "the chip's own tab holds no such pane: what used to drop them"
    );
    let at = app
        .terminal_menu_origin_index(WID, shown, origin)
        .expect("its own tab does");

    // Rename: the row works, and its editor stands over the chip that stands for the worker.
    app.dispatch_tab_menu_session_action(WID, shown, origin, MenuAction::SetRole);
    if let Some(edit) = app.inline_rename_edit(WID) {
        assert_eq!(edit.session, workers[0]);
        assert_eq!(set(&app).chip_of(edit.tab), Some(home));
        app.end_session_rename_editor(WID);
    } else {
        assert!(!app.tab_strip_enabled(), "no surface is the only excuse");
    }
    assert_eq!(
        active_session(&app),
        Some(workers[0]),
        "the worker stays on show"
    );

    // Close Tab: as the native row's `close_tab_at` of the tab it was sent.
    let index = app.tab_index_for_id(WID, shown).expect("its index");
    assert_eq!(index, at);
    assert!(!app.close_tab_at(WID, index));
    app.sync_window(WID);
    assert!(!app.session_is_parked(workers[0]), "the worker closed");
    assert_eq!(set(&app).parked().len(), 1);
    assert_eq!(set(&app).active_id(), Some(home), "and the list shows");

    // The list in front: the chip's menu is its own again.
    let titles = app.tab_titles(WID);
    let ext = app.tab_chrome_ext(WID, &titles);
    assert_eq!(ext[home_index(&app).unwrap()].menu_tab, None);
}

#[test]
fn keeping_a_worker_refreshes_the_chip_while_the_list_is_behind() {
    let (mut app, _, workers) = app_with(1, 2);
    mark(&app, workers[0], TaskState::Working);
    mark(&app, workers[1], TaskState::Done);
    let (_, _, instance) = app.workers_page(WID).expect("a page");
    assert!(app.focus_session_preferring(Some(WID), workers[0]));
    app.publish_workers(WID);
    let home_presentation = |app: &App| {
        set(app)
            .get(set(app).home().expect("a chip"))
            .expect("its tab")
            .presentation
            .clone()
    };
    assert!(home_presentation(&app).indicators.busy, "one is working");

    assert!(app.keep_as_tab(workers[0]));
    assert_eq!(
        app.native_runtime.workers_listed(instance),
        [workers[1]],
        "any change of who is parked publishes, list in front or not"
    );
    assert!(!home_presentation(&app).indicators.busy, "none is working");
    assert_eq!(
        home_presentation(&app).tooltip.as_deref(),
        Some("Workers: 1 finished")
    );
}

#[test]
fn a_close_of_the_list_says_why_it_stays() {
    let (mut app, _, _) = app_with(1, 1);
    app.switch_tab_in(WID, home_index(&app).unwrap());
    assert_eq!(app.close_active_tab_in(WID), None);
    assert_eq!(set(&app).parked().len(), 1, "nothing closed");
    assert!(set(&app).home().is_some());
    assert_eq!(
        app.pending_action_refusal.take().as_deref(),
        Some(crate::app_workers::WORKERS_TAB_STAYS)
    );
    assert!(
        app.messages.live_rows().any(|row| row
            .msg
            .detail
            .iter()
            .any(|line| line.contains(crate::app_workers::WORKERS_TAB_STAYS))),
        "the person sees the words"
    );
}

#[test]
fn no_surface_names_a_worker_by_a_tab_number() {
    use crate::consent_card::TabPlace;
    let (mut app, tabs, workers) = app_with(1, 2);
    assert!(app.focus_session_preferring(Some(WID), workers[1]));
    let meta = |session: u64, role: &str| {
        let handle = app
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .cloned()
            .expect("live");
        handle.ctx.meta.lock().unwrap().role = Some(role.to_owned());
    };
    meta(workers[1], "bench");
    let sid = app.session_id_text(workers[1]).expect("a sid");
    assert_eq!(app.upgrade_place(&sid), "in Workers \u{25b8} bench");
    assert_eq!(app.deck_tab(WID).1, "Workers \u{25b8} bench");
    assert_eq!(
        app.tab_place_of_session(workers[1]),
        Some(TabPlace::Workers)
    );
    let index = set(&app).active_index().unwrap();
    let tab = set(&app).active().unwrap();
    assert_eq!(app.tab_name(tab, index), "Workers \u{25b8} bench");

    // CONTROL: a person's tab keeps its number on all of them.
    let front = app.session_id_text(tabs[0]).expect("a sid");
    assert_eq!(app.upgrade_place(&front), "in tab 1");
    assert_eq!(app.tab_place_of_session(tabs[0]), Some(TabPlace::Number(1)));
    app.switch_tab_in(WID, 0);
    assert_eq!(app.deck_tab(WID).1, "Tab 1");
}

#[test]
fn a_window_holding_only_an_empty_list_is_not_captured() {
    let (mut app, tabs, workers) = app_with(1, 1);
    assert_eq!(
        app.capture_restore_manifest().windows.len(),
        1,
        "CONTROL: a window with tabs is"
    );
    assert!(app.close_session_tab_for_test(tabs[0]));
    app.sync_window(WID);
    assert_eq!(
        app.capture_restore_manifest().windows.len(),
        1,
        "the worker alone behind the list still comes back"
    );
    assert!(app.close_session_tab_for_test(workers[0]));
    app.sync_window(WID);
    assert_eq!(set(&app).len(), 1, "the list stays with no worker");
    assert_eq!(
        app.capture_restore_manifest().windows.len(),
        0,
        "nothing in it to bring back"
    );
}

/// A handoff carries a lone list only for a target that rebuilds it, as the row the successor
/// reads, and with the window's frame. A quit, whose next launch opens its own window, never does.
#[test]
#[cfg(unix)]
fn a_handoff_carries_a_lone_list_only_for_a_target_that_rebuilds_it() {
    let app = app_with_a_lone_list();
    assert_eq!(app.capture_restore_manifest().windows.len(), 0, "a quit");
    assert_eq!(
        app.capture_handoff_layout().windows.len(),
        0,
        "a target with no desk1, whose emptiness tells the lane the desk holds no tab"
    );
    let carried = app.capture_handoff_layout_for(true);
    assert_eq!(carried.windows.len(), 1, "a target that declares desk1");
    let row = &carried.windows[0];
    assert!(row.is_lone_workers_list(), "{row:?}");
    assert_eq!(row.active_item, None, "the read-back's shape check holds");
    assert!(!carried.is_empty());
    let wire = carried.to_toml().expect("the row serializes");
    assert_eq!(
        crate::restore::RestoreManifest::from_toml(&wire).as_ref(),
        Some(&carried)
    );

    let (with_a_tab, _, _) = app_with(1, 1);
    let tabbed = with_a_tab.capture_handoff_layout_for(true);
    assert!(
        !tabbed.carries_lone_workers_list(),
        "CONTROL: a list with a worker behind it is not lone"
    );
    assert_eq!(
        (row.rows, row.cols),
        (tabbed.windows[0].rows, tabbed.windows[0].cols),
        "the row keeps the window's grid"
    );
}

/// Allocation opens as its own tab rather than split a Workers list: a pane beside the list would
/// go with its tab, which no capture can describe, and an empty list would still read as all its
/// window holds. Beside a list with workers behind it, the list stays as it was.
#[cfg(unix)]
#[test]
fn allocation_never_splits_a_workers_list() {
    let mut app = app_with_a_lone_list();
    app.frontmost_window = Some(WID);
    app.open_allocation(true).expect("Allocation opens");
    app.sync_window(WID);
    assert_eq!(
        set(&app).home(),
        None,
        "the empty list retires beside a tab"
    );
    assert_eq!(set(&app).shown_len(), 1);
    let tab = set(&app).active().expect("Allocation's tab");
    assert_eq!(tab.root.leaves().len(), 1, "Allocation alone");
    let (instance, _) = app.active_native_view(WID).expect("a native tab");
    assert_eq!(
        app.native_runtime.app(instance).expect("its app").kind(),
        crate::native_app::AppKind::Allocation
    );

    let (mut app, _, _) = app_with(1, 1);
    app.frontmost_window = Some(WID);
    app.bring_workers_list_front(WID);
    assert!(app.workers_list_on_screen(WID), "PRECONDITION");
    let home = set(&app).home().expect("PRECONDITION: a worker's list");
    app.open_allocation(true).expect("Allocation opens");
    app.sync_window(WID);
    assert_eq!(
        set(&app).home(),
        Some(home),
        "the list stands for its worker"
    );
    let list = set(&app)
        .tabs()
        .iter()
        .find(|tab| tab.id == home)
        .expect("the list");
    assert_eq!(list.root.leaves().len(), 1, "with no pane beside it");
}

/// The desk's facts read the window, not the row: a list stands alone with no tab beside it and
/// no worker behind it.
#[test]
#[cfg(target_os = "macos")]
fn a_list_stands_alone_only_with_nothing_else_in_its_window() {
    let alone = |app: &App| crate::app_workers::workers_list_stands_alone(&app.windows[&WID]);
    assert!(alone(&app_with_a_lone_list()));
    assert!(!alone(&App::headless_for_test()), "CONTROL: no list");
    assert!(!alone(&app_with(1, 1).0), "CONTROL: a tab and a worker");
    let (mut app, tabs, _) = app_with(1, 1);
    assert!(app.close_session_tab_for_test(tabs[0]));
    app.sync_window(WID);
    assert_eq!(set(&app).shown_len(), 1, "PRECONDITION");
    assert!(!alone(&app), "CONTROL: a worker behind the list");
}

/// What a lone list comes back as is a working list, and the next handoff carries it the same.
#[test]
#[cfg(unix)]
fn a_lone_list_round_trips_a_handoff() {
    let parent = app_with_a_lone_list();
    let wire = parent
        .capture_handoff_layout_for(true)
        .to_toml()
        .expect("the row serializes");
    let mut layout =
        crate::restore::RestoreManifest::from_toml(&wire).expect("the successor reads it");
    let mut successor = App::headless_for_test();
    successor.handoff_successor = true;
    successor.restore_into_window(WID, layout.windows.remove(0));
    assert_eq!(
        successor.capture_handoff_layout_for(true),
        parent.capture_handoff_layout_for(true),
        "the next handoff carries the same row"
    );
    assert_eq!(
        successor.capture_handoff_layout().windows.len(),
        0,
        "and a target with no desk1 is still handed nothing"
    );
    let home = set(&successor).home().expect("a list");
    assert!(
        !successor.workers_tab_holds(WID, home),
        "the person may close it"
    );
    assert!(
        set(&successor).tabs()[0].presentation.closable,
        "and its tab says so"
    );
    successor
        .open_workers_tab()
        .expect("Agents > Workers finds it");
    let tab = person_tab(&mut successor);
    assert_eq!(
        set(&successor).home(),
        None,
        "a person's tab retires the list"
    );
    assert_eq!(active_session(&successor), Some(tab));
}

/// A successor's lone list reaches the glass: its window paints, so the handoff's paint proof can
/// go. A window whose tab set stood empty would present nothing.
#[test]
fn a_lone_list_reaches_the_glass() {
    use crate::scripted_redraw_fixture::scripted_app;
    let mut app = scripted_app();
    let serial = |app: &App| app.windows[&WID].capture_present_serial;
    let before = serial(&app);
    app.restore_into_window(WID, crate::restore::WindowLayout::lone_workers_list(24, 80));
    assert_eq!(set(&app).home(), set(&app).active_id());
    app.redraw_window(WID);
    assert!(serial(&app) > before, "the list presented");
    assert!(app.windows[&WID].painted_for_handoff());
}

fn parked_record(app: &App) -> crate::restore::RestoredTab {
    let tab = set(app).parked().first().expect("a worker");
    app.tab_restore_descriptor(tab).expect("captured")
}

fn terminal_leaf(record: &crate::restore::RestoredTab) -> &crate::restore::TerminalLeafRestore {
    match &record.root {
        crate::restore::RestoredSplitTree::Leaf {
            view: crate::restore::RestoredView::Terminal(leaf),
        } => leaf,
        other => panic!("a terminal leaf, not {other:?}"),
    }
}

#[test]
fn a_parked_tab_captures_its_workers_record_and_a_persons_does_not() {
    let (app, tabs, workers) = app_with(1, 1);
    mark(&app, workers[0], TaskState::Working);
    mark(&app, tabs[0], TaskState::Working);
    let record = parked_record(&app);
    let row = terminal_leaf(&record).task.clone().expect("the record");
    assert!(row.contains("state=working"), "{row}");
    assert!(
        !row.contains("age_ms"),
        "its ages would write every image anew: {row}"
    );
    assert!(record.parked && record.ephemeral, "a worker's tab, marked");
    let shown = app
        .tab_restore_descriptor(&set(&app).tabs()[0])
        .expect("captured");
    assert_eq!(terminal_leaf(&shown).task, None);
    assert!(!shown.parked && !shown.ephemeral, "CONTROL: a person's tab");
    let wrote = aterm_toml::to_string(&record).expect("written");
    let read: crate::restore::RestoredTab = aterm_toml::from_str(&wrote).expect("read");
    assert_eq!(terminal_leaf(&read).task, Some(row));
    assert!(read.ephemeral);
}

#[test]
fn a_persons_stop_worker_press_asks_first_and_stops_only_on_stop() {
    let (mut app, _, workers) = listing(1);
    mark(&app, workers[0], TaskState::Working);
    app.publish_workers(WID);
    let stop = format!("workers/stop/{}", workers[0]);
    app.agents.owner_sheet_for_test = Some(Some(false));
    press_control(&mut app, &stop);
    assert_eq!(app.agents.stop_signals, 0, "Cancel stops nothing");
    app.agents.owner_sheet_for_test = Some(Some(true));
    press_control(&mut app, &stop);
    assert_eq!(app.agents.stop_signals, 1, "Stop signals once");
}

/// THE LIST ON SCREEN IS A LOOK AT ITS CONCLUDED ROWS: a worker that finished behind it reads seen
/// once the list is in front of a looked-at window. NEGATIVE CONTROL: the person's own tab in front
/// sees no worker's end.
#[test]
fn the_list_on_screen_is_a_look_at_its_concluded_rows() {
    let (mut app, _, workers) = app_with(1, 2);
    mark(&app, workers[0], TaskState::Done);
    mark(&app, workers[1], TaskState::Working);
    app.frontmost_window = Some(WID);
    app.person_look = Some(WID);
    app.rebuild_teams();
    app.note_done_seen(WID);
    let entered = |app: &App, session: u64| {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let handle = store.by_local(session).expect("a live session");
        let timeline = handle.ctx.timeline.lock().unwrap();
        timeline.task().map(|task| task.entered)
    };
    assert_ne!(
        app.done_seen.get(&workers[0]).copied(),
        entered(&app, workers[0]),
        "CONTROL: the person's tab in front sees no worker's end"
    );
    app.switch_tab_in(WID, home_index(&app).expect("a list"));
    app.note_done_seen(WID);
    assert_eq!(
        app.done_seen.get(&workers[0]).copied(),
        entered(&app, workers[0]),
        "the row says how it ended"
    );
    assert_eq!(app.done_seen.get(&workers[1]), None, "nothing ended yet");
}

/// TWO WINDOWS' WORKERS PAGES KEEP THEIR OWN REVISIONS AND ACTIONS: publishing on window 0 bumps
/// only window 0's Workers view revision, and draining window 0's page leaves a pending action on
/// window 1's page untouched until window 1 drains its own instance. NEGATIVE CONTROL: draining
/// window 1's own instance then runs its queued action.
#[test]
fn two_windows_workers_pages_keep_their_own_actions_and_revisions() {
    let other = WindowId(1);
    let (mut app, _, workers0) = listing(1);
    let person1 = app.next_session_id;
    app.next_session_id += 1;
    app.install_window_state(other, stub_session(person1), 38, 132);
    app.sync_window(other);
    let worker1 = app.next_session_id;
    app.next_session_id += 1;
    let parent1 = app.new_tab_parent(other);
    app.install_tab_session_parked(other, stub_session(worker1), parent1);
    app.sync_window(other);

    let (_, view0, instance0) = app.workers_page(WID).expect("window 0 page");
    let (_, view1, instance1) = app.workers_page(other).expect("window 1 page");
    assert_ne!(instance0, instance1);

    let rev = |app: &App, view| {
        app.native_runtime
            .view_state(view)
            .expect("workers view")
            .common()
            .presentation_revision
    };
    let rev0_before = rev(&app, view0);
    let rev1_before = rev(&app, view1);
    mark(&app, workers0[0], TaskState::Done);
    app.publish_workers(WID);
    assert_ne!(
        rev(&app, view0),
        rev0_before,
        "window 0's revision advances"
    );
    assert_eq!(
        rev(&app, view1),
        rev1_before,
        "window 1's revision stays put"
    );

    // Queue an action directly on window 1's WorkersApp without draining window 1 yet.
    app.native_runtime
        .dispatch(
            instance1,
            view1,
            crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new(format!("workers/keep/{worker1}")),
                value: None,
            }),
        )
        .expect("dispatch on window 1");
    assert!(app.session_is_parked(worker1));

    app.drain_workers_actions(WID, instance0);
    assert!(
        app.session_is_parked(worker1),
        "draining window 0 leaves window 1's queued action untouched"
    );

    app.drain_workers_actions(other, instance1);
    assert!(
        !app.session_is_parked(worker1),
        "CONTROL: draining window 1 runs its queued Keep as Tab"
    );
}

/// OVERVIEW AND `read_result` AGREE WITH `result_to_read` ACROSS EXIT AND REOPEN: a landed worker
/// whose agent later exits keeps `has_result = true` in Overview, whereas a reopened task refuses
/// `read_result` instead of opening a stale prior-run file. NEGATIVE CONTROL: before reopening, the
/// exited landed task's result path is accepted by `result_to_read`.
#[test]
fn overview_and_read_result_agree_with_result_to_read_across_exit_and_reopen() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let result_file = dir.path().join("worker-result.md");
    std::fs::write(&result_file, "# Landed\n\nAll checks passed.\n").expect("result file");

    let (mut app, _, workers) = app_with(1, 1);
    let worker = workers[0];
    let sid = {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let handle = store.by_local(worker).expect("live worker");
        let mut landed = TaskMark::new(TaskState::Done);
        landed.why = Some("landed");
        landed.result_bytes = 30;
        landed.result_path = Some(result_file.clone());
        mark_session_task(&handle.ctx, worker, &landed).expect("mark done");
        let mut exited = TaskMark::new(TaskState::Exited);
        exited.why = Some("exit");
        exited.exit = Some(0);
        exited.result_bytes = 30;
        exited.result_path = Some(result_file.clone());
        mark_session_task(&handle.ctx, worker, &exited).expect("mark exited after landing");
        handle.sid.clone()
    };

    let (groups, _) = app.overview_parts();
    let chip = groups
        .iter()
        .flat_map(|g| g.chips.iter())
        .find(|c| c.sid == sid)
        .expect("worker chip in overview");
    assert!(
        chip.has_result,
        "CONTROL: an agent exit after landing keeps Read Result available in Overview"
    );

    // Simulate a reopened working task whose record still carries stale result metadata.
    {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let handle = store.by_local(worker).expect("live worker");
        let mut timeline = handle
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut reopened = timeline.task().cloned().expect("task record");
        reopened.state = TaskState::Working;
        reopened.result_bytes = 30;
        reopened.result_path = Some(result_file);
        timeline.seed_task(reopened);
    }
    assert_eq!(
        app.read_result(WID, worker),
        Err("no result to read".to_owned()),
        "a reopened working task refuses stale result_path"
    );
}

/// A WORKER WHOSE AGENT HAS GONE KEEPS ITS AGENT'S NAME: its tab's title is its shell's folder by
/// then (a live lab's stopped worker read `/private/tmp/at.QAnl/proj`), so its row says the agent.
/// NEGATIVE CONTROLS: a running worker and one stalled at a screen read their tab's own title
/// (Claude Code titles its tab by its topic), and a pinned title names a worker whatever its agent
/// did.
#[test]
fn a_worker_whose_agent_has_gone_keeps_its_agents_name() {
    let (app, _, workers) = app_with(1, 4);
    let cases = [
        (TaskState::Exited, None),
        (TaskState::Working, None),
        (TaskState::Stalled, Some("held")),
        (TaskState::Exited, None),
    ];
    for (&worker, (state, why)) in workers.iter().zip(cases) {
        let term = app.pool.get(worker).expect("pooled").term.clone();
        crate::term_lock(&term).process(b"\x1b]2;/private/tmp/lab/proj\x07");
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let handle = store.by_local(worker).expect("a live session");
        let mut mark = TaskMark::new(state);
        mark.why = why;
        mark_session_task(&handle.ctx, handle.local_id, &mark).expect("unfenced");
        handle
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_task_worker(aterm_phase::Program::Claude, None);
    }
    {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let handle = store.by_local(workers[3]).expect("a live session");
        handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .user_title = Some("release notes".to_owned());
    }
    let rows = rows(&app);
    let title = |session: u64| {
        rows.iter()
            .find(|row| row.session == session)
            .map(|row| row.title.clone())
            .expect("a row")
    };
    assert_eq!(title(workers[0]), "Claude Code", "exited: the agent's name");
    for &running in &workers[1..3] {
        assert_eq!(
            title(running),
            app.tab_label_of_session(running),
            "its agent is there: the tab's own title"
        );
        assert_ne!(title(running), "Claude Code");
    }
    assert_eq!(title(workers[3]), "release notes", "a pinned title wins");
}

/// What the shell's integration writes as it shows its prompt in `folder`
/// (`aterm_shell_integration.zsh`'s precmd): the tab's title, the folder, then the prompt's marks.
pub(crate) fn prompt_in(folder: &str) -> Vec<u8> {
    format!("\x1b]0;{folder}\x07\x1b]133;A\x07% \x1b]133;B\x07").into_bytes()
}

/// What the shell's integration writes as it starts running `line` at its prompt
/// (`aterm_shell_integration.zsh`'s preexec): the line for OSC 633;E, encoded as
/// `__aterm_encode_cmd` encodes it, then the tab's title, the line itself.
pub(crate) fn line_starts(line: &str) -> Vec<u8> {
    let encoded: String = line
        .chars()
        .map(|c| match c {
            '\\' => "\\\\".to_owned(),
            ';' => "\\x3b".to_owned(),
            ' ' => "\\x20".to_owned(),
            c if c.is_control() => format!("\\x{:02x}", u32::from(c)),
            c => c.to_string(),
        })
        .collect();
    format!("\x1b]633;E;{encoded}\x07\x1b]0;{line}\x07\x1b]133;C\x07").into_bytes()
}

/// A WORKER'S TAB IS NAMED BY ITS PROGRAM WHILE ITS SHELL TITLES IT BY THE LAUNCH LINE: a shell
/// titles its tab by its folder at a prompt and by each line it starts, and a spawn's line opens
/// with environment words, so a worker's first Autopilot record named it `Workers ▸
/// CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 'c` (or by its folder, recorded before its line), and its
/// landing, once Claude Code had titled the tab, `Workers ▸ Claude Code`. Each names it Claude Code
/// now, a busy terminal's as the strip last read it, and a worker whose shell runs another agent
/// by the one in front. CONTROLS: a worker whose Claude Code titled the tab by its topic keeps
/// that title, and a tab on the strip, a kept worker's or a person's, keeps the line its shell
/// titled it by, as its chip reads, whatever agent the line starts.
#[test]
fn a_workers_tab_is_named_by_its_program_while_its_shell_titles_it_by_the_launch_line() {
    use aterm_agent::supervise::Autopilot;
    let (mut app, tabs, workers) = app_with(2, 3);
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let launch = " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 'claude' --permission-mode 'acceptEdits' \
                  \"$(cat '/private/tmp/lab/w1/task.md')\"";
    let ctx = |app: &App, session: u64| app.pool.get(session).expect("pooled").ctx.clone();
    let term = |app: &App, session: u64| app.pool.get(session).expect("pooled").term.clone();
    let typed = |app: &App, session: u64, bytes: &[u8]| {
        crate::term_lock(&term(app, session)).process(bytes);
    };
    let claude_worker = |app: &App, session: u64| {
        mark(app, session, TaskState::Working);
        ctx(app, session)
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_task_worker(aterm_phase::Program::Claude, None);
    };
    let in_front = |app: &App, session: u64, program: &str| {
        let ctx = ctx(app, session);
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(4242);
        timeline.set_program(4242, Some(program.to_owned()));
    };
    let record_land = |app: &mut App, session: u64| {
        let sid = app.session_id_text(session).expect("a sid");
        assert!(
            crate::session_timeline::apply_autopilot(
                &ctx(app, session),
                Some("land".to_owned()),
                None,
                aterm_slog::Actor::Aterm,
                None,
            )
            .moved
        );
        app.on_meta_changed(session);
        let key = format!("harness.autopilot.{sid}");
        app.messages
            .log()
            .records()
            .find(|r| r.key.as_deref() == Some(key.as_str()))
            .map(|r| (r.title.clone(), r.detail.first().cloned()))
            .expect("the Autopilot record")
    };
    let named = (
        "Autopilot set to land".to_owned(),
        Some(format!(
            "Workers \u{25b8} Claude Code: {}",
            crate::autopilot::mode_words(Autopilot::Land).record
        )),
    );
    for &worker in &workers {
        claude_worker(&app, worker);
        typed(&app, worker, &prompt_in("~/proj"));
    }

    assert_eq!(
        record_land(&mut app, workers[0]),
        named,
        "a worker recorded at its prompt, before its launcher typed its line, is named by its agent"
    );
    typed(&app, workers[0], &line_starts(launch));
    typed(&app, workers[1], &line_starts(launch));
    assert_eq!(
        record_land(&mut app, workers[1]),
        named,
        "a tab whose title is still the spawn's launch line is named by its program"
    );

    app.tab_titles(WID);
    {
        let busy = term(&app, workers[1]);
        let _reading = crate::term_lock(&busy);
        assert_eq!(
            app.tab_label_of_session(workers[1]),
            "Claude Code",
            "a busy terminal's worker is named as the strip last read its title"
        );
    }

    typed(&app, workers[0], b"\x1b]0;\xe2\x9c\xb3 Claude Code\x07");
    let sid = app.session_id_text(workers[0]).expect("a sid");
    assert_eq!(
        (app.upgrade_place(&sid), app.session_place(&sid)),
        (
            "in Workers \u{25b8} Claude Code".to_owned(),
            "in Workers \u{25b8} Claude Code".to_owned()
        ),
        "the landing's record names the tab Claude Code titled the same"
    );
    typed(
        &app,
        workers[1],
        b"\x1b]0;\xe2\x9c\xb3 Fix the tab names\x07",
    );
    assert_eq!(
        app.tab_label_of_session(workers[1]),
        "Fix the tab names",
        "CONTROL: Claude Code's own title names its tab"
    );

    typed(&app, workers[2], &line_starts("codex --model o3"));
    in_front(&app, workers[2], "codex");
    assert_eq!(
        app.tab_label_of_session(workers[2]),
        "Codex",
        "the agent in front names a worker's tab before the one its task started"
    );

    claude_worker(&app, tabs[0]);
    typed(&app, tabs[0], &prompt_in("~/proj"));
    typed(&app, tabs[0], &line_starts("make test"));
    assert_eq!(
        app.tab_label_of_session(tabs[0]),
        "make test",
        "CONTROL: a worker kept as a tab is named as its chip reads"
    );
    let line = "codex --model o3 'fix foo'";
    typed(&app, tabs[1], &prompt_in("~/proj"));
    typed(&app, tabs[1], &line_starts(line));
    in_front(&app, tabs[1], "codex");
    assert_eq!(
        app.tab_label_of_session(tabs[1]),
        line,
        "CONTROL: a person's tab whose line starts an agent is named as its chip reads"
    );
    host.shutdown_and_join();
}

/// A WORKER THAT NEVER STARTED KEEPS ITS AGENT'S NAME: its tab's title is its shell's, a folder
/// that names no worker, whichever launch step stopped it. NEGATIVE CONTROL: a launch refused for a
/// sign-in or an install, or stopped by a shutdown, read as a running agent and showed the folder.
/// CONTROL: `why=held` has started and shows its tab's own title.
#[test]
fn a_worker_that_never_started_keeps_its_agents_name() {
    let words = [
        "startup",
        "shell",
        crate::task_record::WHY_SIGNIN,
        crate::task_record::WHY_INSTALL,
        crate::task_record::WHY_CANCELLED,
        "held",
    ];
    let (app, _, workers) = app_with(1, words.len());
    for (&worker, word) in workers.iter().zip(words) {
        let term = app.pool.get(worker).expect("pooled").term.clone();
        crate::term_lock(&term).process(b"\x1b]2;/private/tmp/lab/proj\x07");
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let handle = store.by_local(worker).expect("a live session");
        let mut mark = TaskMark::new(TaskState::Stalled);
        mark.why = Some(word);
        mark_session_task(&handle.ctx, handle.local_id, &mark).expect("unfenced");
        handle
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_task_worker(aterm_phase::Program::Claude, None);
    }
    let rows = rows(&app);
    for (&worker, word) in workers.iter().zip(words) {
        let row = rows
            .iter()
            .find(|row| row.session == worker)
            .expect("a row");
        if word == "held" {
            assert_eq!(row.title, app.tab_label_of_session(worker), "{word}");
            assert_ne!(row.title, "Claude Code", "{word}");
        } else {
            assert_eq!(row.title, "Claude Code", "{word}");
        }
    }
}

/// Make `session`'s task a program worker's (`spawn program=`) in `mark`'s state, its program named
/// `program` on its record (`None`, as on a record an older build carried).
fn program_worker(app: &App, session: u64, mark: &TaskMark, program: Option<&str>) {
    let ctx = app.pool.get(session).expect("pooled").ctx.clone();
    mark_session_task(&ctx, session, mark).expect("unfenced");
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.set_task_worker(aterm_phase::Program::Generic, None);
    timeline.set_task_program(program);
}

/// A stall for `why`.
fn stalled(why: &'static str) -> TaskMark {
    TaskMark {
        why: Some(why),
        ..TaskMark::new(TaskState::Stalled)
    }
}

/// `bytes`, as `session`'s program or shell wrote them.
fn typed(app: &App, session: u64, bytes: &[u8]) {
    let term = app.pool.get(session).expect("pooled").term.clone();
    crate::term_lock(&term).process(bytes);
}

/// The line a program worker's launcher types for `program=/opt/lab/bench arg=--quick`.
const BENCH_LINE: &str = "'/opt/lab/bench' '--quick' < \"$ATERM_TASK_FILE\"";

/// A PROGRAM WORKER ITS RECORD DOES NOT NAME IS `the program`: a record an older build carried has
/// `agent=generic` and no `program=`, so its row, stalled before its line ran, read its tab's
/// folder, and its chip and every message placing it read its folder at a prompt and its launch
/// line while it ran. Each reads `the program` now, as its row did before a program worker's
/// program was named. CONTROL: a title its program gives the tab names it.
#[test]
fn a_program_worker_its_record_does_not_name_is_the_program() {
    let the_program = aterm_phase::Program::Generic.product();
    let (app, _, workers) = app_with(1, 2);
    for &worker in &workers {
        typed(&app, worker, &prompt_in("~/proj"));
    }
    program_worker(&app, workers[0], &stalled("startup"), None);
    let row = rows(&app)
        .into_iter()
        .find(|row| row.session == workers[0])
        .expect("its row");
    assert_eq!(
        row.title, the_program,
        "its row, stalled before its line ran"
    );
    assert_eq!(
        app.tab_label_of_session(workers[0]),
        the_program,
        "its chip, at its prompt"
    );

    program_worker(&app, workers[1], &TaskMark::new(TaskState::Working), None);
    typed(&app, workers[1], &line_starts(BENCH_LINE));
    let sid = app.session_id_text(workers[1]).expect("a sid");
    assert_eq!(
        app.session_place(&sid),
        format!("in Workers \u{25b8} {the_program}"),
        "while its shell titles its tab by its line"
    );
    typed(&app, workers[1], b"\x1b]0;bench: 3 of 10\x07");
    assert_eq!(
        app.tab_label_of_session(workers[1]),
        "bench: 3 of 10",
        "CONTROL: a title its program gives the tab names it"
    );
}

/// A PARKED WORKER WHOSE TAB'S TITLE SAYS NOTHING IS NAMED BY WHAT IT RUNS, NOT BY ITS FOLDER: a
/// program that empties its tab's title leaves its label nothing but the folder its shell
/// reported, so its chip and every message placing it read `Workers ▸ /private/tmp/lab/proj` while
/// it ran. They read its program now, a busy terminal's as the strip last read it. CONTROL: a
/// person's tab whose program empties its title is named by its folder.
#[test]
fn a_parked_workers_tab_whose_title_says_nothing_is_named_by_what_it_runs() {
    let (mut app, tabs, workers) = app_with(1, 1);
    for session in [tabs[0], workers[0]] {
        typed(
            &app,
            session,
            b"\x1b]7;file://localhost/private/tmp/lab/proj\x07",
        );
        typed(&app, session, &prompt_in("/private/tmp/lab/proj"));
        typed(&app, session, &line_starts(BENCH_LINE));
        typed(&app, session, b"\x1b]2;\x07");
    }
    program_worker(
        &app,
        workers[0],
        &TaskMark::new(TaskState::Working),
        Some("bench"),
    );
    assert_eq!(
        app.tab_label_of_session(workers[0]),
        "bench",
        "a program worker whose program emptied its tab's title"
    );
    app.tab_titles(WID);
    {
        let busy = app.pool.get(workers[0]).expect("pooled").term.clone();
        let _reading = crate::term_lock(&busy);
        assert_eq!(
            app.tab_label_of_session(workers[0]),
            "bench",
            "a busy terminal's, as the strip last read it"
        );
    }
    assert_eq!(
        app.tab_label_of_session(tabs[0]),
        "/private/tmp/lab/proj",
        "CONTROL: a person's tab whose program emptied its title is named by its folder"
    );
}

/// A PROGRAM WORKER'S ROW NAMES ITS PROGRAM ONCE IT HAS GONE, WHATEVER TITLES ITS TAB: a worker
/// stalled at its start in a shell that showed no integration marks keeps the title that shell
/// gave it, which aterm cannot know to be its shell's, so its chip reads that title while its row
/// reads its program. NEGATIVE CONTROL: a working program worker's row reads its chip's title.
#[test]
fn a_gone_program_workers_row_names_its_program_whatever_titles_its_tab() {
    let (app, _, workers) = app_with(1, 2);
    for &worker in &workers {
        typed(&app, worker, b"\x1b]2;/private/tmp/lab/proj\x07");
    }
    program_worker(&app, workers[0], &stalled("startup"), Some("bench"));
    program_worker(
        &app,
        workers[1],
        &TaskMark::new(TaskState::Working),
        Some("bench"),
    );
    for &worker in &workers {
        assert_eq!(
            app.tab_label_of_session(worker),
            "/private/tmp/lab/proj",
            "PRECONDITION: its chip reads its tab's title"
        );
    }
    let rows = rows(&app);
    let title = |session: u64| {
        rows.iter()
            .find(|row| row.session == session)
            .map(|row| row.title.clone())
            .expect("its row")
    };
    assert_eq!(
        title(workers[0]),
        "bench",
        "stalled before its program started"
    );
    assert_eq!(
        title(workers[1]),
        "/private/tmp/lab/proj",
        "NEGATIVE CONTROL: its program runs"
    );
}

/// The strip line's pushed tooltips are what the strip was given: one tip per shown tab, never a
/// parked worker's, so a reader that matches them to the chips (`count=` against
/// `pushed_tooltips=`, the lab's hover read) reads every tip while a worker is parked. Before,
/// every tab's tip was kept, and the line named more tips than chips.
#[test]
fn the_strip_line_pushes_one_tooltip_per_shown_tab_while_workers_are_parked() {
    let (mut app, _, _) = app_with(2, 1);
    app.sync_window(WID);
    let shown = set(&app).shown_len();
    assert!(set(&app).tabs().len() > shown, "a worker is parked");
    let pushed = &app.windows.get(&WID).expect("the window").tooltips_pushed;
    assert_eq!(pushed.len(), shown, "{pushed:?}");
    let line = app.stripless_tab_chrome().expect("the tab line");
    assert!(line.contains(&format!("count={shown} ")), "{line}");
}

/// A candidate that ends before its Commit adopted its predecessor's Finished rows and owns none
/// of what they left: the predecessor still lists them and answers from the same result files, and
/// nothing can write those again. CONTROL: past the Commit a quit drops the rows and the files.
#[test]
fn a_candidate_that_ends_before_its_commit_leaves_its_predecessors_results() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::tempdir().expect("tempdir");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        let sid = aterm_session::SessionId::new("s-c0de");
        let dir = crate::task_record::tasks_dir().expect("a state directory");
        std::fs::create_dir_all(&dir).expect("the tasks directory");
        let result = dir.join(format!("{}-2.result.md", sid.as_str()));
        std::fs::write(&result, b"what it left").expect("the result file");
        let (record, _) = TaskMark::new(TaskState::Done).apply(
            None,
            &Default::default(),
            std::time::Instant::now(),
            1_000,
        );
        let row = crate::retired_workers::Retired {
            sid: sid.clone(),
            local: None,
            window: None,
            title: "a worker".to_owned(),
            last_words: None,
            record,
            closed_unix_ms: 1_000,
        };
        let kept = |app: &App| {
            app.store
                .read()
                .unwrap()
                .retired_workers()
                .by_sid(&sid)
                .is_some()
        };
        app.store.write().unwrap().retired_workers_mut().keep(row);

        app.incoming_handoff_pending = true;
        app.forget_retired_workers_at_quit();
        assert!(kept(&app), "the row stays before the Commit");
        assert!(result.exists(), "and what it left");

        app.incoming_handoff_pending = false;
        app.forget_retired_workers_at_quit();
        assert!(!kept(&app), "a quit drops the row");
        assert!(!result.exists(), "and what it left");
    });
}
