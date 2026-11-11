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
    let deadline = tty.now().checked_add(PROBE_TIMEOUT);
    let mut probed = Probed::default();
    let mut done = false;
    let mut buf = [0u8; 4096];
    while !done {
        match tty.read(&mut buf, deadline)? {
            Readiness::Bytes(n) => {
                let read = buf.get(..n).unwrap_or(&buf);
                for event in decoder.feed(read, tty.now()) {
                    match event {
                        Event::Reply(reply) if !done => done = probed.caps.read(reply),
                        other => probed.events.push(other),
                    }
                }
                if let Some(deadline) = deadline {
                    done |= tty.now() >= deadline;
                }
            }
            Readiness::Signal => probed.signals = probed.signals.union(tty.take_signals()?),
            Readiness::Timeout | Readiness::Closed => done = true,
        }
    }
    Ok(probed)
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;
