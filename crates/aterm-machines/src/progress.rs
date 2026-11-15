// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Progress lines on a terminal nobody may be reading.
//!
//! The root half of a Mac Setup tab (`aterm machines here apply --plain` under `sudo`) writes
//! its progress into the tab's PTY. After aterm quits, the keeper keeps that PTY's master and
//! nothing reads it, so a plain blocking write stops the install once about a dozen
//! outage-retry lines fill the terminal (ATERM-KI-1110-146). [`DropOnFull`] writes a line only
//! when the terminal has room for it, drops it otherwise, and counts what it dropped; the next
//! line that finds room says that count first, and [`settle`] says it once the work is over
//! when the terminal has room then; otherwise the count waits for the next line with room.
//! Nothing here waits for a reader, so the run goes on to its later items and exits.
//!
//! A progress line is never what a run's outcome rests on: the plain root run prints no report;
//! its exit status and the engine's journal of each item's outcome carry that.

use std::os::fd::RawFd;
use std::sync::{Mutex, PoisonError};

/// The most bytes one write carries. A pipe that polls writable has room for `PIPE_BUF` (512
/// on macOS) and a macOS tty slave room for 768 (its low-water mark is 256 of 1024), so a
/// write this size after a ready poll does not block, `\n` to `\r\n` included.
pub(crate) const CHUNK: usize = 512;

/// What [`DropOnFull::line`] did with a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Written whole, after the count of the lines dropped before it, if any.
    Written,
    /// Dropped and counted: the terminal had no room, or is gone.
    Dropped,
}

/// A writer of progress lines to one descriptor that never waits on its reader.
#[derive(Debug)]
pub(crate) struct DropOnFull {
    fd: RawFd,
    dropped: u64,
}

impl DropOnFull {
    /// A writer to `fd`, which the caller keeps open for as long as it writes.
    pub(crate) const fn new(fd: RawFd) -> Self {
        Self { fd, dropped: 0 }
    }

    /// The lines dropped since the last one written.
    #[cfg(test)]
    pub(crate) const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Write `text` as one line when the descriptor has room, else drop and count it. A line
    /// longer than [`CHUNK`] (with the drop count before it) is cut at a character boundary.
    pub(crate) fn line(&mut self, text: &str) -> Delivery {
        let mut out = self.note();
        let room = CHUNK.saturating_sub(out.len() + 1);
        out.push_str(clip(text, room));
        out.push('\n');
        if ready(self.fd) && write_now(self.fd, out.as_bytes()) {
            self.dropped = 0;
            Delivery::Written
        } else {
            self.dropped = self.dropped.saturating_add(1);
            Delivery::Dropped
        }
    }

    /// Once the work is over, say how many lines were dropped since the last one written, when
    /// the descriptor has room now. With no room (nobody reads it) or a gone terminal the note
    /// is not written and the count stays for the next line with room: this never waits, so
    /// the items after the caller's and the run's exit do not wait on a reader.
    pub(crate) fn settle(&mut self) -> Delivery {
        if self.dropped == 0 {
            return Delivery::Written;
        }
        let note = self.note();
        if ready(self.fd) && write_now(self.fd, note.as_bytes()) {
            self.dropped = 0;
            Delivery::Written
        } else {
            Delivery::Dropped
        }
    }

    fn note(&self) -> String {
        match self.dropped {
            0 => String::new(),
            1 => "(1 progress line dropped while nothing read this terminal)\n".to_owned(),
            n => format!("({n} progress lines dropped while nothing read this terminal)\n"),
        }
    }
}

/// This process's standard error, shared by every progress line it writes.
static STDERR: Mutex<DropOnFull> = Mutex::new(DropOnFull::new(libc::STDERR_FILENO));

/// One progress line on standard error, dropped and counted when nothing reads it.
pub(crate) fn say(line: &str) {
    STDERR
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .line(line);
}

/// Say on standard error how many progress lines were dropped, once the work is over, when
/// it has room; never waits for a reader.
pub(crate) fn settle() {
    STDERR
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .settle();
}

/// The longest prefix of `text` within `room` bytes that ends at a character boundary.
fn clip(text: &str, room: usize) -> &str {
    if text.len() <= room {
        return text;
    }
    let mut end = room;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Whether `fd` has room for a [`CHUNK`] now: it polls writable and neither hung up nor
/// failed.
fn ready(fd: RawFd) -> bool {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        // SAFETY: `poll` is one valid pollfd for the duration of the call.
        let answered = unsafe { libc::poll(&raw mut poll, 1, 0) };
        if answered >= 0 {
            break;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
    }
    poll.revents & libc::POLLOUT != 0
        && poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0
}

/// Write `bytes` (at most [`CHUNK`]) to `fd`, which polled ready. False where the descriptor
/// refused it (gone, or non-blocking and full).
fn write_now(fd: RawFd, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        // SAFETY: `bytes` is a valid buffer of its length; `fd` is open (see `new`).
        let wrote = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        match usize::try_from(wrote) {
            Ok(0) => return false,
            Ok(n) => bytes = &bytes[n..],
            Err(_) if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {
            }
            Err(_) => return false,
        }
    }
    true
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;
