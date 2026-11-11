// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! ConPTY resize input guard: writes a sacrificial input record after a size change so MSYS2/Cygwin console readers drop the guard record instead of user input.

use std::borrow::Cow;
use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use aterm_types::MutexExt;

use super::INPUT_WRITE_MAX;
use crate::ConsoleHostKind;

/// conhost's request for win32-input-mode records (DECSET 9001).
const WIN32_INPUT_REQUEST: &[u8] = b"\x1b[?9001h";

/// How much of the output stream is searched for [`WIN32_INPUT_REQUEST`].
const HANDSHAKE_WINDOW: usize = 4 * 1024;

/// Shift key-up as a win32-input-mode record: Vk 16 (VK_SHIFT), Sc 42, no
/// UnicodeChar, Kd 0 (released), no control-key state, repeat 1.
const GUARD_RECORD: &[u8] = b"\x1b[16;42;0;0;0;1_";

/// The Escape key as the win32-input-mode record pair (down, then up):
/// VK_ESCAPE, scan 1, UnicodeChar ESC, no control-key state, repeat 1 — the
/// bytes the key encoder writes for Escape once conhost asked for records.
const ESCAPE_KEY_RECORDS: &[u8] = b"\x1b[27;1;27;1;0;1_\x1b[27;1;27;0;0;1_";

/// Number of writes after a resize that lead with [`GUARD_RECORD`].
const GUARDED_WRITES: u8 = 2;

/// Where the INPUT byte stream stands after the bytes written so far: at a
/// boundary, inside a UTF-8 character, or inside an escape sequence a later
/// write completes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum InputTail {
    #[default]
    Ground,
    /// Inside a UTF-8 character, this many continuation bytes still to come.
    Utf8(u8),
    Escape,
    Csi,
    Ss3,
    Str,
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

impl InputTail {
    fn step(self, b: u8) -> Self {
        match (self, b) {
            (_, ESC) => Self::Escape,
            (_, CAN | SUB) => Self::Ground,
            (Self::Ground, _) => Self::lead(b),
            (Self::Utf8(left), 0x80..=0xbf) if left > 1 => Self::Utf8(left - 1),
            (Self::Utf8(_), 0x80..=0xbf) => Self::Ground,
            (Self::Utf8(_), _) => Self::lead(b),
            (Self::Escape, b'[') => Self::Csi,
            (Self::Escape, b'O') => Self::Ss3,
            (Self::Escape, b']' | b'P' | b'_' | b'^' | b'X') => Self::Str,
            (Self::Escape | Self::Ss3, _) => Self::lead(b),
            (Self::Csi, 0x40..=0x7e) => Self::Ground,
            (Self::Csi, _) => Self::Csi,
            (Self::Str, BEL) => Self::Ground,
            (Self::Str, _) => Self::Str,
        }
    }

    /// Next state after reading `b` at a sequence boundary.
    fn lead(b: u8) -> Self {
        match b {
            0xc2..=0xdf => Self::Utf8(1),
            0xe0..=0xef => Self::Utf8(2),
            0xf0..=0xf4 => Self::Utf8(3),
            _ => Self::Ground,
        }
    }
}

/// What the seam knows of the input conhost has been sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InputStream {
    /// Where the bytes written so far end.
    tail: InputTail,
    /// Whether a win32-input-mode record (`CSI … _`) has been written.
    switched: bool,
}

impl InputStream {
    /// The stream after `bytes` follow it.
    fn after(self, bytes: &[u8]) -> Self {
        bytes.iter().fold(self, |s, &b| Self {
            switched: s.switched || (s.tail == InputTail::Csi && b == b'_'),
            tail: s.tail.step(b),
        })
    }
}

/// The reader thread's search for conhost's win32-input-mode request.
#[derive(Debug, Default)]
struct HandshakeScan {
    /// Bytes of [`WIN32_INPUT_REQUEST`] matched at the end of the stream so far.
    matched: usize,
    /// Output bytes searched so far.
    scanned: usize,
}

impl HandshakeScan {
    /// Search `chunk`; `true` once the request has been seen.
    fn feed(&mut self, chunk: &[u8]) -> bool {
        for &b in chunk {
            if self.scanned >= HANDSHAKE_WINDOW {
                return false;
            }
            self.scanned += 1;
            self.matched = if b == WIN32_INPUT_REQUEST[self.matched] {
                self.matched + 1
            } else {
                usize::from(b == ESC)
            };
            if self.matched == WIN32_INPUT_REQUEST.len() {
                return true;
            }
        }
        false
    }
}

/// Who a write is from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Writer {
    /// Input a person or a program drove into the tab: keys, pastes, reports.
    Input,
    /// The terminal's own answer to a query its program or console host sent: it passes the guard, which stays owed to the next [`Writer::Input`].
    Reply,
}

/// Pack a ConPTY size for change detection.
fn pack(rows: i16, cols: i16) -> u32 {
    (u32::from(rows as u16) << 16) | u32::from(cols as u16)
}

/// [`pack`]'s inverse: `(rows, cols)`.
fn unpack(size: u32) -> (i16, i16) {
    ((size >> 16) as u16 as i16, size as u16 as i16)
}

/// The win32-input-mode records of one character beyond U+FFFF: its high and
/// low surrogates' key-downs, then their key-ups, with no virtual key, scan
/// code or control-key state.
fn push_astral_records(wire: &mut Vec<u8>, c: char) {
    let mut units = [0u16; 2];
    let pair = c.encode_utf16(&mut units);
    for down in [1, 0] {
        for unit in pair.iter() {
            wire.extend_from_slice(format!("\x1b[0;0;{unit};{down};0;1_").as_bytes());
        }
    }
}

/// Rewrite 4-byte UTF-8 characters starting at `Ground` as surrogate-pair
/// win32-input-mode records for inbox conhost, into at most `room` bytes of
/// wire.
fn astral_as_records(mut tail: InputTail, bytes: &[u8], room: usize) -> Option<(Vec<u8>, usize)> {
    let mut wire: Option<Vec<u8>> = None;
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if tail == InputTail::Ground && (0xf0..=0xf4).contains(&bytes[i]) {
            let end = (i + 4).min(bytes.len());
            match std::str::from_utf8(&bytes[i..end]) {
                Ok(s) => {
                    if let Some(c) = s.chars().next() {
                        let w = wire.get_or_insert_with(|| Vec::with_capacity(bytes.len() + 64));
                        w.extend_from_slice(&bytes[copied..i]);
                        copied = i;
                        let at = w.len();
                        push_astral_records(w, c);
                        if w.len() > room && i > 0 {
                            w.truncate(at);
                            break;
                        }
                        i = end;
                        copied = end;
                        continue;
                    }
                }
                Err(e) if e.error_len().is_none() && i > 0 => {
                    let mut w = wire.unwrap_or_default();
                    w.extend_from_slice(&bytes[copied..i]);
                    return Some((w, i));
                }
                Err(_) => {}
            }
        }
        if wire.as_ref().map_or(0, Vec::len) + (i - copied) >= room {
            break;
        }
        tail = tail.step(bytes[i]);
        i += 1;
    }
    wire.map(|mut w| {
        w.extend_from_slice(&bytes[copied..i]);
        (w, i)
    })
}

/// One session's resize guard.
#[derive(Debug)]
pub(super) struct ResizeGuard {
    /// The console host the session runs in: conhost reads the inline record
    /// and needs astral characters as records; OpenConsole gets the client's.
    host: ConsoleHostKind,
    /// The last size handed to conhost ([`pack`]); 0 while unknown.
    size: AtomicU32,
    /// Writes still to lead with the guard record.
    pending: AtomicU8,
    /// How many size changes have armed the guard: [`Self::nudge`] acts only
    /// for the latest.
    armings: AtomicU64,
    /// An OpenConsole session's next write first runs the guard's client: only
    /// conhost delivers the inline record.
    client_owed: AtomicBool,
    /// The input writes' turn at the client ([`Self::settle`]): held while the
    /// client runs, so an input write that comes meanwhile waits for its record
    /// to be in.
    client_turn: Mutex<()>,
    /// conhost asked for win32-input-mode, so its parser reads the record.
    records: AtomicBool,
    /// The reader thread's scan for that request.
    handshake: Mutex<HandshakeScan>,
    /// Input stream state after the last write.
    input: Mutex<InputStream>,
}

impl ResizeGuard {
    /// A guard for a pseudoconsole in `host` created at `rows`x`cols` (`None` when the size is not known, as for an adopted handoff: its first resize arms).
    pub(super) fn new(size: Option<(i16, i16)>, host: ConsoleHostKind) -> Self {
        let guard = Self {
            host,
            size: AtomicU32::new(0),
            pending: AtomicU8::new(0),
            armings: AtomicU64::new(0),
            client_owed: AtomicBool::new(false),
            client_turn: Mutex::new(()),
            records: AtomicBool::new(false),
            handshake: Mutex::new(HandshakeScan::default()),
            input: Mutex::new(InputStream::default()),
        };
        if let Some((rows, cols)) = size {
            guard.size.store(pack(rows, cols), Ordering::Relaxed);
        }
        guard
    }

    /// Record that conhost was sent `rows`x`cols`; a CHANGED size arms the guard and answers the arming's number, for [`Self::nudge`] to name.
    pub(super) fn note_resize(&self, rows: i16, cols: i16) -> Option<u64> {
        let size = pack(rows, cols);
        if self.size.swap(size, Ordering::AcqRel) == size {
            return None;
        }
        self.pending.store(GUARDED_WRITES, Ordering::Release);
        if self.host == ConsoleHostKind::OpenConsole {
            self.client_owed.store(true, Ordering::Release);
        }
        Some(self.armings.fetch_add(1, Ordering::AcqRel).wrapping_add(1))
    }

    /// Send a standalone guard record for `arming` ([`Self::note_resize`]) when no input has followed the resize and `arming` is still the latest.
    pub(super) fn nudge(
        &self,
        arming: u64,
        runtime_in_tab: impl FnOnce() -> bool,
        client: impl FnOnce(i16, i16),
        mut write: impl FnMut(&[u8]) -> io::Result<usize>,
    ) -> io::Result<bool> {
        let latest = || self.armings.load(Ordering::Acquire) == arming;
        if !latest() {
            return Ok(false);
        }
        if self.host == ConsoleHostKind::OpenConsole {
            let mut ran = false;
            self.settle(Writer::Input, |rows, cols| {
                ran = true;
                client(rows, cols);
            });
            return Ok(ran);
        }
        let unsent = || self.pending.load(Ordering::Acquire) == GUARDED_WRITES;
        if !self.records.load(Ordering::Acquire) || !unsent() || !runtime_in_tab() {
            return Ok(false);
        }
        let mut input = self.input.lock_or_recover();
        if input.tail != InputTail::Ground
            || !latest()
            || self
                .pending
                .compare_exchange(
                    GUARDED_WRITES,
                    GUARDED_WRITES - 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
        {
            return Ok(false);
        }
        let mut written = 0;
        while written < GUARD_RECORD.len() {
            let landed = match write(&GUARD_RECORD[written..]) {
                Ok(0) => Err(io::ErrorKind::WriteZero.into()),
                landed => landed,
            };
            match landed {
                Ok(n) => {
                    let n = n.min(GUARD_RECORD.len() - written);
                    *input = input.after(&GUARD_RECORD[written..written + n]);
                    written += n;
                }
                Err(e) => {
                    if written == 0 {
                        self.pending.fetch_add(1, Ordering::AcqRel);
                    }
                    return Err(e);
                }
            }
        }
        Ok(true)
    }

    /// How many of the writes after the last size change still owe the record.
    #[cfg(test)]
    pub(super) fn records_owed(&self) -> u8 {
        self.pending.load(Ordering::Acquire)
    }

    /// Feed a chunk of conhost's OUTPUT (reader thread): watches for the win32-input-mode request that makes the record safe to write.
    pub(super) fn note_output(&self, chunk: &[u8]) {
        if self.records.load(Ordering::Acquire) {
            return;
        }
        if self.handshake.lock_or_recover().feed(chunk) {
            self.records.store(true, Ordering::Release);
        }
    }

    /// Test helper: [`Self::write_with_client`] as [`Writer::Input`] with astral record encoding enabled.
    #[cfg(test)]
    pub(super) fn write_with(
        &self,
        bytes: &[u8],
        write: impl FnMut(&[u8]) -> io::Result<usize>,
    ) -> io::Result<usize> {
        self.write_with_client(bytes, Writer::Input, write, |_, _| {}, || true)
    }

    /// Run `client` with the last size when an OpenConsole session's guard is owed, serialized by `client_turn` and outside `input`'s lock so [`Writer::Reply`] writes can proceed concurrently without deadlocking OpenConsole's cursor-position query.
    pub(super) fn settle(&self, writer: Writer, client: impl FnOnce(i16, i16)) {
        if writer != Writer::Input {
            return;
        }
        let _turn = self.client_turn.lock_or_recover();
        if self.client_owed.swap(false, Ordering::AcqRel) {
            let (rows, cols) = unpack(self.size.load(Ordering::Acquire));
            client(rows, cols);
        }
    }

    /// Write `bytes` for `writer`, settling `client` on OpenConsole and rewriting astral characters or lone `ESC` when required.
    pub(super) fn write_with_client(
        &self,
        bytes: &[u8],
        writer: Writer,
        mut write: impl FnMut(&[u8]) -> io::Result<usize>,
        client: impl FnOnce(i16, i16),
        runtime_in_tab: impl FnOnce() -> bool,
    ) -> io::Result<usize> {
        let input_write = !bytes.is_empty() && writer == Writer::Input;
        if input_write {
            self.settle(writer, client);
        }
        let mut input = self.input.lock_or_recover();
        let records = self.records.load(Ordering::Acquire);
        let at_boundary = input.tail == InputTail::Ground;
        let escape_key = records && input.switched && at_boundary && bytes == [ESC];
        let guarded = input_write
            && at_boundary
            && records
            && self
                .pending
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok();
        let prefix: &[u8] = if guarded { GUARD_RECORD } else { &[] };
        let room = INPUT_WRITE_MAX - prefix.len();
        let rewritten = (records && !escape_key && self.host == ConsoleHostKind::Inbox)
            .then(|| astral_as_records(input.tail, bytes, room))
            .flatten()
            .filter(|_| runtime_in_tab());
        let (payload, taken): (Cow<'_, [u8]>, usize) = match rewritten {
            Some((wire, taken)) => (Cow::Owned(wire), taken),
            None if escape_key => (Cow::Borrowed(ESCAPE_KEY_RECORDS), 1),
            None => {
                let fits = bytes.len().min(room);
                (Cow::Borrowed(&bytes[..fits]), fits)
            }
        };
        let finish = matches!(payload, Cow::Owned(_));
        let framed;
        let wire: &[u8] = if guarded {
            framed = [GUARD_RECORD, &payload].concat();
            &framed
        } else {
            &payload
        };
        match write(wire) {
            Ok(0) => {
                if guarded {
                    self.pending.fetch_add(1, Ordering::AcqRel);
                }
                Ok(0)
            }
            Ok(written) => {
                let mut written = written.min(wire.len());
                *input = input.after(&wire[..written]);
                if finish {
                    while written < wire.len() {
                        let more = write(&wire[written..])?.min(wire.len() - written);
                        if more == 0 {
                            return Err(io::ErrorKind::WriteZero.into());
                        }
                        *input = input.after(&wire[written..written + more]);
                        written += more;
                    }
                    return Ok(taken);
                }
                let landed = written.saturating_sub(prefix.len());
                Ok(if escape_key {
                    usize::from(landed == payload.len())
                } else {
                    landed
                })
            }
            Err(e) => {
                if guarded {
                    self.pending.fetch_add(1, Ordering::AcqRel);
                }
                Err(e)
            }
        }
    }
}

#[cfg(test)]
#[path = "resize_guard_tests.rs"]
mod tests;
