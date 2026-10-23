// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the output-to-redraw-request span and IME preedit-to-commit clock.

use crate::WindowId;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn close_pipe(pipe: [i32; 2]) {
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

#[test]
fn the_output_to_redraw_request_span_is_booked_once_per_burst() {
    let (mut app, _sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    app.lat_epoch = Instant::now()
        .checked_sub(Duration::from_secs(10))
        .expect("an epoch ten seconds back");
    let session = app
        .front_terminal_mirror(WindowId(0))
        .expect("terminal")
        .session;
    let stamp_ms_ago = |ms: u64| {
        let now = u64::try_from(app.lat_epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let stamp = now.saturating_sub(ms * 1_000_000).max(1);
        app.pool
            .get(session)
            .expect("session")
            .last_output_ns
            .store(stamp, Ordering::Relaxed);
    };
    let requests = || crate::metrics::METRICS.window.input.out_req.copy_out().n();
    let before = requests();
    stamp_ms_ago(7);
    let span = app
        .note_output_redraw_request(session)
        .expect("the first request after the edge is booked");
    assert!(span >= 7_000_000, "measured from the edge: {span}");
    assert_eq!(
        app.note_output_redraw_request(session),
        None,
        "a second wake for the same burst is not a second request"
    );
    stamp_ms_ago(3);
    assert!(
        app.note_output_redraw_request(session).is_some(),
        "the next burst books its own span"
    );
    assert!(requests() >= before + 2);
    let cap_ms = crate::metrics::OUTPUT_REDRAW_REQUEST_CAP_NS / 1_000_000;
    stamp_ms_ago(cap_ms + 5);
    assert_eq!(
        app.note_output_redraw_request(session),
        None,
        "a stamp older than the cap aged unwatched: never booked"
    );
    drop(app);
    close_pipe(pipe);
}

#[test]
fn the_ime_clock_times_preedit_to_commit_and_counts_cancels() {
    let (mut app, _sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    let wid = WindowId(0);
    let composed_before = crate::metrics::METRICS.window.input.ime.copy_out().n();
    app.on_ime_preedit(wid, "に".into(), Some((0, 3)));
    let started = app.windows[&wid].ime_compose.expect("the clock started").0;
    app.on_ime_preedit(wid, "にほ".into(), Some((0, 6)));
    assert_eq!(
        app.windows[&wid].ime_compose.map(|c| c.0),
        Some(started),
        "a growing composition keeps its start"
    );
    app.on_ime_preedit(wid, String::new(), None);
    assert_eq!(
        app.windows[&wid].ime_compose,
        Some((started, true)),
        "the platform's clear before a commit keeps the start"
    );
    app.on_ime_commit(wid, "日本".into());
    assert_eq!(app.windows[&wid].ime_compose, None);
    assert!(
        crate::metrics::METRICS.window.input.ime.copy_out().n() > composed_before,
        "the committed composition booked its duration"
    );

    let now = Instant::now();
    let open = super::ime_compose_on_preedit(None, true, now);
    let cleared = super::ime_compose_on_preedit(open, false, now);
    assert_eq!(cleared, Some((now, true)));
    let later = now + Duration::from_millis(5);
    assert_eq!(
        super::ime_compose_on_preedit(cleared, true, later),
        Some((later, false)),
        "a new composition restarts the clock"
    );
    assert_eq!(super::ime_compose_on_preedit(None, false, now), None);
    drop(app);
    close_pipe(pipe);
}
