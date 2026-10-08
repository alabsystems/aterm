// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use aterm_session::sink::SinkWriter;

use super::{App, WindowId};
use crate::input::Source;

/// A headless App whose session sink is the WRITE end of a nonblocking pipe,
/// plus the READ end: every byte the paste seam sends to the "shell" is
/// observable, and an empty pipe is proof nothing was delivered.
fn app_with_pty_observer() -> (App, i32) {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "observer pipe");
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let app = App::headless_for_test_with_sink(Arc::new(SinkWriter::new(pipe[1])));
    (app, pipe[0])
}

/// Everything queued on the observer pipe (empty = the paste never reached the PTY).
/// Delivery happens on the per-session writer thread, so this polls until bytes arrive
/// or the deadline passes; negative assertions use the same deadline.
fn drain_within(fd: i32, ms: u64) -> Vec<u8> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            out.extend_from_slice(&buf[..n as usize]);
            continue; // pull the rest of the frame before returning
        }
        if !out.is_empty() || std::time::Instant::now() >= deadline {
            return out;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The positive form: wait for the writer thread's delivery, a minute — a
/// hang detector, never a latency budget (it returns at the first bytes).
fn drain(fd: i32) -> Vec<u8> {
    drain_within(fd, 60_000)
}

/// The negative form: a parked/cancelled paste submits NO writer job, so a
/// short grace window is enough to catch a leak without slowing the suite.
fn assert_no_bytes(fd: i32, why: &str) {
    assert_eq!(drain_within(fd, 150), b"", "{why}");
}

/// A multi-line unbracketed human paste parks on the banner with zero bytes delivered;
/// Escape drops it; a re-presented paste answered with Enter delivers through the real
/// seam (LF→CR, unbracketed).
#[test]
fn a_risky_paste_parks_fail_closed_until_enter_answers_it() {
    let (mut app, rx) = app_with_pty_observer();
    let wid = WindowId(0);
    let owner = app.focused_session_id(wid);
    app.deliver_paste(wid, owner, "ls\nrm -rf ~".to_string(), Source::Human);
    let parked = app.paste_banner.as_ref().expect("the paste must park");
    assert_eq!(parked.wid, wid);
    assert_eq!(
        parked.session, owner,
        "the banner remembers the gesture's owner"
    );
    assert_eq!(parked.text(), "ls\nrm -rf ~");
    assert_no_bytes(rx, "parked text must not touch the PTY");

    // Escape: the only copy of the text dies with the entry.
    app.answer_paste_banner(false);
    assert!(app.paste_banner.is_none(), "cancel clears the banner");
    assert_no_bytes(rx, "a cancelled paste delivers NOTHING");

    // Enter on a fresh confirmation: the parked text — exactly it — delivers.
    app.deliver_paste(wid, owner, "ls\nrm -rf ~".to_string(), Source::Human);
    app.answer_paste_banner(true);
    assert!(app.paste_banner.is_none(), "accept consumes the banner");
    assert_eq!(
        drain(rx),
        b"ls\rrm -rf ~",
        "accept delivers through the real paste seam (LF->CR, unbracketed)"
    );
}

/// One question at a time: a second unconfirmed paste while one is parked is dropped
/// and never delivers, even after the first is answered.
#[test]
fn a_second_unconfirmed_paste_never_replaces_the_open_question() {
    let (mut app, rx) = app_with_pty_observer();
    let wid = WindowId(0);
    let owner = app.focused_session_id(wid);
    app.deliver_paste(wid, owner, "first\nquestion".to_string(), Source::Human);
    app.deliver_paste(wid, owner, "second\nsneaks-in".to_string(), Source::Human);
    assert_eq!(
        app.paste_banner.as_ref().expect("still parked").text(),
        "first\nquestion",
        "the open question keeps ITS text"
    );
    app.answer_paste_banner(true);
    assert_eq!(
        drain(rx),
        b"first\rquestion",
        "only the answered paste delivers; the dropped one is gone"
    );
    assert!(app.paste_banner.is_none());
}

/// Flip DEC 2004 on `wid`'s live engine exactly as a program on the other
/// end of the PTY would, so a test can stand inside the window between the
/// gesture and the drain.
fn set_bracketed(app: &App, wid: WindowId, on: bool) {
    let mirror = app.front_terminal(wid).expect("a terminal to flip");
    let seq: &[u8] = if on { b"\x1b[?2004h" } else { b"\x1b[?2004l" };
    super::term_lock(&mirror.term).process(seq);
}

/// Framing, forward: the banner asked an unbracketed question and the program turns DEC
/// 2004 on while the answer is owed; the confirmed paste is framed as the person was
/// told (one observation of the mode).
#[test]
fn a_mode_flip_while_the_question_stands_cannot_reframe_the_answer() {
    let (mut app, rx) = app_with_pty_observer();
    let wid = WindowId(0);
    let owner = app.focused_session_id(wid);
    set_bracketed(&app, wid, false);
    app.deliver_paste(wid, owner, "ls\nrm -rf ~".to_string(), Source::Human);
    assert!(
        app.paste_banner.is_some(),
        "the unbracketed question stands"
    );
    // The program races the answer: DEC 2004 on, between gesture and drain.
    set_bracketed(&app, wid, true);
    app.answer_paste_banner(true);
    assert_eq!(
        drain(rx),
        b"ls\rrm -rf ~",
        "the confirmed paste keeps the framing its question was asked under"
    );
}

/// Bytes larger than any pipe can hold: one such frame parks the FIFO
/// writer thread inside its blocking write, which is what makes the window
/// between a gesture and its drain a place a test can stand rather than a
/// race it has to win.
const WEDGE_BYTES: usize = 256 * 1024;

/// Read until `want` bytes have arrived (or `ms` elapses), then take
/// whatever else follows within a short idle grace — so an answer SHORTER
/// than expected still returns promptly and a longer one is never truncated.
fn drain_at_least(fd: i32, want: usize, ms: u64) -> Vec<u8> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            out.extend_from_slice(&buf[..n as usize]);
            continue;
        }
        if out.len() >= want || std::time::Instant::now() >= deadline {
            out.extend_from_slice(&drain_within(fd, 150));
            return out;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Framing, reverse: DEC 2004 was on at the gesture, so no question was asked; the
/// program turns it off before the writer drains. Framing by the drain-time mode would
/// submit every line at a bare prompt. A wedge-sized paste ahead parks the writer so
/// the flip provably precedes formatting.
#[test]
fn a_mode_flip_before_the_drain_cannot_unframe_a_paste_nobody_was_asked_about() {
    let (mut app, rx) = app_with_pty_observer();
    let wid = WindowId(0);
    let owner = app.focused_session_id(wid);
    set_bracketed(&app, wid, true);
    // No newline in the wedge: it is plumbing, and the guard must stay silent
    // about it for the same reason it stays silent about the real paste.
    app.deliver_paste(wid, owner, "w".repeat(WEDGE_BYTES), Source::Human);
    app.deliver_paste(wid, owner, "ls\nrm -rf ~".to_string(), Source::Human);
    assert!(
        app.paste_banner.is_none(),
        "bracketed paste is never flagged — nobody was asked"
    );
    // The program races the drain: DEC 2004 off while the paste is in flight.
    set_bracketed(&app, wid, false);
    let out = drain_at_least(rx, WEDGE_BYTES + 12 + 11, 60_000);
    assert!(
        out.ends_with(b"\x1b[200~ls\rrm -rf ~\x1b[201~"),
        "an unasked paste keeps the framing that made it unaskable; tail was {:?}",
        String::from_utf8_lossy(&out[out.len().saturating_sub(32)..]).into_owned()
    );
}

/// `confirm_multiline_paste = false` (the explicit opt-out) delivers a risky
/// paste straight through, exactly as before the banner existed.
#[test]
fn the_opt_out_keeps_the_direct_delivery_path() {
    let (mut app, rx) = app_with_pty_observer();
    let wid = WindowId(0);
    let owner = app.focused_session_id(wid);
    app.confirm_multiline_paste = false;
    app.deliver_paste(wid, owner, "a\nb".to_string(), Source::Human);
    assert!(app.paste_banner.is_none(), "opt-out never parks");
    assert_eq!(drain(rx), b"a\rb");
}

/// `paste_primary_into` (middle click) runs the same pastejacking guard: a multi-line
/// PRIMARY parks on the banner, a single line delivers at once. PRIMARY is stubbed
/// (`PRIMARY_STUB`) so the own-slot branch runs deterministically.
#[cfg(target_os = "linux")]
#[test]
fn middle_click_primary_paste_runs_the_same_guard() {
    let (mut app, rx) = app_with_pty_observer();
    let wid = WindowId(0);
    crate::control::PRIMARY_STUB.with(|s| *s.borrow_mut() = Some("ls\npwned".to_string()));
    app.paste_primary_into(wid);
    let parked = app.paste_banner.as_ref().expect("guard must park it");
    assert_eq!(parked.text(), "ls\npwned");
    assert_no_bytes(rx, "the raw-input bypass stays closed");
    app.answer_paste_banner(false);

    // The common benign case: a single-line selection pastes immediately.
    crate::control::PRIMARY_STUB.with(|s| *s.borrow_mut() = Some("hello".to_string()));
    app.paste_primary_into(wid);
    assert!(app.paste_banner.is_none(), "single-line never asks");
    assert_eq!(drain(rx), b"hello");
    crate::control::PRIMARY_STUB.with(|s| *s.borrow_mut() = None);
}
