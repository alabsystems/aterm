// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A look from another app is no look (§14.11 of `docs/DESIGN-harness-autopilot-2026-10-15.md`):
//! the front tab of the focused frontmost window is looked at, and his return to it is the look.

use std::sync::Arc;

use aterm_agent::harness::acts::TaskFace;

use crate::control::control_media::PersonAsked;
use crate::presence::ChipLevel;
use crate::{App, WindowId};

const SEC: u64 = 1_000;
const W: WindowId = WindowId(0);

struct TestHost(crate::harness_host::HostHandle);

impl Drop for TestHost {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

/// Session 0 a Claude Code under Land with this host's claim, in tab 0 of window 0, with a
/// stub tab beside it; window 0 the frontmost window, on screen when `on_screen`.
fn desk(on_screen: bool) -> (App, String, TestHost) {
    let mut app = App::headless_for_test();
    let host = TestHost(crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    ));
    app.harness = Some(host.0.clone());
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
    let id = app.next_session_id;
    app.push_stub_tab(W, crate::stub_session(id));
    app.frontmost_window = Some(W);
    app.windows
        .get_mut(&W)
        .expect("window 0")
        .band_on_screen_for_test = on_screen;
    (app, ctx.self_id.as_str().to_string(), host)
}

fn publish(app: &App, word: &'static str) -> bool {
    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .timeline
        .lock()
        .expect("timeline")
        .publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        )
}

/// Session 0 opens a task, works `ms` and lands: an end past the work floor (§11.2.3).
fn end_after(app: &mut App, sid: &str, ms: u64) {
    app.apply_harness_fact(sid, crate::HarnessFact::Task(Some(TaskFace::Open)));
    assert!(publish(app, "busy"), "busy published");
    app.note_harness_phase(0);
    crate::turn_ledger::advance_for_test(ms);
    assert!(publish(app, "idle"), "idle published");
    app.note_harness_phase(0);
    app.apply_harness_fact(sid, crate::HarnessFact::Task(Some(TaskFace::Landed)));
}

/// `status`'s `end_seen=` for session 0.
fn end_seen(app: &App) -> String {
    let line = app.session_status_record(0).expect("status");
    line.split_whitespace()
        .find_map(|w| w.strip_prefix("end_seen="))
        .unwrap_or_else(|| panic!("no end_seen= in {line}"))
        .to_owned()
}

/// Each tab's strip mark in window 0, in strip order.
fn marks(app: &App) -> Vec<ChipLevel> {
    let ws = &app.windows[&W];
    let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
        .tab_set
        .tabs()
        .iter()
        .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
        .collect();
    app.stamp_presence_chips(W, &mut metadata);
    metadata.iter().map(|m| m.attention).collect()
}

/// He leaves for another app: window 0 loses the focus and stays the frontmost window, on screen.
fn away(app: &mut App) {
    app.window_focus_event(W, false);
    let ws = &app.windows[&W];
    assert!(
        !ws.focused && !ws.os_key,
        "PRECONDITION: another app is active"
    );
    assert_eq!(
        app.frontmost_window,
        Some(W),
        "PRECONDITION: still aterm's frontmost window"
    );
    assert!(
        crate::messages_host::band_on_screen(ws),
        "PRECONDITION: on screen, uncovered"
    );
}

#[test]
fn an_unfocused_window_looks_at_nothing() {
    let (mut app, sid, _host) = desk(true);
    app.switch_tab_in(W, 0);
    away(&mut app);
    end_after(&mut app, &sid, 45 * SEC);
    assert_eq!(
        end_seen(&app),
        "0",
        "a landing in the front tab while another app is active stands"
    );
    let g = app.status_glance();
    assert_eq!(g.done.len(), 1, "its Done for you row: {:?}", g.done);
    assert_eq!(g.button_title(), "\u{276f}\u{2022}");
    assert_ne!(
        marks(&app)[0],
        ChipLevel::Done,
        "the front tab still never wears the `•`"
    );
    app.note_done_seen_looking();
    app.on_occlusion_changed(W, false);
    app.switch_tab_in(W, 1);
    assert_eq!(
        marks(&app)[0],
        ChipLevel::Done,
        "behind, its tab wears the `•`"
    );
    app.switch_tab_in(W, 0);
    assert_eq!(
        end_seen(&app),
        "0",
        "a menu-bar rebuild, a reveal and a tab switch nobody made from the window see nothing while he is away"
    );
    assert_eq!(app.status_glance().done.len(), 1, "the row stands");
}

#[test]
fn a_workers_done_in_the_front_tab_waits_for_his_return_too() {
    let mut app = App::headless_for_test();
    app.push_stub_tab(W, crate::stub_session(1));
    app.frontmost_window = Some(W);
    app.windows
        .get_mut(&W)
        .expect("window 0")
        .band_on_screen_for_test = true;
    let ctx = app.pool.get(1).expect("pooled").ctx.clone();
    assert_eq!(
        app.focused_session_id(W),
        Some(1),
        "PRECONDITION: the worker's tab is in front"
    );
    away(&mut app);
    for state in [
        crate::task_record::TaskState::Working,
        crate::task_record::TaskState::Done,
    ] {
        let mark = crate::task_record::TaskMark::new(state);
        crate::task_record::mark_session_task(&ctx, 1, &mark).expect("marked");
        app.on_task_changed(1);
    }
    app.note_done_seen_looking();
    assert_eq!(
        app.status_glance().done.len(),
        1,
        "a worker's done in the front tab stands while he is away"
    );
    app.window_focus_event(W, true);
    assert!(
        app.status_glance().done.is_empty(),
        "and goes when he comes back"
    );
}

#[test]
fn a_refocus_marks_it_seen_once() {
    let (mut app, sid, _host) = desk(true);
    app.switch_tab_in(W, 0);
    away(&mut app);
    end_after(&mut app, &sid, 45 * SEC);
    assert_eq!(end_seen(&app), "0", "PRECONDITION: it stands");
    let _ = crate::work_counts::take_end_restamps();
    app.take_status_rebuild();
    app.window_focus_event(W, true);
    assert_eq!(end_seen(&app), "1", "his return to the window is the look");
    assert_eq!(
        crate::work_counts::take_end_restamps(),
        1,
        "one re-stamp for the look"
    );
    assert!(app.take_status_rebuild(), "the look dirties the glance");
    let g = app.status_glance();
    assert!(g.done.is_empty(), "the row goes: {:?}", g.done);
    assert_eq!(g.button_title(), "\u{276f}");
    away(&mut app);
    app.window_focus_event(W, true);
    assert_eq!(
        crate::work_counts::take_end_restamps(),
        0,
        "away and back again marks nothing more"
    );
    end_after(&mut app, &sid, 40 * SEC);
    assert_eq!(
        end_seen(&app),
        "1",
        "while aterm is active, a landing in its front tab is seen as it lands"
    );
    assert!(app.status_glance().done.is_empty());
}

#[test]
fn the_stand_in_still_marks_a_headless_window() {
    let (mut app, sid, _host) = desk(false);
    app.switch_tab_in(W, 0);
    app.window_focus_event(W, false);
    end_after(&mut app, &sid, 45 * SEC);
    assert_eq!(end_seen(&app), "0", "a headless window sees nothing");
    let said = app.person_stand_in(PersonAsked::Look(Some(0)));
    assert_eq!(said.as_deref(), Ok("person look window=0"));
    assert_eq!(
        end_seen(&app),
        "1",
        "the stand-in looks for a headless window, focused or not"
    );
}

/// What `aterm ctl … focus in|out` does to window `wid`: the controller arm of `Wake::Input`
/// hands an `InputEvent::Focus` straight to `on_focus`, the one call it makes (a driven window
/// gets no `WindowEvent::Focused`).
fn ctl_focus(app: &mut App, wid: WindowId, focused: bool) {
    app.on_focus(wid, focused);
    assert_eq!(
        app.windows[&wid].focused, focused,
        "PRECONDITION: the controller moved the window's focus flag"
    );
}

#[test]
fn a_controllers_focus_in_is_no_look() {
    let (mut app, sid, _host) = desk(true);
    app.switch_tab_in(W, 0);
    away(&mut app);
    end_after(&mut app, &sid, 45 * SEC);
    ctl_focus(&mut app, W, true);
    app.note_done_seen_looking();
    app.on_occlusion_changed(W, false);
    assert_eq!(
        end_seen(&app),
        "0",
        "a controller's `focus in` while he is in another app is no look: the landing stands"
    );
    assert_eq!(app.status_glance().done.len(), 1, "its row stands");
    app.window_focus_event(W, true);
    assert_eq!(end_seen(&app), "1", "his own return is the look");
}

#[test]
fn a_controllers_focus_out_does_not_end_his_look() {
    let (mut app, sid, _host) = desk(true);
    app.switch_tab_in(W, 0);
    app.window_focus_event(W, true);
    ctl_focus(&mut app, W, false);
    end_after(&mut app, &sid, 45 * SEC);
    assert_eq!(
        end_seen(&app),
        "1",
        "while the window is his key window, a controller's `focus out` leaves him looking: seen as it lands"
    );
    assert!(app.status_glance().done.is_empty(), "no row stands");
}

#[test]
fn the_attach_time_focus_is_the_oss_word_too() {
    let (mut app, sid, _host) = desk(true);
    app.switch_tab_in(W, 0);
    app.adopt_initial_focus(W, Some(false));
    end_after(&mut app, &sid, 45 * SEC);
    assert_eq!(
        end_seen(&app),
        "0",
        "a window attached while another app is active is not his key window: the landing stands"
    );
    app.adopt_initial_focus(W, Some(true));
    end_after(&mut app, &sid, 40 * SEC);
    assert_eq!(
        end_seen(&app),
        "1",
        "once the attach-time read says it is key, a landing in its front tab is seen as it lands"
    );
}
