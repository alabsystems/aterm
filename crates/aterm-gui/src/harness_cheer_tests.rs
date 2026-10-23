// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kitty pet cheer behavior on task landing across focus and tab switches.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;
use aterm_effects::kitty_pet::PetAction;

use crate::harness_glass::EndFace;
use crate::{App, WindowId};

const SEC: u64 = 1_000;
const FRAME: Duration = Duration::from_millis(16);
const WID: WindowId = WindowId(0);

fn idle_host() -> crate::harness_host::HostHandle {
    crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    )
}

fn cheer_app(behind: bool) -> (App, String, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    app.config.serious_mode = Some(false);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty pet".into());
    app.config.motion = Some("full".into());
    app.kitty_cursor_enabled_cache = None;
    app.recompute_sparkle();
    assert!(app.trail_is_kitty_pet(), "fixture: the pet is the trail");
    {
        let ws = app.windows.get_mut(&WID).expect("window");
        ws.present = Some(crate::PresentTarget::Cpu {
            surface: crate::present::CpuSurface::Scripted(Default::default()),
        });
        ws.focused = false;
    }
    let host = idle_host();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    ctx.meta
        .lock()
        .unwrap()
        .set("autopilot", Some("land".into()));
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .unwrap();
    if behind {
        let id = app.next_session_id;
        app.push_stub_tab(WID, crate::stub_session(id));
    }
    app.frontmost_window = Some(WID);
    app.windows.get_mut(&WID).unwrap().band_on_screen_for_test = true;
    (app, ctx.self_id.as_str().to_string(), host)
}

fn publish(app: &App, word: &'static str) -> bool {
    app.pool
        .get(0)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
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

fn land(app: &mut App, sid: &str) -> crate::harness_glass::EndGlass {
    app.apply_harness_fact(sid, crate::HarnessFact::Task(Some(TaskFace::Open)));
    assert!(publish(app, "busy"), "busy published");
    app.note_harness_phase(0);
    crate::turn_ledger::advance_for_test(45 * SEC);
    assert!(publish(app, "idle"), "idle published");
    app.note_harness_phase(0);
    app.apply_harness_fact(sid, crate::HarnessFact::Task(Some(TaskFace::Landed)));
    let end = app.harness_glass[&0].end().expect("fixture: an end");
    assert_eq!((end.face, end.cheered), (EndFace::Landed, 0));
    end
}

fn draw(app: &mut App, t: &mut Instant) {
    *t += FRAME;
    app.redraw_window_at(WID, *t);
}

fn landed(app: &App) -> Option<u32> {
    app.harness_glass
        .get(&0)
        .and_then(crate::harness_glass::SessionGlass::landed)
}

fn pet(app: &App) -> &aterm_effects::kitty_pet::PetBrain {
    app.windows[&WID].fx.companion().brain()
}

fn frolics_within(app: &mut App, t: &mut Instant, secs: u64) -> bool {
    let mut frolic = false;
    let frames = secs * 1000 / FRAME.as_millis() as u64;
    for _ in 0..frames {
        draw(app, t);
        frolic |= pet(app).action() == PetAction::Frolic;
    }
    frolic
}

/// Unfocused frames do not consume a landed task end; the first focused frame does.
#[test]
fn an_unfocused_windows_frames_do_not_consume_an_end_and_the_first_focused_frame_does() {
    let (mut app, sid, host) = cheer_app(false);
    let mut t = Instant::now();
    for _ in 0..10 {
        draw(&mut app, &mut t);
    }
    let end = land(&mut app, &sid);
    for _ in 0..60 {
        draw(&mut app, &mut t);
    }
    assert_eq!(landed(&app), Some(end.serial), "unfocused: the end waits");
    assert!(!pet(&app).cheer_owed(), "unfocused: no cheer latched");
    app.windows.get_mut(&WID).unwrap().focused = true;
    draw(&mut app, &mut t);
    assert_eq!(landed(&app), None, "the first focused frame takes it");
    assert_eq!(
        app.harness_glass[&0].end().map(|e| e.cheered),
        Some(end.serial)
    );
    assert!(pet(&app).cheer_owed(), "and the pet latched the cheer");
    assert!(
        frolics_within(&mut app, &mut t, 5),
        "the cheer plays: the frolic"
    );
    assert!(!pet(&app).cheer_owed(), "then nothing is owed");
    assert_eq!(landed(&app), None, "and nothing is handed again");
    host.shutdown_and_join();
}

/// Switching onto a landed background tab consumes the end and latches the cheer.
#[test]
fn a_tab_switch_onto_the_landed_tab_consumes_it() {
    let (mut app, sid, host) = cheer_app(true);
    app.windows.get_mut(&WID).unwrap().focused = true;
    let mut t = Instant::now();
    for _ in 0..10 {
        draw(&mut app, &mut t);
    }
    let end = land(&mut app, &sid);
    for _ in 0..60 {
        draw(&mut app, &mut t);
    }
    assert_eq!(
        landed(&app),
        Some(end.serial),
        "behind: not this pane's room"
    );
    assert!(!pet(&app).cheer_owed(), "behind: no cheer");
    app.switch_tab_in(WID, 0);
    draw(&mut app, &mut t);
    assert_eq!(landed(&app), None, "the switch is the sighting");
    assert!(
        pet(&app).cheer_owed(),
        "the pet latched the cheer in that pane"
    );
    assert!(frolics_within(&mut app, &mut t, 5), "and plays it");
    app.switch_tab_in(WID, 1);
    draw(&mut app, &mut t);
    app.switch_tab_in(WID, 0);
    draw(&mut app, &mut t);
    assert!(!pet(&app).cheer_owed(), "away and back cheers nothing");
    host.shutdown_and_join();
}
