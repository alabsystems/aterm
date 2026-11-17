// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The effect lane of a window nobody attends: what armed each slot the lane serves, and what
//! the lane still plans once a window's attention has lapsed. Every instant is the test's own.

use crate::effect_lane_tally::{LaneTally, LaneTerm};
use crate::window_rest::ATTENTION;
use crate::{App, WindowId, WindowState};
use std::time::{Duration, Instant};

/// Every term's fires together.
fn total(tally: LaneTally) -> u32 {
    LaneTerm::ALL.iter().map(|term| tally.fires(*term)).sum()
}

/// The slot armed at `d` under `term` is not served before `d`, and serving it at `d` books
/// exactly one fire, under `term`.
fn serve_books(ws: &mut WindowState, d: Instant, term: LaneTerm, what: &str) {
    assert_eq!(
        ws.trail_tick_term,
        Some(term),
        "{what}: the armed slot's term"
    );
    let before = ws.lane_tally;
    assert!(
        !ws.service_due_terminal_effect_tick(d - Duration::from_nanos(1)),
        "{what}: not served before its slot"
    );
    assert_eq!(
        ws.lane_tally, before,
        "{what}: nothing booked before its slot"
    );
    assert!(
        ws.service_due_terminal_effect_tick(d),
        "{what}: served at its slot"
    );
    assert_eq!(
        ws.lane_tally.fires(term),
        before.fires(term) + 1,
        "{what}: one fire under {}",
        term.name()
    );
    assert_eq!(
        total(ws.lane_tally),
        total(before) + 1,
        "{what}: and under no other term"
    );
    assert_eq!(
        ws.trail_tick_term, None,
        "{what}: the served slot's term is spent"
    );
}

/// One present of the resident pet at `now` with its caret at row 4, column `col`, under the
/// load-shed envelope and latch `shed`.
pub(crate) fn present_pet(ws: &mut WindowState, now: Instant, col: u16, shed: (f32, bool)) {
    let tick = crate::app_render::pet_prepare_shed_for_test(ws, now, 0, Some((4, col)), None, shed);
    let _ = ws.fx.companion_mut().tick(tick);
}

/// A focused window whose resident runs after a far caret under a load shed: its brain moves,
/// off the frame train, and its next step is offered at the shed cadence. Returns the instant
/// of the shed present.
pub(crate) fn shed_moving_pet(ws: &mut WindowState, start: Instant) -> Instant {
    ws.focused = true;
    let mut now = start;
    for _ in 0..40 {
        now += Duration::from_millis(16);
        present_pet(ws, now, 10, (1.0, false));
    }
    now += Duration::from_millis(16);
    present_pet(ws, now, 60, (1.0, false));
    now += Duration::from_millis(16);
    present_pet(ws, now, 60, (1.0, true));
    assert!(
        ws.fx.companion().brain().needs_frames() && !ws.fx.companion().needs_frames(),
        "fixture: the shed resident's brain moves, off the frame train"
    );
    now
}

/// An output streak with a live episode at `now` (`output_streak_next_change_deadline` is
/// `now`).
fn active_streak(now: Instant) -> crate::output_streak::OutputStreak {
    let mut engine = crate::output_streak::OutputStreak::new(2);
    let cfg = crate::output_streak::StreakConfig {
        enabled: true,
        ..crate::output_streak::StreakConfig::default()
    };
    assert!(!engine.note_output(1, &[(4, 0, 40)], now, false));
    let spawned = now + Duration::from_millis(350);
    assert!(engine.note_output(2, &[(4, 0, 40)], spawned, false));
    engine.tick(spawned, crate::geom_24x80_for_test(), &cfg, &mut Vec::new());
    assert!(engine.is_active(), "fixture: the episode is live");
    engine
}

/// Each slot the lane serves is booked once, under the term that armed it: a decoration's hold,
/// the load-shed fade's, the shed pet's coarse offer, an output streak. A plan that keeps a held
/// slot keeps the term that armed it, and plans before a slot is due book nothing.
///
/// NEGATIVE CONTROL: the held arm of the planner's slot match set the fresh term
/// (`self.trail_tick_term = Some(fresh_term)` on `Some(held) if held <= fresh`): "the held slot
/// keeps the term that armed it" failed (left `Some(Pet)`, right `Some(Deco)`).
/// NEGATIVE CONTROL: the planner booked the fresh term as it armed the slot
/// (`self.lane_tally.book(fresh_term)` beside `self.trail_tick_term = Some(fresh_term)`): "two
/// plans before the slot is due book no fire" failed (left 1, right 0).
#[test]
fn each_lane_fire_is_booked_under_the_term_that_armed_it() {
    let mut app = App::headless_for_test();
    let mut ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless window");
    let t0 = Instant::now();
    assert_eq!(
        ws.plan_terminal_effect_lane(t0, false, true),
        None,
        "fixture: the lane is quiet"
    );
    assert_eq!(ws.trail_tick_term, None, "a quiet lane has no term");

    ws.note_deco_animating(t0);
    let d = ws
        .plan_terminal_effect_lane(t0, false, true)
        .expect("a decoration's hold arms the lane");
    assert_eq!(
        ws.plan_terminal_effect_lane(t0 + Duration::from_millis(1), false, true),
        Some(d),
        "a second plan keeps the armed slot"
    );
    assert_eq!(
        total(ws.lane_tally),
        0,
        "two plans before the slot is due book no fire"
    );
    serve_books(&mut ws, d, LaneTerm::Deco, "a decoration's hold");
    ws.park_terminal_effect_scheduler();

    let t1 = d + Duration::from_secs(1);
    ws.note_shed_fade_animating(t1);
    let d = ws
        .plan_terminal_effect_lane(t1, false, true)
        .expect("the fade's hold arms the lane");
    serve_books(&mut ws, d, LaneTerm::Fade, "the load-shed fade's hold");
    ws.park_terminal_effect_scheduler();

    let t2 = d + Duration::from_secs(1);
    ws.note_shed_fade_animating(t2);
    ws.note_deco_animating(t2);
    assert!(
        ws.plan_terminal_effect_lane(t2, false, true).is_some(),
        "the two holds arm the lane"
    );
    assert_eq!(
        ws.trail_tick_term,
        Some(LaneTerm::Deco),
        "a decoration that asks for the fade's whole hold takes it back"
    );
    ws.park_terminal_effect_scheduler();
    let t3 = t2 + Duration::from_millis(1);
    ws.note_deco_animating(t2);
    ws.note_shed_fade_animating(t3);
    assert!(ws.plan_terminal_effect_lane(t3, false, true).is_some());
    assert_eq!(
        ws.trail_tick_term,
        Some(LaneTerm::Fade),
        "the fade that holds past every decoration is the fade's"
    );
    ws.park_terminal_effect_scheduler();

    let mut app = App::headless_for_test();
    let mut ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless window");
    let now = shed_moving_pet(&mut ws, Instant::now());
    let offer = now + aterm_effects::companion::RESIDENT_SHED_CADENCE;
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(now, false, true, true, false, 10.0, true),
        Some(offer),
        "the shed pet's coarse offer arms the lane"
    );
    serve_books(&mut ws, offer, LaneTerm::Pet, "the shed pet's offer");

    let now = offer;
    ws.note_deco_animating(now);
    let held = ws
        .plan_terminal_effect_lane_with_recording(now, false, true, true, false, 10.0, true)
        .expect("the decoration's hold arms the lane");
    assert_eq!(
        ws.trail_tick_term,
        Some(LaneTerm::Deco),
        "fixture: the hold armed it"
    );
    let later = now + ws.deco_anim_hold();
    assert!(
        !ws.deco_anim_frame_active(later),
        "fixture: the hold has ended"
    );
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(later, false, true, true, false, 10.0, true),
        Some(held),
        "the held slot, sooner than the pet's fresh offer, wins"
    );
    assert_eq!(
        ws.trail_tick_term,
        Some(LaneTerm::Deco),
        "the held slot keeps the term that armed it"
    );
    serve_books(&mut ws, held, LaneTerm::Deco, "the held decoration slot");

    let mut app = App::headless_for_test();
    let mut ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless window");
    let now = Instant::now() + Duration::from_secs(1);
    *ws.fx.streak_slot() = Some(Box::new(active_streak(now)));
    assert_eq!(
        ws.plan_terminal_effect_lane(now, false, true),
        Some(now),
        "a live streak arms the lane at its next change"
    );
    serve_books(&mut ws, now, LaneTerm::Streak, "an output streak");
}

/// A focused window whose resident has settled at a caret: its brain no longer moves, and it
/// owes a coarse next step. Presents it from `start` until it settles; returns the instant of
/// its last present.
pub(crate) fn settled_pet(ws: &mut WindowState, start: Instant) -> Instant {
    ws.focused = true;
    let mut now = start;
    for _ in 0..2_000 {
        now += Duration::from_millis(16);
        present_pet(ws, now, 10, (1.0, false));
        if !ws.fx.companion().brain().needs_frames() && now > start + Duration::from_secs(1) {
            break;
        }
    }
    assert!(
        !ws.fx.companion().brain().needs_frames(),
        "fixture: the resident settled"
    );
    assert!(
        ws.fx.companion().next_change_deadline(now).is_some(),
        "fixture: the settled resident owes its next step"
    );
    now
}

/// Lapses `ws`'s attention at `at`, which is at least [`ATTENTION`] after its last touch.
fn lapse(ws: &mut WindowState, at: Instant) {
    let _ = ws.rest.settle_attended(at, Some(ATTENTION));
    assert!(
        !ws.rest.attended(),
        "fixture: the window's attention lapsed"
    );
}

/// Plans the lane at `now` with the resident pet in motion's gate open and nothing else.
fn plan_pet(ws: &mut WindowState, now: Instant) -> Option<Instant> {
    ws.plan_terminal_effect_lane_with_recording(now, false, true, true, false, 10.0, true)
}

/// Fires a two-bar celebration armed on `done` for the window's front session at `at`.
fn celebrate(ws: &mut WindowState, at: Instant) {
    use aterm_effects::kitty_sing::{CelebrateOn, song_signature};
    let sid = ws.front_terminal().expect("a front terminal").session;
    let sing = &mut ws.fx.companions_mut().kitty_sing;
    sing.arm_external(at, sid, song_signature('C'), 2, CelebrateOn::Done)
        .expect("the celebration arms");
    assert!(
        sing.note_program_done(at, sid),
        "fixture: the celebration fires"
    );
    assert!(sing.external_live(), "fixture: the celebration plays");
}

/// A focused headless window whose attention lapsed at the returned instant.
fn lapsed_focused_app() -> (App, Instant) {
    let mut app = App::headless_for_test();
    let at = Instant::now() + ATTENTION;
    let mut ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless window");
    ws.focused = true;
    lapse(&mut ws, at);
    drop(ws);
    (app, at)
}

/// The resident pet's coarse offer is admitted while a person is here (attended and not
/// resting), while its brain moves, for [`crate::app_render::CURSOR_FX_TYPED_WAKE`] after a
/// completion or a landing it saw (an event that old no longer admits it), and while a
/// celebration plays; otherwise it is not, row by row over every combination.
///
/// NEGATIVE CONTROL: `self.person_here()` dropped from the admission: "attended=true
/// resting=false moving=false event=none live=false" failed (left false, right true).
/// NEGATIVE CONTROL: `!resting` dropped (the admission read `self.rest.attended()` in place of
/// `self.person_here()`): "attended=true resting=true moving=false event=none live=false"
/// failed (left true, right false).
/// NEGATIVE CONTROL: the brain's motion (`self.fx.companion().brain().needs_frames()`) dropped:
/// "attended=false resting=false moving=true event=none live=false" failed (left false, right
/// true).
/// NEGATIVE CONTROL: the event stamp (`self.pet_event_at.is_some_and(..)`) dropped:
/// "attended=false resting=false moving=false event=1s live=false" failed (left false, right
/// true).
/// NEGATIVE CONTROL: `self.fx.companions().kitty_sing.external_live()` dropped:
/// "attended=false resting=false moving=false event=none live=true" failed (left false, right
/// true).
/// NEGATIVE CONTROL: the stamp's edge made inclusive (`< crate::app_render::CURSOR_FX_TYPED_WAKE`
/// made `<=`): "attended=false resting=false moving=false event=wake live=false" failed (left
/// true, right false).
#[test]
fn the_pet_offer_is_admitted_while_a_person_is_here_or_an_event_plays() {
    for moving in [false, true] {
        let mut app = App::headless_for_test();
        let start = Instant::now() + ATTENTION;
        let mut ws = app
            .windows
            .get_mut(&WindowId(0))
            .expect("the headless window");
        let now = if moving {
            shed_moving_pet(&mut ws, start)
        } else {
            settled_pet(&mut ws, start)
        };
        for live in [false, true] {
            if live {
                celebrate(&mut ws, now);
            }
            for (attended, resting) in [(true, false), (false, false), (true, true), (false, true)]
            {
                if attended {
                    let _ = ws.rest.touch(now, true);
                } else {
                    lapse(&mut ws, now + ATTENTION);
                }
                if resting {
                    let _ = ws
                        .rest
                        .settle(now + ATTENTION, Some(Duration::from_secs(2)));
                } else {
                    let _ = ws.rest.stir(now);
                }
                assert_eq!(
                    (ws.rest.attended(), ws.rest.resting()),
                    (attended, resting),
                    "fixture: the rest"
                );
                let wake = crate::app_render::CURSOR_FX_TYPED_WAKE;
                for (event, age, open) in [
                    ("none", None, false),
                    ("1s", Some(Duration::from_secs(1)), true),
                    ("wake-1ns", Some(wake - Duration::from_nanos(1)), true),
                    ("wake", Some(wake), false),
                    ("9s", Some(Duration::from_secs(9)), false),
                ] {
                    ws.pet_event_at = age.map(|age| now - age);
                    let want = (attended && !resting) || moving || open || live;
                    assert_eq!(
                        ws.pet_offer_admitted(now),
                        want,
                        "attended={attended} resting={resting} moving={moving} event={event} \
                         live={live}"
                    );
                }
            }
        }
    }
}

/// A settled resident with a step owed plans exactly that step while a person is here, and
/// nothing once its window's attention lapses or it rests: no slot is armed.
///
/// NEGATIVE CONTROL: the admission dropped from the planner's pet gate (`&&
/// self.pet_offer_admitted(now)` removed): "a lapsed window's settled pet plans no offer"
/// failed (left Some(Instant { .. }), right None).
#[test]
fn an_unattended_windows_settled_pet_plans_no_offer() {
    let mut app = App::headless_for_test();
    let start = Instant::now() + ATTENTION;
    let mut ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless window");
    let now = settled_pet(&mut ws, start);
    let _ = ws.rest.touch(now, true);
    assert!(
        !ws.fx.companion().needs_frames(),
        "fixture: off the frame train"
    );
    let offer = ws
        .fx
        .companion()
        .next_change_deadline(now)
        .expect("fixture: the settled resident owes a step");
    assert_eq!(
        plan_pet(&mut ws, now),
        Some(offer),
        "an attended window's settled pet plans exactly its step"
    );
    assert_eq!(ws.trail_tick_term, Some(LaneTerm::Pet));
    ws.park_terminal_effect_scheduler();

    lapse(&mut ws, now + ATTENTION);
    assert_eq!(
        plan_pet(&mut ws, now),
        None,
        "a lapsed window's settled pet plans no offer"
    );
    assert_eq!(ws.next_trail_tick, None, "…and arms no slot");

    let _ = ws.rest.touch(now, true);
    assert_eq!(
        plan_pet(&mut ws, now),
        Some(offer),
        "fixture: attended again"
    );
    ws.park_terminal_effect_scheduler();
    let _ = ws
        .rest
        .settle(now + ATTENTION, Some(Duration::from_secs(2)));
    assert!(
        ws.rest.attended() && ws.rest.resting(),
        "fixture: attended and resting"
    );
    assert_eq!(
        plan_pet(&mut ws, now),
        None,
        "a resting window's settled pet plans no offer"
    );
}

/// A resident whose brain moves when its window's attention lapses plays its motion out on its
/// coarse offers, and once it settles the lane plans nothing for it.
///
/// NEGATIVE CONTROL: the admission read the companion's motion
/// (`self.fx.companion().needs_frames()`, false under the shed): "the moving pet plays on
/// unattended" failed at the first plan (left None, right Some(Instant { .. })).
/// NEGATIVE CONTROL: the admission read `self.fx.companion().brain().is_active()`: "once it
/// settles, an unattended pet plans nothing" failed (left Some(Instant { .. }), right None).
#[test]
fn a_moving_pet_plays_to_its_end_unattended() {
    use aterm_effects::companion::RESIDENT_SHED_CADENCE;
    let mut app = App::headless_for_test();
    let start = Instant::now() + ATTENTION;
    let mut ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless window");
    let mut now = shed_moving_pet(&mut ws, start);
    lapse(&mut ws, now);
    assert_eq!(
        plan_pet(&mut ws, now),
        Some(now + RESIDENT_SHED_CADENCE),
        "the moving pet plays on unattended"
    );
    let mut settled = false;
    for _ in 0..400 {
        now = ws
            .fx
            .companion()
            .next_change_deadline(now)
            .expect("a moving pet is offered");
        present_pet(&mut ws, now, 60, (0.0, true));
        if !ws.fx.companion().brain().needs_frames() {
            settled = true;
            break;
        }
        assert!(
            plan_pet(&mut ws, now).is_some(),
            "the moving pet plays on unattended"
        );
    }
    assert!(settled, "fixture: the offers carry it home");
    assert!(
        ws.fx.companion().brain().is_active(),
        "fixture: the settled pet is still on glass"
    );
    ws.park_terminal_effect_scheduler();
    assert_eq!(
        plan_pet(&mut ws, now),
        None,
        "once it settles, an unattended pet plans nothing"
    );
}

/// What an event started plays in a focused window whose attention lapsed: a celebration plans
/// its next event, a live output streak its next change, and a decoration's hold, served slot
/// by slot, the frame cadence until the hold ends, when the lane plans nothing.
///
/// NEGATIVE CONTROL: a blanket gate on the whole lane (`if !self.person_here() {
/// self.park_terminal_effect_scheduler(); return None; }` at the planner's top): "a celebration
/// plays unattended" failed (left None, right Some(Instant { .. })).
/// NEGATIVE CONTROL: a hold that never ends (`deco_anim_frame_active`'s `now < until` made
/// `now < until || true`): "…until the hold ends" failed (left Some(Instant { .. }), right
/// None).
#[test]
fn event_started_terms_play_unattended() {
    let (mut app, at) = lapsed_focused_app();
    let mut ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless window");
    celebrate(&mut ws, at);
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(at, false, false, true, false, 10.0, true),
        Some(at + ws.effect_present_interval()),
        "a celebration plays unattended"
    );

    let (mut app, at) = lapsed_focused_app();
    let mut ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless window");
    *ws.fx.streak_slot() = Some(Box::new(active_streak(at)));
    assert_eq!(
        ws.plan_terminal_effect_lane(at, false, true),
        Some(at),
        "a live streak plays unattended"
    );

    let (mut app, at) = lapsed_focused_app();
    let mut ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless window");
    ws.note_deco_animating(at);
    let ended = at + ws.deco_anim_hold();
    let interval = ws.effect_present_interval();
    let mut now = at;
    while now < ended {
        let d = ws
            .plan_terminal_effect_lane(now, false, true)
            .expect("a decoration's hold plays unattended at the frame cadence");
        assert!(d > now && d <= now + interval, "…at the frame cadence");
        assert!(
            ws.service_due_terminal_effect_tick(d),
            "fixture: the slot is served"
        );
        now = d;
    }
    assert_eq!(
        ws.plan_terminal_effect_lane(now, false, true),
        None,
        "…until the hold ends"
    );
}

/// A completion or a landing the pet saw stamps the window's pet event; a tick with neither
/// stamps nothing.
///
/// NEGATIVE CONTROL: every tick stamped (`|| true` added to `if tick.completion.is_some() ||
/// tick.landed_seen.is_some()`): "a tick with no event stamps nothing" failed (left
/// Some(Instant { .. }), right None).
/// NEGATIVE CONTROL: the landing dropped (`|| tick.landed_seen.is_some()` removed): "a landing
/// the pet saw stamps the pet event" failed (left the completion's instant, right the
/// landing's).
#[test]
fn a_pet_event_is_stamped_only_by_a_completion_or_a_landing() {
    let mut app = App::headless_for_test();
    let mut ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless window");
    let t = Instant::now();
    let quiet = crate::app_render::pet_prepare_for_test(&mut ws, t, 0, None, None);
    assert!(
        quiet.completion.is_none() && quiet.landed_seen.is_none(),
        "fixture: no event"
    );
    ws.note_pet_events(&quiet, t);
    assert_eq!(ws.pet_event_at, None, "a tick with no event stamps nothing");
    let _ = ws.fx.companion_mut().tick(quiet);

    let done_at = t + Duration::from_millis(16);
    let done =
        crate::app_render::pet_prepare_for_test(&mut ws, done_at, 0, None, Some((1, 0, None)));
    assert!(
        done.completion.is_some(),
        "fixture: the tick carries a completion"
    );
    ws.note_pet_events(&done, done_at);
    assert_eq!(
        ws.pet_event_at,
        Some(done_at),
        "a completion stamps the pet event"
    );
    let _ = ws.fx.companion_mut().tick(done);

    // The pet's prepare sets the tick's `landed_seen` when the pane showed a task end the room
    // named; it is set here by hand, as that prepare would, on a tick with no completion.
    let landed_at = done_at + Duration::from_millis(16);
    let mut landed = crate::app_render::pet_prepare_for_test(&mut ws, landed_at, 0, None, None);
    assert!(
        landed.completion.is_none(),
        "fixture: the tick carries no completion"
    );
    landed.landed_seen = Some((0, 1));
    ws.note_pet_events(&landed, landed_at);
    assert_eq!(
        ws.pet_event_at,
        Some(landed_at),
        "a landing the pet saw stamps the pet event"
    );
}
