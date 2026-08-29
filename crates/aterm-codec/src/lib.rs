// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Base64 and hex encoding/decoding for aterm.
//!
//! Zero external dependencies. Provides:
//!
//! - [`base64`] — standard and URL-safe Base64 with optional padding.
//! - [`hex`] — hexadecimal encoding and decoding.
//! - [`inflate`] — RFC 1951 (DEFLATE) + RFC 1950 (zlib) decompression with a
//!   decompression-bomb output ceiling.
//!
//! ## Usage
//!
//! ```rust
//! use aterm_codec::{base64, hex};
//!
//! // Base64
//! let encoded = base64::encode(b"Hello, world!").unwrap();
//! assert_eq!(encoded, "SGVsbG8sIHdvcmxkIQ==");
//! let decoded = base64::decode(&encoded).unwrap();
//! assert_eq!(decoded, b"Hello, world!");
//!
//! // URL-safe Base64 (no padding)
//! let encoded = base64::encode_url_safe_no_pad(b"Hello, world!").unwrap();
//! let decoded = base64::decode_url_safe_no_pad(&encoded).unwrap();
//! assert_eq!(decoded, b"Hello, world!");
//!
//! // Hex
//! let encoded = hex::encode(b"\xde\xad\xbe\xef").unwrap();
//! assert_eq!(encoded, "deadbeef");
//! let decoded = hex::decode(&encoded).unwrap();
//! assert_eq!(decoded, b"\xde\xad\xbe\xef");
//! ```

#![deny(clippy::all)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod base64;
pub mod crc32;
pub mod hex;
pub mod inflate;

use std::fmt;

/// Write `v` in decimal, byte-identical to `write!(f, "{v}")` for every
/// `usize` — including ignoring any width/fill flags on `f`, exactly as a
/// `write!` with its own fresh format args would.
///
/// Exists because the `format_args!` expansion inside `write!` calls the
/// unsafe `fmt::Arguments` constructor, which Trust's model fails closed on
/// (`hardened_unsafe_operation`); plain `write_str` avoids it entirely.
pub(crate) fn write_usize_decimal(f: &mut fmt::Formatter<'_>, v: usize) -> fmt::Result {
    // usize is at most 64 bits, so at most 20 decimal digits.
    let mut buf = [0u8; 20];
    let mut v = v;
    let mut i = buf.len();
    while i > 0 {
        i -= 1;
        if let Some(slot) = buf.get_mut(i) {
            // `v % 10 <= 9`, so this is at most 48 + 9 = 57 and never actually
            // saturates; the saturating form makes the no-overflow bound
            // locally provable (the verifier drops the `% 10` range across
            // the cast). Identical digit on every path.
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
/// Codec inputs arrive from untrusted sources (e.g. OSC 52 clipboard payloads
/// and OSC 1337 inline-image data emitted by arbitrary programs). This cap
/// bounds every allocation the codec performs so a hostile peer cannot force an
/// unbounded `Vec`/`String` reservation (DoS hardening). It is deliberately set
/// well below the verifier's per-allocation DoS threshold: at 64 MiB input,
/// every derived count (base64/hex output length = at most ~4/3 or 2x the
/// input) stays under the 268_435_456-element threshold AND no `usize`
/// length multiply can overflow.
pub const MAX_INPUT_LEN: usize = 64 << 20;
