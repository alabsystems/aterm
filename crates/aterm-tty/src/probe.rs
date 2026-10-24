// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Terminal capability probe.

use std::io;
use std::time::Duration;

use crate::decode::{Decoder, Event, ModeState, Reply};
use crate::signals::Signals;
use crate::tty::{Readiness, Tty};

/// How long the probe waits for DA1.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Probe query: XTVERSION, kitty flags, DECRQM (2026, 2048, 1004, 2004), and DA1 last.
pub const QUERY: &[u8] = b"\x1b[>0q\x1b[?u\x1b[?2026$p\x1b[?2048$p\x1b[?1004$p\x1b[?2004$p\x1b[c";

/// Terminal capabilities reported by [`probe`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caps {
    /// Supports kitty keyboard protocol push.
    pub kitty: bool,
    /// Supports DEC 2026 synchronized updates.
    pub sync_2026: bool,
    /// Supports DEC 2048 in-band resize reports.
    pub resize_2048: bool,
    /// Supports DEC 1004 focus reports.
    pub focus: bool,
    /// Supports DEC 2004 bracketed paste.
    pub paste: bool,
    /// XTVERSION name (`aterm(<version>)` on aterm).
    pub name: Option<String>,
}

impl Caps {
    /// Record one reply; returns `true` on DA1.
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

/// Probe results and events or signals received while waiting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probed {
    pub caps: Caps,
    /// Non-reply events (typeahead, resize reports) read during the probe, in order.
    pub events: Vec<Event>,
    /// Signals taken during the probe, for the caller to handle as its loop would: after `CONT`
    /// (raw again) a repaint, after `WINCH` a new size.
    pub signals: Signals,
}

/// Query terminal capabilities through `decoder` until DA1 or [`PROBE_TIMEOUT`].
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
                let read = buf.get(..n).unwrap_or(&buf);
                for event in decoder.feed(read, tty.now()) {
                    match event {
                        Event::Reply(reply) if !done => done = probed.caps.read(reply),
                        other => probed.events.push(other),
                    }
                }
                done |= tty.now() >= deadline;
            }
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

    fn aterm_replies(setup: &[u8]) -> Vec<u8> {
        let mut term = Terminal::new(24, 80);
        term.process(setup);
        let _ = term.take_response();
        term.process(QUERY);
        term.take_response().expect("aterm answers the probe")
    }

    #[test]
    fn parses_replies_and_stops_at_da1() {
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

        let (mut tty, _peer) = MemTty::pair(SIZE);
        let start = tty.now();
        let probed = probe(&mut tty, &mut Decoder::new()).expect("probed");
        assert_eq!(probed, Probed::default());
        assert_eq!(tty.now(), start + PROBE_TIMEOUT);
    }
}
