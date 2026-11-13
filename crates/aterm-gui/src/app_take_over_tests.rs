// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::menu::MenuAction;
use crate::palette::PaletteState;
use crate::session_timeline::MetaField;
use std::sync::Arc;

#[cfg(unix)]
#[test]
#[ignore = "owned native child exercised by the hold-action parent"]
fn native_hold_action_child() {
    use std::io::Write as _;
    let args: Vec<String> = std::env::args().collect();
    let Some(gate) = args.iter().find_map(|arg| arg.strip_prefix("hold-gate=")) else {
        return;
    };
    let port: u16 = args
        .iter()
        .find_map(|arg| arg.strip_prefix("hold-port="))
        .unwrap()
        .parse()
        .unwrap();
    println!("owned hold action started");
    let mut proof = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    proof.write_all(b"ready\n").unwrap();
    let _writer = aterm_update_core::FileLock::acquire(Path::new(gate)).unwrap();
    println!("owned hold action completed");
    eprintln!("owned hold action diagnostic");
    if let Some(code) = args.iter().find_map(|arg| arg.strip_prefix("hold-exit=")) {
        std::process::exit(code.parse().unwrap());
    }
}

#[cfg(unix)]
#[test]
fn a_required_fleet_hold_action_waits_for_its_native_command() {
    use std::cell::RefCell;
    use std::io::Read as _;

    assert_eq!(
        CliVerb::ReadThisMac.cap(),
        Some(crate::bounded_child::READ_CAP)
    );
    assert_eq!(
        CliVerb::ReadFleet.cap(),
        Some(crate::bounded_child::READ_CAP)
    );
    assert_eq!(
        CliVerb::FleetPeers.cap(),
        Some(crate::bounded_child::READ_CAP)
    );
    assert_eq!(
        CliVerb::ReadPeer.cap(),
        Some(crate::bounded_child::READ_CAP)
    );
    for verb in [CliVerb::BusStatus, CliVerb::BusOn, CliVerb::BusOff] {
        assert_eq!(verb.cap(), None);
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let gate = root.path().join("hold.lock");
    let child = std::env::current_exe().unwrap();
    for (age, exit) in [(31, 0), (61, 0), (0, 0), (31, 7)] {
        let held = RefCell::new(Some(aterm_update_core::FileLock::acquire(&gate).unwrap()));
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let proof = RefCell::new(None);
        let mut released = false;
        let args = vec![
            "--ignored".into(),
            "--exact".into(),
            "app_take_over::tests::native_hold_action_child".into(),
            "--nocapture".into(),
            format!("hold-gate={}", gate.display()),
            format!("hold-port={port}"),
            format!("hold-exit={exit}"),
        ];
        let got = crate::bounded_child::run_bounded_observing(
            &child,
            &args,
            CliVerb::DrivePeer.cap(),
            || {
                let (mut connected, _) = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Until(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
                    aterm_uds::AcceptedMode::Blocking,
                )
                .expect("owned child reached its writer");
                // The sanctioned accept makes the stream blocking explicitly, so the read
                // timeout below waits through the child's planted connect-to-write delay.
                connected
                    .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                    .unwrap();
                let mut ready = [0; 6];
                connected.read_exact(&mut ready).unwrap();
                assert_eq!(&ready, b"ready\n");
                *proof.borrow_mut() = Some(connected);
                Instant::now() - Duration::from_secs(age)
            },
            || {
                if !released {
                    assert_eq!(
                        aterm_update_core::FileLock::acquire_within(&gate, Duration::ZERO)
                            .err()
                            .unwrap()
                            .kind(),
                        std::io::ErrorKind::TimedOut,
                    );
                    drop(held.borrow_mut().take());
                    released = true;
                }
            },
        );
        if age == 61 {
            assert!(
                got.is_err(),
                "the actual expired child is killed and reaped"
            );
            assert!(!released, "an expired allowance cannot release custody");
            assert!(held.borrow().is_some());
            assert_eq!(
                aterm_update_core::FileLock::acquire_within(&gate, Duration::ZERO)
                    .err()
                    .unwrap()
                    .kind(),
                std::io::ErrorKind::TimedOut,
            );
        } else {
            let got = got.expect("the requested command retains its completion allowance");
            assert!(released);
            assert_eq!(got.code, Some(exit));
            assert!(
                got.stdout.contains("owned hold action completed"),
                "{got:?}"
            );
            assert!(
                got.stderr.contains("owned hold action diagnostic"),
                "{got:?}"
            );
        }
        assert_eq!(
            proof.borrow_mut().as_mut().unwrap().read(&mut [0]).unwrap(),
            0,
            "the actual child ended before its owned scratch is removed"
        );
        drop(held.into_inner());
        drop(aterm_update_core::FileLock::acquire_within(&gate, Duration::ZERO).unwrap());
    }
}

fn app_with_stub() -> (App, WindowId, u64, Arc<crate::SessionCtx>) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(sid));
    app.frontmost_window = Some(wid);
    let ctx = app.pool.get(sid).expect("stub pooled").ctx.clone();
    (app, wid, sid, ctx)
}

fn row(app: &App, wid: WindowId, a: MenuAction) -> crate::palette::PaletteRow {
    app.palette_snapshot(wid)
        .rows()
        .iter()
        .find(|r| r.action == a)
        .cloned()
        .unwrap_or_else(|| panic!("{a:?} has a row"))
}

#[test]
fn take_over_sets_the_person_slot_and_hand_back_lifts_it_then_a_clients() {
    let (mut app, wid, sid, ctx) = app_with_stub();
    app.refresh_presence_window(wid);
    assert!(row(&app, wid, MenuAction::TakeOver).enabled);
    assert!(!row(&app, wid, MenuAction::HandBack).enabled);

    app.take_over(wid);
    assert_eq!(
        ctx.fabric.holds().person(),
        Some(crate::fabric::TAKEN_OVER),
        "the window path sets origin person"
    );
    assert!(app.front_person_hold(wid));
    assert!(!row(&app, wid, MenuAction::TakeOver).enabled);
    assert!(row(&app, wid, MenuAction::HandBack).enabled);
    assert_eq!(
        ctx.timeline.lock().unwrap().need().hold_word(),
        "person",
        "the published verdict names the person's hold"
    );
    let wire = format!("{} off", ctx.self_id.as_str());
    assert_eq!(
        crate::fabric::cmd_hold(&app.store, &wire, crate::fabric::HoldIssuer::Owner),
        "OK hold=1\n",
        "the held agent's own hold off lifts nothing of the person's"
    );
    assert!(ctx.fabric.holds().person().is_some());
    assert!(crate::fabric::halt_refusal(&ctx, "send").is_some());
    let held = app.person_holds();
    assert_eq!(
        held.iter()
            .map(|h| (h.session, h.refused))
            .collect::<Vec<_>>(),
        [(sid, 1)]
    );

    let on = format!("{} on reason=mine", ctx.self_id.as_str());
    assert_eq!(
        crate::fabric::cmd_hold(&app.store, &on, crate::fabric::HoldIssuer::Owner),
        "OK hold=1\n"
    );
    app.hand_back(wid);
    assert_eq!(ctx.fabric.holds().origins(), "local");
    assert!(app.person_holds().is_empty());
    assert!(app.front_person_hold(wid));
    assert!(!row(&app, wid, MenuAction::TakeOver).enabled);
    assert!(row(&app, wid, MenuAction::HandBack).enabled);
    app.hand_back(wid);
    assert_eq!(ctx.fabric.holds().origins(), "-");
    assert!(!app.front_person_hold(wid));
    assert!(row(&app, wid, MenuAction::TakeOver).enabled);
    assert!(!row(&app, wid, MenuAction::HandBack).enabled);
    let fleet = format!("{} on origin=fleet reason=bus", ctx.self_id.as_str());
    assert_eq!(
        crate::fabric::cmd_hold(&app.store, &fleet, crate::fabric::HoldIssuer::Bridge),
        "OK hold=1\n"
    );
    assert!(!app.front_person_hold(wid));
    app.hand_back(wid);
    assert_eq!(ctx.fabric.holds().origins(), "fleet");
}

#[test]
fn a_controller_key_into_the_palette_cannot_hand_back_and_a_persons_can() {
    use crate::input::{InputEvent, Source};
    let (mut app, wid, _sid, ctx) = app_with_stub();
    app.take_over(wid);
    assert!(ctx.fabric.holds().person().is_some());
    let enter = InputEvent::Key {
        key: aterm_types::keyboard::Key::Named(aterm_types::keyboard::NamedKey::Enter),
        mods: aterm_types::keyboard::Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    };
    for (src, lifted) in [(Source::Controller, false), (Source::Human, true)] {
        app.palette_enter();
        app.palette_input_event(wid, &InputEvent::Text("Hand Back".to_string()));
        assert_eq!(
            app.windows[&wid]
                .palette()
                .and_then(PaletteState::selected_target),
            Some(crate::palette::PaletteTarget::Menu(MenuAction::HandBack)),
            "the filter lands on Hand Back"
        );
        let _ = app.input_to_session(
            wid,
            enter.clone(),
            src,
            None,
            crate::app_input::PressPhase::Initial,
        );
        assert_eq!(
            ctx.fabric.holds().person().is_none(),
            lifted,
            "{src:?}: the person's slot"
        );
        assert!(
            !app.person_gesture_refused(),
            "the mark ends with the gesture"
        );
    }
    app.take_over(wid);
    let session = app.focused_session_id(wid).expect("a session");
    assert!(
        app.with_gesture_source(true, |app| app.hand_back_session(session))
            .is_err()
    );
    assert!(ctx.fabric.holds().person().is_some());
}

#[test]
fn the_tab_menus_take_over_refuses_a_controller_and_takes_the_persons() {
    let (mut app, wid, _sid, ctx) = app_with_stub();
    app.tab_strip_rows = 1;
    let take_over_row = |app: &App| {
        app.windows[&wid]
            .tab_menu
            .as_ref()
            .expect("the menu is open")
            .entries
            .iter()
            .position(|e| {
                matches!(
                    e,
                    crate::session_chrome::TabMenuEntry::SessionAction {
                        action: MenuAction::TakeOver,
                        ..
                    }
                )
            })
            .expect("the AGENT section's Take Over row")
    };
    for (controller, taken) in [(true, false), (false, true)] {
        assert!(app.open_active_tab_context_menu(wid));
        let i = take_over_row(&app);
        app.with_gesture_source(controller, |app| app.activate_tab_menu_entry(wid, i));
        assert_eq!(
            ctx.fabric.holds().person().is_some(),
            taken,
            "controller={controller}"
        );
    }
}

#[cfg(unix)]
fn hand_back_menu_origin_case(change: &str) {
    let (mut app, wid, source, source_ctx) = app_with_stub();
    app.tab_strip_rows = 1;
    let sibling = app.split_active_stub_tab(wid);
    let sibling_ctx = app.pool.get(sibling).unwrap().ctx.clone();
    crate::term_lock(app.pool.get(source).unwrap().live_term()).process(b"source terminal");
    crate::term_lock(app.pool.get(sibling).unwrap().live_term()).process(b"sibling terminal");
    assert!(app.take_over_session(source).is_ok());
    assert!(app.take_over_session(sibling).is_ok());
    assert_eq!(
        source_ctx.fabric.holds().person(),
        Some(crate::fabric::TAKEN_OVER)
    );
    assert_eq!(
        sibling_ctx.fabric.holds().person(),
        Some(crate::fabric::TAKEN_OVER)
    );
    assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Left));
    assert_eq!(app.focused_session_id(wid), Some(source));
    let tab = app.windows[&wid].tab_set.active_id().unwrap();
    let source_view = app.windows[&wid].tab_set.active().unwrap().focus;
    assert!(app.open_active_tab_context_menu(wid));
    let menu = app.windows[&wid].tab_menu.as_ref().unwrap();
    assert_eq!(menu.tab, tab);
    let hand_back = menu
        .entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                crate::session_chrome::TabMenuEntry::SessionAction {
                    action: MenuAction::HandBack,
                    enabled: true,
                    ..
                }
            )
        })
        .expect("the source's held state offers Hand Back");
    assert!(crate::tab_menu::is_selectable(&menu.entries[hand_back]));

    match change {
        "unchanged" => {}
        "focus sibling" => {
            assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Right));
            assert!(app.view_store.contains(source_view));
        }
        "close source" => {
            assert_eq!(app.close_active_tab_in(wid), None);
            assert!(!app.view_store.contains(source_view));
            assert!(app.pool.get(source).is_none());
            assert!(app.begin_active_session_rename(wid));
            assert!(app.rename_field_edit(
                wid,
                crate::app_search::SearchEdit::Insert("unsubmitted sibling draft".into())
            ));
            assert_eq!(
                app.windows[&wid].rename_edit.as_ref().unwrap().text,
                "unsubmitted sibling draft"
            );
        }
        _ => unreachable!(),
    }
    let expected_focus = if change == "unchanged" {
        source
    } else {
        sibling
    };
    assert_eq!(app.windows[&wid].tab_set.active_id(), Some(tab));
    assert_eq!(app.focused_session_id(wid), Some(expected_focus));
    let retained_menu = app.windows[&wid]
        .tab_menu
        .as_ref()
        .expect("the captured menu survives");
    assert_eq!(retained_menu.tab, tab);
    assert!(matches!(
        retained_menu.entries[hand_back],
        crate::session_chrome::TabMenuEntry::SessionAction {
            action: MenuAction::HandBack,
            enabled: true,
            ..
        }
    ));
    assert_eq!(
        sibling_ctx.fabric.holds().person(),
        Some(crate::fabric::TAKEN_OVER)
    );
    let source_hold = source_ctx.fabric.holds().person().map(str::to_string);
    let at = std::time::Instant::now();
    if change == "close source" {
        let window = app.windows.get_mut(&wid).unwrap();
        window.fx.cursor_mut().glow.note_typed_cells(at, 1);
        window.fx.cursor_mut().trail.note_typed(at);
        assert!(window.fx.cursor().glow.move_licensed(at));
        assert!(window.fx.cursor().trail.move_licensed(at));
    }
    let live = |app: &App| {
        app.pool
            .iter()
            .map(|session| {
                let terminal = crate::term_lock(session.live_term());
                let metadata = session.ctx.meta.lock().unwrap();
                (
                    session.id,
                    (
                        terminal.visible_content(),
                        terminal.content_seq(),
                        session.ctx.sink.input_epoch(),
                        metadata.presentation_value("title"),
                        metadata.presentation_value("role"),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let live_before = live(&app);
    let tabs = app.windows[&wid].tab_set.clone();
    let front = app.windows[&wid].front_content;
    let draft = app.windows[&wid].rename_edit.clone();
    let focus_order = app.focus_order.clone();
    let config = app.native_config_service.snapshot();

    app.with_gesture_source(false, |app| app.activate_tab_menu_entry(wid, hand_back));

    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "the chosen menu closes"
    );
    assert_eq!(
        live(&app),
        live_before,
        "menu activation writes no terminal input or metadata"
    );
    assert_eq!(app.windows[&wid].tab_set, tabs);
    assert_eq!(app.windows[&wid].front_content, front);
    assert_eq!(app.windows[&wid].rename_edit, draft);
    assert_eq!(app.frontmost_window, Some(wid));
    assert_eq!(app.focus_order, focus_order);
    assert_eq!(app.focused_session_id(wid), Some(expected_focus));
    assert_eq!(
        app.active_handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|session| session.id),
        Some(expected_focus)
    );
    assert_eq!(app.native_config_service.snapshot(), config);
    assert!(app.structural_invariants_ok());
    if change == "close source" {
        assert!(app.windows[&wid].fx.cursor().glow.move_licensed(at));
        assert!(app.windows[&wid].fx.cursor().trail.move_licensed(at));
        assert_eq!(
            source_ctx.fabric.holds().person().map(str::to_string),
            source_hold
        );
    }
    let source_was_returned = source_ctx.fabric.holds().person().is_none();
    let sibling_stays_held = sibling_ctx.fabric.holds().person() == Some(crate::fabric::TAKEN_OVER);
    assert!(
        sibling_stays_held && (change == "close source" || source_was_returned),
        "{change}: captured source returned={source_was_returned}, unrelated sibling still held={sibling_stays_held}"
    );
}

#[cfg(unix)]
#[test]
fn a_tab_menus_hand_back_returns_its_unchanged_source_session() {
    hand_back_menu_origin_case("unchanged");
}

#[cfg(unix)]
#[test]
fn a_tab_menus_hand_back_does_not_retarget_a_focused_sibling() {
    hand_back_menu_origin_case("focus sibling");
}

#[cfg(unix)]
#[test]
fn a_tab_menus_hand_back_cancels_when_its_source_pane_closed() {
    hand_back_menu_origin_case("close source");
}

#[cfg(unix)]
fn captured_session_row(
    app: &App,
    wid: WindowId,
    action: MenuAction,
) -> (usize, crate::session_chrome::TerminalMenuOrigin) {
    let menu = app.windows[&wid]
        .tab_menu
        .as_ref()
        .expect("the actual menu is open");
    menu.entries
        .iter()
        .enumerate()
        .find_map(|(index, entry)| match entry {
            crate::session_chrome::TabMenuEntry::SessionAction {
                action: candidate,
                enabled: true,
                origin: Some(origin),
                ..
            } if *candidate == action => Some((index, *origin)),
            _ => None,
        })
        .expect("the enabled session action has its captured terminal owner")
}

#[test]
fn session_menu_editors_follow_the_supported_strip_setting() {
    use crate::session_chrome::{TabMenuEntry, TerminalMenuOrigin};

    let (mut app, wid, source, source_ctx) = app_with_stub();
    assert_eq!(app.tab_strip_rows, 0);
    assert!(!app.can_rename_session(wid));
    let tab = app.windows[&wid].tab_set.active().unwrap();
    let (tab_id, view, root) = (tab.id, tab.focus, tab.root.clone());
    let origin = TerminalMenuOrigin {
        view,
        session: source,
    };
    let editor_rows = |entries: &[TabMenuEntry]| {
        [MenuAction::RenameSession, MenuAction::SetRole].map(|wanted| {
            entries.iter().find_map(|entry| match entry {
                TabMenuEntry::SessionAction {
                    action,
                    enabled,
                    origin,
                    ..
                } if *action == wanted => Some((*enabled, *origin)),
                _ => None,
            })
        })
    };
    let titles = app.tab_titles(wid);
    let chrome = app.tab_chrome_ext(wid, &titles);
    let index = app.windows[&wid].tab_set.active_index().unwrap();
    assert_eq!(
        editor_rows(&chrome[index].menu),
        [Some((false, Some(origin))); 2],
        "the cached source has no editor surface before the setting changes"
    );
    let input_epoch = source_ctx.sink.input_epoch();
    let content = crate::term_lock(app.pool.get(source).unwrap().live_term()).visible_content();
    let config = app.native_config_service.snapshot();

    app.set_tab_strip_rows(1, Instant::now());

    assert_eq!(app.tab_strip_rows, 1);
    assert!(app.can_rename_session(wid));
    assert!(app.open_active_tab_context_menu(wid));
    let menu = app.windows[&wid].tab_menu.as_ref().unwrap();
    assert_eq!(menu.tab, tab_id);
    let actual = editor_rows(&menu.entries);
    assert_eq!(app.windows[&wid].tab_set.active().unwrap().focus, view);
    assert_eq!(app.windows[&wid].tab_set.active().unwrap().root, root);
    assert_eq!(app.focused_session_id(wid), Some(source));
    assert_eq!(app.frontmost_window, Some(wid));
    assert_eq!(source_ctx.sink.input_epoch(), input_epoch);
    assert_eq!(
        crate::term_lock(app.pool.get(source).unwrap().live_term()).visible_content(),
        content
    );
    assert_eq!(app.native_config_service.snapshot(), config);
    assert!(app.structural_invariants_ok());
    assert_eq!(
        actual,
        [Some((true, Some(origin))); 2],
        "the actual card follows the newly available editor surface without another session event"
    );
}

#[cfg(unix)]
#[test]
fn session_menu_editors_keep_the_captured_pane_after_sibling_focus() {
    for (action, field) in [
        (MenuAction::RenameSession, MetaField::Title),
        (MenuAction::SetRole, MetaField::Role),
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        let source = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(source));
        let source_ctx = app.pool.get(source).unwrap().ctx.clone();
        let sibling = app.split_active_stub_tab(wid);
        let sibling_ctx = app.pool.get(sibling).unwrap().ctx.clone();
        crate::term_lock(app.pool.get(source).unwrap().live_term()).process(b"source text");
        crate::term_lock(app.pool.get(sibling).unwrap().live_term()).process(b"sibling text");
        assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Left));
        let tab = app.windows[&wid].tab_set.active_id().unwrap();
        let view = app.windows[&wid].tab_set.active().unwrap().focus;
        assert!(app.open_active_tab_context_menu(wid));
        let (row, origin) = captured_session_row(&app, wid, action);
        assert_eq!((origin.view, origin.session), (view, source));
        assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Right));
        assert_eq!(app.focused_session_id(wid), Some(sibling));
        let live = |app: &App| {
            app.pool
                .iter()
                .map(|session| {
                    let terminal = crate::term_lock(session.live_term());
                    (
                        session.id,
                        (
                            terminal.visible_content(),
                            terminal.content_seq(),
                            session.ctx.sink.input_epoch(),
                        ),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let before = live(&app);
        let root = app.windows[&wid].tab_set.active().unwrap().root.clone();
        let config = app.native_config_service.snapshot();

        app.activate_tab_menu_entry(wid, row);

        assert!(app.windows[&wid].tab_menu.is_none());
        let edit = app.windows[&wid]
            .rename_edit
            .as_ref()
            .expect("the captured session's editor opens");
        assert_eq!((edit.session, edit.tab, edit.field), (source, tab, field));
        assert_eq!(app.windows[&wid].tab_set.active().unwrap().focus, view);
        assert_eq!(app.focused_session_id(wid), Some(source));
        assert!(app.rename_field_edit(
            wid,
            crate::app_search::SearchEdit::Insert("source edit".into())
        ));
        app.commit_session_rename(wid, source, "source edit");
        assert_eq!(
            source_ctx
                .meta
                .lock()
                .unwrap()
                .presentation_value(field.wire_name())
                .as_deref(),
            Some("source edit")
        );
        assert!(
            sibling_ctx
                .meta
                .lock()
                .unwrap()
                .presentation_value("title")
                .is_none()
        );
        assert!(
            sibling_ctx
                .meta
                .lock()
                .unwrap()
                .presentation_value("role")
                .is_none()
        );
        assert_eq!(live(&app), before);
        assert_eq!(app.windows[&wid].tab_set.active().unwrap().root, root);
        assert_eq!(app.frontmost_window, Some(wid));
        assert_eq!(
            app.active_handle
                .lock()
                .unwrap()
                .as_ref()
                .map(|session| session.id),
            Some(source)
        );
        assert_eq!(app.native_config_service.snapshot(), config);
        assert!(app.structural_invariants_ok());
    }
}

#[cfg(unix)]
#[test]
fn session_menu_take_over_and_log_keep_the_captured_session() {
    for action in [MenuAction::TakeOver, MenuAction::HarnessLog] {
        let (mut app, wid, source, source_ctx) = app_with_stub();
        app.tab_strip_rows = 1;
        let sibling = app.split_active_stub_tab(wid);
        let sibling_ctx = app.pool.get(sibling).unwrap().ctx.clone();
        assert!(app.take_over_session(sibling).is_ok());
        assert!(source_ctx.fabric.holds().person().is_none());
        assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Left));
        let tab = app.windows[&wid].tab_set.active_id().unwrap();
        let source_view = app.windows[&wid].tab_set.active().unwrap().focus;
        assert!(app.open_active_tab_context_menu(wid));
        let (row, origin) = captured_session_row(&app, wid, action);
        assert_eq!((origin.view, origin.session), (source_view, source));
        assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Right));
        assert_eq!(app.focused_session_id(wid), Some(sibling));
        let live = |app: &App| {
            app.pool
                .iter()
                .map(|session| {
                    let terminal = crate::term_lock(session.live_term());
                    (
                        session.id,
                        (
                            terminal.visible_content(),
                            terminal.content_seq(),
                            session.ctx.sink.input_epoch(),
                        ),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let before = live(&app);
        let tabs = app.windows[&wid].tab_set.clone();
        let front = app.windows[&wid].front_content;
        let config = app.native_config_service.snapshot();

        app.with_gesture_source(false, |app| app.activate_tab_menu_entry(wid, row));

        assert!(app.windows[&wid].tab_menu.is_none());
        match action {
            MenuAction::TakeOver => {
                assert_eq!(
                    source_ctx.fabric.holds().person(),
                    Some(crate::fabric::TAKEN_OVER)
                );
                assert!(app.last_ledger_plan.is_none());
            }
            MenuAction::HarnessLog => {
                assert!(source_ctx.fabric.holds().person().is_none());
                assert_eq!(
                    app.last_ledger_plan.as_ref().unwrap().sid,
                    source_ctx.self_id.as_str()
                );
            }
            _ => unreachable!(),
        }
        assert_eq!(
            sibling_ctx.fabric.holds().person(),
            Some(crate::fabric::TAKEN_OVER)
        );
        assert_eq!(app.windows[&wid].tab_set.active_id(), Some(tab));
        assert_eq!(app.windows[&wid].tab_set, tabs);
        assert_eq!(app.windows[&wid].front_content, front);
        assert_eq!(app.focused_session_id(wid), Some(sibling));
        assert_eq!(
            app.active_handle
                .lock()
                .unwrap()
                .as_ref()
                .map(|session| session.id),
            Some(sibling)
        );
        assert_eq!(live(&app), before);
        assert_eq!(app.frontmost_window, Some(wid));
        assert_eq!(app.native_config_service.snapshot(), config);
        assert!(app.structural_invariants_ok());
    }
}

#[cfg(unix)]
#[test]
fn session_menu_rows_bind_each_shared_view_and_drop_a_closed_original() {
    use crate::session_chrome::TabMenuEntry;

    let (mut app, wid, source, source_ctx) = app_with_stub();
    app.tab_strip_rows = 1;
    let sibling = app.split_active_stub_tab(wid);
    let sibling_ctx = app.pool.get(sibling).unwrap().ctx.clone();
    crate::term_lock(app.pool.get(source).unwrap().live_term())
        .process(b"\x1b]7;file://localhost/tmp/menu-source\x07");
    crate::term_lock(app.pool.get(sibling).unwrap().live_term())
        .process(b"\x1b]7;file://localhost/tmp/menu-sibling\x07");
    assert!(app.take_over_session(source).is_ok());
    assert!(app.take_over_session(sibling).is_ok());
    assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Left));
    let tab = app.windows[&wid].tab_set.active_id().unwrap();
    let source_view = app.windows[&wid].tab_set.active().unwrap().focus;
    let mirror = app.open_active_session_in_new_window_logical().unwrap();
    let mirror_view = app.windows[&mirror].tab_set.active().unwrap().focus;
    assert_ne!(source_view, mirror_view);
    assert_eq!(app.pool.views(source), Some(2));
    app.window_focus_event(mirror, false);
    app.window_focus_event(wid, true);
    assert!(app.open_active_tab_context_menu(wid));
    let (row, origin) = captured_session_row(&app, wid, MenuAction::HandBack);
    assert_eq!((origin.view, origin.session), (source_view, source));
    let source_index = app.windows[&wid].tab_set.active_index().unwrap();
    assert_eq!(
        app.terminal_menu_origin_index(wid, tab, origin),
        Some(source_index)
    );
    let origins = |entries: &[TabMenuEntry]| {
        entries
            .iter()
            .filter_map(|entry| match entry {
                TabMenuEntry::SessionAction { origin, .. } => Some(*origin),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let source_titles = app.tab_titles(wid);
    let source_ext = app.tab_chrome_ext(wid, &source_titles);
    let source_owners = origins(&source_ext[source_index].menu);
    assert!(!source_owners.is_empty());
    assert!(source_owners.iter().all(|owner| *owner == Some(origin)));
    let mirror_titles = app.tab_titles(mirror);
    let mirror_ext = app.tab_chrome_ext(mirror, &mirror_titles);
    let mirror_owners = origins(&mirror_ext[0].menu);
    assert_eq!(mirror_owners.len(), source_owners.len());
    assert!(mirror_owners.iter().all(|owner| {
        owner.is_some_and(|owner| owner.view == mirror_view && owner.session == source)
    }));
    let cached = origins(&app.session_chrome[&source].ext.menu);
    assert_eq!(cached.len(), source_owners.len());
    assert!(cached.iter().all(Option::is_none));
    let clipboard_rows = source_ext[source_index].menu.iter().filter(|entry| matches!(entry,
        TabMenuEntry::SessionAction { action: MenuAction::CopySessionId | MenuAction::CopyCwd, enabled: true, origin: Some(owner), .. } if *owner == origin
    )).count();
    assert_eq!(
        clipboard_rows, 2,
        "both clipboard rows carry the captured target without executing a clipboard write"
    );
    assert_eq!(
        app.session_id_text(origin.session).as_deref(),
        Some(source_ctx.self_id.as_str())
    );
    assert_eq!(
        app.session_cwd(origin.session).as_deref(),
        Some("/tmp/menu-source")
    );
    assert_eq!(
        app.session_cwd(sibling).as_deref(),
        Some("/tmp/menu-sibling")
    );
    assert_ne!(
        app.session_id_text(origin.session),
        app.session_id_text(sibling)
    );

    assert_eq!(app.close_active_tab_in(wid), None);
    assert!(!app.view_store.contains(source_view));
    assert!(app.view_store.contains(mirror_view));
    assert_eq!(app.pool.views(source), Some(1));
    assert_eq!(app.windows[&wid].tab_set.active_id(), Some(tab));
    assert_eq!(app.focused_session_id(wid), Some(sibling));
    assert_eq!(app.terminal_menu_origin_index(wid, tab, origin), None);
    for window in [wid, mirror] {
        assert!(app.begin_active_session_rename(window));
        assert!(app.rename_field_edit(
            window,
            crate::app_search::SearchEdit::Insert("unsubmitted shared-view draft".into())
        ));
    }
    let at = std::time::Instant::now();
    for window in app.windows.values_mut() {
        window.fx.cursor_mut().glow.note_typed_cells(at, 1);
        window.fx.cursor_mut().trail.note_typed(at);
        assert!(window.fx.cursor().glow.move_licensed(at));
        assert!(window.fx.cursor().trail.move_licensed(at));
    }
    let windows = |app: &App| {
        app.windows
            .iter()
            .map(|(id, window)| {
                (
                    *id,
                    (
                        window.tab_set.clone(),
                        window.layouts.clone(),
                        window.front_content,
                        window.rename_edit.clone(),
                        window.fx.cursor().glow.move_licensed(at),
                        window.fx.cursor().trail.move_licensed(at),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let sessions = |app: &App| {
        app.pool
            .iter()
            .map(|session| {
                let terminal = crate::term_lock(session.live_term());
                let metadata = session.ctx.meta.lock().unwrap();
                (
                    session.id,
                    (
                        app.pool.views(session.id),
                        terminal.visible_content(),
                        terminal.content_seq(),
                        session.ctx.sink.input_epoch(),
                        metadata.presentation_value("title"),
                        metadata.presentation_value("role"),
                        session.ctx.fabric.holds().person().map(str::to_string),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let before_windows = windows(&app);
    let before_sessions = sessions(&app);
    let focus_order = app.focus_order.clone();
    let config = app.native_config_service.snapshot();

    app.with_gesture_source(false, |app| app.activate_tab_menu_entry(wid, row));

    assert!(app.windows[&wid].tab_menu.is_none());
    assert_eq!(windows(&app), before_windows);
    assert_eq!(sessions(&app), before_sessions);
    assert_eq!(
        source_ctx.fabric.holds().person(),
        Some(crate::fabric::TAKEN_OVER)
    );
    assert_eq!(
        sibling_ctx.fabric.holds().person(),
        Some(crate::fabric::TAKEN_OVER)
    );
    assert_eq!(app.frontmost_window, Some(wid));
    assert_eq!(app.focus_order, focus_order);
    assert_eq!(
        app.active_handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|session| session.id),
        Some(sibling)
    );
    assert_eq!(app.native_config_service.snapshot(), config);
    assert!(app.structural_invariants_ok());
}

#[cfg(unix)]
#[test]
fn a_whole_tab_menu_close_still_targets_the_tab_after_its_original_pane_closed() {
    use crate::session_chrome::TabMenuEntry;

    let (mut app, wid, _source, _source_ctx) = app_with_stub();
    app.tab_strip_rows = 1;
    let sibling = app.split_active_stub_tab(wid);
    assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Left));
    let tab = app.windows[&wid].tab_set.active_id().unwrap();
    let source_view = app.windows[&wid].tab_set.active().unwrap().focus;
    assert!(app.open_active_tab_context_menu(wid));
    let menu = app.windows[&wid].tab_menu.as_ref().unwrap();
    let close = menu
        .entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                TabMenuEntry::Action {
                    action: MenuAction::CloseTab,
                    enabled: true,
                    ..
                }
            )
        })
        .expect("whole-tab Close is independent of a terminal owner");
    assert!(crate::tab_menu::is_selectable(&menu.entries[close]));
    assert_eq!(app.close_active_tab_in(wid), None);
    assert!(!app.view_store.contains(source_view));
    assert_eq!(app.windows[&wid].tab_set.active_id(), Some(tab));
    assert_eq!(app.focused_session_id(wid), Some(sibling));
    let retained = app.windows[&wid].tab_menu.as_ref().unwrap();
    assert_eq!(retained.tab, tab);
    assert!(matches!(
        retained.entries[close],
        TabMenuEntry::Action {
            action: MenuAction::CloseTab,
            enabled: true,
            ..
        }
    ));
    assert!(app.close_tab_menu(wid));
    let terminal = app.pool.get(0).unwrap().live_term().clone();
    let untouched = {
        let term = crate::term_lock(&terminal);
        (
            term.visible_content(),
            app.pool.get(0).unwrap().ctx.sink.input_epoch(),
        )
    };
    let config = app.native_config_service.snapshot();

    let index = app
        .prepare_tab_menu_action(wid, tab, MenuAction::CloseTab)
        .expect("the whole tab still exists after its original pane closed");
    assert!(
        !app.close_tab_at(wid, index),
        "the other terminal tab keeps the window open"
    );

    assert!(app.windows[&wid].tab_set.get(tab).is_none());
    assert!(app.pool.get(sibling).is_none());
    assert_eq!(app.focused_session_id(wid), Some(0));
    assert_eq!(app.frontmost_window, Some(wid));
    assert_eq!(
        {
            let term = crate::term_lock(&terminal);
            (
                term.visible_content(),
                app.pool.get(0).unwrap().ctx.sink.input_epoch(),
            )
        },
        untouched
    );
    assert_eq!(app.native_config_service.snapshot(), config);
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_take_over_under_a_committing_update_answers_at_once() {
    let (mut app, wid, _sid, ctx) = app_with_stub();
    let _ = ctx.fabric.fence_hold(true);
    let blocks = crate::fabric::fence_blocks_on_this_thread();
    app.take_over(wid);
    assert_eq!(crate::fabric::fence_blocks_on_this_thread(), blocks);
    assert!(ctx.fabric.holds().person().is_none(), "nothing moved");
    assert!(
        app.messages
            .live_by_key(crate::message_reporters::KEY_FABRIC_HOLD)
            .is_some(),
        "the refusal is said"
    );
    ctx.fabric.lift_hold_fence();
    app.take_over(wid);
    assert!(ctx.fabric.holds().person().is_some());
}

fn set_hold_clock(unix_s: Option<u64>) {
    HOLD_UNIX_CLOCK_FOR_TEST.with(|c| c.set(unix_s));
}

#[test]
fn a_long_hold_is_one_entry_and_hand_back_sweeps_it() {
    let (mut app, wid, _sid, ctx) = app_with_stub();
    let prefix = aterm_messages::exceptions::Family::LongHold.prefix();
    let named = |app: &App| {
        app.messages
            .digest()
            .entries
            .iter()
            .filter(|e| e.key.starts_with(&prefix))
            .count()
    };
    let t0 = 1_800_000_000;
    set_hold_clock(Some(t0));
    app.take_over(wid);
    app.hand_long_holds();
    assert_eq!(named(&app), 0, "a fresh hold names none");
    assert!(app.long_hold_deadline().is_some());
    set_hold_clock(Some(t0 + LONG_HOLD.as_secs() + 1));
    app.hand_long_holds();
    assert_eq!(named(&app), 1, "past the mark: one entry");
    assert_eq!(
        app.agents.long_holds_due,
        Some(t0 + LONG_HOLD.as_secs() + 1 + LONG_HOLD_RESTATE.as_secs()),
        "the next wake composes its words again"
    );
    assert!(ctx.fabric.holds().person().is_some());
    app.hand_back(wid);
    set_hold_clock(None);
    if let Some(due) = app.messages_deadline() {
        let _ = app.settle_messages(due + Duration::from_secs(1));
    }
    assert_eq!(named(&app), 0, "Hand Back sweeps it");
}

#[test]
fn a_clients_long_hold_is_named_and_hand_back_sweeps_it() {
    let (mut app, wid, _sid, ctx) = app_with_stub();
    let prefix = aterm_messages::exceptions::Family::LongHold.prefix();
    let named = |app: &App| -> Vec<String> {
        app.messages
            .digest()
            .entries
            .iter()
            .filter(|e| e.key.starts_with(&prefix))
            .map(|e| e.key.clone())
            .collect()
    };
    let t0 = 1_800_000_000;
    set_hold_clock(Some(t0));
    let on = format!("{} on reason=review", ctx.self_id.as_str());
    assert_eq!(
        crate::fabric::cmd_hold(&app.store, &on, crate::fabric::HoldIssuer::Owner),
        "OK hold=1\n"
    );
    app.refresh_presence_window(wid);
    app.hand_long_holds();
    assert!(named(&app).is_empty(), "a fresh hold names none");
    assert!(app.long_hold_deadline().is_some(), "its mark is armed");
    set_hold_clock(Some(t0 + LONG_HOLD.as_secs() + 1));
    app.hand_long_holds();
    let keys = named(&app);
    assert_eq!(keys.len(), 1, "past the mark: one entry ({keys:?})");
    assert!(keys[0].ends_with(".client-held"), "{keys:?}");
    assert!(app.person_holds().is_empty(), "no hold of the person's");
    app.hand_back(wid);
    assert_eq!(ctx.fabric.holds().origins(), "-", "Hand Back lifted it");
    set_hold_clock(None);
    if let Some(due) = app.messages_deadline() {
        let _ = app.settle_messages(due + Duration::from_secs(1));
    }
    assert!(named(&app).is_empty(), "Hand Back sweeps it");
    set_hold_clock(Some(t0));
    let fleet = format!("{} on origin=fleet reason=bus", ctx.self_id.as_str());
    assert_eq!(
        crate::fabric::cmd_hold(&app.store, &fleet, crate::fabric::HoldIssuer::Bridge),
        "OK hold=1\n"
    );
    app.refresh_presence_window(wid);
    set_hold_clock(Some(t0 + 2 * LONG_HOLD.as_secs()));
    app.hand_long_holds();
    assert!(named(&app).is_empty(), "a fleet hold is not named");
    set_hold_clock(None);
}

#[test]
fn a_standing_long_hold_says_its_refusals_as_they_move() {
    let (mut app, wid, _sid, ctx) = app_with_stub();
    let t0 = 1_800_000_000;
    set_hold_clock(Some(t0));
    app.take_over(wid);
    let past = t0 + LONG_HOLD.as_secs() + 1;
    set_hold_clock(Some(past));
    app.hand_long_holds();
    let refused = |app: &App| {
        app.agents
            .long_holds_named
            .as_ref()
            .and_then(|w| w.first())
            .map(|w| w.refused)
    };
    assert_eq!(refused(&app), Some(0));
    assert!(crate::fabric::halt_refusal(&ctx, "send").is_some());
    assert_eq!(ctx.fabric.person_refusals(), 1);
    let walks = app.agents.long_hold_walks;
    set_hold_clock(Some(past + 30));
    app.hand_long_holds();
    assert_eq!(
        app.agents.long_hold_walks, walks,
        "inside the minute: no walk"
    );
    assert_eq!(refused(&app), Some(0));
    set_hold_clock(Some(past + LONG_HOLD_RESTATE.as_secs()));
    app.hand_long_holds();
    assert_eq!(app.agents.long_hold_walks, walks + 1, "the minute's walk");
    assert_eq!(refused(&app), Some(1), "the refusal is said");
    let said = app.agents.long_holds_named.clone();
    set_hold_clock(Some(past + 2 * LONG_HOLD_RESTATE.as_secs()));
    app.hand_long_holds();
    assert_eq!(app.agents.long_hold_walks, walks + 2);
    assert_eq!(app.agents.long_holds_named, said);
    set_hold_clock(None);
}

#[test]
fn a_look_where_no_hold_moved_walks_none() {
    let (mut app, wid, sid, _ctx) = app_with_stub();
    let t0 = 1_800_000_000;
    set_hold_clock(Some(t0));
    app.hand_long_holds();
    let first = app.agents.long_hold_walks;
    assert!(first >= 1, "the first look walks");
    for _ in 0..3 {
        app.hand_long_holds();
    }
    assert_eq!(app.agents.long_hold_walks, first, "quiet looks walk none");
    app.take_over(wid);
    let taken = app.agents.long_hold_walks;
    assert!(taken > first, "a Take Over walks");
    app.hand_long_holds();
    assert_eq!(app.agents.long_hold_walks, taken, "and the next look not");
    set_hold_clock(Some(t0 + LONG_HOLD.as_secs()));
    app.hand_long_holds();
    assert_eq!(app.agents.long_hold_walks, taken + 1, "the mark passed");
    app.restate_long_holds();
    app.hand_long_holds();
    assert_eq!(app.agents.long_hold_walks, taken + 2, "the settle restates");
    let tab = app.windows[&wid].tab_set.active_index().expect("a tab");
    let _ = app.close_tab_at(wid, tab);
    assert!(app.pool.get(sid).is_none(), "the held tab closed");
    app.hand_long_holds();
    assert_eq!(app.agents.long_hold_walks, taken + 3, "a held tab left");
    set_hold_clock(None);
}

#[test]
fn the_agents_live_bits_are_published_where_they_can_move() {
    use aterm_types::needs::NeedWord;
    let before = crate::menu::agents_live();
    let (mut app, _wid, _sid, ctx) = app_with_stub();
    let set_need = |need: Option<crate::needs::Need>| {
        let _ = ctx
            .timeline
            .lock()
            .unwrap()
            .set_need(crate::needs::Verdict {
                need,
                ..crate::needs::Verdict::default()
            });
    };
    let now = Instant::now();
    app.publish_agents_live(false, now);
    assert!(!crate::menu::agents_live().needs_local, "the first publish");
    set_need(Some(crate::needs::Need {
        word: NeedWord::Ask,
        key: 1,
        what: String::new(),
        harness: None,
        report: None,
    }));
    app.publish_agents_live(false, now + Duration::from_millis(500));
    assert!(
        !crate::menu::agents_live().needs_local,
        "a quiet turn within the bound re-reads nothing"
    );
    app.publish_agents_live(true, now + Duration::from_millis(500));
    assert!(
        crate::menu::agents_live().needs_local,
        "a rebuilt glance publishes"
    );
    set_need(None);
    app.publish_agents_live(false, now + Duration::from_millis(600));
    assert!(
        crate::menu::agents_live().needs_local,
        "still within the bound"
    );
    app.publish_agents_live(false, now + AGENTS_LIVE_TTL + Duration::from_secs(1));
    assert!(
        !crate::menu::agents_live().needs_local,
        "past the bound it re-reads"
    );
    crate::menu::set_agents_live(before);
}

#[test]
fn a_long_hold_entry_an_earlier_process_left_ends_where_no_hold_here_is_long() {
    use aterm_messages::exceptions::Family;
    let prefix = Family::LongHold.prefix();
    let keys = |app: &App| -> Vec<String> {
        app.messages
            .digest()
            .entries
            .iter()
            .filter(|e| e.key.starts_with(&prefix))
            .map(|e| e.key.clone())
            .collect()
    };
    let settle = |app: &mut App| {
        let due = app.messages.digest_deadline().expect("an end is due");
        let _ = app.settle_messages(due + Duration::from_secs(1));
    };
    let seed = |app: &mut App| {
        let earlier = crate::message_reporters::long_hold_entry("s-earlier", "bench", 30, None);
        app.except_replace(Family::LongHold, &[earlier]);
        assert_eq!(keys(app).len(), 1, "the earlier process's entry");
    };

    let (mut app, _wid, _sid, _ctx) = app_with_stub();
    app.agents.long_holds_named = None;
    seed(&mut app);
    app.ripen_exceptions(Instant::now());
    assert!(
        app.messages.digest_deadline().is_some(),
        "the first look's empty answer started the entry's end"
    );
    settle(&mut app);
    assert!(keys(&app).is_empty(), "swept");

    let (mut app, _wid, _sid, _ctx) = app_with_stub();
    let now = Instant::now();
    app.ripen_exceptions(now);
    seed(&mut app);
    app.ripen_exceptions(now + Duration::from_secs(1));
    assert!(
        app.messages.digest_deadline().is_none(),
        "an unchanged answer is not stated before the settle"
    );
    app.ripen_exceptions(now + crate::exceptions_host::RESUME_SETTLE + Duration::from_secs(1));
    settle(&mut app);
    assert!(keys(&app).is_empty(), "swept at the settle");

    let (mut app, wid, _sid, ctx) = app_with_stub();
    app.agents.long_holds_named = None;
    seed(&mut app);
    let t0 = 1_800_000_000;
    set_hold_clock(Some(t0));
    app.take_over(wid);
    set_hold_clock(Some(t0 + LONG_HOLD.as_secs() + 1));
    let now = Instant::now();
    app.ripen_exceptions(now + LONG_HOLD + Duration::from_secs(1));
    set_hold_clock(None);
    let _ = app.settle_messages(
        now + LONG_HOLD + aterm_messages::need::RESOLVE_AFTER + Duration::from_secs(2),
    );
    let mine = crate::message_reporters::long_hold_entry(ctx.self_id.as_str(), "-", 30, None).key();
    assert_eq!(keys(&app), [mine], "this tab's entry, the earlier one gone");
}

#[test]
fn set_role_writes_the_role_through_the_pins_editor() {
    let (mut app, wid, sid, ctx) = app_with_stub();
    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_meta_edit(wid, MetaField::Role));
    let edit = app
        .windows
        .get(&wid)
        .and_then(|ws| ws.rename_edit.as_ref())
        .expect("an edit is live");
    assert_eq!(edit.field, MetaField::Role);
    assert_eq!(edit.session, sid);
    app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("manager".into()));
    app.commit_session_rename(wid, sid, "manager");
    let meta = ctx.meta.lock().unwrap();
    assert_eq!(meta.role.as_deref(), Some("manager"));
    assert_eq!(meta.user_title, None, "the title pin is untouched");
    drop(meta);
    assert_eq!(
        app.tab_label_of_session(sid),
        "manager",
        "the hovers name the tab by its role"
    );
    assert!(app.windows.get(&wid).unwrap().rename_edit.is_none());
    assert_eq!(
        crate::menu::MenuAction::SetRole.invoke_authority(),
        crate::menu::InvokeAuthority::WriteInput
    );
}

#[test]
fn the_cli_rows_compose_their_argv_and_run_nothing_headless() {
    let (mut app, wid, _sid, _ctx) = app_with_stub();
    for verb in [
        CliVerb::ReadThisMac,
        CliVerb::ReadFleet,
        CliVerb::FleetPeers,
        CliVerb::BusStatus,
        CliVerb::BusOn,
        CliVerb::BusOff,
    ] {
        app.run_cli_to_tab(wid, verb);
        let plan = app.last_cli_plan.clone().expect("a plan");
        assert_eq!(plan.verb, verb);
        assert!(!plan.out.exists(), "headless writes nothing");
        let first = plan.argv.first().map(String::as_str);
        match verb {
            CliVerb::ReadThisMac | CliVerb::ReadFleet => {
                assert_eq!(plan.argv[..2], ["ctl", "survey"]);
                assert!(!plan.argv.contains(&"--sock".to_string()), "the whole Mac");
                assert_eq!(
                    plan.argv.contains(&"--fleet".to_string()),
                    verb == CliVerb::ReadFleet
                );
            }
            CliVerb::FleetPeers => assert_eq!(plan.argv, ["fleet", "peers"]),
            CliVerb::BusStatus | CliVerb::BusOn | CliVerb::BusOff => {
                assert_eq!(first, Some("fabric"));
            }
            CliVerb::ReadPeer | CliVerb::DrivePeer => unreachable!("not a row's verb"),
        }
        assert_eq!(
            verb.confirmation().is_some(),
            matches!(verb, CliVerb::BusOn | CliVerb::BusOff),
            "{verb:?}"
        );
    }
    assert!(app.last_menu_document.is_none());
}

#[test]
fn the_cli_tab_fences_its_output() {
    let plan = CliPlan::peer(
        ["ctl", "survey", "--host", "mac-17"]
            .map(String::from)
            .to_vec(),
        "mac-17".to_string(),
    );
    let text = cli_markdown(&plan, &Ok(Some(1)), "a ``` b\n", "");
    assert!(
        text.starts_with("# aterm ctl survey --host mac-17\n"),
        "{text}"
    );
    assert!(text.contains("````text\na ``` b\n````"), "{text}");
    assert_eq!(text.matches("````text").count(), 1);
    assert_eq!(
        cli_markdown(&plan, &Ok(Some(1)), "a ``` b\n", "e")
            .matches("text\n")
            .count(),
        2
    );
    assert!(!cli_markdown(&plan, &Err("x".to_string()), "", "").contains("```"));
}

#[test]
fn a_take_over_on_another_mac_says_how_to_hand_it_back() {
    let plan = |way: &str| {
        CliPlan::driven(
            ["ctl", "--host", "mac-17.local", "hold", "s-ab12", way]
                .map(String::from)
                .to_vec(),
            "Take Over on mac-17 \u{b7} s-ab12".to_string(),
        )
    };
    let took = cli_markdown(&plan("on"), &Ok(Some(0)), "OK hold=1\n", "");
    assert!(
        took.contains("aterm ctl --host mac-17.local hold s-ab12 off"),
        "{took}"
    );
    assert!(took.contains("@s-ab12 send"), "{took}");
    let refused = cli_markdown(&plan("on"), &Ok(Some(1)), "", "ERR denied\n");
    assert!(!refused.contains("hold s-ab12 off"), "{refused}");
    let back = cli_markdown(&plan("off"), &Ok(Some(0)), "OK hold=0\n", "");
    assert!(!back.contains("hold s-ab12 off`"), "{back}");
}

#[test]
fn the_menu_bars_reads_raise_their_window_and_nothing_else_does() {
    use crate::native_app::AppKind;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-status-read-raise")
        .tempdir()
        .expect("scratch dir");
    let file = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("# {name}\n")).unwrap();
        path
    };
    let mut app = App::headless_for_test();
    let wid = app.frontmost_window.expect("the test App's window");
    let _ = crate::app_tabs::take_raised_windows();

    let opened = crate::native_document_host::path_to_file_uri(&file("open.md")).unwrap();
    app.open_document_tab(AppKind::Markdown, &opened)
        .expect("app open opens its tab");
    assert_eq!(crate::app_tabs::take_raised_windows(), Vec::new());

    let ended = |plan: CliPlan| CliTabOutcome {
        wid,
        plan,
        status: Ok(Some(0)),
    };
    let mut row = CliPlan::new(CliVerb::ReadThisMac);
    row.out = file("row.md");
    app.complete_cli_tab(ended(row));
    assert_eq!(
        crate::app_tabs::take_raised_windows(),
        Vec::new(),
        "an Agents row's read raises nothing"
    );
    let mut read = CliPlan::new(CliVerb::ReadThisMac).raised();
    read.out = file("read.md");
    app.complete_cli_tab(ended(read));
    assert_eq!(crate::app_tabs::take_raised_windows(), vec![wid]);

    let result = |path| crate::status_item::ResultRef {
        session: 0,
        entered: 7,
        end_serial: None,
        path,
        label: "lint".to_string(),
    };
    app.read_status_result(&result(dir.path().join("missing.md")));
    assert_eq!(crate::app_tabs::take_raised_windows(), Vec::new());
    assert_eq!(app.done_seen.get(&0), None, "nothing opened: not seen");
    app.read_status_result(&result(file("result.md")));
    assert_eq!(crate::app_tabs::take_raised_windows(), vec![wid]);
    assert_eq!(app.done_seen.get(&0), Some(&7));

    let entry = app.post_message(aterm_messages::Message::new(
        aterm_messages::tags::CONFIG,
        aterm_messages::Severity::Warn,
        "to look at",
    ));
    assert!(app.show_status_exception(entry.raw()));
    assert_eq!(crate::app_tabs::take_raised_windows(), vec![wid]);
}

#[test]
fn next_need_walks_the_needs_stuck_first_and_drops_a_resolved_one() {
    use aterm_types::needs::{NeedWord, Severity};
    let stop = |word: NeedWord, since: i64, session: u64, key: u64| NeedStop {
        severity: word.severity(),
        carries_on: false,
        since,
        label: String::new(),
        session,
        key,
    };
    let stops = [
        stop(NeedWord::Approval, 300, 1, 10),
        stop(NeedWord::Frozen, 500, 2, 20),
        stop(NeedWord::Ask, 100, 3, 30),
    ];
    assert_eq!(stops[1].severity, Severity::Stuck);
    let first = pick_next_need(&stops, None).expect("one");
    assert_eq!(first.session, 2, "stuck first, however young");
    let second = pick_next_need(&stops, Some((2, 20))).expect("two");
    assert_eq!(
        second.session, 3,
        "then the oldest decide, whatever its word"
    );
    let third = pick_next_need(&stops, Some((3, 30))).expect("three");
    assert_eq!(third.session, 1);
    assert_eq!(
        pick_next_need(&stops, Some((1, 10))).map(|s| s.session),
        Some(2)
    );
    assert_eq!(
        pick_next_need(&stops, Some((2, 99))).map(|s| s.session),
        Some(2)
    );
    assert_eq!(pick_next_need(&[], None), None);

    let (mut app, wid, sid, ctx) = app_with_stub();
    assert!(app.next_need().is_err(), "nothing needs you");
    let _ = ctx
        .timeline
        .lock()
        .unwrap()
        .set_need(crate::needs::Verdict {
            need: Some(crate::needs::Need {
                word: NeedWord::Ask,
                key: 7,
                what: String::new(),
                harness: None,
                report: None,
            }),
            ..crate::needs::Verdict::default()
        });
    assert_eq!(app.local_needs().len(), 1);
    assert_eq!(app.next_need(), Ok(()));
    assert_eq!(app.agents.next_need, Some((sid, 7)));
    let _ = wid;
}

#[test]
fn next_need_starts_where_the_status_items_needs_start() {
    use aterm_types::needs::NeedWord;
    let (mut app, wid, first, first_ctx) = app_with_stub();
    let second = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(second));
    let second_ctx = app.pool.get(second).expect("pooled").ctx.clone();
    for (ctx, key) in [(&first_ctx, 1), (&second_ctx, 2)] {
        let _ = ctx
            .timeline
            .lock()
            .unwrap()
            .set_need(crate::needs::Verdict {
                need: Some(crate::needs::Need {
                    word: NeedWord::Ask,
                    key,
                    what: "approve?".to_string(),
                    harness: None,
                    report: None,
                }),
                ..crate::needs::Verdict::default()
            });
    }
    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
    for (session, at) in [(second, now - 3600), (first, now)] {
        let handle = app
            .store
            .read()
            .unwrap()
            .by_local(session)
            .cloned()
            .expect("registered");
        let row = app.status_session_row(&handle);
        let _ = app.presence.need_ages.observe(session, Some(&row), at);
    }
    assert!(
        first < second,
        "control: the session order picks the earlier tab"
    );
    let listed = app.status_glance().local_need_sessions();
    assert_eq!(listed, vec![second, first], "❯ lists the older first");
    assert_eq!(app.next_need(), Ok(()));
    assert_eq!(app.agents.next_need.map(|(s, _)| s), Some(listed[0]));
    assert_eq!(app.next_need(), Ok(()));
    assert_eq!(app.agents.next_need.map(|(s, _)| s), Some(listed[1]));
}

/// STOPPED WORK FIRST (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.7): an hour-old
/// `NEEDS YOU:` report its agent carries on past comes after a box that has just stopped
/// another tab's agent, on ❯ and on Next That Needs You (⇧⌘J) alike, which starts where ❯
/// starts.
#[test]
fn next_need_starts_at_a_stopped_need_before_an_older_report() {
    use aterm_types::needs::NeedWord;
    let (mut app, wid, first, first_ctx) = app_with_stub();
    let second = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(second));
    let second_ctx = app.pool.get(second).expect("pooled").ctx.clone();
    let report = crate::needs::HarnessAsk::told(
        &["the release permission to sign 0.48.0".to_string()],
        aterm_phase::Program::Claude,
    );
    assert!(report.is_some(), "a report");
    for (ctx, key, harness) in [(&first_ctx, 1, report), (&second_ctx, 2, None)] {
        let _ = ctx
            .timeline
            .lock()
            .unwrap()
            .set_need(crate::needs::Verdict {
                need: Some(crate::needs::Need {
                    word: NeedWord::Ask,
                    key,
                    what: "approve?".to_string(),
                    harness,
                    report: None,
                }),
                ..crate::needs::Verdict::default()
            });
    }
    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
    for (session, at) in [(first, now - 3 * 3600), (second, now - 120)] {
        let handle = app
            .store
            .read()
            .unwrap()
            .by_local(session)
            .cloned()
            .expect("registered");
        let row = app.status_session_row(&handle);
        let _ = app.presence.need_ages.observe(session, Some(&row), at);
    }
    let stops = app.local_needs();
    let stop = |s: u64| stops.iter().find(|n| n.session == s).cloned();
    assert_eq!(
        (
            stop(first).map(|n| n.carries_on),
            stop(second).map(|n| n.carries_on)
        ),
        (Some(true), Some(false)),
        "the report carries on, the box stopped its agent"
    );
    assert!(
        stop(first).map(|n| n.since) < stop(second).map(|n| n.since),
        "control: the report is the older"
    );
    let listed = app.status_glance().local_need_sessions();
    assert_eq!(
        listed,
        vec![second, first],
        "❯ lists the stopped box before the older report"
    );
    assert_eq!(app.next_need(), Ok(()));
    assert_eq!(app.agents.next_need.map(|(s, _)| s), Some(second));
    assert_eq!(app.next_need(), Ok(()));
    assert_eq!(app.agents.next_need.map(|(s, _)| s), Some(first));
}

#[test]
fn the_agent_facts_name_the_manager_and_its_workers() {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let (mut app, wid, manager, manager_ctx) = app_with_stub();
    let worker = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(worker));
    let worker_ctx = app.pool.get(worker).expect("pooled").ctx.clone();
    let mark = TaskMark {
        from: Some(manager_ctx.self_id.as_str().to_string()),
        ..TaskMark::new(TaskState::Working)
    };
    mark_session_task(&worker_ctx, worker, &mark).expect("unfenced");
    app.on_task_changed(worker);
    let of_worker = app.agent_menu_facts(worker);
    assert_eq!(
        of_worker.manager.as_ref().map(|(sid, _)| sid.clone()),
        Some(manager_ctx.self_id.clone())
    );
    assert!(of_worker.stoppable);
    assert!(of_worker.workers.is_empty());
    let of_manager = app.agent_menu_facts(manager);
    assert_eq!(of_manager.manager, None);
    assert!(!of_manager.stoppable, "no task of its own");
    assert_eq!(
        of_manager
            .workers
            .iter()
            .map(|(sid, _)| sid)
            .collect::<Vec<_>>(),
        [&worker_ctx.self_id]
    );
    assert!(!of_manager.person_hold);
    assert!(app.take_over_session(manager).is_ok());
    assert!(app.agent_menu_facts(manager).person_hold);
}

#[test]
fn a_background_worker_is_named_by_its_osc_title() {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let (mut app, wid, manager, manager_ctx) = app_with_stub();
    let worker = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(worker));
    let untitled = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(untitled));
    app.switch_tab_in(wid, 0);
    let osc = |app: &App, session: u64, title: &str| {
        let term = app.pool.get(session).expect("pooled").term.clone();
        crate::term_lock(&term).process(format!("\x1b]2;{title}\x07").as_bytes());
    };
    osc(&app, manager, "\u{2733} lab-sweep");
    osc(&app, worker, "\u{25d0} bench");
    for w in [worker, untitled] {
        let ctx = app.pool.get(w).expect("pooled").ctx.clone();
        let mark = TaskMark {
            from: Some(manager_ctx.self_id.as_str().to_string()),
            ..TaskMark::new(TaskState::Working)
        };
        mark_session_task(&ctx, w, &mark).expect("unfenced");
        app.on_task_changed(w);
    }
    let registry = |session: u64| {
        app.store
            .read()
            .unwrap()
            .by_local(session)
            .map(|h| h.title.clone())
    };
    assert_ne!(
        registry(worker).as_deref(),
        Some("bench"),
        "not the registry's"
    );
    assert_ne!(registry(manager).as_deref(), Some("lab-sweep"));

    assert_eq!(app.tab_label_of_session(worker), "bench");
    assert_eq!(app.tab_label_of_session(manager), "lab-sweep");
    let last = format!("tab {}", app.windows[&wid].tab_set.tabs().len());
    assert_eq!(app.tab_label_of_session(untitled), last);
    let of_worker = app.agent_menu_facts(worker);
    assert_eq!(
        of_worker.manager.map(|(_, label)| label).as_deref(),
        Some("lab-sweep")
    );
    let rows = app.agent_menu_facts(manager).workers;
    assert_eq!(rows.len(), 2);
    assert!(rows[0].1.contains(" bench "), "{rows:?}");
    assert!(rows[1].1.contains(&format!(" {last} ")), "{rows:?}");
    osc(&app, worker, "\u{25f4} bench");
    assert_eq!(app.tab_label_of_session(worker), "bench");
    let ctx = app.pool.get(worker).expect("pooled").ctx.clone();
    let pinned = ctx
        .meta
        .lock()
        .unwrap()
        .set("title", Some("\u{2826} deploy".to_string()));
    assert_eq!(pinned, Some(true), "the pin stored");
    assert_eq!(app.tab_label_of_session(worker), "\u{2826} deploy");
    app.pool
        .get(worker)
        .expect("pooled")
        .ctx
        .meta
        .lock()
        .unwrap()
        .role = Some("fmt".to_string());
    assert_eq!(app.tab_label_of_session(worker), "fmt");
}

/// A shell's title for the line it runs is that line without its control characters, or its
/// first 255 and `…` past 256, cut by characters or, in a C locale, by bytes. CONTROLS: a
/// title that is only the line's head with no `…`, a head and `…` of a line no shell cuts, a
/// title that runs past the line, and a program's own title are none; a line that ends in `…`
/// itself is its own title.
#[test]
fn a_title_is_its_lines_whole_or_cut_and_nothing_else() {
    let line = " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 'claude'";
    assert!(is_title_of_line(line, line));
    assert!(
        is_title_of_line("echo aecho b", "echo a\necho b"),
        "its newline dropped"
    );
    let long: String = std::iter::repeat_n("abc ", 80).collect();
    let cut: String = long.chars().take(255).chain(['\u{2026}']).collect();
    assert!(is_title_of_line(&cut, &long), "cut past 256");
    let wide: String = std::iter::repeat_n('a', 254)
        .chain("\u{e9}tude".chars())
        .collect();
    let bytes: String = String::from_utf8_lossy(&wide.as_bytes()[..255])
        .chars()
        .chain(['\u{2026}'])
        .collect();
    assert!(
        is_title_of_line(&bytes, &wide),
        "cut by bytes through its last character"
    );
    assert!(
        is_title_of_line("echo wait\u{2026}", "echo wait\u{2026}"),
        "a line that ends in `…` itself"
    );
    let head: String = long.chars().take(255).collect();
    assert!(
        !is_title_of_line(&head, &long),
        "CONTROL: a head with no `…`"
    );
    assert!(
        !is_title_of_line("claude\u{2026}", "claude --resume"),
        "CONTROL: a head and `…` of a line no shell cuts"
    );
    assert!(
        !is_title_of_line("e\u{2026}", "echo hello"),
        "CONTROL: a head and `…` of a line no shell cuts"
    );
    assert!(
        !is_title_of_line("make test all", "make test"),
        "CONTROL: past the line"
    );
    assert!(
        !is_title_of_line("\u{2733} Claude Code", line),
        "CONTROL: the program's own"
    );
    assert!(!is_title_of_line("", line), "CONTROL: no title");
}

#[test]
fn hwkey_is_refused_while_a_gateway_card_is_open() {
    let (mut app, wid, sid, _ctx) = app_with_stub();
    app.tab_strip_rows = 1;
    assert_eq!(app.hwkey_target(), Ok((0, false)), "no card open");
    app.palette_enter();
    assert!(app.hwkey_target().is_err(), "the palette");
    app.palette_exit();
    assert!(app.open_overview().is_ok());
    assert!(app.hwkey_target().is_err(), "the Overview");
    app.overview_exit(wid);
    assert!(app.open_active_tab_context_menu(wid));
    assert!(app.hwkey_target().is_err(), "a tab menu");
    app.close_tab_menu(wid);
    assert_eq!(app.hwkey_target(), Ok((0, false)), "closed again");

    assert!(!app.with_gesture_source(true, |app| app.controller_card_refused()));
    assert!(app.take_over_session(sid).is_ok());
    assert!(app.with_gesture_source(true, |app| app.controller_card_refused()));
    assert!(
        !app.controller_card_refused(),
        "the person's own chord opens it"
    );
}

/// `hwkey`'s hop refuses while System State is the front window's active view, where its
/// unmarked keys would count as a person's press. CONTROL: with the terminal tab in front
/// again it answers the window's number (`0` headless).
#[test]
fn hwkey_is_refused_while_system_state_is_in_front() {
    let (mut app, wid, _sid, _ctx) = app_with_stub();
    assert_eq!(app.hwkey_target(), Ok((0, false)), "a terminal in front");
    assert!(app.open_system_state_tab());
    assert!(app.system_state_active_in(wid));
    let refusal = app.hwkey_target().expect_err("System State in front");
    assert!(refusal.contains("System State"), "{refusal}");
    app.switch_tab_in(wid, 0);
    assert!(!app.system_state_active_in(wid));
    assert_eq!(
        app.hwkey_target(),
        Ok((0, false)),
        "the terminal tab in front again"
    );
}

#[test]
fn native_focus_never_reaches_its_observer_in_a_headless_app() {
    let (mut app, wid, _sid, _ctx) = app_with_stub();
    let unavailable = |app: &App| {
        assert_eq!(app.native_focus(), None);
        assert_eq!(
            app.native_focus_when_windowed(|| panic!("headless reached the native observer")),
            None
        );
    };
    unavailable(&app);
    app.palette_enter();
    assert!(app.gateway_card_open(wid));
    unavailable(&app);
    app.palette_exit();
    assert!(app.open_system_state_tab());
    assert!(app.system_state_active_in(wid));
    unavailable(&app);
}

#[test]
fn the_cards_go_to_manager_row_raises_the_manager() {
    use crate::session_chrome::{ConnVerb, TabMenuEntry};
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let (mut app, wid, _manager, manager_ctx) = app_with_stub();
    app.tab_strip_rows = 1;
    let manager_tab = app.windows[&wid].tab_set.active_index();
    let worker = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(worker));
    let worker_ctx = app.pool.get(worker).expect("pooled").ctx.clone();
    let mark = TaskMark {
        from: Some(manager_ctx.self_id.as_str().to_string()),
        ..TaskMark::new(TaskState::Working)
    };
    mark_session_task(&worker_ctx, worker, &mark).expect("unfenced");
    app.on_task_changed(worker);
    assert_ne!(
        app.windows[&wid].tab_set.active_index(),
        manager_tab,
        "the worker's tab is in front"
    );
    assert!(app.open_active_tab_context_menu(wid));
    let go = app.windows[&wid]
        .tab_menu
        .as_ref()
        .expect("the menu is open")
        .entries
        .iter()
        .position(|e| {
            matches!(
                e,
                TabMenuEntry::ConnectionAction { peer_sid, verb: ConnVerb::Show, .. }
                    if *peer_sid == manager_ctx.self_id
            )
        })
        .expect("the worker's menu names its manager");
    app.activate_tab_menu_entry(wid, go);
    assert_eq!(
        app.windows[&wid].tab_set.active_index(),
        manager_tab,
        "the manager's tab is in front"
    );
}
