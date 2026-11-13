// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Behind a Windows console host a native console program's `CSI > 4 ; 2 m`
//! reaches the terminal (OpenConsole and the inbox conhost both forward it),
//! and conhost, which reads the key bytes, has no reader for the
//! `CSI 27 ; m ; c ~` chords it would ask for: PSReadLine gets nothing for
//! Shift+A, Ctrl+A/E/K or Alt+B until the level is reset.
use crate::grid::ResizePolicy;
use crate::terminal::Terminal;
use crate::terminal::program_evidence;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey, encode_key_with_layout};

fn chords(term: &Terminal) -> Vec<Vec<u8>> {
    let c = |k: char| Key::Character(k);
    let n = Key::Named;
    let cases = [
        (c('a'), Modifiers::CTRL),
        (c('e'), Modifiers::CTRL),
        (c('k'), Modifiers::CTRL),
        (c('A'), Modifiers::SHIFT),
        (c('@'), Modifiers::SHIFT),
        (c('a'), Modifiers::ALT),
        (c('b'), Modifiers::ALT),
        (c('a'), Modifiers::CTRL | Modifiers::ALT),
        (c('K'), Modifiers::CTRL | Modifiers::SHIFT),
        (c('i'), Modifiers::CTRL),
        (n(NamedKey::Tab), Modifiers::SHIFT),
        (n(NamedKey::Tab), Modifiers::CTRL),
        (n(NamedKey::Enter), Modifiers::ALT),
    ];
    cases
        .iter()
        .map(|(key, mods)| {
            encode_key_with_layout(
                key,
                *mods,
                term.mode_mirror().keyboard_mode(),
                KeyEventType::Press,
                None,
            )
        })
        .collect()
}

fn session(policy: ResizePolicy) -> Terminal {
    let mut term = Terminal::new(24, 80);
    term.set_console_host_posture(policy);
    term.process(b"\x1b[?9001h");
    term
}

fn mok_query(term: &mut Terminal) -> Vec<u8> {
    term.process(b"\x1b[?4m");
    term.take_response().unwrap_or_default()
}

#[test]
fn a_console_host_session_ignores_modify_other_keys_and_reads_level_0() {
    let untouched = chords(&session(ResizePolicy::ConPty));
    assert_eq!(untouched[0], [0x01], "control: legacy Ctrl+A is ^A");
    assert_eq!(untouched[3], b"A", "control: legacy Shift+A is A");

    let mut native = session(ResizePolicy::Native);
    native.process(b"\x1b[>4;2m");
    assert_eq!(mok_query(&mut native), b"\x1b[>4;2m");
    assert_eq!(chords(&native)[0], b"\x1b[27;5;97~");

    let mut term = session(ResizePolicy::ConPty);
    let requests: [&[u8]; 4] = [b"\x1b[>4;2m", b"\x1b[>4;1m", b"\x1b[>4;1f", b"\x1b[>4n"];
    for level in requests {
        term.process(level);
        assert_eq!(chords(&term), untouched, "{level:?}");
        assert_eq!(mok_query(&mut term), b"\x1b[>4;0m", "{level:?}");
    }
    term.process(b"\x1b[?4g");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[>4;0f");
    assert_eq!(
        term.keyboard_mode().xterm_modify_other_keys_level(),
        0,
        "`ctl modes` reads the level the encoder uses"
    );
    assert_eq!(
        term.take_evidence_asserted()
            & (program_evidence::MODIFY_OTHER_KEYS | program_evidence::FORMAT_OTHER_KEYS),
        0
    );
    assert!(!term.program_owns_terminal());
}

#[test]
fn a_level_negotiated_before_the_console_host_posture_stops_reaching_the_encoder() {
    let mut term = session(ResizePolicy::Native);
    term.process(b"\x1b[>4;2m");
    let negotiated = chords(&term);
    term.set_console_host_posture(ResizePolicy::ConPty);
    term.process(b"\x1b[?9001h");
    assert_eq!(chords(&term), chords(&session(ResizePolicy::ConPty)));
    assert_eq!(mok_query(&mut term), b"\x1b[>4;0m");
    term.set_console_host_posture(ResizePolicy::Native);
    assert_eq!(chords(&term), negotiated);
}
