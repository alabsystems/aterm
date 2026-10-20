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
//!
//! ONLY CONHOST DELIVERS IT. The runtime reads in VT-input mode, and for such
//! a reader OpenConsole passes text alone: the guard (and every key-up) arrives
//! as nothing, so the drop takes the user's key again. An OpenConsole session
//! gets the OTHER half instead: before the first input write after a size
//! change (not a reply: REPLIES), or by itself when no input follows (THE
//! STALL), when a runtime program is in the tab, a second console client
//! writes a `MENU_EVENT` record no reader acts on ([`super::console_guard`],
//! with the numbers), and the write waits for it. The inline record still
//! rides along there; it is inert.
//!
//! ASTRAL CHARACTERS. The inbox conhost hands a VT-input reader a character
//! beyond U+FFFF typed as UTF-8 text without its high surrogate (measured
//! 2026-10-17: Git Bash read `printf %s 'A😀B'` as `41 ed b8 80 42`, a lone
//! U+DE00, for every emoji typed or pasted), while the same character sent as
//! win32-input-mode records, both surrogates' key-downs then both key-ups with
//! no virtual key, reached Git Bash, Windows PowerShell 5.1 and cmd whole
//! (`VK_PACKET` records did not: PSReadLine dropped them). So once conhost has
//! asked for records, and while a program of that runtime is in the tab (the
//! same probe of the tab's job and process tree as the OpenConsole half's,
//! `console_guard::runtime_in_tab`), every four-byte
//! UTF-8 character that starts at a boundary goes as those four records, and a
//! write that ends partway into one stops before it, for the caller's next
//! write (which starts with it) to carry it whole. Only then: as text the
//! character already reaches pwsh 7, Windows PowerShell 5.1 and cmd whole,
//! and as records cmd's own line editor echoed a narrow one (𝐀, U+1D400) as
//! U+FFFD although the command still got it (measured 2026-10-17). OpenConsole
//! delivers such text intact and is sent it as typed.
//!
//! REPLIES pass the guard ([`Writer::Reply`]): the terminal's answer to a query
//! neither runs the OpenConsole client nor carries the inline record, and the
//! guard stays owed to the next input. OpenConsole asks for the cursor itself
//! after a resize: the first `GetConsoleScreenBufferInfo` once the size changed
//! writes `ESC[6n` and holds the host's one API thread up to 500 ms for the
//! answer (microsoft/terminal `WaitForConptyCursorPositionToBeSynchronized`),
//! and the MSYS2 runtime makes that call on every size event. A guarded answer
//! waited for the client, and the client could not connect to a host whose API
//! thread was waiting for that answer, so each resize stalled the runtime 500
//! ms and its answer landed after the host had stopped waiting. The host
//! captures an answer only while it has asked (one flag, not a count), so with
//! resizes 50 to 300 ms apart, a window drag, late answers and new requests
//! crossed and an answer the host no longer expected reached Git Bash as typed
//! text: `$ ;1Rtyped` or `$ Rtyped`, after a bell, in 4 of 4 tries (measured
//! 2026-10-18, OpenConsole 1.25.2609.30003).
//! An answer that goes straight is captured within milliseconds and never
//! becomes input, so the record the runtime drops is still the next input's,
//! and that is where the guard waits. Nor may a write that waits for the
//! client hold an answer up: an input write runs the client
//! ([`ResizeGuard::settle`]) before it takes the input stream's lock, and
//! aterm-session's sink has it run before the sink's frame lock
//! (`aterm_pty::settle_input`), which the reply writer takes too. Run under
//! those locks, a key typed while the host waited for its answer held the
//! answer back the same way.
//!
//! REPORTS are input. A full-screen program that turns on DEC 2048 (vim does,
//! and OpenConsole forwards it) is sent an in-band size report after every
//! resize, and it went out in the same write as the answer to the host's
//! cursor query, as a reply: past the guard, the record after the size event.
//! The runtime dropped its ESC, and vim read `[48;20;70;460;630t` as keys,
//! showed `900t` or `630t` pending and kept the old width: 9 of 36 resizes in
//! pwsh and Git Bash tabs (OpenConsole 1.25.2609.30003, measured 2026-10-18).
//! The terminal keeps the reports it sends unasked apart from its answers
//! (aterm-core's `Terminal::take_reports`), and aterm-gui sends them through
//! the session's ordered input writer, as it sends mouse and focus reports,
//! behind the burst's answers: that writer waits for the client, the reply
//! writer never does. 0 of 36 after.
//!
//! THE STALL. The dropped record is not only lost: until a record comes for
//! the runtime's discard to take, its read sits there, and the program with
//! it. The inbox conhost does not forward 2048, so vim there redraws on the
//! size event alone, and it kept the old size after 10 of 36 resizes until a
//! key came, whose record the guard then gave up (measured 2026-10-18). So
//! when no input has followed a size change within 50 ms (the backend's
//! `NUDGE_AFTER`), the guard sends its record by itself
//! ([`ResizeGuard::nudge`]): in a tab with a runtime program, the inline
//! record to an inbox conhost, and the client in OpenConsole. 0 of 36 after. A
//! newer size change takes the nudge over, so a window drag whose steps come
//! faster than that sends one record when it stops, not one per step.

use std::borrow::Cow;
use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use aterm_types::MutexExt;

use crate::ConsoleHostKind;

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

/// Who a write is from (module docs, REPLIES).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Writer {
    /// Input a person or a program drove into the tab: keys, pastes, reports.
    Input,
    /// The terminal's own answer to a query its program or console host sent:
    /// it passes the guard, which stays owed to the next [`Writer::Input`].
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

/// `bytes` as an inbox conhost must be sent them after `tail` (module docs,
/// ASTRAL CHARACTERS): each four-byte UTF-8 character that starts at a boundary
/// as its records. Returns the wire and how many of `bytes` it carries, or
/// `None` when `bytes` go as they are. A four-byte character that the write cuts
/// off at its end is left out of the count when anything comes before it, for
/// the caller's next write to start with; alone, it goes as bytes (there is no
/// count short of zero to report).
fn astral_as_records(mut tail: InputTail, bytes: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut wire: Option<Vec<u8>> = None;
    // `bytes[..copied]` are in `wire` already.
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
                        push_astral_records(w, c);
                        i = end;
                        copied = end;
                        continue;
                    }
                }
                // A whole character's start, cut off by the end of the write.
                Err(e) if e.error_len().is_none() && i > 0 => {
                    let mut w = wire.unwrap_or_default();
                    w.extend_from_slice(&bytes[copied..i]);
                    return Some((w, i));
                }
                Err(_) => {}
            }
        }
        tail = tail.step(bytes[i]);
        i += 1;
    }
    wire.map(|mut w| {
        w.extend_from_slice(&bytes[copied..]);
        (w, bytes.len())
    })
}

/// One session's resize guard. Lives in the session registry entry beside the
/// handles it guards; every operation is on the seam's own threads.
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
    /// to be in. Never held with [`Self::input`], and a reply never takes it
    /// (module docs, REPLIES).
    client_turn: Mutex<()>,
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
    /// A guard for a pseudoconsole in `host` created at `rows`x`cols` (`None`
    /// when the size is not known, as for an adopted handoff: its first resize
    /// arms).
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

    /// Record that conhost was sent `rows`x`cols`; a CHANGED size arms the guard
    /// and answers the arming's number, for [`Self::nudge`] to name.
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

    /// THE STALL (module docs): the guard's record by itself, for arming
    /// `arming` ([`Self::note_resize`]) when no input has carried it since, so
    /// a program of the MSYS2/Cygwin runtime that is waiting for one more
    /// record after the size change gets one and goes on. Only the latest
    /// arming nudges: a newer size change armed the guard again and nudges for
    /// itself. In OpenConsole it settles the client ([`Self::settle`], which
    /// asks the tab for the runtime itself); to an inbox conhost that asked for
    /// records, in a tab where `runtime_in_tab`, it writes [`GUARD_RECORD`] at a
    /// boundary as the first of the [`GUARDED_WRITES`], so the next input still
    /// carries the second. Whether it sent or ran anything.
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
        // Cheap questions first, then the probe, outside the stream's lock.
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
    /// Astral characters go as records here, as in a tab running the MSYS2
    /// runtime.
    #[cfg(test)]
    pub(super) fn write_with(
        &self,
        bytes: &[u8],
        write: impl FnMut(&[u8]) -> io::Result<usize>,
    ) -> io::Result<usize> {
        self.write_with_client(bytes, Writer::Input, write, |_, _| {}, || true)
    }

    /// Run `client` with the size last sent when an OpenConsole session's guard
    /// is owed (module docs, ONLY CONHOST DELIVERS IT), and return once it has
    /// run: in the input writes' turn and outside the input stream's lock, so
    /// an input write that comes while it runs waits here for its record, and
    /// a reply, which settles nothing, goes past it. An input writer calls this
    /// before it takes any lock a reply needs (`aterm_pty::settle_input`): the
    /// client may be waiting on the host, and the host on the terminal's
    /// answer to its own cursor query (module docs, REPLIES).
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

    /// [`Self::write_with`] for `writer`, first settling `client` when an
    /// OpenConsole session's guard is owed ([`Self::settle`]), and sending an
    /// inbox conhost each astral character
    /// as its records when `runtime_in_tab` says a program of the MSYS2/Cygwin
    /// runtime is in the tab (ASTRAL CHARACTERS; asked only of a write that
    /// holds one). A write whose wire was rewritten that way is finished here:
    /// a record cut short would leave conhost inside it, and the caller's retry
    /// starts its character over. A [`Writer::Reply`] runs no client and
    /// carries no record, and leaves both owed (module docs, REPLIES).
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
        let rewritten = (records && !escape_key && self.host == ConsoleHostKind::Inbox)
            .then(|| astral_as_records(input.tail, bytes))
            .flatten()
            .filter(|_| runtime_in_tab());
        let (payload, taken): (Cow<'_, [u8]>, usize) = match rewritten {
            Some((wire, taken)) => (Cow::Owned(wire), taken),
            None if escape_key => (Cow::Borrowed(ESCAPE_KEY_RECORDS), 1),
            None => (Cow::Borrowed(bytes), bytes.len()),
        };
        let finish = matches!(payload, Cow::Owned(_));
        let guarded = input_write
            && at_boundary
            && records
            && self
                .pending
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok();
        let framed;
        let (prefix, wire): (&[u8], &[u8]) = if guarded {
            framed = [GUARD_RECORD, &payload].concat();
            (GUARD_RECORD, &framed)
        } else {
            (&[], &payload)
        };
        match write(wire) {
            // A write a sever or a stop released before anything landed: the
            // next write owes the record, and the caller sees nothing taken.
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
    /// And no Escape rewrite either, whatever was written before.
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
        // Near misses are not the request.
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

    /// Nor a UTF-8 character: a slice that ends after U+4E2D's lead byte
    /// (`E4 | B8 AD`, the review's 4 KiB boundary) and one that ends after its
    /// second byte are each finished before the record goes out.
    #[test]
    fn the_record_never_splits_a_utf8_character() {
        let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
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
        let g = ResizeGuard::new(Some((24, 80)), ConsoleHostKind::Inbox);
        g.note_output(b"\x1b[?9001h");
        assert_eq!(write(&g, b"\x1b"), (1, b"\x1b".to_vec()));
        assert_eq!(write(&g, b"z"), (1, b"z".to_vec()));
        g.note_resize(23, 80);
        assert_eq!(write(&g, b"a"), (1, guarded(b"a")));
        assert_eq!(write(&g, b"\x1b"), (1, guarded(ESCAPE_KEY_RECORDS)));
        assert_eq!(write(&g, b"\x1b"), (1, ESCAPE_KEY_RECORDS.to_vec()));
        // An encoder record (Shift+Enter) throws the switch with no resize.
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

    /// U+1F600's four records: both surrogates down, then both up.
    const GRIN_RECORDS: &[u8] = b"\x1b[0;0;55357;1;0;1_\x1b[0;0;56832;1;0;1_\
                                  \x1b[0;0;55357;0;0;1_\x1b[0;0;56832;0;0;1_";

    /// A guard in `host` that has seen the record request and no resize.
    fn asked(host: ConsoleHostKind) -> ResizeGuard {
        let g = ResizeGuard::new(Some((24, 80)), host);
        g.note_output(b"\x1b[?9001h");
        g
    }

    /// ASTRAL CHARACTERS: once conhost asked for records, an emoji goes to it as
    /// its surrogate pair's records, the text around it as typed, and the
    /// caller hears its own byte count; the records throw THE SWITCH like any
    /// other, so a lone ESC after them is the Escape key.
    #[test]
    fn an_astral_character_goes_to_conhost_as_its_surrogate_records() {
        let g = asked(ConsoleHostKind::Inbox);
        let typed = "A😀B".as_bytes();
        assert_eq!(
            write(&g, typed),
            (typed.len(), [b"A", GRIN_RECORDS, b"B"].concat())
        );
        assert_eq!(write(&g, b"\x1b"), (1, ESCAPE_KEY_RECORDS.to_vec()));
        // Two in a row, a bracketed paste's body, and Plane 2 (U+20000).
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
        // The guard rides in front, and the caller still hears its own count.
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

    /// ONLY CONHOST DELIVERS IT: an OpenConsole session's first write after a
    /// size change runs the client with the size sent, once; an unchanged size
    /// and a conhost session never run it.
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

    /// REPLIES: after a size change an OpenConsole session's answer to the
    /// host's `ESC[6n` goes out at once, as it is, with no client run before
    /// it (the host's API thread waits for this very answer, and the client
    /// cannot connect until it has it); the client stays owed to the next
    /// input, which runs it once, for the last size.
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

    /// REPLIES: an input write that owes the OpenConsole client runs it outside
    /// the input stream's lock, so a reply written while the client runs goes
    /// through. The client may be waiting on the console host and the host on
    /// that reply (its own cursor query after a resize); with the client run
    /// under the lock, the reply waited for the client and the client for the
    /// reply, until the host gave up. The input's bytes follow the client.
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

    /// THE STALL: with no input after a size change, an inbox conhost in a tab
    /// of the MSYS2 runtime gets the record by itself, once, as the first of
    /// the two the resize owes, so the next input still carries the second and
    /// the one after it none.
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
