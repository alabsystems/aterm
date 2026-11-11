// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use super::{MemTty, Mode, Modes, Readiness, Size, Tty, reset_bytes};
use crate::signals::Signals;

const SIZE: Size = Size { cols: 80, rows: 24 };

#[test]
fn mem_tty_reads_its_script_on_a_virtual_clock() {
    let (mut tty, peer) = MemTty::pair(SIZE);
    let start = tty.now();
    peer.send(b"ab");
    peer.send_after(Duration::from_millis(300), b"cdef");
    peer.resize_after(
        Duration::from_millis(5),
        Size {
            cols: 100,
            rows: 30,
        },
    );
    let mut buf = [0u8; 3];
    assert_eq!(tty.read(&mut [], None).unwrap(), Readiness::Bytes(0));
    assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Bytes(2));
    assert_eq!(&buf[..2], b"ab");
    let deadline = start + Duration::from_millis(100);
    assert_eq!(
        tty.read(&mut buf, Some(deadline)).unwrap(),
        Readiness::Timeout
    );
    assert_eq!(tty.now(), deadline);
    assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Bytes(3));
    assert_eq!(&buf, b"cde");
    assert_eq!(tty.now(), start + Duration::from_millis(300));
    assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Bytes(1));
    assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Signal);
    assert_eq!(tty.take_signals().unwrap(), Signals::WINCH);
    assert_eq!(
        tty.size().unwrap(),
        Size {
            cols: 100,
            rows: 30
        }
    );
    assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Closed);
    assert!(peer.script_done());
    peer.send_after(Duration::MAX, b"z");
    assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Bytes(1));
}

#[test]
fn modes_are_set_once_and_put_back_in_order() {
    let (mut tty, peer) = MemTty::pair(SIZE);
    for mode in [
        Mode::AltScreen,
        Mode::KittyKeys(5),
        Mode::Mouse,
        Mode::Paste,
        Mode::Focus,
        Mode::InBandResize,
        Mode::Cursor,
    ] {
        tty.enable(mode).unwrap();
        tty.enable(mode).unwrap();
    }
    assert_eq!(
        peer.take_output(),
        b"\x1b[?1049h\x1b[>5u\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?2048h"
    );
    let all = peer.modes();
    assert_eq!(all, Modes::from_bits(0x7f));
    assert_eq!(
        reset_bytes(all),
        b"\x1b[?2026l\x1b[m\x1b[0 q\x1b[?25h\x1b[?2048l\x1b[?1004l\x1b[?2004l\
          \x1b[?1006l\x1b[?1002l\x1b[<u\x1b[?1049l"
    );
    tty.suspend().unwrap();
    let mut expected = reset_bytes(all);
    expected.extend_from_slice(
        b"\x1b[?1049h\x1b[>5u\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?2048h",
    );
    assert_eq!(peer.take_output(), expected);
    assert_eq!(peer.suspensions(), 1);
    assert_eq!(tty.take_signals().unwrap(), Signals::CONT);
    assert_eq!(
        peer.take_output(),
        b"\x1b[?1049h\x1b[=5;1u\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?2048h"
    );
    tty.disable(Mode::Paste).unwrap();
    tty.disable(Mode::Paste).unwrap();
    assert_eq!(peer.take_output(), b"\x1b[?2004l");
}

/// The kitty flags of the screen aterm's engine shows after `bytes`, and whether it
/// shows the alternate screen.
fn shown(bytes: &[u8]) -> (String, bool) {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    term.process(bytes);
    let _ = term.take_response();
    term.process(b"\x1b[?u\x1b[?1049$p");
    let reply = term.take_response().expect("the engine answers");
    let reply = String::from_utf8_lossy(&reply).into_owned();
    let flags = reply
        .split("\x1b[?")
        .find_map(|part| part.strip_suffix('u'))
        .expect("the kitty flags")
        .to_owned();
    (flags, reply.contains("1049;1$y"))
}

/// Each screen has its own kitty stack: whichever order the kitty entry and the
/// alternate screen are set and put back in, the entry is on the screen shown, and
/// a restore leaves the main screen shown with no flags.
#[test]
fn the_kitty_entry_follows_the_screen_shown() {
    for kitty_first in [true, false] {
        let (mut tty, peer) = MemTty::pair(SIZE);
        let order = if kitty_first {
            [Mode::KittyKeys(5), Mode::AltScreen]
        } else {
            [Mode::AltScreen, Mode::KittyKeys(5)]
        };
        let mut seen = Vec::new();
        tty.enable(order[0]).unwrap();
        tty.enable(order[1]).unwrap();
        seen.extend(peer.take_output());
        assert_eq!(shown(&seen), ("5".to_owned(), true), "{order:?}");

        tty.disable(Mode::AltScreen).unwrap();
        seen.extend(peer.take_output());
        assert_eq!(shown(&seen), ("5".to_owned(), false), "{order:?}");
        tty.enable(Mode::AltScreen).unwrap();
        seen.extend(peer.take_output());
        assert_eq!(shown(&seen), ("5".to_owned(), true), "{order:?}");

        seen.extend(reset_bytes(peer.modes()));
        assert_eq!(shown(&seen), ("0".to_owned(), false), "{order:?}");
    }
    let (mut tty, peer) = MemTty::pair(SIZE);
    tty.enable(Mode::KittyKeys(5)).unwrap();
    let mut seen = peer.take_output();
    assert_eq!(shown(&seen), ("5".to_owned(), false));
    seen.extend(reset_bytes(peer.modes()));
    assert_eq!(shown(&seen), ("0".to_owned(), false));
}

/// A suspension leaves CONT pending, which the next read reports before any input,
/// as the real terminal's wake does; so does any signal no read has taken.
#[test]
fn a_pending_signal_is_read_before_input() {
    let (mut tty, peer) = MemTty::pair(SIZE);
    peer.send(b"x");
    tty.suspend().unwrap();
    let mut buf = [0u8; 4];
    assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Signal);
    assert_eq!(
        tty.read(&mut buf, None).unwrap(),
        Readiness::Signal,
        "until taken"
    );
    assert_eq!(tty.take_signals().unwrap(), Signals::CONT);
    assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Bytes(1));
}
