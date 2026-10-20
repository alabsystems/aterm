// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The capability probe (docs/DESIGN-amacs-2026-10-19.md §5.2).
//!
//! One write asks the terminal who it is (XTVERSION), for its kitty keyboard flags, and
//! whether it knows DEC 2026 (synchronized output), 2048 (in-band resize), 1004 (focus)
//! and 2004 (bracketed paste), then sends DA1, which every terminal since the VT100
//! answers, as the sentinel: the replies come back in order, so DA1's marks the end. The
//! replies are read through [`crate::decode`] until DA1 arrives or [`PROBE_TIMEOUT`]
//! passes; no DA1 means a VT100 subset that answered nothing. The decoder is never
//! flushed meanwhile, so a reply split across slow reads is never read as typing; keys
//! typed during the probe come back with the capabilities, and anything the decoder
//! still holds stays in it for the caller's loop. Signals that arrive meanwhile are
//! taken (a read reports them until they are) and come back with it too.
//!
//! A mode counts as supported when the terminal reports it set, reset or permanently
//! set; "not recognized" and "permanently reset" mean it cannot be used.

use std::io;
use std::time::Duration;

use crate::decode::{Decoder, Event, ModeState, Reply};
use crate::signals::Signals;
use crate::tty::{Readiness, Tty};

/// How long the probe waits for the terminal's DA1.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// The probe's one write: XTVERSION, kitty's flags, DECRQM for 2026, 2048, 1004 and
/// 2004, and DA1 last.
pub const QUERY: &[u8] = b"\x1b[>0q\x1b[?u\x1b[?2026$p\x1b[?2048$p\x1b[?1004$p\x1b[?2004$p\x1b[c";

/// What the terminal said it can do. The default is a terminal that answered nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caps {
    /// It reported kitty keyboard flags, so it takes a kitty push.
    pub kitty: bool,
    /// DEC 2026: frames can be synchronized updates.
    pub sync_2026: bool,
    /// DEC 2048: resizes can come in band, as reports.
    pub resize_2048: bool,
    /// DEC 1004: focus changes can be reported.
    pub focus: bool,
    /// DEC 2004: pastes can be bracketed.
    pub paste: bool,
    /// Its XTVERSION name: aterm says `aterm(<version>)`.
    pub name: Option<String>,
}

impl Caps {
    /// Read one reply in; `true` for DA1, the last.
    fn read(&mut self, reply: Reply) -> bool {
        match reply {
            Reply::Version(name) => self.name = Some(name),
            Reply::KittyFlags(_) => self.kitty = true,
            Reply::Mode { mode, state } => {
                let usable = matches!(
                    state,
                    ModeState::Set | ModeState::Reset | ModeState::PermanentlySet
                );
                match mode {
                    2026 => self.sync_2026 = usable,
                    2048 => self.resize_2048 = usable,
                    1004 => self.focus = usable,
                    2004 => self.paste = usable,
                    _ => {}
                }
            }
            Reply::PrimaryAttributes(_) => return true,
        }
        false
    }
}

/// What [`probe`] learned, and what arrived for the caller's loop while it waited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probed {
    pub caps: Caps,
    /// The events read that were not the probe's replies (typeahead, a resize report),
    /// in order.
    pub events: Vec<Event>,
    /// The signals taken meanwhile, to be handled as if the loop had taken them: after
    /// [`Signals::CONT`] the terminal is raw again and wants a repaint; after
    /// [`Signals::WINCH`], a new size.
    pub signals: Signals,
}

/// Ask the terminal what it can do, reading its replies through `decoder` until DA1 or
/// [`PROBE_TIMEOUT`] (on the terminal's clock).
///
/// # Errors
/// The write's or a read's OS error, or setting the terminal raw again after a SIGCONT.
pub fn probe<T: Tty + ?Sized>(tty: &mut T, decoder: &mut Decoder) -> io::Result<Probed> {
    tty.write_all(QUERY)?;
    let deadline = tty.now() + PROBE_TIMEOUT;
    let mut probed = Probed::default();
    let mut done = false;
    let mut buf = [0u8; 4096];
    while !done {
        match tty.read(&mut buf, Some(deadline))? {
            Readiness::Bytes(n) => {
                for event in decoder.feed(&buf[..n], tty.now()) {
                    match event {
                        Event::Reply(reply) if !done => done = probed.caps.read(reply),
                        other => probed.events.push(other),
                    }
                }
                // A terminal that keeps sending (a held key) still ends the probe on time.
                done |= tty.now() >= deadline;
            }
            // Taken, or every read until they are would report them.
            Readiness::Signal => probed.signals = probed.signals.union(tty.take_signals()?),
            Readiness::Timeout | Readiness::Closed => done = true,
        }
    }
    Ok(probed)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aterm_core::terminal::Terminal;

    use super::{Caps, PROBE_TIMEOUT, Probed, QUERY, probe};
    use crate::decode::{Decoder, Event};
    use crate::signals::Signals;
    use crate::tty::{MemTty, Size, Tty};

    const SIZE: Size = Size { cols: 80, rows: 24 };

    fn typed(c: char) -> Event {
        Event::Key {
            key: aterm_types::keyboard::Key::Character(c),
            mods: aterm_types::keyboard::Modifiers::empty(),
            text: Some(c),
        }
    }

    /// aterm's own engine answering the probe.
    fn aterm_replies(setup: &[u8]) -> Vec<u8> {
        let mut term = Terminal::new(24, 80);
        term.process(setup);
        let _ = term.take_response();
        term.process(QUERY);
        term.take_response().expect("aterm answers the probe")
    }

    #[test]
    fn parses_replies_and_stops_at_da1() {
        // aterm, with focus reporting on: every query answered, in one read.
        let (mut tty, peer) = MemTty::pair(SIZE);
        let start = tty.now();
        peer.send_after(Duration::from_millis(3), &aterm_replies(b"\x1b[?1004h"));
        let mut decoder = Decoder::new();
        let probed = probe(&mut tty, &mut decoder).expect("probed");
        assert_eq!(peer.take_output(), QUERY, "one write");
        let name = probed.caps.name.clone().expect("XTVERSION");
        assert!(name.starts_with("aterm("), "{name}");
        assert_eq!(
            probed,
            Probed {
                caps: Caps {
                    kitty: true,
                    sync_2026: true,
                    resize_2048: true,
                    focus: true,
                    paste: true,
                    name: Some(name),
                },
                ..Probed::default()
            }
        );
        assert_eq!(
            tty.now(),
            start + Duration::from_millis(3),
            "no wait past DA1"
        );

        // Another terminal's replies, split mid-sequence across reads 300 ms apart (the
        // decoder is never flushed, so the halves still meet), keys typed before and
        // after them, and a resize signal in between, which the probe takes and hands on.
        let (mut tty, peer) = MemTty::pair(SIZE);
        let start = tty.now();
        peer.send(b"a\x1bP>|XTe");
        peer.send_after(
            Duration::from_millis(300),
            b"rm(390)\x1b\\\x1b[?2026;0$y\x1b[?20",
        );
        peer.signal_after(Duration::from_millis(10), Signals::WINCH);
        peer.send_after(
            Duration::from_millis(100),
            b"48;2$y\x1b[?1004;4$y\x1b[?2004;3$y\x1b[?64;1;2c",
        );
        peer.send_after(Duration::from_millis(20), b"b");
        let mut decoder = Decoder::new();
        let probed = probe(&mut tty, &mut decoder).expect("probed");
        assert_eq!(
            probed,
            Probed {
                caps: Caps {
                    kitty: false,
                    sync_2026: false,
                    resize_2048: true,
                    focus: false,
                    paste: true,
                    name: Some("XTerm(390)".to_owned()),
                },
                events: vec![typed('a')],
                signals: Signals::WINCH,
            }
        );
        assert_eq!(
            tty.now(),
            start + Duration::from_millis(410),
            "stopped at DA1"
        );
        assert_eq!(tty.take_signals().unwrap(), Signals::NONE, "taken once");
        assert!(!peer.script_done(), "the key after DA1 is the loop's");
    }

    #[test]
    fn times_out_at_500ms_without_da1() {
        // XTVERSION and one mode answered, DA1 never: the probe ends on its cap, with what
        // it learned, and leaves the half reply it holds in the decoder.
        let (mut tty, peer) = MemTty::pair(SIZE);
        let start = tty.now();
        peer.send_after(
            Duration::from_millis(50),
            b"\x1bP>|kitty(0.36.4)\x1b\\\x1b[?2026;2$yx\x1b[",
        );
        peer.send_after(Duration::from_millis(900), b"late");
        let mut decoder = Decoder::new();
        let probed = probe(&mut tty, &mut decoder).expect("probed");
        assert_eq!(tty.now(), start + PROBE_TIMEOUT);
        assert_eq!(
            probed.caps,
            Caps {
                sync_2026: true,
                name: Some("kitty(0.36.4)".to_owned()),
                ..Caps::default()
            }
        );
        assert_eq!(probed.events, [typed('x')]);
        assert!(
            decoder.deadline().is_some(),
            "the held `ESC [` is the loop's"
        );

        // A terminal that answers nothing at all.
        let (mut tty, _peer) = MemTty::pair(SIZE);
        let start = tty.now();
        let probed = probe(&mut tty, &mut Decoder::new()).expect("probed");
        assert_eq!(probed, Probed::default());
        assert_eq!(tty.now(), start + PROBE_TIMEOUT);
    }
}
