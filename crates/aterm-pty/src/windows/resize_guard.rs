// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The resize guard: one input record written ahead of the input that
//! follows a ConPTY resize, so a console reader that loses the record after a
//! resize loses that one instead of the user's.
//!
//! WHAT IS LOST, measured 2026-09-27 on this box (conhost 10.0.26200, Git for
//! Windows bash 5.3.9 on the MSYS2 runtime 3.6.7): after `aterm ctl resize`,
//! the FIRST input record that reached bash vanished. A bracketed paste lost
//! its opening ESC, so readline inserted `[200~xyzzy~` (and a turn ran
//! `[200~cd` — "command not found") in 6 of 16 tries; a typed `xyzzy` came out
//! `yzzy` in 2 of 12. pwsh and cmd lost nothing. `turn` hit it (4 of 8) on its
//! FIRST use in a window: the tab strip grows a row for the turn (`dims`
//! tab_rows 1 -> 2, `stty size` 24 -> 23), so its paste followed a resize.
//!
//! WHERE, read from the MSYS2 runtime source (winsup/cygwin/fhandler/console.cc
//! at msys2-3.6.7; the same code is on cygwin `main` as of 2026-09-27):
//! `process_input_message` peeks the console input queue and, on a
//! `WINDOW_BUFFER_SIZE_EVENT` that changed the size, `send_winch_maybe`
//! RELEASES the input mutex around `kill_pgrp (SIGWINCH)`. The runtime's
//! `cons_master_thread` can take the mutex in that gap, read the queue, drop
//! the size event as processed and write the rest back. `process_input_message`
//! then discards `i + 1` records from the head of the queue — its count from
//! the peek, one more than is left of it — so the record AFTER the size event
//! goes, whenever it arrives (a paste a full second after the resize lost its
//! ESC too). A terminal cannot keep the runtime from dropping that record, so
//! the seam makes sure the record dropped is one nobody needs.
//!
//! THE RECORD is a Shift key-UP in conhost's win32-input-mode
//! (`CSI Vk;Sc;Uc;Kd;Cs;Rc _`, microsoft/terminal spec #4999). Measured with a
//! `ReadConsoleInputW` dump under this conhost: it arrives as exactly one
//! KEY_EVENT (key up, VK_SHIFT, scan 0x2A, no char, no modifiers) both with
//! and without ENABLE_VIRTUAL_TERMINAL_INPUT. No reader acts on it as a key:
//! the MSYS2 runtime skips key-ups, and bash, pwsh (PSReadLine) and cmd showed
//! nothing and lost nothing with three of them queued. With the guard in this
//! seam the same bash lost nothing in 48 resize-then-input tries (24 pastes,
//! 24 typed lines), and ten first turns went through whole.
//!
//! THE SWITCH it throws. The record is not inert to conhost itself. The first
//! win32-input-mode record conhost's input parser reads sets a flag it never
//! clears (`_encounteredWin32InputModeSequence`, microsoft/terminal
//! `InputStateMachineEngine.cpp`), and from then on the parser stops settling
//! a read that ENDS in a lone ESC, or in an ESC plus one byte it cannot finish,
//! as the Escape key or Alt+key (`StateMachine::ProcessString`: a terminal
//! speaking win32-input-mode never sends those, so a trailing ESC must be a
//! sequence cut across reads). It holds the ESC and joins it to the next key.
//! Measured 2026-09-27 (pwsh `[Console]::ReadKey`, raw bytes by `ctl send
//! --stdin`): before any resize, `ESC`, 1.5 s, `z` read as Escape then Z; after
//! a resize and one `a`, the same bytes read as Alt+Z. The guard only brings
//! the switch forward (aterm's Shift+Enter record already threw it), and two
//! halves keep the keys whole across it:
//! * the key ENCODER sends every key whose legacy bytes the switched parser
//!   holds as that key's record (`win32_escape_record` in aterm-types
//!   `keyboard/encode.rs`: Escape, Ctrl+[, Alt+[, Alt+Backspace, …). This guard
//!   must not ship without it: alone it would hold Alt+Backspace, Alt+[ and
//!   the other two-byte forms in every tab after the first resize (measured
//!   after the switch: raw `ESC DEL` then `z` read as Alt+Z, while `ctl key
//!   alt+backspace` read as Alt+Backspace).
//!   `the_seam_escape_is_the_key_encoders_escape` fails without it.
//! * the SEAM keeps the Escape key of a RAW writer (`ctl send`, `feed`,
//!   `feed-bin` with a bare `\x1b`): once a record has gone to this conhost, a
//!   write that is exactly one ESC, at a sequence boundary, goes out as the
//!   Escape key's record pair, the key conhost read that ESC as before the
//!   switch. A raw two-byte held form (`ESC [`, `ESC O`, `ESC ESC`, `ESC DEL`:
//!   Alt+[, Alt+Shift+O, Alt+Escape, Alt+Backspace) is still held after the
//!   switch, because the seam cannot know which physical key those bytes mean;
//!   `ctl key` sends those keys through the encoder.
//!
//! WHEN the guard record is written — three gates:
//! * conhost ASKED for win32-input-mode (`CSI ? 9001 h`, the first 8 bytes a
//!   fresh ConPTY writes on 10.0.26200). A conhost that never asked may not
//!   parse the record, and one that does not would hand a VT-input reader the
//!   raw bytes, so without the request nothing is ever written.
//! * the resize CHANGED the size. The runtime drops a record only for a size
//!   it had not seen (`send_winch_maybe` compares), so an unchanged size needs
//!   no guard.
//! * the input stream is at a BOUNDARY: not inside an escape sequence and not
//!   inside a UTF-8 character. A paste of 64 KiB or more is written in 4 KiB
//!   slices cut at raw byte offsets. A record spliced into a half-written
//!   `CSI 201 ~` would break the very marker it protects, and one spliced
//!   between a character's bytes makes conhost's UTF-8 decoder replace the
//!   character (measured 2026-09-27: a U+4E2D at bytes 4095..4097 of a
//!   74,104-byte paste after a resize reached `cat` as three U+FFFD). The
//!   guard then waits for the next write that starts clean.
//!
//! It rides the first [`GUARDED_WRITES`] writes after the resize, not just
//! one: conhost reads the resize (signal pipe) and the input (input pipe) on
//! two threads, so a write issued right after `ResizePseudoConsole` can be
//! queued AHEAD of the size event, and then the record the runtime drops is
//! the one in the NEXT write — which carries the guard too.

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use aterm_types::MutexExt;

/// conhost's request for win32-input-mode records (DECSET 9001).
const WIN32_INPUT_REQUEST: &[u8] = b"\x1b[?9001h";

/// How much of the output stream is searched for [`WIN32_INPUT_REQUEST`].
/// conhost writes it first (bytes 0..8 of a fresh ConPTY on 10.0.26200); the
/// window only bounds the search so a conhost that never asks costs nothing
/// past its startup preamble.
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
/// write completes. Only as fine-grained as that question needs — CSI, SS3,
/// the string controls (OSC/DCS/APC/PM/SOS, which terminal replies use), and
/// a two-byte `ESC x` (an Alt-prefixed key).
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
            // ESC starts a sequence wherever it falls (inside a string it is
            // the first byte of ST or of whatever cancels the string; inside a
            // character the decoder replaces the fragment and reads it afresh).
            (_, ESC) => Self::Escape,
            // CAN/SUB abort any sequence in progress.
            (_, CAN | SUB) => Self::Ground,
            (Self::Ground, _) => Self::lead(b),
            (Self::Utf8(left), 0x80..=0xbf) if left > 1 => Self::Utf8(left - 1),
            (Self::Utf8(_), 0x80..=0xbf) => Self::Ground,
            // Any other byte ends the fragment and is read afresh.
            (Self::Utf8(_), _) => Self::lead(b),
            (Self::Escape, b'[') => Self::Csi,
            (Self::Escape, b'O') => Self::Ss3,
            (Self::Escape, b']' | b'P' | b'_' | b'^' | b'X') => Self::Str,
            // `ESC x` (and ST's `ESC \`) is complete at its second byte — which
            // may lead a character, as Alt+é's `ESC C3 A9` does.
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
    /// holds a trailing lone ESC. Sticky, as conhost's flag is.
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
                // ESC occurs only at the pattern's start, so a mismatch can
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

/// Pack a ConPTY size for change detection.
fn pack(rows: i16, cols: i16) -> u32 {
    (u32::from(rows as u16) << 16) | u32::from(cols as u16)
}

/// One session's resize guard. Lives in the session registry entry beside the
/// handles it guards; every operation is on the seam's own threads.
#[derive(Debug, Default)]
pub(super) struct ResizeGuard {
    /// The last size handed to conhost ([`pack`]); 0 while unknown.
    size: AtomicU32,
    /// Writes still to lead with the guard record.
    pending: AtomicU8,
    /// conhost asked for win32-input-mode, so its parser reads the record.
    records: AtomicBool,
    /// The reader thread's scan for that request.
    handshake: Mutex<HandshakeScan>,
    /// The input stream after the last write. Held across each write, so the
    /// decision and the bytes it decided about stay in order; writes on one
    /// pipe handle are serialized by the kernel anyway.
    input: Mutex<InputStream>,
}

impl ResizeGuard {
    /// A guard for a pseudoconsole created at `rows`x`cols` (`None` when the
    /// size is not known, as for an adopted handoff: its first resize arms).
    pub(super) fn new(size: Option<(i16, i16)>) -> Self {
        let guard = Self::default();
        if let Some((rows, cols)) = size {
            guard.size.store(pack(rows, cols), Ordering::Relaxed);
        }
        guard
    }

    /// Record that conhost was sent `rows`x`cols`; a CHANGED size arms the guard.
    pub(super) fn note_resize(&self, rows: i16, cols: i16) {
        let size = pack(rows, cols);
        if self.size.swap(size, Ordering::AcqRel) != size {
            self.pending.store(GUARDED_WRITES, Ordering::Release);
        }
    }

    /// Feed a chunk of conhost's OUTPUT (reader thread): watches for the
    /// win32-input-mode request that makes the record safe to write.
    pub(super) fn note_output(&self, chunk: &[u8]) {
        if self.records.load(Ordering::Acquire) {
            return;
        }
        if self.handshake.lock_or_recover().feed(chunk) {
            self.records.store(true, Ordering::Release);
        }
    }

    /// Write `bytes` with `write` (one `WriteFile` on the input pipe), leading
    /// with [`GUARD_RECORD`] when the guard is armed and may be written, and
    /// sending a lone ESC as [`ESCAPE_KEY_RECORDS`] once conhost has switched.
    /// Returns the count of the CALLER's bytes that landed: the guard's own
    /// bytes are left out, and a lone ESC counts once its whole pair is in.
    pub(super) fn write_with(
        &self,
        bytes: &[u8],
        write: impl FnOnce(&[u8]) -> io::Result<usize>,
    ) -> io::Result<usize> {
        let mut input = self.input.lock_or_recover();
        let records = self.records.load(Ordering::Acquire);
        let at_boundary = input.tail == InputTail::Ground;
        let escape_key = records && input.switched && at_boundary && bytes == [ESC];
        let payload = if escape_key {
            ESCAPE_KEY_RECORDS
        } else {
            bytes
        };
        let guarded = !bytes.is_empty()
            && at_boundary
            && records
            && self
                .pending
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok();
        let framed;
        let (prefix, wire): (&[u8], &[u8]) = if guarded {
            framed = [GUARD_RECORD, payload].concat();
            (GUARD_RECORD, &framed)
        } else {
            (&[], payload)
        };
        match write(wire) {
            Ok(written) => {
                let written = written.min(wire.len());
                *input = input.after(&wire[..written]);
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
        let g = ResizeGuard::new(Some((24, 80)));
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
        let g = ResizeGuard::new(Some((24, 80)));
        g.note_output(b"\x1b[?9001h");
        g.note_resize(24, 80);
        assert_eq!(write(&g, b"x"), (1, b"x".to_vec()));
    }

    /// An adopted session's size is unknown: its first resize arms.
    #[test]
    fn an_unknown_starting_size_arms_on_the_first_resize() {
        let g = ResizeGuard::new(None);
        g.note_output(b"\x1b[?9001h");
        g.note_resize(24, 80);
        assert_eq!(write(&g, b"x").1, guarded(b"x"));
    }

    /// No request from conhost, no record — ever: a conhost that did not ask
    /// may not parse it and could hand the raw bytes to a VT-input reader.
    /// And no Escape rewrite either, whatever was written before.
    #[test]
    fn no_record_without_the_win32_input_request() {
        let g = ResizeGuard::new(Some((24, 80)));
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
            let g = ResizeGuard::new(Some((24, 80)));
            g.note_output(&stream[..cut]);
            g.note_output(&stream[cut..]);
            assert!(g.records.load(Ordering::Acquire), "split at {cut}");
        }
        let late = ResizeGuard::new(Some((24, 80)));
        late.note_output(&vec![b'.'; HANDSHAKE_WINDOW]);
        late.note_output(WIN32_INPUT_REQUEST);
        assert!(!late.records.load(Ordering::Acquire));
        // Near misses are not the request.
        for miss in [
            b"\x1b[?9001l".as_slice(),
            b"\x1b[?90011h",
            b"\x1b[?900\x1b[?9001l",
        ] {
            let g = ResizeGuard::new(Some((24, 80)));
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
        let g = ResizeGuard::new(Some((24, 80)));
        g.note_output(b"\x1b[?9001h");
        let (_, first) = write(&g, b"body\x1b[20");
        assert_eq!(first, b"body\x1b[20");
        g.note_resize(23, 80);
        let (n, rest) = write(&g, b"1~");
        assert_eq!((n, rest), (2, b"1~".to_vec()));
        assert_eq!(write(&g, b"x").1, guarded(b"x"));
    }

    /// Nor a UTF-8 character: a slice that ends after U+4E2D's lead byte
    /// (`E4 | B8 AD`, the review's 4 KiB boundary) and one that ends after its
    /// second byte are each finished before the record goes out.
    #[test]
    fn the_record_never_splits_a_utf8_character() {
        let g = ResizeGuard::new(Some((24, 80)));
        g.note_output(b"\x1b[?9001h");
        assert_eq!(write(&g, b"a\xe4").1, b"a\xe4");
        g.note_resize(23, 80);
        assert_eq!(write(&g, b"\xb8\xad"), (2, b"\xb8\xad".to_vec()));
        assert_eq!(write(&g, b"b").1, guarded(b"b"));
        // The same character cut after its second byte, and a four-byte one
        // cut after each of its first three.
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
        let g = ResizeGuard::new(Some((24, 80)));
        g.note_output(b"\x1b[?9001h");
        assert_eq!(write(&g, b"\x1b"), (1, b"\x1b".to_vec()));
        assert_eq!(write(&g, b"z"), (1, b"z".to_vec()));
        g.note_resize(23, 80);
        assert_eq!(write(&g, b"a"), (1, guarded(b"a")));
        assert_eq!(write(&g, b"\x1b"), (1, guarded(ESCAPE_KEY_RECORDS)));
        assert_eq!(write(&g, b"\x1b"), (1, ESCAPE_KEY_RECORDS.to_vec()));
        // An encoder record (Shift+Enter) throws the switch with no resize.
        let e = ResizeGuard::new(Some((24, 80)));
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
        let g = ResizeGuard::new(Some((24, 80)));
        g.note_output(b"\x1b[?9001h");
        write(&g, b"\x1b[13;28;13;1;16;1_");
        assert_eq!(write(&g, b"\x1b]11;?").1, b"\x1b]11;?");
        assert_eq!(write(&g, b"\x1b").1, b"\x1b");
        assert_eq!(write(&g, b"\\").1, b"\\");
        assert_eq!(write(&g, b"a\x1b").1, b"a\x1b");
        assert_eq!(write(&g, b"[A").1, b"[A");
        assert_eq!(write(&g, b"\x1b[A").1, b"\x1b[A");
        assert_eq!(write(&g, b"\x1b\x1b").1, b"\x1b\x1b");
        // The pair is written whole or the caller is told nothing went.
        assert_eq!(write(&g, b"x").1, b"x");
        let short = g.write_with(b"\x1b", |_| Ok(ESCAPE_KEY_RECORDS.len() - 1));
        assert_eq!(short.expect("short write"), 0);
    }

    /// The seam's Escape is the key encoder's Escape under win32-input-mode:
    /// a raw `\x1b` and `ctl key escape` must name the same key. This is also
    /// the encoder half of THE SWITCH (module docs) — without it the encoder
    /// writes a bare ESC the switched parser holds.
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
            // UTF-8: whole characters, cut ones, and what ends a fragment.
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
            // Alt+é is ESC plus a whole character, cut or not.
            (b"\x1b\xc3", Utf8(1)),
            (b"\x1b\xc3\xa9", Ground),
            // Inside a string, UTF-8 is the string's business.
            (b"\x1b]0;\xe4", Str),
        ];
        for (bytes, want) in cases {
            assert_eq!(tail_after(Ground, bytes), *want, "{bytes:?}");
        }
        // Continuation across writes, the way a sliced frame arrives.
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
}
