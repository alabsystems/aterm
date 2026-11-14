// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
fn publish_meta(app: &mut App, session: u64, field: &str, value: &str) {
    {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let h = store.by_local(session).expect("registered session");
        h.ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set(field, Some(value.to_string()));
    }
    app.publish_need(session);
}

#[test]
fn a_codex_terminal_keeps_its_program_icon_while_busy() {
    let app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    for program in [Some(aterm_phase::Program::Codex), None] {
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(4242);
            timeline.set_program(4242, Some("codex".into()));
            timeline.publish_agent(
                "unknown",
                None,
                None,
                program,
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        }
        assert_eq!(
            app.tab_strip_metadata(WindowId(0))[0].icon,
            program.map(|_| crate::tab_model::TabIconKind::Codex)
        );
    }
}

/// A CODEX TAB AT REST RESTS ON ITS ROW, AND ONE AT A BOX NEEDS ITS PERSON: where its reader
/// names no state (`agent=unknown`), the row is classed by the survey's word for the
/// program's own shown report (`program_state::survey_word`), so an `idle`, `done` or
/// `error` report is resting and a `working` one, or none, working; a `blocked` one is
/// published as the tab's need (`needs::program_need`, through `publish_need` as every
/// presence refresh does), except under a proven self-claim, which speaks for the tab: there
/// it works. A word its reader names stands over any report. `NeedAges::observe` sees the
/// class move.
#[test]
fn a_codex_tabs_own_report_rests_it_only_where_its_reader_names_no_state() {
    use aterm_core::terminal::program_state::{
        ProgramBlockedKind as K, ProgramState, ProgramStateWord as W,
    };
    let mut app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let publish = |word: &'static str, reader: aterm_phase::Program, program: &str| {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(4242);
        tl.set_program(4242, Some(program.into()));
        tl.publish_agent(
            word,
            None,
            None,
            Some(reader),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 7 },
                fp: 0xc0de,
            },
        );
        assert_eq!(tl.agent().word, word);
    };
    let reports = std::cell::Cell::new(0_u64);
    let report_kind = |state: Option<W>, kind: Option<K>| {
        reports.set(reports.get() + 1);
        ctx.timeline.lock().unwrap().note_program_state(
            state.map(|state| ProgramState {
                state,
                kind,
                app: aterm_core::terminal::program_state::ProgramApp::Codex,
            }),
            reports.get(),
        );
    };
    let report = |state: Option<W>| report_kind(state, None);
    // The row's class as (needs its person, rests, works), one of them 1.
    let here = |app: &mut App| {
        app.publish_need(0);
        let handle = app
            .store
            .read()
            .unwrap()
            .by_local(0)
            .cloned()
            .expect("registered");
        let row = app.status_session_row(&handle);
        match (row.need.is_some(), row.glance_word()) {
            (true, _) => (1, 0, 0),
            (false, "idle") => (0, 1, 0),
            (false, _) => (0, 0, 1),
        }
    };
    let claim = |proven: Option<bool>| {
        ctx.meta.lock().unwrap().supervisor =
            proven.map(|proven| crate::session_timeline::SupervisorClaim {
                holder: "codex-alab@4242".to_string(),
                conn: Some(1),
                expires_us: None,
                proven: proven.then(|| crate::self_claim::Proven {
                    pid: 4242,
                    start: "Mon Oct 20 16:37:58 2026".to_string(),
                }),
                scoped: None,
            });
    };
    let proven = |held: bool| claim(held.then_some(true));
    let need = |app: &mut App| {
        app.publish_need(0);
        ctx.timeline.lock().unwrap().need().need.clone()
    };
    publish("unknown", aterm_phase::Program::Codex, "codex");
    report(None);
    assert_eq!(
        here(&mut app),
        (0, 0, 1),
        "no report: unknown stays working"
    );
    for state in [W::Idle, W::Done, W::Error] {
        report(Some(state));
        assert_eq!(
            here(&mut app),
            (0, 1, 0),
            "{state:?}: a codex at rest rests"
        );
    }
    report(Some(W::Working));
    assert_eq!(here(&mut app), (0, 0, 1), "a working report is work");
    for kind in [None, Some(K::Permission), Some(K::Question), Some(K::Auth)] {
        report_kind(Some(W::Blocked), kind);
        assert_eq!(
            here(&mut app),
            (1, 0, 0),
            "blocked {kind:?}: a codex at a box no claim speaks for needs you"
        );
        proven(true);
        assert_eq!(
            here(&mut app),
            (0, 0, 1),
            "blocked {kind:?} under a proven self-claim: it carries itself, so it works"
        );
        proven(false);
    }
    // Under a person's hold another holder's claim speaks for the tab only where it is a
    // proven self-claim: the same holder unproven leaves the box the person's.
    report_kind(Some(W::Blocked), Some(K::Permission));
    assert!(crate::fabric::apply_hold_for_test(
        &ctx,
        Some(crate::fabric::Hold {
            reason: "-".into(),
            origin: "local".into(),
        })
    ));
    claim(Some(true));
    assert_eq!(need(&mut app), None, "held: a proven self-claim carries it");
    claim(Some(false));
    assert_eq!(
        need(&mut app).map(|need| need.what),
        Some(crate::needs::CODEX_APPROVAL_WORDS.to_string()),
        "held: the same holder unproven speaks for nothing"
    );
    claim(None);
    assert!(crate::fabric::apply_hold_for_test(&ctx, None));
    // The claim is codex's alone: another agent whose reader names no state (a Claude Code
    // under `node`, which the gate honours for its name) raises nothing from the report.
    publish("unknown", aterm_phase::Program::Claude, "node");
    report_kind(Some(W::Blocked), Some(K::Permission));
    assert_eq!(here(&mut app), (0, 0, 1), "no codex: no claim need");
    assert_eq!(need(&mut app), None);
    publish("unknown", aterm_phase::Program::Codex, "codex");
    report(Some(W::Idle));
    let handle = app
        .store
        .read()
        .unwrap()
        .by_local(0)
        .cloned()
        .expect("registered");
    assert_eq!(here(&mut app), (0, 1, 0), "the box answered: no need");
    let resting = app.status_session_row(&handle);
    assert_eq!(resting.program, Some("idle"));
    report(Some(W::Working));
    let working = app.status_session_row(&handle);
    let now = status_unix_now();
    assert!(app.presence.need_ages.observe(0, Some(&resting), now));
    assert!(
        app.presence.need_ages.observe(0, Some(&working), now),
        "the class moved with the report alone"
    );
    assert!(
        !app.presence.need_ages.observe(0, Some(&working), now),
        "control: no move"
    );
    publish("busy", aterm_phase::Program::Claude, "claude");
    report_kind(Some(W::Blocked), Some(K::Permission));
    assert_eq!(
        here(&mut app),
        (0, 0, 1),
        "a word its reader names stands over the report, a box's included"
    );
    assert_eq!(app.status_session_row(&handle).program, None);
}

/// A fleet glance over a book that says this Mac is in a fleet, read by no thread.
fn in_a_fleet() -> crate::fleet_glance::FleetGlance {
    let mut slot = crate::fleet_glance::FleetSlot::default();
    slot.book.in_fleet = true;
    crate::fleet_glance::FleetGlance::over(slot)
}

#[test]
fn chrome_reads_the_status_menu_as_its_builder_composes_it() {
    let now = aterm_time::unix_millis().unwrap_or(0);
    let mut app = App::headless_for_test();
    let lines = app.status_item_chrome_lines_at(now);
    assert!(
        lines[0].starts_with("statusitem title=\u{276f} fleet=off ")
            && lines[0].ends_with(" blocked=no"),
        "{lines:#?}"
    );
    assert_eq!(lines.len(), 2, "out of a fleet the menu is one line");
    assert!(
        lines[1].ends_with("not in a fleet\" enabled=no at=0"),
        "{lines:#?}"
    );

    app.fleet_glance = Some(in_a_fleet());
    let hour = crate::status_item::a_blocked_hour(now);
    assert!(
        app.fleet_traffic
            .set(Some(crate::fleet_traffic::FleetTraffic::over(
                Some(hour),
                0
            )))
            .is_ok(),
        "no reader in a unit test"
    );
    let lines = app.status_item_chrome_lines_at(now);
    assert!(lines[0].ends_with(" blocked=yes"), "{lines:#?}");
    let glance = app.status_glance();
    assert_eq!(
        lines[1..],
        crate::status_item::menu_lines(&crate::status_item::compose_status_menu(&glance, now))[..],
    );
    assert_eq!(
        lines.iter().filter(|l| l.contains(" checked=yes")).count(),
        1,
        "the level in force: {lines:#?}"
    );
    assert_eq!(
        lines.iter().filter(|l| l.contains(" blocked=yes")).count(),
        3,
        "the bar, the headline and the Mac this one refused: {lines:#?}"
    );
}

/// THE BAR'S BLOCKED MARK CLEARS WHEN A MENU THAT SHOWS THE REFUSAL OPENS: the open states what
/// the painted menu showed the person, the menu still lists it, and reading the menu as text
/// opens nothing. A menu painted before the refusal arrived showed none of it: its open leaves
/// the mark. NEGATIVE CONTROL: an open that clears the mark up to now, whatever was painted,
/// fails the second assertion after the first open.
#[test]
fn opening_the_menu_clears_the_bars_blocked_mark_and_keeps_the_refusal_listed() {
    let now = aterm_time::unix_millis().unwrap_or(0);
    let mut app = App::headless_for_test();
    app.fleet_glance = Some(in_a_fleet());
    let hour = crate::status_item::a_blocked_hour(now);
    let _ = app
        .fleet_traffic
        .set(Some(crate::fleet_traffic::FleetTraffic::over(
            Some(hour.clone()),
            0,
        )));
    assert!(app.status_glance().blocked);
    let _ = app.status_item_chrome_lines();
    assert!(app.status_glance().blocked, "a read as text is no open");
    let _ = app.take_status_rebuild();
    app.on_status_menu_opening(None);
    assert!(app.take_status_rebuild(), "the open owes the bar a rebuild");
    assert!(
        app.status_glance().blocked,
        "the menu that opened was painted before the refusal: not seen"
    );
    let painted = app.status_glance().blocked_shown_ms();
    assert_eq!(painted, hour.last_blocked_in_ms);
    assert!(painted.is_some());
    app.on_status_menu_opening(painted);
    let glance = app.status_glance();
    assert!(!glance.blocked, "seen");
    assert!(!glance.button_face().blocked);
    assert_eq!(
        glance.traffic.as_ref().map(|t| t.blocked_in),
        Some(2),
        "the menu still says what was refused"
    );

    // Out of a fleet nothing is marked, whatever the record holds, and its menu shows none.
    let alone = App::headless_for_test();
    let _ = alone
        .fleet_traffic
        .set(Some(crate::fleet_traffic::FleetTraffic::over(
            Some(crate::status_item::a_blocked_hour(now)),
            0,
        )));
    assert!(!alone.status_glance().blocked);
    assert_eq!(alone.status_glance().blocked_shown_ms(), None);
}

/// THE MENU'S OPEN READS NO FILE: it shows the level the last change notification brought
/// (`Wake::FleetAccessChanged`, `reread_fleet_access`). The level here moves under the App with
/// no notification, which nothing in aterm does; the open leaves the face as it was, and the
/// re-read a notification makes brings the new level. NEGATIVE CONTROL: an open that reads the
/// level again shows Full after it. Unix, as the seam that sets a test thread's level is.
#[cfg(unix)]
#[test]
fn opening_the_menu_reads_no_level_and_shows_the_one_last_brought() {
    use aterm_types::fleet_access::FleetAccess;
    let mut app = App::headless_for_test();
    app.fleet_glance = Some(in_a_fleet());
    app.reread_fleet_access();
    assert_eq!(app.status_glance().access, FleetAccess::ReadOnly);
    let _level = crate::control::FleetAccessForTest::set(FleetAccess::Full);
    app.on_status_menu_opening(None);
    assert_eq!(
        app.status_glance().access,
        FleetAccess::ReadOnly,
        "the open read nothing"
    );
    app.reread_fleet_access();
    assert_eq!(app.status_glance().access, FleetAccess::Full);
}

/// ALL ACTIVITY… RUNS `aterm fleet activity` INTO A TAB, RAISED: the menu's one row that opens
/// anything opens the record its traffic lines are summed from.
#[cfg(target_os = "macos")]
#[test]
fn all_activity_runs_the_fleet_activity_read_raised() {
    let mut app = App::headless_for_test();
    assert!(app.last_cli_plan.is_none());
    app.dispatch_status_action(crate::status_item::StatusAction::AllActivity);
    let plan = app.last_cli_plan.as_ref().expect("the read's plan");
    assert_eq!(plan.argv, ["fleet", "activity"]);
    assert!(plan.raise, "its tab comes to the front");
}

#[test]
fn hand_back_all_lifts_every_person_hold_and_nothing_else() {
    use aterm_types::needs::HoldWord;
    let mut app = App::headless_for_test();
    let wid = app.frontmost_window.expect("the test App's window");
    for _ in 0..2 {
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    }
    let ctx = |app: &App, session: u64| app.pool.get(session).expect("pooled").ctx.clone();
    app.take_over_session(0).expect("the person takes tab 1");
    app.take_over_session(1).expect("and tab 2");
    assert!(crate::fabric::apply_hold_for_test(
        &ctx(&app, 2),
        Some(crate::fabric::Hold {
            reason: "-".into(),
            origin: "local".into(),
        })
    ));
    app.refresh_presence_session(2);
    let held = |app: &App, session: u64| {
        let handle = app
            .store
            .read()
            .unwrap()
            .by_local(session)
            .cloned()
            .expect("registered");
        app.status_session_row(&handle).held
    };
    assert_eq!(
        [0, 1, 2].map(|session| held(&app, session)),
        [
            Some(HoldWord::Person),
            Some(HoldWord::Person),
            Some(HoldWord::Local)
        ]
    );
    assert!(app.any_person_hold(), "the Agents menu's row is live");
    app.hand_back_all();
    assert!(app.person_holds().is_empty(), "every person hold lifted");
    assert!(!app.any_person_hold());
    assert!(ctx(&app, 2).fabric.holds().is_held(), "the client's stands");
    assert_eq!(
        [0, 1, 2].map(|session| held(&app, session)),
        [None, None, Some(HoldWord::Local)]
    );
}

#[cfg(unix)]
#[test]
fn an_adopted_standing_need_keeps_its_age_and_its_read_done() {
    use crate::task_record::TaskState;
    let desk = |app: &mut App, task: Option<&str>| {
        app.push_stub_tab(WindowId(0), crate::stub_session(1));
        let ctx = app.pool.get(1).expect("pooled").ctx.clone();
        match task {
            Some(row) => crate::task_record::seed_carried(&ctx, Some(row)),
            None => {
                for state in [TaskState::Working, TaskState::Done] {
                    let mark = crate::task_record::TaskMark::new(state);
                    crate::task_record::mark_session_task(&ctx, 1, &mark).expect("marked");
                }
            }
        }
        app.on_task_changed(1);
        publish_meta(app, 1, "attention", "wedged on CI");
        ctx
    };
    let mut parent = App::headless_for_test();
    let ctx = desk(&mut parent, None);
    let on_screen = |app: &mut App, on: bool| {
        app.windows
            .get_mut(&WindowId(0))
            .expect("window 0")
            .window_on_screen_for_test = on;
    };
    on_screen(&mut parent, true);
    parent.note_done_seen_looking();
    on_screen(&mut parent, false);
    parent.age_session_need(1);
    assert!(
        parent.presence.need_ages.need_since(1).is_some(),
        "PRECONDITION: the parent saw the need"
    );
    let row = parent
        .status_carry_rows()
        .into_iter()
        .find_map(|(id, row)| (id == 1).then_some(row))
        .expect("session 1's row");
    let task = ctx
        .timeline
        .lock()
        .unwrap()
        .task()
        .expect("the task")
        .carry_row(std::time::Instant::now());

    let successor = |carried: bool| {
        let mut app = App::headless_for_test();
        let ctx = desk(&mut app, Some(&task));
        if carried {
            let mut carry = crate::status_item::StatusCarry::parse(&row);
            carry.need = carry.need.map(|(word, _)| (word, 1_000));
            app.status_carry
                .insert(ctx.self_id.as_str().to_string(), carry);
        }
        app.age_session_need(1);
        app
    };
    // Whether session 1's done is one its owner looked at, as its row says.
    let done_seen = |app: &App| {
        let handle = app
            .store
            .read()
            .unwrap()
            .by_local(1)
            .cloned()
            .expect("registered");
        app.status_session_row(&handle).task.map(|task| task.seen)
    };
    let mut adopted = successor(true);
    assert_eq!(
        adopted.presence.need_ages.need_since(1),
        Some(1_000),
        "the carried age"
    );
    assert_eq!(done_seen(&adopted), Some(true), "read before it");
    assert!(adopted.status_carry.is_empty(), "spent");
    publish_meta(&mut adopted, 1, "attention", "a new question");
    adopted.age_session_need(1);
    assert_ne!(
        adopted.presence.need_ages.need_since(1),
        Some(1_000),
        "a new ask after it ages from its own sight"
    );

    let fresh = successor(false);
    assert_ne!(fresh.presence.need_ages.need_since(1), Some(1_000));
    assert_eq!(done_seen(&fresh), Some(false), "no carry: unread");
}

#[test]
fn go_focuses_the_named_pane_of_a_split_tab() {
    let focused = |app: &App| {
        app.active_tree(WindowId(0))
            .map(crate::pane::PaneTree::focus)
    };
    let looked_at = |app: &App, session: u64| {
        app.notify_suppress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&session)
    };
    let mut app = App::headless_for_test();
    let right = app.split_active_stub_tab(WindowId(0));
    assert_eq!(
        focused(&app),
        Some(right),
        "fixture: the split pane has the keyboard"
    );
    assert!(
        !looked_at(&app, 0),
        "fixture: the needy pane is not looked at"
    );
    assert!(app.focus_session_window(0));
    assert_eq!(focused(&app), Some(0), "the needy pane has the keyboard");
    assert!(looked_at(&app, 0), "and it is looked at");
    assert!(!looked_at(&app, right));

    let mut single = App::headless_for_test();
    assert_eq!(focused(&single), Some(0));
    assert!(single.focus_session_window(0));
    assert_eq!(focused(&single), Some(0), "one pane: unchanged");
    assert!(looked_at(&single, 0));
}

#[test]
fn a_notice_click_goes_to_its_tab_and_pane() {
    use crate::notice_center::{DEFAULT_ACTION, clicked_session};
    let wid = WindowId(0);
    let state = |app: &App| {
        (
            app.windows[&wid].tabs.active,
            app.active_tree(wid).map(crate::pane::PaneTree::focus),
        )
    };
    let own = std::process::id();
    let pid = own.to_string();
    let incarnation = "captured-notice-process";
    let instance = Some(incarnation);
    let mut app = App::headless_for_test();
    let right = app.split_active_stub_tab(wid);
    assert_eq!(state(&app), (0, Some(right)), "fixture: [0 | right]");
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    let elsewhere = state(&app);
    assert_eq!(elsewhere.0, 1, "fixture: the owner is on another tab");
    let _ = take_raised_windows();

    let dismissed = "com.apple.UNNotificationDismissActionIdentifier";
    assert_eq!(
        clicked_session(dismissed, Some("0"), Some(&pid), instance, own, incarnation),
        None
    );
    assert_eq!(
        clicked_session(
            DEFAULT_ACTION,
            Some("0"),
            Some("1"),
            instance,
            own,
            incarnation
        ),
        None
    );
    let gone = clicked_session(
        DEFAULT_ACTION,
        Some("999"),
        Some(&pid),
        instance,
        own,
        incarnation,
    )
    .expect("a click on this process's notice names its session");
    app.go_to_session(gone);
    assert_eq!(state(&app), elsewhere, "a session gone moves nothing");
    assert_eq!(take_raised_windows(), Vec::new(), "and raises nothing");

    let clicked = clicked_session(
        DEFAULT_ACTION,
        Some("0"),
        Some(&pid),
        instance,
        own,
        incarnation,
    );
    app.go_to_session(clicked.expect("the notice named session 0"));
    assert_eq!(state(&app), (0, Some(0)), "its tab, and its pane");
    assert_eq!(take_raised_windows(), vec![wid], "its window raised");
}
