// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{apply_pty_keyboard_posture, new_live_terminal};
use crate::term_lock;
use aterm_core::terminal::Terminal;
use aterm_types::keyboard::{
    Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key_with_layout,
};
use std::sync::Mutex;

const KITTY_BITS: KeyboardMode = KeyboardMode::DISAMBIGUATE_ESC_CODES
    .union(KeyboardMode::REPORT_EVENT_TYPES)
    .union(KeyboardMode::REPORT_ALTERNATE_KEYS)
    .union(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
    .union(KeyboardMode::REPORT_ASSOCIATED_TEXT);

fn live_engine() -> Mutex<Terminal> {
    Mutex::new(new_live_terminal(24, 80, None, None))
}

fn probe_then_push(term: &Mutex<Terminal>) -> (Option<Vec<u8>>, KeyboardMode, [Vec<u8>; 2]) {
    let mut t = term_lock(term);
    t.process(b"\x1b[?u");
    let reply = t.take_response();
    t.process(b"\x1b[>1u");
    let mode = t.keyboard_mode();
    let press = |ch, mods| {
        encode_key_with_layout(&Key::Character(ch), mods, mode, KeyEventType::Press, None)
    };
    (
        reply,
        mode.intersection(KITTY_BITS),
        [press('l', Modifiers::CTRL), press('a', Modifiers::ALT)],
    )
}

#[test]
fn a_native_session_keeps_the_kitty_keyboard_protocol() {
    for master in [-1, 0x7fff_ffff] {
        let term = live_engine();
        apply_pty_keyboard_posture(&term, master);
        let (reply, kitty, keys) = probe_then_push(&term);
        assert_eq!(reply.as_deref(), Some(&b"\x1b[?0u"[..]), "master {master}");
        assert_eq!(
            kitty,
            KeyboardMode::DISAMBIGUATE_ESC_CODES,
            "master {master}"
        );
        assert_eq!(keys, [b"\x1b[108;5u".to_vec(), b"\x1b[97;3u".to_vec()]);
    }
}

#[test]
fn a_native_session_ignores_a_programs_win32_input_request() {
    for master in [-1, 0x7fff_ffff] {
        let term = live_engine();
        apply_pty_keyboard_posture(&term, master);
        let mut t = term_lock(&term);
        t.process(b"\x1b[?9001h");
        let mode = t.mode_mirror().keyboard_mode();
        assert!(!mode.contains(KeyboardMode::WIN32_INPUT), "master {master}");
        let press =
            |key: Key, mods| encode_key_with_layout(&key, mods, mode, KeyEventType::Press, None);
        assert_eq!(
            press(Key::Named(NamedKey::Escape), Modifiers::empty()),
            b"\x1b",
            "master {master}"
        );
        assert_eq!(
            press(Key::Named(NamedKey::Enter), Modifiers::SHIFT),
            b"\n",
            "master {master}"
        );
    }
}

#[cfg(windows)]
#[test]
fn a_conpty_session_does_not_advertise_the_kitty_keyboard_protocol() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateEventW(
            attrs: *mut std::ffi::c_void,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> isize;
    }
    // SAFETY: NULL attrs and name; manual-reset, initially unsignaled. `adopt_handoff` owns
    // the handle from its `Ok` on.
    let client = unsafe { CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
    assert_ne!(client, 0, "CreateEventW");
    let master = aterm_pty::adopt_handoff(0, 0, 0, client)
        .expect("register a ConPTY session")
        .master;
    let term = live_engine();
    apply_pty_keyboard_posture(&term, master);
    aterm_pty::close_master(master);

    let (reply, kitty, keys) = probe_then_push(&term);
    assert_eq!(reply, None, "a ConPTY session must not answer CSI ? u");
    assert!(
        kitty.is_empty(),
        "a push must not reach the encoder: {kitty:?}"
    );
    assert_eq!(keys, [vec![0x0c], b"\x1ba".to_vec()]);
    term_lock(&term).process(b"\x1b[?9001h");
    assert!(
        term_lock(&term)
            .keyboard_mode()
            .contains(KeyboardMode::WIN32_INPUT)
    );
}

#[test]
fn an_openconsole_session_does_not_advertise_the_kitty_keyboard_protocol() {
    let term = live_engine();
    crate::alt_screen_resync::apply_posture(
        &mut term_lock(&term),
        aterm_core::grid::ResizePolicy::ConPty,
        Some(aterm_pty::ConsoleHostKind::OpenConsole),
    );
    let (reply, kitty, keys) = probe_then_push(&term);
    assert_eq!(
        reply, None,
        "an OpenConsole session must not answer CSI ? u"
    );
    assert!(
        kitty.is_empty(),
        "a push must not reach the encoder: {kitty:?}"
    );
    assert_eq!(keys, [vec![0x0c], b"\x1ba".to_vec()]);
}
