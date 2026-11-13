// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_delayed_key_after_paste_releases_its_echo_at_the_half_refresh_floor() {
    let mut app = App::headless_for_test();
    let ws = app.windows.get_mut(&WindowId(0)).unwrap();
    let t0 = Instant::now();
    let full = Duration::from_millis(16);
    ws.input_hot = true;
    ws.input_hot_until = Some(t0 + crate::app_input::INPUT_HOT_WINDOW);
    ws.on_content_presented(t0 + Duration::from_millis(60));
    assert!(!ws.input_hot);
    ws.last_present_at = Some(t0 + Duration::from_millis(86));
    ws.content_pending = true;
    ws.redraw_pending = true;
    assert_eq!(ws.content_pace_floor(full), full);

    assert!(ws.queued_key_delivery_releases_pending_frame(
        t0 + Duration::from_millis(90),
        t0 + Duration::from_millis(95),
        full,
    ));
    assert_eq!(ws.content_pace_floor(full), full / 2);
    assert_eq!(ws.input_hot_until, Some(t0 + Duration::from_millis(140)));
    ws.on_content_presented(t0 + Duration::from_millis(100));
    assert!(
        ws.input_hot,
        "a concurrent output frame cannot spend the key tail"
    );
    ws.on_content_presented(t0 + Duration::from_millis(141));
    assert!(!ws.input_hot, "the write-time priority remains bounded");

    ws.content_pending = false;
    ws.redraw_pending = false;
    assert!(!ws.queued_key_delivery_releases_pending_frame(
        t0 + Duration::from_millis(150),
        t0 + Duration::from_millis(155),
        full,
    ));
    assert!(!ws.queued_key_delivery_releases_pending_frame(
        t0 + Duration::from_millis(90),
        t0 + Duration::from_millis(141),
        full,
    ));
}
