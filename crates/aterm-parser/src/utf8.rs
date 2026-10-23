// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! UTF-8 multi-byte sequence handling for the parser.

use crate::Parser;
use crate::action::ActionSink;

/// The length of the well-formed multi-byte UTF-8 sequence `lead` begins (Unicode
/// §3.9, Table 3-7), or `None` for a byte that begins none: ASCII, a continuation
/// byte, `0xC0`/`0xC1` (every sequence they begin is overlong) and `0xF5..=0xFF`
/// (every sequence they begin is past U+10FFFF).
#[inline]
pub(crate) const fn utf8_sequence_len(lead: u8) -> Option<u8> {
    match lead {
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

/// Whether `byte` begins a well-formed multi-byte UTF-8 sequence.
#[inline]
pub(crate) const fn is_utf8_lead(byte: u8) -> bool {
    utf8_sequence_len(byte).is_some()
}

/// Whether `byte` extends the well-formed prefix of `held` bytes (`1..=3`)
/// that `lead` begins (Unicode §3.9, Table 3-7).
#[inline]
pub(crate) const fn utf8_continues(lead: u8, held: u8, byte: u8) -> bool {
    if !is_utf8_lead(lead) {
        return false;
    }
    let (low, high) = match (held, lead) {
        (1, 0xE0) => (0xA0, 0xBF),
        (1, 0xED) => (0x80, 0x9F),
        (1, 0xF0) => (0x90, 0xBF),
        (1, 0xF4) => (0x80, 0x8F),
        _ => (0x80, 0xBF),
    };
    low <= byte && byte <= high
}

/// Whether `continuation` extends `lead` the way the decoder would take it ([`utf8_continues`]).
#[inline]
pub(crate) fn utf8_prefix_well_formed(lead: u8, continuation: &[u8]) -> bool {
    let mut held = 1u8;
    for &byte in continuation {
        if !utf8_continues(lead, held, byte) {
            return false;
        }
        held = held.saturating_add(1);
    }
    true
}

/// The head of a split UTF-8 character that a byte stream ends in: the bytes of a well-formed
/// prefix ([`utf8_continues`], [`Utf8Prefix::continues`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Utf8Prefix {
    bytes: [u8; 3],
    len: u8,
}

impl Utf8Prefix {
    /// No character in progress.
    pub(crate) const NONE: Self = Self {
        bytes: [0; 3],
        len: 0,
    };

    /// The bytes held, lead first.
    #[inline]
    pub(crate) fn as_slice(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// Whether `byte` extends the character this prefix begins.
    #[inline]
    pub(crate) const fn continues(self, byte: u8) -> bool {
        self.len != 0 && utf8_continues(self.bytes[0], self.len, byte)
    }

    /// The prefix the stream ends in once `byte` follows it.
    #[inline]
    pub(crate) fn step(self, byte: u8) -> Self {
        if self.continues(byte) {
            let len = self.len.saturating_add(1);
            if utf8_sequence_len(self.bytes[0]) == Some(len) {
                return Self::NONE;
            }
            let mut next = self;
            if let Some(slot) = next.bytes.get_mut(usize::from(self.len)) {
                *slot = byte;
            }
            next.len = len;
            next
        } else if is_utf8_lead(byte) {
            Self {
                bytes: [byte, 0, 0],
                len: 1,
            }
        } else {
            Self::NONE
        }
    }

    /// The prefix the stream ends in once all of `bytes` follow it ([`Utf8Prefix::step`]).
    #[inline]
    pub(crate) fn after(self, bytes: &[u8]) -> Self {
        let (start, tail) = match bytes.len().checked_sub(3) {
            Some(skip) => (Self::NONE, bytes.get(skip..).unwrap_or(&[])),
            None => (self, bytes),
        };
        tail.iter().fold(start, |prefix, &byte| prefix.step(byte))
    }

    /// The prefix that is exactly `bytes` ([`Utf8Prefix::NONE`] when empty), or `None` when no
    /// stream ends in them.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let Some((&lead, continuation)) = bytes.split_first() else {
            return Some(Self::NONE);
        };
        let expected = usize::from(utf8_sequence_len(lead)?);
        if bytes.len() >= expected || !utf8_prefix_well_formed(lead, continuation) {
            return None;
        }
        Some(
            bytes
                .iter()
                .fold(Self::NONE, |prefix, &byte| prefix.step(byte)),
        )
    }

    /// The length of the sequence the prefix begins, or `0` for none.
    #[inline]
    pub(crate) const fn expected(self) -> u8 {
        if self.len == 0 {
            0
        } else if let Some(len) = utf8_sequence_len(self.bytes[0]) {
            len
        } else {
            0
        }
    }
}

impl Parser {
    /// Decode a codepoint from bytes [`utf8_continues`] already checked, by bit arithmetic.
    #[inline]
    pub(crate) fn decode_utf8_validated(buf: [u8; 4], len: u8) -> char {
        let cp = match len {
            2 => {
                let b0 = u32::from(buf[0] & 0x1F);
                let b1 = u32::from(buf[1] & 0x3F);
                let v = (b0 << 6) | b1;
                if v < 0x80 {
                    return char::REPLACEMENT_CHARACTER;
                }
                v
            }
            3 => {
                let b0 = u32::from(buf[0] & 0x0F);
                let b1 = u32::from(buf[1] & 0x3F);
                let b2 = u32::from(buf[2] & 0x3F);
                let v = (b0 << 12) | (b1 << 6) | b2;
                if v < 0x800 {
                    return char::REPLACEMENT_CHARACTER;
                }
                v
            }
            4 => {
                let b0 = u32::from(buf[0] & 0x07);
                let b1 = u32::from(buf[1] & 0x3F);
                let b2 = u32::from(buf[2] & 0x3F);
                let b3 = u32::from(buf[3] & 0x3F);
                let v = (b0 << 18) | (b1 << 12) | (b2 << 6) | b3;
                if v < 0x10000 {
                    return char::REPLACEMENT_CHARACTER;
                }
                v
            }
            _ => return char::REPLACEMENT_CHARACTER,
        };
        char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER)
    }

    /// Start a UTF-8 multi-byte sequence on `byte` ([`is_utf8_lead`]).
    #[inline]
    pub(crate) fn start_utf8(&mut self, byte: u8) {
        self.utf8_buffer[0] = byte;
        self.utf8_len = 1;
        self.utf8_expected = utf8_sequence_len(byte).unwrap_or(2);
    }

    /// Process a byte as part of a UTF-8 sequence.
    #[inline]
    pub(crate) fn process_utf8_byte<S: ActionSink>(&mut self, byte: u8, sink: &mut S) {
        if utf8_continues(self.utf8_buffer[0], self.utf8_len, byte) {
            // `utf8_len < 4` while collecting, so `get_mut` is always `Some`.
            if let Some(slot) = self.utf8_buffer.get_mut(usize::from(self.utf8_len)) {
                *slot = byte;
            }
            self.utf8_len = self.utf8_len.saturating_add(1);

            if self.utf8_len == self.utf8_expected {
                let c = Self::decode_utf8_validated(self.utf8_buffer, self.utf8_len);
                sink.print(c);
                self.utf8_len = 0;
                self.utf8_expected = 0;
            }
        } else {
            sink.print(char::REPLACEMENT_CHARACTER);
            self.utf8_len = 0;
            self.utf8_expected = 0;

            if is_utf8_lead(byte) {
                self.start_utf8(byte);
            } else if byte >= 0x80 {
                sink.print(char::REPLACEMENT_CHARACTER);
            } else {
                self.process_byte_inner(byte, sink);
            }
        }
    }
}
