// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! UTF-8 multi-byte sequence handling for the parser.

use crate::Parser;
use crate::action::ActionSink;

impl Parser {
    /// Decode a UTF-8 codepoint from pre-validated bytes.
    ///
    /// The lead byte and all continuation bytes have already been range-checked,
    /// so we extract the codepoint directly via bit manipulation. This avoids
    /// the overhead of `std::str::from_utf8` which re-scans for validity.
    ///
    /// Returns REPLACEMENT_CHARACTER for overlong encodings or surrogates.
    #[inline]
    pub(crate) fn decode_utf8_validated(buf: [u8; 4], len: u8) -> char {
        let cp = match len {
            2 => {
                let b0 = u32::from(buf[0] & 0x1F);
                let b1 = u32::from(buf[1] & 0x3F);
                let v = (b0 << 6) | b1;
                // Reject overlong: 2-byte must encode >= 0x80
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
                // Reject overlong: 3-byte must encode >= 0x800
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
                // Reject overlong: 4-byte must encode >= 0x10000
                if v < 0x10000 {
                    return char::REPLACEMENT_CHARACTER;
                }
                v
            }
            _ => return char::REPLACEMENT_CHARACTER,
        };
        // char::from_u32 rejects surrogates (0xD800..0xDFFF) and values > 0x10FFFF
        char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER)
    }

    /// Start a UTF-8 multi-byte sequence.
    #[inline]
    pub(crate) fn start_utf8(&mut self, byte: u8) {
        self.utf8_buffer[0] = byte;
        self.utf8_len = 1;

        // Determine expected sequence length from lead byte
        self.utf8_expected = if byte >= 0xF0 {
            4
        } else if byte >= 0xE0 {
            3
        } else if byte >= 0xC0 {
            2
        } else {
            1 // Should not happen, but handle gracefully
        };
    }

    /// Process a byte as part of a UTF-8 sequence.
    #[inline]
    pub(crate) fn process_utf8_byte<S: ActionSink>(&mut self, byte: u8, sink: &mut S) {
        // Check if this is a valid continuation byte
        if (0x80..=0xBF).contains(&byte) {
            // Invariant: `utf8_len < 4` while collecting (utf8_expected <= 4),
            // so `get_mut` is always `Some` at runtime; the `get_mut` +
            // saturating increment spelling keeps the buffer write provably
            // in-bounds without changing behavior.
            if let Some(slot) = self.utf8_buffer.get_mut(usize::from(self.utf8_len)) {
                *slot = byte;
            }
            self.utf8_len = self.utf8_len.saturating_add(1);

            if self.utf8_len == self.utf8_expected {
                // Decode directly from validated bytes — skips std::str::from_utf8 scan.
                // Each byte has already been range-checked (lead in start_utf8,
                // continuations above), so we can extract the codepoint via bit math.
                let c = Self::decode_utf8_validated(self.utf8_buffer, self.utf8_len);
                sink.print(c);
                self.utf8_len = 0;
                self.utf8_expected = 0;
            }
        } else {
            // Invalid continuation - emit replacement for partial sequence
            sink.print(char::REPLACEMENT_CHARACTER);
            self.utf8_len = 0;
            self.utf8_expected = 0;

            // Re-process this byte (it might be a new sequence or control)
            if (0xC0..=0xF7).contains(&byte) {
                self.start_utf8(byte);
            } else if byte >= 0x80 {
                // Another invalid byte
                sink.print(char::REPLACEMENT_CHARACTER);
            } else {
                // ASCII or control - process normally
                self.process_byte_inner(byte, sink);
            }
        }
    }
}
