// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The present that lands the GPU-lost remedy (`gpu_lost_remedy_floor`, set on Windows)
/// resolves its row on the center alone: no window draws the row, so the present asks every
/// window for no frame, and it settles nothing, so a strip fall the floor let go waits for
/// the loop's own settle rather than re-gridding every window from inside a present.
#[test]
fn the_present_that_lands_the_gpu_lost_remedy_resolves_its_row_and_asks_no_frame() {
    use aterm_messages::chrome::CHROME_REGROW_SETTLE;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .chrome_regrids_for_test = true;
    let t0 = Instant::now()
        .checked_sub(CHROME_REGROW_SETTLE * 2)
        .expect("the clock reads past two floors");
    app.set_tab_strip_rows(1, t0);
    app.set_tab_strip_rows(0, t0 + Duration::from_millis(20));
    assert_eq!(
        app.tab_strip_rows, 1,
        "PRECONDITION: the floor held the fall"
    );
    let row = app
        .messages
        .post(
            crate::message_reporters::gpu_lost(),
            app.messages_wall.now(),
            Instant::now(),
        )
        .id;
    assert!(
        app.messages.live(row).is_some(),
        "PRECONDITION: the row is live"
    );
    app.gpu_lost_remedy_floor = Some(wid);
    let asks = crate::redraw_all_requests_on_this_thread();
    let plan = app.active_visible_leaf_plan(wid).expect("visible plan");
    app.finalize_successful_present(
        wid,
        metrics::StartupPresentTiming::collapsed(Instant::now()),
        0,
        None,
        SuccessfulPresentRoute::Terminal,
        crate::VisibleContentRoute::Terminal { composed: false },
        HostVisualState::default(),
        None,
        &plan,
    );
    assert_eq!(app.gpu_lost_remedy_floor, None, "the remedy landed");
    assert!(app.messages.live(row).is_none(), "its row is resolved");
    assert_eq!(
        crate::redraw_all_requests_on_this_thread(),
        asks,
        "the present asks every window for no frame"
    );
    assert_eq!(
        app.tab_strip_rows, 1,
        "the present settles nothing: the fall is the loop's settle's"
    );
}
