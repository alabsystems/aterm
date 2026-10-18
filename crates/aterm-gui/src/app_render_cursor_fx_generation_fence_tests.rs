// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::retire_torn_cursor_fx;
use crate::{App, WindowId};
use aterm_core::terminal::ContentScrollState;
use std::time::{Duration, Instant};

/// A rainbow-kitty window with one lit ribbon cell on glass, built through the real frame path.
fn rainbow_window_with_a_lit_band(t0: Instant) -> (App, WindowId, Instant) {
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    let mut seed = super::CursorFxInputs::sample_for_test(t0);
    seed.cur = Some((2, 2));
    app.cursor_frame(wid, seed).expect("seed cursor engines");
    let typed = t0 + Duration::from_millis(1);
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.last_key_at = Some(typed);
        ws.fx.cursor_mut().cadence.on_keystroke(typed);
        ws.fx.cursor_mut().glow.note_typed(typed);
    }
    let mut live = super::CursorFxInputs::sample_for_test(t0 + Duration::from_millis(2));
    live.cur = Some((2, 3));
    app.cursor_frame(wid, live).expect("live cursor tick");
    // Settled past the ribbon's 18 ms edge-in attack, so each seam below is
    // judged on whether the light only goes down from here.
    let settled = t0 + Duration::from_millis(40);
    let mut on = super::CursorFxInputs::sample_for_test(settled);
    on.cur = Some((2, 3));
    app.cursor_frame(wid, on).expect("settle tick");
    (app, wid, settled)
}

/// The brightest quad of the `glow_under` stream — the ribbon body's own.
fn band_peak(app: &App, wid: WindowId) -> u8 {
    app.windows[&wid]
        .fx
        .cursor()
        .glow
        .under_quads()
        .iter()
        .map(|q| q.alpha)
        .max()
        .unwrap_or(0)
}

/// All the light the `glow_under` stream lays on glass: each quad's
/// premultiplied colour at both ends of its ramp, over its pixel area. What a
/// MOVING band's "brighter or not" is read from ([`band_peak`] is not: see the
/// curtain test).
fn band_light(app: &App, wid: WindowId) -> u64 {
    let rgb =
        |c: u32| u64::from((c >> 16) & 0xFF) + u64::from((c >> 8) & 0xFF) + u64::from(c & 0xFF);
    app.windows[&wid]
        .fx
        .cursor()
        .glow
        .under_quads()
        .iter()
        .map(|q| (rgb(q.color) + rgb(q.color2)) * u64::from(q.w) * u64::from(q.h))
        .sum()
}

/// `(leftmost x, rightmost x + w)` of the band on glass, in window pixels.
fn band_span(app: &App, wid: WindowId) -> Option<(i32, i32)> {
    let quads = app.windows[&wid].fx.cursor().glow.under_quads();
    let l = quads.iter().map(|q| i32::from(q.x)).min()?;
    let r = quads
        .iter()
        .map(|q| i32::from(q.x) + i32::from(q.w))
        .max()?;
    Some((l, r))
}

fn tick_at(app: &mut App, wid: WindowId, at: Instant, cur: (u16, u16), live_viewport: bool) {
    let mut fx = super::CursorFxInputs::sample_for_test(at);
    fx.cur = Some(cur);
    fx.live_viewport = live_viewport;
    app.cursor_frame(wid, fx).expect("cursor tick");
}

/// The real `retire_torn_cursor_fx` seam (alt screen, tab switch, re-grid) curtains the ribbon
/// instead of cutting it: still on glass half-way through the 0.24 s curtain, exactly gone at its
/// end, with the engine still owning the frame and every other coordinate-bound owner retired.
#[test]
fn a_coordinate_space_seam_curtains_the_rainbow_ribbon_over_a_quarter_second() {
    // The seam reads the wall clock itself, so the band is laid 300 ms in the past (whole and
    // settled) and the seam falls at the real present.
    let t0 = Instant::now() - Duration::from_millis(300);
    let (mut app, wid, _settled) = rainbow_window_with_a_lit_band(t0);
    let full = band_peak(&app, wid);
    assert!(
        full > 0,
        "precondition: the typed key laid a lit ribbon cell"
    );
    let full_light = band_light(&app, wid);
    assert!(
        full_light > 0,
        "precondition: the lit ribbon cell lays light"
    );

    // The seam, on its own clock (`Instant::now()` inside it).
    retire_torn_cursor_fx(app.windows.get_mut(&wid).expect("window"));
    let fell = Instant::now();
    assert!(
        app.windows[&wid]
            .fx
            .cursor()
            .glow
            .v2_ribbon()
            .is_some_and(|r| r.curtained() && !r.at_rest()),
        "the ribbon is curtaining, its cells kept"
    );
    assert!(
        !app.windows[&wid].fx.cursor().trail.is_active(),
        "…while the other engines are retired as before"
    );
    // Law A2: the light only goes down and draws on at least three 30 fps frames; six 33 ms
    // frames fit inside the 0.24 s curtain.
    //
    // "Never brightens" is read off the band's whole light, not its brightest quad: the band
    // MOVES under a curtain (drawn into the hand), and a quad's opacity follows the hue sampled
    // where it lands (the per-hue ceilings of `GlowQuad::alpha`). The 2026-09-29 snapshot
    // (6ad1a274d8) recorded it with a 10 px cell on Windows: the brightest quad re-snapped at
    // +132 ms from x=27 w=3 to x=28 w=2 before its cell's turn to spend, one row reading 218
    // against 217, while the band's light fell by a third.
    let mut prev = full_light;
    let mut spending = 0;
    for ms in [33_u64, 66, 99, 132, 165, 198] {
        tick_at(
            &mut app,
            wid,
            fell + Duration::from_millis(ms),
            (2, 3),
            true,
        );
        let now = band_light(&app, wid);
        assert!(
            now <= prev,
            "+{ms} ms: the curtain never brightens (light {prev} -> {now})"
        );
        if band_peak(&app, wid) > 0 {
            spending += 1;
        }
        prev = now;
    }
    assert!(
        spending >= 3,
        "the curtain draws on at least three frames at 30 fps (A2), not {spending}"
    );
    assert!(
        app.windows[&wid].fx.cursor().glow.is_active(),
        "…and the glow reports itself live so the frame train keeps running"
    );
    tick_at(
        &mut app,
        wid,
        fell + Duration::from_millis(260),
        (2, 3),
        true,
    );
    assert_eq!(
        band_peak(&app, wid),
        0,
        "at the curtain's end the body is exactly gone"
    );
    assert!(
        app.windows[&wid]
            .fx
            .cursor()
            .glow
            .v2_ribbon()
            .is_some_and(|r| r.at_rest() && !r.curtained()),
        "…and the pool is empty, the engine still owning the frame"
    );
}

/// Entering the alternate screen curtains the band (`sync_cursor_effect_coordinate_space`);
/// leaving it fires the same seam over an empty pool, since the band was spent, not parked.
#[test]
fn entering_the_alternate_screen_curtains_the_band_and_leaving_it_returns_nothing() {
    // Laid 300 ms back: the seam reads the wall clock.
    let t0 = Instant::now() - Duration::from_millis(300);
    let (mut app, wid, _settled) = rainbow_window_with_a_lit_band(t0);
    assert!(band_peak(&app, wid) > 0, "precondition: a lit band");
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        // The frame path's own first observation of this space.
        super::sync_cursor_effect_coordinate_space(ws, 7, false);
        // …then the program enters the alternate screen.
        assert!(
            super::sync_cursor_effect_coordinate_space(ws, 7, true),
            "the alt-screen edge is a coordinate-space change"
        );
    }
    let fell = Instant::now();
    assert!(
        app.windows[&wid]
            .fx
            .cursor()
            .glow
            .v2_ribbon()
            .is_some_and(|r| r.curtained()),
        "alt-screen ENTRY drops the curtain (D-2), it does not cut"
    );
    tick_at(
        &mut app,
        wid,
        fell + Duration::from_millis(100),
        (2, 3),
        true,
    );
    assert!(
        band_peak(&app, wid) > 0,
        "…and the app's first alt-screen frames are drawn under a band that is still there"
    );
    tick_at(
        &mut app,
        wid,
        fell + Duration::from_millis(260),
        (2, 3),
        true,
    );
    assert_eq!(band_peak(&app, wid), 0, "gone at the curtain's end");
    // Exit: nothing returns (D-2); the seam fires on an empty pool.
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        assert!(super::sync_cursor_effect_coordinate_space(ws, 7, false));
    }
    tick_at(
        &mut app,
        wid,
        fell + Duration::from_millis(300),
        (2, 3),
        true,
    );
    assert_eq!(
        band_peak(&app, wid),
        0,
        "leaving the alternate screen brings nothing back"
    );
    assert!(
        app.windows[&wid]
            .fx
            .cursor()
            .glow
            .v2_ribbon()
            .is_some_and(|r| r.at_rest() && !r.curtained()),
        "…and no second curtain was armed over an empty pool"
    );
}

/// Every other coordinate-space seam curtains the same way, one row per seam, each asserting its
/// trigger held: it curtains (never cuts), still draws half-way through, and is gone at the end.
#[test]
fn every_coordinate_space_seam_curtains_the_band() {
    type Seam = fn(&mut App, WindowId);
    let rows: [(&str, Seam); 3] = [
        (
            // A terminal/tab switch: the `terminal_id` changes with the front
            // tab or pane while the screen flag is unchanged.
            "a terminal switch",
            |app, wid| {
                let ws = app.windows.get_mut(&wid).expect("window");
                super::sync_cursor_effect_coordinate_space(ws, 7, false);
                assert!(
                    super::sync_cursor_effect_coordinate_space(ws, 8, false),
                    "a different terminal is a different coordinate space"
                );
            },
        ),
        (
            // A content invalidation: `sync_cursor_effect_scroll`'s
            // `Invalidate` arm (RIS, `ESC[3J`, a splice), where retained
            // coordinates stop sharing one transform.
            "a content invalidation",
            |app, wid| {
                let ws = app.windows.get_mut(&wid).expect("window");
                super::sync_cursor_effect_scroll(ws, ContentScrollState::default());
                let change = super::sync_cursor_effect_scroll(
                    ws,
                    ContentScrollState {
                        invalidation_epoch: 9,
                        ..ContentScrollState::default()
                    },
                );
                assert!(change.invalidated, "precondition: the arm under test");
            },
        ),
        (
            // The resize seam: `apply_term_resize` changes the grid's meaning
            // under every retained cell, and `resize` exposes no reflow map to
            // translate cells through, so the curtain is the whole law here.
            "a grid resize",
            |app, wid| {
                let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
                app.apply_term_resize(
                    wid,
                    rows.saturating_sub(2).max(2),
                    cols.saturating_sub(7).max(8),
                );
            },
        ),
    ];
    for (label, seam) in rows {
        let t0 = Instant::now() - Duration::from_millis(300);
        let (mut app, wid, _settled) = rainbow_window_with_a_lit_band(t0);
        assert!(
            band_peak(&app, wid) > 0,
            "{label}: precondition: a lit band"
        );
        seam(&mut app, wid);
        let fell = Instant::now();
        assert!(
            app.windows[&wid]
                .fx
                .cursor()
                .glow
                .v2_ribbon()
                .is_some_and(|r| r.curtained() && !r.at_rest()),
            "{label} curtains the band, it does not cut it"
        );
        tick_at(
            &mut app,
            wid,
            fell + Duration::from_millis(100),
            (2, 3),
            true,
        );
        assert!(
            band_peak(&app, wid) > 0,
            "{label}: still drawing half-way through"
        );
        tick_at(
            &mut app,
            wid,
            fell + Duration::from_millis(260),
            (2, 3),
            true,
        );
        assert_eq!(
            band_peak(&app, wid),
            0,
            "{label}: gone at the curtain's end"
        );
    }
}

/// Scrollback hides the band and the live viewport brings it back on its own
/// clocks: nothing is composed while history shows (no `glow_under` quad),
/// every clock runs underneath, and the live viewport returns the band at
/// the same span, dimmer by the time that passed.
#[test]
fn a_history_viewport_hides_the_band_and_the_live_viewport_returns_it_on_its_own_clocks() {
    // Every clock here is injected: the hide seam reads no wall clock of its own.
    let t0 = Instant::now();
    let (mut app, wid, settled) = rainbow_window_with_a_lit_band(t0);
    let before = band_peak(&app, wid);
    let span = band_span(&app, wid).expect("a band on glass");
    assert!(before > 0, "precondition: a lit band");
    let cells = app.windows[&wid]
        .fx
        .cursor()
        .glow
        .v2_ribbon()
        .map(|r| r.cells().len())
        .expect("v2 owns the frame");

    // History: three frames of it, nothing composed, nothing destroyed.
    for ms in [20_u64, 60, 120] {
        tick_at(
            &mut app,
            wid,
            settled + Duration::from_millis(ms),
            (2, 3),
            false,
        );
        assert_eq!(
            app.windows[&wid].fx.cursor().glow.under_quads().len(),
            0,
            "+{ms} ms of history composes no ribbon quad (A4)"
        );
        assert_eq!(
            app.windows[&wid]
                .fx
                .cursor()
                .glow
                .v2_ribbon()
                .map(|r| r.cells().len()),
            Some(cells),
            "+{ms} ms of history keeps every cell — a hide is not a reset"
        );
    }

    // Back: the same span, dimmer by the elapsed time.
    tick_at(
        &mut app,
        wid,
        settled + Duration::from_millis(160),
        (2, 3),
        true,
    );
    let after = band_peak(&app, wid);
    assert!(after > 0, "the live viewport brings the band back");
    assert_eq!(
        band_span(&app, wid),
        Some(span),
        "…at the span its clocks say, not re-laid and not moved"
    );
    assert!(
        after <= before,
        "…and its clocks ran while it was hidden ({before} -> {after})"
    );
    assert!(
        app.windows[&wid]
            .fx
            .cursor()
            .glow
            .v2_ribbon()
            .is_some_and(|r| !r.curtained()),
        "a history viewport is not a coordinate-space seam: no curtain (D-4)"
    );
}
