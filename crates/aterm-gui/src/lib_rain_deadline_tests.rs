// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_effects::matrix_rain::RainConfig;
use std::time::{Duration, Instant};

use super::{App, WindowId};

fn rain_cfg() -> RainConfig {
    RainConfig {
        enabled: true,
        output_material: false,
        seed: 7,
        ..RainConfig::default()
    }
}

fn rain_front_scheduler_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        RainFrontScheduler {
            const Buggy = 0;
            var front = 1;  // 1 terminal, 0 native
            var active = 1; // retained rain engine charge
            var armed = 0;  // event-loop rain deadline
            action Schedule {
                armed = if active == 1 {
                    if front == 1 { 1 } else { if Buggy == 1 { 1 } else { 0 } }
                } else { 0 };
            }
            action EnterNative when (front == 1) {
                front = 0;
                armed = if Buggy == 1 { armed } else { 0 };
            }
            action ReturnTerminal when (front == 0) {
                front = 1;
                armed = 0;
            }
            action Drain when (active == 1) {
                active = 0;
                armed = 0;
            }
            action Restart when (active == 0) {
                active = 1;
                armed = 0;
            }
            invariant BooleanBounds: front <= 1 && active <= 1 && armed <= 1;
            invariant NativeOwnsNoRainClock: (if front == 0 { armed } else { 0 }) == 0;
            invariant DrainedOwnsNoRainClock: (if active == 0 { armed } else { 0 }) == 0;
        }
    }
}

#[test]
fn rain_front_scheduler_model_proves_and_catches_native_wake_regression() {
    let model = rain_front_scheduler_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn native_front_freezes_active_rain_and_parks_its_deadline() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let deadline = Instant::now() + Duration::from_secs(1);
    let model = rain_front_scheduler_model();
    let mut state = model.init_state();
    {
        let ws = app.windows.get_mut(&wid).expect("headless window");
        ws.fx.build_rain(rain_cfg());
        ws.next_rain_tick = Some(deadline);
        assert!(ws.matrix_rain_frame_period().is_some());
    }
    assert!(model.fire("Schedule", &mut state));
    assert_eq!(state["armed"], 1);

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(model.fire("EnterNative", &mut state));

    let ws = app.windows.get(&wid).expect("headless window");
    assert!(ws.front_terminal().is_none(), "Settings is native content");
    assert!(
        ws.fx.rain().is_some_and(|rain| rain.is_active()),
        "the terminal rain episode is retained, not destructively reset"
    );
    assert_eq!(
        ws.matrix_rain_frame_period(),
        None,
        "invisible rain contributes no about_to_wait cadence"
    );
    assert_eq!(
        ws.next_rain_tick, None,
        "the terminal-era rain deadline is parked at the native boundary"
    );
    assert_eq!(state["front"], 0);
    assert_eq!(
        state["active"],
        i64::from(ws.fx.rain().unwrap().is_active())
    );
    assert_eq!(state["armed"], i64::from(ws.next_rain_tick.is_some()));
    assert!(model.check_invariant("NativeOwnsNoRainClock", &state));

    assert!(model.fire("Schedule", &mut state));
    assert_eq!(state["armed"], 0);
    assert_eq!(ws.matrix_rain_frame_period(), None);
}
