// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::App;
use crate::WindowId;
use crate::app_introspect::AuxTarget;
use crate::input::InputEvent;
use crate::overlay::OverlayKind;

fn key(named: aterm_types::keyboard::NamedKey) -> InputEvent {
    InputEvent::Key {
        key: aterm_types::keyboard::Key::Named(named),
        mods: aterm_types::keyboard::Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    }
}

fn app_with_three() -> (App, WindowId, Vec<aterm_session::SessionId>) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.splice_tab_strip(wid);
    let sids = {
        let g = app.store.read().unwrap();
        let mut handles = g.snapshot();
        handles.sort_by_key(|h| h.local_id);
        handles.into_iter().map(|h| h.sid).collect()
    };
    (app, wid, sids)
}

fn ctx_of(app: &App, sid: &aterm_session::SessionId) -> std::sync::Arc<crate::SessionCtx> {
    app.store.read().unwrap().by_sid(sid).unwrap().ctx.clone()
}

fn local_of(app: &App, sid: &aterm_session::SessionId) -> u64 {
    app.store.read().unwrap().by_sid(sid).unwrap().local_id
}

fn give_task(app: &App, worker: &aterm_session::SessionId, from: &str) {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let ctx = ctx_of(app, worker);
    let mark = TaskMark {
        from: Some(from.to_string()),
        ..TaskMark::new(TaskState::Working)
    };
    mark_session_task(&ctx, local_of(app, worker), &mark).expect("unfenced");
}

fn chip_ordinal(app: &App, wid: WindowId, sid: &aterm_session::SessionId) -> usize {
    app.windows[&wid]
        .overview()
        .expect("open")
        .controls_lines()
        .iter()
        .filter(|l| l.contains(" chip ") || l.contains(" manages "))
        .position(|l| l.contains(&format!("sid={} ", sid.as_str())))
        .expect("the session is listed")
}

fn chip_line(app: &App, sid: &aterm_session::SessionId) -> String {
    app.read_aux_controls(AuxTarget::Overview)
        .into_iter()
        .find(|l| l.contains(" chip ") && l.contains(&format!("sid={} ", sid.as_str())))
        .expect("the chip")
}

#[test]
fn status_the_hover_and_the_card_name_who_typed() {
    use crate::typed_by::{Caller, Who};
    let (mut app, _wid, sids) = app_with_three();
    let (manager, worker) = (&sids[1], &sids[2]);
    ctx_of(&app, manager).meta.lock().unwrap().role = Some("lab-sweep".into());
    let local = local_of(&app, worker);
    let status = |app: &App| app.session_status_record(local).expect("live");
    assert!(status(&app).contains(" typed_by=- typed_ms=- "));
    assert!(app.presence_hover(local, None).is_none());

    let by = Caller::new(Who::Edge {
        src: manager.clone(),
        client: Box::new(Who::Word("a script".into())),
    });
    crate::typed_by::stamp(&ctx_of(&app, worker), by);
    app.on_typed(worker);
    assert!(
        status(&app).contains(" typed_by=a%20script%20on%20lab-sweep"),
        "{}",
        status(&app)
    );
    let hover = app.presence_hover(local, None).expect("the typist is said");
    assert!(
        hover.starts_with("last typed into by a script on lab-sweep's edge"),
        "{hover}"
    );
    assert!(app.open_overview().is_ok());
    let line = chip_line(&app, worker);
    assert!(
        line.contains(" typed_by=a%20script%20on%20lab-sweep"),
        "{line}"
    );
    assert!(chip_line(&app, manager).contains(" typed_by=- "));
}

#[test]
fn a_typist_tab_is_named_by_its_role_its_pin_or_its_id() {
    let (app, _wid, sids) = app_with_three();
    let tab = &sids[1];
    let local = local_of(&app, tab);
    let id = aterm_messages::presence::short_sid(tab.as_str());
    let named = |app: &App| {
        let named = app.typist_tab(local).expect("pooled");
        (named.words, named.facts)
    };
    let term = app.pool.get(local).expect("pooled").term.clone();
    crate::term_lock(&term).process(b"\x1b]2;aterm\x07");
    assert_eq!(app.tab_label_of_session(local), "aterm", "its label");
    assert_eq!(
        named(&app),
        (id.clone(), Some(id.clone())),
        "never its title"
    );
    let ctx = ctx_of(&app, tab);
    let pinned = ctx
        .meta
        .lock()
        .unwrap()
        .set("title", Some("acme-renewal".to_string()));
    assert_eq!(pinned, Some(true), "the pin stored");
    assert_eq!(
        named(&app),
        ("acme-renewal".to_string(), Some(id.clone())),
        "the person's name, which facts withhold"
    );
    ctx.meta.lock().unwrap().role = Some("aterm".to_string());
    assert_eq!(named(&app).0, "acme-renewal", "no role takes aterm's word");
    for posing in [
        "mac-03 over the fleet",
        "mac-03 (node 1a2b3c4d)  Over The  Fleet",
    ] {
        ctx.meta.lock().unwrap().role = Some(posing.to_string());
        assert_eq!(named(&app).0, "acme-renewal", "{posing}");
    }
    ctx.meta.lock().unwrap().role = Some("fleet-sweeper".to_string());
    assert_eq!(named(&app).0, "fleet-sweeper");
    ctx.meta.lock().unwrap().role = Some("lab-sweep".to_string());
    assert_eq!(
        named(&app),
        ("lab-sweep".to_string(), Some("lab-sweep".to_string()))
    );
}

#[test]
fn open_lists_sessions_workers_and_who_manages_them() {
    let (mut app, wid, sids) = app_with_three();
    give_task(&app, &sids[1], sids[0].as_str());
    give_task(&app, &sids[2], sids[0].as_str());
    assert_eq!(
        app.read_aux_controls(AuxTarget::Overview),
        vec!["overview open=false".to_string()]
    );
    assert!(app.open_overview().is_ok());
    assert_eq!(
        app.windows[&wid].overlay().map(|o| o.kind()),
        Some(OverlayKind::Overview)
    );
    assert_eq!(AuxTarget::parse("overview"), Some(AuxTarget::Overview));
    assert_eq!(AuxTarget::parse("connections"), Some(AuxTarget::Overview));
    assert_eq!(
        app.windows[&wid].overlay().unwrap().kind().keyword(),
        "overview"
    );
    let lines = app.read_aux_controls(AuxTarget::Overview);
    assert!(
        lines[0].contains("sessions=3")
            && lines[0].contains("workers=2")
            && lines[0].contains("manages=1"),
        "{lines:?}"
    );
    for worker in &sids[1..] {
        let line = chip_line(&app, worker);
        assert!(
            line.contains("task_state=working") && line.contains("driver=worker"),
            "{line}"
        );
        assert!(line.contains("stoppable=1"), "{line}");
    }
    assert!(
        lines.iter().any(|l| l.starts_with(&format!(
            "overview manages manager={} local={}",
            sids[0].as_str(),
            local_of(&app, &sids[0])
        )) && l.contains(sids[1].as_str())
            && l.contains(sids[2].as_str())),
        "{lines:?}"
    );
    assert!(app.close_aux_overlay(AuxTarget::Overview).is_ok());
    assert!(app.windows[&wid].overview().is_none());
}

#[test]
fn the_card_names_background_tabs_by_their_osc_titles() {
    use crate::overview::Driver;
    let (mut app, wid, sids) = app_with_three();
    app.switch_tab_in(wid, 0);
    for (sid, title) in [(&sids[0], "lab-sweep"), (&sids[1], "bench")] {
        let local = local_of(&app, sid);
        let term = app.pool.get(local).expect("pooled").term.clone();
        crate::term_lock(&term).process(format!("\x1b]2;{title}\x07").as_bytes());
    }
    give_task(&app, &sids[1], sids[0].as_str());
    give_task(&app, &sids[2], sids[0].as_str());
    let (groups, manages) = app.overview_parts();
    let chip = |sid: &aterm_session::SessionId| {
        groups
            .iter()
            .flat_map(|g| g.chips.iter())
            .find(|c| c.sid == *sid)
            .cloned()
            .expect("listed")
    };
    assert_eq!(chip(&sids[0]).label, "lab-sweep");
    assert_eq!(chip(&sids[1]).label, "bench");
    assert_eq!(
        chip(&sids[1]).driver,
        Some(Driver::WorkerOf("lab-sweep".to_string()))
    );
    assert_eq!(chip(&sids[2]).label, "tab 3", "the untitled tab");
    assert_eq!(manages.len(), 1);
    assert_eq!(manages[0].manager_label, "lab-sweep");
    assert_eq!(
        manages[0]
            .workers
            .iter()
            .map(|(_, label)| label.as_str())
            .collect::<Vec<_>>(),
        ["bench", "tab 3"]
    );
}

#[test]
fn enter_on_a_chip_raises_and_closes() {
    let (mut app, wid, sids) = app_with_three();
    app.switch_tab_in(wid, 0);
    assert!(app.open_overview().is_ok());
    let target = chip_ordinal(&app, wid, &sids[2]);
    app.overview_move(wid, target as isize);
    app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter));
    assert!(app.windows[&wid].overview().is_none(), "closed");
    assert_eq!(app.windows[&wid].tab_set.active_index(), Some(2));
}

#[test]
fn t_takes_the_selected_tab_over_and_a_controllers_t_does_not() {
    use crate::input::Source;
    let (mut app, wid, sids) = app_with_three();
    assert!(app.open_overview().is_ok());
    let target = chip_ordinal(&app, wid, &sids[1]);
    app.overview_move(wid, target as isize);
    let ctx = ctx_of(&app, &sids[1]);
    let t = InputEvent::Text("t".to_string());
    let _ = app.input_to_session(
        wid,
        t.clone(),
        Source::Controller,
        None,
        crate::app_input::PressPhase::Initial,
    );
    assert!(ctx.fabric.holds().person().is_none(), "a controller's t");
    app.overview_input_event(wid, &t);
    assert!(ctx.fabric.holds().person().is_some());
    let line = chip_line(&app, &sids[1]);
    assert!(
        line.contains("mark=yours") && line.contains("driver=person"),
        "{line}"
    );
    app.overview_input_event(wid, &t);
    assert!(ctx.fabric.holds().person().is_none(), "handed back");
}

#[test]
fn delete_confirms_inline_before_it_stops_a_worker() {
    let (mut app, wid, sids) = app_with_three();
    give_task(&app, &sids[1], crate::task_record::FROM_CTL);
    assert!(app.open_overview().is_ok());
    let quiet = chip_ordinal(&app, wid, &sids[2]);
    app.overview_move(wid, quiet as isize);
    app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
    assert!(app.read_aux_controls(AuxTarget::Overview)[0].contains("confirm=-"));
    let worker = chip_ordinal(&app, wid, &sids[1]);
    app.overview_move(wid, worker as isize - quiet as isize);
    app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
    assert!(app.read_aux_controls(AuxTarget::Overview)[0].contains(&format!("confirm={worker}")));
    app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Escape));
    assert!(app.windows[&wid].overview().is_some());
    assert!(app.read_aux_controls(AuxTarget::Overview)[0].contains("confirm=-"));
    app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
    app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter));
    assert!(app.windows[&wid].overview().is_none(), "⏎ went there");
    assert_eq!(app.agents.stop_signals, 0, "⏎ never confirms a stop");
    assert!(app.open_overview().is_ok());
    let worker = chip_ordinal(&app, wid, &sids[1]);
    app.overview_move(wid, worker as isize);
    app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
    app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
    assert_eq!(app.agents.stop_signals, 1, "the confirmed stop signals");
    assert!(
        matches!(
            app.agents.last_stop_refused,
            Some(crate::app_overview::StopRefused::Signal(_))
        ),
        "{:?}",
        app.agents.last_stop_refused
    );
    assert!(stop_row_on_glass(&app), "the person's failed stop is said");
    assert!(app.read_aux_controls(AuxTarget::Overview)[0].contains("confirm=-"));
}

fn stop_row_on_glass(app: &App) -> bool {
    app.messages.live_rows().any(|l| {
        l.msg
            .key
            .as_deref()
            .is_some_and(|k| k.starts_with(crate::message_reporters::KEY_STOP_WORKER))
    })
}

fn stop_twice(
    app: &mut App,
    wid: WindowId,
    sid: &aterm_session::SessionId,
    src: crate::input::Source,
) {
    let target = chip_ordinal(app, wid, sid);
    let at = app.windows[&wid].overview().expect("open").controls_lines()[0].clone();
    let selected: isize = at
        .split(' ')
        .find_map(|kv| kv.strip_prefix("selected="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    app.overview_move(wid, target as isize - selected);
    for _ in 0..2 {
        let _ = app.input_to_session(
            wid,
            key(aterm_types::keyboard::NamedKey::Backspace),
            src,
            None,
            crate::app_input::PressPhase::Initial,
        );
    }
}

#[test]
fn codex_renewal_owner_stop_shares_the_apps_live_task_refusal() {
    use crate::app_overview::{StopRefused, stop_worker_in, stop_worker_refusal_in};
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let (app, _, sids) = app_with_three();
    give_task(&app, &sids[1], crate::task_record::FROM_CTL);
    let local = local_of(&app, &sids[1]);
    let ctx = ctx_of(&app, &sids[1]);
    assert_eq!(
        app.stop_worker_refusal(local),
        stop_worker_refusal_in(&ctx, false)
    );
    assert_eq!(stop_worker_refusal_in(&ctx, false), None);
    mark_session_task(&ctx, local, &TaskMark::new(TaskState::Done)).unwrap();
    assert_eq!(
        app.stop_worker_refusal(local),
        Some(StopRefused::NotRunning)
    );
    assert_eq!(
        stop_worker_in(-1, &ctx, aterm_slog::Actor::Human),
        Err(StopRefused::NotRunning)
    );
}

#[test]
fn stop_worker_reads_the_task_again_when_it_acts() {
    use crate::app_overview::StopRefused;
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    for ends in [true, false] {
        let (mut app, wid, sids) = app_with_three();
        give_task(&app, &sids[1], crate::task_record::FROM_CTL);
        assert!(app.open_overview().is_ok());
        let worker = chip_ordinal(&app, wid, &sids[1]);
        app.overview_move(wid, worker as isize);
        app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
        assert!(
            app.read_aux_controls(AuxTarget::Overview)[0].contains(&format!("confirm={worker}")),
            "armed"
        );
        if ends {
            let ctx = ctx_of(&app, &sids[1]);
            mark_session_task(
                &ctx,
                local_of(&app, &sids[1]),
                &TaskMark::new(TaskState::Done),
            )
            .expect("unfenced");
        }
        app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
        if ends {
            assert_eq!(app.agents.last_stop_refused, Some(StopRefused::NotRunning));
            assert_eq!(app.agents.stop_signals, 0, "an ended task is not signalled");
            assert!(
                !stop_row_on_glass(&app),
                "asked for what already is: a record"
            );
        } else {
            assert!(
                matches!(
                    app.agents.last_stop_refused,
                    None | Some(StopRefused::Signal(_))
                ),
                "{:?}",
                app.agents.last_stop_refused
            );
            assert_eq!(app.agents.stop_signals, 1, "a working task is");
        }
    }
}

#[test]
fn a_worker_with_only_its_shell_in_front_is_refused() {
    use crate::app_overview::StopRefused;
    use crate::session_chrome::ConnVerb;
    for program in ["zsh", "claude"] {
        let (mut app, wid, sids) = app_with_three();
        give_task(&app, &sids[1], sids[0].as_str());
        {
            let ctx = ctx_of(&app, &sids[1]);
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_program(7, Some(program.to_string()));
        }
        let shell = program == "zsh";
        assert!(app.open_overview().is_ok());
        stop_twice(&mut app, wid, &sids[1], crate::input::Source::Human);
        if shell {
            assert_eq!(app.agents.last_stop_refused, Some(StopRefused::NoAgent));
            assert_eq!(app.agents.stop_signals, 0, "the shell is not signalled");
            let rows =
                app.messages
                    .live_rows()
                    .filter(|l| {
                        l.msg.key.as_deref().is_some_and(|k| {
                            k.starts_with(crate::message_reporters::KEY_STOP_WORKER)
                        })
                    })
                    .count();
            assert_eq!(rows, 1, "one gesture row");
            app.agents.owner_sheet_for_test = Some(Some(true));
            let tab = app.windows[&wid].tab_set.tabs()[1].id;
            app.dispatch_tab_menu_connection(wid, tab, &sids[1], ConnVerb::StopWorker);
            assert_eq!(app.agents.stop_signals, 0, "the tab menu's row too");
        } else {
            assert_eq!(app.agents.stop_signals, 1, "an agent in front is signalled");
            assert_ne!(app.agents.last_stop_refused, Some(StopRefused::NoAgent));
        }
    }
}

#[test]
fn a_control_clients_tab_menu_stop_opens_no_card() {
    use crate::session_chrome::ConnVerb;
    for controller in [true, false] {
        let (mut app, wid, sids) = app_with_three();
        give_task(&app, &sids[1], sids[0].as_str());
        let tab = app.windows[&wid].tab_set.tabs()[1].id;
        app.with_gesture_source(controller, |app| {
            app.dispatch_tab_menu_connection(wid, tab, &sids[1], ConnVerb::StopWorker);
        });
        if controller {
            assert_eq!(app.agents.stop_signals, 1, "the signal verb's act");
            assert!(app.windows[&wid].overview().is_none(), "no card");
            assert!(!stop_row_on_glass(&app), "no row");
        } else {
            assert_eq!(app.agents.stop_signals, 0, "nothing unconfirmed");
            let line = chip_line(&app, &sids[1]);
            assert!(line.contains("confirm=true"), "{line}");
            assert!(
                app.read_aux_controls(AuxTarget::Overview)[0].contains("confirm_by=person"),
                "armed for the person's hand"
            );
        }
    }
}

#[test]
fn a_controller_cannot_stop_a_held_worker_and_the_person_can() {
    use crate::app_overview::StopRefused;
    use crate::input::Source;
    use crate::session_chrome::ConnVerb;
    for person in [false, true] {
        let (mut app, wid, sids) = app_with_three();
        give_task(&app, &sids[1], sids[0].as_str());
        give_task(&app, &sids[2], sids[0].as_str());
        let held = ctx_of(&app, &sids[1]);
        if person {
            assert!(app.take_over_session(local_of(&app, &sids[1])).is_ok());
        } else {
            let on = format!("{} on reason=mine", sids[1].as_str());
            assert_eq!(
                crate::fabric::cmd_hold(&app.store, &on, crate::fabric::HoldIssuer::Owner),
                "OK hold=1\n"
            );
        }
        let wire = crate::fabric::halt_refusal(&held, "signal").expect("the wire is halted");
        assert!(wire.starts_with("ERR halted "), "{wire}");
        let counted = held.fabric.person_refusals();
        assert!(app.open_overview().is_ok());

        stop_twice(&mut app, wid, &sids[1], Source::Controller);
        assert!(
            matches!(app.agents.last_stop_refused, Some(StopRefused::Halted(_))),
            "person={person}: {:?}",
            app.agents.last_stop_refused
        );
        assert_eq!(app.agents.stop_signals, 0, "person={person}: no signal");
        assert!(held.fabric.holds().is_held(), "the hold stands");
        assert!(
            app.read_aux_controls(AuxTarget::Overview)[0].contains("confirm=-"),
            "no confirm left armed"
        );
        assert_eq!(
            held.fabric.person_refusals(),
            counted + 2 * u64::from(person),
            "each refused ⌫, counted only under the person's hold"
        );
        assert!(!stop_row_on_glass(&app), "person={person}: no row");
        assert!(
            !app.person_gesture_refused(),
            "the mark ends with the gesture"
        );
        app.agents.last_stop_refused = None;
        let tab = app.windows[&wid].tab_set.tabs()[0].id;
        app.with_gesture_source(true, |app| {
            app.dispatch_tab_menu_connection(wid, tab, &sids[1], ConnVerb::StopWorker);
        });
        assert!(
            matches!(app.agents.last_stop_refused, Some(StopRefused::Halted(_))),
            "person={person}: the tab menu's row"
        );
        assert_eq!(app.agents.stop_signals, 0);
        assert!(!stop_row_on_glass(&app), "person={person}: no row");

        stop_twice(&mut app, wid, &sids[1], Source::Human);
        assert_eq!(
            app.agents.stop_signals, 1,
            "person={person}: the person stops it"
        );
        stop_twice(&mut app, wid, &sids[2], Source::Controller);
        assert_eq!(
            app.agents.stop_signals, 2,
            "an unheld worker, as the wire verb"
        );
    }
}

#[test]
fn session_close_retargets_the_open_card() {
    let (mut app, _wid, sids) = app_with_three();
    assert!(app.open_overview().is_ok());
    assert!(app.read_aux_controls(AuxTarget::Overview)[0].contains("sessions=3"));
    let local = local_of(&app, &sids[2]);
    app.retire_session_registration(local);
    let lines = app.read_aux_controls(AuxTarget::Overview);
    assert!(lines[0].contains("sessions=2"), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains(sids[2].as_str())));
}

#[test]
fn the_title_counts_another_aterms_sessions_apart() {
    let (mut app, wid, _sids) = app_with_three();
    assert!(app.open_overview().is_ok());
    let title = |app: &App| {
        app.windows
            .get(&wid)
            .and_then(|ws| ws.overview())
            .map(crate::overview::OverviewState::title)
            .expect("open")
    };
    assert!(title(&app).starts_with("This aterm \u{2014} 3 sessions"));
    assert!(!title(&app).contains("another aterm"), "{}", title(&app));
    app.siblings = vec![crate::status_item::SiblingInstance {
        pid: 900,
        sessions: 3,
        ..crate::status_item::SiblingInstance::default()
    }];
    app.overview_refresh_all();
    assert!(
        title(&app).starts_with("This aterm \u{2014} 3 sessions (+3 in another aterm)"),
        "{}",
        title(&app)
    );
    app.siblings.clear();
    app.overview_refresh_all();
    assert!(!title(&app).contains("another aterm"), "{}", title(&app));
}

/// A CODEX TAB'S OWN REPORT ON THE SURFACES THE PERSON READS: codex's reader names no state
/// (`agent=unknown`), so the `working` report codex writes, fed to the tab's real engine and
/// folded by the real sweep, is what the Overview chip (`busy`, the working mark), Settings ▸
/// Harness's row (`working`) and the tab hover's activity line (`codex working`) say, and the
/// hover's cached line's revision moves with it (the status machine settled first, so only the
/// report moves it); the hover's state line says the context codex's rollout counts beside the
/// report (`ctx 12% left ⚠`); a permission box is `waiting on permission` in Settings, never
/// idle; `agent=` stays `unknown`.
/// NEGATIVE CONTROLS: the same tab before any report says none of it, and with Tab Status off
/// (nothing folds the report) its hover keeps no report standing.
#[test]
fn a_codex_tabs_own_report_is_its_word_on_the_overview_settings_and_the_hover() {
    use crate::overview::ChipMark;
    const WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";
    let mut app = App::headless_for_test();
    let local = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(local));
    let (term, ctx) = {
        let pooled = app.pool.get(local).expect("pooled");
        (pooled.term.clone(), pooled.ctx.clone())
    };
    {
        let mut tl = ctx.timeline.lock().expect("timeline");
        tl.note_foreground_group(4242);
        tl.set_program(4242, Some("codex".into()));
        tl.publish_agent(
            "unknown",
            None,
            None,
            Some(aterm_phase::Program::Codex),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    }
    let sid = ctx.self_id.clone();
    let surfaces = |app: &App| {
        let (groups, _) = app.overview_parts();
        let chip = groups
            .iter()
            .flat_map(|g| g.chips.iter())
            .find(|c| c.sid == sid)
            .cloned()
            .expect("listed");
        let row = app
            .tab_control_state(crate::autopilot::HarnessPosture::default())
            .tabs
            .into_iter()
            .find(|row| row.show_local_id == Some(local))
            .expect("its row");
        (
            chip.mark,
            chip.doing,
            row.ai_status,
            app.session_status_text(local),
        )
    };
    let t0 = std::time::Instant::now();
    let sweep = |app: &mut App, s: u64| {
        let _ = app.observe_session_statuses(t0 + std::time::Duration::from_secs(s));
    };
    sweep(&mut app, 0);
    // The status machine's dwell publishes the stub's first reading here, not after the report.
    sweep(&mut app, 1);
    let (mark, doing, status, activity) = surfaces(&app);
    assert_eq!(mark, ChipMark::Quiet, "no report yet: {doing}");
    assert!(doing.starts_with("unknown "), "{doing}");
    assert!(!status.starts_with("working"), "{status}");
    assert_ne!(activity.as_deref(), Some("codex working"));
    let before = app.session_status_revision(local);

    sweep(&mut app, 2);
    assert_eq!(
        app.session_status_revision(local),
        before,
        "CONTROL: settled, a sweep with no report leaves the revision"
    );
    crate::term_lock(&term).process(WORKING);
    sweep(&mut app, 3);
    let (mark, doing, status, activity) = surfaces(&app);
    assert_eq!(mark, ChipMark::Working, "{doing}");
    assert!(doing.starts_with("busy "), "{doing}");
    assert!(status.starts_with("working"), "{status}");
    assert_eq!(activity.as_deref(), Some("codex working"));
    assert_ne!(
        app.session_status_revision(local),
        before,
        "the hover's cached line is read again"
    );
    assert_eq!(
        ctx.timeline.lock().expect("timeline").agent().word,
        "unknown",
        "the reader's word is never moved"
    );
    // The hover says the context codex's rollout counts beside the report, as a Claude tab's
    // says its own (CONTROL: no figure read yet, none said).
    let hover = |app: &mut App| {
        app.refresh_presence_session(local);
        app.presence_hover(local, None).unwrap_or_default()
    };
    let line = hover(&mut app);
    assert!(line.contains("codex working"), "{line}");
    assert!(!line.contains("ctx "), "{line}");
    let survey = crate::survey::CodexSurvey {
        facts: aterm_agent::harness::codex_facts::CodexFacts {
            ctx_left_pct: Some(12),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(
        ctx.timeline
            .lock()
            .expect("timeline")
            .set_codex_survey(4242, survey),
        "read for the program in front"
    );
    let line = hover(&mut app);
    assert!(
        line.contains("codex working \u{b7} ctx 12% left \u{26a0}"),
        "{line}"
    );

    crate::term_lock(&term).process(b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\");
    sweep(&mut app, 4);
    let (_, _, status, activity) = surfaces(&app);
    assert!(status.starts_with("waiting on permission"), "{status}");
    assert_eq!(activity.as_deref(), Some("codex blocked"));

    app.config.tab_status = Some(false);
    app.reconfigure_session_status();
    assert_eq!(
        app.session_status_text(local),
        None,
        "Tab Status off: no report stands in the hover"
    );
}

/// WHO DRIVES A CODEX TAB, on the Overview and the hover alike (`autopilot::carried`): a host
/// that admits codex drives a plain codex in front of it not at all, and a codex-alab that
/// proved its own claim is named as its own native autopilot on both.
#[test]
fn a_codex_is_driven_on_the_overview_and_the_hover_only_under_its_proven_claim() {
    use crate::overview::Driver;
    let mut app = App::headless_for_test();
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    {
        let mut tl = ctx.timeline.lock().expect("timeline");
        tl.note_foreground_group(7);
        tl.set_program(7, Some("codex".into()));
        tl.publish_agent(
            "unknown",
            None,
            None,
            Some(aterm_phase::Program::Codex),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    }
    ctx.meta
        .lock()
        .expect("meta")
        .set("autopilot", Some("land".into()));
    let sid = ctx.self_id.clone();
    let driver = |app: &App| {
        let (groups, _) = app.overview_parts();
        groups
            .iter()
            .flat_map(|g| g.chips.iter())
            .find(|c| c.sid == sid)
            .and_then(|c| c.driver.clone())
    };
    assert_eq!(
        driver(&app),
        None,
        "CONTROL: a plain codex, admitted, unclaimed"
    );
    assert_eq!(app.driven_by_aterm(0), None);
    crate::session_timeline::claim_supervisor_as(
        &ctx,
        &format!("codex-alab@{}", std::process::id()),
        Some(1),
        None,
        Some(crate::self_claim::stand_in::running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the proven claim");
    let words = app.driven_by_aterm(0).expect("the hover names the driver");
    assert_eq!(driver(&app), Some(Driver::CodexAlab(words)));
    host.shutdown_and_join();
}

#[test]
fn the_overview_names_the_chips_face_and_a_landed_task() {
    use crate::overview::Driver;
    use aterm_agent::harness::acts::TaskFace;
    let mut app = App::headless_for_test();
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    {
        let mut tl = ctx.timeline.lock().expect("timeline");
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
        tl.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    }
    ctx.meta
        .lock()
        .expect("meta")
        .set("autopilot", Some("land".into()));
    let sid = ctx.self_id.clone();
    let chip = |app: &App| {
        let (groups, _) = app.overview_parts();
        groups
            .iter()
            .flat_map(|g| g.chips.iter())
            .find(|c| c.sid == sid)
            .cloned()
            .expect("listed")
    };
    assert_eq!(
        chip(&app).driver,
        None,
        "a host that admits it but holds no claim drives nothing"
    );
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claimed");
    assert_eq!(chip(&app).driver, Some(Driver::Aterm("land".to_string())));
    app.apply_harness_fact(sid.as_str(), crate::HarnessFact::Task(Some(TaskFace::Open)));
    app.apply_harness_fact(
        sid.as_str(),
        crate::HarnessFact::Task(Some(TaskFace::Landed)),
    );
    assert_eq!(
        chip(&app).driver,
        Some(Driver::Aterm("land, landed".to_string()))
    );
    assert_eq!(
        chip(&app).driver.map(|d| d.words()).as_deref(),
        Some("driven by aterm (land, landed)")
    );
    let mut mark = crate::task_record::TaskMark::new(crate::task_record::TaskState::Done);
    mark.why = Some(crate::task_record::WHY_LANDED);
    crate::task_record::mark_session_task(&ctx, 0, &mark).expect("marked");
    let words = chip(&app).task_words.expect("a task");
    assert!(words.starts_with("task landed "), "{words}");
    assert!(!words.contains("done"), "{words}");
    assert_eq!(words.matches("landed").count(), 1, "said once: {words}");
    host.shutdown_and_join();
}

#[test]
fn the_overview_says_why_a_landing_holds() {
    use aterm_agent::harness::acts::{HoldLeg, LandHeld, OpenWord, TaskFace};
    let mut app = App::headless_for_test();
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    {
        let mut tl = ctx.timeline.lock().expect("timeline");
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    ctx.meta
        .lock()
        .expect("meta")
        .set("autopilot", Some("land".into()));
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claimed");
    let sid = ctx.self_id.clone();
    let chip = |app: &App| {
        let (groups, _) = app.overview_parts();
        groups
            .iter()
            .flat_map(|g| g.chips.iter())
            .find(|c| c.sid == sid)
            .map(|c| {
                (
                    c.driver.as_ref().map(crate::overview::Driver::words),
                    c.held.clone(),
                )
            })
            .expect("its chip")
    };
    let tell = |app: &mut App, held: Option<LandHeld>| {
        if crate::harness_host::store_land_held(&app.store, sid.as_str(), held) {
            app.apply_harness_fact(sid.as_str(), crate::HarnessFact::LandHeld);
        }
    };
    let land = Some("driven by aterm (land)".to_string());
    app.apply_harness_fact(sid.as_str(), crate::HarnessFact::Task(Some(TaskFace::Open)));
    assert_eq!(chip(&app), (land.clone(), None));
    let held = LandHeld {
        since_ms: Some(1_792_159_320_000),
        nudges: 3,
        read: true,
        legs: HoldLeg::Repo.bit() | HoldLeg::Open.bit(),
        open: [OpenWord::of("follow-up"), None, None, None],
        unpushed: 2,
        ..LandHeld::default()
    };
    tell(
        &mut app,
        Some(LandHeld {
            read: false,
            legs: 0,
            ..held
        }),
    );
    assert_eq!(chip(&app), (land.clone(), None), "no yield read yet");
    tell(&mut app, Some(held));
    assert_eq!(
        chip(&app),
        (
            land.clone(),
            Some(
                "not landed yet: 2 unpushed commits; open in its report: follow-up \
                 \u{b7} 3 nudges"
                    .to_string()
            )
        )
    );
    ctx.meta
        .lock()
        .expect("meta")
        .set("autopilot", Some("cruise".into()));
    assert_eq!(chip(&app).1, None, "none under Cruise");
    ctx.meta
        .lock()
        .expect("meta")
        .set("autopilot", Some("land".into()));
    tell(&mut app, None);
    assert_eq!(chip(&app), (land, None));
    host.shutdown_and_join();
}

#[test]
fn a_harness_need_says_needs_you_on_the_overview() {
    let (mut app, _wid, sids) = app_with_three();
    let (harness, person) = (&sids[1], &sids[2]);
    let need = "grant Screen Recording to com.aterm.aterm in System Settings \u{25b8} \
                Privacy & Security \u{25b8} Screen Recording";
    let ask = |sid: &aterm_session::SessionId, owner: &str, text: String| {
        ctx_of(&app, sid)
            .meta
            .lock()
            .unwrap()
            .set_attention_kind(
                owner,
                Some(text),
                Some(aterm_types::needs::AttentionKind::Ask),
            )
            .expect("a slot");
    };
    ask(
        harness,
        crate::harness_host::NEEDS_ATTENTION_OWNER,
        format!("claude needs you: {need} (+1 more) \u{2014} carrying on"),
    );
    ask(person, "person", "sign the release notes".to_string());
    for sid in [harness, person] {
        app.publish_need(local_of(&app, sid));
    }
    let doing = |sid: &aterm_session::SessionId| {
        let (groups, _) = app.overview_parts();
        groups
            .iter()
            .flat_map(|g| g.chips.iter())
            .find(|c| &c.sid == sid)
            .map(|c| c.doing.clone())
            .expect("listed")
    };
    let words = doing(harness);
    assert!(
        words.starts_with("needs you: grant Screen Recording")
            && words.ends_with("\u{2026} (+1 more)"),
        "{words}"
    );
    assert!(!words.contains("claude"), "{words}");
    assert!(words.len() <= crate::needs::ROW_WHAT_MAX, "{words}");
    assert_eq!(doing(person), "sign the release notes");
}

/// On a Retina window a pointer lands on an Overview item as far right as its row is painted:
/// past where a card capped at 760 device pixels would end, and not one pixel further.
#[test]
fn a_retina_pointer_lands_on_an_overview_row_past_a_device_pixel_cap() {
    let (mut app, wid, _) = app_with_three();
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.cols = 400;
        ws.scale = 2.0;
        ws.input_scratch.cells = vec![Vec::new(); 50];
    }
    assert!(app.open_overview().is_ok());
    let (cw, ch) = app.win_cell_size(wid);
    let window = &app.windows[&wid];
    let painted = crate::settings::SettingsGeom {
        cw: cw as f32,
        ch: ch as f32,
        font_px: app.win_font_px(wid),
        cols: usize::from(window.cols),
        panel_rows: window.overlay_rows(),
        px_per_pt: 2.0,
    };
    let t = app
        .overlay_coordinate_transform(wid)
        .expect("the open card's transform");
    assert_eq!(t.scale, 1.0, "the terminal route: device-pixel geometry");
    let (ox, oy) = app.frame_origin(wid);
    let to_window = |x: f32, y: f32| {
        (
            ox as f64 + t.origin_x + f64::from(x),
            oy as f64 + t.origin_y + f64::from(y),
        )
    };
    let card = window.overview().expect("open");
    let mut items = 0;
    for slot in 0.. {
        let Some((rx, ry, rw, rh)) = crate::overview::overview_row_rect(card, &painted, slot)
        else {
            break;
        };
        assert!(
            rw > 760.0,
            "the 2x row ({rw} px) reaches past a card capped at 760 px"
        );
        let (cx, cy) = (rx + rw - 0.5, ry + rh * 0.5);
        let want = crate::overview::overview_item_hit(card, &painted, cx, cy);
        let (x, y) = to_window(cx, cy);
        assert_eq!(
            app.overview_item_at_pointer(wid, x, y),
            want,
            "slot {slot}'s right end"
        );
        let (x, y) = to_window(rx + rw + 0.5, cy);
        assert_eq!(
            app.overview_item_at_pointer(wid, x, y),
            None,
            "slot {slot}: past its painted right end"
        );
        items += usize::from(want.is_some());
    }
    assert_eq!(
        items, 3,
        "each of the three tabs' chips is hit at its right end"
    );
}

/// The lab's pointer selects an Overview row on a headless window that never drew the card,
/// as a person's pointer selects one on glass: `pointer move item=<n>` lays the glass the next
/// frame shows, as `pointer move <r> <c>` does, and aims at the centre of that item's row
/// (§14.16).
#[test]
fn the_labs_pointer_selects_an_overview_row_the_window_never_drew() {
    use crate::app_mouse::PointerAction;
    let (mut app, wid, _) = app_with_three();
    // A headless window composes a frame only for a capture: none here.
    app.windows
        .get_mut(&wid)
        .expect("the window")
        .input_scratch
        .cells
        .clear();
    assert!(app.open_overview().is_ok());
    let head = |app: &App| app.windows[&wid].overview().expect("open").controls_lines()[0].clone();
    for item in [2, 0, 1] {
        app.pointer_cmd(PointerAction::MoveItem(item))
            .unwrap_or_else(|why| panic!("item {item}: {why}"));
        assert!(
            head(&app).contains(&format!(" selected={item} ")),
            "the pointer over item {item}'s row selects it: {}",
            head(&app)
        );
    }
    assert_eq!(
        app.pointer_cmd(PointerAction::MoveItem(3)),
        Err("item 3 is not a row the card shows".to_string()),
    );
    assert!(
        head(&app).contains(" selected=1 "),
        "a refused aim moves nothing"
    );
}

/// `pointer move item=<n>` over an Overview on a Settings page of a Retina window, its frame
/// inside a larger window: each aim, read back through the hover's own path, is the centre of
/// that item's row and selects it, as on the terminal route (§14.16).
#[test]
fn an_item_aim_is_the_overview_rows_centre_over_a_retina_settings_page() {
    let (mut app, wid, _) = app_with_three();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    app.windows.get_mut(&wid).expect("the window").scale = 2.0;
    assert!(app.open_overview().is_ok());
    let (cw, ch) = app.win_cell_size(wid);
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (
            usize::from(ws.rows) + usize::from(app.chrome_rows(wid)),
            usize::from(ws.cols),
        )
    };
    let (pad, top) = (app.win_pad(wid), app.win_pad_top(wid) + app.win_head(wid));
    app.windows.get_mut(&wid).expect("the window").win_px =
        Some(aterm_window::dpi::PhysicalSize::new(
            (cols * cw + 2 * pad + cw - 1) as u32,
            (rows * ch + top + pad + ch - 1) as u32,
        ));
    assert!(
        app.frame_origin(wid).0 > 0,
        "the frame sits inside its window"
    );
    assert_eq!(
        app.overlay_coordinate_transform(wid).map(|t| t.scale),
        Some(2.0),
        "over a page the card's points are scaled to the window's pixels"
    );
    let card = app.windows[&wid].overview().expect("open");
    let mut items = 0;
    while let Ok((x, y)) = app.overlay_card_item_px(wid, items) {
        let (t, lx, ly) = app
            .overlay_card_point(wid, x, y)
            .expect("the card's transform");
        let (cx, cy) = crate::overview::overview_item_centre(card, &t.geom, items)
            .expect("an item the card shows");
        assert!(
            (lx - cx).abs() < 1e-3 && (ly - cy).abs() < 1e-3,
            "item {items} aims at ({x}, {y}), which the hover reads as card point ({lx}, \
             {ly}), not the row's centre ({cx}, {cy})"
        );
        assert_eq!(
            app.overview_item_at_pointer(wid, x, y),
            Some(items),
            "item {items}"
        );
        items += 1;
    }
    assert!(items >= 3, "the card shows {items} items");
}

#[test]
fn ages_and_sizes_read_in_coarse_units() {
    assert_eq!(super::coarse_age(59_000), "just now");
    assert_eq!(super::coarse_age(4 * 60_000 + 59_000), "4m");
    assert_eq!(super::coarse_age(2 * 3_600_000), "2h");
    assert_eq!(super::size_words(812), "812 B");
    assert_eq!(super::size_words(2150), "2.1 KB");
}
