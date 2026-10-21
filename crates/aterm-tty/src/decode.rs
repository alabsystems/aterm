// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The input decoder: the bytes a terminal writes to the program on it, read back
//! into keys, mouse reports, pastes, focus changes, resizes and probe replies
//! (docs/DESIGN-amacs-2026-10-19.md §5.3).
//!
//! It is pure. [`Decoder::feed`] takes what one read returned and when it was read;
//! [`Decoder::flush`] resolves what the reads left unfinished once its deadline has
//! passed with nothing more read. Nothing sleeps and nothing reads a clock, and how the
//! bytes were split into reads never changes the events: only a `flush` can, and only
//! past the deadline.
//!
//! # What it reads
//!
//! - Legacy keys: C0 controls, ESC-prefixed Alt, UTF-8 assembled across reads
//!   (malformed bytes become U+FFFD exactly where [`String::from_utf8_lossy`] puts
//!   one).
//! - `CSI … ~` keys and the `CSI 1 ; m X` letter keys with xterm's `m`, SS3 keys
//!   (F1-F4, application-mode cursor keys, the application keypad), `CSI Z`, and
//!   xterm's `CSI 27 ; m ; code ~`.
//! - Kitty's `CSI code[:shifted[:base]] [; mods[:event]] [; text] u`, every
//!   [`NamedKey`] by its kitty code. amacs pushes flags 1 and 4 (disambiguate, and the
//!   shifted key of a Shift chord); neither touches text, so IME composition stays
//!   safe. The base-layout key is read past and dropped.
//! - SGR 1006 mouse reports, and X10 ones (`CSI M` and three raw bytes) so a terminal
//!   that ignores 1006 never types its reports into a document.
//! - Bracketed paste (`CSI 200 ~` … `CSI 201 ~`) read raw, with no key decoding
//!   inside, and capped at [`MAX_PASTE_BYTES`]; focus (`CSI I`, `CSI O`); DEC 2048's
//!   in-band size report (`CSI 48 ; rows ; cols ; … t`).
//! - Probe replies ([`Reply`]): XTVERSION, kitty's flags, DECRPM and DA1.
//!
//! # Canonical readings
//!
//! Legacy bytes lose information, so the decoder picks one reading of each; kitty
//! flag 1 spells every one of these chords as `CSI … u`, which reads back exactly.
//!
//! - TAB is Tab (Ctrl+I), CR is Enter (Ctrl+M), and a lone ESC is Escape (Ctrl+[).
//! - BS and DEL are both Backspace: aterm's Backspace is DEL and its Ctrl+H and
//!   Ctrl+Backspace are BS, but a terminal whose Backspace key sends BS (DECBKM) must
//!   still delete.
//! - LF is Ctrl+J, aterm's legacy Shift+Enter included. NUL is Ctrl+Space, and FS,
//!   GS, RS and US are Ctrl+\, Ctrl+], Ctrl+^ and Ctrl+_.
//! - An ASCII capital is its letter with Shift (`A` is Shift+a, text `A`), as the GUI
//!   names the key; every other glyph is itself with no Shift (`!`, not Shift+1),
//!   since the layout that typed it is unknown. A space is [`NamedKey::Space`].
//! - A kitty Shift chord with its shifted key (flag 4) reads as that glyph, as legacy
//!   bytes spell it: `CSI 44:60 ; 4 u` is Alt+`<`, as `ESC <` is. Without the shifted
//!   key it stays the base key with Shift (Alt+Shift+`,`): the decoder does not guess
//!   a layout.
//! - A keypad key is its main-block twin ([`Key::main_block_twin`]), as legacy bytes
//!   and the GUI's native pages read it: keypad 5 is `5` and types `5` when no chord
//!   modifier is held (Shift picks a keypad level, never a glyph), keypad Enter is
//!   Enter. Keypad Begin has no twin and stays itself.
//! - Caps Lock and Num Lock are locks, not modifiers: kitty reports them on chords,
//!   and they are dropped, so Ctrl+a reads the same with Caps Lock on.
//! - `ESC [` and `ESC O` begin sequences, so legacy Alt+[ and Alt+Shift+O are read as
//!   such only when nothing follows before a flush, or when what follows makes no
//!   sequence (`ESC [` and a final byte no terminal sends bare). `ESC P` begins a
//!   reply only when the next byte could start one, and `ESC ]` only when a number and
//!   `;` follow (amacs asks no OSC question, so Alt+] 1 C-g stays three keys);
//!   otherwise they are Alt+Shift+P and Alt+]. A reply interrupted by a control byte
//!   was typed keys.
//! - `CSI 1 ; m R` is Shift/Ctrl/Alt+F3, never a cursor position report: amacs does
//!   not ask for one. Typeahead of Alt+[, digits and a final byte no key uses
//!   (`ESC [ 1 a`) is a well-formed CSI and is delivered [`Event::Unknown`]: reading
//!   unknown sequences as typing would put a terminal's unsolicited reports into the
//!   document instead.
//!
//! # Time
//!
//! A read that ends inside a sequence (a lone ESC included) arms a deadline
//! [`ESC_TIMEOUT`] after it, and one that ends inside a bracketed paste
//! [`PASTE_TIMEOUT`] after it. Bytes read later always continue what is held, however
//! late they are read: they may have arrived long before, while the caller was busy.
//! The caller waits for input until [`Decoder::deadline`] and calls [`Decoder::flush`]
//! when that wait found nothing to read, as vim's `ttimeout` measures from when the
//! editor is idle. The flush delivers a held sequence as the keys its bytes spell: a
//! lone ESC is [`NamedKey::Escape`], which amacs's keymap reads as the Meta prefix, so
//! the split never changes meaning. It delivers a stalled paste as far as it got and
//! goes back to reading keys, so an end marker that never comes (a lost write, or an
//! `ESC [ 200 ~` that was typed) cannot hold the keyboard. A caller waiting on a
//! probe's replies does not flush before its own cap, so a reply split across slow
//! reads is not read as typing.

use std::time::{Duration, Instant};

use aterm_types::keyboard::{Key, Modifiers, NamedKey};
use aterm_types::mouse::{ALT_MASK, CTRL_MASK, MouseButton, SHIFT_MASK, WheelDir};

/// How long a read that ended inside a sequence waits for the rest of it.
pub const ESC_TIMEOUT: Duration = Duration::from_millis(25);

/// How long a bracketed paste may stall, with nothing read, before what came of it is
/// delivered. Long enough for a congested ssh link to resume; short enough that a
/// person who stops typing gets the keyboard back.
pub const PASTE_TIMEOUT: Duration = Duration::from_secs(2);

/// The most of one bracketed paste kept; the rest of it is read and dropped. It is
/// amacs's document size limit, so no paste can make a document it could not open.
pub const MAX_PASTE_BYTES: usize = 32 * 1024 * 1024;

/// The most of one control sequence or string kept. A reply is far shorter; one that
/// is not is read to its end and reported [`Event::Unknown`] with this much of it.
const MAX_SEQUENCE_BYTES: usize = 4096;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
/// The bracketed paste's closing sequence, matched raw.
const PASTE_END: &[u8] = b"\x1b[201~";
/// The mouse report's motion bit (xterm `Cb` + 32).
const MOTION_MASK: u8 = 32;
/// The lock bits kitty reports among a chord's modifiers.
const LOCKS: Modifiers = Modifiers::CAPS_LOCK.union(Modifiers::NUM_LOCK);

/// One thing the terminal said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A key press. `text` is the character it types: set for a printable key with no
    /// chord modifier (Shift aside), `None` for a chord or a key that types nothing.
    Key {
        key: Key,
        mods: Modifiers,
        text: Option<char>,
    },
    /// A mouse report, at a 0-based cell.
    Mouse {
        kind: MouseKind,
        /// The button pressed, released or held. `None` for a wheel notch, a motion
        /// with no button held, and an X10 release, which does not say which.
        button: Option<MouseButton>,
        col: u16,
        row: u16,
        mods: Modifiers,
    },
    /// A bracketed paste's body, exactly as the terminal sent it (line breaks are
    /// usually CR, as aterm and xterm send them), cut at [`MAX_PASTE_BYTES`].
    Paste(String),
    /// Focus gained (`true`) or lost.
    Focus(bool),
    /// DEC 2048's in-band size report.
    Resize { cols: u16, rows: u16 },
    /// A reply to a probe.
    Reply(Reply),
    /// A complete sequence this decoder does not read, as it arrived (at most
    /// `MAX_SEQUENCE_BYTES` of it).
    Unknown(Vec<u8>),
}

/// What a mouse report did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Release,
    /// The pointer moved: a drag when [`Event::Mouse::button`] is set.
    Motion,
    Wheel(WheelDir),
}

/// A probe reply (docs/DESIGN-amacs-2026-10-19.md §5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// XTVERSION, `DCS > | name ST`: aterm says `aterm(<version>)`.
    Version(String),
    /// Kitty's keyboard flags, `CSI ? flags u`.
    KittyFlags(u32),
    /// DECRPM, `CSI ? mode ; state $ y`.
    Mode { mode: u32, state: ModeState },
    /// DA1, `CSI ? attributes c`, the probe's sentinel.
    PrimaryAttributes(Vec<u32>),
}

/// A DECRPM mode state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeState {
    NotRecognized,
    Set,
    Reset,
    PermanentlySet,
    PermanentlyReset,
}

impl ModeState {
    fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            0 => Self::NotRecognized,
            1 => Self::Set,
            2 => Self::Reset,
            3 => Self::PermanentlySet,
            4 => Self::PermanentlyReset,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    /// Inside a UTF-8 character, after its lead: `need` more bytes, the next one in
    /// `lo..=hi` (the lead's second-byte range).
    Utf8 {
        need: u8,
        lo: u8,
        hi: u8,
    },
    /// Inside a UTF-8 character, past its second byte: `need` more trailing bytes.
    Utf8Tail {
        need: u8,
    },
    Esc,
    EscEsc,
    Csi,
    Ss3,
    /// `ESC P` with nothing after it yet.
    DcsStart,
    Dcs,
    /// `ESC ]` with nothing after it yet.
    OscStart,
    /// `ESC ]` and the digits of an OSC number, before its `;`.
    OscNumber,
    Osc,
    /// An ESC inside a DCS or OSC string: `\` ends the string.
    StringEsc {
        osc: bool,
    },
    /// `CSI M`: an X10 mouse report with `got` of its three bytes read.
    X10 {
        got: u8,
    },
    /// Inside a bracketed paste, `matched` bytes into [`PASTE_END`].
    Paste {
        matched: usize,
    },
}

/// The input decoder. See the module docs.
#[derive(Debug)]
pub struct Decoder {
    state: State,
    /// The raw bytes of the unit being read, from its first byte.
    seq: Vec<u8>,
    /// The unit is ESC-prefixed: `ESC ESC [`, `ESC ESC O`, or ESC and a UTF-8 lead.
    alt: bool,
    /// The unit outgrew `MAX_SEQUENCE_BYTES`; the rest of it is read, not kept.
    overflow: bool,
    paste: Vec<u8>,
    paste_overflow: bool,
    deadline: Option<Instant>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            seq: Vec::new(),
            alt: false,
            overflow: false,
            paste: Vec::new(),
            paste_overflow: false,
            deadline: None,
        }
    }

    /// Decode `bytes`, read at `now`. They continue whatever an earlier read left
    /// unfinished, however late they are read; `now` only arms the next deadline.
    #[must_use]
    pub fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<Event> {
        let mut out = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.state == (State::Paste { matched: 0 }) {
                // The body runs to the next ESC: take it whole.
                let run = rest.iter().position(|&b| b == ESC).unwrap_or(rest.len());
                let (body, after) = rest.split_at_checked(run).unwrap_or((rest, &[]));
                self.paste_extend(body);
                rest = after;
            }
            if let Some((&b, after)) = rest.split_first() {
                self.step(b, &mut out);
                rest = after;
            }
        }
        match self.wait() {
            None => self.deadline = None,
            Some(wait) if !bytes.is_empty() => {
                self.deadline = Some(now.checked_add(wait).unwrap_or(now));
            }
            Some(_) => {}
        }
        out
    }

    /// Resolve what the reads left unfinished, if its deadline has passed by `now`.
    /// The caller calls it when a wait until [`Self::deadline`] found nothing to read.
    /// A held sequence is delivered as the keys its bytes spell (a lone ESC is Escape,
    /// a broken character U+FFFD), or as [`Event::Unknown`] when it was too long to
    /// keep or was a mouse report cut short; a stalled paste is delivered as far as it
    /// got, and the bytes after it are read as keys.
    #[must_use]
    pub fn flush(&mut self, now: Instant) -> Vec<Event> {
        let mut out = Vec::new();
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            if let State::Paste { matched } = self.state {
                self.paste_extend(PASTE_END.get(..matched).unwrap_or_default());
                self.finish_paste(&mut out);
            } else {
                self.abort(&mut out);
            }
            self.deadline = None;
        }
        out
    }

    /// When [`Self::flush`] next has something to resolve: `None` while nothing is
    /// held.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// How long what the decoder holds waits for more bytes: `None` when it holds
    /// nothing.
    fn wait(&self) -> Option<Duration> {
        match self.state {
            State::Ground => None,
            State::Paste { .. } => Some(PASTE_TIMEOUT),
            _ => Some(ESC_TIMEOUT),
        }
    }

    fn reset(&mut self) {
        self.state = State::Ground;
        self.seq.clear();
        self.alt = false;
        self.overflow = false;
    }

    fn enter(&mut self, state: State, b: u8) {
        self.seq.push(b);
        self.state = state;
    }

    fn keep(&mut self, b: u8) {
        if self.seq.len() < MAX_SEQUENCE_BYTES {
            self.seq.push(b);
        } else {
            self.overflow = true;
        }
    }

    /// End the unit being read without completing it: what was held is delivered as
    /// typed keys, or as `Unknown` when it cannot be (an overlong unit, a cut X10
    /// report).
    fn abort(&mut self, out: &mut Vec<Event>) {
        match self.state {
            State::Ground | State::Paste { .. } => return,
            State::X10 { .. } => out.push(Event::Unknown(std::mem::take(&mut self.seq))),
            _ if self.overflow => out.push(Event::Unknown(std::mem::take(&mut self.seq))),
            _ => typed_keys(&self.seq, out),
        }
        self.reset();
    }

    fn step(&mut self, b: u8, out: &mut Vec<Event>) {
        match self.state {
            State::Ground => self.ground(b, out),
            State::Utf8 { need, lo, hi } if (lo..=hi).contains(&b) => self.utf8_byte(need, b, out),
            State::Utf8Tail { need } if is_utf8_continuation(b) => self.utf8_byte(need, b, out),
            State::Utf8 { .. } | State::Utf8Tail { .. } => {
                self.abort(out);
                self.ground(b, out);
            }
            State::Esc => match b {
                b'[' => self.enter(State::Csi, b),
                b'O' => self.enter(State::Ss3, b),
                b'P' => self.enter(State::DcsStart, b),
                b']' => self.enter(State::OscStart, b),
                ESC => self.enter(State::EscEsc, b),
                0x00..=0x7F => {
                    out.push(alt_key(char::from(b)));
                    self.reset();
                }
                _ => match utf8_lead(b) {
                    Some((need, lo, hi)) => {
                        self.alt = true;
                        self.enter(State::Utf8 { need, lo, hi }, b);
                    }
                    // Escape, then U+FFFD for the stray byte.
                    None => {
                        self.abort(out);
                        self.ground(b, out);
                    }
                },
            },
            State::EscEsc => match b {
                b'[' => {
                    self.alt = true;
                    self.enter(State::Csi, b);
                }
                b'O' => {
                    self.alt = true;
                    self.enter(State::Ss3, b);
                }
                // Alt+Escape, then `b` afresh.
                _ => {
                    self.abort(out);
                    self.ground(b, out);
                }
            },
            State::Csi if is_csi_body(b) => self.keep(b),
            State::Csi if is_csi_final(b) => self.csi_final(b, out),
            State::Ss3 if matches!(b, b'0'..=b'9' | b';') => self.keep(b),
            State::Ss3 if is_csi_final(b) => self.ss3_final(b, out),
            State::Csi | State::Ss3 => {
                self.abort(out);
                self.ground(b, out);
            }
            // A DCS reply opens with a parameter or intermediate byte (`>|`, `!|`,
            // `1$r`), an OSC reply with its number; anything else was typed.
            State::DcsStart if is_csi_body(b) => {
                self.keep(b);
                self.state = State::Dcs;
            }
            State::OscStart | State::OscNumber if b.is_ascii_digit() => {
                self.keep(b);
                self.state = State::OscNumber;
            }
            State::OscNumber if b == b';' => {
                self.keep(b);
                self.state = State::Osc;
            }
            State::DcsStart | State::OscStart | State::OscNumber => {
                self.abort(out);
                self.ground(b, out);
            }
            State::Dcs | State::Osc => {
                let osc = self.state == State::Osc;
                match b {
                    ESC => {
                        self.keep(b);
                        self.state = State::StringEsc { osc };
                    }
                    BEL if osc => {
                        self.keep(b);
                        self.finish_string(osc, out);
                    }
                    _ if is_string_control(b) => {
                        self.abort(out);
                        self.ground(b, out);
                    }
                    _ => self.keep(b),
                }
            }
            State::StringEsc { osc } => {
                if b == b'\\' {
                    self.keep(b);
                    self.finish_string(osc, out);
                } else {
                    // Not ST: the string was typed keys, and this ESC starts afresh.
                    if !self.overflow {
                        self.seq.pop();
                    }
                    self.abort(out);
                    self.ground(ESC, out);
                    self.step(b, out);
                }
            }
            State::X10 { got } => {
                // Every X10 report byte is offset by 32.
                if b < 0x20 {
                    self.abort(out);
                    return self.ground(b, out);
                }
                self.seq.push(b);
                if got < 2 {
                    self.state = State::X10 { got: got + 1 };
                    return;
                }
                let report = self
                    .seq
                    .len()
                    .checked_sub(3)
                    .and_then(|at| self.seq.get(at..));
                let event = match report {
                    Some(&[cb, col, row]) => mouse(
                        u32::from(cb.saturating_sub(32)),
                        u32::from(col.saturating_sub(32)),
                        u32::from(row.saturating_sub(32)),
                        false,
                        true,
                    ),
                    _ => None,
                };
                self.emit(event, out);
                self.reset();
            }
            State::Paste { matched } => self.paste_byte(matched, b, out),
        }
    }

    /// Take `b`, which fits, into the UTF-8 character being read: `need` bytes were
    /// still to come. Every byte after the second is one of Table 3-7's trailing bytes.
    fn utf8_byte(&mut self, need: u8, b: u8, out: &mut Vec<Event>) {
        self.seq.push(b);
        if need > 1 {
            self.state = State::Utf8Tail { need: need - 1 };
            return;
        }
        let start = usize::from(self.alt);
        let c = self
            .seq
            .get(start..)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|s| s.chars().next())
            .unwrap_or(char::REPLACEMENT_CHARACTER);
        out.push(if self.alt { alt_key(c) } else { plain_key(c) });
        self.reset();
    }

    fn ground(&mut self, b: u8, out: &mut Vec<Event>) {
        match b {
            ESC => self.enter(State::Esc, b),
            0x00..=0x7F => out.push(plain_key(char::from(b))),
            _ => match utf8_lead(b) {
                Some((need, lo, hi)) => self.enter(State::Utf8 { need, lo, hi }, b),
                None => out.push(plain_key(char::REPLACEMENT_CHARACTER)),
            },
        }
    }

    /// Deliver a complete non-key unit: an ESC prefix on it was a press of Escape.
    fn emit(&mut self, event: Option<Event>, out: &mut Vec<Event>) {
        match event {
            Some(event) => {
                if self.alt {
                    out.push(named(NamedKey::Escape, Modifiers::empty()));
                }
                out.push(event);
            }
            None => out.push(Event::Unknown(std::mem::take(&mut self.seq))),
        }
    }

    fn csi_final(&mut self, b: u8, out: &mut Vec<Event>) {
        if self.overflow || self.seq.len() >= MAX_SEQUENCE_BYTES {
            out.push(Event::Unknown(std::mem::take(&mut self.seq)));
            return self.reset();
        }
        let introducer = if self.alt { 3 } else { 2 };
        let body = self.seq.get(introducer..).unwrap_or_default();
        if body.is_empty() && b == b'M' {
            return self.enter(State::X10 { got: 0 }, b);
        }
        let outcome = csi(body, b);
        self.seq.push(b);
        match outcome {
            Csi::Key(key, mods, text) => out.push(self.key(key, mods, text)),
            Csi::Event(event) => self.emit(event, out),
            Csi::PasteStart => {
                // The paste is delivered whole at its end; an ESC before it was Escape.
                if self.alt {
                    out.push(named(NamedKey::Escape, Modifiers::empty()));
                }
                self.reset();
                self.paste.clear();
                self.paste_overflow = false;
                self.state = State::Paste { matched: 0 };
                return;
            }
            Csi::Typed => typed_keys(&self.seq, out),
            Csi::Unknown => out.push(Event::Unknown(std::mem::take(&mut self.seq))),
        }
        self.reset();
    }

    fn ss3_final(&mut self, b: u8, out: &mut Vec<Event>) {
        let introducer = if self.alt { 3 } else { 2 };
        let params = self.seq.get(introducer..).unwrap_or_default();
        let mods = match fields(params).last() {
            None => Some(Modifiers::empty()),
            Some(field) => modifiers(field),
        };
        match (ss3_key(b), mods, self.overflow) {
            (Some(named), Some(mods), false) => {
                let (key, mods, text) = reading(Key::Named(named), mods, None, None);
                out.push(self.key(key, mods, text));
            }
            // No terminal sends any other SS3: it was Alt+Shift+O and typed keys.
            _ => {
                self.keep(b);
                if self.overflow {
                    out.push(Event::Unknown(std::mem::take(&mut self.seq)));
                } else {
                    typed_keys(&self.seq, out);
                }
            }
        }
        self.reset();
    }

    fn finish_string(&mut self, osc: bool, out: &mut Vec<Event>) {
        let seq = std::mem::take(&mut self.seq);
        // `ESC P > | name ESC \`.
        let version = (!osc && !self.overflow)
            .then(|| seq.strip_prefix(b"\x1bP>|")?.strip_suffix(b"\x1b\\"))
            .flatten();
        out.push(match version {
            Some(name) => Event::Reply(Reply::Version(String::from_utf8_lossy(name).into_owned())),
            None => Event::Unknown(seq),
        });
        self.reset();
    }

    /// A key read from a sequence, with Alt when it was ESC-prefixed (a chord then,
    /// typing no text).
    fn key(&self, key: Key, mods: Modifiers, text: Option<char>) -> Event {
        if self.alt {
            Event::Key {
                key,
                mods: mods | Modifiers::ALT,
                text: None,
            }
        } else {
            Event::Key { key, mods, text }
        }
    }

    fn paste_byte(&mut self, matched: usize, b: u8, out: &mut Vec<Event>) {
        if PASTE_END.get(matched) == Some(&b) {
            let matched = matched.saturating_add(1);
            if matched < PASTE_END.len() {
                self.state = State::Paste { matched };
                return;
            }
            return self.finish_paste(out);
        }
        // What matched was body after all; only an ESC can start the end again.
        self.paste_extend(PASTE_END.get(..matched).unwrap_or_default());
        if b == ESC {
            self.state = State::Paste { matched: 1 };
        } else {
            self.paste_extend(&[b]);
            self.state = State::Paste { matched: 0 };
        }
    }

    /// Deliver the paste read so far and go back to reading keys.
    fn finish_paste(&mut self, out: &mut Vec<Event>) {
        let mut body = std::mem::take(&mut self.paste);
        if self.paste_overflow {
            trim_partial_char(&mut body);
        }
        self.paste_overflow = false;
        let text = String::from_utf8(body).unwrap_or_else(|err| lossy_paste(err.as_bytes()));
        out.push(Event::Paste(text));
        self.reset();
    }

    fn paste_extend(&mut self, bytes: &[u8]) {
        let room = MAX_PASTE_BYTES.saturating_sub(self.paste.len());
        let (kept, dropped) = bytes.split_at(bytes.len().min(room));
        self.paste.extend_from_slice(kept);
        if !dropped.is_empty() {
            self.paste_overflow = true;
        }
    }
}

/// Decode an invalid paste without letting replacement characters exceed its byte cap.
fn lossy_paste(bytes: &[u8]) -> String {
    let mut text = String::new();
    for chunk in bytes.utf8_chunks() {
        let valid = chunk.valid();
        let room = MAX_PASTE_BYTES.saturating_sub(text.len());
        let end = valid.floor_char_boundary(valid.len().min(room));
        text.push_str(valid.get(..end).unwrap_or_default());
        if end < valid.len() {
            break;
        }
        if !chunk.invalid().is_empty() {
            if MAX_PASTE_BYTES.saturating_sub(text.len()) < char::REPLACEMENT_CHARACTER.len_utf8() {
                break;
            }
            text.push(char::REPLACEMENT_CHARACTER);
        }
    }
    text
}

/// The key, modifiers and text a lone character spells, with the legacy readings of
/// the module docs.
fn plain(c: char) -> (Key, Modifiers, Option<char>) {
    let none = Modifiers::empty();
    let ctrl = |c: char| (Key::Character(c), Modifiers::CTRL, None);
    match c {
        '\0' => (Key::Named(NamedKey::Space), Modifiers::CTRL, None),
        '\x08' | '\x7f' => (Key::Named(NamedKey::Backspace), none, None),
        '\t' => (Key::Named(NamedKey::Tab), none, None),
        '\r' => (Key::Named(NamedKey::Enter), none, None),
        '\x1b' => (Key::Named(NamedKey::Escape), none, None),
        '\x1c' => ctrl('\\'),
        '\x1d' => ctrl(']'),
        '\x1e' => ctrl('^'),
        '\x1f' => ctrl('_'),
        // The rest of C0, LF included: Ctrl and the letter.
        '\x01'..='\x1a' => ctrl(char::from(c as u8 + b'`')),
        ' ' => (Key::Named(NamedKey::Space), none, Some(' ')),
        'A'..='Z' => (
            Key::Character(c.to_ascii_lowercase()),
            Modifiers::SHIFT,
            Some(c),
        ),
        _ => (Key::Character(c), none, (!c.is_control()).then_some(c)),
    }
}

/// [`plain`] as an event.
fn plain_key(c: char) -> Event {
    let (key, mods, text) = plain(c);
    Event::Key { key, mods, text }
}

/// [`plain_key`] ESC-prefixed: the same key with Alt, typing nothing.
fn alt_key(c: char) -> Event {
    let (key, mods, _) = plain(c);
    Event::Key {
        key,
        mods: mods | Modifiers::ALT,
        text: None,
    }
}

fn named(key: NamedKey, mods: Modifiers) -> Event {
    Event::Key {
        key: Key::Named(key),
        mods,
        text: None,
    }
}

/// Deliver held bytes as the keys they spell when they made no sequence: an ESC and
/// the key after it are that key with Alt (a lone or trailing ESC is Escape), and
/// the rest decodes as legacy keys.
fn typed_keys(bytes: &[u8], out: &mut Vec<Event>) {
    let mut rest = bytes;
    while let Some((&b, after)) = rest.split_first() {
        if b == ESC {
            match after.split_first() {
                Some((&next, tail)) if next.is_ascii() => {
                    out.push(alt_key(char::from(next)));
                    rest = tail;
                }
                _ => match first_char(after) {
                    Some((c, len)) => {
                        out.push(alt_key(c));
                        rest = after.get(len..).unwrap_or_default();
                    }
                    None => {
                        out.push(named(NamedKey::Escape, Modifiers::empty()));
                        rest = after;
                    }
                },
            }
        } else {
            let run = rest.iter().position(|&b| b == ESC).unwrap_or(rest.len());
            let (text, after) = rest.split_at(run);
            out.extend(String::from_utf8_lossy(text).chars().map(plain_key));
            rest = after;
        }
    }
}

/// The complete, valid UTF-8 character `bytes` starts with, and its length.
fn first_char(bytes: &[u8]) -> Option<(char, usize)> {
    let len = usize::from(utf8_len(*bytes.first()?));
    let c = std::str::from_utf8(bytes.get(..len)?)
        .ok()?
        .chars()
        .next()?;
    Some((c, len))
}

// The byte classes below are the decoder's reading of two standards, each a Trust
// contract whose cited theorem is about the compiled fn (SPECS.md): the UTF-8 rows of
// Unicode Table 3-7 ("Well-Formed UTF-8 Byte Sequences") and the control-sequence
// byte classes of ECMA-48 §5.4.

trust_contract_fn! {
    /// The length of the UTF-8 character a byte leads (1 for a byte that leads none):
    /// Table 3-7's first column, which leaves out C0 and C1 (they could only lead
    /// overlong forms) and F5-FF (they could only lead code points past U+10FFFF).
    fn utf8_len(lead: u8) -> u8
    clauses [ensures result == utf8_len_spec(lead) by utf8_len_is_table_3_7]
    {
        match lead {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => 1,
        }
    }
}

trust_contract_fn! {
    /// The least byte that may follow a multi-byte lead (Table 3-7's second column):
    /// above 0x80 only after E0 and F0, whose smaller second bytes spell overlong
    /// forms.
    fn utf8_second_min(lead: u8) -> u8
    clauses [ensures result == utf8_second_min_spec(lead) by utf8_second_min_is_table_3_7]
    {
        match lead {
            0xE0 => 0xA0,
            0xF0 => 0x90,
            _ => 0x80,
        }
    }
}

trust_contract_fn! {
    /// The greatest byte that may follow a multi-byte lead (Table 3-7's second column):
    /// below 0xBF only after ED, whose larger second bytes spell the surrogates, and
    /// F4, whose larger ones spell code points past U+10FFFF.
    fn utf8_second_max(lead: u8) -> u8
    clauses [ensures result == utf8_second_max_spec(lead) by utf8_second_max_is_table_3_7]
    {
        match lead {
            0xED => 0x9F,
            0xF4 => 0x8F,
            _ => 0xBF,
        }
    }
}

trust_contract_fn! {
    /// A byte that continues a UTF-8 character: Table 3-7's trailing bytes, 80-BF.
    fn is_utf8_continuation(b: u8) -> bool
    clauses [ensures result == (0x80 <= b && b <= 0xBF) by utf8_continuation_is_table_3_7]
    {
        matches!(b, 0x80..=0xBF)
    }
}

trust_contract_fn! {
    /// A byte that continues a control sequence's body (ECMA-48 §5.4): a parameter
    /// byte (03/00-03/15) or an intermediate byte (02/00-02/15).
    fn is_csi_body(b: u8) -> bool
    clauses [ensures result == (0x20 <= b && b <= 0x3F) by csi_body_is_ecma_48]
    {
        matches!(b, 0x20..=0x3F)
    }
}

trust_contract_fn! {
    /// A byte of a parameter string's sub-strings (ECMA-48 §5.4.1: 03/00-03/11, the
    /// digits, `:` and `;`).
    fn is_csi_parameter(b: u8) -> bool
    clauses [ensures result == (0x30 <= b && b <= 0x3B) by csi_parameter_is_ecma_48]
    {
        matches!(b, 0x30..=0x3B)
    }
}

trust_contract_fn! {
    /// A byte that opens a private parameter string (ECMA-48 §5.4.1: 03/12-03/15,
    /// `<`, `=`, `>` and `?`).
    fn is_csi_private_marker(b: u8) -> bool
    clauses [ensures result == (0x3C <= b && b <= 0x3F) by csi_private_marker_is_ecma_48]
    {
        matches!(b, 0x3C..=0x3F)
    }
}

trust_contract_fn! {
    /// An intermediate byte (ECMA-48 §5.4: 02/00-02/15).
    fn is_csi_intermediate(b: u8) -> bool
    clauses [ensures result == (0x20 <= b && b <= 0x2F) by csi_intermediate_is_ecma_48]
    {
        matches!(b, 0x20..=0x2F)
    }
}

trust_contract_fn! {
    /// The byte that ends a control sequence (ECMA-48 §5.4: 04/00-07/14).
    fn is_csi_final(b: u8) -> bool
    clauses [ensures result == (0x40 <= b && b <= 0x7E) by csi_final_is_ecma_48]
    {
        matches!(b, 0x40..=0x7E)
    }
}

trust_contract_fn! {
    /// A C0 control or DEL: inside a DCS or OSC string it means the string was typed
    /// keys (ECMA-48 §5.6 admits only 00/08-00/13 and 02/00-07/14 there; BEL ends an
    /// OSC and ESC may start ST, and both are read before this).
    fn is_string_control(b: u8) -> bool
    clauses [ensures result == (b <= 0x1F || b == 0x7F) by string_control_is_c0_or_del]
    {
        matches!(b, 0x00..=0x1F | 0x7F)
    }
}

spec_island! {
    clean {
        -- Unicode Table 3-7, Well-Formed UTF-8 Byte Sequences: the lead byte ranges of
        -- its rows 2 to 9 and the sequence length each starts; any other byte leads none.
        def utf8_len_spec (lead : UInt8) : UInt8 :=
          if And (0xC2 <= lead) (lead <= 0xDF) then 2
          else if And (0xE0 <= lead) (lead <= 0xEF) then 3
          else if And (0xF0 <= lead) (lead <= 0xF4) then 4
          else 1

        -- Table 3-7, second column: the rows whose second byte does not start at 80.
        def utf8_second_min_spec (lead : UInt8) : UInt8 :=
          if lead = 0xE0 then 0xA0 else if lead = 0xF0 then 0x90 else 0x80

        -- Table 3-7, second column: the rows whose second byte does not end at BF.
        def utf8_second_max_spec (lead : UInt8) : UInt8 :=
          if lead = 0xED then 0x9F else if lead = 0xF4 then 0x8F else 0xBF

        -- A range pattern compiles to one test per bound, so a range condition splits.
        -- The proof reduces each decision, so the dev seal checks it as the lane prover
        -- does; the dev seal refuses a simp proof of it (SPECS.md).
        theorem ite_and_split {t : Type} (P Q : Prop) [dp : Decidable P] [dq : Decidable Q] (a b : t) :
            (if And P Q then a else b) = (if P then (if Q then a else b) else b) := by
          cases dp <;> cases dq <;> rfl
    }
}

// The theorems tie each contract to its spec, one island each, so a refusal names its
// own theorem.
verify_island! {
    clean {
        theorem utf8_len_is_table_3_7 :
            forall (lead : UInt8), trust_import_aterm_tty__decode__utf8_len lead = utf8_len_spec lead := by
          intro lead
          simp only [trust_import_aterm_tty__decode__utf8_len, utf8_len_spec, ite_and_split]
    }
}

verify_island! {
    clean {
        theorem utf8_second_min_is_table_3_7 :
            forall (lead : UInt8), trust_import_aterm_tty__decode__utf8_second_min lead = utf8_second_min_spec lead := by
          intro lead
          simp only [trust_import_aterm_tty__decode__utf8_second_min, utf8_second_min_spec]
    }
}

verify_island! {
    clean {
        theorem utf8_second_max_is_table_3_7 :
            forall (lead : UInt8), trust_import_aterm_tty__decode__utf8_second_max lead = utf8_second_max_spec lead := by
          intro lead
          simp only [trust_import_aterm_tty__decode__utf8_second_max, utf8_second_max_spec]
    }
}

verify_island! {
    clean {
        theorem utf8_continuation_is_table_3_7 :
            forall (b : UInt8), trust_import_aterm_tty__decode__is_utf8_continuation b = (decide (0x80 <= b) && decide (b <= 0xBF)) := by
          intro b
          simp only [trust_import_aterm_tty__decode__is_utf8_continuation]
    }
}

verify_island! {
    clean {
        theorem csi_body_is_ecma_48 :
            forall (b : UInt8), trust_import_aterm_tty__decode__is_csi_body b = (decide (0x20 <= b) && decide (b <= 0x3F)) := by
          intro b
          simp only [trust_import_aterm_tty__decode__is_csi_body]
    }
}

verify_island! {
    clean {
        theorem csi_parameter_is_ecma_48 :
            forall (b : UInt8), trust_import_aterm_tty__decode__is_csi_parameter b = (decide (0x30 <= b) && decide (b <= 0x3B)) := by
          intro b
          simp only [trust_import_aterm_tty__decode__is_csi_parameter]
    }
}

verify_island! {
    clean {
        theorem csi_private_marker_is_ecma_48 :
            forall (b : UInt8), trust_import_aterm_tty__decode__is_csi_private_marker b = (decide (0x3C <= b) && decide (b <= 0x3F)) := by
          intro b
          simp only [trust_import_aterm_tty__decode__is_csi_private_marker]
    }
}

verify_island! {
    clean {
        theorem csi_intermediate_is_ecma_48 :
            forall (b : UInt8), trust_import_aterm_tty__decode__is_csi_intermediate b = (decide (0x20 <= b) && decide (b <= 0x2F)) := by
          intro b
          simp only [trust_import_aterm_tty__decode__is_csi_intermediate]
    }
}

verify_island! {
    clean {
        theorem csi_final_is_ecma_48 :
            forall (b : UInt8), trust_import_aterm_tty__decode__is_csi_final b = (decide (0x40 <= b) && decide (b <= 0x7E)) := by
          intro b
          simp only [trust_import_aterm_tty__decode__is_csi_final]
    }
}

verify_island! {
    clean {
        theorem string_control_is_c0_or_del :
            forall (b : UInt8), trust_import_aterm_tty__decode__is_string_control b = (decide (b <= 0x1F) || decide (b = 0x7F)) := by
          intro b
          simp only [trust_import_aterm_tty__decode__is_string_control]
    }
}

/// For a multi-byte UTF-8 lead: the bytes still to come and the range the next one
/// must fall in (which rules out overlong forms, surrogates and code points past
/// U+10FFFF, as the standard decoder does).
fn utf8_lead(b: u8) -> Option<(u8, u8, u8)> {
    let need = match utf8_len(b) {
        2 => 1,
        3 => 2,
        4 => 3,
        _ => return None,
    };
    Some((need, utf8_second_min(b), utf8_second_max(b)))
}

/// Drop a character the paste cap cut in two.
fn trim_partial_char(bytes: &mut Vec<u8>) {
    let len = bytes.len();
    let lead = (1..=len.min(4))
        .filter_map(|back| len.checked_sub(back))
        .find(|&at| bytes.get(at).is_some_and(|&b| !is_utf8_continuation(b)));
    if let Some(at) = lead
        && bytes
            .get(at)
            .is_some_and(|&b| at.saturating_add(usize::from(utf8_len(b))) > len)
    {
        bytes.truncate(at);
    }
}

/// What a complete `CSI body final` means.
enum Csi {
    Key(Key, Modifiers, Option<char>),
    /// A non-key event, or `None` for a malformed one (delivered `Unknown`).
    Event(Option<Event>),
    PasteStart,
    /// No terminal sends this bare: `ESC [` was Alt+[ and the final a typed key.
    Typed,
    Unknown,
}

/// One `;`-separated parameter and its `:`-separated sub-parameters (`None` empty).
type Field = Vec<Option<u32>>;

fn csi(body: &[u8], fin: u8) -> Csi {
    if body.is_empty() {
        return match fin {
            b'I' => Csi::Event(Some(Event::Focus(true))),
            b'O' => Csi::Event(Some(Event::Focus(false))),
            b'Z' => Csi::Key(Key::Named(NamedKey::Tab), Modifiers::SHIFT, None),
            _ => match csi_letter_key(fin) {
                Some(named) => Csi::Key(Key::Named(named), Modifiers::empty(), None),
                None => Csi::Typed,
            },
        };
    }
    // A private marker, then parameters, then intermediates.
    let (marker, rest) = match body.split_first() {
        Some((&m, rest)) if is_csi_private_marker(m) => (Some(m), rest),
        _ => (None, body),
    };
    let end = rest
        .iter()
        .position(|&b| !is_csi_parameter(b))
        .unwrap_or(rest.len());
    let (params, intermediates) = rest.split_at_checked(end).unwrap_or((rest, &[]));
    if !intermediates.iter().all(|&b| is_csi_intermediate(b)) {
        return Csi::Unknown;
    }
    let fields = fields(params);
    match (marker, intermediates, fin) {
        (None, b"", b'~') => tilde(&fields),
        (None, b"", b'u') => kitty_key(&fields),
        (None, b"", b'Z') => match letter_mods(&fields) {
            Some(mods) => Csi::Key(Key::Named(NamedKey::Tab), mods | Modifiers::SHIFT, None),
            None => Csi::Unknown,
        },
        (None, b"", b't') => Csi::Event(resize(&fields)),
        (None, b"", _) => match (csi_letter_key(fin), letter_mods(&fields)) {
            (Some(named), Some(mods)) => Csi::Key(Key::Named(named), mods, None),
            _ => Csi::Unknown,
        },
        (Some(b'?'), b"", b'u') => Csi::Event(match fields.as_slice() {
            [flags] => Some(Event::Reply(Reply::KittyFlags(value(flags).unwrap_or(0)))),
            _ => None,
        }),
        (Some(b'?'), b"$", b'y') => Csi::Event(match fields.as_slice() {
            [mode, state] => value(mode)
                .zip(value(state).and_then(ModeState::from_code))
                .map(|(mode, state)| Event::Reply(Reply::Mode { mode, state })),
            _ => None,
        }),
        (Some(b'?'), b"", b'c') => Csi::Event(Some(Event::Reply(Reply::PrimaryAttributes(
            fields.iter().map(|f| value(f).unwrap_or(0)).collect(),
        )))),
        (Some(b'<'), b"", b'M' | b'm') => Csi::Event(match fields.as_slice() {
            [cb, col, row] => match (value(cb), value(col), value(row)) {
                (Some(cb), Some(col), Some(row)) => mouse(cb, col, row, fin == b'm', false),
                _ => None,
            },
            _ => None,
        }),
        _ => Csi::Unknown,
    }
}

fn fields(params: &[u8]) -> Vec<Field> {
    if params.is_empty() {
        return Vec::new();
    }
    params
        .split(|&b| b == b';')
        .map(|field| field.split(|&b| b == b':').map(number).collect())
        .collect()
}

/// A run of ASCII digits, saturating; `None` when empty.
fn number(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() {
        return None;
    }
    Some(digits.iter().fold(0u32, |n, d| {
        n.saturating_mul(10)
            .saturating_add(u32::from(d.saturating_sub(b'0')))
    }))
}

fn value(field: &Field) -> Option<u32> {
    field.first().copied().flatten()
}

/// The modifiers of a `1 + bits` parameter, which xterm and kitty agree on (Shift 1,
/// Alt 2, Ctrl 4, Super 8; kitty adds Hyper and Meta above, and the Caps Lock and
/// Num Lock bits, which are dropped). `None` for a kitty release (`:3`), which a
/// flag-1 terminal never sends.
fn modifiers(field: &Field) -> Option<Modifiers> {
    if !matches!(field.get(1).copied().flatten(), None | Some(1 | 2)) {
        return None;
    }
    let bits = value(field).unwrap_or(1).saturating_sub(1);
    u8::try_from(bits)
        .ok()
        .map(|bits| Modifiers::from_bits_retain(bits) - LOCKS)
}

/// The modifiers of a letter key's `1 ; m` (or bare `1`) parameters.
fn letter_mods(fields: &[Field]) -> Option<Modifiers> {
    match fields {
        [] => Some(Modifiers::empty()),
        [lead] if matches!(value(lead), None | Some(1)) => Some(Modifiers::empty()),
        [lead, mods] if matches!(value(lead), None | Some(1)) => modifiers(mods),
        _ => None,
    }
}

fn tilde(fields: &[Field]) -> Csi {
    let key = |n: &Field, mods: Option<Modifiers>| match (value(n).and_then(tilde_key), mods) {
        (Some(named), Some(mods)) => Csi::Key(Key::Named(named), mods, None),
        _ => Csi::Unknown,
    };
    match fields {
        [n] if n.len() == 1 && value(n) == Some(200) => Csi::PasteStart,
        [n] => key(n, Some(Modifiers::empty())),
        [n, mods] => key(n, modifiers(mods)),
        // xterm's modifyOtherKeys: `CSI 27 ; m ; code ~`.
        [lead, mods, code] if value(lead) == Some(27) => {
            match (value(code).and_then(code_key), modifiers(mods)) {
                (Some(key), Some(mods)) => {
                    let (key, mods, text) = reading(key, mods, None, None);
                    Csi::Key(key, mods, text)
                }
                _ => Csi::Unknown,
            }
        }
        _ => Csi::Unknown,
    }
}

fn kitty_key(fields: &[Field]) -> Csi {
    let Some((code, rest)) = fields.split_first() else {
        return Csi::Unknown;
    };
    if rest.len() > 2 {
        return Csi::Unknown;
    }
    let mods = match rest.first() {
        Some(field) => modifiers(field),
        None => Some(Modifiers::empty()),
    };
    let glyph = |n: Option<u32>| n.and_then(char::from_u32).filter(|c| !c.is_control());
    // `code:shifted:base`: the shifted key (flag 4) while Shift is held; the base-layout
    // key is not read.
    let shifted = glyph(code.get(1).copied().flatten());
    // Associated text (flag 16) is the third parameter; flags 1 and 4 send none.
    let text = glyph(rest.get(1).and_then(value));
    match (value(code).and_then(code_key), mods) {
        (Some(key), Some(mods)) => {
            let (key, mods, text) = reading(key, mods, shifted, text);
            Csi::Key(key, mods, text)
        }
        _ => Csi::Unknown,
    }
}

/// The canonical reading (module docs) of a key a sequence names by its code: a keypad
/// key is its main-block twin, its glyph typing itself unless a chord modifier is held;
/// a Shift chord with its `shifted` key is that glyph as legacy bytes spell it; and a
/// character or space with no modifier types itself.
fn reading(
    key: Key,
    mods: Modifiers,
    shifted: Option<char>,
    text: Option<char>,
) -> (Key, Modifiers, Option<char>) {
    match key.main_block_twin() {
        Some(Key::Character(glyph)) if (mods - Modifiers::SHIFT).is_empty() => {
            return (Key::Character(glyph), Modifiers::empty(), Some(glyph));
        }
        Some(twin) => return (twin, mods, None),
        None => {}
    }
    let glyph = match (&key, shifted) {
        (Key::Character(_), Some(shifted)) if mods.contains(Modifiers::SHIFT) => shifted,
        (Key::Character(c), _) if mods.is_empty() && text.is_none() => *c,
        (Key::Named(NamedKey::Space), _) if mods.is_empty() && text.is_none() => ' ',
        _ => return (key, mods, text),
    };
    let (key, glyph_mods, glyph_text) = plain(glyph);
    let mods = (mods - Modifiers::SHIFT) | glyph_mods;
    let chord = !(mods - Modifiers::SHIFT).is_empty();
    (key, mods, if chord { None } else { glyph_text })
}

fn resize(fields: &[Field]) -> Option<Event> {
    let cells = |field: &Field| value(field).map(|v| u16::try_from(v).unwrap_or(u16::MAX));
    match fields {
        [ps, rows, cols] | [ps, rows, cols, _, _] if value(ps) == Some(48) => Some(Event::Resize {
            cols: cells(cols)?,
            rows: cells(rows)?,
        }),
        _ => None,
    }
}

/// A mouse report's event from xterm's `Cb` and 1-based cell. `legacy` reads `Cb` 3
/// as a release of an unnamed button, as X10 sends one.
fn mouse(cb: u32, col: u32, row: u32, release: bool, legacy: bool) -> Option<Event> {
    let cb = u8::try_from(cb).ok()?;
    let mut mods = Modifiers::empty();
    mods.set(Modifiers::SHIFT, cb & SHIFT_MASK != 0);
    mods.set(Modifiers::ALT, cb & ALT_MASK != 0);
    mods.set(Modifiers::CTRL, cb & CTRL_MASK != 0);
    let motion = cb & MOTION_MASK != 0;
    let base = cb & !(SHIFT_MASK | ALT_MASK | CTRL_MASK | MOTION_MASK);
    let (kind, button) = match (base, motion, release) {
        (64, false, false) => (MouseKind::Wheel(WheelDir::Up), None),
        (65, false, false) => (MouseKind::Wheel(WheelDir::Down), None),
        (66, false, false) => (MouseKind::Wheel(WheelDir::Left), None),
        (67, false, false) => (MouseKind::Wheel(WheelDir::Right), None),
        (3, true, false) => (MouseKind::Motion, None),
        (3, false, false) if legacy => (MouseKind::Release, None),
        (_, true, true) => return None,
        (_, motion, release) => {
            let button = match base {
                0 => MouseButton::Left,
                1 => MouseButton::Middle,
                2 => MouseButton::Right,
                128 => MouseButton::Back,
                129 => MouseButton::Forward,
                _ => return None,
            };
            let kind = if motion {
                MouseKind::Motion
            } else if release {
                MouseKind::Release
            } else {
                MouseKind::Press
            };
            (kind, Some(button))
        }
    };
    let cell = |one_based: u32| u16::try_from(one_based.saturating_sub(1)).unwrap_or(u16::MAX);
    Some(Event::Mouse {
        kind,
        button,
        col: cell(col),
        row: cell(row),
        mods,
    })
}

/// The key a kitty or modifyOtherKeys code names: a [`NamedKey`] by its kitty code,
/// otherwise the character with that code point.
fn code_key(code: u32) -> Option<Key> {
    let named = match code {
        9 => Some(NamedKey::Tab),
        13 => Some(NamedKey::Enter),
        27 => Some(NamedKey::Escape),
        32 => Some(NamedKey::Space),
        127 => Some(NamedKey::Backspace),
        _ => code
            .checked_sub(FIRST_FUNCTIONAL_CODE)
            .and_then(|at| FUNCTIONAL_KEYS.get(usize::try_from(at).ok()?))
            .copied(),
    };
    match named {
        Some(named) => Some(Key::Named(named)),
        None => char::from_u32(code).map(Key::Character),
    }
}

/// The letter-final CSI keys: the cursor keys, Home, End and keypad Begin, and F1-F4
/// (xterm's modified forms; kitty's unmodified F1, F2 and F4 too).
fn csi_letter_key(fin: u8) -> Option<NamedKey> {
    Some(match fin {
        b'A' => NamedKey::ArrowUp,
        b'B' => NamedKey::ArrowDown,
        b'C' => NamedKey::ArrowRight,
        b'D' => NamedKey::ArrowLeft,
        b'H' => NamedKey::Home,
        b'F' => NamedKey::End,
        b'E' => NamedKey::NumpadBegin,
        b'P' => NamedKey::F1,
        b'Q' => NamedKey::F2,
        b'R' => NamedKey::F3,
        b'S' => NamedKey::F4,
        _ => return None,
    })
}

/// The `CSI n ~` keys: xterm's numbers as aterm's legacy encoder writes them (F13-F24
/// on from 25), with the rxvt and VT220 Home and End (1, 4, 7, 8) and F1-F4 (11-14).
fn tilde_key(n: u32) -> Option<NamedKey> {
    use NamedKey::*;
    Some(match n {
        1 | 7 => Home,
        2 => Insert,
        3 => Delete,
        4 | 8 => End,
        5 => PageUp,
        6 => PageDown,
        11 => F1,
        12 => F2,
        13 => F3,
        14 => F4,
        15 => F5,
        17 => F6,
        18 => F7,
        19 => F8,
        20 => F9,
        21 => F10,
        23 => F11,
        24 => F12,
        25 => F13,
        26 => F14,
        28 => F15,
        29 => F16,
        31 => F17,
        32 => F18,
        33 => F19,
        34 => F20,
        35 => F21,
        36 => F22,
        37 => F23,
        38 => F24,
        _ => return None,
    })
}

/// The SS3 keys: F1-F4, the application-mode cursor keys, Home and End, and the
/// application keypad.
fn ss3_key(fin: u8) -> Option<NamedKey> {
    use NamedKey::*;
    Some(match fin {
        b'A' => ArrowUp,
        b'B' => ArrowDown,
        b'C' => ArrowRight,
        b'D' => ArrowLeft,
        b'H' => Home,
        b'F' => End,
        b'P' => F1,
        b'Q' => F2,
        b'R' => F3,
        b'S' => F4,
        b'E' => NumpadBegin,
        b'M' => NumpadEnter,
        b'X' => NumpadEqual,
        b'j' => NumpadMultiply,
        b'k' => NumpadAdd,
        b'l' => NumpadSeparator,
        b'm' => NumpadSubtract,
        b'n' => NumpadDecimal,
        b'o' => NumpadDivide,
        b'p' => Numpad0,
        b'q' => Numpad1,
        b'r' => Numpad2,
        b's' => Numpad3,
        b't' => Numpad4,
        b'u' => Numpad5,
        b'v' => Numpad6,
        b'w' => Numpad7,
        b'x' => Numpad8,
        b'y' => Numpad9,
        _ => return None,
    })
}

/// The kitty code of `FUNCTIONAL_KEYS[0]`; each next key's code is one more
/// ([`NamedKey::kitty_code`]).
const FIRST_FUNCTIONAL_CODE: u32 = 57348;

/// Every [`NamedKey`] with a code in kitty's private-use block, in code order.
const FUNCTIONAL_KEYS: [NamedKey; 105] = {
    use NamedKey::*;
    [
        Insert,
        Delete,
        ArrowLeft,
        ArrowRight,
        ArrowUp,
        ArrowDown,
        PageUp,
        PageDown,
        Home,
        End,
        CapsLock,
        ScrollLock,
        NumLock,
        PrintScreen,
        Pause,
        ContextMenu,
        F1,
        F2,
        F3,
        F4,
        F5,
        F6,
        F7,
        F8,
        F9,
        F10,
        F11,
        F12,
        F13,
        F14,
        F15,
        F16,
        F17,
        F18,
        F19,
        F20,
        F21,
        F22,
        F23,
        F24,
        F25,
        F26,
        F27,
        F28,
        F29,
        F30,
        F31,
        F32,
        F33,
        F34,
        F35,
        Numpad0,
        Numpad1,
        Numpad2,
        Numpad3,
        Numpad4,
        Numpad5,
        Numpad6,
        Numpad7,
        Numpad8,
        Numpad9,
        NumpadDecimal,
        NumpadDivide,
        NumpadMultiply,
        NumpadSubtract,
        NumpadAdd,
        NumpadEnter,
        NumpadEqual,
        NumpadSeparator,
        NumpadArrowLeft,
        NumpadArrowRight,
        NumpadArrowUp,
        NumpadArrowDown,
        NumpadPageUp,
        NumpadPageDown,
        NumpadHome,
        NumpadEnd,
        NumpadInsert,
        NumpadDelete,
        NumpadBegin,
        MediaPlay,
        MediaPause,
        MediaPlayPause,
        MediaReverse,
        MediaStop,
        MediaFastForward,
        MediaRewind,
        MediaTrackNext,
        MediaTrackPrevious,
        MediaRecord,
        AudioVolumeDown,
        AudioVolumeUp,
        AudioVolumeMute,
        ShiftLeft,
        ControlLeft,
        AltLeft,
        SuperLeft,
        HyperLeft,
        MetaLeft,
        ShiftRight,
        ControlRight,
        AltRight,
        SuperRight,
        HyperRight,
        MetaRight,
    ]
};
