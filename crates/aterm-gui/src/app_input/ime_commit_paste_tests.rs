// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Input-method commits holding control characters reach the PTY as pastes,
//! except single C0 or DEL keys.

use super::paste_order;
use crate::WindowId;

fn set_bracketed(app: &crate::App, wid: WindowId, on: bool) {
    let front = app.front_terminal(wid).expect("a terminal");
    let seq: &[u8] = if on { b"\x1b[?2004h" } else { b"\x1b[?2004l" };
    crate::term_lock(&front.term).process(seq);
}

fn received(sink: &aterm_session::sink::SinkWriter, pipe: [i32; 2]) -> Vec<u8> {
    paste_order::await_drain_for_test(sink);
    assert!(!paste_order::is_ordering(sink), "the writer drained");
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let n = unsafe { libc::read(pipe[0], buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return out;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
}

fn close(pipe: [i32; 2]) {
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

fn mismatches(
    cases: &[(&str, &[u8])],
    bracketed: bool,
    guard: bool,
) -> Vec<(String, String, String)> {
    let mut wrong = Vec::new();
    for &(commit, want) in cases {
        let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
        let wid = WindowId(0);
        app.confirm_multiline_paste = guard;
        set_bracketed(&app, wid, bracketed);
        app.on_ime_commit(wid, commit.to_string());
        let got = received(&sink, pipe);
        if got != want {
            wrong.push((
                format!("{commit:?}"),
                format!("got {:?}", String::from_utf8_lossy(&got)),
                format!("want {:?}", String::from_utf8_lossy(want)),
            ));
        }
        drop(app);
        close(pipe);
    }
    wrong
}

#[test]
fn a_control_bearing_commit_is_a_bracketed_sanitized_paste() {
    let cases: [(&str, &[u8]); 5] = [
        ("ls\n", b"\x1b[200~ls\r\x1b[201~"),
        ("\x1b[201~", b"\x1b[200~[201~\x1b[201~"),
        ("a\x1bb", b"\x1b[200~ab\x1b[201~"),
        ("rm -rf x\r\n", b"\x1b[200~rm -rf x\r\x1b[201~"),
        ("a\u{9b}201~", b"\x1b[200~a201~\x1b[201~"),
    ];
    let wrong = mismatches(&cases, true, true);
    assert!(
        wrong.is_empty(),
        "each commit must arrive as a bracketed, sanitized paste: {wrong:#?}"
    );
}

#[test]
fn a_control_bearing_commit_at_an_unbracketed_prompt_is_sanitized() {
    let cases: [(&str, &[u8]); 3] = [("ls\n", b"ls\r"), ("a\x1bb", b"ab"), ("a\x03b", b"ab")];
    let wrong = mismatches(&cases, false, false);
    assert!(
        wrong.is_empty(),
        "each commit must be sanitized as a paste is: {wrong:#?}"
    );
}

/// The multi-line guard asks about a commit holding a line break at an
/// unbracketed prompt, as it asks about any such paste, and nothing reaches the
/// PTY until it is answered. Linux's guard is the in-window banner a test can
/// read; macOS's sheet and Windows' dialog are reached through the same
/// `deliver_paste` call.
#[cfg(not(any(target_os = "macos", windows)))]
#[test]
fn a_committed_line_break_waits_for_the_multi_line_guard() {
    let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    let wid = WindowId(0);
    set_bracketed(&app, wid, false);
    app.on_ime_commit(wid, "ls\n".to_string());
    let parked = app.paste_banner.as_ref().expect("the guard must park it");
    assert_eq!(parked.text(), "ls\n");
    assert_eq!(
        parked.session,
        app.focused_session_id(wid),
        "the question names the terminal the commit was made in"
    );
    assert_eq!(
        received(&sink, pipe),
        b"",
        "nothing reaches the PTY unasked"
    );
    app.answer_paste_banner(true);
    assert_eq!(received(&sink, pipe), b"ls\r");
    drop(app);
    close(pipe);
}

#[test]
fn a_printable_commit_keeps_the_typed_key_path() {
    for commit in ["日本", "é", "🙂", "a b"] {
        let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
        let wid = WindowId(0);
        set_bracketed(&app, wid, true);
        app.on_ime_commit(wid, commit.to_string());
        assert!(
            !paste_order::is_ordering(&sink),
            "a typed commit writes inline, never through the paste writer"
        );
        assert_eq!(received(&sink, pipe), commit.as_bytes());
        drop(app);
        close(pipe);
    }
}

#[test]
fn a_lone_control_key_commit_keeps_the_typed_key_path() {
    use aterm_types::keyboard::KeyboardMode;
    for commit in ["\r", "\t", "\x08", "\x7f", "\x1b", "\x03"] {
        let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
        let wid = WindowId(0);
        set_bracketed(&app, wid, true);
        app.on_ime_commit(wid, commit.to_string());
        assert!(
            !paste_order::is_ordering(&sink),
            "{commit:?}: a typed commit writes inline, never through the paste writer"
        );
        let got = received(&sink, pipe);
        assert_eq!(
            got,
            crate::keymap::encode_committed_text(commit, KeyboardMode::empty()),
            "{commit:?} must go out as the typed key, got {:?}",
            String::from_utf8_lossy(&got)
        );
        drop(app);
        close(pipe);
    }
}

#[test]
fn an_open_overlay_still_claims_a_control_bearing_commit() {
    let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    let wid = WindowId(0);
    set_bracketed(&app, wid, true);
    app.windows.get_mut(&wid).unwrap().input_scratch.cells = vec![Vec::new(); 24];
    app.palette_enter();
    assert!(app.windows[&wid].overlay_open(), "the palette is open");
    app.on_ime_commit(wid, "ls\n".to_string());
    assert_eq!(
        received(&sink, pipe),
        b"",
        "the session behind gets nothing"
    );
    drop(app);
    close(pipe);
}
