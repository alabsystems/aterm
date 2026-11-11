// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::Terminal;
use aterm_types::mouse::WheelDir;

/// SGR (DECSET 1006) so the button code is readable as decimal text rather
/// than a `+32` byte — the encoding a modern TUI actually asks for.
fn sgr_tracking() -> Terminal {
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[?1000h\x1b[?1006h");
    t
}

fn wheel(t: &Terminal, dir: WheelDir) -> String {
    String::from_utf8(t.encode_mouse_wheel(dir, 10, 5, 0).expect("wheel encodes"))
        .expect("SGR reports are ASCII")
}

/// All four directions: 64/65 up/down and 66/67 left/right (xterm's
/// buttons 4-7).
#[test]
fn wheel_encodes_all_four_xterm_buttons() {
    let t = sgr_tracking();
    assert_eq!(wheel(&t, WheelDir::Up), "\x1b[<64;11;6M");
    assert_eq!(wheel(&t, WheelDir::Down), "\x1b[<65;11;6M");
    assert_eq!(wheel(&t, WheelDir::Left), "\x1b[<66;11;6M");
    assert_eq!(wheel(&t, WheelDir::Right), "\x1b[<67;11;6M");
}

/// Modifiers OR into the horizontal codes as into the vertical ones (shift=4, alt=8,
/// ctrl=16).
#[test]
fn wheel_modifiers_or_into_the_horizontal_codes() {
    let t = sgr_tracking();
    let bytes = t
        .encode_mouse_wheel(WheelDir::Right, 10, 5, aterm_types::mouse::CTRL_MASK)
        .expect("wheel encodes");
    assert_eq!(String::from_utf8(bytes).expect("ascii"), "\x1b[<83;11;6M");
}

/// Tracking off (never on, or turned on and off again) and X10 (DECSET 9,
/// press-only): every direction reports nothing; the wheel belongs to
/// aterm's own viewport.
#[test]
fn wheel_reports_nothing_without_wheel_tracking() {
    for setup in [&b""[..], b"\x1b[?1000h\x1b[?1000l", b"\x1b[?9h"] {
        let mut t = Terminal::new(24, 80);
        t.process(setup);
        for dir in [
            WheelDir::Up,
            WheelDir::Down,
            WheelDir::Left,
            WheelDir::Right,
        ] {
            assert_eq!(
                t.encode_mouse_wheel(dir, 10, 5, 0),
                None,
                "{setup:?} {dir:?}"
            );
        }
    }
}

/// The report memo repeats only the position last recorded, under the mode
/// and encoding it was recorded in: a new position, a new tracking mode or a
/// new encoding (cells to DEC 1016 pixels) is a fresh report.
#[test]
fn the_report_memo_repeats_only_the_same_position_in_the_same_terms() {
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[?1003h\x1b[?1006h");
    assert!(
        !t.mouse_motion_repeats_last_report(7, 4),
        "nothing reported yet"
    );
    t.note_mouse_report(7, 4);
    assert!(t.mouse_motion_repeats_last_report(7, 4));
    assert!(!t.mouse_motion_repeats_last_report(8, 4));
    assert!(!t.mouse_motion_repeats_last_report(7, 5));
    t.process(b"\x1b[?1002h");
    assert!(
        !t.mouse_motion_repeats_last_report(7, 4),
        "a mode change starts a new conversation"
    );
    t.note_mouse_report(7, 4);
    t.process(b"\x1b[?1016h");
    assert!(
        !t.mouse_motion_repeats_last_report(7, 4),
        "cell and pixel coordinates are never the same position"
    );
}
