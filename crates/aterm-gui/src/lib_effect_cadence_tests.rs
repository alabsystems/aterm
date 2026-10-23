// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super::effect_tick_interval`] and the effect present lane.
use super::{AURORA_TICK_INTERVAL, MIN_EFFECT_TICK_INTERVAL, effect_tick_interval};
use std::time::Duration;

#[test]
fn the_effect_train_runs_at_the_panels_refresh_within_its_floors() {
    let hz = |millihertz: u64| Duration::from_nanos(1_000_000_000_000 / millihertz);
    let (promotion, hz144, hz30) = (hz(120_000), hz(144_000), hz(30_000));
    let rows: [(&str, Option<Duration>, Duration); 6] = [
        (
            "120 Hz panel ⇒ 120 Hz effect train",
            Some(promotion),
            promotion,
        ),
        (
            "144 Hz too — nothing is hard-coded to one panel",
            Some(hz144),
            hz144,
        ),
        ("a 30 Hz panel is not over-driven", Some(hz30), hz30),
        (
            "an unknown refresh falls back to 60 fps",
            None,
            AURORA_TICK_INTERVAL,
        ),
        (
            "a \"10 kHz\" report is floored",
            Some(Duration::from_micros(100)),
            MIN_EFFECT_TICK_INTERVAL,
        ),
        (
            "a zero period is floored",
            Some(Duration::ZERO),
            MIN_EFFECT_TICK_INTERVAL,
        ),
    ];
    for (label, panel, expected) in rows {
        assert_eq!(effect_tick_interval(panel), expected, "{label}");
    }
    assert!(hz30 > AURORA_TICK_INTERVAL, "30 Hz is slower than 60 Hz");
    assert_ne!(
        effect_tick_interval(None),
        super::MIN_FRAME_INTERVAL,
        "the output-pacing floor is not a refresh rate"
    );
}

#[test]
fn the_effect_lane_presents_strictly_slower_than_the_panel_refreshes() {
    let promotion = Duration::from_nanos(1_000_000_000_000 / 120_000); // 120 Hz
    let capped = super::effect_present_interval_with(Some(promotion), 2);
    assert_eq!(capped, promotion * 2, "120 Hz panel ⇒ 60 Hz effect lane");
    assert!(
        capped > effect_tick_interval(Some(promotion)),
        "a lane that presents once per refresh keeps the Metal pool empty"
    );
    assert_eq!(
        super::effect_present_interval_with(Some(promotion), 1),
        effect_tick_interval(Some(promotion)),
        "ATERM_EFFECT_PACE=panel restores the uncapped lane"
    );
    assert_eq!(
        super::effect_present_interval_with(Some(promotion), 0),
        effect_tick_interval(Some(promotion)),
        "0 clamps to 1, never to an unpaced lane"
    );
    assert_eq!(
        super::effect_present_interval_with(None, 2),
        AURORA_TICK_INTERVAL * 2
    );
    assert_eq!(
        super::effect_present_interval_with(Some(Duration::ZERO), 2),
        MIN_EFFECT_TICK_INTERVAL * 2
    );
}

#[test]
fn a_sixty_hertz_panel_presents_effects_at_panel_rate_off_apple_silicon() {
    let hz = |mhz: u64| Duration::from_nanos(1_000_000_000_000 / mhz);
    let (hz30, hz50, hz59_94, hz60, hz61, hz120) = (
        hz(30_000),
        hz(50_000),
        hz(59_940),
        hz(60_000),
        hz(61_000),
        hz(120_000),
    );
    for slow in [hz30, hz50, hz59_94, hz60] {
        assert_eq!(
            super::effect_present_panel_periods_with(Some(slow), true),
            1,
            "{slow:?}: at or below 60 Hz ⇒ panel rate where the arm holds"
        );
        assert_eq!(
            super::effect_present_panel_periods_with(Some(slow), false),
            super::EFFECT_PRESENT_PANEL_PERIODS,
            "{slow:?}: the M3-measured halving everywhere else"
        );
    }
    for fast in [hz61, hz120] {
        assert_eq!(
            super::effect_present_panel_periods_with(Some(fast), true),
            super::EFFECT_PRESENT_PANEL_PERIODS,
            "{fast:?}: a panel faster than 60 Hz keeps the halving on every host"
        );
    }
    assert_eq!(
        super::effect_present_panel_periods_with(None, true),
        super::EFFECT_PRESENT_PANEL_PERIODS,
        "an unknown refresh is not a measured panel"
    );
    assert!(hz60 >= super::EFFECT_LANE_PANEL_RATE_MIN_PERIOD);
    assert!(hz61 < super::EFFECT_LANE_PANEL_RATE_MIN_PERIOD);
    let sixty_lane = if super::EFFECT_LANE_PANEL_RATE_AT_60HZ {
        hz60
    } else {
        hz60 * 2
    };
    assert_eq!(super::effect_present_interval(Some(hz60)), sixty_lane);
    assert_eq!(
        super::effect_present_interval(Some(hz120)),
        hz120 * 2,
        "120 Hz panel ⇒ 60 Hz effect lane on every host"
    );
    assert_eq!(
        super::effect_present_interval(None),
        AURORA_TICK_INTERVAL * 2,
        "unknown refresh ⇒ the 60 fps fallback, stretched, on every host"
    );
    assert_eq!(
        super::EFFECT_LANE_PANEL_RATE_AT_60HZ,
        cfg!(any(not(target_os = "macos"), target_arch = "x86_64")),
        "the arm is every host but an arm64 Mac"
    );
    if cfg!(target_os = "linux") {
        assert_eq!(
            super::effect_present_interval(Some(hz60)),
            hz60,
            "a 60 Hz Linux panel animates the effect lane at 60 fps, not 30"
        );
    }
}

#[test]
fn a_parked_acquire_drops_the_window_to_the_halving_until_the_lane_idles() {
    let hz60 = Duration::from_nanos(1_000_000_000_000 / 60_000);
    let mut app = super::App::headless_for_test();
    let ws = app
        .windows
        .get_mut(&super::WindowId(0))
        .expect("the headless fixture owns window 0");
    ws.frame_interval = Some(hz60);
    let platform = super::effect_present_interval(Some(hz60));
    let halved = hz60 * super::EFFECT_PRESENT_PANEL_PERIODS;
    assert_eq!(
        ws.effect_present_interval(),
        platform,
        "a fresh window presents at the platform's arm"
    );
    ws.note_acquire_wait(super::EFFECT_LANE_ACQUIRE_PARK_NS - 1);
    assert!(!ws.effect_lane_parked, "a clean acquire is not a park");
    assert_eq!(ws.effect_present_interval(), platform);
    ws.note_acquire_wait(super::EFFECT_LANE_ACQUIRE_PARK_NS);
    assert!(ws.effect_lane_parked, "the threshold itself is a park");
    assert_eq!(
        ws.effect_present_interval(),
        halved,
        "a parked window keeps the halving on every host"
    );
    ws.note_acquire_wait(0);
    assert!(
        ws.effect_lane_parked,
        "one clean present after a park does not retry: monotone within an episode"
    );
    assert_eq!(ws.effect_present_interval(), halved);
    ws.park_terminal_effect_scheduler();
    assert!(!ws.effect_lane_parked, "the lane idling ends the episode");
    assert_eq!(
        ws.effect_present_interval(),
        platform,
        "the next episode probes the pool again at the platform's arm"
    );
    ws.note_acquire_wait(super::EFFECT_LANE_ACQUIRE_PARK_NS);
    assert_eq!(ws.effect_present_interval(), halved);
    let sid = app.next_session_id;
    let wid_b = app.insert_logical_window(super::stub_session(sid), 24, 80);
    let other = app.windows.get_mut(&wid_b).expect("the second window");
    other.frame_interval = Some(hz60);
    assert!(!other.effect_lane_parked);
    assert_eq!(other.effect_present_interval(), platform);
    assert_eq!(
        app.windows[&super::WindowId(0)].effect_present_interval(),
        halved,
        "the first window's park outlives the second window's arrival"
    );
    assert_eq!(
        platform == halved,
        !super::EFFECT_LANE_PANEL_RATE_AT_60HZ,
        "on an arm64 Mac the platform's arm IS the halving"
    );
}

#[test]
fn the_decoration_latch_is_sized_for_the_lane_a_park_stretches_it_to() {
    let hz60 = Duration::from_nanos(1_000_000_000_000 / 60_000);
    let halved =
        super::effect_present_interval_with(Some(hz60), super::EFFECT_PRESENT_PANEL_PERIODS);
    let panel_rate = super::effect_present_interval_with(Some(hz60), 1);
    assert!(
        panel_rate * super::DECO_ANIM_LEVEL_FRAMES < halved * 3,
        "which is why the latch may not count in the lane the window is on"
    );
    assert!(halved * super::DECO_ANIM_LEVEL_FRAMES >= halved * 3);
    let mut app = super::App::headless_for_test();
    let ws = app
        .windows
        .get_mut(&super::WindowId(0))
        .expect("the headless fixture owns window 0");
    ws.frame_interval = Some(hz60);
    assert_eq!(
        ws.deco_anim_hold(),
        halved * super::DECO_ANIM_LEVEL_FRAMES,
        "the hold is the halved lane's, on every host"
    );
    let t0 = std::time::Instant::now();
    ws.note_deco_animating(t0);
    let until = ws.deco_anim_until.expect("the decoration armed the latch");
    ws.note_acquire_wait(super::EFFECT_LANE_ACQUIRE_PARK_NS);
    assert_eq!(ws.effect_present_interval(), halved);
    assert!(
        until - t0 >= ws.effect_present_interval() * 3,
        "the latch armed before the park must outlive arm → fire → redraw at \
         the stretched lane"
    );
    assert_eq!(
        until - t0,
        ws.deco_anim_hold(),
        "and it is exactly the hold, not longer"
    );
    assert!(
        ws.deco_anim_frame_active(t0 + ws.effect_present_interval() * 3 - Duration::from_nanos(1))
    );
    assert!(
        !ws.deco_anim_frame_active(t0 + ws.deco_anim_hold()),
        "and it still expires: a decoration that stopped moving stops paying"
    );
}
