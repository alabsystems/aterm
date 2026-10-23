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

/// Writes after one resize that lead with [`GUARD_RECORD`] (see the module
/// docs for why two).
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

    /// The state after `b` read at a boundary: inside the UTF-8 character it
    /// leads, or at the boundary still (ASCII, a stray continuation, or a byte
    /// UTF-8 never uses — each of which the decoder settles at once).
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
    /// A win32-input-mode record (a CSI whose final byte is `_`) has been
    /// written, so conhost's parser has thrown THE SWITCH (module docs) and
    /// holds a trailing lone ESC.
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
                // restart a match only on an ESC.
                usize::from(b == ESC)
            };
            if self.matched == WIN32_INPUT_REQUEST.len() {
                return true;
            }
        }
        false
    }
}

/// Who a write is from (module docs, REPLIES).
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

/// The win32-input-mode records of one character beyond U+FFFF (module docs,
/// ASTRAL CHARACTERS): its high and low surrogates' key-downs, then their
/// key-ups, with no virtual key, scan code or control-key state.
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
    /// An OpenConsole session's next write first runs the guard's client
    /// (module docs, ONLY CONHOST DELIVERS IT).
    client_owed: AtomicBool,
    /// The input writes' turn at the client ([`Self::settle`]): held while the
    /// client runs, so an input write that comes meanwhile waits for its record
    /// to be in.
    client_turn: Mutex<()>,
    /// conhost asked for win32-input-mode, so its parser reads the record.
    records: AtomicBool,
    /// The reader thread's scan for that request.
    handshake: Mutex<HandshakeScan>,
    /// The input stream after the last write.
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
                        // Nothing landed: the next write still owes it.
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
                    // Nothing landed: the next write owes the record.
                    self.pending.fetch_add(1, Ordering::AcqRel);
                }
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A guard that has seen conhost's request, at a known size.
    fn armed_guard() -> ResizeGuard {
        let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        g.note_output(b"\x1b[?9001h\x1b[?1004h\x1b[?25l");
        g.note_resize(23, 80);
        g
    }

    /// Run `bytes` through `g` into a recording "pipe"; returns (reply, wire).
    fn write(g: &ResizeGuard, bytes: &[u8]) -> (usize, Vec<u8>) {
        let mut wire = Vec::new();
        let n = g
            .write_with(bytes, |b| {
                wire.extend_from_slice(b);
                Ok(b.len())
            })
            .expect("recording write");
        (n, wire)
    }

    fn guarded(bytes: &[u8]) -> Vec<u8> {
        [GUARD_RECORD, bytes].concat()
    }

    /// Where the stream stands after `bytes` follow `from`.
    fn tail_after(from: InputTail, bytes: &[u8]) -> InputTail {
        InputStream {
            tail: from,
            switched: false,
        }
        .after(bytes)
        .tail
    }

    /// The record is a KEY-UP of Shift: field 4 (Kd) is 0, field 1 is VK_SHIFT,
    /// no char — the parts every reader keys its "ignore this" on.
    #[test]
    fn the_guard_record_is_a_shift_key_up() {
        let body = GUARD_RECORD
            .strip_prefix(b"\x1b[")
            .and_then(|b| b.strip_suffix(b"_"))
            .expect("CSI ... _ framing");
        let fields: Vec<u32> = std::str::from_utf8(body)
            .expect("ascii")
            .split(';')
            .map(|f| f.parse().expect("numeric field"))
            .collect();
        assert_eq!(fields, [16, 42, 0, 0, 0, 1]);
        assert_eq!(
            tail_after(InputTail::Ground, GUARD_RECORD),
            InputTail::Ground
        );
    }

    /// The first two writes after a size change lead with the record, the
    /// third does not, and the caller is told only its own bytes were taken.
    #[test]
    fn a_resize_guards_the_next_two_writes_and_reports_only_caller_bytes() {
        let g = armed_guard();
        let paste = b"\x1b[200~cd /c/Windows/Temp\x1b[201~";
        assert_eq!(write(&g, paste), (paste.len(), guarded(paste)));
        assert_eq!(write(&g, b"\r"), (1, guarded(b"\r")));
        assert_eq!(write(&g, b"x"), (1, b"x".to_vec()));
    }

    /// Without a size change nothing is written: an unchanged size queues no
    /// size event, and there is nothing for the runtime to drop.
    #[test]
    fn an_unchanged_size_writes_no_record() {
        let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        g.note_output(b"\x1b[?9001h");
        g.note_resize(24, 80);
        assert_eq!(write(&g, b"x"), (1, b"x".to_vec()));
    }

    /// An adopted session's size is unknown: its first resize arms.
    #[test]
    fn an_unknown_starting_size_arms_on_the_first_resize() {
        let g = ResizeGuard::new(None, ConsoleHostKind::Inbox);
        g.note_output(b"\x1b[?9001h");
        g.note_resize(24, 80);
        assert_eq!(write(&g, b"x").1, guarded(b"x"));
    }

    /// No request from conhost, no record — ever: a conhost that did not ask
    /// may not parse it and could hand the raw bytes to a VT-input reader.
    #[test]
    fn no_record_without_the_win32_input_request() {
        let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        g.note_output(b"\x1b[?1004h\x1b[?25l\x1b[2J");
        g.note_resize(23, 80);
        assert_eq!(write(&g, b"x"), (1, b"x".to_vec()));
        assert_eq!(write(&g, b"\x1b[13;28;13;1;16;1_").0, 18);
        assert_eq!(write(&g, b"\x1b"), (1, b"\x1b".to_vec()));
    }

    /// The request is found split across reads at every position, anywhere in
    /// the startup window, and not past it.
    #[test]
    fn the_request_is_found_across_read_boundaries_inside_the_window() {
        let stream = [b"\x1b[?1004h\x1b[?25".as_slice(), b"l\x1b[?9001h\x1b[2J"].concat();
        for cut in 0..=stream.len() {
            let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
            g.note_output(&stream[..cut]);
            g.note_output(&stream[cut..]);
            assert!(g.records.load(Ordering::Acquire), "split at {cut}");
        }
        let late = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        late.note_output(&vec![b'.'; HANDSHAKE_WINDOW]);
        late.note_output(WIN32_INPUT_REQUEST);
        assert!(!late.records.load(Ordering::Acquire));
        for miss in [
            b"\x1b[?9001l".as_slice(),
            b"\x1b[?90011h",
            b"\x1b[?900\x1b[?9001l",
        ] {
            let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
            g.note_output(miss);
            assert!(!g.records.load(Ordering::Acquire), "{miss:?}");
        }
    }

    /// A write that leaves the stream inside a sequence (a 4 KiB paste slice
    /// cut through `CSI 201 ~`) is never followed by a spliced record: the
    /// guard waits for the write that completes the sequence, then rides the
    /// next one.
    #[test]
    fn the_record_never_splits_an_escape_sequence() {
        let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        g.note_output(b"\x1b[?9001h");
        let (_, first) = write(&g, b"body\x1b[20");
        assert_eq!(first, b"body\x1b[20");
        g.note_resize(23, 80);
        let (n, rest) = write(&g, b"1~");
        assert_eq!((n, rest), (2, b"1~".to_vec()));
        assert_eq!(write(&g, b"x").1, guarded(b"x"));
    }

    /// A slice ending mid-UTF-8 character (`U+4E2D` split after byte 1 or 2)
    /// finishes the character before emitting the guard record.
    #[test]
    fn the_record_never_splits_a_utf8_character() {
        let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        g.note_output(b"\x1b[?9001h");
        assert_eq!(write(&g, b"a\xe4").1, b"a\xe4");
        g.note_resize(23, 80);
        assert_eq!(write(&g, b"\xb8\xad"), (2, b"\xb8\xad".to_vec()));
        assert_eq!(write(&g, b"b").1, guarded(b"b"));
        for (head, tail) in [
            (b"\xe4\xb8".as_slice(), b"\xad".as_slice()),
            (b"\xf0", b"\x9f\x98\x80"),
            (b"\xf0\x9f", b"\x98\x80"),
            (b"\xf0\x9f\x98", b"\x80"),
        ] {
            let g = armed_guard();
            assert_eq!(write(&g, b"c").1, guarded(b"c"));
            assert_eq!(write(&g, head).1, guarded(head));
            g.note_resize(22, 80);
            assert_eq!(write(&g, tail).1, tail, "{head:?} | {tail:?}");
            assert_eq!(write(&g, b"d").1, guarded(b"d"), "{head:?} | {tail:?}");
        }
    }

    /// A failed write keeps the record owed to the next one.
    #[test]
    fn a_failed_write_keeps_the_record_owed() {
        let g = armed_guard();
        let err = g.write_with(b"x", |_| Err(io::Error::other("pipe gone")));
        assert!(err.is_err());
        assert_eq!(write(&g, b"x").1, guarded(b"x"));
        assert_eq!(write(&g, b"y").1, guarded(b"y"));
        assert_eq!(write(&g, b"z").1, b"z".to_vec());
    }

    /// Before conhost has read any record a lone ESC goes out as the byte it
    /// is (conhost still reads it as the Escape key); after the guard's record
    /// — or any other writer's — it goes out as the Escape key's record pair,
    /// guarded when a guard is owed, and the caller is told its one byte went.
    #[test]
    fn a_lone_esc_after_the_switch_is_the_escape_key() {
        let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        g.note_output(b"\x1b[?9001h");
        assert_eq!(write(&g, b"\x1b"), (1, b"\x1b".to_vec()));
        assert_eq!(write(&g, b"z"), (1, b"z".to_vec()));
        g.note_resize(23, 80);
        assert_eq!(write(&g, b"a"), (1, guarded(b"a")));
        assert_eq!(write(&g, b"\x1b"), (1, guarded(ESCAPE_KEY_RECORDS)));
        assert_eq!(write(&g, b"\x1b"), (1, ESCAPE_KEY_RECORDS.to_vec()));
        let e = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        e.note_output(b"\x1b[?9001h");
        assert_eq!(
            write(&e, b"\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_").0,
            36
        );
        assert_eq!(write(&e, b"\x1b"), (1, ESCAPE_KEY_RECORDS.to_vec()));
    }

    /// Only a write that is EXACTLY one ESC at a boundary is the Escape key:
    /// an ESC that completes a sequence (ST's `ESC \` cut after the ESC) or
    /// opens one a later write finishes, and an ESC among other bytes, are
    /// the bytes their writer sent.
    #[test]
    fn only_a_lone_esc_at_a_boundary_is_rewritten() {
        let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        g.note_output(b"\x1b[?9001h");
        write(&g, b"\x1b[13;28;13;1;16;1_");
        assert_eq!(write(&g, b"\x1b]11;?").1, b"\x1b]11;?");
        assert_eq!(write(&g, b"\x1b").1, b"\x1b");
        assert_eq!(write(&g, b"\\").1, b"\\");
        assert_eq!(write(&g, b"a\x1b").1, b"a\x1b");
        assert_eq!(write(&g, b"[A").1, b"[A");
        assert_eq!(write(&g, b"\x1b[A").1, b"\x1b[A");
        assert_eq!(write(&g, b"\x1b\x1b").1, b"\x1b\x1b");
        assert_eq!(write(&g, b"x").1, b"x");
        let short = g.write_with(b"\x1b", |_| Ok(ESCAPE_KEY_RECORDS.len() - 1));
        assert_eq!(short.expect("short write"), 0);
    }

    /// The seam's Escape is the key encoder's Escape under win32-input-mode:
    /// a raw `\x1b` and `ctl key escape` must name the same key.
    #[test]
    fn the_seam_escape_is_the_key_encoders_escape() {
        use aterm_types::keyboard::{Key, KeyboardMode, Modifiers, NamedKey, encode_key};
        let encoded = encode_key(
            &Key::Named(NamedKey::Escape),
            Modifiers::empty(),
            KeyboardMode::WIN32_INPUT,
        );
        assert_eq!(encoded, ESCAPE_KEY_RECORDS);
    }

    /// The boundary tracker over the sequences aterm writes as input.
    #[test]
    fn the_input_tail_follows_the_sequences_aterm_writes() {
        use InputTail::*;
        let cases: &[(&[u8], InputTail)] = &[
            (b"echo hi\r", Ground),
            (b"\x1b[A", Ground),
            (b"\x1bOP", Ground),
            (b"\x1b", Escape),
            (b"\x1bx", Ground),
            (b"\x1b[200~text", Ground),
            (b"text\x1b[20", Csi),
            (b"\x1b[<0;10;5M", Ground),
            (b"\x1b[13;28;13;1;0;1_", Ground),
            (b"\x1b]11;rgb:0000/0000/0000\x07", Ground),
            (b"\x1b]11;rgb:0000/0000/0000\x1b\\", Ground),
            (b"\x1bP1$r0m", Str),
            (b"\x1bP1$r0m\x1b\\", Ground),
            (b"\x1b[12\x18", Ground),
            (b"\x1bO", Ss3),
            ("中é😀".as_bytes(), Ground),
            (b"\xc3", Utf8(1)),
            (b"\xe4\xb8", Utf8(1)),
            (b"\xf0\x9f\x98", Utf8(1)),
            (b"\xf0", Utf8(3)),
            (b"\xe4x", Ground),
            (b"\xe4\x1b", Escape),
            (b"\xe4\x18", Ground),
            (b"\xe4\xe4", Utf8(2)),
            (b"\x80\xbf\xc0\xf5\xff", Ground),
            (b"\x1b\xc3", Utf8(1)),
            (b"\x1b\xc3\xa9", Ground),
            (b"\x1b]0;\xe4", Str),
        ];
        for (bytes, want) in cases {
            assert_eq!(tail_after(Ground, bytes), *want, "{bytes:?}");
        }
        assert_eq!(tail_after(Csi, b";1R"), Ground);
        assert_eq!(tail_after(Str, b"still inside"), Str);
        assert_eq!(tail_after(Str, b"end\x07"), Ground);
        assert_eq!(tail_after(Escape, b"["), Csi);
        assert_eq!(tail_after(Utf8(2), b"\xb8"), Utf8(1));
        assert_eq!(tail_after(Utf8(1), b"\xad"), Ground);
    }

    /// The switch is thrown by a completed `CSI … _` and by nothing else: not
    /// by other CSI finals, not by `_` outside a CSI, and not by a record cut
    /// before its final byte — until the write that completes it.
    #[test]
    fn the_switch_follows_completed_win32_records_only() {
        let start = InputStream::default();
        for bytes in [
            b"plain_text".as_slice(),
            b"\x1b[A_",
            b"\x1b_APC\x1b\\",
            b"\x1b[16;42;0;0;0;1",
        ] {
            assert!(!start.after(bytes).switched, "{bytes:?}");
        }
        let cut = start.after(b"\x1b[16;42;0;0;0;1");
        assert!(cut.after(b"_").switched);
        let on = start.after(GUARD_RECORD);
        assert!(on.switched);
        assert!(
            on.after(b"anything").switched,
            "sticky, as conhost's flag is"
        );
    }

    /// U+1F600's four records: both surrogates down, then both up.
    const GRIN_RECORDS: &[u8] = b"\x1b[0;0;55357;1;0;1_\x1b[0;0;56832;1;0;1_\
                                  \x1b[0;0;55357;0;0;1_\x1b[0;0;56832;0;0;1_";

    /// A guard in `host` that has seen the record request and no resize.
    fn asked(host: ConsoleHostKind) -> ResizeGuard {
        let g = ResizeGuard::new(Some((24, 80)), host);
        g.note_output(b"\x1b[?9001h");
        g
    }

    /// Once conhost requested win32-input-mode, astral characters are encoded as
    /// surrogate-pair records, surrounding BMP text is unchanged, and the
    /// records set `switched` so a subsequent lone `ESC` uses `ESCAPE_KEY_RECORDS`.
    #[test]
    fn an_astral_character_goes_to_conhost_as_its_surrogate_records() {
        let g = asked(ConsoleHostKind::Inbox);
        let typed = "A😀B".as_bytes();
        assert_eq!(
            write(&g, typed),
            (typed.len(), [b"A", GRIN_RECORDS, b"B"].concat())
        );
        assert_eq!(write(&g, b"\x1b"), (1, ESCAPE_KEY_RECORDS.to_vec()));
        let paste = "\x1b[200~😀😀\x1b[201~".as_bytes();
        assert_eq!(
            write(&g, paste).1,
            [b"\x1b[200~", GRIN_RECORDS, GRIN_RECORDS, b"\x1b[201~"].concat()
        );
        let plane2 = "\u{20000}".as_bytes();
        assert_eq!(
            write(&g, plane2),
            (
                4,
                b"\x1b[0;0;55360;1;0;1_\x1b[0;0;56320;1;0;1_\x1b[0;0;55360;0;0;1_\x1b[0;0;56320;0;0;1_"
                    .to_vec()
            )
        );
        g.note_resize(23, 80);
        assert_eq!(
            write(&g, typed),
            (typed.len(), guarded(&[b"A", GRIN_RECORDS, b"B"].concat()))
        );
    }

    /// Everything else goes as typed: a character of the BMP, an astral
    /// character inside a sequence (a title string), any character before
    /// conhost asked for records, and every character for OpenConsole, which
    /// delivers the text whole.
    #[test]
    fn only_an_astral_character_at_a_boundary_to_an_asking_conhost_is_rewritten() {
        let g = asked(ConsoleHostKind::Inbox);
        for kept in [
            "é中✓ plain".as_bytes(),
            "\x1b]0;😀\x07".as_bytes(),
            b"\xf0\x28\x8c\xbc".as_slice(),
        ] {
            assert_eq!(write(&g, kept), (kept.len(), kept.to_vec()), "{kept:?}");
        }
        let typed = "A😀B".as_bytes();
        let unasked = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        assert_eq!(write(&unasked, typed), (typed.len(), typed.to_vec()));
        let openconsole = asked(ConsoleHostKind::OpenConsole);
        assert_eq!(write(&openconsole, typed), (typed.len(), typed.to_vec()));
    }

    /// Only a tab with a program of the MSYS2/Cygwin runtime in it gets the
    /// records: anywhere else the text goes as typed (a cut-off character
    /// included), as it reaches pwsh and cmd whole, and the tab is asked only
    /// when a write holds an astral character.
    #[test]
    fn astral_records_go_only_to_a_tab_running_the_runtime() {
        let g = asked(ConsoleHostKind::Inbox);
        let asks = std::cell::Cell::new(0);
        let native = |bytes: &[u8]| {
            let mut wire = Vec::new();
            let n = g
                .write_with_client(
                    bytes,
                    Writer::Input,
                    |b| {
                        wire.extend_from_slice(b);
                        Ok(b.len())
                    },
                    |_, _| {},
                    || {
                        asks.set(asks.get() + 1);
                        false
                    },
                )
                .expect("recording write");
            (n, wire)
        };
        let typed = "A😀B".as_bytes();
        assert_eq!(native(typed), (typed.len(), typed.to_vec()));
        let cut = [b"ab".as_slice(), &"😀".as_bytes()[..2]].concat();
        assert_eq!(native(&cut), (cut.len(), cut.clone()));
        assert_eq!(asks.get(), 2);
        assert_eq!(native("plain é中".as_bytes()).0, "plain é中".len());
        assert_eq!(asks.get(), 2, "no astral character, no question");
        let g = asked(ConsoleHostKind::Inbox);
        assert_eq!(
            write(&g, typed),
            (typed.len(), [b"A", GRIN_RECORDS, b"B"].concat())
        );
    }

    /// A write that ends partway into an astral character stops before it (the
    /// metered paste writer's next slice starts there), and the next write
    /// carries it whole; a cut-off character alone goes as its bytes.
    #[test]
    fn a_write_cut_inside_an_astral_character_leaves_it_for_the_next() {
        let g = asked(ConsoleHostKind::Inbox);
        let grin = "😀".as_bytes();
        for cut in 1..4 {
            let head = [b"ab", &grin[..cut]].concat();
            assert_eq!(write(&g, &head), (2, b"ab".to_vec()), "cut after {cut}");
            let next = [grin, b"c"].concat();
            assert_eq!(write(&g, &next), (5, [GRIN_RECORDS, b"c"].concat()));
            assert_eq!(
                write(&g, &grin[..cut]),
                (cut, grin[..cut].to_vec()),
                "alone"
            );
            assert_eq!(write(&g, &grin[cut..]).0, 4 - cut);
        }
    }

    /// A rewritten wire that the pipe takes in pieces is finished in this one
    /// call: a record cut short would leave conhost inside it.
    #[test]
    fn a_rewritten_write_is_finished_across_short_writes() {
        let g = asked(ConsoleHostKind::Inbox);
        let typed = "A😀B".as_bytes();
        let mut wire = Vec::new();
        let n = g
            .write_with(typed, |b| {
                let n = b.len().min(7);
                wire.extend_from_slice(&b[..n]);
                Ok(n)
            })
            .expect("short writes");
        assert_eq!(
            (n, wire),
            (typed.len(), [b"A", GRIN_RECORDS, b"B"].concat())
        );
        assert_eq!(write(&g, b"x").1, b"x", "the stream is at a boundary again");
    }

    /// A paste of astral characters whose records outgrow the pipe (72 bytes
    /// of records for 4 of text) goes in writes of at most [`INPUT_WRITE_MAX`],
    /// the record included, each ending between characters: the pipe takes
    /// such a write whole or, released, not at all, so the caller hears the
    /// characters each one took, and a released one answers `Ok(0)`, never an
    /// error after part of the paste went.
    #[test]
    fn records_that_outgrow_the_pipe_go_in_writes_it_takes_whole() {
        let g = armed_guard();
        let paste = "😀".repeat(1024);
        let wires = std::cell::RefCell::new(Vec::<Vec<u8>>::new());
        let pipe = |b: &[u8]| -> io::Result<usize> {
            let mut wires = wires.borrow_mut();
            wires.push(b.to_vec());
            Ok(if wires.len() == 1 {
                b.len().min(INPUT_WRITE_MAX)
            } else {
                0
            })
        };
        let fit = (INPUT_WRITE_MAX - GUARD_RECORD.len()) / GRIN_RECORDS.len();
        let first = g
            .write_with(paste.as_bytes(), &pipe)
            .expect("a write the pipe took");
        assert_eq!(first, fit * 4, "as many whole characters as fit");
        let released = g
            .write_with(&paste.as_bytes()[first..], &pipe)
            .expect("a released write");
        assert_eq!(released, 0);
        assert_eq!(
            *wires.borrow(),
            [
                guarded(&GRIN_RECORDS.repeat(fit)),
                guarded(&GRIN_RECORDS.repeat(1024 - fit)),
            ]
        );
        assert_eq!(write(&g, b"x").1, guarded(b"x"), "the record still owed");
        let g = asked(ConsoleHostKind::Inbox);
        let long = [vec![b'a'; INPUT_WRITE_MAX], "😀".into()].concat();
        assert_eq!(
            write(&g, &long),
            (INPUT_WRITE_MAX, vec![b'a'; INPUT_WRITE_MAX])
        );
        assert_eq!(
            write(&g, &long[INPUT_WRITE_MAX..]),
            (4, GRIN_RECORDS.to_vec())
        );
    }

    /// An OpenConsole session's first write after a size change runs `client`
    /// once with the new size; unchanged sizes and conhost sessions never run it.
    #[test]
    fn an_openconsole_resize_runs_the_client_once_before_the_next_write() {
        let runs = std::cell::RefCell::new(Vec::new());
        let client = |rows, cols| runs.borrow_mut().push((rows, cols));
        let run = |g: &ResizeGuard, bytes: &[u8]| {
            g.write_with_client(bytes, Writer::Input, |b| Ok(b.len()), client, || true)
                .expect("write")
        };
        let g = asked(ConsoleHostKind::OpenConsole);
        run(&g, b"x");
        g.note_resize(24, 80);
        run(&g, b"x");
        assert!(runs.borrow().is_empty(), "no change, no client");
        g.note_resize(30, 100);
        g.note_resize(28, 90);
        run(&g, b"echo");
        run(&g, b"\r");
        assert_eq!(*runs.borrow(), [(28, 90)], "one client, for the last size");
        let conhost = asked(ConsoleHostKind::Inbox);
        conhost.note_resize(30, 100);
        run(&conhost, b"x");
        assert_eq!(runs.borrow().len(), 1, "conhost reads the inline record");
    }

    /// Run `bytes` through `g` as `writer` into a recording "pipe", with a
    /// client that counts its runs; returns (reply, wire).
    fn write_as(
        g: &ResizeGuard,
        writer: Writer,
        bytes: &[u8],
        runs: &std::cell::Cell<u32>,
    ) -> (usize, Vec<u8>) {
        let mut wire = Vec::new();
        let n = g
            .write_with_client(
                bytes,
                writer,
                |b| {
                    wire.extend_from_slice(b);
                    Ok(b.len())
                },
                |_, _| runs.set(runs.get() + 1),
                || true,
            )
            .expect("recording write");
        (n, wire)
    }

    /// After a size change, [`Writer::Reply`] writes immediately without running
    /// the OpenConsole client, leaving the client owed to the next [`Writer::Input`].
    #[test]
    fn a_reply_after_a_resize_leaves_the_openconsole_client_to_the_next_input() {
        let runs = std::cell::Cell::new(0);
        let g = asked(ConsoleHostKind::OpenConsole);
        g.note_resize(20, 60);
        let cpr = b"\x1b[4;1R";
        assert_eq!(
            write_as(&g, Writer::Reply, cpr, &runs),
            (cpr.len(), cpr.to_vec())
        );
        g.note_resize(40, 130);
        assert_eq!(write_as(&g, Writer::Reply, cpr, &runs).1, cpr);
        assert_eq!(runs.get(), 0, "no client for a reply");
        assert_eq!(write_as(&g, Writer::Input, b"typed", &runs).0, 5);
        assert_eq!(runs.get(), 1, "the next input runs it");
        write_as(&g, Writer::Input, b"\r", &runs);
        assert_eq!(runs.get(), 1, "once");
    }

    /// The same on conhost: a reply carries no inline record and does not use
    /// up the two the resize owes, so the next two INPUT writes carry them.
    #[test]
    fn a_reply_after_a_resize_carries_no_record_and_leaves_both_owed() {
        let runs = std::cell::Cell::new(0);
        let g = armed_guard();
        let da1 = b"\x1b[?64;6;22c";
        assert_eq!(
            write_as(&g, Writer::Reply, da1, &runs),
            (da1.len(), da1.to_vec())
        );
        assert_eq!(write_as(&g, Writer::Input, b"x", &runs).1, guarded(b"x"));
        assert_eq!(write_as(&g, Writer::Reply, da1, &runs).1, da1);
        assert_eq!(write_as(&g, Writer::Input, b"y", &runs).1, guarded(b"y"));
        assert_eq!(write_as(&g, Writer::Input, b"z", &runs).1, b"z");
        assert_eq!(runs.get(), 0, "conhost never runs the client");
    }

    /// A wait for something that must happen: a hang detector, never a budget.
    const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);

    /// An input write that owes the OpenConsole client runs it outside `input`'s
    /// lock so a concurrent [`Writer::Reply`] (e.g. `ESC[6n` cursor answer)
    /// completes while the client waits on the host.
    #[test]
    fn a_reply_goes_past_an_input_waiting_for_the_openconsole_client() {
        use std::sync::{Arc, mpsc};
        let g = Arc::new(asked(ConsoleHostKind::OpenConsole));
        g.note_resize(20, 60);
        let wire = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (replied_tx, replied_rx) = mpsc::channel::<()>();
        let input = std::thread::spawn({
            let (g, wire) = (Arc::clone(&g), Arc::clone(&wire));
            move || {
                g.write_with_client(
                    b"typed",
                    Writer::Input,
                    |b| {
                        wire.lock_or_recover().extend_from_slice(b);
                        Ok(b.len())
                    },
                    |_, _| {
                        started_tx.send(()).expect("the test waits");
                        replied_rx
                            .recv_timeout(HANG_DETECTOR)
                            .expect("a reply went past the client");
                    },
                    || true,
                )
            }
        });
        started_rx
            .recv_timeout(HANG_DETECTOR)
            .expect("the input ran the client");
        let cpr = b"\x1b[4;1R";
        let n = g
            .write_with_client(
                cpr,
                Writer::Reply,
                |b| {
                    wire.lock_or_recover().extend_from_slice(b);
                    Ok(b.len())
                },
                |_, _| panic!("a reply runs no client"),
                || true,
            )
            .expect("the reply");
        assert_eq!(n, cpr.len());
        replied_tx.send(()).expect("the client waits");
        assert_eq!(input.join().expect("the input write").expect("written"), 5);
        assert_eq!(
            *wire.lock_or_recover(),
            [&cpr[..], GUARD_RECORD, b"typed"].concat(),
            "the reply first, then the input behind its record"
        );
    }

    /// A guard for `host` that has seen the record request, at 24x80, armed by
    /// a size change; returns the arming.
    fn armed_for_nudge(host: ConsoleHostKind) -> (ResizeGuard, u64) {
        let g = asked(host);
        let arming = g.note_resize(23, 80).expect("a changed size arms");
        (g, arming)
    }

    /// Nudge `g` for `arming` into a recording pipe, with `runtime` as the
    /// tab's answer; returns (whether it sent anything, the wire, client runs).
    fn nudge(g: &ResizeGuard, arming: u64, runtime: bool) -> (bool, Vec<u8>, u32) {
        let mut wire = Vec::new();
        let mut runs = 0;
        let sent = g
            .nudge(
                arming,
                || runtime,
                |_, _| runs += 1,
                |b| {
                    wire.extend_from_slice(b);
                    Ok(b.len())
                },
            )
            .expect("recording nudge");
        (sent, wire, runs)
    }

    /// With no input after a size change, `nudge` on inbox conhost with the
    /// MSYS2 runtime writes the first of the two [`GUARDED_WRITES`] records.
    #[test]
    fn a_nudge_sends_an_inbox_conhost_the_record_no_input_carried() {
        let (g, arming) = armed_for_nudge(ConsoleHostKind::Inbox);
        assert_eq!(nudge(&g, arming, true), (true, GUARD_RECORD.to_vec(), 0));
        assert_eq!(nudge(&g, arming, true), (false, Vec::new(), 0), "once");
        assert_eq!(write(&g, b"x").1, guarded(b"x"), "the second");
        assert_eq!(write(&g, b"y").1, b"y");
    }

    /// No nudge where it is not owed: a tab with no program of the runtime, a
    /// size change an input already followed, an older arming than the latest,
    /// a stream inside a sequence, a conhost that never asked for records.
    #[test]
    fn a_nudge_sends_nothing_where_no_record_is_owed() {
        let (g, arming) = armed_for_nudge(ConsoleHostKind::Inbox);
        assert_eq!(
            nudge(&g, arming, false),
            (false, Vec::new(), 0),
            "no runtime"
        );
        let (g, arming) = armed_for_nudge(ConsoleHostKind::Inbox);
        assert_eq!(write(&g, b"x").1, guarded(b"x"));
        assert!(!nudge(&g, arming, true).0, "an input carried it");
        let (g, older) = armed_for_nudge(ConsoleHostKind::Inbox);
        let newer = g.note_resize(22, 80).expect("armed again");
        assert!(!nudge(&g, older, true).0, "not the latest");
        assert!(nudge(&g, newer, true).0, "the latest");
        let (g, _) = armed_for_nudge(ConsoleHostKind::Inbox);
        assert_eq!(write(&g, b"\x1b[").1, guarded(b"\x1b["));
        let mid = g.note_resize(21, 80).expect("armed again");
        assert!(!nudge(&g, mid, true).0, "inside a sequence");
        let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        let arming = g.note_resize(23, 80).expect("armed");
        assert!(!nudge(&g, arming, true).0, "no record request");
        assert_eq!(
            g.note_resize(23, 80),
            None,
            "an unchanged size arms nothing"
        );
    }

    /// In OpenConsole the nudge settles the client, once, for the last size,
    /// and writes nothing itself (the inline record is inert there); an input
    /// after it finds the client already run.
    #[test]
    fn a_nudge_runs_the_openconsole_client_the_input_did_not() {
        let (g, arming) = armed_for_nudge(ConsoleHostKind::OpenConsole);
        assert_eq!(nudge(&g, arming, true), (true, Vec::new(), 1));
        assert_eq!(nudge(&g, arming, true), (false, Vec::new(), 0), "once");
        let runs = std::cell::Cell::new(0);
        write_as(&g, Writer::Input, b"x", &runs);
        assert_eq!(runs.get(), 0, "the input finds it run");
    }
}
