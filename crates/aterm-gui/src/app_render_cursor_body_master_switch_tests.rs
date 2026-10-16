// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::CursorFxInputs;
use crate::{App, WindowId};
use std::time::Instant;

/// Every trail style that owns a block-cursor body engine, as a user spells
/// it in `cursor_trail_style`. `rainbow kitty pet` is the default.
const BODY_STYLES: [&str; 4] = ["rainbow kitty pet", "water", "comet", "phaser"];

/// Drive one focused block-cursor frame. Returns the additive glow quads in `glow_scratch` (they
/// become `RenderInput::cursor_glow_add` and demand-build `EffectPipeline::GlowAdd`) and whether
/// a body effect claimed the caret's fill.
fn one_focused_block_frame(trail_on: bool, style: &str) -> (usize, bool) {
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(trail_on);
    app.config.cursor_trail_style = Some(style.into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).expect("headless window");
        window.focused = true;
        window.poof_row_buf = vec![' '; 80];
    }
    let mut fx = CursorFxInputs::sample_for_test(Instant::now());
    fx.cur = Some((2, 2));
    let tick = app.cursor_frame(wid, fx).expect("cursor fx tick");
    let window = app.windows.get(&wid).expect("headless window");
    (
        window.fx.cursor().glow_scratch.len(),
        tick.block_fill.is_some(),
    )
}

/// `cursor_trail = false` masters the whole cursor-effect family: no block-body engine publishes
/// an idle halo into `glow_scratch` or a `cursor_fill_override`, so nothing builds `glow_add`.
#[test]
fn the_cursor_trail_master_silences_every_block_cursor_body() {
    for style in BODY_STYLES {
        // Control, master on: the style really drives the caret.
        let (on_quads, on_body) = one_focused_block_frame(true, style);
        assert!(
            on_quads > 0 && on_body,
            "control: `{style}` must light the caret with cursor_trail = true \
             (quads={on_quads}, body={on_body})"
        );

        let (off_quads, off_body) = one_focused_block_frame(false, style);
        assert_eq!(
            off_quads, 0,
            "`{style}` published {off_quads} additive glow quads with \
             cursor_trail = false — that stream demand-builds the glow_add pipeline"
        );
        assert!(
            !off_body,
            "`{style}` claimed the caret's body fill with cursor_trail = false"
        );
    }
}

/// The master is read every frame: flipping `cursor_trail` at runtime (hot reload, Settings)
/// lights the body on the next frame of the same window.
#[test]
fn switching_the_master_on_at_runtime_lights_the_body_on_the_next_frame() {
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(false);
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).expect("headless window");
        window.focused = true;
        window.poof_row_buf = vec![' '; 80];
    }
    let t0 = Instant::now();

    let mut dark = CursorFxInputs::sample_for_test(t0);
    dark.cur = Some((2, 2));
    let dark_tick = app.cursor_frame(wid, dark).expect("dark frame");
    assert!(
        app.windows
            .get(&wid)
            .expect("window")
            .fx
            .cursor()
            .glow_scratch
            .is_empty()
            && dark_tick.block_fill.is_none(),
        "precondition: the master is off, so this frame draws no body"
    );

    // The hot-reload seam: the only thing that changes is the config.
    app.config.cursor_trail = Some(true);

    let mut lit = CursorFxInputs::sample_for_test(t0 + std::time::Duration::from_millis(16));
    lit.cur = Some((2, 2));
    let lit_tick = app.cursor_frame(wid, lit).expect("lit frame");
    let window = app.windows.get(&wid).expect("headless window");
    assert!(
        !window.fx.cursor().glow_scratch.is_empty(),
        "a master switched on at runtime must publish its glow stream on the next frame"
    );
    assert!(
        lit_tick.block_fill.is_some(),
        "a master switched on at runtime must reclaim the caret's body on the next frame"
    );
}

/// The master also darkens the momentum glow, which every style (`off` included) otherwise lights
/// on fast typing. The master-on arm is the control.
#[test]
fn a_master_off_window_typed_into_fast_lights_no_momentum_glow() {
    for master in [true, false] {
        let mut app = App::headless_for_test();
        app.config.motion = Some("full".into());
        app.config.cursor_trail = Some(master);
        app.config.cursor_trail_style = Some("off".into());
        app.config.trail_sounds = Some(false);
        let wid = WindowId(0);
        let t0 = Instant::now();
        {
            let window = app.windows.get_mut(&wid).expect("headless window");
            window.focused = true;
            for k in 0..24u64 {
                window.fx.cursor_mut().momentum.on_key(
                    t0 + std::time::Duration::from_millis(k * 40),
                    aterm_effects::cursor_momentum::MOMENTUM_GLOW_TAU_S,
                );
            }
        }
        let mut fx =
            CursorFxInputs::sample_for_test(t0 + std::time::Duration::from_millis(24 * 40));
        fx.cur = Some((2, 2));
        let tick = app.cursor_frame(wid, fx).expect("tick");
        let lit = !app.windows[&wid].fx.cursor().glow_scratch.is_empty();
        if master {
            assert!(
                lit,
                "control: a fast hand lights the momentum glow under the `off` style"
            );
        } else {
            assert!(
                !lit && tick.block_fill.is_none() && tick.momentum_steady.is_none(),
                "the master off paints no momentum halo and claims no caret"
            );
        }
    }
}

/// `fx_under_frames` counts ticks whose `glow_under` stream (the rainbow ribbon body) was
/// non-empty. The counters are process-global, so the positive arm reads a delta and the zero
/// arm the window's own stream. Control: the `off` style composes `glow_add` and no `glow_under`.
#[test]
fn the_cursor_effect_tick_publishes_its_ribbon_witness() {
    let t0 = Instant::now();
    let before = crate::metrics::snapshot().fx_under_frames;
    let (app, wid, _) = ribbon_witness_take("rainbow kitty", t0);
    let window = &app.windows[&wid];
    assert!(
        !window.fx.cursor().glow.under_quads().is_empty(),
        "fixture: the rainbow kitty band is on the frame"
    );
    assert!(
        crate::metrics::snapshot().fx_under_frames > before,
        "a tick that composed the band books an under frame"
    );

    let (app, wid, _) = ribbon_witness_take("off", t0);
    let window = &app.windows[&wid];
    assert!(
        !window.fx.cursor().glow_scratch.is_empty(),
        "control: the off style still lights glow_add, which the any-quad witness counted"
    );
    assert!(
        window.fx.cursor().glow.under_quads().is_empty(),
        "the off style composes no glow_under, so the witness reads 0"
    );
}

/// Type three licensed keys into a focused window of `style` through the real tick path, each
/// fed to the engines the printable-key arm feeds.
fn ribbon_witness_take(style: &str, t0: Instant) -> (App, WindowId, Instant) {
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some(style.into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).expect("headless window");
        window.focused = true;
        window.poof_row_buf = vec![' '; 80];
    }
    let mut seed = CursorFxInputs::sample_for_test(t0);
    seed.cur = Some((2, 2));
    app.cursor_frame(wid, seed).expect("seed cursor engines");
    let mut at = t0;
    for col in 3..=5u16 {
        let typed = at + std::time::Duration::from_millis(1);
        {
            let ws = app.windows.get_mut(&wid).expect("window");
            ws.last_key_at = Some(typed);
            ws.fx.cursor_mut().cadence.on_keystroke(typed);
            ws.fx.cursor_mut().glow.note_typed(typed);
            // The printable-key arm's feed, as `app_input` makes it.
            ws.fx
                .cursor_mut()
                .momentum
                .on_key(typed, aterm_effects::cursor_momentum::MOMENTUM_GLOW_TAU_S);
        }
        at = typed + std::time::Duration::from_millis(1);
        let mut live = CursorFxInputs::sample_for_test(at);
        live.cur = Some((2, col));
        app.cursor_frame(wid, live).expect("live cursor tick");
    }
    let settled = at + std::time::Duration::from_millis(40);
    let mut on = CursorFxInputs::sample_for_test(settled);
    on.cur = Some((2, 5));
    app.cursor_frame(wid, on).expect("settle tick");
    (app, wid, settled)
}
