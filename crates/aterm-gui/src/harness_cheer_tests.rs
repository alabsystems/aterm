// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KITTY CHEERS THE LANDING, WHEN YOU ARE THERE TO SEE IT
//! (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.3; §11.3.5's aterm-gui
//! tests), on the real redraw path (a scripted CPU surface, the resident pet
//! the trail, Full motion): a task end told the way slice 2a tells one (a
//! session under Land claimed by this host, open, 45 s of its agent's busy
//! time, landed) is the room's `landed` on that session's frames. An unfocused
//! window's frames do not consume it; the first focused frame of the pane
//! does, marks it cheered on the App's glass, and the pet plays the cheer
//! there. A tab switch onto the landed tab IS the sighting.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;
use aterm_effects::kitty_pet::PetAction;

use crate::harness_glass::EndFace;
use crate::{App, WindowId};

const SEC: u64 = 1_000;
const FRAME: Duration = Duration::from_millis(16);
const WID: WindowId = WindowId(0);

/// A host that supervises nothing: what the App's posture needs to read
/// `On`, shut down by the caller.
fn idle_host() -> crate::harness_host::HostHandle {
    crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    )
}

/// An App presenting through a scripted CPU surface (the real redraw path,
/// as `scripted_redraw_fixture` builds it) with the resident pet as the
/// trail and Full motion. Its session 0 runs Claude Code under Land,
/// claimed by this host, in window 0's first tab — with a second tab in
/// front where `behind` — and window 0 the frontmost, on screen, and not
/// focused. Its sid, and the host to shut down.
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

/// The agent word the status sweep publishes for session 0.
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

/// Session 0's task comes down the way slice 2a counts an end: its loop
/// tells open, its agent works 45 s on the ledger clock (moved on, not
/// waited), and its loop tells landed (Land's end; Cruise never ends). The
/// end standing on its glass.
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

/// One frame of window 0 at `*t`, the clock moved on a frame.
fn draw(app: &mut App, t: &mut Instant) {
    *t += FRAME;
    app.redraw_window_at(WID, *t);
}

/// The end of session 0 no pet has cheered yet: the room's `landed`.
fn landed(app: &App) -> Option<u32> {
    app.harness_glass
        .get(&0)
        .and_then(crate::harness_glass::SessionGlass::landed)
}

fn pet(app: &App) -> &aterm_effects::kitty_pet::PetBrain {
    app.windows[&WID].fx.companion().brain()
}

/// Draw `secs` of frames; whether the pet frolicked among them.
fn frolics_within(app: &mut App, t: &mut Instant, secs: u64) -> bool {
    let mut frolic = false;
    let frames = secs * 1000 / FRAME.as_millis() as u64;
    for _ in 0..frames {
        draw(app, t);
        frolic |= pet(app).action() == PetAction::Frolic;
    }
    frolic
}

/// AN UNFOCUSED WINDOW'S FRAMES DO NOT CONSUME AN END; THE FIRST FOCUSED
/// FRAME OF THAT PANE DOES (§11.3.5, aterm-gui): the end stands uncheered
/// through a second of unfocused frames, with no cheer latched; the first
/// focused frame marks it cheered and latches the cheer, which plays (the
/// frolic) and is then spent — a later frame never hands it again.
/// NEGATIVE CONTROL (as run, failing this test): an end consumed regardless
/// of visibility (`CompanionOwner::take_landed` without its `presentable`
/// gate) is spent by the unfocused frames.
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

/// A TAB SWITCH ONTO THE LANDED TAB CONSUMES IT (§11.3.3, item 3; §11.3.5,
/// aterm-gui): the end of a background tab's session is never the front
/// pane's room, so frames with another tab in front leave it standing; the
/// switch's first frame takes it, and the pet — the window's one pet, now in
/// that pane — cheers. NEGATIVE CONTROL (as run, failing this test): a
/// silent re-baseline at a session switch, in the style of the completion
/// latch `cmd_seq` (`take_landed` taking the first frame of a new session
/// as its baseline), swallows the cheer the switch is.
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
    // Away and back: a cheered end is never news again.
    app.switch_tab_in(WID, 1);
    draw(&mut app, &mut t);
    app.switch_tab_in(WID, 0);
    draw(&mut app, &mut t);
    assert!(!pet(&app).cheer_owed(), "away and back cheers nothing");
    host.shutdown_and_join();
}
