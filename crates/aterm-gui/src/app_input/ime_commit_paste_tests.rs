// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An input method's committed text that holds a control character reaches the
//! PTY as a paste, unless the whole commit is one C0 or DEL key
//! (`docs/AUDIT-boundaries-2026-10-09.md`). Every host lowers its input-method
//! commit to `Ime::Commit` and `App::on_ime_commit`: X11's XIM lookup, Wayland's
//! text-input `commit_string`, Windows' IMM result string, and macOS's
//! `insertText:` (dictation included) under marked text. These tests drive that
//! dispatch with no live input method and read what a private pipe-backed PTY
//! received.

use super::paste_order;
use crate::WindowId;

/// Flip DEC 2004 on `wid`'s live engine as a program on the other end of the PTY
/// would.
fn set_bracketed(app: &crate::App, wid: WindowId, on: bool) {
    let front = app.front_terminal(wid).expect("a terminal");
    let seq: &[u8] = if on { b"\x1b[?2004h" } else { b"\x1b[?2004l" };
    crate::term_lock(&front.term).process(seq);
}

/// Everything the PTY pipe holds once the session's ordered writer has drained:
/// a paste writes on the writer thread and retires its slot only after its PTY
/// write completed, and a typed commit writes inline, so both are in the pipe.
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

/// Commit each case into a fresh window (DEC 2004 set to `bracketed`, the
/// multi-line guard to `guard`) and return every commit whose PTY bytes differ
/// from the expected ones, with what arrived, so one run names every miss.
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

/// Under bracketed paste a commit holding a line break, ESC or a C1 control goes
/// out framed and sanitized, exactly as `Terminal::format_paste` frames a paste:
/// the line break cannot submit, and a planted `ESC [ 201 ~` cannot close the
/// bracket early.
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

/// At an unbracketed prompt the same commit is sanitized as a paste is (line
/// breaks to CR, ESC and the C0 signals dropped). The multi-line guard is opted
/// out here so the bytes are observable on every host; that the guard asks first
/// is the next test, on the host whose guard a test can see.
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

/// The common commit (CJK, an accent, an emoji) keeps the typed-key path:
/// written inline with no bracket even under bracketed paste.
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

/// A commit that is one C0 or DEL character is a key: X11 commits an unfiltered
/// Return, Tab, BackSpace or Escape that way while an XIM preedit is open. Under
/// bracketed paste it keeps the typed-key path, so Return still runs the line
/// and BackSpace still erases, where a paste would frame the CR as an inserted
/// line break and the sanitizer would drop BS and DEL.
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

/// A commit made while an overlay owns the window is the overlay's text, never
/// the PTY's: the paste route must not pin it past the overlay to the session
/// behind it.
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
