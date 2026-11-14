// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn per_agent_off_questions_need_you_only_for_disabled_agents() {
    use aterm_agent::supervise::SupervisorConfig;
    use aterm_phase::Program;

    let mut app = App::headless_for_test();
    app.push_stub_tab(WindowId(0), crate::stub_session(app.next_session_id));
    for (id, program) in [(0, Program::Agy), (1, Program::Claude)] {
        let ctx = &app.pool.get(id).unwrap().ctx;
        let mut timeline = ctx.timeline.lock().unwrap();
        timeline.set_program(4242 + id as i32, Some(program.name().to_string()));
        timeline.publish_agent(
            "question",
            None,
            None,
            Some(program),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    }
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let mut observed = Vec::new();
    for off in [
        Some("agy.enabled"),
        Some("claude.enabled"),
        None,
        Some("enabled"),
    ] {
        let mut cfg = SupervisorConfig::default();
        if let Some(key) = off {
            cfg.set(key, "false").unwrap();
        }
        host.set_config(cfg);
        app.publish_needs_all();
        observed.push({
            let mut sessions: Vec<u64> =
                app.local_needs().iter().map(|need| need.session).collect();
            sessions.sort_unstable();
            sessions
        });
    }
    let mut cfg = SupervisorConfig::default();
    cfg.set("agy.enabled", "false").unwrap();
    host.set_config(cfg);
    let own = crate::harness_host::holder();
    let mut claimed = Vec::new();
    let mut claims_unchanged = Vec::new();
    for holder in [own.as_str(), "external-supervisor"] {
        for id in 0..2 {
            let ctx = &app.pool.get(id).unwrap().ctx;
            ctx.meta.lock().unwrap().supervisor = None;
            crate::session_timeline::claim_supervisor(
                ctx,
                holder,
                None,
                None,
                crate::metrics::now_us(),
            )
            .unwrap();
        }
        app.publish_needs_all();
        claimed.push({
            let mut sessions: Vec<u64> =
                app.local_needs().iter().map(|need| need.session).collect();
            sessions.sort_unstable();
            sessions
        });
        claims_unchanged.push((0..2).all(|id| {
            app.pool
                .get(id)
                .unwrap()
                .ctx
                .meta
                .lock()
                .unwrap()
                .live_supervisor(crate::metrics::now_us())
                == Some(holder)
        }));
    }
    host.shutdown_and_join();
    assert_eq!(
        observed,
        [vec![0], vec![1], vec![], vec![0, 1]],
        "only an admitted agent's question is assigned to the harness"
    );
    assert_eq!(
        claimed,
        [vec![0], vec![]],
        "a disabled own claim is winding down; an external supervisor still answers its questions"
    );
    assert!(
        claims_unchanged.into_iter().all(|unchanged| unchanged),
        "projection never rewrites the actual claims"
    );
}

fn file_uri(path: &std::path::Path) -> String {
    crate::native_document_host::path_to_file_uri(path).unwrap()
}

fn assert_terminal_fully_retired(app: &App, session: u64, view: crate::tab_model::ViewId) {
    assert!(app.pool.get(session).is_none(), "pool owner must be gone");
    assert_eq!(app.pool.views(session), None, "pool refcount must be gone");
    assert!(
        app.view_store.get(view).is_none(),
        "stable terminal view link must be gone"
    );
    assert!(
        app.store
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .by_local(session)
            .is_none(),
        "process session registry entry must be gone"
    );
    assert!(app.windows.values().all(|window| {
        window
            .tab_set
            .tabs()
            .iter()
            .all(|tab| !tab.root.contains(view))
            && window
                .layouts
                .iter()
                .all(|layout| !layout.contains(session))
            && !window.leaf_render_cache.contains_key(&view)
    }));
}

fn assert_native_sibling_survives(
    app: &App,
    instance: crate::tab_model::AppInstanceId,
    view: crate::tab_model::ViewId,
) {
    assert!(matches!(
        app.view_store.get(view),
        Some(crate::tab_model::View::Native(native)) if native.instance == instance
    ));
    assert!(app.native_runtime.app(instance).is_some());
    assert!(app.native_runtime.view_state(view).is_some());
}

fn enter_settings_draft(app: &mut App, wid: WindowId, view: crate::tab_model::ViewId) {
    app.dispatch_native_view_event(
        wid,
        view,
        crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new(format!(
                "settings/set/{}",
                crate::prefs::EDIT_FONT_FAMILY
            )),
            value: None,
        }),
    )
    .unwrap();
    app.dispatch_native_view_event(
        wid,
        view,
        crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::SelectAll),
    )
    .unwrap();
    app.dispatch_native_view_event(
        wid,
        view,
        crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
            "Topology Draft Mono".to_string(),
        )),
    )
    .unwrap();
}

fn assert_exact_settings_close_recovery(app: &App, wid: WindowId) {
    let lines = app.windows[&wid]
        .palette()
        .expect("blocked close opens recovery palette")
        .controls_lines();
    assert!(
        lines
            .first()
            .is_some_and(|line| line.contains("rows=2 shown=2")),
        "only app-supplied recovery is surfaced: {lines:?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("target=native"))
            .count(),
        2,
        "exactly two native recovery rows: {lines:?}"
    );
    for action in ["settings/drafts/review", "settings/drafts/discard-all"] {
        assert!(
            lines.iter().any(|line| {
                line.contains("target=native")
                    && line.contains(&format!("action={action}"))
                    && line.contains("enabled=true")
            }),
            "recovery exposes {action}: {lines:?}"
        );
    }
}

#[test]
fn cmd_w_mixed_leaf_preserves_settings_draft_and_surfaces_exact_recovery() {
    let mut failures = Vec::new();
    for (replay, separate_front) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, settings) = app.active_native_view(wid).unwrap();
        let (_, terminal) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let tab = app.windows[&wid].tab_set.active_id().unwrap();
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(settings);
        app.sync_window(wid);
        enter_settings_draft(&mut app, wid, settings);
        let before = app.windows[&wid].tab_set.get(tab).unwrap().root.clone();
        let drafts = match app.native_runtime.view_state(settings).unwrap() {
            crate::native_app::AppViewState::Settings(state) => state.carried_field_drafts(),
            _ => panic!("Settings view"),
        };

        if replay {
            app.switch_tab_in(wid, 0);
            assert!(app.active_native_view(wid).is_none());
        }
        let front = separate_front.then(|| {
            let session = app.next_session_id;
            let front = app.insert_logical_window(crate::stub_session(session), 24, 80);
            term_lock(&app.pool.get(session).unwrap().term).process(b"FRONT WINDOW UNCHANGED");
            (front, session)
        });
        let owner = front.map_or(wid, |(front, _)| front);
        for (&id, window) in &mut app.windows {
            window.focused = id == owner;
        }
        let front_tabs = front.map(|(front, _)| app.windows[&front].tab_set.tabs().to_vec());
        let _ = take_raised_windows();
        if replay {
            assert_eq!(
                app.replay_deferred_handoff_view_close(wid, tab, settings),
                None
            );
        } else {
            assert_eq!(app.close_active_tab_in(wid), None);
        }
        assert_eq!(
            app.windows[&wid].tab_set.get(tab).unwrap().root,
            before,
            "replay={replay}: a blocked close cannot detach either mixed leaf"
        );
        assert!(before.contains(terminal));
        assert_eq!(app.active_native_view(wid), Some((instance, settings)));
        assert_exact_settings_close_recovery(&app, wid);
        assert!(
            !app.native_runtime
                .presentation(instance, settings)
                .unwrap()
                .closable
        );
        if let Some((front, session)) = front {
            assert_eq!(app.windows[&front].tab_set.tabs(), front_tabs.unwrap());
            check_blocked_close_front_edit(&mut app, front, session, &mut failures);
        } else {
            assert_eq!(app.frontmost_window, Some(wid));
        }
        assert_eq!(app.windows[&wid].focused, !separate_front);
        assert!(app.windows[&owner].focused);
        assert!(take_raised_windows().is_empty());
        assert!(app.closed_recovery.tabs.is_empty());
        assert!(app.closed_recovery.views.is_empty());
        assert!(matches!(app.native_runtime.view_state(settings),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.carried_field_drafts() == drafts));
        assert_exact_settings_close_recovery(&app, wid);
        assert!(app.structural_invariants_ok());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn deferred_terminal_view_close_uses_stable_target_after_tab_selection() {
    check_deferred_view_close_target(false);
}

#[test]
fn deferred_native_view_close_uses_stable_target_after_tab_selection() {
    check_deferred_view_close_target(true);
}

fn check_deferred_view_close_target(native: bool) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    if native {
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    }
    let target = app.windows[&wid].tab_set.active().unwrap();
    let target_tab = target.id;
    let target_view = target.focus;
    let count_before = app.windows[&wid].tab_set.len();
    let parked_session = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(parked_session));
    let parked = app.windows[&wid].tab_set.active().unwrap().clone();
    let parked_term = app.pool.get(parked_session).unwrap().term.clone();
    term_lock(&parked_term).process(b"PARKED WINDOW UNCHANGED");
    let parked_state = || {
        let term = term_lock(&parked_term);
        (
            term.content_seq(),
            term.grid().display_offset(),
            term.grid().rows(),
            term.grid().cols(),
            term.text_selection().clone(),
            term.row_text(0),
        )
    };
    let parked_before = parked_state();
    let front_session = app.next_session_id;
    let front = app.insert_logical_window(crate::stub_session(front_session), 24, 80);
    term_lock(&app.pool.get(front_session).unwrap().term).process(b"FRONT WINDOW UNCHANGED");
    let front_tabs = app.windows[&front].tab_set.tabs().to_vec();
    app.windows.get_mut(&wid).unwrap().focused = false;
    app.windows.get_mut(&front).unwrap().focused = true;
    assert_eq!(app.frontmost_window, Some(front));
    assert_eq!(app.windows[&wid].tab_set.active_id(), Some(parked.id));
    assert_eq!(app.front_terminal(wid).unwrap().session, parked_session);
    let _ = take_raised_windows();

    assert_eq!(
        app.replay_deferred_handoff_view_close(wid, target_tab, target_view),
        None
    );

    assert!(
        app.view_store.get(target_view).is_none(),
        "the deferred stable target must close"
    );
    assert!(app.windows[&wid].tab_set.get(target_tab).is_none());
    assert_eq!(app.windows[&wid].tab_set.len(), count_before);
    assert_eq!(app.windows[&wid].tab_set.get(parked.id), Some(&parked));
    assert_eq!(app.windows[&wid].tab_set.active_id(), Some(parked.id));
    assert_eq!(app.windows[&wid].tabs.active, usize::from(native));
    assert_eq!(app.active_tree(wid).unwrap().focus(), parked_session);
    assert_eq!(app.front_terminal(wid).unwrap().session, parked_session);
    assert!(std::sync::Arc::ptr_eq(
        &app.pool.get(parked_session).unwrap().term,
        &parked_term
    ));
    assert_eq!(parked_state(), parked_before);
    assert_eq!(app.windows[&front].tab_set.tabs(), front_tabs);
    assert!(!app.windows[&wid].focused);
    assert!(app.windows[&front].focused);
    assert!(take_raised_windows().is_empty());
    let mut failures = Vec::new();
    check_blocked_close_front_edit(&mut app, front, front_session, &mut failures);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_replayed_pane_close_in_a_background_tab_keeps_its_pane_tree_on_its_focus() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let kept = 0;
    let closed = app.split_active_stub_tab(wid);
    let heir = app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
    assert!(app.focus_session_window(kept));
    let target = app.windows[&wid].tab_set.active().unwrap().clone();
    let closed_view = target
        .root
        .leaves()
        .into_iter()
        .find(|view| {
            app.view_store
                .get(*view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                == Some(closed)
        })
        .unwrap();
    let front = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(front));

    assert_eq!(
        app.replay_deferred_handoff_view_close(wid, target.id, closed_view),
        None
    );
    assert!(app.view_store.get(closed_view).is_none());
    assert_eq!(app.structural_invariant_violation(), None);
    assert_eq!(app.focused_session_id(wid), Some(front));
    app.switch_tab_in(wid, 0);
    assert_eq!(app.focused_session_id(wid), Some(kept));
    assert_eq!(
        app.active_tree(wid).map(crate::pane::PaneTree::focus),
        Some(kept),
        "the pane tree keeps the tab's focus, not the closed pane's heir"
    );
    app.close_active_tab();
    assert!(
        app.pool.get(kept).is_none() && app.pool.get(heir).is_some(),
        "Cmd-W closes the pane in focus"
    );
}

#[test]
fn closing_a_background_settings_tab_leaves_the_front_unless_its_close_asks() {
    for (drafted, by_settings_close) in [(false, false), (true, false), (false, true), (true, true)]
    {
        let at = format!("drafted={drafted} by_settings_close={by_settings_close}");
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let next = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(next));
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, settings) = app.active_native_view(wid).unwrap();
        let settings_tab = app.windows[&wid].tab_set.active_id().unwrap();
        if drafted {
            enter_settings_draft(&mut app, wid, settings);
        }
        app.switch_tab_in(wid, 0);

        let closed = if by_settings_close {
            app.close_settings_tabs()
        } else {
            let _ = app.close_tab_at(wid, 2);
            app.windows[&wid].tab_set.get(settings_tab).is_none()
        };
        if drafted {
            assert!(!closed, "{at}: the draft blocks the close");
            assert_eq!(
                app.windows[&wid].tab_set.active_id(),
                Some(settings_tab),
                "{at}: the close's recovery keeps Settings in front"
            );
            assert_exact_settings_close_recovery(&app, wid);
        } else {
            assert!(closed, "{at}");
            assert_eq!(
                app.windows[&wid].tab_set.active_index(),
                Some(0),
                "{at}: the front tab stays in front"
            );
            assert_eq!(app.focused_session_id(wid), Some(0), "{at}");
        }
        assert_eq!(app.structural_invariant_violation(), None, "{at}");
    }
}

#[test]
fn closing_split_settings_from_behind_leaves_its_terminals_failure_unseen() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    for ws in app.windows.values_mut() {
        ws.os_window_for_test = true;
        ws.focused = true;
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let settings_tab = app.windows[&wid].tab_set.active_id().unwrap();
    let (failed, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    app.switch_tab_in(wid, 0);
    let term = app
        .pool
        .get(failed)
        .expect("the split's terminal")
        .term
        .clone();
    crate::term_lock(&term).process(
        b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\make\r\n\x1b]133;C\x1b\\\
          \x1b]133;D;2\x1b\\\x1b]133;A\x1b\\\x1b]133;B\x1b\\",
    );
    let t0 = std::time::Instant::now();
    app.observe_session_statuses(t0);
    app.observe_session_statuses(t0 + std::time::Duration::from_secs(1));
    let unseen = |app: &App| {
        app.session_status
            .indicators(failed)
            .is_some_and(|bits| bits.status_attention)
    };
    assert!(unseen(&app), "PRECONDITION: a failure nobody has looked at");

    assert!(app.close_settings_tabs());
    assert_eq!(
        app.windows[&wid].tab_set.active_index(),
        Some(0),
        "the front tab stays in front"
    );
    assert_eq!(
        app.windows[&wid]
            .tab_set
            .get(settings_tab)
            .map(|tab| tab.root.len()),
        Some(1),
        "the split keeps its terminal"
    );
    assert_eq!(app.focused_session_id(wid), Some(0));
    assert!(unseen(&app), "the split's terminal never came to the front");
    assert_eq!(app.structural_invariant_violation(), None);

    app.switch_tab_in(wid, 1);
    assert!(!unseen(&app), "CONTROL: brought to the front, it is seen");
}

#[test]
fn closing_settings_in_a_split_keeps_the_focus_where_the_person_left_it() {
    for behind in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (c, _) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
        let (a, _) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
        // Settings beside [c | a] with a focused: Settings' own close would focus c.
        assert_ne!(c, a);
        assert_eq!(app.focused_session_id(wid), Some(a), "PRECONDITION");
        if behind {
            app.switch_tab_in(wid, 0);
        }

        assert!(app.close_settings_tabs(), "behind={behind}");
        if behind {
            assert_eq!(app.windows[&wid].tab_set.active_index(), Some(0));
            app.switch_tab_in(wid, 1);
        }
        assert_eq!(
            app.focused_session_id(wid),
            Some(a),
            "behind={behind}: the pane the person focused keeps the focus"
        );
        assert_eq!(
            app.structural_invariant_violation(),
            None,
            "behind={behind}"
        );
    }
}

fn check_blocked_close_front_edit(
    app: &mut App,
    front: WindowId,
    session: u64,
    failures: &mut Vec<String>,
) {
    let term = app.pool.get(session).unwrap().term.clone();
    if app.frontmost_window != Some(front) {
        failures.push("blocked background close changed the global front window".into());
    }
    if !app
        .active_handle
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|active| active.id == session && std::sync::Arc::ptr_eq(&active.term, &term))
    {
        failures.push("blocked background close replaced the global terminal input handle".into());
    }
    if app
        .frontmost_window
        .and_then(|wid| app.clipboard_session_in(wid))
        != Some(session)
    {
        failures.push("native menu clipboard ownership left the front terminal".into());
    }
    let before = {
        let term = term_lock(&term);
        assert!(!term.text_selection().has_selection());
        (
            term.content_seq(),
            term.grid().display_offset(),
            term.grid().rows(),
            term.grid().cols(),
        )
    };
    let expected = format!(
        "FRONT WINDOW UNCHANGED{}",
        "\n".repeat(usize::from(before.2) - 1)
    );
    app.select_all();
    if term_lock(&term).selection_to_string().as_deref() != Some(expected.as_str()) {
        failures.push("Edit Select All did not select the front terminal's screen".into());
    }
    assert!(crate::control::pbcopy("keep clipboard"));
    if !app.copy_selection() {
        failures.push("Edit Copy refused the front terminal's selected screen".into());
    }
    if crate::control::pbpaste().as_deref() != Some(expected.as_str()) {
        failures.push("Edit Copy did not copy the front terminal's exact screen".into());
    }
    let term = term_lock(&term);
    assert_eq!(
        (
            term.content_seq(),
            term.grid().display_offset(),
            term.grid().rows(),
            term.grid().cols()
        ),
        before
    );
    assert_eq!(term.row_text(0).as_deref(), Some("FRONT WINDOW UNCHANGED"));
}

#[test]
fn an_out_of_range_tab_action_refuses_instead_of_reporting_ok() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    let count = app.windows[&wid].tab_set.len();
    assert!(count >= 2, "two tabs so a real move has somewhere to go");

    app.apply_tab_cmd_in(wid, TabAction::Select(count))
        .expect_err("an index no tab holds must refuse");

    app.apply_tab_cmd_in(wid, TabAction::Close(Some(count + 3)))
        .expect_err("closing an index no tab holds must refuse");

    app.apply_tab_cmd_in(
        wid,
        TabAction::Move {
            from: 0,
            to: count + 1,
        },
    )
    .expect_err("moving to an index no tab holds must refuse");

    assert_eq!(
        app.apply_tab_cmd_in(wid, TabAction::Select(0)),
        Ok((0, count)),
        "an in-range select still reports the state"
    );
    assert_eq!(
        app.apply_tab_cmd_in(wid, TabAction::Move { from: 0, to: 1 }),
        Ok((1, count)),
        "an in-range move still reports the state"
    );
    let after = app
        .apply_tab_cmd_in(wid, TabAction::Close(Some(0)))
        .expect("an in-range close acts");
    assert_eq!(after.1, count - 1, "the tab count really moved");
}

#[test]
fn background_whole_tab_and_settings_control_close_focus_blocker_and_recovery() {
    let mut failures = Vec::new();
    for separate_front in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, settings) = app.active_native_view(wid).unwrap();
        enter_settings_draft(&mut app, wid, settings);
        app.switch_tab_in(wid, 0);
        assert!(app.active_native_view(wid).is_none());
        let drafts = match app.native_runtime.view_state(settings).unwrap() {
            crate::native_app::AppViewState::Settings(state) => state.carried_field_drafts(),
            _ => panic!("Settings view"),
        };
        let before = app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|tab| (tab.id, tab.root.clone()))
            .collect::<Vec<_>>();
        let front = separate_front.then(|| {
            let session = app.next_session_id;
            let front = app.insert_logical_window(crate::stub_session(session), 24, 80);
            term_lock(&app.pool.get(session).unwrap().term).process(b"FRONT WINDOW UNCHANGED");
            (front, session)
        });
        let owner = front.map_or(wid, |(front, _)| front);
        for (&id, window) in &mut app.windows {
            window.focused = id == owner;
        }
        let front_tabs = front.map(|(front, _)| app.windows[&front].tab_set.tabs().to_vec());
        let _ = take_raised_windows();

        app.apply_tab_cmd_in(wid, TabAction::Close(Some(1)))
            .expect_err("a settings draft declines the close");
        assert_eq!(app.active_native_view(wid), Some((instance, settings)));
        assert_exact_settings_close_recovery(&app, wid);
        assert_eq!(app.windows[&wid].tab_set.len(), 2);
        assert!(app.native_runtime.view_state(settings).is_some());
        assert_eq!(
            app.windows[&wid]
                .tab_set
                .tabs()
                .iter()
                .map(|tab| (tab.id, tab.root.clone()))
                .collect::<Vec<_>>(),
            before
        );
        if let Some((front, session)) = front {
            assert_eq!(
                app.windows[&front].tab_set.tabs(),
                front_tabs.as_ref().unwrap()
            );
            check_blocked_close_front_edit(&mut app, front, session, &mut failures);
        } else {
            assert_eq!(app.frontmost_window, Some(wid));
        }
        assert_eq!(app.windows[&wid].focused, !separate_front);
        assert!(app.windows[&owner].focused);
        assert!(take_raised_windows().is_empty());
        app.palette_activate_in(wid);
        assert!(app.windows[&wid].palette().is_none());
        assert!(matches!(
            app.native_runtime.view_state(settings),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::Modified
        ));

        // Give the independent Settings-close gesture the same front-window precondition.
        if let Some((front, session)) = front {
            term_lock(&app.pool.get(session).unwrap().term)
                .text_selection_mut()
                .clear();
            app.frontmost_window = Some(front);
            app.sync_active_session();
        }
        app.switch_tab_in(wid, 0);
        assert!(!app.close_settings_tabs());
        assert_eq!(app.active_native_view(wid), Some((instance, settings)));
        assert_exact_settings_close_recovery(&app, wid);
        assert_eq!(app.windows[&wid].tab_set.len(), 2);
        if let Some((front, session)) = front {
            assert_eq!(
                app.windows[&front].tab_set.tabs(),
                front_tabs.as_ref().unwrap()
            );
            check_blocked_close_front_edit(&mut app, front, session, &mut failures);
        } else {
            assert_eq!(app.frontmost_window, Some(wid));
        }
        assert_eq!(app.windows[&wid].focused, !separate_front);
        assert!(app.windows[&owner].focused);
        assert!(take_raised_windows().is_empty());
        assert!(matches!(app.native_runtime.view_state(settings),
        Some(crate::native_app::AppViewState::Settings(state))
            if state.carried_field_drafts() == drafts));
        assert_exact_settings_close_recovery(&app, wid);
        assert_eq!(
            app.windows[&wid]
                .tab_set
                .tabs()
                .iter()
                .map(|tab| (tab.id, tab.root.clone()))
                .collect::<Vec<_>>(),
            before
        );
        assert!(app.structural_invariants_ok());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_quiet_shutdown_probe_reports_the_same_block_without_taking_the_screen() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (instance, settings) = app.active_native_view(wid).unwrap();
    enter_settings_draft(&mut app, wid, settings);
    app.switch_tab_in(wid, 0);
    let working_tab = app.windows[&wid].tab_set.active_id().unwrap();
    assert!(app.active_native_view(wid).is_none());

    assert!(
        !app.prepare_all_native_shutdown(
            crate::native_app::CloseScope::Relaunch,
            ClosePreflightVisibility::Quiet
        )
        .unwrap(),
        "a quiet probe still refuses the shutdown"
    );
    assert!(
        app.windows[&wid].overlay.is_none(),
        "a quiet probe opened an overlay over the user's work"
    );
    assert_eq!(
        app.windows[&wid].tab_set.active_id(),
        Some(working_tab),
        "a quiet probe switched the active tab"
    );
    assert!(
        app.active_native_view(wid).is_none(),
        "a quiet probe moved focus onto the blocking view"
    );
    assert!(
        !app.native_runtime
            .presentation(instance, settings)
            .unwrap()
            .closable,
        "the draft is still what refused"
    );

    assert!(
        !app.prepare_all_native_shutdown(
            crate::native_app::CloseScope::Relaunch,
            ClosePreflightVisibility::Interactive
        )
        .unwrap()
    );
    assert_eq!(app.active_native_view(wid), Some((instance, settings)));
    assert_exact_settings_close_recovery(&app, wid);

    let tabs = app.windows[&wid].tab_set.len();
    assert!(
        !app.prepare_window_native_shutdown(wid, crate::native_app::CloseScope::Window)
            .unwrap()
    );
    assert!(!app.prepare_quit_native_shutdown().unwrap());
    assert_eq!(app.windows[&wid].tab_set.len(), tabs);
    assert!(app.native_runtime.view_state(settings).is_some());
    assert_exact_settings_close_recovery(&app, wid);

    for _ in 0..2 {
        app.dispatch_native_view_event(
            wid,
            settings,
            crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/drafts/discard-all"),
                value: None,
            }),
        )
        .unwrap();
    }
    assert!(
        app.prepare_window_native_shutdown(wid, crate::native_app::CloseScope::Window)
            .unwrap()
    );
    assert!(
        app.native_runtime
            .presentation(instance, settings)
            .unwrap()
            .closable
    );
    assert_eq!(app.windows[&wid].tab_set.len(), tabs);
    assert!(app.structural_invariants_ok());
}

#[test]
fn focused_terminal_eof_collapses_mixed_tree_without_orphans() {
    use aterm_models_gui::pane_tree_model;
    use aterm_spec::interp::{State, admits};

    fn project(tab: &crate::tab_model::Tab) -> State {
        let leaves = tab.root.leaves();
        let focused = leaves
            .iter()
            .position(|view| *view == tab.focus)
            .expect("focused identity is a live leaf");
        State::from([
            ("leaf_count", leaves.len() as i64),
            ("focused", focused as i64),
        ])
    }

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (instance, native_view) = app.active_native_view(wid).unwrap();
    let (session, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let tab_id = app.windows[&wid].tab_set.active_id().unwrap();
    let before = project(app.windows[&wid].tab_set.get(tab_id).unwrap());
    assert_eq!(app.focused_session_id(wid), Some(session));
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    assert!(
        app.windows[&wid]
            .leaf_render_cache
            .contains_key(&terminal_view)
    );

    assert!(
        app.exit_session_logical(session).is_empty(),
        "the native sibling keeps the window alive"
    );

    assert_terminal_fully_retired(&app, session, terminal_view);
    assert_native_sibling_survives(&app, instance, native_view);
    let survivor = app.windows[&wid].tab_set.get(tab_id).unwrap();
    assert_eq!(survivor.root.leaves(), vec![native_view]);
    assert_eq!(survivor.focus, native_view);
    assert_eq!(app.active_native_view(wid), Some((instance, native_view)));

    let model = pane_tree_model();
    let after = project(survivor);
    assert!(model.successors("Close", &before).contains(&after));
    let mut dangling = after.clone();
    dangling.insert("focused", dangling["leaf_count"]);
    assert_eq!(admits(&model, &before, &dangling), None);
    assert!(!model.check_invariant("FocusInRange", &dangling));
    assert!(app.structural_invariants_ok());
}

#[test]
fn background_mixed_terminal_eof_repairs_exact_tab_and_keeps_front_tab() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (instance, native_view) = app.active_native_view(wid).unwrap();
    let (session, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    let background_tab = app.windows[&wid].tab_set.active_id().unwrap();

    let foreground_session = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(foreground_session));
    let foreground_tab = app.windows[&wid].tab_set.active_id().unwrap();
    assert_ne!(foreground_tab, background_tab);
    assert!(app.exit_session_logical(session).is_empty());

    assert_eq!(app.windows[&wid].tab_set.active_id(), Some(foreground_tab));
    let repaired = app.windows[&wid].tab_set.get(background_tab).unwrap();
    assert_eq!(repaired.root.leaves(), vec![native_view]);
    assert_eq!(repaired.focus, native_view);
    assert_terminal_fully_retired(&app, session, terminal_view);
    assert_native_sibling_survives(&app, instance, native_view);
    assert!(app.structural_invariants_ok());
}

#[test]
fn shared_session_migrated_beside_source_exit_retires_every_occurrence() {
    let mut app = App::headless_for_test();
    let source = WindowId(0);
    let original_view = app.windows[&source].tab_set.active().unwrap().focus;

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (settings_instance, settings_view) = app.active_native_view(source).unwrap();
    app.switch_tab_in(source, 0);
    let shared_window = app
        .open_active_session_in_new_window_logical()
        .expect("share the active session into a second window");
    let shared_view = app.windows[&shared_window].tab_set.active().unwrap().focus;
    assert_ne!(shared_view, original_view);
    assert_eq!(app.pool.views(0), Some(2));

    let _ = app.migrate_active_tab_to_next_window();
    assert_eq!(app.windows.len(), 1, "the emptied share window closes");
    assert!(app.windows.contains_key(&source));
    assert_eq!(app.frontmost_window, Some(source));
    assert_eq!(
        app.windows[&source]
            .tab_set
            .tabs()
            .iter()
            .flat_map(|tab| tab.root.leaves())
            .filter(|view| {
                app.view_store
                    .get(*view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    == Some(0)
            })
            .count(),
        2,
        "both shared-session occurrences now live in one window"
    );

    {
        let cache = &mut app.windows.get_mut(&source).unwrap().leaf_render_cache;
        cache.insert(original_view, crate::LeafRenderCache::default());
        cache.insert(shared_view, crate::LeafRenderCache::default());
        cache.insert(settings_view, crate::LeafRenderCache::default());
    }

    assert!(
        app.exit_session_logical(0).is_empty(),
        "Settings keeps the sole window alive"
    );

    assert_terminal_fully_retired(&app, 0, original_view);
    assert_terminal_fully_retired(&app, 0, shared_view);
    assert_native_sibling_survives(&app, settings_instance, settings_view);
    assert_eq!(app.windows[&source].tab_set.len(), 1);
    assert_eq!(
        app.active_native_view(source),
        Some((settings_instance, settings_view)),
        "focus repairs onto the surviving native tab"
    );
    assert!(
        app.windows[&source]
            .leaf_render_cache
            .contains_key(&settings_view),
        "retirement does not evict a live sibling's retained raster"
    );
    assert_eq!(app.windows_displaying(0).count(), 0);
    assert!(app.structural_invariants_ok());
}

#[test]
fn native_split_retires_only_its_terminal_projection_and_close_restores_it() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.switch_tab_in(wid, 1);
    let tab_id = app.windows[&wid].tab_set.active_id().unwrap();
    let terminal_view = app.windows[&wid].tab_set.active().unwrap().focus;
    let layouts = app.windows[&wid].layouts.clone();
    let next_session = app.next_session_id;
    let instance = app
        .native_runtime
        .insert_instance(crate::native_app::NativeApp::Allocation(Box::default()))
        .unwrap();

    let native_view = app
        .split_active_with_native(
            wid,
            crate::tab_model::SplitAxis::Horizontal,
            instance,
            crate::native_app::AppViewState::Allocation(Box::default()),
        )
        .unwrap();

    let window = &app.windows[&wid];
    assert_eq!(window.tab_set.active_id(), Some(tab_id));
    assert_eq!(window.tab_set.len(), 3);
    assert_eq!(window.tabs.count, 2);
    assert_eq!(window.layouts, vec![layouts[0].clone(), layouts[2].clone()]);
    assert_eq!(
        window.tab_set.get(tab_id).unwrap().root.leaves(),
        vec![terminal_view, native_view]
    );
    assert_eq!(app.next_session_id, next_session);
    assert!(app.structural_invariants_ok());

    app.close_focused_mixed_leaf(wid).unwrap();

    let window = &app.windows[&wid];
    assert_eq!(window.tab_set.active_id(), Some(tab_id));
    assert_eq!(window.tab_set.active().unwrap().focus, terminal_view);
    assert_eq!(window.layouts, layouts);
    assert_eq!(window.tabs.count, 3);
    assert_eq!(app.next_session_id, next_session);
    assert!(app.structural_invariants_ok());
}

#[test]
fn heterogeneous_split_runtime_paints_sizes_routes_and_drags_every_leaf() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(app.front_terminal(wid).is_none());
    assert_eq!(app.focused_session_id(wid), None);
    let (instance, first_native) = app.active_native_view(wid).expect("Settings focused");
    let second_state = crate::native_app::AppViewState::Settings(Box::new(
        crate::native_settings::SettingsViewState::new(&app.config),
    ));
    let second_native = app
        .split_active_with_native(
            wid,
            crate::tab_model::SplitAxis::Horizontal,
            instance,
            second_state,
        )
        .expect("second native view");
    let (terminal_session, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    assert_eq!(app.focused_session_id(wid), Some(terminal_session));
    assert_eq!(
        app.front_terminal(wid).map(|front| front.session),
        Some(terminal_session)
    );

    let plan = app.active_visible_leaf_plan(wid).expect("visible plan");
    assert_eq!(
        plan.leaves
            .iter()
            .map(|leaf| leaf.view)
            .collect::<std::collections::BTreeSet<_>>(),
        [first_native, second_native, terminal_view]
            .into_iter()
            .collect()
    );
    let terminal_leaf = plan.leaf(terminal_view).expect("terminal placement");
    let terminal = app.pool.get(terminal_session).expect("terminal pool owner");
    let terminal = crate::term_lock(&terminal.term);
    assert_eq!(
        (terminal.rows(), terminal.cols()),
        (
            (terminal_leaf.rect.size.height.round() as u16).max(1),
            (terminal_leaf.rect.size.width.round() as u16).max(1),
        ),
        "terminal SIGWINCH geometry comes from the shared visible plan"
    );
    drop(terminal);

    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let window = &app.windows[&wid];

    let second_raster = window.leaf_render_cache[&second_native]
        .native
        .as_ref()
        .expect("second retained raster");
    let second_stamp = second_raster.stamp;
    let second_pixels = second_raster.rgba.as_ptr() as usize;
    let damage = crate::native_app::DamageRegion::Rect {
        x: 7,
        y: 11,
        width: 19,
        height: 23,
    };
    app.invalidate_native_view_cache(wid, first_native, damage);
    assert_eq!(
        app.windows[&wid].leaf_render_cache[&first_native].native_damage,
        Some(damage)
    );
    assert_eq!(
        app.windows[&wid].leaf_render_cache[&second_native].native_damage, None,
        "view-local damage never dirties a native sibling"
    );
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let second_raster = app.windows[&wid].leaf_render_cache[&second_native]
        .native
        .as_ref()
        .expect("second raster remains retained");
    assert_eq!(second_raster.stamp, second_stamp);
    assert_eq!(
        second_raster.rgba.as_ptr() as usize,
        second_pixels,
        "undamaged native sibling keeps its retained pixel allocation"
    );
    assert_eq!(
        app.windows[&wid].leaf_render_cache[&first_native].native_damage, None,
        "render consumes the exact leaf's pending damage"
    );
    assert!(matches!(
        app.windows[&wid].leaf_render_cache[&first_native]
            .native
            .as_ref()
            .expect("first raster remains retained")
            .last_work,
        crate::NativeRasterWork::Region { .. }
    ));

    assert_eq!(
        app.native_runtime.view_lifecycle(first_native),
        Some(crate::front_content::ViewLifecycle::Mounted)
    );
    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap()
        .set_focus(first_native);
    app.sync_window(wid);
    assert!(app.front_terminal(wid).is_none());
    assert_eq!(app.focused_session_id(wid), None);
    app.toggle_pane_zoom();
    assert_eq!(
        app.native_runtime.view_lifecycle(first_native),
        Some(crate::front_content::ViewLifecycle::Mounted)
    );
    assert_eq!(
        app.native_runtime.view_lifecycle(second_native),
        Some(crate::front_content::ViewLifecycle::Suspended),
        "zoom suspends hidden native siblings without destroying view state"
    );
    app.toggle_pane_zoom();
    assert_eq!(
        app.native_runtime.view_lifecycle(second_native),
        Some(crate::front_content::ViewLifecycle::Mounted)
    );
    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap()
        .set_focus(terminal_view);
    app.sync_window(wid);

    let first = plan.leaf(first_native).expect("first native");
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.last_mouse_window_cell = (
            (first.rect.origin.y + first.rect.size.height * 0.5) as u16,
            (first.rect.origin.x + first.rect.size.width * 0.5) as u16,
        );
    }
    assert!(app.focus_pane_under_pointer(wid));
    assert_eq!(app.active_native_view(wid), Some((instance, first_native)));
    assert!(app.front_terminal(wid).is_none());

    let before = app.active_visible_leaf_plan(wid).unwrap();
    let divider = before.dividers.first().expect("root divider").clone();
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.last_mouse_window_cell = (
            (divider.rect.origin.y + divider.rect.size.height * 0.5) as u16,
            (divider.rect.origin.x + divider.rect.size.width * 0.5) as u16,
        );
    }
    assert!(app.begin_divider_drag(wid));
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.last_mouse_window_cell.1 = window.cols / 3;
    }
    app.drag_divider(wid);
    assert!(app.finish_divider_drag(wid));
    let after = app.active_visible_leaf_plan(wid).unwrap();
    assert_ne!(
        before.leaf(first_native).unwrap().rect,
        after.leaf(first_native).unwrap().rect,
        "canonical divider drag relayouts native and terminal siblings together"
    );

    assert!(app.structural_invariants_ok());
}

#[test]
fn closing_native_from_mixed_promotes_terminal_projection_and_reopen_demotes_it() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, settings) = app.active_native_view(wid).unwrap();
    let (_, terminal) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap()
        .set_focus(settings);
    app.sync_window(wid);
    assert_eq!(
        app.windows[&wid].layouts.len(),
        1,
        "only bootstrap projected"
    );

    app.close_active_tab();
    assert_eq!(app.windows[&wid].layouts.len(), 2);
    assert!(matches!(
        app.view_store
            .get(app.windows[&wid].tab_set.active().unwrap().focus),
        Some(crate::tab_model::View::Terminal(_))
    ));
    assert!(
        app.windows[&wid]
            .tab_set
            .active()
            .unwrap()
            .root
            .contains(terminal)
    );
    assert!(app.structural_invariants_ok());

    app.reopen_last_closed_view().unwrap();
    assert_eq!(app.windows[&wid].layouts.len(), 1);
    let tab = app.windows[&wid].tab_set.active().unwrap();
    assert_eq!(tab.root.len(), 2);
    assert!(matches!(
        app.view_store.get(tab.focus),
        Some(crate::tab_model::View::Native(_))
    ));
    assert!(app.structural_invariants_ok());
}

#[test]
fn reopening_a_closed_terminal_pane_activates_its_original_tab_and_input_target() {
    let terminal_state = |app: &App, session| {
        let terminal = crate::term_lock(&app.pool.get(session).unwrap().term);
        (
            terminal.grid().rows(),
            terminal.grid().cols(),
            (0..usize::from(terminal.grid().rows()))
                .map(|row| terminal.row_text(row))
                .collect::<Vec<_>>(),
            terminal.text_selection().clone(),
            terminal.grid().display_offset(),
        )
    };
    let mut failures = Vec::new();
    for settings_front in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let closed_session = app.split_active_stub_tab(wid);
        let original = app.windows[&wid].tab_set.active().unwrap();
        let original_tab = original.id;
        let original_root = original.root.clone();
        let old_view = original.focus;
        let sibling_view = original_root.leaves()[0];
        let sibling_term = app.pool.get(0).unwrap().term.clone();
        crate::term_lock(&sibling_term).process(b"SIBLING SURVIVES");
        let sibling_before = terminal_state(&app, 0);
        assert_eq!(original_root.leaves()[1], old_view);
        assert_eq!(app.focused_session_id(wid), Some(closed_session));

        assert_eq!(app.close_active_tab(), None, "only the pane closes");
        assert_eq!(app.closed_recovery.views.len(), 1);
        assert!(app.closed_recovery.tabs.is_empty());
        assert!(app.pool.get(closed_session).is_none());
        assert!(app.view_store.get(old_view).is_none());
        assert_eq!(app.windows[&wid].tab_set.active_id(), Some(original_tab));
        assert_eq!(app.focused_session_id(wid), Some(0));

        let unrelated_session = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(unrelated_session));
        let unrelated_tab = app.windows[&wid].tab_set.active().unwrap().clone();
        let unrelated_term = app.pool.get(unrelated_session).unwrap().term.clone();
        crate::term_lock(&unrelated_term).process(b"UNRELATED TAB STAYS");
        let unrelated_before = terminal_state(&app, unrelated_session);
        let unrelated_layout = app.windows[&wid].layouts[1].clone();
        if settings_front {
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
            assert!(app.active_native_view(wid).is_some());
        }
        let tab_order = app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>();
        assert_ne!(app.windows[&wid].tab_set.active_id(), Some(original_tab));
        assert_eq!(app.windows[&wid].tabs.active, 1);
        let view_count = app.view_store.len();

        app.reopen_last_closed_view().unwrap();

        let ws = &app.windows[&wid];
        let restored = ws.tab_set.get(original_tab).unwrap();
        let reopened_view = restored.focus;
        assert_ne!(
            reopened_view, old_view,
            "the closed view identity is retired"
        );
        assert_ne!(reopened_view, sibling_view, "the reopened pane owns focus");
        let reopened_session = app
            .view_store
            .get(reopened_view)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)
            .expect("the reopened pane is a terminal");
        assert_ne!(
            reopened_session, closed_session,
            "the closed shell is not revived"
        );
        let expected_root = original_root.map(&mut |view| {
            if *view == old_view {
                reopened_view
            } else {
                *view
            }
        });
        assert_eq!(
            restored.root, expected_root,
            "split placement and sibling order return"
        );
        assert!(!restored.zoomed);
        if ws.tab_set.active_id() != Some(original_tab) {
            failures.push(format!(
                "settings_front={settings_front}: original tab stayed inactive"
            ));
        }
        if ws.tabs.active != 0 {
            failures.push(format!(
                "settings_front={settings_front}: terminal projection stayed {}",
                ws.tabs.active
            ));
        }
        if app.focused_session_id(wid) != Some(reopened_session)
            || app.front_terminal(wid).map(|terminal| terminal.session) != Some(reopened_session)
            || app
                .active_handle
                .lock()
                .unwrap()
                .as_ref()
                .map(|handle| handle.id)
                != Some(reopened_session)
            || app.active_tree(wid).map(pane::PaneTree::focus) != Some(reopened_session)
        {
            failures.push(format!("settings_front={settings_front}: reopened session {reopened_session} is not the active input target"));
        }
        assert_eq!(ws.layouts[0].sessions(), vec![0, reopened_session]);
        assert_eq!(ws.layouts[0].focus(), reopened_session);
        assert_eq!(
            ws.layouts[1], unrelated_layout,
            "the unrelated layout stays intact"
        );
        assert_eq!(ws.tab_set.get(unrelated_tab.id), Some(&unrelated_tab));
        assert_eq!(
            ws.tab_set
                .tabs()
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            tab_order,
            "reopen reuses the original tab without reordering others"
        );
        assert_eq!(app.view_store.len(), view_count + 1);
        assert!(std::sync::Arc::ptr_eq(
            &sibling_term,
            &app.pool.get(0).unwrap().term
        ));
        assert!(std::sync::Arc::ptr_eq(
            &unrelated_term,
            &app.pool.get(unrelated_session).unwrap().term
        ));
        assert_eq!(
            terminal_state(&app, 0),
            sibling_before,
            "the retained pane keeps its data"
        );
        assert_eq!(terminal_state(&app, unrelated_session), unrelated_before);
        assert!(app.closed_recovery.views.is_empty());
        assert!(app.closed_recovery.tabs.is_empty());
        assert!(app.structural_invariants_ok());

        let after_root = restored.root.clone();
        let after_active = ws.tab_set.active_id();
        let after_next_session = app.next_session_id;
        assert!(
            app.reopen_last_closed_view().is_err(),
            "the record is consumed once"
        );
        assert_eq!(
            app.windows[&wid].tab_set.get(original_tab).unwrap().root,
            after_root
        );
        assert_eq!(app.windows[&wid].tab_set.active_id(), after_active);
        assert_eq!(app.next_session_id, after_next_session);
        assert_eq!(app.view_store.len(), view_count + 1);
        assert!(app.structural_invariants_ok());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn reopening_a_closed_pane_in_another_window_preserves_the_front_input_owner() {
    let mut app = App::headless_for_test();
    let original_window = WindowId(0);
    let closed_session = app.split_active_stub_tab(original_window);
    let original = app.windows[&original_window].tab_set.active().unwrap();
    let original_tab = original.id;
    let original_root = original.root.clone();
    let old_view = original.focus;
    assert_eq!(app.close_active_tab(), None);
    assert_eq!(app.closed_recovery.views.len(), 1);
    let parked_session = app.next_session_id;
    app.push_stub_tab(original_window, crate::stub_session(parked_session));
    let parked_tab = app.windows[&original_window]
        .tab_set
        .active()
        .unwrap()
        .clone();

    let front_session = app.next_session_id;
    let front_window = app.insert_logical_window(crate::stub_session(front_session), 24, 80);
    let front_term = app.pool.get(front_session).unwrap().term.clone();
    crate::term_lock(&front_term).process(b"FRONT WINDOW UNCHANGED");
    let front_text = |app: &App| {
        let terminal = crate::term_lock(&app.pool.get(front_session).unwrap().term);
        (
            terminal.grid().rows(),
            terminal.grid().cols(),
            (0..usize::from(terminal.grid().rows()))
                .map(|row| terminal.row_text(row))
                .collect::<Vec<_>>(),
            terminal.text_selection().clone(),
            terminal.grid().display_offset(),
        )
    };
    let before_text = front_text(&app);
    let front = &app.windows[&front_window];
    let before_tabs = front.tab_set.tabs().to_vec();
    let before_active = front.tab_set.active_id();
    let before_layouts = front.layouts.clone();
    let before_projection = front.tabs;
    let before_content = front.front_content;
    let before_focus = front.window_focus;
    assert_eq!(app.frontmost_window, Some(front_window));
    assert_eq!(
        app.active_handle.lock().unwrap().as_ref().unwrap().id,
        front_session
    );
    assert_eq!(app.clipboard_session_in(front_window), Some(front_session));
    let view_count = app.view_store.len();

    app.reopen_last_closed_view().unwrap();

    let restored_window = &app.windows[&original_window];
    let restored = restored_window.tab_set.active().unwrap();
    assert_eq!(
        restored.id, original_tab,
        "the original window selects its restored tab"
    );
    assert_eq!(restored_window.tabs.active, 0);
    let reopened_view = restored.focus;
    assert_ne!(reopened_view, old_view);
    let reopened_session = app
        .view_store
        .get(reopened_view)
        .copied()
        .and_then(crate::tab_model::View::terminal_session)
        .unwrap();
    assert_ne!(reopened_session, closed_session);
    assert_eq!(
        restored.root,
        original_root.map(&mut |view| if *view == old_view {
            reopened_view
        } else {
            *view
        }),
        "the closed pane returns to its existing split position"
    );
    assert_eq!(
        app.focused_session_id(original_window),
        Some(reopened_session)
    );
    assert_eq!(
        app.active_tree(original_window).map(pane::PaneTree::focus),
        Some(reopened_session)
    );
    assert_eq!(
        restored_window.tab_set.get(parked_tab.id),
        Some(&parked_tab)
    );

    let mut failures = Vec::new();
    if app.frontmost_window != Some(front_window) {
        failures.push("reopening a background pane changed the front window");
    }
    let active = app.active_handle.lock().unwrap();
    if !active.as_ref().is_some_and(|handle| {
        handle.id == front_session && std::sync::Arc::ptr_eq(&handle.term, &front_term)
    }) {
        failures.push("reopening a background pane replaced the global input handle");
    }
    drop(active);
    if app
        .frontmost_window
        .and_then(|wid| app.clipboard_session_in(wid))
        != Some(front_session)
    {
        failures.push("the native menu Paste resolver no longer belongs to the front terminal");
    }
    let front = &app.windows[&front_window];
    assert_eq!(front.tab_set.tabs(), before_tabs);
    assert_eq!(front.tab_set.active_id(), before_active);
    assert_eq!(front.layouts, before_layouts);
    assert_eq!(front.tabs, before_projection);
    assert_eq!(front.front_content, before_content);
    assert_eq!(front.window_focus, before_focus);
    assert_eq!(app.focused_session_id(front_window), Some(front_session));
    assert!(std::sync::Arc::ptr_eq(
        &front_term,
        &app.front_terminal(front_window).unwrap().term
    ));
    assert_eq!(front_text(&app), before_text);
    assert_eq!(app.view_store.len(), view_count + 1);
    assert!(app.closed_recovery.views.is_empty());
    assert!(app.closed_recovery.tabs.is_empty());
    assert!(app.structural_invariants_ok());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn reopened_background_recovery_preserves_the_global_input_owner() {
    let terminal_state = |app: &App, session| {
        let terminal = crate::term_lock(&app.pool.get(session).unwrap().term);
        (
            terminal.grid().rows(),
            terminal.grid().cols(),
            (0..usize::from(terminal.grid().rows()))
                .map(|row| terminal.row_text(row))
                .collect::<Vec<_>>(),
            terminal.text_selection().clone(),
            terminal.grid().display_offset(),
        )
    };
    let mut failures = Vec::new();
    for (path, pane_fallback, native, background) in [
        ("terminal tab", false, false, false),
        ("terminal tab", false, false, true),
        ("Settings tab", false, true, false),
        ("Settings tab", false, true, true),
        ("pane fallback", true, false, false),
        ("pane fallback", true, false, true),
    ] {
        let mut app = App::headless_for_test();
        let target_window = WindowId(0);
        let (closed_views, closed_shape, survivor_session) = if pane_fallback {
            let survivor = app.split_active_stub_tab(target_window);
            let closed = app.split_active_stub_tab(target_window);
            let closed_view = app.windows[&target_window].tab_set.active().unwrap().focus;
            assert_eq!(app.close_active_tab(), None);
            let now_ms = app.lat_epoch.elapsed().as_millis() as u64;
            let candidate = app.closed_recovery.views.candidate(now_ms).unwrap();
            assert_eq!(
                candidate.value.placement.parent_path,
                vec![crate::restore::RestoreBranch::Second]
            );
            // Reader EOF removes the older outer sibling without adding a human-close
            // recovery entry. The saved Second path no longer exists in the live host.
            assert!(!app.close_session(target_window, 0));
            assert_eq!(app.closed_recovery.views.len(), 1);
            assert_eq!(
                app.windows[&target_window]
                    .tab_set
                    .active()
                    .unwrap()
                    .root
                    .len(),
                1
            );
            assert_eq!(app.focused_session_id(target_window), Some(survivor));
            assert!(app.pool.get(closed).is_none());
            (
                vec![closed_view],
                crate::tab_model::SplitTree::leaf(false),
                survivor,
            )
        } else {
            if native {
                assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
            } else {
                app.push_stub_tab(target_window, crate::stub_session(app.next_session_id));
                app.split_active_stub_tab(target_window);
            }
            let tab = app.windows[&target_window].tab_set.active().unwrap();
            let views = tab.root.leaves();
            let shape = tab.root.map(&mut |view| {
                matches!(
                    app.view_store.get(*view),
                    Some(crate::tab_model::View::Native(_))
                )
            });
            assert!(!app.close_tab_at(target_window, 1));
            assert_eq!(app.closed_recovery.tabs.len(), 1);
            assert!(app.closed_recovery.views.is_empty());
            (views, shape, 0)
        };
        assert!(
            closed_views
                .iter()
                .all(|view| !app.view_store.contains(*view))
        );
        let surviving_tabs = app.windows[&target_window].tab_set.tabs().to_vec();
        let surviving_layouts = app.windows[&target_window].layouts.clone();
        let survivor_term = app.pool.get(survivor_session).unwrap().term.clone();
        crate::term_lock(&survivor_term).process(b"SURVIVOR KEPT");
        let survivor_before = terminal_state(&app, survivor_session);

        let front_window = if background {
            app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80)
        } else {
            target_window
        };
        let front_session = app.focused_session_id(front_window).unwrap();
        let front_term = app.pool.get(front_session).unwrap().term.clone();
        if background {
            crate::term_lock(&front_term).process(b"FRONT OWNER KEPT");
        }
        let before_front_text = terminal_state(&app, front_session);
        let front = &app.windows[&front_window];
        let before_front = (
            front.tab_set.tabs().to_vec(),
            front.tab_set.active_id(),
            front.layouts.clone(),
            front.tabs,
            front.front_content,
            front.window_focus,
        );
        assert_eq!(app.frontmost_window, Some(front_window));
        assert_eq!(
            app.active_handle.lock().unwrap().as_ref().unwrap().id,
            front_session
        );
        assert_eq!(app.clipboard_session_in(front_window), Some(front_session));
        let view_count = app.view_store.len();

        if pane_fallback {
            app.reopen_last_closed_view().unwrap();
        } else {
            app.reopen_last_closed_tab().unwrap();
        }

        let target = &app.windows[&target_window];
        let restored = target.tab_set.active().unwrap();
        assert_eq!(
            target.tab_set.len(),
            surviving_tabs.len() + 1,
            "{path}: a fresh tab is selected"
        );
        assert!(surviving_tabs.iter().all(|tab| tab.id != restored.id));
        assert_eq!(target.tab_set.active_index(), Some(1));
        assert_eq!(
            restored.root.map(&mut |view| matches!(
                app.view_store.get(*view),
                Some(crate::tab_model::View::Native(_))
            )),
            closed_shape,
            "{path}: restored leaf kinds, split ratio and order"
        );
        assert!(
            restored
                .root
                .leaves()
                .iter()
                .all(|view| !closed_views.contains(view))
        );
        assert_eq!(app.view_store.len(), view_count + closed_views.len());
        for (index, tab) in surviving_tabs.iter().enumerate() {
            assert_eq!(
                target.tab_set.tab_at(index),
                Some(tab),
                "{path}: retained tab unchanged"
            );
            assert_eq!(target.layouts[index], surviving_layouts[index]);
        }
        assert!(std::sync::Arc::ptr_eq(
            &survivor_term,
            &app.pool.get(survivor_session).unwrap().term
        ));
        assert_eq!(terminal_state(&app, survivor_session), survivor_before);
        let restored_session = if native {
            let (instance, view) = app.active_native_view(target_window).unwrap();
            assert_eq!(view, restored.focus);
            assert!(matches!(app.native_runtime.view_state(view),
                Some(crate::native_app::AppViewState::Settings(state))
                    if state.route == crate::native_settings::SettingsRoute::About));
            assert_eq!(
                app.native_runtime.app(instance).unwrap().kind(),
                crate::native_app::AppKind::Settings
            );
            None
        } else {
            let session = app.focused_session_id(target_window).unwrap();
            assert_eq!(
                target.tabs.active, 1,
                "{path}: projection follows the new tab"
            );
            assert_eq!(app.active_tree(target_window).unwrap().focus(), session);
            Some(session)
        };
        let expected_owner = if background {
            Some(front_session)
        } else {
            restored_session
        };
        if app.frontmost_window != Some(front_window) {
            failures.push(format!(
                "{path}, background={background}: changed the front window"
            ));
        }
        let active = app.active_handle.lock().unwrap();
        if active.as_ref().map(|handle| handle.id) != expected_owner
            || (background
                && !active
                    .as_ref()
                    .is_some_and(|handle| std::sync::Arc::ptr_eq(&handle.term, &front_term)))
        {
            failures.push(format!(
                "{path}, background={background}: changed the global input owner"
            ));
        }
        drop(active);
        if app
            .frontmost_window
            .and_then(|wid| app.clipboard_session_in(wid))
            != expected_owner
        {
            failures.push(format!(
                "{path}, background={background}: changed the menu Paste resolver owner"
            ));
        }
        if background {
            let front = &app.windows[&front_window];
            assert_eq!(
                (
                    front.tab_set.tabs().to_vec(),
                    front.tab_set.active_id(),
                    front.layouts.clone(),
                    front.tabs,
                    front.front_content,
                    front.window_focus
                ),
                before_front
            );
            assert_eq!(terminal_state(&app, front_session), before_front_text);
            assert!(std::sync::Arc::ptr_eq(
                &front_term,
                &app.front_terminal(front_window).unwrap().term
            ));
        }
        assert!(app.closed_recovery.tabs.is_empty());
        assert!(app.closed_recovery.views.is_empty());
        assert!(app.structural_invariants_ok());
        let after_view_count = app.view_store.len();
        let after_next = app.next_session_id;
        let after_tab = restored.clone();
        let again = if pane_fallback {
            app.reopen_last_closed_view()
        } else {
            app.reopen_last_closed_tab()
        };
        assert!(again.is_err(), "{path}: recovery is consumed once");
        assert_eq!(
            app.windows[&target_window].tab_set.active(),
            Some(&after_tab)
        );
        assert_eq!(app.view_store.len(), after_view_count);
        assert_eq!(app.next_session_id, after_next);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn whole_mixed_tab_close_and_reopen_preserves_one_tab_record_and_topology() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (terminal, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    let before = app
        .tab_restore_descriptor(app.windows[&wid].tab_set.active().unwrap())
        .unwrap();
    assert!(!app.close_tab_at(wid, 1));
    assert_eq!(app.closed_recovery.tabs.len(), 1);
    assert!(app.closed_recovery.views.is_empty());
    assert!(app.pool.get(terminal).is_none());
    assert_eq!(app.windows[&wid].tab_set.len(), 1);

    app.reopen_last_closed_tab().unwrap();
    let restored = app
        .tab_restore_descriptor(app.windows[&wid].tab_set.active().unwrap())
        .unwrap();
    assert_eq!(restored.focused_path, before.focused_path);
    assert_eq!(restored.zoomed, before.zoomed);
    assert!(matches!(
        restored.root,
        crate::restore::RestoredSplitTree::Split {
            axis: crate::restore::SplitKind::Vertical,
            ..
        }
    ));
    assert_eq!(app.windows[&wid].tab_set.active_index(), Some(1));
    assert!(app.closed_recovery.tabs.is_empty());
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_reopened_mixed_tab_stages_without_fronting_a_later_tab() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    for ws in app.windows.values_mut() {
        ws.os_window_for_test = true;
        ws.focused = true;
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let _ = app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    let failed = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(failed));
    app.switch_tab_in(wid, 0);
    let term = app.pool.get(failed).expect("the later tab").term.clone();
    crate::term_lock(&term).process(
        b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\make\r\n\x1b]133;C\x1b\\\
          \x1b]133;D;2\x1b\\\x1b]133;A\x1b\\\x1b]133;B\x1b\\",
    );
    let t0 = std::time::Instant::now();
    app.observe_session_statuses(t0);
    app.observe_session_statuses(t0 + std::time::Duration::from_secs(1));
    let unseen = |app: &App| {
        app.session_status
            .indicators(failed)
            .is_some_and(|bits| bits.status_attention)
    };
    assert!(unseen(&app), "PRECONDITION: a failure nobody has looked at");
    assert!(!app.close_tab_at(wid, 1));
    assert_eq!(app.windows[&wid].tab_set.active_index(), Some(0));

    app.reopen_last_closed_tab().unwrap();
    assert!(
        unseen(&app),
        "staging the reopened tab's Settings leaf fronts no other tab"
    );
    assert_eq!(app.windows[&wid].tab_set.active_index(), Some(1));
    assert_eq!(app.structural_invariant_violation(), None);
}

#[test]
fn canonical_mixed_switch_cycle_close_and_status_ignore_terminal_projection_indices() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));

    assert_eq!(
        app.apply_tab_cmd(TabAction::Select(1)),
        Ok((1, 2)),
        "control status reports canonical mixed-tab active/count"
    );
    assert_eq!(app.windows[&wid].tabs, TabIndex::new(0, 1));
    assert!(app.active_handle.lock().unwrap().is_none());
    assert!(!app.is_visible_session(0));
    assert_eq!(app.windows_displaying(0).count(), 0);
    assert!(
        !app.notify_suppress.lock().unwrap().contains(&0),
        "the parked terminal behind Settings is not notification-visible"
    );

    app.switch_tab_in(wid, 0);
    assert_eq!(app.windows[&wid].tab_set.active_index(), Some(0));
    assert!(
        app.active_handle.lock().unwrap().is_some(),
        "selecting the canonical terminal republishes its PTY target"
    );
    assert!(app.is_visible_session(0));
    assert_eq!(app.windows_displaying(0).count(), 1);

    app.cycle_tab(true);
    assert_eq!(app.windows[&wid].tab_set.active_index(), Some(1));
    assert!(app.active_handle.lock().unwrap().is_none());
    app.cycle_tab(false);
    assert_eq!(app.windows[&wid].tab_set.active_index(), Some(0));

    assert_eq!(
        app.apply_tab_cmd(TabAction::Close(Some(0))),
        Ok((0, 1)),
        "Settings survives as the sole canonical tab"
    );
    let ws = &app.windows[&wid];
    assert_eq!(ws.tab_set.len(), 1);
    assert!(matches!(
        app.view_store.get(ws.tab_set.active().unwrap().focus),
        Some(crate::tab_model::View::Native(_))
    ));
    assert!(ws.layouts.is_empty());
    assert_eq!(ws.tabs, TabIndex::new(0, 0));
    assert!(!ws.pending_close);
    assert!(
        app.pool.get(0).is_none(),
        "the closed terminal was torn down"
    );
    assert!(app.active_handle.lock().unwrap().is_none());
    assert!(app.structural_invariants_ok());
}

#[test]
fn native_reorder_detach_and_migrate_move_no_terminal_projection_or_pool_view() {
    let mut app = App::headless_for_test();
    let source = WindowId(0);
    let sid1 = app.next_session_id;
    app.push_stub_tab(source, crate::stub_session(sid1));
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));

    let (terminal_ids, native_id) = {
        let ws = &app.windows[&source];
        (
            terminal_tab_ids(&ws.tab_set, &app.view_store),
            ws.tab_set.active_id().unwrap(),
        )
    };
    let unmoved = |app: &App, what: &str| {
        let ws = &app.windows[&source];
        assert_eq!(
            terminal_tab_ids(&ws.tab_set, &app.view_store),
            terminal_ids,
            "{what}"
        );
        assert_eq!(
            ws.layouts
                .iter()
                .map(pane::PaneTree::focus)
                .collect::<Vec<_>>(),
            vec![0, sid1],
            "{what}"
        );
        assert_eq!(app.pool.views(0), Some(1), "{what}");
        assert_eq!(app.pool.views(sid1), Some(1), "{what}");
        assert!(app.structural_invariants_ok(), "{what}");
    };
    unmoved(&app, "before");

    app.move_tab(source, 2, 0);
    assert_eq!(app.windows[&source].tab_set.tabs()[0].id, native_id);
    assert_eq!(
        app.windows[&source].tabs.active, 1,
        "parked terminal identity is unchanged"
    );
    unmoved(&app, "after the reorder");

    let native_window = app
        .detach_active_tab_logical()
        .expect("mixed source can detach its active native tab");
    unmoved(&app, "after the detach");
    {
        let ws = &app.windows[&native_window];
        assert_eq!(ws.tab_set.len(), 1);
        assert_eq!(ws.tab_set.active_id(), Some(native_id));
        assert!(ws.layouts.is_empty());
        assert_eq!(ws.tabs, TabIndex::new(0, 0));
        assert!(ws.active_terminal.is_none());
        assert!(
            ws.front_terminal().is_none(),
            "no terminal capability is fabricated"
        );
    }

    let _ = app.migrate_active_tab_to_next_window();
    assert_eq!(app.windows.len(), 1);
    assert_eq!(app.windows[&source].tab_set.active_id(), Some(native_id));
    unmoved(&app, "after the migrate");
}

#[test]
fn terminal_reorder_across_native_tab_reorders_only_its_projection_entry() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid1 = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(sid1));
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));

    app.move_tab(wid, 2, 0);
    let native = app.windows[&wid].tab_set.active_id().unwrap();
    app.move_tab(wid, 1, 2);
    let ws = &app.windows[&wid];
    assert_eq!(ws.tab_set.active_id(), Some(native));
    assert!(matches!(
        app.view_store.get(ws.tab_set.tabs()[0].focus),
        Some(crate::tab_model::View::Native(_))
    ));
    assert_eq!(
        ws.layouts
            .iter()
            .map(pane::PaneTree::focus)
            .collect::<Vec<_>>(),
        vec![sid1, 0]
    );
    assert_eq!(ws.tabs.active, 0, "parked T1 follows its stable identity");
    assert!(app.structural_invariants_ok());

    app.switch_tab_in(wid, 2);
    assert_eq!(
        app.front_terminal(wid)
            .expect("terminal restored to front")
            .session,
        0
    );
    assert_eq!(app.windows[&wid].tabs.active, 1);
    assert_eq!(app.apply_tab_cmd(TabAction::Select(0)), Ok((0, 3)));
    assert!(app.active_handle.lock().unwrap().is_none());
}

#[test]
fn reopen_settings_mints_fresh_tab_view_and_instance_with_route() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (old_instance, old_view) = app.active_native_view(wid).unwrap();
    assert_eq!(app.pool.views(0), Some(1));

    app.close_active_native_tab(wid).unwrap();
    assert!(app.view_store.get(old_view).is_none());
    assert!(app.native_runtime.app(old_instance).is_none());
    app.reopen_last_closed_tab().unwrap();

    let (new_instance, new_view) = app.active_native_view(wid).unwrap();
    assert_ne!(new_view, old_view, "closed ViewId is never reused");
    assert_ne!(
        new_instance, old_instance,
        "final-view close retires and remints its app instance"
    );
    assert!(matches!(
        app.native_runtime.view_state(new_view),
        Some(crate::native_app::AppViewState::Settings(state))
            if state.route == crate::native_settings::SettingsRoute::About
    ));
    assert!(
        app.dispatch_native_view_event(
            wid,
            old_view,
            crate::native_app::AppEvent::FocusChanged(None),
        )
        .is_err(),
        "a delayed event for the retired view may not target its replacement"
    );
    assert_eq!(
        app.pool.views(0),
        Some(1),
        "native reopen has zero PTY churn"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn reopened_editor_keeps_document_identity_and_shares_only_a_live_controller() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-reopen-shared-document-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("shared.md");
    std::fs::write(&path, "shared\n").unwrap();
    let uri = file_uri(&path);

    let wid = WindowId(0);
    for keep_peer in [false, true] {
        let mut app = App::headless_for_test();
        app.open_document_tab(crate::native_app::AppKind::Markdown, &uri)
            .unwrap();
        let opened = app.active_native_view(wid).unwrap();
        app.open_document_tab(crate::native_app::AppKind::Editor, &uri)
            .unwrap();
        let (old_instance, old_view) = app.active_native_view(wid).unwrap();
        assert_eq!(
            (old_instance, old_view),
            opened,
            "Read and Edit reuse one view"
        );
        let document = app.native_runtime.document_id(old_instance).unwrap();
        let canonical_uri = app
            .document_store
            .canonical_uri(document)
            .expect("canonical shared URI")
            .to_string();
        let old_tab = app.windows[&wid].tab_set.active_id().unwrap();
        assert_eq!(app.document_store.view_count(document), Some(1));
        if keep_peer {
            let peer = WindowId(1);
            app.install_window_state(peer, crate::stub_session(99), 38, 132);
            app.open_document_tab_in_window(peer, crate::native_app::AppKind::Editor, &uri)
                .unwrap();
            let (peer_instance, peer_view) = app.active_native_view(peer).unwrap();
            assert_eq!(peer_instance, old_instance);
            assert_ne!(peer_view, old_view);
            app.frontmost_window = Some(wid);
        }
        let retained_views = usize::from(keep_peer);
        app.close_active_native_tab(wid).unwrap();
        assert_eq!(
            app.document_store.view_count(document),
            Some(retained_views)
        );
        assert_eq!(app.native_runtime.app(old_instance).is_some(), keep_peer);
        app.reopen_last_closed_tab().unwrap();
        let (new_instance, new_view) = app.active_native_view(wid).unwrap();
        let new_tab = app.windows[&wid].tab_set.active_id().unwrap();
        assert_eq!(new_instance == old_instance, keep_peer);
        assert_ne!(new_view, old_view);
        assert_ne!(new_tab, old_tab);
        assert_eq!(app.native_runtime.document_id(new_instance), Some(document));
        assert_eq!(
            app.document_store.id_for_uri(&canonical_uri),
            Some(document)
        );
        assert_eq!(
            app.document_store.view_count(document),
            Some(retained_views + 1)
        );
        assert!(
            app.dispatch_native_view_event(
                wid,
                old_view,
                crate::native_app::AppEvent::FocusChanged(None),
            )
            .is_err(),
            "retired view input cannot reach the reopened view"
        );
        assert!(app.structural_invariants_ok());
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn reopen_missing_document_installs_recovery_view_instead_of_losing_the_record() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-reopen-missing-document-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gone.md");
    std::fs::write(&path, "gone soon\n").unwrap();
    let uri = file_uri(&path);

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(crate::native_app::AppKind::Markdown, &uri)
        .unwrap();
    let (_, old_view) = app.active_native_view(wid).unwrap();
    app.close_active_native_tab(wid).unwrap();
    std::fs::remove_file(&path).unwrap();
    let before_tabs = app.windows[&wid].tab_set.len();
    let before_views = app.view_store.len();

    app.reopen_last_closed_tab().unwrap();
    assert_eq!(app.windows[&wid].tab_set.len(), before_tabs + 1);
    assert_eq!(app.view_store.len(), before_views + 1);
    assert!(app.view_store.get(old_view).is_none());
    assert!(app.closed_recovery.tabs.is_empty());
    let (instance, _) = app.active_native_view(wid).expect("Recovery active");
    assert_eq!(
        app.native_runtime.app(instance).map(|app| app.kind()),
        Some(crate::native_app::AppKind::Recovery)
    );
    assert!(app.structural_invariants_ok());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn closed_terminal_tab_reattaches_a_still_live_shared_session() {
    let mut app = App::headless_for_test();
    let original = WindowId(0);
    let shared = app
        .open_active_session_in_new_window_logical()
        .expect("second terminal viewer");
    assert_eq!(app.pool.views(0), Some(2));
    assert!(app.close_tab_at(shared, 0), "only tab closes its window");
    assert_eq!(app.closed_recovery.tabs.len(), 1);
    assert_eq!(app.close_window_logical(shared), crate::CloseOutcome::Stay);
    assert_eq!(app.pool.views(0), Some(1));
    app.frontmost_window = Some(original);

    app.reopen_last_closed_tab().unwrap();
    assert_eq!(app.pool.views(0), Some(2));
    assert_eq!(app.windows[&original].tab_set.len(), 2);
    assert!(app.closed_recovery.tabs.is_empty());
    assert!(app.structural_invariants_ok());

    let reopened = app.windows[&original].tab_set.active_id().unwrap();
    let reopened_index = app.windows[&original].tab_set.active_index().unwrap();
    app.move_tab(original, reopened_index, 1);
    let first = app.windows[&original].tab_set.tabs()[0].id;
    assert_eq!(app.windows[&original].tab_set.active_id(), Some(reopened));
    assert_eq!(app.windows[&original].tab_set.active_index(), Some(1));
    assert_ne!(reopened, first, "the reopened viewer is active");
    for preferred in [Some(original), None] {
        assert!(app.focus_session_preferring(preferred, 0));
        assert_eq!(
            app.windows[&original].tab_set.active_id(),
            Some(reopened),
            "focus keeps the active viewer, not its background twin"
        );
    }

    let unrelated = app.next_session_id;
    app.push_stub_tab(original, crate::stub_session(unrelated));
    assert_eq!(app.focused_session_id(original), Some(unrelated));
    assert!(app.focus_session_preferring(Some(original), 0));
    assert_eq!(
        app.windows[&original].tab_set.active_id(),
        Some(first),
        "without an active viewer, the first occurrence still wins"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn reopen_same_window_settings_mints_an_independent_view_instead_of_aliasing() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let existing = app.active_native_view(wid).expect("Settings active");
    let tab_count = app.windows[&wid].tab_set.len();
    let descriptor = crate::restore::NativeTabRestore::Settings {
        route: "/about".to_string(),
    };
    app.closed_recovery.tabs.push(
        crate::closed_recovery::ClosedTab {
            original_window: wid,
            original_index: 0,
            tab: crate::restore::RestoredTab::from_legacy_native(&descriptor).unwrap(),
        },
        0,
    );

    app.reopen_last_closed_tab().unwrap();
    let reopened = app.active_native_view(wid).expect("reopened Settings view");
    assert_eq!(reopened.0, existing.0, "singleton controller is shared");
    assert_ne!(reopened.1, existing.1, "presentation identity is fresh");
    assert_eq!(app.windows[&wid].tab_set.len(), tab_count + 1);
    assert!(app.closed_recovery.tabs.is_empty());
    assert!(!app.can_reopen_closed_tab());
    assert!(app.structural_invariants_ok());
}

#[test]
fn native_reopen_ledger_tier1_conforms_and_negative_controls_are_rejected() {
    use aterm_models_native::native_reopen_ledger_model;
    use aterm_spec::interp::{State, admits};

    fn assert_step(
        model: &aterm_spec::derive::Model,
        before: &State,
        after: &State,
        action: &'static str,
    ) {
        assert_eq!(
            model.successors(action, before).as_slice(),
            std::slice::from_ref(after),
            "shipping reopen transition must conform specifically to {action}"
        );
        assert_eq!(admits(model, before, after), Some(action));
    }

    let model = native_reopen_ledger_model();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    assert_eq!(app.windows[&wid].tab_set.active_id().unwrap().get(), 2);
    let before_close = model.init_state();
    app.close_active_native_tab(wid).unwrap();
    let mut after_close = before_close.clone();
    after_close.insert("ledger", app.closed_recovery.tabs.len() as i64);
    after_close.insert("native_live", 0);
    after_close.insert("retired_id", 2);
    assert_step(&model, &before_close, &after_close, "Close");

    app.reopen_last_closed_tab().unwrap();
    let reopened = app.windows[&wid].tab_set.active_id().unwrap().get();
    let mut after_reopen = after_close.clone();
    after_reopen.insert("ledger", app.closed_recovery.tabs.len() as i64);
    after_reopen.insert("native_live", 1);
    after_reopen.insert("opened_id", reopened as i64);
    after_reopen.insert("next_id", reopened.saturating_add(1) as i64);
    assert_step(&model, &after_close, &after_reopen, "Reopen");

    let mut reused = after_reopen.clone();
    reused.insert("opened_id", after_close["retired_id"]);
    reused.insert("next_id", after_close["next_id"]);
    reused.insert("reused_retired", 1);
    assert_eq!(admits(&model, &after_close, &reused), None);
    assert!(!model.check_invariant("FreshReopenIdentity", &reused));

    let mut failed = App::headless_for_test();
    assert!(failed.open_settings_tab(crate::native_settings::SettingsRoute::About));
    failed.close_active_native_tab(wid).unwrap();
    failed.windows.clear();
    failed.frontmost_window = None;
    assert!(failed.reopen_last_closed_tab().is_err());
    let mut after_failure = after_close.clone();
    after_failure.insert("ledger", failed.closed_recovery.tabs.len() as i64);
    after_failure.insert("failures", 1);
    assert_step(&model, &after_close, &after_failure, "FailReopen");

    let mut lossy = after_failure.clone();
    lossy.insert("ledger", 0);
    lossy.insert("lost_on_failure", 1);
    assert_eq!(admits(&model, &after_close, &lossy), None);
    assert!(!model.check_invariant("FailedReopenRetainsDescriptor", &lossy));

    let cap = model
        .consts
        .iter()
        .find_map(|&(name, value)| (name == "Cap").then_some(value))
        .expect("NativeReopenLedger declares Cap");
    let mut full = App::headless_for_test();
    full.closed_recovery.tabs = crate::closed_recovery::RecoveryLedger::new(
        usize::try_from(cap).expect("small model capacity"),
        crate::closed_recovery::CLOSED_TAB_MAX_AGE_MS,
    );
    let active_id = |app: &App| app.windows[&wid].tab_set.active_id().unwrap().get() as i64;
    assert!(full.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let mut state = model.init_state();
    let mut before_last_close = state.clone();
    for close in 0..=cap {
        if close > 0 {
            assert!(full.open_settings_tab(crate::native_settings::SettingsRoute::About));
            let opened = active_id(&full);
            let mut after_open = state.clone();
            after_open.insert("native_live", 1);
            after_open.insert("opened_id", opened);
            after_open.insert("next_id", opened + 1);
            assert_step(&model, &state, &after_open, "OpenAnother");
            state = after_open;
        }
        let retired = active_id(&full);
        full.close_active_native_tab(wid).unwrap();
        let mut after_close = state.clone();
        after_close.insert("ledger", full.closed_recovery.tabs.len() as i64);
        after_close.insert("native_live", 0);
        after_close.insert("retired_id", retired);
        assert_step(&model, &state, &after_close, "Close");
        before_last_close = std::mem::replace(&mut state, after_close);
    }
    assert_eq!(state["ledger"], cap, "the full ledger evicted its oldest");

    let mut overfull = state.clone();
    overfull.insert("ledger", cap + 1);
    assert_eq!(admits(&model, &before_last_close, &overfull), None);
    assert!(!model.check_invariant("LedgerBounded", &overfull));
}
