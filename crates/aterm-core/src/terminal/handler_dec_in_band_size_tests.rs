// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::terminal::Terminal;

#[test]
fn mode_2048_reports_no_pixels_before_a_host_measures() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?2048h");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[48;24;80;0;0t"
    );
}

#[test]
fn mode_2048_reports_a_new_cell_box_at_the_same_size() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?2048h");
    let _ = term.take_response();
    term.set_cell_pixel_size(9, 19);
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[48;24;80;456;720t"
    );
    term.set_cell_pixel_size(18, 38);
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[48;24;80;912;1440t"
    );
    term.set_cell_pixel_size(18, 38);
    assert!(
        term.take_response().is_none(),
        "an unchanged box (a host's next layout pass) reports nothing"
    );
}

#[test]
fn mode_2048_repeated_enable_emits_once() {
    let mut term = Terminal::new(24, 80);
    let mut input = Vec::new();
    for _ in 0..10_000 {
        input.extend_from_slice(b"\x1b[?2048h");
    }
    term.process(&input);
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[48;24;80;0;0t",
        "10k repeated enables emit exactly one report"
    );
}

/// A cell-box storm against a host that does not drain: a host call's report passes no
/// rate limit, so the cap alone bounds what is owed, and the storm reaches it.
#[test]
fn a_cell_box_storm_stops_at_the_response_cap() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?2048h");
    let _ = term.take_response();
    let one = b"\x1b[48;24;80;480;880t".len();
    for _ in 0..70_000 {
        term.set_cell_pixel_size(10, 20);
        term.set_cell_pixel_size(11, 20);
    }
    let owed = term.take_response().unwrap_or_default().len();
    let cap = crate::terminal::MAX_RESPONSE_BUFFER_SIZE;
    assert!(owed <= cap, "{owed} owed past the {cap}-byte cap");
    assert!(
        owed > cap - one,
        "the storm ({} B) reaches the cap: {owed}",
        140_000 * one
    );
}

/// A toggle flood draws on the bucket every program reply draws on: at one clock
/// reading the flood and the queries after it share one burst.
#[test]
fn mode_2048_toggle_flood_spends_the_reply_rate_limit() {
    let mut term = Terminal::new(24, 80);
    let clock = crate::terminal::ClockReading::now();
    let mut input = Vec::new();
    for _ in 0..200_000 {
        input.extend_from_slice(b"\x1b[?2048l\x1b[?2048h");
    }
    term.process_at(&input, clock);
    let flood = term.take_response().unwrap_or_default();
    term.process_at(&b"\x1b[6n".repeat(100), clock);
    let replies = term.take_response().unwrap_or_default();
    let burst = crate::terminal::response_rate_limiter::DEFAULT_BURST_BYTES;
    assert!(!flood.is_empty(), "the flood's first reports go out");
    assert!(
        (flood.len() + replies.len()) as u64 <= burst,
        "the flood ({}) and the queries after it ({}) share the {burst}-byte burst",
        flood.len(),
        replies.len()
    );
}

/// A policy that drops every program reply drops the report the enable provokes,
/// while the mode stays on and a host resize still reports.
#[test]
fn a_policy_that_drops_replies_drops_the_enable_report() {
    let mut term = Terminal::new(24, 80);
    term.apply_policy_engine(silencing_policy());
    term.process(b"\x1b[6n");
    assert!(
        term.take_response().is_none(),
        "PRECONDITION: the policy drops a query reply"
    );
    term.process(b"\x1b[?2048h");
    assert!(
        term.take_response().is_none(),
        "the enable report is dropped"
    );
    assert!(term.modes().in_band_size_reports, "the mode is on");
    term.resize(30, 100);
    assert_eq!(
        term.take_reports().unwrap_or_default(),
        b"\x1b[48;30;100;0;0t",
        "a host resize is not a program reply"
    );
}

fn silencing_policy() -> aterm_policy::engine::PolicyEngine {
    use aterm_policy::{OriginTag, Response};
    aterm_policy::engine::PolicyEngine::new(crate::terminal::policy_bridge::one_rule_policy(
        "response any",
        Response::Drop,
        OriginTag::NetworkUntrusted,
        Response::Execute,
    ))
}

#[test]
fn mode_2048_emits_on_resize_and_honors_cell_px() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    term.process(b"\x1b[?2048h");
    let _ = term.take_response();
    term.resize(30, 100);
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[48;30;100;600;1000t"
    );
}

#[test]
fn a_resize_report_is_taken_apart_from_the_replies() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?2048h");
    let _ = term.take_response();
    term.resize(30, 100);
    term.process(b"\x1b[6n");
    assert_eq!(
        term.take_reports().unwrap_or_default(),
        b"\x1b[48;30;100;0;0t"
    );
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[1;1R");
    term.resize(24, 80);
    term.process(b"\x1b[6n");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[1;1R\x1b[48;24;80;0;0t"
    );
    assert!(term.take_reports().is_none(), "taken with the replies");
}

#[test]
fn no_report_when_2048_disabled() {
    let mut term = Terminal::new(24, 80);
    term.resize(30, 100);
    assert!(
        term.take_response().is_none(),
        "no in-band report unless mode 2048 is set"
    );
}

fn console_host_tab() -> Terminal {
    let mut term = Terminal::new(30, 100);
    term.set_console_host_posture(crate::grid::ResizePolicy::ConPty);
    term
}

#[test]
fn behind_a_console_host_the_enable_report_waits_for_the_alternate_screen() {
    for (enter, leave) in [
        (&b"\x1b[?1049h"[..], &b"\x1b[?1049l"[..]),
        (b"\x1b[?1047h", b"\x1b[?1047l"),
        (b"\x1b[?47h", b"\x1b[?47l"),
    ] {
        let mut term = console_host_tab();
        term.process(b"\x1b[?2048h");
        assert!(term.take_response().is_none(), "{enter:?}: held");
        assert!(term.modes().in_band_size_reports, "the mode is on");
        term.resize(30, 90);
        let _ = term.take_response();
        term.process(enter);
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;30;90;0;0t",
            "{enter:?}"
        );
        term.process(leave);
        term.process(enter);
        assert!(term.take_response().is_none(), "{enter:?}: sent once");
    }
}

/// The held report is still the enable's reply when the alternate screen releases it.
/// Negative control: with no policy the same tab holds the enable and the entry releases it.
#[test]
fn a_policy_that_drops_replies_drops_the_held_report() {
    let mut open = console_host_tab();
    open.process(b"\x1b[?2048h");
    assert!(open.take_response().is_none(), "PRECONDITION: held");
    open.process(b"\x1b[?1049h");
    assert_eq!(
        open.take_response().unwrap_or_default(),
        b"\x1b[48;30;100;0;0t",
        "NEG: no policy releases it"
    );

    let mut term = console_host_tab();
    term.apply_policy_engine(silencing_policy());
    term.process(b"\x1b[?2048h");
    term.process(b"\x1b[?1049h");
    assert!(
        term.take_response().is_none(),
        "the released report is dropped"
    );
    assert!(term.modes().in_band_size_reports, "the mode is on");
}

#[test]
fn behind_a_console_host_an_enable_on_the_alternate_screen_reports_at_once() {
    let mut term = console_host_tab();
    term.process(b"\x1b[?1049h\x1b[?2048h");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[48;30;100;0;0t"
    );
}

#[test]
fn a_held_report_is_dropped_when_its_program_is_done() {
    for end in [
        &b"\x1b[?2048l"[..],
        b"\x1b]133;A\x07PS> ",
        b"\x1b]633;A\x07PS> ",
        b"\x1bc",
    ] {
        let mut term = console_host_tab();
        term.process(b"\x1b[?2048h");
        term.process(end);
        term.process(b"\x1b[?1049h");
        assert!(term.take_response().is_none(), "{end:?}");
    }
    let mut term = console_host_tab();
    term.process(b"\x1b[?2048h\x1b]133;A\x07PS> \x1b[?1049h\x1b[?2048l\x1b[?2048h");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[48;30;100;0;0t"
    );
}

#[test]
fn a_native_session_reports_the_enable_at_once() {
    let mut term = Terminal::new(30, 100);
    term.set_console_host_posture(crate::grid::ResizePolicy::Native);
    term.process(b"\x1b[?2048h");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[48;30;100;0;0t"
    );
}
