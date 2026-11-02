// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Diagnostic records stay readable without visible alert rows.

use std::time::{Duration, Instant};

use aterm_messages::{
    ANIM_FRAME, Amount, Hold, Message, Meter, STALE_UPDATE, Severity, Unit, tags,
};

use crate::{App, WindowId};

const WID: WindowId = WindowId(0);

/// Headless `App` with window 0 on screen, focused, and every motion gate open.
fn on_screen_app() -> App {
    let mut app = App::headless_for_test();
    let ws = app.windows.get_mut(&WID).expect("window 0");
    ws.band_on_screen_for_test = true;
    ws.focused = true;
    app.system_reduce_motion = false;
    app.serious_mode = false;
    app.perf_reduced = false;
    app.config.motion = None;
    app
}

/// A live update download with an amount: a determinate bar with an ETA slot.
fn download(done: u64) -> Message {
    let total = 74_000_000;
    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.92.0")
        .meter(Meter {
            fill_permille: Some(u16::try_from(done * 1000 / total).unwrap()),
            stats: String::new(),
            amount: Some(Amount {
                series: Amount::series_of("aterm 0.92.0"),
                done,
                total,
                unit: Unit::Bytes,
            }),
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: STALE_UPDATE,
        })
        .key("update.progress")
}

/// `appstatus` output is invariant across time and band animation frames.
#[test]
fn appstatus_carries_no_motion_words() {
    let mut app = on_screen_app();
    app.post_message(download(30_000_000));
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    let read = |app: &App, at: Instant| {
        aterm_messages::wire::activity_rows_compat(&app.messages, at, &crate::control::pct_encode)
    };
    let now = Instant::now();
    let first = read(&app, now);
    assert_eq!(first.len(), 2, "{first:?}");
    for k in 0..60u32 {
        app.prepare_band_motion(WID, now + ANIM_FRAME * k);
    }
    assert_eq!(read(&app, now + Duration::from_secs(30)), first);
}

/// `Hold::LogOnly` messages stay off the glass and appear in Settings ▸ Messages newest first.
#[test]
fn the_messages_page_lists_log_only_records() {
    let mut app = on_screen_app();
    let first = app.record_message(Message::new(
        tags::TOOLCHAIN,
        Severity::Info,
        "ALab tools installed",
    ));
    let second = app.record_message(
        Message::new(tags::UPDATE, Severity::Warn, "aterm update failed")
            .line("could not verify the download"),
    );
    assert_eq!(app.message_band_rows, 0, "never on the glass");
    assert_eq!(app.messages_deadline(), None);
    let state = app.messages_state();
    let ids: Vec<u64> = state.entries.iter().map(|e| e.id).collect();
    assert_eq!(ids, [second.raw(), first.raw()], "newest first");
    for id in [first, second] {
        let e = state.entry(id.raw()).expect("listed");
        assert_eq!(e.state, "recorded");
    }
    assert_eq!(
        state.entry(second.raw()).unwrap().detail,
        vec!["could not verify the download"],
        "its detail rides the page"
    );
}
