// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Base64, hex, the wire's percent escape, the JSON string escape, the
//! typed record sinks, the terminal-safe rendering, CRC-32 and DEFLATE for
//! aterm.
//!
//! Its one dependency is astream-wire (itself dependency-free), whose CRC-32
//! [`crc32`] wraps. Provides:
//!
//! - [`base64`] — standard Base64 (RFC 4648 §4), strict and lenient decode.
//! - [`crc32`] — CRC-32/ISO-HDLC, the gzip trailer's checksum, as a running
//!   value over astream-wire's fold.
//! - [`hex`] — lowercase/uppercase hex encode, either-case decode, and the
//!   "exactly N hex digits" checks.
//! - [`fields`] — one record, two syntaxes: a [`fields::FieldSink`] that a
//!   producer writes each field to once, spelled as the control wire's
//!   `key=value` line ([`fields::TextSink`]) or one JSON object
//!   ([`fields::JsonSink`]).
//! - [`inflate`] — RFC 1951 (DEFLATE) + RFC 1950 (zlib) decompression with a
//!   decompression-bomb output ceiling.
//! - [`json`] — the control path's JSON string escape: RFC 8259's, plus every
//!   character a terminal acts on as `\uXXXX`.
//! - [`pct`] — the control wire's percent escape (uppercase `%XX` for every
//!   non-graphic byte and `%`) and its total decoder.
//! - [`tty`] — text for a terminal: what a terminal acts on or reorders text
//!   around, shown as text (`\xNN`, `\u{…}`), whole or as it streams.
//!
//! ## Usage
//!
//! ```rust
//! use aterm_codec::base64;
//!
//! // Base64
//! let encoded = base64::encode(b"Hello, world!").unwrap();
//! assert_eq!(encoded, "SGVsbG8sIHdvcmxkIQ==");
//! let decoded = base64::decode(&encoded).unwrap();
//! assert_eq!(decoded, b"Hello, world!");
//! ```

pub mod base64;
/// The cell wire's attribute vocabulary and strict style reader.
pub mod cell;
pub mod crc32;
pub mod fields;
pub mod hex;
pub mod inflate;
pub mod json;
pub mod pct;
pub mod tty;

use std::fmt;

/// Write `v` in decimal, identical to `write!(f, "{v}")` (width/fill flags on
/// `f` are ignored). Avoids `format_args!`, whose unsafe `fmt::Arguments`
/// constructor Trust's model fails closed on.
#[inline]
pub(crate) fn write_usize_decimal(f: &mut fmt::Formatter<'_>, v: usize) -> fmt::Result {
    // usize is at most 64 bits, so at most 20 decimal digits.
    let mut buf = [0u8; 20];
    let mut v = v;
    let mut i = buf.len();
    while i > 0 {
        i -= 1;
        if let Some(slot) = buf.get_mut(i) {
            // At most 57, so this never saturates; the saturating form makes
            // the no-overflow bound locally provable.
            *slot = b'0'.saturating_add((v % 10) as u8);
        }
        v /= 10;
        if v == 0 {
            break;
        }
    }
    match std::str::from_utf8(buf.get(i..).unwrap_or(&[])) {
        Ok(s) => f.write_str(s),
        // Unreachable: the buffer holds only ASCII digits.
        Err(_) => Err(fmt::Error),
    }
}

/// Write `b` as exactly two uppercase hex digits, byte-identical to
/// `write!(f, "{b:02X}")` (a `u8` always renders as exactly two digits under
/// `02X`). Same rationale as [`write_usize_decimal`]: no `format_args!`.
#[inline]
pub(crate) fn write_hex_byte_upper(f: &mut fmt::Formatter<'_>, b: u8) -> fmt::Result {
    const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";
    let digits = [HEX_UPPER[(b >> 4) as usize], HEX_UPPER[(b & 0x0F) as usize]];
    match std::str::from_utf8(&digits) {
        Ok(s) => f.write_str(s),
        // Unreachable: both bytes are ASCII hex digits.
        Err(_) => Err(fmt::Error),
    }
}

/// Maximum length, in bytes, of untrusted input accepted by any codec entry
/// point (encode or decode).
///
/// Inputs come from programs (OSC 52 clipboard, OSC 1337 images), so this caps
/// every allocation. At 64 MiB every derived length stays under the verifier's
/// 268_435_456-element allocation threshold and no length multiply overflows.
pub const MAX_INPUT_LEN: usize = 64 << 20;
