// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Config;
use crate::keybinding::Action;

fn cfg(toml: &str) -> Config {
    aterm_toml::from_str(toml).expect("valid toml")
}

#[test]
fn toggle_dispatch_flips_front_session_override() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.recompute_matrix_rain();
    assert!(!app.session_rain_enabled(0), "default config: off");
    assert!(
        app.windows.values().all(|ws| ws.fx.rain().is_none()),
        "no engine is constructed while rain is off"
    );

    app.dispatch_action(wid, Action::ToggleMatrixRain);
    assert_eq!(app.pool.rain_override(0), Some(true));
    assert!(app.session_rain_enabled(0), "override forces rain on");
    assert!(app.rain_dirty, "toggle marks the resolve stale");
    app.recompute_matrix_rain();
    assert!(
        app.rain.is_some(),
        "params available for the forced-on field"
    );

    app.config = cfg("[matrix_rain]\nenabled = false");
    app.rain_dirty = true;
    app.recompute_matrix_rain();
    assert!(
        app.session_rain_enabled(0),
        "session override survives (and outranks) a config reload"
    );

    let rc = crate::rain_config_for_window(app.rain.unwrap(), wid);
    app.windows.get_mut(&wid).unwrap().fx.build_rain(rc);
    app.dispatch_action(wid, Action::ToggleMatrixRain);
    assert_eq!(app.pool.rain_override(0), Some(false));
    assert!(!app.session_rain_enabled(0));
    assert!(
        app.windows[&wid].fx.rain().is_some(),
        "the engine survives the off flip and drains via the suspend path"
    );

    app.config = cfg("[matrix_rain]\nenabled = true");
    app.rain_dirty = true;
    app.recompute_matrix_rain();
    assert!(
        !app.session_rain_enabled(0),
        "session off-override outranks an enabled config"
    );
    assert_eq!(app.pool.rain_override(999), None, "unknown session: none");
}

#[test]
fn rain_control_verb_reads_and_writes_front_session() {
    let mut app = crate::App::headless_for_test();
    assert_eq!(
        app.rain_control(crate::RainCtlOp::Status).as_deref(),
        Ok(
            "config_enabled=false session_override=none effective=false \
             engine=none active=false scope=window focused=true animating=true"
        ),
        "status is a pure read"
    );
    assert_eq!(
        app.rain_control(crate::RainCtlOp::On).as_deref(),
        Ok("config_enabled=false session_override=on effective=true \
             engine=none active=false scope=window focused=true animating=true"),
    );
    assert_eq!(
        app.rain_control(crate::RainCtlOp::Toggle).as_deref(),
        Ok("config_enabled=false session_override=off effective=false \
             engine=none active=false scope=window focused=true animating=true"),
    );
    app.config = cfg("[matrix_rain]\nenabled = true");
    app.rain_dirty = true;
    assert_eq!(
        app.rain_control(crate::RainCtlOp::Status).as_deref(),
        Ok("config_enabled=true session_override=off effective=false \
             engine=none active=false scope=window focused=true animating=true"),
        "the off override keeps winning over the enabled config"
    );
}

#[test]
fn fx_control_verb_arms_once_and_reports() {
    let mut app = crate::App::headless_for_test();
    app.config = cfg("cursor_trail = true\ncursor_trail_style = \"rainbow\"");
    let status = app
        .fx_control(crate::FxCtlOp::Status)
        .expect("status reads");
    assert!(
        status.starts_with(
            "armed=none sig=00000000 bars=0 live=false bar=none drive=0.00 cooldown_ms=0 style="
        ),
        "{status}"
    );
    assert!(
        status.ends_with("style=rainbow-kitty"),
        "positive fixture must enable the real style"
    );
    let sig = aterm_effects::kitty_sing::song_signature('C');
    let arm = app.fx_control(crate::FxCtlOp::Celebrate {
        sig,
        bars: 2,
        on: aterm_effects::kitty_sing::CelebrateOn::Green,
    });
    let line = arm.expect("the first arm is admitted");
    assert!(
        line.starts_with(&format!(
            "armed=green sig={sig:08x} bars=2 live=false bar=none drive=0.00 cooldown_ms="
        )),
        "{line}"
    );
    let again = app.fx_control(crate::FxCtlOp::Celebrate {
        sig,
        bars: 1,
        on: aterm_effects::kitty_sing::CelebrateOn::Enter,
    });
    assert!(again.is_err(), "one arm at a time");
    let status = app
        .fx_control(crate::FxCtlOp::Status)
        .expect("status reads");
    assert!(
        status.starts_with("armed=green"),
        "status is a pure read: {status}"
    );
}

#[test]
fn fx_control_refuses_inactive_visual_policies_without_spending_an_arm() {
    use aterm_effects::kitty_sing::CelebrateOn;
    for (config, on) in [
        "cursor_trail = false\ncursor_trail_style = \"rainbow\"",
        "cursor_trail = true\ncursor_trail_style = \"laser\"",
        "cursor_trail = true\ncursor_trail_style = \"rainbow\"\nserious_mode = true",
    ]
    .into_iter()
    .flat_map(|c| [CelebrateOn::Green, CelebrateOn::Done].map(|on| (c, on)))
    {
        let mut app = crate::App::headless_for_test();
        app.config = cfg(config);
        app.apply_serious_mode(app.config.serious_mode_or_default());
        let arm = app.fx_control(crate::FxCtlOp::Celebrate {
            sig: aterm_effects::kitty_sing::song_signature('C'),
            bars: 2,
            on,
        });
        assert!(arm.is_err(), "{config} {on:?}: {arm:?}");
        let status = app
            .fx_control(crate::FxCtlOp::Status)
            .expect("status still reads");
        assert!(
            status.starts_with(
                "armed=none sig=00000000 bars=0 live=false bar=none drive=0.00 cooldown_ms=0 "
            ),
            "refusal spends no arm or cooldown: {status}"
        );
    }
}

#[test]
fn zero_seed_derives_stable_per_window() {
    let base = crate::matrix_rain::RainConfig {
        enabled: true,
        ..Default::default()
    };
    for raw in [0, 1, 2, 0x5048_4F53_5048_4F52, u64::MAX] {
        let wid = crate::WindowId(raw);
        let a = crate::rain_config_for_window(base, wid);
        let b = crate::rain_config_for_window(base, wid);
        assert_ne!(a.seed, 0, "the sentinel resolves to a real seed ({wid:?})");
        assert_eq!(
            a.seed, b.seed,
            "stable for the window's whole life ({wid:?})"
        );
    }
    let pinned = crate::matrix_rain::RainConfig { seed: 7, ..base };
    assert_eq!(
        crate::rain_config_for_window(pinned, crate::WindowId(3)).seed,
        7,
        "a configured seed is reproducible verbatim"
    );
}
