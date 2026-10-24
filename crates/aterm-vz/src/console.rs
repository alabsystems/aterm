// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The console watcher (`docs/DESIGN-vz-guest-2026-10-23.md` §10): it reads hvc0's bytes as
//! the helper writes them to `console.log`, keeps the tail every `Lost` and `Failed` carries,
//! says when the guest last made console progress, and sees a kernel panic.
//!
//! Progress is a new line, never a byte: systemd's status spinner rewrites one line with a
//! carriage return many times a second while a job hangs (fact 38: 227 frames over two
//! minutes). So a line counts as progress only when no earlier line was the same once its
//! escape sequences, digits and spinner glyphs are removed; `systemd.show_status=error` keeps
//! the spinner off hvc0 in the first place, and this keeps a spinner that shows anyway from
//! reading as a boot that moves.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

/// The kernel's panic line.
pub const PANIC: &str = "Kernel panic - not syncing";

/// The lines the tail keeps.
pub const TAIL_LINES: usize = 40;

/// The bytes one line of the tail keeps.
pub const LINE_BYTES: usize = 512;

/// What a feed saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// The kernel panicked: the panic line.
    Panic(String),
}

/// The watcher's state over one boot's console.
#[derive(Debug, Default)]
pub struct Console {
    partial: Vec<u8>,
    tail: VecDeque<String>,
    seen: HashSet<String>,
    last_progress: Option<Duration>,
    lines: u64,
    repeats: u64,
    panicked: bool,
}

/// The time units systemd glues to a number, longest first.
const UNITS: [&str; 6] = ["min", "ms", "us", "s", "h", "d"];

/// `line` with its ANSI escape sequences, numbers (with a time unit glued to one) and
/// spinner glyphs removed and its spaces collapsed: the form two frames of one spinner line
/// share.
#[must_use]
pub fn normalized(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC [ parameters… final byte in @..~; any other escape: its next char.
            if chars.peek() == Some(&'[') {
                chars.next();
                for f in chars.by_ref() {
                    if ('@'..='~').contains(&f) {
                        break;
                    }
                }
            } else {
                chars.next();
            }
            continue;
        }
        if c.is_ascii_digit() {
            // A number, and the time unit glued to it (systemd's "1min 59s", "2.5ms").
            while chars
                .peek()
                .is_some_and(|n| n.is_ascii_digit() || *n == '.')
            {
                chars.next();
            }
            let rest: String = chars.clone().take(4).collect();
            let glued = UNITS.iter().find(|unit| {
                rest.strip_prefix(**unit)
                    .is_some_and(|after| !after.starts_with(char::is_alphanumeric))
            });
            for _ in 0..glued.map_or(0, |unit| unit.len()) {
                chars.next();
            }
            continue;
        }
        if c == '*' || c.is_control() {
            continue;
        }
        if c.is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// `line` cut to [`LINE_BYTES`] on a character boundary.
fn capped(line: &str) -> String {
    if line.len() <= LINE_BYTES {
        return line.to_string();
    }
    let mut end = LINE_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

impl Console {
    /// A watcher with nothing seen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed `bytes` that arrived at `now`; a line ends at `\n` or `\r`.
    pub fn feed(&mut self, bytes: &[u8], now: Duration) -> Vec<Seen> {
        let mut seen = Vec::new();
        for &b in bytes {
            if b == b'\n' || b == b'\r' {
                let raw = std::mem::take(&mut self.partial);
                let line = String::from_utf8_lossy(&raw).into_owned();
                if let Some(s) = self.line(&line, now) {
                    seen.push(s);
                }
            } else if self.partial.len() < 4 * LINE_BYTES {
                self.partial.push(b);
            }
        }
        seen
    }

    fn line(&mut self, line: &str, now: Duration) -> Option<Seen> {
        let key = normalized(line);
        if key.is_empty() {
            return None;
        }
        self.lines += 1;
        if self.seen.insert(key) {
            self.last_progress = Some(now);
        } else {
            self.repeats += 1;
        }
        let text = capped(line.trim_end());
        if self.tail.len() == TAIL_LINES {
            self.tail.pop_front();
        }
        self.tail.push_back(text.clone());
        if !self.panicked && line.contains(PANIC) {
            self.panicked = true;
            return Some(Seen::Panic(text));
        }
        None
    }

    /// When the last line no earlier line matched arrived.
    #[must_use]
    pub const fn last_progress(&self) -> Option<Duration> {
        self.last_progress
    }

    /// The lines seen, and how many of them repeated an earlier one.
    #[must_use]
    pub const fn counts(&self) -> (u64, u64) {
        (self.lines, self.repeats)
    }

    /// The last whole line, capped.
    #[must_use]
    pub fn last_line(&self) -> Option<&str> {
        self.tail.back().map(String::as_str)
    }

    /// The tail: the last [`TAIL_LINES`] lines, each capped, one per line.
    #[must_use]
    pub fn tail(&self) -> String {
        let mut out = String::new();
        for line in &self.tail {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn a_spinners_frames_are_one_line_and_its_job_line_progress_once() {
        let mut c = Console::new();
        // systemd's spinner: a carriage return, an erase, the frame, colours, a moving
        // asterisk run and a growing elapsed time.
        let frames = [
            "\r\u{1b}[K[\u{1b}[0;1;31m*     \u{1b}[0m] A start job is running for Wait for Network to be Configured (2s / no limit)",
            "\r\u{1b}[K[\u{1b}[0;1;31m**    \u{1b}[0m] A start job is running for Wait for Network to be Configured (3s / no limit)",
            "\r\u{1b}[K[ \u{1b}[0;31m***  \u{1b}[0m] A start job is running for Wait for Network to be Configured (1min 59s / no limit)",
        ];
        c.feed(frames[0].as_bytes(), S);
        c.feed(b"\r", S);
        assert_eq!(c.last_progress(), Some(S));
        for (i, frame) in frames.iter().enumerate().skip(1) {
            let at = S * (10 + u32::try_from(i).unwrap());
            c.feed(frame.as_bytes(), at);
            c.feed(b"\r", at);
        }
        assert_eq!(c.last_progress(), Some(S), "a spinner frame is no progress");
        assert_eq!(c.counts(), (3, 2));
        c.feed(b"[  OK  ] Reached target network.target.\n", S * 30);
        assert_eq!(c.last_progress(), Some(S * 30), "a new line is");
    }

    #[test]
    fn the_negative_control_distinct_lines_are_each_progress() {
        let mut c = Console::new();
        for i in 0..5u32 {
            let line = format!(
                "[  OK  ] Started unit-{}.service.\n",
                char::from(b'a' + i as u8)
            );
            c.feed(line.as_bytes(), S * i);
            assert_eq!(c.last_progress(), Some(S * i));
        }
        assert_eq!(c.counts(), (5, 0));
    }

    #[test]
    fn a_kernel_panic_is_seen_once_with_its_line() {
        let mut c = Console::new();
        assert!(
            c.feed(b"[    1.2] Run /init as init process\n", S)
                .is_empty()
        );
        let seen = c.feed(
            b"[    1.9] Kernel panic - not syncing: VFS: Unable to mount root fs on unknown-block(0,0)\n",
            S,
        );
        assert_eq!(
            seen,
            [Seen::Panic(
                "[    1.9] Kernel panic - not syncing: VFS: Unable to mount root fs on unknown-block(0,0)"
                    .into()
            )]
        );
        assert!(c.feed(b"Kernel panic - not syncing: again\n", S).is_empty());
        assert!(c.tail().contains("Unable to mount root fs"));
    }

    #[test]
    fn the_tail_keeps_the_last_lines_each_capped_and_a_partial_line_waits() {
        let mut c = Console::new();
        for i in 0..(TAIL_LINES + 5) {
            c.feed(format!("line {i} of {}\n", "x".repeat(i)).as_bytes(), S);
        }
        let tail = c.tail();
        assert_eq!(tail.lines().count(), TAIL_LINES);
        assert!(tail.starts_with("line 5 of"), "{tail}");
        c.feed(&b"y".repeat(LINE_BYTES * 2), S);
        assert_eq!(
            c.tail(),
            tail,
            "a line without its end is not in the tail yet"
        );
        c.feed(b"\n", S);
        let last = c.tail().lines().last().unwrap().to_string();
        assert!(
            last.ends_with('…') && last.len() <= LINE_BYTES + 3,
            "{}",
            last.len()
        );
    }

    #[test]
    fn normalizing_drops_escapes_digits_and_spinner_glyphs() {
        assert_eq!(
            normalized(
                "\u{1b}[0;1;31m[ ***] \u{1b}[0mJob x.service/start running (12s / 1min 30s)"
            ),
            "[ ] Job x.service/start running ( / )"
        );
        assert_eq!(normalized("   \u{1b}[K  "), "");
        // A word that only starts like a unit stays.
        assert_eq!(normalized("took 3 seconds, 4 days"), "took seconds, days");
        assert_eq!(normalized("eth0 up after 1min 2s"), "eth up after");
        assert_eq!(normalized("4é5mé"), "émé");
    }
}
