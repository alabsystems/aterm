// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId, term_lock};
use std::time::{Duration, Instant};

fn compose(app: &mut App, wid: WindowId, now: Instant) -> bool {
    app.redraw_compose(wid, 24, 80, false, false, None, 0, now)
        .is_some()
}

fn settle(app: &mut App, wid: WindowId, now: &mut Instant) {
    let mut settled = false;
    for _ in 0..40 {
        *now += Duration::from_millis(16);
        if !compose(app, wid, *now) {
            settled = true;
            break;
        }
    }
    assert!(settled, "the fixture must reach a settled (skipping) state");
    *now += Duration::from_millis(16);
    assert!(!compose(app, wid, *now), "settled stays settled");
}

#[test]
fn recovery_edge_buys_one_present_not_a_train() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("window").focused = true;
    let mut now = Instant::now();
    settle(&mut app, wid, &mut now);
    app.windows
        .get_mut(&wid)
        .expect("window")
        .present_retry
        .recovery_redraw_outstanding = true;
    now += Duration::from_millis(16);
    assert!(
        compose(&mut app, wid, now),
        "the delivered edge services one recomposed present"
    );
    assert!(
        !app.windows[&wid].present_retry.recovery_redraw_outstanding,
        "the composed commit acknowledged the delivered edge"
    );
    now += Duration::from_millis(16);
    assert!(
        !compose(&mut app, wid, now),
        "and the train ends: the settled compose takes the early-out"
    );
}

#[test]
fn split_settles_and_every_real_change_still_repaints() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(false);
    app.config.cursor_trail_style = Some("off".into());
    app.config.output_streak = Some(crate::app_config::OutputStreakConfig {
        enabled: Some(false),
        ..Default::default()
    });
    app.kitty_cursor_enabled_cache = None;
    app.recompute_sparkle();
    let sid = app.split_active_stub_tab(wid);
    app.windows.get_mut(&wid).expect("window").focused = true;
    let mut now = Instant::now();
    settle(&mut app, wid, &mut now);

    let bg = app.pool.get(0).expect("background session").term.clone();
    term_lock(&bg).process(b"background write");
    now += Duration::from_millis(16);
    assert!(
        compose(&mut app, wid, now),
        "background PTY damage repaints"
    );
    settle(&mut app, wid, &mut now);

    let fg = app.pool.get(sid).expect("focused session").term.clone();
    term_lock(&fg).process(b"\x1b[C");
    now += Duration::from_millis(16);
    assert!(
        compose(&mut app, wid, now),
        "a damage-free cursor move repaints"
    );
    settle(&mut app, wid, &mut now);

    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.blink_phase = !ws.blink_phase;
    }
    now += Duration::from_millis(16);
    assert!(compose(&mut app, wid, now), "a blink phase flip repaints");
    settle(&mut app, wid, &mut now);

    app.config.show_build_badge = Some(true);
    now += Duration::from_millis(16);
    assert!(compose(&mut app, wid, now), "a config change repaints");
    settle(&mut app, wid, &mut now);
}
