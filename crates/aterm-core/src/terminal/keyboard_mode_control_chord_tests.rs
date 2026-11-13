// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `control_chords_escaped` is the encoder's own answer.
use crate::terminal::Terminal;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, encode_key_with_layout};

fn chord(term: &Terminal, c: char) -> Vec<u8> {
    encode_key_with_layout(
        &Key::Character(c),
        Modifiers::CTRL,
        term.keyboard_mode(),
        KeyEventType::Press,
        None,
    )
}

#[test]
fn control_chords_escaped_is_what_the_encoder_sends() {
    let negotiations: [(&str, &[u8], bool); 7] = [
        ("legacy", b"", false),
        ("kitty disambiguate", b"\x1b[>1u", true),
        ("kitty report all keys", b"\x1b[>8u", true),
        ("kitty event types alone", b"\x1b[>2u", false),
        ("modifyOtherKeys 1", b"\x1b[>4;1m", false),
        ("modifyOtherKeys 2", b"\x1b[>4;2m", true),
        ("modifyOtherKeys 2 reset", b"\x1b[>4;2m\x1b[>4;0m", false),
    ];
    for (label, bytes, escaped) in negotiations {
        let mut term = Terminal::new(4, 20);
        term.process(bytes);
        assert_eq!(term.control_chords_escaped(), escaped, "{label}");
        assert_eq!(
            chord(&term, 'a') != [0x01] && chord(&term, 'e') != [0x05],
            escaped,
            "{label}: the encoder agrees ({:?})",
            String::from_utf8_lossy(&chord(&term, 'a'))
        );
    }
}
