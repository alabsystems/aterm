// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_window::event::InputOrigin;

use super::*;
use crate::presence::ChipLevel;
use crate::task_record::{TaskMark, TaskState, mark_session_task};
use crate::{App, WindowId};
use std::sync::Arc;

fn app_with_stub() -> (App, WindowId, u64, Arc<crate::SessionCtx>) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(sid));
    let ctx = app.pool.get(sid).expect("stub pooled").ctx.clone();
    assert_eq!(
        app.focused_session_id(wid),
        Some(sid),
        "the stub is the front session"
    );
    (app, wid, sid, ctx)
}

fn second_tab(app: &mut App, wid: WindowId) -> (u64, Arc<crate::SessionCtx>) {
    let sid = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(sid));
    let ctx = app.pool.get(sid).expect("stub pooled").ctx.clone();
    (sid, ctx)
}

fn ask(ctx: &crate::SessionCtx, text: Option<&str>) {
    ctx.meta.lock().unwrap().attention = text.map(str::to_string);
}

fn task(ctx: &crate::SessionCtx, local: u64, state: TaskState) {
    let mark = TaskMark {
        why: (state == TaskState::Stalled).then_some("startup"),
        ..TaskMark::new(state)
    };
    mark_session_task(ctx, local, &mark).expect("unfenced");
}

#[test]
fn an_ask_and_a_needs_you_line_each_publish_a_need() {
    let (mut app, _wid, sid, ctx) = app_with_stub();
    for text in ["deploy to prod?", "NEEDS YOU: a routine decision"] {
        ask(&ctx, Some(text));
        app.refresh_presence_session(sid);
        assert!(ctx.timeline.lock().unwrap().need().need.is_some());
    }
}

/// A session in two tabs of a window is one projection there a refresh: the list
/// [`Desk::windows_showing`] hands `refresh_session` names that window once, as it names each
/// of two windows showing the session. NEGATIVE CONTROL: without the walk's `contains`, the
/// tabs repeat the window the focused walk already named.
#[test]
fn a_session_in_two_tabs_of_a_window_is_projected_there_once() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    app.switch_tab_in(wid, 0);
    let twin = app
        .open_active_session_in_new_window_logical()
        .expect("share the session into a second window");
    assert_eq!(Desk::windows_showing(&app, 0), [wid, twin], "once at each");

    let _ = app.migrate_active_tab_to_next_window();
    assert_eq!(app.windows.len(), 1, "the emptied share window closes");
    assert_eq!(app.tabs_viewing_session(0).len(), 2, "two tabs show it");
    assert_eq!(Desk::windows_showing(&app, 0), [wid], "the window, once");
}

#[test]
fn a_hundred_verdict_flips_regrid_no_pane() {
    let (mut app, wid, first, first_ctx) = app_with_stub();
    let _ = second_tab(&mut app, wid);
    let rows = app.chrome_rows(wid);
    let _ = take_chrome_regrids_applied();
    for i in 0..100 {
        ask(&first_ctx, (i % 2 == 0).then_some("approve?"));
        app.refresh_presence_session(first);
    }
    assert!(
        take_chrome_regrids_applied().is_empty(),
        "no flip re-gridded a pane"
    );
    assert_eq!(app.chrome_rows(wid), rows);
    app.set_tab_strip_rows(app.tab_strip_rows + 1, Instant::now());
    assert!(
        take_chrome_regrids_applied()
            .iter()
            .any(|(w, r)| *w == wid && *r == rows + 1),
        "the strip's new row is a re-grid the probe records"
    );
}

#[test]
fn a_presence_refresh_never_classifies() {
    let (mut app, _wid, sid, ctx) = app_with_stub();
    let calls = presence::classifier_calls();
    for i in 0..1000 {
        ask(&ctx, (i % 3 == 0).then_some("approve?"));
        app.refresh_presence_session(sid);
    }
    assert_eq!(
        presence::classifier_calls() - calls,
        0,
        "presence folds the published verdict; it never reads the screen"
    );
}

#[test]
fn each_chip_carries_its_tabs_load_until_the_bars_are_off() {
    let (mut app, wid, _first, _ctx) = app_with_stub();
    let (second, _second_ctx) = second_tab(&mut app, wid);
    let mut snap = crate::stats_deck::tests::snapshot();
    snap.tabs[0].key = second;
    snap.tabs[0].scope.cores.level = 5.0;
    app.vitals.snapshot = Some(std::sync::Arc::new(snap));
    let loads = |app: &App| {
        let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
            .collect();
        app.stamp_presence_chips(wid, &mut metadata);
        metadata.iter().map(|m| m.load).collect::<Vec<_>>()
    };
    let lit = loads(&app);
    assert_eq!(lit.last(), Some(&128), "five of ten cores: half the meter");
    assert!(
        lit[..lit.len() - 1].iter().all(|&l| l == 0),
        "an unmeasured tab draws no meter: {lit:?}"
    );
    assert_eq!(
        app.tab_strip_metadata(wid).last().map(|m| m.load),
        Some(128)
    );
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    assert!(
        loads(&app).iter().all(|&l| l == 0),
        "the bars off, no meters"
    );
}

#[test]
fn the_tab_marks_read_the_verdict_and_the_done_dot_clears_once_seen() {
    let (mut app, wid, first, first_ctx) = app_with_stub();
    let (second, second_ctx) = second_tab(&mut app, wid);
    let marks = |app: &App| {
        let ws = &app.windows[&wid];
        let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
            .tab_set
            .tabs()
            .iter()
            .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
            .collect();
        app.stamp_presence_chips(wid, &mut metadata);
        metadata[metadata.len() - 2..]
            .iter()
            .map(|m| m.attention)
            .collect::<Vec<_>>()
    };
    app.windows.get_mut(&wid).unwrap().window_on_screen_for_test = true;
    assert_eq!(app.frontmost_window, Some(wid));
    ask(&first_ctx, Some("approve?"));
    app.refresh_presence_session(first);
    assert_eq!(marks(&app), [ChipLevel::Wait, ChipLevel::Off]);
    ask(&first_ctx, None);
    task(&first_ctx, first, TaskState::Done);
    app.on_task_changed(first);
    assert_eq!(
        marks(&app),
        [ChipLevel::Done, ChipLevel::Off],
        "a finished task behind is the dot"
    );
    task(&second_ctx, second, TaskState::Done);
    app.on_task_changed(second);
    assert_eq!(marks(&app)[1], ChipLevel::Off);
    let tabs = app.windows[&wid].tab_set.tabs().len();
    app.switch_tab_in(wid, tabs - 2);
    assert_eq!(marks(&app), [ChipLevel::Off, ChipLevel::Off]);
    app.switch_tab_in(wid, tabs - 1);
    assert_eq!(marks(&app), [ChipLevel::Off, ChipLevel::Off]);
    task(&first_ctx, first, TaskState::Stalled);
    app.on_task_changed(first);
    assert!(matches!(marks(&app)[0], ChipLevel::Stop(_)));
}

#[cfg(unix)]
#[test]
fn a_done_the_owner_saw_stays_seen_across_a_seamless_update() {
    let behind = |app: &App, wid: WindowId| {
        let ws = &app.windows[&wid];
        let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
            .tab_set
            .tabs()
            .iter()
            .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
            .collect();
        app.stamp_presence_chips(wid, &mut metadata);
        [metadata[1].attention, metadata[2].attention]
    };
    let (mut parent, wid, seen, seen_ctx) = app_with_stub();
    let (unseen, unseen_ctx) = second_tab(&mut parent, wid);
    let _ = second_tab(&mut parent, wid);
    parent
        .windows
        .get_mut(&wid)
        .unwrap()
        .window_on_screen_for_test = true;
    parent.frontmost_window = Some(wid);
    for (ctx, local) in [(&seen_ctx, seen), (&unseen_ctx, unseen)] {
        task(ctx, local, TaskState::Done);
        parent.on_task_changed(local);
    }
    parent.switch_tab_in(wid, 1);
    parent.switch_tab_in(wid, 3);
    assert_eq!(
        behind(&parent, wid),
        [ChipLevel::Off, ChipLevel::Done],
        "PRECONDITION: one seen, one not"
    );
    let rows = parent.status_carry_rows();
    let carried = |local: u64, ctx: &crate::SessionCtx| {
        let task = ctx
            .timeline
            .lock()
            .unwrap()
            .task()
            .expect("a task")
            .carry_row(std::time::Instant::now());
        let status = rows
            .iter()
            .find_map(|(id, row)| (*id == local).then(|| row.clone()));
        (task, status)
    };
    let (seen_task, seen_row) = carried(seen, &seen_ctx);
    let (unseen_task, unseen_row) = carried(unseen, &unseen_ctx);
    assert!(
        unseen_row.is_none(),
        "nothing the status item knows of the unseen one"
    );
    let (mut successor, wid, first, first_ctx) = app_with_stub();
    let (second, second_ctx) = second_tab(&mut successor, wid);
    let _ = second_tab(&mut successor, wid);
    for (local, ctx, task_row, status_row) in [
        (first, &first_ctx, seen_task, seen_row),
        (second, &second_ctx, unseen_task, unseen_row),
    ] {
        crate::task_record::seed_carried(ctx, Some(&task_row));
        App::register_session(
            &successor.store,
            successor.pool.get(local).expect("pooled"),
            None,
        );
        if let Some(row) = status_row {
            successor.status_carry.insert(
                ctx.self_id.as_str().to_string(),
                crate::status_item::StatusCarry::parse(&row),
            );
        }
    }
    successor.refresh_presence_all_windows();
    successor.age_session_need(first);
    successor.age_session_need(second);
    assert_eq!(
        behind(&successor, wid),
        [ChipLevel::Off, ChipLevel::Done],
        "the look crossed; the unlooked dot stands"
    );
}

/// AN EXITED WORKER'S END IS CLEARED BY A LOOK AS A DONE ONE'S IS, with no dot on its tab: its
/// Done for you row says it, and the look covers it. Going to it from that row clears it as well.
/// NEGATIVE CONTROL: where nobody looks it stays unread.
#[test]
fn an_exited_workers_end_is_seen_by_a_look_and_by_going_to_it() {
    use crate::task_record::WHY_EXIT;
    let seen_row = |app: &App, session: u64| app.done_seen.get(&session).copied();
    let (mut app, wid, _first, _) = app_with_stub();
    let (second, second_ctx) = second_tab(&mut app, wid);
    let tabs = app.windows[&wid].tab_set.tabs().len();
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.window_on_screen_for_test = true;
        ws.occluded = true;
    }
    app.frontmost_window = Some(wid);
    let exit = TaskMark {
        why: Some(WHY_EXIT),
        ..TaskMark::new(TaskState::Exited)
    };
    mark_session_task(&second_ctx, second, &exit).expect("unfenced");
    app.on_task_changed(second);
    app.refresh_presence_all_windows();
    let entered = second_ctx
        .timeline
        .lock()
        .unwrap()
        .task()
        .expect("a task")
        .entered;
    assert_eq!(app.task_exited.get(&second), Some(&entered));
    assert_eq!(seen_row(&app, second), None, "nobody looked");
    app.on_occlusion_changed(wid, false);
    app.switch_tab_in(wid, tabs - 1);
    assert_eq!(seen_row(&app, second), Some(entered), "a look");

    let (mut app, wid, _first, _) = app_with_stub();
    let (second, second_ctx) = second_tab(&mut app, wid);
    mark_session_task(&second_ctx, second, &exit).expect("unfenced");
    app.on_task_changed(second);
    app.note_session_done_seen(second);
    let entered = second_ctx
        .timeline
        .lock()
        .unwrap()
        .task()
        .expect("a task")
        .entered;
    assert_eq!(seen_row(&app, second), Some(entered), "going to it");
}

#[test]
fn a_task_done_where_nobody_looks_keeps_its_dot() {
    let (mut app, wid, _first, _) = app_with_stub();
    let (second, second_ctx) = second_tab(&mut app, wid);
    let tabs = app.windows[&wid].tab_set.tabs().len();
    let dot = |app: &App| {
        let ws = &app.windows[&wid];
        let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
            .tab_set
            .tabs()
            .iter()
            .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
            .collect();
        app.stamp_presence_chips(wid, &mut metadata);
        metadata[tabs - 1].attention
    };
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.window_on_screen_for_test = true;
        ws.occluded = true;
    }
    task(&second_ctx, second, TaskState::Done);
    app.on_task_changed(second);
    app.refresh_presence_all_windows();
    app.switch_tab_in(wid, tabs - 2);
    assert_eq!(dot(&app), ChipLevel::Done, "nobody looked");
    app.switch_tab_in(wid, tabs - 1);
    app.on_occlusion_changed(wid, false);
    app.switch_tab_in(wid, tabs - 2);
    assert_eq!(dot(&app), ChipLevel::Off, "seen once revealed");
}

#[test]
fn a_split_panes_task_done_in_front_is_seen() {
    let behind = |occluded: bool| {
        let (mut app, wid, worker, worker_ctx) = app_with_stub();
        let other = app.split_active_stub_tab(wid);
        assert_eq!(app.focused_session_id(wid), Some(other), "the new pane");
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.window_on_screen_for_test = true;
            ws.occluded = occluded;
        }
        app.frontmost_window = Some(wid);
        task(&worker_ctx, worker, TaskState::Done);
        app.on_task_changed(worker);
        let split = app.windows[&wid].tab_set.tabs().len() - 1;
        let _ = second_tab(&mut app, wid);
        let ws = &app.windows[&wid];
        let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
            .tab_set
            .tabs()
            .iter()
            .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
            .collect();
        app.stamp_presence_chips(wid, &mut metadata);
        metadata[split].attention
    };
    assert_eq!(behind(false), ChipLevel::Off, "looked at in front");
    assert_eq!(behind(true), ChipLevel::Done, "CONTROL: nobody looked");
}

#[test]
fn a_team_chip_lands_in_the_window_clicked() {
    let shared = |via_chip: bool| {
        let (mut app, w0, manager, manager_ctx) = app_with_stub();
        app.frontmost_window = Some(w0);
        let w1 = app
            .open_active_session_in_new_window_logical()
            .expect("the share");
        let (worker, worker_ctx) = second_tab(&mut app, w1);
        assert!(w0 < w1, "PRECONDITION: the first window showing it is w0");
        let mark = TaskMark {
            from: Some(manager_ctx.self_id.as_str().to_string()),
            ..TaskMark::new(TaskState::Working)
        };
        mark_session_task(&worker_ctx, worker, &mark).expect("unfenced");
        app.on_task_changed(worker);
        assert_eq!(app.focused_session_id(w1), Some(worker));
        if via_chip {
            app.note_claude_team(w1, worker);
            app.press_claude_chip(
                w1,
                worker,
                crate::claude_lights::Chip::Team,
                std::time::Instant::now(),
            );
        } else {
            let _ = app.focus_session_window(manager);
        }
        (manager, app.focused_session_id(w1))
    };
    let (manager, focused) = shared(true);
    assert_eq!(
        focused,
        Some(manager),
        "the window clicked shows the manager"
    );
    let (manager, focused) = shared(false);
    assert_ne!(focused, Some(manager), "control: w1 stays where it was");
}

#[test]
fn a_hold_is_said_on_the_hover_and_nowhere_else() {
    let (mut app, wid, sid, ctx) = app_with_stub();
    let _ = second_tab(&mut app, wid);
    app.refresh_presence_session(sid);
    assert_eq!(app.presence_hover(sid, None), None, "nothing to say");
    let held = |app: &mut App, origin: &str| {
        assert!(crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "-".into(),
                origin: origin.into(),
            })
        ));
        app.refresh_presence_session(sid);
        assert_eq!(
            app.presence.slot(sid).map(|s| s.chip()),
            Some(ChipLevel::Off),
            "{origin}: no mark"
        );
        let hover = app.presence_hover(sid, None).expect("the hold is said");
        assert_eq!(hover.lines().count(), 1, "{hover}");
        assert!(crate::fabric::apply_hold_for_test(&ctx, None));
        hover
    };
    let local = held(&mut app, "local");
    let bridge = held(&mut app, "fleet");
    assert_ne!(local, bridge, "each origin says its own");
}

#[test]
fn a_managers_team_chip_opens_the_overview_at_it() {
    use crate::claude_lights::Chip;
    use crate::overview::{OverviewAct, OverviewItem};
    let (mut app, wid, manager, manager_ctx) = app_with_stub();
    let (worker, worker_ctx) = second_tab(&mut app, wid);
    let (stray, stray_ctx) = second_tab(&mut app, wid);
    let from = |sid: &str| TaskMark {
        from: Some(sid.to_string()),
        ..TaskMark::new(TaskState::Working)
    };
    let manager_sid = manager_ctx.self_id.as_str().to_string();
    mark_session_task(&worker_ctx, worker, &from(&manager_sid)).expect("unfenced");
    app.on_task_changed(worker);
    mark_session_task(&stray_ctx, stray, &from("s-not-here")).expect("unfenced");
    app.on_task_changed(stray);
    let _ = crate::app_tabs::take_raised_windows();

    app.note_claude_team(wid, stray);
    app.press_claude_chip(wid, stray, Chip::Team, std::time::Instant::now());
    assert!(app.windows[&wid].overview().is_none(), "control: no card");
    assert_eq!(crate::app_tabs::take_raised_windows(), Vec::new());

    app.note_claude_team(wid, manager);
    app.press_claude_chip(wid, manager, Chip::Team, std::time::Instant::now());
    assert_eq!(crate::app_tabs::take_raised_windows(), vec![wid]);
    assert_eq!(app.focused_session_id(wid), Some(stray), "no tab moved");
    let card = app
        .windows
        .get_mut(&wid)
        .and_then(|ws| ws.overview_mut())
        .expect("the card is open");
    assert!(matches!(
        card.selected_item(),
        Some(OverviewItem::Manages { .. })
    ));
    assert_eq!(card.activate(), OverviewAct::Raise(manager));
}

#[test]
fn workers_name_their_manager_on_the_chip_the_strip_and_the_press() {
    use crate::claude_lights::TeamFace;
    let (mut app, wid, manager, manager_ctx) = app_with_stub();
    manager_ctx.meta.lock().unwrap().role = Some("lab-sweep".into());
    let (bench, bench_ctx) = second_tab(&mut app, wid);
    let (fmt, fmt_ctx) = second_tab(&mut app, wid);
    let (solo, solo_ctx) = second_tab(&mut app, wid);
    let from = manager_ctx.self_id.as_str().to_string();
    for (local, ctx) in [(bench, &bench_ctx), (fmt, &fmt_ctx)] {
        let mark = TaskMark {
            from: Some(from.clone()),
            ..TaskMark::new(TaskState::Working)
        };
        mark_session_task(ctx, local, &mark).expect("unfenced");
        app.on_task_changed(local);
    }
    task(&solo_ctx, solo, TaskState::Working);
    app.on_task_changed(solo);
    assert_eq!(
        app.team_face(manager),
        Some(TeamFace::Manager {
            mark: std::sync::Arc::from("\u{21c9}2"),
            workers: 2
        })
    );
    assert!(matches!(
        app.team_face(bench),
        Some(TeamFace::Worker { manager: Some(m), .. }) if m == manager
    ));
    assert_eq!(app.team_face(solo), None, "control: from=ctl is no team");
    app.refresh_window_tabs(wid);
    let shown = |app: &App, session: u64| {
        let ws = &app.windows[&wid];
        let tab = ws
            .tab_set
            .tabs()
            .iter()
            .find(|t| {
                app.view_store
                    .get(t.focus)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    == Some(session)
            })
            .expect("the session's tab")
            .id;
        ws.tab_chrome_titles_by_tab[&tab].1.clone()
    };
    assert!(
        shown(&app, bench).starts_with('\u{21b3}'),
        "{}",
        shown(&app, bench)
    );
    assert!(
        shown(&app, manager).ends_with("\u{21c9}2"),
        "{}",
        shown(&app, manager)
    );
    assert!(!shown(&app, solo).contains('\u{21b3}'));
    assert!(
        !app.tab_label_of_session(manager).contains('\u{21c9}'),
        "the tab's own name keeps no mark"
    );
    app.note_claude_team(wid, bench);
    app.press_claude_chip(
        wid,
        bench,
        crate::claude_lights::Chip::Team,
        std::time::Instant::now(),
    );
    assert_eq!(app.focused_session_id(wid), Some(manager));
    assert!(app.detach_session_view(fmt), "the last view of it");
    assert!(matches!(
        app.team_face(manager),
        Some(TeamFace::Manager { workers: 1, .. })
    ));
}

#[test]
fn a_panic_inside_a_chrome_regrid_batch_ends_the_batch() {
    let mut app = App::headless_for_test();
    let _ = take_chrome_regrids_applied();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.with_chrome_regrid_batch(|app| {
            app.regrid_for_chrome_rows();
            assert!(take_chrome_regrids_applied().is_empty(), "recorded");
            panic!("inside the batch");
        })
    }));
    assert!(caught.is_err(), "the panic resumed");
    assert!(app.chrome_regrid_batch.is_none(), "the batch ended");
    assert!(
        !take_chrome_regrids_applied().is_empty(),
        "what it recorded was applied"
    );
    app.regrid_for_chrome_rows();
    assert!(
        !take_chrome_regrids_applied().is_empty(),
        "the next re-grid applies at once"
    );
}

#[test]
fn a_calm_key_reads_the_story_on_the_wire() {
    let (mut app, wid, sid, _ctx) = app_with_stub();
    app.refresh_presence_session(sid);
    app.tell_story(sid, presence::StoryVerb::Chose)
        .expect("the stub is held");
    assert_eq!(app.presence_session_level(sid), presence::Level::Story);
    app.note_human_acted_on(wid, sid);
    assert_eq!(app.presence_session_level(sid), presence::Level::Quiet);
}

#[test]
fn posted_window_keys_keep_activity_without_refreshing_person_presence() {
    use aterm_window::event::{DeviceId, ElementState, KeyEvent, WindowEvent};
    use aterm_window::keyboard::{Key, KeyCode, KeyLocation, PhysicalKey, SmolStr};
    let mut app = App::headless_for_test();
    app.user_input_readable = || false;
    let event = WindowEvent::KeyboardInput {
        device_id: DeviceId::dummy(),
        event: KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::KeyX),
            Key::Character("x".into()),
            Some(SmolStr::new("x")),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        ),
        is_synthetic: false,
    };
    assert!(app.last_person_input_at.is_none());
    app.note_window_input_arrival(&event, InputOrigin::Posted);
    assert!(
        app.last_keystroke_at.is_some(),
        "automation still counts as input activity"
    );
    assert!(app.last_person_input_at.is_none());
    assert!(!app.person_present_within(Duration::from_secs(60), Instant::now()));
    app.last_keystroke_at = None;
    app.note_window_input_arrival(&event, InputOrigin::Person);
    assert!(app.last_keystroke_at.is_some());
    let person = app
        .last_person_input_at
        .expect("a physical key establishes presence");
    assert!(app.person_present_within(Duration::from_secs(60), Instant::now()));
    app.note_window_input_arrival(&event, InputOrigin::Posted);
    assert_eq!(
        app.last_person_input_at,
        Some(person),
        "a later posted key preserves the actual person's stamp"
    );
    app.last_person_input_at = None;
    app.note_window_input_arrival(&event, InputOrigin::Relayed);
    assert!(
        app.last_person_input_at.is_some(),
        "a key another process posted (Screen Sharing, an on-screen keyboard) is presence"
    );
}

/// A CONTROL CLIENT'S PRESS READS NO STORY: the story watermark records what the person
/// saw, so a key an `hwkey` burst posted (the winit key arm, under the controller mark) and a
/// `pointer` click (under the controller's gesture) leave the session's story standing; the
/// person's key by the same arm reads it.
/// NEGATIVE CONTROL: without the guard in `note_human_acted_on`, the posted key read the
/// story (Quiet).
#[test]
fn a_control_clients_press_reads_no_story() {
    use aterm_window::event::{ElementState, KeyEvent, MouseButton};
    use aterm_window::keyboard::{Key, KeyCode, KeyLocation, PhysicalKey, SmolStr};
    let (mut app, wid, sid, _ctx) = app_with_stub();
    app.refresh_presence_session(sid);
    app.tell_story(sid, presence::StoryVerb::Chose)
        .expect("the stub is held");
    let key = || {
        KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::KeyX),
            Key::Character(SmolStr::new("x")),
            Some(SmolStr::new("x")),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        )
    };
    app.dispatch_hw_key(wid, key(), InputOrigin::Posted);
    assert_eq!(
        app.presence_session_level(sid),
        presence::Level::Story,
        "a posted key"
    );
    app.with_gesture_source(true, |app| {
        app.mouse_input_then_fold(wid, ElementState::Pressed, MouseButton::Left);
        app.mouse_input_then_fold(wid, ElementState::Released, MouseButton::Left);
    });
    assert_eq!(
        app.presence_session_level(sid),
        presence::Level::Story,
        "a pointer click"
    );
    app.dispatch_hw_key(wid, key(), InputOrigin::Person);
    assert_eq!(
        app.presence_session_level(sid),
        presence::Level::Quiet,
        "the person's key reads it"
    );
}

#[test]
fn a_proven_self_claim_with_resume_limits_reads_a_usage_wall_as_note() {
    use crate::presence::Level;
    crate::fabric::with_link_reset(|| {
        let (mut app, _wid, sid, ctx) = app_with_stub();
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(42);
            timeline.set_program(42, Some("claude".into()));
        }
        let rows = aterm_phase::prompt::fixtures::screen(
            aterm_phase::prompt::fixtures::SESSION_LIMIT_THEN_LOCAL_COMMANDS_MEASURED,
        );
        let step = app.session_status.agent_observe(
            sid,
            crate::control::ScreenGen { epoch: 0, seq: 1 },
            Some(rows),
            crate::presence::Cursor::Unknown,
            Some("claude".into()),
            42,
            false,
            "",
            Instant::now(),
        );
        let (word, detail, subject, reader) = step.publish.expect("the wall was read");
        ctx.timeline.lock().unwrap().publish_agent(
            word,
            detail,
            subject,
            reader,
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 0, seq: 1 },
                fp: 1,
            },
        );
        let facts = app.presence_facts(sid).unwrap();
        assert!(
            matches!(
                facts.agent.as_ref().map(|a| &a.phase),
                Some(presence::AgentPhase::Wall {
                    kind: aterm_phase::WallKind::UsageSession,
                    ..
                })
            ),
            "precondition: a usage wall"
        );
        assert_eq!(facts.wall_answer, presence::WallAnswer::Nobody, "no claim");
        app.harness = Some(crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        ));
        let level = |app: &mut App| {
            app.refresh_presence_session(sid);
            let slot = app.presence.slot(sid).expect("a refreshed slot");
            (
                app.presence_facts(sid).unwrap().wall_answer,
                slot.level(slot.story_seq),
            )
        };
        crate::session_timeline::claim_supervisor(
            &ctx,
            "codex-alab@4242",
            Some(7),
            None,
            crate::metrics::now_us(),
        )
        .unwrap();
        assert_eq!(
            level(&mut app),
            (presence::WallAnswer::Nobody, Level::Limited),
            "an unproven self-claim"
        );
        crate::session_timeline::release_supervisor(
            &ctx,
            None,
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        );
        crate::session_timeline::claim_supervisor_as(
            &ctx,
            "codex-alab@4242",
            Some(7),
            None,
            Some(crate::self_claim::Proven {
                pid: 4242,
                start: "test".into(),
            }),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .unwrap();
        assert!(
            ctx.meta
                .lock()
                .unwrap()
                .supervisor_proven(crate::metrics::now_us())
        );
        assert_eq!(
            level(&mut app),
            (presence::WallAnswer::Nobody, Level::Limited),
            "Claude Code's wall under a codex-alab claim is nobody's"
        );
        let in_front = |pgid: i32, program: &str| {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(pgid);
            timeline.set_program(pgid, Some(program.into()));
        };
        in_front(4242, "codex");
        assert_eq!(
            level(&mut app),
            (presence::WallAnswer::Nobody, Level::Limited),
            "a reading made under Claude Code is not the claimant's"
        );
        app.session_status.stand_in_reading_program(sid, "codex");
        for claimant in ["codex-alab", "codex"] {
            in_front(4242, claimant);
            assert_eq!(
                level(&mut app),
                (presence::WallAnswer::Harness, Level::Note),
                "{claimant}: resumes by itself"
            );
        }
        in_front(77, "codex");
        assert_eq!(
            level(&mut app),
            (presence::WallAnswer::Nobody, Level::Limited),
            "a codex the proven pid does not lead"
        );
        in_front(4242, "codex-alab");
        for toml in [
            "[harness]\nresume_limits = false\n",
            "[harness]\nenabled = false\n",
        ] {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read(toml),
            );
            assert_eq!(
                level(&mut app),
                (presence::WallAnswer::Nobody, Level::Limited),
                "{toml:?}"
            );
        }
        if let Some(host) = app.harness.take() {
            host.shutdown_and_join();
        }
    });
}

/// A WALL THE LOOP TYPES NOTHING INTO IS NOBODY'S: under this host's own claim, Claude Code's
/// usage wall is the harness's to resume, and a wall of a program no act of the loop reaches
/// ([`aterm_phase::Program::supervisable`]) is nobody's, so its need is a person's rather
/// than a `waiting:` nothing ends. Codex is one: the host types nothing into it (its worker
/// watches beside codex-alab and never takes the claim), so the same standing wall, once a
/// Codex leads the tab under the host's claim, reads nobody's where it once read the
/// harness's; that screen-level step is the one the host's per-agent gate decides. Grok
/// Build's weekly limit reads nobody's on the screen too, but there two filters ahead of the
/// host decide it whatever the host says (its reading is no evidence, so the presence shows
/// no wall, and [`crate::harness_host::agent_of`] drops a program with no
/// `[harness.<agent>]` table), so for it the host's own answer is asked directly: it is what
/// stays honest once either filter moves.
#[test]
fn under_the_host_a_grok_limit_wall_is_nobodys_and_claudes_resumes() {
    crate::fabric::with_link_reset(|| {
        let (mut app, _wid, sid, ctx) = app_with_stub();
        app.harness = Some(crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        ));
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .unwrap();
        let mut seq = 0;
        let mut wall = |app: &mut App, program: &str, screen: &str| {
            seq += 1;
            {
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.note_foreground_group(42);
                timeline.set_program(42, Some(program.into()));
            }
            let step = app.session_status.agent_observe(
                sid,
                crate::control::ScreenGen { epoch: 0, seq },
                Some(aterm_phase::prompt::fixtures::screen(screen)),
                crate::presence::Cursor::Unknown,
                Some(program.into()),
                42,
                false,
                "",
                Instant::now(),
            );
            let (word, detail, subject, reader) = step.publish.expect("the wall was read");
            ctx.timeline.lock().unwrap().publish_agent(
                word,
                detail,
                subject,
                reader,
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 0, seq },
                    fp: seq,
                },
            );
            let facts = app.presence_facts(sid).unwrap();
            let kind = match facts.agent.as_ref().map(|a| &a.phase) {
                Some(presence::AgentPhase::Wall { kind, .. }) => Some(*kind),
                _ => None,
            };
            (kind, facts.wall_answer)
        };
        assert_eq!(
            wall(
                &mut app,
                "claude",
                aterm_phase::prompt::fixtures::SESSION_LIMIT_THEN_LOCAL_COMMANDS_MEASURED,
            ),
            (
                Some(aterm_phase::WallKind::UsageSession),
                presence::WallAnswer::Harness
            ),
            "Claude Code's usage wall under the host's claim resumes"
        );
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(43);
            timeline.set_program(43, Some("codex".into()));
        }
        let facts = app.presence_facts(sid).unwrap();
        assert!(
            matches!(
                facts.agent.as_ref().map(|a| &a.phase),
                Some(presence::AgentPhase::Wall {
                    kind: aterm_phase::WallKind::UsageSession,
                    ..
                })
            ),
            "precondition: the wall still stands"
        );
        assert_eq!(
            facts.wall_answer,
            presence::WallAnswer::Nobody,
            "a wall while a Codex leads the tab under the host's claim is nobody's"
        );
        let (_, grok) = wall(
            &mut app,
            "grok",
            aterm_phase::grok::fixtures::WALL_WEEKLY_LIMIT,
        );
        assert_eq!(
            grok,
            presence::WallAnswer::Nobody,
            "Grok Build's weekly limit under the host's claim is nobody's"
        );
        let host = app.harness.take().expect("the host");
        for kind in [
            aterm_phase::WallKind::UsageSession,
            aterm_phase::WallKind::UsageWeekly,
            aterm_phase::WallKind::Spend,
            aterm_phase::WallKind::Overloaded,
            aterm_phase::WallKind::Auth,
        ] {
            for (agent, expected) in [
                (aterm_phase::Program::Grok, presence::WallAnswer::Nobody),
                (
                    aterm_phase::Program::Kimi,
                    if kind == aterm_phase::WallKind::Auth {
                        presence::WallAnswer::Nobody
                    } else {
                        presence::WallAnswer::Harness
                    },
                ),
                (aterm_phase::Program::Codex, presence::WallAnswer::Nobody),
            ] {
                assert_eq!(
                    host.wall_answer(agent, kind, true),
                    expected,
                    "{agent:?} {kind:?}"
                );
            }
        }
        assert_eq!(
            host.wall_answer(
                aterm_phase::Program::Claude,
                aterm_phase::WallKind::UsageWeekly,
                true
            ),
            presence::WallAnswer::Harness
        );
        assert_eq!(
            host.wall_answer(
                aterm_phase::Program::Claude,
                aterm_phase::WallKind::Auth,
                false
            ),
            presence::WallAnswer::Harness,
            "Claude Code's lost login is carried"
        );
        assert_eq!(
            host.wall_answer(
                aterm_phase::Program::Agy,
                aterm_phase::WallKind::Auth,
                false
            ),
            presence::WallAnswer::Nobody,
            "agy's lost login is not"
        );
        host.shutdown_and_join();
    });
}

#[test]
fn status_hand_pid_names_the_leases_kernel_attested_taker() {
    crate::fabric::with_link_reset(|| {
        let (mut app, _wid, sid, ctx) = app_with_stub();
        let own = crate::harness_host::holder();
        let me = std::process::id();
        let now_us = crate::metrics::now_us();
        let drive = |holder_pid: Option<u32>, expires_us: u64| crate::Lease::Drive {
            holder: own.clone(),
            expires_us,
            conn: None,
            hard: true,
            here: false,
            holder_pid,
        };
        let mut read = |lease: Option<crate::Lease>| {
            *ctx.turn_lease.lock().unwrap() = lease;
            app.on_presence_wake(&ctx.self_id);
            app.presence_status_tail(sid)
        };
        let hand = format!("hand=lease:{}", crate::control::pct_encode(&own));
        let tail = read(Some(drive(Some(me), now_us + 60_000_000)));
        assert!(
            tail.starts_with(&format!("{hand} hand_pid={me} ")),
            "{tail}"
        );
        let forger = me + 1;
        let tail = read(Some(drive(Some(forger), now_us + 60_000_000)));
        assert!(
            tail.starts_with(&format!("{hand} hand_pid={forger} ")),
            "{tail}"
        );
        let tail = read(Some(drive(None, now_us + 60_000_000)));
        assert!(tail.starts_with(&format!("{hand} hand_pid=- ")), "{tail}");
        let tail = read(Some(drive(Some(me), now_us.saturating_sub(1))));
        assert!(tail.starts_with("hand=- hand_pid=- "), "lapsed: {tail}");
        let tail = read(Some(crate::Lease::Turn {
            id: 9,
            driver: None,
            typing: false,
        }));
        assert!(tail.starts_with("hand=turn:9 hand_pid=- "), "{tail}");
        let tail = read(None);
        assert!(tail.starts_with("hand=- hand_pid=- "), "{tail}");
    });
}

#[test]
fn a_background_codex_turn_end_puts_the_dot_on_its_tab() {
    crate::fabric::with_link_reset(|| {
        let (mut app, wid, sid_a, _ctx_a) = app_with_stub();
        let ia = app.windows[&wid].tab_set.tabs().len() - 1;
        let sid_b = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid_b));
        let ib = ia + 1;
        assert_eq!(
            app.focused_session_id(wid),
            Some(sid_b),
            "A is in the background"
        );
        let report = |bytes: &[u8]| {
            let mut term = aterm_core::terminal::Terminal::new(24, 80);
            term.process(bytes);
            term.program_state()
        };
        let working = report(b"\x1b]7501;state=working:app=codex\x1b\\");
        let mut done = report(b"\x1b]7501;state=done:app=codex\x1b\\");
        done.seq = working.seq + 1;
        let chips = |app: &App| {
            let mut m: Vec<_> = app.windows[&wid]
                .tab_set
                .tabs()
                .iter()
                .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
                .collect();
            app.stamp_presence_chips(wid, &mut m);
            (m[ia].attention, m[ib].attention)
        };
        let now = Instant::now();
        for sid in [sid_a, sid_b] {
            app.session_status
                .observe_program_state(sid, working, true, false, now);
            app.refresh_presence_session(sid);
        }
        assert_eq!(
            chips(&app),
            (ChipLevel::Off, ChipLevel::Off),
            "working is no news"
        );
        app.windows.get_mut(&wid).expect("window").focused = true;
        for sid in [sid_a, sid_b] {
            app.session_status
                .observe_program_state(sid, done, true, false, now);
            app.refresh_presence_session(sid);
        }
        assert_eq!(
            chips(&app),
            (ChipLevel::Done, ChipLevel::Off),
            "the background tab's dot, and none on the tab in front: A `{}` B `{}`",
            app.presence_status_tail(sid_a),
            app.presence_status_tail(sid_b)
        );
        assert!(
            app.presence_status_tail(sid_a).contains("level=story"),
            "the background tab's turn end is a story: `{}`",
            app.presence_status_tail(sid_a)
        );
        assert!(
            app.presence_status_tail(sid_b).contains("level=quiet"),
            "the tab in front is not news: `{}`",
            app.presence_status_tail(sid_b)
        );
        app.windows.get_mut(&wid).expect("window").focused = false;
        let mut working2 = working;
        working2.seq = done.seq + 1;
        let mut done2 = done;
        done2.seq = working2.seq + 1;
        for state in [working2, done2] {
            app.session_status
                .observe_program_state(sid_b, state, true, false, now);
            app.refresh_presence_session(sid_b);
        }
        assert!(
            app.presence_status_tail(sid_b).contains("level=story"),
            "`{}`",
            app.presence_status_tail(sid_b)
        );
        app.windows
            .get_mut(&wid)
            .expect("window")
            .window_on_screen_for_test = true;
        assert_eq!(app.frontmost_window, Some(wid));
        app.switch_tab_in(wid, ia);
        assert_eq!(app.focused_session_id(wid), Some(sid_a));
        assert_eq!(chips(&app), (ChipLevel::Off, ChipLevel::Done));
        app.switch_tab_in(wid, ib);
        assert_eq!(chips(&app), (ChipLevel::Off, ChipLevel::Off));
    });
}
